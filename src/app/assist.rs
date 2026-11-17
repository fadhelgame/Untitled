//! Bantuan saat menggeser: snapping ke objek lain / frame dengan garis bantu, dan zona rotasi.

use super::render::{Guide, Snap};
use super::*;
use crate::doc::rotate;

const GUIDE: Color32 = Color32::from_rgb(255, 64, 64);

fn union(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> (f32, f32, f32, f32) {
    (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))
}

impl App {
    /// Kumpulkan garis kandidat (tepi dan tengah objek lain) saat mulai menggeser.
    pub(super) fn begin_snap(&mut self) {
        self.ext.snap = None;
        self.ext.guides.clear();
        let ids = self.roots();
        let Some(orig) = ids.iter().filter_map(|id| self.doc.find(*id).map(|n| n.aabb())).reduce(union) else { return };
        let (mut xs, mut ys) = (Vec::new(), Vec::new());
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
        self.ext.snap = Some(Snap { orig, xs, ys });
    }

    /// Sesuaikan pergeseran `total` supaya tepi/tengah seleksi menempel ke kandidat terdekat,
    /// dan catat garis bantu yang perlu digambar.
    pub(super) fn snap_move(&mut self, total: Vec2) -> Vec2 {
        self.ext.guides.clear();
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
        let out = total + vec2(dx.unwrap_or(0.0), dy.unwrap_or(0.0));
        let b = (o.0 + out.x, o.1 + out.y, o.2 + out.x, o.3 + out.y);
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
        let (x, y) = rotate(x0 + (x1 - x0) * u, y0 + (y1 - y0) * v, cx, cy, n.rotation);
        self.view.to_screen(self.canvas_rect.min, x, y)
    }

    fn grad_from_screen(&self, n: &Node, p: Pos2) -> (f32, f32) {
        let (x0, y0, x1, y1) = n.bounds();
        let (cx, cy) = n.center();
        let wp = self.view.to_world(self.canvas_rect.min, p);
        let (x, y) = rotate(wp.x, wp.y, cx, cy, -n.rotation);
        ((x - x0) / (x1 - x0).max(1e-3), (y - y0) / (y1 - y0).max(1e-3))
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
