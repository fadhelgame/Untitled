//! Text on a path in the app: attaching and detaching, keeping the box of an attached text
//! around its glyphs, sliding it along the path when it is dragged, and Outline text.

use super::render::{PathBox, PathSeen};
use super::App;
use crate::doc::{Kind, Node};
use crate::textpath::{self, can_carry, guide_of};
use eframe::egui;
use std::collections::HashMap;

impl App {
    /// Copies were given ids of their own (`ids`: old to new) and are selected: a text among
    /// them whose path was copied too is on the copy of the path.
    pub(super) fn relink_paths(&mut self, ids: &HashMap<u64, u64>) {
        for id in self.sel.clone() {
            if self.doc.find(id).is_some_and(|n| n.any(&|c| c.on_path.is_some_and(|p| ids.contains_key(&p)))) {
                if let Some(n) = self.doc.find_mut(id) {
                    textpath::relink(n, ids);
                }
            }
        }
    }

    /// The line the text `n` follows, if it is on a path.
    fn guide_for(&self, n: &Node) -> Option<textpath::Guide> {
        n.on_path.and_then(|p| self.doc.find(p)).filter(|p| can_carry(p)).and_then(guide_of)
    }

    /// Give a text on a path the box around its glyphs right away (the canvas does the same
    /// when it paints the text, but only once it is in view).
    pub(super) fn fit_path_text(&mut self, ctx: &egui::Context, id: u64) -> bool {
        let Some(n) = self.doc.find(id).filter(|n| n.kind == Kind::Text).cloned() else { return false };
        let Some(guide) = self.guide_for(&n) else { return false };
        let Some(b) = textpath::laid(ctx, &n, &guide, &mut self.fonts).bbox else { return false };
        if let Some(n) = self.doc.find_mut(id) {
            (n.x, n.y, n.w, n.h) = (b.0, b.1, (b.2 - b.0).max(1.0), (b.3 - b.1).max(1.0));
            (n.rotation, n.flip_h, n.flip_v) = (0.0, false, false);
        }
        self.ext.path_seen.remove(&id);
        true
    }

    /// The canvas laid a text out on its path: its box becomes the box around its glyphs, so
    /// selecting, snapping, culling and export bounds work as for any node. If the node is not
    /// where the canvas last put it although neither the path nor the offset changed, it was
    /// moved by hand (dragged, nudged, aligned): that slides the text along its path, by as much
    /// as the move goes in the direction of the path.
    pub(super) fn apply_path_box(&mut self, b: PathBox) {
        let Some(n) = self.doc.find(b.id) else { return };
        let (x, y, w, h) = (b.bbox.0, b.bbox.1, (b.bbox.2 - b.bbox.0).max(1.0), (b.bbox.3 - b.bbox.1).max(1.0));
        let slide = match self.ext.path_seen.get(&b.id) {
            // (The parts of an instance stay as the main has them.)
            Some(s) if s.guide == b.guide && s.offset == n.path_offset.to_bits() && n.source.is_none() => {
                let (dx, dy) = (n.x - s.at.0, n.y - s.at.1);
                (dx != 0.0 || dy != 0.0).then(|| dx * b.tan.0 + dy * b.tan.1)
            }
            _ => None,
        };
        let near = |a: f32, b: f32| (a - b).abs() <= 0.01;
        let same = near(n.x, x) && near(n.y, y) && near(n.w, w) && near(n.h, h) && n.rotation == 0.0 && !n.flip_h && !n.flip_v;
        if slide.is_none() && same {
            self.ext.path_seen.insert(b.id, PathSeen { at: (n.x, n.y), guide: b.guide, offset: n.path_offset.to_bits() });
            return;
        }
        if let Some(n) = self.doc.find_mut(b.id) {
            n.path_offset += slide.unwrap_or(0.0);
            (n.x, n.y, n.w, n.h) = (x, y, w, h);
            (n.rotation, n.flip_h, n.flip_v) = (0.0, false, false);
            let seen = PathSeen { at: (x, y), guide: b.guide, offset: n.path_offset.to_bits() };
            self.ext.path_seen.insert(b.id, seen);
        }
        if self.ext.path_seen.len() > 8192 {
            self.ext.path_seen.clear();
        }
        // Not an edit, but the instances of a main with this text get its box too.
        self.components.recheck();
    }

