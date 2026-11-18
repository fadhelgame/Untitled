//! Auto layout in the app: keeping the frames laid out after every edit and while dragging,
//! Shift+A, re-ordering items with the mouse and the arrow keys, and the sections of the Design
//! panel. What a layout does with its items is in `crate::layout`.

use super::*;
use crate::layout::{self, Align, AutoLayout, Dir, Sizing};


const ITEM: &str = "An auto-layout item is placed by its frame: drag it to re-order, change the alignment of the frame, or set the item to \"Absolute position\".";

/// What a button of the Auto layout section asks for.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Act {
    Add,
    Remove,
}

impl App {
    /// The items that are being dragged: they keep their room in the layout, but follow the pointer.
    fn lifted(&self) -> Vec<u64> {
        match self.drag {
            Drag::Move { moved: true, .. } => self.roots(),
            _ => Vec::new(),
        }
    }

    /// While something is dragged or typed, the layouts around it follow on every frame; these
    /// changes are not committed yet, so `settle_doc` does not see them.
    pub(super) fn settle_live(&mut self) {
        if matches!(self.drag, Drag::None | Drag::Pan | Drag::Marquee { .. }) && self.editing.is_none() {
            return;
        }
        let ids: Vec<u64> = self.sel.iter().copied().chain(self.editing).collect();
        self.doc.settle_around(&ids, &self.lifted());
    }

    /// Lay out the auto-layout frames and bring instances in line with their mains, until
    /// neither changes anything. Called once per frame, after everything that can edit the
    /// document (and after every remote command), and does nothing unless the document was
    /// committed to since the last time (`touch`). Edits commit in different ways (some push
    /// the undo step before they change anything), so both compare against the document of the
    /// last call rather than against an undo snapshot: only the top-level nodes that are not
    /// shared with it are looked at.
    pub(super) fn settle_doc(&mut self) {
        if self.components.rev == self.ext.rev && self.components.base.is_some() {
            return self.settle_live();
        }
        // A text whose path was deleted is plain text again.
        self.doc.drop_lost_paths();
        let base = self.components.base.take();
        if self.doc.settle(base.as_ref(), &self.lifted()) {
            // Instances took what their mains have now, text included: what Find and replace
            // found in them is of the old text.
            self.find.gen += 1;
            // Parts whose layer was removed from the main are gone.
            let doc = &self.doc;
            self.sel.retain(|id| doc.find(*id).is_some());
        }
        self.components.base = Some(self.doc.clone());
        self.components.rev = self.ext.rev;
    }

    /// Something in the selection is an item of an auto layout (see `Doc::auto_item`).
    pub(super) fn sel_auto_items(&self) -> bool {
        self.sel.iter().any(|id| self.doc.auto_item(*id))
    }

    /// For edits that place layers by hand: says why not when the selection has an item of an
    /// auto layout in it. Result: the edit must not happen.
    pub(super) fn refuse_auto_items(&mut self) -> bool {
        let items = self.sel_auto_items();
        if items {
            self.status = ITEM.into();
        }
        items
    }

    /// Shift+A: the selected frame gets auto layout; anything else is wrapped in a frame with one.
    pub(super) fn add_auto_layout(&mut self) {
        if self.sel.is_empty() || self.sel_locked() {
            self.status = "Select a frame or some layers to add auto layout to.".into();
            return;
        }
        let pre = self.doc.clone();
        match self.doc.add_auto_layout(&self.sel) {
            Ok(id) => {
                self.sel = vec![id];
                self.expanded.insert(id);
                self.push_undo(pre);
                self.status = "Auto layout added: the frame places its children itself. Drag an item to re-order it; Alt+Shift+A removes the layout.".into();
            }
            Err(e) => self.status = e.into(),
        }
    }

