//! Mode crop gambar, seperti Figma: klik dua kali pada gambar (atau Enter) untuk masuk.
//!
//! Kotak node adalah kotak crop; `Node::crop` menyimpan bagian gambar yang tampil di dalamnya.
//! Di mode crop gambar utuh tampil redup di balik kotak crop. Pegangan crop (siku putih) memotong
//! gambar, menarik gambar menggesernya di balik kotak, dan pegangan sudut gambar (kotak biru)
//! mengubah ukurannya. Enter / klik di luar = terapkan, Esc = batal.

use super::*;
use crate::doc::rotate;

/// Sesi crop yang sedang aktif. `orig` = keadaan gambar saat masuk (untuk batal dan undo).
pub struct Session {
    pub id: u64,
    orig: (f32, f32, f32, f32, Option<[f32; 4]>),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Part {
    /// Pegangan kotak crop (indeks `HANDLE_F`).
    Edge(u8),
    /// Pegangan sudut gambar utuh (indeks `HANDLE_F`: 0, 2, 4, 6).
    Image(u8),
    /// Badan gambar: menggeser gambar di balik kotak crop.
    Pan,
}

/// Kotak crop dan kotak gambar utuh di ruang lokal node: koordinat dunia sebelum rotasi `rot`
/// terhadap `pivot`.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Geom {
    pivot: Pos2,
    rot: f32,
    pub crop: Rect,
    pub img: Rect,
}

impl Geom {
    pub fn of(n: &Node) -> Geom {
        let (x0, y0, x1, y1) = n.bounds();
        let (ix0, iy0, ix1, iy1) = n.image_rect();
        let (cx, cy) = n.center();
        Geom {
            pivot: pos2(cx, cy),
            rot: n.rotation,
            crop: Rect::from_min_max(pos2(x0, y0), pos2(x1, y1)),
            img: Rect::from_min_max(pos2(ix0, iy0), pos2(ix1, iy1)),
        }
    }

    fn turn(&self, p: Pos2, deg: f32) -> Pos2 {
        if deg == 0.0 {
            return p;
        }
        let (x, y) = rotate(p.x, p.y, self.pivot.x, self.pivot.y, deg);
        pos2(x, y)
    }

    /// Dunia → ruang lokal.
    fn local(&self, p: Pos2) -> Pos2 {
        self.turn(p, -self.rot)
    }

    /// Ruang lokal → dunia.
    fn world(&self, p: Pos2) -> Pos2 {
        self.turn(p, self.rot)
    }

    /// Tulis ke node. Node berputar terhadap pusat kotaknya sendiri, jadi posisinya dihitung dari
    /// pusat kotak crop di dunia; dengan begitu gambar tidak bergeser saat kotak crop berubah.
    fn write(&self, n: &mut Node) {
        let c = self.world(self.crop.center());
        n.w = self.crop.width().max(1.0);
        n.h = self.crop.height().max(1.0);
        n.x = c.x - n.w * 0.5;
        n.y = c.y - n.h * 0.5;
        let (iw, ih) = (self.img.width().max(1e-3), self.img.height().max(1e-3));
        let uv = [
            ((self.crop.min.x - self.img.min.x) / iw).clamp(0.0, 1.0),
            ((self.crop.min.y - self.img.min.y) / ih).clamp(0.0, 1.0),
            ((self.crop.max.x - self.img.min.x) / iw).clamp(0.0, 1.0),
            ((self.crop.max.y - self.img.min.y) / ih).clamp(0.0, 1.0),
        ];
        let full = uv[0] < 1e-4 && uv[1] < 1e-4 && uv[2] > 1.0 - 1e-4 && uv[3] > 1.0 - 1e-4;
        n.crop = (!full).then_some(uv);
    }

