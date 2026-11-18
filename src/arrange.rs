//! Arranging a selection: flip, distribute spacing, tidy up.
//!
//! Flip. A node is mirrored about the centre of its own box, then rotated. Rectangles, ellipses,
//! images, text and frames remember that in `flip_h` / `flip_v`, and canvas and export mirror
//! what they draw (`Node::flips`). Paths and lines have their points mirrored instead, so
//! editing them works on the geometry that is shown; their flags stay off.

use crate::doc::{Doc, Gradient, Kind, Node};

type Box4 = (f32, f32, f32, f32);

fn union(a: Box4, b: Box4) -> Box4 {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

impl Gradient {
    /// Mirror the gradient inside its box, left-right (`horizontal`) or top-bottom.
    pub fn mirror(&mut self, horizontal: bool) {
        let k = if horizontal { 0 } else { 1 };
        for line in [&mut self.line, &mut self.radial_line].into_iter().flatten() {
            line[k] = 1.0 - line[k];
            line[k + 2] = 1.0 - line[k + 2];
        }
        // Without a line the linear gradient runs through the centre at `angle`.
        let turned = if horizontal { 180.0 - self.angle } else { -self.angle };
        self.angle = turned.rem_euclid(360.0);
    }
}

impl Node {
    /// How canvas and export mirror what this node draws: (left-right, top-bottom).
    pub fn flips(&self) -> (bool, bool) {
        if matches!(self.kind, Kind::Path | Kind::Line) {
            (false, false)
        } else {
            (self.flip_h, self.flip_v)
        }
    }

    /// Mirror what the node shows about the centre of its own box; the box stays where it is.
    pub fn mirror_look(&mut self, horizontal: bool) {
        match self.kind {
            Kind::Path => {
                let k = if horizontal { 0 } else { 1 };
                for q in self.points.iter_mut().chain(self.holes.iter_mut().flatten()) {
                    // Rounded so that flipping back gives the very same numbers.
                    let turn = |v: f32| ((1.0 - v) * 1e6).round() / 1e6;
                    if horizontal {
                        q.x = turn(q.x);
                    } else {
                        q.y = turn(q.y);
                    }
                    q.hin[k] = -q.hin[k];
                    q.hout[k] = -q.hout[k];
                }
                if let Some(g) = &mut self.gradient {
                    g.mirror(horizontal);
                }
            }
            // A line is its two end points.
            Kind::Line if horizontal => (self.x, self.w) = (self.x + self.w, -self.w),
            Kind::Line => (self.y, self.h) = (self.y + self.h, -self.h),
            _ if horizontal => self.flip_h = !self.flip_h,
            _ => self.flip_v = !self.flip_v,
        }
    }

    /// Mirror the node and everything in it about the vertical line x = `axis` (`horizontal`)
    /// or the horizontal line y = `axis`.
    pub fn mirror(&mut self, axis: f32, horizontal: bool) {
        let (cx, cy) = self.center();
        self.mirror_look(horizontal);
        // The axis of a turned selection is no round number; keep that noise out of the position.
        let land = |v: f32| (v * 100.0).round() / 100.0;
        if horizontal {
            self.x = land(self.x + 2.0 * (axis - cx));
        } else {
            self.y = land(self.y + 2.0 * (axis - cy));
        }
        // Mirroring a turned node turns it the other way.
        if self.rotation != 0.0 {
            self.rotation = -self.rotation;
        }
        for c in &mut self.children {
            c.mirror(axis, horizontal);
        }
        crate::layout::flip_layout(self, horizontal);
    }

    /// Before the node becomes a path: its flip moves into the gradient, the flags go.
    pub fn settle_flip(&mut self) {
        let (h, v) = (std::mem::take(&mut self.flip_h), std::mem::take(&mut self.flip_v));
        if let Some(g) = &mut self.gradient {
            if h {
                g.mirror(true);
            }
            if v {
                g.mirror(false);
            }
        }
    }
}

/// What distribute and tidy up move as one: a node, or the selected members of one group.
struct Unit {
    ids: Vec<u64>,
    group: u64,
    b: Box4,
}

impl Unit {
    fn span(&self, horizontal: bool) -> (f32, f32) {
        if horizontal {
            (self.b.0, self.b.2)
        } else {
            (self.b.1, self.b.3)
        }
    }

    fn size(&self, horizontal: bool) -> f32 {
        let (lo, hi) = self.span(horizontal);
        hi - lo
    }

    fn mid(&self, horizontal: bool) -> f32 {
        let (lo, hi) = self.span(horizontal);
        (lo + hi) * 0.5
    }
}

/// Clusters along one axis: units whose centre falls inside the span of a cluster join it.
/// The clusters come in order, and so do the units in each (by their near edge).
fn bands(units: &[Unit], horizontal: bool) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..units.len()).collect();
    order.sort_by(|a, b| units[*a].span(horizontal).0.total_cmp(&units[*b].span(horizontal).0));
    let mut out: Vec<(f32, f32, Vec<usize>)> = Vec::new();
    for i in order {
        let (lo, hi) = units[i].span(horizontal);
        let mid = units[i].mid(horizontal);
        match out.last_mut() {
            // Either centre inside the other: a small unit beside a tall one is the same band.
            Some(band) if (mid >= band.0 && mid <= band.1) || ((band.0 + band.1) * 0.5 >= lo && (band.0 + band.1) * 0.5 <= hi) => {
                (band.0, band.1) = (band.0.min(lo), band.1.max(hi));
                band.2.push(i);
            }
            _ => out.push((lo, hi, vec![i])),
        }
    }
    out.into_iter().map(|b| b.2).collect()
}