    /// Alt+Shift+A: the selected frames keep their children where they are and stop placing them.
    pub(super) fn remove_auto_layout(&mut self) {
        let pre = self.doc.clone();
        let mut any = false;
        for id in self.sel.clone() {
            any |= self.doc.remove_auto_layout(id);
        }
        if any {
            self.push_undo(pre);
            self.status = "Auto layout removed. The layers stay where they are.".into();
        } else if self.sel.iter().any(|id| self.doc.find(*id).is_some_and(|n| n.is_auto())) {
            self.status = "The layout of an instance follows the main component: remove it there, or detach the instance first.".into();
        } else {
            self.status = "Select a frame with auto layout to remove it.".into();
        }
    }

    /// Arrow keys with items of an auto layout selected: each trades places with its neighbour
    /// along the direction of its frame. The caller commits (see `App::nudge`). Result: `None`
    /// if no item is selected, else whether any of them moved.
    pub(super) fn swap_items(&mut self, dx: f32, dy: f32) -> Option<bool> {
        let mut ids: Vec<u64> = self.roots().into_iter().filter(|id| self.doc.auto_item(*id)).collect();
        if ids.is_empty() {
            return None;
        }
        self.doc.sort_by_stack(&mut ids);
        // The one in front goes first, so that neighbours that move together don't leapfrog.
        if dx + dy > 0.0 {
            ids.reverse();
        }
        let mut any = false;
        for id in &ids {
            let horizontal = self.doc.parent_of(*id).and_then(|p| self.doc.find(p)).and_then(|f| f.layout.as_ref()).is_some_and(|l| l.dir == Dir::Horizontal);
            let d = if horizontal { dx } else { dy };
            let neighbour_moves = |doc: &Doc| {
                let f = doc.parent_of(*id).and_then(|p| doc.find(p));
                let items: Vec<u64> = f.map(|f| f.children.iter().filter(|c| c.in_flow()).map(|c| c.id).collect()).unwrap_or_default();
                let at = items.iter().position(|c| c == id);
                let next = at.and_then(|i| if d > 0.0 { items.get(i + 1) } else { i.checked_sub(1).and_then(|i| items.get(i)) });
                next.is_some_and(|n| ids.contains(n))
            };
            if d != 0.0 && !neighbour_moves(&self.doc) {
                any |= self.doc.swap_item(*id, d > 0.0);
            }
        }
        Some(any)
    }

    /// The frame a dragged layer belongs in with the pointer at (x, y): as `Doc::frame_for`, but
    /// the frame it is in already need not be larger than it (a row hugs the buttons in it).
    fn drop_frame(&self, id: u64, x: f32, y: f32, roots: &[u64]) -> Option<u64> {
        let (n, parent) = (self.doc.find(id)?, self.doc.parent_of(id));
        let found = self.doc.frame_at(x, y, roots)?;
        if n.kind != Kind::Frame {
            return Some(found);
        }
        self.doc.lineage(found).into_iter().rev().find(|f| Some(f.id) == parent || (n.w.abs() < f.w.abs() && n.h.abs() < f.h.abs())).map(|f| f.id)
    }

    /// Is the drag about an auto layout: an item is being dragged, or what is dragged is over a
    /// frame that would take it in as one? (Snapping is off then: the layout decides the place.)
    pub(super) fn drag_in_layout(&self, at: Pos2) -> bool {
        let roots = self.roots();
        roots.iter().any(|id| self.doc.auto_item(*id))
            || roots.first().and_then(|id| self.drop_frame(*id, at.x, at.y, &roots)).and_then(|f| self.doc.find(f)).is_some_and(|f| f.is_auto())
    }

