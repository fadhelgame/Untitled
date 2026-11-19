//! `--bench` with `KANVAS_BENCH_LAYERS=<n>`: what a large artboard costs per frame.
//!
//! One artboard with `n` small rectangles (5000 when the value is not a number) and one
//! auto-layout button. Timed, each over some forty frames: the canvas at rest with nothing
//! selected, with every layer of the artboard selected (the Design panel shows them all),
//! one rectangle dragged around inside the artboard, and typing in the label of the button.

use crate::doc::{Kind, Node};
use crate::layout::{Align, AutoLayout, Sizing};
use eframe::egui;

thread_local! {
    /// Frame times of the phase that is being timed.
    static TIMES: std::cell::RefCell<Vec<f32>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn report(what: &str) {
    let mut v = TIMES.take();
    v.sort_by(|a, b| a.total_cmp(b));
    let avg = v.iter().sum::<f32>() / v.len().max(1) as f32;
    eprintln!("LAYERS {what}: avg {avg:.2} ms, p95 {:.2} ms, max {:.2} ms ({} frames)", v.get(v.len() * 95 / 100).copied().unwrap_or(0.0), v.last().copied().unwrap_or(0.0), v.len());
}

impl super::App {
    /// Builds the artboard. Result: its id, the id of the rectangle that gets dragged and the
    /// id of the label of the button.
    pub(super) fn bench_layers(&mut self, count: usize) -> Vec<u64> {
        let (x, y) = (-3000.0, 90000.0);
        let cols = 100;
        let rows = count.div_ceil(cols);
        let stage = self.doc.alloc_id();
        let mut f = Node::new(stage, Kind::Frame, x, y);
        (f.name, f.w, f.h, f.fill) = ("Many layers".into(), cols as f32 * 24.0 + 24.0, rows as f32 * 24.0 + 120.0, Some([248, 248, 250, 255]));
        for i in 0..count {
            let mut n = Node::new(self.doc.alloc_id(), Kind::Rect, x + 14.0 + (i % cols) as f32 * 24.0, y + 14.0 + (i / cols) as f32 * 24.0);
            let shade = (i * 37 % 200) as u8;
            (n.name, n.w, n.h, n.fill, n.stroke_w) = (format!("Layer {i}"), 20.0, 20.0, Some([40, shade, 255 - shade, 255]), 0.0);
            f.children.push(n);
        }
        let mut button = Node::new(self.doc.alloc_id(), Kind::Frame, x + 14.0, y + f.h - 80.0);
        (button.name, button.w, button.h, button.fill) = ("Button".into(), 120.0, 44.0, Some([13, 153, 255, 255]));
        (button.layout, button.size_h, button.size_v) = (Some(AutoLayout { pad: [12.0, 20.0, 12.0, 20.0], cross: Align::Center, ..Default::default() }), Sizing::Hug, Sizing::Hug);
        let mut label = Node::new(self.doc.alloc_id(), Kind::Text, button.x + 20.0, button.y + 12.0);
        (label.name, label.text, label.font_size, label.fill, label.w, label.h) = ("Label".into(), "Button".into(), 16.0, Some([255, 255, 255, 255]), 52.0, 20.0);
        let (first, text) = (f.children[cols + 3].id, label.id);
        button.children.push(label);
        f.children.push(button);
        let (w, h) = (f.w, f.h);
        self.doc.insert(f, None);
        self.zoom_to((x - 40.0, y - 40.0, x + w + 40.0, y + h + 40.0), 8.0);
        self.fit_pending = false;
        self.sel.clear();
        vec![stage, first, text]
    }

    /// `--bench` with `KANVAS_BENCH_REVEAL=1`: a layer far down a closed artboard is clicked on
    /// the canvas. The Layers panel is to open the artboard and scroll to the row of the layer.
    pub(super) fn bench_reveal_frame(&mut self, frame: u32, inject: &mut Vec<(u32, egui::Event)>) {
        match frame {
            191 => {
                let ids = self.bench_layers(300);
                let Some(c) = self.doc.find(ids[1]).map(|n| n.center()) else { return };
                let at = self.view.to_screen(self.canvas_rect.min, c.0, c.1);
                let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                inject.extend([(200, egui::Event::PointerMoved(at)), (202, button(true)), (203, button(false))]);
                eprintln!("REVEAL before the click: artboard open {}, list scrolled {:.0}", self.expanded.contains(&ids[0]), self.layers_scroll);
            }
            230 => {
                let sel: Vec<&str> = self.sel.iter().filter_map(|id| self.doc.find(*id)).map(|n| n.name.as_str()).collect();
                let open = self.sel.first().and_then(|id| self.doc.parent_of(*id)).is_some_and(|p| self.expanded.contains(&p));
                eprintln!("REVEAL after the click: selected {sel:?}, artboard open {open}, list scrolled {:.0} (want [Layer 103], true, its row 198 in the middle: near 30 × 198 less half the list)", self.layers_scroll);
            }
            _ => {}
        }
    }

    /// On every frame: starts the next phase and reports the one that ended.
    pub(super) fn bench_layers_frame(&mut self, frame: u32, ids: &[u64], inject: &mut Vec<(u32, egui::Event)>) {
        let (stage, first, text) = (ids[0], ids[1], ids[2]);
        if matches!(frame, 216..=258 | 276..=318 | 330..=368 | 382..=420) {
            TIMES.with_borrow_mut(|t| t.push(self.frame_ms));
        }
        match frame {
            // The pointer rests over the artboard all along, as it would in use: what is under
            // it is looked for on every frame.
            200 | 262 => inject.push((frame + 2, egui::Event::PointerMoved(self.canvas_rect.center() + egui::vec2(3.0, 5.0)))),
            259 => {
                report("at rest, nothing selected");
                self.sel = self.doc.find(stage).map(|f| f.children.iter().map(|c| c.id).collect()).unwrap_or_default();
            }
            319 => {
                report(&format!("at rest, {} layers selected", self.sel.len()));
                self.sel = vec![first];
                let Some(c) = self.doc.find(first).map(|n| n.center()) else { return };
                let from = self.view.to_screen(self.canvas_rect.min, c.0, c.1);
                let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                inject.extend([(322, egui::Event::PointerMoved(from)), (324, button(from, true))]);
                for i in 0..42u32 {
                    let t = i as f32 / 42.0 * std::f32::consts::TAU;
                    inject.push((326 + i, egui::Event::PointerMoved(from + egui::vec2(1.0 - t.cos(), t.sin()) * 40.0)));
                }
                inject.push((370, button(from, false)));
            }
            372 => {
                report("one layer dragged inside the artboard");
                self.sel = vec![text];
                self.start_text_edit(text, self.doc.clone());
                inject.extend((0..10u32).map(|i| (380 + i * 4, egui::Event::Text("x".into()))));
            }
            421 => {
                report("typing in the label of the auto-layout button");
                let label = self.doc.find(text).map(|n| (n.text.clone(), n.w));
                let button = self.doc.parent_of(text).and_then(|p| self.doc.find(p)).map(|b| b.w);
                eprintln!("LAYERS label {label:?}, button width {button:?} (it hugs the label: label + 40)");
                self.finish_text_edit();
                self.sel.clear();
            }
            _ => {}
        }
    }
}
