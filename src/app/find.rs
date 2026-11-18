//! Find and replace in the app: the floating window (Ctrl+F / Ctrl+H), stepping through the
//! matches with the canvas following, the highlights on the canvas, and replacing as one undo
//! step. Searching and replacing themselves are in `crate::find`.

use super::*;
use crate::find::{self, Match, Options, Scope};
use std::collections::HashMap;

/// Every match in view is tinted with the first; the one stepped to with the second.
const FAINT: Color32 = Color32::from_rgba_premultiplied(96, 75, 0, 96);
const STRONG: Color32 = Color32::from_rgba_premultiplied(153, 84, 0, 153);
const STRONG_EDGE: Color32 = Color32::from_rgb(255, 122, 0);

#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
enum Where {
    #[default]
    Page,
    All,
    Selection,
}

impl Where {
    fn label(self) -> &'static str {
        match self {
            Where::Page => "This page",
            Where::All => "All pages",
            Where::Selection => "Selection",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Find,
    Replace,
}

/// What the list of matches was made from: it is made again only when one of these changes.
#[derive(Clone, PartialEq)]
struct Made {
    query: String,
    opt: Options,
    scope: Scope,
    /// `Ext::rev`: counts every committed change to the document (and undo, redo, page switch).
    rev: u64,
    /// `Find::gen`: counts the changes that are not commits.
    gen: u64,
    page: usize,
}

#[derive(Default)]
pub(super) struct Find {
    /// The Find and replace window is open.
    pub open: bool,
    query: String,
    with: String,
    opt: Options,
    scope: Where,
    /// "Selection": the layers that were selected when that scope was chosen. Stepping to a
    /// match selects its layer, so the selection itself can't be the scope.
    within: Vec<u64>,
    /// The field to put the keyboard focus in on the next frame.
    focus: Option<Field>,
    matches: Vec<Match>,
    /// For each layer with matches: where they are in `matches` (first, one past the last).
    by_node: HashMap<u64, (usize, usize)>,
    /// The match that was stepped to, as an index into `matches`.
    current: Option<usize>,
    made: Option<Made>,
    /// Where the window was on the last frame: a match under it counts as not visible.
    rect: Option<Rect>,
    /// How long the last search took.
    pub scan_ms: f32,
    /// Counts the times the document changed without a commit of its own: instances that
    /// took the new text of their main when the document was settled (`App::settle_doc`).
    /// The matches found before that are of the old texts.
    pub gen: u64,
}

impl Find {
    /// The document was replaced as a whole: search again.
    pub fn reset(&mut self) {
        self.made = None;
        self.current = None;
    }
}

/// A button with an arrow head pointing up or down.
fn arrow_button(ui: &mut egui::Ui, up: bool, enabled: bool, tip: &str) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(26.0, 26.0), if enabled { Sense::click() } else { Sense::hover() });
    let visuals = ui.visuals();
    let fill = if enabled && resp.hovered() { visuals.widgets.hovered.weak_bg_fill } else { visuals.widgets.inactive.weak_bg_fill };
    ui.painter().rect_filled(rect, CornerRadius::same(8), fill);
    let color = if enabled { Color32::from_gray(222) } else { Color32::from_gray(110) };
    let (c, d) = (rect.center(), if up { -1.0 } else { 1.0 });
    let tip_at = c + vec2(0.0, 2.5 * d);
    let stroke = Stroke::new(1.6_f32, color);
    ui.painter().line_segment([c + vec2(-4.5, -2.5 * d), tip_at], stroke);
    ui.painter().line_segment([c + vec2(4.5, -2.5 * d), tip_at], stroke);
    resp.on_hover_text(tip).clicked()
}

impl App {
    /// Ctrl+F (`replace`: Ctrl+H, with the keyboard in the Replace field).
    pub(super) fn open_find(&mut self, replace: bool) {
        self.finish_text_edit();
        let f = &mut self.find;
        if !f.open && f.scope == Where::Selection && !self.sel.is_empty() {
            f.within.clone_from(&self.sel);
        }
        f.open = true;
        f.focus = Some(if replace { Field::Replace } else { Field::Find });
    }

    fn close_find(&mut self) {
        self.find.open = false;
        self.find.focus = None;
    }

    /// Esc on the canvas while the window is open closes it. Result: it was open.
    pub(super) fn find_escape(&mut self) -> bool {
        let open = self.find.open;
        if open {
            self.close_find();
        }
        open
    }

    fn find_scope(&self) -> Scope {
        match self.find.scope {
            Where::Page => Scope::Page,
            Where::All => Scope::All,
            Where::Selection => Scope::Within(self.find.within.clone()),
        }
    }

