//! Editable SVG import: an SVG becomes a frame of Kanvas nodes instead of a bitmap.
//!
//! `usvg` does the reading (CSS, `use`, units, basic shapes and text as paths, transforms); what
//! is left is a tree of groups, paths and images. Every path becomes a vector node with its
//! transform baked into the points, gradients are carried over into the box of that node, and
//! groups become Kanvas groups. What Kanvas cannot show (masks, clip paths, filters, pattern
//! fills) is left out and counted.

use crate::doc::{Gradient, Kind, Node, Stop};
use crate::outline::{self, Contour, Rule};
use resvg::tiny_skia::Transform;
use resvg::usvg;

/// The result of an import. Ids (of nodes and of groups) are small numbers of their own,
/// starting at 1: whoever puts the nodes in a document gives them real ones.
pub struct Imported {
    /// A frame the size of the SVG at (0, 0), holding the shapes in stacking order.
    pub root: Node,
    /// Group id → name.
    pub groups: Vec<(u64, String)>,
    /// Embedded bitmaps: (id of the image node, file bytes).
    pub images: Vec<(u64, Vec<u8>)>,
    /// How many layers were made.
    pub shapes: usize,
    /// What was left out: (kind, how often), e.g. ("mask", 2).
    pub dropped: Vec<(&'static str, usize)>,
}

impl Imported {
    pub fn dropped_count(&self) -> usize {
        self.dropped.iter().map(|d| d.1).sum()
    }

