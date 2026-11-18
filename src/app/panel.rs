//! Panel properti (kanan), toolbar bawah, export, dan loop `update`.

use super::autolayout::{layout_section, sizing_row};
use super::picker::paint_row;
use super::*;
use crate::adjust::Adjust;
use crate::layout::Sizing;

const FORMATS: [&str; 4] = ["PNG", "JPG", "SVG", "PDF"];

/// A combo box for a property that is one of a few named values (`value` = its index).
fn choice(ui: &mut egui::Ui, salt: &str, width: f32, label: &str, names: &[&str], value: &mut u8) -> egui::Response {
    let shown = names[(*value as usize).min(names.len() - 1)];
    egui::ComboBox::from_id_salt(salt)
        .width(width - 8.0)
        .selected_text(format!("{label}  {shown}"))
        .show_ui(ui, |ui| {
            for (i, name) in names.iter().enumerate() {
                ui.selectable_value(value, i as u8, *name);
            }
        })
        .response
}

impl App {
    pub(super) fn props(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if self.tool == Tool::Frame {
            self.frame_presets(ui);
            return;
        }
        if self.tool == Tool::Wand {
            self.wand_options(ui);
        }
        if self.tool == Tool::Polygon {
            self.polygon_options(ui);
        }
        ui.add_space(6.0);
        if self.sel.len() > 1 {
            self.multi_props(ui, &ctx);
            return;
        }
        if self.sel.len() != 1 {
            ui.label(theme::title("Design"));
            if self.sel.is_empty() {
                ui.weak("Nothing selected.");
                self.styles_manager(ui);
                ui.add_space(8.0);
                for line in [
                    "V  move         K  scale",
                    "F  frame        R  rectangle",
                    "O  ellipse      L  line",
                    "P  pen          T  text",
                    "H  hand         Y  magic wand",
                    "Shift+M  shape builder",
                    "",
                    "Text: click = auto width,",
                    "      drag  = paragraph box",
                    "Pen:  click = corner, drag = curve,",
                    "      Enter / double-click = finish",
                    "      on a selected vector: click a",
                    "      segment = add anchor, an anchor",
                    "      = remove, an end = continue,",
                    "      Alt+click = smooth / corner",
                    "      Caps Lock = precise cursor",
                    "Shape builder: drag across regions",
                    "      = merge, Alt+click = remove",
                    "Drop or paste (Ctrl+V) images / SVG",
                    "      (an SVG becomes editable layers)",
                    "Drag outside a corner = rotate",
                    "Shift while resizing = keep ratio",
                    "Alt while resizing = from center",
                    "Double-click a vector = edit points",
                    "Double-click a nested frame = go inside",
                    "Ctrl+click = deepest object",
                    "Moving snaps to edges/centers (Ctrl = off)",
                    "",
                    "Space+drag     pan canvas",
                    "Ctrl+scroll    zoom",
                    "Shift+0/1/2    100% / fit / selection",
                    "+  -  Ctrl+0   zoom in / out / 100%",
                    "Alt+drag       duplicate",
                    "Shift+drag     lock axis",
                    "] and [        bring front / send back",
                    "Alt+A D W S    align left/right/top/bottom",
                    "Alt+H V        align centers",
                    "Alt+Shift+H V  distribute spacing",
                    "Ctrl+Alt+T     tidy up",
                    "Shift+H V      flip horizontal / vertical",
                    "1 … 9, 0       opacity 10% … 100%",
                    "Shift+X        swap fill/stroke",
                    "Ctrl+Shift+O   outline stroke / text",
                    "Ctrl+Alt+P     text on path / off it",
                    "      (select a text and a shape; drag",
                    "      the text to slide it along)",
                    "Ctrl+V         paste at the pointer",
                    "Ctrl+Shift+V   paste in place",
                    "Ctrl+Shift+H   show/hide",
                    "Ctrl+F         find in text",
                    "Ctrl+H         find and replace",
                    "Ctrl+U         underline text",
                    "Ctrl+Shift+X   strikethrough text",
                    "Ctrl+Alt+G     clipping mask",
                    "Ctrl+Alt+K     create component",
                    "Ctrl+Alt+B     detach instance",
                    "Shift+A        add auto layout",
                    "Alt+Shift+A    remove auto layout",
                    "Auto layout: drag an item to re-order,",
                    "      arrows swap it with its neighbour",
                    "Enter          edit text / into a frame",
                    "Shift+Enter    select the parent frame",
                    "Ctrl+\\         hide panels",
                ] {
                    ui.label(egui::RichText::new(line).monospace().size(11.0).weak());
                }
            }
            self.export_section(ui, &ctx);
            return;
        }
        let id = self.sel[0];
        let Some(mut orig) = self.doc.find(id).cloned() else { return };
        // What the node holds is not edited here: it stays out of the two copies below (a
        // frame of thousands of layers was copied three times per frame) and is put back
        // into the one that is written to the document, further down.
        let held = std::mem::take(&mut orig.children);

        // Sedang mengedit teks dengan huruf terseleksi: panel menampilkan dan mengubah
        // gaya rentang itu saja, bukan seluruh teks.
        let editing = self.editing == Some(id);
        let range = if editing { self.ext.edit_sel.filter(|(a, b)| a != b) } else { None };
        let base = orig.base_style();
        let shown = range.map(|(a, _)| orig.style_at(a));
        let mut n = orig.clone();
        if let Some(style) = &shown {
            n.apply_style(style);
        }
        let before = n.clone();
        orig.children = held;
        let mut align: Option<u8> = None;
        let mut flip: Option<bool> = None;
        let mut remove_bg = false;
        let mut crop_image = false;
        // A slider of the Adjust section is held down.
        let mut adjusting = false;
        let mut detach_path = false;
        let mut style_action: Option<super::styles::StyleAction> = None;
        let mut comp_act: Option<super::components::Act> = None;
        let mut constrain: super::constraints::Pair = (None, None);
        let has_parent = self.doc.parent_of(id).is_some();
        let mut auto_act: Option<super::autolayout::Act> = None;
        let mut sized: (Option<Sizing>, Option<Sizing>) = (None, None);
        // In a frame with auto layout, and placed by it (unless set to absolute).
        let in_auto = self.doc.parent_of(id).and_then(|p| self.doc.find(p)).is_some_and(|p| p.is_auto());
        let item = self.doc.auto_item(id);
        let choices = (self.sizing_choices(id, true), self.sizing_choices(id, false));
        // A part of an instance: its look can be changed, its place, size and name follow the main.
        let pinned = orig.source.is_some();
        let linked = orig.is_linked() || pinned;

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(theme::title(n.kind.label()));
            ui.add_enabled(!pinned, egui::TextEdit::singleline(&mut n.name).desired_width(f32::INFINITY));
            if linked {
                comp_act = self.component_section(ui);
            }

            let half = (ui.available_width() - 8.0) / 2.0;
            ui.add_enabled_ui(!pinned, |ui| {
                section(ui, "Position");
                sub(ui, "Alignment");
                ui.add_enabled_ui(has_parent && !item, |ui| {
                    if let Some(mode) = align_buttons(ui) {
                        align = Some(mode);
                    }
                });
                sub(ui, "Position");
                ui.add_enabled_ui(!item, |ui| {
                    ui.horizontal(|ui| {
                        field(ui, half, "X", &mut n.x);
                        field(ui, half, "Y", &mut n.y);
                    });
                });
                // (A text on a path is left out of the layout anyway: the path places it.)
                if in_auto && !(n.kind == Kind::Text && n.on_path.is_some()) {
                    ui.checkbox(&mut n.absolute, "Absolute position").on_hover_text("Left out of the auto layout of its frame: it stays where you put it, and follows its constraints when the frame is resized.");
                    if item {
                        ui.weak("Placed by the auto layout of its frame.");
                    }
                }
                let spins = !matches!(n.kind, Kind::Frame | Kind::Line);
                sub(ui, if spins { "Rotation" } else { "Flip" });
                ui.horizontal(|ui| {
                    if spins {
                        ui.add_sized(
                            [half, 28.0],
                            egui::DragValue::new(&mut n.rotation).speed(1.0).range(-360.0..=360.0).max_decimals(1).suffix("°"),
                        )
                        .on_hover_text("Or drag just outside a corner of the selection (Shift = 15° steps)");
                        ui.add_space(6.0);
                    }
                    flip = flip_buttons(ui);
                });

                if n.kind != Kind::Text || n.wrap || n.fixed_h {
                    section(ui, "Layout");
                    sub(ui, "Dimensions");
                    ui.horizontal(|ui| {
                        // Of a text: the width of a paragraph, and a height that was set by hand.
                        ui.add_enabled_ui(n.kind != Kind::Text || n.wrap, |ui| field(ui, half, "W", &mut n.w));
                        ui.add_enabled_ui(n.kind != Kind::Text || n.fixed_h, |ui| field(ui, half, "H", &mut n.h));
                    });
                    if n.kind != Kind::Line {
                        n.w = n.w.max(1.0);
                        n.h = n.h.max(1.0);
                    }
                    sized = sizing_row(ui, half, (Some(n.size_h), Some(n.size_v)), (&choices.0, &choices.1));
                }
            });
            if n.kind == Kind::Frame {
                let locked = (pinned || n.instance_of.is_some()).then_some("The layout of an instance follows its main component.");
                if n.layout.is_some() || locked.is_none() {
                    auto_act = layout_section(ui, &mut n.layout, locked, "");
                }
            }
            // An item of an auto layout has nothing to be pinned to.
            if has_parent && !item {
                constrain = super::constraints::constraints_section(ui, (Some(n.constrain_h), Some(n.constrain_v)), pinned);
            }

            section(ui, "Appearance");
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    sub(ui, "Opacity");
                    let mut pct = n.opacity * 100.0;
                    let dv = egui::DragValue::new(&mut pct).speed(1.0).range(0.0..=100.0).max_decimals(0).suffix("%");
                    if ui.add_sized([half, 28.0], dv).changed() {
                        n.opacity = pct / 100.0;
                    }
                });
                if matches!(n.kind, Kind::Rect | Kind::Image | Kind::Path) {
                    ui.vertical(|ui| {
                        sub(ui, "Corner radius");
                        let dv = ui.add_sized([half, 28.0], egui::DragValue::new(&mut n.radius).speed(0.5).range(0.0..=2000.0).max_decimals(1));
                        if n.kind == Kind::Path {
                            // Angka di panel berlaku untuk semua sudut: jari-jari per sudut dihapus.
                            if dv.changed() {
                                n.clear_corner_radii();
                            }
                            dv.on_hover_text("Rounds every sharp corner of this vector. Drag a corner widget on the canvas instead; hold Alt to round one corner only.");
                        }
                    });
                }
            });
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("blend")
                    .width(half)
                    .selected_text(blend::NAMES[(n.blend as usize).min(blend::NAMES.len() - 1)])
                    .show_ui(ui, |ui| {
                        for (i, name) in blend::NAMES.iter().enumerate() {
                            ui.selectable_value(&mut n.blend, i as u8, *name);
                        }
                    })
                    .response
                    .on_hover_text("Blend mode");
                ui.checkbox(&mut n.visible, "Visible");
            });
            ui.checkbox(&mut n.locked, "Locked").on_hover_text("Can't be selected or moved on the canvas. Select it in the Layers panel to unlock (Ctrl+Shift+L).");

            if n.kind == Kind::Text && n.on_path.is_some() && range.is_none() {
                section(ui, "Text on path");
                ui.horizontal(|ui| {
                    ui.add_sized([half, 28.0], egui::DragValue::new(&mut n.path_offset).speed(1.0).max_decimals(1).prefix("Offset  "))
                        .on_hover_text("How far along the path the text is moved from where its alignment puts it. Dragging the text on the canvas changes this too.");
                    ui.checkbox(&mut n.path_side, "Flip side").on_hover_text("The text stands on the other side of the path and runs the other way.");
                });
                ui.add_enabled_ui(!pinned, |ui| {
                    if ui.add(egui::Button::new("Detach from path").min_size(vec2(ui.available_width(), 28.0))).on_hover_text("Plain text again  (Ctrl+Alt+P)").clicked() {
                        detach_path = true;
                    }
                });
                ui.weak("One line along the shape; align left / center / right places it on the path.");
            }
            if n.kind == Kind::Text {
                self.typography(ui, &ctx, &mut n, editing, range, None);
                if range.is_none() {
                    if let Some(a) = super::styles::type_style_row(&self.doc, ui, &mut n) {
                        style_action = Some(a);
                    }
                }
            }

            if !matches!(n.kind, Kind::Line | Kind::Image) {
                section(ui, if range.is_some() { "Fill (selected text)" } else { "Fill" });
                // Teks hanya mendukung warna solid.
                let gradient = if n.kind == Kind::Text { None } else { Some(&mut n.gradient) };
                paint_row(ui, &mut self.ext.picker, (id, 0), &mut n.fill, gradient, [217, 217, 217, 255]);
                if range.is_none() {
                    if let Some(a) = super::styles::color_style_row(&self.doc, ui, &mut n, false) {
                        style_action = Some(a);
                    }
                }
            }
            if n.kind == Kind::Image {
                // Color overlay: mewarnai ulang gambar, transparansinya tetap.
                section(ui, "Fill");
                paint_row(ui, &mut self.ext.picker, (id, 0), &mut n.fill, None, [255, 255, 255, 255]);
                if n.fill.is_some() {
                    ui.weak("Recolors the image; transparency is kept.");
                }
                section(ui, "Crop");
                let cropping = self.crop_id() == Some(id);
                let label = if cropping { "Cropping…" } else if n.crop.is_some() { "Edit crop" } else { "Crop image" };
                let btn = ui
                    .add_enabled(!cropping, egui::Button::new(label).min_size(vec2(ui.available_width(), 28.0)))
                    .on_hover_text("Or double-click the image on the canvas.");
                if btn.clicked() {
                    crop_image = true;
                }
                adjusting = adjust_section(ui, &mut n.adjust, "", n.fill.is_some());
                section(ui, "Background");
                if self.rembg_running(id) {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Removing background…");
                    });
                } else {
                    let btn = ui
                        .add_enabled(self.rembg.is_none(), egui::Button::new("Remove background").min_size(vec2(ui.available_width(), 28.0)))
                        .on_hover_text("Runs on this computer. The first use downloads a 170 MB model.");
                    if btn.clicked() {
                        remove_bg = true;
                    }
                }
            }
            if n.kind != Kind::Text {
                section(ui, "Stroke");
                paint_row(ui, &mut self.ext.picker, (id, 1), &mut n.stroke, None, [0, 0, 0, 255]);
                if let Some(a) = super::styles::color_style_row(&self.doc, ui, &mut n, true) {
                    style_action = Some(a);
                }
                if n.stroke.is_some() {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            [half, 28.0],
                            egui::DragValue::new(&mut n.stroke_w).speed(0.25).range(0.0..=500.0).max_decimals(2).prefix("Weight  "),
                        );
                        // Posisi stroke: hanya untuk bentuk yang punya "dalam" dan "luar" yang jelas.
                        if matches!(n.kind, Kind::Rect | Kind::Frame | Kind::Image | Kind::Ellipse) {
                            let mut pos = n.stroke_pos();
                            egui::ComboBox::from_id_salt("stroke_pos")
                                .width(half - 8.0)
                                .selected_text(["Inside", "Center", "Outside"][pos as usize])
                                .show_ui(ui, |ui| {
                                    for (i, name) in ["Inside", "Center", "Outside"].iter().enumerate() {
                                        ui.selectable_value(&mut pos, i as u8, *name);
                                    }
                                })
                                .response
                                .on_hover_text("Stroke position");
                            if pos != n.stroke_pos() {
                                n.stroke_align = Some(pos);
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.add_sized([half, 28.0], egui::DragValue::new(&mut n.dash).speed(0.25).range(0.0..=2000.0).max_decimals(1).prefix("Dash  "))
                            .on_hover_text("Dash length; 0 = solid line");
                        ui.add_enabled_ui(n.dash > 0.0, |ui| {
                            ui.add_sized([half, 28.0], egui::DragValue::new(&mut n.gap).speed(0.25).range(0.0..=2000.0).max_decimals(1).prefix("Gap  "))
                                .on_hover_text("Space between dashes; 0 = same as the dash");
                        });
                    });
                    // Ends: of a line, an open path, and of every dash. Corners: of a path and of
                    // a rectangle that has no corner radius.
                    let ends = matches!(n.kind, Kind::Line) || (n.kind == Kind::Path && !n.closed) || n.dash > 0.0;
                    let corners = match n.kind {
                        Kind::Path | Kind::Frame => true,
                        Kind::Rect | Kind::Image => n.radius <= 0.0,
                        _ => false,
                    };
                    if ends || corners {
                        ui.horizontal(|ui| {
                            if ends {
                                choice(ui, "stroke_cap", half, "Cap", &["Butt", "Round", "Square"], &mut n.cap)
                                    .on_hover_text("Ends of the stroke and of each dash: cut off at the end point, rounded, or squared off half the weight past it");
                            }
                            if corners {
                                choice(ui, "stroke_join", half, "Join", &["Miter", "Round", "Bevel"], &mut n.join)
                                    .on_hover_text("Corners of the stroke: pointed (very sharp corners are cut off), rounded, or cut off");
                            }
                        });
                    }
                }
            }
            let (inner, glass) = (has_inside(&n), glassy(&n));
            effects_section(
                ui,
                &mut self.ext.picker,
                id,
                &mut n,
                true,
                inner,
                glass,
                "",
                "Blurs the whole layer, with its shadow. On a frame, the contents are blurred too.",
            );
            if !linked {
                comp_act = self.component_section(ui);
            }
            self.export_section(ui, &ctx);
            ui.add_space(12.0);
        });

        if let Some(mode) = align {
            self.align(mode);
            return;
        }
        if let Some(horizontal) = flip {
            self.flip(horizontal);
            return;
        }
        if remove_bg {
            self.rembg_start(id, &ctx);
            return;
        }
        if crop_image {
            self.start_crop(id);
            return;
        }
        if detach_path {
            self.detach_from_path();
            return;
        }
        if let Some(action) = style_action {
            self.style_action(id, action);
            return;
        }
        if let Some(act) = comp_act {
            self.component_act(act);
            return;
        }
        if constrain != (None, None) {
            self.set_constraints(constrain);
            return;
        }
        match auto_act {
            Some(super::autolayout::Act::Add) => return self.add_auto_layout(),
            Some(super::autolayout::Act::Remove) => return self.remove_auto_layout(),
            None => {}
        }
        if pinned {
            (n.x, n.y, n.w, n.h, n.rotation, n.wrap) = (before.x, before.y, before.w, before.h, before.rotation, before.wrap);
            n.fixed_h = before.fixed_h;
            n.layout.clone_from(&before.layout);
            (n.size_h, n.size_v, n.absolute) = (before.size_h, before.size_v, before.absolute);
        } else {
            (n.size_h, n.size_v) = (sized.0.unwrap_or(n.size_h), sized.1.unwrap_or(n.size_v));
            // A size that was typed in is the size: that side no longer hugs or fills.
            if n.w != before.w {
                n.size_h = Sizing::Fixed;
            }
            if n.h != before.h {
                n.size_v = Sizing::Fixed;
            }
        }
        let now = ctx.input(|i| i.time);
        if adjusting && self.last_prop.0 == id {
            // One drag of a slider is one undo step, however long it rests on the way.
            self.last_prop.1 = now;
        }
        if n == before {
            return;
        }
        n.children.clone_from(&orig.children);
        // Perubahan beruntun pada objek yang sama digabung jadi satu langkah undo.
        if self.last_prop.0 != id || now - self.last_prop.1 > 0.8 {
            let pre = self.doc.clone();
            self.push_undo(pre);
        }
        self.last_prop = (id, now);
        self.touch();

        if let (Some((a, b)), Some(shown)) = (range, shown) {
            // Terapkan hanya properti yang berubah ke huruf terseleksi; gaya dasar node dikembalikan.
            let new = n.base_style();
            n.apply_style(&base);
            n.fill = orig.fill;
            let changed = new != shown;
            n.style_range(a, b, |st| {
                if new.font != shown.font {
                    st.font = new.font.clone();
                }
                if new.weight != shown.weight {
                    st.weight = new.weight;
                }
                if new.italic != shown.italic {
                    st.italic = new.italic;
                }
                if new.size != shown.size {
                    st.size = new.size;
                }
                if new.fill != shown.fill {
                    st.fill = new.fill;
                }
                if new.underline != shown.underline {
                    st.underline = new.underline;
                }
                if new.strike != shown.strike {
                    st.strike = new.strike;
                }
            });
            if changed {
                self.ext.edit_refocus = true;
            }
        } else {
            // Underline and strikethrough of the whole text reach its styled ranges too.
            if n.underline != before.underline {
                n.set_decoration(false, n.underline);
            }
            if n.strike != before.strike {
                n.set_decoration(true, n.strike);
            }
        }
        if n.text != orig.text {
            let (old, new) = (orig.text.clone(), n.text.clone());
            n.sync_spans(&old, &new);
        }
        let (dx, dy) = (n.x - orig.x, n.y - orig.y);
        if dx != 0.0 || dy != 0.0 {
            for c in &mut n.children {
                c.translate(dx, dy);
            }
        }
        if n.kind == Kind::Frame && (n.w, n.h) != (orig.w, orig.h) {
            // What the frame holds is laid out from how it was before this undo step (one drag
            // of the field), so making it small and large again in one go loses nothing.
            let to = (n.x, n.y, n.w, n.h);
            let start = self.undo.last().and_then(|d| d.find(id)).filter(|s| s.kind == Kind::Frame && s.count() == orig.count());
            let mut laid = start.unwrap_or(&orig).clone();
            laid.layout.clone_from(&n.layout);
            (laid.size_h, laid.size_v) = (n.size_h, n.size_v);
            crate::layout::set_box(&mut laid, to, crate::layout::Children::Constrain);
            (n.w, n.h, n.children) = (laid.w, laid.h, laid.children);
            self.doc.refit(&mut n, &orig);
        }
        if let Some(slot) = self.doc.find_mut(id) {
            *slot = n;
        }
    }

    /// What the Design panel shows about a selection of several layers as a whole (see
    /// `SelFacts`). Each of these asks about every selected layer and the frame it is in, so
    /// they are worked out once and kept until the document or the selection changes.
    fn sel_facts(&mut self, ids: &[u64]) -> SelFacts {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.doc.reindex(), self.doc.page, ids).hash(&mut h);
        let key = h.finish();
        if let Some((_, facts)) = self.sel_memo.as_ref().filter(|m| m.0 == key) {
            return facts.clone();
        }
        let facts = SelFacts {
            group: self.selected_whole_group(),
            pinned: self.sel_pinned(),
            units: self.doc.unit_count(ids),
            constrained: self.constraints_shown(),
            in_auto: ids.iter().all(|id| self.doc.parent_of(*id).and_then(|p| self.doc.find(p)).is_some_and(|p| p.is_auto())),
            // The sizes that every selected layer can have.
            choices: [true, false].map(|horizontal| {
                let lists: Vec<Vec<Sizing>> = ids.iter().map(|id| self.sizing_choices(*id, horizontal)).collect();
                Sizing::ALL.into_iter().filter(|s| lists.iter().all(|l| l.contains(s))).collect::<Vec<_>>()
            }),
        };
        self.sel_memo = Some((key, facts.clone()));
        facts
    }

    /// Properti bersama untuk beberapa objek terpilih. Nilai yang tampil diambil dari objek pertama;
    /// hanya properti yang diubah di panel yang disalin ke semua objek terpilih.
    fn multi_props(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let ids = self.sel.clone();
        let nodes: Vec<&Node> = ids.iter().filter_map(|id| self.doc.find(*id)).collect();
        let Some(first) = nodes.first() else { return };
        let all = |f: &dyn Fn(&Node) -> bool| nodes.iter().all(|n| f(n));
        let all_text = all(&|n| n.kind == Kind::Text);
        let any_text = !all(&|n| n.kind != Kind::Text);
        let fillable = all(&|n| !matches!(n.kind, Kind::Line | Kind::Image));
        let rounded = all(&|n| matches!(n.kind, Kind::Rect | Kind::Image | Kind::Path));
        let same_kind = all(&|n| n.kind == first.kind);
        let mixed_type = !all(&|n| {
            (&n.font, n.weight, n.italic, n.font_size, n.line_height, n.letter_spacing, n.align)
                == (&first.font, first.weight, first.italic, first.font_size, first.line_height, first.letter_spacing, first.align)
                && (n.list, n.valign, n.fixed_h, n.wrap) == (first.list, first.valign, first.fixed_h, first.wrap)
                && (n.decorated(0, 0, false), n.decorated(0, 0, true)) == (first.decorated(0, 0, false), first.decorated(0, 0, true))
        });
        // The U and S buttons are lit when every text has the line through all of it, so a
        // click turns it on unless all have it (as Ctrl+U does, see `App::toggle_decoration`).
        let lined = [false, true].map(|strike| all_text && all(&|n| n.decorated(0, 0, strike)));
        let mixed_fill = !all(&|n| n.fill == first.fill && n.gradient == first.gradient);
        let mixed_stroke = !all(&|n| n.stroke == first.stroke && n.stroke_w == first.stroke_w);
        let mixed_look = !all(&|n| n.opacity == first.opacity && n.blend == first.blend && n.radius == first.radius);
        let mixed_blur = !all(&|n| n.blur == first.blur && n.shadow == first.shadow && n.inner_shadow == first.inner_shadow && n.bg_blur == first.bg_blur);
        let (inner_all, glass_all) = (all(&|n| has_inside(n)), all(&|n| glassy(n)));
        let images = all(&|n| n.kind == Kind::Image);
        let mixed_adjust = !all(&|n| n.adjustment() == first.adjustment());
        let overlaid = all(&|n| n.fill.is_some());
        // Frames with auto layout of their own (not that of a main component).
        let auto_all = all(&|n| n.is_auto() && n.instance_of.is_none() && n.source.is_none());
        let mixed_layout = !all(&|n| n.layout == first.layout);
        let sizes = (all(&|n| n.size_h == first.size_h).then_some(first.size_h), all(&|n| n.size_v == first.size_v).then_some(first.size_v));
        // Frame tidak bisa diputar.
        let spinnable = all(&|n| n.kind != Kind::Frame);
        let title = if same_kind { format!("{} × {}", first.kind.label(), nodes.len()) } else { "Design".to_owned() };
        let mut n = (*first).clone();
        if all_text {
            (n.underline, n.strike) = (lined[0], lined[1]);
        }
        n.children.clear();
        drop(nodes);
        if any_text {
            // Teks hanya mendukung warna solid.
            n.gradient = None;
        }
        let before = n.clone();
        let key = ids[0];
        let facts = self.sel_facts(&ids);
        let (group, pinned) = (facts.group, facts.pinned);
        let mut comp_act: Option<super::components::Act> = None;
        let mut align: Option<u8> = None;
        let mut flip: Option<bool> = None;
        let (units, constrained) = (facts.units, facts.constrained);
        let mut constrain: super::constraints::Pair = (None, None);
        let mut auto_act: Option<super::autolayout::Act> = None;
        let mut sized: (Option<Sizing>, Option<Sizing>) = (None, None);
        let mut adjusting = false;
        let (in_auto, choices) = (facts.in_auto, facts.choices);
        const MIXED: &str = "Mixed — showing the first layer; a change applies to all.";

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(theme::title(&title));
            if let Some(g) = group {
                // Seleksi persis satu group: namanya bisa diubah di sini.
                let mut name = self.doc.group_name(g).to_owned();
                sub(ui, "Group name");
                if ui.add(egui::TextEdit::singleline(&mut name).desired_width(f32::INFINITY)).changed() {
                    let now = ctx.input(|i| i.time);
                    self.set_group_name(g, name, now);
                }
            }
            ui.weak(format!("{} layers selected", ids.len()));
            section(ui, "Position");
            sub(ui, "Alignment");
            if let Some(mode) = align_buttons(ui) {
                align = Some(mode);
            }
            ui.add_enabled_ui(!pinned, |ui| {
                if let Some(mode) = distribute_buttons(ui, units) {
                    align = Some(mode);
                }
            });
            let half = (ui.available_width() - 8.0) / 2.0;
            let spins = spinnable && !pinned;
            sub(ui, if spins { "Rotation" } else { "Flip" });
            ui.horizontal(|ui| {
                if spins {
                    ui.add_sized(
                        [half, 28.0],
                        egui::DragValue::new(&mut n.rotation).speed(1.0).range(-360.0..=360.0).max_decimals(1).suffix("°"),
                    )
                    .on_hover_text("Rotates the selection together around its center. Or drag just outside a corner of the selection box (Shift = 15° steps)");
                    ui.add_space(6.0);
                }
                flip = flip_buttons(ui);
            });
            if !pinned {
                sized = sizing_row(ui, half, sizes, (&choices[0], &choices[1]));
                if in_auto {
                    ui.checkbox(&mut n.absolute, "Absolute position").on_hover_text("Left out of the auto layout of their frame: they stay where you put them.");
                }
                auto_act = if auto_all { layout_section(ui, &mut n.layout, None, if mixed_layout { MIXED } else { "" }) } else { layout_section(ui, &mut None, None, "") };
            }
            if let Some(shown) = constrained {
                constrain = super::constraints::constraints_section(ui, shown, pinned);
            }

            section(ui, "Appearance");
            if mixed_look {
                ui.weak(MIXED);
            }
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    sub(ui, "Opacity");
                    let mut pct = n.opacity * 100.0;
                    let dv = egui::DragValue::new(&mut pct).speed(1.0).range(0.0..=100.0).max_decimals(0).suffix("%");
                    if ui.add_sized([half, 28.0], dv).changed() {
                        n.opacity = pct / 100.0;
                    }
                });
                if rounded {
                    ui.vertical(|ui| {
                        sub(ui, "Corner radius");
                        ui.add_sized([half, 28.0], egui::DragValue::new(&mut n.radius).speed(0.5).range(0.0..=2000.0).max_decimals(1));
                    });
                }
            });
            egui::ComboBox::from_id_salt("blend")
                .width(half)
                .selected_text(blend::NAMES[(n.blend as usize).min(blend::NAMES.len() - 1)])
                .show_ui(ui, |ui| {
                    for (i, name) in blend::NAMES.iter().enumerate() {
                        ui.selectable_value(&mut n.blend, i as u8, *name);
                    }
                })
                .response
                .on_hover_text("Blend mode");

            if all_text {
                self.typography(ui, ctx, &mut n, false, None, Some(if mixed_type { MIXED } else { "" }));
            }
            if fillable {
                section(ui, "Fill");
                if mixed_fill {
                    ui.weak(MIXED);
                }
                let gradient = if any_text { None } else { Some(&mut n.gradient) };
                paint_row(ui, &mut self.ext.picker, (key, 0), &mut n.fill, gradient, [217, 217, 217, 255]);
            }
            if !any_text {
                section(ui, "Stroke");
                if mixed_stroke {
                    ui.weak(MIXED);
                }
                paint_row(ui, &mut self.ext.picker, (key, 1), &mut n.stroke, None, [0, 0, 0, 255]);
                if n.stroke.is_some() {
                    ui.add_sized(
                        [half, 28.0],
                        egui::DragValue::new(&mut n.stroke_w).speed(0.25).range(0.0..=500.0).max_decimals(2).prefix("Weight  "),
                    );
                }
            }
            if images {
                adjusting = adjust_section(ui, &mut n.adjust, if mixed_adjust { MIXED } else { "" }, overlaid);
            }
            // Anggota satu group dengan bayangan yang sama berbayangan bersama (lihat `shadow_run`).
            effects_section(
                ui,
                &mut self.ext.picker,
                key,
                &mut n,
                true,
                inner_all,
                glass_all,
                if mixed_blur { MIXED } else { "" },
                if group.is_some() {
                    "Blurs the whole group as one layer."
                } else {
                    "Blurs each selected layer. Group them first to blur them together as one."
                },
            );
            if !pinned {
                comp_act = self.component_section(ui);
            }
            self.export_section(ui, ctx);
            ui.add_space(12.0);
        });

        if let Some(mode) = align {
            self.align(mode);
            return;
        }
        if let Some(horizontal) = flip {
            self.flip(horizontal);
            return;
        }
        if let Some(act) = comp_act {
            self.component_act(act);
            return;
        }
        if constrain != (None, None) {
            self.set_constraints(constrain);
            return;
        }
        match auto_act {
            Some(super::autolayout::Act::Add) => return self.add_auto_layout(),
            Some(super::autolayout::Act::Remove) => return self.remove_auto_layout(),
            None => {}
        }
        let now = ctx.input(|i| i.time);
        let undo_key = key | 1 << 63;
        if adjusting && self.last_prop.0 == undo_key {
            self.last_prop.1 = now;
        }
        if n == before && sized == (None, None) {
            return;
        }
        // Perubahan beruntun pada seleksi yang sama digabung jadi satu langkah undo.
        if self.last_prop.0 != undo_key || now - self.last_prop.1 > 0.8 {
            let pre = self.doc.clone();
            self.push_undo(pre);
        }
        self.last_prop = (undo_key, now);
        self.touch();

        if n.rotation != before.rotation {
            // Putar seleksi bersama terhadap titik tengah pusat-pusat objeknya (titik itu tidak
            // berubah oleh putaran, jadi menggeser nilainya berulang kali tidak membuat objek hanyut).
            let centers: Vec<(f32, f32)> = ids.iter().filter_map(|id| self.doc.find(*id)).map(|t| t.center()).collect();
            let count = centers.len().max(1) as f32;
            let pivot = (centers.iter().map(|c| c.0).sum::<f32>() / count, centers.iter().map(|c| c.1).sum::<f32>() / count);
            for id in &ids {
                if let Some(t) = self.doc.find_mut(*id) {
                    super::spin_node(t, pivot, n.rotation - before.rotation);
                }
            }
        }
        let paint = n.fill != before.fill || n.gradient != before.gradient;
        for id in ids {
            let Some(t) = self.doc.find_mut(id) else { continue };
            (t.size_h, t.size_v) = (sized.0.unwrap_or(t.size_h), sized.1.unwrap_or(t.size_v));
            if n.absolute != before.absolute {
                t.absolute = n.absolute;
            }
            if let (Some(new), Some(old), Some(l)) = (&n.layout, &before.layout, &mut t.layout) {
                // Only what was changed in the panel; the rest of each layout stays its own.
                if (new.dir, new.wrap) != (old.dir, old.wrap) {
                    (l.dir, l.wrap) = (new.dir, new.wrap);
                }
                if new.gap != old.gap {
                    l.gap = new.gap;
                }
                if new.row_gap != old.row_gap {
                    l.row_gap = new.row_gap;
                }
                for i in 0..4 {
                    if new.pad[i] != old.pad[i] {
                        l.pad[i] = new.pad[i];
                    }
                }
                if new.align != old.align {
                    l.align = new.align;
                }
                if new.cross != old.cross {
                    l.cross = new.cross;
                }
            }
            if n.shadow != before.shadow {
                t.shadow = n.shadow;
            }
            if n.adjust != before.adjust && t.kind == Kind::Image {
                // Only the sliders that were moved; the others stay as each image has them.
                let (new, old, mut mine) = (n.adjust.unwrap_or_default().values(), before.adjust.unwrap_or_default().values(), t.adjust.unwrap_or_default().values());
                for i in 0..mine.len() {
                    if new[i] != old[i] || n.adjust.is_none() {
                        mine[i] = new[i];
                    }
                }
                t.adjust = Adjust::from_values(mine).normalized();
            }
            if n.opacity != before.opacity {
                t.opacity = n.opacity;
            }
            if n.blend != before.blend {
                t.blend = n.blend;
            }
            if n.blur != before.blur {
                t.blur = n.blur;
            }
            if n.inner_shadow != before.inner_shadow {
                t.inner_shadow = n.inner_shadow;
            }
            if n.bg_blur != before.bg_blur {
                t.bg_blur = n.bg_blur;
            }
            if n.radius != before.radius && matches!(t.kind, Kind::Rect | Kind::Image | Kind::Path) {
                t.radius = n.radius;
                t.clear_corner_radii();
            }
            if paint {
                t.fill = n.fill;
                t.gradient = if t.kind == Kind::Text { None } else { n.gradient.clone() };
                if let Some(c) = n.fill {
                    // Warna per rentang huruf ikut diganti supaya seluruh teks berubah.
                    for span in &mut t.spans {
                        span.style.fill = c;
                    }
                }
            }
            if n.stroke != before.stroke {
                t.stroke = n.stroke;
                if t.stroke.is_some() && t.stroke_w <= 0.0 {
                    t.stroke_w = n.stroke_w.max(1.0);
                }
            }
            if n.stroke_w != before.stroke_w {
                t.stroke_w = n.stroke_w;
            }
            if t.kind != Kind::Text {
                continue;
            }
            if n.font != before.font {
                t.font = n.font.clone();
                for span in &mut t.spans {
                    span.style.font = n.font.clone();
                }
            }
            if n.weight != before.weight {
                t.weight = n.weight;
                for span in &mut t.spans {
                    span.style.weight = n.weight;
                }
            }
            if n.italic != before.italic {
                t.italic = n.italic;
                for span in &mut t.spans {
                    span.style.italic = n.italic;
                }
            }
            if n.font_size != before.font_size {
                t.font_size = n.font_size;
                for span in &mut t.spans {
                    span.style.size = n.font_size;
                }
            }
            if n.line_height != before.line_height {
                t.line_height = n.line_height;
            }
            if n.letter_spacing != before.letter_spacing {
                t.letter_spacing = n.letter_spacing;
            }
            if n.align != before.align {
                t.align = n.align;
            }
            if n.underline != before.underline {
                t.set_decoration(false, n.underline);
            }
            if n.strike != before.strike {
                t.set_decoration(true, n.strike);
            }
            if n.list != before.list {
                t.list = n.list;
            }
            if n.valign != before.valign {
                t.valign = n.valign;
            }
            if n.fixed_h != before.fixed_h {
                t.fixed_h = n.fixed_h;
            }
            if n.wrap != before.wrap {
                t.wrap = n.wrap;
                if t.wrap {
                    t.w = t.w.max(20.0);
                }
            }
        }
    }

    /// `multi`: beberapa teks terpilih sekaligus (isi teks tidak ditampilkan); isinya catatan
    /// yang tampil di bawah judul, mis. saat nilainya berbeda-beda.
    fn typography(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        n: &mut Node,
        editing: bool,
        range: Option<(usize, usize)>,
        multi: Option<&str>,
    ) {
        section(ui, "Typography");
        if let Some(note) = multi.filter(|s| !s.is_empty()) {
            ui.weak(note);
        }
        match range {
            Some((a, b)) => {
                ui.colored_label(ACCENT, format!("Styling {} selected characters", b - a));
            }
            None if editing => {
                ui.weak("Select characters to style only that part.");
            }
            None => {}
        }
        let half = (ui.available_width() - 8.0) / 2.0;

        // Font family: tombol yang membuka daftar Google Fonts dengan preview.
        let preview = self.fonts.resolve_preview(&n.font);
        let label = egui::RichText::new(format!("{}   ⏷", n.font)).family(preview.unwrap_or(egui::FontFamily::Proportional)).size(15.0);
        let btn = ui.add_sized([ui.available_width(), 30.0], egui::Button::new(label)).on_hover_text("Font family");
        let popup_id = ui.make_persistent_id("font_popup");
        if btn.clicked() {
            ui.memory_mut(|m| m.toggle_popup(popup_id));
            self.font_focus = true;
        }
        egui::popup::popup_below_widget(ui, popup_id, &btn, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
            ui.set_min_width(270.0);
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.font_search)
                    .hint_text("Search Google Fonts…")
                    .desired_width(f32::INFINITY),
            );
            if self.font_focus {
                search.request_focus();
                self.font_focus = false;
            }
            match &self.fonts.catalog_state {
                CatalogState::Loading => {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Loading Google Fonts…");
                    });
                }
                CatalogState::Failed(e) => {
                    ui.colored_label(Color32::from_rgb(255, 120, 100), format!("Couldn't load the font list: {e}"));
                    if ui.button("Retry").clicked() {
                        self.fonts.load_catalog(ctx);
                    }
                }
                CatalogState::Ready => {
                    let q = self.font_search.trim().to_lowercase();
                    let filtered: Vec<usize> = self
                        .fonts
                        .catalog
                        .iter()
                        .enumerate()
                        .filter(|(_, f)| q.is_empty() || f.lower.contains(&q))
                        .map(|(i, _)| i)
                        .collect();
                    ui.weak(format!("{} families", filtered.len()));
                    // Tiap baris ditulis dengan font-nya sendiri (potongan kecil, diunduh saat terlihat).
                    let row_h = 30.0;
                    ui.spacing_mut().item_spacing.y = 0.0;
                    egui::ScrollArea::vertical()
                        .id_salt("font_list")
                        .max_height(380.0)
                        .auto_shrink([false, false])
                        .show_rows(ui, row_h, filtered.len(), |ui, rows| {
                            for i in rows {
                                let name = self.fonts.catalog[filtered[i]].name.clone();
                                let preview = self.fonts.resolve_preview(&name);
                                let f = &self.fonts.catalog[filtered[i]];
                                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click());
                                let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                                if n.font == f.name {
                                    painter.rect_filled(rect, CornerRadius::same(4), ACCENT.gamma_multiply(0.35));
                                } else if resp.hovered() {
                                    painter.rect_filled(rect, CornerRadius::same(4), ui.visuals().widgets.hovered.weak_bg_fill);
                                }
                                let color = if preview.is_some() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                                painter.text(
                                    pos2(rect.left() + 8.0, rect.center().y),
                                    Align2::LEFT_CENTER,
                                    &f.name,
                                    FontId::new(17.0, preview.unwrap_or(egui::FontFamily::Proportional)),
                                    color,
                                );
                                let resp = resp.on_hover_text(format!("{} · {} · {} styles", f.name, f.category, f.variants.len()));
                                if resp.clicked() {
                                    n.font = f.name.clone();
                                    if !f.variants.contains(&(n.weight, n.italic)) {
                                        n.italic = false;
                                        n.weight = f
                                            .variants
                                            .iter()
                                            .filter(|v| !v.1)
                                            .map(|v| v.0)
                                            .min_by_key(|w| w.abs_diff(n.weight))
                                            .unwrap_or(400);
                                    }
                                    ui.memory_mut(|m| m.close_popup());
                                }
                            }
                        });
                }
            }
        });
        if let Some(s) = self.fonts.status(&n.font, n.weight, n.italic) {
            ui.weak(s);
        }

        // Weight + size
        let catalog_ready = matches!(self.fonts.catalog_state, CatalogState::Ready);
        let variants: Vec<(u16, bool)> = self
            .fonts
            .family(&n.font)
            .map(|f| f.variants.clone())
            .unwrap_or_else(|| vec![(400, false), (700, false)]);
        let mut weights: Vec<u16> = variants.iter().map(|v| v.0).collect();
        weights.dedup();
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("weight")
                .width(half)
                .selected_text(weight_name(n.weight))
                .show_ui(ui, |ui| {
                    for w in &weights {
                        ui.selectable_value(&mut n.weight, *w, format!("{}  {}", weight_name(*w), w));
                    }
                });
            ui.add_sized(
                [half, 28.0],
                egui::DragValue::new(&mut n.font_size).speed(0.5).range(1.0..=2000.0).max_decimals(1).prefix("Size  "),
            );
        });
        ui.horizontal(|ui| {
            let has_italic = variants.contains(&(n.weight, true));
            ui.add_enabled(has_italic || n.italic, egui::Checkbox::new(&mut n.italic, "Italic"));
            if catalog_ready && !has_italic {
                n.italic = false;
            }
            // Underline and strikethrough: like the rest here, for the selected characters only
            // while some are selected in the editor.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                if icon_button(ui, n.strike, "Strikethrough  (Ctrl+Shift+X)", |p, c, color| decoration_icon(p, c, true, color)) {
                    n.strike = !n.strike;
                }
                if icon_button(ui, n.underline, "Underline  (Ctrl+U)", |p, c, color| decoration_icon(p, c, false, color)) {
                    n.underline = !n.underline;
                }
            });
        });

        // Line height + letter spacing (persen dari ukuran font) berlaku untuk seluruh teks.
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                sub(ui, "Line height");
                ui.horizontal(|ui| {
                    if n.line_height <= 0.0 {
                        if ui.add_sized([half, 28.0], egui::Button::new("Auto")).on_hover_text("Click to set a custom line height").clicked() {
                            n.line_height = 120.0;
                        }
                    } else {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.add_sized(
                            [half - 24.0, 28.0],
                            egui::DragValue::new(&mut n.line_height).speed(1.0).range(40.0..=600.0).max_decimals(0).suffix("%"),
                        );
                        if ui.add_sized([22.0, 28.0], egui::Button::new("A")).on_hover_text("Back to Auto").clicked() {
                            n.line_height = 0.0;
                        }
                    }
                });
            });
            ui.vertical(|ui| {
                sub(ui, "Letter spacing");
                ui.add_sized(
                    [half, 28.0],
                    egui::DragValue::new(&mut n.letter_spacing).speed(0.1).range(-50.0..=300.0).max_decimals(1).suffix("%"),
                );
            });
        });

        sub(ui, "Alignment");
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (mode, tip) in [(0u8, "Align left"), (1, "Align center"), (2, "Align right"), (3, "Justify (fixed width: every line but the last of a paragraph fills the box)")] {
                if icon_button(ui, n.align == mode, tip, |p, c, color| text_align_icon(p, c, mode, color)) {
                    n.align = mode;
                }
            }
        });

        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.set_width(half);
                sub(ui, "List");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    for (mode, tip) in [(0u8, "No list"), (1, "Bulleted list: a bullet before every paragraph"), (2, "Numbered list: a number before every paragraph")] {
                        if icon_button(ui, n.list == mode, tip, |p, c, color| list_icon(p, c, mode, color)) {
                            n.list = mode;
                        }
                    }
                });
            });
            ui.vertical(|ui| {
                sub(ui, "Vertical");
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    // Only seen in a box that is higher (or lower) than its lines.
                    ui.add_enabled_ui(n.fixed_h, |ui| {
                        for (mode, tip) in [(0u8, "Align top"), (1, "Align middle"), (2, "Align bottom")] {
                            let tip = if n.fixed_h { tip.to_owned() } else { format!("{tip} (needs a fixed height)") };
                            if icon_button(ui, n.fixed_h && n.valign == mode, &tip, |p, c, color| valign_icon(p, c, mode, color)) {
                                n.valign = mode;
                            }
                        }
                    });
                });
            });
        });

        sub(ui, "Resizing");
        ui.horizontal(|ui| {
            if ui.selectable_label(!n.wrap, "Auto width").on_hover_text("The box grows with the text").clicked() {
                n.wrap = false;
            }
            if ui.selectable_label(n.wrap, "Fixed width").on_hover_text("Paragraph: lines wrap inside the box").clicked() && !n.wrap {
                n.wrap = true;
                n.w = n.w.max(20.0);
            }
        });
        ui.horizontal(|ui| {
            if ui.selectable_label(!n.fixed_h, "Auto height").on_hover_text("The box is as high as its lines").clicked() {
                n.fixed_h = false;
            }
            let tip = "The box keeps its height (drag its top or bottom handle); the lines are aligned in it top, middle or bottom";
            if ui.selectable_label(n.fixed_h, "Fixed height").on_hover_text(tip).clicked() {
                n.fixed_h = true;
            }
        });

        if !editing && multi.is_none() {
            sub(ui, "Content");
            ui.add(egui::TextEdit::multiline(&mut n.text).desired_rows(2).desired_width(f32::INFINITY));
        }
    }

    // ------------------------------------------------------------ export

    /// Kontrol export: skala bebas (ketik angka), format, tombol.
    fn export_controls(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.horizontal(|ui| {
            ui.add_sized(
                [70.0, 28.0],
                egui::DragValue::new(&mut self.ext.export_scale).speed(0.05).range(0.1..=16.0).max_decimals(2).suffix("x"),
            )
            .on_hover_text("Scale: drag, or click and type any value (e.g. 2 or 0.5)");
            egui::ComboBox::from_id_salt(ui.id().with("export_format"))
                .width(70.0)
                .selected_text(FORMATS[self.ext.export_fmt.min(3)])
                .show_ui(ui, |ui| {
                    for (i, name) in FORMATS.iter().enumerate() {
                        ui.selectable_value(&mut self.ext.export_fmt, i, *name);
                    }
                });
        });
        self.print_controls(ui);
        let target = match self.sel.as_slice() {
            [id] => self.doc.find(*id).map(|n| n.name.clone()).unwrap_or_default(),
            _ if self.ext.export_fmt == 3 => match self.pdf_targets().len() {
                1 => "1 page".to_owned(),
                n => format!("{n} pages"),
            },
            sel if sel.len() > 1 => format!("{} files", sel.len()),
            _ => "whole document".to_owned(),
        };
        if self.exporting.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak(self.export_progress().unwrap_or_else(|| "Exporting…".into()));
            });
            return;
        }
        let label = format!("Export {}", target.chars().take(22).collect::<String>());
        if ui.add_sized([ui.available_width().max(150.0), 28.0], egui::Button::new(label)).clicked() {
            self.export_now(ctx);
        }
    }

    /// The "Print" group of the export controls: bleed and crop marks for every format, and
    /// for PDF the colour (RGB or CMYK), flattening and the ICC profile of the press.
    fn print_controls(&mut self, ui: &mut egui::Ui) {
        let pdf = self.ext.export_fmt == 3;
        let size = self.print_size();
        let mut note = None;
        let was = self.doc.print.clone();
        let p = &mut self.doc.print;
        // (Open from the start when something in it is set.)
        let open = *p != crate::print::Print::default();
        egui::CollapsingHeader::new("Print").id_salt(ui.id().with("export_print")).default_open(open).show(ui, |ui| {
            egui::Grid::new(ui.id().with("export_print_grid")).num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                ui.label("Bleed");
                ui.add(egui::DragValue::new(&mut p.bleed_mm).speed(0.1).range(0.0..=50.0).max_decimals(1).suffix(" mm"))
                    .on_hover_text("Extra area around the page, cut off after printing (usually 3 mm).
A frame's fill and the layers that stick out of it fill the bleed.");
                ui.end_row();
                ui.label("Resolution");
                ui.add(egui::DragValue::new(&mut p.ppi).speed(1.0).range(18.0..=2400.0).max_decimals(0).suffix(" ppi"))
                    .on_hover_text("How many px of the document make one inch on paper.
72: 1 px is 1 pt, as in the Paper presets (A4 = 595 × 842).
300: for a design made in 300 dpi pixels (A4 = 2480 × 3508).");
                ui.end_row();
                if pdf {
                    ui.label("Colour");
                    egui::ComboBox::from_id_salt(ui.id().with("export_colour")).width(70.0).selected_text(if p.cmyk { "CMYK" } else { "RGB" }).show_ui(ui, |ui| {
                        ui.selectable_value(&mut p.cmyk, false, "RGB");
                        ui.selectable_value(&mut p.cmyk, true, "CMYK");
                    });
                    ui.end_row();
                }
            });
            ui.checkbox(&mut p.marks, "Crop marks").on_hover_text("Trim marks at the four corners, in a margin around the bleed");
            if pdf {
                ui.checkbox(&mut p.flatten, "Flatten to image (300 dpi)")
                    .on_hover_text("Every page as one picture instead of vectors: looks exactly like the PNG export,
but text and shapes are no longer sharp at any size");
            }
            if pdf && p.cmyk {
                ui.horizontal(|ui| {
                    let name = p.icc.as_ref().and_then(|f| f.file_name()).map(|f| f.to_string_lossy().into_owned());
                    let label = name.as_deref().map_or("ICC profile…".to_owned(), |n| n.chars().take(20).collect());
                    let tip = "The CMYK profile of the press (.icc), from your print shop. It is embedded as the
output intent and colours are converted through it. Without one, colours are
converted by a plain formula that knows nothing about the press.";
                    if ui.button(label).on_hover_text(tip).clicked() {
                        if let Some(file) = rfd::FileDialog::new().add_filter("ICC profile", &["icc", "icm"]).pick_file() {
                            match crate::print::read_profile(&file) {
                                Ok(profile) => {
                                    note = Some(format!("ICC profile: {}", profile.name));
                                    p.icc = Some(file);
                                }
                                Err(e) => note = Some(format!("Can't use that profile: {e}")),
                            }
                        }
                    }
                    if p.icc.is_some() && ui.small_button("×").on_hover_text("Remove the profile").clicked() {
                        p.icc = None;
                    }
                });
            }
            if let Some((w, h)) = size {
                ui.weak(p.hint(w, h));
            }
        });
        if let Some(note) = note {
            self.status = note;
        }
        // The settings are saved with the document: a change of them is a change of it (but
        // not a step of its history: undo leaves them alone).
        if self.doc.print != was {
            self.touch();
        }
    }

    /// The size, in document px, of the first thing the Export button would write.
    fn print_size(&self) -> Option<(f32, f32)> {
        let first = match (self.sel.first(), self.ext.export_fmt) {
            (Some(id), _) => Some(*id),
            (None, 3) => export::pdf_pages(&self.doc).first().copied(),
            _ => None,
        };
        let (x0, y0, x1, y1) = match first {
            Some(id) => self.doc.find(id)?.aabb(),
            None => self.doc.bounds()?,
        };
        Some((x1 - x0, y1 - y0))
    }

    /// Is there anything `export::to_svg` would write for `only`?
    fn exportable(&self, only: Option<u64>) -> bool {
        match only {
            Some(id) => self.doc.find(id).is_some(),
            None => self.doc.bounds().is_some(),
        }
    }

    fn export_section(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        section(ui, "Export");
        self.export_controls(ui, ctx);
    }

    /// Halaman untuk export PDF: objek terpilih, atau semua frame kalau tidak ada yang dipilih.
    /// `None` di dalam daftar = seluruh dokumen sebagai satu halaman (dokumen tanpa frame).
    fn pdf_targets(&self) -> Vec<Option<u64>> {
        if !self.sel.is_empty() {
            return self.sel.iter().map(|id| Some(*id)).collect();
        }
        let frames = export::pdf_pages(&self.doc);
        if frames.is_empty() {
            return vec![None];
        }
        frames.into_iter().map(Some).collect()
    }

    fn export_pdf(&mut self, ctx: &egui::Context) {
        let targets: Vec<Option<u64>> = self.pdf_targets().into_iter().filter(|t| self.exportable(*t)).collect();
        if targets.is_empty() {
            self.status = "Nothing to export.".into();
            return;
        }
        let print = self.doc.print.clone();
        let name = match (targets.as_slice(), &self.path) {
            ([Some(id)], _) => self.doc.find(*id).map(|n| n.name.clone()).unwrap_or_default(),
            (_, Some(path)) => path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            _ => "kanvas".into(),
        };
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
        let Some(path) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{safe}.pdf")).save_file() else {
            return;
        };
        self.exporting = Some(self.export_job(ctx, move |cx| {
            let pages = cx.pages(&targets, &print, false, 1.0);
            if pages.is_empty() {
                return "Nothing to export.".into();
            }
            match cx.save_pdf(&pages, &print, &path) {
                Ok(colour) => format!("Exported {} page(s), {colour}: {}", pages.len(), path.display()),
                Err(e) => format!("Export failed: {e}"),
            }
        }));
    }

    /// Run an export on another thread, so the window does not freeze ("Not Responding") while
    /// a large document is turned into SVG and rendered. The work gets a copy of the document
    /// and of the fonts as they are now (see `ExportJob`); what it says while it runs is shown
    /// in the status bar, and its result arrives through the channel.
    pub(super) fn export_job<T: Send + 'static>(&mut self, ctx: &egui::Context, work: impl FnOnce(&mut ExportJob) -> T + Send + 'static) -> std::sync::mpsc::Receiver<T> {
        // Paragraphs that were not painted since their width changed get their height first.
        self.measure_paragraphs(ctx);
        let (tx, rx) = std::sync::mpsc::channel();
        let (doc, fonts, ppp, wake) = (self.doc.clone(), self.fonts.snapshot(), ctx.pixels_per_point(), ctx.clone());
        let note = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        self.export_note = note.clone();
        std::thread::spawn(move || {
            let ctx = fonts.worker_context(ppp);
            let mut job = ExportJob { doc, fonts, ctx, ppp, note, wake };
            let _ = tx.send(work(&mut job));
            job.wake.request_repaint();
        });
        self.status = "Exporting…".into();
        rx
    }

    /// Every frame: show how far the export is, and take its result when it is done.
    fn export_poll(&mut self) {
        let Some(rx) = &self.exporting else { return };
        self.status = match rx.try_recv() {
            Ok(status) => status,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                if let Some(note) = self.export_progress() {
                    self.status = note;
                }
                return;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => "Export failed: the export stopped unexpectedly (out of memory?)".into(),
        };
        self.exporting = None;
    }

    /// What the running export last said about its progress.
    pub(super) fn export_progress(&self) -> Option<String> {
        let note = self.export_note.lock().unwrap_or_else(|e| e.into_inner());
        (!note.is_empty()).then(|| note.clone())
    }

    fn export_now(&mut self, ctx: &egui::Context) {
        if self.ext.export_fmt == 3 {
            return self.export_pdf(ctx);
        }
        if self.sel.len() > 1 {
            return self.export_batch(ctx);
        }
        let only = if self.sel.len() == 1 { Some(self.sel[0]) } else { None };
        if !self.exportable(only) {
            self.status = "Nothing to export.".into();
            return;
        }
        let fmt = self.ext.export_fmt.min(3);
        let scale = self.ext.export_scale.clamp(0.1, 16.0);
        let print = self.doc.print.clone();
        let ext = FORMATS[fmt].to_lowercase();
        let name = only.and_then(|id| self.doc.find(id)).map(|n| n.name.clone()).unwrap_or_else(|| "kanvas".into());
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
        let suffix = if fmt < 2 && scale != 1.0 { format!("@{scale}x") } else { String::new() };
        let Some(path) = rfd::FileDialog::new()
            .add_filter(FORMATS[fmt], &[ext.as_str()])
            .set_file_name(format!("{safe}{suffix}.{ext}"))
            .save_file()
        else {
            return;
        };
        self.exporting = Some(self.export_job(ctx, move |cx| {
            let Some((svg, w, h)) = cx.svg(only, &print, true, scale) else { return "Nothing to export.".into() };
            let result = match fmt {
                2 => std::fs::write(&path, svg).map_err(|e| e.to_string()),
                _ => export::save_raster(&svg, w, h, scale, fmt == 1, &path),
            };
            match result {
                Ok(()) => format!("Exported: {}", path.display()),
                Err(e) => format!("Export failed: {e}"),
            }
        }));
    }

    /// Batch export: beberapa objek terpilih menjadi satu file per objek di folder pilihan,
    /// dengan skala dan format yang sedang aktif.
    fn export_batch(&mut self, ctx: &egui::Context) {
        let named: Vec<(u64, String)> = self.sel.iter().filter_map(|id| self.doc.find(*id).map(|n| (*id, n.name.clone()))).collect();
        if named.is_empty() {
            self.status = "Nothing to export.".into();
            return;
        }
        let fmt = self.ext.export_fmt.min(2);
        let scale = self.ext.export_scale.clamp(0.1, 16.0);
        let print = self.doc.print.clone();
        let Some(dir) = rfd::FileDialog::new().set_title("Choose a folder for the exported files").pick_folder() else {
            return;
        };
        self.exporting = Some(self.export_job(ctx, move |cx| {
            let targets: Vec<Option<u64>> = named.iter().map(|(id, _)| Some(*id)).collect();
            let pages = cx.pages(&targets, &print, true, scale);
            let items: Vec<(String, String, f32, f32)> = named.into_iter().zip(pages).map(|((_, name), (svg, w, h))| (name, svg, w, h)).collect();
            cx.note(format!("Exporting: writing {} files…", items.len()));
            match export::save_batch(&items, &dir, fmt, scale) {
                (n, None) => format!("Exported {n} files: {}", dir.display()),
                (n, Some(e)) => format!("Exported {n} of {} files, failed on {e}", items.len()),
            }
        }));
    }

    // ------------------------------------------------------------ toolbar

    /// The toolbar floating at the bottom of the canvas. It always fits inside the canvas: when
    /// the canvas is narrow its buttons get smaller, and after that the less used ones move into
    /// a "More" popup (see `bar_fit`), so it never covers the side panels.
    pub(super) fn toolbar(&mut self, ctx: &egui::Context) {
        let c = self.canvas_rect;
        let margin = if c.width() < 160.0 { 4.0 } else { 12.0 };
        let fit = bar_fit(c.width() - margin * 2.0);
        egui::Area::new(egui::Id::new("kanvas_toolbar"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(c.center().x, c.bottom() - 18.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::CARD)
                    .corner_radius(CornerRadius::same(if fit.size < 38.0 { 15 } else { 18 }))
                    .stroke(Stroke::new(1.0_f32, Color32::from_gray(66)))
                    .inner_margin(egui::Margin::same(BAR_MARGIN as i8))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(110) })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = fit.gap;
                            let export_id = ui.make_persistent_id("export_popup");
                            let more_id = ui.make_persistent_id("toolbar_more");
                            let mut anchor = None;
                            let mut last: Option<BarItem> = None;
                            for item in &fit.shown {
                                if last.is_some_and(|l| l.group() != item.group()) {
                                    bar_separator(ui, &fit);
                                }
                                last = Some(*item);
                                let resp = self.bar_button(ui, *item, fit.size, export_id);
                                if *item == BarItem::Export {
                                    anchor = Some(resp);
                                }
                            }
                            if !fit.more.is_empty() {
                                // The tools that did not fit: a popup with a grid of them.
                                let open = ui.memory(|m| m.is_popup_open(more_id));
                                let inside = fit.more.iter().any(|i| *i != BarItem::Export && self.bar_active(*i, export_id, ui));
                                let (rect, resp) = bar_slot(ui, open || inside, fit.size);
                                let color = if open || inside { Color32::WHITE } else { Color32::from_gray(225) };
                                for dx in [-6.0, 0.0, 6.0] {
                                    ui.painter().circle_filled(rect.center() + vec2(dx, 0.0), 1.7, color);
                                }
                                let resp = resp.on_hover_text("More tools");
                                if resp.clicked() {
                                    ui.memory_mut(|m| m.toggle_popup(more_id));
                                }
                                egui::popup::popup_above_or_below_widget(ui, more_id, &resp, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                                    ui.spacing_mut().item_spacing = vec2(4.0, 4.0);
                                    let columns = fit.more.len().clamp(1, 5);
                                    ui.set_max_width(columns as f32 * 34.0 + (columns - 1) as f32 * 4.0);
                                    ui.horizontal_wrapped(|ui| {
                                        for item in &fit.more {
                                            // (Export opens its own popup in place of this one.)
                                            if self.bar_button(ui, *item, 34.0, export_id).clicked() && *item != BarItem::Export {
                                                ui.memory_mut(|m| m.close_popup());
                                            }
                                        }
                                    });
                                });
                                anchor = anchor.or(Some(resp));
                            }
                            // Export: a popup above the toolbar with the scale and the format.
                            if let Some(resp) = anchor {
                                egui::popup::popup_above_or_below_widget(ui, export_id, &resp, egui::AboveOrBelow::Above, egui::PopupCloseBehavior::CloseOnClickOutside, |ui| {
                                    ui.set_min_width(190.0);
                                    ui.label(egui::RichText::new("Export").strong());
                                    self.export_controls(ui, ctx);
                                });
                            }
                        });
                    });
            });
    }

    /// `--bench`: where the "More" button of the toolbar is, if the toolbar has one now.
    pub(super) fn bench_more_button(&self) -> Option<Pos2> {
        let c = self.canvas_rect;
        let fit = bar_fit(c.width() - if c.width() < 160.0 { 4.0 } else { 12.0 } * 2.0);
        (!fit.more.is_empty()).then(|| pos2(c.center().x + fit.width() * 0.5 - BAR_MARGIN - 1.0 - fit.size * 0.5, c.bottom() - 18.0 - BAR_MARGIN - 1.0 - fit.size * 0.5))
    }

    /// Is the thing behind this toolbar button on (the tool chosen, the window open)?
    fn bar_active(&self, item: BarItem, export_id: egui::Id, ui: &egui::Ui) -> bool {
        match item {
            BarItem::Tool(i) => self.tool == TOOLS[i].0,
            BarItem::Eyedrop => self.ext.picker.eyedrop.is_some(),
            BarItem::Icons => self.icons.open,
            BarItem::Photos => self.photos.open,
            BarItem::Library => self.library.open,
            BarItem::Components => self.components.open,
            BarItem::Export => ui.memory(|m| m.is_popup_open(export_id)),
        }
    }

    /// One button of the toolbar (or of its "More" popup): drawn, and acted on when clicked.
    fn bar_button(&mut self, ui: &mut egui::Ui, item: BarItem, size: f32, export_id: egui::Id) -> egui::Response {
        let active = self.bar_active(item, export_id, ui);
        let (rect, resp) = bar_slot(ui, active, size);
        let color = if active { Color32::WHITE } else { Color32::from_gray(225) };
        let (p, c) = (ui.painter(), rect.center());
        let tip = match item {
            BarItem::Tool(i) => {
                draw_icon(p, TOOLS[i].0, c, color);
                format!("{}   {}", TOOLS[i].1, TOOLS[i].2)
            }
            // The eyedropper is not a canvas tool: it is a mode for one pick.
            BarItem::Eyedrop => {
                super::picker::eyedropper_icon(p, c, Stroke::new(1.6_f32, color));
                "Eyedropper   I".into()
            }
            BarItem::Icons => {
                super::icons::toolbar_icon(p, c, color);
                "Icons".into()
            }
            BarItem::Photos => {
                super::photos::toolbar_icon(p, c, color);
                "Photos".into()
            }
            BarItem::Library => {
                super::library::toolbar_icon(p, c, color);
                "Library: your own logos and images".into()
            }
            BarItem::Components => {
                super::components::toolbar_icon(p, c, color);
                "Components".into()
            }
            BarItem::Export => {
                export_icon(p, c, color);
                "Export".into()
            }
        };
        let resp = resp.on_hover_text(tip);
        if resp.clicked() {
            match item {
                BarItem::Tool(i) => self.tool = TOOLS[i].0,
                BarItem::Eyedrop => self.toggle_eyedropper(),
                BarItem::Icons => self.toggle_icons(),
                BarItem::Photos => self.toggle_photos(),
                BarItem::Library => self.toggle_library(),
                BarItem::Components => self.toggle_components(),
                BarItem::Export => ui.memory_mut(|m| m.toggle_popup(export_id)),
            }
        }
        resp
    }
}