    /// Bring the list of matches up to date. It is searched for again only when the query, an
    /// option, the scope or the document changed, not on every frame. (Typing in a text on the
    /// canvas is committed when the editor is left; until then its matches are the old ones.)
    fn find_refresh(&mut self) {
        let made = Made { query: self.find.query.clone(), opt: self.find.opt, scope: self.find_scope(), rev: self.ext.rev, gen: self.find.gen, page: self.doc.page };
        if self.find.made.as_ref() == Some(&made) {
            return;
        }
        let start = std::time::Instant::now();
        let matches = find::find_all(&self.doc, &made.query, made.opt, &made.scope);
        let f = &mut self.find;
        f.scan_ms = start.elapsed().as_secs_f32() * 1000.0;
        // The match that was stepped to stays the current one where it still exists (the
        // document changed elsewhere, or the page was switched); a new query starts over.
        let same_search = f.made.as_ref().is_some_and(|m| (&m.query, m.opt, &m.scope) == (&made.query, made.opt, &made.scope));
        let kept = f.current.filter(|_| same_search).and_then(|i| f.matches.get(i).copied());
        f.current = kept.and_then(|k| matches.iter().position(|m| (m.id, m.start) == (k.id, k.start)));
        f.by_node.clear();
        for (i, m) in matches.iter().enumerate() {
            f.by_node.entry(m.id).and_modify(|r| r.1 = i + 1).or_insert((i, i + 1));
        }
        f.matches = matches;
        f.made = Some(made);
    }