    /// "42 shapes" plus, if anything was left out, " (2 effects not supported: mask, filter)".
    pub fn summary(&self) -> String {
        let shapes = format!("{} shape{}", self.shapes, if self.shapes == 1 { "" } else { "s" });
        let n = self.dropped_count();
        if n == 0 {
            return shapes;
        }
        let kinds: Vec<&str> = self.dropped.iter().map(|d| d.0).collect();
        format!("{shapes} ({n} effect{} not supported: {})", if n == 1 { "" } else { "s" }, kinds.join(", "))
    }
}

struct Cx {
    next: u64,
    nodes: Vec<Node>,
    groups: Vec<(u64, String)>,
    images: Vec<(u64, Vec<u8>)>,
    dropped: Vec<(&'static str, usize)>,
    size: (f32, f32),
    /// Groups without a name of their own so far.
    unnamed: usize,
}

/// What a node inherits from the groups around it.
#[derive(Clone, Copy)]
struct Inherit {
    ts: Transform,
    opacity: f32,
    blend: u8,
    /// The Kanvas group its layers go into (0 = none yet).
    group: u64,
}

fn parse(svg: &[u8]) -> Result<usvg::Tree, String> {
    let mut opt = usvg::Options::default();
    // Loading the system fonts is slow; they are only needed to turn text into paths.
    if svg.windows(5).any(|w| w == b"<text") {
        opt.fontdb_mut().load_system_fonts();
    }
    usvg::Tree::from_data(svg, &opt).map_err(|e| e.to_string())
}

/// Convert an SVG file (or markup) into Kanvas nodes. `name` names the frame around them.
pub fn import(svg: &[u8], name: &str) -> Result<Imported, String> {
    let tree = parse(svg)?;
    let size = (tree.size().width(), tree.size().height());
    let mut cx = Cx { next: 1, nodes: Vec::new(), groups: Vec::new(), images: Vec::new(), dropped: Vec::new(), size, unnamed: 0 };
    let top = Inherit { ts: Transform::identity(), opacity: 1.0, blend: 0, group: 0 };
    cx.group(tree.root(), top, true);
    // A group of one is no group.
    for (g, _) in cx.groups.clone() {
        if cx.nodes.iter().filter(|n| n.group == g).count() < 2 {
            cx.nodes.iter_mut().filter(|n| n.group == g).for_each(|n| n.group = 0);
            cx.groups.retain(|x| x.0 != g);
        }
    }
    let mut root = Node::new(cx.id(), Kind::Frame, 0.0, 0.0);
    (root.name, root.w, root.h) = (name.to_owned(), size.0.max(1.0), size.1.max(1.0));
    let shapes = cx.nodes.len();
    root.children = cx.nodes;
    Ok(Imported { root, groups: cx.groups, images: cx.images, shapes, dropped: cx.dropped })
}

fn rgba(c: usvg::Color, opacity: f32) -> [u8; 4] {
    [c.red, c.green, c.blue, (opacity.clamp(0.0, 1.0) * 255.0).round() as u8]
}

/// One colour for a paint that Kanvas has no room for (the stroke takes a plain colour only).
fn flat_color(paint: &usvg::Paint, opacity: f32) -> [u8; 4] {
    let average = |stops: &[usvg::Stop]| {
        let n = stops.len().max(1) as f32;
        let sum = stops.iter().fold([0.0f32; 4], |s, st| {
            let c = st.color();
            [s[0] + c.red as f32, s[1] + c.green as f32, s[2] + c.blue as f32, s[3] + st.opacity().get()]
        });
        [(sum[0] / n).round() as u8, (sum[1] / n).round() as u8, (sum[2] / n).round() as u8, (sum[3] / n * opacity.clamp(0.0, 1.0) * 255.0).round() as u8]
    };
    match paint {
        usvg::Paint::Color(c) => rgba(*c, opacity),
        usvg::Paint::LinearGradient(g) => average(g.stops()),
        usvg::Paint::RadialGradient(g) => average(g.stops()),
        usvg::Paint::Pattern(_) => rgba(usvg::Color::new_rgb(128, 128, 128), opacity),
    }
}

fn stops(list: &[usvg::Stop], opacity: f32) -> Vec<Stop> {
    list.iter().map(|s| Stop { pos: s.offset().get(), color: rgba(s.color(), s.opacity().get() * opacity) }).collect()
}

/// The uniform part of a transform: what a stroke width is multiplied by.
fn scale_of(ts: Transform) -> f32 {
    (ts.sx * ts.sy - ts.kx * ts.ky).abs().sqrt()
}

fn apply(ts: Transform, x: f32, y: f32) -> (f32, f32) {
    (ts.sx * x + ts.kx * y + ts.tx, ts.ky * x + ts.sy * y + ts.ty)
}

impl Cx {
    fn id(&mut self) -> u64 {
        self.next += 1;
        self.next - 1
    }

    fn drop_one(&mut self, kind: &'static str) {
        match self.dropped.iter_mut().find(|d| d.0 == kind) {
            Some(d) => d.1 += 1,
            None => self.dropped.push((kind, 1)),
        }
    }

    fn new_group(&mut self, name: &str) -> u64 {
        let g = self.id();
        if name.is_empty() {
            self.unnamed += 1;
        }
        self.groups.push((g, if name.is_empty() { format!("Group {}", self.unnamed) } else { name.to_owned() }));
        g
    }

    /// A clip path that is just the page: the frame around the import does that already.
    fn clips_the_page(&self, clip: &usvg::ClipPath, ts: Transform) -> bool {
        if clip.clip_path().is_some() {
            return false;
        }
        let [usvg::Node::Path(p)] = clip.root().children() else { return false };
        let ts = ts.pre_concat(clip.transform());
        let pts: Vec<(f32, f32)> = p.data().points().iter().map(|q| apply(ts, q.x, q.y)).collect();
        let b = pts.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, q| (b.0.min(q.0), b.1.min(q.1), b.2.max(q.0), b.3.max(q.1)));
        let on = |v: f32, a: f32, c: f32| (v - a).abs() < 0.01 || (v - c).abs() < 0.01;
        let rect = pts.len() <= 5 && pts.iter().all(|q| on(q.0, b.0, b.2) && on(q.1, b.1, b.3));
        rect && b.0 <= 0.01 && b.1 <= 0.01 && b.2 >= self.size.0 - 0.01 && b.3 >= self.size.1 - 0.01
    }

