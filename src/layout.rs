//! Layout: where the children of a frame go.
//!
//! Constraints: what the children do when the box of the frame changes. Every such change goes
//! through `set_box` (a node that is not in the document) or `Doc::set_box` (one that is, which
//! also lays resized instances out again from their mains).
//!
//! Auto layout: a frame with `Node::layout` places its children itself, in a row, a column or
//! wrapping rows (`arrange`). The order of the items is the order of `children`: the first
//! child is the first item, and so the bottom one in the stack and in the Layers panel. Anything
//! that can change the outcome is followed by `Doc::settle_layout`, which lays out the frames
//! of the top-level nodes that changed and writes back only what came out different.

use crate::doc::{Doc, Kind, Node};
use std::sync::Arc;
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
/// moves takes what it holds along, one that changes size lays its children out again, by
/// their constraints or by its auto layout.
pub fn set_box(n: &mut Node, to: Rect, children: Children) {
    set_box_with(n, to, children, &[]);
}

fn set_box_with(n: &mut Node, to: Rect, children: Children, lifted: &[u64]) {
    let from = (n.x, n.y, n.w, n.h);
    if children == Children::Constrain && (from.2, from.3) == (to.2, to.3) {
        return n.translate(to.0 - from.0, to.1 - from.1);
    }
    let floor = if n.kind == Kind::Line { f32::MIN } else { 1.0 };
    (n.x, n.y, n.w, n.h) = (to.0, to.1, to.2.max(floor), to.3.max(floor));
    if n.kind == Kind::Frame && children == Children::Constrain {
        let to = (n.x, n.y, n.w, n.h);
        let auto = n.layout.is_some();
        for c in &mut n.children {
            if auto && !c.floats() {
                c.translate(to.0 - from.0, to.1 - from.1);
            } else {
                reflow_with(c, from, to, lifted);
            }
        }
        if auto {
            arrange_with(n, lifted);
        }
    }
}

/// Place child `c` of a frame whose box went from `from` to `to`, by its constraints. A
/// rotated child is placed by its unrotated box.
pub fn reflow(c: &mut Node, from: Rect, to: Rect) {
    reflow_with(c, from, to, &[]);
}

fn reflow_with(c: &mut Node, from: Rect, to: Rect, lifted: &[u64]) {
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
        // (A height that was set by hand is kept like that of any other box.)
        Kind::Text if c.wrap => {
            (c.x, c.y, c.w) = (x, y, nw);
            if c.fixed_h {
                c.h = nh;
            }
        }
        Kind::Text => {
            c.x = match c.align {
                1 => x + (nw - w) * 0.5,
                2 => x + nw - w,
                _ => x,
            };
            c.y = y;
            if c.fixed_h {
                c.h = nh;
            }
        }
        _ => set_box_with(c, (x, y, nw, nh), Children::Constrain, lifted),
    }
}

// ---------------------------------------------------------------- auto layout

/// The direction the items of an auto-layout frame run in.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    #[default]
    Horizontal,
    Vertical,
}

/// Where the items sit along an axis of their frame. `Between` only means something along the
/// direction of the layout: the first and last item at the edges, the free room shared out.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
    Between,
}

impl Align {
    pub const ALL: [Align; 4] = [Align::Start, Align::Center, Align::End, Align::Between];

    /// The name in the remote API.
    pub fn key(self) -> &'static str {
        match self {
            Align::Start => "start",
            Align::Center => "center",
            Align::End => "end",
            Align::Between => "space_between",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == s)
    }
}

/// How one axis of a node gets its size.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sizing {
    /// As set.
    #[default]
    Fixed,
    /// An auto-layout frame: as large as its items and padding.
    Hug,
    /// An item of an auto-layout frame: the room that frame has left.
    Fill,
}

impl Sizing {
    pub const ALL: [Sizing; 3] = [Sizing::Fixed, Sizing::Hug, Sizing::Fill];

    pub fn is_default(&self) -> bool {
        *self == Sizing::Fixed
    }

    /// The name in the Design panel.
    pub fn label(self) -> &'static str {
        match self {
            Sizing::Fixed => "Fixed",
            Sizing::Hug => "Hug contents",
            Sizing::Fill => "Fill container",
        }
    }

    /// The name in the remote API.
    pub fn key(self) -> &'static str {
        match self {
            Sizing::Fixed => "fixed",
            Sizing::Hug => "hug",
            Sizing::Fill => "fill",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == s)
    }
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Auto layout of a frame: its children are its items, in the order of `Node::children`.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AutoLayout {
    pub dir: Dir,
    /// Horizontal only: items that don't fit the width go on to a new row.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub wrap: bool,
    /// Between neighbours along the direction.
    pub gap: f32,
    /// Between the rows of a wrapping layout.
    #[serde(skip_serializing_if = "is_zero")]
    pub row_gap: f32,
    /// Left, top, right, bottom.
    pub pad: [f32; 4],
    /// Along the direction.
    pub align: Align,
    /// Across it: `Start`, `Center` or `End`.
    pub cross: Align,
}

impl Node {
    /// A frame that lays its children out itself.
    pub fn is_auto(&self) -> bool {
        self.kind == Kind::Frame && self.layout.is_some()
    }

    /// In an auto-layout frame: is this child one of the items it places?
    pub fn in_flow(&self) -> bool {
        self.visible && !self.floats()
    }

    /// Left out of the auto layout of its frame whether it is shown or not: set to "Absolute
    /// position", or a text on a path (the path decides where it stands; a frame that placed
    /// it as well would push it along the path on every frame).
    pub fn floats(&self) -> bool {
        self.absolute || (self.kind == Kind::Text && self.on_path.is_some())
    }

    pub fn sizing(&self, horizontal: bool) -> Sizing {
        if horizontal {
            self.size_h
        } else {
            self.size_v
        }
    }

    pub fn set_sizing(&mut self, horizontal: bool, s: Sizing) {
        *(if horizontal { &mut self.size_h } else { &mut self.size_v }) = s;
    }

    /// Can this item take the room its frame has left on that axis? A turned item and a line
    /// keep their size; of a text only the width of a paragraph can be given.
    pub fn can_fill(&self, horizontal: bool) -> bool {
        self.rotation == 0.0
            && match self.kind {
                Kind::Line => false,
                Kind::Text if horizontal => self.wrap,
                Kind::Text => self.fixed_h,
                _ => true,
            }
    }

    fn fills(&self, horizontal: bool) -> bool {
        self.sizing(horizontal) == Sizing::Fill && self.can_fill(horizontal)
    }

    fn hugs(&self, horizontal: bool) -> bool {
        self.is_auto() && self.sizing(horizontal) == Sizing::Hug
    }

    /// Does anything at or below this node ask for the layout pass?
    fn wants_layout(&self) -> bool {
        self.any(&|n| n.layout.is_some() || n.absolute || n.size_h != Sizing::Fixed || n.size_v != Sizing::Fixed)
    }
}

/// (start, length) of a box along one axis.
fn span((x0, y0, x1, y1): Rect, horizontal: bool) -> (f32, f32) {
    if horizontal {
        (x0, x1 - x0)
    } else {
        (y0, y1 - y0)
    }
}

/// Move `c` (with what it holds) so that the box around it starts at (x, y).
fn move_to(c: &mut Node, x: f32, y: f32) {
    let b = c.aabb();
    let (dx, dy) = (x - b.0, y - b.1);
    // Already there, give or take the rounding of the last move: laying out twice changes nothing.
    if dx.abs() < 1e-3 && dy.abs() < 1e-3 {
        return;
    }
    c.translate(dx, dy);
    if c.rotation == 0.0 && c.w >= 0.0 && c.h >= 0.0 {
        (c.x, c.y) = (x, y);
    }
}

/// Give an item its size on the axes that fill (`None`: as it is), and lay out what it holds.
fn resize(c: &mut Node, w: Option<f32>, h: Option<f32>, lifted: &[u64]) {
    let to = (w.unwrap_or(c.w), h.unwrap_or(c.h));
    match c.kind {
        Kind::Frame => {
            if to != (c.w, c.h) {
                set_box_with(c, (c.x, c.y, to.0, to.1), Children::Constrain, lifted);
                if !c.is_auto() {
                    c.children.iter_mut().for_each(|cc| settle_with(cc, lifted));
                }
            } else {
                settle_with(c, lifted);
            }
        }
        // Its height is what the renderer measures for the new width.
        Kind::Text => {
            c.w = to.0;
            if c.fixed_h {
                c.h = to.1;
            }
        }
        _ => (c.w, c.h) = to,
    }
}

/// A node that is not an item of an auto layout, or not a frame with one, has no use for
/// the sizes that belong to those.
fn normalize(n: &mut Node, item: bool) {
    for horizontal in [true, false] {
        let s = n.sizing(horizontal);
        if (s == Sizing::Fill && !item) || (s == Sizing::Hug && !n.is_auto()) {
            n.set_sizing(horizontal, Sizing::Fixed);
        }
    }
    if !item {
        n.absolute = false;
    }
}

/// Lay out every auto-layout frame at or below `n`.
pub fn settle(n: &mut Node) {
    settle_with(n, &[]);
}

fn settle_with(n: &mut Node, lifted: &[u64]) {
    if n.is_auto() {
        return arrange_with(n, lifted);
    }
    for c in &mut n.children {
        normalize(c, false);
        settle_with(c, lifted);
    }
}

/// The rows of a layout: indices into `sizes` (the box of each item, as (along, across)).
fn rows(sizes: &[(f32, f32)], wrap: bool, room: f32, gap: f32) -> Vec<Vec<usize>> {
    if !wrap {
        return vec![(0..sizes.len()).collect()];
    }
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut used = 0.0;
    for (i, s) in sizes.iter().enumerate() {
        match out.last_mut() {
            Some(row) if used + gap + s.0 <= room + 1e-3 => {
                row.push(i);
                used += gap + s.0;
            }
            _ => {
                out.push(vec![i]);
                used = s.0;
            }
        }
    }
    out
}

/// What `arrange` works with, read off a frame.
struct Flow {
    horizontal: bool,
    wrap: bool,
    hug_main: bool,
    hug_cross: bool,
    /// Padding before and after the items, along and across the direction.
    pad_main: (f32, f32),
    pad_cross: (f32, f32),
    gap: f32,
    row_gap: f32,
    align: Align,
    cross: Align,
}

impl Flow {
    fn of(f: &Node) -> Option<Flow> {
        let l = f.layout.as_ref().filter(|_| f.kind == Kind::Frame)?;
        let horizontal = l.dir == Dir::Horizontal;
        let hug_main = f.hugs(horizontal);
        let (h, v) = ((l.pad[0], l.pad[2]), (l.pad[1], l.pad[3]));
        Some(Flow {
            horizontal,
            // Rows need a width to break at.
            wrap: l.wrap && horizontal && !hug_main,
            hug_main,
            hug_cross: f.hugs(!horizontal),
            pad_main: if horizontal { h } else { v },
            pad_cross: if horizontal { v } else { h },
            gap: l.gap,
            row_gap: l.row_gap,
            align: l.align,
            cross: l.cross,
        })
    }

