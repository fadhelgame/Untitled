//! Strokes with caps, joins and dashes on the canvas. The shape of the stroke comes from the
//! same stroker the export is rendered with (`outline::stroke_path`, i.e. `tiny_skia`), so both
//! show the same ends, corners and dashes; here it is flattened at screen resolution, filled
//! and given a soft edge one pixel wide.

use super::vtext;
use crate::outline::{stroke_path, Contour, StrokeStyle};
use eframe::egui::{self, vec2, Color32, Mesh, Pos2, Vec2};
use resvg::tiny_skia::PathSegment;

/// How far a flattened curve may be from the true one, in points.
const TOLERANCE: f32 = 0.1;

/// The closed polygons (in screen space) of the stroke along `lines` (world coordinates).
/// `to_screen` maps a world point; `zoom` is its scale.
pub fn polygons(lines: &[Contour], style: &StrokeStyle, to_screen: impl Fn(f32, f32) -> Pos2, zoom: f32) -> Vec<Vec<Pos2>> {
    let Some((path, o)) = stroke_path(lines, style, zoom.clamp(0.05, 256.0)) else { return Vec::new() };
    let at = |x: f32, y: f32| to_screen(o.0 + x, o.1 + y);
    let mut out: Vec<Vec<Pos2>> = Vec::new();
    let mut cur: Vec<Pos2> = Vec::new();
    let mut close = |cur: &mut Vec<Pos2>| {
        let mut poly = std::mem::take(cur);
        poly.dedup_by(|a, b| (a.x - b.x).abs() < 1e-4 && (a.y - b.y).abs() < 1e-4);
        while poly.len() > 1 && (poly[0] - poly[poly.len() - 1]).length_sq() < 1e-8 {
            poly.pop();
        }
        if poly.len() >= 3 && poly.iter().all(|p| p.x.is_finite() && p.y.is_finite()) {
            out.push(poly);
        }
    };
    // Steps for a curve whose control polygon deviates from a line by `dev` (second difference).
    let steps = |dev: f32, k: f32| ((dev * k / TOLERANCE).sqrt().ceil() as usize).clamp(1, 200);
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                close(&mut cur);
                cur.push(at(p.x, p.y));
            }
            PathSegment::LineTo(p) => cur.push(at(p.x, p.y)),
            PathSegment::QuadTo(c, p) => {
                let Some(a) = cur.last().copied() else { continue };
                let (c, b) = (at(c.x, c.y), at(p.x, p.y));
                let n = steps((a.to_vec2() - c.to_vec2() * 2.0 + b.to_vec2()).length(), 0.25);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    cur.push((a.to_vec2() * (u * u) + c.to_vec2() * (2.0 * u * t) + b.to_vec2() * (t * t)).to_pos2());
                }
            }
            PathSegment::CubicTo(c1, c2, p) => {
                let Some(a) = cur.last().copied() else { continue };
                let (c1, c2, b) = (at(c1.x, c1.y), at(c2.x, c2.y), at(p.x, p.y));
                let dev = (a.to_vec2() - c1.to_vec2() * 2.0 + c2.to_vec2()).length().max((c1.to_vec2() - c2.to_vec2() * 2.0 + b.to_vec2()).length());
                let n = steps(dev, 0.75);
                for i in 1..=n {
                    let t = i as f32 / n as f32;
                    let u = 1.0 - t;
                    let q = a.to_vec2() * (u * u * u) + c1.to_vec2() * (3.0 * u * u * t) + c2.to_vec2() * (3.0 * u * t * t) + b.to_vec2() * (t * t * t);
                    cur.push(q.to_pos2());
                }
            }
            PathSegment::Close => close(&mut cur),
        }
    }
    close(&mut cur);
    out
}

/// Twice the signed area of a polygon.
fn area2(c: &[Pos2]) -> f32 {
    let o = c[0];
    (0..c.len()).map(|i| (c[i] - o).x * (c[(i + 1) % c.len()] - o).y - (c[(i + 1) % c.len()] - o).x * (c[i] - o).y).sum()
}