/// What `App::multi_props` shows about the selection as a whole.
#[derive(Clone)]
pub(super) struct SelFacts {
    /// The group, when the selection is exactly one whole group.
    group: Option<u64>,
    /// A part of an instance is selected.
    pinned: bool,
    /// How many units distribute and tidy up would see.
    units: usize,
    constrained: Option<super::constraints::Pair>,
    /// Every selected layer is an item of an auto layout.
    in_auto: bool,
    /// The sizes every selected layer can have: horizontal, vertical.
    choices: [Vec<Sizing>; 2],
}

/// What an export has to work with on its own thread: the document and the fonts as they were
/// when it started, and a context of its own to lay text out with (same fonts, same pixels
/// per point, so the result is what the window's thread would have written).
pub(super) struct ExportJob {
    pub doc: Doc,
    pub fonts: FontManager,
    pub ctx: egui::Context,
    ppp: f32,
    note: std::sync::Arc<std::sync::Mutex<String>>,
    /// The context of the window, to have it show the progress.
    wake: egui::Context,
}

impl ExportJob {
    /// Progress, for the status bar.
    pub fn note(&self, text: String) {
        *self.note.lock().unwrap_or_else(|e| e.into_inner()) = text;
        self.wake.request_repaint();
    }

    /// The export of `only` with the print settings: the bleed around it (see
    /// `export::to_svg_bleed`) and, with `marks`, for the formats that are one picture, the
    /// crop marks (for a picture exported at `scale`).
    pub fn svg(&mut self, only: Option<u64>, print: &crate::print::Print, marks: bool, scale: f32) -> Option<(String, f32, f32)> {
        let (svg, w, h) = export::to_svg_bleed(&self.doc, only, print.bleed_px(), &mut self.fonts, &self.ctx)?;
        // (Lets go of the glyphs and layouts of this page.)
        self.fonts.worker_pass(&self.ctx, self.ppp);
        Some(if marks { crate::print::sheet_svg(svg, w, h, print, scale) } else { (svg, w, h) })
    }

