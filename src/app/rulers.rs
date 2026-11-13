//! Rulers dan guides: penggaris di tepi atas dan kiri canvas (Shift+R), dan garis bantu yang
//! ditarik keluar dari penggaris. Guide disimpan di dokumen (koordinat dunia), bisa digeser lagi
//! dengan Move tool, dan dihapus dengan menariknya kembali ke penggaris. Objek yang digeser
//! menempel ke guide lewat mekanisme snapping yang sama dengan tepi objek lain.

use super::*;
use crate::doc::RulerGuide;

/// Tebal penggaris (poin layar).
const SIZE: f32 = 20.0;
/// Jarak pegang guide dari kursor (poin layar).
const GRAB: f32 = 4.0;
const BG: Color32 = Color32::from_rgb(30, 30, 33);
const LINE: Color32 = Color32::from_rgb(64, 200, 255);

#[derive(Default)]
pub(super) struct Rulers {
    pub show: bool,
    drag: Option<GuideDrag>,
}

/// Guide yang sedang ditarik: baru (dari penggaris) atau yang sudah ada (`index`).
struct GuideDrag {
    index: Option<usize>,
    vertical: bool,
    at: f32,
}

/// Jarak antar angka di penggaris: bilangan "bulat" (1, 2, 5 × 10ⁿ) yang di layar ±64 poin.
fn step_for(zoom: f32) -> f32 {
    let raw = 64.0 / zoom.max(1e-6);
    let pow = 10f32.powf(raw.log10().floor());
    let m = raw / pow;
    pow * if m <= 1.0 {
        1.0
    } else if m <= 2.0 {
        2.0
    } else if m <= 5.0 {
        5.0
    } else {
        10.0
    }
}

fn label(v: f32, step: f32) -> String {
    if step >= 1.0 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}")
    }
}

impl App {
    pub(super) fn toggle_rulers(&mut self) {
        self.rulers.show = !self.rulers.show;
        self.rulers.drag = None;
        self.status = if self.rulers.show { "Rulers on. Drag from a ruler to add a guide.".into() } else { "Rulers and guides hidden.".into() };
    }

    /// Guide di bawah kursor (hanya dengan Move tool, supaya tool lain tidak terganggu).
    fn guide_at(&self, p: Pos2) -> Option<usize> {
        if !self.rulers.show || self.tool != Tool::Select || self.editing.is_some() || self.ext.path_edit.is_some() {
            return None;
        }
        let o = self.canvas_rect.min;
        self.doc.guides.iter().position(|g| {
            let s = self.view.to_screen(o, g.at, g.at);
            (if g.vertical { p.x - s.x } else { p.y - s.y }).abs() <= GRAB
        })
    }