    /// For the Text tool: the shape whose outline is within `tol` of `p` (the nearest, if
    /// several are), and how far along it that point is. Only shapes that show a stroke and
    /// have no fill count: a click on a filled shape means text on top of it.
    pub(super) fn outline_under(&self, p: (f32, f32), tol: f32) -> Option<(u64, f32)> {
        let mut best: Option<(f32, u64, f32)> = None;
        self.doc.walk(|n| {
            let shown = n.visible && !n.locked && n.source.is_none() && n.stroke.is_some_and(|c| c[3] > 0) && n.stroke_w > 0.0;
            if !shown || !can_carry(n) || (n.fill.is_some() && n.kind != Kind::Line) {
                return;
            }
            let b = n.aabb();
            if p.0 < b.0 - tol || p.0 > b.2 + tol || p.1 < b.1 - tol || p.1 > b.3 + tol {
                return;
            }
            if let Some((far, along)) = guide_of(n).map(|g| g.nearest(p)) {
                if far <= tol && best.is_none_or(|b| far < b.0) {
                    best = Some((far, n.id, along));
                }
            }
        });
        best.map(|b| (b.1, b.2))
    }

    /// The selected texts that are on a path.
    pub(super) fn sel_on_path(&self) -> Vec<u64> {
        self.sel.iter().copied().filter(|id| self.doc.find(*id).is_some_and(|n| n.kind == Kind::Text && n.on_path.is_some())).collect()
    }

    /// Ctrl+Alt+P. With one text and one shape selected: the text goes on the shape. With only
    /// texts that are on a path selected: they come off it.
    pub(super) fn text_on_path(&mut self, ctx: &egui::Context) {
        if self.sel_locked() || self.refuse_pinned() {
            return;
        }
        let attached = self.sel_on_path();
        if !attached.is_empty() && attached.len() == self.sel.len() {
            return self.detach_from_path();
        }
        let kind = |id: &u64| self.doc.find(*id).map(|n| (n.kind == Kind::Text, can_carry(n)));
        let texts: Vec<u64> = self.sel.iter().copied().filter(|id| kind(id) == Some((true, false))).collect();
        let shapes: Vec<u64> = self.sel.iter().copied().filter(|id| kind(id) == Some((false, true))).collect();
        let (&[text], &[shape], 2) = (&texts[..], &shapes[..], self.sel.len()) else {
            self.status = "Text on path: select one text and one shape (a vector path, line, ellipse or rectangle).".into();
            return;
        };
        self.finish_text_edit();
        let pre = self.doc.clone();
        match self.doc.attach_text(text, shape) {
            Ok(()) => {
                self.fit_path_text(ctx, text);
                self.push_undo(pre);
                self.sel = vec![text];
                self.status = "Text is on the path: it follows the shape when that changes. Drag the text to slide it along; Offset and Flip side are in the panel; Ctrl+Alt+P takes it off again.".into();
            }
            Err(why) => self.status = format!("Text on path: {why}."),
        }
    }

    /// The selected texts come off their paths and are plain text again, where they were.
    pub(super) fn detach_from_path(&mut self) {
        let ids = self.sel_on_path();
        if ids.is_empty() || self.sel_locked() || self.refuse_pinned() {
            return;
        }
        let pre = self.doc.clone();
        for id in &ids {
            if let Some(n) = self.doc.find_mut(*id) {
                textpath::detach(n);
            }
            self.ext.path_seen.remove(id);
        }
        self.push_undo(pre);
        self.status = "Detached from the path: plain text again.".into();
    }

