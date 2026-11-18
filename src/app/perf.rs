//! `--bench <out.png>` with `KANVAS_PERF` set: time the canvas instead of taking screenshots.
//!
//! The document is the one given on the command line, or the stress-test document. The run goes
//! through a fixed list of phases — loading (until every image is decoded), a canvas that stands
//! still, panning, zooming, dragging one object with the mouse, and standing still again — and writes the time spent
//! per frame in each of them to `<out>.txt`.

use super::{App, Bench};
use eframe::egui::{self, vec2};
use std::time::Instant;

/// Frames per timed phase (`KANVAS_PERF_LEN` sets another number: caches that let go of what
/// was not used for some hundred frames only show in a longer run).
fn len() -> u32 {
    static LEN: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *LEN.get_or_init(|| std::env::var("KANVAS_PERF_LEN").ok().and_then(|v| v.parse().ok()).unwrap_or(120).max(40))
}
/// Longest wait for images, in frames.
const LOAD_MAX: u32 = 6000;
const PHASES: [&str; 6] = ["load", "still", "pan", "zoom", "drag", "after"];

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
    /// Text layouts built over the phase.
    layouts: u64,
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
    shots: u32,
}

fn line(name: &str, samples: &[f32], extra: &str) -> String {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let at = |q: usize| s.get((s.len() * q / 100).min(s.len().saturating_sub(1))).copied().unwrap_or(0.0);
    let avg = s.iter().sum::<f32>() / s.len().max(1) as f32;
    let slow = s.iter().filter(|v| **v > 16.7).count();
    format!("{name:<6} frames={:<5} avg_ms={avg:<7.2} p50_ms={:<7.2} p95_ms={:<7.2} max_ms={:<8.2} over_16ms={slow}{extra}\n", s.len(), at(50), at(95), s.last().copied().unwrap_or(0.0))
}

/// `KANVAS_PERF_IDLE=<seconds>` (without `--bench`): counts the frames the app paints while
/// nothing happens. Called once per frame. After half the time (the document has loaded, its
/// textures have arrived) the frames of the other half are counted and printed, and the app
/// exits. An app at rest paints none: a number that keeps up with the display is a repaint loop.
pub(crate) fn idle_frame() {
    static FRAMES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static STARTED: std::sync::Once = std::sync::Once::new();
    FRAMES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    STARTED.call_once(|| {
        let Some(secs) = std::env::var("KANVAS_PERF_IDLE").ok().and_then(|v| v.parse::<f32>().ok()) else { return };
        std::thread::spawn(move || {
            let half = std::time::Duration::from_secs_f32(secs.max(1.0) * 0.5);
            std::thread::sleep(half);
            let before = FRAMES.load(std::sync::atomic::Ordering::Relaxed);
            std::thread::sleep(half);
            let painted = FRAMES.load(std::sync::atomic::Ordering::Relaxed) - before;
            eprintln!("IDLE {painted} frames painted in {:.1} s at rest ({:.1} per second)", half.as_secs_f32(), painted as f32 / half.as_secs_f32());
            std::process::exit(0);
        });
    });
}

impl App {
    pub(super) fn run_perf(&mut self, ctx: &egui::Context, b: &mut Bench) {
        let frame = b.frame;
        if b.perf.phase >= PHASES.len() {
            return;
        }
        if frame == 1 && std::env::var_os("KANVAS_PERF_ABOUT").is_some() {
            self.about = true;
        }
        if frame == 1 && self.doc.count() == 0 {
            self.stress_test();
        }
        if frame < 3 {
            // The canvas size is known after the first frame.
            self.fit_all();
            b.perf.mark = frame;
            b.perf.started = Some(Instant::now());
            take_spent();
            return;
        }
        // A picture of the still canvas and of the canvas after panning, to compare runs by eye.
        let shot = ctx.input(|i| i.events.iter().find_map(|e| if let egui::Event::Screenshot { image, .. } = e { Some(image.clone()) } else { None }));
        if let Some(img) = shot {
            let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            b.perf.shots += 1;
            let name = format!("{}-{}.png", b.out.file_stem().map_or("perf".into(), |s| s.to_string_lossy()), b.perf.shots);
            let _ = crate::export::save_rgba(&b.out.with_file_name(name), img.size[0] as u32, img.size[1] as u32, &rgba);
        }
        let p = &mut b.perf;
        let at = frame - p.mark;
        if at == len() - 20 && matches!(PHASES[p.phase], "still" | "pan" | "zoom" | "drag" | "after") && std::env::var_os("KANVAS_PERF_SHOT").is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
        }
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
        p.layouts += p.counts[0];
        p.fill = ctx.fonts(|f| f.font_atlas_fill_ratio());
        let center = self.canvas_rect.center();
        let done = match PHASES[p.phase] {
            "load" => {
                p.quiet = if self.ext.busy() + self.fonts.pending() == 0 { p.quiet + 1 } else { 0 };
                p.quiet >= 30 || at > LOAD_MAX
            }
            "still" | "after" => at > len(),
            "pan" => {
                let base = *p.base.get_or_insert(self.view);
                let t = at as f32 / len() as f32 * std::f32::consts::TAU;
                self.view.pan = base.pan + vec2(t.cos() - 1.0, t.sin()) * 300.0;
                at > len()
            }
            "zoom" => {
                self.zoom_at(center, if at <= len() / 2 { 1.03 } else { 1.0 / 1.03 });
                at > len()
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
                        for i in 0..len() - 12 {
                            let t = i as f32 / (len() - 12) as f32 * std::f32::consts::TAU;
                            b.inject.push((frame + 5 + i, egui::Event::PointerMoved(from + vec2(1.0 - t.cos(), t.sin()) * 60.0)));
                        }
                        b.inject.push((frame + len() - 4, button(from, false)));
                    }
                }
                at > len()
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
        let extra = format!("{extra} | avg {} | layouts={}", parts.join(" "), std::mem::take(&mut p.layouts));
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
            // `KANVAS_PERF_UNDO=<n>`: n edits, each an undo step, of one top-level layer after
            // the other; then the app holds still for a moment before it closes, so that its
            // memory can be read from outside with the steps in it.
            if let Some(n) = std::env::var("KANVAS_PERF_UNDO").ok().and_then(|v| v.parse::<usize>().ok()) {
                for i in 0..n {
                    let Some(id) = self.doc.nodes.get(i % self.doc.nodes.len().max(1)).map(|t| t.id) else { break };
                    let pre = self.doc.clone();
                    if let Some(node) = self.doc.find_mut(id) {
                        node.translate(1.0, 0.0);
                    }
                    self.push_undo(pre);
                }
                p.report += &format!("undo steps={}\n", self.undo.len());
                std::thread::sleep(std::time::Duration::from_millis(2500));
            }
            let kept = self.ext.kept();
            let head = format!("{head}kept_frames={} pieces={} vertices={} not_kept={}
", kept.0, kept.1, kept.2, kept.3);
            let _ = std::fs::write(b.out.with_extension("txt"), head + &p.report);
            self.dirty = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}
