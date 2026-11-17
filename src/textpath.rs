//! Text as vector outlines, straight or along a path.
//!
//! A text node with `on_path` runs along another node of its page (a path, line, ellipse or
//! rectangle): one line, every glyph at its advance along the flattened shape, turned to the
//! tangent, with its baseline on the line. The canvas, the export and Outline text all take the
//! glyph places from `laid`, so they agree exactly. The same module turns any text into filled
//! contours (`text_parts`), which is what "Outline text" puts into the document.

use crate::app::render::{layout_text, text_job, text_offset, PenPt};
use crate::doc::{rotate, Doc, Kind, Node};
use crate::fonts::FontManager;
use crate::outline::{self, Contour, Rule};
use eframe::egui::{self, Color32, FontFamily};
use std::collections::HashMap;

type P = (f32, f32);

// ---------------------------------------------------------------- the line to follow

/// A polyline in world coordinates with the distance along it at every point.
#[derive(Clone, Debug, PartialEq)]
pub struct Guide {
    pts: Vec<P>,
    /// Distance from the start to each point; for a closed line one more: back at the start.
    cum: Vec<f32>,
    pub closed: bool,
}

impl Guide {
    /// `None`: fewer than two different points.
    pub fn new(mut pts: Vec<P>, closed: bool) -> Option<Guide> {
        pts.dedup_by(|a, b| (a.0 - b.0).abs() < 1e-5 && (a.1 - b.1).abs() < 1e-5);
        if closed && pts.len() > 2 && (pts[0].0 - pts[pts.len() - 1].0).abs() < 1e-5 && (pts[0].1 - pts[pts.len() - 1].1).abs() < 1e-5 {
            pts.pop();
        }
        if pts.len() < 2 || pts.iter().any(|p| !p.0.is_finite() || !p.1.is_finite()) {
            return None;
        }
        let mut cum = Vec::with_capacity(pts.len() + 1);
        let mut total = 0.0;
        cum.push(0.0);
        let count = if closed { pts.len() } else { pts.len() - 1 };
        for i in 0..count {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            total += (b.0 - a.0).hypot(b.1 - a.1);
            cum.push(total);
        }
        (total > 1e-4).then_some(Guide { pts, cum, closed })
    }

    /// Its whole length.
    pub fn len(&self) -> f32 {
        self.cum[self.cum.len() - 1]
    }

    /// The point at distance `s` from the start and the direction of travel there (unit length).
    /// A closed line goes round and round; an open one has nothing before its start or past its
    /// end.
    pub fn at(&self, s: f32) -> Option<(P, P)> {
        let len = self.len();
        let s = if self.closed {
            s.rem_euclid(len)
        } else if s < -1e-3 || s > len + 1e-3 {
            return None;
        } else {
            s.clamp(0.0, len)
        };
        // The segment that holds `s`: the last one that starts at or before it.
        let mut i = self.cum.partition_point(|c| *c <= s).saturating_sub(1).min(self.cum.len() - 2);
        // (Segments of no length have no direction.)
        while i + 2 < self.cum.len() && self.cum[i + 1] - self.cum[i] < 1e-7 {
            i += 1;
        }
        let (a, b) = (self.pts[i], self.pts[(i + 1) % self.pts.len()]);
        let seg = (self.cum[i + 1] - self.cum[i]).max(1e-7);
        let (along, tan) = ((s - self.cum[i]).clamp(0.0, seg), ((b.0 - a.0) / seg, (b.1 - a.1) / seg));
        Some(((a.0 + tan.0 * along, a.1 + tan.1 * along), tan))
    }

    /// The point of the line nearest to `p`: (how far away it is, its distance from the start).
    pub fn nearest(&self, p: P) -> (f32, f32) {
        let mut best = (f32::MAX, 0.0);
        for i in 0..self.cum.len() - 1 {
            let (a, b) = (self.pts[i], self.pts[(i + 1) % self.pts.len()]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len2 = dx * dx + dy * dy;
            let t = if len2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let d = (p.0 - (a.0 + dx * t)).hypot(p.1 - (a.1 + dy * t));
            if d < best.0 {
                best = (d, self.cum[i] + (self.cum[i + 1] - self.cum[i]) * t);
            }
        }
        best
    }

    /// The same line walked the other way: text on it stands on the other side. A closed line
    /// keeps its starting point.
    pub fn reversed(&self) -> Guide {
        let mut pts = self.pts.clone();
        if self.closed {
            pts[1..].reverse();
        } else {
            pts.reverse();
        }
        Guide::new(pts, self.closed).unwrap_or_else(|| self.clone())
    }
}

/// A cubic curve as points close enough to it for text to follow: within `tol`, and turning by
/// small steps, since every glyph takes the direction of the piece it stands on.
fn flatten(c: &Contour, tol: f32) -> Vec<P> {
    let n = c.pts.len();
    let mut out: Vec<P> = Vec::with_capacity(n * 8);
    if n == 0 {
        return out;
    }
    out.push((c.pts[0].x, c.pts[0].y));
    let count = if c.closed { n } else { n - 1 };
    for i in 0..count {
        let (a, b) = (c.pts[i], c.pts[(i + 1) % n]);
        let (p0, p3) = ((a.x, a.y), (b.x, b.y));
        if a.hout == [0.0; 2] && b.hin == [0.0; 2] {
            out.push(p3);
            continue;
        }
        let (c1, c2) = ((a.x + a.hout[0], a.y + a.hout[1]), (b.x + b.hin[0], b.y + b.hin[1]));
        let dev = |p: P, q: P, r: P| (p.0 - 2.0 * q.0 + r.0).hypot(p.1 - 2.0 * q.1 + r.1);
        let by_tol = (0.75 * dev(p0, c1, c2).max(dev(c1, c2, p3)) / tol.max(1e-6)).sqrt();
        let legs = [(c1.0 - p0.0, c1.1 - p0.1), (c2.0 - c1.0, c2.1 - c1.1), (p3.0 - c2.0, p3.1 - c2.1)];
        let legs: Vec<P> = legs.into_iter().filter(|d| d.0.hypot(d.1) > 1e-9).collect();
        let turn: f32 = legs.windows(2).map(|w| (w[0].0 * w[1].1 - w[0].1 * w[1].0).atan2(w[0].0 * w[1].0 + w[0].1 * w[1].1).abs()).sum();
        let steps = (by_tol.max(turn / 0.03).ceil() as usize).clamp(2, 600);
        for s in 1..=steps {
            let t = s as f32 / steps as f32;
            let u = 1.0 - t;
            let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            out.push((k0 * p0.0 + k1 * c1.0 + k2 * c2.0 + k3 * p3.0, k0 * p0.1 + k1 * c1.1 + k2 * c2.1 + k3 * p3.1));
        }
    }
    out
}

/// Can text be put on this node?
pub fn can_carry(n: &Node) -> bool {
    matches!(n.kind, Kind::Path | Kind::Line | Kind::Ellipse | Kind::Rect)
}

/// The line a text on `n` follows, where the node is drawn (its rotation included). A path gives
/// its main contour from its first point; a line runs from its start to its end; a rectangle
/// starts at its top left corner and an ellipse at its top, both clockwise.
pub fn guide_of(n: &Node) -> Option<Guide> {
    let (x0, y0, x1, y1) = n.bounds();
    let corner = |x: f32, y: f32| PenPt { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
    let contour = match n.kind {
        Kind::Line => Contour { pts: vec![corner(n.x, n.y), corner(n.x + n.w, n.y + n.h)], closed: false },
        Kind::Path => outline::path_contours(n).into_iter().next()?,
        Kind::Rect => {
            let (w, h) = (x1 - x0, y1 - y0);
            let mut c = outline::rounded_rect((x0, y0, x1, y1), n.radius.min(w * 0.5).min(h * 0.5).max(0.0));
            if c.pts.len() == 8 {
                // From the end of the top left corner, along the top edge.
                c.pts.rotate_right(1);
            }
            c
        }
        Kind::Ellipse => {
            let mut c = outline::ellipse(((x0 + x1) * 0.5, (y0 + y1) * 0.5), ((x1 - x0) * 0.5, (y1 - y0) * 0.5));
            c.pts.rotate_left(3);
            c
        }
        _ => return None,
    };
    let diag = (x1 - x0).hypot(y1 - y0);
    let mut pts = flatten(&contour, (diag * 2e-4).max(1e-4));
    if n.rotation != 0.0 {
        let (cx, cy) = n.center();
        for p in &mut pts {
            *p = rotate(p.0, p.1, cx, cy, n.rotation);
        }
    }
    Guide::new(pts, contour.closed)
}

/// Everything about a node that `guide_of` depends on, as one number.
pub fn guide_key(n: &Node) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (n.kind as u8, n.x.to_bits(), n.y.to_bits(), n.w.to_bits(), n.h.to_bits(), n.rotation.to_bits(), n.radius.to_bits(), n.closed).hash(&mut h);
    if n.kind == Kind::Path {
        for q in &n.points {
            (q.x.to_bits(), q.y.to_bits(), q.hin.map(f32::to_bits), q.hout.map(f32::to_bits), q.r.map(f32::to_bits)).hash(&mut h);
        }
    }
    h.finish()
}

// ---------------------------------------------------------------- placing glyphs

/// Where the text starts on a line of length `len`: the distance of its left edge from the
/// start of the line. On an open line the text is aligned within the line (left: at its start,
/// centre: in its middle, right: ending at its end). A closed line has no ends, so there the
/// text is aligned on the starting point (starts at it, is centred on it, ends at it). `offset`
/// moves it along from there.
pub fn start(len: f32, width: f32, align: u8, offset: f32, closed: bool) -> f32 {
    let span = if closed { 0.0 } else { len };
    offset
        + match align {
            1 => (span - width) * 0.5,
            2 => span - width,
            _ => 0.0,
        }
}

/// A glyph on the line: the point of its origin (left end of its baseline) and the direction
/// its baseline runs in.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub at: P,
    pub tan: P,
}