    /// `top`: still looking for the level where the layers of the file are (wrappers with a
    /// single group in them are passed through). Each group right below that level becomes a
    /// Kanvas group; groups further down are part of it, as Kanvas groups do not nest.
    fn group(&mut self, g: &usvg::Group, inh: Inherit, top: bool) {
        let mut inh = inh;
        inh.ts = inh.ts.pre_concat(g.transform());
        inh.opacity *= g.opacity().get();
        match g.blend_mode() {
            usvg::BlendMode::Normal => {}
            usvg::BlendMode::Darken => inh.blend = 1,
            usvg::BlendMode::Multiply => inh.blend = 2,
            usvg::BlendMode::Lighten => inh.blend = 3,
            usvg::BlendMode::Screen => inh.blend = 4,
            usvg::BlendMode::Exclusion => inh.blend = 6,
            _ => self.drop_one("blend mode"),
        }
        if let Some(clip) = g.clip_path() {
            if !self.clips_the_page(clip, inh.ts) {
                self.drop_one("clip path");
            }
        }
        if g.mask().is_some() {
            self.drop_one("mask");
        }
        for _ in g.filters() {
            self.drop_one("filter");
        }
        let wrapper = top && matches!(g.children(), [usvg::Node::Group(_)]);
        for child in g.children() {
            match child {
                usvg::Node::Group(c) if wrapper => self.group(c, inh, true),
                usvg::Node::Group(c) => {
                    let mut inner = inh;
                    if inner.group == 0 {
                        inner.group = self.new_group(c.id());
                    }
                    self.group(c, inner, false);
                }
                usvg::Node::Path(p) => self.path(p, inh),
                usvg::Node::Image(img) => self.image(img, inh),
                usvg::Node::Text(t) => {
                    let before = self.nodes.len();
                    let mut inner = inh;
                    if inner.group == 0 {
                        inner.group = self.new_group(if t.id().is_empty() { "Text" } else { t.id() });
                    }
                    self.group(t.flattened(), inner, false);
                    if self.nodes.len() == before {
                        // No font for it on this machine.
                        self.drop_one("text");
                    }
                    self.nodes[before..].iter_mut().filter(|n| n.name == "Vector").for_each(|n| n.name = "Text".into());
                }
            }
        }
    }

    fn node(&mut self, name: &str, inh: Inherit) -> Node {
        let mut n = Node::new(self.id(), Kind::Path, 0.0, 0.0);
        n.name = if name.is_empty() { "Vector".to_owned() } else { name.to_owned() };
        (n.opacity, n.blend, n.group) = (inh.opacity.clamp(0.0, 1.0), inh.blend, inh.group);
        n.stroke_w = 0.0;
        n
    }

