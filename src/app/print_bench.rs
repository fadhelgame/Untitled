//! `--bench` with `KANVAS_BENCH_PRINT`: a page for print, exported with bleed, crop marks and
//! in CMYK (see `crate::print`).

use crate::doc::{Gradient, Kind, Node, Shadow, Stop};
use crate::print::Print;
use eframe::egui;

/// A photo-like picture for the bench page: soft colour fields with some grain, as JPEG (so
/// the CMYK export has a DCT image to convert).
fn bench_photo() -> Vec<u8> {
    let (w, h) = (600u32, 400u32);
    let img = image::RgbImage::from_fn(w, h, |px, py| {
        let (fx, fy) = (px as f32 / w as f32, py as f32 / h as f32);
        let wave = ((fx * 9.0).sin() * (fy * 7.0).cos() * 0.5 + 0.5) * 60.0;
        let grain = ((px * 37 + py * 91) % 17) as f32;
        image::Rgb([(40.0 + 200.0 * fx + grain) as u8, (60.0 + 120.0 * fy + wave) as u8, (200.0 - 150.0 * fx * fy + grain) as u8])
    });
    let mut jpg = Vec::new();
    let _ = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 88).encode(&img, w, h, image::ExtendedColorType::Rgb8);
    jpg
}

thread_local! {
    /// `KANVAS_BENCH_PRINT_DOC`: a document read from a file, to time its export. It is never
    /// shown, edited or saved.
    static EXTRA: std::cell::RefCell<Option<crate::doc::Doc>> = const { std::cell::RefCell::new(None) };
}

impl super::App {
    /// Are the fonts there that the pages of `bench_print` (and the document of
    /// `KANVAS_BENCH_PRINT_DOC`) use? Asks for them, as the pages may be out of view.
    pub(super) fn bench_print_ready(&mut self, pages: &[u64]) -> bool {
        let mut fonts = Vec::new();
        let mut collect = |n: &Node| {
            if n.kind == Kind::Text {
                fonts.push((n.font.clone(), n.weight, n.italic));
            }
        };
        pages.iter().filter_map(|id| self.doc.find(*id)).for_each(|n| n.walk(&mut collect));
        EXTRA.with_borrow(|doc| doc.iter().flat_map(|d| d.nodes.iter()).for_each(|n| n.walk(&mut collect)));
        fonts.sort();
        fonts.dedup();
        for (font, weight, italic) in &fonts {
            self.fonts.resolve(font, *weight, *italic);
        }
        fonts.iter().all(|(font, weight, italic)| self.fonts.settled(font, *weight, *italic))
    }

    /// `KANVAS_BENCH_PRINT_DOC=<file.kanvas>` with `KANVAS_BENCH_PRINT`: every frame of that
    /// document (its current page) is exported as well, as a design made in 300 dpi pixels with
    /// 3 mm bleed and crop marks: RGB, CMYK and CMYK flattened, each timed. The file is only read.
    fn bench_print_doc(&mut self, ctx: &egui::Context, out: &std::path::Path) {
        let Some(doc) = EXTRA.take() else { return };
        let print = Print { bleed_mm: 3.0, marks: true, ppi: 300.0, ..Default::default() };
        let frames = crate::export::pdf_pages(&doc);
        let start = std::time::Instant::now();
        let art: Vec<(String, f32, f32)> = frames.iter().filter_map(|id| crate::export::to_svg_bleed(&doc, Some(*id), print.bleed_px(), &mut self.fonts, ctx)).collect();
        let Some(first) = art.first() else { return };
        eprintln!("PRINT doc: {} pages, {}, svg made in {:.0} ms ({} MB)", art.len(), print.hint(first.1 - print.bleed_px() * 2.0, first.2 - print.bleed_px() * 2.0), start.elapsed().as_secs_f32() * 1000.0, art.iter().map(|a| a.0.len()).sum::<usize>() / (1 << 20));
        for (name, print) in [("doc-rgb.pdf", print.clone()), ("doc-cmyk.pdf", Print { cmyk: true, ..print.clone() }), ("doc-cmyk-flattened.pdf", Print { cmyk: true, flatten: true, ..print.clone() })] {
            let path = out.with_file_name(name);
            let start = std::time::Instant::now();
            let result = crate::print::save_pdf(&art, &print, &path);
            eprintln!("PRINT {name}: {:.1} s, {} MB, {result:?}", start.elapsed().as_secs_f32(), std::fs::metadata(&path).map_or(0, |m| m.len() >> 20));
        }
    }

