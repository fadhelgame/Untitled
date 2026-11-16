//! Widget sudut di canvas (seperti live corners di Illustrator): bulatan kecil di tiap sudut tajam
//! vector yang terpilih. Tarik = bulatkan semua sudut; Alt+tarik = hanya sudut itu.

use super::pen::geo;
use super::*;

/// Jarak minimum widget dari titik sudutnya (piksel layar), supaya tetap bisa diraih saat radius 0.
const OFFSET: f32 = 16.0;
/// Ruas yang lebih pendek dari ini di layar tidak diberi widget (terlalu rapat).
const MIN_SIDE: f32 = 30.0;

/// Satu sudut yang bisa dibulatkan, dalam koordinat dunia (rotasi node sudah diterapkan).
pub(super) struct Corner {
    /// (kontur, indeks titik); kontur 0 = kontur utama, sisanya lubang.
    pub at: (usize, usize),
    p: Pos2,
    /// Arah garis bagi sudut (vektor satuan), menuju pusat busur.
    bis: Vec2,
    sin_half: f32,
    /// Jari-jari terbesar kalau sudut tetangga ikut dibulatkan / kalau hanya sudut ini.
    max_all: f32,
    max_one: f32,
    /// Jari-jari yang sedang berlaku (sudah dibatasi).
    pub r: f32,
    /// Panjang ruas terpendek di sudut ini (satuan dunia).
    side: f32,
}

impl Corner {
    /// Posisi widget di dunia: di pusat busur, tapi tidak lebih dekat dari `OFFSET` piksel ke sudutnya.
    fn widget(&self, zoom: f32) -> Pos2 {
        self.p + self.bis * (self.r / self.sin_half).max(OFFSET / zoom)
    }
}

/// Semua sudut tajam (titik tanpa pegangan di antara dua ruas lurus) dari sebuah vector.
pub(super) fn corners(n: &Node) -> Vec<Corner> {
    // Kotak dan gambar: empat pojok dengan satu jari-jari bersama (`Node::radius`).
    if matches!(n.kind, Kind::Rect | Kind::Image) {
        let (x0, y0, x1, y1) = n.bounds();
        let (cx, cy) = n.center();
        let side = (x1 - x0).min(y1 - y0);
        let max = side * 0.5;
        let half = std::f32::consts::FRAC_1_SQRT_2;
        return [(x0, y0, 1.0, 1.0), (x1, y0, -1.0, 1.0), (x1, y1, -1.0, -1.0), (x0, y1, 1.0, -1.0)]
            .into_iter()
            .enumerate()
            .map(|(i, (x, y, dx, dy))| {
                let (px, py) = crate::doc::rotate(x, y, cx, cy, n.rotation);
                let (bx, by) = crate::doc::rotate(dx * half, dy * half, 0.0, 0.0, n.rotation);
                Corner { at: (0, i), p: pos2(px, py), bis: vec2(bx, by), sin_half: half, max_all: max, max_one: max, r: n.radius.clamp(0.0, max.max(0.0)), side }
            })
            .collect();
    }
    let zero = [0.0f32; 2];
    let g = geo(n);
    let mut out = Vec::new();
    for (c, pts) in g.cs.iter().enumerate() {
        let (len, closed) = (pts.len(), c > 0 || g.closed);
        if len < 3 {
            continue;
        }
        let sharp = |i: usize| {
            (closed || (i > 0 && i + 1 < len)) && pts[i].hin == zero && pts[i].hout == zero && pts[(i + len - 1) % len].hout == zero && pts[(i + 1) % len].hin == zero
        };
        let radius = |i: usize| pts[i].r.unwrap_or(n.radius).max(0.0);
        for i in 0..len {
            if !sharp(i) {
                continue;
            }
            let (ia, ib) = ((i + len - 1) % len, (i + 1) % len);
            let p = pos2(pts[i].x, pts[i].y);
            let (v1, v2) = (pos2(pts[ia].x, pts[ia].y) - p, pos2(pts[ib].x, pts[ib].y) - p);
            let (l1, l2) = (v1.length(), v2.length());
            if l1 < 1e-4 || l2 < 1e-4 {
                continue;
            }
            let (d1, d2) = (v1 / l1, v2 / l2);
            let cos = d1.dot(d2).clamp(-1.0, 1.0);
            if 1.0 + cos < 1e-4 || 1.0 - cos < 1e-4 {
                continue;
            }
            let tan_half = ((1.0 - cos) / (1.0 + cos)).sqrt();
            let sin_half = ((1.0 - cos) * 0.5).sqrt();
            let limit = |l: f32, j: usize| if sharp(j) && radius(j) > 0.0 { l * 0.5 } else { l };
            let max_one = limit(l1, ia).min(limit(l2, ib)) * tan_half;
            out.push(Corner {
                at: (c, i),
                p,
                bis: (d1 + d2).normalized(),
                sin_half,
                max_all: l1.min(l2) * 0.5 * tan_half,
                max_one,
                r: radius(i).min(max_one),
                side: l1.min(l2),
            });
        }
    }
    out
}