    /// `print::save_pdf`, reporting which page it is at.
    pub fn save_pdf(&self, pages: &[(String, f32, f32)], print: &crate::print::Print, path: &std::path::Path) -> Result<String, String> {
        let n = pages.len();
        crate::print::save_pdf_with(pages, print, path, &mut |i| self.note(format!("Exporting page {}/{n}…", i + 1)))
    }

    /// `svg` for each of `targets`, reporting which one it is at.
    pub fn pages(&mut self, targets: &[Option<u64>], print: &crate::print::Print, marks: bool, scale: f32) -> Vec<(String, f32, f32)> {
        let mut pages = Vec::new();
        for (i, target) in targets.iter().enumerate() {
            self.note(format!("Exporting: preparing page {}/{}…", i + 1, targets.len()));
            pages.extend(self.svg(*target, print, marks, scale));
        }
        pages
    }
}

/// A button of the bottom toolbar, in the order they are shown.
#[derive(Clone, Copy, PartialEq, Debug)]
enum BarItem {
    /// An index into `TOOLS`.
    Tool(usize),
    Eyedrop,
    Icons,
    Photos,
    Library,
    Components,
    Export,
}

impl BarItem {
    fn all() -> Vec<BarItem> {
        let tools = (0..TOOLS.len()).map(BarItem::Tool);
        tools.chain([BarItem::Eyedrop, BarItem::Icons, BarItem::Photos, BarItem::Library, BarItem::Components, BarItem::Export]).collect()
    }