    /// Enter / Shift+Enter: to the next or the previous match, around the ends.
    fn find_step(&mut self, forward: bool) {
        self.finish_text_edit();
        self.find_refresh();
        let n = self.find.matches.len();
        if n == 0 {
            return;
        }
        let i = match (self.find.current, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        self.find_show(i);
    }

    /// Make match `i` the current one: go to its page, select its layer, open the frames
    /// around it in the Layers panel and bring it into view.
    fn find_show(&mut self, i: usize) {
        let Some(m) = self.find.matches.get(i).copied() else { return };
        if m.page != self.doc.page {
            self.go_page(m.page);
            // (A page that was never opened would be fitted to its content on the next frame.)
            self.fit_pending = false;
            // The page switch counts as a change; the list is the same, in the same order.
            self.find_refresh();
        }
        self.find.current = self.find.matches.iter().position(|x| *x == m);
        self.expanded.extend(self.doc.ancestors(m.id));
        self.sel = vec![m.id];
        self.find_reveal(m.id);
    }

    /// Bring a layer into view. A layer that is in view already, and not under the window,
    /// leaves the view alone. One that fits the canvas at the current zoom is panned to: just
    /// far enough when a part of it shows already, to the middle otherwise. The view zooms out
    /// when the layer does not fit, and zooms in (to 100% at most) only when its text would be
    /// too small to read.
    fn find_reveal(&mut self, id: u64) {
        let Some(n) = self.doc.find(id) else { return };
        let (b, size) = (n.aabb(), n.max_size());
        let canvas = self.canvas_rect;
        // (The toolbar lies over the bottom of the canvas.)
        let mut room = Rect::from_min_max(canvas.min + vec2(48.0, 48.0), canvas.max - vec2(48.0, 104.0));
        // The window lies over the canvas: keep the layer below it when there is room for that.
        let window = self.find.rect.filter(|w| w.intersects(canvas));
        if let Some(w) = window.filter(|w| w.bottom() + 24.0 < canvas.center().y) {
            room.min.y = w.bottom() + 24.0;
        }
        let (bw, bh) = ((b.2 - b.0).max(1.0), (b.3 - b.1).max(1.0));
        let z = self.view.zoom;
        let fit = (room.width() / bw).min(room.height() / bh);
        let unreadable = size * z < 7.0;
        let zoom = if z > fit {
            fit
        } else if unreadable {
            fit.min(1.0).max(z)
        } else {
            z
        }
        .clamp(MIN_ZOOM, MAX_ZOOM);
        let o = canvas.min;
        let on_screen = Rect::from_min_max(self.view.to_screen(o, b.0, b.1), self.view.to_screen(o, b.2, b.3));
        let seen = canvas.shrink(8.0).contains_rect(on_screen) && !window.is_some_and(|w| w.intersects(on_screen));
        if seen && zoom == z {
            return;
        }
        if zoom == z && on_screen.intersects(canvas) {
            let into = |lo: f32, hi: f32, min: f32, max: f32| if lo < min { min - lo } else { (max - hi).min(0.0) };
            self.view.pan += vec2(into(on_screen.left(), on_screen.right(), room.left(), room.right()), into(on_screen.top(), on_screen.bottom(), room.top(), room.bottom()));
            return;
        }
        let centre = room.center() - o;
        self.view.zoom = zoom;
        self.view.pan = vec2(centre.x - (b.0 + b.2) * 0.5 * zoom, centre.y - (b.1 + b.3) * 0.5 * zoom);
    }

    /// Measure the texts that were changed, on whatever page they are, as the canvas would when
    /// it draws them: an auto-width text gets its new width, a paragraph its new height.
    pub(super) fn find_measure(&mut self, ctx: &egui::Context, ids: &[u64]) {
        let mut sizes: HashMap<u64, (f32, f32)> = HashMap::with_capacity(ids.len());
        for id in ids {
            let Some(n) = self.doc.find_all(*id).filter(|n| n.kind == Kind::Text && !n.text.is_empty()) else { continue };
            let f = (200.0 / n.max_size()).min(1.0);
            let layout = render::layout_text(ctx, n, &n.text, &mut self.fonts, f, 1.0, true);
            sizes.insert(*id, (if n.wrap { n.w } else { (layout.width / f).max(2.0) }, (layout.height / f).max(1.0)));
        }
        self.doc.update_nodes(
            |n| sizes.contains_key(&n.id),
            |n| {
                let (w, h) = sizes[&n.id];
                // (A height that was set by hand stays.)
                (n.w, n.h) = (w, if n.fixed_h { n.h } else { h });
            },
        );
    }

    /// Replace these matches as one undo step (the caller measures the texts afterwards, see
    /// `find_measure`). Text of a part of an instance can be overridden,
    /// so it is replaced like any other; a main component passes its new text on when the
    /// document is settled at the end of the frame, and the auto layouts around the texts are
    /// laid out again then. `query`, `opt`: what the matches were found with; one that the text
    /// no longer holds is skipped. A text left with nothing to show is removed, as when it is
    /// emptied in the text editor (not a part of an instance: that keeps its text).
    /// Result: (matches replaced, layers changed, layers removed).
    pub(super) fn replace_matches(&mut self, matches: &[Match], query: &str, opt: Options, with: &str) -> (usize, Vec<u64>, usize) {
        let pre = self.doc.clone();
        let (count, changed, removed) = find::replace_matches(&mut self.doc, matches, with, Some(&find::Needle::new(query, opt)));
        if count == 0 {
            return (0, changed, 0);
        }
        self.sel.retain(|id| !removed.contains(id));
        self.push_undo(pre);
        (count, changed, removed.len())
    }

    /// Search and replace in one go, for the remote `replace_text`.
    pub(super) fn replace_text(&mut self, query: &str, with: &str, opt: Options, all_pages: bool) -> (usize, Vec<u64>, usize) {
        let matches = find::find_all(&self.doc, query, opt, if all_pages { &Scope::All } else { &Scope::Page });
        self.replace_matches(&matches, query, opt, with)
    }

    /// "Replace": the current match is replaced and the next one shown. Without a current
    /// match the first press only goes to one, so what will be replaced is seen first.
    fn find_replace_one(&mut self, ctx: &egui::Context) {
        self.finish_text_edit();
        self.find_refresh();
        let Some((i, m)) = self.find.current.and_then(|i| Some((i, *self.find.matches.get(i)?))) else {
            return self.find_step(true);
        };
        let (query, opt, with) = (self.find.query.clone(), self.find.opt, self.find.with.clone());
        let (count, changed, removed) = self.replace_matches(&[m], &query, opt, &with);
        if count == 0 {
            // (The text of a part of an instance can't be emptied.)
            if self.doc.pinned(m.id) {
                self.status = "Not replaced: that would leave a text of an instance empty. Edit the main component, or detach the instance.".into();
            }
            return;
        }
        self.find_measure(ctx, &changed);
        self.find_refresh();
        // What was before the match is as it was. The new text may hold the query again
        // ("a" replaced with "aa"): the next match is the first one behind it.
        let behind = m.start + with.chars().count();
        let list = &self.find.matches;
        let mut next = i;
        while list.get(next).is_some_and(|x| x.id == m.id && x.start < behind) {
            next += 1;
        }
        let left = list.len();
        self.find.current = None;
        if left == 0 {
            self.status = "Replaced 1 match. That was the last one.".into();
        } else {
            self.status = format!("Replaced 1 match. {left} left.");
            self.find_show(if next < left { next } else { 0 });
        }
        if removed > 0 {
            self.status.push_str(" The text was left empty, so its layer was removed.");
        }
    }

    /// "Replace all": every match in the scope, as one undo step.
    fn find_replace_all(&mut self, ctx: &egui::Context) {
        self.finish_text_edit();
        self.find_refresh();
        let (matches, with) = (self.find.matches.clone(), self.find.with.clone());
        let (query, opt) = (self.find.query.clone(), self.find.opt);
        let (count, layers, removed) = self.replace_matches(&matches, &query, opt, &with);
        self.find_measure(ctx, &layers);
        self.find.current = None;
        let s = |n: usize| if n == 1 { "" } else { "s" };
        self.status = match count {
            0 => format!("Nothing to replace: \"{}\" was not found.", self.find.query),
            _ => format!("Replaced {count} match{} in {} layer{}.", if count == 1 { "" } else { "es" }, layers.len() + removed, s(layers.len() + removed)),
        };
        if removed > 0 {
            self.status.push_str(&format!(" {removed} layer{} left empty {} removed.", s(removed), if removed == 1 { "was" } else { "were" }));
        }
    }

    /// The Find and replace window.
    pub(super) fn find_window(&mut self, ctx: &egui::Context) {
        if !self.find.open {
            self.find.rect = None;
            return;
        }
        self.find_refresh();
        let total = self.find.matches.len();
        let counter = match (self.find.query.is_empty(), total, self.find.current) {
            (true, ..) => String::new(),
            (_, 0, _) => "No results".to_owned(),
            (_, n, Some(i)) => format!("{} of {n}", i + 1),
            (_, n, None) => format!("{n} found"),
        };
        let has_sel = !self.sel.is_empty();
        let (mut open, mut step, mut replace, mut replace_all, mut close) = (true, None, false, false, false);
        let mut scope = self.find.scope;
        let shown = egui::Window::new("Find and replace")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_pos(self.canvas_rect.right_top() + vec2(-432.0, 16.0))
            .fixed_size([400.0, 0.0])
            .show(ctx, |ui| {
                let f = &mut self.find;
                let focus = f.focus.take();
                let (enter, shift, escape) = ui.input(|i| (i.key_pressed(Key::Enter), i.modifiers.shift, i.key_pressed(Key::Escape)));
                let field_w = ui.available_width() - 192.0;
                ui.horizontal(|ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut f.query).hint_text("Find in text layers…").desired_width(field_w));
                    if focus == Some(Field::Find) {
                        edit.request_focus();
                    }
                    // Enter takes the keyboard out of a single-line field; it stays for the next Enter.
                    if edit.lost_focus() && enter {
                        step = Some(!shift);
                        edit.request_focus();
                    }
                    close |= (edit.has_focus() || edit.lost_focus()) && escape;
                    if arrow_button(ui, true, total > 0, "Previous match  (Shift+Enter)") {
                        step = Some(false);
                    }
                    if arrow_button(ui, false, total > 0, "Next match  (Enter)") {
                        step = Some(true);
                    }
                    ui.label(egui::RichText::new(&counter).color(if total == 0 { ui.visuals().weak_text_color() } else { ui.visuals().text_color() }));
                });
                ui.horizontal(|ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut f.with).hint_text("Replace with…").desired_width(field_w));
                    if focus == Some(Field::Replace) {
                        edit.request_focus();
                    }
                    if edit.lost_focus() && enter {
                        replace = true;
                        edit.request_focus();
                    }
                    close |= (edit.has_focus() || edit.lost_focus()) && escape;
                    let one = ui.add_enabled(total > 0, egui::Button::new("Replace"));
                    replace |= one.on_hover_text("Replace this match and go to the next  (Enter in this field)").clicked();
                    let all = ui.add_enabled(total > 0, egui::Button::new("Replace all"));
                    replace_all |= all.on_hover_text("Replace every match, as one undo step").clicked();
                });
                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("find_scope").width(104.0).selected_text(scope.label()).show_ui(ui, |ui| {
                        ui.selectable_value(&mut scope, Where::Page, Where::Page.label());
                        ui.selectable_value(&mut scope, Where::All, Where::All.label());
                        let selection = ui.add_enabled(has_sel || scope == Where::Selection, egui::SelectableLabel::new(scope == Where::Selection, Where::Selection.label()));
                        if selection.on_hover_text("The selected layers, and everything inside the selected frames").clicked() {
                            scope = Where::Selection;
                        }
                    });
                    ui.checkbox(&mut f.opt.match_case, "Match case");
                    ui.checkbox(&mut f.opt.whole_word, "Whole word");
                });
                if f.scope == Where::Selection {
                    let n = f.within.len();
                    ui.weak(match n {
                        0 => "Nothing was selected: select layers, then choose Selection again.".to_owned(),
                        1 => "Searching in 1 selected layer.".to_owned(),
                        _ => format!("Searching in {n} selected layers."),
                    });
                }
            });
        self.find.rect = shown.map(|s| s.response.rect);
        if scope != self.find.scope {
            self.find.scope = scope;
            if scope == Where::Selection {
                self.find.within.clone_from(&self.sel);
            }
        }
        if let Some(forward) = step {
            self.find_step(forward);
        }
        if replace_all {
            self.find_replace_all(ctx);
        } else if replace {
            self.find_replace_one(ctx);
        }
        if close || !open {
            self.close_find();
        }
    }

    /// Tint the matches on the canvas: all that are in view faintly, the current one strongly.
    /// The rectangles come from the same layout the text is drawn with, row by row, so they
    /// follow wrapping, justified lines, list indents and vertical alignment.
    pub(super) fn paint_find(&mut self, ctx: &egui::Context, painter: &egui::Painter) {
        use egui::text::CCursor;
        if !self.find.open {
            return;
        }
        self.find_refresh();
        if self.find.by_node.is_empty() {
            return;
        }
        let (o, z, view) = (self.canvas_rect.min, self.view.zoom, self.view);
        let (tl, br) = (view.to_world(o, self.canvas_rect.min), view.to_world(o, self.canvas_rect.max));
        // (The lines of a text in a box of fixed height may run out of it, with a match in them.)
        let ext = &self.ext;
        let in_view = |n: &Node| {
            let (b, spill) = (n.aabb(), ext.spill(n).unwrap_or(0.0));
            b.2 + spill >= tl.x && b.0 - spill <= br.x && b.3 + spill >= tl.y && b.1 - spill <= br.y
        };
        // The same glyph size the canvas uses on this frame, so the layouts come from its cache.
        let moving = ctx.input(|i| i.time) - self.last_zoom.1 < 0.15;
        let ppp = ctx.pixels_per_point();
        let (f, fonts, editing) = (&self.find, &mut self.fonts, self.editing);
        let current = f.current.and_then(|i| f.matches.get(i));
        let mut texts: Vec<(&Node, (usize, usize))> = Vec::new();
        for top in self.doc.nodes.iter().filter(|n| n.visible && in_view(n)) {
            top.walk(&mut |n| {
                if n.kind == Kind::Text && Some(n.id) != editing {
                    if let Some(range) = f.by_node.get(&n.id).filter(|_| in_view(n)) {
                        texts.push((n, *range));
                    }
                }
            });
        }
        for (n, (from, to)) in texts {
            let (x0, y0, x1, y1) = n.bounds();
            let r = Rect::from_min_max(view.to_screen(o, x0, y0), view.to_screen(o, x1, y1));
            let pivot = r.center();
            let (flip_h, flip_v) = n.flips();
            // As the node is drawn: mirrored about the centre of its box, then rotated.
            let place = |p: Pos2| {
                let p = pos2(if flip_h { 2.0 * pivot.x - p.x } else { p.x }, if flip_v { 2.0 * pivot.y - p.y } else { p.y });
                let (x, y) = crate::doc::rotate(p.x, p.y, pivot.x, pivot.y, n.rotation);
                pos2(x, y)
            };
            let tint = |rect: Rect, strong: bool| {
                let points = vec![place(rect.left_top()), place(rect.right_top()), place(rect.right_bottom()), place(rect.left_bottom())];
                let (fill, edge) = if strong { (STRONG, Stroke::new(1.5_f32, STRONG_EDGE)) } else { (FAINT, Stroke::NONE) };
                painter.add(egui::Shape::convex_polygon(points, fill, edge));
            };
            let found = &f.matches[from..to];
            let max = n.max_size();
            let px = max * z;
            // On a path: the glyphs are where the canvas put them along it, each turned with
            // the path, so every matched glyph is tinted where it stands. (Not laid out there:
            // the path is gone or of no length, and the text is drawn straight, as below.)
            if let Some(laid) = n.on_path.and_then(|_| ext.path_text(n.id)).filter(|l| l.bbox.is_some()) {
                for m in found {
                    let (fill, edge) = if current == Some(m) { (STRONG, Stroke::new(1.5_f32, STRONG_EDGE)) } else { (FAINT, Stroke::NONE) };
                    for g in laid.glyphs.iter().filter(|g| g.index >= m.start && g.index < m.end) {
                        let points = g.corners().map(|(x, y)| view.to_screen(o, x, y)).to_vec();
                        painter.add(egui::Shape::convex_polygon(points, fill, edge));
                    }
                }
                continue;
            }
            if px < 3.0 {
                // Drawn as a bar, too small to read: the whole layer is tinted.
                tint(r.expand(1.0), current.is_some_and(|c| c.id == n.id));
                continue;
            }
            let (lay, k) = render::glyph_scale(px, moving, ppp);
            let scale = lay / max;
            let layout = render::layout_text(ctx, n, &n.text, fonts, scale, 1.0, true);
            let origin = r.min + render::text_offset(n, &layout, scale) * k;
            let g = &layout.galley;
            for m in found {
                let (ca, cb) = (g.from_ccursor(CCursor::new(m.start)), g.from_ccursor(CCursor::new(m.end)));
                let (ra, rb) = (g.pos_from_cursor(&ca), g.pos_from_cursor(&cb));
                for row_i in ca.rcursor.row..=cb.rcursor.row {
                    let Some(row) = g.rows.get(row_i) else { continue };
                    let left = if row_i == ca.rcursor.row { ra.left() } else { row.rect.left() };
                    let right = if row_i == cb.rcursor.row { rb.left() } else { row.rect.right() };
                    if right <= left {
                        continue;
                    }
                    let rect = Rect::from_min_max(origin + vec2(left, row.rect.top()) * k, origin + vec2(right, row.rect.bottom()) * k);
                    tint(rect.expand2(vec2(1.0, 0.0)), current == Some(m));
                }
            }
        }
    }
}

