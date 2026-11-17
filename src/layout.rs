//! Layout: what the children of a frame do when the box of the frame changes.
//!
//! Every change of a frame's box goes through `set_box` (a node that is not in the document)
//! or `Doc::set_box` (one that is, which also lays resized instances out again from their
//! mains). Today the children answer with their constraints; a frame with a layout mode of its
//! own would place them here instead.

use crate::doc::{Doc, Kind, Node};
use serde::{Deserialize, Serialize};

/// A box as (x, y, w, h), in world coordinates.
pub type Rect = (f32, f32, f32, f32);

/// How a child of a frame keeps its place along one axis when the frame is resized. Named for
/// the horizontal axis; vertically `Min` is the top and `Max` the bottom.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Constraint {
    /// Keeps its distance to the left (top) edge.
    #[default]
    Min,
    /// Keeps its distance to the right (bottom) edge.
    Max,
    /// Keeps both distances: its size changes with the frame.
    Stretch,
    /// Keeps the offset of its centre from the centre of the frame.
    Center,
    /// Place and size scale with the frame.
    Scale,
}

impl Constraint {
    pub const ALL: [Constraint; 5] = [Constraint::Min, Constraint::Max, Constraint::Stretch, Constraint::Center, Constraint::Scale];

    pub fn is_default(&self) -> bool {
        *self == Constraint::Min
    }

    /// The name in the Design panel.
    pub fn label(self, horizontal: bool) -> &'static str {
        match (self, horizontal) {
            (Constraint::Min, true) => "Left",
            (Constraint::Min, false) => "Top",
            (Constraint::Max, true) => "Right",
            (Constraint::Max, false) => "Bottom",
            (Constraint::Stretch, true) => "Left and right",
            (Constraint::Stretch, false) => "Top and bottom",
            (Constraint::Center, _) => "Center",
            (Constraint::Scale, _) => "Scale",
        }
    }

    /// The name in the remote API.
    pub fn key(self, horizontal: bool) -> &'static str {
        match (self, horizontal) {
            (Constraint::Min, true) => "left",
            (Constraint::Min, false) => "top",
            (Constraint::Max, true) => "right",
            (Constraint::Max, false) => "bottom",
            (Constraint::Stretch, true) => "left_right",
            (Constraint::Stretch, false) => "top_bottom",
            (Constraint::Center, _) => "center",
            (Constraint::Scale, _) => "scale",
        }
    }

    pub fn parse(s: &str, horizontal: bool) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key(horizontal) == s)
    }

    /// One axis: a child at `p`, `s` long, in a frame that goes from (start, length) `from` to
    /// `to`. Result: its new (start, length), never shorter than `min`.
    pub fn span(self, (p, s): (f32, f32), from: (f32, f32), to: (f32, f32), min: f32) -> (f32, f32) {
        match self {
            Constraint::Min => (to.0 + (p - from.0), s),
            Constraint::Max => (to.0 + to.1 - (from.0 + from.1 - p), s),
            Constraint::Stretch => (to.0 + (p - from.0), (s + to.1 - from.1).max(min)),
            Constraint::Center => (to.0 + to.1 * 0.5 + (p - (from.0 + from.1 * 0.5)), s),
            Constraint::Scale => {
                let k = if from.1.abs() > 1e-3 { to.1 / from.1 } else { 1.0 };
                (to.0 + (p - from.0) * k, (s * k).max(min))
            }
        }
    }
}

/// What the children of a frame do when its box changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Children {
    /// Each child follows its constraints, and frames among them pass the change on.
    Constrain,
    /// They stay where they are in world space.
    Keep,
}

/// Give `n` the box `to`. This is the one way the box of a frame changes: a frame that only
/// moves takes what it holds along, one that changes size lays its children out again.
pub fn set_box(n: &mut Node, to: Rect, children: Children) {
    let from = (n.x, n.y, n.w, n.h);
    if children == Children::Constrain && (from.2, from.3) == (to.2, to.3) {
        return n.translate(to.0 - from.0, to.1 - from.1);
    }
    let floor = if n.kind == Kind::Line { f32::MIN } else { 1.0 };
    (n.x, n.y, n.w, n.h) = (to.0, to.1, to.2.max(floor), to.3.max(floor));
    if n.kind == Kind::Frame && children == Children::Constrain {
        let to = (n.x, n.y, n.w, n.h);
        for c in &mut n.children {
            reflow(c, from, to);
        }
    }
}