    /// `--bench` with `KANVAS_BENCH_PRINT`: an A5 page (420 × 595, the Paper preset) with a
    /// gradient as its fill, a photo that runs off the right edge and a band that runs off the
    /// left and the bottom, text in pure black, in grey and in colour, a card with a drop
    /// shadow and a glass panel (background blur) over the photo, and two plain pages beside
    /// it for the multi-page file. The print settings are set to 3 mm bleed with crop marks,
    /// PDF, CMYK, and the page is selected with the panel scrolled to its Export section.
    /// Result: [the page, the second page, the third page]; `bench_print_finish` exports
    /// them. `=view:x0,y0,x1,y1` shows that part of the stage (the exported sheet is put at
    /// x 1500).
    pub(super) fn bench_print(&mut self, ctx: &egui::Context, mode: &str, inject: &mut Vec<(u32, egui::Event)>) -> Vec<u64> {
        let (x, y) = (-3000.0, 36000.0);
        let (w, h) = (420.0, 595.0);
        let gradient = |a: [u8; 4], b: [u8; 4], angle: f32| Some(Gradient { radial: false, angle, stops: vec![Stop { pos: 0.0, color: a }, Stop { pos: 1.0, color: b }], line: None, radial_line: None });
        let page = |app: &mut Self, at: f32, name: &str, fill: [u8; 4]| {
            let id = app.doc.alloc_id();
            let mut n = Node::new(id, Kind::Frame, x + at, y);
            (n.name, n.w, n.h, n.fill) = (name.into(), w, h, Some(fill));
            app.doc.insert(n, None);
            id
        };
        let add = |app: &mut Self, parent: u64, kind: Kind, (nx, ny, nw, nh): (f32, f32, f32, f32), edit: &dyn Fn(&mut Node)| {
            let (px, _) = app.doc.find(parent).map_or((x, y), |p| (p.x, p.y));
            let mut n = Node::new(app.doc.alloc_id(), kind, px + nx, y + ny);
            (n.w, n.h) = (nw, nh);
            edit(&mut n);
            app.doc.insert(n, Some(parent));
        };
        let text = |n: &mut Node, s: &str, size: f32, weight: u16, fill: [u8; 4]| {
            (n.text, n.name, n.font_size, n.weight, n.fill) = (s.into(), s.into(), size, weight, Some(fill));
        };

        let first = page(self, 0.0, "Print page", [255, 255, 255, 255]);
        if let Some(f) = self.doc.find_mut(first) {
            f.gradient = gradient([254, 240, 138, 255], [253, 186, 116, 255], 90.0);
        }
        let photo = self.doc.add_asset(bench_photo());
        // Off the right edge by 40, and off the top by 30.
        add(self, first, Kind::Image, (150.0, -30.0, 310.0, 250.0), &|n| (n.image, n.stroke_w, n.name) = (Some(photo), 0.0, "Photo off the edge".into()));
        // A band off the left edge and the bottom.
        add(self, first, Kind::Rect, (-50.0, 500.0, 300.0, 140.0), &|n| (n.fill, n.name) = (Some([30, 64, 175, 255]), "Band off the edge".into()));
        add(self, first, Kind::Text, (28.0, 250.0, 360.0, 60.0), &|n| text(n, "Pure black", 44.0, 800, [0, 0, 0, 255]));
        add(self, first, Kind::Text, (30.0, 306.0, 360.0, 30.0), &|n| text(n, "Grey text, 60 % black", 20.0, 500, [102, 102, 102, 255]));
        add(self, first, Kind::Text, (30.0, 338.0, 360.0, 30.0), &|n| text(n, "Text in colour", 26.0, 700, [190, 18, 60, 255]));
        add(self, first, Kind::Text, (30.0, 524.0, 200.0, 30.0), &|n| text(n, "White on the band", 18.0, 600, [255, 255, 255, 255]));
        add(self, first, Kind::Rect, (30.0, 390.0, 170.0, 80.0), &|n| {
            (n.radius, n.fill, n.name) = (14.0, Some([255, 255, 255, 255]), "Card with shadow".into());
            n.shadow = Some(Shadow { x: 0.0, y: 8.0, blur: 18.0, color: [0, 0, 0, 110] });
        });
        add(self, first, Kind::Text, (46.0, 418.0, 150.0, 24.0), &|n| text(n, "Drop shadow", 18.0, 600, [15, 23, 42, 255]));
        add(self, first, Kind::Rect, (230.0, 130.0, 160.0, 110.0), &|n| {
            (n.radius, n.fill, n.bg_blur, n.stroke, n.stroke_w, n.name) = (18.0, Some([255, 255, 255, 70]), 16.0, Some([255, 255, 255, 170]), 1.5, "Glass".into());
        });
        add(self, first, Kind::Text, (246.0, 172.0, 140.0, 24.0), &|n| text(n, "Glass panel", 18.0, 700, [255, 255, 255, 255]));
        add(self, first, Kind::Ellipse, (250.0, 390.0, 120.0, 80.0), &|n| {
            (n.fill, n.gradient, n.name) = (Some([0, 0, 0, 255]), gradient([0, 0, 0, 255], [220, 38, 38, 255], 0.0), "Black to red".into());
        });

        let second = page(self, w + 60.0, "Print page 2", [28, 37, 64, 255]);
        add(self, second, Kind::Text, (30.0, 40.0, 360.0, 60.0), &|n| text(n, "Page two", 44.0, 800, [250, 247, 240, 255]));
        add(self, second, Kind::Ellipse, (260.0, 380.0, 240.0, 240.0), &|n| n.fill = Some([251, 146, 60, 255]));
        let third = page(self, (w + 60.0) * 2.0, "Print page 3", [250, 247, 240, 255]);
        add(self, third, Kind::Text, (30.0, 40.0, 360.0, 60.0), &|n| text(n, "Page three", 44.0, 800, [0, 0, 0, 255]));
        add(self, third, Kind::Rect, (-20.0, 300.0, 460.0, 60.0), &|n| n.fill = Some([13, 148, 136, 255]));

        if let Some(file) = std::env::var_os("KANVAS_BENCH_PRINT_DOC") {
            match crate::file::read(std::path::Path::new(&file)) {
                Ok((doc, _)) => EXTRA.set(Some(doc)),
                Err(e) => eprintln!("PRINT doc: {e}"),
            }
        }
        self.ext.print = Print { bleed_mm: 3.0, marks: true, cmyk: true, ..Default::default() };
        self.ext.export_fmt = 3;
        let part: Vec<f32> = mode.strip_prefix("view:").map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
        let view = if part.len() == 4 { (x + part[0], y + part[1], x + part[2], y + part[3]) } else { (x - 40.0, y - 60.0, x + 2000.0, y + h + 60.0) };
        self.zoom_to(view, 32.0);
        self.fit_pending = false;
        self.sel = vec![first];
        // The Export section is at the end of the panel.
        let p = egui::pos2(ctx.screen_rect().right() - 150.0, 500.0);
        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0.0, -900.0), modifiers: Default::default() };
        inject.extend([(200, egui::Event::PointerMoved(p)), (204, wheel.clone()), (212, wheel.clone()), (220, wheel.clone()), (228, wheel), (250, egui::Event::PointerMoved(self.canvas_rect.center_top() + egui::vec2(0.0, 40.0)))]);
        vec![first, second, third]
    }

    /// The second half of `bench_print`, once the fonts are loaded: the sample files are
    /// written beside the screenshot (RGB with bleed and marks, CMYK vector, CMYK flattened,
    /// CMYK multi-page, CMYK through the SWOP profile Windows ships, a PNG and a JPG of the
    /// sheet), the time each takes is printed, and the PNG of the sheet is placed right of the
    /// pages.
    pub(super) fn bench_print_finish(&mut self, ctx: &egui::Context, pages: &[u64], out: &std::path::Path) {
        let Some((x, y)) = self.doc.find(pages[0]).map(|f| (f.x, f.y)) else { return };
        let print = Print { bleed_mm: 3.0, marks: true, ..Default::default() };
        let start = std::time::Instant::now();
        let art: Vec<(String, f32, f32)> = pages.iter().filter_map(|id| crate::export::to_svg_bleed(&self.doc, Some(*id), print.bleed_px(), &mut self.fonts, ctx)).collect();
        eprintln!("PRINT {} svg made in {:.1} ms ({} kB)", art.len(), start.elapsed().as_secs_f32() * 1000.0, art.iter().map(|a| a.0.len()).sum::<usize>() / 1024);
        let Some(first) = art.first().cloned() else { return };
        let save = |name: &str, pages: &[(String, f32, f32)], print: &Print| {
            let path = out.with_file_name(name);
            let start = std::time::Instant::now();
            let result = crate::print::save_pdf(pages, print, &path);
            let size = std::fs::metadata(&path).map_or(0, |m| m.len() / 1024);
            eprintln!("PRINT {name}: {:.0} ms, {size} kB, {result:?}", start.elapsed().as_secs_f32() * 1000.0);
        };
        let one = std::slice::from_ref(&first);
        save("print-rgb-bleed-marks.pdf", one, &print);
        save("print-cmyk-vector.pdf", one, &Print { cmyk: true, ..print.clone() });
        save("print-rgb-flattened.pdf", one, &Print { flatten: true, ..print.clone() });
        save("print-cmyk-flattened.pdf", one, &Print { cmyk: true, flatten: true, ..print.clone() });
        save("print-cmyk-multipage.pdf", &art, &Print { cmyk: true, ..print.clone() });
        let swop = std::path::PathBuf::from(r"C:\Windows\System32\spool\drivers\color\RSWOP.icm");
        if swop.exists() {
            save("print-cmyk-icc-swop.pdf", one, &Print { cmyk: true, icc: Some(swop), ..print.clone() });
        }
        // The demo frame of the bench document: masks, see-through gradients, blend modes.
        if let Some(demo) = self.doc.nodes.iter().find(|n| n.name == "Feature demo").map(|n| n.id) {
            if let Some(art) = crate::export::to_svg_bleed(&self.doc, Some(demo), print.bleed_px(), &mut self.fonts, ctx) {
                save("print-demo-rgb.pdf", std::slice::from_ref(&art), &print);
                save("print-demo-cmyk.pdf", &[art], &Print { cmyk: true, ..print.clone() });
            }
        }
        // The same page as the app exported it before there were print settings.
        if let Some(plain) = crate::export::to_svg(&self.doc, Some(pages[0]), &mut self.fonts, ctx) {
            save("print-plain-rgb.pdf", &[plain], &Print::default());
        }
        // The sheet as a picture: what the PDFs with marks should look like.
        let (sheet, sw, sh) = crate::print::sheet_svg(first.0, first.1, first.2, &print, 1.0);
        let _ = crate::export::save_raster(&sheet, sw, sh, 3.0, false, &out.with_file_name("print-sheet.png"));
        let _ = crate::export::save_raster(&sheet, sw, sh, 2.0, true, &out.with_file_name("print-sheet.jpg"));
        if let Ok((png, _, _)) = crate::export::render_png(&sheet, sw, sh, 3.0) {
            let asset = self.doc.add_asset(png);
            let mut n = Node::new(self.doc.alloc_id(), Kind::Image, x + 1500.0, y - (sh - 595.0) * 0.5);
            (n.name, n.w, n.h, n.image, n.stroke_w) = ("Print sheet".into(), sw, sh, Some(asset), 0.0);
            self.doc.insert(n, None);
        }
        eprintln!("PRINT all written in {:.0} ms", start.elapsed().as_secs_f32() * 1000.0);
        // The same through the remote API (the `export` tool of the MCP server).
        let file = |name: &str| out.with_file_name(name).to_string_lossy().into_owned();
        for args in [
            serde_json::json!({ "path": file("remote-cmyk.pdf"), "id": pages[0], "bleed": 3, "crop_marks": true, "color": "cmyk" }),
            serde_json::json!({ "path": file("remote-flattened.pdf"), "id": pages[1], "bleed": 3, "flatten": true, "resolution": 144 }),
            serde_json::json!({ "path": file("remote-sheet.png"), "id": pages[2], "bleed": 3, "crop_marks": true, "scale": 2 }),
            serde_json::json!({ "path": file("remote-plain.png"), "id": pages[2] }),
            serde_json::json!({ "path": file("remote-wrong.png"), "id": pages[2], "color": "cmyk" }),
            serde_json::json!({ "path": file("remote-wrong.pdf"), "id": pages[2], "color": "cmyk", "icc": file("print-sheet.png") }),
        ] {
            eprintln!("PRINT remote {args} -> {}", self.remote_bench_export(ctx, &args));
        }
        self.bench_print_doc(ctx, out);
    }
}