    /// While layers are dragged with the pointer at `grab` (world): those over an auto-layout
    /// frame become its items at the place pointed at, right away, so that the others make
    /// room; an item dragged out of its frame leaves it right away, so that the others close up.
    pub(super) fn reorder_drag(&mut self, grab: Pos2) {
        self.drop_mark = None;
        let roots = self.roots();
        // (Nothing is selected any more: a remote command removed what was being dragged.)
        if roots.is_empty() {
            return;
        }
        let parents: Vec<Option<u64>> = roots.iter().map(|id| self.doc.parent_of(*id)).collect();
        let together = roots.len() == 1 || parents.windows(2).all(|w| w[0] == w[1]);
        let mut touched = roots.clone();
        // Siblings go where the pointer is, as one run; a selection across frames each by its own centre.
        let batches: Vec<(Vec<u64>, (f32, f32))> = if together {
            let mut ids = roots.clone();
            self.doc.sort_by_stack(&mut ids);
            vec![(ids, (grab.x, grab.y))]
        } else {
            roots.iter().filter_map(|id| self.doc.find(*id).map(|n| (vec![*id], n.center()))).collect()
        };
        for (ids, (x, y)) in batches {
            let (target, parent) = (self.drop_frame(ids[0], x, y, &roots), self.doc.parent_of(ids[0]));
            // An absolute child moves about in its frame like in any other.
            let stays = |n: &Node| n.floats() && parent == target;
            let ids: Vec<u64> = ids.into_iter().filter(|id| self.doc.find(*id).is_some_and(|n| !stays(n) && (target == parent || !self.doc.would_cycle(n, target)))).collect();
            if ids.is_empty() {
                continue;
            }
            touched.extend(parent.into_iter().chain(target));
            // (the frame, the child to go in below, anything to move?)
            let plan = target.and_then(|t| self.doc.find(t)).filter(|f| f.is_auto()).map(|f| {
                let rest: Vec<u64> = f.children.iter().map(|c| c.id).filter(|c| !ids.contains(c)).collect();
                let at = layout::insert_index(f, &ids, x, y);
                let there = rest[..at].iter().chain(&ids).chain(&rest[at..]).eq(f.children.iter().map(|c| &c.id));
                (f.id, rest.get(at).copied(), !there)
            });
            match plan {
                Some((frame, next, moves)) => {
                    if moves {
                        self.doc.move_nodes(&ids, Some(frame), next.map(|n| (n, false)), 0);
                    }
                    self.doc.settle_around(&touched, &roots);
                    let slot = self.doc.find(frame).and_then(|f| Some((layout::slot_of(f, ids[0])?, f.layout.as_ref()?.dir)));
                    if let Some(((sx, sy, sw, sh), dir)) = slot {
                        self.drop_mark = Some(if dir == Dir::Horizontal { [pos2(sx, sy), pos2(sx, sy + sh)] } else { [pos2(sx, sy), pos2(sx + sw, sy)] });
                    }
                }
                None if parent != target && ids.iter().any(|id| self.doc.auto_item(*id)) => {
                    for id in ids {
                        if let Some(node) = self.doc.remove(id) {
                            self.doc.insert(node, target);
                        }
                    }
                    self.doc.settle_around(&touched, &roots);
                }
                None => {}
            }
        }
    }

    /// The line where the dragged item will land in its auto layout.
    pub(super) fn paint_drop_mark(&self, painter: &egui::Painter) {
        if let Some([a, b]) = self.drop_mark {
            let o = self.canvas_rect.min;
            let (a, b) = (self.view.to_screen(o, a.x, a.y), self.view.to_screen(o, b.x, b.y));
            painter.line_segment([a, b], Stroke::new(2.0_f32, ACCENT));
            for p in [a, b] {
                painter.circle_filled(p, 2.5, ACCENT);
            }
        }
    }

    /// The sizes the Design panel offers for one axis of a node: as it is set or, for a frame
    /// with auto layout, hugging; and filling, for an item whose frame has room to give.
    pub(super) fn sizing_choices(&self, id: u64, horizontal: bool) -> Vec<Sizing> {
        let Some(n) = self.doc.find(id) else { return Vec::new() };
        let mut out = match n.instance_of.and_then(|m| self.doc.main(m)) {
            // An instance hugs when its main does.
            Some(main) => vec![if main.sizing(horizontal) == Sizing::Hug { Sizing::Hug } else { Sizing::Fixed }],
            None if n.is_auto() && n.source.is_none() => vec![Sizing::Fixed, Sizing::Hug],
            None => vec![Sizing::Fixed],
        };
        if self.doc.auto_item(id) && n.can_fill(horizontal) {
            out.push(Sizing::Fill);
        }
        out
    }

