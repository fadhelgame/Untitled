//! Bantuan saat menggeser: snapping ke objek lain / frame dengan garis bantu, dan zona rotasi.

use super::render::{GapMark, Guide, Snap};
use super::*;
use crate::doc::rotate;

const GUIDE: Color32 = Color32::from_rgb(255, 64, 64);

/// Equal-spacing snapping looks at no more neighbours than this (the nearest ones), so that
/// dragging inside a frame with hundreds of layers stays cheap.
const SPACING_BOXES: usize = 400;
/// Two gaps this close (document units) count as equal.
const SAME_GAP: f32 = 0.05;

type Box4 = (f32, f32, f32, f32);

fn union(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

/// A box as (start, end) along the axis, then (start, end) across it.
fn span(b: Box4, horizontal: bool) -> (f32, f32, f32, f32) {
    if horizontal {
        (b.0, b.2, b.1, b.3)
    } else {
        (b.1, b.3, b.0, b.2)
    }
}

/// For every box the next one along the axis that it shares a row (or column) with.
fn next_pairs(boxes: &[Box4], horizontal: bool) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (i, a) in boxes.iter().enumerate() {
        let (_, a1, ap0, ap1) = span(*a, horizontal);
        let mut best: Option<(usize, f32)> = None;
        for (j, b) in boxes.iter().enumerate() {
            let (b0, _, bp0, bp1) = span(*b, horizontal);
            if j != i && b0 >= a1 && bp0 < ap1 && bp1 > ap0 && best.map_or(true, |(_, x)| b0 < x) {
                best = Some((j, b0));
            }
        }
        out.extend(best.map(|(j, _)| (i, j)));
    }
    out
}

/// The gaps along one axis around the dragged box `b`: between it and its nearest neighbour on
/// either side, and between neighbours that share its row. Each is (gap, lo, hi, at, own).
fn gaps_around(snap: &Snap, b: Box4, horizontal: bool, slack: f32) -> (Option<usize>, Option<usize>, Vec<(f32, f32, f32, f32, bool)>) {
    let (m0, m1, q0, q1) = span(b, horizontal);
    let in_row = |c: Box4| {
        let (_, _, p0, p1) = span(c, horizontal);
        p0 < q1 && p1 > q0
    };
    let (mut left, mut right): (Option<usize>, Option<usize>) = (None, None);
    for (i, c) in snap.boxes.iter().enumerate().filter(|(_, c)| in_row(**c)) {
        let (c0, c1, _, _) = span(*c, horizontal);
        if c1 <= m0 + slack && left.map_or(true, |l| c1 > span(snap.boxes[l], horizontal).1) {
            left = Some(i);
        }
        if c0 >= m1 - slack && right.map_or(true, |r| c0 < span(snap.boxes[r], horizontal).0) {
            right = Some(i);
        }
    }
    // Drawn halfway across what the two boxes share.
    let across = |a: Box4, c: Box4| {
        let ((_, _, a0, a1), (_, _, c0, c1)) = (span(a, horizontal), span(c, horizontal));
        (a0.max(c0) + a1.min(c1)) * 0.5
    };
    let mut gaps = Vec::new();
    if let Some(l) = left {
        let l1 = span(snap.boxes[l], horizontal).1;
        gaps.push((m0 - l1, l1, m0, across(snap.boxes[l], b), true));
    }
    if let Some(r) = right {
        let r0 = span(snap.boxes[r], horizontal).0;
        gaps.push((r0 - m1, m1, r0, across(b, snap.boxes[r]), true));
    }
    for (i, j) in &snap.pairs[if horizontal { 0 } else { 1 }] {
        let (a, c) = (snap.boxes[*i], snap.boxes[*j]);
        // The pair the dragged box sits between has no gap of its own any more.
        if in_row(a) && in_row(c) && !(left == Some(*i) && right == Some(*j)) {
            let (a1, c0) = (span(a, horizontal).1, span(c, horizontal).0);
            gaps.push((c0 - a1, a1, c0, across(a, c), false));
        }
    }
    (left, right, gaps)
}

/// How far to move `b` along one axis so that a gap beside it equals another gap in its row:
/// centred between its two neighbours, or as far from one as two others are from each other.
fn spacing_snap(snap: &Snap, b: Box4, horizontal: bool, tol: f32) -> Option<f32> {
    let (left, right, gaps) = gaps_around(snap, b, horizontal, tol);
    let (m0, m1, _, _) = span(b, horizontal);
    let mut best: Option<f32> = None;
    let mut offer = |d: f32| {
        if d.abs() <= tol && best.map_or(true, |b| d.abs() < b.abs()) {
            best = Some(d);
        }
    };
    let (l1, r0) = (left.map(|l| span(snap.boxes[l], horizontal).1), right.map(|r| span(snap.boxes[r], horizontal).0));
    if let (Some(l1), Some(r0)) = (l1, r0) {
        let room = r0 - l1 - (m1 - m0);
        if room > 1.0 {
            offer(l1 + room * 0.5 - m0);
        }
    }
    for g in gaps.iter().filter(|g| !g.4 && g.0 > 0.5).map(|g| g.0) {
        if let Some(l1) = l1 {
            offer(l1 + g - m0);
        }
        if let Some(r0) = r0 {
            offer(r0 - g - m1);
        }
    }
    best
}

