//! Components in the app: making mains and instances, keeping instances in line with their
//! mains after every edit, the Component section of the Design panel and the Components window.
//! The model and the rules of following are in `crate::component`.

use super::*;

/// Mains and instances are drawn in this colour: name label, outline, Layers panel icon.
pub(super) const PURPLE: Color32 = Color32::from_rgb(151, 71, 255);

const PINNED: &str = "This layer is part of an instance: its place, size and order follow the main component. Edit the main, or detach the instance (Ctrl+Alt+B).";
const CYCLE: &str = "A component can't contain an instance of itself.";

#[derive(Default)]
pub(super) struct Components {
    /// The Components window is open.
    pub open: bool,
    filter: String,
    focus: bool,
    /// The document as it was when instances were last brought in line with their mains.
    /// `None`: take the document as it is (just opened, or after undo / redo).
    base: Option<Doc>,
    rev: u64,
}

impl Components {
    /// The document was replaced as a whole: there is no edit to follow.
    pub fn rebase(&mut self) {
        self.base = None;
    }

    /// Look for changes again on the next frame, though nothing was committed (text was measured).
    pub fn recheck(&mut self) {
        self.rev = u64::MAX;
    }
}

/// What a button in the Component section asks for.
#[derive(Clone, Copy)]
pub(super) enum Act {
    Create,
    GoToMain,
    Reset,
    Detach,
    Remove,
}

/// Four diamonds for a main component, one outlined diamond for an instance.
pub(super) fn component_icon(p: &egui::Painter, c: Pos2, main: bool, color: Color32) {
    let diamond = |c: Pos2, r: f32| vec![c + vec2(0.0, -r), c + vec2(r, 0.0), c + vec2(0.0, r), c + vec2(-r, 0.0)];
    if main {
        for d in [vec2(0.0, -3.4), vec2(3.4, 0.0), vec2(0.0, 3.4), vec2(-3.4, 0.0)] {
            p.add(egui::Shape::convex_polygon(diamond(c + d, 2.3), color, Stroke::NONE));
        }
    } else {
        p.add(egui::Shape::closed_line(diamond(c, 5.0), Stroke::new(1.4_f32, color)));
    }
}

/// Toolbar button: the four diamonds, a little larger.
pub(super) fn toolbar_icon(p: &egui::Painter, c: Pos2, color: Color32) {
    let diamond = |c: Pos2, r: f32| vec![c + vec2(0.0, -r), c + vec2(r, 0.0), c + vec2(0.0, r), c + vec2(-r, 0.0)];
    for d in [vec2(0.0, -5.0), vec2(5.0, 0.0), vec2(0.0, 5.0), vec2(-5.0, 0.0)] {
        p.add(egui::Shape::closed_line(diamond(c + d, 3.3), Stroke::new(1.5_f32, color)));
    }
}

impl App {
    pub(super) fn toggle_components(&mut self) {
        self.components.open = !self.components.open;
        self.components.focus = self.components.open;
    }

    /// Something in the selection is a part of an instance (see `Doc::pinned`).
    pub(super) fn sel_pinned(&self) -> bool {
        self.sel.iter().any(|id| self.doc.pinned(*id))
    }

    /// For edits that move, resize, reorder or remove layers: says why not when the selection
    /// has a part of an instance in it. Result: the edit must not happen.
    pub(super) fn refuse_pinned(&mut self) -> bool {
        let pinned = self.sel_pinned();
        if pinned {
            self.status = PINNED.into();
        }
        pinned
    }

    /// May `node` go into frame `parent`? Not into an instance, and not where a component
    /// would come to contain itself; the status bar says which.
    pub(super) fn may_enter(&mut self, node: &Node, parent: Option<u64>) -> bool {
        let Some(p) = parent else { return true };
        if self.doc.in_instance(p) {
            self.status = "Layers can't be added to an instance. Add them to the main component, or detach the instance (Ctrl+Alt+B).".into();
            return false;
        }
        if self.doc.would_cycle(node, parent) {
            self.status = CYCLE.into();
            return false;
        }
        true
    }

    /// Bring instances in line with their mains. Called once per frame, after everything that
    /// can edit the document, and does nothing unless the document was committed to since the
    /// last time (`touch`). Edits commit in different ways (some push the undo step before
    /// they change anything), so the comparison is against the document of the last call
    /// rather than against an undo snapshot.
    pub(super) fn sync_components(&mut self) {
        if self.components.rev == self.ext.rev && self.components.base.is_some() {
            return;
        }
        if let Some(base) = self.components.base.take() {
            if self.doc.sync_components(&base) {
                // Parts whose layer was removed from the main are gone.
                let doc = &self.doc;
                self.sel.retain(|id| doc.find(*id).is_some());
            }
        }
        self.components.base = Some(self.doc.clone());
        self.components.rev = self.ext.rev;
    }