    /// Geser gambar di balik kotak crop; gambar harus tetap menutupi kotak crop.
    fn pan(&mut self, d: Vec2) {
        let range = |lo: f32, hi: f32, v: f32| if lo <= hi { v.clamp(lo, hi) } else { 0.0 };
        let dx = range(self.crop.max.x - self.img.max.x, self.crop.min.x - self.img.min.x, d.x);
        let dy = range(self.crop.max.y - self.img.max.y, self.crop.min.y - self.img.min.y, d.y);
        self.img = self.img.translate(vec2(dx, dy));
    }

    /// Tarik pegangan kotak crop `h` sejauh `d`. Kotak crop tidak bisa keluar dari gambar.
    /// `keep`: rasio kotak crop terjaga (pegangan sudut).
    fn drag_edge(&mut self, h: u8, d: Vec2, keep: bool) {
        let (c, im) = (self.crop, self.img);
        let snap = |v: f32| if self.rot == 0.0 { v.round() } else { v };
        let (mut l, mut t, mut r, mut b) = (c.min.x, c.min.y, c.max.x, c.max.y);
        if matches!(h, 0 | 6 | 7) {
            l = snap(l + d.x).min(r - 1.0).max(im.min.x);
        }
        if matches!(h, 2 | 3 | 4) {
            r = snap(r + d.x).max(l + 1.0).min(im.max.x);
        }
        if matches!(h, 0 | 1 | 2) {
            t = snap(t + d.y).min(b - 1.0).max(im.min.y);
        }
        if matches!(h, 4 | 5 | 6) {
            b = snap(b + d.y).max(t + 1.0).min(im.max.y);
        }
        if keep && matches!(h, 0 | 2 | 4 | 6) && c.width() > 0.0 && c.height() > 0.0 {
            // Sudut seberang jadi jangkar; skala dibatasi jarak jangkar ke tepi gambar.
            let room_x = if matches!(h, 0 | 6) { c.max.x - im.min.x } else { im.max.x - c.min.x };
            let room_y = if matches!(h, 0 | 2) { c.max.y - im.min.y } else { im.max.y - c.min.y };
            let s = ((r - l) / c.width()).max((b - t) / c.height()).min(room_x / c.width()).min(room_y / c.height());
            let (w, hh) = (c.width() * s, c.height() * s);
            if matches!(h, 0 | 6) {
                l = r - w;
            } else {
                r = l + w;
            }
            if matches!(h, 0 | 2) {
                t = b - hh;
            } else {
                b = t + hh;
            }
        }
        self.crop = Rect::from_min_max(pos2(l, t), pos2(r, b));
    }

    /// Tarik pegangan sudut gambar `h` sejauh `d`: gambar membesar/mengecil seragam terhadap sudut
    /// seberangnya, dan tidak bisa lebih kecil dari yang dibutuhkan untuk menutupi kotak crop.
    fn drag_image(&mut self, h: u8, d: Vec2) {
        let (c, im) = (self.crop, self.img);
        let (fx, fy) = HANDLE_F[h as usize % 8];
        let (iw, ih) = (im.width().max(1e-3), im.height().max(1e-3));
        let anchor = pos2(im.min.x + iw * (1.0 - fx), im.min.y + ih * (1.0 - fy));
        let grab = pos2(im.min.x + iw * fx, im.min.y + ih * fy);
        let to = grab + d;
        let s = ((to.x - anchor.x) / (grab.x - anchor.x)).max((to.y - anchor.y) / (grab.y - anchor.y));
        let need_x = if fx == 0.0 { anchor.x - c.min.x } else { c.max.x - anchor.x } / iw;
        let need_y = if fy == 0.0 { anchor.y - c.min.y } else { c.max.y - anchor.y } / ih;
        let s = s.max(need_x).max(need_y).clamp(0.01, 100.0);
        let (w, hh) = (iw * s, ih * s);
        let min = pos2(if fx == 0.0 { anchor.x - w } else { anchor.x }, if fy == 0.0 { anchor.y - hh } else { anchor.y });
        self.img = Rect::from_min_size(min, vec2(w, hh));
    }