/// The mesh of polygons filled under the non-zero rule, with an edge that fades out over
/// `feather` outside of them. Outer edges of a stroke all wind one way and the edges of its
/// holes the other, so one sign says which side of every edge is outside. Where a stroke
/// overlaps itself (the inside of a corner, a path that crosses itself) pieces of those edges
/// lie inside it; their soft edge is hidden by an opaque colour, but would show as darker lines
/// in a see-through one, so for those the polygons are united first.
pub fn fill(polys: &[Vec<Pos2>], color: Color32, feather: f32) -> Option<Mesh> {
    fill_edge(polys, color, feather, 0.0)
}

/// `fill` with the soft edge moved inward by `inner` of its width (0.5: centred on the edge of
/// the polygons, for shapes that are not made narrower beforehand to make up for it; only an
/// opaque colour hides the part that then lies on the shape).
pub fn fill_edge(polys: &[Vec<Pos2>], color: Color32, feather: f32, inner: f32) -> Option<Mesh> {
    let inner = if color.a() == 255 { inner } else { 0.0 };
    let united: Vec<(Vec<Pos2>, f32)>;
    let clean: Vec<Vec<Pos2>>;
    let (polys, sides): (&[Vec<Pos2>], Vec<f32>) = if color.a() < 255 {
        let exact: Vec<Vec<(f64, f64)>> = polys.iter().map(|c| c.iter().map(|p| (p.x as f64, p.y as f64)).collect()).collect();
        united = crate::outline::unite(&exact)
            .into_iter()
            .flat_map(|shape| shape.into_iter().enumerate())
            .map(|(i, c)| (c.into_iter().map(|p| Pos2::new(p.0 as f32, p.1 as f32)).collect::<Vec<Pos2>>(), if i == 0 { 1.0 } else { -1.0 }))
            .filter(|(c, _)| c.len() >= 3)
            // The first contour of a shape is its outer edge, the others are holes: outside is
            // away from what the first encloses and into what the others enclose.
            .map(|(c, outer)| {
                let side = if area2(&c) >= 0.0 { outer } else { -outer };
                (c, side)
            })
            .collect();
        clean = united.iter().map(|(c, _)| c.clone()).collect();
        (&clean, united.iter().map(|(_, side)| *side).collect())
    } else {
        let side = if polys.iter().map(|c| area2(c)).sum::<f32>() >= 0.0 { 1.0 } else { -1.0 };
        (polys, vec![side; polys.len()])
    };
    let (verts, indices) = vtext::fill_contours(polys, vtext::FillRule::NonZero)?;
    let mut mesh = Mesh::default();
    let vertex = |pos: Pos2, color: Color32| egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color };
    mesh.vertices.extend(verts.into_iter().map(|p| vertex(p, color)));
    mesh.indices = indices;
    for (c, side) in polys.iter().zip(sides) {
        let normal = |a: Pos2, b: Pos2| vec2(b.y - a.y, a.x - b.x).normalized() * side;
        let n = c.len();
        let base = mesh.vertices.len() as u32;
        for i in 0..n {
            let (n1, n2) = (normal(c[(i + n - 1) % n], c[i]), normal(c[i], c[(i + 1) % n]));
            let m = n1 + n2;
            // A mitre of length 1 / cos(half the angle), kept short at sharp corners.
            let len2 = m.length_sq();
            let m: Vec2 = if len2 < 0.16 { m.normalized() * 2.5 } else { m * (2.0 / len2) };
            let m = if m.is_finite() { m } else { n2 };
            mesh.vertices.push(vertex(c[i] - m * (feather * inner), color));
            mesh.vertices.push(vertex(c[i] + m * (feather * (1.0 - inner)), Color32::TRANSPARENT));
        }
        let n = n as u32;
        for i in 0..n {
            let (a, b) = (base + 2 * i, base + 2 * ((i + 1) % n));
            mesh.indices.extend([a, a + 1, b, b, a + 1, b + 1]);
        }
    }
    Some(mesh)
}

