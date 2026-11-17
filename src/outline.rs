//! Outline stroke: the stroke of a shape as a filled path of its own, plus the geometry both
//! this and the SVG import share: contours with bezier handles to and from `tiny_skia` paths,
//! and turning overlapping contours (a raw stroke outline, a `fill-rule: nonzero` path) into
//! plain outer + hole contours that fill the same under the even-odd rule Kanvas paths use.
//!
//! The outline follows what the canvas and the export draw (`stroke_lines` is what all three
//! start from): the caps and joins of the node, mitred corners cut off to a bevel past the miter
//! limit, the stroke of a rectangle or ellipse moved inside / outside by shrinking or growing the
//! shape it runs along, and dashes that start at the first point.

use crate::app::builder::fit_closed_tol;
use crate::app::pen::{set_geo, Geo};
use crate::app::render::PenPt;
use crate::doc::{rotate, Doc, Kind, Node};
use crate::layout::Constraint;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::Overlay;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use resvg::tiny_skia as sk;

type P2 = (f64, f64);
type Pt = IntPoint<i32>;

/// One contour in world coordinates; the handles are relative to their point.
#[derive(Clone, PartialEq)]
pub struct Contour {
    pub pts: Vec<PenPt>,
    pub closed: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Join {
    /// A pointed corner; a bevel where it would reach further than the miter limit.
    Miter,
    Round,
    Bevel,
}

/// How far a mitred corner may reach, in stroke widths from its inner to its outer tip (the
/// default of SVG, which the export states too): corners sharper than about 29° are bevelled.
pub const MITER_LIMIT: f32 = 4.0;

impl Cap {
    /// From `Node::cap`.
    pub fn of(v: u8) -> Cap {
        match v {
            1 => Cap::Round,
            2 => Cap::Square,
            _ => Cap::Butt,
        }
    }
}

impl Join {
    /// From `Node::join`.
    pub fn of(v: u8) -> Join {
        match v {
            1 => Join::Round,
            2 => Join::Bevel,
            _ => Join::Miter,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct StrokeStyle {
    pub width: f32,
    pub cap: Cap,
    pub join: Join,
    pub miter_limit: f32,
    /// Dash and gap length; `None` = a solid line.
    pub dash: Option<(f32, f32)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Rule {
    NonZero,
    EvenOdd,
}

fn corner(x: f32, y: f32) -> PenPt {
    PenPt { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None }
}

// ---------------------------------------------------------------- tiny_skia paths

/// The subpaths of a `tiny_skia` path; quadratic segments become cubic ones.
pub fn from_skia(path: &sk::Path) -> Vec<Contour> {
    let mut out: Vec<Contour> = Vec::new();
    let mut cur: Vec<PenPt> = Vec::new();
    let mut flush = |cur: &mut Vec<PenPt>, closed: bool| {
        let mut pts = std::mem::take(cur);
        if closed && pts.len() > 1 {
            // A contour that ends on its first point: the two are one anchor.
            let (first, last) = (pts[0], pts[pts.len() - 1]);
            if (first.x - last.x).abs() < 1e-4 && (first.y - last.y).abs() < 1e-4 {
                pts[0].hin = last.hin;
                pts.pop();
            }
        }
        if pts.len() >= 2 {
            out.push(Contour { pts, closed });
        }
    };
    for seg in path.segments() {
        match seg {
            sk::PathSegment::MoveTo(p) => {
                flush(&mut cur, false);
                cur.push(corner(p.x, p.y));
            }
            sk::PathSegment::LineTo(p) => cur.push(corner(p.x, p.y)),
            sk::PathSegment::QuadTo(c, p) => {
                if let Some(last) = cur.last_mut() {
                    last.hout = [(c.x - last.x) * 2.0 / 3.0, (c.y - last.y) * 2.0 / 3.0];
                }
                let mut q = corner(p.x, p.y);
                q.hin = [(c.x - p.x) * 2.0 / 3.0, (c.y - p.y) * 2.0 / 3.0];
                cur.push(q);
            }
            sk::PathSegment::CubicTo(c1, c2, p) => {
                if let Some(last) = cur.last_mut() {
                    last.hout = [c1.x - last.x, c1.y - last.y];
                }
                let mut q = corner(p.x, p.y);
                q.hin = [c2.x - p.x, c2.y - p.y];
                cur.push(q);
            }
            sk::PathSegment::Close => flush(&mut cur, true),
        }
    }
    flush(&mut cur, false);
    out
}

/// The contours as one `tiny_skia` path, moved by (-ox, -oy).
fn to_skia(contours: &[Contour], (ox, oy): (f32, f32)) -> Option<sk::Path> {
    let mut pb = sk::PathBuilder::new();
    for c in contours {
        let n = c.pts.len();
        if n < 2 {
            continue;
        }
        pb.move_to(c.pts[0].x - ox, c.pts[0].y - oy);
        let count = if c.closed { n } else { n - 1 };
        for i in 0..count {
            let (a, b) = (c.pts[i], c.pts[(i + 1) % n]);
            let (bx, by) = (b.x - ox, b.y - oy);
            if a.hout == [0.0; 2] && b.hin == [0.0; 2] {
                // The closing line is drawn by `close`.
                if !(c.closed && i == n - 1) {
                    pb.line_to(bx, by);
                }
            } else {
                pb.cubic_to(a.x - ox + a.hout[0], a.y - oy + a.hout[1], bx + b.hin[0], by + b.hin[1], bx, by);
            }
        }
        if c.closed {
            pb.close();
        }
    }
    pb.finish()
}

// ---------------------------------------------------------------- polygons

/// The contour as a closed polygon (the first point is not repeated), scaled by `k`. Curves
/// stay within `tol` (after scaling) and turn by small steps, which the curve fitting needs
/// to tell a curve from a corner.
fn flatten(c: &Contour, k: f64, tol: f64) -> Vec<P2> {
    let n = c.pts.len();
    let mut out: Vec<P2> = Vec::with_capacity(n * 4);
    if n == 0 {
        return out;
    }
    let at = |x: f32, y: f32| (x as f64 * k, y as f64 * k);
    out.push(at(c.pts[0].x, c.pts[0].y));
    for i in 0..n {
        let (a, b) = (c.pts[i], c.pts[(i + 1) % n]);
        let last = i == n - 1;
        let (p0, p3) = (at(a.x, a.y), at(b.x, b.y));
        if a.hout == [0.0; 2] && b.hin == [0.0; 2] {
            if !last {
                out.push(p3);
            }
            continue;
        }
        let (c1, c2) = (at(a.x + a.hout[0], a.y + a.hout[1]), at(b.x + b.hin[0], b.y + b.hin[1]));
        let dev = |p: P2, q: P2, r: P2| (p.0 - 2.0 * q.0 + r.0).hypot(p.1 - 2.0 * q.1 + r.1);
        let by_tol = (0.75 * dev(p0, c1, c2).max(dev(c1, c2, p3)) / tol).sqrt();
        // How far the curve turns, read off its control polygon.
        let legs = [(c1.0 - p0.0, c1.1 - p0.1), (c2.0 - c1.0, c2.1 - c1.1), (p3.0 - c2.0, p3.1 - c2.1)];
        let legs: Vec<P2> = legs.into_iter().filter(|d| d.0.hypot(d.1) > 1e-9).collect();
        let turn: f64 = legs
            .windows(2)
            .map(|w| {
                let (cross, dot) = (w[0].0 * w[1].1 - w[0].1 * w[1].0, w[0].0 * w[1].0 + w[0].1 * w[1].1);
                cross.atan2(dot).abs()
            })
            .sum();
        let steps = (by_tol.max(turn / 0.14).ceil() as usize).clamp(2, 400);
        for s in 1..=steps {
            if last && s == steps {
                break;
            }
            let t = s as f64 / steps as f64;
            let u = 1.0 - t;
            let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            out.push((k0 * p0.0 + k1 * c1.0 + k2 * c2.0 + k3 * p3.0, k0 * p0.1 + k1 * c1.1 + k2 * c2.1 + k3 * p3.1));
        }
    }
    out
}

/// Polygons on the integer grid the boolean operations work on.
struct Grid {
    ox: f64,
    oy: f64,
    s: f64,
}

impl Grid {
    fn around(polys: &[Vec<P2>]) -> Option<Grid> {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in polys.iter().flatten() {
            (x0, y0, x1, y1) = (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1));
        }
        let side = (x1 - x0).max(y1 - y0);
        if !(side.is_finite() && side > 0.0) {
            return None;
        }
        Some(Grid { ox: x0, oy: y0, s: (1u32 << 28) as f64 / side })
    }

    fn snap(&self, polys: &[Vec<P2>]) -> Vec<Vec<Pt>> {
        polys
            .iter()
            .filter(|p| p.len() >= 3)
            .map(|p| p.iter().map(|q| IntPoint::new(((q.0 - self.ox) * self.s).round() as i32, ((q.1 - self.oy) * self.s).round() as i32)).collect())
            .collect()
    }

    fn back(&self, p: Pt) -> P2 {
        (p.x as f64 / self.s + self.ox, p.y as f64 / self.s + self.oy)
    }
}

/// What the contours fill under `rule`, as shapes (outer contour, then its holes) that do not
/// overlap.
fn filled(contours: &[Vec<Pt>], rule: Rule) -> Vec<Vec<Vec<Pt>>> {
    if contours.is_empty() {
        return Vec::new();
    }
    let rule = if rule == Rule::NonZero { FillRule::NonZero } else { FillRule::EvenOdd };
    Overlay::<i32>::from_subj(contours).overlay(OverlayRule::Subject, rule)
}

/// What closed polygons fill under the non-zero rule, as shapes that do not overlap: for each
/// its outer edge, then its holes.
pub fn unite(polys: &[Vec<P2>]) -> Vec<Vec<Vec<P2>>> {
    let Some(grid) = Grid::around(polys) else { return Vec::new() };
    let shapes = filled(&grid.snap(polys), Rule::NonZero);
    shapes.iter().map(|shape| shape.iter().map(|c| c.iter().map(|p| grid.back(*p)).collect()).collect()).collect()
}

fn grid_area(shapes: &[Vec<Vec<Pt>>]) -> f64 {
    shapes
        .iter()
        .map(|shape| {
            let mut sum = 0.0;
            for c in shape {
                let m = c.len();
                for i in 0..m {
                    let (a, b) = (c[i], c[(i + 1) % m]);
                    sum += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
                }
            }
            (sum * 0.5).abs()
        })
        .sum()
}

/// How often the polygons wind around a point.
fn winding(polys: &[Vec<P2>], (x, y): P2) -> i32 {
    let mut w = 0;
    for p in polys {
        let m = p.len();
        for i in 0..m {
            let (a, b) = (p[i], p[(i + 1) % m]);
            if (a.1 <= y) != (b.1 <= y) {
                let cross = (b.0 - a.0) * (y - a.1) - (x - a.0) * (b.1 - a.1);
                if a.1 <= y && cross > 0.0 {
                    w += 1;
                } else if a.1 > y && cross < 0.0 {
                    w -= 1;
                }
            }
        }
    }
    w
}

/// Contours that fill, under the even-odd rule, what `contours` (each taken as closed) fill
/// under `rule`. Where the two rules already agree the contours come back as they are. Where
/// they only differ by contours that add nothing (a part inside another part that winds the
/// same way), those are dropped and the rest keeps its exact curves. Anything else, such as
/// contours that cross each other, is united and fitted with curves again; `detail` is the
/// size of what must stay crisp then (a stroke width; 0 = judge by the size of the whole).
pub fn simplify(contours: &[Contour], rule: Rule, detail: f32) -> Vec<Contour> {
    let closed: Vec<Contour> = contours.iter().filter(|c| c.pts.len() >= 2).map(|c| Contour { pts: c.pts.clone(), closed: true }).collect();
    if rule == Rule::EvenOdd || closed.is_empty() {
        return closed;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in closed.iter().flat_map(|c| c.pts.iter()) {
        (x0, y0, x1, y1) = (x0.min(q.x), y0.min(q.y), x1.max(q.x), y1.max(q.y));
    }
    let diag = ((x1 - x0) as f64).hypot((y1 - y0) as f64);
    if !(diag.is_finite() && diag > 1e-6) {
        return closed;
    }
    // The fitting works with fixed small lengths, so it is done at a scale where they are fine
    // compared with the shape (or with the stroke that is being outlined).
    let k = if detail > 0.0 { (100.0 / detail as f64).min(20000.0 / diag) } else { 2000.0 / diag };
    let polys: Vec<Vec<P2>> = closed.iter().map(|c| flatten(c, k, 0.05)).collect();
    let Some(grid) = Grid::around(&polys) else { return closed };
    let snapped = grid.snap(&polys);
    let wanted = filled(&snapped, rule);
    let area = grid_area(&wanted);
    let same = |other: f64| (other - area).abs() <= area * 1e-7 + 64.0;
    if same(grid_area(&filled(&snapped, Rule::EvenOdd))) {
        return closed;
    }
    // A contour bounds the filled area when the winding is zero on one side of it only.
    let keep: Vec<usize> = (0..polys.len())
        .filter(|&i| {
            let p = &polys[i];
            let m = p.len();
            if m < 3 {
                return false;
            }
            let Some(e) = (0..m).max_by(|&a, &b| {
                let len = |j: usize| (p[(j + 1) % m].0 - p[j].0).hypot(p[(j + 1) % m].1 - p[j].1);
                len(a).total_cmp(&len(b))
            }) else {
                return false;
            };
            let (a, b) = (p[e], p[(e + 1) % m]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = dx.hypot(dy);
            if len < 1e-9 {
                return false;
            }
            let (mx, my, d) = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5, 0.01);
            let (nx, ny) = (-dy / len * d, dx / len * d);
            (winding(&polys, (mx + nx, my + ny)) == 0) != (winding(&polys, (mx - nx, my - ny)) == 0)
        })
        .collect();
    if !keep.is_empty() && keep.len() < polys.len() {
        let subset: Vec<Vec<P2>> = keep.iter().map(|&i| polys[i].clone()).collect();
        if same(grid_area(&filled(&grid.snap(&subset), Rule::EvenOdd))) {
            return keep.into_iter().map(|i| closed[i].clone()).collect();
        }
    }
    let tol = if detail > 0.0 { Some(0.3) } else { None };
    let mut out = Vec::new();
    for contour in wanted.iter().flatten() {
        let poly: Vec<P2> = contour.iter().map(|p| grid.back(*p)).collect();
        let mut pts = fit_closed_tol(&poly, tol);
        if pts.len() < 3 {
            continue;
        }
        let inv = (1.0 / k) as f32;
        for q in &mut pts {
            (q.x, q.y) = ((q.x as f64 / k) as f32, (q.y as f64 / k) as f32);
            (q.hin, q.hout) = (q.hin.map(|v| v * inv), q.hout.map(|v| v * inv));
        }
        out.push(Contour { pts, closed: true });
    }
    out
}

/// The area the contours fill under the even-odd rule (curves flattened finely).
#[cfg(test)]
pub fn area(contours: &[Contour]) -> f64 {
    let polys: Vec<Vec<P2>> = contours.iter().map(|c| flatten(c, 1.0, 0.001)).collect();
    let Some(grid) = Grid::around(&polys) else { return 0.0 };
    grid_area(&filled(&grid.snap(&polys), Rule::EvenOdd)) / (grid.s * grid.s)
}

// ---------------------------------------------------------------- stroke → outline

/// The outline of a stroke along `contours`: closed contours that fill its shape under the
/// even-odd rule (outer edges and holes).
pub fn stroke_outline(contours: &[Contour], style: &StrokeStyle) -> Vec<Contour> {
    let res = (64.0 / style.width.max(1e-3)).clamp(4.0, 256.0);
    let Some((stroked, o)) = stroke_path(contours, style, res) else { return Vec::new() };
    let mut out = simplify(&from_skia(&stroked), Rule::NonZero, style.width);
    for q in out.iter_mut().flat_map(|c| c.pts.iter_mut()) {
        q.x += o.0;
        q.y += o.1;
    }
    out
}

/// The shape of a stroke along `contours` as `tiny_skia` makes it (the stroker the export is
/// rendered with): a path to fill under the non-zero rule, and the offset it has to be moved
/// by. `res`: how many times finer than one unit its curves have to be.
pub fn stroke_path(contours: &[Contour], style: &StrokeStyle, res: f32) -> Option<(sk::Path, (f32, f32))> {
    if !(style.width > 0.0) {
        return None;
    }
    let first = contours.iter().flat_map(|c| c.pts.first()).next()?;
    // Near the origin, where single precision is at its finest.
    let o = (first.x.round(), first.y.round());
    let mut path = to_skia(contours, o)?;
    if let Some((dash, gap)) = style.dash.filter(|d| d.0 > 0.0 && d.1 > 0.0) {
        path = sk::StrokeDash::new(vec![dash, gap], 0.0).and_then(|d| path.dash(&d, res))?;
    }
    let stroke = sk::Stroke {
        width: style.width,
        miter_limit: style.miter_limit,
        line_cap: match style.cap {
            Cap::Butt => sk::LineCap::Butt,
            Cap::Round => sk::LineCap::Round,
            Cap::Square => sk::LineCap::Square,
        },
        line_join: match style.join {
            Join::Miter => sk::LineJoin::Miter,
            Join::Round => sk::LineJoin::Round,
            Join::Bevel => sk::LineJoin::Bevel,
        },
        dash: None,
    };
    Some((path.stroke(&stroke, res)?, o))
}

/// A rectangle with rounded corners, clockwise. It starts where the canvas starts its dashes.
pub fn rounded_rect((x0, y0, x1, y1): (f32, f32, f32, f32), r: f32) -> Contour {
    let r = r.min((x1 - x0) * 0.5).min((y1 - y0) * 0.5).max(0.0);
    if r < 0.01 {
        return Contour { pts: vec![corner(x0, y0), corner(x1, y0), corner(x1, y1), corner(x0, y1)], closed: true };
    }
    let k = r * 0.552_284_8;
    let pt = |x: f32, y: f32, hin: [f32; 2], hout: [f32; 2]| PenPt { x, y, hin, hout, r: None };
    let z = [0.0; 2];
    let pts = vec![
        pt(x1 - r, y0, z, [k, 0.0]),
        pt(x1, y0 + r, [0.0, -k], z),
        pt(x1, y1 - r, z, [0.0, k]),
        pt(x1 - r, y1, [k, 0.0], z),
        pt(x0 + r, y1, z, [-k, 0.0]),
        pt(x0, y1 - r, [0.0, k], z),
        pt(x0, y0 + r, z, [0.0, -k]),
        pt(x0 + r, y0, [-k, 0.0], z),
    ];
    Contour { pts, closed: true }
}

/// An ellipse as four curves, clockwise from its rightmost point (like the canvas).
pub fn ellipse((cx, cy): (f32, f32), (rx, ry): (f32, f32)) -> Contour {
    let (kx, ky) = (rx * 0.552_284_8, ry * 0.552_284_8);
    let pt = |x: f32, y: f32, hin: [f32; 2], hout: [f32; 2]| PenPt { x, y, hin, hout, r: None };
    let pts = vec![
        pt(cx + rx, cy, [0.0, -ky], [0.0, ky]),
        pt(cx, cy + ry, [kx, 0.0], [-kx, 0.0]),
        pt(cx - rx, cy, [0.0, ky], [0.0, -ky]),
        pt(cx, cy - ry, [-kx, 0.0], [kx, 0.0]),
    ];
    Contour { pts, closed: true }
}

/// The contours of a path node in world coordinates, corner radii applied; rotation is not.
pub fn path_contours(n: &Node) -> Vec<Contour> {
    let world = |points: &[crate::doc::PathPoint], closed: bool| Contour {
        pts: n
            .rounded_points(points, closed)
            .iter()
            .map(|q| PenPt { x: n.x + q.x * n.w, y: n.y + q.y * n.h, hin: [q.hin[0] * n.w, q.hin[1] * n.h], hout: [q.hout[0] * n.w, q.hout[1] * n.h], r: None })
            .collect(),
        closed,
    };
    let mut out = vec![world(&n.points, n.closed)];
    out.extend(n.holes.iter().map(|h| world(h, true)));
    out.retain(|c| c.pts.len() >= 2);
    out
}

/// What the stroke of a node is made from.
enum Along {
    /// The lines it runs along.
    Lines(Vec<Contour>),
    /// A stroke as wide as its shape, which covers all of it: the shape to fill.
    Solid(Vec<Contour>),
}

fn has_stroke(n: &Node) -> bool {
    n.kind != Kind::Text && n.stroke.is_some_and(|c| c[3] > 0) && n.stroke_w > 0.0
}

fn stroke_along(n: &Node) -> Along {
    let (x0, y0, x1, y1) = n.bounds();
    let inset = n.stroke_inset();
    match n.kind {
        Kind::Rect | Kind::Frame | Kind::Image => {
            let (w, h) = (x1 - x0, y1 - y0);
            let rad = if n.kind == Kind::Frame { 0.0 } else { n.radius.min(w * 0.5).min(h * 0.5).max(0.0) };
            let along = (x0 + inset, y0 + inset, x1 - inset, y1 - inset);
            if along.2 - along.0 <= 0.0 || along.3 - along.1 <= 0.0 {
                let grow = n.stroke_w * 0.5 - inset;
                Along::Solid(vec![rounded_rect((x0 - grow, y0 - grow, x1 + grow, y1 + grow), if rad > 0.0 { rad + grow } else { 0.0 })])
            } else {
                Along::Lines(vec![rounded_rect(along, if rad > 0.0 { (rad - inset).max(0.0) } else { 0.0 })])
            }
        }
        Kind::Ellipse => {
            let (rx, ry) = ((x1 - x0) * 0.5 - inset, (y1 - y0) * 0.5 - inset);
            let c = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
            if rx <= 0.0 || ry <= 0.0 {
                let grow = n.stroke_w * 0.5 - inset;
                Along::Solid(vec![ellipse(c, ((x1 - x0) * 0.5 + grow, (y1 - y0) * 0.5 + grow))])
            } else {
                Along::Lines(vec![ellipse(c, (rx, ry))])
            }
        }
        Kind::Line => Along::Lines(vec![Contour { pts: vec![corner(n.x, n.y), corner(n.x + n.w, n.y + n.h)], closed: false }]),
        _ => Along::Lines(path_contours(n)),
    }
}

/// How the stroke of a node is drawn: its width, caps, joins and dashes.
pub fn stroke_style(n: &Node) -> StrokeStyle {
    let dash = (n.dash > 0.0).then(|| (n.dash, if n.gap > 0.0 { n.gap } else { n.dash }));
    StrokeStyle { width: n.stroke_w, cap: Cap::of(n.cap), join: Join::of(n.join), miter_limit: MITER_LIMIT, dash }
}

/// The lines the stroke of a node runs along, in world coordinates before its rotation and
/// flips, with the stroke of a rectangle or ellipse already moved inside / outside. The canvas,
/// the export of dashed rounded rectangles and Outline stroke all start from these, so a dash
/// begins at the same point in each. `None`: nothing to stroke, or the stroke covers the whole
/// shape.
pub fn stroke_lines(n: &Node) -> Option<Vec<Contour>> {
    if !has_stroke(n) {
        return None;
    }
    match stroke_along(n) {
        Along::Lines(lines) if !lines.is_empty() => Some(lines),
        _ => None,
    }
}

/// The outline of the stroke of a node, where the canvas draws it (rotation and flips
/// included). `None`: the node has no stroke to outline, or is of a kind that has none (text).
pub fn node_outline(n: &Node) -> Option<Vec<Contour>> {
    if !has_stroke(n) {
        return None;
    }
    let mut out = match stroke_along(n) {
        Along::Lines(lines) => stroke_outline(&lines, &stroke_style(n)),
        Along::Solid(shape) => shape,
    };
    if out.is_empty() {
        return None;
    }
    let (cx, cy) = n.center();
    let (flip_h, flip_v) = n.flips();
    for q in out.iter_mut().flat_map(|c| c.pts.iter_mut()) {
        if flip_h {
            (q.x, q.hin[0], q.hout[0]) = (2.0 * cx - q.x, -q.hin[0], -q.hout[0]);
        }
        if flip_v {
            (q.y, q.hin[1], q.hout[1]) = (2.0 * cy - q.y, -q.hin[1], -q.hout[1]);
        }
        if n.rotation != 0.0 {
            (q.x, q.y) = rotate(q.x, q.y, cx, cy, n.rotation);
            let turn = |v: [f32; 2]| {
                let (x, y) = rotate(v[0], v[1], 0.0, 0.0, n.rotation);
                [x, y]
            };
            (q.hin, q.hout) = (turn(q.hin), turn(q.hout));
        }
    }
    Some(out)
}

/// Make `n` the path through these contours (all closed, unless there is only one).
pub fn set_contours(n: &mut Node, contours: Vec<Contour>) {
    let closed = contours.first().is_none_or(|c| c.closed);
    set_geo(n, &Geo { cs: contours.into_iter().map(|c| c.pts).collect(), closed });
}

// ---------------------------------------------------------------- document

/// What `Doc::outline_strokes` did.
#[derive(Default, Debug)]
pub struct Outlined {
    /// The nodes to select: each outline, and the shape it was taken from where that stays.
    pub ids: Vec<u64>,
    /// The outline nodes alone.
    pub outlines: Vec<u64>,
    /// Text layers: their strokes cannot be outlined.
    pub text: usize,
    /// Frames whose stroke is not simply inside them, and layers inside an instance.
    pub skipped: usize,
}

impl Doc {
    /// Outline stroke: the stroke of each of these nodes becomes a filled path. A shape with a
    /// fill stays, without its stroke, and the outline goes right above it, in one group with
    /// it; a shape without a fill is replaced by its outline (and keeps its id). The stroke of
    /// a frame becomes the top layer inside the frame. Nodes without a stroke are passed over.
    pub fn outline_strokes(&mut self, ids: &[u64]) -> Outlined {
        let mut done = Outlined::default();
        let mut seen: Vec<u64> = Vec::new();
        for &id in ids {
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            let Some(n) = self.find(id).cloned() else { continue };
            if n.stroke.is_none_or(|c| c[3] == 0) || !(n.stroke_w > 0.0) {
                continue;
            }
            if n.kind == Kind::Text {
                done.text += 1;
                continue;
            }
            // The border of a frame is drawn over what the frame holds and is cut off at its
            // box, so only a stroke that lies inside it can become a layer in the frame.
            let frame = n.kind == Kind::Frame;
            if n.source.is_some() || (frame && (n.stroke_pos() != 0 || n.instance_of.is_some() || n.rotation != 0.0)) {
                done.skipped += 1;
                continue;
            }
            let Some(contours) = node_outline(&n) else { continue };
            let keep = match n.kind {
                Kind::Frame | Kind::Image => true,
                Kind::Line => false,
                Kind::Path => n.closed && n.fill.is_some(),
                _ => n.fill.is_some(),
            };
            let mut o = Node::new(if keep { self.alloc_id() } else { n.id }, Kind::Path, 0.0, 0.0);
            o.name = format!("{} outline", n.name);
            (o.fill, o.fill_style) = (n.stroke, n.stroke_style);
            (o.visible, o.locked, o.tag) = (n.visible, n.locked, n.tag);
            set_contours(&mut o, contours);
            if frame {
                // Like the border it was, it follows the frame when that is resized.
                (o.constrain_h, o.constrain_v) = (Constraint::Stretch, Constraint::Stretch);
                o.absolute = n.is_auto();
                let oid = o.id;
                if let Some(f) = self.find_mut(id) {
                    (f.stroke, f.stroke_style) = (None, None);
                    f.children.push(o);
                }
                done.ids.push(oid);
                done.outlines.push(oid);
                continue;
            }
            (o.opacity, o.blend, o.clip, o.shadow, o.blur) = (n.opacity, n.blend, n.clip, n.shadow, n.blur);
            (o.constrain_h, o.constrain_v, o.absolute) = (n.constrain_h, n.constrain_v, n.absolute);
            o.group = n.group;
            if !keep {
                if let Some(slot) = self.find_mut(id) {
                    *slot = o;
                }
                done.ids.push(id);
                done.outlines.push(id);
                continue;
            }
            let parent = self.parent_of(id);
            let Some(at) = self.locate(id).and_then(|path| path.last().copied()) else { continue };
            if n.group == 0 {
                o.group = self.alloc_id();
                self.group_names.insert(o.group, n.name.clone());
            }
            // Beside an item of an auto layout the outline would be an item of its own.
            o.absolute |= parent.and_then(|p| self.find(p)).is_some_and(|p| p.is_auto());
            let (oid, group) = (o.id, o.group);
            if let Some(shape) = self.find_mut(id) {
                (shape.stroke, shape.stroke_style, shape.group) = (None, None, group);
            }
            self.insert_at(o, parent, at + 1);
            done.ids.extend([id, oid]);
            done.outlines.push(oid);
        }
        done
    }
}

impl Doc {
    /// Outline text: the text node `id` is replaced by filled paths made from `parts` (see
    /// `textpath::text_parts`): per colour one path with the glyphs, and one with the underlines
    /// and strikethroughs if there are any. The first takes the place and the id of the text;
    /// several are one group. Result: the ids of the paths, empty if nothing was made.
    pub fn text_to_paths(&mut self, id: u64, parts: Vec<crate::textpath::Part>) -> Vec<u64> {
        let Some(n) = self.find(id).filter(|n| n.kind == Kind::Text).cloned() else { return Vec::new() };
        let size = n.max_size();
        let mut made: Vec<Node> = Vec::new();
        for part in parts {
            let [r, g, b, a] = part.color.to_srgba_unmultiplied();
            let glyphs = crate::textpath::even_odd(part.glyphs, size);
            // Lines of different sizes can overlap where they meet.
            let bars = simplify(&part.bars, Rule::NonZero, 0.0);
            for (contours, what) in [(glyphs, "outline"), (bars, "lines")] {
                if contours.is_empty() {
                    continue;
                }
                let mut o = Node::new(0, Kind::Path, 0.0, 0.0);
                o.name = format!("{} {what}", n.name);
                o.fill = Some([r, g, b, a]);
                o.fill_style = n.fill_style.filter(|_| n.fill == o.fill);
                (o.visible, o.locked, o.tag) = (n.visible, n.locked, n.tag);
                (o.opacity, o.blend, o.clip, o.shadow, o.blur) = (n.opacity, n.blend, n.clip, n.shadow, n.blur);
                o.inner_shadow = n.inner_shadow;
                (o.constrain_h, o.constrain_v, o.absolute) = (n.constrain_h, n.constrain_v, n.absolute);
                o.group = n.group;
                set_contours(&mut o, contours);
                made.push(o);
            }
        }
        if made.is_empty() {
            return Vec::new();
        }
        let parent = self.parent_of(id);
        let Some(at) = self.locate(id).and_then(|path| path.last().copied()) else { return Vec::new() };
        if made.len() > 1 && n.group == 0 {
            let group = self.alloc_id();
            self.group_names.insert(group, n.name.clone());
            made.iter_mut().for_each(|o| o.group = group);
        }
        let mut ids = Vec::new();
        for (i, mut o) in made.into_iter().enumerate() {
            o.id = if i == 0 { id } else { self.alloc_id() };
            ids.push(o.id);
            if i == 0 {
                if let Some(slot) = self.find_mut(id) {
                    *slot = o;
                }
            } else {
                // Beside an item of an auto layout the others would be items of their own.
                o.absolute |= parent.and_then(|p| self.find(p)).is_some_and(|p| p.is_auto());
                self.insert_at(o, parent, at + i);
            }
        }
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f32, y: f32, side: f32) -> Contour {
        rounded_rect((x, y, x + side, y + side), 0.0)
    }

    fn solid(width: f32, cap: Cap, join: Join) -> StrokeStyle {
        StrokeStyle { width, cap, join, miter_limit: 4.0, dash: None }
    }

    fn near(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol
    }

    fn rect_node(align: u8) -> Node {
        let mut n = Node::new(1, Kind::Rect, 100.0, 50.0);
        (n.w, n.h, n.stroke, n.stroke_w, n.stroke_align) = (100.0, 100.0, Some([10, 20, 30, 255]), 10.0, Some(align));
        n
    }

    #[test]
    fn square_stroke_has_the_area_of_its_band() {
        // Centre: 110² - 90². Inside: 100² - 80². Outside: 120² - 100².
        for (align, want) in [(1, 4000.0), (0, 3600.0), (2, 4400.0)] {
            let out = node_outline(&rect_node(align)).unwrap();
            assert_eq!(out.len(), 2, "outer edge and hole (align {align})");
            assert!(near(area(&out), want, 1.0), "align {align}: {} (want {want})", area(&out));
        }
    }

    #[test]
    fn open_line_with_butt_caps_is_a_rectangle() {
        let line = Contour { pts: vec![corner(10.0, 20.0), corner(110.0, 20.0)], closed: false };
        let out = stroke_outline(&[line], &solid(8.0, Cap::Butt, Join::Miter));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pts.len(), 4);
        assert!(out[0].pts.iter().all(|q| q.hin == [0.0; 2] && q.hout == [0.0; 2]));
        let (xs, ys): (Vec<f32>, Vec<f32>) = out[0].pts.iter().map(|q| (q.x, q.y)).unzip();
        let span = |v: &[f32]| (v.iter().copied().fold(f32::MAX, f32::min), v.iter().copied().fold(f32::MIN, f32::max));
        assert_eq!((span(&xs), span(&ys)), ((10.0, 110.0), (16.0, 24.0)));
        assert!(near(area(&out), 800.0, 0.01));
    }

    #[test]
    fn round_and_square_caps_reach_past_the_ends() {
        let line = Contour { pts: vec![corner(10.0, 20.0), corner(110.0, 20.0)], closed: false };
        let span = |out: &[Contour]| out.iter().flat_map(|c| c.pts.iter()).fold((f32::MAX, f32::MIN), |b, q| (b.0.min(q.x), b.1.max(q.x)));
        // Square: the rectangle is longer by half the width at each end.
        let square = stroke_outline(&[line.clone()], &solid(8.0, Cap::Square, Join::Miter));
        assert_eq!((square.len(), span(&square)), (1, (6.0, 114.0)));
        assert!(square[0].pts.iter().all(|q| q.hin == [0.0; 2] && q.hout == [0.0; 2]));
        assert!(near(area(&square), 108.0 * 8.0, 0.01));
        // Round: a half disc at each end, kept as curves.
        let round = stroke_outline(&[line], &solid(8.0, Cap::Round, Join::Miter));
        assert_eq!(round.len(), 1);
        let (x0, x1) = span(&round);
        assert!((x0 - 6.0).abs() < 0.05 && (x1 - 114.0).abs() < 0.05, "{x0} .. {x1}");
        assert!(round[0].pts.iter().any(|q| q.hin != [0.0; 2] || q.hout != [0.0; 2]));
        let want = 800.0 + std::f64::consts::PI * 16.0;
        assert!(near(area(&round), want, want * 0.003), "{} (want {want})", area(&round));
    }

    #[test]
    fn joins_fill_the_corner_each_in_their_own_way() {
        // A right angle: the square outside the corner is 5 × 5.
        let bend = Contour { pts: vec![corner(0.0, 0.0), corner(50.0, 0.0), corner(50.0, 50.0)], closed: false };
        let of = |join| area(&stroke_outline(&[bend.clone()], &solid(10.0, Cap::Butt, join)));
        assert!(near(of(Join::Miter), 1000.0, 0.5), "{}", of(Join::Miter));
        assert!(near(of(Join::Bevel), 987.5, 0.5), "{}", of(Join::Bevel));
        let round = 975.0 + std::f64::consts::PI * 25.0 / 4.0;
        assert!(near(of(Join::Round), round, 0.5), "{} (want {round})", of(Join::Round));
        // A corner of about 14 degrees: past the miter limit, so the point is cut off like a bevel.
        let spike = Contour { pts: vec![corner(0.0, 0.0), corner(100.0, 12.0), corner(0.0, 24.0)], closed: false };
        let tip = |join| stroke_outline(&[spike.clone()], &solid(10.0, Cap::Butt, join)).iter().flat_map(|c| c.pts.iter()).fold(f32::MIN, |m, q| m.max(q.x));
        assert!((tip(Join::Miter) - tip(Join::Bevel)).abs() < 0.01 && tip(Join::Miter) < 102.0, "{}", tip(Join::Miter));
        // A corner of 60 degrees is within it: the point reaches a whole stroke width past the corner.
        let wedge = Contour { pts: vec![corner(0.0, 0.0), corner(100.0, 57.735), corner(0.0, 115.47)], closed: false };
        let reach = stroke_outline(&[wedge], &solid(10.0, Cap::Butt, Join::Miter)).iter().flat_map(|c| c.pts.iter()).fold(f32::MIN, |m, q| m.max(q.x));
        assert!((reach - 110.0).abs() < 0.05, "{reach}");
    }

    #[test]
    fn the_outline_of_a_node_follows_its_cap_and_join() {
        let mut line = Node::new(1, Kind::Line, 0.0, 0.0);
        (line.w, line.h, line.stroke, line.stroke_w) = (100.0, 0.0, Some([0, 0, 0, 255]), 10.0);
        let width = |n: &Node| {
            let out = node_outline(n).unwrap();
            let (a, b) = out.iter().flat_map(|c| c.pts.iter()).fold((f32::MAX, f32::MIN), |b, q| (b.0.min(q.x), b.1.max(q.x)));
            b - a
        };
        assert_eq!(width(&line), 100.0);
        line.cap = 2;
        assert_eq!(width(&line), 110.0);
        line.cap = 1;
        assert!((width(&line) - 110.0).abs() < 0.05);
        // Each dash gets the caps.
        (line.dash, line.gap) = (10.0, 10.0);
        let dashes = node_outline(&line).unwrap();
        assert_eq!(dashes.len(), 5);
        let want = 5.0 * (100.0 + std::f64::consts::PI * 25.0);
        assert!(near(area(&dashes), want, want * 0.003), "{}", area(&dashes));
        // A square with round corners: the outer edge is rounded, the inner one is not.
        let mut rect = rect_node(1);
        rect.join = 1;
        let want = 4000.0 - (4.0 - std::f64::consts::PI) * 25.0;
        let got = area(&node_outline(&rect).unwrap());
        assert!(near(got, want, 1.5), "{got} (want {want})");
        rect.join = 2;
        assert!(near(area(&node_outline(&rect).unwrap()), 4000.0 - 50.0, 1.5));
    }

    #[test]
    fn closed_path_gives_an_outer_edge_and_a_hole() {
        let tri = Contour { pts: vec![corner(0.0, 0.0), corner(200.0, 0.0), corner(100.0, 160.0)], closed: true };
        let out = stroke_outline(&[tri.clone()], &solid(10.0, Cap::Butt, Join::Miter));
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|c| c.closed && c.pts.len() == 3), "three corners each: {:?}", out.iter().map(|c| c.pts.len()).collect::<Vec<_>>());
        // The band is as long as the triangle's edge and as wide as the stroke (mitred corners
        // add outside what they take away inside, up to the second order).
        let edge = 200.0 + 2.0 * (100.0f64).hypot(160.0);
        assert!(near(area(&out), edge * 10.0, edge * 10.0 * 0.03), "{}", area(&out));
    }

    #[test]
    fn circle_outline_keeps_smooth_curves() {
        let mut n = Node::new(1, Kind::Ellipse, 0.0, 0.0);
        (n.w, n.h, n.stroke, n.stroke_w) = (200.0, 200.0, Some([0, 0, 0, 255]), 20.0);
        let out = node_outline(&n).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|c| c.pts.len() <= 16 && c.pts.iter().all(|q| q.hin != [0.0; 2] && q.hout != [0.0; 2])));
        let want = std::f64::consts::PI * (110.0f64.powi(2) - 90.0f64.powi(2));
        assert!(near(area(&out), want, want * 0.002), "{} (want {want})", area(&out));
    }