    /// Kotak crop terbesar dengan rasio `ratio` (lebar / tinggi) di dalam gambar, sedekat mungkin
    /// dengan pusat kotak crop sekarang.
    fn fit_ratio(&mut self, ratio: f32) {
        let im = self.img;
        let w = im.width().min(im.height() * ratio);
        let h = w / ratio;
        let c = self.crop.center();
        let x = (c.x - w * 0.5).min(im.max.x - w).max(im.min.x);
        let y = (c.y - h * 0.5).min(im.max.y - h).max(im.min.y);
        self.crop = Rect::from_min_size(pos2(x, y), vec2(w, h));
    }
}

const RATIOS: [(&str, f32); 7] =
    [("1:1", 1.0), ("4:3", 4.0 / 3.0), ("3:2", 1.5), ("16:9", 16.0 / 9.0), ("3:4", 0.75), ("2:3", 2.0 / 3.0), ("9:16", 9.0 / 16.0)];

impl App {
    pub(super) fn crop_id(&self) -> Option<u64> {
        self.crop.as_ref().map(|s| s.id)
    }

    /// Masuk mode crop untuk gambar `id`.
    pub(super) fn start_crop(&mut self, id: u64) {
        let Some(n) = self.doc.find(id).filter(|n| n.kind == Kind::Image && n.image.is_some()) else { return };
        if self.locked(id) {
            return;
        }
        self.crop = Some(Session { id, orig: (n.x, n.y, n.w, n.h, n.crop) });
        self.sel = vec![id];
        self.tool = Tool::Select;
        self.status = "Crop: drag the handles to crop, drag the image to reposition it. Enter applies, Esc cancels.".into();
    }

    /// Keluar dari mode crop. `apply`: perubahan dipertahankan (satu langkah undo); kalau tidak,
    /// gambar dikembalikan seperti saat masuk.
    pub(super) fn finish_crop(&mut self, apply: bool) {
        let Some(s) = self.crop.take() else { return };
        if matches!(self.drag, Drag::Crop { .. }) {
            self.drag = Drag::None;
            self.pre_drag = None;
        }
        let restore = |n: &mut Node| (n.x, n.y, n.w, n.h, n.crop) = s.orig;
        let Some(n) = self.doc.find(s.id) else { return };
        if (n.x, n.y, n.w, n.h, n.crop) == s.orig {
            self.status = "Crop unchanged.".into();
            return;
        }
        if apply {
            // Snapshot undo = dokumen sekarang dengan gambar ini seperti sebelum di-crop.
            let mut pre = self.doc.clone();
            if let Some(n) = pre.find_mut(s.id) {
                restore(n);
            }
            self.push_undo(pre);
            self.status = "Image cropped. Double-click the image to adjust the crop.".into();
        } else {
            if let Some(n) = self.doc.find_mut(s.id) {
                restore(n);
            }
            self.ext.rev += 1;
            self.status = "Crop cancelled.".into();
        }
    }

    fn crop_geom(&self) -> Option<Geom> {
        self.doc.find(self.crop_id()?).map(Geom::of)
    }

    /// Ubah geometri crop gambar yang sedang di-crop.
    fn crop_set(&mut self, g: Geom) {
        if let Some(n) = self.crop_id().and_then(|id| self.doc.find_mut(id)) {
            g.write(n);
        }
    }