impl App {
    /// Objek yang widget sudutnya tampil: satu vector, kotak, atau gambar terpilih dengan Select/Scale tool.
    fn corner_target(&self) -> Option<&Node> {
        let [id] = self.sel[..] else { return None };
        if !self.pointer_tool() || self.editing.is_some() || self.ext.path_edit.is_some() || self.crop.is_some() || self.locked(id) {
            return None;
        }
        self.doc.find(id).filter(|n| matches!(n.kind, Kind::Path | Kind::Rect | Kind::Image) && n.visible)
    }

    /// Sudut yang widget-nya cukup lega untuk ditampilkan, dengan posisi layarnya.
    fn corner_widgets(&self) -> Vec<(Corner, Pos2)> {
        let Some(n) = self.corner_target() else { return Vec::new() };
        let (o, z) = (self.canvas_rect.min, self.view.zoom);
        let list = corners(n);
        if list.len() > 64 {
            return Vec::new();
        }
        list.into_iter()
            .filter(|c| c.side * z >= MIN_SIDE)
            .map(|c| {
                let w = c.widget(z);
                let s = self.view.to_screen(o, w.x, w.y);
                (c, s)
            })
            .collect()
    }

    /// Widget sudut di bawah kursor: (id node, sudut, jari-jari awal, posisi awal sepanjang garis bagi).
    pub(super) fn corner_hit(&self, p: Pos2) -> Option<(u64, (usize, usize), f32, f32)> {
        let id = self.corner_target()?.id;
        let wp = self.view.to_world(self.canvas_rect.min, p);
        let (c, _) = self.corner_widgets().into_iter().filter(|(_, s)| s.distance(p) <= 8.0).min_by(|a, b| a.1.distance(p).total_cmp(&b.1.distance(p)))?;
        Some((id, c.at, c.r, (wp - c.p).dot(c.bis)))
    }

    /// Menarik widget sudut. `single`: hanya sudut itu; kalau tidak, semua sudut vector ikut.
    pub(super) fn corner_drag(&mut self, id: u64, at: (usize, usize), single: bool, r0: f32, s0: f32, wp: Pos2) {
        let Some(n) = self.doc.find(id) else { return };
        let Some(c) = corners(n).into_iter().find(|c| c.at == at) else { return };
        let max = if single { c.max_one.max(c.max_all) } else { c.max_all };
        let r = (r0 + ((wp - c.p).dot(c.bis) - s0) * c.sin_half).clamp(0.0, max.max(0.0));
        let r = (r * 10.0).round() / 10.0;
        let Some(n) = self.doc.find_mut(id) else { return };
        if n.kind != Kind::Path {
            // Kotak dan gambar hanya punya satu jari-jari untuk keempat pojoknya.
            n.radius = r;
            self.status = format!("Corner radius {r:.1}.");
            return;
        }
        if single {
            let pts = if at.0 == 0 { Some(&mut n.points) } else { n.holes.get_mut(at.0 - 1) };
            if let Some(q) = pts.and_then(|v| v.get_mut(at.1)) {
                q.r = Some(r);
            }
        } else {
            n.radius = r;
            n.clear_corner_radii();
        }
        self.status = if single { format!("Corner radius {r:.1} (this corner only).") } else { format!("Corner radius {r:.1}. Hold Alt while dragging to round one corner only.") };
    }

    /// Posisi layar widget sudut pertama (untuk uji `--bench`).
    pub(super) fn bench_corner(&self) -> Option<Pos2> {
        self.corner_widgets().first().map(|(_, s)| *s)
    }

    pub(super) fn paint_corners(&self, painter: &egui::Painter, cursor: Option<Pos2>) {
        let active = match &self.drag {
            Drag::None => None,
            Drag::Corner { at, .. } => Some(*at),
            _ => return,
        };
        for (c, s) in self.corner_widgets() {
            let hot = active == Some(c.at) || (active.is_none() && cursor.is_some_and(|p| s.distance(p) <= 8.0));
            painter.circle_filled(s, 5.5, Color32::from_black_alpha(60));
            painter.circle_filled(s, 4.5, if hot { ACCENT } else { Color32::WHITE });
            painter.circle_stroke(s, 4.5, Stroke::new(1.2_f32, ACCENT));
            painter.circle_filled(s, 1.5, if hot { Color32::WHITE } else { ACCENT });
            if active == Some(c.at) {
                let galley = painter.layout_no_wrap(format!("{:.1}", c.r), FontId::proportional(11.5), Color32::WHITE);
                let bg = Rect::from_min_size(s + vec2(10.0, -22.0), galley.size() + vec2(10.0, 4.0));
                painter.rect_filled(bg, egui::CornerRadius::same(4), ACCENT);
                painter.galley(bg.min + vec2(5.0, 2.0), galley, Color32::WHITE);
            }
        }
    }
}
