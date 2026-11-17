//! The commands around `svg_import` and `outline`: an SVG that is dropped, pasted, picked or
//! taken from the libraries becomes editable layers, and Outline stroke (Ctrl+Shift+O) turns
//! strokes into filled paths.

use super::*;
use crate::svg_import::{self, Imported};
use std::collections::HashMap;

/// A file name without its extension.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |s| s.0).trim()
}

impl App {
    /// Give imported nodes their place in this document: images become assets, groups and
    /// nodes get ids of their own. Result: the frame holding them, still at (0, 0).
    pub(super) fn adopt_svg(&mut self, mut imp: Imported, keep_groups: bool) -> Node {
        for (id, bytes) in std::mem::take(&mut imp.images) {
            let key = self.doc.add_asset(bytes);
            if let Some(n) = imp.root.find_mut(id) {
                n.image = Some(key);
            }
        }
        let mut map: HashMap<u64, u64> = HashMap::new();
        if keep_groups {
            for (g, name) in std::mem::take(&mut imp.groups) {
                let new = self.doc.alloc_id();
                self.doc.group_names.insert(new, name);
                map.insert(g, new);
            }
        }
        imp.root.walk_mut(&mut |n| {
            if n.group != 0 {
                n.group = map.get(&n.group).copied().unwrap_or(0);
            }
        });
        self.doc.reassign_ids(&mut imp.root);
        imp.root
    }

    /// Import an SVG as editable layers, centred at `at` (a screen position; default: the
    /// middle of the canvas), as one undo step. Normally they sit in a frame the size of the
    /// SVG, named `name`. With `side` (icons) there is no frame: the layers are scaled so the
    /// longest side of the SVG is `side`, and form one group. Result: what was imported, in
    /// words; an error when the SVG cannot be read or has nothing in it to import.
    pub(super) fn add_svg(&mut self, bytes: &[u8], name: &str, at: Option<Pos2>, side: Option<f32>) -> Result<String, String> {
        let imp = svg_import::import(bytes, name)?;
        if imp.shapes == 0 {
            return Err("nothing in it that can be imported".into());
        }
        let summary = imp.summary();
        let screen = at.filter(|p| self.canvas_rect.contains(*p)).unwrap_or(self.canvas_rect.center());
        let c = self.view.to_world(self.canvas_rect.min, screen);
        let pre = self.doc.clone();
        let mut root = self.adopt_svg(imp, side.is_none());
        root.translate((c.x - root.w * 0.5).round(), (c.y - root.h * 0.5).round());
        match side {
            None => {
                let parent = self.doc.frame_for(&root, c.x, c.y, &[]);
                self.sel = vec![root.id];
                self.doc.insert(root, parent);
            }
            Some(side) => {
                let (cx, cy) = root.center();
                let k = side / root.w.max(root.h).max(1.0);
                scale_node(&mut root, k, cx, cy);
                let parent = self.doc.frame_at(c.x, c.y, &[]);
                let mut nodes = std::mem::take(&mut root.children);
                if let [only] = &mut nodes[..] {
                    only.name = name.to_owned();
                } else {
                    let g = self.doc.alloc_id();
                    self.doc.group_names.insert(g, name.to_owned());
                    nodes.iter_mut().for_each(|n| n.group = g);
                }
                self.sel = nodes.iter().map(|n| n.id).collect();
                for n in nodes {
                    self.doc.insert(n, parent);
                }
            }
        }
        self.push_undo(pre);
        self.tool = Tool::Select;
        Ok(summary)
    }

    /// A file that was dropped, picked or taken from the library: an SVG becomes editable
    /// layers, anything else (and an SVG that cannot be converted) an image.
    pub(super) fn place_file(&mut self, bytes: Vec<u8>, name: &str, at: Option<Pos2>) -> bool {
        if render::is_svg(&bytes) {
            if let Ok(summary) = self.add_svg(&bytes, stem(name), at, None) {
                self.status = format!("Imported {summary} from \"{name}\" as editable layers. (File > Place SVG as image keeps it one picture.)");
                return true;
            }
        }
        self.add_image(bytes, name, at)
    }