    /// Ctrl+Alt+K: the selected frame becomes a main component; anything else is wrapped in one.
    pub(super) fn create_component(&mut self) {
        if self.sel.is_empty() {
            self.status = "Select a frame or some layers to make a component of.".into();
            return;
        }
        let pre = self.doc.clone();
        match self.doc.make_component(&self.sel) {
            Ok(id) => {
                self.sel = vec![id];
                self.push_undo(pre);
                self.status = "Main component created. Copies of it (Ctrl+D, Alt+drag, copy and paste, or the Components window) are instances that follow it.".into();
            }
            Err(e) => self.status = e.into(),
        }
    }

    /// Put a new instance of main `main` at the centre of the view (or at screen position
    /// `at`), inside the frame there if there is one. Result: its id.
    pub(super) fn insert_instance(&mut self, main: u64, at: Option<Pos2>) -> Option<u64> {
        let pre = self.doc.clone();
        let Some(mut n) = self.doc.instantiate(main) else {
            self.status = "That main component is gone.".into();
            return None;
        };
        let c = self.view.to_world(self.canvas_rect.min, at.unwrap_or(self.canvas_rect.center()));
        n.translate((c.x - n.w * 0.5 - n.x).round(), (c.y - n.h * 0.5 - n.y).round());
        let parent = self.doc.frame_for(&n, c.x, c.y, &[]);
        if self.doc.would_cycle(&n, parent) {
            self.doc = pre;
            self.status = format!("{CYCLE} Move the view off the main component first.");
            return None;
        }
        let id = n.id;
        self.status = format!("Added an instance of \"{}\".", n.name);
        self.doc.insert(n, parent);
        self.sel = vec![id];
        self.tool = Tool::Select;
        self.push_undo(pre);
        Some(id)
    }

    /// Ctrl+Alt+B: the selected instances become plain frames.
    pub(super) fn detach_selection(&mut self) {
        let ids: Vec<u64> = self.sel.iter().copied().filter(|id| self.doc.find(*id).is_some_and(|n| n.is_instance())).collect();
        if ids.is_empty() {
            if self.sel_pinned() {
                self.status = "Select the instance itself to detach it (Shift+Enter selects the parent).".into();
            }
            return;
        }
        let pre = self.doc.clone();
        for id in &ids {
            self.doc.detach(*id);
        }
        self.push_undo(pre);
        self.status = "Instance detached: it is a plain frame now and no longer follows the main component.".into();
    }

    fn reset_overrides(&mut self, id: u64) {
        let pre = self.doc.clone();
        if self.doc.reset_overrides(id) {
            self.push_undo(pre);
            self.status = "Overrides reset to the main component.".into();
        } else {
            self.status = "Nothing to reset: this already matches the main component.".into();
        }
    }

    /// Select what `id` mirrors (the main of an instance, or the layer of the main that a part
    /// stands for), on whatever page it is, and zoom to it.
    fn go_to_main(&mut self, id: u64) {
        let target = self.doc.find(id).and_then(|n| n.instance_of.or(n.source));
        let Some((target, page)) = target.and_then(|t| Some((t, self.doc.page_of(t)?))) else {
            self.status = "The main component of this instance is gone.".into();
            return;
        };
        self.go_page(page);
        self.fit_pending = false;
        self.expanded.extend(self.doc.ancestors(target));
        self.sel = vec![target];
        self.zoom_selection();
    }

    fn remove_component(&mut self, id: u64) {
        let pre = self.doc.clone();
        if let Some(n) = self.doc.find_mut(id) {
            n.component = false;
        }
        self.push_undo(pre);
        self.status = "No longer a component. Its instances keep their look and stop following it.".into();
    }

    pub(super) fn component_act(&mut self, act: Act) {
        let first = self.sel.first().copied();
        match (act, first) {
            (Act::Create, _) => self.create_component(),
            (Act::Detach, _) => self.detach_selection(),
            (Act::GoToMain, Some(id)) => self.go_to_main(id),
            (Act::Reset, Some(id)) => self.reset_overrides(id),
            (Act::Remove, Some(id)) => self.remove_component(id),
            _ => {}
        }
    }