    /// Buttons of one group stand together; a thin line separates the groups.
    fn group(self) -> u8 {
        match self {
            BarItem::Tool(0..=3) => 0,
            BarItem::Tool(4) => 1,
            BarItem::Tool(5..=10) => 2,
            BarItem::Tool(_) | BarItem::Eyedrop => 3,
            BarItem::Export => 5,
            _ => 4,
        }
    }

    /// The order in which buttons leave a toolbar that is too narrow: the highest goes first.
    fn rank(self) -> u8 {
        match self {
            BarItem::Tool(i) => match TOOLS[i].0 {
                Tool::Select => 0,
                Tool::Hand => 2,
                Tool::Frame => 3,
                Tool::Rect => 4,
                Tool::Text => 5,
                Tool::Pen => 6,
                Tool::Ellipse => 7,
                Tool::Line => 8,
                Tool::Scale => 9,
                Tool::Polygon => 10,
                Tool::Wand => 12,
                _ => 13,
            },
            BarItem::Export => 1,
            BarItem::Eyedrop => 11,
            BarItem::Components => 14,
            BarItem::Icons => 15,
            BarItem::Photos => 16,
            BarItem::Library => 17,
        }
    }
}

/// Space between the toolbar's edge and its buttons.
const BAR_MARGIN: f32 = 6.0;