    /// A fill for a node whose box is already set: a colour, or a gradient laid into that box.
    fn fill(&mut self, n: &mut Node, fill: &usvg::Fill, ts: Transform) {
        let opacity = fill.opacity().get();
        let (w, h) = (n.w.max(1e-3), n.h.max(1e-3));
        match fill.paint() {
            usvg::Paint::Color(c) => n.fill = Some(rgba(*c, opacity)),
            usvg::Paint::LinearGradient(g) => {
                let st = stops(g.stops(), opacity);
                let Some(last) = st.last().cloned() else { return };
                n.fill = Some(last.color);
                // Gradient space → document. Along the gradient t = ((q - p1) · d) / |d|², with
                // q the point taken back into gradient space: a linear function of the document
                // point, so it is a straight gradient there too, whatever the transform.
                let to_doc = ts.pre_concat(g.transform());
                let (d, len2) = ((g.x2() - g.x1(), g.y2() - g.y1()), (g.x2() - g.x1()).powi(2) + (g.y2() - g.y1()).powi(2));
                let Some(back) = to_doc.invert().filter(|_| len2 > 1e-12) else { return };
                let t = |x: f32, y: f32| {
                    let q = apply(back, x, y);
                    ((q.0 - g.x1()) * d.0 + (q.1 - g.y1()) * d.1) / len2
                };
                let start = apply(to_doc, g.x1(), g.y1());
                let slope = (t(start.0 + 1.0, start.1) - t(start.0, start.1), t(start.0, start.1 + 1.0) - t(start.0, start.1));
                let s2 = slope.0 * slope.0 + slope.1 * slope.1;
                if !(s2.is_finite() && s2 > 1e-12) {
                    return;
                }
                // The line in the document, then in the box (0..1), where the lines of equal
                // colour must still be at right angles to it *in the document*.
                let (dx, dy) = (slope.0 / s2, slope.1 / s2);
                let c = (dx * dx + dy * dy) / (w * w * dx * dx + h * h * dy * dy).max(1e-12);
                let (u, v) = ((start.0 - n.x) / w, (start.1 - n.y) / h);
                n.gradient = Some(Gradient { radial: false, angle: 0.0, stops: st, line: Some([u, v, u + c * w * dx, v + c * h * dy]), radial_line: None });
            }
            usvg::Paint::RadialGradient(g) => {
                let st = stops(g.stops(), opacity);
                let Some(last) = st.last().cloned() else { return };
                n.fill = Some(last.color);
                let to_doc = ts.pre_concat(g.transform());
                let r = g.r().get();
                // The circle as the box sees it. Kanvas draws a circle in box space (an ellipse
                // along the axes of the box); anything else gets the circle of the same area.
                let (a, b) = ((to_doc.sx * r / w, to_doc.ky * r / h), (to_doc.kx * r / w, to_doc.sy * r / h));
                let radius = (a.0 * b.1 - a.1 * b.0).abs().sqrt();
                if !(radius.is_finite() && radius > 1e-6) {
                    return;
                }
                let c = apply(to_doc, g.cx(), g.cy());
                let (u, v) = ((c.0 - n.x) / w, (c.1 - n.y) / h);
                n.gradient = Some(Gradient { radial: true, angle: 0.0, stops: st, line: None, radial_line: Some([u, v, u + radius, v]) });
            }
            usvg::Paint::Pattern(_) => {
                self.drop_one("pattern fill");
                n.fill = Some(rgba(usvg::Color::new_rgb(128, 128, 128), opacity));
            }
        }
    }

    fn stroke(&mut self, n: &mut Node, stroke: &usvg::Stroke, ts: Transform) {
        let k = scale_of(ts);
        n.stroke = Some(flat_color(stroke.paint(), stroke.opacity().get()));
        n.stroke_w = stroke.width().get() * k;
        if let Some(d) = stroke.dasharray().filter(|d| !d.is_empty()) {
            n.dash = d[0] * k;
            n.gap = d.get(1).copied().unwrap_or(d[0]) * k;
        }
    }