    /// File > Import… / Place SVG as image…
    pub(super) fn import_dialog(&mut self, as_image: bool) {
        let dialog = if as_image {
            rfd::FileDialog::new().add_filter("SVG", &["svg"])
        } else {
            rfd::FileDialog::new().add_filter("Images and SVG", &["svg", "png", "jpg", "jpeg", "webp", "gif", "bmp"])
        };
        let Some(paths) = dialog.pick_files() else { return };
        let mut offset = 0.0;
        for path in paths {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let Ok(bytes) = std::fs::read(&path) else {
                self.status = format!("Couldn't read \"{name}\".");
                continue;
            };
            let at = Some(self.canvas_rect.center() + vec2(offset, offset));
            let placed = if as_image { self.add_image(bytes, &name, at) } else { self.place_file(bytes, &name, at) };
            if placed {
                offset += 24.0;
            }
        }
    }

    /// Pasted text that is SVG markup becomes editable layers. Result: it was.
    pub(super) fn paste_svg(&mut self, text: &str, at: Option<Pos2>) -> bool {
        if !render::is_svg(text.as_bytes()) {
            return false;
        }
        match self.add_svg(text.as_bytes(), "Pasted SVG", at, None) {
            Ok(summary) => {
                self.status = format!("Pasted {summary} as editable layers.");
                true
            }
            Err(_) => false,
        }
    }

    /// Ctrl+Shift+O: the stroke of each selected shape becomes a filled path (see
    /// `Doc::outline_strokes`), as one undo step. The outlines are selected afterwards.
    pub(super) fn outline_stroke(&mut self) {
        if self.sel.is_empty() {
            self.status = "Select a shape with a stroke to outline it.".into();
            return;
        }
        if self.sel_locked() || self.refuse_pinned() {
            return;
        }
        self.ext.path_edit = None;
        let pre = self.doc.clone();
        let ids = self.sel.clone();
        let done = self.doc.outline_strokes(&ids);
        let mut left = Vec::new();
        if done.text > 0 {
            left.push("the stroke of a text can't be outlined");
        }
        if done.skipped > 0 {
            left.push("the border of a frame only when it is inside the frame");
        }
        if done.outlines.is_empty() {
            self.doc = pre;
            self.status = match left.first() {
                Some(why) => format!("Nothing outlined: {why}."),
                None => "Nothing to outline: the selection has no stroke.".into(),
            };
            return;
        }
        self.push_undo(pre);
        self.sel = done.ids;
        self.tool = Tool::Select;
        let n = done.outlines.len();
        let note = left.first().map(|why| format!(" Left as it was: {why}.")).unwrap_or_default();
        self.status = if n == 1 { format!("Outlined the stroke: it is a filled path now.{note}") } else { format!("Outlined {n} strokes: they are filled paths now.{note}") };
    }
}

// ---------------------------------------------------------------- bench