/// How the bottom toolbar is laid out for the width it has (see `bar_fit`).
struct BarFit {
    /// Side of a button, space between two widgets, width of a separator.
    size: f32,
    gap: f32,
    sep: f32,
    shown: Vec<BarItem>,
    /// The buttons that moved into the "More" popup.
    more: Vec<BarItem>,
}

impl BarFit {
    /// The width of the whole toolbar, its edge included.
    fn width(&self) -> f32 {
        let seps = self.shown.windows(2).filter(|w| w[0].group() != w[1].group()).count();
        let buttons = self.shown.len() + !self.more.is_empty() as usize;
        let widgets = buttons + seps;
        buttons as f32 * self.size + seps as f32 * self.sep + widgets.saturating_sub(1) as f32 * self.gap + BAR_MARGIN * 2.0 + 2.0
    }
}

/// The toolbar for a canvas with `avail` px of room: at full size when that fits, else with
/// smaller buttons, else with the least used buttons (by `BarItem::rank`) in the "More" popup.
/// At least the "More" button is always there.
fn bar_fit(avail: f32) -> BarFit {
    let mut fit = BarFit { size: 38.0, gap: 4.0, sep: 9.0, shown: BarItem::all(), more: Vec::new() };
    if fit.width() <= avail {
        return fit;
    }
    (fit.size, fit.gap, fit.sep) = (32.0, 2.0, 7.0);
    while fit.width() > avail && !fit.shown.is_empty() {
        let Some(at) = (0..fit.shown.len()).max_by_key(|i| fit.shown[*i].rank()) else { break };
        fit.more.push(fit.shown.remove(at));
    }
    // In the popup they are in toolbar order again.
    let all = BarItem::all();
    fit.more.sort_by_key(|item| all.iter().position(|a| a == item));
    fit
}