impl Doc {
    /// The units among `ids`, in the order given. Members of one group form one unit; what sits
    /// inside another selected node goes along with that node.
    fn units(&self, ids: &[u64]) -> Vec<Unit> {
        let mut out: Vec<Unit> = Vec::new();
        for id in self.roots_of(ids) {
            let Some(n) = self.find(id) else { continue };
            let b = n.aabb();
            match out.iter_mut().find(|u| n.group != 0 && u.group == n.group) {
                Some(u) => {
                    u.ids.push(id);
                    u.b = union(u.b, b);
                }
                None => out.push(Unit { ids: vec![id], group: n.group, b }),
            }
        }
        out
    }

    /// How many units distribute and tidy up would see in `ids`.
    pub fn unit_count(&self, ids: &[u64]) -> usize {
        self.units(ids).len()
    }

    fn shift(&mut self, unit: &Unit, dx: f32, dy: f32) {
        // An unmoved node is left alone, so its top-level frame stays shared with undo snapshots.
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        for id in &unit.ids {
            if let Some(n) = self.find_mut(*id) {
                n.translate(dx, dy);
            }
        }
    }

    /// Align (`mode`: 0 left, 1 right, 2 centre, 3 top, 4 bottom, 5 middle). Several units line
    /// up on that side of the box around all of them; a single unit goes to that side of the
    /// frame it is in. A group moves as one, so its members keep their places in it, and a
    /// turned node is measured by the box around it. Result: whether there was anything to
    /// align to (a single unit at the top level has nothing).
    pub fn align(&mut self, ids: &[u64], mode: u8) -> bool {
        let units = self.units(ids);
        let target = match &units[..] {
            [] => None,
            [one] => self.parent_of(one.ids[0]).and_then(|p| self.find(p)).map(|f| f.aabb()),
            many => many.iter().map(|u| u.b).reduce(union),
        };
        let Some(t) = target else { return false };
        for u in &units {
            let b = u.b;
            let (dx, dy) = match mode {
                0 => (t.0 - b.0, 0.0),
                1 => (t.2 - b.2, 0.0),
                2 => ((t.0 + t.2 - b.0 - b.2) * 0.5, 0.0),
                3 => (0.0, t.1 - b.1),
                4 => (0.0, t.3 - b.3),
                _ => (0.0, (t.1 + t.3 - b.1 - b.3) * 0.5),
            };
            self.shift(u, dx.round(), dy.round());
        }
        true
    }

    /// Distribute spacing: the two outermost units stay, the ones between are moved so that the
    /// gaps between neighbours are equal. Needs three units. Result: whether it applied.
    pub fn distribute(&mut self, ids: &[u64], horizontal: bool) -> bool {
        let mut units = self.units(ids);
        if units.len() < 3 {
            return false;
        }
        units.sort_by(|a, b| a.mid(horizontal).total_cmp(&b.mid(horizontal)));
        let (first, last) = (&units[0], &units[units.len() - 1]);
        let inner: f32 = units[1..units.len() - 1].iter().map(|u| u.size(horizontal)).sum();
        let gap = (last.span(horizontal).0 - first.span(horizontal).1 - inner) / (units.len() - 1) as f32;
        let mut at = first.span(horizontal).1 + gap;
        for u in &units[1..units.len() - 1] {
            let d = at - u.span(horizontal).0;
            // Whole numbers stay whole: no drift from rounding in the sums above.
            let d = if (d - d.round()).abs() < 1e-3 { d.round() } else { d };
            self.shift(u, if horizontal { d } else { 0.0 }, if horizontal { 0.0 } else { d });
            at += u.size(horizontal) + gap;
        }
        true
    }

