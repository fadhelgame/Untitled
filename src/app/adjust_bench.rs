//! `--bench` with `KANVAS_BENCH_ADJUST`: image adjustments (see `crate::adjust`) on the canvas
//! and in the export, and what dragging a slider on a large photo costs.

use crate::adjust::{Adjust, FIELDS};
use crate::doc::{Kind, Node, Shadow};
use eframe::egui;

/// A picture with what adjustments are judged by: a sky that runs from deep blue to a nearly
/// white sun, a dark foreground with detail in the shadows, a face-like oval in skin tones,
/// strong colour patches and a grey ramp.
fn test_picture(w: u32, h: u32) -> image::RgbImage {
    let mix = |a: [f32; 3], b: [f32; 3], t: f32| [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t];
    let smooth = |t: f32| {
        let t = t.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    };
    image::RgbImage::from_fn(w, h, |px, py| {
        let (x, y) = (px as f32 / w as f32, py as f32 / h as f32);
        // Sky, with the sun in the upper right.
        let mut c = mix([38.0, 84.0, 170.0], [196.0, 222.0, 244.0], smooth(y / 0.62));
        let sun = (-(((x - 0.8) * 1.5).powi(2) + (y - 0.2).powi(2)) / 0.012).exp();
        c = mix(c, [255.0, 250.0, 232.0], sun.min(1.0));
        let glow = (-(((x - 0.8) * 1.5).powi(2) + (y - 0.2).powi(2)) / 0.09).exp();
        c = mix(c, [255.0, 214.0, 150.0], glow * 0.45);
        // Hills: dark greens and browns with some texture.
        let ridge = 0.6 + 0.05 * (x * 9.0).sin() + 0.03 * (x * 23.0 + 1.0).sin();
        if y > ridge {
            let d = (y - ridge) / (1.0 - ridge);
            let texture = ((x * 140.0).sin() * (y * 190.0).cos() * 0.5 + 0.5) * 14.0;
            c = mix([58.0, 92.0, 48.0], [22.0, 30.0, 26.0], smooth(d * 1.3));
            c = [c[0] + texture * 0.7, c[1] + texture, c[2] + texture * 0.4];
        }
        // A face-like oval, lit from the right.
        let (fx, fy) = ((x - 0.27) / 0.13, (y - 0.5) / 0.3);
        let face = fx * fx + fy * fy;
        if face < 1.0 {
            let light = smooth(0.5 + fx * 0.6 - fy * 0.2);
            let skin = mix([126.0, 78.0, 58.0], [244.0, 200.0, 170.0], light);
            c = mix(skin, c, smooth((face - 0.92) / 0.08));
        }
        // Colour patches and a grey ramp along the bottom.
        if y > 0.86 && y < 0.93 {
            const PATCHES: [[f32; 3]; 8] =
                [[220.0, 40.0, 40.0], [240.0, 140.0, 30.0], [240.0, 220.0, 50.0], [50.0, 170.0, 70.0], [40.0, 180.0, 200.0], [50.0, 80.0, 210.0], [150.0, 60.0, 190.0], [230.0, 70.0, 150.0]];
            c = PATCHES[((x * 8.0) as usize).min(7)];
        } else if y >= 0.93 {
            c = [x * 255.0; 3];
        }
        image::Rgb(c.map(|v| v.clamp(0.0, 255.0) as u8))
    })
}

fn png(img: &image::RgbImage) -> Vec<u8> {
    let mut out = Vec::new();
    let _ = image::DynamicImage::ImageRgb8(img.clone()).write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png);
    out
}

fn stats(mut v: Vec<f32>) -> (f32, f32, f32) {
    v.sort_by(|a, b| a.total_cmp(b));
    let avg = v.iter().sum::<f32>() / v.len().max(1) as f32;
    (avg, v.get(v.len() * 95 / 100).copied().unwrap_or(0.0), v.last().copied().unwrap_or(0.0))
}

thread_local! {
    /// The drag of `=drag`: time per frame (the app's own, and from frame to frame).
    static TIMES: std::cell::RefCell<(Vec<f32>, Vec<f32>, Option<std::time::Instant>)> = const { std::cell::RefCell::new((Vec::new(), Vec::new(), None)) };
}