/// The stroke along `lines` (world coordinates) as a mesh on the screen. The soft edge is
/// centred on the true edge of the stroke: the filled part is narrower by one `feather`, and a
/// stroke thinner than that is drawn fainter instead.
pub fn mesh(lines: &[Contour], style: &StrokeStyle, to_screen: impl Fn(f32, f32) -> Pos2, zoom: f32, color: Color32, feather: f32) -> Option<Mesh> {
    let _span = super::perf::span(4);
    let wide = style.width * zoom;
    let (core, color) = if wide > feather * 1.25 { (wide - feather, color) } else { (feather * 0.25, color.gamma_multiply((wide / (feather * 1.25)).clamp(0.0, 1.0))) };
    let style = StrokeStyle { width: core / zoom, ..*style };
    let polys = polygons(lines, &style, to_screen, zoom);
    fill(&polys, color, feather)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::render::PenPt;
    use crate::outline::{Cap, Join};
    use eframe::egui::pos2;

    fn line(pts: &[(f32, f32)], closed: bool) -> Contour {
        Contour { pts: pts.iter().map(|p| PenPt { x: p.0, y: p.1, hin: [0.0; 2], hout: [0.0; 2], r: None }).collect(), closed }
    }

    fn bounds(polys: &[Vec<Pos2>]) -> (f32, f32, f32, f32) {
        polys.iter().flatten().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, p| (b.0.min(p.x), b.1.min(p.y), b.2.max(p.x), b.3.max(p.y)))
    }

    #[test]
    fn caps_reach_past_the_ends_and_the_view_is_applied() {
        let l = [line(&[(10.0, 20.0), (110.0, 20.0)], false)];
        let style = |cap| StrokeStyle { width: 8.0, cap, join: Join::Miter, miter_limit: 4.0, dash: None };
        // Twice the size, moved by (5, 7).
        let view = |x: f32, y: f32| pos2(x * 2.0 + 5.0, y * 2.0 + 7.0);
        let near = |a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)| (a.0 - b.0).abs() + (a.1 - b.1).abs() + (a.2 - b.2).abs() + (a.3 - b.3).abs() < 0.2;
        let butt = polygons(&l, &style(Cap::Butt), view, 2.0);
        assert_eq!(butt.len(), 1);
        assert!(near(bounds(&butt), (25.0, 39.0, 225.0, 55.0)), "{:?}", bounds(&butt));
        for cap in [Cap::Round, Cap::Square] {
            let got = polygons(&l, &style(cap), view, 2.0);
            assert!(near(bounds(&got), (17.0, 39.0, 233.0, 55.0)), "{cap:?}: {:?}", bounds(&got));
        }
        // A round end is an arc of many points, a square one adds none.
        assert!(polygons(&l, &style(Cap::Round), view, 2.0)[0].len() > 12);
        assert!(polygons(&l, &style(Cap::Square), view, 2.0)[0].len() <= 6);
    }

    #[test]
    fn dashes_are_pieces_and_a_closed_line_gets_a_hole() {
        let l = [line(&[(0.0, 0.0), (100.0, 0.0)], false)];
        let dashed = StrokeStyle { width: 4.0, cap: Cap::Butt, join: Join::Miter, miter_limit: 4.0, dash: Some((10.0, 10.0)) };
        assert_eq!(polygons(&l, &dashed, pos2, 1.0).len(), 5);
        let square = [line(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)], true)];
        let solid = StrokeStyle { dash: None, ..dashed };
        let got = polygons(&square, &solid, pos2, 1.0);
        assert_eq!(got.len(), 2);
        // Outer edge and hole wind opposite ways, which is what the soft edge relies on.
        assert!(area2(&got[0]) * area2(&got[1]) < 0.0);
        let m = fill(&got, Color32::BLACK, 1.0).unwrap();
        assert!(m.indices.len() % 3 == 0 && !m.indices.is_empty());
        // The soft edge lies outside the band: no vertex strays more than a feather from it.
        let b = bounds(&[m.vertices.iter().map(|v| v.pos).collect()]);
        assert!(b.0 >= -3.01 && b.2 <= 103.01, "{b:?}");
        // A see-through stroke is united first: its soft edge lies outside the band everywhere
        // (the inside of each corner would have pieces of edge within the band otherwise).
        let m = fill(&got, Color32::from_rgba_unmultiplied(0, 0, 0, 128), 1.0).unwrap();
        let soft: Vec<Pos2> = m.vertices.iter().filter(|v| v.color == Color32::TRANSPARENT).map(|v| v.pos).collect();
        assert_eq!(soft.len(), 8);
        let outside = |p: &Pos2| (p.x - 50.0).abs().max((p.y - 50.0).abs());
        assert!(soft.iter().all(|p| (outside(p) - 53.0).abs() < 0.01 || (outside(p) - 47.0).abs() < 0.01), "{soft:?}");
    }
}

// ---------------------------------------------------------------- bench