    /// The room for the items, along and across the direction.
    fn inner(&self, f: &Node) -> (f32, f32) {
        let (main, cross) = if self.horizontal { (f.w, f.h) } else { (f.h, f.w) };
        ((main - self.pad_main.0 - self.pad_main.1).max(0.0), (cross - self.pad_cross.0 - self.pad_cross.1).max(0.0))
    }

    /// The gap that the items are at least apart: with space between, whatever is left.
    fn min_gap(&self) -> f32 {
        if self.align == Align::Between && !self.hug_main {
            0.0
        } else {
            self.gap
        }
    }

    fn sizes(&self, f: &Node, items: &[usize]) -> Vec<(f32, f32)> {
        items.iter().map(|i| f.children[*i].aabb()).map(|b| (span(b, self.horizontal).1, span(b, !self.horizontal).1)).collect()
    }

    /// The size the frame hugs its items with, along and across (`None`: nothing to hug).
    fn hug(&self, sizes: &[(f32, f32)], room: f32) -> Option<(f32, f32)> {
        if sizes.is_empty() {
            return None;
        }
        let rows = rows(sizes, self.wrap, room, self.gap);
        let main = sizes.iter().map(|s| s.0).sum::<f32>() + self.gap * (sizes.len() - 1) as f32;
        let cross = rows.iter().map(|r| r.iter().map(|i| sizes[*i].1).fold(0.0, f32::max)).sum::<f32>() + self.row_gap * (rows.len() - 1) as f32;
        Some((main + self.pad_main.0 + self.pad_main.1, cross + self.pad_cross.0 + self.pad_cross.1))
    }

    /// Where each item goes: the corner of its box, in world coordinates.
    fn slots(&self, f: &Node, sizes: &[(f32, f32)]) -> Vec<(f32, f32)> {
        let (room, across) = self.inner(f);
        let mut out = vec![(0.0, 0.0); sizes.len()];
        if sizes.is_empty() {
            return out;
        }
        let mut top = 0.0;
        for row in rows(sizes, self.wrap, room, self.gap) {
            let n = row.len();
            let total: f32 = row.iter().map(|i| sizes[*i].0).sum();
            let spare = room - total - self.gap * (n - 1) as f32;
            let (mut at, step) = match self.align {
                Align::Between if n > 1 && !self.hug_main => (0.0, ((room - total) / (n - 1) as f32).max(0.0)),
                Align::Center => (spare * 0.5, self.gap),
                Align::End => (spare, self.gap),
                _ => (0.0, self.gap),
            };
            // Rows stack from the top; a single row has the whole frame to align in.
            let height = if self.wrap { row.iter().map(|i| sizes[*i].1).fold(0.0, f32::max) } else { across };
            for i in row {
                let off = match self.cross {
                    Align::Center => (height - sizes[i].1) * 0.5,
                    Align::End => height - sizes[i].1,
                    _ => 0.0,
                };
                let (m, c) = (self.pad_main.0 + at, self.pad_cross.0 + top + off);
                out[i] = if self.horizontal { (f.x + m, f.y + c) } else { (f.x + c, f.y + m) };
                at += sizes[i].0 + step;
            }
            top += height + self.row_gap;
        }
        out
    }
}

/// Lay out an auto-layout frame: its box is given, except on the axes it hugs. Hidden and
/// absolute children are left alone; a turned child is placed by the box around it. Items
/// that fill share the room left along the direction equally, and take all of it across.
/// Frames among the items are laid out in turn, with the size they get here.
pub fn arrange(f: &mut Node) {
    arrange_with(f, &[]);
}

/// `lifted`: items that are being dragged. They keep their room in the row, but not their place.
fn arrange_with(f: &mut Node, lifted: &[u64]) {
    let Some(flow) = Flow::of(f) else { return };
    let h = flow.horizontal;
    let mut items = Vec::new();
    for (i, c) in f.children.iter_mut().enumerate() {
        normalize(c, true);
        if c.in_flow() {
            items.push(i);
        } else {
            settle_with(c, lifted);
        }
    }
    let (room, across) = flow.inner(f);
    // A frame that hugs has no room to give, and neither have wrapping rows.
    let fill_main = |c: &Node| !flow.hug_main && !flow.wrap && c.fills(h);
    let fill_cross = |c: &Node| !flow.hug_cross && !flow.wrap && c.fills(!h);
    let sized = |c: &mut Node, main: Option<f32>, cross: Option<f32>| {
        let (w, h) = if h { (main, cross) } else { (cross, main) };
        resize(c, w, h, lifted);
    };
    // The items with a size of their own first: what they leave is for the ones that fill.
    let mut filling = Vec::new();
    for &i in &items {
        let c = &mut f.children[i];
        let cross = fill_cross(c).then_some(across.max(1.0));
        if fill_main(c) {
            filling.push((i, cross));
        } else {
            sized(c, None, cross);
        }
    }
    if !filling.is_empty() {
        let taken: f32 = items.iter().filter(|i| !filling.iter().any(|f| f.0 == **i)).map(|i| span(f.children[*i].aabb(), h).1).sum();
        let each = ((room - taken - flow.min_gap() * (items.len() - 1) as f32) / filling.len() as f32).max(1.0);
        for (i, cross) in filling {
            sized(&mut f.children[i], Some(each), cross);
        }
    }
    let sizes = flow.sizes(f, &items);
    if let Some((main, cross)) = flow.hug(&sizes, room) {
        let (main, cross) = (main.max(1.0), cross.max(1.0));
        if flow.hug_main {
            *(if h { &mut f.w } else { &mut f.h }) = main;
        }
        if flow.hug_cross {
            *(if h { &mut f.h } else { &mut f.w }) = cross;
        }
    }
    for (i, (x, y)) in items.iter().zip(flow.slots(f, &sizes)) {
        let c = &mut f.children[*i];
        if !lifted.contains(&c.id) {
            move_to(c, x, y);
        }
    }
}

/// The room that item `id` of auto-layout frame `f` has in it, wherever the item itself is.
pub fn slot_of(f: &Node, id: u64) -> Option<Rect> {
    let flow = Flow::of(f)?;
    let items: Vec<usize> = (0..f.children.len()).filter(|i| f.children[*i].in_flow()).collect();
    let at = items.iter().position(|i| f.children[*i].id == id)?;
    let sizes = flow.sizes(f, &items);
    let (x, y) = flow.slots(f, &sizes)[at];
    let (w, h) = if flow.horizontal { sizes[at] } else { (sizes[at].1, sizes[at].0) };
    Some((x, y, w, h))
}

/// Where in the children of auto-layout frame `f` something dropped at (x, y) goes: its index
/// among the children that are not in `skip` (the ones being dragged). It comes after every
/// item that the point has passed the middle of.
pub fn insert_index(f: &Node, skip: &[u64], x: f32, y: f32) -> usize {
    let rest = f.children.iter().filter(|c| !skip.contains(&c.id));
    let Some(l) = &f.layout else { return rest.count() };
    let horizontal = l.dir == Dir::Horizontal;
    let mut at = 0;
    for (k, c) in rest.enumerate().filter(|(_, c)| c.in_flow()) {
        let b = c.aabb();
        let passed = match (horizontal, l.wrap) {
            // Rows read like text: everything in a row above, then what is to the left.
            (true, true) => y > b.3 || (y >= b.1 && x > (b.0 + b.2) * 0.5),
            (true, false) => x > (b.0 + b.2) * 0.5,
            (false, _) => y > (b.1 + b.3) * 0.5,
        };
        if passed {
            at = k + 1;
        }
    }
    at
}

/// An auto-layout frame was mirrored left-right (`horizontal`) or top-bottom: the layout that
/// keeps its items where the mirror put them. Along the direction they run the other way.
///
/// A layout that wraps is left as it is, and its items go back to reading order (what each
/// shows stays mirrored): which items share a row depends on the order they come in, and
/// rows always stack from the top, so the mirror image of wrapped rows is not something an
/// order and an alignment can say.
pub fn flip_layout(f: &mut Node, horizontal: bool) {
    if Flow::of(f).is_some_and(|flow| flow.wrap) {
        return;
    }
    let Some(l) = f.layout.as_mut().filter(|_| f.kind == Kind::Frame) else { return };
    let turn = |a: Align| match a {
        Align::Start => Align::End,
        Align::End => Align::Start,
        other => other,
    };
    if horizontal {
        l.pad.swap(0, 2);
    } else {
        l.pad.swap(1, 3);
    }
    if (l.dir == Dir::Horizontal) != horizontal {
        l.cross = turn(l.cross);
        return;
    }
    l.align = turn(l.align);
    reverse_items(f, horizontal);
}

/// The items of auto-layout frame `f` in the opposite order, if they run along that axis
/// (`horizontal`: left-right) and the layout is one that `flip_layout` mirrors. On its own
/// this is for a frame that was mirrored before it was given its children.
pub fn reverse_items(f: &mut Node, horizontal: bool) {
    if !Flow::of(f).is_some_and(|flow| !flow.wrap && flow.horizontal == horizontal) {
        return;
    }
    let items: Vec<usize> = (0..f.children.len()).filter(|i| f.children[*i].in_flow()).collect();
    for k in 0..items.len() / 2 {
        f.children.swap(items[k], items[items.len() - 1 - k]);
    }
}

/// What `infer` reads off an arrangement of boxes.
#[derive(Clone, PartialEq, Debug)]
pub struct Inferred {
    pub dir: Dir,
    pub gap: f32,
    /// The boxes in the order they come along the direction.
    pub order: Vec<usize>,
    pub cross: Align,
}

/// Read a layout off boxes (x0, y0, x1, y1) as they are placed: a row or a column (whichever way
/// their centres are spread further), the usual gap between neighbours, and how they line up.
pub fn infer(boxes: &[Rect]) -> Inferred {
    let spread = |horizontal: bool| {
        let mids = boxes.iter().map(|b| span(*b, horizontal)).map(|(s, l)| s + l * 0.5);
        mids.clone().fold(f32::MIN, f32::max) - mids.fold(f32::MAX, f32::min)
    };
    let horizontal = boxes.len() < 2 || spread(true) >= spread(false);
    let mut order: Vec<usize> = (0..boxes.len()).collect();
    order.sort_by(|a, b| span(boxes[*a], horizontal).0.total_cmp(&span(boxes[*b], horizontal).0));
    let gaps: Vec<f32> = order.windows(2).map(|w| span(boxes[w[1]], horizontal).0 - span(boxes[w[0]], horizontal).0 - span(boxes[w[0]], horizontal).1).collect();
    let gap = if gaps.is_empty() { 10.0 } else { (gaps.iter().sum::<f32>() / gaps.len() as f32).round().max(0.0) };
    let lined = |at: &dyn Fn((f32, f32)) -> f32| boxes.windows(2).all(|w| (at(span(w[0], !horizontal)) - at(span(w[1], !horizontal))).abs() < 0.5);
    let cross = if lined(&|s| s.0) {
        Align::Start
    } else if lined(&|s| s.0 + s.1 * 0.5) {
        Align::Center
    } else if lined(&|s| s.0 + s.1) {
        Align::End
    } else {
        Align::Start
    };
    Inferred { dir: if horizontal { Dir::Horizontal } else { Dir::Vertical }, gap, order, cross }
}