/// Place child `c` of a frame whose box went from `from` to `to`, by its constraints. A
/// rotated child is placed by its unrotated box.
pub fn reflow(c: &mut Node, from: Rect, to: Rect) {
    if (from.2, from.3) == (to.2, to.3) {
        // Whatever the constraints, it only moves along; this way without rounding.
        return c.translate(to.0 - from.0, to.1 - from.1);
    }
    let (x0, y0, x1, y1) = c.bounds();
    let (w, h) = (x1 - x0, y1 - y0);
    // Never below 1, but what is thinner than that (a straight line) stays as thin as it is.
    let (x, nw) = c.constrain_h.span((x0, w), (from.0, from.2), (to.0, to.2), w.min(1.0));
    let (y, nh) = c.constrain_v.span((y0, h), (from.1, from.3), (to.1, to.3), h.min(1.0));
    match c.kind {
        Kind::Line => {
            // A line is a vector: the end that was on the left stays the one on the left.
            (c.x, c.w) = if c.w < 0.0 { (x + nw, -nw) } else { (x, nw) };
            (c.y, c.h) = if c.h < 0.0 { (y + nh, -nh) } else { (y, nh) };
        }
        // The height of a text follows what it says. A paragraph wraps again in its new width;
        // auto-width text keeps its size, where its alignment anchors it in the new box.
        Kind::Text if c.wrap => (c.x, c.y, c.w) = (x, y, nw),
        Kind::Text => {
            c.x = match c.align {
                1 => x + (nw - w) * 0.5,
                2 => x + nw - w,
                _ => x,
            };
            c.y = y;
        }
        _ => set_box(c, (x, y, nw, nh), Children::Constrain),
    }
}

impl Doc {
    /// Give node `id` the box `to`. `from`: the node as it was when the gesture began, so a
    /// drag is worked out from there every time and nothing drifts; `None` takes the node as
    /// it is. Result: whether anything changed.
    pub fn set_box(&mut self, id: u64, from: Option<&Node>, to: Rect, children: Children) -> bool {
        let Some(was) = from.or_else(|| self.find(id)).cloned() else { return false };
        let mut n = was.clone();
        set_box(&mut n, to, children);
        self.refit(&mut n, &was);
        let changed = self.find(id).is_some_and(|now| *now != n);
        if changed {
            *self.find_mut(id).unwrap() = n;
        }
        changed
    }