/// Markers for the gaps beside `b` that equal another gap in its row, and for those others.
fn gap_marks(snap: &Snap, b: Box4, horizontal: bool, out: &mut Vec<GapMark>) {
    let (_, _, gaps) = gaps_around(snap, b, horizontal, SAME_GAP);
    let mut show = vec![false; gaps.len()];
    for (i, own) in gaps.iter().enumerate().filter(|(_, g)| g.4 && g.0 > 0.5) {
        for (j, other) in gaps.iter().enumerate() {
            if j != i && (other.0 - own.0).abs() < SAME_GAP {
                (show[i], show[j]) = (true, true);
            }
        }
    }
    out.extend(gaps.iter().zip(show).filter(|(_, s)| *s).map(|(g, _)| GapMark { vertical: !horizontal, lo: g.1, hi: g.2, at: g.3 }));
}

impl App {
    /// Kumpulkan garis kandidat (tepi dan tengah objek lain) saat mulai menggeser.
    pub(super) fn begin_snap(&mut self) {
        self.ext.snap = None;
        self.ext.guides.clear();
        self.ext.gaps.clear();
        let ids = self.roots();
        let Some(orig) = ids.iter().filter_map(|id| self.doc.find(*id).map(|n| n.aabb())).reduce(union) else { return };
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
        let mut boxes: Vec<Box4> = Vec::new();
        let mut add = |b: (f32, f32, f32, f32)| {
            for x in [b.0, (b.0 + b.2) * 0.5, b.2] {
                xs.push((x, b.1, b.3));
            }
            for y in [b.1, (b.1 + b.3) * 0.5, b.3] {
                ys.push((y, b.0, b.2));
            }
        };
        let parent = ids.first().and_then(|id| self.doc.parent_of(*id)).and_then(|p| self.doc.find(p));
        match parent {
            // Di dalam frame: frame itu sendiri (tepi + tengah) dan saudara-saudaranya.
            Some(frame) => {
                add(frame.bounds());
                for c in frame.children.iter().filter(|c| c.visible && !ids.contains(&c.id)) {
                    add(c.aabb());
                    boxes.push(c.aabb());
                }
            }
            // Level atas: objek lain yang terlihat di layar.
            None => {
                let o = self.canvas_rect.min;
                let (tl, br) = (self.view.to_world(o, self.canvas_rect.min), self.view.to_world(o, self.canvas_rect.max));
                let visible = |n: &&std::sync::Arc<Node>| {
                    let b = n.aabb();
                    n.visible && !ids.contains(&n.id) && b.2 >= tl.x && b.0 <= br.x && b.3 >= tl.y && b.1 <= br.y
                };
                for n in self.doc.nodes.iter().filter(visible).take(300) {
                    add(n.aabb());
                    boxes.push(n.aabb());
                }
            }
        }
        // Guide dari penggaris juga jadi kandidat tempel.
        if self.rulers.show {
            for g in &self.doc.guides {
                if g.vertical {
                    xs.push((g.at, orig.1, orig.3));
                } else {
                    ys.push((g.at, orig.0, orig.2));
                }
            }
        }
        if boxes.len() > SPACING_BOXES {
            let c = ((orig.0 + orig.2) * 0.5, (orig.1 + orig.3) * 0.5);
            let far = |b: &Box4| ((b.0 + b.2) * 0.5 - c.0).abs() + ((b.1 + b.3) * 0.5 - c.1).abs();
            boxes.sort_by(|a, b| far(a).total_cmp(&far(b)));
            boxes.truncate(SPACING_BOXES);
        }
        let pairs = [next_pairs(&boxes, true), next_pairs(&boxes, false)];
        self.ext.snap = Some(Snap { orig, xs, ys, boxes, pairs });
    }