/// Put cells (left edge and advance, in the units of the line) on the line: each is turned
/// about the middle of its advance, which sits on the line. `None`: that cell falls off an open
/// line, or would come round to where the text began on a closed one.
pub fn place(guide: &Guide, cells: &[(f32, f32)], start: f32) -> Vec<Option<Spot>> {
    let len = guide.len();
    cells
        .iter()
        .map(|&(x, adv)| {
            let mid = x + adv * 0.5;
            if guide.closed && (mid > len || mid < 0.0) {
                return None;
            }
            let (p, t) = guide.at(start + mid)?;
            Some(Spot { at: (p.0 - t.0 * adv * 0.5, p.1 - t.1 * adv * 0.5), tan: t })
        })
        .collect()
}

// ---------------------------------------------------------------- one line of text

/// One character of a line of text, in world units from the left end of the baseline.
#[derive(Clone, Debug)]
pub struct Cell {
    pub chr: char,
    pub family: FontFamily,
    pub size: f32,
    pub color: Color32,
    pub x: f32,
    pub adv: f32,
    /// How far the font reaches above and below the baseline (for the box of the text).
    pub asc: f32,
    pub desc: f32,
}

/// An underline or strikethrough: from `x0` to `x1` along the baseline, from `y0` to `y1`
/// below it (negative = above).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bar {
    pub x0: f32,
    pub x1: f32,
    pub y0: f32,
    pub y1: f32,
    pub color: Color32,
}

#[derive(Clone, Debug, Default)]
pub struct Line {
    pub cells: Vec<Cell>,
    pub bars: Vec<Bar>,
    pub width: f32,
}

/// Size of the largest font in the layout the glyph advances are read from. They do not depend
/// on the zoom, so text on a path is the same on the canvas at any zoom and in the export.
const REF_SIZE: f32 = 128.0;

/// The layout job of the text of `n` as one line (line breaks count as spaces): what `line`
/// reads the advances from, and what tells whether that line has to be made again.
pub fn line_job(n: &Node, fonts: &mut FontManager) -> (egui::text::LayoutJob, f32) {
    let f = REF_SIZE / n.max_size().max(1e-3);
    let text: std::borrow::Cow<str> = if n.text.contains('\n') { n.text.replace('\n', " ").into() } else { n.text.as_str().into() };
    let mut job = text_job(n, &text, fonts, f, 1.0, false);
    job.wrap.max_width = f32::INFINITY;
    (job, f)
}

/// The text of `n` as one line: its characters with their advances (letter spacing and mixed
/// styles included) and its underlines and strikethroughs.
pub fn line(ctx: &egui::Context, n: &Node, fonts: &mut FontManager) -> Line {
    let (job, f) = line_job(n, fonts);
    let galley = fonts.reference_layout(ctx, job);
    let mut out = Line::default();
    for g in galley.rows.iter().flat_map(|row| &row.glyphs) {
        let Some(format) = galley.job.sections.get(g.section_index as usize).map(|s| &s.format) else { continue };
        let cell = Cell {
            chr: g.chr,
            family: format.font_id.family.clone(),
            size: format.font_id.size / f,
            color: format.color,
            x: g.pos.x / f,
            adv: g.advance_width / f,
            asc: g.font_ascent / f,
            desc: (g.font_height - g.font_ascent) / f,
        };
        out.width = out.width.max(cell.x + cell.adv);
        out.cells.push(cell);
    }
    // Lines under and through runs of the same size and colour (as `render::decorate` draws
    // them for straight text); spaces at the end get none.
    let end = out.cells.iter().rposition(|c| !c.chr.is_whitespace()).map_or(0, |i| i + 1);
    let formats: Vec<(bool, bool)> = galley
        .rows
        .iter()
        .flat_map(|row| &row.glyphs)
        .map(|g| galley.job.sections.get(g.section_index as usize).map_or((false, false), |s| (s.format.underline != egui::Stroke::NONE, s.format.strikethrough != egui::Stroke::NONE)))
        .collect();
    for strike in [false, true] {
        let mut open: Option<(u32, Color32)> = None;
        for (cell, marks) in out.cells[..end].iter().zip(&formats) {
            if !(if strike { marks.1 } else { marks.0 }) {
                open = None;
                continue;
            }
            let key = (cell.size.to_bits(), cell.color);
            let right = cell.x + cell.adv;
            if open == Some(key) {
                if let Some(bar) = out.bars.last_mut() {
                    bar.x1 = right;
                }
            } else {
                let thick = cell.size / 14.0;
                let top = if strike { -cell.size * 0.27 - thick * 0.5 } else { cell.size * 0.09 };
                out.bars.push(Bar { x0: cell.x, x1: right, y0: top, y1: top + thick, color: cell.color });
                open = Some(key);
            }
        }
    }
    out
}