    /// `KANVAS_BENCH_AUTOLAYOUT`: a card (a column that hugs its height) with a title, a row of
    /// two buttons (one fills the row, one hugs its label) and a wrapping row of chips; a main
    /// component "Pill" with auto layout and an instance of it whose label is longer; and a row
    /// of three boxes to re-order with the mouse. Result: ids of [card, title, button row, fill
    /// button, hug button, chips, pill main, pill instance, label of the instance, box row, box A].
    pub(super) fn bench_autolayout(&mut self) -> Vec<u64> {
        let (x, y) = (-1500.0, 7000.0);
        let pre = self.doc.clone();
        let stage = self.doc.alloc_id();
        let mut n = Node::new(stage, Kind::Frame, x, y);
        (n.name, n.w, n.h, n.fill) = ("Auto layout demo".into(), 900.0, 520.0, Some([250, 250, 250, 255]));
        self.doc.insert(n, None);
        let frame = |app: &mut Self, parent: u64, name: &str, at: (f32, f32), fill: Option<[u8; 4]>, l: AutoLayout, size: (Sizing, Sizing)| {
            let id = app.doc.alloc_id();
            let mut n = Node::new(id, Kind::Frame, x + at.0, y + at.1);
            (n.name, n.w, n.h, n.fill, n.layout, n.size_h, n.size_v) = (name.into(), 100.0, 40.0, fill, Some(l), size.0, size.1);
            app.doc.insert(n, Some(parent));
            id
        };
        let text = |app: &mut Self, parent: u64, s: &str, size: f32, weight: u16, fill: [u8; 4]| {
            let id = app.doc.alloc_id();
            let mut n = Node::new(id, Kind::Text, x, y);
            (n.name, n.text, n.font_size, n.weight, n.fill) = (s.into(), s.into(), size, weight, Some(fill));
            (n.w, n.h) = (s.chars().count() as f32 * size * 0.55, size * 1.2);
            app.doc.insert(n, Some(parent));
            id
        };
        let row = |gap: f32, pad: [f32; 4]| AutoLayout { gap, pad, cross: Align::Center, ..Default::default() };
        let (white, ink, blue) = ([255, 255, 255, 255], [30, 30, 40, 255], [13, 153, 255, 255]);
        use Sizing::{Fill, Fixed, Hug};

        let card = frame(self, stage, "Card", (40.0, 40.0), Some(white), AutoLayout { dir: Dir::Vertical, gap: 16.0, pad: [24.0; 4], ..Default::default() }, (Fixed, Hug));
        if let Some(n) = self.doc.find_mut(card) {
            (n.w, n.stroke, n.stroke_w) = (360.0, Some([210, 214, 222, 255]), 1.0);
        }
        let title = text(self, card, "Auto layout", 28.0, 700, ink);
        let buttons = frame(self, card, "Buttons", (0.0, 0.0), None, row(12.0, [0.0; 4]), (Fill, Hug));
        let pad = [16.0, 10.0, 16.0, 10.0];
        let primary = frame(self, buttons, "Fill button", (0.0, 0.0), Some(blue), AutoLayout { align: Align::Center, ..row(8.0, pad) }, (Fill, Hug));
        text(self, primary, "Continue", 16.0, 700, white);
        let cancel = frame(self, buttons, "Hug button", (0.0, 0.0), Some([232, 236, 242, 255]), row(8.0, pad), (Hug, Hug));
        text(self, cancel, "Cancel", 16.0, 400, ink);
        let chips = frame(self, card, "Chips", (0.0, 0.0), None, AutoLayout { wrap: true, row_gap: 8.0, ..row(8.0, [0.0; 4]) }, (Fill, Hug));
        for label in ["Design", "Prototype", "Components", "Auto layout", "Constraints", "Export"] {
            let chip = frame(self, chips, label, (0.0, 0.0), Some([226, 240, 255, 255]), row(4.0, [10.0, 4.0, 10.0, 4.0]), (Hug, Hug));
            text(self, chip, label, 13.0, 400, [0, 80, 160, 255]);
        }

        let pill = frame(self, stage, "Pill", (460.0, 40.0), Some([20, 174, 92, 255]), row(8.0, [18.0, 10.0, 18.0, 10.0]), (Hug, Hug));
        text(self, pill, "OK", 18.0, 700, white);
        let boxes = frame(self, stage, "Boxes", (460.0, 260.0), Some([238, 238, 242, 255]), row(10.0, [10.0; 4]), (Hug, Hug));
        let mut first = 0;
        for (i, (name, fill)) in [("A", [242, 72, 72, 255]), ("B", [255, 205, 41, 255]), ("C", [151, 71, 255, 255])].into_iter().enumerate() {
            let id = self.doc.alloc_id();
            let mut n = Node::new(id, Kind::Rect, x, y);
            (n.name, n.w, n.h, n.fill) = (name.into(), 60.0, 40.0, Some(fill));
            self.doc.insert(n, Some(boxes));
            if i == 0 {
                first = id;
            }
        }
        self.zoom_to((x - 40.0, y - 60.0, x + 940.0, y + 580.0), 1.0);
        self.fit_pending = false;
        // From here on the paths a user takes; each step is a frame of its own for them.
        self.push_undo(pre);
        self.settle_doc();
        self.sel = vec![pill];
        self.create_component();
        self.settle_doc();
        let o = self.canvas_rect.min;
        let inst = self.insert_instance(pill, Some(self.view.to_screen(o, x + 560.0, y + 160.0))).unwrap_or_default();
        self.settle_doc();
        let label = self.doc.find(inst).and_then(|n| n.children.first()).map_or(0, |c| c.id);
        self.sel = vec![label];
        self.edit_selected(|n| n.text = "A much longer label".into());
        self.expanded.extend([stage, card, buttons, chips, inst, boxes]);
        self.sel = vec![card];
        self.tool = Tool::Select;
        vec![card, title, buttons, primary, cancel, chips, pill, inst, label, boxes, first]
    }
}