    /// Sesuaikan pergeseran `total` supaya tepi/tengah seleksi menempel ke kandidat terdekat,
    /// dan catat garis bantu yang perlu digambar.
    pub(super) fn snap_move(&mut self, total: Vec2) -> Vec2 {
        self.ext.guides.clear();
        self.ext.gaps.clear();
        let Some(snap) = self.ext.snap.take() else { return total };
        let tol = 6.0 / self.view.zoom;
        let o = snap.orig;
        let nearest = |mine: [f32; 3], cands: &[(f32, f32, f32)]| {
            let mut best: Option<f32> = None;
            for c in cands {
                for m in mine {
                    let d = c.0 - m;
                    if d.abs() <= tol && best.map_or(true, |b| d.abs() < b.abs()) {
                        best = Some(d);
                    }
                }
            }
            best
        };
        let b = (o.0 + total.x, o.1 + total.y, o.2 + total.x, o.3 + total.y);
        let dx = nearest([b.0, (b.0 + b.2) * 0.5, b.2], &snap.xs);
        let dy = nearest([b.1, (b.1 + b.3) * 0.5, b.3], &snap.ys);
        // Equal spacing wins where it is the nearer of the two.
        let nearer = |edge: Option<f32>, gap: Option<f32>| match (edge, gap) {
            (Some(e), Some(g)) if g.abs() < e.abs() => (None, Some(g)),
            (None, g) => (None, g),
            (e, _) => (e, None),
        };
        let (dx, gx) = nearer(dx, spacing_snap(&snap, b, true, tol));
        let (dy, gy) = nearer(dy, spacing_snap(&snap, b, false, tol));
        let out = total + vec2(dx.or(gx).unwrap_or(0.0), dy.or(gy).unwrap_or(0.0));
        let b = (o.0 + out.x, o.1 + out.y, o.2 + out.x, o.3 + out.y);
        gap_marks(&snap, b, true, &mut self.ext.gaps);
        gap_marks(&snap, b, false, &mut self.ext.gaps);
        if dx.is_some() {
            for m in [b.0, (b.0 + b.2) * 0.5, b.2] {
                let hits: Vec<_> = snap.xs.iter().filter(|c| (c.0 - m).abs() < 0.02).collect();
                if !hits.is_empty() {
                    let lo = hits.iter().map(|c| c.1).fold(b.1, f32::min);
                    let hi = hits.iter().map(|c| c.2).fold(b.3, f32::max);
                    self.ext.guides.push(Guide { vertical: true, at: m, lo, hi });
                }
            }
        }
        if dy.is_some() {
            for m in [b.1, (b.1 + b.3) * 0.5, b.3] {
                let hits: Vec<_> = snap.ys.iter().filter(|c| (c.0 - m).abs() < 0.02).collect();
                if !hits.is_empty() {
                    let lo = hits.iter().map(|c| c.1).fold(b.0, f32::min);
                    let hi = hits.iter().map(|c| c.2).fold(b.2, f32::max);
                    self.ext.guides.push(Guide { vertical: false, at: m, lo, hi });
                }
            }
        }
        self.ext.snap = Some(snap);
        out
    }

    pub(super) fn end_snap(&mut self) {
        self.ext.snap = None;
        self.ext.guides.clear();
        self.ext.gaps.clear();
    }

    pub(super) fn paint_guides(&self, painter: &egui::Painter) {
        let o = self.canvas_rect.min;
        let stroke = Stroke::new(1.0_f32, GUIDE);
        for g in &self.ext.guides {
            let (a, b) = if g.vertical {
                (self.view.to_screen(o, g.at, g.lo), self.view.to_screen(o, g.at, g.hi))
            } else {
                (self.view.to_screen(o, g.lo, g.at), self.view.to_screen(o, g.hi, g.at))
            };
            painter.line_segment([a, b], stroke);
            // Tanda silang kecil di kedua ujung.
            for p in [a, b] {
                painter.line_segment([p + vec2(-3.0, -3.0), p + vec2(3.0, 3.0)], stroke);
                painter.line_segment([p + vec2(-3.0, 3.0), p + vec2(3.0, -3.0)], stroke);
            }
        }
        // Equal spacing: each gap measured out, with its size beside it.
        for g in &self.ext.gaps {
            let (a, b, tick) = if g.vertical {
                (self.view.to_screen(o, g.at, g.lo), self.view.to_screen(o, g.at, g.hi), vec2(4.0, 0.0))
            } else {
                (self.view.to_screen(o, g.lo, g.at), self.view.to_screen(o, g.hi, g.at), vec2(0.0, 4.0))
            };
            painter.line_segment([a, b], stroke);
            for p in [a, b] {
                painter.line_segment([p - tick, p + tick], stroke);
            }
            let size = g.hi - g.lo;
            let text = if (size - size.round()).abs() < 0.05 { format!("{size:.0}") } else { format!("{size:.1}") };
            let galley = painter.layout_no_wrap(text, FontId::proportional(10.0), Color32::WHITE);
            let mid = a + (b - a) * 0.5;
            let at = if g.vertical { mid + vec2(8.0, -galley.size().y * 0.5 - 1.0) } else { mid + vec2(-galley.size().x * 0.5 - 4.0, 7.0) };
            let bg = Rect::from_min_size(at, galley.size() + vec2(8.0, 2.0));
            painter.rect_filled(bg, CornerRadius::same(3), GUIDE);
            painter.galley(bg.min + vec2(4.0, 1.0), galley, Color32::WHITE);
        }
    }

