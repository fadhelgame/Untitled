//! Pages: beberapa halaman dalam satu dokumen, dengan daftarnya di atas panel layer. Tiap halaman
//! punya layer, guide dan tampilan (pan/zoom) sendiri; style dan gambar dipakai bersama.
//! `Doc::nodes` selalu berisi halaman aktif (lihat `Doc::pages`), jadi canvas, panel dan export
//! tidak perlu tahu soal halaman.

use super::*;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct Pages {
    /// Tampilan terakhir tiap halaman, per id halaman.
    views: HashMap<u64, View>,
    /// Halaman yang namanya sedang diedit: (indeks, teks, fokus sudah diminta).
    rename: Option<(usize, String, bool)>,
}

impl Pages {
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }
}

enum Act {
    Go(usize),
    Add,
    Rename(usize, String),
    Duplicate(usize),
    Delete(usize),
    Move(usize, usize),
}

impl App {
    /// Sebelum berganti halaman: selesaikan pekerjaan yang menggantung di halaman ini.
    fn leave_page(&mut self) {
        self.finish_text_edit();
        if self.ext.pen.is_some() {
            self.finish_pen(false);
        }
        self.ext.path_edit = None;
        self.ext.builder = None;
        self.sel.clear();
        self.drag = Drag::None;
        self.pre_drag = None;
        self.layer_drag = None;
        self.group_rename = None;
    }

    /// Halaman aktif berganti (dari `old`): simpan tampilan halaman lama, pulihkan tampilan
    /// halaman baru. Halaman yang belum pernah dibuka dipaskan ke isinya.
    pub(super) fn page_changed(&mut self, old: u64) {
        let new = self.doc.page_id();
        if old == new {
            return;
        }
        self.pages.views.insert(old, self.view);
        // (Where the canvas put the texts on a path of the old page says nothing about this one.)
        self.ext.path_seen.clear();
        match self.pages.views.get(&new) {
            Some(v) => self.view = *v,
            None if self.doc.nodes.is_empty() => self.view = View { pan: vec2(80.0, 80.0), zoom: 1.0 },
            None => self.fit_pending = true,
        }
        let doc = &self.doc;
        self.sel.retain(|id| doc.find(*id).is_some());
    }

    pub(super) fn go_page(&mut self, i: usize) {
        if i == self.doc.page || i >= self.doc.page_count() {
            return;
        }
        self.leave_page();
        self.doc.ensure_pages();
        let old = self.doc.page_id();
        if self.doc.switch_page(i) {
            self.page_changed(old);
            self.touch();
            self.status = format!("Page: {}", self.doc.page_name(i));
        }
    }

    /// Halaman berikutnya / sebelumnya (Page Down / Page Up).
    pub(super) fn step_page(&mut self, forward: bool) {
        let i = if forward { self.doc.page + 1 } else { self.doc.page.wrapping_sub(1) };
        self.go_page(i);
    }

    /// Jalankan perubahan daftar halaman sebagai satu langkah undo.
    fn edit_pages(&mut self, leave: bool, f: impl FnOnce(&mut Doc) -> bool) -> bool {
        if leave {
            self.leave_page();
        }
        let pre = self.doc.clone();
        self.doc.ensure_pages();
        let old = self.doc.page_id();
        if !f(&mut self.doc) {
            self.doc = pre;
            return false;
        }
        self.page_changed(old);
        self.push_undo(pre);
        true
    }

    pub(super) fn add_page(&mut self) {
        self.edit_pages(true, |d| {
            d.add_page();
            true
        });
        self.status = format!("Added {}.", self.doc.page_name(self.doc.page));
    }

    pub(super) fn duplicate_page(&mut self, i: usize) {
        if self.edit_pages(true, |d| d.duplicate_page(i).is_some()) {
            self.status = format!("Duplicated as {}.", self.doc.page_name(self.doc.page));
        }
    }

    pub(super) fn delete_page(&mut self, i: usize) {
        let name = self.doc.page_name(i).to_owned();
        if self.edit_pages(i == self.doc.page, |d| d.delete_page(i)) {
            self.status = format!("Deleted {name}. Ctrl+Z brings it back.");
        }
    }