// ---------------------------------------------------------------- bench

impl App {
    /// `KANVAS_BENCH_FIND`: a frame "Find demo" with the word "kanvas" 19 times in texts of
    /// every kind (styled ranges, one of them across the end of a match; a justified paragraph;
    /// a numbered list; a box of fixed height with its lines at the bottom; a rotated text; a
    /// text in a frame in a frame; a large one drawn from outlines; the label of a main
    /// component that hugs it, and of an instance of that), a hidden and a locked text that
    /// must not be found, and one more match on a second page. The window is opened with
    /// that word as the query, except for `keys`, which opens it with Ctrl+F.
    pub(super) fn bench_find(&mut self, mode: &str) {
        let (x, y) = (-1500.0, 12000.0);
        let (ink, blue, red) = ([30, 30, 40, 255], [13, 110, 220, 255], [220, 50, 60, 255]);
        // Before the demo is added: how long a search of the 500-frame document takes.
        for query in ["card", "e", "not in there"] {
            let start = std::time::Instant::now();
            let mut found = 0;
            for _ in 0..20 {
                found = find::find_all(&self.doc, query, Options::default(), &Scope::All).len();
            }
            let ms = start.elapsed().as_secs_f32() * 1000.0 / 20.0;
            eprintln!("FIND search \"{query}\" in {} layers: {found} matches in {ms:.3} ms", self.doc.count());
        }
        let frame = |app: &mut Self, name: &str, (nx, ny, w, h): (f32, f32, f32, f32), fill: [u8; 4], parent: Option<u64>| {
            let id = app.doc.alloc_id();
            let mut n = Node::new(id, Kind::Frame, x + nx, y + ny);
            (n.name, n.w, n.h, n.fill) = (name.into(), w, h, Some(fill));
            app.doc.insert(n, parent);
            id
        };
        let stage = frame(self, "Find demo", (0.0, 0.0, 1000.0, 760.0), [252, 252, 250, 255], None);
        // `w` 0: auto width.
        let text = |app: &mut Self, s: &str, (nx, ny, w): (f32, f32, f32), size: f32, parent: u64, edit: &dyn Fn(&mut Node)| {
            let id = app.doc.alloc_id();
            let mut n = Node::new(id, Kind::Text, x + nx, y + ny);
            (n.name, n.text, n.font_size, n.fill) = (s.lines().next().unwrap_or_default().chars().take(40).collect(), s.into(), size, Some(ink));
            (n.wrap, n.w, n.h) = (w > 0.0, if w > 0.0 { w } else { 100.0 }, size * 1.2);
            edit(&mut n);
            app.doc.insert(n, Some(parent));
            id
        };
        let s = "Kanvas is fast, KANVAS is native, kanvas is light";
        let at = |part: &str| {
            let a = s.find(part).map_or(0, |i| s[..i].chars().count());
            (a, a + part.chars().count())
        };
        text(self, s, (40.0, 40.0, 0.0), 26.0, stage, &|n| {
            let (a, b) = at("fast");
            n.toggle_decoration(a, b, false);
            let (a, b) = at("KANVAS");
            n.style_range(a, b, |st| (st.weight, st.fill, st.size) = (700, blue, 34.0));
            // From the middle of the third match on.
            let (a, b) = at("vas is li");
            n.style_range(a, b, |st| (st.fill, st.italic) = (red, true));
        });
        let paragraph = "Every kanvas file opens at once. The kanvas canvas never ends, and a kanvas frame can hold another frame. Text in Kanvas stays sharp at any zoom, so a headline set in kanvas reads as well as the small print.\nA second paragraph mentions kanvas once more.";
        text(self, paragraph, (40.0, 130.0, 420.0), 17.0, stage, &|n| n.align = 3);
        text(self, "Open Kanvas\nDraw a frame in kanvas\nExport from Kanvas as PNG", (520.0, 130.0, 440.0), 18.0, stage, &|n| n.list = 2);
        frame(self, "Box", (520.0, 270.0, 200.0, 110.0), [226, 236, 250, 255], Some(stage));
        text(self, "Bottom line:\nkanvas", (520.0, 270.0, 200.0), 18.0, stage, &|n| (n.fixed_h, n.h, n.valign, n.align) = (true, 110.0, 2, 1));
        text(self, "Tilted kanvas", (770.0, 300.0, 0.0), 22.0, stage, &|n| (n.rotation, n.w, n.h) = (15.0, 140.0, 27.0));
        let card = frame(self, "Card", (40.0, 400.0, 420.0, 300.0), [236, 238, 242, 255], Some(stage));
        let badge = frame(self, "Badge", (70.0, 440.0, 300.0, 120.0), blue, Some(card));
        text(self, "Made with Kanvas", (95.0, 482.0, 0.0), 24.0, badge, &|n| (n.fill, n.weight) = (Some([255, 255, 255, 255]), 700));
        text(self, "A card about kanvas", (70.0, 600.0, 0.0), 20.0, card, &|_| {});
        text(self, "Hidden kanvas", (520.0, 420.0, 0.0), 18.0, stage, &|n| n.visible = false);
        text(self, "Locked kanvas (not found)", (520.0, 460.0, 0.0), 18.0, stage, &|n| (n.locked, n.fill) = (true, Some([150, 150, 156, 255])));
        text(self, "Big Kanvas", (520.0, 540.0, 0.0), 72.0, stage, &|n| n.weight = 700);
        // A button that hugs its label, as a main component, and an instance of it.
        let made = self.remote_bench(
            "create_nodes",
            &serde_json::json!({ "parent": stage, "nodes": [{
                "kind": "frame", "name": "Pill", "x": 520, "y": 660, "fill": "#14AE5C", "component": true,
                "layout": { "padding": [18, 10], "align": "center" },
                "children": [{ "kind": "text", "text": "Get Kanvas", "font_size": 18, "weight": 700, "fill": "#FFFFFF" }],
            }] }),
        );
        let pill: u64 = made.chars().filter(char::is_ascii_digit).collect::<String>().parse().unwrap_or(0);
        self.remote_bench("create_nodes", &serde_json::json!({ "parent": stage, "nodes": [{ "kind": "instance", "instance_of": pill, "name": "Pill copy", "x": 720, "y": 660 }] }));
        // A second page with one more.
        self.add_page();
        let id = self.doc.alloc_id();
        let mut n = Node::new(id, Kind::Text, 100.0, 100.0);
        (n.name, n.text, n.font_size, n.fill, n.w, n.h) = ("Page two".into(), "Page two has kanvas too".into(), 40.0, Some([240, 240, 240, 255]), 460.0, 48.0);
        self.doc.insert(n, None);
        self.go_page(0);

        self.zoom_to((x - 30.0, y - 40.0, x + 1030.0, y + 790.0), 1.0);
        self.fit_pending = false;
        self.expanded.insert(stage);
        self.sel.clear();
        self.tool = Tool::Select;
        self.find.query = "kanvas".into();
        match mode {
            "keys" => self.find.query.clear(),
            "all" => self.find.scope = Where::All,
            "selection" => {
                self.sel = vec![card];
                self.find.scope = Where::Selection;
            }
            "far" => self.fit_all(),
            "many" => {
                // Two rows of four screens of the stress test: some 150 matches in view.
                self.find.query = "a".into();
                if let Some(b) = self.doc.nodes.iter().find(|n| n.name == "Screen 1").map(|n| n.bounds()) {
                    self.zoom_to((b.0, b.1, b.0 + 470.0 * 4.0, b.1 + 964.0 * 2.0), 1.0);
                }
            }
            _ => {}
        }
        if mode != "keys" {
            self.open_find(false);
        }
    }