/// A hand-written SVG that uses what the import must cope with: groups with transforms and
/// opacity, `use`, gradients in both kinds of units, holes under both fill rules, parts that
/// overlap, open and dashed strokes, every kind of curve, text, and a few things Kanvas has
/// no place for (a filter, a mask, a clip path).
const BENCH_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="400" height="300" viewBox="0 0 800 600">
<defs>
  <linearGradient id="sky" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="800" y2="600"><stop offset="0" stop-color="#1e3a8a"/><stop offset="0.55" stop-color="#7c3aed"/><stop offset="1" stop-color="#f97316"/></linearGradient>
  <linearGradient id="bar" x1="0" y1="0" x2="1" y2="0" gradientTransform="rotate(30 0.5 0.5)"><stop offset="0" stop-color="#22d3ee"/><stop offset="1" stop-color="#fde047"/></linearGradient>
  <radialGradient id="sun" cx="0.5" cy="0.5" r="0.5"><stop offset="0" stop-color="#fff7ae"/><stop offset="0.7" stop-color="#fbbf24"/><stop offset="1" stop-color="#f59e0b" stop-opacity="0.2"/></radialGradient>
  <radialGradient id="pill" cx="0.3" cy="0.4" r="0.6"><stop offset="0" stop-color="#ffffff"/><stop offset="1" stop-color="#10b981"/></radialGradient>
  <filter id="soft"><feGaussianBlur stdDeviation="3"/></filter>
  <mask id="fade"><rect width="800" height="600" fill="#fff"/></mask>
  <clipPath id="page"><rect width="800" height="600"/></clipPath>
  <clipPath id="disc"><circle cx="700" cy="520" r="40"/></clipPath>
  <path id="leaf" d="M0 0 C20 -30 60 -30 80 0 C60 30 20 30 0 0 Z"/>
</defs>
<g clip-path="url(#page)">
  <rect x="20" y="20" width="760" height="560" rx="48" fill="url(#sky)"/>
  <circle cx="640" cy="140" r="80" fill="url(#sun)" stroke="#fde68a" stroke-width="6"/>
  <g id="hills" opacity="0.85">
    <path d="M20 460 Q200 300 380 440 T780 400 V532 A48 48 0 0 1 732 580 H68 A48 48 0 0 1 20 532 Z" fill="#064e3b"/>
    <path d="M20 520 C180 420 300 520 460 470 S700 430 780 500 V532 A48 48 0 0 1 732 580 H68 A48 48 0 0 1 20 532 Z" fill="#10b981" fill-opacity="0.8"/>
  </g>
  <g id="badge" transform="translate(150 170) rotate(-15) scale(1.2)">
    <path d="M-80 -60 H80 V60 H-80 Z M-50 -30 V30 H-10 V-30 Z M10 -30 H50 V30 H10 Z" fill="#f8fafc" stroke="#0f172a" stroke-width="5"/>
    <path fill-rule="evenodd" d="M-36 -16 H-24 V16 H-36 Z M24 -16 H36 V16 H24 Z M26 -12 H34 V12 H26 Z" fill="#ef4444"/>
  </g>
  <g id="overlap" transform="translate(330 90)">
    <path d="M0 0 H90 V90 H0 Z M50 50 H140 V140 H50 Z M70 20 a25 25 0 1 0 0.1 0 Z" fill="#fb7185" stroke="#881337" stroke-width="3"/>
  </g>
  <g id="ghosts" opacity="0.5">
    <rect x="60" y="330" width="120" height="80" fill="#38bdf8"/>
    <rect x="120" y="360" width="120" height="80" fill="#f472b6" opacity="0.8"/>
  </g>
  <polyline points="300,330 340,290 380,330 420,290 460,330 500,290" fill="none" stroke="#fef3c7" stroke-width="8" stroke-dasharray="18 10"/>
  <path d="M300 380 C340 340 380 420 420 380 Q450 350 480 380 A30 30 0 0 0 540 380 L560 350" fill="none" stroke="#a7f3d0" stroke-width="6"/>
  <rect x="560" y="260" width="180" height="60" rx="30" fill="url(#bar)" stroke="#0f172a" stroke-width="2"/>
  <ellipse cx="650" cy="380" rx="90" ry="34" fill="url(#pill)" transform="skewX(-12) translate(80 0)"/>
  <g fill="#bbf7d0" stroke="#14532d" stroke-width="2">
    <use xlink:href="#leaf" x="80" y="500"/>
    <use xlink:href="#leaf" transform="translate(200 520) rotate(-25) scale(1.4 1)"/>
    <use xlink:href="#leaf" transform="translate(360 530) scale(-1 1.3)" fill="#fde68a"/>
  </g>
  <text x="250" y="262" font-family="Arial, Helvetica, sans-serif" font-size="56" font-weight="bold" fill="#ffffff">Kanvas</text>
  <circle cx="470" cy="500" r="26" fill="#0ea5e9" filter="url(#soft)"/>
  <rect x="520" y="470" width="70" height="60" fill="#e879f9" mask="url(#fade)"/>
  <rect x="650" y="470" width="100" height="100" fill="#facc15" clip-path="url(#disc)"/>