// ---------------------------------------------------------------- text on its path

/// A glyph where it is drawn, in world coordinates.
#[derive(Clone, Debug)]
pub struct Glyph {
    pub chr: char,
    pub family: FontFamily,
    pub size: f32,
    pub color: Color32,
    pub at: P,
    pub tan: P,
}

/// A text laid along its path.
#[derive(Clone, Debug, Default)]
pub struct Laid {
    pub glyphs: Vec<Glyph>,
    /// Underlines and strikethroughs as closed polygons that follow the path.
    pub bars: Vec<(Vec<P>, Color32)>,
    /// The box around all of it; `None` when nothing of the text is on the path.
    pub bbox: Option<(f32, f32, f32, f32)>,
    /// The direction of the path under the middle of the text.
    pub tan: P,
}

/// Lay a line of text along `guide` (already walked the way the text runs).
pub fn lay(line: &Line, guide: &Guide, align: u8, offset: f32) -> Laid {
    let s0 = start(guide.len(), line.width, align, offset, guide.closed);
    let cells: Vec<(f32, f32)> = line.cells.iter().map(|c| (c.x, c.adv)).collect();
    let spots = place(guide, &cells, s0);
    let mut out = Laid { tan: (1.0, 0.0), ..Default::default() };
    let mut bbox: Option<(f32, f32, f32, f32)> = None;
    let mut grow = |p: P| bbox = Some(bbox.map_or((p.0, p.1, p.0, p.1), |b| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1))));
    for (cell, spot) in line.cells.iter().zip(spots) {
        let Some(Spot { at, tan }) = spot else { continue };
        // The normal of the baseline points down, into the descenders.
        let nrm = (-tan.1, tan.0);
        for (dx, dy) in [(0.0, -cell.asc), (cell.adv, -cell.asc), (cell.adv, cell.desc), (0.0, cell.desc)] {
            grow((at.0 + tan.0 * dx + nrm.0 * dy, at.1 + tan.1 * dx + nrm.1 * dy));
        }
        out.glyphs.push(Glyph { chr: cell.chr, family: cell.family.clone(), size: cell.size, color: cell.color, at, tan });
    }
    let (lo, hi) = if guide.closed { (0.0, guide.len().min(line.width)) } else { (-s0, guide.len() - s0) };
    for bar in &line.bars {
        let (x0, x1) = (bar.x0.max(lo), bar.x1.min(hi));
        if x1 - x0 < 1e-3 {
            continue;
        }
        // Close enough steps for the band to bend with the path.
        let steps = (((x1 - x0) / (bar.y1 - bar.y0).abs().max(1e-3) / 2.0).ceil() as usize).clamp(1, 2000);
        let edge = |y: f32| {
            (0..=steps).filter_map(move |i| {
                let (p, t) = guide.at(s0 + x0 + (x1 - x0) * i as f32 / steps as f32)?;
                Some((p.0 - t.1 * y, p.1 + t.0 * y))
            })
        };
        let mut poly: Vec<P> = edge(bar.y0).collect();
        let mut back: Vec<P> = edge(bar.y1).collect();
        back.reverse();
        poly.extend(back);
        if poly.len() >= 4 {
            out.bars.push((poly, bar.color));
        }
    }
    if let Some((_, t)) = guide.at(s0 + line.width * 0.5).or_else(|| guide.at((s0 + line.width * 0.5).clamp(0.0, guide.len()))) {
        out.tan = t;
    }
    out.bbox = bbox;
    out
}

/// The text of `n` along `guide`, the line its path gives (`guide_of`): walked backwards when
/// the text is on the other side.
pub fn laid(ctx: &egui::Context, n: &Node, guide: &Guide, fonts: &mut FontManager) -> Laid {
    let line = line(ctx, n, fonts);
    if n.path_side {
        lay(&line, &guide.reversed(), n.align, n.path_offset)
    } else {
        lay(&line, guide, n.align, n.path_offset)
    }
}

// ---------------------------------------------------------------- glyph outlines

/// Collects the outline of a glyph as contours, mapped by `to` from font units (y up).
struct Builder<F: Fn(f32, f32) -> P> {
    to: F,
    out: Vec<Contour>,
    cur: Vec<PenPt>,
}

impl<F: Fn(f32, f32) -> P> Builder<F> {
    fn end(&mut self) {
        let mut pts = std::mem::take(&mut self.cur);
        if pts.len() > 1 {
            // A contour that ends on its first point: the two are one anchor.
            let (first, last) = (pts[0], pts[pts.len() - 1]);
            if (first.x - last.x).abs() < 1e-5 && (first.y - last.y).abs() < 1e-5 {
                pts[0].hin = last.hin;
                pts.pop();
            }
        }
        if pts.len() >= 2 {
            self.out.push(Contour { pts, closed: true });
        }
    }

    fn curve(&mut self, c1: P, c2: P, p: P) {
        if let Some(last) = self.cur.last_mut() {
            last.hout = [c1.0 - last.x, c1.1 - last.y];
        }
        self.cur.push(PenPt { x: p.0, y: p.1, hin: [c2.0 - p.0, c2.1 - p.1], hout: [0.0; 2], r: None });
    }
}

impl<F: Fn(f32, f32) -> P> ttf_parser::OutlineBuilder for Builder<F> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.end();
        let p = (self.to)(x, y);
        self.cur.push(PenPt { x: p.0, y: p.1, hin: [0.0; 2], hout: [0.0; 2], r: None });
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = (self.to)(x, y);
        self.cur.push(PenPt { x: p.0, y: p.1, hin: [0.0; 2], hout: [0.0; 2], r: None });
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let Some(a) = self.cur.last().map(|q| (q.x, q.y)) else { return };
        let (c, p) = ((self.to)(x1, y1), (self.to)(x, y));
        let third = |from: P| (from.0 + (c.0 - from.0) * 2.0 / 3.0, from.1 + (c.1 - from.1) * 2.0 / 3.0);
        self.curve(third(a), third(p), p);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = ((self.to)(x1, y1), (self.to)(x2, y2), (self.to)(x, y));
        self.curve(c1, c2, p);
    }
    fn close(&mut self) {
        self.end();
    }
}

