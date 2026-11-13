//! Tool Polygon / star: tarik untuk membuat segi banyak beraturan atau bintang. Hasilnya path
//! tertutup biasa (bukan bentuk parametrik), jadi setelah dibuat titik-titiknya bisa diedit
//! seperti path lain.

use super::*;
use crate::doc::PathPoint;

pub(super) struct Options {
    /// Jumlah sisi (polygon) atau jumlah ujung (bintang).
    pub sides: u32,
    pub star: bool,
    /// Bintang: jari-jari lekukan dalam sebagai pecahan jari-jari ujungnya.
    pub inner: f32,
}

impl Default for Options {
    fn default() -> Self {
        Options { sides: 5, star: false, inner: 0.45 }
    }
}

/// Titik-titik bentuk, dinormalkan ke kotak pembatasnya (0..1) dengan satu ujung menghadap atas.
pub(super) fn points(o: &Options) -> Vec<PathPoint> {
    let sides = o.sides.clamp(3, 60) as usize;
    let count = if o.star { sides * 2 } else { sides };
    let raw: Vec<(f32, f32)> = (0..count)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::TAU * i as f32 / count as f32;
            let r = if o.star && i % 2 == 1 { o.inner.clamp(0.05, 0.95) } else { 1.0 };
            (r * a.cos(), r * a.sin())
        })
        .collect();
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for &(x, y) in &raw {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let (w, h) = ((x1 - x0).max(1e-6), (y1 - y0).max(1e-6));
    raw.into_iter().map(|(x, y)| PathPoint { x: (x - x0) / w, y: (y - y0) / h, hin: [0.0, 0.0], hout: [0.0, 0.0] }).collect()
}

impl App {
    /// Pengaturan tool di panel kanan (tampil saat tool Polygon aktif).
    pub(super) fn polygon_options(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.label(theme::title("Polygon / star"));
        ui.weak("Drag on the canvas to draw. Hold Shift for equal width and height.");
        ui.add_space(2.0);
        let o = &mut self.poly;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut o.star, false, "Polygon");
            ui.selectable_value(&mut o.star, true, "Star");
        });
        ui.horizontal(|ui| {
            let label = if o.star { "Points  " } else { "Sides  " };
            ui.add_sized([120.0, 28.0], egui::DragValue::new(&mut o.sides).speed(0.1).range(3..=60).prefix(label));
            if o.star {
                let mut pct = o.inner * 100.0;
                let ratio = egui::DragValue::new(&mut pct).speed(0.5).range(5.0..=95.0).max_decimals(0).prefix("Inner  ").suffix("%");
                if ui.add_sized([120.0, 28.0], ratio).on_hover_text("How deep the star's inner corners are").changed() {
                    o.inner = pct / 100.0;
                }
            }
        });
        ui.label(egui::RichText::new("Double-click the shape afterwards to edit its points.").size(11.0).weak());
        ui.separator();
    }
}
