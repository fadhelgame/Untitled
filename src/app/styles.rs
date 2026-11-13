//! Color styles dan text styles: warna dan gaya teks bernama yang dipakai bersama banyak objek.
//! Objek menyimpan id style-nya; nilai sebenarnya (warna, font, ukuran) tetap ada di objek dan
//! disamakan ulang setiap kali style berubah, jadi jalur render dan export tidak perlu tahu soal style.
//!
//! Objek yang terhubung boleh menyimpang dari style-nya (mis. warnanya digeser di picker); panel
//! lalu menawarkan "Update style" untuk mendorong perubahan itu ke style dan semua pemakainya.

use super::picker::paint_row;
use super::*;
use crate::doc::{Doc, TypeStyle};

/// Aksi style dari panel properti; dijalankan setelah UI selesai digambar.
pub(super) enum StyleAction {
    LinkColor { stroke: bool, style: u64 },
    CreateColor { stroke: bool },
    UpdateColor { stroke: bool },
    DetachColor { stroke: bool },
    LinkType(u64),
    CreateType,
    UpdateType,
    DetachType,
}

fn swatch(ui: &mut egui::Ui, color: [u8; 4]) {
    let (rect, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(3), col(color));
    ui.painter().rect_stroke(rect, CornerRadius::same(3), Stroke::new(1.0_f32, Color32::from_gray(90)), StrokeKind::Inside);
}

/// Baris style di bawah Fill atau Stroke: menu pilih style, buat style baru, lepas, dan
/// "Update style" kalau warna objek sudah berbeda dari style-nya.
pub(super) fn color_style_row(doc: &Doc, ui: &mut egui::Ui, n: &mut Node, stroke: bool) -> Option<StyleAction> {
    let id = if stroke { n.stroke_style } else { n.fill_style };
    let linked = id.and_then(|id| doc.color_style(id));
    if id.is_some() && linked.is_none() {
        // Style-nya sudah dihapus (atau objek ditempel dari dokumen lain).
        if stroke {
            n.stroke_style = None;
        } else {
            n.fill_style = None;
        }
    }
    // Hanya warna solid yang bisa jadi style.
    let solid = if stroke { n.stroke } else { n.fill.filter(|_| n.gradient.is_none()) };
    if doc.colors.is_empty() && solid.is_none() {
        return None;
    }
    let mut action = None;
    ui.horizontal(|ui| {
        if let Some(s) = linked {
            swatch(ui, s.color);
        }
        let label = linked.map_or("Style  ⏷".to_owned(), |s| format!("{}  ⏷", s.name));
        ui.menu_button(label, |ui| {
            ui.set_min_width(190.0);
            for s in &doc.colors {
                ui.horizontal(|ui| {
                    swatch(ui, s.color);
                    if ui.selectable_label(linked.is_some_and(|l| l.id == s.id), &s.name).clicked() {
                        action = Some(StyleAction::LinkColor { stroke, style: s.id });
                        ui.close_menu();
                    }
                });
            }
            if !doc.colors.is_empty() {
                ui.separator();
            }
            if ui.add_enabled(solid.is_some(), egui::Button::new("New style from this color")).clicked() {
                action = Some(StyleAction::CreateColor { stroke });
                ui.close_menu();
            }
            if linked.is_some() && ui.button("Detach style").clicked() {
                action = Some(StyleAction::DetachColor { stroke });
                ui.close_menu();
            }
        })
        .response
        .on_hover_text("Color style: one named color shared by many objects");
        if let (Some(s), Some(c)) = (linked, solid) {
            if c != s.color && ui.button("Update style").on_hover_text("Apply this color to the style and to everything that uses it").clicked() {
                action = Some(StyleAction::UpdateColor { stroke });
            }
        }
    });
    action
}

/// Baris style di bagian Typography.
pub(super) fn type_style_row(doc: &Doc, ui: &mut egui::Ui, n: &mut Node) -> Option<StyleAction> {
    let linked = n.type_style.and_then(|id| doc.type_style(id));
    if n.type_style.is_some() && linked.is_none() {
        n.type_style = None;
    }
    let mut action = None;
    ui.horizontal(|ui| {
        let label = linked.map_or("Text style  ⏷".to_owned(), |s| format!("{}  ⏷", s.name));
        ui.menu_button(label, |ui| {
            ui.set_min_width(210.0);
            for s in &doc.type_styles {
                let text = format!("{}   ({})", s.name, s.summary());
                if ui.selectable_label(linked.is_some_and(|l| l.id == s.id), text).clicked() {
                    action = Some(StyleAction::LinkType(s.id));
                    ui.close_menu();
                }
            }
            if !doc.type_styles.is_empty() {
                ui.separator();
            }
            if ui.button("New style from this text").clicked() {
                action = Some(StyleAction::CreateType);
                ui.close_menu();
            }
            if linked.is_some() && ui.button("Detach style").clicked() {
                action = Some(StyleAction::DetachType);
                ui.close_menu();
            }
        })
        .response
        .on_hover_text("Text style: font, weight, size and spacing shared by many texts");
        if linked.is_some_and(|s| !s.matches(n)) && ui.button("Update style").on_hover_text("Apply this text's font settings to the style and to everything that uses it").clicked() {
            action = Some(StyleAction::UpdateType);
        }
    });
    action
}

