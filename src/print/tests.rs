use super::pdf::{get, File, Val};
use super::*;
use crate::doc::{Doc, Gradient, Kind, Node, Shadow, Stop};
use crate::fonts::FontManager;
use eframe::egui;

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-3
}

fn context() -> egui::Context {
    let ctx = egui::Context::default();
    let _ = ctx.run(egui::RawInput::default(), |_| {});
    ctx
}

/// A small picture, red on the left and blue on the right, with a see-through corner.
fn picture() -> Vec<u8> {
    let img = image::RgbaImage::from_fn(8, 8, |x, y| match (x, y) {
        (0, 0) => image::Rgba([0, 0, 0, 0]),
        (x, _) if x < 4 => image::Rgba([255, 0, 0, 255]),
        _ => image::Rgba([0, 0, 255, 255]),
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    png
}

/// A page of 200 × 100 with a gradient fill, holding: a red rectangle that sticks out on
/// the left, pure black, grey and a coloured stroke, a gradient, a picture, a shadow
/// (a bitmap in the PDF) and a half transparent layer. Returns the document and the frame.
fn page() -> (Doc, u64) {
    let mut doc = Doc::default();
    let frame = doc.alloc_id();
    let mut f = Node::new(frame, Kind::Frame, 0.0, 0.0);
    (f.w, f.h, f.fill) = (200.0, 100.0, Some([255, 255, 255, 255]));
    f.gradient = Some(Gradient { radial: false, angle: 0.0, stops: vec![Stop { pos: 0.0, color: [255, 255, 0, 255] }, Stop { pos: 1.0, color: [0, 255, 255, 255] }], line: None, radial_line: None });
    doc.insert(f, None);
    let picture = doc.add_asset(picture());
    let mut add = |kind: Kind, (x, y, w, h): (f32, f32, f32, f32), edit: &dyn Fn(&mut Node)| {
        let mut n = Node::new(doc.alloc_id(), kind, x, y);
        (n.w, n.h) = (w, h);
        edit(&mut n);
        doc.insert(n, Some(frame));
    };
    add(Kind::Rect, (-30.0, 40.0, 60.0, 20.0), &|n| n.fill = Some([255, 0, 0, 255]));
    add(Kind::Rect, (40.0, 10.0, 20.0, 20.0), &|n| n.fill = Some([0, 0, 0, 255]));
    add(Kind::Rect, (70.0, 10.0, 20.0, 20.0), &|n| (n.fill, n.stroke, n.stroke_w) = (Some([128, 128, 128, 255]), Some([0, 128, 0, 255]), 2.0));
    add(Kind::Ellipse, (100.0, 10.0, 30.0, 20.0), &|n| {
        n.fill = Some([0, 0, 0, 255]);
        n.gradient = Some(Gradient { radial: true, angle: 0.0, stops: vec![Stop { pos: 0.0, color: [255, 0, 0, 255] }, Stop { pos: 0.5, color: [0, 0, 0, 255] }, Stop { pos: 1.0, color: [0, 0, 255, 128] }], line: None, radial_line: None });
    });
    add(Kind::Image, (140.0, 10.0, 40.0, 40.0), &|n| (n.image, n.stroke_w) = (Some(picture.clone()), 0.0));
    add(Kind::Rect, (40.0, 60.0, 30.0, 20.0), &|n| (n.fill, n.shadow) = (Some([255, 255, 255, 255]), Some(Shadow { x: 2.0, y: 2.0, blur: 4.0, color: [0, 0, 0, 160] })));
    add(Kind::Rect, (90.0, 60.0, 30.0, 20.0), &|n| (n.fill, n.opacity) = (Some([255, 0, 255, 255]), 0.5));
    (doc, frame)
}

fn svg(doc: &Doc, id: u64, bleed: f32) -> (String, f32, f32) {
    let ctx = context();
    let mut fonts = FontManager::new(&ctx);
    crate::export::to_svg_bleed(doc, Some(id), bleed, &mut fonts, &ctx).unwrap()
}

/// Does `hay` hold the operator `op` (a whole word)?
fn has_operator(hay: &[u8], op: &str) -> bool {
    hay.split(|c| c.is_ascii_whitespace()).any(|word| word == op.as_bytes())
}

fn find(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn near(a: &[f32], b: [f32; 4]) -> bool {
    a.len() == 4 && a.iter().zip(b).all(|(a, b)| (a - b).abs() < 0.01)
}

/// Everything of a PDF that says how it is painted, streams decoded: for searching.
struct Dump {
    file: File,
    /// The objects without their streams.
    dicts: Vec<u8>,
    /// The content streams, decoded.
    content: Vec<u8>,
}

fn dump(bytes: &[u8]) -> Dump {
    let file = File::parse(bytes).unwrap();
    let mut bare = file.clone();
    bare.objects.iter_mut().for_each(|o| o.stream = None);
    Dump { dicts: bare.write(), content: file.contents().join(&b"\n"[..]), file }
}

impl Dump {
    fn page(&self, index: usize) -> &pdf::Dict {
        self.file.objects.iter().filter_map(|o| o.val.dict()).filter(|d| get(d, "Type").is_some_and(|t| t.is_name("Page"))).nth(index).unwrap()
    }

    fn rect(&self, page: usize, key: &str) -> Vec<f32> {
        get(self.page(page), key).and_then(|v| v.nums()).unwrap_or_default().into_iter().map(|n| n as f32).collect()
    }

    fn images(&self) -> Vec<(&pdf::Dict, &Vec<u8>)> {
        self.file.objects.iter().filter_map(|o| Some((o.val.dict()?, o.stream.as_ref()?))).filter(|(d, _)| get(d, "Subtype").is_some_and(|s| s.is_name("Image"))).collect()
    }
}

#[test]
fn rgb_to_cmyk_by_the_plain_formula() {
    // Black is black ink only, white is no ink at all, greys are black ink only.
    assert_eq!(naive([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(naive([1.0, 1.0, 1.0]), [0.0, 0.0, 0.0, 0.0]);
    let grey = naive([0.5, 0.5, 0.5]);
    assert_eq!(&grey[..3], &[0.0, 0.0, 0.0]);
    assert!(close(grey[3], 0.5));
    // The primaries and their mixes.
    assert_eq!(naive([1.0, 0.0, 0.0]), [0.0, 1.0, 1.0, 0.0]);
    assert_eq!(naive([0.0, 1.0, 0.0]), [1.0, 0.0, 1.0, 0.0]);
    assert_eq!(naive([0.0, 0.0, 1.0]), [1.0, 1.0, 0.0, 0.0]);
    assert_eq!(naive([0.0, 1.0, 1.0]), [1.0, 0.0, 0.0, 0.0]);
    assert_eq!(naive([1.0, 0.0, 1.0]), [0.0, 1.0, 0.0, 0.0]);
    assert_eq!(naive([1.0, 1.0, 0.0]), [0.0, 0.0, 1.0, 0.0]);
    // A dark colour: black takes what the three have in common.
    let navy = naive([0.0, 0.0, 0.5]);
    assert!(close(navy[0], 1.0) && close(navy[1], 1.0) && close(navy[2], 0.0) && close(navy[3], 0.5));
    // Out of range values are clamped.
    assert_eq!(naive([2.0, -1.0, 0.0]), [0.0, 1.0, 1.0, 0.0]);

    // Vector colours: the same, and neutral ones are black only to the last digit.
    let colour = Colour::naive();
    assert!(!colour.managed());
    assert_eq!(colour.vector([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(colour.vector([1.0, 1.0, 1.0]), [0.0, 0.0, 0.0, 0.0]);
    assert_eq!(colour.vector([0.25, 0.25, 0.25]), [0.0, 0.0, 0.0, 0.75]);
    assert_eq!(colour.vector([1.0, 0.0, 0.0]), [0.0, 1.0, 1.0, 0.0]);
    // Pixels: the same numbers in bytes.
    assert_eq!(colour.pixels(&[0, 0, 0, 255, 255, 255, 128, 128, 128, 255, 0, 0, 0, 0, 128]), [0, 0, 0, 255, 0, 0, 0, 0, 0, 0, 0, 127, 0, 255, 255, 0, 255, 255, 0, 127]);
}

#[test]
fn page_boxes_with_bleed_and_marks() {
    let mm = MM;
    // Nothing asked for: the page is the trim size.
    let s = sheet(595.0, 842.0, 0.0, false, mm);
    assert_eq!((s.media, s.trim, s.bleed), ((595.0, 842.0), [0.0, 0.0, 595.0, 842.0], [0.0, 0.0, 595.0, 842.0]));
    assert!(s.marks.is_empty());
    // Bleed only: the page is the bleed box.
    let b = 3.0 * mm;
    let s = sheet(595.0, 842.0, b, false, mm);
    assert!(close(s.media.0, 595.0 + 2.0 * b) && close(s.media.1, 842.0 + 2.0 * b));
    assert!(close(s.trim[0], b) && close(s.trim[1], b) && close(s.trim[2], b + 595.0) && close(s.trim[3], b + 842.0));
    assert!(close(s.bleed[0], 0.0) && close(s.bleed[1], 0.0) && close(s.bleed[2], s.media.0) && close(s.bleed[3], s.media.1));
    // Bleed and marks: a margin of bleed + 5 mm around the trim size.
    let s = sheet(595.0, 842.0, b, true, mm);
    let margin = 8.0 * mm;
    assert!(close(s.media.0, 595.0 + 2.0 * margin) && close(s.media.1, 842.0 + 2.0 * margin));
    assert!(close(s.trim[0], margin) && close(s.trim[3], margin + 842.0));
    assert!(close(s.bleed[0], margin - b) && close(s.bleed[1], margin - b) && close(s.bleed[2], margin + 595.0 + b) && close(s.bleed[3], margin + 842.0 + b));
    // Marks without bleed keep 3 mm from the trim edge, and so does a small bleed.
    for bleed in [0.0, 1.0 * mm] {
        let s = sheet(100.0, 50.0, bleed, true, mm);
        assert!(close(s.trim[0], 8.0 * mm), "{bleed}");
        assert!(close(s.bleed[0], 8.0 * mm - bleed));
    }
    // A wide bleed pushes the marks out.
    let s = sheet(100.0, 50.0, 10.0 * mm, true, mm);
    assert!(close(s.trim[0], 15.0 * mm) && close(s.media.0, 100.0 + 30.0 * mm));
}

#[test]
fn crop_marks_point_away_from_the_corners() {
    let (mm, b) = (MM, 3.0 * MM);
    let s = sheet(200.0, 100.0, b, true, mm);
    assert_eq!(s.marks.len(), 8);
    let [x0, y0, x1, y1] = s.trim;
    for [ax, ay, bx, by] in &s.marks {
        // Each is 5 mm long, horizontal or vertical, on the line of a trim edge …
        let (dx, dy) = ((bx - ax).abs(), (by - ay).abs());
        assert!((close(dx, 5.0 * mm) && dy == 0.0) || (close(dy, 5.0 * mm) && dx == 0.0));
        if dy == 0.0 {
            assert!(close(*ay, y0) || close(*ay, y1));
        } else {
            assert!(close(*ax, x0) || close(*ax, x1));
        }
        // … inside the page, and outside the bleed box.
        for (x, y) in [(*ax, *ay), (*bx, *by)] {
            assert!(x >= -1e-3 && y >= -1e-3 && x <= s.media.0 + 1e-3 && y <= s.media.1 + 1e-3);
            let inside = x > s.bleed[0] + 1e-3 && x < s.bleed[2] - 1e-3 && y > s.bleed[1] + 1e-3 && y < s.bleed[3] - 1e-3;
            assert!(!inside, "a mark reaches into the bleed: {x} {y}");
        }
    }
    // The mark left of the first corner: from the bleed edge to the edge of the page.
    assert!(s.marks.iter().any(|m| close(m[1], y0) && close(m[3], y0) && close(m[0], x0 - b) && close(m[2], 0.0)));
    // And the one above (or below) it.
    assert!(s.marks.iter().any(|m| close(m[0], x0) && close(m[2], x0) && close(m[1], y0 - b) && close(m[3], 0.0)));
    // Two at every corner.
    for (cx, cy) in [(x0, y0), (x1, y0), (x0, y1), (x1, y1)] {
        let at_corner = |m: &&[f32; 4]| {
            let vertical = close(m[0], cx) && close(m[2], cx) && (m[1] - cy).abs() < 9.0 * mm && (m[3] - cy).abs() < 9.0 * mm;
            let horizontal = close(m[1], cy) && close(m[3], cy) && (m[0] - cx).abs() < 9.0 * mm && (m[2] - cx).abs() < 9.0 * mm;
            vertical || horizontal
        };
        assert_eq!(s.marks.iter().filter(at_corner).count(), 2);
    }
}

#[test]
fn sizes_on_paper() {
    let mut p = Print::default();
    // 72 px per inch: a px is a point.
    assert_eq!(p.hint(595.0, 842.0), "210 × 297 mm");
    assert!(close(p.pt_per_px(), 1.0) && p.bleed_px() == 0.0);
    p.bleed_mm = 3.0;
    assert!(close(p.bleed_px(), 8.504));
    assert_eq!(p.hint(595.0, 842.0), "210 × 297 mm + 3 mm bleed");
    // A design made at 300 dpi pixel sizes.
    p.ppi = 300.0;
    assert_eq!(p.hint(2480.0, 1772.0), "210 × 150 mm + 3 mm bleed");
    assert!(close(p.bleed_px(), 35.433) && close(p.pt_per_px(), 0.24));
    // Nonsense falls back to something usable.
    p.ppi = 0.0;
    p.bleed_mm = f32::NAN;
    assert!(close(p.pt_per_px(), 1.0) && p.bleed_px() == 0.0);
    // No bleed for something smaller than the bleed itself.
    let p = Print { bleed_mm: 3.0, ..Default::default() };
    assert_eq!(p.page_bleed(12.0, 300.0), 0.0);
    assert!(close(p.page_bleed(300.0, 300.0), 8.504));
}

#[test]
fn a_frame_fills_its_bleed() {
    let (doc, frame) = page();
    let pixel = |svg: &str, w: f32, h: f32, x: u32, y: u32| {
        let px = crate::export::rasterize(svg, w, h, 1.0).unwrap().pixel(x, y).unwrap();
        (px.red(), px.green(), px.blue(), px.alpha())
    };
    // Without bleed: the frame as it is, the rectangle cut off at its left edge.
    let (plain, w, h) = svg(&doc, frame, 0.0);
    assert_eq!((w, h), (200.0, 100.0));
    assert_eq!(pixel(&plain, w, h, 2, 50), (255, 0, 0, 255));
    assert_eq!(pixel(&plain, w, h, 50, 20), (0, 0, 0, 255));
    // With 10 px of bleed the picture is larger by that on every side …
    let (bled, w, h) = svg(&doc, frame, 10.0);
    assert_eq!((w, h), (220.0, 120.0));
    // … the frame's own fill reaches the corners of it (a gradient from yellow to cyan) …
    let (r, g, b, a) = pixel(&bled, w, h, 1, 1);
    assert!(r > 240 && g > 240 && b < 15 && a == 255, "{r} {g} {b} {a}");
    let (r, g, b, a) = pixel(&bled, w, h, 218, 118);
    assert!(r < 15 && g > 240 && b > 240 && a == 255, "{r} {g} {b} {a}");
    // … what sticks out of the frame shows in the bleed, up to the edge of the bleed …
    assert_eq!(pixel(&bled, w, h, 1, 60), (255, 0, 0, 255));
    // … and what is inside has not moved (the black square is at 40,10 of the frame).
    assert_eq!(pixel(&bled, w, h, 60, 30), (0, 0, 0, 255));
    // Another top-level layer beside the frame is not part of it.
    let mut doc = doc;
    let mut n = Node::new(doc.alloc_id(), Kind::Rect, -8.0, -8.0);
    (n.w, n.h, n.fill) = (6.0, 6.0, Some([0, 0, 255, 255]));
    doc.insert(n, None);
    let (bled, w, h) = svg(&doc, frame, 10.0);
    let (_, _, b, _) = pixel(&bled, w, h, 5, 5);
    assert!(b < 15);
    // A layer that is not a frame just gets room around it.
    let rect = doc.nodes[0].children[1].id;
    let (around, w, h) = svg(&doc, rect, 10.0);
    assert_eq!((w, h), (40.0, 40.0));
    assert_eq!(pixel(&around, w, h, 5, 5).3, 0);
    assert_eq!(pixel(&around, w, h, 20, 20), (0, 0, 0, 255));
}

#[test]
fn marks_around_an_image() {
    let (doc, frame) = page();
    let print = Print { bleed_mm: 3.0, marks: true, ..Default::default() };
    let (art, w, h) = svg(&doc, frame, print.bleed_px());
    let (sheet_svg, sw, sh) = sheet_svg(art, w, h, &print);
    let margin = 8.0 * MM;
    assert!(close(sw, 200.0 + margin * 2.0) && close(sh, 100.0 + margin * 2.0));
    let map = crate::export::rasterize(&sheet_svg, sw, sh, 4.0).unwrap();
    let at = |x: f32, y: f32| {
        let px = map.pixel((x * 4.0) as u32, (y * 4.0) as u32).unwrap();
        (px.red(), px.green(), px.blue(), px.alpha())
    };
    // The margin is empty, the bleed is filled, and the page starts at the margin.
    assert_eq!(at(2.0, 2.0).3, 0);
    assert_eq!(at(margin - 3.0 * MM + 1.0, margin + 50.0), (255, 0, 0, 255));
    assert_eq!(at(margin + 50.0, margin + 20.0), (0, 0, 0, 255));
    // A mark left of the top left corner, on the line of the top edge, and one above it;
    // beside the marks the margin is empty.
    assert_eq!(at(5.0, margin), (0, 0, 0, 255));
    assert_eq!(at(5.0, margin + 3.0).3, 0);
    assert_eq!(at(margin, 5.0), (0, 0, 0, 255));
    // Without marks the picture is left alone.
    let plain = Print { bleed_mm: 3.0, ..Default::default() };
    assert_eq!(super::sheet_svg("<svg/>".into(), 10.0, 20.0, &plain), ("<svg/>".to_owned(), 10.0, 20.0));
}

#[test]
fn pdf_read_and_written_again() {
    let (doc, frame) = page();
    let (art, w, h) = svg(&doc, frame, 0.0);
    let bytes = pdf_bytes(&[(art, w, h)], &Print::default()).unwrap();
    let file = File::parse(&bytes).unwrap();
    assert!(file.objects.len() > 10);
    assert!(matches!(get(&file.trailer, "Root"), Some(Val::Ref(_))));
    // Written and read again it is the same file, object for object (ids are 1, 2, 3 …).
    let again = File::parse(&file.write()).unwrap();
    assert_eq!(again.objects.len(), file.objects.len());
    assert_eq!(File::parse(&again.write()).unwrap(), again);
    for (a, b) in file.objects.iter().zip(&again.objects) {
        assert_eq!(a.stream, b.stream);
    }
    // Values of every kind.
    let src = b"%PDF-1.7\n1 0 obj\n<< /A [1 -2.5 (a\\) (b) c) <48> /N#20 true null 3 0 R] /B << /C 0.00001 /M [0.0022882888 -612.0078] >> >>\nendobj\n3 0 obj\n<< /Length 5 >>\nstream\nab\ncd\nendstream\nendobj\nxref\n0 1\ntrailer\n<< /Root 1 0 R >>\nstartxref\n0\n%%EOF";
    let file = File::parse(src).unwrap();
    let a = get(file.objects[0].val.dict().unwrap(), "A").unwrap();
    assert_eq!(
        a,
        &Val::Arr(vec![Val::Num(1.0), Val::Num(-2.5), Val::Str(b"(a\\) (b) c)".to_vec()), Val::Str(b"<48>".to_vec()), Val::Name(b"N#20".to_vec()), Val::Bool(true), Val::Null, Val::Ref(3)])
    );
    assert_eq!(file.objects[1].stream.as_deref(), Some(&b"ab\ncd"[..]));
    // Object 3 becomes object 2.
    let out = file.write();
    assert!(find(&out, "2 0 R]") && find(&out, "/C 0.00001") && find(&out, "/Root 1 0 R"));
    // Numbers keep the digits of the `f32` they were (the matrix of a page is 1 / its size).
    let again = File::parse(&out).unwrap();
    let m = get(get(again.objects[0].val.dict().unwrap(), "B").unwrap().dict().unwrap(), "M").unwrap().nums().unwrap();
    assert_eq!((m[0] as f32, m[1] as f32), (0.0022882888, -612.0078));
    assert!(File::parse(b"1 0 obj << /A >> endobj").is_err());
}

#[test]
fn content_colours_become_cmyk() {
    let colour = Colour::naive();
    let run = |src: &str| String::from_utf8(pdf::content_to_cmyk(src.as_bytes(), &colour).unwrap()).unwrap();
    assert_eq!(run("/cs0 cs 1 0 0 scn 0 0 10 10 re f"), "/cs0 cs 0 1 1 0 k 0 0 10 10 re f");
    assert_eq!(run("0 0 0 rg 0.5 0.5 0.5 RG"), "0 0 0 1 k 0 0 0 0.5 K");
    assert_eq!(run("/cs0 CS 0 0 1 SCN\n1 w"), "/cs0 CS 1 1 0 0 K\n1 w");
    // Patterns, greys, CMYK and other operators with three numbers are left alone.
    for same in ["/Pattern cs /p0 scn", "0.5 g 0.2 G", "0 0 0 1 k 1 1 1 1 K", "1 2 3 4 5 6 cm 1 2 m 3 4 l S", "0.5 scn 0.1 0.2 0.3 0.4 scn", "(1 0 0 rg) Tj [1 0 0] 0 d", "/a << /b 1 0 0 >> BDC"] {
        assert_eq!(run(same), same);
    }
    assert!(pdf::content_to_cmyk(b"BI /W 1 ID x EI", &colour).is_err());
}

#[test]
fn rgb_pdf_with_bleed_and_marks() {
    let (doc, frame) = page();
    let print = Print { bleed_mm: 3.0, marks: true, ..Default::default() };
    let (art, w, h) = svg(&doc, frame, print.bleed_px());
    let d = dump(&pdf_bytes(&[(art, w, h)], &print).unwrap());
    let (b, margin) = (3.0 * MM, 8.0 * MM);
    assert!(near(&d.rect(0, "MediaBox"), [0.0, 0.0, 200.0 + margin * 2.0, 100.0 + margin * 2.0]), "{:?}", d.rect(0, "MediaBox"));
    assert!(near(&d.rect(0, "TrimBox"), [margin, margin, margin + 200.0, margin + 100.0]), "{:?}", d.rect(0, "TrimBox"));
    assert!(near(&d.rect(0, "BleedBox"), [margin - b, margin - b, margin + 200.0 + b, margin + 100.0 + b]), "{:?}", d.rect(0, "BleedBox"));
    // Still RGB, the marks are black lines a quarter of a point wide.
    assert!(find(&d.content, "0.25 w") && has_operator(&d.content, "RG") && has_operator(&d.content, "scn"));
    assert!(!has_operator(&d.content, "k") && !find(&d.dicts, "DeviceCMYK"));
    // Without print settings the page is the frame and has no other boxes.
    let (art, w, h) = svg(&doc, frame, 0.0);
    let d = dump(&pdf_bytes(&[(art, w, h)], &Print::default()).unwrap());
    assert!(near(&d.rect(0, "MediaBox"), [0.0, 0.0, 200.0, 100.0]));
    assert!(d.rect(0, "TrimBox").is_empty() && d.rect(0, "BleedBox").is_empty());
    // 300 px per inch: the same frame is a quarter of that size on paper (0.24).
    let fine = Print { ppi: 300.0, ..Default::default() };
    let (art, w, h) = svg(&doc, frame, 0.0);
    let d = dump(&pdf_bytes(&[(art, w, h)], &fine).unwrap());
    assert!(near(&d.rect(0, "MediaBox"), [0.0, 0.0, 48.0, 24.0]));
}

#[test]
fn cmyk_pdf_has_no_rgb_left() {
    let (doc, frame) = page();
    let print = Print { bleed_mm: 3.0, marks: true, cmyk: true, ..Default::default() };
    let (art, w, h) = svg(&doc, frame, print.bleed_px());
    let pages = [(art.clone(), w, h), (art, w, h)];
    let rgb = dump(&pdf_bytes(&pages, &Print { cmyk: false, ..print.clone() }).unwrap());
    let d = dump(&pdf_bytes(&pages, &print).unwrap());
    // What this is about is in the RGB file to begin with: colours, shadings, pictures.
    assert!(has_operator(&rgb.content, "scn") && find(&rgb.dicts, "ICCBased") && find(&rgb.dicts, "DeviceRGB") && find(&rgb.dicts, "/FunctionType"));
    // No RGB operator, colour space or profile is left …
    for op in ["rg", "RG", "sc", "SC"] {
        assert!(!has_operator(&d.content, op), "{op}");
    }
    assert!(!find(&d.dicts, "DeviceRGB") && !find(&d.dicts, "ICCBased") && !find(&d.content, "DeviceRGB"));
    // (`scn` now only names patterns: never three numbers.)
    let words: Vec<&[u8]> = d.content.split(|c| c.is_ascii_whitespace()).filter(|w| !w.is_empty()).collect();
    for (i, _) in words.iter().enumerate().filter(|(_, w)| matches!(**w, b"scn" | b"SCN")) {
        assert!(words[i - 1].starts_with(b"/"), "{:?}", String::from_utf8_lossy(words[i - 1]));
    }
    // … and CMYK is there instead: fills and strokes, the spaces of shadings and groups.
    assert!(has_operator(&d.content, "k") && has_operator(&d.content, "K") && find(&d.dicts, "DeviceCMYK"));
    // Pure black is black ink only, the grey as well, red is magenta and yellow.
    assert!(find(&d.content, "0 0 0 1 k") && find(&d.content, "0 0 0 0.498 k") && find(&d.content, "0 1 1 0 k"));
    // The dark green stroke (0, 128, 0).
    assert!(find(&d.content, "1 0 1 0.498 K"));
    // The crop marks are on all four plates.
    assert!(find(&d.content, "1 1 1 1 K") && find(&d.content, "0.25 w"));
    // Gradient stops have four components (or one, for the opacity of a stop).
    let mut stops = 0;
    for f in d.file.objects.iter().filter_map(|o| o.val.dict()).filter(|f| get(f, "FunctionType").is_some()) {
        for key in ["C0", "C1"] {
            if let Some(c) = get(f, key).and_then(|v| v.nums()) {
                assert!(c.len() == 4 || c.len() == 1, "{c:?}");
                stops += 1;
            }
        }
        let range = get(f, "Range").and_then(|v| v.nums()).unwrap();
        assert!(range.len() == 8 || range.len() == 2);
    }
    assert!(stops >= 6);
    // The frame's gradient starts yellow; the radial one has a black stop: black only.
    assert!(find(&d.dicts, "/C0 [0 0 1 0]") && find(&d.dicts, "[0 0 0 1]"));
    // Pictures are CMYK or grey (soft masks), with as many samples as that takes.
    let images = d.images();
    assert_eq!(images.len(), rgb.images().len());
    let mut cmyk = 0;
    for (dict, data) in &images {
        let int = |key: &str| get(dict, key).and_then(|v| v.num()).unwrap() as usize;
        let space = get(dict, "ColorSpace").unwrap();
        let samples = pdf::decode(dict, data).unwrap();
        assert_eq!(int("BitsPerComponent"), 8);
        if space.is_name("DeviceCMYK") {
            cmyk += 1;
            assert_eq!(samples.len(), int("Width") * int("Height") * 4);
        } else {
            assert!(space.is_name("DeviceGray"));
            assert_eq!(samples.len(), int("Width") * int("Height"));
        }
    }
    // (The picture and the shadow, on both pages.)
    assert!(cmyk >= 4, "{cmyk}");
    // The picture itself: red on the left is magenta + yellow, blue on the right cyan + magenta.
    let (dict, data) = images.iter().find(|(d, _)| get(d, "Width").and_then(|v| v.num()) == Some(8.0) && get(d, "ColorSpace").unwrap().is_name("DeviceCMYK")).unwrap();
    let samples = pdf::decode(dict, data).unwrap();
    assert_eq!(&samples[4..8], &[0, 255, 255, 0]);
    assert_eq!(&samples[28..32], &[255, 255, 0, 0]);
    // Its soft mask is still there.
    assert!(get(dict, "SMask").is_some());
    // Both pages keep their boxes.
    let (b, margin) = (3.0 * MM, 8.0 * MM);
    for page in 0..2 {
        assert!(near(&d.rect(page, "MediaBox"), [0.0, 0.0, 200.0 + margin * 2.0, 100.0 + margin * 2.0]));
        assert!(near(&d.rect(page, "TrimBox"), [margin, margin, margin + 200.0, margin + 100.0]));
        assert!(near(&d.rect(page, "BleedBox"), [margin - b, margin - b, margin + 200.0 + b, margin + 100.0 + b]));
    }
    // Every reference in the file leads to an object (one that does not is written as null).
    assert!(!find(&d.dicts, "null"));
    // No output intent without a profile.
    assert!(!find(&d.dicts, "OutputIntents"));
}

#[test]
fn flattened_pdf_is_one_image_a_page() {
    let (doc, frame) = page();
    for cmyk in [false, true] {
        let print = Print { bleed_mm: 3.0, marks: true, cmyk, flatten: true, ..Default::default() };
        let (art, w, h) = svg(&doc, frame, print.bleed_px());
        let d = dump(&pdf_bytes(&[(art, w, h)], &print).unwrap());
        let images = d.images();
        assert_eq!(images.len(), 1);
        let (dict, data) = images[0];
        let int = |key: &str| get(dict, key).and_then(|v| v.num()).unwrap() as usize;
        // 300 dpi: 200 pt + 2 × 3 mm of bleed is 3.014 in.
        let wide = ((200.0 + 6.0 * MM) * 300.0 / 72.0).ceil() as usize;
        assert_eq!(int("Width"), wide);
        let (space, parts) = if cmyk { ("DeviceCMYK", 4) } else { ("DeviceRGB", 3) };
        assert!(get(dict, "ColorSpace").unwrap().is_name(space));
        let samples = pdf::decode(dict, data).unwrap();
        assert_eq!(samples.len(), int("Width") * int("Height") * parts);
        // The top left corner is the yellow of the frame's gradient, in the bleed.
        if cmyk {
            assert!(samples[0] < 8 && samples[1] < 8 && samples[2] > 247 && samples[3] < 8, "{:?}", &samples[..4]);
            assert!(!find(&d.dicts, "DeviceRGB") && find(&d.content, "1 1 1 1 K"));
        } else {
            assert!(samples[0] > 247 && samples[1] > 247 && samples[2] < 8, "{:?}", &samples[..3]);
        }
        assert_eq!(d.rect(0, "TrimBox").len(), 4);
    }
}

/// A CMYK profile made up for the tests: a header and a description, nothing a colour
/// engine could use.
fn fake_profile(v4: bool) -> Vec<u8> {
    let mut icc = vec![0u8; 128];
    icc[16..20].copy_from_slice(b"CMYK");
    icc[36..40].copy_from_slice(b"acsp");
    icc.extend(1u32.to_be_bytes());
    icc.extend(b"desc");
    icc.extend(144u32.to_be_bytes());
    let tag: Vec<u8> = if v4 {
        let text: Vec<u8> = "Press Ä".encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        [&b"mluc\0\0\0\0"[..], &1u32.to_be_bytes(), &12u32.to_be_bytes(), b"enUS", &(text.len() as u32).to_be_bytes(), &28u32.to_be_bytes(), &text].concat()
    } else {
        [&b"desc\0\0\0\0"[..], &11u32.to_be_bytes(), b"Test Press\0"].concat()
    };
    icc.extend((tag.len() as u32).to_be_bytes());
    icc.extend(tag);
    icc
}

#[test]
fn icc_profile_becomes_the_output_intent() {
    let dir = std::env::temp_dir().join(format!("kanvas-print-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("press.icc");
    std::fs::write(&path, fake_profile(false)).unwrap();
    assert_eq!(read_profile(&path).unwrap().name, "Test Press");
    std::fs::write(&path, fake_profile(true)).unwrap();
    assert_eq!(read_profile(&path).unwrap().name, "Press Ä");
    // Not a profile, and a profile that is not CMYK.
    let other = dir.join("other.icc");
    std::fs::write(&other, b"hello").unwrap();
    assert!(read_profile(&other).is_err());
    let mut rgb = fake_profile(false);
    rgb[16..20].copy_from_slice(b"RGB ");
    std::fs::write(&other, rgb).unwrap();
    assert!(read_profile(&other).err().unwrap().contains("RGB"));
    // A profile the colour engine can't use is embedded all the same; the numbers are
    // the plain ones.
    std::fs::write(&path, fake_profile(false)).unwrap();
    let colour = Colour::with_profile(read_profile(&path).unwrap());
    assert!(!colour.managed());
    assert_eq!(colour.vector([1.0, 0.0, 0.0]), [0.0, 1.0, 1.0, 0.0]);
    let (doc, frame) = page();
    let print = Print { cmyk: true, icc: Some(path.clone()), ..Default::default() };
    let (art, w, h) = svg(&doc, frame, 0.0);
    let d = dump(&pdf_bytes(&[(art.clone(), w, h)], &print).unwrap());
    assert!(find(&d.dicts, "/OutputIntents") && find(&d.dicts, "/GTS_PDFX") && find(&d.dicts, "/DestOutputProfile") && find(&d.dicts, "(Test Press)"));
    // The profile is in the file, byte for byte, as a stream of four components.
    let profile = d.file.objects.iter().find(|o| o.val.dict().and_then(|d| get(d, "N")).and_then(|n| n.num()) == Some(4.0)).unwrap();
    assert_eq!(pdf::decode(profile.val.dict().unwrap(), profile.stream.as_ref().unwrap()).unwrap(), fake_profile(false));
    assert!(!find(&d.dicts, "DeviceRGB") && !find(&d.dicts, "ICCBased"));
    // In RGB the profile is not used at all; a missing file is an error in CMYK.
    let d = dump(&pdf_bytes(&[(art.clone(), w, h)], &Print { cmyk: false, ..print.clone() }).unwrap());
    assert!(!find(&d.dicts, "OutputIntents"));
    assert!(pdf_bytes(&[(art, w, h)], &Print { icc: Some(dir.join("none.icc")), ..print }).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// With a real CMYK profile (Windows ships one), colours go through it; neutral vector
/// colours stay black only. Skipped where that profile is not installed.
#[test]
fn colours_through_a_real_profile() {
    let path = Path::new(r"C:\Windows\System32\spool\drivers\color\RSWOP.icm");
    let Ok(profile) = read_profile(path) else { return };
    assert!(!profile.name.is_empty());
    let colour = Colour::with_profile(profile);
    if !colour.managed() {
        return;
    }
    assert_eq!(colour.vector([0.0, 0.0, 0.0]), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(colour.vector([0.5, 0.5, 0.5]), [0.0, 0.0, 0.0, 0.5]);
    // Red is mostly magenta and yellow, with little cyan; white is (nearly) no ink.
    let [c, m, y, _] = colour.vector([1.0, 0.0, 0.0]);
    assert!(m > 0.8 && y > 0.8 && c < 0.3, "{c} {m} {y}");
    let white = colour.pixels(&[255, 255, 255]);
    assert!(white.iter().all(|v| *v < 12), "{white:?}");
    // A dark pixel of a picture carries a lot of ink, black among it.
    let black = colour.pixels(&[0, 0, 0]);
    assert!(black[3] > 150, "{black:?}");
    // Back through the profile, greys and a colour inside the gamut come out near what went
    // in. (Loose: this only guards against a conversion that is plainly wrong.)
    let press = moxcms::ColorProfile::new_from_slice(&std::fs::read(path).unwrap()).unwrap();
    let options = moxcms::TransformOptions { rendering_intent: moxcms::RenderingIntent::RelativeColorimetric, ..Default::default() };
    let back = press.create_transform_8bit(moxcms::Layout::Rgba, &moxcms::ColorProfile::new_srgb(), moxcms::Layout::Rgb, options).unwrap();
    let src = [40u8, 40, 40, 128, 128, 128, 200, 200, 200, 255, 255, 255, 200, 150, 100];
    let mut rgb = [0u8; 15];
    back.transform(&colour.pixels(&src), &mut rgb).unwrap();
    for (a, b) in src.iter().zip(&rgb) {
        assert!((*a as i32 - *b as i32).abs() <= 16, "{src:?} came back as {rgb:?}");
    }
}


/// The value of a function dictionary (type 2 with N 1, or type 3 made of those) at `t`.
fn function_at(f: &pdf::Dict, t: f32) -> Vec<f32> {
    let nums = |key: &str| get(f, key).and_then(|v| v.nums()).unwrap();
    if get(f, "FunctionType").and_then(|v| v.num()) == Some(2.0) {
        return nums("C0").iter().zip(nums("C1")).map(|(a, b)| (*a + (b - *a) * t as f64) as f32).collect();
    }
    let bounds = nums("Bounds");
    let Some(Val::Arr(pieces)) = get(f, "Functions") else { panic!("no pieces") };
    assert_eq!(pieces.len(), bounds.len() + 1);
    let i = bounds.iter().filter(|b| t as f64 >= **b).count();
    let (lo, hi) = (if i == 0 { 0.0 } else { bounds[i - 1] }, bounds.get(i).copied().unwrap_or(1.0));
    function_at(pieces[i].dict().unwrap(), ((t as f64 - lo) / (hi - lo)) as f32)
}

#[test]
fn gradients_keep_their_colours_in_between() {
    let colour = Colour::naive();
    let far = |f: &pdf::Dict, from: [f32; 3], to: [f32; 3]| {
        (0..=40).map(|i| i as f32 / 40.0).fold(0.0_f32, |far, t| {
            let want = colour.vector([0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t));
            function_at(f, t).iter().zip(want).fold(far, |far, (a, b)| far.max((a - b).abs()))
        })
    };
    // Yellow to cyan, black to white: straight lines in CMYK as well, one piece.
    for (from, to) in [([1.0, 1.0, 0.0], [0.0, 1.0, 1.0]), ([0.0, 0.0, 0.0], [1.0, 1.0, 1.0])] {
        let f = pdf::blend_to_cmyk(from, to, &colour);
        assert_eq!(get(&f, "FunctionType"), Some(&Val::Num(2.0)));
        assert!(far(&f, from, to) < 0.02);
    }
    // Black to red: halfway is dark red (all the magenta and yellow, half the black), which a
    // straight line from black ink to red would miss by half. Cut into pieces it is close.
    let (from, to) = ([0.0, 0.0, 0.0], [1.0, 0.0, 0.0]);
    let f = pdf::blend_to_cmyk(from, to, &colour);
    assert_eq!(get(&f, "FunctionType"), Some(&Val::Num(3.0)));
    let mid = function_at(&f, 0.5);
    assert!(close(mid[0], 0.0) && close(mid[1], 1.0) && close(mid[2], 1.0) && close(mid[3], 0.5), "{mid:?}");
    // (Right at black the colour is black ink only, and the first piece leaves it at once:
    // that is where the blend is furthest from the RGB one.)
    assert_eq!(function_at(&f, 0.0), [0.0, 0.0, 0.0, 1.0]);
    assert_eq!(function_at(&f, 1.0), [0.0, 1.0, 1.0, 0.0]);
    let off = (4..=40).map(|i| i as f32 / 40.0).fold(0.0_f32, |far, t| {
        let want = colour.vector([t, 0.0, 0.0]);
        function_at(&f, t).iter().zip(want).fold(far, |far, (a, b)| far.max((a - b).abs()))
    });
    assert!(off < 0.02, "{off}");
    // The pieces have four components and say so.
    let Some(Val::Arr(pieces)) = get(&f, "Functions") else { panic!() };
    assert_eq!(pieces.len(), 16);
    assert_eq!(get(&f, "Range").and_then(|v| v.nums()).unwrap().len(), 8);
    assert_eq!(get(&f, "Encode").and_then(|v| v.nums()).unwrap().len(), 32);
}

#[test]
fn bitmaps_in_a_vector_pdf_have_a_sensible_resolution() {
    // 72 px per inch: 3 pixels per px (216 dpi). 300: the document's own pixels. Finer than
    // that: 300 dpi.
    assert!(close(super::raster_scale(72.0), 3.0));
    assert!(close(super::raster_scale(150.0), 1.44));
    assert!(close(super::raster_scale(300.0), 1.0));
    assert!(close(super::raster_scale(600.0), 0.5));
}