    #[test]
    fn dashes_become_separate_pieces() {
        let line = Contour { pts: vec![corner(0.0, 0.0), corner(100.0, 0.0)], closed: false };
        let style = StrokeStyle { dash: Some((10.0, 10.0)), ..solid(4.0, Cap::Butt, Join::Bevel) };
        let out = stroke_outline(&[line], &style);
        assert_eq!(out.len(), 5);
        assert!(near(area(&out), 5.0 * 40.0, 0.01));
    }

    #[test]
    fn self_overlap_is_united() {
        // A stroke wider than the gap between two strokes of a "V": the inside fills up, and
        // the outline is one contour without a hole.
        let v = Contour { pts: vec![corner(0.0, 0.0), corner(10.0, 100.0), corner(20.0, 0.0)], closed: false };
        let out = stroke_outline(&[v], &solid(30.0, Cap::Butt, Join::Bevel));
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn nonzero_parts_that_overlap_are_merged() {
        // Two squares that wind the same way and overlap: non-zero fills the overlap, even-odd
        // would leave it open.
        let out = simplify(&[square(0.0, 0.0, 100.0), square(50.0, 50.0, 100.0)], Rule::NonZero, 0.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].pts.len(), 8);
        assert!(near(area(&out), 17500.0, 1.0));
    }