    /// Kursor tepat di luar salah satu sudut objek terpilih: zona untuk memutar.
    pub(super) fn rotate_zone(&self, p: Pos2) -> Option<u64> {
        if self.sel.len() != 1 || !self.pointer_tool() || self.editing.is_some() || self.ext.path_edit.is_some() || self.sel_locked() || self.sel_pinned() {
            return None;
        }
        if self.crop.is_some() {
            return None;
        }
        let n = self.doc.find(self.sel[0])?;
        if matches!(n.kind, Kind::Frame | Kind::Line) {
            return None;
        }
        let o = self.canvas_rect.min;
        let wp = self.view.to_world(o, p);
        let (cx, cy) = n.center();
        let (x0, y0, x1, y1) = n.bounds();
        // Di dalam kotak (ruang lokal): bukan zona rotasi.
        let (lx, ly) = rotate(wp.x, wp.y, cx, cy, -n.rotation);
        if lx >= x0 && lx <= x1 && ly >= y0 && ly <= y1 {
            return None;
        }
        [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].into_iter().find_map(|(x, y)| {
            let (rx, ry) = rotate(x, y, cx, cy, n.rotation);
            let d = (self.view.to_screen(o, rx, ry) - p).length();
            (d > 6.0 && d <= 30.0).then_some(n.id)
        })
    }

    /// Seperti `rotate_zone` untuk beberapa objek terpilih (mis. satu group): kursor tepat di luar
    /// sudut kotak seleksi. Hasil: pusat kotak (poros putar). Frame tidak bisa diputar.
    pub(super) fn multi_rotate_zone(&self, p: Pos2) -> Option<Pos2> {
        if self.sel_locked() || self.sel_pinned() || self.crop.is_some() {
            return None;
        }
        let (x0, y0, x1, y1) = self.sel_box()?;
        if self.roots().iter().any(|id| self.doc.find(*id).is_some_and(|n| n.kind == Kind::Frame)) {
            return None;
        }
        let o = self.canvas_rect.min;
        let wp = self.view.to_world(o, p);
        if wp.x >= x0 && wp.x <= x1 && wp.y >= y0 && wp.y <= y1 {
            return None;
        }
        [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].into_iter().find_map(|(x, y)| {
            let d = (self.view.to_screen(o, x, y) - p).length();
            (d > 6.0 && d <= 30.0).then_some(egui::pos2((x0 + x1) * 0.5, (y0 + y1) * 0.5))
        })
    }
}

impl App {
    /// Kursor putar: busur dengan dua kepala panah, menghadap keluar dari pusat objek.
    pub(super) fn paint_rotate_cursor(&self, painter: &egui::Painter, pos: Option<Pos2>, hovered: bool) {
        let Some(p) = pos else { return };
        let center_of = |id: u64| self.doc.find(id).map(|n| (n.center(), n.rotation));
        // Poros putar (dunia), dan sudut yang ditampilkan selama memutar.
        let ((cx, cy), angle) = match &self.drag {
            Drag::Rotate { id, .. } => match center_of(*id) {
                Some((c, rot)) => (c, Some(rot)),
                None => return,
            },
            Drag::MultiRotate { center, angle, .. } => ((center.x, center.y), Some(*angle)),
            Drag::None if hovered => match self.rotate_zone(p).and_then(center_of) {
                Some((c, _)) => (c, None),
                None => match self.multi_rotate_zone(p) {
                    Some(c) => ((c.x, c.y), None),
                    None => return,
                },
            },
            _ => return,
        };
        let dir = (p - self.view.to_screen(self.canvas_rect.min, cx, cy)).angle();
        let radius = 11.0;
        let center = p - Vec2::angled(dir) * radius;
        let span = 1.9_f32;
        let arc: Vec<Pos2> = (0..=14).map(|i| center + Vec2::angled(dir + (i as f32 / 14.0 - 0.5) * span) * radius).collect();
        let (first, last) = (arc[0], arc[14]);
        // Kepala panah mengikuti arah garis singgung di kedua ujung busur.
        let head = |tip: Pos2, tangent: f32| {
            [0.55_f32, -0.55].map(|spread| [tip, tip + Vec2::angled(tangent + std::f32::consts::PI + spread) * 5.5])
        };
        let heads = [head(first, dir - span * 0.5 - std::f32::consts::FRAC_PI_2), head(last, dir + span * 0.5 + std::f32::consts::FRAC_PI_2)];
        for (width, color) in [(4.5_f32, Color32::WHITE), (1.8, Color32::from_gray(20))] {
            painter.add(egui::Shape::line(arc.clone(), Stroke::new(width, color)));
            for h in heads.iter().flatten() {
                painter.line_segment(*h, Stroke::new(width, color));
            }
        }
        if let Some(angle) = angle {
            // Sudut saat ini di dekat kursor.
            let galley = painter.layout_no_wrap(format!("{angle:.0}°"), FontId::proportional(11.0), Color32::WHITE);
            let bg = Rect::from_min_size(p + vec2(16.0, 16.0), galley.size() + vec2(10.0, 4.0));
            painter.rect_filled(bg, CornerRadius::same(3), ACCENT);
            painter.galley(bg.min + vec2(5.0, 2.0), galley, Color32::WHITE);
        }
    }
}