impl super::App {
    /// `--bench` with `KANVAS_BENCH_CAPS`: a frame with one column per cap × join. Down the
    /// rows: an open path with a sharp corner, the same dashed, a closed path, a dashed star,
    /// two lines (solid and dashed), a square with its stroke inside / centred / outside, and
    /// a dashed rounded rectangle and ellipse (some turned and flipped). Right of it, the
    /// export of that frame placed as an image, to compare. `=view:x0,y0,x1,y1` shows that
    /// part of the stage (the image starts at x 960).
    pub(super) fn bench_caps(&mut self, ctx: &egui::Context, mode: &str, out: &std::path::Path, inject: &mut Vec<(u32, egui::Event)>) {
        use crate::doc::{Kind, Node};
        use crate::outline::set_contours;
        let (x, y) = (-3000.0, 20000.0);
        let (w, h) = (920.0, 660.0);
        let stage = self.doc.alloc_id();
        let mut n = Node::new(stage, Kind::Frame, x, y);
        (n.name, n.w, n.h, n.fill) = ("Caps demo".into(), w, h, Some([250, 250, 248, 255]));
        self.doc.insert(n, None);
        let (blue, ink, pale, red) = ([37, 99, 235, 255], [15, 23, 42, 255], [219, 234, 254, 255], [220, 38, 38, 150]);
        let pt = |px: f32, py: f32| super::render::PenPt { x: px, y: py, hin: [0.0; 2], hout: [0.0; 2], r: None };
        for col in 0..9u8 {
            let (cap, join) = (col / 3, col % 3);
            let cx = x + 14.0 + col as f32 * 100.0;
            let add = |app: &mut Self, kind: Kind, row: f32, edit: &dyn Fn(&mut Node, f32, f32)| {
                let (nx, ny) = (cx, y + 14.0 + row * 92.0);
                let mut n = Node::new(app.doc.alloc_id(), kind, nx, ny);
                (n.w, n.h, n.stroke, n.stroke_w, n.cap, n.join) = (80.0, 70.0, Some(blue), 10.0, cap, join);
                edit(&mut n, nx, ny);
                app.doc.insert(n, Some(stage));
            };
            // Corners of about 100°, 37° and 14°: the last is past the miter limit.
            let zigzag = |nx: f32, ny: f32| vec![pt(nx + 6.0, ny + 64.0), pt(nx + 22.0, ny + 12.0), pt(nx + 40.0, ny + 62.0), pt(nx + 52.0, ny + 8.0), pt(nx + 76.0, ny + 60.0)];
            add(self, Kind::Path, 0.0, &|n, nx, ny| set_contours(n, vec![Contour { pts: zigzag(nx, ny), closed: false }]));
            add(self, Kind::Path, 1.0, &|n, nx, ny| {
                set_contours(n, vec![Contour { pts: zigzag(nx, ny), closed: false }]);
                (n.dash, n.gap, n.stroke_w) = (16.0, 11.0, 8.0);
            });
            add(self, Kind::Path, 2.0, &|n, nx, ny| {
                set_contours(n, vec![Contour { pts: vec![pt(nx + 8.0, ny + 62.0), pt(nx + 30.0, ny + 8.0), pt(nx + 74.0, ny + 62.0), pt(nx + 40.0, ny + 44.0)], closed: true }]);
                (n.fill, n.stroke_w, n.stroke) = (Some(pale), 7.0, Some(ink));
            });
            add(self, Kind::Path, 3.0, &|n, nx, ny| {
                let star: Vec<super::render::PenPt> = (0..10)
                    .map(|i| {
                        let (a, r) = (std::f32::consts::TAU * i as f32 / 10.0 - std::f32::consts::FRAC_PI_2, if i % 2 == 0 { 36.0 } else { 15.0 });
                        pt(nx + 40.0 + r * a.cos(), ny + 36.0 + r * a.sin())
                    })
                    .collect();
                set_contours(n, vec![Contour { pts: star, closed: true }]);
                (n.dash, n.gap, n.stroke_w, n.stroke) = (9.0, 7.0, 5.0, Some(red));
            });
            add(self, Kind::Line, 4.0, &|n, nx, ny| (n.x, n.y, n.w, n.h) = (nx + 8.0, ny + 10.0, 64.0, 18.0));
            add(self, Kind::Line, 4.0, &|n, nx, ny| (n.x, n.y, n.w, n.h, n.dash, n.gap, n.stroke_w) = (nx + 8.0, ny + 44.0, 64.0, 18.0, 12.0, 9.0, 8.0));
            add(self, Kind::Rect, 5.0, &|n, nx, ny| (n.x, n.y, n.w, n.h, n.fill, n.stroke_align) = (nx + 14.0, ny + 12.0, 52.0, 46.0, Some(pale), Some(cap)));
            add(self, Kind::Rect, 6.0, &|n, nx, ny| {
                (n.x, n.y, n.w, n.h, n.radius, n.stroke_w, n.dash, n.gap) = (nx + 4.0, ny + 4.0, 40.0, 62.0, 12.0, 5.0, 10.0, 8.0);
                (n.rotation, n.flip_h, n.stroke_align) = (if join == 1 { 25.0 } else { 0.0 }, join == 2, Some(join));
            });
            add(self, Kind::Ellipse, 6.0, &|n, nx, ny| (n.x, n.y, n.w, n.h, n.stroke_w, n.dash, n.gap, n.stroke) = (nx + 50.0, ny + 6.0, 32.0, 58.0, 5.0, 8.0, 8.0, Some(ink)));
        }
        // The export of the frame, at twice the size, as an image beside it.
        let start = std::time::Instant::now();
        if let Some((svg, sw, sh)) = crate::export::to_svg(&self.doc, Some(stage), &mut self.fonts, ctx) {
            let _ = crate::export::save_raster(&svg, sw, sh, 2.0, false, &out.with_file_name("bench-caps-export.png"));
            let _ = std::fs::write(out.with_file_name("bench-caps-export.svg"), &svg);
            if let Ok((png, _, _)) = crate::export::render_png(&svg, sw, sh, 4.0) {
                let asset = self.doc.add_asset(png);
                let mut n = Node::new(self.doc.alloc_id(), Kind::Image, x + w + 40.0, y);
                (n.name, n.w, n.h, n.image, n.stroke_w) = ("Caps export".into(), w, h, Some(asset), 0.0);
                self.doc.insert(n, None);
            }
        }
        eprintln!("CAPS exported in {:.1} ms", start.elapsed().as_secs_f32() * 1000.0);
        let part: Vec<f32> = mode.strip_prefix("view:").map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect()).unwrap_or_default();
        let mut view = if part.len() == 4 { (x + part[0], y + part[1], x + part[2], y + part[3]) } else { (x - 20.0, y - 20.0, x + w * 2.0 + 60.0, y + h + 20.0) };
        if mode == "dash" {
            // `=dash`: long top-level paths (no frame around them, so nothing keeps their
            // meshes but the strokes' own cache): a dashed wave of 1200 points, a see-through
            // one and a solid one, seen close up; most of each is off screen.
            let (wx, wy) = (x, y + h + 400.0);
            for (row, (dash, color)) in [(14.0, blue), (9.0, red), (0.0, ink)].into_iter().enumerate() {
                let pts: Vec<super::render::PenPt> = (0..1200).map(|i| pt(wx + i as f32 * 5.0, wy + row as f32 * 60.0 + if i % 2 == 0 { 0.0 } else { 34.0 } + (i as f32 * 0.05).sin() * 20.0)).collect();
                let mut n = Node::new(self.doc.alloc_id(), Kind::Path, wx, wy);
                set_contours(&mut n, vec![Contour { pts, closed: false }]);
                (n.name, n.stroke, n.stroke_w, n.dash, n.gap, n.cap, n.join) = ("Wave".into(), Some(color), 6.0, dash, dash * 0.6, 1, 1);
                self.doc.insert(n, None);
            }
            view = (wx + 2900.0, wy - 40.0, wx + 3100.0, wy + 200.0);
        }
        self.zoom_to(view, 32.0);
        self.fit_pending = false;
        self.sel.clear();
        if mode == "panel" {
            self.sel = self.doc.find(stage).and_then(|f| f.children.get(1)).map(|c| c.id).into_iter().collect();
            let p = Pos2::new(self.canvas_rect.right() + 170.0, self.canvas_rect.center().y);
            let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -640.0), modifiers: Default::default() };
            inject.extend([(215, egui::Event::PointerMoved(p)), (218, wheel), (224, egui::Event::PointerMoved(self.canvas_rect.center_top() + vec2(0.0, 40.0)))]);
        }
    }
}
