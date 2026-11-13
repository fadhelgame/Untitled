//! Tema antarmuka: panel berupa kartu membulat yang mengambang di atas latar canvas,
//! font sans-serif sistem, dan jarak antar-elemen yang lebih lega.

use super::{ACCENT, CANVAS_BG};
use crate::fonts::UI_STRONG;
use eframe::egui::{self, vec2, Color32, CornerRadius, FontFamily, FontId, Margin, Stroke, TextStyle};

pub const CARD: Color32 = Color32::from_rgb(38, 38, 41);
const CARD_STROKE: Color32 = Color32::from_rgb(54, 54, 58);
const RADIUS: u8 = 14;
/// Jarak kartu ke tepi jendela; antar-kartu memakai setengahnya dari tiap sisi.
const GAP: i8 = 8;
const HALF: i8 = GAP / 2;

/// Family semibold untuk judul.
pub fn strong() -> FontFamily {
    FontFamily::Name(UI_STRONG.into())
}

/// Judul bagian panel.
pub fn title(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text).family(strong()).size(14.0).color(Color32::from_gray(240))
}

pub fn apply(ctx: &egui::Context) {
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, FontId::proportional(11.5)),
            (TextStyle::Body, FontId::proportional(13.5)),
            (TextStyle::Button, FontId::proportional(13.5)),
            (TextStyle::Heading, FontId::new(19.0, strong())),
            (TextStyle::Monospace, FontId::monospace(12.5)),
        ]
        .into();

        let sp = &mut s.spacing;
        sp.item_spacing = vec2(8.0, 7.0);
        sp.button_padding = vec2(10.0, 5.0);
        sp.interact_size = vec2(40.0, 28.0);
        sp.menu_margin = Margin::same(8);
        sp.window_margin = Margin::same(12);
        sp.scroll.bar_width = 8.0;

        let v = &mut s.visuals;
        v.panel_fill = CARD;
        v.window_fill = Color32::from_rgb(44, 44, 48);
        v.window_stroke = Stroke::new(1.0_f32,Color32::from_gray(64));
        v.window_corner_radius = CornerRadius::same(RADIUS);
        v.menu_corner_radius = CornerRadius::same(12);
        v.extreme_bg_color = Color32::from_rgb(26, 26, 28);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.55);
        v.selection.stroke = Stroke::new(1.0_f32,Color32::WHITE);

        let round = CornerRadius::same(8);
        let w = &mut v.widgets;
        w.noninteractive.corner_radius = round;
        w.noninteractive.bg_stroke = Stroke::new(1.0_f32,CARD_STROKE);
        w.noninteractive.fg_stroke = Stroke::new(1.0_f32,Color32::from_gray(196));
        w.inactive.corner_radius = round;
        w.inactive.bg_fill = Color32::from_rgb(56, 56, 60);
        w.inactive.weak_bg_fill = Color32::from_rgb(56, 56, 60);
        w.inactive.fg_stroke = Stroke::new(1.0_f32,Color32::from_gray(222));
        w.hovered.corner_radius = round;
        w.hovered.bg_fill = Color32::from_rgb(70, 70, 75);
        w.hovered.weak_bg_fill = Color32::from_rgb(70, 70, 75);
        w.hovered.bg_stroke = Stroke::new(1.0_f32,Color32::from_gray(96));
        w.hovered.expansion = 0.0;
        w.active.corner_radius = round;
        w.active.expansion = 0.0;
        w.open.corner_radius = round;
        w.open.bg_fill = Color32::from_rgb(56, 56, 60);
        w.open.weak_bg_fill = Color32::from_rgb(64, 64, 69);
    });
}

/// Sisi jendela tempat sebuah panel menempel; menentukan margin luarnya.
#[derive(Clone, Copy)]
pub enum Edge {
    Top,
    Bottom,
    Left,
    Right,
    /// Panel kiri yang menempel sampai tepi atas dan bawah jendela (layar Home).
    LeftFull,
}

/// Bingkai luar panel: tak terlihat (sewarna canvas), hanya memberi jarak di sekeliling kartu.
pub fn gutter(edge: Edge) -> egui::Frame {
    let m = match edge {
        Edge::Top => Margin { left: GAP, right: GAP, top: GAP, bottom: HALF },
        Edge::Bottom => Margin { left: GAP, right: GAP, top: HALF, bottom: GAP },
        Edge::Left => Margin { left: GAP, right: HALF, top: HALF, bottom: HALF },
        Edge::Right => Margin { left: HALF, right: GAP, top: HALF, bottom: HALF },
        Edge::LeftFull => Margin { left: GAP, right: HALF, top: GAP, bottom: GAP },
    };
    egui::Frame::new().fill(CANVAS_BG).inner_margin(m)
}

fn card_frame(pad: Margin) -> egui::Frame {
    egui::Frame::new()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32,CARD_STROKE))
        .corner_radius(CornerRadius::same(RADIUS))
        .inner_margin(pad)
}

/// Kartu selebar panel, setinggi isinya (bar atas dan bawah).
pub fn bar<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_frame(Margin::symmetric(12, 6))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            add(ui)
        })
        .inner
}

/// Kartu yang mengisi seluruh panel samping.
pub fn card<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    card_frame(Margin::same(12))
        .show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            add(ui)
        })
        .inner
}