// ---------------------------------------------------------------- Design panel

/// The size dropdowns under W and H: one per axis that has a choice (`choices`, see
/// `App::sizing_choices`; `None` = the selection is mixed). Result: what was picked.
pub(super) fn sizing_row(ui: &mut egui::Ui, half: f32, shown: (Option<Sizing>, Option<Sizing>), choices: (&[Sizing], &[Sizing])) -> (Option<Sizing>, Option<Sizing>) {
    let mut picked = (None, None);
    if choices.0.len() < 2 && choices.1.len() < 2 {
        return picked;
    }
    ui.horizontal(|ui| {
        for horizontal in [true, false] {
            let (now, list) = if horizontal { (shown.0, choices.0) } else { (shown.1, choices.1) };
            ui.add_enabled_ui(list.len() > 1, |ui| {
                let combo = egui::ComboBox::from_id_salt(if horizontal { "size_h" } else { "size_v" })
                    .width(half - 8.0)
                    .selected_text(now.map_or("Mixed", |s| s.label()))
                    .show_ui(ui, |ui| {
                        for s in list {
                            if ui.selectable_label(now == Some(*s), s.label()).clicked() {
                                *(if horizontal { &mut picked.0 } else { &mut picked.1 }) = Some(*s);
                            }
                        }
                    });
                combo.response.on_hover_text(if horizontal {
                    "Width: fixed, hugging what the frame holds, or filling the auto layout it sits in"
                } else {
                    "Height: fixed, hugging what the frame holds, or filling the auto layout it sits in"
                });
            });
        }
    });
    picked
}