    #[test]
    fn nonzero_part_inside_a_part_is_dropped_and_a_hole_is_kept() {
        let (outer, inner) = (square(0.0, 0.0, 100.0), square(25.0, 25.0, 50.0));
        // Same direction: no hole under non-zero. The outer contour comes back untouched.
        let out = simplify(&[outer.clone(), inner.clone()], Rule::NonZero, 0.0);
        assert!(out == vec![outer.clone()]);
        // Opposite direction: a hole under both rules, nothing changes.
        let mut hole = inner.clone();
        hole.pts.reverse();
        let out = simplify(&[outer.clone(), hole.clone()], Rule::NonZero, 0.0);
        assert!(out == vec![outer, hole]);
        assert!(near(area(&out), 7500.0, 0.5));
    }

    #[test]
    fn rotation_and_flip_are_baked_into_the_outline() {
        let mut n = rect_node(1);
        (n.w, n.h, n.rotation) = (200.0, 20.0, 90.0);
        let out = node_outline(&n).unwrap();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for q in out.iter().flat_map(|c| c.pts.iter()) {
            (x0, y0, x1, y1) = (x0.min(q.x), y0.min(q.y), x1.max(q.x), y1.max(q.y));
        }
        // Centre (200, 60); turned by 90°, the 210 × 30 box of the stroke stands upright.
        let want = (185.0, -45.0, 215.0, 165.0);
        for (got, want) in [(x0, want.0), (y0, want.1), (x1, want.2), (y1, want.3)] {
            assert!((got - want).abs() < 0.01, "{:?} (want {want:?})", (x0, y0, x1, y1));
        }
    }