    pub(super) fn rename_page(&mut self, i: usize, name: &str) {
        self.edit_pages(false, |d| d.rename_page(i, name));
    }

    /// Daftar halaman di atas panel layer.
    pub(super) fn pages_ui(&mut self, ui: &mut egui::Ui) {
        let mut act: Option<Act> = None;
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            ui.label(theme::title("Pages").size(15.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (rect, resp) = ui.allocate_exact_size(vec2(24.0, 24.0), Sense::click());
                if resp.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(6), ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let (c, s) = (rect.center(), Stroke::new(1.5_f32, Color32::from_gray(222)));
                ui.painter().line_segment([c - vec2(5.0, 0.0), c + vec2(5.0, 0.0)], s);
                ui.painter().line_segment([c - vec2(0.0, 5.0), c + vec2(0.0, 5.0)], s);
                if resp.on_hover_text("Add page").clicked() {
                    act = Some(Act::Add);
                }
            });
        });
        ui.add_space(2.0);
        let count = self.doc.page_count();
        let row_h = 28.0;
        let mut rename = self.pages.rename.take();
        ui.scope(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical().id_salt("pages").max_height(row_h * 6.0).auto_shrink([false, true]).show(ui, |ui| {
                for i in 0..count {
                    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                    let active = i == self.doc.page;
                    let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                    if active {
                        painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), Color32::from_rgb(58, 58, 63));
                    } else if resp.hovered() {
                        painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.6));
                    }
                    if rename.as_ref().is_some_and(|r| r.0 == i) {
                        // Sedang mengganti nama: Enter / klik di luar = simpan, Esc = batal.
                        let (_, text, focused) = rename.as_mut().unwrap();
                        let area = Rect::from_min_max(pos2(rect.left() + 4.0, rect.top() + 2.0), pos2(rect.right() - 4.0, rect.bottom() - 2.0));
                        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
                        let edit = child.add(egui::TextEdit::singleline(text).font(FontId::proportional(14.0)).desired_width(area.width()));
                        if !*focused {
                            *focused = true;
                            edit.request_focus();
                        }
                        if edit.lost_focus() {
                            if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                act = Some(Act::Rename(i, text.trim().to_owned()));
                            }
                            rename = None;
                        }
                        continue;
                    }
                    let (font, color) = if active {
                        (FontId::new(14.0, theme::strong()), Color32::from_gray(245))
                    } else {
                        (FontId::proportional(14.0), ui.visuals().text_color())
                    };
                    painter.text(pos2(rect.left() + 10.0, rect.center().y), Align2::LEFT_CENTER, self.doc.page_name(i), font, color);
                    if resp.double_clicked() {
                        rename = Some((i, self.doc.page_name(i).to_owned(), false));
                    } else if resp.clicked() && !active {
                        act = Some(Act::Go(i));
                    }
                    resp.context_menu(|ui| {
                        ui.set_min_width(180.0);
                        let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, a: Act| {
                            if ui.add_enabled(enabled, egui::Button::new(label)).clicked() {
                                act = Some(a);
                                ui.close_menu();
                            }
                        };
                        if ui.button("Rename").clicked() {
                            rename = Some((i, self.doc.page_name(i).to_owned(), false));
                            ui.close_menu();
                        }
                        item(ui, true, "Duplicate", Act::Duplicate(i));
                        ui.separator();
                        item(ui, i > 0, "Move up", Act::Move(i, i.wrapping_sub(1)));
                        item(ui, i + 1 < count, "Move down", Act::Move(i, i + 1));
                        ui.separator();
                        item(ui, count > 1, "Delete", Act::Delete(i));
                    });
                }
            });
        });
        self.pages.rename = rename;
        match act {
            Some(Act::Go(i)) => self.go_page(i),
            Some(Act::Add) => self.add_page(),
            Some(Act::Rename(i, name)) => self.rename_page(i, &name),
            Some(Act::Duplicate(i)) => self.duplicate_page(i),
            Some(Act::Delete(i)) => self.delete_page(i),
            Some(Act::Move(from, to)) => {
                self.edit_pages(false, |d| d.move_page(from, to));
            }
            None => {}
        }
    }
}