    fn path(&mut self, p: &usvg::Path, inh: Inherit) {
        if !p.is_visible() || inh.opacity <= 0.0 {
            return;
        }
        let Some(world) = p.data().clone().transform(inh.ts) else { return };
        let subs = outline::from_skia(&world);
        if subs.is_empty() {
            return;
        }
        let mut made: Vec<Node> = Vec::new();
        let mut stroke = p.stroke();
        if let Some(fill) = p.fill() {
            // A fill closes every subpath. Under the non-zero rule, parts that overlap or lie
            // in one another count as one area; Kanvas fills even-odd, so those are resolved.
            let closed: Vec<Contour> = subs.iter().map(|c| Contour { pts: c.pts.clone(), closed: true }).collect();
            let rule = if fill.rule() == usvg::FillRule::NonZero { Rule::NonZero } else { Rule::EvenOdd };
            let area = outline::simplify(&closed, rule, 0.0);
            if !area.is_empty() {
                // The stroke runs along the same contours: one node does both.
                let both = stroke.is_some() && p.paint_order() == usvg::PaintOrder::FillAndStroke && subs.iter().all(|c| c.closed) && area == closed;
                let mut n = self.node(p.id(), inh);
                outline::set_contours(&mut n, area);
                self.fill(&mut n, fill, inh.ts);
                if both {
                    if let Some(s) = stroke.take() {
                        self.stroke(&mut n, s, inh.ts);
                    }
                }
                made.push(n);
            }
        }
        if let Some(s) = stroke {
            // A Kanvas path is one open line, or any number of closed contours.
            let (closed, open): (Vec<Contour>, Vec<Contour>) = subs.into_iter().partition(|c| c.closed);
            let mut lines: Vec<Vec<Contour>> = open.into_iter().map(|c| vec![c]).collect();
            if !closed.is_empty() {
                lines.insert(0, closed);
            }
            let under = p.paint_order() == usvg::PaintOrder::StrokeAndFill;
            for (i, line) in lines.into_iter().enumerate() {
                let mut n = self.node(p.id(), inh);
                outline::set_contours(&mut n, line);
                self.stroke(&mut n, s, inh.ts);
                if under {
                    made.insert(i, n);
                } else {
                    made.push(n);
                }
            }
        }
        if made.len() > 1 && inh.group == 0 {
            let g = self.new_group(p.id());
            made.iter_mut().for_each(|n| n.group = g);
        }
        self.nodes.extend(made);
    }

