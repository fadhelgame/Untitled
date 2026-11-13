//! Panel properti (kanan), toolbar bawah, export, dan loop `update`.

use super::picker::paint_row;
use super::*;

const FORMATS: [&str; 4] = ["PNG", "JPG", "SVG", "PDF"];

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
        ui.add_space(6.0);
        if self.sel.len() > 1 {
            self.multi_props(ui, &ctx);
            return;
        }
        if self.sel.len() != 1 {
            ui.label(theme::title("Design"));
            if self.sel.is_empty() {
                ui.weak("Nothing selected.");
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
                    "Drag outside a corner = rotate",
                    "Shift while resizing = keep ratio",
                    "Alt while resizing = from center",
                    "Double-click a vector = edit points",
                    "Moving snaps to edges/centers (Ctrl = off)",
                    "",
                    "Space+drag     pan canvas",
                    "Ctrl+scroll    zoom",
                    "Shift+0/1/2    100% / fit / selection",
                    "Alt+drag       duplicate",
                    "Shift+drag     lock axis",
                    "] and [        bring front / send back",
                    "Alt+A D W S    align left/right/top/bottom",
                    "Alt+H V        align centers",
                    "1 … 9, 0       opacity 10% … 100%",
                    "Shift+X        swap fill/stroke",
                    "Ctrl+Shift+H   show/hide",
                    "Ctrl+Alt+G     clipping mask",
                    "Enter          edit text",
                    "Ctrl+\\         hide panels",
                ] {
                    ui.label(egui::RichText::new(line).monospace().size(11.0).weak());
                }
            }
            self.export_section(ui, &ctx);
            return;
        }
        let id = self.sel[0];
        let Some(orig) = self.doc.find(id).cloned() else { return };

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
        let mut align: Option<u8> = None;
        let mut remove_bg = false;
        let has_parent = self.doc.parent_of(id).is_some();

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(theme::title(n.kind.label()));
            ui.add(egui::TextEdit::singleline(&mut n.name).desired_width(f32::INFINITY));

            section(ui, "Position");
            sub(ui, "Alignment");
            ui.add_enabled_ui(has_parent, |ui| {
                if let Some(mode) = align_buttons(ui) {
                    align = Some(mode);
                }
            });
            sub(ui, "Position");
            let half = (ui.available_width() - 8.0) / 2.0;
            ui.horizontal(|ui| {
                field(ui, half, "X", &mut n.x);
                field(ui, half, "Y", &mut n.y);
            });
            if !matches!(n.kind, Kind::Frame | Kind::Line) {
                sub(ui, "Rotation");
                ui.add_sized(
                    [half, 28.0],
                    egui::DragValue::new(&mut n.rotation).speed(1.0).range(-360.0..=360.0).max_decimals(1).suffix("°"),
                )
                .on_hover_text("Or drag just outside a corner of the selection (Shift = 15° steps)");
            }

            if n.kind != Kind::Text || n.wrap {
                section(ui, "Layout");
                sub(ui, "Dimensions");
                ui.horizontal(|ui| {
                    field(ui, half, "W", &mut n.w);
                    ui.add_enabled_ui(n.kind != Kind::Text, |ui| field(ui, half, "H", &mut n.h));
                });
                if n.kind != Kind::Line {
                    n.w = n.w.max(1.0);
                    n.h = n.h.max(1.0);
                }
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
                if matches!(n.kind, Kind::Rect | Kind::Image) {
                    ui.vertical(|ui| {
                        sub(ui, "Corner radius");
                        ui.add_sized([half, 28.0], egui::DragValue::new(&mut n.radius).speed(0.5).range(0.0..=2000.0).max_decimals(1));
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

            if n.kind == Kind::Text {
                self.typography(ui, &ctx, &mut n, editing, range, None);
            }

            if !matches!(n.kind, Kind::Line | Kind::Image) {
                section(ui, if range.is_some() { "Fill (selected text)" } else { "Fill" });
                // Teks hanya mendukung warna solid.
                let gradient = if n.kind == Kind::Text { None } else { Some(&mut n.gradient) };
                paint_row(ui, &mut self.ext.picker, (id, 0), &mut n.fill, gradient, [217, 217, 217, 255]);
            }
            if n.kind == Kind::Image {
                // Color overlay: mewarnai ulang gambar, transparansinya tetap.
                section(ui, "Fill");
                paint_row(ui, &mut self.ext.picker, (id, 0), &mut n.fill, None, [255, 255, 255, 255]);
                if n.fill.is_some() {
                    ui.weak("Recolors the image; transparency is kept.");
                }
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
                if n.stroke.is_some() {
                    ui.add_sized(
                        [half, 28.0],
                        egui::DragValue::new(&mut n.stroke_w).speed(0.25).range(0.0..=500.0).max_decimals(2).prefix("Weight  "),
                    );
                }
            }
            section(ui, "Drop shadow");
            let mut shadow_color = n.shadow.map(|s| s.color);
            paint_row(ui, &mut self.ext.picker, (id, 2), &mut shadow_color, None, crate::doc::Shadow::default().color);
            n.shadow = match (shadow_color, n.shadow) {
                (Some(color), Some(s)) => Some(crate::doc::Shadow { color, ..s }),
                (Some(color), None) => Some(crate::doc::Shadow { color, ..Default::default() }),
                (None, _) => None,
            };
            if let Some(s) = &mut n.shadow {
                ui.horizontal(|ui| {
                    let third = (ui.available_width() - 16.0) / 3.0;
                    ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.x).speed(0.25).max_decimals(1).prefix("X  "));
                    ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.y).speed(0.25).max_decimals(1).prefix("Y  "));
                    ui.add_sized([third, 28.0], egui::DragValue::new(&mut s.blur).speed(0.25).range(0.0..=500.0).max_decimals(1).prefix("Blur  "));
                });
            }
            self.export_section(ui, &ctx);
            ui.add_space(12.0);
        });

        if let Some(mode) = align {
            self.align(mode);
            return;
        }
        if remove_bg {
            self.rembg_start(id, &ctx);
            return;
        }
        if n == before {
            return;
        }
        // Perubahan beruntun pada objek yang sama digabung jadi satu langkah undo.
        let now = ctx.input(|i| i.time);
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
            });
            if changed {
                self.ext.edit_refocus = true;
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
        if let Some(slot) = self.doc.find_mut(id) {
            *slot = n;
        }
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
        let rounded = all(&|n| matches!(n.kind, Kind::Rect | Kind::Image));
        let same_kind = all(&|n| n.kind == first.kind);
        let mixed_type = !all(&|n| {
            (&n.font, n.weight, n.italic, n.font_size, n.line_height, n.letter_spacing, n.align)
                == (&first.font, first.weight, first.italic, first.font_size, first.line_height, first.letter_spacing, first.align)
        });
        let mixed_fill = !all(&|n| n.fill == first.fill && n.gradient == first.gradient);
        let mixed_stroke = !all(&|n| n.stroke == first.stroke && n.stroke_w == first.stroke_w);
        let mixed_look = !all(&|n| n.opacity == first.opacity && n.blend == first.blend && n.radius == first.radius);
        let title = if same_kind { format!("{} × {}", first.kind.label(), nodes.len()) } else { "Design".to_owned() };
        let mut n = (*first).clone();
        n.children.clear();
        drop(nodes);
        if any_text {
            // Teks hanya mendukung warna solid.
            n.gradient = None;
        }
        let before = n.clone();
        let key = ids[0];
        let group = self.selected_whole_group();
        let mut align: Option<u8> = None;
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

            section(ui, "Appearance");
            if mixed_look {
                ui.weak(MIXED);
            }
            let half = (ui.available_width() - 8.0) / 2.0;
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
            self.export_section(ui, ctx);
            ui.add_space(12.0);
        });

        if let Some(mode) = align {
            self.align(mode);
            return;
        }
        if n == before {
            return;
        }
        // Perubahan beruntun pada seleksi yang sama digabung jadi satu langkah undo.
        let now = ctx.input(|i| i.time);
        let undo_key = key | 1 << 63;
        if self.last_prop.0 != undo_key || now - self.last_prop.1 > 0.8 {
            let pre = self.doc.clone();
            self.push_undo(pre);
        }
        self.last_prop = (undo_key, now);
        self.touch();

        let paint = n.fill != before.fill || n.gradient != before.gradient;
        for id in ids {
            let Some(t) = self.doc.find_mut(id) else { continue };
            if n.opacity != before.opacity {
                t.opacity = n.opacity;
            }
            if n.blend != before.blend {
                t.blend = n.blend;
            }
            if n.radius != before.radius && matches!(t.kind, Kind::Rect | Kind::Image) {
                t.radius = n.radius;
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
            for (mode, tip) in [(0u8, "Align left"), (1, "Align center"), (2, "Align right")] {
                if icon_button(ui, n.align == mode, tip, |p, c, color| text_align_icon(p, c, mode, color)) {
                    n.align = mode;
                }
            }
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
        let target = match self.sel.as_slice() {
            [id] => self.doc.find(*id).map(|n| n.name.clone()).unwrap_or_default(),
            _ if self.ext.export_fmt == 3 => match self.pdf_targets().len() {
                1 => "1 page".to_owned(),
                n => format!("{n} pages"),
            },
            _ => "whole document".to_owned(),
        };
        let label = format!("Export {}", target.chars().take(22).collect::<String>());
        if ui.add_sized([ui.available_width().max(150.0), 28.0], egui::Button::new(label)).clicked() {
            self.export_now(ctx);
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
        let targets = self.pdf_targets();
        let pages: Vec<(String, f32, f32)> = targets.iter().filter_map(|t| export::to_svg(&self.doc, *t, &mut self.fonts, ctx)).collect();
        if pages.is_empty() {
            self.status = "Nothing to export.".into();
            return;
        }
        let name = match (targets.as_slice(), &self.path) {
            ([Some(id)], _) => self.doc.find(*id).map(|n| n.name.clone()).unwrap_or_default(),
            (_, Some(path)) => path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            _ => "kanvas".into(),
        };
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
        let Some(path) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(format!("{safe}.pdf")).save_file() else {
            return;
        };
        self.status = match export::save_pdf(&pages, &path) {
            Ok(()) => format!("Exported {} page(s): {}", pages.len(), path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    fn export_now(&mut self, ctx: &egui::Context) {
        if self.ext.export_fmt == 3 {
            return self.export_pdf(ctx);
        }
        let only = if self.sel.len() == 1 { Some(self.sel[0]) } else { None };
        let Some((svg, w, h)) = export::to_svg(&self.doc, only, &mut self.fonts, ctx) else {
            self.status = "Nothing to export.".into();
            return;
        };
        let fmt = self.ext.export_fmt.min(3);
        let scale = self.ext.export_scale.clamp(0.1, 16.0);
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
        let result = match fmt {
            2 => std::fs::write(&path, svg).map_err(|e| e.to_string()),
            _ => export::save_raster(&svg, w, h, scale, fmt == 1, &path),
        };
        self.status = match result {
            Ok(()) => format!("Exported: {}", path.display()),
            Err(e) => format!("Export failed: {e}"),
        };
    }

    // ------------------------------------------------------------ toolbar

    /// Toolbar mengambang di bawah canvas.
    pub(super) fn toolbar(&mut self, ctx: &egui::Context) {
        let c = self.canvas_rect;
        egui::Area::new(egui::Id::new("kanvas_toolbar"))
            .order(egui::Order::Middle)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(c.center().x, c.bottom() - 18.0))
            .show(ctx, |ui| {
                egui::Frame::new()
                    .fill(theme::CARD)
                    .corner_radius(CornerRadius::same(18))
                    .stroke(Stroke::new(1.0_f32, Color32::from_gray(66)))
                    .inner_margin(egui::Margin::same(6))
                    .shadow(egui::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(110) })
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 4.0;
                            let separator = |ui: &mut egui::Ui| {
                                let (sep, _) = ui.allocate_exact_size(vec2(9.0, 38.0), Sense::hover());
                                ui.painter().vline(sep.center().x, sep.y_range().shrink(8.0), Stroke::new(1.0_f32, Color32::from_gray(74)));
                            };
                            let button = |ui: &mut egui::Ui, active: bool| {
                                let (rect, resp) = ui.allocate_exact_size(vec2(38.0, 38.0), Sense::click());
                                if active {
                                    ui.painter().rect_filled(rect, CornerRadius::same(12), ACCENT);
                                } else if resp.hovered() {
                                    ui.painter().rect_filled(rect, CornerRadius::same(12), Color32::from_gray(64));
                                }
                                (rect, resp)
                            };
                            for (i, (tool, name, key)) in TOOLS.iter().enumerate() {
                                if matches!(i, 4 | 5 | 10) {
                                    separator(ui);
                                }
                                let active = self.tool == *tool;
                                let (rect, resp) = button(ui, active);
                                let color = if active { Color32::WHITE } else { Color32::from_gray(225) };
                                draw_icon(ui.painter(), *tool, rect.center(), color);
                                if resp.on_hover_text(format!("{name}   {key}")).clicked() {
                                    self.tool = *tool;
                                }
                            }
                            // Eyedropper: bukan tool canvas, melainkan mode sekali pakai.
                            let active = self.ext.picker.eyedrop.is_some();
                            let (rect, resp) = button(ui, active);
                            let color = if active { Color32::WHITE } else { Color32::from_gray(225) };
                            super::picker::eyedropper_icon(ui.painter(), rect.center(), Stroke::new(1.6_f32, color));
                            if resp.on_hover_text("Eyedropper   I").clicked() {
                                self.toggle_eyedropper();
                            }
                            // Icons: buka/tutup jendela pencarian icon.
                            separator(ui);
                            let active = self.icons.open;
                            let (rect, resp) = button(ui, active);
                            super::icons::toolbar_icon(ui.painter(), rect.center(), if active { Color32::WHITE } else { Color32::from_gray(225) });
                            if resp.on_hover_text("Icons").clicked() {
                                self.toggle_icons();
                            }
                            // Photos: buka/tutup jendela pencarian foto stok.
                            let active = self.photos.open;
                            let (rect, resp) = button(ui, active);
                            super::photos::toolbar_icon(ui.painter(), rect.center(), if active { Color32::WHITE } else { Color32::from_gray(225) });
                            if resp.on_hover_text("Photos").clicked() {
                                self.toggle_photos();
                            }
                            // Export: popup di atas toolbar dengan skala dan format.
                            separator(ui);
                            let popup_id = ui.make_persistent_id("export_popup");
                            let open = ui.memory(|m| m.is_popup_open(popup_id));
                            let (rect, resp) = button(ui, open);
                            export_icon(ui.painter(), rect.center(), if open { Color32::WHITE } else { Color32::from_gray(225) });
                            let resp = resp.on_hover_text("Export");
                            if resp.clicked() {
                                ui.memory_mut(|m| m.toggle_popup(popup_id));
                            }
                            egui::popup::popup_above_or_below_widget(
                                ui,
                                popup_id,
                                &resp,
                                egui::AboveOrBelow::Above,
                                egui::PopupCloseBehavior::CloseOnClickOutside,
                                |ui| {
                                    ui.set_min_width(190.0);
                                    ui.label(egui::RichText::new("Export").strong());
                                    self.export_controls(ui, ctx);
                                },
                            );
                        });
                    });
            });
    }
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
                    raw_input.events.push(event.clone());
                }
                *at > frame
            });
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.fonts.poll(ctx);
        self.ext.poll(ctx);
        self.rembg_poll();
        self.icons.poll(ctx);
        self.photos_poll(ctx);
        self.remote_poll(ctx);
        self.frame_ms = frame.info().cpu_usage.unwrap_or(0.0) * 1000.0;
        self.close_flow(ctx);
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
            .show(ctx, |ui| self.canvas(ui));
        self.toolbar(ctx);
        self.icons_window(ctx);
        self.photos_window(ctx);
        self.eyedropper(ctx);

        let name = self.path.as_ref().and_then(|p| p.file_name()).map_or("Untitled".into(), |n| n.to_string_lossy());
        let title = format!("{}{} — Kanvas", name, if self.dirty { " *" } else { "" });
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }
        self.autosave(ctx);
        self.run_bench(ctx);
    }
}