    /// Tidy up: one row or one column is aligned (top / left) and gets equal gaps; anything else
    /// is snapped into a grid, row by row in reading order, with one column gap and one row gap.
    /// Needs two units. Result: whether it applied.
    pub fn tidy(&mut self, ids: &[u64]) -> bool {
        let units = self.units(ids);
        if units.len() < 2 {
            return false;
        }
        let rows = bands(&units, false);
        let cols = bands(&units, true);
        let all = units.iter().map(|u| u.b).reduce(union).unwrap_or_default();
        if rows.len() == 1 || cols.len() == 1 {
            // Along the row (or down the column): keep the ends, share the room between them.
            let horizontal = rows.len() == 1;
            let order = if horizontal { &cols } else { &rows };
            let order: Vec<usize> = order.iter().flatten().copied().collect();
            let used: f32 = units.iter().map(|u| u.size(horizontal)).sum();
            let (lo, hi) = if horizontal { (all.0, all.2) } else { (all.1, all.3) };
            let gap = ((hi - lo - used) / (units.len() - 1) as f32).max(0.0).round();
            let (mut at, edge) = (lo.round(), if horizontal { all.1 } else { all.0 }.round());
            for i in order {
                let u = &units[i];
                let (dx, dy) = if horizontal { (at - u.b.0, edge - u.b.1) } else { (edge - u.b.0, at - u.b.1) };
                self.shift(u, dx, dy);
                at += u.size(horizontal) + gap;
            }
            return true;
        }
        // Grid: the rows as found, each read left to right; a unit's column is its place in its row.
        let rows: Vec<Vec<usize>> = rows
            .into_iter()
            .map(|mut r| {
                r.sort_by(|a, b| units[*a].b.0.total_cmp(&units[*b].b.0));
                r
            })
            .collect();
        let wide = rows.iter().map(Vec::len).max().unwrap_or(0);
        let col_w: Vec<f32> = (0..wide).map(|c| rows.iter().filter_map(|r| r.get(c)).map(|i| units[*i].size(true)).fold(0.0, f32::max)).collect();
        let row_h: Vec<f32> = rows.iter().map(|r| r.iter().map(|i| units[*i].size(false)).fold(0.0, f32::max)).collect();
        // The gaps the layout already has, averaged.
        let mean = |gaps: Vec<f32>| if gaps.is_empty() { 0.0 } else { (gaps.iter().sum::<f32>() / gaps.len() as f32).max(0.0).round() };
        let col_gap = mean(rows.iter().flat_map(|r| r.windows(2).map(|w| units[w[1]].b.0 - units[w[0]].b.2)).collect());
        let edge = |r: &Vec<usize>, top: bool| r.iter().map(|i| if top { units[*i].b.1 } else { units[*i].b.3 }).fold(if top { f32::MAX } else { f32::MIN }, if top { f32::min } else { f32::max });
        let row_gap = mean(rows.windows(2).map(|w| edge(&w[1], true) - edge(&w[0], false)).collect());
        let mut y = all.1.round();
        for (ri, row) in rows.iter().enumerate() {
            let mut x = all.0.round();
            for (ci, i) in row.iter().enumerate() {
                let u = &units[*i];
                self.shift(u, x - u.b.0, y - u.b.1);
                x += col_w[ci] + col_gap;
            }
            y += row_h[ri] + row_gap;
        }
        true
    }