    /// Bagian mode crop di bawah kursor (posisi layar).
    pub(super) fn crop_hit(&self, p: Pos2) -> Option<(Part, Geom)> {
        let g = self.crop_geom()?;
        let o = self.canvas_rect.min;
        let near = |q: Pos2| {
            let w = g.world(q);
            let s = self.view.to_screen(o, w.x, w.y);
            (s.x - p.x).abs() <= 8.0 && (s.y - p.y).abs() <= 8.0
        };
        let at = |r: Rect, i: usize| r.min + vec2(r.width() * HANDLE_F[i].0, r.height() * HANDLE_F[i].1);
        // Sudut dulu, baru sisi; pegangan crop menang atas pegangan gambar.
        for i in [0, 2, 4, 6, 1, 3, 5, 7] {
            if near(at(g.crop, i)) {
                return Some((Part::Edge(i as u8), g));
            }
        }
        for i in [0, 2, 4, 6] {
            if near(at(g.img, i)) {
                return Some((Part::Image(i as u8), g));
            }
        }
        // Sepanjang tepi kotak crop juga bisa ditarik.
        let lp = g.local(self.view.to_world(o, p));
        let tol = 5.0 / self.view.zoom;
        let c = g.crop;
        if c.expand(tol).contains(lp) {
            let edge = [(lp.y - c.min.y).abs(), (lp.x - c.max.x).abs(), (lp.y - c.max.y).abs(), (lp.x - c.min.x).abs()];
            if let Some(i) = (0..4).filter(|i| edge[*i] <= tol).min_by(|a, b| edge[*a].total_cmp(&edge[*b])) {
                return Some((Part::Edge(i as u8 * 2 + 1), g));
            }
        }
        g.img.contains(lp).then_some((Part::Pan, g))
    }

    /// Mouse ditekan di canvas saat mode crop.
    pub(super) fn crop_press(&mut self, p: Pos2, wp: Pos2) {
        match self.crop_hit(p) {
            Some((part, geom)) => self.drag = Drag::Crop { part, start: wp, geom },
            // Klik di luar gambar: selesai, seperti Figma.
            None => self.finish_crop(true),
        }
    }

    pub(super) fn crop_drag(&mut self, part: Part, start: Pos2, geom: Geom, wp: Pos2, shift: bool) {
        let mut g = geom;
        let d = g.local(wp) - g.local(start);
        match part {
            Part::Pan => g.pan(if g.rot == 0.0 { vec2(d.x.round(), d.y.round()) } else { d }),
            Part::Edge(h) => g.drag_edge(h, d, shift),
            Part::Image(h) => g.drag_image(h, d),
        }
        self.crop_set(g);
    }

    /// Panah di mode crop: geser gambar di balik kotak crop.
    pub(super) fn crop_nudge(&mut self, dx: f32, dy: f32) -> bool {
        let Some(mut g) = self.crop_geom() else { return false };
        g.pan(vec2(dx, dy));
        self.crop_set(g);
        true
    }

    /// Kotak crop kembali ke seluruh gambar (`None`) atau ke rasio tertentu.
    fn crop_ratio(&mut self, ratio: Option<f32>) {
        let Some(mut g) = self.crop_geom() else { return };
        match ratio {
            Some(r) => g.fit_ratio(r),
            None => g.crop = g.img,
        }
        self.crop_set(g);
    }

    pub(super) fn crop_cursor(&self, p: Option<Pos2>) -> Option<CursorIcon> {
        self.crop.as_ref()?;
        if let Drag::Crop { part, .. } = &self.drag {
            return Some(part_cursor(*part, true));
        }
        Some(p.and_then(|p| self.crop_hit(p)).map_or(CursorIcon::Default, |(part, _)| part_cursor(part, false)))
    }