    fn image(&mut self, img: &usvg::Image, inh: Inherit) {
        if !img.is_visible() {
            return;
        }
        let bytes = match img.kind() {
            usvg::ImageKind::JPEG(b) | usvg::ImageKind::PNG(b) | usvg::ImageKind::GIF(b) | usvg::ImageKind::WEBP(b) => b.as_ref().clone(),
            usvg::ImageKind::SVG(tree) => {
                let mut inner = inh;
                if inner.group == 0 {
                    inner.group = self.new_group(img.id());
                }
                return self.group(tree.root(), inner, false);
            }
        };
        let (w, h) = (img.size().width(), img.size().height());
        let (o, x, y) = (apply(inh.ts, 0.0, 0.0), apply(inh.ts, w, 0.0), apply(inh.ts, 0.0, h));
        let (ax, ay) = ((x.0 - o.0, x.1 - o.1), (y.0 - o.0, y.1 - o.1));
        let (nw, nh) = (ax.0.hypot(ax.1), ay.0.hypot(ay.1));
        if nw < 1e-3 || nh < 1e-3 {
            return;
        }
        let c = apply(inh.ts, w * 0.5, h * 0.5);
        let mut n = Node::new(self.id(), Kind::Image, c.0 - nw * 0.5, c.1 - nh * 0.5);
        n.name = if img.id().is_empty() { "Image".to_owned() } else { img.id().to_owned() };
        (n.w, n.h, n.stroke_w) = (nw, nh, 0.0);
        (n.opacity, n.blend, n.group) = (inh.opacity.clamp(0.0, 1.0), inh.blend, inh.group);
        n.rotation = ax.1.atan2(ax.0).to_degrees();
        // Mirrored: the y axis points the other way round the x axis.
        n.flip_v = ax.0 * ay.1 - ax.1 * ay.0 < 0.0;
        self.images.push((n.id, bytes));
        self.nodes.push(n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn svg(body: &str) -> Imported {
        let text = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">{body}</svg>"#);
        import(text.as_bytes(), "test").expect("valid svg")
    }

    fn near(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.02
    }

    fn boxed(n: &Node, want: (f32, f32, f32, f32)) -> bool {
        near(n.x, want.0) && near(n.y, want.1) && near(n.w, want.2) && near(n.h, want.3)
    }

    /// The area a path node fills.
    fn area(n: &Node) -> f64 {
        outline::area(&outline::path_contours(n))
    }

    #[test]
    fn shapes_become_paths_with_their_transform_baked_in() {
        let got = svg(
            r##"<rect x="10" y="10" width="40" height="20" fill="#ff0000"/>
               <circle cx="100" cy="50" r="20" fill="#00ff00" fill-opacity="0.5"/>
               <path d="M0 0 L10 0 L10 10 Z" fill="blue" transform="translate(150 20) scale(2)"/>"##,
        );
        assert_eq!((got.root.kind, got.root.w, got.root.h, got.root.fill), (Kind::Frame, 200.0, 100.0, None));
        assert_eq!(got.root.name, "test");
        let c = &got.root.children;
        assert_eq!((got.shapes, c.len(), got.dropped_count()), (3, 3, 0));
        assert!(c.iter().all(|n| n.kind == Kind::Path && n.closed && n.stroke.is_none() && n.group == 0));
        assert!(boxed(&c[0], (10.0, 10.0, 40.0, 20.0)), "{:?}", (c[0].x, c[0].y, c[0].w, c[0].h));
        assert_eq!((c[0].fill, c[0].points.len()), (Some([255, 0, 0, 255]), 4));
        assert!(boxed(&c[1], (80.0, 30.0, 40.0, 40.0)), "{:?}", (c[1].x, c[1].y, c[1].w, c[1].h));
        assert_eq!(c[1].fill, Some([0, 255, 0, 128]));
        assert!(c[1].points.iter().all(|q| q.hin != [0.0; 2] && q.hout != [0.0; 2]), "a circle is all curves");
        assert!((area(&c[1]) - std::f64::consts::PI * 400.0).abs() < 2.0);
        assert!(boxed(&c[2], (150.0, 20.0, 20.0, 20.0)), "{:?}", (c[2].x, c[2].y, c[2].w, c[2].h));
        assert_eq!(c[2].fill, Some([0, 0, 255, 255]));
    }

    #[test]
    fn a_hole_stays_a_hole_under_both_fill_rules() {
        // Even-odd: the inner square is a hole whichever way it winds.
        let got = svg(r#"<path fill-rule="evenodd" d="M0 0H100V100H0Z M25 25H75V75H25Z"/>"#);
        let n = &got.root.children[0];
        assert_eq!((got.shapes, n.points.len(), n.holes.len()), (1, 4, 1));
        assert!((area(n) - 7500.0).abs() < 1.0);
        // Non-zero, wound the other way: a hole as well, and the curves are left alone.
        let got = svg(r#"<path d="M0 0H100V100H0Z M25 25V75H75V25Z"/>"#);
        let n = &got.root.children[0];
        assert_eq!((n.points.len(), n.holes.len()), (4, 1));
        assert!((area(n) - 7500.0).abs() < 1.0);
        // Non-zero, wound the same way: no hole in SVG, so none here.
        let got = svg(r#"<path d="M0 0H100V100H0Z M25 25H75V75H25Z"/>"#);
        let n = &got.root.children[0];
        assert_eq!((n.points.len(), n.holes.len()), (4, 0));
        assert!((area(n) - 10000.0).abs() < 1.0);
        // Non-zero, two parts that overlap: one area.
        let got = svg(r#"<path d="M0 0H60V60H0Z M40 40H100V100H40Z"/>"#);
        let n = &got.root.children[0];
        assert_eq!((got.shapes, n.holes.len()), (1, 0));
        assert!((area(n) - (3600.0 + 3600.0 - 400.0)).abs() < 1.0, "{}", area(n));
    }

    #[test]
    fn groups_become_kanvas_groups_and_pass_their_opacity_down() {
        let got = svg(
            r#"<g id="outer" opacity="0.5">
                 <rect width="10" height="10"/>
                 <g opacity="0.5" transform="translate(20 0)"><rect width="10" height="10"/><rect y="20" width="10" height="10"/></g>
               </g>
               <g id="single"><rect x="100" width="10" height="10"/></g>
               <rect x="150" width="10" height="10" opacity="0.4"/>"#,
        );
        let c = &got.root.children;
        assert_eq!(c.len(), 5);
        // Nested groups are one Kanvas group; a group of one is none.
        assert!(c[0].group != 0 && c[1].group == c[0].group && c[2].group == c[0].group);
        assert_eq!((c[3].group, c[4].group), (0, 0));
        assert_eq!(got.groups, vec![(c[0].group, "outer".to_owned())]);
        assert_eq!((c[0].opacity, c[1].opacity, c[2].opacity, c[3].opacity), (0.5, 0.25, 0.25, 1.0));
        assert!(near(c[4].opacity, 0.4));
        assert!(boxed(&c[2], (20.0, 20.0, 10.0, 10.0)));
    }

    #[test]
    fn linear_gradient_is_laid_into_the_box_of_the_shape() {
        let got = svg(
            r##"<defs>
                 <linearGradient id="a"><stop offset="0" stop-color="#ff0000"/><stop offset="1" stop-color="#0000ff" stop-opacity="0.5"/></linearGradient>
                 <linearGradient id="b" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="0" y2="100"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#000"/></linearGradient>
                 <radialGradient id="c"><stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#000"/></radialGradient>
               </defs>
               <rect x="20" y="10" width="100" height="50" fill="url(#a)"/>
               <rect x="150" y="50" width="20" height="25" fill="url(#b)"/>
               <rect x="20" y="70" width="60" height="20" fill="url(#c)"/>"##,
        );
        let c = &got.root.children;
        let g = c[0].gradient.as_ref().expect("gradient");
        assert!(!g.radial && c[0].fill.is_some());
        assert_eq!(g.stops.iter().map(|s| (s.pos, s.color)).collect::<Vec<_>>(), vec![(0.0, [255, 0, 0, 255]), (1.0, [0, 0, 255, 128])]);
        // Box units: left edge to right edge.
        let line = g.line();
        assert!(near(line[0], 0.0) && near(line[1], 0.0) && near(line[2], 1.0) && near(line[3], 0.0), "{line:?}");
        // User space: y from 0 to 100, seen from a box at y 50..75.
        let line = c[1].gradient.as_ref().expect("gradient").line();
        assert!(near(line[1], -2.0) && near(line[3], 2.0) && near(line[0], line[2]), "{line:?}");
        // Radial, box units: centre of the box, out to its edge.
        let g = c[2].gradient.as_ref().expect("gradient");
        let line = g.line();
        assert!(g.radial && near(line[0], 0.5) && near(line[1], 0.5) && near(line[2], 1.0) && near(line[3], 0.5), "{line:?}");
    }

    #[test]
    fn open_polyline_keeps_its_stroke_and_gets_no_fill() {
        let got = svg(r##"<g transform="scale(2)"><polyline points="10,10 30,10 30,30" fill="none" stroke="#123456" stroke-width="3" stroke-opacity="0.5" stroke-dasharray="4 2"/></g>"##);
        let n = &got.root.children[0];
        assert_eq!((got.shapes, n.closed, n.fill, n.points.len()), (1, false, None, 3));
        assert_eq!((n.stroke, n.stroke_w, n.dash, n.gap), (Some([0x12, 0x34, 0x56, 128]), 6.0, 8.0, 4.0));
        assert!(boxed(n, (20.0, 20.0, 40.0, 40.0)));
        // Filled and stroked, with an open part: the fill closes it, the stroke does not.
        let got = svg(r##"<path d="M10 10H50V50" fill="#ff0000" stroke="#000"/>"##);
        let c = &got.root.children;
        assert_eq!(c.len(), 2);
        assert!(c[0].closed && c[0].fill.is_some() && c[0].stroke.is_none());
        assert!(!c[1].closed && c[1].fill.is_none() && c[1].stroke.is_some());
        assert!(c[0].group != 0 && c[0].group == c[1].group);
        // Closed: one node carries both.
        let got = svg(r##"<rect x="10" y="10" width="50" height="50" fill="#ff0000" stroke="#000" stroke-width="2"/>"##);
        let n = &got.root.children[0];
        assert_eq!((got.shapes, n.fill, n.stroke, n.stroke_w), (1, Some([255, 0, 0, 255]), Some([0, 0, 0, 255]), 2.0));
    }

    #[test]
    fn quadratic_curves_become_cubic_ones() {
        let got = svg(r#"<path d="M0 0 Q50 100 100 0 Z"/>"#);
        let n = &got.root.children[0];
        assert_eq!(n.points.len(), 2);
        // Control points of the cubic: two thirds of the way to the quadratic one, in a box
        // that is 100 × 50 (the curve reaches half as far as its control point).
        assert!(boxed(n, (0.0, 0.0, 100.0, 50.0)), "{:?}", (n.x, n.y, n.w, n.h));
        let (a, b) = (n.points[0], n.points[1]);
        let out = [a.hout[0] * n.w, a.hout[1] * n.h];
        let inn = [b.hin[0] * n.w, b.hin[1] * n.h];
        assert!(near(out[0], 100.0 / 3.0) && near(out[1], 200.0 / 3.0), "{out:?}");
        assert!(near(inn[0], -100.0 / 3.0) && near(inn[1], 200.0 / 3.0), "{inn:?}");
        assert!((area(n) - 100.0 * 100.0 / 3.0).abs() < 2.0, "{}", area(n));
    }

    #[test]
    fn view_box_scales_everything() {
        let text = r##"<svg xmlns="http://www.w3.org/2000/svg" width="480" height="240" viewBox="0 0 24 12"><rect x="2" y="1" width="10" height="5" fill="none" stroke="#000" stroke-width="2"/></svg>"##;
        let got = import(text.as_bytes(), "icon").expect("valid svg");
        assert_eq!((got.root.w, got.root.h), (480.0, 240.0));
        let n = &got.root.children[0];
        assert!(boxed(n, (40.0, 20.0, 200.0, 100.0)), "{:?}", (n.x, n.y, n.w, n.h));
        assert_eq!(n.stroke_w, 40.0);
    }

    #[test]
    fn what_cannot_be_shown_is_counted() {
        let got = svg(
            r##"<defs>
                 <filter id="f"><feGaussianBlur stdDeviation="2"/></filter>
                 <mask id="m"><rect width="50" height="50" fill="#fff"/></mask>
                 <clipPath id="page"><rect width="200" height="100"/></clipPath>
                 <clipPath id="c"><circle cx="20" cy="20" r="10"/></clipPath>
               </defs>
               <g clip-path="url(#page)">
                 <rect width="40" height="40" filter="url(#f)"/>
                 <rect width="40" height="40" mask="url(#m)"/>
                 <rect width="40" height="40" clip-path="url(#c)"/>
               </g>"##,
        );
        assert_eq!(got.shapes, 3);
        let mut dropped = got.dropped.clone();
        dropped.sort();
        assert_eq!(dropped, vec![("clip path", 1), ("filter", 1), ("mask", 1)]);
        assert_eq!(got.summary(), "3 shapes (3 effects not supported: filter, mask, clip path)");
    }

    #[test]
    fn garbage_is_an_error_and_an_empty_file_has_no_shapes() {
        assert!(import(b"not an svg at all", "x").is_err());
        assert!(import(b"<svg", "x").is_err());
        assert!(import(&[0xff, 0xd8, 0xff, 0xe0, 0, 16], "x").is_err());
        assert_eq!(svg("").shapes, 0);
    }
}