    /// Interaksi penggaris dan guide. Hasil `true`: pointer sudah ditangani di sini, jangan
    /// diteruskan ke interaksi canvas biasa.
    pub(super) fn ruler_pointer(&mut self, p: Pos2, inp: &Inp, hovered: bool) -> bool {
        if !self.rulers.show {
            return false;
        }
        let rect = self.canvas_rect;
        let wp = self.view.to_world(rect.min, p);
        let (in_top, in_left) = (p.y < rect.top() + SIZE, p.x < rect.left() + SIZE);
        if let Some(drag) = &mut self.rulers.drag {
            drag.at = (if drag.vertical { wp.x } else { wp.y }).round();
            if inp.down {
                return true;
            }
            // Dilepas di atas penggaris asalnya (atau di luar canvas): guide dibuang.
            let Some(drag) = self.rulers.drag.take() else { return true };
            let discard = !rect.contains(p) || if drag.vertical { in_left } else { in_top };
            let pre = self.doc.clone();
            let axis = if drag.vertical { "x" } else { "y" };
            self.status = match (drag.index, discard) {
                (Some(i), true) if i < self.doc.guides.len() => {
                    self.doc.guides.remove(i);
                    "Guide removed.".to_owned()
                }
                (Some(i), false) => match self.doc.guides.get_mut(i) {
                    Some(g) if g.at != drag.at => {
                        g.at = drag.at;
                        format!("Guide moved to {axis} = {}.", drag.at)
                    }
                    _ => return true,
                },
                (None, false) => {
                    self.doc.guides.push(RulerGuide { vertical: drag.vertical, at: drag.at });
                    format!("Guide added at {axis} = {}. Drag it back onto the ruler to remove it.", drag.at)
                }
                _ => return true,
            };
            self.push_undo(pre);
            return true;
        }
        if !(inp.pressed && hovered && matches!(self.drag, Drag::None)) {
            return false;
        }
        if in_top || in_left {
            // Dari penggaris kiri keluar guide vertikal, dari penggaris atas guide horizontal.
            if !(in_top && in_left) {
                self.rulers.drag = Some(GuideDrag { index: None, vertical: in_left, at: (if in_left { wp.x } else { wp.y }).round() });
            }
            return true;
        }
        if self.handle_at(p).is_none() && self.multi_handle_at(p).is_none() {
            if let Some(i) = self.guide_at(p) {
                let g = self.doc.guides[i];
                self.rulers.drag = Some(GuideDrag { index: Some(i), vertical: g.vertical, at: g.at });
                return true;
            }
        }
        false
    }

    /// Garis guide di atas dokumen (di bawah overlay seleksi lain).
    pub(super) fn paint_ruler_guides(&self, painter: &egui::Painter, pos: Option<Pos2>) {
        if !self.rulers.show {
            return;
        }
        let rect = self.canvas_rect;
        let o = rect.min;
        let line = |vertical: bool, at: f32, stroke: Stroke| {
            let s = self.view.to_screen(o, at, at);
            if vertical {
                painter.vline(s.x, rect.y_range(), stroke);
            } else {
                painter.hline(rect.x_range(), s.y, stroke);
            }
        };
        let dragged = self.rulers.drag.as_ref().and_then(|d| d.index);
        let hover = if self.rulers.drag.is_none() && matches!(self.drag, Drag::None) { pos.and_then(|p| self.guide_at(p)) } else { None };
        for (i, g) in self.doc.guides.iter().enumerate() {
            if dragged != Some(i) {
                let width: f32 = if hover == Some(i) { 2.0 } else { 1.0 };
                line(g.vertical, g.at, Stroke::new(width, LINE));
            }
        }
        if let Some(d) = &self.rulers.drag {
            line(d.vertical, d.at, Stroke::new(1.0_f32, ACCENT));
            // Posisi guide di dekat kursor selama ditarik.
            if let Some(p) = pos {
                let galley = painter.layout_no_wrap(format!("{}", d.at as i64), FontId::proportional(11.0), Color32::WHITE);
                let bg = Rect::from_min_size(p + vec2(12.0, 12.0), galley.size() + vec2(10.0, 4.0));
                painter.rect_filled(bg, CornerRadius::same(3), ACCENT);
                painter.galley(bg.min + vec2(5.0, 2.0), galley, Color32::WHITE);
            }
        }
    }

