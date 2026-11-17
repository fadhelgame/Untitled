//! `--bench <out.png>` with `KANVAS_PERF` set: time the canvas instead of taking screenshots.
//!
//! The document is the one given on the command line, or the stress-test document. The run goes
//! through a fixed list of phases — loading (until every image is decoded), a canvas that stands
//! still, panning, zooming, and dragging one object with the mouse — and writes the time spent
//! per frame in each of them to `<out>.txt`.

use super::{App, Bench};
use eframe::egui::{self, vec2};
use std::time::Instant;

/// Frames per timed phase.
const LEN: u32 = 120;
/// Longest wait for images, in frames.
const LOAD_MAX: u32 = 6000;
const PHASES: [&str; 5] = ["load", "still", "pan", "zoom", "drag"];

/// Parts of a frame that are timed on their own (see `span`).
pub(crate) const PARTS: [&str; 6] = ["update", "poll", "canvas", "text", "vtext", "upload"];
static SPENT: [std::sync::atomic::AtomicU64; 6] = [const { std::sync::atomic::AtomicU64::new(0) }; 6];

/// Things counted per frame: text layouts built, font atlas rebuilds noticed.
pub(crate) static COUNT: [std::sync::atomic::AtomicU64; 2] = [const { std::sync::atomic::AtomicU64::new(0) }; 2];

pub(crate) fn count(what: usize) {
    COUNT[what].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) struct Span(usize, Instant);

/// Time the code up to the end of the returned value's scope as part `PARTS[part]`.
pub(crate) fn span(part: usize) -> Span {
    Span(part, Instant::now())
}

impl Drop for Span {
    fn drop(&mut self) {
        SPENT[self.0].fetch_add(self.1.elapsed().as_micros() as u64, std::sync::atomic::Ordering::Relaxed);
    }
}

fn take_spent() -> [f32; 6] {
    std::array::from_fn(|i| SPENT[i].swap(0, std::sync::atomic::Ordering::Relaxed) as f32 / 1000.0)
}

#[derive(Default)]
pub(super) struct State {
    /// Where the time of the frame before this one went, and of the slow frames of this phase.
    parts: [f32; 6],
    /// Sum of `parts` over the phase.
    total: [f32; 6],
    counts: [u64; 2],
    fill: f32,
    slow: String,

    phase: usize,
    /// Frame at which the current phase began.
    mark: u32,
    /// Frames in a row with nothing left to decode.
    quiet: u32,
    started: Option<Instant>,
    samples: Vec<f32>,
    base: Option<super::View>,
    report: String,
}

fn line(name: &str, samples: &[f32], extra: &str) -> String {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let at = |q: usize| s.get((s.len() * q / 100).min(s.len().saturating_sub(1))).copied().unwrap_or(0.0);
    let avg = s.iter().sum::<f32>() / s.len().max(1) as f32;
    let slow = s.iter().filter(|v| **v > 16.7).count();
    format!("{name:<6} frames={:<5} avg_ms={avg:<7.2} p50_ms={:<7.2} p95_ms={:<7.2} max_ms={:<8.2} over_16ms={slow}{extra}\n", s.len(), at(50), at(95), s.last().copied().unwrap_or(0.0))
}