// ---------------------------------------------------------------- pegangan gradasi

impl App {
    /// Node yang gradasi linearnya sedang diedit (popup fill terbuka): pegangan arahnya tampil di canvas.
    fn grad_target(&self) -> Option<&Node> {
        let id = self.ext.picker.grad_open?;
        if self.sel != [id] || !self.pointer_tool() || self.editing.is_some() || self.ext.path_edit.is_some() {
            return None;
        }
        let n = self.doc.find(id)?;
        (n.visible && n.fill.is_some() && n.gradient.is_some()).then_some(n)
    }

    /// Titik di ruang kotak pembatas (0..1) → layar, mengikuti rotasi node.
    fn grad_to_screen(&self, n: &Node, u: f32, v: f32) -> Pos2 {
        let (x0, y0, x1, y1) = n.bounds();
        let (cx, cy) = n.center();
        // A flipped node shows its gradient mirrored.
        let (fh, fv) = n.flips();
        let (u, v) = (if fh { 1.0 - u } else { u }, if fv { 1.0 - v } else { v });
        let (x, y) = rotate(x0 + (x1 - x0) * u, y0 + (y1 - y0) * v, cx, cy, n.rotation);
        self.view.to_screen(self.canvas_rect.min, x, y)
    }

    fn grad_from_screen(&self, n: &Node, p: Pos2) -> (f32, f32) {
        let (x0, y0, x1, y1) = n.bounds();
        let (cx, cy) = n.center();
        let wp = self.view.to_world(self.canvas_rect.min, p);
        let (x, y) = rotate(wp.x, wp.y, cx, cy, -n.rotation);
        let (u, v) = ((x - x0) / (x1 - x0).max(1e-3), (y - y0) / (y1 - y0).max(1e-3));
        let (fh, fv) = n.flips();
        (if fh { 1.0 - u } else { u }, if fv { 1.0 - v } else { v })
    }

    /// Posisi layar: ujung awal, ujung akhir, dan penanda tiap stop (di samping garis).
    fn grad_layout(&self, n: &Node) -> Option<(Pos2, Pos2, Vec<Pos2>)> {
        let g = n.gradient.as_ref()?;
        let [x1, y1, x2, y2] = g.line();
        let (a, b) = (self.grad_to_screen(n, x1, y1), self.grad_to_screen(n, x2, y2));
        let d = b - a;
        let side = if d.length() > 0.5 { d.normalized().rot90() } else { vec2(0.0, -1.0) };
        let stops = g.stops.iter().map(|s| a + d * s.pos.clamp(0.0, 1.0) + side * 16.0).collect();
        Some((a, b, stops))
    }

    /// Bagian pegangan gradasi di bawah kursor: 0 = ujung awal, 1 = ujung akhir, 2+i = stop ke-i.
    pub(super) fn grad_hit(&self, p: Pos2) -> Option<(u64, usize)> {
        let n = self.grad_target()?;
        let (a, b, stops) = self.grad_layout(n)?;
        let sel = self.ext.picker.stop;
        // Stop terpilih didahulukan kalau beberapa penanda bertumpuk.
        let mut order: Vec<usize> = (0..stops.len()).collect();
        order.sort_by_key(|i| *i != sel);
        if let Some(i) = order.into_iter().find(|i| stops[*i].distance(p) <= 10.0) {
            return Some((n.id, 2 + i));
        }
        if b.distance(p) <= 8.0 {
            return Some((n.id, 1));
        }
        (a.distance(p) <= 8.0).then_some((n.id, 0))
    }

    pub(super) fn grad_drag(&mut self, id: u64, part: usize, p: Pos2, shift: bool) {
        let Some(n) = self.doc.find(id) else { return };
        let Some((a, b, _)) = self.grad_layout(n) else { return };
        let mut p = p;
        if shift && part < 2 {
            // Shift: arah garis terkunci ke kelipatan 15°.
            let other = if part == 0 { b } else { a };
            let d = p - other;
            let step = 15.0_f32.to_radians();
            p = other + Vec2::angled((d.angle() / step).round() * step) * d.length();
        }
        let (u, v) = self.grad_from_screen(n, p);
        let Some(g) = self.doc.find_mut(id).and_then(|n| n.gradient.as_mut()) else { return };
        match part {
            0 | 1 => {
                let mut line = g.line();
                let k = part * 2;
                (line[k], line[k + 1]) = ((u * 1000.0).round() / 1000.0, (v * 1000.0).round() / 1000.0);
                g.set_line(line);
            }
            _ => {
                let d = b - a;
                let t = ((p - a).dot(d) / d.length_sq().max(1e-3)).clamp(0.0, 1.0);
                if let Some(s) = g.stops.get_mut(part - 2) {
                    s.pos = (t * 100.0).round() / 100.0;
                }
            }
        }
    }