    /// Flip the selection about the centre of the box around it, left-right (`horizontal`) or
    /// top-bottom: one node flips in place, several also swap sides, and a frame takes
    /// everything in it along. Result: whether there was anything to flip.
    pub fn flip(&mut self, ids: &[u64], horizontal: bool) -> bool {
        let roots = self.roots_of(ids);
        let Some(b) = roots.iter().filter_map(|id| self.find(*id).map(|n| n.aabb())).reduce(union) else { return false };
        let axis = if horizontal { (b.0 + b.2) * 0.5 } else { (b.1 + b.3) * 0.5 };
        for id in roots {
            if let Some(n) = self.find_mut(id) {
                n.mirror(axis, horizontal);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::PathPoint;

    fn add(doc: &mut Doc, kind: Kind, parent: Option<u64>, (x, y, w, h): (f32, f32, f32, f32)) -> u64 {
        let id = doc.alloc_id();
        let mut n = Node::new(id, kind, x, y);
        (n.w, n.h, n.fill) = (w, h, Some([0, 0, 0, 255]));
        doc.insert(n, parent);
        id
    }

    fn at(doc: &Doc, id: u64) -> (f32, f32) {
        let n = doc.find(id).unwrap();
        (n.x, n.y)
    }

    #[test]
    fn distribute_equalizes_gaps_between_the_outermost() {
        let mut doc = Doc::default();
        // Given out of order: what counts is where they are.
        let c = add(&mut doc, Kind::Rect, None, (300.0, 40.0, 40.0, 40.0));
        let a = add(&mut doc, Kind::Rect, None, (0.0, 0.0, 20.0, 20.0));
        let b = add(&mut doc, Kind::Rect, None, (30.0, 90.0, 60.0, 20.0));
        let d = add(&mut doc, Kind::Rect, None, (200.0, 10.0, 20.0, 20.0));
        assert!(!doc.distribute(&[a, b], true), "two units have nothing between them");
        assert!(doc.distribute(&[c, a, b, d], true));
        // 300 - 20 = 280 of room, 80 taken by b and d: three gaps of 200 / 3.
        let gap = 200.0 / 3.0;
        assert_eq!((at(&doc, a), at(&doc, c)), ((0.0, 0.0), (300.0, 40.0)));
        assert!((at(&doc, b).0 - (20.0 + gap)).abs() < 1e-3 && (at(&doc, d).0 - (80.0 + 2.0 * gap)).abs() < 1e-3);
        assert_eq!((at(&doc, b).1, at(&doc, d).1), (90.0, 10.0), "the other axis is left alone");
        assert!(doc.distribute(&[a, b, c, d], false));
        // Top to bottom: a (0..20), d, c (40..80), b (90..110): 10 of room left for three gaps.
        assert!((at(&doc, d).1 - (20.0 + 10.0 / 3.0)).abs() < 1e-3 && (at(&doc, c).1 - (40.0 + 20.0 / 3.0)).abs() < 1e-3);
    }

    #[test]
    fn align_moves_a_group_as_one_and_measures_turned_boxes() {
        let mut doc = Doc::default();
        let frame = add(&mut doc, Kind::Frame, None, (100.0, 100.0, 600.0, 400.0));
        let a = add(&mut doc, Kind::Rect, Some(frame), (150.0, 150.0, 40.0, 40.0));
        let g1 = add(&mut doc, Kind::Rect, Some(frame), (260.0, 200.0, 20.0, 20.0));
        let g2 = add(&mut doc, Kind::Rect, Some(frame), (290.0, 230.0, 30.0, 20.0));
        let turned = add(&mut doc, Kind::Rect, Some(frame), (400.0, 300.0, 20.0, 60.0));
        doc.group(&[g1, g2]);
        doc.find_mut(turned).unwrap().rotation = 90.0;
        let was = doc.clone();
        // The group alone: to the left edge of its frame, the members as far apart as before
        // (each on its own would land on that edge, one on top of the other).
        assert!(doc.align(&[g1, g2], 0));
        assert_eq!((at(&doc, g1), at(&doc, g2)), ((100.0, 200.0), (130.0, 230.0)));
        // And to its bottom: the lower member touches it.
        assert!(doc.align(&[g2, g1], 4));
        assert_eq!((at(&doc, g1), at(&doc, g2)), ((100.0, 450.0), (130.0, 480.0)));
        // A turned box alone: the box around it (60 wide, 20 high) goes to the right edge.
        assert!(doc.align(&[turned], 1));
        let b = doc.find(turned).unwrap().aabb();
        assert_eq!((b.0, b.2), (640.0, 700.0));
        // Several: to the box around all of them (150..440 by 150..340), the group as one unit.
        doc = was;
        assert!(doc.align(&[a, g1, g2, turned], 3));
        assert_eq!((at(&doc, a).1, at(&doc, g1).1, at(&doc, g2).1, doc.find(turned).unwrap().aabb().1), (150.0, 150.0, 180.0, 150.0));
        assert!(doc.align(&[a, g1, g2, turned], 2));
        // Centres at 295: a is 40 wide, the group 60, the turned box 60.
        assert_eq!((at(&doc, a).0, at(&doc, g1).0, at(&doc, g2).0, doc.find(turned).unwrap().aabb().0), (275.0, 265.0, 295.0, 265.0));
        // Nothing to align to at the top level, and nothing moves.
        let loose = add(&mut doc, Kind::Rect, None, (900.0, 900.0, 10.0, 10.0));
        assert!(!doc.align(&[loose], 0) && !doc.align(&[], 0));
        assert_eq!(at(&doc, loose), (900.0, 900.0));
    }

    #[test]
    fn distribute_moves_a_group_as_one_and_measures_turned_boxes() {
        let mut doc = Doc::default();
        let frame = add(&mut doc, Kind::Frame, None, (0.0, 0.0, 600.0, 200.0));
        let a = add(&mut doc, Kind::Rect, Some(frame), (0.0, 0.0, 40.0, 40.0));
        let g1 = add(&mut doc, Kind::Rect, Some(frame), (60.0, 0.0, 20.0, 20.0));
        let g2 = add(&mut doc, Kind::Rect, Some(frame), (90.0, 30.0, 30.0, 20.0));
        let turned = add(&mut doc, Kind::Rect, Some(frame), (300.0, 0.0, 20.0, 60.0));
        let z = add(&mut doc, Kind::Rect, Some(frame), (500.0, 0.0, 40.0, 40.0));
        doc.group(&[g1, g2]);
        // A quarter turn: on screen the box is 60 wide and 20 high, around the same centre.
        doc.find_mut(turned).unwrap().rotation = 90.0;
        assert_eq!(doc.unit_count(&[a, g1, g2, turned, z]), 4);
        assert!(doc.distribute(&[a, g1, g2, turned, z], true));
        // Room 500 - 40 = 460; the group is 60 wide and the turned box 60: two gaps… three, of 340 / 3.
        let gap = (460.0 - 60.0 - 60.0) / 3.0;
        let (x1, x2) = (at(&doc, g1).0, at(&doc, g2).0);
        assert!((x1 - (40.0 + gap)).abs() < 1e-3, "group starts at {x1}");
        assert_eq!(x2 - x1, 30.0, "the members keep their places in the group");
        let b = doc.find(turned).unwrap().aabb();
        assert!((b.0 - (40.0 + gap + 60.0 + gap)).abs() < 1e-3 && (b.2 - b.0 - 60.0).abs() < 1e-3, "turned box at {b:?}");
        assert!((500.0 - b.2 - gap).abs() < 1e-3);
        assert_eq!((at(&doc, a), at(&doc, z)), ((0.0, 0.0), (500.0, 0.0)));
    }

    #[test]
    fn tidy_straightens_a_row_and_a_column() {
        let mut doc = Doc::default();
        let a = add(&mut doc, Kind::Rect, None, (10.0, 12.0, 40.0, 40.0));
        let b = add(&mut doc, Kind::Rect, None, (70.0, 5.0, 40.0, 40.0));
        let c = add(&mut doc, Kind::Rect, None, (190.0, 20.0, 40.0, 40.0));
        assert!(doc.tidy(&[c, a, b]));
        // 220 wide, 120 taken: gaps of 50; tops at the highest one.
        assert_eq!((at(&doc, a), at(&doc, b), at(&doc, c)), ((10.0, 5.0), (100.0, 5.0), (190.0, 5.0)));

        let mut doc = Doc::default();
        let a = add(&mut doc, Kind::Rect, None, (3.0, 0.0, 80.0, 20.0));
        let b = add(&mut doc, Kind::Rect, None, (0.0, 26.0, 60.0, 20.0));
        let c = add(&mut doc, Kind::Rect, None, (9.0, 100.0, 70.0, 20.0));
        assert!(doc.tidy(&[a, b, c]));
        assert_eq!((at(&doc, a), at(&doc, b), at(&doc, c)), ((0.0, 0.0), (0.0, 50.0), (0.0, 100.0)));
        assert!(!doc.tidy(&[a]));
    }

    #[test]
    fn tidy_snaps_a_loose_layout_into_a_grid() {
        let mut doc = Doc::default();
        // Two rows of three, given in no particular order; the second row is one short.
        let r1 = [(0.0, 0.0, 50.0, 30.0), (62.0, 8.0, 40.0, 40.0), (118.0, 3.0, 50.0, 30.0)];
        let r2 = [(4.0, 70.0, 60.0, 30.0), (80.0, 64.0, 40.0, 30.0)];
        let mut ids: Vec<u64> = r2.iter().chain(&r1).map(|b| add(&mut doc, Kind::Rect, None, *b)).collect();
        ids.reverse();
        assert!(doc.tidy(&ids));
        let mut got: Vec<(f32, f32)> = ids.iter().map(|id| at(&doc, *id)).collect();
        got.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.total_cmp(&b.0)));
        // Columns as wide as their widest cell (60, 40, 50), rows as high as their highest (40, 30).
        // Column gap: mean of 12, 16, 16 = 15 (rounded); row gap: 64 - 48 = 16.
        assert_eq!(got, vec![(0.0, 0.0), (75.0, 0.0), (130.0, 0.0), (0.0, 56.0), (75.0, 56.0)]);
    }

    fn arrow() -> Node {
        // Points right: (0,0) → (1,0.5) → (0,1), with a curve leaving the first point.
        let mut n = Node::new(1, Kind::Path, 100.0, 50.0);
        (n.w, n.h, n.closed) = (40.0, 20.0, true);
        let p = |x: f32, y: f32| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        n.points = vec![PathPoint { hout: [0.25, 0.1], ..p(0.0, 0.0) }, p(1.0, 0.5), PathPoint { r: Some(3.0), ..p(0.0, 1.0) }];
        n.holes = vec![vec![p(0.1, 0.4), p(0.3, 0.5), p(0.1, 0.6)]];
        n
    }

    #[test]
    fn flipping_a_path_mirrors_its_points() {
        let mut doc = Doc::default();
        doc.next_id = 1;
        doc.insert(arrow(), None);
        assert!(doc.flip(&[1], true));
        let n = doc.find(1).unwrap();
        // Alone it flips in place, and stays an ordinary path: nothing left for the renderer to do.
        assert_eq!((n.x, n.y, n.w, n.h), (100.0, 50.0, 40.0, 20.0));
        assert_eq!(n.flips(), (false, false));
        assert!(!n.flip_h && !n.flip_v);
        assert_eq!(n.points.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>(), vec![(1.0, 0.0), (0.0, 0.5), (1.0, 1.0)]);
        assert_eq!((n.points[0].hout, n.points[2].r), ([-0.25, 0.1], Some(3.0)));
        assert_eq!(n.holes[0][1].x, 0.7);
        // The tip now points left on the canvas too.
        let tip = n.path_points(8).into_iter().map(|p| p.0).fold(f32::MAX, f32::min);
        assert_eq!(tip, 100.0);
        assert!(doc.flip(&[1], false));
        assert_eq!(doc.find(1).unwrap().points[0].hout, [-0.25, -0.1]);
    }

    #[test]
    fn flipping_twice_changes_nothing() {
        let mut doc = Doc::default();
        doc.next_id = 1;
        let mut path = arrow();
        path.rotation = 30.0;
        path.gradient = Some(Gradient { line: Some([0.0, 0.25, 1.0, 0.5]), ..Gradient::from_color([255, 0, 0, 255]) });
        doc.insert(path, None);
        let frame = add(&mut doc, Kind::Frame, None, (200.0, 0.0, 300.0, 200.0));
        let text = add(&mut doc, Kind::Text, Some(frame), (220.0, 20.0, 80.0, 20.0));
        let line = add(&mut doc, Kind::Line, Some(frame), (400.0, 150.0, -60.0, 30.0));
        let image = add(&mut doc, Kind::Image, None, (10.0, 300.0, 64.0, 48.0));
        {
            let n = doc.find_mut(image).unwrap();
            (n.rotation, n.crop) = (-12.0, Some([0.1, 0.0, 0.6, 1.0]));
            doc.find_mut(text).unwrap().gradient = Some(Gradient::from_color([0, 0, 255, 255]));
        }
        let before: Vec<Node> = doc.nodes.iter().map(|n| (**n).clone()).collect();
        let all = [1, frame, text, line, image];
        for horizontal in [true, false] {
            assert!(doc.flip(&all, horizontal));
            assert!(doc.nodes.iter().zip(&before).all(|(a, b)| **a != *b), "the first flip shows");
            assert!(doc.flip(&all, horizontal));
            let after: Vec<Node> = doc.nodes.iter().map(|n| (**n).clone()).collect();
            assert_eq!(after, before);
        }
    }

    #[test]
    fn flipping_a_selection_mirrors_places_and_turns() {
        let mut doc = Doc::default();
        let a = add(&mut doc, Kind::Rect, None, (0.0, 0.0, 40.0, 20.0));
        let b = add(&mut doc, Kind::Image, None, (100.0, 50.0, 60.0, 30.0));
        let l = add(&mut doc, Kind::Line, None, (10.0, 40.0, 30.0, 10.0));
        doc.find_mut(b).unwrap().rotation = 20.0;
        // The box around them: the turned image reaches a little past its own box.
        let old = doc.find(b).unwrap().aabb();
        let (left, right) = (0.0, old.2);
        assert!(doc.flip(&[a, b, l], true));
        let (na, nb, nl) = (doc.find(a).unwrap(), doc.find(b).unwrap(), doc.find(l).unwrap());
        assert!((na.x - (left + right - 40.0)).abs() < 0.02 && na.y == 0.0, "a swapped sides: {}", na.x);
        assert!(na.flip_h && !na.flip_v && nb.flip_h);
        assert_eq!(nb.rotation, -20.0);
        let new = nb.aabb();
        assert!((new.0 - left).abs() < 0.02 && (new.2 - new.0 - (old.2 - old.0)).abs() < 0.02, "the image now touches the left edge: {new:?}");
        // The line runs the other way from its mirrored start; no flag, its end points say it all.
        assert!((nl.x - (right - 10.0)).abs() < 0.02 && nl.w == -30.0 && (nl.y, nl.h) == (40.0, 10.0));
        assert!(!nl.flip_h);
        // Top-bottom about the middle of the box: only y and the other flag change.
        let x = na.x;
        assert!(doc.flip(&[a, b, l], false));
        let na = doc.find(a).unwrap();
        assert!(na.x == x && na.flip_h && na.flip_v && na.y > 0.0);
        assert_eq!(doc.find(b).unwrap().rotation, 20.0);
    }

    #[test]
    fn flipping_a_frame_mirrors_everything_in_it() {
        let mut doc = Doc::default();
        let frame = add(&mut doc, Kind::Frame, None, (100.0, 100.0, 200.0, 100.0));
        let left = add(&mut doc, Kind::Text, Some(frame), (110.0, 110.0, 50.0, 20.0));
        let inner = add(&mut doc, Kind::Frame, Some(frame), (220.0, 120.0, 60.0, 60.0));
        let deep = add(&mut doc, Kind::Ellipse, Some(inner), (225.0, 125.0, 10.0, 10.0));
        let other = add(&mut doc, Kind::Rect, None, (0.0, 0.0, 10.0, 10.0));
        let shared = doc.nodes[1].clone();
        assert!(doc.flip(&[frame, left], true), "what sits in a selected frame is not flipped twice");
        let at = |id: u64| doc.find(id).map(|n| (n.x, n.y, n.flip_h)).unwrap();
        assert_eq!(at(frame), (100.0, 100.0, true));
        assert_eq!(at(left), (240.0, 110.0, true));
        assert_eq!(at(inner), (120.0, 120.0, true));
        assert_eq!(at(deep), (165.0, 125.0, true));
        // Untouched top-level nodes stay shared with the undo snapshots.
        assert!(std::sync::Arc::ptr_eq(&doc.nodes[1], &shared));
        assert_eq!(at(other), (0.0, 0.0, false));
        // Members of a group flip as one: about the middle of the box around them.
        let (p, q) = (add(&mut doc, Kind::Rect, None, (0.0, 300.0, 10.0, 10.0)), add(&mut doc, Kind::Rect, None, (50.0, 300.0, 30.0, 10.0)));
        doc.group(&[p, q]);
        let members = doc.group_members(&[doc.find(p).unwrap().group]);
        assert!(doc.flip(&members, true));
        assert_eq!((doc.find(p).unwrap().x, doc.find(q).unwrap().x), (70.0, 0.0));
    }
}