</g>
</svg>"##;

/// Icons in the styles the icon library serves: strokes with round ends, and a filled glyph
/// whose holes rely on the direction of the contours.
const BENCH_ICONS: [(&str, &str); 3] = [
    (
        "heart",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7Z"/></svg>"##,
    ),
    (
        "account",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24"><path fill="currentColor" d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm0 3c1.66 0 3 1.34 3 3s-1.34 3-3 3-3-1.34-3-3 1.34-3 3-3zm0 14.2c-2.5 0-4.71-1.28-6-3.22.03-1.99 4-3.08 6-3.08 1.99 0 5.97 1.09 6 3.08-1.29 1.94-3.5 3.22-6 3.22z"/></svg>"##,
    ),
    (
        "sun",
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/></svg>"##,
    ),
];

/// An SVG of `count` small shapes with curves, holes, strokes and groups, for timing.
fn big_svg(count: usize) -> String {
    use std::fmt::Write;
    let cols = (count as f32).sqrt().ceil() as usize;
    let side = cols * 20;
    let mut s = format!(r##"<svg xmlns="http://www.w3.org/2000/svg" width="{side}" height="{side}" viewBox="0 0 {side} {side}">"##);
    for i in 0..count {
        let (x, y) = ((i % cols) * 20, (i / cols) * 20);
        let hue = (i * 37) % 360;
        if i % 10 == 0 {
            let _ = write!(s, r##"<g transform="translate({x} {y})" opacity="0.9">"##);
        } else {
            let _ = write!(s, r##"<g transform="translate({x} {y})">"##);
        }
        let _ = match i % 4 {
            0 => write!(s, r##"<path fill="hsl({hue},70%,55%)" d="M2 10C2 4 6 2 10 2s8 2 8 8-4 8-8 8-8-2-8-8zm5 0c0 2 1 3 3 3s3-1 3-3-1-3-3-3-3 1-3 3z"/>"##),
            1 => write!(s, r##"<path fill="none" stroke="hsl({hue},70%,45%)" stroke-width="1.5" d="M2 16Q6 2 10 10T18 4"/>"##),
            2 => write!(s, r##"<rect x="3" y="3" width="14" height="14" rx="4" fill="hsl({hue},60%,60%)" stroke="#222" stroke-width="1"/>"##),
            _ => write!(s, r##"<path fill="hsl({hue},70%,55%)" d="M2 2h10v10h-10zM8 8h10v10h-10z"/>"##),
        };
        s.push_str("</g>");
    }
    s.push_str("</svg>");
    s
}

impl App {
    /// `--bench` with `KANVAS_BENCH_SVG`: on a white stage, the test SVG imported as layers
    /// beside the same SVG as a bitmap; three icons as vectors above the same as bitmaps; and
    /// a row of stroked shapes above the same row after Outline stroke. `=svg`, `=icons`,
    /// `=outline`, `=outline2`, `=view:x0,y0,x1,y1`: the view goes close to that part. `=big`: an SVG of 4000
    /// shapes is imported as well. `=remote`: the two remote commands are run too. `=keys`: the
    /// open path of the first row is selected and Ctrl+Shift+O is pressed. Timings go to stderr.
    pub(super) fn bench_svg(&mut self, mode: &str, inject: &mut Vec<(u32, egui::Event)>) {
        use crate::outline::{set_contours, Contour};
        let (x, y) = (-3000.0, 14000.0);
        let stage_id = self.doc.alloc_id();
        let mut stage = Node::new(stage_id, Kind::Frame, x, y);
        (stage.name, stage.w, stage.h, stage.fill) = ("SVG demo".into(), 1500.0, 900.0, Some([250, 250, 248, 255]));
        self.doc.insert(stage, None);

        // The import, and the old way beside it.
        let start = std::time::Instant::now();
        match svg_import::import(BENCH_SVG.as_bytes(), "test") {
            Ok(imp) => {
                let summary = imp.summary();
                let groups: Vec<String> = imp.groups.iter().map(|g| g.1.clone()).collect();
                let mut root = self.adopt_svg(imp, true);
                root.translate(x + 40.0, y + 40.0);
                self.doc.insert(root, Some(stage_id));
                eprintln!("SVG import: {summary} in {:.2} ms; groups {groups:?}", start.elapsed().as_secs_f32() * 1000.0);
            }
            Err(e) => eprintln!("SVG import failed: {e}"),
        }
        let bitmap = |app: &mut Self, svg: &str, name: &str, (bx, by, w, h): (f32, f32, f32, f32)| {
            let asset = app.doc.add_asset(svg.as_bytes().to_vec());
            let mut n = Node::new(app.doc.alloc_id(), Kind::Image, x + bx, y + by);
            (n.name, n.w, n.h, n.image, n.stroke_w) = (name.into(), w, h, Some(asset), 0.0);
            app.doc.insert(n, Some(stage_id));
        };
        bitmap(self, BENCH_SVG, "test as image", (480.0, 40.0, 400.0, 300.0));

        // Icons: as vectors (the way the icon library adds them now), and as images.
        let o = self.canvas_rect.min;
        for (i, (name, svg)) in BENCH_ICONS.iter().enumerate() {
            let (ix, iy) = (940.0 + i as f32 * 180.0, 50.0);
            let before = self.undo.len();
            let at = self.view.to_world(o, self.canvas_rect.center());
            match self.add_svg(svg.as_bytes(), name, None, Some(140.0)) {
                Ok(summary) => {
                    // Placed in the middle of the view: move it to its spot on the stage.
                    let ids = self.sel.clone();
                    for id in &ids {
                        if let Some(mut n) = self.doc.remove(*id) {
                            n.translate(x + ix + 70.0 - at.x.round(), y + iy + 70.0 - at.y.round());
                            self.doc.insert(n, Some(stage_id));
                        }
                    }
                    eprintln!("SVG icon {name}: {summary}, {} layer(s), undo steps +{}", ids.len(), self.undo.len() - before);
                }
                Err(e) => eprintln!("SVG icon {name} failed: {e}"),
            }
            bitmap(self, svg, name, (ix, iy + 170.0, 140.0, 140.0));
        }

        // Stroked shapes: a row as they are, and a row outlined.
        let (blue, ink, pale) = ([37, 99, 235, 255], [15, 23, 42, 255], [219, 234, 254, 255]);
        let pt = |px: f32, py: f32| render::PenPt { x: px, y: py, hin: [0.0; 2], hout: [0.0; 2], r: None };
        let mut rows: Vec<Vec<u64>> = vec![Vec::new(), Vec::new()];
        for (row, ids) in rows.iter_mut().enumerate() {
            let top = y + 420.0 + row as f32 * 220.0;
            let mut col = 0;
            let mut add = |app: &mut Self, kind: Kind, (w, h): (f32, f32), edit: &dyn Fn(&mut Node, f32, f32)| {
                let (nx, ny) = (x + 40.0 + col as f32 * 112.0, top);
                col += 1;
                let mut n = Node::new(app.doc.alloc_id(), kind, nx + (90.0 - w) * 0.5, ny + (90.0 - h) * 0.5);
                (n.w, n.h, n.stroke, n.stroke_w) = (w, h, Some(blue), 10.0);
                edit(&mut n, nx, ny);
                ids.push(n.id);
                app.doc.insert(n, Some(stage_id));
            };
            for align in 0..3u8 {
                add(self, Kind::Rect, (80.0, 80.0), &|n, _, _| (n.radius, n.fill, n.stroke_align) = (20.0, Some(pale), Some(align)));
            }
            for align in 0..3u8 {
                add(self, Kind::Ellipse, (90.0, 60.0), &|n, _, _| n.stroke_align = Some(align));
            }
            // An open path with a corner and a curve.
            add(self, Kind::Path, (90.0, 90.0), &|n, nx, ny| {
                let mut wave = vec![pt(nx, ny + 80.0), pt(nx + 25.0, ny + 10.0), pt(nx + 50.0, ny + 70.0), pt(nx + 90.0, ny + 20.0)];
                (wave[2].hout, wave[3].hin) = ([30.0, 20.0], [-30.0, 30.0]);
                set_contours(n, vec![Contour { pts: wave, closed: false }]);
                n.stroke_w = 8.0;
            });
            // A closed path with a hole, filled.
            add(self, Kind::Path, (90.0, 90.0), &|n, nx, ny| {
                let outer = vec![pt(nx, ny + 10.0), pt(nx + 45.0, ny), pt(nx + 90.0, ny + 10.0), pt(nx + 80.0, ny + 90.0), pt(nx + 10.0, ny + 90.0)];
                let hole = vec![pt(nx + 30.0, ny + 35.0), pt(nx + 60.0, ny + 35.0), pt(nx + 60.0, ny + 65.0), pt(nx + 30.0, ny + 65.0)];
                set_contours(n, vec![Contour { pts: outer, closed: true }, Contour { pts: hole, closed: true }]);
                (n.fill, n.stroke_w, n.stroke) = (Some([254, 215, 170, 255]), 6.0, Some(ink));
            });
            add(self, Kind::Line, (80.0, 60.0), &|n, _, _| n.stroke_w = 8.0);
            // Dashes; a turned and flipped rectangle with a see-through stroke; a star.
            add(self, Kind::Rect, (80.0, 80.0), &|n, _, _| (n.stroke_w, n.dash, n.gap, n.stroke_align) = (6.0, 14.0, 8.0, Some(1)));
            add(self, Kind::Rect, (80.0, 50.0), &|n, _, _| (n.radius, n.rotation, n.flip_h, n.fill, n.stroke_align, n.stroke) = (12.0, 30.0, true, Some(pale), Some(2), Some([220, 38, 38, 160])));
            add(self, Kind::Path, (90.0, 90.0), &|n, nx, ny| {
                let star: Vec<render::PenPt> = (0..10)
                    .map(|i| {
                        let (a, r) = (std::f32::consts::TAU * i as f32 / 10.0 - std::f32::consts::FRAC_PI_2, if i % 2 == 0 { 42.0 } else { 17.0 });
                        pt(nx + 45.0 + r * a.cos(), ny + 45.0 + r * a.sin())
                    })
                    .collect();
                set_contours(n, vec![Contour { pts: star, closed: true }]);
                n.stroke_w = 7.0;
            });
            add(self, Kind::Ellipse, (90.0, 70.0), &|n, _, _| (n.stroke_w, n.dash, n.gap, n.fill) = (5.0, 10.0, 6.0, Some(pale)));
        }
        let undo = self.undo.len();
        self.sel.clone_from(&rows[1]);
        let start = std::time::Instant::now();
        self.outline_stroke();
        let made: Vec<(Kind, usize, usize)> = self.sel.iter().filter_map(|id| self.doc.find(*id)).filter(|n| n.name.ends_with("outline")).map(|n| (n.kind, n.points.len(), n.holes.len())).collect();
        eprintln!("OUTLINE: {} shapes in {:.2} ms, undo steps +{}; status: {}", rows[1].len(), start.elapsed().as_secs_f32() * 1000.0, self.undo.len() - undo, self.status);
        eprintln!("OUTLINE (kind, points, further contours) per outline: {made:?}");

        if mode == "remote" {
            let args = serde_json::json!({ "svg": BENCH_ICONS[1].1, "name": "remote icon", "x": 1300.0, "y": 420.0, "parent": stage_id });
            eprintln!("SVG remote import_svg: {}", self.remote_bench("import_svg", &args));
            eprintln!("SVG remote import_svg, garbage: {}", self.remote_bench("import_svg", &serde_json::json!({ "svg": "<svg" })));
            let args = serde_json::json!({ "ids": [rows[0][0], rows[0][3]] });
            eprintln!("SVG remote outline_stroke: {}", self.remote_bench("outline_stroke", &args));
            eprintln!("SVG remote outline_stroke again (no strokes left): {}", self.remote_bench("outline_stroke", &args));
            // The ways a file or markup comes in by hand: paste, and drop / File > Import.
            let look = |app: &Self| app.sel.first().and_then(|id| app.doc.find(*id)).map(|n| (n.name.clone(), n.kind, n.w, n.h, n.children.len()));
            let undo = self.undo.len();
            self.paste_text(BENCH_ICONS[2].1, false, None);
            eprintln!("SVG pasted markup: {:?}, undo steps +{}; status: {}", look(self), self.undo.len() - undo, self.status);
            self.paste_text("<svg but not really", false, None);
            eprintln!("SVG pasted text that only starts like one: {:?}", look(self).map(|l| (l.0, l.1)));
            self.place_file(BENCH_SVG.as_bytes().to_vec(), "drawing.svg", None);
            eprintln!("SVG dropped file: {:?}; status: {}", look(self), self.status);
            self.place_file(b"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"10\" height=\"10\"/>".to_vec(), "empty.svg", None);
            eprintln!("SVG dropped file with nothing in it: {:?}; status: {}", look(self), self.status);
            for _ in 0..4 {
                self.undo_cmd();
            }
        }
        if mode == "big" {
            let svg = big_svg(4000);
            let start = std::time::Instant::now();
            match svg_import::import(svg.as_bytes(), "big") {
                Ok(imp) => {
                    let (summary, parsed) = (imp.summary(), start.elapsed().as_secs_f32() * 1000.0);
                    let mut root = self.adopt_svg(imp, true);
                    root.translate(x, y + 940.0);
                    let pre = self.doc.clone();
                    self.doc.insert(root, None);
                    self.push_undo(pre);
                    eprintln!("SVG big: {summary} from {} KB converted in {parsed:.1} ms, in the document after {:.1} ms", svg.len() / 1024, start.elapsed().as_secs_f32() * 1000.0);
                }
                Err(e) => eprintln!("SVG big failed: {e}"),
            }
        }
        self.sel.clear();
        if mode == "keys" {
            self.sel = vec![rows[0][6]];
            let modifiers = Modifiers::COMMAND | Modifiers::SHIFT;
            let key = |pressed| egui::Event::Key { key: Key::O, physical_key: None, pressed, repeat: false, modifiers };
            inject.extend([(220, key(true)), (222, key(false))]);
        }
        // `=view:x0,y0,x1,y1`: that part of the stage.
        let part: Vec<f32> = mode.strip_prefix("view:").map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
        let view = match mode {
            _ if part.len() == 4 => (x + part[0], y + part[1], x + part[2], y + part[3]),
            "svg" => (x + 20.0, y + 20.0, x + 900.0, y + 360.0),
            "icons" => (x + 920.0, y + 30.0, x + 1480.0, y + 380.0),
            "outline" | "keys" => (x + 20.0, y + 400.0, x + 760.0, y + 740.0),
            "outline2" => (x + 740.0, y + 400.0, x + 1480.0, y + 740.0),
            "big" => (x, y + 940.0, x + 700.0, y + 1400.0),
            _ => (x - 20.0, y - 20.0, x + 1520.0, y + 920.0),
        };
        self.zoom_to(view, 16.0);
    }
}