    pub(super) fn paint_grad(&mut self, painter: &egui::Painter) {
        self.ext.picker.canvas_handles.clear();
        let Some(n) = self.grad_target() else { return };
        let Some((a, b, stops)) = self.grad_layout(n) else { return };
        let Some(g) = n.gradient.as_ref() else { return };
        let shadow = Color32::from_black_alpha(110);
        if g.radial {
            // Garis tepi gradasi (t = 1): elips di ruang kotak pembatas yang melewati titik akhir.
            let [x1, y1, x2, y2] = g.line();
            let r = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
            let ring: Vec<Pos2> = (0..64)
                .map(|i| {
                    let a = i as f32 / 64.0 * std::f32::consts::TAU;
                    self.grad_to_screen(n, x1 + r * a.cos(), y1 + r * a.sin())
                })
                .collect();
            painter.add(egui::Shape::closed_line(ring.clone(), Stroke::new(3.0_f32, shadow)));
            painter.add(egui::Shape::closed_line(ring, Stroke::new(1.0_f32, Color32::WHITE)));
        }
        painter.line_segment([a, b], Stroke::new(3.0_f32, shadow));
        painter.line_segment([a, b], Stroke::new(1.2_f32, Color32::WHITE));
        for end in [a, b] {
            painter.circle_filled(end, 5.0, shadow);
            painter.circle_filled(end, 4.0, Color32::WHITE);
        }
        let sel = self.ext.picker.stop;
        let mut order: Vec<usize> = (0..stops.len()).collect();
        order.sort_by_key(|i| *i == sel);
        for i in order {
            let c = g.stops[i].color;
            let sq = Rect::from_center_size(stops[i], vec2(13.0, 13.0));
            let border = if i == sel { ACCENT } else { Color32::WHITE };
            painter.rect_filled(sq.expand(3.5), egui::CornerRadius::same(5), shadow);
            painter.rect_filled(sq.expand(2.5), egui::CornerRadius::same(4), border);
            painter.rect_filled(sq, egui::CornerRadius::same(2), Color32::from_rgb(c[0], c[1], c[2]));
        }
        let mut all = vec![a, b];
        all.extend(stops);
        self.ext.picker.canvas_handles = all;
    }
}

// ---------------------------------------------------------------- bench

impl App {
    /// `KANVAS_BENCH_FLIP`: a frame with an image, a text, a gradient shape and a vector, each
    /// beside a copy flipped left-right and one flipped top-bottom (flipped the way a user does).
    pub(super) fn bench_flip(&mut self) {
        let (x, y) = (-1500.0, 3200.0);
        // An "F" on a yellow-to-orange ground: the same from no other side.
        let mut pm = resvg::tiny_skia::Pixmap::new(160, 100).unwrap();
        for (i, px) in pm.pixels_mut().iter_mut().enumerate() {
            let (px_x, px_y) = (i % 160, i / 160);
            let bar = |x0: usize, x1: usize, y0: usize, y1: usize| (x0..x1).contains(&px_x) && (y0..y1).contains(&px_y);
            let c = if bar(30, 50, 15, 85) || bar(30, 110, 15, 33) || bar(30, 90, 43, 58) { (40, 40, 90) } else { (255, 210 - px_x as u8, 60) };
            *px = resvg::tiny_skia::PremultipliedColorU8::from_rgba(c.0, c.1, c.2, 255).unwrap();
        }
        let asset = self.doc.add_asset(pm.encode_png().unwrap_or_default());
        let mut make = |kind: Kind, name: &str, (nx, ny, w, h): (f32, f32, f32, f32), fill: Option<[u8; 4]>| {
            let mut n = Node::new(self.doc.alloc_id(), kind, x + nx, y + ny);
            (n.name, n.w, n.h, n.fill) = (name.into(), w, h, fill);
            n
        };
        let stage = make(Kind::Frame, "Flip demo", (0.0, 0.0, 900.0, 640.0), Some([250, 250, 250, 255]));
        let mut image = make(Kind::Image, "Picture", (60.0, 40.0, 160.0, 100.0), None);
        (image.image, image.crop, image.radius) = (Some(asset), Some([0.0, 0.0, 0.85, 1.0]), 14.0);
        image.w = 136.0;
        let mut text = make(Kind::Text, "Words", (60.0, 190.0, 200.0, 60.0), Some([20, 40, 90, 255]));
        (text.text, text.font_size, text.weight) = ("Flip me".into(), 46.0, 700);
        let mut shape = make(Kind::Rect, "Gradient", (60.0, 320.0, 200.0, 110.0), Some([13, 153, 255, 255]));
        let stops = vec![crate::doc::Stop { pos: 0.0, color: [13, 153, 255, 255] }, crate::doc::Stop { pos: 1.0, color: [255, 102, 196, 255] }];
        shape.gradient = Some(crate::doc::Gradient { stops, line: Some([0.0, 0.0, 1.0, 1.0]), ..crate::doc::Gradient::from_color([0, 0, 0, 255]) });
        (shape.radius, shape.stroke, shape.stroke_w, shape.dash) = (20.0, Some([20, 40, 90, 255]), 3.0, 14.0);
        let mut arrow = make(Kind::Path, "Arrow", (60.0, 480.0, 200.0, 110.0), Some([20, 174, 92, 255]));
        let p = |x: f32, y: f32| crate::doc::PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        arrow.points = vec![p(0.0, 0.2), p(0.6, 0.2), p(0.6, 0.0), p(1.0, 0.6), p(0.6, 1.0), p(0.6, 0.6), p(0.0, 0.6)];
        (arrow.closed, arrow.rotation) = (true, -8.0);
        let (stage_id, b) = (stage.id, stage.bounds());
        self.doc.insert(stage, None);
        let mut picked = Vec::new();
        for n in [image, text, shape, arrow] {
            // The copies: one flipped left-right, one top-bottom, each through `flip` on a selection.
            for (col, horizontal) in [(1.0_f32, Some(true)), (2.0, Some(false)), (0.0, None)] {
                let mut copy = n.clone();
                copy.id = self.doc.alloc_id();
                copy.translate(col * 290.0, 0.0);
                let id = copy.id;
                self.doc.insert(copy, Some(stage_id));
                if let Some(horizontal) = horizontal {
                    self.sel = vec![id];
                    self.flip(horizontal);
                    picked.push(id);
                }
            }
        }
        self.zoom_to((b.0 - 40.0, b.1 - 40.0, b.2 + 40.0, b.3 + 40.0), 1.0);
        self.fit_pending = false;
        self.expanded.insert(stage_id);
        // The mirrored picture stays selected: outline, handles and panel of a flipped node.
        self.sel = picked.into_iter().take(1).collect();
    }