    /// Outline text: the text node becomes filled paths (see `Doc::text_to_paths`). Result: the
    /// ids of the paths; empty when the text has nothing to draw.
    pub(super) fn text_outline(&mut self, ctx: &egui::Context, id: u64) -> Vec<u64> {
        let Some(n) = self.doc.find(id).filter(|n| n.kind == Kind::Text).cloned() else { return Vec::new() };
        let guide = self.guide_for(&n);
        let parts = textpath::text_parts(ctx, &n, guide.as_ref(), &mut self.fonts);
        self.ext.path_seen.remove(&id);
        self.doc.text_to_paths(id, parts)
    }
}

// ---------------------------------------------------------------- bench

impl App {
    /// `--bench` with `KANVAS_BENCH_TEXTPATH`: on the left half of a frame, texts on a wavy open
    /// path, around a circle (centred), right-aligned on an arc, on the inside of a circle
    /// (flipped side), with mixed styles and lines under and through, around a turned rounded
    /// rectangle and on a line with an offset. The right half holds the same again; those
    /// texts are outlined once the fonts are there (`bench_textpath_finish`), and the export
    /// of the frame is then placed beside it. Result: [frame, the texts to outline..].
    /// `=view:x0,y0,x1,y1` shows that part of the stage (the export starts at x 1040).
    /// `=select`: the text on the circle is selected, to show its box and the panel.
    /// `=checks`: see `bench_textpath_checks`.
    pub(super) fn bench_textpath(&mut self, ctx: &egui::Context, mode: &str) -> Vec<u64> {
        use crate::doc::{PathPoint, Span, Style};
        let (x, y) = (-3000.0, 26000.0);
        let stage = self.doc.alloc_id();
        let mut n = Node::new(stage, Kind::Frame, x, y);
        (n.name, n.w, n.h, n.fill) = ("Textpath demo".into(), 1000.0, 760.0, Some([252, 252, 250, 255]));
        self.doc.insert(n, None);
        let (ink, blue, red, rail) = ([30, 30, 40, 255], [13, 110, 220, 255], [220, 50, 60, 255], [150, 190, 235, 255]);
        let curve = |px: f32, py: f32, hx: f32, hy: f32| PathPoint { x: px, y: py, hin: [-hx, -hy], hout: [hx, hy], r: None };
        let (mut out, mut checks, mut picked) = (vec![stage], Vec::new(), Vec::new());
        for (half, dx) in [0.0, 500.0].into_iter().enumerate() {
            let shape = |app: &mut Self, kind: Kind, (nx, ny, w, h): (f32, f32, f32, f32), edit: &dyn Fn(&mut Node)| {
                let mut n = Node::new(app.doc.alloc_id(), kind, x + dx + nx, y + ny);
                (n.w, n.h, n.stroke, n.stroke_w) = (w, h, Some(rail), 1.5);
                edit(&mut n);
                let id = n.id;
                app.doc.insert(n, Some(stage));
                id
            };
            let text = |app: &mut Self, on: u64, s: &str, size: f32, edit: &dyn Fn(&mut Node)| {
                let id = app.doc.alloc_id();
                let mut n = Node::new(id, Kind::Text, x + dx + 10.0, y + 10.0);
                (n.name, n.text, n.font_size, n.fill, n.w, n.h) = (s.chars().take(12).collect(), s.into(), size, Some(ink), 100.0, size * 1.2);
                app.doc.insert(n, Some(stage));
                let _ = app.doc.attach_text(id, on);
                if let Some(n) = app.doc.find_mut(id) {
                    edit(n);
                }
                app.fit_path_text(ctx, id);
                id
            };
            let wave = shape(self, Kind::Path, (30.0, 50.0, 420.0, 110.0), &|n| n.points = vec![curve(0.0, 0.5, 0.15, -0.6), curve(0.5, 0.5, 0.15, 0.6), curve(1.0, 0.5, 0.15, -0.6)]);
            let circle = shape(self, Kind::Ellipse, (40.0, 220.0, 180.0, 180.0), &|_| {});
            let arc = shape(self, Kind::Path, (270.0, 230.0, 180.0, 150.0), &|n| {
                n.points = vec![PathPoint { x: 0.0, y: 1.0, hin: [0.0; 2], hout: [0.0, -0.55], r: None }, PathPoint { x: 1.0, y: 0.0, hin: [-0.55, 0.0], hout: [0.0; 2], r: None }];
            });
            let inside = shape(self, Kind::Ellipse, (40.0, 450.0, 180.0, 180.0), &|_| {});
            let hill = shape(self, Kind::Path, (250.0, 440.0, 220.0, 70.0), &|n| n.points = vec![curve(0.0, 1.0, 0.2, -0.9), curve(1.0, 1.0, 0.2, 0.9)]);
            let card = shape(self, Kind::Rect, (285.0, 560.0, 160.0, 100.0), &|n| (n.radius, n.rotation) = (26.0, 15.0));
            let line = shape(self, Kind::Line, (30.0, 735.0, 430.0, -22.0), &|_| {});
            let texts = [
                text(self, wave, "Text follows a wavy open path, glyph by glyph", 21.0, &|_| {}),
                text(self, circle, "CENTRED ON THE TOP OF A CIRCLE", 17.0, &|n| (n.align, n.letter_spacing, n.weight) = (1, 12.0, 700)),
                text(self, arc, "right aligned on an arc", 20.0, &|n| (n.align, n.fill) = (2, Some(blue))),
                // Half way round, so it reads upright along the bottom.
                text(self, inside, "flipped side: inside the circle", 16.0, &|n| (n.align, n.path_side, n.path_offset) = (1, true, 90.0 * std::f32::consts::PI)),
                text(self, hill, "Mixed sizes, bold, under and through", 15.0, &|n| {
                    let st = |weight: u16, size: f32, fill: [u8; 4], underline: bool, strike: bool| Style { font: n.font.clone(), weight, italic: false, size, fill, underline, strike };
                    n.spans = vec![
                        Span { start: 0, end: 5, style: st(700, 24.0, red, false, false) },
                        Span { start: 13, end: 17, style: st(700, 15.0, ink, false, false) },
                        Span { start: 19, end: 24, style: st(400, 15.0, blue, true, false) },
                        Span { start: 29, end: 36, style: st(400, 18.0, ink, false, true) },
                    ];
                }),
                text(self, card, "around a turned rounded rectangle and on around it", 12.0, &|_| {}),
                text(self, line, "on a line, moved along by 60", 15.0, &|n| n.path_offset = 60.0),
            ];
            if half == 1 {
                out.extend(texts);
            }
            if half == 0 && mode == "select" {
                picked = vec![texts[1]];
            }
            if half == 0 && mode == "checks" {
                checks = vec![texts[0], wave, texts[2], arc, line];
            }
        }
        out.extend(checks);
        let part: Vec<f32> = mode.strip_prefix("view:").map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
        let view = if part.len() == 4 { (x + part[0], y + part[1], x + part[2], y + part[3]) } else { (x - 20.0, y - 20.0, x + 2060.0, y + 780.0) };
        self.zoom_to(view, 32.0);
        self.fit_pending = false;
        self.sel = picked;
        out
    }