impl App {
    pub(super) fn run_perf(&mut self, ctx: &egui::Context, b: &mut Bench) {
        let frame = b.frame;
        if b.perf.phase >= PHASES.len() {
            return;
        }
        if frame == 1 && self.doc.count() == 0 {
            self.stress_test();
        }
        if frame < 4 {
            // The canvas size is known after the first frames.
            self.fit_all();
            b.perf.mark = frame;
            b.perf.started = Some(Instant::now());
            return;
        }
        let p = &mut b.perf;
        let at = frame - p.mark;
        // `frame_ms` is the cost of the frame before this one.
        if at >= 2 {
            p.samples.push(self.frame_ms);
            if self.frame_ms > 12.0 {
                let parts: Vec<String> = PARTS.iter().zip(p.parts).map(|(n, v)| format!("{n}={v:.1}")).collect();
                p.slow += &format!("  slow frame {at}: {:.1} ms ({}) layouts={} atlas_rebuilt={} atlas_fill={:.2} zoom={:.3}
", self.frame_ms, parts.join(" "), p.counts[0], p.counts[1], p.fill, self.view.zoom);
            }
        }
        p.parts = take_spent();
        for (sum, v) in p.total.iter_mut().zip(p.parts) {
            *sum += v;
        }
        p.counts = std::array::from_fn(|i| COUNT[i].swap(0, std::sync::atomic::Ordering::Relaxed));
        p.fill = ctx.fonts(|f| f.font_atlas_fill_ratio());
        let center = self.canvas_rect.center();
        let done = match PHASES[p.phase] {
            "load" => {
                p.quiet = if self.ext.busy() + self.fonts.pending() == 0 { p.quiet + 1 } else { 0 };
                p.quiet >= 30 || at > LOAD_MAX
            }
            "still" => at > LEN,
            "pan" => {
                let base = *p.base.get_or_insert(self.view);
                let t = at as f32 / LEN as f32 * std::f32::consts::TAU;
                self.view.pan = base.pan + vec2(t.cos() - 1.0, t.sin()) * 300.0;
                at > LEN
            }
            "zoom" => {
                self.zoom_at(center, if at <= LEN / 2 { 1.03 } else { 1.0 / 1.03 });
                at > LEN
            }
            _ => {
                if at == 0 {
                    // Press on an object in the middle of the document and carry it in a circle.
                    let top = self.doc.nodes.get(self.doc.nodes.len() / 2);
                    let target = top.map(|n| n.children.first().unwrap_or(n)).map(|n| n.center());
                    if let Some((x, y)) = target {
                        let from = self.view.to_screen(self.canvas_rect.min, x, y);
                        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                        b.inject.extend([(frame + 1, egui::Event::PointerMoved(from)), (frame + 3, button(from, true))]);
                        for i in 0..LEN - 12 {
                            let t = i as f32 / (LEN - 12) as f32 * std::f32::consts::TAU;
                            b.inject.push((frame + 5 + i, egui::Event::PointerMoved(from + vec2(1.0 - t.cos(), t.sin()) * 60.0)));
                        }
                        b.inject.push((frame + LEN - 4, button(from, false)));
                    }
                }
                at > LEN
            }
        };
        if !done {
            return;
        }
        let p = &mut b.perf;
        let extra = match PHASES[p.phase] {
            "load" => format!(" seconds={:.2}", p.started.map_or(0.0, |t| t.elapsed().as_secs_f32())),
            _ => String::new(),
        };
        if PHASES[p.phase] == "load" {
            // The last frames of the load phase only waited for things to settle.
            let keep = p.samples.len().saturating_sub(30).max(1);
            p.samples.truncate(keep);
        }
        let n = p.samples.len().max(1) as f32;
        let parts: Vec<String> = PARTS.iter().zip(std::mem::take(&mut p.total)).take(5).map(|(name, v)| format!("{name}={:.2}", v / n)).collect();
        let extra = format!("{extra} | avg {}", parts.join(" "));
        p.report += &line(PHASES[p.phase], &p.samples, &extra);
        if std::env::var_os("KANVAS_PERF_SERIES").is_some() {
            p.report += &format!("  series: {}
", p.samples.iter().map(|v| format!("{v:.0}")).collect::<Vec<_>>().join(" "));
        }
        p.report += &std::mem::take(&mut p.slow);
        p.samples.clear();
        p.base = None;
        p.mark = frame;
        p.phase += 1;
        if PHASES.get(p.phase) == Some(&"drag") {
            self.fit_all();
        }
        if p.phase == PHASES.len() {
            let head = format!(
                "objects={} top_level={} images={} canvas={:.0}x{:.0} ppp={}\n",
                self.doc.count(),
                self.doc.nodes.len(),
                self.doc.assets.len(),
                self.canvas_rect.width(),
                self.canvas_rect.height(),
                ctx.pixels_per_point()
            );
            let _ = std::fs::write(b.out.with_extension("txt"), head + &p.report);
            self.dirty = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
