//! Canvas: gambar dokumen, overlay seleksi, editor teks, dan semua interaksi pointer
//! (pilih, geser, resize, scale, buat bentuk, teks paragraf, pen, gambar).

use super::render::{self, paint_list, PaintCx};
use super::*;
use crate::doc::PathPoint;
use crate::layout::{Children, Sizing};

impl App {
    pub(super) fn canvas(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), Sense::click_and_drag());
        self.canvas_rect = rect;
        if self.fit_pending {
            self.fit_pending = false;
            self.fit_all();
        }
        let o = rect.min;
        let painter = ui.painter_at(rect);

        let inp = ctx.input(|i| Inp {
            pos: i.pointer.latest_pos(),
            pressed: i.pointer.primary_pressed(),
            down: i.pointer.primary_down(),
            mid_pressed: i.pointer.button_pressed(PointerButton::Middle),
            mid_down: i.pointer.button_down(PointerButton::Middle),
            delta: i.pointer.delta(),
            mods: i.modifiers,
            space: i.key_down(Key::Space),
            zoom: i.zoom_delta(),
            scroll: i.smooth_scroll_delta,
        });
        // Saat editor teks (TextEdit tersembunyi) memegang fokus, egui tidak selalu melaporkan
        // canvas sebagai "hovered" pada frame tombol ditekan, sehingga klik di dalam teks hilang.
        // Dalam mode edit, cukup pastikan pointer ada di canvas dan tidak tertutup popup/toolbar.
        let hovered = resp.hovered()
            || (self.editing.is_some()
                && inp.pos.is_some_and(|p| rect.contains(p) && ctx.layer_id_at(p) == Some(resp.layer_id)));
        let space = inp.space && !ctx.wants_keyboard_input();

        // Ganti tool/seleksi di tengah jalan: selesaikan pekerjaan yang menggantung.
        if self.tool != Tool::Pen && self.ext.pen.is_some() {
            self.finish_pen(false);
        }
        if self.tool != Tool::Pen {
            self.ext.pen_cont = None;
            self.ext.pen_undo.clear();
            self.ext.pen_redo.clear();
        }
        if self.tool != Tool::ShapeBuilder {
            self.ext.builder = None;
        }
        if self.ext.path_edit.as_ref().is_some_and(|e| !self.pointer_tool() || self.sel != [e.id]) {
            self.ext.path_edit = None;
        }
        if let Some(id) = self.editing {
            if self.sel != [id] {
                self.finish_text_edit();
            }
        }
        // Mode crop berakhir (diterapkan) begitu seleksi atau tool berganti.
        if self.crop_id().is_some_and(|id| self.tool != Tool::Select || self.sel != [id] || self.doc.find(id).is_none()) {
            self.finish_crop(true);
        }

        if hovered {
            if let Some(p) = inp.pos {
                if inp.zoom != 1.0 {
                    self.zoom_at(p, inp.zoom);
                }
            }
            self.view.pan += inp.scroll;
        }
        if let Some(p) = inp.pos {
            // Penggaris dan guide didahulukan; sisanya interaksi canvas biasa.
            if !self.ruler_pointer(p, &inp, hovered) {
                self.pointer(p, &inp, hovered, space);
            }
            // What is drawn below is already laid out around what was just dragged.
            self.settle_live();
            if resp.double_clicked() {
                if self.tool == Tool::Pen {
                    // egui menghitung klik ganda dari waktu saja; dua titik yang diklik cepat di tempat
                    // berbeda bukan tanda selesai. Selesai hanya kalau kedua klik di tempat yang sama.
                    let [a, b] = self.ext.pen_clicks;
                    if (a - b).length() <= 7.0 {
                        self.finish_pen(false);
                    }
                } else if self.ext.path_edit.is_some() {
                    self.path_edit_double(p);
                } else if self.crop.is_some() {
                    // Klik dua kali lagi: selesai meng-crop.
                    self.finish_crop(true);
                } else if self.pointer_tool() && self.editing.is_none() {
                    let (wp, tol) = (self.view.to_world(o, p), 4.0 / self.view.zoom);
                    // The object a click here selects (the first click of the two already did).
                    if let Some(id) = self.hit(p, wp, tol) {
                        match self.doc.find(id).map(|n| (n.kind, n.group != 0)) {
                            // Double-click on a frame: go one level in, to its child under the pointer.
                            Some((Kind::Frame, grouped)) => match self.doc.hit_within(id, wp.x, wp.y, tol) {
                                Some(child) => self.sel = self.with_group(child),
                                None if grouped => self.sel = vec![id],
                                None => {}
                            },
                            Some((Kind::Text, _)) => {
                                let pre = self.doc.clone();
                                self.start_text_edit(id, pre);
                                // Seperti Figma: klik dua kali masuk mode edit dengan caret di posisi klik.
                                // Klik dua kali lagi di dalam teks baru menyeleksi kata.
                                self.ext.edit_pending = Some(render::EditPending::CaretAt(p));
                                // Klik cepat berikutnya di tempat yang sama dihitung sebagai klik ketiga.
                                self.ext.edit_click = (p, ctx.input(|i| i.time), 2);
                            }
                            Some((Kind::Path, _)) => {
                                self.sel = vec![id];
                                if !self.refuse_pinned() {
                                    self.start_path_edit(id);
                                }
                            }
                            // Klik dua kali pada gambar: mode crop (anggota group dipilih sendiri dulu).
                            Some((Kind::Image, grouped)) if !grouped || self.sel == [id] => self.start_crop(id),
                            // Klik dua kali pada anggota group: pilih objek itu saja.
                            Some((_, true)) => self.sel = vec![id],
                            _ => {}
                        }
                    }
                }
            }
        }

        // --- gambar dokumen (dengan culling) ---
        let z = self.view.zoom;
        let now = ctx.input(|i| i.time);
        if z != self.last_zoom.0 {
            self.last_zoom = (z, now);
        }
        let moving = now - self.last_zoom.1 < 0.15;
        if moving {
            // Setelah zoom berhenti, gambar ulang sekali dengan ukuran glyph persis (tajam).
            ctx.request_repaint_after(std::time::Duration::from_millis(170));
        }
        let (tl, br) = (self.view.to_world(o, rect.min), self.view.to_world(o, rect.max));
        self.fonts.layouts.begin(&ctx);
        // (What the pointer did above may have changed the document.)
        self.doc.reindex();
        let origin = self.view.to_screen(o, 0.0, 0.0);
        let still = origin == self.ext.last_origin;
        self.ext.last_origin = origin;
        let mut pc = PaintCx {
            ctx: &ctx,
            fonts: &mut self.fonts,
            ext: &mut self.ext,
            assets: &self.doc.assets,
            view: self.view,
            o,
            canvas: rect,
            vis: (tl.x, tl.y, br.x, br.y),
            editing: self.editing,
            moving,
            measures: Vec::new(),
            boxes: Vec::new(),
            doc: &self.doc,
            rec_top: None,
            drawn: 0,
            capture: None,
            skip_shadow: false,
            tops: &self.doc.nodes,
            still,
            depth: 0,
            recording: false,
            no_keep: false,
            keep_later: false,
            specials: Vec::new(),
        };
        self.labels.clear();
        let (labels, sel, view) = (&mut self.labels, &self.sel, self.view);
        paint_list(&mut pc, &painter, &self.doc.nodes, 1.0, &mut |n| {
            if n.kind == Kind::Frame && n.visible {
                let r = Rect::from_min_size(view.to_screen(o, n.x, n.y), vec2(n.w * z, n.h * z));
                if r.width() > 36.0 {
                    let lr = Rect::from_min_max(pos2(r.left(), r.top() - 18.0), pos2(r.right(), r.top()));
                    if lr.intersects(rect) {
                        // Mains and instances carry their icon and colour in the name.
                        let linked = n.is_linked();
                        let color = match (linked, sel.contains(&n.id)) {
                            (true, _) => components::PURPLE,
                            (false, true) => ACCENT,
                            (false, false) => Color32::from_gray(160),
                        };
                        if linked {
                            components::component_icon(&painter.with_clip_rect(lr.intersect(rect)), pos2(r.left() + 6.0, r.top() - 10.0), n.component, color);
                        }
                        // A name that fits above its frame needs no clip rectangle of its own, so
                        // egui can draw all such names in one go instead of one draw call each.
                        let galley = painter.layout_no_wrap(n.name.clone(), FontId::proportional(11.0), color);
                        let indent = if linked { 16.0 } else { 0.0 };
                        let at = pos2(r.left() + indent, r.top() - 4.0 - galley.size().y);
                        if galley.size().x + indent <= lr.width() && rect.contains_rect(lr) {
                            painter.galley(at, galley, color);
                        } else {
                            painter.with_clip_rect(lr.intersect(rect)).galley(at, galley, color);
                        }
                        labels.push((n.id, lr));
                    }
                }
            }
        });
        // Mode crop: gambar utuh tampil redup di balik kotak crop.
        if let Some(n) = self.crop.as_ref().and_then(|s| self.doc.find(s.id)) {
            render::paint_crop_ghost(&mut pc, &painter, n);
        }
        self.drawn = pc.drawn;
        let boxes = std::mem::take(&mut pc.boxes);
        let measures = std::mem::take(&mut pc.measures);
        for b in boxes {
            self.apply_path_box(b);
        }
        for (id, w, h) in measures {
            // (A height that was set by hand stays.)
            let differs = self.doc.find(id).is_some_and(|n| (n.w - w).abs() > 0.5 || (!n.fixed_h && (n.h - h).abs() > 0.5));
            if differs {
                if let Some(n) = self.doc.find_mut(id) {
                    n.w = w;
                    if !n.fixed_h {
                        n.h = h;
                    }
                }
                // Not an edit, but the instances of a main with this text get its size too.
                self.components.recheck();
            }
        }

        if self.doc.nodes.is_empty() && self.ext.pen.is_none() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                "Empty canvas.\nPress F and drag to create a frame, drop an image here, or try View → Stress test.",
                FontId::proportional(14.0),
                Color32::from_gray(120),
            );
        }

        // Under the selection outlines: what Find and replace found.
        self.paint_find(&ctx, &painter);

        // --- overlay: hover, seleksi, pegangan, marquee ---
        let idle = matches!(self.drag, Drag::None);
        let mut under = None;
        if idle && hovered && self.pointer_tool() && self.editing.is_none() && self.crop.is_none() {
            if let Some(p) = inp.pos {
                let wp = self.view.to_world(o, p);
                under = self.hit(p, wp, 4.0 / z).filter(|id| !self.sel.contains(id));
            }
        }
        if under.is_some() || !self.sel.is_empty() {
            // The outlines are kept as one mesh for as long as the document, the selection and
            // the zoom stay as they are: with thousands of layers selected, or a few outlined
            // texts, making them anew on every frame was most of the frame.
            // (Painting may have changed the document too: texts are measured there.)
            let stamp = self.doc.reindex();
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            (stamp, self.doc.page, under, &self.sel, z.to_bits(), ctx.pixels_per_point().to_bits()).hash(&mut h);
            let (doc, sel, view) = (&self.doc, &self.sel, self.view);
            let mesh = render::overlay_mesh(&mut self.ext, &ctx, h.finish(), view.to_screen(o, 0.0, 0.0), || {
                let mut shapes = Vec::new();
                let mut budget = OUTLINE_CONTOURS;
                if let Some(n) = under.and_then(|id| doc.find(id)) {
                    outline(&mut shapes, view, o, n, 1.0, &mut budget);
                }
                // (The selection has a budget of its own: what is under the pointer takes none of it.)
                budget = OUTLINE_CONTOURS;
                for n in sel.iter().filter_map(|id| doc.find(*id)) {
                    outline(&mut shapes, view, o, n, 1.5, &mut budget);
                }
                shapes
            });
            painter.add(egui::Shape::mesh(mesh));
        }
        if self.sel.len() == 1 && self.editing.is_none() && self.ext.path_edit.is_none() {
            if let Some(n) = self.doc.find(self.sel[0]) {
                if self.crop.is_some() {
                    self.paint_crop(&painter);
                } else if self.pointer_tool() && n.source.is_none() {
                    for (_, x, y) in handles(n, self.tool == Tool::Scale) {
                        let hr = Rect::from_center_size(self.view.to_screen(o, x, y), vec2(7.0, 7.0));
                        painter.rect_filled(hr, CornerRadius::same(1), Color32::WHITE);
                        painter.rect_stroke(hr, CornerRadius::same(1), Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
                    }
                }
                // Label ukuran di bawah objek.
                let (x0, _, x1, y1) = n.aabb();
                let anchor = self.view.to_screen(o, (x0 + x1) * 0.5, y1) + vec2(0.0, 8.0);
                let label = format!("{:.0} × {:.0}", n.w.abs(), n.h.abs());
                let galley = painter.layout_no_wrap(label, FontId::proportional(11.0), Color32::WHITE);
                let bg = Rect::from_min_size(pos2(anchor.x - galley.size().x * 0.5 - 5.0, anchor.y), galley.size() + vec2(10.0, 4.0));
                painter.rect_filled(bg, CornerRadius::same(3), ACCENT);
                painter.galley(bg.min + vec2(5.0, 2.0), galley, Color32::WHITE);
            }
        }
        // Beberapa objek terpilih: satu kotak seleksi dengan pegangan untuk resize bersama.
        if let Some((x0, y0, x1, y1)) = self.sel_box() {
            let br = Rect::from_min_max(self.view.to_screen(o, x0, y0), self.view.to_screen(o, x1, y1));
            painter.rect_stroke(br, CornerRadius::ZERO, Stroke::new(1.0_f32, ACCENT), StrokeKind::Middle);
            for (fx, fy) in HANDLE_F {
                let hr = Rect::from_center_size(br.min + vec2(br.width() * fx, br.height() * fy), vec2(7.0, 7.0));
                painter.rect_filled(hr, CornerRadius::same(1), Color32::WHITE);
                painter.rect_stroke(hr, CornerRadius::same(1), Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
            }
            let galley = painter.layout_no_wrap(format!("{:.0} × {:.0}", x1 - x0, y1 - y0), FontId::proportional(11.0), Color32::WHITE);
            let bg = Rect::from_min_size(pos2(br.center().x - galley.size().x * 0.5 - 5.0, br.bottom() + 8.0), galley.size() + vec2(10.0, 4.0));
            painter.rect_filled(bg, CornerRadius::same(3), ACCENT);
            painter.galley(bg.min + vec2(5.0, 2.0), galley, Color32::WHITE);
        }
        match (&self.drag, inp.pos) {
            (Drag::Marquee { start, .. }, Some(p)) => {
                let mr = Rect::from_two_pos(self.view.to_screen(o, start.x, start.y), p);
                painter.rect_filled(mr, CornerRadius::ZERO, ACCENT.gamma_multiply(0.12));
                painter.rect_stroke(mr, CornerRadius::ZERO, Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
            }
            (Drag::TextBox { start, cur }, _) => {
                // Kotak batas teks paragraf yang sedang ditarik.
                let tr = Rect::from_two_pos(self.view.to_screen(o, start.x, start.y), self.view.to_screen(o, cur.x, cur.y));
                if tr.width() > 4.0 {
                    painter.rect_stroke(tr, CornerRadius::ZERO, Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
                }
            }
            _ => {}
        }
        self.paint_ruler_guides(&painter, inp.pos);
        self.paint_guides(&painter);
        self.paint_drop_mark(&painter);
        self.paint_grad(&painter);
        if self.tool == Tool::Pen {
            self.paint_pen_anchors(&painter);
        }
        self.paint_pen(&painter, inp.pos, inp.mods.shift);
        self.paint_path_edit(&painter, inp.pos);
        self.paint_corners(&painter, inp.pos.filter(|_| hovered));
        if self.tool == Tool::ShapeBuilder {
            self.paint_builder(&painter, inp.pos.filter(|_| hovered && idle && !space), inp.mods.alt);
        }

        // --- kursor ---
        let pen_precise = self.tool == Tool::Pen && super::pen::caps_lock();
        if hovered || !idle {
            let icon = match (&self.drag, self.tool) {
                (Drag::Pan, _) => CursorIcon::Grabbing,
                (Drag::Rotate { .. } | Drag::MultiRotate { .. }, _) => CursorIcon::None,
                _ if space => CursorIcon::Grab,
                (_, Tool::Hand) => CursorIcon::Grab,
                (_, Tool::Text) => CursorIcon::Text,
                (_, Tool::Wand) => CursorIcon::PointingHand,
                // Pen: ikon pena digambar sendiri; Caps Lock menyala = kursor presisi (+).
                (_, Tool::Pen) if pen_precise => CursorIcon::Crosshair,
                (_, Tool::Pen) => CursorIcon::None,
                (_, Tool::ShapeBuilder) => CursorIcon::Default,
                (_, Tool::Select | Tool::Scale) => match inp
                    .pos
                    .and_then(|p| self.handle_at(p))
                    .map(|(id, h)| (self.tool == Tool::Scale || self.doc.find(id).is_some_and(|n| n.kind != Kind::Line), h))
                    .or_else(|| inp.pos.and_then(|p| self.multi_handle_at(p)).map(|h| (true, h)))
                {
                    Some((true, h)) => match h {
                        0 | 4 => CursorIcon::ResizeNwSe,
                        2 | 6 => CursorIcon::ResizeNeSw,
                        1 | 5 => CursorIcon::ResizeVertical,
                        _ => CursorIcon::ResizeHorizontal,
                    },
                    Some(_) => CursorIcon::Crosshair,
                    // Tepat di luar sudut: zona putar.
                    None if inp.pos.is_some_and(|p| self.rotate_zone(p).is_some() || self.multi_rotate_zone(p).is_some()) => CursorIcon::None,
                    None => CursorIcon::Default,
                },
                _ => CursorIcon::Crosshair,
            };
            let panning = space || matches!(self.drag, Drag::Pan);
            let icon = self.crop_cursor(inp.pos).filter(|_| !panning).unwrap_or(icon);
            ctx.set_cursor_icon(icon);
        }
        if self.tool == Tool::Pen && hovered && idle && !space && !pen_precise {
            if let Some(p) = inp.pos {
                let hover = self.pen_hover(p, inp.mods.alt);
                self.paint_pen_cursor(&painter, p, hover);
            }
        }
        // Di zona putar kursor sistem disembunyikan dan diganti ikon panah melengkung.
        self.paint_rotate_cursor(&painter, inp.pos, hovered);

        self.text_editor(&ctx, &painter, hovered);
        self.paint_rulers(&ctx, &painter, inp.pos, hovered);
    }

    /// Kotak di layar tempat klik masih dianggap "di dalam teks yang sedang diedit".
    fn edit_box(&self) -> Option<Rect> {
        let n = self.doc.find(self.editing?)?;
        let z = self.view.zoom;
        let min = self.view.to_screen(self.canvas_rect.min, n.x, n.y);
        // Huruf bisa menjulang keluar kotak baris (line height rapat), jadi diberi kelonggaran.
        // (And the lines of a text in a box of fixed height may run out of it altogether.)
        let spill = self.ext.spill(n).unwrap_or(0.0) * z;
        Some(Rect::from_min_size(min, vec2(n.w.max(2.0), n.h.max(2.0)) * z).expand(8.0 + n.max_size() * z * 0.25).expand2(vec2(0.0, spill)))
    }

    /// Editor teks di atas canvas.
    ///
    /// Teks, sorotan seleksi, dan caret digambar sendiri dengan layout yang sama persis dengan
    /// canvas (`layout_text`), dan klik/drag mouse ditangani di sini. `TextEdit` egui hanya
    /// dipakai secara tersembunyi untuk keyboard (mengetik, panah, Ctrl+A/C/V, IME).
    /// Mode edit tetap aktif saat fokus pindah ke panel, supaya gaya bisa diubah untuk huruf
    /// yang diseleksi saja; seleksinya tetap terlihat.
    fn text_editor(&mut self, ctx: &egui::Context, painter: &egui::Painter, hovered: bool) {
        use egui::text::{CCursor, CCursorRange};
        let Some(id) = self.editing else { return };
        let Some(style) = self.doc.find(id).cloned() else {
            self.editing = None;
            return;
        };
        let first = self.edit_frames == 0;
        self.edit_frames += 1;
        if first {
            self.ext.edit_sel = None;
            self.ext.edit_drag = false;
            // Riwayat undo `TextEdit` tersimpan per id widget, dan id itu dipakai semua teks. Tanpa
            // dikosongkan, Ctrl+Z di sini mengembalikan isi teks lain yang diedit sebelumnya.
            let widget_id = egui::Id::new("kanvas_text_edit_widget");
            if let Some(mut state) = egui::TextEdit::load_state(ctx, widget_id) {
                state.clear_undoer();
                state.store(ctx, widget_id);
            }
        }
        let o = self.canvas_rect.min;
        let z = self.view.zoom;
        let max = style.max_size();
        let (lay, k) = render::glyph_scale(max * z, false, ctx.pixels_per_point());
        let f = lay / max;
        let node_pos = self.view.to_screen(o, style.x, style.y);
        let old_text = style.text.clone();
        let len = old_text.chars().count();
        let widget_id = egui::Id::new("kanvas_text_edit_widget");
        let wrap = style.wrap;
        // Titik (0,0) galley di layar. Teks lebar-otomatis rata tengah/kanan punya baris di x negatif.
        // Paragraf rata tengah/kanan: x = 0 galley ada di tengah / tepi kanan kotaknya.
        // In a box of fixed height the lines start where the vertical alignment puts them.
        let mut frame = Node::new(0, Kind::Text, style.x, style.y);
        (frame.w, frame.h, frame.wrap, frame.align, frame.fixed_h, frame.valign) = (style.w, style.h, style.wrap, style.align, style.fixed_h, style.valign);
        let origin = |l: &render::TextLayout| node_pos + render::text_offset(&frame, l, f) * k;

        // --- mouse: klik menaruh caret, drag menyeleksi, klik dua kali = kata, tiga kali = semua ---
        let geo = render::layout_text(ctx, &style, &old_text, &mut self.fonts, f, 1.0, true);
        let geo_origin = origin(&geo);
        let char_at = |p: Pos2| geo.galley.cursor_from_pos((p - geo_origin) / k).ccursor.index.min(len);
        let (pos, pressed, down, shift, time) = ctx.input(|i| {
            (i.pointer.latest_pos(), i.pointer.primary_pressed(), i.pointer.primary_down(), i.modifiers.shift, i.time)
        });
        let over = hovered && !first && pos.zip(self.edit_box()).is_some_and(|(p, r)| r.contains(p));
        // (jangkar, ujung yang bergerak)
        let mut set_range: Option<(usize, usize)> = None;
        if let Some(p) = pos {
            if pressed && over {
                let at = char_at(p);
                // Klik beruntun dihitung sendiri: harus cepat DAN di tempat yang sama, supaya klik
                // di huruf lain tidak dianggap klik dua/tiga kali (yang menyeleksi kata/baris).
                let (last_pos, last_time, last_count) = self.ext.edit_click;
                let count = if time - last_time < 0.4 && last_pos.distance(p) < 6.0 { last_count % 3 + 1 } else { 1 };
                self.ext.edit_click = (p, time, count);
                match count {
                    // Klik dua kali = kata, tiga kali = satu baris/paragraf.
                    2 | 3 => {
                        let (a, b) = if count == 2 { word_at(&old_text, at) } else { line_at(&old_text, at) };
                        self.ext.edit_anchor = a;
                        set_range = Some((a, b));
                    }
                    _ if shift => {
                        set_range = Some((self.ext.edit_anchor.min(len), at));
                        self.ext.edit_drag = true;
                    }
                    _ => {
                        self.ext.edit_anchor = at;
                        set_range = Some((at, at));
                        self.ext.edit_drag = true;
                    }
                }
            } else if down && self.ext.edit_drag {
                set_range = Some((self.ext.edit_anchor.min(len), char_at(p)));
            }
        }
        if !down {
            self.ext.edit_drag = false;
        }
        if !first {
            match self.ext.edit_pending.take() {
                Some(render::EditPending::All) => set_range = Some((0, len)),
                Some(render::EditPending::CaretAt(p)) => {
                    // The click was on the mirrored text; it is edited unmirrored.
                    let c = self.view.to_screen(o, style.x + style.w * 0.5, style.y + style.h * 0.5);
                    let (fh, fv) = style.flips();
                    let p = pos2(if fh { 2.0 * c.x - p.x } else { p.x }, if fv { 2.0 * c.y - p.y } else { p.y });
                    let at = char_at(p);
                    self.ext.edit_anchor = at;
                    set_range = Some((at, at));
                }
                None => {}
            }
        }
        // Gaya baru saja diubah lewat panel: kembalikan fokus dan seleksi ke teks.
        let refocus = std::mem::take(&mut self.ext.edit_refocus);
        if refocus && set_range.is_none() {
            set_range = self.ext.edit_sel;
        }
        if let Some((anchor, moving)) = set_range {
            let mut state = egui::TextEdit::load_state(ctx, widget_id).unwrap_or_default();
            state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(anchor), CCursor::new(moving))));
            state.store(ctx, widget_id);
            self.ext.edit_sel = Some((anchor.min(moving), anchor.max(moving)));
        }
        let want_focus = first || refocus || set_range.is_some();

        // --- keyboard: TextEdit tersembunyi (tidak digambar, tidak menerima mouse) ---
        let mut text = old_text.clone();
        let (mut focused, mut lost_focus, mut caret) = (false, false, None);
        let mut selection = None;
        {
            let fonts = &mut self.fonts;
            let mut layouter =
                |ui: &egui::Ui, s: &str, _wrap: f32| render::layout_text(ui.ctx(), &style, s, fonts, f, 1.0, true).galley;
            egui::Area::new(egui::Id::new("kanvas_text_edit"))
                .order(egui::Order::Foreground)
                .fixed_pos(node_pos)
                .constrain(false)
                .interactable(false)
                .show(ctx, |ui| {
                    ui.set_clip_rect(Rect::NOTHING);
                    let out = egui::TextEdit::multiline(&mut text)
                        .id(widget_id)
                        .frame(false)
                        .margin(egui::Margin::ZERO)
                        .desired_rows(1)
                        .desired_width(200.0)
                        .layouter(&mut layouter)
                        .show(ui);
                    if want_focus {
                        out.response.request_focus();
                    }
                    focused = out.response.has_focus();
                    lost_focus = out.response.lost_focus();
                    if focused {
                        if let Some(range) = out.cursor_range {
                            selection = Some(range.as_sorted_char_range());
                            caret = Some(range.primary.ccursor.index);
                        }
                    }
                });
        }
        if let Some(r) = selection {
            self.ext.edit_sel = Some((r.start, r.end));
        }

        // --- terapkan perubahan teks, lalu layout akhir ---
        let mut cur = style;
        if text != old_text {
            cur.sync_spans(&old_text, &text);
            cur.name = text.lines().next().unwrap_or("Text").chars().take(40).collect();
            cur.text = text;
        }
        let fin = render::layout_text(ctx, &cur, &cur.text, &mut self.fonts, f, 1.0, true);
        let fin_origin = origin(&fin);
        // (For `edit_box`: how far the lines run out of a box of fixed height.)
        self.ext.note_spill(&cur, fin.height / f);
        let (w, h) = (if wrap { cur.w } else { (fin.width / f).max(2.0) }, (fin.height / f).max(1.0));
        if let Some(n) = self.doc.find_mut(id) {
            if n.text != cur.text {
                n.text = cur.text.clone();
                n.spans = cur.spans.clone();
                n.name = cur.name.clone();
            }
            // Kotak seleksi mengikuti ukuran teks selama mengetik.
            let h = if n.fixed_h { n.h } else { h };
            if (n.w - w).abs() > 0.5 || (n.h - h).abs() > 0.5 {
                n.w = w;
                n.h = h;
            }
        }

        // --- gambar: sorotan seleksi transparan, teks di atasnya, lalu caret ---
        let to_screen = |p: Pos2| fin_origin + p.to_vec2() * k;
        let total = cur.text.chars().count();
        let sel = self.ext.edit_sel.map(|(a, b)| (a.min(total), b.min(total))).filter(|(a, b)| a != b);
        if let Some((a, b)) = sel {
            let g = &fin.galley;
            let (ca, cb) = (g.from_ccursor(CCursor::new(a)), g.from_ccursor(CCursor::new(b)));
            let (ra, rb) = (g.pos_from_cursor(&ca), g.pos_from_cursor(&cb));
            for r in ca.rcursor.row..=cb.rcursor.row {
                let Some(row) = g.rows.get(r) else { continue };
                let left = if r == ca.rcursor.row { ra.left() } else { row.rect.left() };
                // Baris yang berlanjut ke bawah: sorotan sedikit melewati huruf terakhir.
                let right = if r == cb.rcursor.row { rb.left() } else { row.rect.right() + lay * 0.25 };
                let rect = Rect::from_min_max(to_screen(pos2(left, row.rect.top())), to_screen(pos2(right.max(left), row.rect.bottom())));
                painter.rect_filled(rect, CornerRadius::ZERO, Color32::from_rgba_unmultiplied(13, 153, 255, 80));
            }
        }
        // Teks digambar setelah sorotan, jadi tidak ada huruf yang tertutup (termasuk baris di atas/bawahnya).
        // Teks besar digambar sebagai mesh vector, sama seperti di canvas, supaya tetap tajam.
        if let Some(mesh) = self.ext.vector_text(ctx, &self.fonts, &fin, fin_origin, k, max * z, painter.clip_rect()) {
            painter.add(egui::Shape::mesh(mesh));
        } else if (k - 1.0).abs() < 0.01 {
            painter.add(egui::Shape::galley(fin_origin, fin.galley.clone(), Color32::BLACK));
        } else {
            let pos = (fin_origin.to_vec2() / k).to_pos2();
            let mut shape = egui::Shape::Text(egui::epaint::TextShape::new(pos, fin.galley.clone(), Color32::BLACK));
            shape.transform(egui::emath::TSTransform::from_scaling(k));
            painter.add(shape);
        }
        if focused {
            let now = ctx.input(|i| i.time);
            let caret_at = caret.unwrap_or(0).min(total);
            // Caret berhenti berkedip sebentar setiap kali posisi atau seleksi berubah.
            if (self.ext.edit_blink.0, self.ext.edit_blink.1) != (caret_at, sel) {
                self.ext.edit_blink = (caret_at, sel, now);
            }
            if (now - self.ext.edit_blink.2) % 1.0 < 0.55 {
                let r = fin.galley.pos_from_cursor(&fin.galley.from_ccursor(CCursor::new(caret_at)));
                let color = cur.style_at(caret_at.saturating_sub(1)).fill;
                let color = Color32::from_rgb(color[0], color[1], color[2]);
                painter.line_segment([to_screen(r.center_top()), to_screen(r.center_bottom())], Stroke::new((lay * k * 0.05).clamp(1.0, 3.0), color));
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(275));
        }
        if over {
            ctx.set_cursor_icon(CursorIcon::Text);
        }

        // Selesai dengan Esc saat teks yang punya fokus (klik di luar teks ditangani di `pointer`).
        if !first && (focused || lost_focus) && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.finish_text_edit();
        }
    }

    // ------------------------------------------------------------ pointer

    fn pointer(&mut self, p: Pos2, inp: &Inp, hovered: bool, space: bool) {
        let o = self.canvas_rect.min;
        let wp = self.view.to_world(o, p);
        let z = self.view.zoom;

        if matches!(self.drag, Drag::None) && hovered {
            if inp.mid_pressed || (inp.pressed && (space || self.tool == Tool::Hand)) {
                self.drag = Drag::Pan;
            } else if inp.pressed && self.editing.is_some() {
                // Klik di dalam teks ditangani editor (caret/seleksi); di luar: selesai mengedit.
                if !self.edit_box().is_some_and(|r| r.contains(p)) {
                    self.finish_text_edit();
                }
            } else if inp.pressed && self.crop.is_some() {
                self.crop_press(p, wp);
            } else if inp.pressed && self.tool == Tool::Pen {
                self.pen_press(p, wp, inp.mods);
            } else if inp.pressed && self.ext.path_edit.is_none() {
                self.press(p, wp, inp);
            }
        }
        // Mode edit titik path mengambil alih klik dan drag.
        if self.ext.path_edit.is_some() && matches!(self.drag, Drag::None) {
            self.path_edit_pointer(p, wp, inp, hovered);
        }

        if self.tool == Tool::ShapeBuilder && matches!(self.drag, Drag::None) {
            self.builder_pointer(wp, inp, hovered);
        }

        // Pen: tahan dan tarik setelah klik untuk membuat lengkungan.
        self.pen_drag_update(wp, inp);

        let mut drag = std::mem::replace(&mut self.drag, Drag::None);
        match &mut drag {
            Drag::None => return,
            Drag::Pan => self.view.pan += inp.delta,
            Drag::TextBox { cur, .. } => *cur = wp,
            Drag::Gradient { id, part, .. } => self.grad_drag(*id, *part, p, inp.mods.shift),
            Drag::Crop { part, start, geom } => self.crop_drag(*part, *start, *geom, wp, inp.mods.shift),
            Drag::Corner { id, at, single, r0, s0 } => self.corner_drag(*id, *at, *single, *r0, *s0, wp),
            Drag::Move { start, applied, moved, dup } => {
                let t = wp - *start;
                // A part of an instance is picked by the click, but stays where the main has it.
                let free = !self.sel_pinned();
                if !free && t.length() * z > 3.0 {
                    self.refuse_pinned();
                }
                if free && (*moved || t.length() * z > 3.0) {
                    let mut total = vec2(t.x.round(), t.y.round());
                    if inp.mods.shift {
                        // Shift: kunci ke sumbu yang dominan.
                        if total.x.abs() >= total.y.abs() {
                            total.y = 0.0;
                        } else {
                            total.x = 0.0;
                        }
                    }
                    // Snapping ke objek lain / frame, dengan garis bantu (tahan Ctrl untuk mematikan).
                    // An item of an auto layout is placed by its frame: nothing to snap to.
                    if inp.mods.command || self.drag_in_layout(*start + total) {
                        self.ext.guides.clear();
                        self.ext.gaps.clear();
                    } else {
                        total = self.snap_move(total);
                    }
                    let d = total - *applied;
                    if d != Vec2::ZERO {
                        *applied = total;
                        *moved = true;
                        for id in self.roots() {
                            if let Some(n) = self.doc.find_mut(id) {
                                n.translate(d.x, d.y);
                            }
                        }
                    }
                }
                if *moved {
                    self.reorder_drag(*start + *applied);
                }
                // Alt ditahan kapan pun selama drag: yang digeser jadi salinan, aslinya kembali ke
                // tempat semula. Alt dilepas lagi sebelum mouse dilepas: salinan dibuang (seperti Figma).
                if *moved && inp.mods.alt != dup.is_some() {
                    match dup.take() {
                        None => {
                            let (originals, roots, mut nodes) = (self.sel.clone(), self.roots(), self.selected_nodes());
                            self.doc.regroup(&mut nodes);
                            self.sel.clear();
                            let mut ids = std::collections::HashMap::new();
                            for mut n in nodes {
                                // Alt-dragging a main component drags out an instance of it.
                                if !self.doc.settle_copy(&mut n) {
                                    self.doc.reassign_ids_into(&mut n, &mut ids);
                                }
                                let (cx, cy) = n.center();
                                let parent = self.doc.frame_for(&n, cx, cy, &[]);
                                let parent = parent.filter(|_| !self.doc.would_cycle(&n, parent));
                                self.sel.push(n.id);
                                self.doc.insert(n, parent);
                            }
                            // A text copied together with its path is on the copy of the path.
                            self.relink_paths(&ids);
                            // The originals go back to where the drag began: also into the frame
                            // and the place in the row that the drag may have taken them out of.
                            let mut back: Vec<(Node, Option<u64>, usize)> = Vec::new();
                            if let Some(pre) = &self.pre_drag {
                                for id in &roots {
                                    let parent = pre.parent_of(*id);
                                    let at = pre.child_ids(parent).iter().position(|c| c == id);
                                    back.extend(pre.find(*id).cloned().zip(at).map(|(n, at)| (n, parent, at)));
                                }
                            }
                            back.sort_by_key(|b| b.2);
                            for (n, parent, at) in back {
                                self.doc.remove(n.id);
                                self.doc.insert_at(n, parent, at);
                            }
                            *dup = Some(originals);
                        }
                        Some(originals) => {
                            for id in self.roots() {
                                self.doc.remove(id);
                            }
                            self.sel = originals;
                            for id in self.roots() {
                                if let Some(n) = self.doc.find_mut(id) {
                                    n.translate(applied.x, applied.y);
                                }
                            }
                        }
                    }
                }
            }
            Drag::Resize { id, handle, orig, start } => {
                let d = wp - *start;
                let (ox, oy, ow, oh) = *orig;
                // How it was sized when the drag began: a side that is back at its old size
                // hugs or fills as before.
                let sized = self.pre_drag.as_ref().and_then(|d| d.find(*id)).map(|n| (n.size_h, n.size_v));
                if let Some(n) = self.doc.find_mut(*id) {
                    // Alt: ukuran berubah dari tengah (sisi seberang ikut bergerak berlawanan), seperti Figma.
                    let centered = inp.mods.alt;
                    if n.kind == Kind::Line {
                        if centered {
                            // Kedua ujung bergerak berlawanan, titik tengah garis tetap.
                            let d = if *handle == 0 { -d } else { d };
                            n.x = (ox - d.x).round();
                            n.y = (oy - d.y).round();
                            n.w = (ox + ow + d.x).round() - n.x;
                            n.h = (oy + oh + d.y).round() - n.y;
                        } else if *handle == 0 {
                            n.x = (ox + d.x).round();
                            n.y = (oy + d.y).round();
                            n.w = ox + ow - n.x;
                            n.h = oy + oh - n.y;
                        } else {
                            n.w = (ow + d.x).round();
                            n.h = (oh + d.y).round();
                        }
                    } else {
                        // Objek berputar: pergeseran kursor dihitung di ruang lokal objek.
                        let rot = n.rotation;
                        let d = if rot != 0.0 {
                            let (dx, dy) = crate::doc::rotate(d.x, d.y, 0.0, 0.0, -rot);
                            vec2(dx, dy)
                        } else {
                            d
                        };
                        let (mut l, mut t, mut r, mut b) = (ox, oy, ox + ow, oy + oh);
                        match *handle {
                            0 | 6 | 7 => l += d.x,
                            2 | 3 | 4 => r += d.x,
                            _ => {}
                        }
                        match *handle {
                            0 | 1 | 2 => t += d.y,
                            4 | 5 | 6 => b += d.y,
                            _ => {}
                        }
                        if centered {
                            // Sisi seberang bergerak sama jauh ke arah berlawanan.
                            (l, r) = (l - (r - (ox + ow)), r - (l - ox));
                            (t, b) = (t - (b - (oy + oh)), b - (t - oy));
                        }
                        // Shift: ukuran berubah seragam (rasio terjaga). Gambar selalu begitu dari pegangan sudut.
                        let corner = matches!(*handle, 0 | 2 | 4 | 6);
                        if (inp.mods.shift || (n.kind == Kind::Image && corner)) && ow > 0.0 && oh > 0.0 {
                            if corner {
                                let scale = ((r - l).abs() / ow).max((b - t).abs() / oh);
                                let (w, h) = (ow * scale, oh * scale);
                                if matches!(*handle, 0 | 6) {
                                    l = r - w;
                                } else {
                                    r = l + w;
                                }
                                if matches!(*handle, 0 | 2) {
                                    t = b - h;
                                } else {
                                    b = t + h;
                                }
                            } else if matches!(*handle, 3 | 7) {
                                // Pegangan sisi: sumbu lain ikut membesar dari tengah.
                                let h = (r - l).abs() * oh / ow;
                                (t, b) = (oy + (oh - h) * 0.5, oy + (oh + h) * 0.5);
                            } else {
                                let w = (b - t).abs() * ow / oh;
                                (l, r) = (ox + (ow - w) * 0.5, ox + (ow + w) * 0.5);
                            }
                        }
                        if centered {
                            // Penguncian rasio di atas berjangkar di sudut seberang; kembalikan ke tengah.
                            let (cx, cy, w, h) = (ox + ow * 0.5, oy + oh * 0.5, r - l, b - t);
                            (l, r) = (cx - w * 0.5, cx + w * 0.5);
                            (t, b) = (cy - h * 0.5, cy + h * 0.5);
                        }
                        // The top and bottom handles of a text give it a height of its own.
                        let own_height = n.fixed_h || matches!(*handle, 1 | 5);
                        if n.kind == Kind::Text && !own_height {
                            // Tinggi teks mengikuti isinya; hanya lebar kotaknya yang bisa diubah.
                            (t, b) = (oy, oy + oh);
                        }
                        if rot == 0.0 {
                            (l, t, r, b) = (l.round(), t.round(), r.round(), b.round());
                        } else {
                            // Node berputar terhadap pusatnya; geser supaya sisi seberang tetap di tempat.
                            let (dcx, dcy) = ((l + r) * 0.5 - (ox + ow * 0.5), (t + b) * 0.5 - (oy + oh * 0.5));
                            let (rx, ry) = crate::doc::rotate(dcx, dcy, 0.0, 0.0, rot);
                            let (sx, sy) = (rx - dcx, ry - dcy);
                            (l, t, r, b) = (l + sx, t + sy, r + sx, b + sy);
                        }
                        n.x = l.min(r);
                        n.y = t.min(b);
                        n.w = (r - l).abs().max(1.0);
                        n.h = (b - t).abs().max(1.0);
                        // Seperti Figma: menarik pegangan teks lebar-otomatis menjadikannya lebar-tetap.
                        if n.kind == Kind::Text && (n.w - ow).abs() > 0.5 {
                            n.wrap = true;
                        }
                        // From then on the height no longer follows the lines ("Auto height" in
                        // the panel gives it back).
                        if n.kind == Kind::Text && own_height && (n.h - oh).abs() > 0.5 {
                            n.fixed_h = true;
                        }
                        // A side that was dragged has the size it was dragged to: it no longer
                        // hugs or fills. (The other one may only have been rounded.)
                        let (across, down) = (!matches!(*handle, 1 | 5) || inp.mods.shift, !matches!(*handle, 3 | 7) || inp.mods.shift);
                        if let Some(sized) = sized {
                            (n.size_h, n.size_v) = sized;
                        }
                        if across && (n.w - ow).abs() > 1e-3 {
                            n.size_h = Sizing::Fixed;
                        }
                        if down && (n.h - oh).abs() > 1e-3 {
                            n.size_v = Sizing::Fixed;
                        }
                    }
                }
                // A frame lays out what it holds, from how it was when the drag began (an
                // instance: from its main). Ctrl: the children stay where they are.
                if let Some((to, sizes)) = self.doc.find(*id).filter(|n| n.kind == Kind::Frame).map(|n| ((n.x, n.y, n.w, n.h), (n.size_h, n.size_v))) {
                    let children = if inp.mods.command { Children::Keep } else { Children::Constrain };
                    let mut from = self.pre_drag.as_ref().and_then(|d| d.find(*id)).cloned();
                    if let Some(f) = &mut from {
                        (f.size_h, f.size_v) = sizes;
                    }
                    self.doc.set_box(*id, from.as_ref(), to, children);
                }
            }
            Drag::Scale { id, handle, orig } => {
                let (x0, y0, x1, y1) = orig.bounds();
                let (fx, fy) = HANDLE_F[*handle as usize % 8];
                let (hx, hy) = (x0 + (x1 - x0) * fx, y0 + (y1 - y0) * fy);
                let (ax, ay) = (x0 + (x1 - x0) * (1.0 - fx), y0 + (y1 - y0) * (1.0 - fy));
                let sx = (fx != 0.5 && (hx - ax).abs() > 0.001).then(|| (wp.x - ax) / (hx - ax));
                let sy = (fy != 0.5 && (hy - ay).abs() > 0.001).then(|| (wp.y - ay) / (hy - ay));
                let mut s = match (sx, sy) {
                    (Some(a), Some(b)) => a.max(b),
                    (Some(a), None) | (None, Some(a)) => a,
                    _ => 1.0,
                }
                .clamp(0.01, 1000.0);
                let (mut ax, mut ay) = (ax, ay);
                if orig.rotation != 0.0 {
                    // Objek berputar: skala terhadap pusat, dari jarak kursor ke pusat.
                    let (cx, cy) = orig.center();
                    let (rhx, rhy) = crate::doc::rotate(hx, hy, cx, cy, orig.rotation);
                    let base = ((rhx - cx).powi(2) + (rhy - cy).powi(2)).sqrt().max(0.001);
                    s = (((wp.x - cx).powi(2) + (wp.y - cy).powi(2)).sqrt() / base).clamp(0.01, 1000.0);
                    (ax, ay) = (cx, cy);
                }
                let mut scaled = orig.clone();
                scale_node(&mut scaled, s, ax, ay);
                if s != 1.0 && scaled.size_h == Sizing::Fill {
                    scaled.size_h = Sizing::Fixed;
                }
                if s != 1.0 && scaled.size_v == Sizing::Fill {
                    scaled.size_v = Sizing::Fixed;
                }
                if let Some(slot) = self.doc.find_mut(*id) {
                    *slot = scaled;
                }
            }
            Drag::MultiResize { handle, bounds, start, orig, scale } => {
                let (x0, y0, x1, y1) = *bounds;
                let (bw, bh) = (x1 - x0, y1 - y0);
                let (fx, fy) = HANDLE_F[*handle as usize % 8];
                let d = wp - *start;
                // Alt: ukuran berubah dari tengah, jadi kedua sisi bergerak.
                let centered = inp.mods.alt;
                let both = if centered { 2.0 } else { 1.0 };
                let (dw, dh) = ((fx - 0.5) * 2.0 * d.x * both, (fy - 0.5) * 2.0 * d.y * both);
                let grow = |size: f32, delta: f32| if size > 0.001 { ((size + delta) / size).max(1.0 / size) } else { 1.0 };
                let (mut sx, mut sy) = (grow(bw, dw), grow(bh, dh));
                // Scale tool, Shift, atau seleksi berisi gambar (dari sudut): rasio terjaga.
                let corner = fx != 0.5 && fy != 0.5;
                let uniform = *scale || inp.mods.shift || (corner && orig.iter().any(|n| n.kind == Kind::Image));
                if uniform {
                    let s = if corner { sx.max(sy) } else if fx != 0.5 { sx } else { sy };
                    (sx, sy) = (s.max(0.01), s.max(0.01));
                } else {
                    if bw > 0.001 {
                        sx = (bw * sx).round().max(1.0) / bw;
                    }
                    if bh > 0.001 {
                        sy = (bh * sy).round().max(1.0) / bh;
                    }
                }
                // Sisi seberang pegangan tetap di tempat (atau pusat kotak, dengan Alt / pegangan tengah).
                let place = |f: f32, a0: f32, a1: f32, size: f32| {
                    if centered || f == 0.5 {
                        (a0 + a1 - size) * 0.5
                    } else if f == 0.0 {
                        a1 - size
                    } else {
                        a0
                    }
                };
                let (nx, ny) = (place(fx, x0, x1, bw * sx), place(fy, y0, y1, bh * sy));
                for node in orig.iter() {
                    let mut n = node.clone();
                    // Stretched by hand: that side no longer hugs or fills.
                    if sx != 1.0 && (!*scale || n.size_h == Sizing::Fill) {
                        n.size_h = Sizing::Fixed;
                    }
                    if sy != 1.0 && (!*scale || n.size_v == Sizing::Fill) {
                        n.size_v = Sizing::Fixed;
                    }
                    if *scale {
                        scale_node(&mut n, sx, x0, y0);
                        n.translate(nx - x0, ny - y0);
                    } else if n.kind == Kind::Frame && !inp.mods.command {
                        // The frame is stretched with the selection; what it holds follows its constraints.
                        let mut g = Node::new(0, Kind::Frame, n.x, n.y);
                        (g.w, g.h, g.rotation) = (n.w, n.h, n.rotation);
                        g.stretch((x0, y0), (nx, ny), (sx, sy));
                        crate::layout::set_box(&mut n, (g.x, g.y, g.w, g.h), Children::Constrain);
                        self.doc.refit(&mut n, node);
                    } else {
                        n.stretch((x0, y0), (nx, ny), (sx, sy));
                    }
                    if let Some(slot) = self.doc.find_mut(n.id) {
                        *slot = n;
                    }
                }
            }
            Drag::Rotate { id, center, start, orig } => {
                let angle = (wp.y - center.y).atan2(wp.x - center.x).to_degrees();
                let mut rot = *orig + angle - *start;
                if inp.mods.shift {
                    // Shift: kelipatan 15°.
                    rot = (rot / 15.0).round() * 15.0;
                }
                rot = (rot + 180.0).rem_euclid(360.0) - 180.0;
                if let Some(n) = self.doc.find_mut(*id) {
                    n.rotation = (rot * 10.0).round() / 10.0;
                }
            }
            Drag::MultiRotate { center, start, orig, angle } => {
                let mut rot = (wp.y - center.y).atan2(wp.x - center.x).to_degrees() - *start;
                if inp.mods.shift {
                    rot = (rot / 15.0).round() * 15.0;
                }
                rot = (rot + 180.0).rem_euclid(360.0) - 180.0;
                *angle = (rot * 10.0).round() / 10.0;
                // Selalu dihitung dari keadaan awal, jadi posisi tidak bergeser sedikit demi sedikit.
                for node in orig.iter() {
                    let mut n = node.clone();
                    spin_node(&mut n, (center.x, center.y), *angle);
                    if let Some(slot) = self.doc.find_mut(n.id) {
                        *slot = n;
                    }
                }
            }
            Drag::Marquee { start, base } => {
                let mut sel = base.clone();
                for id in self.marquee_pick(*start, wp) {
                    if !sel.contains(&id) {
                        sel.push(id);
                    }
                }
                self.sel = sel;
            }
            Drag::Create { id, start } => {
                let (mut dw, mut dh) = (wp.x.round() - start.x, wp.y.round() - start.y);
                if let Some(n) = self.doc.find_mut(*id) {
                    if n.kind == Kind::Line {
                        n.w = dw;
                        n.h = dh;
                    } else {
                        if inp.mods.shift {
                            let m = dw.abs().max(dh.abs());
                            dw = m * dw.signum();
                            dh = m * dh.signum();
                        }
                        n.x = start.x.min(start.x + dw);
                        n.y = start.y.min(start.y + dh);
                        n.w = dw.abs();
                        n.h = dh.abs();
                    }
                }
            }
        }

        let ended = match drag {
            Drag::Pan => !inp.down && !inp.mid_down,
            _ => !inp.down,
        };
        if ended {
            // Klik (tanpa menarik) di area kosong frame berisi objek: pilih frame-nya, tanpa menggesernya.
            if let Drag::Marquee { start, .. } = &drag {
                if (wp - *start).length() * z <= 3.0 {
                    // Ctrl+click: the deepest object under the pointer, however many frames it sits in.
                    let deep = if inp.mods.command { self.doc.hit_deep(wp.x, wp.y, 4.0 / z) } else { None };
                    if let Some(id) = deep.or_else(|| self.doc.hit(wp.x, wp.y, 4.0 / z).filter(|id| self.body_locked(*id))) {
                        if !self.sel.contains(&id) {
                            self.sel.push(id);
                        }
                    }
                }
            }
            self.release(drag);
        } else {
            self.drag = drag;
        }
    }

    fn press(&mut self, p: Pos2, wp: Pos2, inp: &Inp) {
        let tol = 4.0 / self.view.zoom;
        match self.tool {
            Tool::Hand | Tool::Pen | Tool::ShapeBuilder => {}
            Tool::Wand => self.wand_click(p, wp, inp),
            Tool::Select | Tool::Scale => {
                if let Some((id, part)) = self.grad_hit(p) {
                    // Pegangan gradasi (tampil saat popup fill terbuka) menang atas pegangan ukuran.
                    if let Some(orig) = self.doc.find(id).and_then(|n| n.gradient.clone()) {
                        if part >= 2 {
                            self.ext.picker.stop = part - 2;
                        }
                        self.pre_drag = Some(self.doc.clone());
                        self.drag = Drag::Gradient { id, part, orig };
                    }
                } else if let (Tool::Scale, Some((id, handle))) = (self.tool, self.handle_at(p)) {
                    if let Some(orig) = self.doc.find(id).cloned() {
                        self.pre_drag = Some(self.doc.clone());
                        self.drag = Drag::Scale { id, handle, orig };
                    }
                } else if let Some((id, handle)) = self.handle_at(p) {
                    if let Some(orig) = self.doc.find(id).map(|n| (n.x, n.y, n.w, n.h)) {
                        self.pre_drag = Some(self.doc.clone());
                        self.drag = Drag::Resize { id, handle, orig, start: wp };
                    }
                } else if let Some(handle) = self.multi_handle_at(p) {
                    if let Some(bounds) = self.sel_box() {
                        self.pre_drag = Some(self.doc.clone());
                        let (orig, scale) = (self.selected_nodes(), self.tool == Tool::Scale);
                        self.drag = Drag::MultiResize { handle, bounds, start: wp, orig, scale };
                    }
                } else if let Some((id, at, r0, s0)) = self.corner_hit(p) {
                    // Widget sudut vector: tarik = semua sudut, Alt+tarik = sudut itu saja.
                    self.pre_drag = Some(self.doc.clone());
                    self.drag = Drag::Corner { id, at, single: inp.mods.alt, r0, s0 };
                } else if let Some(id) = self.rotate_zone(p) {
                    // Tarik dari luar sudut: putar terhadap pusat objek.
                    if let Some((cx, cy, orig)) = self.doc.find(id).map(|n| (n.center().0, n.center().1, n.rotation)) {
                        let start = (wp.y - cy).atan2(wp.x - cx).to_degrees();
                        self.pre_drag = Some(self.doc.clone());
                        self.drag = Drag::Rotate { id, center: pos2(cx, cy), start, orig };
                    }
                } else if let Some(center) = self.multi_rotate_zone(p) {
                    // Beberapa objek / group: putar bersama terhadap pusat kotak seleksi.
                    let start = (wp.y - center.y).atan2(wp.x - center.x).to_degrees();
                    self.pre_drag = Some(self.doc.clone());
                    self.drag = Drag::MultiRotate { center, start, orig: self.selected_nodes(), angle: 0.0 };
                } else if let Some(id) = self.hit(p, wp, tol).filter(|_| !inp.mods.command) {
                    // Anggota group dipilih bersama seluruh group-nya.
                    let members = self.with_group(id);
                    if inp.mods.shift {
                        if self.sel.contains(&id) {
                            self.sel.retain(|s| !members.contains(s));
                        } else {
                            for m in members {
                                if !self.sel.contains(&m) {
                                    self.sel.push(m);
                                }
                            }
                        }
                    } else if !self.sel.contains(&id) {
                        self.sel = members;
                    }
                    // Layer terkunci yang ikut terpilih (dari panel layer) tidak ikut digeser.
                    let fixed: Vec<u64> = self.sel.iter().copied().filter(|id| self.locked(*id)).collect();
                    self.sel.retain(|id| !fixed.contains(id));
                    // Alt (duplikat) dicek selama drag, jadi boleh ditekan sebelum atau sesudah mulai menggeser.
                    self.pre_drag = Some(self.doc.clone());
                    self.drag = Drag::Move { start: wp, applied: Vec2::ZERO, moved: false, dup: None };
                    self.begin_snap();
                } else {
                    let base = if inp.mods.shift { self.sel.clone() } else { Vec::new() };
                    self.sel = base.clone();
                    self.drag = Drag::Marquee { start: wp, base };
                }
            }
            // Klik = teks lebar-otomatis; tarik = kotak paragraf (ditentukan saat dilepas).
            Tool::Text => self.drag = Drag::TextBox { start: wp, cur: wp },
            tool => {
                let kind = match tool {
                    Tool::Frame => Kind::Frame,
                    Tool::Rect => Kind::Rect,
                    Tool::Ellipse => Kind::Ellipse,
                    Tool::Polygon => Kind::Path,
                    _ => Kind::Line,
                };
                let pre = self.doc.clone();
                let id = self.doc.alloc_id();
                let start = pos2(wp.x.round(), wp.y.round());
                let mut n = Node::new(id, kind, start.x, start.y);
                match kind {
                    Kind::Frame => {
                        n.name = format!("Frame {}", self.doc.frame_count() + 1);
                        n.fill = Some([255, 255, 255, 255]);
                    }
                    Kind::Line => {
                        let on_frame = self.doc.frame_at(wp.x, wp.y, &[]).is_some();
                        n.stroke = Some(if on_frame { [0, 0, 0, 255] } else { [235, 235, 235, 255] });
                        n.stroke_w = 2.0;
                    }
                    Kind::Path => {
                        // Tool Polygon: path tertutup biasa, jadi titiknya bisa diedit seperti path lain.
                        n.points = super::polygon::points(&self.poly);
                        n.closed = true;
                        n.name = format!("{} {id}", if self.poly.star { "Star" } else { "Polygon" });
                        n.fill = Some([217, 217, 217, 255]);
                    }
                    _ => n.fill = Some([217, 217, 217, 255]),
                }
                // A frame drawn inside a frame becomes its child, like any other object.
                let parent = self.doc.frame_at(wp.x, wp.y, &[]);
                self.doc.insert_placed(n, parent);
                self.pre_drag = Some(pre);
                self.sel = vec![id];
                self.drag = Drag::Create { id, start };
            }
        }
    }

    fn release(&mut self, drag: Drag) {
        let pre = self.pre_drag.take();
        self.end_snap();
        self.drop_mark = None;
        match drag {
            Drag::Rotate { id, orig, .. } => {
                let changed = self.doc.find(id).is_some_and(|n| n.rotation != orig);
                if let (true, Some(pre)) = (changed, pre) {
                    self.push_undo(pre);
                }
            }
            Drag::Move { moved: true, start, applied, .. } => {
                // Objek yang digeser masuk/keluar frame berpindah induk (seperti di Figma): ke frame
                // terdalam di bawah pointer. Beberapa objek sekaligus: masing-masing menurut titik
                // tengahnya, supaya seleksi lintas frame tidak terkumpul di satu frame. Frame hanya
                // masuk ke frame yang lebih besar darinya, dan tidak pernah ke dalam yang ikut digeser.
                let roots = self.roots();
                // Titik pegang setelah snapping / Shift, bukan posisi pointer mentah.
                let grab = start + applied;
                for &id in &roots {
                    let Some(n) = self.doc.find(id) else { continue };
                    // An item of an auto layout was put where it belongs while it was dragged.
                    if self.doc.auto_item(id) {
                        continue;
                    }
                    let (x, y) = if roots.len() == 1 { (grab.x, grab.y) } else { n.center() };
                    let target = self.doc.frame_for(n, x, y, &roots);
                    if target != self.doc.parent_of(id) && self.doc.would_cycle(n, target) {
                        self.status = "A component can't contain an instance of itself: the layer stays in the frame it was in.".into();
                    } else if target != self.doc.parent_of(id) {
                        if let Some(node) = self.doc.remove(id) {
                            self.doc.insert(node, target);
                        }
                    }
                }
                if let Some(pre) = pre {
                    self.push_undo(pre);
                }
            }
            Drag::Resize { id, .. } => {
                // (Against the whole node as it was: its box may be back where it began while
                // something else about it is not.)
                let changed = pre.as_ref().is_some_and(|d| d.find(id) != self.doc.find(id));
                if let (true, Some(pre)) = (changed, pre) {
                    self.push_undo(pre);
                }
            }
            Drag::Corner { id, .. } => {
                let changed = pre.as_ref().is_some_and(|d| d.find(id) != self.doc.find(id));
                if let (true, Some(pre)) = (changed, pre) {
                    self.push_undo(pre);
                }
            }
            Drag::Gradient { id, orig, .. } => {
                let changed = self.doc.find(id).is_some_and(|n| n.gradient.as_ref() != Some(&orig));
                if let (true, Some(pre)) = (changed, pre) {
                    self.push_undo(pre);
                }
            }
            Drag::Scale { id, orig, .. } => {
                let changed = self.doc.find(id).is_some_and(|n| *n != orig);
                if let (true, Some(pre)) = (changed, pre) {
                    self.push_undo(pre);
                }
            }
            Drag::MultiResize { orig, .. } | Drag::MultiRotate { orig, .. } => {
                let changed = orig.iter().any(|o| self.doc.find(o.id) != Some(o));
                if let (true, Some(pre)) = (changed, pre) {
                    self.push_undo(pre);
                }
            }
            Drag::Create { id, .. } => {
                if let Some(n) = self.doc.find_mut(id) {
                    // Klik tanpa drag: beri ukuran bawaan.
                    if n.w.abs() < 3.0 && n.h.abs() < 3.0 {
                        let (w, h) = match n.kind {
                            Kind::Frame => (390.0, 844.0),
                            Kind::Line => (120.0, 0.0),
                            _ => (100.0, 100.0),
                        };
                        n.w = w;
                        n.h = h;
                    }
                }
                if let Some(pre) = pre {
                    self.push_undo(pre);
                }
                self.tool = Tool::Select;
            }
            Drag::TextBox { start, cur } => {
                let width = (cur.x - start.x).abs();
                let dragged = width * self.view.zoom > 8.0;
                let pre = self.doc.clone();
                let id = self.doc.alloc_id();
                let (x, y) = if dragged { (start.x.min(cur.x), start.y.min(cur.y)) } else { (start.x, start.y) };
                let mut n = Node::new(id, Kind::Text, x.round(), y.round());
                n.name = "Text".into();
                let parent = self.doc.frame_at(start.x, start.y, &[]);
                n.fill = Some(if parent.is_some() { [0, 0, 0, 255] } else { [235, 235, 235, 255] });
                n.wrap = dragged;
                n.w = if dragged { width.round() } else { 4.0 };
                n.h = n.font_size * 1.2;
                // A click on the outline of a shape that has no fill: the text goes on it,
                // starting where the click was.
                let carrier = if dragged { None } else { self.outline_under((start.x, start.y), 5.0 / self.view.zoom) };
                if let Some((path, along)) = carrier {
                    (n.on_path, n.path_offset) = (Some(path), along);
                    self.status = "Text on a path: type, then click outside. Ctrl+Alt+P detaches it; drag it to slide it along.".into();
                }
                self.doc.insert_placed(n, parent);
                self.start_text_edit(id, pre);
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ pen

    /// Konten contoh untuk tangkapan layar `--bench`: gradasi, path, paragraf bergaya, gambar.
    pub(super) fn bench_demo(&mut self) {
        use crate::doc::{Gradient, Span, Stop, Style};
        let (fx, fy) = (-1500.0, 0.0);
        let mut frame = Node::new(self.doc.alloc_id(), Kind::Frame, fx, fy);
        frame.name = "Feature demo".into();
        (frame.w, frame.h) = (1200.0, 760.0);
        frame.fill = Some([250, 250, 250, 255]);
        let stops = |list: &[(f32, [u8; 4])]| list.iter().map(|(pos, color)| Stop { pos: *pos, color: *color }).collect::<Vec<_>>();

        let mut rect = Node::new(self.doc.alloc_id(), Kind::Rect, fx + 40.0, fy + 40.0);
        rect.name = "Linear gradient".into();
        (rect.w, rect.h, rect.radius) = (330.0, 200.0, 28.0);
        rect.fill = Some([255, 255, 255, 255]);
        rect.gradient = Some(Gradient {
            radial: false,
            angle: 35.0,
            stops: stops(&[(0.0, [255, 94, 98, 255]), (0.5, [255, 195, 113, 255]), (1.0, [72, 126, 255, 255])]),
            line: None,
            radial_line: None,
        });
        let rect_id = rect.id;

        let mut ellipse = Node::new(self.doc.alloc_id(), Kind::Ellipse, fx + 410.0, fy + 40.0);
        (ellipse.w, ellipse.h) = (200.0, 200.0);
        ellipse.fill = Some([255, 255, 255, 255]);
        ellipse.gradient = Some(Gradient {
            radial: true,
            angle: 0.0,
            stops: stops(&[(0.0, [255, 255, 255, 255]), (0.6, [120, 200, 255, 255]), (1.0, [30, 60, 160, 255])]),
            line: None,
            radial_line: None,
        });

        // Bintang (cekung) dengan gradasi, lewat jalur yang sama dengan pen tool.
        let mut star = Node::new(self.doc.alloc_id(), Kind::Path, fx + 650.0, fy + 40.0);
        (star.w, star.h) = (200.0, 200.0);
        star.points = (0..10)
            .map(|i| {
                let a = std::f32::consts::PI * (i as f32 / 5.0 - 0.5);
                let r = if i % 2 == 0 { 0.5 } else { 0.21 };
                PathPoint { x: 0.5 + r * a.cos(), y: 0.5 + r * a.sin(), hin: [0.0; 2], hout: [0.0; 2], r: None }
            })
            .collect();
        star.closed = true;
        star.rotation = 18.0;
        star.fill = Some([255, 255, 255, 255]);
        star.gradient = Some(Gradient { radial: false, angle: 90.0, stops: stops(&[(0.0, [151, 71, 255, 255]), (1.0, [255, 36, 189, 255])]), line: None, radial_line: None });

        let mut wave = Node::new(self.doc.alloc_id(), Kind::Path, fx + 900.0, fy + 60.0);
        (wave.w, wave.h) = (250.0, 160.0);
        let curve = |x: f32, y: f32, hx: f32, hy: f32| PathPoint { x, y, hin: [-hx, -hy], hout: [hx, hy], r: None };
        wave.points = vec![curve(0.0, 0.5, 0.15, -0.6), curve(0.5, 0.5, 0.15, 0.6), curve(1.0, 0.5, 0.15, -0.6)];
        wave.stroke = Some([20, 174, 92, 255]);
        wave.stroke_w = 6.0;

        // Paragraf: lebar tetap, dengan sebagian huruf memakai font/warna/ukuran lain.
        let mut para = Node::new(self.doc.alloc_id(), Kind::Text, fx + 40.0, fy + 290.0);
        para.text = "Dokumentasi Kajian: paragraph text wraps inside its box, and only the selected characters change style.".into();
        para.name = "Paragraph".into();
        (para.wrap, para.w, para.h) = (true, 520.0, 200.0);
        (para.font_size, para.font, para.line_height) = (34.0, "Montserrat".into(), 125.0);
        para.fill = Some([20, 20, 20, 255]);
        let style = |font: &str, weight: u16, italic: bool, size: f32, fill: [u8; 4]| Style { font: font.into(), weight, italic, size, fill, underline: false, strike: false };
        para.spans = vec![
            Span { start: 12, end: 18, style: style("Playfair Display", 700, true, 44.0, [222, 60, 75, 255]) },
            Span { start: 20, end: 29, style: style("Montserrat", 700, false, 34.0, [13, 120, 255, 255]) },
        ];

        // Tipografi campuran: line height rapat, dua huruf memakai font dan ukuran lain.
        let mut centered = Node::new(self.doc.alloc_id(), Kind::Text, fx + 40.0, fy + 540.0);
        centered.text = "Penyerahan\nBantuan SMP5".into();
        centered.name = "Headline".into();
        (centered.font, centered.weight, centered.font_size) = ("Plus Jakarta Sans".into(), 700, 64.0);
        (centered.line_height, centered.letter_spacing) = (82.0, -4.0);
        centered.fill = Some([24, 60, 160, 255]);
        (centered.w, centered.h) = (400.0, 105.0);
        centered.spans = vec![
            Span { start: 6, end: 7, style: style("La Belle Aurore", 400, false, 92.0, [24, 60, 160, 255]) },
            Span { start: 11, end: 12, style: style("La Belle Aurore", 400, false, 104.0, [222, 60, 75, 255]) },
        ];

        // Blend mode di atas gambar, dan satu kotak yang diputar.
        let mut multiply = Node::new(self.doc.alloc_id(), Kind::Ellipse, fx + 650.0, fy + 330.0);
        multiply.name = "Multiply".into();
        (multiply.w, multiply.h, multiply.blend) = (190.0, 190.0, 2);
        multiply.fill = Some([255, 70, 70, 255]);
        let multiply_id = multiply.id;
        let mut screen = Node::new(self.doc.alloc_id(), Kind::Ellipse, fx + 880.0, fy + 380.0);
        screen.name = "Screen".into();
        (screen.w, screen.h, screen.blend) = (190.0, 190.0, 4);
        screen.fill = Some([60, 90, 255, 255]);
        let mut tilted = Node::new(self.doc.alloc_id(), Kind::Rect, fx + 960.0, fy + 620.0);
        tilted.name = "Rotated".into();
        (tilted.w, tilted.h, tilted.radius, tilted.rotation) = (170.0, 80.0, 14.0, -24.0);
        tilted.fill = Some([20, 174, 92, 255]);
        tilted.stroke = Some([10, 80, 40, 255]);
        tilted.stroke_w = 4.0;

        // Gambar buatan (pola warna) untuk menguji jalur decode → tekstur.
        let img = image::RgbaImage::from_fn(320, 200, |x, y| {
            let on = (x / 40 + y / 40) % 2 == 0;
            image::Rgba([(x * 255 / 320) as u8, if on { 200 } else { 90 }, (y * 255 / 200) as u8, 255])
        });
        let mut png = Vec::new();
        let _ = img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png);
        let asset = self.doc.add_asset(png);
        let mut picture = Node::new(self.doc.alloc_id(), Kind::Image, fx + 620.0, fy + 300.0);
        picture.name = "Image".into();
        (picture.w, picture.h, picture.radius) = (480.0, 300.0, 36.0);
        picture.image = Some(asset);
        picture.opacity = 0.9;

        // Clipping mask: alas lingkaran bergradasi ke transparan; gambar dan kotak miring di-clip kepadanya.
        let mut base = Node::new(self.doc.alloc_id(), Kind::Ellipse, fx + 455.0, fy + 575.0);
        base.name = "Mask base".into();
        (base.w, base.h) = (150.0, 150.0);
        base.fill = Some([255, 255, 255, 255]);
        base.gradient = Some(Gradient {
            radial: true,
            angle: 0.0,
            stops: stops(&[(0.0, [30, 30, 30, 255]), (0.7, [30, 30, 30, 255]), (1.0, [30, 30, 30, 0])]),
            line: None,
            radial_line: None,
        });
        let mut clipped = Node::new(self.doc.alloc_id(), Kind::Image, fx + 430.0, fy + 560.0);
        clipped.name = "Clipped image".into();
        (clipped.w, clipped.h, clipped.clip) = (200.0, 125.0, true);
        clipped.image = Some(asset);
        let mut band = Node::new(self.doc.alloc_id(), Kind::Rect, fx + 430.0, fy + 660.0);
        band.name = "Clipped band".into();
        (band.w, band.h, band.rotation, band.clip) = (200.0, 40.0, -12.0, true);
        band.fill = Some([255, 210, 0, 255]);

        frame.children = vec![rect, ellipse, star, wave, para, centered, base, clipped, band, picture, multiply, screen, tilted];
        let bounds = frame.bounds();
        self.doc.insert(frame, None);
        self.zoom_to(bounds, 1.0);
        let _ = rect_id;
        self.sel = vec![multiply_id];
    }

    /// `KANVAS_BENCH_TYPO`: a frame with underlined and struck ranges in mixed sizes and colours,
    /// a justified paragraph, a bulleted and a numbered list (each with a wrapped item and an
    /// empty paragraph), three boxes of fixed height with their lines at the top, in the middle
    /// and at the bottom, and a line large enough to be drawn from outlines. Faint boxes show
    /// where the text boxes are. Result: ids of [frame, justified paragraph, bulleted list,
    /// numbered list, large line].
    pub(super) fn bench_typo(&mut self) -> Vec<u64> {
        let (x, y) = (-1500.0, 10000.0);
        let (ink, blue, red) = ([30, 30, 40, 255], [13, 110, 220, 255], [220, 50, 60, 255]);
        let stage = self.doc.alloc_id();
        let mut n = Node::new(stage, Kind::Frame, x, y);
        (n.name, n.w, n.h, n.fill) = ("Typography demo".into(), 1000.0, 900.0, Some([252, 252, 250, 255]));
        self.doc.insert(n, None);
        let ground = |app: &mut Self, (nx, ny, w, h): (f32, f32, f32, f32)| {
            let mut n = Node::new(app.doc.alloc_id(), Kind::Rect, x + nx, y + ny);
            (n.name, n.w, n.h, n.fill) = ("Box".into(), w, h, Some([226, 236, 250, 255]));
            app.doc.insert(n, Some(stage));
        };
        // `w` 0: auto width.
        let text = |app: &mut Self, name: &str, s: &str, (nx, ny, w): (f32, f32, f32), size: f32, edit: &dyn Fn(&mut Node)| {
            let id = app.doc.alloc_id();
            let mut n = Node::new(id, Kind::Text, x + nx, y + ny);
            (n.name, n.text, n.font_size, n.fill) = (name.into(), s.into(), size, Some(ink));
            (n.wrap, n.w, n.h) = (w > 0.0, if w > 0.0 { w } else { 100.0 }, size * 1.2);
            edit(&mut n);
            app.doc.insert(n, Some(stage));
            id
        };

        // Ranges: under, through, both; a larger coloured one; bold beside regular (one line under both).
        let s = "Underline, strike, both, big blue, bold and regular";
        let at = |word: &str| {
            let a = s.find(word).map_or(0, |i| s[..i].chars().count());
            (a, a + word.chars().count())
        };
        text(self, "Ranges", s, (40.0, 40.0, 0.0), 26.0, &|n| {
            let (a, b) = at("Underline");
            n.toggle_decoration(a, b, false);
            let (a, b) = at("strike");
            n.toggle_decoration(a, b, true);
            let (a, b) = at("both");
            n.toggle_decoration(a, b, false);
            n.toggle_decoration(a, b, true);
            let (a, b) = at("big blue");
            n.style_range(a, b, |st| (st.size, st.fill, st.underline, st.strike) = (40.0, blue, true, true));
            let (a, b) = at("bold and regular");
            n.toggle_decoration(a, b, false);
            let (a, b) = at("bold");
            n.style_range(a, b, |st| st.weight = 700);
        });
        // The whole node, in another colour, over two lines of auto-width text aligned right.
        text(self, "Whole node", "All of this text is underlined\nand struck through", (40.0, 110.0, 0.0), 20.0, &|n| {
            (n.underline, n.strike, n.fill, n.align) = (true, true, Some(red), 2);
        });

        let lorem = "Justified text stretches every line to the width of its box by sharing the room that is left among the spaces between the words. The last line of a paragraph stays as it is.\nA second paragraph starts here, and its lines are stretched in the same way until the last one.";
        ground(self, (40.0, 190.0, 420.0, 230.0));
        let justified = text(self, "Justified", lorem, (40.0, 190.0, 420.0), 17.0, &|n| {
            n.align = 3;
            let a = lorem.find("every line").map_or(0, |i| lorem[..i].chars().count());
            n.toggle_decoration(a, a + 30, false);
        });
        ground(self, (520.0, 190.0, 440.0, 230.0));
        text(self, "Left", lorem, (520.0, 190.0, 440.0), 17.0, &|_| {});

        let items = "Milk and two eggs\nA long item that does not fit the width of its box, so it wraps and hangs under its own text\n\nBread";
        ground(self, (40.0, 450.0, 300.0, 200.0));
        let bullets = text(self, "Bullets", items, (40.0, 450.0, 300.0), 17.0, &|n| n.list = 1);
        ground(self, (370.0, 450.0, 300.0, 200.0));
        let numbers = text(self, "Numbers", items, (370.0, 450.0, 300.0), 17.0, &|n| {
            (n.list, n.align) = (2, 3);
            n.style_range(0, 4, |st| (st.weight, st.fill, st.size) = (700, red, 24.0));
        });
        // Auto width, twelve items: the numbers get wider and the box grows by the indent.
        let twelve: Vec<String> = (1..=12).map(|i| format!("Item {i}")).collect();
        ground(self, (700.0, 450.0, 110.0, 250.0));
        text(self, "Twelve", &twelve.join("\n"), (700.0, 450.0, 0.0), 14.0, &|n| n.list = 2);
        text(self, "Centred list", "Centred\nlist of three\nitems", (830.0, 450.0, 150.0), 15.0, &|n| (n.list, n.align) = (1, 1));

        for (i, name) in ["Top", "Middle", "Bottom"].into_iter().enumerate() {
            let nx = 40.0 + i as f32 * 200.0;
            ground(self, (nx, 690.0, 180.0, 120.0));
            text(self, name, &format!("{name}: two lines\nin a fixed box"), (nx, 690.0, 180.0), 16.0, &|n| {
                (n.fixed_h, n.h, n.valign, n.align) = (true, 120.0, i as u8, i as u8);
                n.toggle_decoration(0, name.chars().count(), false);
            });
        }
        // Lower than its lines, aligned to the bottom: they run out at the top.
        ground(self, (40.0, 850.0, 180.0, 20.0));
        text(self, "Overflow", "Three lines\nin a box\nof 20", (40.0, 850.0, 180.0), 14.0, &|n| (n.fixed_h, n.h, n.valign) = (true, 20.0, 2));

        // Above the size where glyphs come from the atlas: outlines, with vector lines and markers.
        let large = text(self, "Large", "Big list\nstruck", (650.0, 690.0, 0.0), 0.0, &|n| {
            (n.font_size, n.list, n.weight, n.w, n.h) = (60.0, 1, 700, 340.0, 145.0);
            n.toggle_decoration(4, 8, false);
            n.toggle_decoration(9, 15, true);
            n.style_range(9, 15, |st| st.fill = blue);
        });
        self.zoom_to((x - 30.0, y - 40.0, x + 1030.0, y + 930.0), 1.0);
        self.fit_pending = false;
        self.expanded.insert(stage);
        self.sel = vec![justified];
        self.tool = Tool::Select;
        vec![stage, justified, bullets, numbers, large]
    }

    /// `KANVAS_BENCH_NESTED`: a frame holding a frame holding a frame, each with something sticking
    /// out of it (to show the clipping), opened in the Layers panel down to the deepest one.
    /// Result: ids of [outer frame, card, badge, stripe in the badge, loose rect in the outer frame].
    pub(super) fn bench_nested(&mut self) -> Vec<u64> {
        let (x, y) = (-1500.0, 1000.0);
        let mut make = |kind: Kind, name: &str, (nx, ny, w, h): (f32, f32, f32, f32), fill: [u8; 4]| {
            let mut n = Node::new(self.doc.alloc_id(), kind, x + nx, y + ny);
            (n.name, n.w, n.h, n.fill) = (name.into(), w, h, Some(fill));
            n
        };
        let mut outer = make(Kind::Frame, "Nested demo", (0.0, 0.0, 900.0, 600.0), [250, 250, 250, 255]);
        let mut card = make(Kind::Frame, "Card", (80.0, 80.0, 420.0, 360.0), [222, 236, 255, 255]);
        (card.stroke, card.stroke_w) = (Some([13, 153, 255, 255]), 3.0);
        card.shadow = Some(crate::doc::Shadow { x: 0.0, y: 12.0, blur: 28.0, color: [0, 0, 0, 90] });
        // Sticks out of the card at the top and on the right.
        let circle = make(Kind::Ellipse, "Circle", (380.0, 0.0, 220.0, 220.0), [151, 71, 255, 255]);
        let mut title = make(Kind::Text, "Title", (110.0, 110.0, 200.0, 40.0), [20, 40, 90, 255]);
        (title.text, title.font_size, title.weight) = ("Card title".into(), 34.0, 700);
        let mut badge = make(Kind::Frame, "Badge", (120.0, 270.0, 240.0, 130.0), [255, 196, 87, 255]);
        badge.opacity = 0.95;
        // Wider than the badge: cut off on both sides.
        let stripe = make(Kind::Rect, "Stripe", (60.0, 320.0, 360.0, 36.0), [242, 72, 72, 255]);
        let mut label = make(Kind::Text, "Label", (136.0, 282.0, 100.0, 24.0), [90, 50, 0, 255]);
        (label.text, label.font_size) = ("Badge".into(), 20.0);
        // Sticks out of the outer frame.
        let corner = make(Kind::Ellipse, "Corner", (760.0, 460.0, 260.0, 260.0), [20, 174, 92, 255]);
        let loose = make(Kind::Rect, "Loose", (620.0, 120.0, 160.0, 160.0), [255, 102, 196, 255]);
        let ids = vec![outer.id, card.id, badge.id, stripe.id, loose.id];
        badge.children = vec![stripe, label];
        card.children = vec![circle, title, badge];
        outer.children = vec![card, corner, loose];
        let b = outer.bounds();
        self.doc.insert(outer, None);
        self.expanded.extend(&ids[..3]);
        self.zoom_to((b.0 - 40.0, b.1 - 40.0, b.2 + 40.0, b.3 + 40.0), 1.0);
        self.sel.clear();
        ids
    }

    /// `KANVAS_BENCH_DRAGKEYS`: the rotated box of the demo is dragged, and in the middle of the
    /// drag a key is pressed: `esc`, `del`, `undo` (Ctrl+Z) or `cut` (Ctrl+X). The drag goes on
    /// for a few frames before the button is released. The outcome is printed to stderr.
    /// `zoom`: no drag; Ctrl+= twice, Ctrl+-, Ctrl+0 (the canvas zooms, the interface does not),
    /// then Ctrl+Shift+A, Ctrl+Shift+D and Ctrl+Alt+S (which are not Select all, Duplicate, Save).
    pub(super) fn bench_dragkeys(&mut self, mode: &str, frame: u32, inject: &mut Vec<(u32, egui::Event)>, ctx: &egui::Context) {
        if mode == "zoom" {
            let key = |at: u32, key: Key, modifiers: Modifiers| {
                let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
                [(at, event(true)), (at + 1, event(false))]
            };
            let cmd = Modifiers::COMMAND;
            match frame {
                200 => {
                    self.sel.clear();
                    inject.extend(key(204, Key::Equals, cmd).into_iter().chain(key(208, Key::Equals, cmd)).chain(key(212, Key::Minus, cmd)).chain(key(216, Key::Num0, cmd)));
                    inject.extend(key(220, Key::A, cmd | Modifiers::SHIFT).into_iter().chain(key(224, Key::D, cmd | Modifiers::SHIFT)).chain(key(228, Key::S, cmd | Modifiers::ALT)));
                }
                203 | 207 | 211 | 215 | 219 | 223 | 227 | 232 => {
                    eprintln!("DRAGKEYS zoom frame {frame}: canvas {:.3}, interface {:.3}, {} selected, {} layers, status: {}", self.view.zoom, ctx.zoom_factor(), self.sel.len(), self.doc.count(), self.status);
                }
                _ => {}
            }
            return;
        }
        let node = self.doc.nodes.iter().flat_map(|n| n.children.iter()).find(|c| c.name == "Rotated").map(|c| (c.id, c.center()));
        match frame {
            200 => {
                let Some((_, c)) = node else { return };
                let from = self.view.to_screen(self.canvas_rect.min, c.0, c.1);
                let to = from + vec2(-120.0, -60.0);
                let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                let mv = egui::Event::PointerMoved;
                inject.extend([(202, mv(from)), (204, button(from, true))]);
                inject.extend((1..=8u32).map(|s| (204 + s * 2, mv(from + (to - from) * (s as f32 / 8.0)))));
                inject.push((224, button(to, false)));
                let key = |at: u32, key: Key, modifiers: Modifiers| {
                    let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
                    [(at, event(true)), (at + 1, event(false))]
                };
                match mode {
                    "del" => inject.extend(key(213, Key::Delete, Modifiers::NONE)),
                    "undo" => inject.extend(key(213, Key::Z, Modifiers::COMMAND)),
                    "cut" => inject.push((213, egui::Event::Cut)),
                    _ => inject.extend(key(213, Key::Escape, Modifiers::NONE)),
                }
                eprintln!("DRAGKEYS {mode}: box at {c:?} before the drag, {} undo steps", self.undo.len());
            }
            212 | 216 | 230 => {
                eprintln!(
                    "DRAGKEYS frame {frame}: box at {:?}, dragging {}, selection {:?}, {} undo steps",
                    node.map(|n| n.1),
                    !matches!(self.drag, Drag::None),
                    self.sel,
                    self.undo.len()
                );
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ gambar

    /// Tambah gambar dari byte file (PNG, JPEG, WebP, GIF, BMP). `at` = posisi layar, default tengah canvas.
    pub(super) fn add_image(&mut self, bytes: Vec<u8>, name: &str, at: Option<Pos2>) -> bool {
        let Some((w, h)) = render::image_size(&bytes) else {
            self.status = format!("Couldn't read \"{name}\" as an image.");
            return false;
        };
        let screen = at.filter(|p| self.canvas_rect.contains(*p)).unwrap_or(self.canvas_rect.center());
        let c = self.view.to_world(self.canvas_rect.min, screen);
        let pre = self.doc.clone();
        let asset = self.doc.add_asset(bytes);
        let id = self.doc.alloc_id();
        let mut n = Node::new(id, Kind::Image, (c.x - w as f32 * 0.5).round(), (c.y - h as f32 * 0.5).round());
        n.name = name.to_owned();
        n.w = w as f32;
        n.h = h as f32;
        n.image = Some(asset);
        let parent = self.doc.frame_at(c.x, c.y, &[]);
        self.doc.insert_placed(n, parent);
        self.push_undo(pre);
        self.sel = vec![id];
        self.tool = Tool::Select;
        self.status = format!("Added image \"{name}\" ({w}×{h}).");
        true
    }

    /// File yang dijatuhkan ke jendela: gambar jadi node (an SVG becomes editable layers), `.kanvas` dibuka.
    pub(super) fn dropped_files(&mut self, ctx: &egui::Context) {
        let (files, pos) = ctx.input(|i| (i.raw.dropped_files.clone(), i.pointer.latest_pos()));
        if self.library_take_drop(ctx, &files, pos) {
            return;
        }
        let mut offset = 0.0;
        for file in files {
            let name = file
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| file.name.clone());
            if let Some(path) = file.path.as_ref().filter(|p| p.extension().is_some_and(|e| e == "kanvas")) {
                if self.confirm_discard() {
                    self.load(path.clone());
                }
                continue;
            }
            let bytes = match (&file.path, &file.bytes) {
                (Some(path), _) => std::fs::read(path).ok(),
                (None, Some(bytes)) => Some(bytes.to_vec()),
                _ => None,
            };
            match bytes {
                Some(bytes) => {
                    if self.place_file(bytes, &name, pos.map(|p| p + vec2(offset, offset))) {
                        offset += 24.0;
                    }
                }
                None => self.status = format!("Couldn't read \"{name}\"."),
            }
        }
    }

    /// Tempel dari clipboard sistem (menu Edit dan Ctrl+Shift+V): objek Kanvas atau teks, kalau
    /// tidak ada teks maka gambar.
    pub(super) fn paste_clipboard(&mut self, ctx: &egui::Context, in_place: bool) {
        let at = self.canvas_pointer(ctx);
        match arboard::Clipboard::new().and_then(|mut c| c.get_text()) {
            Ok(text) if !text.is_empty() => self.paste_text(&text.replace("\r\n", "\n"), in_place, at),
            _ => self.paste_image(at),
        }
    }

    fn paste_image(&mut self, at: Option<Pos2>) {
        let image = arboard::Clipboard::new().and_then(|mut c| c.get_image());
        let Ok(img) = image else { return };
        let (w, h) = (img.width as u32, img.height as u32);
        let Some(rgba) = image::RgbaImage::from_raw(w, h, img.bytes.into_owned()) else { return };
        let mut png = Vec::new();
        if rgba.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).is_ok() {
            self.add_image(png, "Pasted image", at);
        }
    }

    /// Ctrl+V dengan gambar di clipboard. egui hanya mengirim event Paste untuk teks,
    /// jadi tempel gambar dideteksi dari tombol V yang tidak menghasilkan teks.
    pub(super) fn clipboard_keys(&mut self, ctx: &egui::Context) {
        let (plain_down, cmd_down, shift_down, up, pasted) = ctx.input(|i| {
            let (mut plain, mut cmd, mut shift, mut up, mut pasted) = (false, false, false, false, false);
            for e in &i.events {
                match e {
                    egui::Event::Key { key: Key::V, pressed: true, modifiers, .. } => {
                        if modifiers.command {
                            cmd = true;
                            shift |= modifiers.shift;
                        } else {
                            plain = true;
                        }
                    }
                    egui::Event::Key { key: Key::V, pressed: false, .. } => up = true,
                    egui::Event::Paste(_) => pasted = true,
                    _ => {}
                }
            }
            (plain, cmd, shift, up, pasted)
        });
        let free = !ctx.wants_keyboard_input() && self.editing.is_none() && !self.dragging();
        self.ext.v_plain |= plain_down;
        self.ext.paste_seen |= pasted;
        let at = self.canvas_pointer(ctx);
        if cmd_down && !self.ext.paste_seen {
            // Ctrl+Shift+V yang tidak menghasilkan event Paste (tergantung versi egui): Paste in Place.
            if free && shift_down {
                self.paste_clipboard(ctx, true);
            } else if free {
                self.paste_image(at);
            }
            self.ext.paste_seen = true;
        }
        if up {
            let plain = std::mem::take(&mut self.ext.v_plain);
            let seen = std::mem::take(&mut self.ext.paste_seen);
            // Ctrl+V ditelan egui saat ditekan (tidak ada event tekan), jadi dikenali saat dilepas.
            if !plain && !seen && free {
                self.paste_image(at);
            }
        }
    }
}

/// Rentang baris (di antara dua ganti baris) yang memuat posisi `at`, untuk klik tiga kali.
fn line_at(text: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    let at = at.min(chars.len());
    let a = chars[..at].iter().rposition(|c| *c == '\n').map_or(0, |i| i + 1);
    let b = chars[at..].iter().position(|c| *c == '\n').map_or(chars.len(), |i| at + i);
    (a, b)
}

/// Rentang kata (indeks karakter) di sekitar posisi `at`, untuk klik dua kali.
fn word_at(text: &str, at: usize) -> (usize, usize) {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return (0, 0);
    }
    let is_word = |c: char| c.is_alphanumeric() || c == '_' || c == '\'';
    let mut i = at.min(chars.len() - 1);
    // Klik tepat setelah sebuah kata: tetap pilih kata itu.
    if i > 0 && !is_word(chars[i]) && is_word(chars[i - 1]) {
        i -= 1;
    }
    let kind = is_word(chars[i]);
    let same = |c: char| c != '\n' && is_word(c) == kind;
    let (mut a, mut b) = (i, i + 1);
    while a > 0 && same(chars[a - 1]) {
        a -= 1;
    }
    while b < chars.len() && same(chars[b]) {
        b += 1;
    }
    (a, b)
}

/// How many contours of paths are outlined for one selection, all its paths together; the
/// paths after that get their box.
const OUTLINE_CONTOURS: usize = 240;

/// The outline of `n` as it is selected or under the pointer: its shape for a line or a path,
/// its box for anything else. `budget`: how many contours of paths may still be outlined
/// (see `OUTLINE_CONTOURS`); what `n` uses of it is taken off.
fn outline(shapes: &mut Vec<egui::Shape>, view: View, o: Pos2, n: &Node, width: f32, budget: &mut usize) {
    let accent = if n.is_linked() || n.source.is_some() { components::PURPLE } else { ACCENT };
    let (cx, cy) = n.center();
    // Titik dunia → layar, mengikuti rotasi node.
    let s = |x: f32, y: f32| {
        let (x, y) = if n.rotation != 0.0 { crate::doc::rotate(x, y, cx, cy, n.rotation) } else { (x, y) };
        view.to_screen(o, x, y)
    };
    let contours = 1 + n.holes.len();
    match n.kind {
        Kind::Line => {
            shapes.push(egui::Shape::line_segment([s(n.x, n.y), s(n.x + n.w, n.y + n.h)], Stroke::new(width + 0.5, accent)));
        }
        // (A path of very many contours, such as a paragraph of outlined text, gets its box
        // like other nodes: thousands of little outlines are slow to draw and say nothing.
        // The same goes for many such paths selected together.)
        Kind::Path if contours <= 61 && contours <= *budget => {
            *budget -= contours;
            let stroke = Stroke::new(width, accent);
            // (Each curve by its own size: outlined text is hundreds of small ones.)
            for (c, ring) in n.contours_within(0.25 / view.zoom.max(1e-6), 16).into_iter().enumerate() {
                let pts: Vec<Pos2> = ring.into_iter().map(|(x, y)| s(x, y)).collect();
                shapes.push(if n.closed || c > 0 { egui::Shape::closed_line(pts, stroke) } else { egui::Shape::line(pts, stroke) });
            }
        }
        _ => {
            let (x0, y0, x1, y1) = n.bounds();
            let corners = vec![s(x0, y0), s(x1, y0), s(x1, y1), s(x0, y1)];
            shapes.push(egui::Shape::closed_line(corners, Stroke::new(width, accent)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{line_at, word_at};

    #[test]
    fn triple_click_selects_the_line() {
        let t = "Penyerahan\nBantuan SMP5";
        assert_eq!(line_at(t, 0), (0, 10));
        assert_eq!(line_at(t, 10), (0, 10));
        assert_eq!(line_at(t, 11), (11, 23));
        assert_eq!(line_at(t, 23), (11, 23));
        assert_eq!(line_at("", 0), (0, 0));
    }

    #[test]
    fn double_click_selects_the_word() {
        let t = "Penyerahan Bantuan SMP5\nbaris dua";
        assert_eq!(word_at(t, 3), (0, 10));
        // Tepat setelah kata: tetap kata itu.
        assert_eq!(word_at(t, 10), (0, 10));
        assert_eq!(word_at(t, 12), (11, 18));
        assert_eq!(word_at(t, 21), (19, 23));
        // Tidak menyeberang baris.
        assert_eq!(word_at(t, 25), (24, 29));
        assert_eq!(word_at("", 0), (0, 0));
        assert_eq!(word_at("a  b", 2), (1, 3));
    }
}