/// The Auto layout section for a frame: + adds a layout, − removes it; between them the
/// direction, the gap ("Auto" = space between), the padding and the alignment of the items.
/// `locked`: why it only shows (the layout of an instance is its main's). Result: + or −.
pub(super) fn layout_section(ui: &mut egui::Ui, layout: &mut Option<AutoLayout>, locked: Option<&str>, note: &str) -> Option<Act> {
    let mut act = None;
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        ui.label(theme::title("Auto layout"));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_enabled_ui(locked.is_none(), |ui| {
                if layout.is_some() {
                    if ui.button("−").on_hover_text("Remove auto layout: the layers stay where they are  (Alt+Shift+A)").clicked() {
                        act = Some(Act::Remove);
                    }
                } else if ui.button("+").on_hover_text("Add auto layout: the frame places its children in a row or a column  (Shift+A)").clicked() {
                    act = Some(Act::Add);
                }
            });
        });
    });
    let Some(l) = layout else { return act };
    if !note.is_empty() {
        ui.weak(note);
    }
    ui.add_enabled_ui(locked.is_none(), |ui| {
        let half = (ui.available_width() - 8.0) / 2.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            let now = (l.dir, l.wrap && l.dir == Dir::Horizontal);
            for (mode, tip) in [((Dir::Horizontal, false), "Horizontal: a row"), ((Dir::Vertical, false), "Vertical: a column"), ((Dir::Horizontal, true), "Wrap: a row that goes on to the next line where the width ends")] {
                if icon_button(ui, now == mode, tip, |p, c, color| direction_icon(p, c, mode, color)) {
                    (l.dir, l.wrap) = mode;
                }
            }
        });
        ui.horizontal(|ui| {
            align_grid(ui, l);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                let width = ui.available_width();
                let between = l.align == Align::Between;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;
                    ui.add_enabled_ui(!between, |ui| {
                        ui.add_sized([width - 50.0, 28.0], egui::DragValue::new(&mut l.gap).speed(0.5).range(0.0..=10000.0).max_decimals(1).prefix("Gap  ")).on_hover_text("Space between neighbouring items");
                    });
                    if ui.add_sized([46.0, 28.0], egui::SelectableLabel::new(between, "Auto")).on_hover_text("Space between: the first and last item at the edges, the room left over shared out between the items").clicked() {
                        l.align = if between { Align::Start } else { Align::Between };
                    }
                });
                if l.wrap && l.dir == Dir::Horizontal {
                    ui.add_sized([width, 28.0], egui::DragValue::new(&mut l.row_gap).speed(0.5).range(0.0..=10000.0).max_decimals(1).prefix("Row gap  ")).on_hover_text("Space between the rows");
                }
            });
        });
        sub(ui, "Padding");
        let id = egui::Id::new("kanvas_padding_sides");
        let uneven = l.pad[0] != l.pad[2] || l.pad[1] != l.pad[3];
        let mut open = ui.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) || uneven;
        let pad = |ui: &mut egui::Ui, width: f32, label: &str, v: &mut f32| ui.add_sized([width, 28.0], egui::DragValue::new(v).speed(0.5).range(0.0..=10000.0).max_decimals(1).prefix(format!("{label}  "))).changed();
        ui.horizontal(|ui| {
            let width = if open { (ui.available_width() - 62.0) / 4.0 } else { half - 20.0 };
            ui.spacing_mut().item_spacing.x = 6.0;
            if open {
                for (i, label) in ["L", "T", "R", "B"].into_iter().enumerate() {
                    pad(ui, width, label, &mut l.pad[i]);
                }
            } else {
                if pad(ui, width, "H", &mut l.pad[0]) {
                    l.pad[2] = l.pad[0];
                }
                if pad(ui, width, "V", &mut l.pad[1]) {
                    l.pad[3] = l.pad[1];
                }
            }
            if ui.add_enabled(!uneven, egui::SelectableLabel::new(open, "⊞")).on_hover_text("Padding of each side: left, top, right, bottom").clicked() {
                open = !open;
            }
        });
        ui.data_mut(|d| d.insert_temp(id, open && !uneven));
    });
    if let Some(why) = locked {
        ui.weak(why);
    }
    act
}