    /// After `n` was laid out again from `was` (the same tree): an instance in it that changed
    /// size takes the places of its parts from its main component, which is where they are
    /// decided.
    pub fn refit(&mut self, n: &mut Node, was: &Node) {
        if n.is_instance() {
            if (n.w, n.h) != (was.w, was.h) {
                self.fit_instance(n);
            }
            return;
        }
        for (c, w) in n.children.iter_mut().zip(&was.children) {
            self.refit(c, w);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Constraint::*;

    fn node(id: u64, kind: Kind, (x, y, w, h): Rect, (ch, cv): (Constraint, Constraint)) -> Node {
        let mut n = Node::new(id, kind, x, y);
        (n.w, n.h, n.constrain_h, n.constrain_v) = (w, h, ch, cv);
        n
    }

    fn rect_of(n: &Node) -> Rect {
        (n.x, n.y, n.w, n.h)
    }

    /// Frame 1 at (100, 100), 200×100, holding `children`.
    fn frame(children: Vec<Node>) -> Node {
        let mut f = node(1, Kind::Frame, (100.0, 100.0, 200.0, 100.0), (Min, Min));
        f.children = children;
        f
    }

    /// The box of a 40×20 child at (20, 30) in the frame, after the frame became 300×160 at (90, 80).
    fn resized(c: (Constraint, Constraint)) -> Rect {
        let mut f = frame(vec![node(2, Kind::Rect, (120.0, 130.0, 40.0, 20.0), c)]);
        set_box(&mut f, (90.0, 80.0, 300.0, 160.0), Children::Constrain);
        assert_eq!(rect_of(&f), (90.0, 80.0, 300.0, 160.0));
        rect_of(&f.children[0])
    }

    #[test]
    fn each_constraint_on_each_axis() {
        // In the frame: 20 from the left, 140 from the right; 30 from the top, 50 from the bottom.
        assert_eq!(resized((Min, Min)), (110.0, 110.0, 40.0, 20.0));
        assert_eq!(resized((Max, Max)), (210.0, 170.0, 40.0, 20.0));
        assert_eq!(resized((Stretch, Stretch)), (110.0, 110.0, 140.0, 80.0));
        // Its centre was 60 left of and 10 above the centre of the frame (now at 240, 160).
        assert_eq!(resized((Center, Center)), (160.0, 140.0, 40.0, 20.0));
        // 1.5 times as wide, 1.6 times as high.
        assert_eq!(resized((Scale, Scale)), (120.0, 128.0, 60.0, 32.0));
        // The axes are independent.
        assert_eq!(resized((Max, Min)), (210.0, 110.0, 40.0, 20.0));
        assert_eq!(resized((Min, Stretch)), (110.0, 110.0, 40.0, 80.0));
        assert_eq!(resized((Center, Scale)), (160.0, 128.0, 40.0, 32.0));
        assert_eq!(resized((Scale, Max)), (120.0, 170.0, 60.0, 20.0));
    }

    #[test]
    fn a_frame_that_only_moves_takes_its_children_along() {
        let mut f = frame(vec![node(2, Kind::Rect, (120.0, 130.0, 40.0, 20.0), (Max, Scale))]);
        set_box(&mut f, (0.5, -7.25, 200.0, 100.0), Children::Constrain);
        assert_eq!((rect_of(&f), rect_of(&f.children[0])), ((0.5, -7.25, 200.0, 100.0), (20.5, 22.75, 40.0, 20.0)));
    }

    #[test]
    fn nested_frames_pass_the_change_on() {
        // A panel pinned to both sides, with a button pinned to its right edge and a hidden
        // badge in its centre; a plain frame beside it only moves, with what it holds.
        let mut panel = node(2, Kind::Frame, (110.0, 110.0, 100.0, 50.0), (Stretch, Min));
        let mut badge = node(4, Kind::Ellipse, (150.0, 125.0, 20.0, 20.0), (Center, Center));
        badge.visible = false;
        panel.children = vec![node(3, Kind::Rect, (180.0, 120.0, 20.0, 10.0), (Max, Min)), badge];
        let mut side = node(5, Kind::Frame, (250.0, 150.0, 40.0, 40.0), (Max, Max));
        side.children = vec![node(6, Kind::Rect, (255.0, 155.0, 10.0, 10.0), (Scale, Scale))];
        let mut f = frame(vec![panel, side]);
        set_box(&mut f, (100.0, 100.0, 260.0, 120.0), Children::Constrain);
        let (panel, side) = (&f.children[0], &f.children[1]);
        assert_eq!(rect_of(panel), (110.0, 110.0, 160.0, 50.0));
        assert_eq!(rect_of(&panel.children[0]), (240.0, 120.0, 20.0, 10.0));
        assert_eq!(rect_of(&panel.children[1]), (180.0, 125.0, 20.0, 20.0));
        assert_eq!((rect_of(side), rect_of(&side.children[0])), ((310.0, 170.0, 40.0, 40.0), (315.0, 175.0, 10.0, 10.0)));
    }

    #[test]
    fn text_lines_and_paths() {
        let mut para = node(2, Kind::Text, (110.0, 110.0, 100.0, 40.0), (Stretch, Stretch));
        para.wrap = true;
        let label = node(3, Kind::Text, (110.0, 160.0, 60.0, 20.0), (Stretch, Scale));
        let mut centred = node(4, Kind::Text, (110.0, 160.0, 60.0, 20.0), (Scale, Max));
        centred.align = 1;
        let mut right = node(5, Kind::Text, (110.0, 160.0, 60.0, 20.0), (Stretch, Center));
        right.align = 2;
        // A line from its right end up to its left end, and a vertical one.
        let slope = node(6, Kind::Line, (180.0, 150.0, -60.0, -30.0), (Stretch, Scale));
        let upright = node(7, Kind::Line, (150.0, 110.0, 0.0, 80.0), (Scale, Stretch));
        let mut path = node(8, Kind::Path, (120.0, 120.0, 40.0, 40.0), (Scale, Stretch));
        let p = |x: f32, y: f32| crate::doc::PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        path.points = vec![p(0.0, 0.0), p(1.0, 0.5), p(0.0, 1.0)];
        let mut spun = node(9, Kind::Rect, (120.0, 120.0, 40.0, 20.0), (Max, Stretch));
        spun.rotation = 30.0;
        let mut f = frame(vec![para, label, centred, right, slope, upright, path.clone(), spun]);
        set_box(&mut f, (100.0, 100.0, 400.0, 200.0), Children::Constrain);
        let at = |i: usize| rect_of(&f.children[i]);
        // A paragraph takes the new width (and wraps again); its height is its content's.
        assert_eq!(at(0), (110.0, 110.0, 300.0, 40.0));
        // Auto-width text only moves: to where its alignment puts it in the box it would have.
        assert_eq!(at(1), (110.0, 220.0, 60.0, 20.0));
        assert_eq!(at(2), (150.0, 260.0, 60.0, 20.0));
        assert_eq!(at(3), (310.0, 210.0, 60.0, 20.0));
        // The line keeps its direction; its box went from (120..180, 120..150) to (120..380, 140..200).
        assert_eq!(at(4), (380.0, 200.0, -260.0, -60.0));
        // Nothing to scale in its width: the vertical line stays vertical.
        assert_eq!(at(5), (200.0, 110.0, 0.0, 180.0));
        // The points of a path live in its box, so they stretch with it.
        assert_eq!((at(6), &f.children[6].points), ((140.0, 120.0, 80.0, 140.0), &path.points));
        // A rotated child: its unrotated box, the angle untouched.
        assert_eq!((at(7), f.children[7].rotation), ((320.0, 120.0, 40.0, 120.0), 30.0));
    }

    #[test]
    fn a_frame_smaller_than_the_margins_leaves_a_sliver() {
        // Margins of 20 + 140 and 30 + 50 don't fit in 100×40: the child keeps its left and top
        // margin and is 1 wide and high, never inside out.
        let mut f = frame(vec![
            node(2, Kind::Rect, (120.0, 130.0, 40.0, 20.0), (Stretch, Stretch)),
            node(3, Kind::Frame, (120.0, 130.0, 40.0, 20.0), (Stretch, Stretch)),
            node(4, Kind::Line, (120.0, 130.0, 40.0, 20.0), (Stretch, Stretch)),
            node(5, Kind::Rect, (120.0, 130.0, 40.0, 20.0), (Scale, Scale)),
        ]);
        set_box(&mut f, (100.0, 100.0, 100.0, 40.0), Children::Constrain);
        assert_eq!(rect_of(&f.children[0]), (120.0, 130.0, 1.0, 1.0));
        assert_eq!(rect_of(&f.children[1]), (120.0, 130.0, 1.0, 1.0));
        assert_eq!(rect_of(&f.children[2]), (120.0, 130.0, 1.0, 1.0));
        // The frame itself never gets thinner than 1 either, and scaling follows that.
        set_box(&mut f, (100.0, 100.0, -50.0, 0.0), Children::Constrain);
        assert_eq!(rect_of(&f), (100.0, 100.0, 1.0, 1.0));
        let c = &f.children[3];
        assert!((c.x - 100.1).abs() < 1e-3 && (c.y - 100.3).abs() < 1e-3 && (c.w, c.h) == (1.0, 1.0));
    }

    #[test]
    fn children_can_be_left_where_they_are() {
        let child = node(2, Kind::Rect, (120.0, 130.0, 40.0, 20.0), (Max, Stretch));
        let mut f = frame(vec![child.clone()]);
        set_box(&mut f, (50.0, 60.0, 500.0, 400.0), Children::Keep);
        assert_eq!((rect_of(&f), &f.children[0]), ((50.0, 60.0, 500.0, 400.0), &child));
    }

    #[test]
    fn a_drag_is_worked_out_from_where_it_began() {
        let mut doc = Doc::default();
        doc.insert(frame(vec![node(2, Kind::Rect, (120.0, 130.0, 40.0, 20.0), (Scale, Center))]), None);
        let start = doc.find(1).unwrap().clone();
        // Many steps of a drag, out and back: the same as one step from the start.
        for w in [201.0, 333.0, 7.0, 1.0, 950.0, 300.0] {
            assert!(doc.set_box(1, Some(&start), (100.0, 100.0, w, 160.0), Children::Constrain));
        }
        assert_eq!(rect_of(doc.find(2).unwrap()), (130.0, 160.0, 60.0, 20.0));
        assert!(!doc.set_box(1, Some(&start), (100.0, 100.0, 300.0, 160.0), Children::Constrain));
        assert!(doc.set_box(1, Some(&start), (100.0, 100.0, 200.0, 100.0), Children::Constrain));
        assert_eq!(doc.find(1), Some(&start));
        // Without a start it goes on from how the node is now.
        assert!(doc.set_box(1, None, (100.0, 100.0, 400.0, 100.0), Children::Constrain));
        assert!(doc.set_box(1, None, (100.0, 100.0, 300.0, 100.0), Children::Keep));
        assert_eq!(rect_of(doc.find(2).unwrap()), (140.0, 130.0, 80.0, 20.0));
        assert!(!doc.set_box(99, None, (0.0, 0.0, 1.0, 1.0), Children::Constrain));
    }

    #[test]
    fn an_instance_in_a_resized_frame_is_laid_out_from_its_main() {
        let mut doc = Doc::default();
        // Main 1 (200×100) with a rect pinned to the right; an instance of it stretches with frame 10.
        doc.insert(frame(vec![node(2, Kind::Rect, (260.0, 110.0, 30.0, 30.0), (Max, Min))]), None);
        doc.next_id = 20;
        assert_eq!(doc.make_component(&[1]), Ok(1));
        let mut inst = doc.instantiate(1).unwrap();
        inst.translate(900.0, 0.0);
        inst.constrain_h = Stretch;
        let id = inst.id;
        let mut holder = node(10, Kind::Frame, (1000.0, 100.0, 200.0, 100.0), (Min, Min));
        holder.children = vec![inst];
        doc.insert(holder, None);
        // Flipped, the part sits at the left edge: only the main can say where it goes.
        assert!(doc.flip(&[id], true));
        assert_eq!(doc.find(id).unwrap().children[0].x, 1010.0);
        assert!(doc.set_box(10, None, (1000.0, 100.0, 300.0, 100.0), Children::Constrain));
        let inst = doc.find(id).unwrap();
        assert_eq!((rect_of(inst), rect_of(&inst.children[0])), ((1000.0, 100.0, 300.0, 100.0), (1010.0, 110.0, 30.0, 30.0)));
    }

    #[test]
    fn constraints_are_saved_and_old_files_have_the_defaults() {
        let n = node(2, Kind::Rect, (0.0, 0.0, 5.0, 5.0), (Stretch, Max));
        let json = serde_json::to_string(&n).unwrap();
        assert!(json.contains(r#""constrain_h":"stretch""#) && json.contains(r#""constrain_v":"max""#));
        assert_eq!(serde_json::from_str::<Node>(&json).unwrap(), n);
        // Only written where they say something.
        let plain = serde_json::to_string(&Node::new(1, Kind::Rect, 0.0, 0.0)).unwrap();
        assert!(!plain.contains("constrain"));
        let old: Node = serde_json::from_str(r#"{"id":2,"name":"R","kind":"Rect","x":0,"y":0,"w":5,"h":5}"#).unwrap();
        assert_eq!((old.constrain_h, old.constrain_v), (Min, Min));
        for c in Constraint::ALL {
            assert_eq!((Constraint::parse(c.key(true), true), Constraint::parse(c.key(false), false)), (Some(c), Some(c)));
        }
        assert_eq!((Constraint::parse("top", true), Constraint::parse("left_right", false)), (None, None));
    }
}