impl App {
    /// Jalankan aksi style untuk node `id` (satu langkah undo).
    pub(super) fn style_action(&mut self, id: u64, action: StyleAction) {
        let Some(node) = self.doc.find(id).cloned() else { return };
        let pre = self.doc.clone();
        let color_of = |stroke: bool| if stroke { node.stroke } else { node.fill.filter(|_| node.gradient.is_none()) };
        let set_color = |doc: &mut Doc, stroke: bool, style: Option<u64>| {
            if let Some(n) = doc.find_mut(id) {
                if stroke {
                    n.stroke_style = style;
                } else {
                    n.fill_style = style;
                }
            }
        };
        match action {
            StyleAction::LinkColor { stroke, style } => {
                set_color(&mut self.doc, stroke, Some(style));
                self.doc.sync_color_style(style);
            }
            StyleAction::CreateColor { stroke } => {
                let Some(color) = color_of(stroke) else { return };
                let style = self.doc.add_color_style(color);
                set_color(&mut self.doc, stroke, Some(style));
                self.status = "Color style created. Rename it in the Design panel with nothing selected.".into();
            }
            StyleAction::UpdateColor { stroke } => {
                let style = if stroke { node.stroke_style } else { node.fill_style };
                let (Some(style), Some(color)) = (style, color_of(stroke)) else { return };
                if let Some(s) = self.doc.colors.iter_mut().find(|s| s.id == style) {
                    s.color = color;
                }
                self.doc.sync_color_style(style);
            }
            StyleAction::DetachColor { stroke } => set_color(&mut self.doc, stroke, None),
            StyleAction::LinkType(style) => {
                if let Some(n) = self.doc.find_mut(id) {
                    n.type_style = Some(style);
                }
                self.doc.sync_type_style(style);
            }
            StyleAction::CreateType => {
                let style = self.doc.add_type_style(&node);
                if let Some(n) = self.doc.find_mut(id) {
                    n.type_style = Some(style);
                }
                self.status = "Text style created. Rename it in the Design panel with nothing selected.".into();
            }
            StyleAction::UpdateType => {
                let Some(style) = node.type_style else { return };
                if let Some(s) = self.doc.type_styles.iter_mut().find(|s| s.id == style) {
                    let name = std::mem::take(&mut s.name);
                    *s = TypeStyle { id: style, name, ..TypeStyle::of(&node) };
                }
                self.doc.sync_type_style(style);
            }
            StyleAction::DetachType => {
                if let Some(n) = self.doc.find_mut(id) {
                    n.type_style = None;
                }
            }
        }
        self.push_undo(pre);
        self.touch();
    }

    /// Daftar style dokumen (tampil di panel Design saat tidak ada yang dipilih): ganti nama,
    /// ubah warna, hapus. Mengubah warna di sini langsung mengubah semua objek yang memakainya.
    pub(super) fn styles_manager(&mut self, ui: &mut egui::Ui) {
        if self.doc.colors.is_empty() && self.doc.type_styles.is_empty() {
            return;
        }
        let before = (self.doc.colors.clone(), self.doc.type_styles.clone());
        let (mut colors, mut types) = before.clone();
        let (mut sync_color, mut sync_none) = (Vec::new(), false);
        if !colors.is_empty() {
            section(ui, "Color styles");
            let mut removed = None;
            for s in &mut colors {
                ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(f32::INFINITY));
                let mut color = Some(s.color);
                paint_row(ui, &mut self.ext.picker, (s.id, 3), &mut color, None, s.color);
                match color {
                    Some(c) if c != s.color => {
                        s.color = c;
                        sync_color.push(s.id);
                    }
                    Some(_) => {}
                    // Tombol "–" di baris warna menghapus style-nya.
                    None => removed = Some(s.id),
                }
                ui.weak(match self.doc.color_style_uses(s.id) {
                    1 => "Used by 1 object".to_owned(),
                    n => format!("Used by {n} objects"),
                });
                ui.add_space(4.0);
            }
            if let Some(id) = removed {
                colors.retain(|s| s.id != id);
                sync_none = true;
            }
        }
        if !types.is_empty() {
            section(ui, "Text styles");
            let mut removed = None;
            for s in &mut types {
                ui.horizontal(|ui| {
                    if ui.button("–").on_hover_text("Delete this text style (texts keep their look)").clicked() {
                        removed = Some(s.id);
                    }
                    ui.add(egui::TextEdit::singleline(&mut s.name).desired_width(f32::INFINITY));
                });
                ui.weak(s.summary());
                ui.add_space(4.0);
            }
            if let Some(id) = removed {
                types.retain(|s| s.id != id);
                sync_none = true;
            }
        }
        if (colors.clone(), types.clone()) == before {
            return;
        }
        // Perubahan beruntun (mengetik nama, menggeser warna) digabung jadi satu langkah undo.
        let now = ui.input(|i| i.time);
        if self.last_prop.0 != u64::MAX || now - self.last_prop.1 > 0.8 {
            let pre = self.doc.clone();
            self.push_undo(pre);
        }
        self.last_prop = (u64::MAX, now);
        self.doc.colors = colors;
        self.doc.type_styles = types;
        for id in sync_color {
            self.doc.sync_color_style(id);
        }
        if sync_none {
            self.doc.drop_missing_styles();
        }
        self.touch();
    }
}