    /// Overlay mode crop: garis gambar utuh dengan pegangan sudutnya, dan pegangan siku kotak crop.
    pub(super) fn paint_crop(&self, painter: &egui::Painter) {
        let Some(g) = self.crop_geom() else { return };
        let o = self.canvas_rect.min;
        let scr = |q: Pos2| {
            let w = g.world(q);
            self.view.to_screen(o, w.x, w.y)
        };
        let corners = |r: Rect| [scr(r.left_top()), scr(r.right_top()), scr(r.right_bottom()), scr(r.left_bottom())];
        let (img, crop) = (corners(g.img), corners(g.crop));

        painter.add(egui::Shape::closed_line(img.to_vec(), Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.8))));
        for p in img {
            let hr = Rect::from_center_size(p, vec2(8.0, 8.0));
            painter.rect_filled(hr, CornerRadius::same(1), ACCENT);
            painter.rect_stroke(hr, CornerRadius::same(1), Stroke::new(1.0_f32, Color32::WHITE), StrokeKind::Inside);
        }

        // Garis sepertiga selama menarik.
        if matches!(self.drag, Drag::Crop { .. }) {
            let thin = Stroke::new(1.0_f32, Color32::from_white_alpha(90));
            for t in [1.0 / 3.0, 2.0 / 3.0] {
                painter.line_segment([crop[0].lerp(crop[1], t), crop[3].lerp(crop[2], t)], thin);
                painter.line_segment([crop[0].lerp(crop[3], t), crop[1].lerp(crop[2], t)], thin);
            }
        }
        painter.add(egui::Shape::closed_line(crop.to_vec(), Stroke::new(1.0_f32, Color32::WHITE)));

        // Pegangan crop: siku di tiap sudut dan batang di tengah tiap sisi, putih bertepi gelap.
        let (dark, light) = (Stroke::new(5.0_f32, Color32::from_black_alpha(170)), Stroke::new(3.0_f32, Color32::WHITE));
        let mut marks: Vec<[Pos2; 2]> = Vec::new();
        for i in 0..4 {
            let (a, next, prev) = (crop[i], crop[(i + 1) % 4], crop[(i + 3) % 4]);
            for other in [next, prev] {
                let len = (other - a).length();
                if len > 1.0 {
                    marks.push([a, a + (other - a) / len * (len * 0.5).min(14.0)]);
                }
            }
            let (mid, len) = (a.lerp(next, 0.5), (next - a).length());
            if len > 44.0 {
                let half = (next - a) / len * 9.0;
                marks.push([mid - half, mid + half]);
            }
        }
        for stroke in [dark, light] {
            for m in &marks {
                painter.line_segment(*m, stroke);
            }
        }
    }

    /// Bar mengambang mode crop: rasio, reset, batal, terapkan.
    pub(super) fn crop_bar(&mut self, ctx: &egui::Context) {
        if self.crop.is_none() {
            return;
        }
        enum Act {
            Ratio(Option<f32>),
            Finish(bool),
        }
        let mut act = None;
        let c = self.canvas_rect;
        egui::Area::new(egui::Id::new("kanvas_crop_bar"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(c.center().x, c.bottom() - 82.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::CARD)
                    .corner_radius(CornerRadius::same(12))
                    .stroke(Stroke::new(1.0_f32, Color32::from_gray(66)))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(110) })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 8.0;
                            ui.label(egui::RichText::new("Crop").strong().color(Color32::WHITE));
                            ui.separator();
                            ui.menu_button("Ratio ⏷", |ui| {
                                if ui.button("Original").clicked() {
                                    act = Some(Act::Ratio(None));
                                    ui.close_menu();
                                }
                                ui.separator();
                                for (name, r) in RATIOS {
                                    if ui.button(name).clicked() {
                                        act = Some(Act::Ratio(Some(r)));
                                        ui.close_menu();
                                    }
                                }
                            })
                            .response
                            .on_hover_text("Crop to an aspect ratio");
                            if ui.button("Reset").on_hover_text("Show the whole image again").clicked() {
                                act = Some(Act::Ratio(None));
                            }
                            ui.separator();
                            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                                act = Some(Act::Finish(false));
                            }
                            let apply = egui::Button::new(egui::RichText::new("✔ Apply").color(Color32::WHITE)).fill(ACCENT);
                            if ui.add(apply).on_hover_text("Enter").clicked() {
                                act = Some(Act::Finish(true));
                            }
                        });
                    });
            });
        match act {
            Some(Act::Ratio(r)) => self.crop_ratio(r),
            Some(Act::Finish(apply)) => self.finish_crop(apply),
            None => {}
        }
    }
}