fn bar_separator(ui: &mut egui::Ui, fit: &BarFit) {
    let (sep, _) = ui.allocate_exact_size(vec2(fit.sep, fit.size), Sense::hover());
    ui.painter().vline(sep.center().x, sep.y_range().shrink(8.0), Stroke::new(1.0_f32, Color32::from_gray(74)));
}

/// The square of a toolbar button, with its background when it is on or under the pointer.
fn bar_slot(ui: &mut egui::Ui, active: bool, size: f32) -> (Rect, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(size, size), Sense::click());
    let radius = CornerRadius::same(if size < 38.0 { 10 } else { 12 });
    if active {
        ui.painter().rect_filled(rect, radius, ACCENT);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, radius, Color32::from_gray(64));
    }
    (rect, resp)
}

/// Bagian "Effects": tombol + membuka daftar efek yang bisa ditambahkan; tiap efek yang terpasang
/// tampil sebagai satu blok dengan tombol − untuk melepasnya. `shadow` = drop shadow boleh diatur
/// di sini; `note` = catatan di bawah judul (mis. nilai campuran); `blur_tip` = penjelasan blur.
/// `inner` / `glass`: the selection can have an inner shadow / a background blur (it has an
/// inside; it is a shape, an image or a frame).
#[allow(clippy::too_many_arguments)]
fn effects_section(ui: &mut egui::Ui, picker: &mut super::picker::Picker, id: u64, n: &mut Node, shadow: bool, inner: bool, glass: bool, note: &str, blur_tip: &str) {
    use crate::doc::Shadow;
    /// What a new inner shadow starts as.
    const INNER: Shadow = Shadow { x: 0.0, y: 4.0, blur: 8.0, color: [0, 0, 0, 64] };
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(theme::title("Effects"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.menu_button("+", |ui| {
                if shadow && ui.add_enabled(n.shadow.is_none(), egui::Button::new("Drop shadow")).clicked() {
                    n.shadow = Some(Shadow::default());
                    ui.close_menu();
                }
                if inner && ui.add_enabled(n.inner_shadow.is_none(), egui::Button::new("Inner shadow")).clicked() {
                    n.inner_shadow = Some(INNER);
                    ui.close_menu();
                }
                if ui.add_enabled(n.blur <= 0.0, egui::Button::new("Layer blur")).clicked() {
                    n.blur = 4.0;
                    ui.close_menu();
                }
                if glass && ui.add_enabled(n.bg_blur <= 0.0, egui::Button::new("Background blur")).clicked() {
                    n.bg_blur = 12.0;
                    ui.close_menu();
                }
            })
            .response
            .on_hover_text("Add effect");
        });
    });
    if !note.is_empty() {
        ui.weak(note);
    }
    if shadow && n.shadow.is_some() {
        sub(ui, "Drop shadow");
        let mut color = n.shadow.map(|s| s.color);
        paint_row(ui, picker, (id, 2), &mut color, None, Shadow::default().color);
        n.shadow = match (color, n.shadow) {
            (Some(color), Some(s)) => Some(Shadow { color, ..s }),
            _ => None,
        };
        if let Some(s) = &mut n.shadow {
            ui.horizontal(|ui| {
                let third = (ui.available_width() - 16.0) / 3.0;
                ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.x).speed(0.25).max_decimals(1).prefix("X  "));
                ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.y).speed(0.25).max_decimals(1).prefix("Y  "));
                ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.blur).speed(0.25).range(0.0..=500.0).max_decimals(1).prefix("Blur  "));
            });
        }
    }
    if inner && n.inner_shadow.is_some() {
        sub(ui, "Inner shadow");
        let mut color = n.inner_shadow.map(|s| s.color);
        paint_row(ui, picker, (id, 4), &mut color, None, INNER.color);
        n.inner_shadow = match (color, n.inner_shadow) {
            (Some(color), Some(s)) => Some(Shadow { color, ..s }),
            _ => None,
        };
        if let Some(s) = &mut n.inner_shadow {
            ui.horizontal(|ui| {
                let third = (ui.available_width() - 16.0) / 3.0;
                ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.x).speed(0.25).max_decimals(1).prefix("X  "));
                ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.y).speed(0.25).max_decimals(1).prefix("Y  "));
                ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.blur).speed(0.25).range(0.0..=500.0).max_decimals(1).prefix("Blur  "));
            });
        }
    }
    if glass && n.bg_blur > 0.0 {
        sub(ui, "Background blur");
        ui.horizontal(|ui| {
            let half = (ui.available_width() - 8.0) / 2.0;
            let tip = if n.blur > 0.0 {
                "Blurs what is behind this layer, inside its shape. Not shown while the layer has a layer blur."
            } else {
                "Blurs what is behind this layer, inside its shape. It shows where the fill is see-through, or where there is none."
            };
            blur_field(ui, half, &mut n.bg_blur).on_hover_text(tip);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("−").on_hover_text("Remove").clicked() {
                    n.bg_blur = 0.0;
                }
            });
        });
    }
    if n.blur > 0.0 {
        sub(ui, "Layer blur");
        ui.horizontal(|ui| {
            let half = (ui.available_width() - 8.0) / 2.0;
            blur_field(ui, half, &mut n.blur).on_hover_text(blur_tip);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("−").on_hover_text("Remove").clicked() {
                    n.blur = 0.0;
                }
            });
        });
    }
}