impl super::App {
    /// `--bench` with `KANVAS_BENCH_ADJUST`: a frame with the same picture again and again. The
    /// first row: as it is, a soft "film" look, black and white, one cropped, rounded and
    /// rotated, one with a drop shadow and an inner shadow, one flipped and half see-through;
    /// then a row per slider at -100, -60, -30, 30, 60 and 100. `bench_adjust_finish` puts the
    /// export of the frame beside it. `KANVAS_BENCH_ADJUST_PHOTO=<file>` uses that image
    /// instead of the made-up one (the file is only read).
    ///
    /// `=view:x0,y0,x1,y1` shows that part of the stage (the export is right of the frame),
    /// `=panel` selects the film look so the panel shows its Adjust section, `=ui` then works
    /// its sliders with the mouse (see `bench_adjust_ui`), and `=drag` is a 12 megapixel photo
    /// alone whose sliders are moved for 200 frames (see `bench_adjust_frame`).
    pub(super) fn bench_adjust(&mut self, ctx: &egui::Context, mode: &str, inject: &mut Vec<(u32, egui::Event)>) -> Vec<u64> {
        let (x, y) = (-3000.0, 70000.0);
        if mode == "drag" {
            let start = std::time::Instant::now();
            let mut jpg = Vec::new();
            let big = test_picture(4000, 3000);
            let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 90).encode(&big, 4000, 3000, image::ExtendedColorType::Rgb8);
            // `KANVAS_BENCH_ADJUST_FX=shadow|cutout|blur|frame`: the photo has a drop shadow (as
            // a JPEG, or `cutout`: as a PNG with see-through corners, whose shadow is made from
            // its pixels) or a layer blur (textures made from the export's SVG), or lies in a
            // top-level frame (whose meshes are kept: see `render::frame_cached`).
            let fx = std::env::var("KANVAS_BENCH_ADJUST_FX").unwrap_or_default();
            if fx == "cutout" {
                let mut cut = image::DynamicImage::ImageRgb8(big).into_rgba8();
                for (px, py, p) in cut.enumerate_pixels_mut() {
                    let (dx, dy) = (px as f32 / 2000.0 - 1.0, py as f32 / 1500.0 - 1.0);
                    p[3] = if dx * dx + dy * dy > 1.0 { 0 } else { 255 };
                }
                jpg.clear();
                let _ = cut.write_to(&mut std::io::Cursor::new(&mut jpg), image::ImageFormat::Png);
            }
            let asset = self.doc.add_asset(jpg);
            let mut n = Node::new(self.doc.alloc_id(), Kind::Image, x, y);
            (n.name, n.w, n.h, n.image, n.stroke_w) = ("Large photo".into(), 1200.0, 900.0, Some(asset), 0.0);
            let id = n.id;
            match fx.as_str() {
                "shadow" | "cutout" => n.shadow = Some(Shadow { x: 0.0, y: 12.0, blur: 24.0, color: [0, 0, 0, 140] }),
                "blur" => n.blur = 6.0,
                _ => {}
            }
            let parent = (fx == "frame").then(|| {
                let mut f = Node::new(self.doc.alloc_id(), Kind::Frame, x - 100.0, y - 100.0);
                (f.name, f.w, f.h, f.fill) = ("Photo frame".into(), 1400.0, 1100.0, Some([250, 250, 250, 255]));
                let frame = f.id;
                self.doc.insert(f, None);
                frame
            });
            self.doc.insert(n, parent);
            self.zoom_to((x - 20.0, y - 20.0, x + 1220.0, y + 920.0), 32.0);
            self.fit_pending = false;
            self.sel = vec![id];
            eprintln!("ADJUST 12 MP photo made in {:.0} ms", start.elapsed().as_secs_f32() * 1000.0);
            return vec![id];
        }
        let photo = std::env::var_os("KANVAS_BENCH_ADJUST_PHOTO").and_then(|p| std::fs::read(p).ok());
        let (bytes, aspect) = match photo.as_ref().and_then(|b| Some((b, super::render::image_size(b)?))) {
            Some((bytes, (w, h))) => (bytes.clone(), w / h.max(1.0)),
            None => (png(&test_picture(900, 600)), 1.5),
        };
        let asset = self.doc.add_asset(bytes);
        let (tw, gap, label) = (240.0, 16.0, 20.0);
        let th = (tw / aspect).round();
        let (cols, rows) = (6, 1 + FIELDS.len());
        let (w, h) = (gap + cols as f32 * (tw + gap), gap + rows as f32 * (th + label + gap));
        let stage = self.doc.alloc_id();
        let mut f = Node::new(stage, Kind::Frame, x, y);
        (f.name, f.w, f.h, f.fill) = ("Adjust demo".into(), w, h, Some([244, 244, 245, 255]));
        self.doc.insert(f, None);
        let mut film = 0;
        let mut tile = |app: &mut Self, col: usize, row: usize, name: &str, edit: &dyn Fn(&mut Node)| {
            let (tx, ty) = (x + gap + col as f32 * (tw + gap), y + gap + row as f32 * (th + label + gap));
            let mut t = Node::new(app.doc.alloc_id(), Kind::Text, tx, ty);
            (t.text, t.name, t.font_size, t.weight, t.fill) = (name.into(), name.into(), 12.0, 500, Some([39, 39, 42, 255]));
            app.doc.insert(t, Some(stage));
            let mut n = Node::new(app.doc.alloc_id(), Kind::Image, tx, ty + label);
            (n.name, n.w, n.h, n.image, n.stroke_w) = (name.into(), tw, th, Some(asset), 0.0);
            edit(&mut n);
            if name == "Film look" {
                film = n.id;
            }
            app.doc.insert(n, Some(stage));
        };
        let set = |n: &mut Node, a: Adjust| n.adjust = a.normalized();
        tile(self, 0, 0, "Original", &|_| {});
        tile(self, 1, 0, "Film look", &|n| set(n, Adjust { exposure: 5.0, contrast: -15.0, saturation: -20.0, temperature: 12.0, highlights: -25.0, shadows: 30.0, ..Default::default() }));
        tile(self, 2, 0, "Black and white, contrast 30", &|n| set(n, Adjust { saturation: -100.0, contrast: 30.0, ..Default::default() }));
        tile(self, 3, 0, "Cropped, rounded, rotated", &|n| {
            (n.crop, n.radius, n.rotation) = (Some([0.1, 0.15, 0.7, 0.85]), 24.0, 8.0);
            (n.x, n.w, n.h) = (n.x + 30.0, tw - 60.0, th - 20.0);
            set(n, Adjust { exposure: 20.0, saturation: 40.0, ..Default::default() });
        });
        tile(self, 4, 0, "Shadow and inner shadow", &|n| {
            (n.radius, n.w, n.x) = (16.0, tw - 30.0, n.x + 10.0);
            n.shadow = Some(Shadow { x: 0.0, y: 8.0, blur: 16.0, color: [0, 0, 0, 120] });
            n.inner_shadow = Some(Shadow { x: 0.0, y: 0.0, blur: 28.0, color: [0, 0, 40, 220] });
            set(n, Adjust { temperature: -40.0, contrast: 25.0, ..Default::default() });
        });
        tile(self, 5, 0, "Flipped, 70 %, stroke", &|n| {
            (n.flip_h, n.opacity, n.stroke, n.stroke_w) = (true, 0.7, Some([24, 24, 27, 255]), 4.0);
            set(n, Adjust { tint: 40.0, shadows: 50.0, ..Default::default() });
        });
        for (row, (_, name)) in FIELDS.iter().enumerate() {
            for (col, v) in [-100.0, -60.0, -30.0, 30.0, 60.0, 100.0].into_iter().enumerate() {
                let mut values = [0.0; 7];
                values[row] = v;
                tile(self, col, row + 1, &format!("{name} {v:+.0}"), &|n| set(n, Adjust::from_values(values)));
            }
        }
        let part: Vec<f32> = mode.strip_prefix("view:").map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
        let view = if part.len() == 4 { (x + part[0], y + part[1], x + part[2], y + part[3]) } else { (x - 20.0, y - 20.0, x + w * 2.0 + 60.0, y + h + 20.0) };
        self.zoom_to(view, 32.0);
        self.fit_pending = false;
        if mode == "panel" || mode == "ui" {
            self.zoom_to((x, y, x + w, y + (th + label + gap) * 2.0), 32.0);
            self.sel = vec![film];
            // The Adjust section is below what fits in the panel.
            let p = egui::pos2(ctx.screen_rect().right() - 150.0, 500.0);
            let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, if mode == "ui" { -430.0 } else { -600.0 }), modifiers: Default::default() };
            inject.extend([(200, egui::Event::PointerMoved(p)), (204, wheel), (250, egui::Event::PointerMoved(self.canvas_rect.center_top() + egui::vec2(0.0, 40.0)))]);
        }
        vec![stage]
    }

    /// The second half of `bench_adjust`, once the fonts are loaded: the frame is exported (a
    /// PNG beside the screenshot, an SVG and a PDF) and the PNG is placed right of the frame.
    pub(super) fn bench_adjust_finish(&mut self, ctx: &egui::Context, stage: u64, out: &std::path::Path) {
        let Some((x, y, w, h)) = self.doc.find(stage).filter(|n| n.kind == Kind::Frame).map(|f| (f.x, f.y, f.w, f.h)) else { return };
        let start = std::time::Instant::now();
        let Some((svg, sw, sh)) = crate::export::to_svg(&self.doc, Some(stage), &mut self.fonts, ctx) else { return };
        let made = start.elapsed().as_secs_f32() * 1000.0;
        let again = std::time::Instant::now();
        let same = crate::export::to_svg(&self.doc, Some(stage), &mut self.fonts, ctx).is_some_and(|s| s.0 == svg);
        eprintln!("ADJUST svg made in {made:.0} ms ({} kB); again in {:.0} ms (the adjusted images are kept), the same: {same}", svg.len() / 1024, again.elapsed().as_secs_f32() * 1000.0);
        let _ = crate::export::save_raster(&svg, sw, sh, 1.0, false, &out.with_file_name("bench-adjust-export.png"));
        let _ = std::fs::write(out.with_file_name("bench-adjust-export.svg"), &svg);
        let _ = crate::export::save_pdf(&[(svg.clone(), sw, sh)], &out.with_file_name("bench-adjust-export.pdf"));
        if let Ok((png, _, _)) = crate::export::render_png(&svg, sw, sh, 2.0) {
            let asset = self.doc.add_asset(png);
            let mut n = Node::new(self.doc.alloc_id(), Kind::Image, x + w + 40.0, y);
            (n.name, n.w, n.h, n.image, n.stroke_w) = ("Adjust export".into(), w, h, Some(asset), 0.0);
            self.doc.insert(n, None);
        }
        eprintln!("ADJUST exported in {:.0} ms", start.elapsed().as_secs_f32() * 1000.0);
    }

    /// `=ui`, on every frame: the Exposure slider of the film look is dragged to the right
    /// with the mouse, held still for longer than successive changes are merged into one undo
    /// step, and dragged on; then the Contrast slider is double-clicked. What the node has and
    /// how many undo steps there are is printed before, between and after. (The sliders are
    /// where the panel puts them in a screenshot of 1700 x 1050 pixels.)
    pub(super) fn bench_adjust_ui(&mut self, ctx: &egui::Context, frame: u32, inject: &mut Vec<(u32, egui::Event)>) {
        let state = |app: &Self| (app.undo.len(), app.sel.first().and_then(|id| app.doc.find(*id)).and_then(|n| n.adjust).map(|a| (a.exposure, a.contrast)));
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        let mv = egui::Event::PointerMoved;
        // (Given in the pixels of a screenshot.)
        let px = |x: f32, y: f32| egui::pos2(x, y) / ctx.pixels_per_point();
        match frame {
            296 => {
                eprintln!("ADJUST ui before: {:?} (undo steps, exposure and contrast)", state(self));
                let (from, to) = (px(1527.0, 745.0), px(1590.0, 745.0));
                inject.extend([(298, mv(from)), (300, button(from, true))]);
                inject.extend((1..=10u32).map(|s| (300 + s * 2, mv(from + (to - from) * (s as f32 / 20.0)))));
                // Longer than the 0.8 s within which changes merge.
                inject.extend((11..=20u32).map(|s| (540 + s * 2, mv(from + (to - from) * (s as f32 / 20.0)))));
                inject.push((584, button(to, false)));
            }
            330 => eprintln!("ADJUST ui halfway through the drag: {:?}", state(self)),
            590 => {
                eprintln!("ADJUST ui after one drag with a rest in it: {:?} (want one undo step more than before)", state(self));
                let at = px(1560.0, 785.0);
                inject.extend([(596, mv(at)), (600, button(at, true)), (602, button(at, false)), (606, button(at, true)), (608, button(at, false)), (616, mv(self.canvas_rect.center_top() + egui::vec2(0.0, 40.0)))]);
            }
            630 => {
                eprintln!("ADJUST ui after a double click on Contrast: {:?} (want contrast 0)", state(self));
                self.do_undo();
                eprintln!("ADJUST ui after one undo: {:?} (want what it was before)", state(self));
                self.do_redo();
                // The same through the remote API: a partial change, a wrong one, and a reset.
                let id = self.sel.first().copied().unwrap_or(0);
                let text = self.doc.parent_of(id).and_then(|p| self.doc.find(p)).and_then(|p| p.children.first()).map_or(0, |t| t.id);
                for args in [
                    serde_json::json!({ "nodes": [{ "id": id, "adjust": { "saturation": -100, "shadows": 250 } }] }),
                    serde_json::json!({ "nodes": [{ "id": id, "adjust": { "hue": 10 } }] }),
                    serde_json::json!({ "nodes": [{ "id": text, "adjust": { "exposure": 10 } }] }),
                ] {
                    let answer = self.remote_bench("update_nodes", &args);
                    eprintln!("ADJUST remote update_nodes {args} -> {}", answer.chars().take(200).collect::<String>());
                }
                let read = self.remote_bench("get_document", &serde_json::json!({ "ids": [id] }));
                eprintln!("ADJUST remote get_document: adjust = {}", read.split("\"adjust\":").nth(1).map_or("none", |s| s.split('}').next().unwrap_or("")));
                let answer = self.remote_bench("update_nodes", &serde_json::json!({ "nodes": [{ "id": id, "adjust": null }] }));
                eprintln!("ADJUST remote reset -> {}; the node has {:?}", answer.chars().take(60).collect::<String>(), self.doc.find(id).and_then(|n| n.adjust));
                self.do_undo();
            }
            _ => {}

        }
    }

    /// `=drag`, on every frame, `at` frames after the photo was loaded: the exposure slider is
    /// moved a step per frame for 100 frames (tables only), then saturation with the shadows
    /// lifted for 100 more (the maths per pixel), as the panel would do it. The time of every
    /// frame and the adjusted textures that exist are printed at the end, and once more after
    /// the value has rested (when the full-size texture has replaced the preview).
    pub(super) fn bench_adjust_frame(&mut self, ctx: &egui::Context, id: u64, at: u32) {
        let textures = |app: &Self| (app.ext.adjusted(), ctx.tex_manager().read().num_allocated());
        match at {
            1 => eprintln!("ADJUST before the drag: {:?}", textures(self)),
            10..=209 => {
                let step = (at - 10) as f32;
                let wave = |s: f32| ((s * 4.0) % 400.0 - 200.0).abs() - 100.0;
                if let Some(n) = self.doc.find_mut(id) {
                    n.adjust = match step < 100.0 {
                        true => Adjust { exposure: wave(step), ..Default::default() },
                        false => Adjust { saturation: wave(step), shadows: 30.0, ..Default::default() },
                    }
                    .normalized();
                }
                self.touch();
                let now = std::time::Instant::now();
                TIMES.with_borrow_mut(|t| {
                    // (The frame time the app reports is that of the frame before.)
                    if at > 11 {
                        t.0.push(self.frame_ms);
                        t.1.extend(t.2.map(|last| (now - last).as_secs_f32() * 1000.0));
                    }
                    t.2 = Some(now);
                });
            }
            210 => {
                let (own, between) = TIMES.with_borrow(|t| (stats(t.0.clone()), stats(t.1.clone())));
                eprintln!("ADJUST drag of 200 steps on 12 MP: frame avg {:.2} ms, p95 {:.2} ms, max {:.2} ms; frame to frame avg {:.2} ms, p95 {:.2} ms, max {:.2} ms", own.0, own.1, own.2, between.0, between.1, between.2);
                eprintln!("ADJUST after 200 steps (finished, live, sources, MB), all textures: {:?}", textures(self));
            }
            270 | 330 => {
                let top = self.doc.parent_of(id).unwrap_or(id);
                eprintln!("ADJUST {} frames after the drag: {:?}; the kept meshes of its frame draw a preview texture: {:?}", at - 210, textures(self), self.ext.kept_preview(top));
            }
            _ => {}
        }
    }
}