fn part_cursor(part: Part, dragging: bool) -> CursorIcon {
    match part {
        Part::Pan if dragging => CursorIcon::Grabbing,
        Part::Pan => CursorIcon::Grab,
        Part::Edge(h) | Part::Image(h) => match h {
            0 | 4 => CursorIcon::ResizeNwSe,
            2 | 6 => CursorIcon::ResizeNeSw,
            1 | 5 => CursorIcon::ResizeVertical,
            _ => CursorIcon::ResizeHorizontal,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(rot: f32) -> Node {
        let mut n = Node::new(1, Kind::Image, 100.0, 50.0);
        (n.w, n.h, n.rotation) = (400.0, 200.0, rot);
        n
    }

    /// Posisi dunia sudut kiri-atas gambar utuh.
    fn image_origin(n: &Node) -> Pos2 {
        let g = Geom::of(n);
        g.world(g.img.min)
    }

    #[test]
    fn cropping_keeps_the_image_in_place() {
        for rot in [0.0, 30.0] {
            let mut n = image(rot);
            let before = image_origin(&n);
            let mut g = Geom::of(&n);
            g.drag_edge(0, vec2(100.0, 40.0), false);
            g.write(&mut n);
            assert_eq!((n.w, n.h), (300.0, 160.0));
            let uv = n.crop.unwrap();
            assert!((uv[0] - 0.25).abs() < 1e-4 && (uv[1] - 0.2).abs() < 1e-4 && uv[2] == 1.0 && uv[3] == 1.0);
            let after = image_origin(&n);
            assert!((after - before).length() < 1e-2, "rot {rot}: {before:?} -> {after:?}");
            // Kotak gambar utuh yang diturunkan dari node tetap berukuran semula.
            let g = Geom::of(&n);
            assert!((g.img.width() - 400.0).abs() < 1e-2 && (g.img.height() - 200.0).abs() < 1e-2);
        }
    }

    #[test]
    fn crop_box_stays_inside_the_image() {
        let mut g = Geom::of(&image(0.0));
        g.drag_edge(4, vec2(500.0, 500.0), false);
        assert_eq!(g.crop, g.img);
        g.drag_edge(0, vec2(900.0, 900.0), false);
        assert_eq!(g.crop.size(), vec2(1.0, 1.0));
    }

    #[test]
    fn shift_keeps_the_crop_ratio() {
        let mut g = Geom::of(&image(0.0));
        g.drag_edge(4, vec2(-100.0, -10.0), true);
        assert!((g.crop.width() / g.crop.height() - 2.0).abs() < 1e-3);
        assert_eq!(g.crop.min, g.img.min);
    }

    #[test]
    fn image_always_covers_the_crop_box() {
        let mut n = image(0.0);
        let mut g = Geom::of(&n);
        g.drag_edge(4, vec2(-200.0, -100.0), false);
        // Geser dan perkecil gambar sejauh mungkin: kotak crop tetap tertutup.
        g.pan(vec2(-1000.0, 1000.0));
        assert!(g.img.contains_rect(g.crop));
        assert_eq!(g.img.max.x, g.crop.max.x);
        g.drag_image(0, vec2(1000.0, 1000.0));
        assert!(g.img.expand(1e-3).contains_rect(g.crop));
        g.write(&mut n);
        assert_eq!((n.w, n.h), (200.0, 100.0));
        // Menutupi persis = tidak di-crop lagi.
        let mut full = Geom::of(&n);
        full.crop = full.img;
        full.write(&mut n);
        assert_eq!(n.crop, None);
    }

    #[test]
    fn ratio_presets_fit_inside_the_image() {
        let mut g = Geom::of(&image(0.0));
        g.fit_ratio(1.0);
        assert_eq!(g.crop.size(), vec2(200.0, 200.0));
        assert_eq!(g.crop.center(), g.img.center());
    }
}