/// Far more rounds than `Doc::settle` needs to come to rest; the cap only matters if layouts
/// and components should ever keep contradicting each other.
const SETTLE_ROUNDS: usize = 8;

/// Lay out one top-level node, if it holds anything to lay out. Written back only when it came
/// out different, so an untouched node stays shared with the undo snapshots.
fn settle_top(top: &mut Arc<Node>, lifted: &[u64]) -> bool {
    if !top.wants_layout() {
        return false;
    }
    let mut n = (**top).clone();
    normalize(&mut n, false);
    settle_with(&mut n, lifted);
    let changed = n != **top;
    if changed {
        *top = Arc::new(n);
    }
    changed
}

impl Doc {
    /// Lay out the auto-layout frames in every top-level node (on any page) that is not shared
    /// with `pre`, the document as it was when this last ran; `None` looks at all of them.
    /// `lifted`: items being dragged (see `arrange_with`). Result: whether anything changed.
    pub fn settle_layout(&mut self, pre: Option<&Doc>, lifted: &[u64]) -> bool {
        // Mostly a node is where it was in the list; only one that is not is looked up.
        let mut was: Option<std::collections::HashSet<*const Node>> = None;
        let mut shared = |top: &Arc<Node>, at: Option<&Arc<Node>>| {
            let Some(pre) = pre else { return false };
            at.is_some_and(|p| Arc::ptr_eq(p, top)) || was.get_or_insert_with(|| pre.all_tops().map(|n| n as *const Node).collect()).contains(&(&**top as *const Node))
        };
        let mut changed = false;
        for (i, top) in self.nodes.iter_mut().enumerate() {
            if !shared(top, pre.and_then(|d| d.nodes.get(i))) {
                changed |= settle_top(top, lifted);
            }
        }
        for (p, page) in self.pages.iter_mut().enumerate() {
            for (i, top) in page.nodes.iter_mut().enumerate() {
                if !shared(top, pre.and_then(|d| d.pages.get(p)).and_then(|d| d.nodes.get(i))) {
                    changed |= settle_top(top, lifted);
                }
            }
        }
        changed
    }

    /// Bring the document to rest after an edit; `pre` is the document as it was when this
    /// last ran (`None`: just opened, there is no edit to follow). The auto layouts are laid
    /// out, then instances follow their mains (`Doc::sync_components`), and again, until
    /// neither changes anything: a main that is laid out again changes its instances, and an
    /// instance that follows changes the layout of the frame it sits in.
    /// Result: whether any instance was brought in line.
    pub fn settle(&mut self, pre: Option<&Doc>, lifted: &[u64]) -> bool {
        let mut any = false;
        for round in 0..SETTLE_ROUNDS {
            // After the first round the components follow what the layout did since they last ran.
            let followed = (round > 0).then(|| self.clone());
            // Everything that changed since `pre` is laid out in every round: that includes
            // whatever the components changed in the round before.
            let laid = self.settle_layout(pre, lifted);
            let synced = followed.as_ref().or(pre).is_some_and(|base| self.sync_components(base));
            any |= synced;
            if !laid && !synced {
                break;
            }
        }
        any
    }

    /// `settle_layout` for the top-level nodes of this page that hold one of `ids`: what a drag
    /// or the text being typed can have changed.
    ///
    /// This runs several times per frame while something is dragged or typed (after the
    /// pointer was handled, and again at the end of the frame), and laying a top-level node
    /// out means copying it and comparing the copy with it. A node that was laid out by the
    /// last call and has not changed since is left alone (see `Doc::settled`).
    pub fn settle_around(&mut self, ids: &[u64], lifted: &[u64]) -> bool {
        let mut tops: Vec<usize> = ids.iter().filter_map(|id| self.find_top(*id).map(|found| found.0)).collect();
        tops.sort_unstable();
        tops.dedup();
        let mut changed = false;
        for i in tops {
            if self.settled(&self.nodes[i], lifted, false) {
                continue;
            }
            changed |= settle_top(&mut self.nodes[i], lifted);
            self.settled(&self.nodes[i], lifted, true);
        }
        changed
    }

    /// Is this node an item of an auto-layout frame (placed by it, so not movable by hand)?
    pub fn auto_item(&self, id: u64) -> bool {
        let line = self.lineage(id);
        match line[..] {
            [.., parent, n] => parent.is_auto() && !n.floats(),
            _ => false,
        }
    }

    /// Put `node` into frame `parent` (or the top level) as child number `at`, counted from the bottom.
    pub fn insert_at(&mut self, node: Node, parent: Option<u64>, at: usize) {
        match parent.and_then(|p| self.find_mut(p)).filter(|f| f.kind == Kind::Frame) {
            Some(f) => f.children.insert(at.min(f.children.len()), node),
            None => self.nodes.insert(at.min(self.nodes.len()), Arc::new(node)),
        }
    }

    /// Put a new node into frame `parent` (or the top level): on top, or, where the frame has
    /// auto layout and the node is an item of it, among the items at the place the node is
    /// at, as a layer dropped there would land.
    pub fn insert_placed(&mut self, node: Node, parent: Option<u64>) {
        let (cx, cy) = node.center();
        let at = parent.and_then(|p| self.find(p)).filter(|f| f.is_auto() && node.in_flow()).map(|f| insert_index(f, &[], cx, cy));
        match at {
            Some(at) => self.insert_at(node, parent, at),
            None => self.insert(node, parent),
        }
    }