/// An arrow for the direction of a layout; for wrapping, one that turns into the next row.
fn direction_icon(p: &egui::Painter, c: Pos2, (dir, wrap): (Dir, bool), color: Color32) {
    let s = Stroke::new(1.5_f32, color);
    let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
    let arrow = |tip: Pos2, back: Vec2| {
        let side = vec2(-back.y, back.x);
        p.line_segment([tip, tip + back + side], s);
        p.line_segment([tip, tip + back - side], s);
    };
    match (dir, wrap) {
        (Dir::Horizontal, false) => {
            p.line_segment([at(-7.0, 0.0), at(7.0, 0.0)], s);
            arrow(at(7.0, 0.0), vec2(-3.5, 0.0));
        }
        (Dir::Vertical, _) => {
            p.line_segment([at(0.0, -7.0), at(0.0, 7.0)], s);
            arrow(at(0.0, 7.0), vec2(0.0, -3.5));
        }
        (Dir::Horizontal, true) => {
            p.add(egui::Shape::line(vec![at(-7.0, -4.0), at(7.0, -4.0), at(7.0, 4.0), at(-6.0, 4.0)], s));
            arrow(at(-7.0, 4.0), vec2(3.5, 0.0));
        }
    }
}

/// Nine places for the items in their frame: columns are left / center / right, rows top /
/// middle / bottom. With space between only the place across the direction is a choice.
fn align_grid(ui: &mut egui::Ui, l: &mut AutoLayout) {
    const PLACES: [Align; 3] = [Align::Start, Align::Center, Align::End];
    let side = 66.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let enabled = ui.is_enabled();
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(8), Color32::from_gray(48));
    let horizontal = l.dir == Dir::Horizontal;
    let between = l.align == Align::Between;
    let pointer = resp.hover_pos().filter(|_| enabled);
    let cell = side / 3.0;
    for row in 0..3usize {
        for col in 0..3usize {
            let c = rect.min + vec2((col as f32 + 0.5) * cell, (row as f32 + 0.5) * cell);
            let zone = Rect::from_center_size(c, vec2(cell, cell));
            let (main, cross) = if horizontal { (PLACES[col], PLACES[row]) } else { (PLACES[row], PLACES[col]) };
            let lit = l.cross == cross && (between || l.align == main);
            let hot = pointer.is_some_and(|m| zone.contains(m));
            if lit {
                // Three bars, lined up the way the items are.
                let color = if enabled { ACCENT } else { Color32::from_gray(150) };
                for (i, len) in [9.0_f32, 13.0, 6.0].into_iter().enumerate() {
                    let along = (i as f32 - 1.0) * 4.5;
                    let off = match cross {
                        Align::Start => (len - 13.0) * 0.5,
                        Align::End => (13.0 - len) * 0.5,
                        _ => 0.0,
                    };
                    let (mid, size) = if horizontal { (c + vec2(along, off), vec2(3.0, len)) } else { (c + vec2(off, along), vec2(len, 3.0)) };
                    p.rect_filled(Rect::from_center_size(mid, size), CornerRadius::same(1), color);
                }
            } else {
                p.circle_filled(c, if hot { 3.0 } else { 1.8 }, Color32::from_gray(if hot { 220 } else if enabled { 120 } else { 80 }));
            }
            if hot && resp.clicked() {
                l.cross = cross;
                if !between {
                    l.align = main;
                }
            }
        }
    }
    if enabled {
        resp.on_hover_text("Where the items sit in the frame");
    }
}