    #[test]
    fn a_filled_shape_stays_and_an_empty_one_is_replaced() {
        let mut doc = Doc::default();
        let mut filled = rect_node(1);
        (filled.id, filled.fill, filled.name) = (doc.alloc_id(), Some([200, 0, 0, 255]), "Card".into());
        let mut empty = rect_node(0);
        (empty.id, empty.x) = (doc.alloc_id(), 400.0);
        let mut text = Node::new(doc.alloc_id(), Kind::Text, 0.0, 0.0);
        (text.stroke, text.stroke_w) = (Some([0, 0, 0, 255]), 2.0);
        let plain = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        let ids = [filled.id, empty.id, text.id, plain.id];
        for n in [filled, empty, text, plain] {
            doc.insert(n, None);
        }
        let done = doc.outline_strokes(&ids);
        assert_eq!((done.outlines.len(), done.text, done.skipped), (2, 1, 0));
        assert_eq!(doc.nodes.len(), 5);
        let (card, outline) = (&doc.nodes[0], &doc.nodes[1]);
        assert_eq!((card.id, card.stroke, card.fill), (ids[0], None, Some([200, 0, 0, 255])));
        assert_eq!((outline.kind, outline.fill, outline.stroke, outline.name.as_str()), (Kind::Path, Some([10, 20, 30, 255]), None, "Card outline"));
        assert!(card.group != 0 && card.group == outline.group);
        assert_eq!((outline.x, outline.y, outline.w, outline.h), (95.0, 45.0, 110.0, 110.0));
        assert_eq!(outline.holes.len(), 1);
        // The empty one is its outline now, under the same id; the inside stroke stays inside.
        let replaced = &doc.nodes[2];
        assert_eq!((replaced.id, replaced.kind, replaced.group), (ids[1], Kind::Path, 0));
        assert_eq!((replaced.x, replaced.y, replaced.w, replaced.h), (400.0, 50.0, 100.0, 100.0));
        assert_eq!(done.ids, vec![ids[0], outline.id, ids[1]]);
    }
}