    /// `KANVAS_BENCH_DISTRIBUTE`: a frame with a row of uneven boxes that is distributed, a loose
    /// grid that is tidied up, and two boxes with a third one to drag beside them. Result: where
    /// to press and where to drag to (screen), three document units short of equal spacing, and
    /// the id of the dragged box.
    pub(super) fn bench_distribute(&mut self) -> (Pos2, Pos2, u64) {
        let (x, y) = (-1500.0, 4200.0);
        let mut make = |kind: Kind, name: &str, (nx, ny, w, h): (f32, f32, f32, f32), fill: [u8; 4]| {
            let mut n = Node::new(self.doc.alloc_id(), kind, x + nx, y + ny);
            (n.name, n.w, n.h, n.fill, n.radius) = (name.into(), w, h, Some(fill), 8.0);
            n
        };
        let stage = make(Kind::Frame, "Distribute demo", (0.0, 0.0, 900.0, 560.0), [250, 250, 250, 255]);
        // Uneven sizes and gaps, the ends at 60 and 840.
        let row = [(60.0, 60.0, 60.0, 70.0), (140.0, 40.0, 100.0, 90.0), (410.0, 70.0, 40.0, 50.0), (470.0, 50.0, 80.0, 80.0), (780.0, 60.0, 60.0, 60.0)].map(|r| make(Kind::Rect, "Row", r, [13, 153, 255, 255]));
        let loose = [(500.0, 400.0, 50.0, 40.0), (562.0, 408.0, 40.0, 40.0), (618.0, 403.0, 50.0, 40.0), (504.0, 470.0, 60.0, 40.0), (580.0, 464.0, 40.0, 40.0)].map(|r| make(Kind::Ellipse, "Loose", r, [151, 71, 255, 255]));
        let trio = [(60.0, 240.0, 80.0, 80.0), (190.0, 240.0, 80.0, 80.0), (560.0, 240.0, 70.0, 80.0)].map(|r| make(Kind::Rect, "Spaced", r, [255, 150, 50, 255]));
        let (stage_id, b) = (stage.id, stage.bounds());
        self.doc.insert(stage, None);
        let mut put = |list: &[Node]| -> Vec<u64> {
            list.iter().map(|n| self.doc.insert(n.clone(), Some(stage_id))).count();
            list.iter().map(|n| n.id).collect()
        };
        let (row, loose, trio) = (put(&row), put(&loose), put(&trio));
        self.zoom_to((b.0 - 40.0, b.1 - 40.0, b.2 + 40.0, b.3 + 40.0), 1.0);
        self.fit_pending = false;
        self.sel = row.clone();
        self.distribute(true);
        let edges: Vec<(f32, f32)> = row.iter().filter_map(|id| self.doc.find(*id)).map(|n| (n.x - x, n.x + n.w - x)).collect();
        eprintln!("DISTRIBUTE row after: {edges:?} (want the ends at 60 and 840, gaps of 110)");
        self.sel = loose.clone();
        self.tidy_up();
        let places: Vec<(f32, f32)> = loose.iter().filter_map(|id| self.doc.find(*id)).map(|n| (n.x - x, n.y - y)).collect();
        eprintln!("DISTRIBUTE tidied: {places:?} (want columns at 500, 575, 630 and rows at 400, 456)");
        self.sel.clear();
        let o = self.canvas_rect.min;
        // The first two are 50 apart; the third is let go 3 short of 50 from the second.
        let from = self.view.to_screen(o, x + 595.0, y + 280.0);
        let to = self.view.to_screen(o, x + 595.0 - (560.0 - 323.0), y + 280.0);
        (from, to, trio[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap_of(boxes: Vec<Box4>) -> Snap {
        let pairs = [next_pairs(&boxes, true), next_pairs(&boxes, false)];
        Snap { orig: (0.0, 0.0, 0.0, 0.0), xs: Vec::new(), ys: Vec::new(), boxes, pairs }
    }

    #[test]
    fn dragging_snaps_to_the_spacing_of_the_neighbours() {
        // Two boxes 30 apart in a row, a third far below (another row).
        let snap = snap_of(vec![(0.0, 0.0, 40.0, 40.0), (70.0, 0.0, 110.0, 40.0), (0.0, 300.0, 40.0, 340.0)]);
        assert_eq!(snap.pairs[0], vec![(0, 1)]);
        // Dragged to 33 past the second one: 3 off from making the same gap.
        let b = (143.0, 10.0, 183.0, 50.0);
        assert_eq!(spacing_snap(&snap, b, true, 6.0), Some(-3.0));
        assert_eq!(spacing_snap(&snap, b, false, 6.0), None);
        assert_eq!(spacing_snap(&snap, (160.0, 10.0, 200.0, 50.0), true, 6.0), None, "too far to snap");
        assert_eq!(spacing_snap(&snap, b, true, 2.0), None);
        let mut marks = Vec::new();
        gap_marks(&snap, (140.0, 10.0, 180.0, 50.0), true, &mut marks);
        let got: Vec<(f32, f32, f32)> = marks.iter().map(|m| (m.lo, m.hi, m.at)).collect();
        assert_eq!(got, vec![(110.0, 140.0, 25.0), (40.0, 70.0, 20.0)]);
        assert!(marks.iter().all(|m| !m.vertical));
        // Off by a pixel: nothing to show.
        marks.clear();
        gap_marks(&snap, (141.0, 10.0, 181.0, 50.0), true, &mut marks);
        assert!(marks.is_empty());
        // Between two boxes: centred, and both gaps are marked.
        let snap = snap_of(vec![(0.0, 0.0, 40.0, 40.0), (200.0, 0.0, 240.0, 40.0)]);
        assert_eq!(spacing_snap(&snap, (102.0, 0.0, 142.0, 40.0), true, 6.0), Some(-2.0));
        gap_marks(&snap, (100.0, 0.0, 140.0, 40.0), true, &mut marks);
        assert_eq!(marks.iter().map(|m| (m.lo, m.hi)).collect::<Vec<_>>(), vec![(40.0, 100.0), (140.0, 200.0)]);
        // Down a column it is the same thing along y.
        let snap = snap_of(vec![(0.0, 0.0, 40.0, 20.0), (0.0, 32.0, 40.0, 52.0)]);
        assert_eq!(spacing_snap(&snap, (5.0, 66.0, 45.0, 86.0), false, 6.0), Some(-2.0));
    }

    #[test]
    fn spacing_stays_cheap_among_hundreds_of_neighbours() {
        let boxes: Vec<Box4> = (0..SPACING_BOXES).map(|i| ((i % 20) as f32 * 50.0, (i / 20) as f32 * 50.0, (i % 20) as f32 * 50.0 + 30.0, (i / 20) as f32 * 50.0 + 30.0)).collect();
        let snap = snap_of(boxes);
        // One neighbour to the right for all but the last box of each row.
        assert_eq!(snap.pairs[0].len(), SPACING_BOXES - SPACING_BOXES / 20);
        let t = std::time::Instant::now();
        for i in 0..200 {
            let x = 1000.0 + i as f32;
            std::hint::black_box(spacing_snap(&snap, (x, 5.0, x + 30.0, 35.0), true, 6.0));
            std::hint::black_box(spacing_snap(&snap, (x, 5.0, x + 30.0, 35.0), false, 6.0));
        }
        // A frame of dragging is two of these; even a debug build stays far below a millisecond each.
        assert!(t.elapsed().as_millis() < 400, "{:?} for 400 lookups", t.elapsed());
    }
}
