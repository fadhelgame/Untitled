//! Constraints in the app: the section of the Design panel that sets them. What they do when
//! a frame is resized is in `crate::layout`.

use super::*;
use crate::layout::Constraint;

/// A choice per axis (horizontal, vertical). Shown: `None` = the selection is mixed. Picked:
/// `None` = left as it is.
pub(super) type Pair = (Option<Constraint>, Option<Constraint>);

const PINNED: &str = "Follows the main component: set the constraints of its layers there.";

impl App {
    /// What the Constraints section shows: the constraints of the selection, when all of it
    /// sits directly in a frame. Top-level layers have nothing to be constrained to.
    pub(super) fn constraints_shown(&self) -> Option<Pair> {
        let nodes: Vec<&Node> = self.sel.iter().filter_map(|id| self.doc.find(*id)).collect();
        let first = nodes.first()?;
        if self.sel.iter().any(|id| self.doc.parent_of(*id).is_none()) {
            return None;
        }
        let same = |f: &dyn Fn(&Node) -> Constraint| nodes.iter().all(|n| f(n) == f(first)).then(|| f(first));
        Some((same(&|n| n.constrain_h), same(&|n| n.constrain_v)))
    }

    /// Set the constraints of everything selected, as one undo step.
    pub(super) fn set_constraints(&mut self, (h, v): Pair) {
        if self.refuse_pinned() {
            return;
        }
        let differs = |n: &Node| h.is_some_and(|h| n.constrain_h != h) || v.is_some_and(|v| n.constrain_v != v);
        if !self.sel.iter().any(|id| self.doc.find(*id).is_some_and(differs)) {
            return;
        }
        let pre = self.doc.clone();
        for id in self.sel.clone() {
            if let Some(n) = self.doc.find_mut(id) {
                (n.constrain_h, n.constrain_v) = (h.unwrap_or(n.constrain_h), v.unwrap_or(n.constrain_v));
            }
        }
        self.push_undo(pre);
    }

    /// `KANVAS_BENCH_CONSTRAINTS`: a frame of 600×300 with five layers, one per horizontal
    /// constraint, zoomed so that it can grow by 200×100. Result: ids of [the frame, sidebar
    /// (left, top and bottom), header (left and right, top), badge (center, center), button
    /// (right, bottom), picture (scale, scale)].
    pub(super) fn bench_constraints(&mut self) -> Vec<u64> {
        use Constraint::*;
        let (x, y) = (-1500.0, 5400.0);
        let mut make = |kind: Kind, name: &str, (nx, ny, w, h): (f32, f32, f32, f32), fill: [u8; 3], c: (Constraint, Constraint)| {
            let mut n = Node::new(self.doc.alloc_id(), kind, x + nx, y + ny);
            (n.name, n.w, n.h, n.fill, n.radius) = (name.into(), w, h, Some([fill[0], fill[1], fill[2], 255]), 6.0);
            (n.constrain_h, n.constrain_v) = c;
            n
        };
        let stage = make(Kind::Frame, "Constraints demo", (0.0, 0.0, 600.0, 300.0), [250, 250, 250], (Min, Min));
        let layers = [
            make(Kind::Rect, "Sidebar", (20.0, 20.0, 80.0, 260.0), [13, 153, 255], (Min, Stretch)),
            make(Kind::Rect, "Header", (120.0, 20.0, 460.0, 40.0), [20, 174, 92], (Stretch, Min)),
            make(Kind::Ellipse, "Badge", (310.0, 130.0, 80.0, 40.0), [255, 150, 50], (Center, Center)),
            make(Kind::Rect, "Button", (500.0, 240.0, 80.0, 40.0), [151, 71, 255], (Max, Max)),
            make(Kind::Rect, "Picture", (120.0, 80.0, 120.0, 60.0), [242, 72, 72], (Scale, Scale)),
        ];
        let mut ids = vec![stage.id];
        self.doc.insert(stage, None);
        for n in layers {
            ids.push(n.id);
            self.doc.insert(n, Some(ids[0]));
        }
        self.zoom_to((x - 60.0, y - 60.0, x + 860.0, y + 460.0), 1.0);
        self.fit_pending = false;
        self.expanded.insert(ids[0]);
        self.sel = vec![ids[0]];
        self.tool = Tool::Select;
        ids
    }
}