    /// The Component section of the Design panel, for the current selection.
    pub(super) fn component_section(&self, ui: &mut egui::Ui) -> Option<Act> {
        let mut act = None;
        section(ui, "Component");
        let wide = |ui: &mut egui::Ui, enabled: bool, label: &str, tip: &str| {
            ui.add_enabled(enabled, egui::Button::new(label).min_size(vec2(ui.available_width(), 28.0))).on_hover_text(tip).clicked()
        };
        let headline = |ui: &mut egui::Ui, main: bool, text: String| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 18.0), Sense::hover());
                component_icon(ui.painter(), r.center(), main, PURPLE);
                ui.add(egui::Label::new(egui::RichText::new(text).color(PURPLE)).wrap());
            });
        };
        let name_of = |main: Option<u64>| main.and_then(|m| self.doc.main(m)).map(|m| m.name.clone());
        let one = match self.sel[..] {
            [id] => self.doc.find(id),
            _ => None,
        };
        match one {
            Some(n) if n.component => {
                let mut count = 0;
                self.doc.walk_all(|c| count += (c.instance_of == Some(n.id) && c.source.is_none()) as usize);
                headline(ui, true, "Main component".into());
                ui.weak(match count {
                    0 => "No instances yet. Duplicate it, or pick it in the Components window.".to_owned(),
                    1 => "1 instance follows it.".to_owned(),
                    _ => format!("{count} instances follow it."),
                });
                if wide(ui, true, "Remove component", "Back to a plain frame. Its instances keep their look and stop following it.") {
                    act = Some(Act::Remove);
                }
            }
            Some(n) if n.source.is_some() => {
                let outer = name_of(self.doc.instance_root(n.id).and_then(|r| r.instance_of));
                headline(ui, false, match (n.instance_of, &outer) {
                    (Some(m), _) => format!("Instance of {}, inside another instance", name_of(Some(m)).unwrap_or_else(|| "a missing component".into())),
                    (None, Some(name)) => format!("Part of an instance of {name}"),
                    (None, None) => "Part of an instance; its main component is missing".into(),
                });
                ui.weak("Its look and text can be changed here. Place, size and layers follow the main component.");
                if wide(ui, n.instance_of.map_or(outer.is_some(), |m| self.doc.main(m).is_some()), "Go to main component", "Select what this mirrors in the main component.") {
                    act = Some(Act::GoToMain);
                }
                if wide(ui, outer.is_some(), "Reset overrides", "Back to how the main component has this layer.") {
                    act = Some(Act::Reset);
                }
            }
            Some(n) if n.instance_of.is_some() => {
                let main = name_of(n.instance_of);
                headline(ui, false, match &main {
                    Some(name) => format!("Instance of {name}"),
                    None => "Instance; its main component is missing".into(),
                });
                if main.is_none() {
                    ui.weak("It keeps its look but follows nothing. Detach it to edit its layers freely.");
                }
                if wide(ui, main.is_some(), "Go to main component", "Select the main component, on whatever page it is.") {
                    act = Some(Act::GoToMain);
                }
                if wide(ui, main.is_some(), "Reset overrides", "Everything back to how the main component has it. The size and place of the instance stay.") {
                    act = Some(Act::Reset);
                }
                if wide(ui, true, "Detach instance", "A plain frame that no longer follows the main component  (Ctrl+Alt+B)") {
                    act = Some(Act::Detach);
                }
            }
            _ => {
                if wide(ui, !self.sel.is_empty(), "Create component", "A frame becomes a main component; other layers are wrapped in one  (Ctrl+Alt+K)") {
                    act = Some(Act::Create);
                }
                let instances = self.sel.iter().any(|id| self.doc.find(*id).is_some_and(|n| n.is_instance()));
                if instances && wide(ui, true, "Detach instances", "Plain frames that no longer follow their main components  (Ctrl+Alt+B)") {
                    act = Some(Act::Detach);
                }
            }
        }
        act
    }

    /// The Components window: every main component of the document; a click places an instance.
    pub(super) fn components_window(&mut self, ctx: &egui::Context) {
        if !self.components.open {
            return;
        }
        let mains = self.doc.components();
        let pages = self.doc.page_count();
        let (mut open, mut place, mut create) = (true, None, false);
        let has_sel = !self.sel.is_empty();
        egui::Window::new("Components")
            .open(&mut open)
            .collapsible(false)
            .default_pos(self.canvas_rect.left_top() + vec2(16.0, 16.0))
            .resizable(false)
            .fixed_size([300.0, 380.0])
            .show(ctx, |ui| {
                let state = &mut self.components;
                ui.horizontal(|ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut state.filter).hint_text("Filter by name…").desired_width(ui.available_width() - 76.0));
                    if std::mem::take(&mut state.focus) {
                        edit.request_focus();
                    }
                    let button = ui.add_enabled(has_sel, egui::Button::new("Create").min_size(vec2(68.0, 0.0)));
                    if button.on_hover_text("Make a main component of the selection  (Ctrl+Alt+K)").clicked() {
                        create = true;
                    }
                });
                ui.add_space(4.0);
                if mains.is_empty() {
                    ui.weak("No components yet. Select a frame and press Ctrl+Alt+K: it becomes a main component, and every copy of it is an instance that follows its changes.");
                } else {
                    ui.weak("Click a component to place an instance of it in the middle of the view.");
                }
                ui.add_space(4.0);
                let filter = state.filter.trim().to_lowercase();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (page, id, name) in &mains {
                        if !filter.is_empty() && !name.to_lowercase().contains(&filter) {
                            continue;
                        }
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
                        if resp.hovered() {
                            ui.painter().rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        let painter = ui.painter().with_clip_rect(rect);
                        component_icon(&painter, pos2(rect.left() + 15.0, rect.center().y), true, PURPLE);
                        let text = painter.text(pos2(rect.left() + 32.0, rect.center().y), Align2::LEFT_CENTER, name, FontId::proportional(14.0), ui.visuals().text_color());
                        if pages > 1 {
                            painter.text(pos2(text.right() + 8.0, rect.center().y), Align2::LEFT_CENTER, self.doc.page_name(*page), FontId::proportional(11.5), ui.visuals().weak_text_color());
                        }
                        if resp.on_hover_text("Place an instance").clicked() {
                            place = Some(*id);
                        }
                    }
                });
            });
        self.components.open = open;
        if create {
            self.create_component();
        }
        if let Some(id) = place {
            self.insert_instance(id, None);
        }
    }

    /// `KANVAS_BENCH_COMPONENT`: a main component "Button" (a rounded rect and a label) in a
    /// frame, an instance twice its size beside it, and below the frame an instance with a
    /// label and fill of its own; all opened in the Layers panel. Result: ids of [main, its rect, its label, the wide instance, the
    /// instance with overrides, the frame].
    pub(super) fn bench_component(&mut self) -> Vec<u64> {
        let (x, y) = (-1500.0, 2000.0);
        let mut make = |kind: Kind, name: &str, (nx, ny, w, h): (f32, f32, f32, f32), fill: Option<[u8; 4]>| {
            let mut n = Node::new(self.doc.alloc_id(), kind, x + nx, y + ny);
            (n.name, n.w, n.h, n.fill) = (name.into(), w, h, fill);
            n
        };
        let stage = make(Kind::Frame, "Component demo", (0.0, 0.0, 900.0, 520.0), Some([250, 250, 250, 255]));
        let button = make(Kind::Frame, "Button", (60.0, 70.0, 220.0, 72.0), None);
        let mut back = make(Kind::Rect, "Background", (60.0, 70.0, 220.0, 72.0), Some([13, 153, 255, 255]));
        back.radius = 16.0;
        let mut label = make(Kind::Text, "Label", (118.0, 90.0, 104.0, 32.0), Some([255, 255, 255, 255]));
        (label.text, label.font_size, label.weight) = ("Button".into(), 26.0, 700);
        let (stage_id, button_id, back_id, label_id, b) = (stage.id, button.id, back.id, label.id, stage.bounds());
        self.doc.insert(stage, None);
        self.doc.insert(button, Some(stage_id));
        self.doc.insert(back, Some(button_id));
        self.doc.insert(label, Some(button_id));
        self.zoom_to((b.0 - 40.0, b.1 - 60.0, b.2 + 40.0, b.3 + 190.0), 1.0);
        self.fit_pending = false;
        // From here on the same paths a user takes: create, place, resize, restyle.
        self.sel = vec![button_id];
        self.create_component();
        let o = self.canvas_rect.min;
        let wide = self.insert_instance(button_id, Some(self.view.to_screen(o, x + 500.0, y + 240.0))).unwrap_or_default();
        // Each step is a frame of its own for a user; instances are brought in line once per frame.
        self.sync_components();
        self.edit_selected(|n| (n.w, n.h) = (n.w * 2.0, n.h * 2.0));
        self.sync_components();
        let own = self.insert_instance(button_id, Some(self.view.to_screen(o, x + 240.0, y + 620.0))).unwrap_or_default();
        let parts: Vec<u64> = self.doc.find(own).map(|n| n.children.iter().map(|c| c.id).collect()).unwrap_or_default();
        if let [rect, text] = parts[..] {
            self.sel = vec![rect];
            self.edit_selected(|n| n.fill = Some([20, 174, 92, 255]));
            self.sel = vec![text];
            self.edit_selected(|n| n.text = "Buy now".into());
        }
        self.expanded.extend([stage_id, button_id, wide, own]);
        self.sel.clear();
        vec![button_id, back_id, label_id, wide, own, stage_id]
    }
}