    /// Penggaris atas dan kiri, digambar paling akhir (di atas dokumen dan overlay).
    pub(super) fn paint_rulers(&self, ctx: &egui::Context, painter: &egui::Painter, pos: Option<Pos2>, hovered: bool) {
        if !self.rulers.show {
            return;
        }
        let rect = self.canvas_rect;
        let o = rect.min;
        let top = Rect::from_min_max(rect.min, pos2(rect.right(), rect.top() + SIZE));
        let left = Rect::from_min_max(rect.min, pos2(rect.left() + SIZE, rect.bottom()));
        painter.rect_filled(top, CornerRadius::ZERO, BG);
        painter.rect_filled(left, CornerRadius::ZERO, BG);

        // Rentang seleksi ditandai di kedua penggaris.
        let sel = self.sel.iter().filter_map(|id| self.doc.find(*id).map(|n| n.aabb())).reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
        if let Some(b) = sel {
            let (a, c) = (self.view.to_screen(o, b.0, b.1), self.view.to_screen(o, b.2, b.3));
            let band = ACCENT.gamma_multiply(0.3);
            painter.rect_filled(Rect::from_min_max(pos2(a.x, top.top()), pos2(c.x, top.bottom())).intersect(top), CornerRadius::ZERO, band);
            painter.rect_filled(Rect::from_min_max(pos2(left.left(), a.y), pos2(left.right(), c.y)).intersect(left), CornerRadius::ZERO, band);
        }

        let step = step_for(self.view.zoom);
        let (tl, br) = (self.view.to_world(o, rect.min), self.view.to_world(o, rect.max));
        let tick = Stroke::new(1.0_f32, Color32::from_gray(96));
        let text = Color32::from_gray(160);
        let font = FontId::proportional(9.5);
        // Lima garis kecil di antara dua angka.
        let minor = step / 5.0;
        let mut k = (tl.x / minor).floor() as i64;
        while (k as f32) * minor <= br.x {
            let v = k as f32 * minor;
            let x = self.view.to_screen(o, v, 0.0).x;
            let major = k.rem_euclid(5) == 0;
            painter.vline(x, (top.bottom() - if major { SIZE } else { 5.0 })..=top.bottom(), tick);
            if major {
                painter.text(pos2(x + 3.0, top.top() + 1.0), Align2::LEFT_TOP, label(v, step), font.clone(), text);
            }
            k += 1;
        }
        let mut k = (tl.y / minor).floor() as i64;
        while (k as f32) * minor <= br.y {
            let v = k as f32 * minor;
            let y = self.view.to_screen(o, 0.0, v).y;
            let major = k.rem_euclid(5) == 0;
            painter.hline((left.right() - if major { SIZE } else { 5.0 })..=left.right(), y, tick);
            if major {
                // Angka di penggaris kiri dibaca dari bawah ke atas.
                let galley = painter.layout_no_wrap(label(v, step), font.clone(), text);
                let shape = egui::epaint::TextShape::new(pos2(left.left() + 1.0, y - 3.0), galley, text).with_angle(-std::f32::consts::FRAC_PI_2);
                painter.add(shape);
            }
            k += 1;
        }
        // Posisi kursor.
        if let Some(p) = pos.filter(|p| rect.contains(*p)) {
            let mark = Stroke::new(1.0_f32, Color32::from_gray(230));
            painter.vline(p.x, top.y_range(), mark);
            painter.hline(left.x_range(), p.y, mark);
        }
        let border = Stroke::new(1.0_f32, Color32::from_gray(58));
        painter.rect_filled(Rect::from_min_size(rect.min, vec2(SIZE, SIZE)), CornerRadius::ZERO, BG);
        painter.hline(rect.x_range(), top.bottom(), border);
        painter.vline(left.right(), rect.y_range(), border);

        // Kursor: panah dua arah saat menarik atau menyorot guide, panah biasa di atas penggaris.
        let grabbed = self.rulers.drag.as_ref().map(|d| d.vertical);
        let over = pos.filter(|_| hovered && matches!(self.drag, Drag::None));
        let on_ruler = over.is_some_and(|p| top.contains(p) || left.contains(p));
        let hover = over.filter(|p| !on_ruler && self.handle_at(*p).is_none()).and_then(|p| self.guide_at(p)).and_then(|i| self.doc.guides.get(i)).map(|g| g.vertical);
        match grabbed.or(hover) {
            Some(true) => ctx.set_cursor_icon(CursorIcon::ResizeHorizontal),
            Some(false) => ctx.set_cursor_icon(CursorIcon::ResizeVertical),
            None if on_ruler => ctx.set_cursor_icon(CursorIcon::Default),
            None => {}
        }
    }
}