    /// The second half of `bench_textpath`, once the fonts are loaded: the texts `ids` become
    /// outlines, and the frame is exported and placed beside itself.
    pub(super) fn bench_textpath_finish(&mut self, ctx: &egui::Context, stage: u64, ids: &[u64], out: &std::path::Path) {
        let start = std::time::Instant::now();
        let pre = self.doc.clone();
        let made: Vec<Vec<u64>> = ids.iter().map(|id| self.text_outline(ctx, *id)).collect();
        self.push_undo(pre);
        let shape = |id: &u64| self.doc.find(*id).map(|n| (n.points.len(), n.holes.len()));
        let shapes: Vec<Vec<(usize, usize)>> = made.iter().map(|m| m.iter().filter_map(shape).collect()).collect();
        eprintln!("TEXTPATH outlined {} texts in {:.1} ms: (points of the first contour, further contours) per path {shapes:?}", ids.len(), start.elapsed().as_secs_f32() * 1000.0);
        let start = std::time::Instant::now();
        let Some((x, y, w, h)) = self.doc.find(stage).map(|f| (f.x, f.y, f.w, f.h)) else { return };
        if let Some((svg, sw, sh)) = crate::export::to_svg(&self.doc, Some(stage), &mut self.fonts, ctx) {
            let _ = crate::export::save_raster(&svg, sw, sh, 2.0, false, &out.with_file_name("bench-textpath-export.png"));
            let _ = std::fs::write(out.with_file_name("bench-textpath-export.svg"), &svg);
            if let Ok((png, _, _)) = crate::export::render_png(&svg, sw, sh, 3.0) {
                let asset = self.doc.add_asset(png);
                let mut n = Node::new(self.doc.alloc_id(), Kind::Image, x + w + 40.0, y);
                (n.name, n.w, n.h, n.image, n.stroke_w) = ("Textpath export".into(), w, h, Some(asset), 0.0);
                self.doc.insert(n, None);
            }
        }
        eprintln!("TEXTPATH exported in {:.1} ms", start.elapsed().as_secs_f32() * 1000.0);
    }