/// The outline of one glyph of `size`, its origin at `at` and its baseline running along `tan`.
/// Empty for a character without an outline (a space) or one no font has. A glyph missing from
/// the chosen font comes from the built-in one, as on the canvas.
pub fn glyph_contours(fonts: &FontManager, family: &FontFamily, chr: char, size: f32, at: P, tan: P) -> Vec<Contour> {
    let face_of = |family: &FontFamily| {
        let face = ttf_parser::Face::parse(fonts.face_data(family)?, 0).ok()?;
        face.glyph_index(chr).map(|gid| (face, gid))
    };
    let Some((face, gid)) = face_of(family).or_else(|| face_of(&FontFamily::Proportional)) else { return Vec::new() };
    let s = size / face.units_per_em() as f32;
    let mut b = Builder { to: |x: f32, y: f32| (at.0 + tan.0 * x * s + tan.1 * y * s, at.1 + tan.1 * x * s - tan.0 * y * s), out: Vec::new(), cur: Vec::new() };
    if face.outline_glyph(gid, &mut b).is_none() {
        return Vec::new();
    }
    b.end();
    b.out
}

/// A text as outlines in one colour.
pub struct Part {
    pub color: Color32,
    /// The contours of each glyph (they fill under the non-zero rule, like every font outline).
    pub glyphs: Vec<Vec<Contour>>,
    /// Underlines and strikethroughs.
    pub bars: Vec<Contour>,
}

fn part_of(parts: &mut Vec<Part>, color: Color32) -> &mut Part {
    let at = match parts.iter().position(|p| p.color == color) {
        Some(at) => at,
        None => {
            parts.push(Part { color, glyphs: Vec::new(), bars: Vec::new() });
            parts.len() - 1
        }
    };
    &mut parts[at]
}

fn polygon(pts: &[P]) -> Contour {
    Contour { pts: pts.iter().map(|p| PenPt { x: p.0, y: p.1, hin: [0.0; 2], hout: [0.0; 2], r: None }).collect(), closed: true }
}

/// A text along its path as outlines, one part per colour in the order the colours first appear.
pub fn path_parts(ctx: &egui::Context, n: &Node, guide: &Guide, fonts: &mut FontManager) -> Vec<Part> {
    let laid = laid(ctx, n, guide, fonts);
    let mut parts: Vec<Part> = Vec::new();
    for g in &laid.glyphs {
        let contours = glyph_contours(fonts, &g.family, g.chr, g.size, g.at, g.tan);
        if !contours.is_empty() {
            part_of(&mut parts, g.color).glyphs.push(contours);
        }
    }
    for (poly, color) in &laid.bars {
        part_of(&mut parts, *color).bars.push(polygon(poly));
    }
    parts
}