/// The "Adjust" section of an image: a slider per value, from -100 to 100. Double-click a
/// slider to put it back to 0. `overlaid`: the image has a colour overlay (a Fill), which shows
/// its silhouette in one colour, so adjustments would change nothing: the sliders are off.
/// Result: a slider is held down.
fn adjust_section(ui: &mut egui::Ui, adjust: &mut Option<Adjust>, note: &str, overlaid: bool) -> bool {
    let was = adjust.unwrap_or_default();
    let mut values = was.values();
    let mut held = false;
    section(ui, "Adjust");
    if !note.is_empty() {
        ui.weak(note);
    }
    if overlaid {
        ui.weak("No effect while the image has a colour overlay: remove its Fill to adjust it.");
    }
    let double = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
    ui.add_enabled_ui(!overlaid, |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        for (v, (_, label)) in values.iter_mut().zip(crate::adjust::FIELDS) {
            ui.horizontal(|ui| {
                let text = egui::RichText::new(label).size(12.0);
                let text = if *v == 0.0 { text.weak() } else { text };
                let name = ui.allocate_ui_with_layout(vec2(84.0, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.set_min_width(84.0);
                    ui.add(egui::Label::new(text).selectable(false).sense(Sense::click()))
                });
                let name = name.inner;
                ui.spacing_mut().slider_width = (ui.available_width() - 54.0).max(40.0);
                let slider = ui.add(egui::Slider::new(v, -100.0..=100.0).step_by(1.0).max_decimals(0).trailing_fill(false));
                held |= slider.dragged() || slider.is_pointer_button_down_on();
                // Back to 0: a double click on the slider or on its name.
                if (double && slider.hovered() && !slider.dragged()) || name.on_hover_text("Double-click to reset").double_clicked() {
                    *v = 0.0;
                }
            });
        }
    });
    let mut now = Adjust::from_values(values);
    ui.horizontal(|ui| {
        let half = (ui.available_width() - 8.0) / 2.0;
        if ui.add_sized([half, 26.0], egui::Button::new("B&W")).on_hover_text("Black and white: saturation to -100").clicked() {
            now.saturation = -100.0;
        }
        if ui.add_enabled_ui(!now.is_identity(), |ui| ui.add_sized([half, 26.0], egui::Button::new("Reset"))).inner.on_hover_text("All adjustments back to 0. The image file itself is never changed.").clicked() {
            now = Adjust::default();
        }
    });
    if now != was {
        *adjust = now.normalized();
    }
    held
}