    /// `KANVAS_BENCH_TEXTPATH=checks`, frame by frame from `since`: `ids` = [text on the wave,
    /// the wave, text on the arc, the arc, the line]. The first text is moved by hand (it must
    /// slide along its path), the wave is moved (the text must follow), text and path are
    /// duplicated together (the copy must be on the copy), and the arc is deleted (its text
    /// must come off it; undo must bring both back). Then the text on the wave is edited, a
    /// text is made with a click of the Text tool on the line, and the remote commands are
    /// tried. The outcome is printed to stderr.
    pub(super) fn bench_textpath_checks(&mut self, frame: u32, since: u32, ids: &[u64], inject: &mut Vec<(u32, egui::Event)>) {
        let (&[text, wave, other, arc, line], true) = (ids, frame >= since) else { return };
        let look = |app: &Self, id: u64| app.doc.find(id).map(|n| (n.on_path, n.path_offset.round(), n.x.round(), n.y.round(), n.w.round(), n.h.round()));
        match frame - since {
            0 => {
                eprintln!("TEXTPATH text on the wave (path, offset, box): {:?}", look(self, text));
                self.sel = vec![text];
                self.nudge(10.0, 0.0);
                self.nudge(10.0, 0.0);
                self.nudge(10.0, 0.0);
            }
            4 => {
                eprintln!("TEXTPATH nudged right by 30: {:?} (want an offset near 30 and a box that moved along the wave), undo steps {}", look(self, text), self.undo.len());
                self.do_undo();
                self.do_undo();
                self.do_undo();
            }
            8 => {
                eprintln!("TEXTPATH nudges undone: {:?} (want offset 0 and the first box)", look(self, text));
                self.sel = vec![wave];
                self.nudge(0.0, 10.0);
                self.nudge(0.0, 10.0);
            }
            12 => {
                eprintln!("TEXTPATH wave moved down by 20: {:?} (want offset 0, the box 20 lower)", look(self, text));
                self.do_undo();
                self.do_undo();
                self.sel = vec![wave, text];
                let nodes = self.selected_nodes();
                self.add_nodes(nodes, egui::vec2(0.0, 30.0), None);
            }
            16 => {
                let copy: Vec<(u64, Kind, Option<u64>)> = self.sel.iter().filter_map(|id| self.doc.find(*id)).map(|n| (n.id, n.kind, n.on_path)).collect();
                let ok = copy.len() == 2 && copy.iter().any(|c| c.1 == Kind::Text && c.2.is_some_and(|p| p != wave && copy.iter().any(|d| d.0 == p && d.1 == Kind::Path)));
                eprintln!("TEXTPATH duplicated together: {copy:?}: the copy of the text is on the copy of the path: {ok}; the original is still on {:?}", self.doc.find(text).and_then(|n| n.on_path));
                self.do_undo();
                self.sel = vec![text];
                let nodes = self.selected_nodes();
                self.add_nodes(nodes, egui::vec2(0.0, 30.0), None);
            }
            20 => {
                eprintln!("TEXTPATH text duplicated alone: on {:?} (want the same path, {wave})", self.sel.first().and_then(|id| self.doc.find(*id)).and_then(|n| n.on_path));
                self.do_undo();
                self.sel = vec![arc];
                self.delete_selection();
            }
            24 => {
                eprintln!("TEXTPATH arc deleted: its text {:?} (want no path; plain text where it was)", look(self, other));
                self.do_undo();
            }
            28 => {
                eprintln!("TEXTPATH deletion undone: {:?} (want the arc again, {arc})", look(self, other));
                // Editing: the usual editor, with the text straight at the place of its box.
                self.sel = vec![text];
                self.start_text_edit(text, self.doc.clone());
                inject.push((frame + 4, egui::Event::Text("XYZ ".into())));
            }
            38 => self.finish_text_edit(),
            42 => {
                eprintln!("TEXTPATH edited: {:?} {:?} (want the text with XYZ in it, still on the wave, a box along it)", self.doc.find(text).map(|n| n.text.clone()), look(self, text));
                self.do_undo();
                // The Text tool, clicked on the line a third of the way along it.
                let Some((a, d)) = self.doc.find(line).map(|n| ((n.x, n.y), (n.w, n.h))) else { return };
                let at = self.view.to_screen(self.canvas_rect.min, a.0 + d.0 / 3.0, a.1 + d.1 / 3.0);
                self.tool = super::Tool::Text;
                let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                inject.extend([(frame + 2, egui::Event::PointerMoved(at)), (frame + 4, button(true)), (frame + 5, button(false)), (frame + 9, egui::Event::Text("clicked".into()))]);
            }
            56 => self.finish_text_edit(),
            60 => {
                let made = self.sel.first().copied().unwrap_or_default();
                let len = self.doc.find(line).map_or(0.0, |n| n.w.hypot(n.h));
                eprintln!("TEXTPATH Text tool on the line: {:?} {:?} (want it on {line}, offset near {:.0})", self.doc.find(made).map(|n| n.text.clone()), look(self, made), len / 3.0);
                self.do_undo();
                use serde_json::json;
                let run = |app: &mut Self, cmd: &str, args: serde_json::Value| {
                    let answer = app.remote_bench(cmd, &args);
                    eprintln!("TEXTPATH remote {cmd} {args} -> {}", answer.chars().take(300).collect::<String>());
                };
                run(self, "update_nodes", json!({ "nodes": [{ "id": text, "path_offset": 40, "path_side": true }] }));
                eprintln!("TEXTPATH remote: {:?} (want offset 40 and a box on the other side of the wave)", look(self, text));
                run(self, "update_nodes", json!({ "nodes": [{ "id": text, "on_path": null }] }));
                run(self, "update_nodes", json!({ "nodes": [{ "id": text, "on_path": text }] }));
                run(self, "update_nodes", json!({ "nodes": [{ "id": wave, "on_path": line }] }));
                run(self, "update_nodes", json!({ "nodes": [{ "id": text, "on_path": line, "align": "center" }] }));
                run(self, "get_document", json!({ "ids": [text] }));
                run(self, "update_nodes", json!({ "nodes": [{ "id": wave, "cap": "round", "join": "bevel" }, { "id": line, "cap": "square" }] }));
                run(self, "get_document", json!({ "ids": [wave] }));
                run(self, "update_nodes", json!({ "nodes": [{ "id": wave, "cap": "pointy" }] }));
                run(self, "outline_stroke", json!({ "ids": [text] }));
            }
            _ => {}
        }
    }
}