/// A text as outlines where the canvas and the export draw it: every glyph the galley places
/// (so justified lines, list markers and vertical alignment carry over), its underlines and
/// strikethroughs, and the node's flips and rotation applied. `guide`: the line it is attached
/// to, if any.
pub fn text_parts(ctx: &egui::Context, n: &Node, guide: Option<&Guide>, fonts: &mut FontManager) -> Vec<Part> {
    if n.kind != Kind::Text || n.text.is_empty() {
        return Vec::new();
    }
    if let Some(guide) = guide {
        return path_parts(ctx, n, guide, fonts);
    }
    // The same layout as the export (see `export::text_svg`).
    let f = (200.0 / n.max_size()).min(1.0);
    let layout = layout_text(ctx, n, &n.text, fonts, f, 1.0, true);
    let off = text_offset(n, &layout, f) / f;
    let (ox, oy) = (n.x + off.x, n.y + off.y);
    let mut parts: Vec<Part> = Vec::new();
    for galley in std::iter::once(&layout.galley).chain(&layout.markers) {
        for g in galley.rows.iter().flat_map(|row| &row.glyphs) {
            let Some(format) = galley.job.sections.get(g.section_index as usize).map(|s| &s.format) else { continue };
            let contours = glyph_contours(fonts, &format.font_id.family, g.chr, format.font_id.size / f, (ox + g.pos.x / f, oy + g.pos.y / f), (1.0, 0.0));
            if !contours.is_empty() {
                part_of(&mut parts, format.color).glyphs.push(contours);
            }
        }
    }
    for line in layout.deco.iter() {
        let r = line.rect;
        let (x0, y0, x1, y1) = (ox + r.min.x / f, oy + r.min.y / f, ox + r.max.x / f, oy + r.max.y / f);
        part_of(&mut parts, line.color).bars.push(polygon(&[(x0, y0), (x1, y0), (x1, y1), (x0, y1)]));
    }
    // Mirrored about the centre of the box, then turned, as the node is drawn.
    let (cx, cy) = n.center();
    let (flip_h, flip_v) = n.flips();
    if flip_h || flip_v || n.rotation != 0.0 {
        for q in parts.iter_mut().flat_map(|p| p.glyphs.iter_mut().flatten().chain(p.bars.iter_mut())).flat_map(|c| c.pts.iter_mut()) {
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
    }
    parts
}

fn bounds(contours: &[Contour]) -> (f32, f32, f32, f32) {
    let mut b = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for q in contours.iter().flat_map(|c| c.pts.iter()) {
        // (Handles can reach past the curve; a box that is a little too large only means two
        // glyphs are looked at together.)
        for (x, y) in [(q.x, q.y), (q.x + q.hin[0], q.y + q.hin[1]), (q.x + q.hout[0], q.y + q.hout[1])] {
            b = (b.0.min(x), b.1.min(y), b.2.max(x), b.3.max(y));
        }
    }
    b
}

/// The contours of glyphs for a Kanvas path, which fills under the even-odd rule: font outlines
/// fill non-zero, so contours that overlap (within a glyph, or between neighbours that touch)
/// are united first, as the SVG import does. Glyphs that touch nothing keep their exact curves.
/// `size`: the font size, which says how fine the result has to be.
pub fn even_odd(glyphs: Vec<Vec<Contour>>, size: f32) -> Vec<Contour> {
    let mut out = Vec::new();
    // Glyphs whose boxes touch are resolved together, a few at a time.
    let mut group: Vec<Contour> = Vec::new();
    let (mut held, mut area) = (0, (0.0, 0.0, 0.0, 0.0));
    let mut flush = |group: &mut Vec<Contour>| {
        if !group.is_empty() {
            out.extend(outline::simplify(group, Rule::NonZero, size / 20.0));
            group.clear();
        }
    };
    for glyph in glyphs {
        let b = bounds(&glyph);
        let touches = held > 0 && b.0 <= area.2 && b.2 >= area.0 && b.1 <= area.3 && b.3 >= area.1;
        if !touches || held >= 8 {
            flush(&mut group);
            (held, area) = (0, b);
        }
        area = (area.0.min(b.0), area.1.min(b.1), area.2.max(b.2), area.3.max(b.3));
        held += 1;
        group.extend(glyph);
    }
    flush(&mut group);
    out
}

// ---------------------------------------------------------------- the document

/// After nodes were copied and given ids of their own (`map`: old id to new): a copied text
/// whose path was copied with it follows the copy of the path.
pub fn relink(n: &mut Node, map: &HashMap<u64, u64>) {
    n.walk_mut(&mut |c| {
        if let Some(new) = c.on_path.and_then(|p| map.get(&p)) {
            c.on_path = Some(*new);
        }
    });
}

/// For nodes that come from somewhere else (the clipboard): a text whose path did not come
/// along is plain text again, since the id it names may be anything here.
pub fn drop_outside_links(nodes: &mut [Node]) {
    let mut ids = std::collections::HashSet::new();
    for n in nodes.iter() {
        n.walk(&mut |c| {
            ids.insert(c.id);
        });
    }
    for n in nodes {
        n.walk_mut(&mut |c| {
            if c.on_path.is_some_and(|p| !ids.contains(&p)) {
                detach(c);
            }
        });
    }
}

/// Plain text again.
pub fn detach(n: &mut Node) {
    (n.on_path, n.path_offset, n.path_side) = (None, 0.0, false);
}

impl Doc {
    /// Put the text `text` on the node `path` (see `can_carry`). The text gives up its own
    /// rotation and flips: where it stands is up to the path from here on.
    pub fn attach_text(&mut self, text: u64, path: u64) -> Result<(), &'static str> {
        if !self.find(path).is_some_and(can_carry) {
            return Err("text can follow a vector path, a line, an ellipse or a rectangle");
        }
        // In an auto layout the frame would place it; on a path the path does.
        let item = self.auto_item(text);
        let Some(n) = self.find_mut(text).filter(|n| n.kind == Kind::Text) else { return Err("only a text layer can be put on a path") };
        (n.on_path, n.path_offset, n.path_side) = (Some(path), 0.0, false);
        (n.rotation, n.flip_h, n.flip_v) = (0.0, false, false);
        n.absolute |= item;
        Ok(())
    }

    /// Texts whose path is gone (deleted, or no longer a shape text can follow) become plain
    /// text again. Result: something changed.
    pub fn drop_lost_paths(&mut self) -> bool {
        let mut wanted: Vec<(u64, u64)> = Vec::new();
        self.walk(|n| {
            if let Some(p) = n.on_path {
                wanted.push((n.id, p));
            }
        });
        let lost: Vec<u64> = wanted.into_iter().filter(|(_, p)| !self.find(*p).is_some_and(can_carry)).map(|(id, _)| id).collect();
        for id in &lost {
            if let Some(n) = self.find_mut(*id) {
                detach(n);
            }
        }
        !lost.is_empty()
    }

    /// The line of every text on a path of this page, by the id of the text (for the export).
    pub fn guides(&self) -> HashMap<u64, std::sync::Arc<Guide>> {
        let mut wanted: Vec<(u64, u64)> = Vec::new();
        self.walk(|n| {
            if let (Kind::Text, Some(p)) = (n.kind, n.on_path) {
                wanted.push((n.id, p));
            }
        });
        let mut made: HashMap<u64, Option<std::sync::Arc<Guide>>> = HashMap::new();
        let mut out = HashMap::new();
        for (text, path) in wanted {
            let guide = made.entry(path).or_insert_with(|| self.find(path).filter(|p| can_carry(p)).and_then(guide_of).map(std::sync::Arc::new));
            if let Some(g) = guide {
                out.insert(text, g.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{PathPoint, Span, Style};

    fn near(a: f32, b: f32, tol: f32) -> bool {
        (a - b).abs() <= tol
    }

    fn near2(a: P, b: P, tol: f32) -> bool {
        near(a.0, b.0, tol) && near(a.1, b.1, tol)
    }

    fn shape(doc: &mut Doc, kind: Kind, parent: Option<u64>, (x, y, w, h): (f32, f32, f32, f32)) -> u64 {
        let id = doc.alloc_id();
        let mut n = Node::new(id, kind, x, y);
        (n.w, n.h) = (w, h);
        if kind == Kind::Text {
            n.text = "Label".into();
        }
        doc.insert(n, parent);
        id
    }

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    #[test]
    fn a_line_gives_point_and_direction_at_a_distance() {
        // A corner: 30 to the right, then 40 down.
        let g = Guide::new(vec![(10.0, 20.0), (40.0, 20.0), (40.0, 60.0)], false).unwrap();
        assert_eq!(g.len(), 70.0);
        assert_eq!(g.at(0.0), Some(((10.0, 20.0), (1.0, 0.0))));
        assert_eq!(g.at(15.0), Some(((25.0, 20.0), (1.0, 0.0))));
        // On the corner the direction is that of the piece that starts there.
        assert_eq!(g.at(30.0), Some(((40.0, 20.0), (0.0, 1.0))));
        assert_eq!(g.at(50.0), Some(((40.0, 40.0), (0.0, 1.0))));
        assert_eq!(g.at(70.0), Some(((40.0, 60.0), (0.0, 1.0))));
        // An open line has nothing before its start or past its end.
        assert_eq!((g.at(-1.0), g.at(71.0)), (None, None));
        // The nearest point: how far away, and how far along.
        assert_eq!(g.nearest((25.0, 23.0)), (3.0, 15.0));
        assert_eq!(g.nearest((45.0, 50.0)), (5.0, 60.0));
        // Walked the other way it starts at the far end.
        let back = g.reversed();
        assert_eq!(back.at(0.0), Some(((40.0, 60.0), (0.0, -1.0))));
        assert_eq!(back.at(55.0), Some(((25.0, 20.0), (-1.0, 0.0))));
        assert!(Guide::new(vec![(1.0, 1.0), (1.0, 1.0)], false).is_none());
    }

    #[test]
    fn a_circle_starts_at_its_top_and_goes_round_clockwise() {
        let mut n = Node::new(1, Kind::Ellipse, 100.0, 50.0);
        (n.w, n.h) = (200.0, 200.0);
        let g = guide_of(&n).unwrap();
        let len = g.len();
        assert!(g.closed && near(len, std::f32::consts::TAU * 100.0, 0.5), "{len}");
        let quarter = |k: f32| g.at(len * 0.25 * k).unwrap();
        // Top, right, bottom, left: the direction is a quarter turn ahead of the radius.
        for (k, at, tan) in [(0.0, (200.0, 50.0), (1.0, 0.0)), (1.0, (300.0, 150.0), (0.0, 1.0)), (2.0, (200.0, 250.0), (-1.0, 0.0)), (3.0, (100.0, 150.0), (0.0, -1.0))] {
            let (p, t) = quarter(k);
            assert!(near2(p, at, 0.2) && near2(t, tan, 0.03), "quarter {k}: {p:?} {t:?}");
        }
        // Every point is on the circle, and the direction is always at a right angle to the radius.
        for i in 0..100 {
            let (p, t) = g.at(len * i as f32 / 100.0).unwrap();
            let r = (p.0 - 200.0, p.1 - 150.0);
            assert!(near(r.0.hypot(r.1), 100.0, 0.05) && near((r.0 * t.0 + r.1 * t.1) / 100.0, 0.0, 0.02));
        }
        // It goes round and round, both ways.
        assert!(near2(g.at(len + 10.0).unwrap().0, g.at(10.0).unwrap().0, 0.01) && near2(g.at(-10.0).unwrap().0, g.at(len - 10.0).unwrap().0, 0.01));
        // The other side: still from the top, the other way round.
        let back = g.reversed();
        let (p, t) = back.at(len * 0.25).unwrap();
        assert!(near2(back.at(0.0).unwrap().0, (200.0, 50.0), 0.01) && near2(p, (100.0, 150.0), 0.2) && near2(t, (0.0, 1.0), 0.03));
        // Turned by a quarter, the top is on the right.
        n.rotation = 90.0;
        assert!(near2(guide_of(&n).unwrap().at(0.0).unwrap().0, (300.0, 150.0), 0.01));
    }

    #[test]
    fn the_other_shapes_give_their_outline_too() {
        let mut line = Node::new(1, Kind::Line, 10.0, 10.0);
        (line.w, line.h) = (30.0, -40.0);
        let g = guide_of(&line).unwrap();
        assert!(!g.closed && g.len() == 50.0 && near2(g.at(25.0).unwrap().0, (25.0, -10.0), 1e-3));
        // A rectangle: from its top left corner along the top; with round corners from where
        // the top edge begins.
        let mut rect = Node::new(2, Kind::Rect, 0.0, 0.0);
        (rect.w, rect.h) = (100.0, 60.0);
        let g = guide_of(&rect).unwrap();
        assert!(g.closed && g.len() == 320.0 && g.at(0.0) == Some(((0.0, 0.0), (1.0, 0.0))) && g.at(130.0) == Some(((100.0, 30.0), (0.0, 1.0))));
        rect.radius = 20.0;
        let g = guide_of(&rect).unwrap();
        assert!(near2(g.at(0.0).unwrap().0, (20.0, 0.0), 1e-3) && near2(g.at(30.0).unwrap().0, (50.0, 0.0), 1e-3));
        assert!(near(g.len(), 320.0 - 160.0 + std::f32::consts::TAU * 20.0, 0.1), "{}", g.len());
        // A path: its main contour, curves and all. What it is made from tells when it changes.
        let mut path = Node::new(3, Kind::Path, 0.0, 0.0);
        (path.w, path.h) = (100.0, 100.0);
        path.points = vec![PathPoint { x: 0.0, y: 1.0, hin: [0.0; 2], hout: [0.0, -0.5523], r: None }, PathPoint { x: 1.0, y: 0.0, hin: [-0.5523, 0.0], hout: [0.0; 2], r: None }];
        let g = guide_of(&path).unwrap();
        assert!(!g.closed && near(g.len(), std::f32::consts::FRAC_PI_2 * 100.0, 0.1), "{}", g.len());
        let key = guide_key(&path);
        path.x += 1.0;
        assert_ne!(guide_key(&path), key);
        path.x -= 1.0;
        path.points[0].hout[1] = -0.4;
        assert_ne!(guide_key(&path), key);
        assert!(guide_of(&Node::new(4, Kind::Frame, 0.0, 0.0)).is_none() && !can_carry(&Node::new(4, Kind::Text, 0.0, 0.0)));
    }

    #[test]
    fn glyphs_are_placed_by_alignment_and_offset() {
        let g = Guide::new(vec![(0.0, 0.0), (100.0, 0.0)], false).unwrap();
        // Three cells of 10: a text 30 wide.
        let cells = [(0.0, 10.0), (10.0, 10.0), (20.0, 10.0)];
        // (To a thousandth: the places come out of sums of single-precision lengths.)
        let xs = |align: u8, offset: f32| -> Vec<Option<f32>> {
            place(&g, &cells, start(g.len(), 30.0, align, offset, false)).iter().map(|s| s.map(|s| (s.at.0 * 1000.0).round() / 1000.0)).collect()
        };
        assert_eq!(xs(0, 0.0), vec![Some(0.0), Some(10.0), Some(20.0)]);
        assert_eq!(xs(1, 0.0), vec![Some(35.0), Some(45.0), Some(55.0)]);
        assert_eq!(xs(2, 0.0), vec![Some(70.0), Some(80.0), Some(90.0)]);
        assert_eq!(xs(1, 5.0), vec![Some(40.0), Some(50.0), Some(60.0)]);
        // What falls off an end of an open line is left out: a glyph is on the line as long
        // as the middle of its advance is.
        assert_eq!(xs(2, 16.0), vec![Some(86.0), None, None]);
        assert_eq!(xs(0, -6.0), vec![None, Some(4.0), Some(14.0)]);
        // The baseline is turned to the line: down a vertical one, glyphs run downward.
        let down = Guide::new(vec![(5.0, 0.0), (5.0, 100.0)], false).unwrap();
        let spot = place(&down, &cells, 0.0)[1].unwrap();
        assert_eq!((spot.at, spot.tan), ((5.0, 10.0), (0.0, 1.0)));
        // Round a corner each glyph turns about the middle of its advance.
        let bend = Guide::new(vec![(0.0, 0.0), (15.0, 0.0), (15.0, 50.0)], false).unwrap();
        let spots = place(&bend, &cells, 0.0);
        assert_eq!(spots[1].map(|s| (s.at, s.tan)), Some(((15.0, -5.0), (0.0, 1.0))));
        // A closed line has no ends: the text is aligned on its starting point and wraps round.
        let square = Guide::new(vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)], true).unwrap();
        assert_eq!((start(40.0, 30.0, 0, 0.0, true), start(40.0, 30.0, 1, 0.0, true), start(40.0, 30.0, 2, 3.0, true)), (0.0, -15.0, -27.0));
        let round = place(&square, &[(0.0, 4.0), (4.0, 4.0)], start(40.0, 8.0, 1, 0.0, true));
        assert_eq!(round[0].map(|s| s.tan), Some((0.0, -1.0)));
        assert_eq!(round[1].map(|s| (s.at, s.tan)), Some(((0.0, 0.0), (1.0, 0.0))));
        // Longer than the line: what would come round to the beginning again is left out.
        let long: Vec<(f32, f32)> = (0..6).map(|i| (i as f32 * 10.0, 10.0)).collect();
        assert_eq!(place(&square, &long, 0.0).iter().filter(|s| s.is_some()).count(), 4);
    }

    #[test]
    fn a_laid_line_reports_its_box_and_bends_its_underline() {
        let cell = |x: f32| Cell { chr: 'a', family: FontFamily::Proportional, size: 10.0, color: Color32::BLACK, x, adv: 10.0, asc: 8.0, desc: 2.0 };
        let line = Line { cells: vec![cell(0.0), cell(10.0)], bars: vec![Bar { x0: 0.0, x1: 20.0, y0: 1.0, y1: 2.0, color: Color32::RED }], width: 20.0 };
        let flat = Guide::new(vec![(100.0, 50.0), (200.0, 50.0)], false).unwrap();
        let laid = lay(&line, &flat, 0, 0.0);
        // Above the line by the ascent, below it by the descent.
        assert_eq!((laid.glyphs.len(), laid.bbox, laid.tan), (2, Some((100.0, 42.0, 120.0, 52.0)), (1.0, 0.0)));
        let (poly, color) = &laid.bars[0];
        let ys: Vec<f32> = poly.iter().map(|p| p.1).collect();
        assert!(*color == Color32::RED && ys.iter().all(|y| *y == 51.0 || *y == 52.0) && poly.iter().all(|p| p.0 >= 100.0 && p.0 <= 120.0));
        // Right aligned and walked the other way: the text ends at the start of the line,
        // upside down under it.
        let laid = lay(&line, &flat.reversed(), 2, 0.0);
        assert_eq!((laid.bbox, laid.tan), (Some((100.0, 48.0, 120.0, 58.0)), (-1.0, 0.0)));
        // Moved off the end: nothing is left, and there is no box.
        let gone = lay(&line, &flat, 0, 500.0);
        assert!(gone.glyphs.is_empty() && gone.bbox.is_none() && gone.bars.is_empty());
        // Half off: the underline stops where the line does.
        let half = lay(&line, &flat, 0, 90.0);
        assert!(half.glyphs.len() == 1 && half.bars[0].0.iter().all(|p| p.0 >= 190.0 && p.0 <= 200.0));
    }

    #[test]
    fn copies_keep_their_text_on_the_copied_path() {
        let mut doc = Doc::default();
        let frame = shape(&mut doc, Kind::Frame, None, (0.0, 0.0, 300.0, 300.0));
        let path = shape(&mut doc, Kind::Ellipse, Some(frame), (10.0, 10.0, 100.0, 100.0));
        let text = shape(&mut doc, Kind::Text, Some(frame), (10.0, 10.0, 50.0, 20.0));
        let loose = shape(&mut doc, Kind::Line, None, (400.0, 0.0, 100.0, 0.0));
        let other = shape(&mut doc, Kind::Text, None, (400.0, 0.0, 50.0, 20.0));
        assert_eq!(doc.attach_text(text, path), Ok(()));
        assert_eq!(doc.attach_text(other, loose), Ok(()));
        // Only a text goes on a path, and only on a shape with an outline to follow.
        assert!(doc.attach_text(path, loose).is_err() && doc.attach_text(text, frame).is_err() && doc.attach_text(text, other).is_err());
        assert_eq!(doc.find(text).unwrap().on_path, Some(path));

        // A frame copied with both in it: the copy of the text is on the copy of the path.
        let mut copy = doc.find(frame).unwrap().clone();
        doc.reassign_ids(&mut copy);
        let (p2, t2) = (copy.children[0].id, &copy.children[1]);
        assert!(p2 != path && t2.id != text && t2.on_path == Some(p2));
        // Two top-level nodes copied together: the same, once both have their ids.
        let mut pair = vec![doc.find(loose).unwrap().clone(), doc.find(other).unwrap().clone()];
        let mut map = HashMap::new();
        for n in &mut pair {
            doc.reassign_ids_into(n, &mut map);
        }
        assert_eq!(pair[1].on_path, Some(loose));
        pair.iter_mut().for_each(|n| relink(n, &map));
        assert!(pair[0].id != loose && pair[1].on_path == Some(pair[0].id));
        // The text copied alone stays on the path it was on.
        let mut alone = doc.find(other).unwrap().clone();
        doc.reassign_ids(&mut alone);
        assert_eq!(alone.on_path, Some(loose));
        // Unless it comes from somewhere else (the clipboard): then that id means nothing here.
        let mut pasted = vec![doc.find(other).unwrap().clone()];
        pasted[0].path_offset = 12.0;
        drop_outside_links(&mut pasted);
        assert_eq!((pasted[0].on_path, pasted[0].path_offset), (None, 0.0));
        let mut both = vec![doc.find(loose).unwrap().clone(), doc.find(other).unwrap().clone()];
        drop_outside_links(&mut both);
        assert_eq!(both[1].on_path, Some(loose));
        // A duplicated page holds copies of both.
        doc.ensure_pages();
        doc.duplicate_page(0);
        let on: Vec<(u64, Option<u64>)> = doc.nodes.iter().flat_map(|n| std::iter::once(&**n).chain(n.children.iter())).filter(|n| n.kind == Kind::Text).map(|n| (n.id, n.on_path)).collect();
        assert!(on.len() == 2 && on.iter().all(|(id, p)| *id != text && *id != other && p.is_some_and(|p| p != path && p != loose && doc.find(p).is_some_and(can_carry))), "{on:?}");
    }

    #[test]
    fn a_text_whose_path_is_gone_is_plain_text_again() {
        let mut doc = Doc::default();
        let path = shape(&mut doc, Kind::Line, None, (0.0, 0.0, 100.0, 100.0));
        let kept = shape(&mut doc, Kind::Rect, None, (0.0, 0.0, 100.0, 100.0));
        let (a, b) = (shape(&mut doc, Kind::Text, None, (0.0, 0.0, 50.0, 20.0)), shape(&mut doc, Kind::Text, None, (0.0, 0.0, 50.0, 20.0)));
        assert!(doc.attach_text(a, path).is_ok() && doc.attach_text(b, kept).is_ok());
        doc.find_mut(a).unwrap().path_offset = 30.0;
        // Nothing is lost: nothing changes.
        assert!(!doc.drop_lost_paths());
        assert_eq!(doc.guides().len(), 2);
        doc.remove(path);
        assert!(doc.drop_lost_paths());
        let n = doc.find(a).unwrap();
        assert_eq!((n.on_path, n.path_offset, n.path_side), (None, 0.0, false));
        assert_eq!(doc.find(b).unwrap().on_path, Some(kept));
        assert!(!doc.drop_lost_paths() && doc.guides().len() == 1);
    }

    #[test]
    fn an_instance_has_its_text_on_its_own_copy_of_the_path() {
        let mut doc = Doc::default();
        let frame = shape(&mut doc, Kind::Frame, None, (0.0, 0.0, 200.0, 200.0));
        let path = shape(&mut doc, Kind::Ellipse, Some(frame), (20.0, 20.0, 100.0, 100.0));
        let text = shape(&mut doc, Kind::Text, Some(frame), (20.0, 20.0, 50.0, 20.0));
        assert!(doc.attach_text(text, path).is_ok());
        assert_eq!(doc.make_component(&[frame]), Ok(frame));
        let inst = doc.instantiate(frame).unwrap();
        let part = |n: &Node, source: u64| n.children.iter().find(|c| c.source == Some(source)).unwrap().clone();
        assert_eq!(part(&inst, text).on_path, Some(part(&inst, path).id));
        assert_ne!(part(&inst, path).id, path);
        let id = inst.id;
        doc.insert(inst, None);
        // The main changes: taken off, put back with an offset. The instance follows each time.
        let edit = |doc: &mut Doc, f: &dyn Fn(&mut Node)| {
            let pre = doc.clone();
            f(doc.find_mut(text).unwrap());
            doc.sync_components(&pre);
            let inst = doc.find(id).unwrap();
            (part(inst, text), part(inst, path).id)
        };
        let (t, _) = edit(&mut doc, &|n| detach(n));
        assert_eq!(t.on_path, None);
        let (t, p) = edit(&mut doc, &|n| (n.on_path, n.path_offset, n.path_side) = (Some(path), 25.0, true));
        assert_eq!((t.on_path, t.path_offset, t.path_side), (Some(p), 25.0, true));
    }

    #[test]
    fn text_becomes_contours_that_fill_the_same() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut doc = Doc::default();
        let id = shape(&mut doc, Kind::Text, None, (100.0, 50.0, 200.0, 60.0));
        {
            let n = doc.find_mut(id).unwrap();
            (n.text, n.font_size, n.fill, n.name) = ("oil ob".into(), 40.0, Some([10, 20, 30, 255]), "Word".into());
        }
        let n = doc.find(id).unwrap().clone();
        let parts = text_parts(&ctx, &n, None, &mut fonts);
        assert_eq!(parts.len(), 1);
        // Five glyphs with an outline (the space has none); "o" and "b" have a hole, "i" a dot.
        let counts: Vec<usize> = parts[0].glyphs.iter().map(|g| g.len()).collect();
        assert_eq!(counts, vec![2, 2, 1, 2, 2]);
        let all: Vec<Contour> = parts[0].glyphs.iter().flatten().cloned().collect();
        let b = bounds(&all);
        assert!(b.0 >= 99.0 && b.2 <= 100.0 + n.w.max(200.0) && b.1 >= 50.0 && b.3 <= 50.0 + 60.0, "{b:?}");
        // Under the even-odd rule of a Kanvas path the holes are holes and nothing else is lost.
        let simple = even_odd(parts[0].glyphs.clone(), 40.0);
        assert_eq!(simple.len(), 9);
        let ink = outline::area(&simple);
        assert!(ink > 600.0 && ink < 2400.0, "{ink}");

        // In the document: one path in the place of the text, under its id, in its colour.
        let made = doc.text_to_paths(id, parts);
        assert_eq!(made, vec![id]);
        let path = doc.find(id).unwrap();
        assert_eq!((path.kind, path.fill, path.name.as_str(), path.group, path.holes.len()), (Kind::Path, Some([10, 20, 30, 255]), "Word outline", 0, 8));
        assert!(near(path.x, b.0, 0.6) && near(path.y + path.h, b.3, 0.6));

        // Two colours and an underline: a path per colour and one for the line, in one group.
        // Turned by a quarter, the outlines stand upright.
        let id = shape(&mut doc, Kind::Text, None, (0.0, 0.0, 200.0, 60.0));
        {
            let n = doc.find_mut(id).unwrap();
            (n.text, n.font_size, n.fill, n.rotation, n.w, n.h) = ("ab cd".into(), 40.0, Some([0, 0, 0, 255]), 90.0, 100.0, 48.0);
            n.spans = vec![Span { start: 3, end: 5, style: Style { font: n.font.clone(), weight: 400, italic: false, size: 40.0, fill: [255, 0, 0, 255], underline: true, strike: false } }];
        }
        let n = doc.find(id).unwrap().clone();
        let parts = text_parts(&ctx, &n, None, &mut fonts);
        assert_eq!(parts.iter().map(|p| (p.glyphs.len(), p.bars.len())).collect::<Vec<_>>(), vec![(2, 0), (2, 1)]);
        let made = doc.text_to_paths(id, parts);
        assert_eq!(made.len(), 3);
        let nodes: Vec<&Node> = made.iter().map(|id| doc.find(*id).unwrap()).collect();
        assert!(nodes[0].group != 0 && nodes.iter().all(|p| p.kind == Kind::Path && p.group == nodes[0].group && p.rotation == 0.0));
        assert_eq!(nodes.iter().map(|p| p.fill).collect::<Vec<_>>(), vec![Some([0, 0, 0, 255]), Some([255, 0, 0, 255]), Some([255, 0, 0, 255])]);
        assert!(nodes.iter().all(|p| p.h > p.w * 0.9) && nodes[2].h > nodes[2].w * 5.0, "{:?}", nodes.iter().map(|p| (p.w, p.h)).collect::<Vec<_>>());
        assert_eq!(doc.nodes.iter().map(|n| n.id).collect::<Vec<_>>(), vec![1, made[0], made[1], made[2]]);
    }

    #[test]
    fn text_on_a_path_is_outlined_and_exported_along_it() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut doc = Doc::default();
        let frame = shape(&mut doc, Kind::Frame, None, (0.0, 0.0, 200.0, 200.0));
        doc.find_mut(frame).unwrap().fill = Some([255, 255, 255, 255]);
        // A line straight down the middle, and a text on it.
        let rail = shape(&mut doc, Kind::Line, Some(frame), (100.0, 20.0, 0.0, 160.0));
        let text = shape(&mut doc, Kind::Text, Some(frame), (0.0, 0.0, 50.0, 20.0));
        {
            let n = doc.find_mut(text).unwrap();
            (n.text, n.font_size, n.fill) = ("HHHH\nHH".into(), 24.0, Some([0, 0, 0, 255]));
        }
        assert!(doc.attach_text(text, rail).is_ok());
        let n = doc.find(text).unwrap().clone();
        let guide = guide_of(doc.find(rail).unwrap()).unwrap();
        // One line (the line break counts as a space): six letters down the rail, their feet
        // on it and their heads to the right of it.
        let laid = laid(&ctx, &n, &guide, &mut fonts);
        assert_eq!(laid.glyphs.iter().filter(|g| !g.chr.is_whitespace()).count(), 6);
        assert!(laid.glyphs.iter().all(|g| g.tan == (0.0, 1.0) && g.at.0 == 100.0));
        let b = laid.bbox.unwrap();
        assert!(b.0 < 100.0 && b.2 > 115.0 && b.1 >= 20.0 && b.3 - b.1 > 80.0, "{b:?}");
        let parts = path_parts(&ctx, &n, &guide, &mut fonts);
        let ink = bounds(&parts[0].glyphs.iter().flatten().cloned().collect::<Vec<_>>());
        assert!(near(ink.0, 100.0, 0.5) && ink.2 > 112.0 && ink.2 < 124.0 && ink.3 - ink.1 > 80.0, "{ink:?}");
        // Centred, it sits around the middle of the rail; flipped, on the other side of it.
        let mut centred = n.clone();
        (centred.align, centred.path_side) = (1, true);
        let ink2 = bounds(&path_parts(&ctx, &centred, &guide, &mut fonts)[0].glyphs.iter().flatten().cloned().collect::<Vec<_>>());
        assert!(near(ink2.2, 100.0, 0.5) && ink2.0 < 88.0 && near((ink2.1 + ink2.3) * 0.5, 100.0, 3.0), "{ink2:?}");

        // The export draws it there too: dark pixels right of the rail, none left of it.
        let (svg, w, h) = crate::export::to_svg(&doc, Some(frame), &mut fonts, &ctx).unwrap();
        let (png, _, _) = crate::export::render_png(&svg, w, h, 1.0).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgb8();
        let dark = |x0: u32, x1: u32| (x0..x1).flat_map(|x| (20..180u32).map(move |y| (x, y))).filter(|&(x, y)| img.get_pixel(x, y)[0] < 80).count();
        assert!(dark(102, 125) > 300 && dark(60, 98) == 0, "{} {}", dark(102, 125), dark(60, 98));
        // Without its path the same text is exported as plain text at its own place.
        doc.remove(rail);
        assert!(doc.drop_lost_paths());
        let (svg, w, h) = crate::export::to_svg(&doc, Some(frame), &mut fonts, &ctx).unwrap();
        let (png, _, _) = crate::export::render_png(&svg, w, h, 1.0).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgb8();
        let dark = (0..80u32).flat_map(|x| (0..60u32).map(move |y| (x, y))).filter(|&(x, y)| img.get_pixel(x, y)[0] < 80).count();
        assert!(dark > 200, "{dark}");
    }
}