/// The Constraints section: the box with a bar per side, and a dropdown per axis. For a part
/// of an instance (`pinned`) it only shows. Result: what was picked.
pub(super) fn constraints_section(ui: &mut egui::Ui, shown: Pair, pinned: bool) -> Pair {
    let mut picked = (None, None);
    section(ui, "Constraints");
    ui.add_enabled_ui(!pinned, |ui| {
        ui.horizontal(|ui| {
            let side = 64.0;
            let boxed = constraint_box(ui, side, shown);
            picked = (picked.0.or(boxed.0), picked.1.or(boxed.1));
            let width = ui.available_width() - 8.0;
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 8.0;
                for horizontal in [true, false] {
                    let now = if horizontal { shown.0 } else { shown.1 };
                    let combo = egui::ComboBox::from_id_salt(if horizontal { "constrain_h" } else { "constrain_v" })
                        .width(width)
                        .selected_text(now.map_or("Mixed", |c| c.label(horizontal)))
                        .show_ui(ui, |ui| {
                            for c in Constraint::ALL {
                                if ui.selectable_label(now == Some(c), c.label(horizontal)).clicked() {
                                    *(if horizontal { &mut picked.0 } else { &mut picked.1 }) = Some(c);
                                }
                            }
                        });
                    combo.response.on_hover_text(if horizontal {
                        "Horizontal: what this layer keeps when its frame gets wider or narrower"
                    } else {
                        "Vertical: what this layer keeps when its frame gets taller or shorter"
                    });
                }
            });
        });
    });
    if pinned {
        ui.weak(PINNED);
    } else if shown.0.is_none() || shown.1.is_none() {
        ui.weak("Mixed — a choice applies to all selected layers.");
    }
    picked
}

/// The frame in small with the layer in its middle: a bar from each side of the layer to the
/// frame, and a cross in the layer for the centre. The lit bars are what is pinned; a click on
/// a bar pins that one, Shift+click pins (or lets go of) a side beside the opposite one.
fn constraint_box(ui: &mut egui::Ui, side: f32, shown: Pair) -> Pair {
    use Constraint::*;
    let (rect, resp) = ui.allocate_exact_size(vec2(side, side), Sense::click());
    let enabled = ui.is_enabled();
    let p = ui.painter();
    p.rect_filled(rect, CornerRadius::same(8), Color32::from_gray(48));
    let inner = Rect::from_center_size(rect.center(), vec2(side * 0.36, side * 0.36));
    p.rect_stroke(inner, CornerRadius::same(3), Stroke::new(1.0_f32, Color32::from_gray(if enabled { 120 } else { 80 })), StrokeKind::Inside);
    let c = rect.center();
    let arm = inner.width() * 0.5 - 4.0;
    // (zone that takes the click, the bar drawn in it, horizontal axis?, what it stands for)
    let bars = [
        (Rect::from_min_max(pos2(rect.left(), inner.top()), pos2(inner.left(), inner.bottom())), [pos2(rect.left() + 6.0, c.y), pos2(inner.left() - 4.0, c.y)], true, Min),
        (Rect::from_min_max(pos2(inner.right(), inner.top()), pos2(rect.right(), inner.bottom())), [pos2(inner.right() + 4.0, c.y), pos2(rect.right() - 6.0, c.y)], true, Max),
        (Rect::from_min_max(pos2(inner.left(), rect.top()), pos2(inner.right(), inner.top())), [pos2(c.x, rect.top() + 6.0), pos2(c.x, inner.top() - 4.0)], false, Min),
        (Rect::from_min_max(pos2(inner.left(), inner.bottom()), pos2(inner.right(), rect.bottom())), [pos2(c.x, inner.bottom() + 4.0), pos2(c.x, rect.bottom() - 6.0)], false, Max),
        (inner, [pos2(c.x - arm, c.y), pos2(c.x + arm, c.y)], true, Center),
        (inner, [pos2(c.x, c.y - arm), pos2(c.x, c.y + arm)], false, Center),
    ];
    let pointer = resp.hover_pos().filter(|_| enabled);
    // In the layer the two arms of the cross share the zone: the nearer one counts.
    let over = |zone: Rect, horizontal: bool, what: Constraint| {
        pointer.is_some_and(|m| zone.contains(m) && (what != Center || ((m.y - c.y).abs() <= (m.x - c.x).abs()) == horizontal))
    };
    let mut picked = (None, None);
    for (zone, bar, horizontal, what) in bars {
        let now = if horizontal { shown.0 } else { shown.1 };
        let lit = now == Some(what) || (now == Some(Stretch) && what != Center);
        let hot = over(zone, horizontal, what);
        let color = match (enabled, lit, hot) {
            (false, true, _) => Color32::from_gray(150),
            (false, false, _) => Color32::from_gray(80),
            (true, true, _) => ACCENT,
            (true, false, true) => Color32::from_gray(220),
            (true, false, false) => Color32::from_gray(120),
        };
        p.line_segment(bar, Stroke::new(if lit { 2.5_f32 } else { 1.5 }, color));
        if hot && resp.clicked() {
            let other = if what == Min { Max } else { Min };
            let shift = ui.input(|i| i.modifiers.shift);
            let pick = match now {
                _ if what == Center => Center,
                Some(c) if shift && c == other => Stretch,
                Some(Stretch) if shift => other,
                _ => what,
            };
            *(if horizontal { &mut picked.0 } else { &mut picked.1 }) = Some(pick);
        }
    }
    if enabled {
        resp.on_hover_text("Click a bar to pin the layer to that side of its frame, or the cross to keep it centered. Shift+click pins both sides.");
    }
    picked
}