    /// What the modes of `KANVAS_BENCH_FIND` do after the document is built (frame 190):
    /// a number: that many steps to the next match. `replace`: Replace all with "Studio".
    /// `one`: Replace twice with "Kanvas Pro" (which holds the query again). `all`: all pages,
    /// one step back, to the match on page two. `selection`: only inside the selected card.
    /// `keys`: everything from the keyboard (Ctrl+F, type, Enter twice, Ctrl+H, type, Enter).
    /// `esc`: Esc in the Find field closes the window. `far`: from the whole document in view,
    /// a step zooms in to the match. `many`: a screenful of matches in the stress test.
    /// `undo`: Replace all, then one undo. `remote`: the remote `replace_text` on all pages.
    /// The outcome is printed to stderr beside what it should be.
    pub(super) fn bench_find_frame(&mut self, ctx: &egui::Context, mode: &str, frame: u32, inject: &mut Vec<(u32, egui::Event)>) {
        let key = |at: u32, key: Key, modifiers: Modifiers| {
            let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
            [(at, event(true)), (at + 1, event(false))]
        };
        let steps: u32 = mode.parse().unwrap_or(0);
        if (215..215 + steps * 3).contains(&frame) && (frame - 215) % 3 == 0 {
            self.find_step(true);
        }
        match (mode, frame) {
            ("replace", 215) => {
                self.find.with = "Studio".into();
                self.find_replace_all(ctx);
            }
            ("undo", 215) => {
                self.find.with = "Studio".into();
                self.find_replace_all(ctx);
            }
            ("undo", 230) => self.undo_cmd(),
            ("remote", 215) => {
                let args = serde_json::json!({ "query": "KANVAS", "replacement": "Studio", "scope": "all" });
                eprintln!("FIND remote replace_text: {}", self.remote_bench("replace_text", &args));
                let bad = serde_json::json!({ "query": "", "replacement": "x" });
                eprintln!("FIND remote replace_text without a query: {}", self.remote_bench("replace_text", &bad));
            }
            ("one", 215) => {
                self.find.with = "Kanvas Pro".into();
                self.find_step(true);
            }
            ("one", 220 | 225) => self.find_replace_one(ctx),
            ("all", 215) => self.find_step(false),
            ("selection" | "far", 215) => {
                eprintln!("FIND zoom before the step: {:.3}", self.view.zoom);
                self.find_step(true);
            }
            ("keys", 200) => {
                inject.extend(key(212, Key::F, Modifiers::COMMAND));
                inject.push((216, egui::Event::Text("kanvas".into())));
                inject.extend(key(220, Key::Enter, Modifiers::NONE));
                inject.extend(key(224, Key::Enter, Modifiers::NONE));
                inject.extend(key(228, Key::H, Modifiers::COMMAND));
                inject.push((232, egui::Event::Text("Studio".into())));
                inject.extend(key(238, Key::Enter, Modifiers::NONE));
            }
            ("esc", 200) => inject.extend(key(215, Key::Escape, Modifiers::NONE)),
            ("esc", 225) => eprintln!("FIND after Esc in the Find field: window open {} (want false)", self.find.open),
            _ => {}
        }
        if frame == 248 {
            let f = &self.find;
            let current = f.current.and_then(|i| f.matches.get(i)).map(|m| (m.page, m.id, m.start, m.end));
            eprintln!(
                "FIND \"{}\" -> \"{}\": {} matches, current {:?} = {current:?}, last search {:.3} ms; selection {:?}, page {}, zoom {:.3}, tool is Move: {}, undo steps {}, unsaved {}",
                f.query,
                f.with,
                f.matches.len(),
                f.current,
                f.scan_ms,
                self.sel,
                self.doc.page,
                self.view.zoom,
                self.tool == Tool::Select,
                self.undo.len(),
                self.dirty,
            );
            eprintln!("FIND status: {}", self.status);
            let mut texts = Vec::new();
            if let Some(stage) = self.doc.nodes.iter().find(|n| n.name == "Find demo") {
                stage.walk(&mut |n| {
                    if n.name.starts_with("Pill") {
                        texts.push(format!("  frame {:?} {:.0}x{:.0}", n.name, n.w, n.h));
                    }
                    if n.kind == Kind::Text {
                        let spans: Vec<(usize, usize)> = n.spans.iter().map(|s| (s.start, s.end)).collect();
                        texts.push(format!("  {:?} {:.0}x{:.0} spans {spans:?} name {:?}", n.text, n.w, n.h, n.name));
                    }
                });
            }
            eprintln!("FIND texts:\n{}", texts.join("\n"));
        }
        if (250..=255).contains(&frame) {
            eprintln!("FIND frame {frame}: {:.2} ms", self.frame_ms);
        }
    }
}