/// Can `n` have an inner shadow (does it have an inside)?
fn has_inside(n: &Node) -> bool {
    match n.kind {
        Kind::Line => false,
        Kind::Path => n.closed,
        _ => true,
    }
}

/// Can `n` have a background blur (see `Node::backdrop`)?
fn glassy(n: &Node) -> bool {
    has_inside(n) && n.kind != Kind::Text
}

fn export_icon(p: &egui::Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
    // Panah ke atas keluar dari baki.
    p.line_segment([at(0.0, 3.0), at(0.0, -8.0)], s);
    p.line_segment([at(-4.0, -4.0), at(0.0, -8.0)], s);
    p.line_segment([at(4.0, -4.0), at(0.0, -8.0)], s);
    p.line_segment([at(-7.0, 2.0), at(-7.0, 7.5)], s);
    p.line_segment([at(-7.0, 7.5), at(7.0, 7.5)], s);
    p.line_segment([at(7.0, 7.5), at(7.0, 2.0)], s);
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        // Batasi atlas font 2048px (maks ±16 MB) supaya RAM tetap kecil; egui mengosongkannya sendiri saat penuh.
        raw_input.max_texture_side = Some(raw_input.max_texture_side.map_or(2048, |m| m.min(2048)));
        // Mode --bench: suntikkan event mouse buatan untuk menguji interaksi.
        if let Some(b) = &mut self.bench {
            let frame = b.frame;
            b.inject.retain(|(at, event)| {
                if *at <= frame {
                    // (A key comes with the modifiers held, as from a real keyboard.)
                    if let egui::Event::Key { pressed, modifiers, .. } = event {
                        raw_input.modifiers = if *pressed { *modifiers } else { Default::default() };
                    }
                    raw_input.events.push(event.clone());
                }
                *at > frame
            });
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        let _update = perf::span(0);
        perf::idle_frame();
        // (Lookups by id are quick for as long as the document stays as it is now.)
        self.doc.reindex();
        {
            let _poll = perf::span(1);
            self.fonts.poll(ctx);
            self.ext.poll(ctx);
        }
        if let Some(notice) = self.ext.notice.take() {
            self.status = notice;
        }
        self.rembg_poll();
        self.export_poll();
        self.icons.poll(ctx);
        self.photos_poll(ctx);
        self.library.poll(ctx);
        self.save_poll();
        self.load_poll();
        // Remote commands wait while a file is being opened: they are meant for that document.
        if !self.loading() {
            self.remote_poll(ctx);
        }
        // `cpu_usage` adalah frame sebelumnya, jadi kurangi dengan waktu tunggu frame sebelumnya.
        self.frame_ms = (frame.info().cpu_usage.unwrap_or(0.0) * 1000.0 - self.pace_ms).max(0.0);
        self.pace_ms = pacer::wait();
        self.close_flow(ctx);
        if self.loading_screen(ctx) {
            return;
        }
        if self.home {
            self.home_screen(ctx);
            if self.home {
                if self.title != "Kanvas" {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Title("Kanvas".into()));
                    self.title = "Kanvas".into();
                }
                return;
            }
        }
        self.clipboard_keys(ctx);
        self.dropped_files(ctx);
        self.shortcuts(ctx);

        if self.show_ui {
            use theme::Edge;
            egui::TopBottomPanel::top("bar")
                .frame(theme::gutter(Edge::Top))
                .show_separator_line(false)
                .show(ctx, |ui| theme::bar(ui, |ui| self.top_bar(ui)));
            egui::TopBottomPanel::bottom("status")
                .frame(theme::gutter(Edge::Bottom))
                .show_separator_line(false)
                .show(ctx, |ui| theme::bar(ui, |ui| self.status_bar(ui)));
            egui::SidePanel::left("layers")
                .frame(theme::gutter(Edge::Left))
                .show_separator_line(false)
                .default_width(288.0)
                .width_range(220.0..=460.0)
                .show(ctx, |ui| theme::card(ui, |ui| self.layers(ui)));
            egui::SidePanel::right("props")
                .frame(theme::gutter(Edge::Right))
                .show_separator_line(false)
                .default_width(312.0)
                .width_range(300.0..=460.0)
                .show(ctx, |ui| theme::card(ui, |ui| self.props(ui)));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(CANVAS_BG))
            .show(ctx, |ui| {
                let _canvas = perf::span(2);
                self.canvas(ui)
            });
        self.toolbar(ctx);
        self.crop_bar(ctx);
        self.icons_window(ctx);
        self.photos_window(ctx);
        self.library_window(ctx);
        self.components_window(ctx);
        self.find_window(ctx);
        self.eyedropper(ctx);
        self.about_window(ctx);

        let name = self.path.as_ref().and_then(|p| p.file_name()).map_or("Untitled".into(), |n| n.to_string_lossy());
        let title = format!("{}{} — Kanvas", name, if self.dirty { " *" } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        // After everything that edits the document, before it is saved.
        self.settle_doc();
        self.autosave(ctx);
        self.run_bench(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolbar_fits_the_canvas_at_every_width() {
        let all = BarItem::all();
        for avail in (40..2000).step_by(7) {
            let fit = bar_fit(avail as f32);
            // Nothing is lost: every button is on the bar or in the popup, once.
            let mut both: Vec<BarItem> = fit.shown.iter().chain(&fit.more).copied().collect();
            both.sort_by_key(|item| all.iter().position(|a| a == item));
            assert_eq!(both, all, "at {avail}");
            // It fits, down to the width of the More button alone.
            if avail >= 46 {
                assert!(fit.width() <= avail as f32, "{} wide in {avail}", fit.width());
            }
        }
        // The default window: no popup. A narrow canvas: Move stays, then Export.
        assert!(bar_fit(900.0).more.is_empty() && bar_fit(900.0).size == 38.0);
        assert!(bar_fit(736.0).more.is_empty() && bar_fit(736.0).size == 32.0);
        assert_eq!(bar_fit(90.0).shown, [BarItem::Tool(0)]);
        assert_eq!(bar_fit(130.0).shown, [BarItem::Tool(0), BarItem::Export]);
    }
}