    /// Shift+A. A single frame without auto layout gets one, read off how its children are
    /// arranged (see `infer`; the padding is the room around them), and its children are put
    /// in the order they come along the direction. Anything else is wrapped in a new frame
    /// that hugs it, in the same parent and at the same place in the stack. Result: the frame.
    pub fn add_auto_layout(&mut self, ids: &[u64]) -> Result<u64, &'static str> {
        let mut roots = self.roots_of(ids);
        self.sort_by_stack(&mut roots);
        let Some(&top) = roots.last() else { return Err("Select a frame or some layers to add auto layout to.") };
        if roots.iter().any(|id| self.pinned(*id)) {
            return Err("Layers inside an instance follow the main component: add auto layout there, or detach the instance first.");
        }
        let plain = |n: &Node| n.kind == Kind::Frame && n.layout.is_none() && n.instance_of.is_none();
        let id = match roots[..] {
            [id] if self.find(id).is_some_and(plain) => id,
            _ => {
                let parent = self.parent_of(top);
                if roots.iter().any(|id| self.parent_of(*id) != parent) {
                    return Err("Select layers from the same frame to add auto layout around them.");
                }
                let b = roots.iter().filter_map(|id| self.find(*id).map(|n| n.aabb())).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))).ok_or("Select a frame or some layers to add auto layout to.")?;
                let id = self.alloc_id();
                let mut frame = Node::new(id, Kind::Frame, b.0, b.1);
                (frame.w, frame.h) = ((b.2 - b.0).max(1.0), (b.3 - b.1).max(1.0));
                frame.name = format!("Frame {}", self.frame_count() + 1);
                // Right above the front-most layer; the layers then move in from below it.
                let at = self.child_ids(parent).iter().position(|c| *c == top).map_or(0, |i| i + 1);
                self.insert_at(frame, parent, at);
                self.move_nodes(&roots, Some(id), None, 0);
                id
            }
        };
        let f = self.find_mut(id).ok_or("Select a frame or some layers to add auto layout to.")?;
        // (A text on a path is no item: it is not read, and stays where its path has it.)
        let shown: Vec<usize> = (0..f.children.len()).filter(|i| f.children[*i].visible && !f.children[*i].floats()).collect();
        let boxes: Vec<Rect> = shown.iter().map(|i| f.children[*i].aabb()).collect();
        let read = infer(&boxes);
        let mut l = AutoLayout { dir: read.dir, gap: read.gap, cross: read.cross, ..Default::default() };
        if let Some(b) = boxes.iter().copied().reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))) {
            l.pad = [b.0 - f.x, b.1 - f.y, f.x + f.w - b.2, f.y + f.h - b.3].map(|p| p.max(0.0));
            (f.size_h, f.size_v) = (Sizing::Hug, Sizing::Hug);
        }
        // The visible children in the order they are read; the others stay where they are in the stack.
        let mut slots: Vec<Option<Node>> = std::mem::take(&mut f.children).into_iter().map(Some).collect();
        let mut next = read.order.iter().map(|k| shown[*k]);
        for i in 0..slots.len() {
            let from = if shown.contains(&i) { next.next().unwrap_or(i) } else { i };
            let mut c = slots[from].take().expect("each child is placed once");
            c.absolute = false;
            f.children.push(c);
        }
        f.layout = Some(l);
        arrange(f);
        Ok(id)
    }

    /// Alt+Shift+A: a plain frame again. Its children stay where they are.
    pub fn remove_auto_layout(&mut self, id: u64) -> bool {
        let Some(f) = self.find_mut(id).filter(|f| f.is_auto() && f.instance_of.is_none() && f.source.is_none()) else { return false };
        f.layout = None;
        normalize(f, true);
        f.children.iter_mut().for_each(|c| normalize(c, false));
        true
    }

    /// Arrow keys on an item of an auto layout: it trades places with the item before it
    /// (`forward` false) or after it. Result: whether it moved.
    pub fn swap_item(&mut self, id: u64, forward: bool) -> bool {
        let Some(f) = self.parent_of(id).and_then(|p| self.find(p)) else { return false };
        let items: Vec<usize> = (0..f.children.len()).filter(|i| f.children[*i].in_flow()).collect();
        let Some(at) = items.iter().position(|i| f.children[*i].id == id) else { return false };
        let other = if forward { items.get(at + 1) } else { at.checked_sub(1).and_then(|i| items.get(i)) };
        let (Some(&b), a, parent) = (other, items[at], f.id) else { return false };
        self.find_mut(parent).is_some_and(|f| {
            f.children.swap(a, b);
            true
        })
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

    // ------------------------------------------------------------ auto layout

    const PAD: [f32; 4] = [20.0, 30.0, 40.0, 50.0];

    fn item(id: u64, w: f32, h: f32) -> Node {
        node(id, Kind::Rect, (0.0, 0.0, w, h), (Min, Min))
    }

    fn row(gap: f32, pad: [f32; 4]) -> AutoLayout {
        AutoLayout { gap, pad, ..Default::default() }
    }

    fn column(gap: f32, pad: [f32; 4]) -> AutoLayout {
        AutoLayout { dir: Dir::Vertical, ..row(gap, pad) }
    }

    /// An auto-layout frame at (100, 100), laid out.
    fn flow(id: u64, (w, h): (f32, f32), sizes: (Sizing, Sizing), l: AutoLayout, children: Vec<Node>) -> Node {
        let mut f = node(id, Kind::Frame, (100.0, 100.0, w, h), (Min, Min));
        (f.layout, f.children, f.size_h, f.size_v) = (Some(l), children, sizes.0, sizes.1);
        arrange(&mut f);
        f
    }

    /// The boxes of the children, from the corner of the frame.
    fn laid(f: &Node) -> Vec<Rect> {
        f.children.iter().map(|c| (c.x - f.x, c.y - f.y, c.w, c.h)).collect()
    }

    fn three() -> Vec<Node> {
        vec![item(2, 60.0, 40.0), item(3, 80.0, 20.0), item(4, 30.0, 30.0)]
    }

    const FIXED: (Sizing, Sizing) = (Sizing::Fixed, Sizing::Fixed);
    const HUG: (Sizing, Sizing) = (Sizing::Hug, Sizing::Hug);

    #[test]
    fn a_row_and_a_column_with_padding_and_gap() {
        let f = flow(1, (400.0, 200.0), FIXED, row(10.0, PAD), three());
        assert_eq!(laid(&f), [(20.0, 30.0, 60.0, 40.0), (90.0, 30.0, 80.0, 20.0), (180.0, 30.0, 30.0, 30.0)]);
        assert_eq!(rect_of(&f), (100.0, 100.0, 400.0, 200.0));
        let f = flow(1, (400.0, 200.0), FIXED, column(10.0, PAD), three());
        assert_eq!(laid(&f), [(20.0, 30.0, 60.0, 40.0), (20.0, 80.0, 80.0, 20.0), (20.0, 110.0, 30.0, 30.0)]);
        // Laying out again changes nothing, to the last bit.
        let mut again = f.clone();
        arrange(&mut again);
        assert_eq!(again, f);
        // The order of the children is the order of the items.
        let mut f = f;
        f.children.swap(0, 2);
        arrange(&mut f);
        assert_eq!(laid(&f), [(20.0, 30.0, 30.0, 30.0), (20.0, 70.0, 80.0, 20.0), (20.0, 100.0, 60.0, 40.0)]);
    }

    #[test]
    fn items_align_along_and_across_the_direction() {
        // Room for the items: 340 × 120; they take 190 of the 340.
        let xs = |align: Align| laid(&flow(1, (400.0, 200.0), FIXED, AutoLayout { align, ..row(10.0, PAD) }, three())).iter().map(|b| b.0).collect::<Vec<_>>();
        assert_eq!(xs(Align::Start), [20.0, 90.0, 180.0]);
        assert_eq!(xs(Align::Center), [95.0, 165.0, 255.0]);
        assert_eq!(xs(Align::End), [170.0, 240.0, 330.0]);
        // Space between: the gap is whatever is left, (340 - 170) / 2.
        assert_eq!(xs(Align::Between), [20.0, 165.0, 330.0]);
        let ys = |cross: Align| laid(&flow(1, (400.0, 200.0), FIXED, AutoLayout { cross, ..row(10.0, PAD) }, three())).iter().map(|b| b.1).collect::<Vec<_>>();
        assert_eq!(ys(Align::Start), [30.0, 30.0, 30.0]);
        assert_eq!(ys(Align::Center), [70.0, 80.0, 75.0]);
        assert_eq!(ys(Align::End), [110.0, 130.0, 120.0]);
        // In a column the two trade places.
        let f = flow(1, (400.0, 200.0), FIXED, AutoLayout { align: Align::End, cross: Align::Center, ..column(10.0, PAD) }, three());
        assert_eq!(laid(&f).iter().map(|b| (b.0, b.1)).collect::<Vec<_>>(), [(160.0, 40.0), (150.0, 90.0), (175.0, 120.0)]);
        // One item has nothing to be spaced from: it starts at the start. Items that don't fit touch.
        let one = flow(1, (400.0, 200.0), FIXED, AutoLayout { align: Align::Between, ..row(10.0, PAD) }, vec![item(2, 60.0, 40.0)]);
        assert_eq!(laid(&one)[0].0, 20.0);
        let tight = flow(1, (200.0, 200.0), FIXED, AutoLayout { align: Align::Between, ..row(10.0, PAD) }, three());
        assert_eq!(laid(&tight).iter().map(|b| b.0).collect::<Vec<_>>(), [20.0, 80.0, 160.0]);
    }

    #[test]
    fn items_that_fill_share_the_room_that_is_left() {
        let fill = |id: u64, h: bool, v: bool| {
            let mut n = item(id, 10.0, 40.0);
            (n.size_h, n.size_v) = (if h { Sizing::Fill } else { Sizing::Fixed }, if v { Sizing::Fill } else { Sizing::Fixed });
            n
        };
        // 340 wide inside, 60 and two gaps taken: 130 each. Across, a filling item takes all 120.
        let f = flow(1, (400.0, 200.0), FIXED, row(10.0, PAD), vec![item(2, 60.0, 40.0), fill(3, true, false), fill(4, true, true)]);
        assert_eq!(laid(&f), [(20.0, 30.0, 60.0, 40.0), (90.0, 30.0, 130.0, 40.0), (230.0, 30.0, 130.0, 120.0)]);
        // In a column the height is shared and the width is taken.
        let f = flow(1, (400.0, 200.0), FIXED, column(10.0, PAD), vec![item(2, 60.0, 40.0), fill(3, true, true)]);
        assert_eq!(laid(&f), [(20.0, 30.0, 60.0, 40.0), (20.0, 80.0, 340.0, 70.0)]);
        // No room left: never thinner than 1.
        let f = flow(1, (70.0, 200.0), FIXED, row(10.0, PAD), vec![item(2, 60.0, 40.0), fill(3, true, false)]);
        assert_eq!(laid(&f)[1], (90.0, 30.0, 1.0, 40.0));
        // With space between there is no gap to keep when something fills.
        let f = flow(1, (400.0, 200.0), FIXED, AutoLayout { align: Align::Between, ..row(10.0, PAD) }, vec![item(2, 60.0, 40.0), fill(3, true, false)]);
        assert_eq!(laid(&f)[1], (80.0, 30.0, 280.0, 40.0));
        // A paragraph takes the width (its height is its content's); auto-width text, a line and a
        // turned item keep the size they have.
        let mut para = node(2, Kind::Text, (0.0, 0.0, 50.0, 18.0), (Min, Min));
        (para.wrap, para.size_h, para.size_v) = (true, Sizing::Fill, Sizing::Fill);
        let mut label = node(3, Kind::Text, (0.0, 0.0, 50.0, 18.0), (Min, Min));
        label.size_h = Sizing::Fill;
        let mut line = node(4, Kind::Line, (0.0, 0.0, 30.0, 0.0), (Min, Min));
        line.size_h = Sizing::Fill;
        let mut turned = fill(5, true, false);
        turned.rotation = 180.0;
        let f = flow(1, (400.0, 200.0), FIXED, column(10.0, PAD), vec![para, label, line, turned]);
        assert_eq!(f.children.iter().map(|c| (c.w, c.h)).collect::<Vec<_>>(), [(340.0, 18.0), (50.0, 18.0), (30.0, 0.0), (10.0, 40.0)]);
    }

    #[test]
    fn a_frame_hugs_its_items_and_padding() {
        let f = flow(1, (999.0, 999.0), HUG, row(10.0, PAD), three());
        assert_eq!((rect_of(&f), laid(&f)[2]), ((100.0, 100.0, 250.0, 120.0), (180.0, 30.0, 30.0, 30.0)));
        let f = flow(1, (999.0, 999.0), HUG, column(10.0, PAD), three());
        assert_eq!((rect_of(&f), laid(&f)[2]), ((100.0, 100.0, 140.0, 190.0), (20.0, 110.0, 30.0, 30.0)));
        // Each side on its own.
        let f = flow(1, (400.0, 999.0), (Sizing::Fixed, Sizing::Hug), AutoLayout { align: Align::End, ..row(10.0, PAD) }, three());
        assert_eq!((rect_of(&f), laid(&f)[0]), ((100.0, 100.0, 400.0, 120.0), (170.0, 30.0, 60.0, 40.0)));
        // A frame that hugs has no room to give: an item that would fill keeps its size. The
        // gap is the gap, also when the items are told to be spaced out.
        let mut wide = item(2, 60.0, 40.0);
        (wide.size_h, wide.size_v) = (Sizing::Fill, Sizing::Fill);
        let f = flow(1, (999.0, 999.0), HUG, AutoLayout { align: Align::Between, ..row(10.0, PAD) }, vec![wide, item(3, 80.0, 20.0)]);
        assert_eq!((rect_of(&f), laid(&f)), ((100.0, 100.0, 210.0, 120.0), vec![(20.0, 30.0, 60.0, 40.0), (90.0, 30.0, 80.0, 20.0)]));
        // Nothing to hug: the size stays.
        let f = flow(1, (300.0, 80.0), HUG, row(10.0, PAD), vec![]);
        assert_eq!(rect_of(&f), (100.0, 100.0, 300.0, 80.0));
    }

    #[test]
    fn rows_wrap_where_the_width_ends() {
        let chips = || vec![item(2, 50.0, 20.0), item(3, 50.0, 30.0), item(4, 50.0, 20.0), item(5, 50.0, 20.0), item(6, 50.0, 20.0)];
        let wrap = |cross: Align| AutoLayout { wrap: true, row_gap: 5.0, cross, ..row(10.0, [10.0; 4]) };
        // 180 wide inside: three fit (170), the fourth starts a row. The first row is as high as its highest item.
        let f = flow(1, (200.0, 999.0), (Sizing::Fixed, Sizing::Hug), wrap(Align::Start), chips());
        assert_eq!(laid(&f).iter().map(|b| (b.0, b.1)).collect::<Vec<_>>(), [(10.0, 10.0), (70.0, 10.0), (130.0, 10.0), (10.0, 45.0), (70.0, 45.0)]);
        assert_eq!(rect_of(&f), (100.0, 100.0, 200.0, 75.0));
        // Items line up within their row; each row is aligned along the direction on its own.
        let f = flow(1, (200.0, 999.0), (Sizing::Fixed, Sizing::Hug), AutoLayout { align: Align::End, ..wrap(Align::Center) }, chips());
        assert_eq!(laid(&f).iter().map(|b| (b.0, b.1)).collect::<Vec<_>>(), [(20.0, 15.0), (80.0, 10.0), (140.0, 15.0), (80.0, 45.0), (140.0, 45.0)]);
        // Exactly fitting still fits; an item wider than the frame has a row to itself.
        let f = flow(1, (190.0, 200.0), FIXED, wrap(Align::Start), vec![item(2, 50.0, 20.0), item(3, 50.0, 20.0), item(4, 50.0, 20.0), item(5, 500.0, 20.0), item(6, 5.0, 20.0)]);
        assert_eq!(laid(&f).iter().map(|b| (b.0, b.1)).collect::<Vec<_>>(), [(10.0, 10.0), (70.0, 10.0), (130.0, 10.0), (10.0, 35.0), (10.0, 60.0)]);
        // Rows need a width to break at: a frame that hugs its width is one row, and so is a column.
        let f = flow(1, (200.0, 999.0), HUG, wrap(Align::Start), chips());
        assert_eq!(rect_of(&f), (100.0, 100.0, 310.0, 50.0));
        let f = flow(1, (200.0, 999.0), (Sizing::Fixed, Sizing::Hug), AutoLayout { dir: Dir::Vertical, ..wrap(Align::Start) }, chips());
        assert_eq!((f.h, laid(&f)[4]), (170.0, (10.0, 140.0, 50.0, 20.0)));
    }

    #[test]
    fn nested_layouts_hug_upward_and_fill_downward() {
        // A column of fixed width that hugs its height, holding a title and a row that fills the
        // width and hugs its height. In the row: a box, a chip that hugs, and a box that fills.
        let mut chip = node(6, Kind::Frame, (0.0, 0.0, 99.0, 99.0), (Min, Min));
        (chip.layout, chip.size_h, chip.size_v, chip.children) = (Some(row(0.0, [3.0; 4])), Sizing::Hug, Sizing::Hug, vec![item(7, 20.0, 10.0)]);
        let mut wide = item(5, 1.0, 50.0);
        wide.size_h = Sizing::Fill;
        let mut inner = node(3, Kind::Frame, (0.0, 0.0, 99.0, 99.0), (Min, Min));
        (inner.layout, inner.size_h, inner.size_v) = (Some(row(10.0, [5.0; 4])), Sizing::Fill, Sizing::Hug);
        inner.children = vec![item(4, 40.0, 30.0), chip, wide];
        let mut f = flow(1, (300.0, 999.0), (Sizing::Fixed, Sizing::Hug), column(10.0, [10.0; 4]), vec![item(2, 100.0, 20.0), inner]);
        let at = |f: &Node, id: u64| f.find(id).map(|n| (n.x - f.x, n.y - f.y, n.w, n.h)).unwrap();
        // The row: 280 wide, 60 high (the highest item and its padding). The chip hugs: 26 × 16.
        assert_eq!((at(&f, 3), at(&f, 6), at(&f, 7)), ((10.0, 40.0, 280.0, 60.0), (65.0, 45.0, 26.0, 16.0), (68.0, 48.0, 20.0, 10.0)));
        // What fills gets the rest: 270 - 40 - 26 - two gaps.
        assert_eq!((at(&f, 4), at(&f, 5)), ((15.0, 45.0, 40.0, 30.0), (101.0, 45.0, 184.0, 50.0)));
        assert_eq!(rect_of(&f), (100.0, 100.0, 300.0, 110.0));
        // The column made wider: the row fills it, and its filling box takes what that adds.
        set_box(&mut f, (50.0, 60.0, 400.0, 110.0), Children::Constrain);
        assert_eq!((at(&f, 3), at(&f, 5), rect_of(&f)), ((10.0, 40.0, 380.0, 60.0), (101.0, 45.0, 284.0, 50.0), (50.0, 60.0, 400.0, 110.0)));
        // Something in the chip grows: chip, row and column follow, and the filling box gives way.
        f.find_mut(7).unwrap().h = 80.0;
        f.find_mut(7).unwrap().w = 50.0;
        arrange(&mut f);
        assert_eq!((at(&f, 6), at(&f, 3), at(&f, 5), f.h), ((65.0, 45.0, 56.0, 86.0), (10.0, 40.0, 380.0, 96.0), (131.0, 45.0, 254.0, 50.0), 146.0));
        // A side that hugs can't be set: it snaps back. A fixed side can.
        set_box(&mut f, (50.0, 60.0, 300.0, 500.0), Children::Constrain);
        assert_eq!((rect_of(&f), at(&f, 5).2), ((50.0, 60.0, 300.0, 146.0), 154.0));
    }

    #[test]
    fn hidden_and_absolute_children_are_left_out() {
        let mut hidden = item(3, 50.0, 20.0);
        (hidden.visible, hidden.x) = (false, 777.0);
        let mut badge = node(4, Kind::Ellipse, (270.0, 90.0, 30.0, 30.0), (Max, Min));
        badge.absolute = true;
        let mut f = flow(1, (200.0, 50.0), FIXED, row(10.0, [0.0; 4]), vec![item(2, 50.0, 20.0), hidden, badge, item(5, 50.0, 20.0)]);
        assert_eq!(laid(&f), [(0.0, 0.0, 50.0, 20.0), (677.0, -100.0, 50.0, 20.0), (170.0, -10.0, 30.0, 30.0), (60.0, 0.0, 50.0, 20.0)]);
        // They don't count for the size either.
        let mut hug = f.clone();
        (hug.size_h, hug.size_v) = HUG;
        arrange(&mut hug);
        assert_eq!((hug.w, hug.h), (110.0, 20.0));
        // When the frame is resized, the absolute child follows its constraints (here: the right
        // edge) and the items are laid out; when it moves, everything moves along.
        set_box(&mut f, (100.0, 100.0, 300.0, 50.0), Children::Constrain);
        assert_eq!((laid(&f)[2], laid(&f)[3]), ((270.0, -10.0, 30.0, 30.0), (60.0, 0.0, 50.0, 20.0)));
        set_box(&mut f, (0.0, 0.0, 300.0, 50.0), Children::Constrain);
        assert_eq!((laid(&f)[2], laid(&f)[3], f.children[3].x), ((270.0, -10.0, 30.0, 30.0), (60.0, 0.0, 50.0, 20.0), 60.0));
        // Shown again, the hidden one takes its place in the row.
        f.children[1].visible = true;
        arrange(&mut f);
        assert_eq!(laid(&f).iter().map(|b| b.0).collect::<Vec<_>>(), [0.0, 60.0, 270.0, 120.0]);
        // Outside an auto layout the flags mean nothing and are dropped.
        f.layout = None;
        settle(&mut f);
        assert!(f.children.iter().all(|c| !c.absolute));
    }

    #[test]
    fn a_text_on_a_path_is_left_out_and_does_not_drift() {
        // A column that hugs: a box, a line to write on, and a text that is on that line.
        let line = node(3, Kind::Line, (0.0, 0.0, 120.0, 0.0), (Min, Min));
        let mut text = node(4, Kind::Text, (0.0, 0.0, 50.0, 12.0), (Min, Min));
        (text.on_path, text.path_offset) = (Some(3), 7.0);
        let mut doc = Doc::default();
        doc.insert(flow(1, (10.0, 10.0), HUG, column(10.0, [5.0; 4]), vec![item(2, 60.0, 40.0), line, text]), None);
        doc.next_id = 10;
        assert!(!doc.find(4).unwrap().in_flow() && !doc.auto_item(4) && doc.auto_item(3));
        // What the canvas does on every frame (`App::apply_path_box`): the box of the text is the
        // box around its glyphs on the line, and a text that is not where it was last put was
        // moved by hand, which slides it along.
        let mut seen: Option<(f32, f32)> = None;
        let mut rounds = Vec::new();
        for _ in 0..6 {
            let pre = doc.clone();
            let on = doc.find(3).map(|l| (l.x + 7.0, l.y - 12.0)).unwrap();
            let t = doc.find_mut(4).unwrap();
            if let Some(at) = seen.filter(|at| *at != (t.x, t.y)) {
                t.path_offset += t.x - at.0;
            }
            (t.x, t.y) = on;
            seen = Some(on);
            doc.settle(Some(&pre), &[]);
            let (f, t) = (doc.find(1).unwrap(), doc.find(4).unwrap());
            rounds.push(((f.w, f.h), laid(f)[2], t.path_offset));
        }
        // The frame hugs the box and the line only, and the text stays where the line has it.
        assert_eq!(rounds[0], ((130.0, 60.0), (12.0, 43.0, 50.0, 12.0), 7.0));
        assert!(rounds.iter().all(|r| *r == rounds[0]), "{rounds:?}");
        assert_eq!(seen, doc.find(4).map(|t| (t.x, t.y)));
        // Shift+A on a frame keeps a text on a path out of the row as well, and taking the
        // layout off again leaves it on its path.
        let mut plain = Doc::default();
        let mut f = frame(vec![node(2, Kind::Line, (120.0, 150.0, 100.0, 0.0), (Min, Min)), node(3, Kind::Text, (130.0, 130.0, 40.0, 12.0), (Min, Min)), node(4, Kind::Rect, (240.0, 140.0, 20.0, 20.0), (Min, Min))]);
        f.children[1].on_path = Some(2);
        plain.insert(f, None);
        plain.next_id = 10;
        let id = plain.nodes[0].id;
        assert_eq!(plain.add_auto_layout(&[id]), Ok(id));
        let f = plain.find(id).unwrap();
        assert_eq!((f.children.len(), plain.auto_item(3), plain.find(3).map(|t| (t.x, t.y, t.on_path))), (3, false, Some((130.0, 130.0, Some(2)))));
        assert!(plain.remove_auto_layout(id) && plain.find(3).unwrap().on_path == Some(2));
    }

    #[test]
    fn a_turned_item_is_placed_by_the_box_around_it() {
        // 60 × 20 turned a quarter: it takes 20 × 60.
        let mut turned = item(2, 60.0, 20.0);
        turned.rotation = 90.0;
        let f = flow(1, (999.0, 999.0), HUG, row(10.0, [10.0; 4]), vec![turned, item(3, 30.0, 30.0)]);
        let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(near(f.w, 80.0) && near(f.h, 80.0), "{:?}", rect_of(&f));
        let b = f.children[0].aabb();
        assert!(near(b.0 - f.x, 10.0) && near(b.1 - f.y, 10.0) && near(b.2 - f.x, 30.0) && near(b.3 - f.y, 70.0), "{b:?}");
        assert_eq!((f.children[0].w, f.children[0].h, f.children[0].rotation), (60.0, 20.0, 90.0));
        assert!(near(laid(&f)[1].0, 40.0) && near(laid(&f)[1].1, 10.0));
        // And it stays there: nothing creeps when it is laid out again.
        let mut again = f.clone();
        arrange(&mut again);
        assert_eq!(again, f);
    }

    #[test]
    fn a_text_that_grows_pushes_its_neighbours() {
        let label = node(2, Kind::Text, (0.0, 0.0, 40.0, 20.0), (Min, Min));
        let l = AutoLayout { cross: Align::Center, ..row(8.0, [12.0, 6.0, 12.0, 6.0]) };
        let button = flow(1, (999.0, 999.0), HUG, l, vec![label, item(3, 16.0, 16.0)]);
        assert_eq!((rect_of(&button), laid(&button)[1]), ((100.0, 100.0, 88.0, 32.0), (60.0, 8.0, 16.0, 16.0)));
        let mut doc = Doc::default();
        doc.insert(button, None);
        doc.insert(item(9, 5.0, 5.0), None);
        let pre = doc.clone();
        // What the renderer does after the text was edited: it writes the measured size back.
        (doc.find_mut(2).unwrap().w, doc.find_mut(2).unwrap().h) = (100.0, 24.0);
        assert!(doc.settle_layout(Some(&pre), &[]));
        let button = doc.find(1).unwrap();
        assert_eq!((rect_of(button), laid(button)), ((100.0, 100.0, 148.0, 36.0), vec![(12.0, 6.0, 100.0, 24.0), (120.0, 10.0, 16.0, 16.0)]));
        // Only the frame that changed was looked at, and a second look writes nothing.
        assert!(Arc::ptr_eq(&doc.nodes[1], &pre.nodes[1]));
        let rested = doc.clone();
        assert!(!doc.settle_layout(Some(&pre), &[]) && !doc.settle_layout(None, &[]));
        assert!(Arc::ptr_eq(&doc.nodes[0], &rested.nodes[0]));
    }

    #[test]
    fn a_layout_is_read_off_an_arrangement() {
        // A row, given out of order: by where the boxes are, the usual gap, the tops lined up.
        let read = infer(&[(110.0, 10.0, 170.0, 40.0), (0.0, 10.0, 50.0, 30.0), (60.0, 10.0, 100.0, 50.0)]);
        assert_eq!(read, Inferred { dir: Dir::Horizontal, gap: 10.0, order: vec![1, 2, 0], cross: Align::Start });
        // A column of centred boxes with uneven gaps (8 and 13): the gap is their mean, rounded.
        let read = infer(&[(0.0, 0.0, 100.0, 20.0), (25.0, 28.0, 75.0, 48.0), (40.0, 61.0, 60.0, 81.0)]);
        assert_eq!(read, Inferred { dir: Dir::Vertical, gap: 11.0, order: vec![0, 1, 2], cross: Align::Center });
        // Lined up at the far side; overlapping boxes have no gap; one box alone is a row.
        assert_eq!(infer(&[(0.0, 0.0, 10.0, 30.0), (5.0, 20.0, 25.0, 30.0)]).cross, Align::End);
        assert_eq!(infer(&[(0.0, 0.0, 30.0, 10.0), (20.0, 0.0, 50.0, 10.0)]).gap, 0.0);
        assert_eq!(infer(&[(3.0, 4.0, 5.0, 6.0)]), Inferred { dir: Dir::Horizontal, gap: 10.0, order: vec![0], cross: Align::Start });
        assert_eq!(infer(&[]).order, Vec::<usize>::new());
    }

    #[test]
    fn a_flipped_layout_keeps_its_items_where_the_mirror_put_them() {
        let mut doc = Doc::default();
        let mut hidden = item(9, 5.0, 5.0);
        hidden.visible = false;
        let mut kids = three();
        kids.insert(1, hidden);
        doc.insert(flow(1, (400.0, 200.0), FIXED, AutoLayout { cross: Align::End, ..row(10.0, PAD) }, kids), None);
        let before = doc.clone();
        let mirrored = |doc: &Doc, horizontal: bool| {
            let f = before.find(1).unwrap();
            [2, 3, 4].iter().all(|id| {
                let (was, now) = (rect_of(before.find(*id).unwrap()), rect_of(doc.find(*id).unwrap()));
                let want = if horizontal { (2.0 * f.x + f.w - was.0 - was.2, was.1) } else { (was.0, 2.0 * f.y + f.h - was.1 - was.3) };
                (now.0, now.1) == want
            })
        };
        // Left to right: the row runs the other way, from the other side.
        assert!(doc.flip(&[1], true) && !doc.settle_layout(None, &[]));
        let l = doc.find(1).unwrap().layout.clone().unwrap();
        assert_eq!((l.pad, l.align, l.cross, doc.child_ids(Some(1))), ([40.0, 30.0, 20.0, 50.0], Align::End, Align::End, vec![4, 9, 3, 2]));
        assert!(mirrored(&doc, true));
        assert!(doc.flip(&[1], true) && !doc.settle_layout(None, &[]));
        assert_eq!(doc.nodes, before.nodes);
        // Top to bottom: the same row, lined up at the other side.
        assert!(doc.flip(&[1], false) && !doc.settle_layout(None, &[]));
        let l = doc.find(1).unwrap().layout.clone().unwrap();
        assert_eq!((l.pad, l.align, l.cross, doc.child_ids(Some(1))), ([20.0, 50.0, 40.0, 30.0], Align::Start, Align::Start, vec![2, 9, 3, 4]));
        assert!(mirrored(&doc, false));
    }

    #[test]
    fn a_wrapping_layout_is_not_mirrored() {
        // 140 of room: the first box has a row of its own, the other two share the next.
        let l = AutoLayout { wrap: true, row_gap: 5.0, cross: Align::End, ..row(10.0, PAD) };
        let mut doc = Doc::default();
        doc.insert(flow(1, (200.0, 200.0), FIXED, l.clone(), three()), None);
        let before = doc.clone();
        assert_eq!(laid(before.find(1).unwrap()).iter().map(|b| (b.0, b.1)).collect::<Vec<_>>(), [(20.0, 30.0), (20.0, 85.0), (110.0, 75.0)]);
        for horizontal in [true, false] {
            assert!(doc.flip(&[1], horizontal));
            doc.settle_layout(None, &[]);
            let f = doc.find(1).unwrap();
            // The layout and the order are as they were, and so the items are where they were;
            // what they show is mirrored.
            assert_eq!((f.layout.as_ref(), doc.child_ids(Some(1))), (Some(&l), vec![2, 3, 4]));
            assert_eq!(laid(f), laid(before.find(1).unwrap()));
            assert!(f.children.iter().all(|c| c.flip_h && (c.flip_v != horizontal)));
        }
    }

    /// Frame 1 at (100, 100), 200 × 100, with three 40 × 20 boxes in a row (ids 2, 3, 4 from the
    /// left, but stacked 4, 2, 3), and a loose box 5 beside it.
    fn arranged() -> Doc {
        let mut doc = Doc::default();
        let at = |id: u64, x: f32| node(id, Kind::Rect, (x, 130.0, 40.0, 20.0), (Min, Min));
        doc.insert(frame(vec![at(4, 220.0), at(2, 120.0), at(3, 170.0)]), None);
        doc.insert(node(5, Kind::Rect, (500.0, 100.0, 10.0, 10.0), (Min, Min)), None);
        doc.next_id = 10;
        doc
    }

    #[test]
    fn shift_a_gives_a_frame_the_layout_its_children_show() {
        let mut doc = arranged();
        doc.find_mut(1).unwrap().children.push({
            let mut hidden = item(6, 500.0, 500.0);
            hidden.visible = false;
            hidden
        });
        let before = doc.clone();
        assert_eq!(doc.add_auto_layout(&[1]), Ok(1));
        let f = doc.find(1).unwrap();
        // The padding is the room around the children, so nothing moves and the frame keeps its size.
        assert_eq!(f.layout, Some(AutoLayout { dir: Dir::Horizontal, wrap: false, gap: 10.0, row_gap: 0.0, pad: PAD, align: Align::Start, cross: Align::Start }));
        assert_eq!((f.size_h, f.size_v, rect_of(f)), (Sizing::Hug, Sizing::Hug, (100.0, 100.0, 200.0, 100.0)));
        // The children are in the order they are read in; the hidden one stays where it was in the stack.
        assert_eq!(doc.child_ids(Some(1)), vec![2, 3, 4, 6]);
        for id in [2, 3, 4, 6] {
            assert_eq!(rect_of(doc.find(id).unwrap()), rect_of(before.find(id).unwrap()));
        }
        assert!(Arc::ptr_eq(&doc.nodes[1], &before.nodes[1]));
        // A frame without children: a row with the default gap, its size as it was.
        doc.insert(node(7, Kind::Frame, (0.0, 900.0, 80.0, 60.0), (Min, Min)), None);
        assert_eq!(doc.add_auto_layout(&[7]), Ok(7));
        let empty = doc.find(7).unwrap();
        assert_eq!((empty.layout.as_ref().map(|l| l.gap), empty.size_h, rect_of(empty)), (Some(10.0), Sizing::Fixed, (0.0, 900.0, 80.0, 60.0)));
        // Removing it leaves a plain frame with everything where it is.
        let laid_out = doc.clone();
        assert!(doc.remove_auto_layout(1) && !doc.remove_auto_layout(1) && !doc.remove_auto_layout(2));
        let f = doc.find(1).unwrap();
        assert_eq!((f.layout.is_none(), f.size_h, f.size_v, &f.children), (true, Sizing::Fixed, Sizing::Fixed, &laid_out.find(1).unwrap().children));
        assert!(!doc.settle_layout(Some(&laid_out), &[]));
    }

    #[test]
    fn shift_a_wraps_other_layers_in_a_frame_that_hugs_them() {
        let mut doc = arranged();
        // Two of the three boxes in the frame, picked right to left.
        let id = doc.add_auto_layout(&[3, 2]).unwrap();
        let f = doc.find(id).unwrap();
        assert_eq!((f.kind, rect_of(f), f.size_h, f.size_v, f.fill), (Kind::Frame, (120.0, 130.0, 90.0, 20.0), Sizing::Hug, Sizing::Hug, None));
        assert_eq!(f.layout, Some(AutoLayout { gap: 10.0, ..Default::default() }));
        // In the same frame, where the front-most of them was in the stack; in reading order inside.
        assert_eq!((doc.parent_of(id), doc.child_ids(Some(1)), doc.child_ids(Some(id))), (Some(1), vec![4, id], vec![2, 3]));
        assert_eq!((rect_of(doc.find(2).unwrap()), rect_of(doc.find(3).unwrap())), ((120.0, 130.0, 40.0, 20.0), (170.0, 130.0, 40.0, 20.0)));
        // A frame that has a layout already is wrapped like any other layer; so are top-level layers.
        let outer = doc.add_auto_layout(&[id]).unwrap();
        assert!(outer != id && doc.parent_of(id) == Some(outer) && doc.parent_of(outer) == Some(1));
        let top = doc.add_auto_layout(&[5]).unwrap();
        assert_eq!((doc.parent_of(top), doc.child_ids(None), rect_of(doc.find(top).unwrap())), (None, vec![1, top], (500.0, 100.0, 10.0, 10.0)));
        // A column, read top to bottom.
        doc.insert(node(20, Kind::Rect, (0.0, 560.0, 30.0, 30.0), (Min, Min)), None);
        doc.insert(node(21, Kind::Rect, (10.0, 500.0, 10.0, 40.0), (Min, Min)), None);
        let col = doc.add_auto_layout(&[20, 21]).unwrap();
        let f = doc.find(col).unwrap();
        assert_eq!((f.layout.as_ref().map(|l| (l.dir, l.gap, l.cross)), doc.child_ids(Some(col))), (Some((Dir::Vertical, 20.0, Align::Center)), vec![21, 20]));
        // Not across frames, not with nothing, not inside an instance.
        assert!(doc.add_auto_layout(&[4, top]).is_err() && doc.add_auto_layout(&[]).is_err());
        doc.find_mut(2).unwrap().source = Some(99);
        assert!(doc.add_auto_layout(&[2]).is_err());
    }

    #[test]
    fn a_drop_lands_between_the_items_it_points_between() {
        // Three 40-wide boxes, 10 apart: their middles are at 120, 170 and 220.
        let f = flow(1, (999.0, 999.0), HUG, row(10.0, [0.0; 4]), vec![item(2, 40.0, 20.0), item(3, 40.0, 20.0), item(4, 40.0, 20.0)]);
        let at = |x: f32, skip: &[u64]| insert_index(&f, skip, x, 110.0);
        assert_eq!([at(0.0, &[]), at(119.0, &[]), at(121.0, &[]), at(180.0, &[]), at(500.0, &[])], [0, 0, 1, 2, 3]);
        // What is being dragged is not counted: the index is among the others.
        assert_eq!([at(100.0, &[2]), at(180.0, &[2]), at(230.0, &[2]), at(180.0, &[3]), at(230.0, &[3, 4])], [0, 1, 2, 1, 1]);
        // A column goes by the height.
        let c = flow(1, (999.0, 999.0), HUG, column(10.0, [0.0; 4]), vec![item(2, 40.0, 20.0), item(3, 40.0, 20.0)]);
        assert_eq!([insert_index(&c, &[], 900.0, 105.0), insert_index(&c, &[], 0.0, 115.0), insert_index(&c, &[], 0.0, 141.0)], [0, 1, 2]);
        // Children that are no items keep their place in the stack: the drop goes after the item it passed.
        let mut hidden = item(9, 40.0, 20.0);
        hidden.visible = false;
        let h = flow(1, (999.0, 999.0), HUG, row(10.0, [0.0; 4]), vec![item(2, 40.0, 20.0), hidden, item(3, 40.0, 20.0)]);
        assert_eq!([insert_index(&h, &[], 130.0, 110.0), insert_index(&h, &[], 180.0, 110.0)], [1, 3]);
        // Wrapping rows read like text: two rows of two (at y 100 and 130).
        let l = AutoLayout { wrap: true, row_gap: 10.0, ..row(10.0, [0.0; 4]) };
        let w = flow(1, (100.0, 999.0), (Sizing::Fixed, Sizing::Hug), l, vec![item(2, 40.0, 20.0), item(3, 40.0, 20.0), item(4, 40.0, 20.0), item(5, 40.0, 20.0)]);
        let at = |x: f32, y: f32| insert_index(&w, &[], x, y);
        assert_eq!([at(100.0, 110.0), at(130.0, 110.0), at(190.0, 110.0), at(100.0, 140.0), at(130.0, 140.0), at(190.0, 145.0), at(100.0, 400.0)], [0, 1, 2, 2, 3, 4, 4]);
        // Not an auto layout: on top, as anywhere else.
        assert_eq!(insert_index(&frame(vec![item(2, 1.0, 1.0), item(3, 1.0, 1.0)]), &[3], 0.0, 0.0), 1);
    }

    #[test]
    fn a_new_layer_goes_among_the_items_where_it_is_made() {
        // A row of 60, 80 and 30 wide boxes from x 120 on, 10 apart.
        let mut doc = Doc::default();
        doc.insert(flow(1, (400.0, 200.0), FIXED, row(10.0, PAD), three()), None);
        let new = |id: u64, x: f32| node(id, Kind::Rect, (x, 140.0, 0.0, 0.0), (Min, Min));
        // Drawn between the first two, before the first, and behind the last.
        doc.insert_placed(new(5, 185.0), Some(1));
        doc.insert_placed(new(6, 105.0), Some(1));
        doc.insert_placed(new(7, 390.0), Some(1));
        assert_eq!(doc.child_ids(Some(1)), [6, 2, 5, 3, 4, 7]);
        // What the layout does not place goes on top, as in a plain frame or at the top level.
        let mut floating = new(8, 105.0);
        floating.absolute = true;
        doc.insert_placed(floating, Some(1));
        doc.insert(frame(Vec::new()), None);
        doc.nodes[1] = Arc::new(Node { id: 20, ..(*doc.nodes[1]).clone() });
        doc.insert(node(21, Kind::Rect, (150.0, 150.0, 10.0, 10.0), (Min, Min)), Some(20));
        doc.insert_placed(new(22, 105.0), Some(20));
        doc.insert_placed(new(23, 0.0), None);
        assert_eq!((doc.child_ids(Some(1)).last(), doc.child_ids(Some(20)), doc.nodes.last().map(|n| n.id)), (Some(&8), vec![21, 22], Some(23)));
    }

    #[test]
    fn a_dragged_item_keeps_its_room_but_not_its_place() {
        let mut doc = Doc::default();
        doc.insert(flow(1, (999.0, 999.0), HUG, row(10.0, [0.0; 4]), vec![item(2, 40.0, 20.0), item(3, 50.0, 20.0), item(4, 40.0, 20.0)]), None);
        // Box 3 is being dragged: it is where the pointer has it, and its room in the row stays.
        doc.find_mut(3).unwrap().translate(33.0, 44.0);
        assert!(!doc.settle_around(&[3], &[3]));
        assert_eq!(laid(doc.find(1).unwrap()), [(0.0, 0.0, 40.0, 20.0), (83.0, 44.0, 50.0, 20.0), (110.0, 0.0, 40.0, 20.0)]);
        assert_eq!(slot_of(doc.find(1).unwrap(), 3), Some((150.0, 100.0, 50.0, 20.0)));
        // Moved to the front of the row while it is dragged: the others make room there.
        doc.find_mut(1).unwrap().children.swap(0, 1);
        assert!(doc.settle_around(&[3], &[3]));
        assert_eq!(laid(doc.find(1).unwrap()), [(83.0, 44.0, 50.0, 20.0), (60.0, 0.0, 40.0, 20.0), (110.0, 0.0, 40.0, 20.0)]);
        assert_eq!(slot_of(doc.find(1).unwrap(), 3), Some((100.0, 100.0, 50.0, 20.0)));
        // Let go, it takes its place.
        assert!(doc.settle_around(&[3], &[]));
        assert_eq!(laid(doc.find(1).unwrap())[0], (0.0, 0.0, 50.0, 20.0));
        // The arrow keys: one place along the row at a time, past children that are no items.
        doc.find_mut(2).unwrap().visible = false;
        assert!(doc.swap_item(3, true) && !doc.swap_item(3, true) && !doc.swap_item(2, false) && !doc.swap_item(99, true));
        assert_eq!(doc.child_ids(Some(1)), vec![4, 2, 3]);
        assert!(doc.swap_item(3, false) && !doc.swap_item(3, false));
        assert_eq!(doc.child_ids(Some(1)), vec![3, 2, 4]);
        // What is an item: a child of a frame with a layout that was not taken out of it.
        doc.find_mut(4).unwrap().absolute = true;
        assert!(doc.auto_item(3) && doc.auto_item(2) && !doc.auto_item(4) && !doc.auto_item(1) && !doc.auto_item(99));
        doc.insert_at(item(8, 5.0, 5.0), Some(1), 1);
        doc.insert_at(item(9, 5.0, 5.0), None, 0);
        doc.insert_at(item(10, 5.0, 5.0), Some(1), 99);
        assert_eq!((doc.child_ids(Some(1)), doc.child_ids(None)), (vec![3, 8, 2, 4, 10], vec![9, 1]));
    }

    #[test]
    fn only_what_changed_is_laid_out_and_only_what_differs_is_written() {
        let mut doc = Doc::default();
        let mut stale = flow(1, (999.0, 999.0), HUG, row(10.0, [0.0; 4]), vec![item(2, 40.0, 20.0), item(3, 40.0, 20.0)]);
        stale.children[1].x += 7.0;
        doc.insert(stale, None);
        doc.insert(flow(4, (999.0, 999.0), HUG, row(10.0, [0.0; 4]), vec![item(5, 40.0, 20.0)]), None);
        doc.insert(frame(vec![item(7, 1.0, 1.0)]), None);
        // A page that is not open is looked at too: instances there follow mains here.
        doc.add_page();
        let mut away = flow(8, (999.0, 999.0), HUG, row(10.0, [0.0; 4]), vec![item(9, 40.0, 20.0)]);
        away.w = 5.0;
        doc.insert(away, None);
        doc.switch_page(0);
        let pre = doc.clone();
        assert!(doc.settle_layout(None, &[]));
        assert_eq!((doc.find(3).unwrap().x, doc.find_all(8).unwrap().w), (150.0, 40.0));
        assert!(!Arc::ptr_eq(&doc.nodes[0], &pre.nodes[0]) && Arc::ptr_eq(&doc.nodes[1], &pre.nodes[1]) && Arc::ptr_eq(&doc.nodes[2], &pre.nodes[2]));
        // Against the document it last saw, a frame that is shared with it is not even looked at.
        let mut edited = doc.clone();
        Arc::make_mut(&mut edited.nodes[0]).children[0].w = 60.0;
        let mut same = (*edited.nodes[1]).clone();
        same.children[0].w = 99.0;
        let untouched = edited.clone();
        assert!(edited.settle_layout(Some(&doc), &[]));
        assert_eq!((edited.find(3).unwrap().x, edited.find(1).unwrap().w), (170.0, 110.0));
        assert!(Arc::ptr_eq(&edited.nodes[1], &untouched.nodes[1]));
        // A node that asks for a size no frame gives it gets a plain one.
        let mut loose = item(20, 10.0, 10.0);
        (loose.size_h, loose.size_v, loose.absolute) = (Sizing::Fill, Sizing::Hug, true);
        edited.insert(loose, None);
        assert!(edited.settle_layout(Some(&untouched), &[]));
        let loose = edited.find(20).unwrap();
        assert_eq!((loose.size_h, loose.size_v, loose.absolute), (Sizing::Fixed, Sizing::Fixed, false));
    }

    /// A button: main component 1, a row that hugs an icon (2) and a label (3). Result: the
    /// document and an instance of the button at (400, 300).
    fn button() -> (Doc, u64) {
        let mut doc = Doc::default();
        let label = node(3, Kind::Text, (0.0, 0.0, 20.0, 20.0), (Min, Min));
        let l = AutoLayout { cross: Align::Center, ..row(8.0, [16.0, 8.0, 16.0, 8.0]) };
        doc.insert(flow(1, (999.0, 999.0), HUG, l, vec![item(2, 16.0, 16.0), label]), None);
        doc.find_mut(3).unwrap().text = "OK".into();
        doc.next_id = 10;
        assert_eq!(doc.make_component(&[1]), Ok(1));
        let mut inst = doc.instantiate(1).unwrap();
        inst.translate(300.0, 200.0);
        let id = inst.id;
        doc.insert(inst, None);
        (doc, id)
    }

    /// An edit and what follows it, as the app does once per frame. Result: the document is at
    /// rest (looking again changes nothing, and writes nothing).
    fn edit(doc: &mut Doc, f: impl FnOnce(&mut Doc)) {
        let pre = doc.clone();
        f(doc);
        doc.settle(Some(&pre), &[]);
        let rested = doc.clone();
        assert!(!doc.settle(Some(&rested), &[]) && !doc.settle_layout(None, &[]));
        assert!(doc.nodes.iter().zip(&rested.nodes).all(|(a, b)| Arc::ptr_eq(a, b)));
    }

    #[test]
    fn an_instance_of_an_auto_layout_main_lays_itself_out() {
        let (mut doc, i) = button();
        let part = |doc: &Doc, source: u64| doc.find(i).unwrap().children.iter().find(|c| c.source == Some(source)).cloned().unwrap();
        let inst = doc.find(i).unwrap();
        assert_eq!((rect_of(doc.find(1).unwrap()), rect_of(inst)), ((100.0, 100.0, 76.0, 36.0), (400.0, 300.0, 76.0, 36.0)));
        assert_eq!((inst.layout.clone(), inst.size_h, inst.size_v), (doc.find(1).unwrap().layout.clone(), Sizing::Hug, Sizing::Hug));
        assert_eq!((rect_of(&part(&doc, 2)), rect_of(&part(&doc, 3))), ((416.0, 310.0, 16.0, 16.0), (440.0, 308.0, 20.0, 20.0)));
        // A longer label in the instance (the renderer measures it): this button grows, the main does not.
        let label = part(&doc, 3).id;
        edit(&mut doc, |d| {
            let t = d.find_mut(label).unwrap();
            (t.text, t.w) = ("A much longer label".into(), 150.0);
        });
        assert_eq!((rect_of(doc.find(i).unwrap()), rect_of(&part(&doc, 3)), doc.find(1).unwrap().w), ((400.0, 300.0, 206.0, 36.0), (440.0, 308.0, 150.0, 20.0), 76.0));
        // The main gets a wider gap, a higher icon and a third layer: the instance follows, around its own label.
        edit(&mut doc, |d| {
            d.find_mut(1).unwrap().layout.as_mut().unwrap().gap = 20.0;
            d.find_mut(2).unwrap().h = 30.0;
            d.insert(item(d.next_id + 1, 10.0, 10.0), Some(1));
            d.next_id += 1;
        });
        assert_eq!(rect_of(doc.find(1).unwrap()), (100.0, 100.0, 118.0, 46.0));
        let inst = doc.find(i).unwrap();
        assert_eq!((rect_of(inst), inst.children.len(), part(&doc, 3).text.as_str()), ((400.0, 300.0, 248.0, 46.0), 3, "A much longer label"));
        assert_eq!(laid(inst), [(16.0, 8.0, 16.0, 30.0), (52.0, 13.0, 150.0, 20.0), (222.0, 18.0, 10.0, 10.0)]);
        // A side that hugs snaps back when the instance is resized; one that the main fixes stays as set.
        assert!(!doc.set_box(i, None, (400.0, 300.0, 500.0, 90.0), Children::Constrain));
        edit(&mut doc, |d| {
            let m = d.find_mut(1).unwrap();
            (m.size_h, m.w, m.layout.as_mut().unwrap().align) = (Sizing::Fixed, 300.0, Align::End);
        });
        let inst = doc.find(i).unwrap();
        assert_eq!((inst.size_h, inst.size_v, rect_of(inst), laid(inst)[2].0), (Sizing::Fixed, Sizing::Hug, (400.0, 300.0, 248.0, 46.0), 222.0));
        assert!(doc.set_box(i, None, (400.0, 300.0, 500.0, 90.0), Children::Constrain));
        let inst = doc.find(i).unwrap();
        assert_eq!((rect_of(inst), laid(inst)[2]), ((400.0, 300.0, 500.0, 46.0), (474.0, 18.0, 10.0, 10.0)));
        // An instance that was not resized keeps taking the size of its main.
        let mut plain = doc.instantiate(1).unwrap();
        plain.translate(0.0, 700.0);
        let p = plain.id;
        doc.insert(plain, None);
        edit(&mut doc, |d| d.find_mut(1).unwrap().w = 320.0);
        assert_eq!((doc.find(p).unwrap().w, doc.find(i).unwrap().w), (320.0, 500.0));
    }

    #[test]
    fn layouts_and_components_come_to_rest_together() {
        let (mut doc, i) = button();
        // The instance fills a column that hugs its height; below it a box.
        edit(&mut doc, |d| {
            let mut inst = d.remove(i).unwrap();
            inst.size_h = Sizing::Fill;
            let mut card = node(20, Kind::Frame, (1000.0, 0.0, 300.0, 999.0), (Min, Min));
            (card.layout, card.size_v, card.children) = (Some(column(10.0, [10.0; 4])), Sizing::Hug, vec![inst, item(21, 50.0, 50.0)]);
            d.insert(card, None);
        });
        let at = |doc: &Doc, id: u64| rect_of(doc.find(id).unwrap());
        assert_eq!((at(&doc, i), at(&doc, 21), at(&doc, 20)), ((1010.0, 10.0, 280.0, 36.0), (1010.0, 56.0, 50.0, 50.0), (1000.0, 0.0, 300.0, 116.0)));
        assert_eq!((doc.find(i).unwrap().size_h, doc.find(i).unwrap().size_v), (Sizing::Fill, Sizing::Hug));
        // The label of the main gets two lines: the main grows, the instance follows, and the
        // column around the instance makes room for it.
        edit(&mut doc, |d| d.find_mut(3).unwrap().h = 44.0);
        assert_eq!((at(&doc, 1), at(&doc, i), at(&doc, 21), at(&doc, 20)), ((100.0, 100.0, 76.0, 60.0), (1010.0, 10.0, 280.0, 60.0), (1010.0, 80.0, 50.0, 50.0), (1000.0, 0.0, 300.0, 140.0)));
        // The card itself is a main with an instance elsewhere: the change goes on through it.
        assert_eq!(doc.make_component(&[20]), Ok(20));
        let mut copy = doc.instantiate(20).unwrap();
        copy.translate(0.0, 2000.0);
        let c = copy.id;
        doc.insert(copy, None);
        edit(&mut doc, |d| d.find_mut(3).unwrap().h = 20.0);
        let nested = doc.find(c).unwrap().children[0].clone();
        assert_eq!((at(&doc, 20).3, at(&doc, c), rect_of(&nested), nested.children.len()), (116.0, (1000.0, 2000.0, 300.0, 116.0), (1010.0, 2010.0, 280.0, 36.0), 2));
        assert_eq!(laid(&nested), [(16.0, 10.0, 16.0, 16.0), (40.0, 8.0, 20.0, 20.0)]);
        // The card made narrower: its instance of the button, and the one in its copy, fill what is left.
        edit(&mut doc, |d| {
            d.set_box(20, None, (1000.0, 0.0, 200.0, 116.0), Children::Constrain);
        });
        assert_eq!((at(&doc, i).2, at(&doc, c).2, doc.find(c).unwrap().children[0].w), (180.0, 200.0, 180.0));
    }

    #[test]
    fn layouts_are_saved_and_old_files_have_none() {
        let mut f = flow(1, (300.0, 999.0), (Sizing::Fixed, Sizing::Hug), AutoLayout { wrap: true, row_gap: 4.0, align: Align::Between, cross: Align::End, ..row(8.0, PAD) }, three());
        (f.children[0].size_h, f.children[1].absolute) = (Sizing::Fill, true);
        let json = serde_json::to_string(&f).unwrap();
        assert!(json.contains(r#""layout":{"dir":"horizontal","wrap":true,"gap":8.0,"row_gap":4.0,"pad":[20.0,30.0,40.0,50.0],"align":"between","cross":"end"}"#), "{json}");
        assert!(json.contains(r#""size_v":"hug""#) && json.contains(r#""size_h":"fill""#) && json.contains(r#""absolute":true"#));
        assert_eq!(serde_json::from_str::<Node>(&json).unwrap(), f);
        // Only written where they say something.
        let plain = serde_json::to_string(&frame(vec![item(2, 5.0, 5.0)])).unwrap();
        assert!(!plain.contains("layout") && !plain.contains("size_") && !plain.contains("absolute") && !plain.contains("row_gap"));
        let column = serde_json::to_string(&AutoLayout { dir: Dir::Vertical, ..Default::default() }).unwrap();
        assert_eq!(column, r#"{"dir":"vertical","gap":0.0,"pad":[0.0,0.0,0.0,0.0],"align":"start","cross":"start"}"#);
        // A file from before auto layout; and a layout with keys missing.
        let old: Node = serde_json::from_str(r#"{"id":1,"name":"F","kind":"Frame","x":0,"y":0,"w":10,"h":10,"children":[{"id":2,"name":"R","kind":"Rect","x":0,"y":0,"w":5,"h":5}]}"#).unwrap();
        assert!(old.layout.is_none() && !old.is_auto() && (old.size_h, old.size_v, old.children[0].absolute) == (Sizing::Fixed, Sizing::Fixed, false));
        let part: Node = serde_json::from_str(r#"{"id":1,"name":"F","kind":"Frame","x":0,"y":0,"w":10,"h":10,"layout":{"dir":"vertical","gap":4}}"#).unwrap();
        assert_eq!(part.layout, Some(AutoLayout { dir: Dir::Vertical, gap: 4.0, ..Default::default() }));
        for a in Align::ALL {
            assert_eq!(Align::parse(a.key()), Some(a));
        }
        for s in Sizing::ALL {
            assert_eq!(Sizing::parse(s.key()), Some(s));
        }
        assert_eq!((Align::parse("middle"), Sizing::parse("auto")), (None, None));
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
