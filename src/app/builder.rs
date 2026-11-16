//! Shape Builder: bentuk-bentuk yang bertumpuk dipecah jadi region; tarik melintasi region untuk
//! menggabungkannya jadi satu bentuk, Alt-klik/tarik untuk menghapusnya.
//!
//! Semua operasi boolean berjalan di kisi bilangan bulat yang sama (lewat `i_overlay`), sehingga
//! region yang bersebelahan berbagi tepi yang persis sama dan bisa digabung tanpa celah.
//! Hasilnya diubah kembali jadi kurva bezier (bukan poligon bersudut banyak).

use super::pen::{set_geo, Geo};
use super::render::PenPt;
use super::vtext;
use super::*;
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay::Overlay;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::i_float::int::point::IntPoint;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

type Pt = IntPoint<i32>;
type Contour = Vec<Pt>;
/// Satu bentuk terhubung: kontur luar + lubang-lubangnya.
type Shape = Vec<Contour>;
type P2 = (f64, f64);

const MAX_SOURCES: usize = 48;
/// Sudut belok (radian) yang dianggap sudut tajam saat mengubah poligon kembali jadi kurva.
const CORNER: f64 = 0.42;

#[derive(Clone, Copy, PartialEq)]
enum State {
    Free,
    Merged(usize),
    Deleted,
}

struct Cell {
    shape: Shape,
    bbox: [i32; 4],
    /// Indeks sumber yang menutupi region ini; `owner` = yang paling atas (yang terlihat).
    members: Vec<usize>,
    owner: usize,
    state: State,
}

struct Sweep {
    cells: Vec<usize>,
    delete: bool,
    trail: Vec<Pos2>,
}

pub struct Session {
    parent: Option<u64>,
    /// Bentuk sumber, urutan bawah → atas; dipakai sebagai cetakan gaya untuk hasilnya.
    srcs: Vec<Node>,
    cells: Vec<Cell>,
    /// Per grup gabungan: indeks sumber yang gayanya dipakai.
    groups: Vec<usize>,
    /// Node di dokumen yang dikelola sesi ini → indeks sumber (slot urutan tumpukan).
    owned: HashMap<u64, usize>,
    rev: u64,
    origin: P2,
    scale: f64,
    hover: Option<usize>,
    sweep: Option<Sweep>,
}

fn eligible(n: &Node) -> bool {
    n.visible
        && n.w.abs() > 0.5
        && n.h.abs() > 0.5
        && match n.kind {
            Kind::Rect | Kind::Ellipse => true,
            Kind::Path => n.closed && n.points.len() >= 3,
            _ => false,
        }
}

fn overlaps(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

/// Kontur bentuk dalam koordinat dunia (rotasi diterapkan).
fn source_contours(n: &Node) -> Vec<Vec<P2>> {
    let (x0, y0, x1, y1) = n.bounds();
    let (w, h) = ((x1 - x0) as f64, (y1 - y0) as f64);
    let (x0, y0) = (x0 as f64, y0 as f64);
    let mut out: Vec<Vec<P2>> = match n.kind {
        Kind::Ellipse => {
            let (rx, ry) = (w * 0.5, h * 0.5);
            // Elips pipih butuh lebih banyak segmen supaya ujungnya tetap mulus.
            let ratio = (rx / ry).max(ry / rx);
            let steps = ((rx.max(ry).sqrt() * 6.0).max(31.0 * ratio).clamp(48.0, 720.0) / 4.0).ceil() as usize * 4;
            let ring = (0..steps)
                .map(|i| {
                    let a = i as f64 / steps as f64 * std::f64::consts::TAU;
                    (x0 + rx + rx * a.cos(), y0 + ry + ry * a.sin())
                })
                .collect();
            vec![ring]
        }
        Kind::Rect => {
            let r = (n.radius as f64).min(w * 0.5).min(h * 0.5).max(0.0);
            if r < 0.05 {
                vec![vec![(x0, y0), (x0 + w, y0), (x0 + w, y0 + h), (x0, y0 + h)]]
            } else {
                let k = ((r.sqrt() * 3.0).ceil() as usize).clamp(8, 32);
                let mut ring = Vec::with_capacity(4 * (k + 1));
                let centers = [(x0 + w - r, y0 + r, -90.0f64), (x0 + w - r, y0 + h - r, 0.0), (x0 + r, y0 + h - r, 90.0), (x0 + r, y0 + r, 180.0)];
                for (cx, cy, start) in centers {
                    for i in 0..=k {
                        let a = (start + 90.0 * i as f64 / k as f64).to_radians();
                        ring.push((cx + r * a.cos(), cy + r * a.sin()));
                    }
                }
                vec![ring]
            }
        }
        _ => {
            let segs = (((w + h).sqrt() * 1.5) as usize).clamp(20, 96);
            n.contours(segs).into_iter().map(|c| c.into_iter().map(|(x, y)| (x as f64, y as f64)).collect()).collect()
        }
    };
    if n.rotation != 0.0 {
        let (cx, cy) = n.center();
        let (s, c) = (n.rotation as f64).to_radians().sin_cos();
        for p in out.iter_mut().flatten() {
            let (dx, dy) = (p.0 - cx as f64, p.1 - cy as f64);
            *p = (cx as f64 + dx * c - dy * s, cy as f64 + dx * s + dy * c);
        }
    }
    out
}

fn flat(shapes: &[Shape]) -> Vec<Contour> {
    shapes.iter().flatten().cloned().collect()
}

/// Operasi boolean antara dua kumpulan bentuk (orientasi kontur konsisten → aturan non-zero).
fn op(a: &[Shape], b: &[Shape], rule: OverlayRule) -> Vec<Shape> {
    if a.is_empty() && b.is_empty() {
        return Vec::new();
    }
    Overlay::<i32>::from_subj_and_clip(&flat(a), &flat(b)).overlay(rule, FillRule::NonZero)
}

fn area(s: &Shape) -> f64 {
    let mut sum = 0.0;
    for c in s {
        let m = c.len();
        for i in 0..m {
            let (a, b) = (c[i], c[(i + 1) % m]);
            sum += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
        }
    }
    (sum * 0.5).abs()
}

fn bbox(s: &Shape) -> [i32; 4] {
    let mut b = [i32::MAX, i32::MAX, i32::MIN, i32::MIN];
    for p in s.iter().flatten() {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    b
}

fn inside(s: &Shape, x: f64, y: f64) -> bool {
    let mut hit = false;
    for c in s {
        let m = c.len();
        if m == 0 {
            continue;
        }
        let mut j = m - 1;
        for i in 0..m {
            let (ax, ay, bx, by) = (c[i].x as f64, c[i].y as f64, c[j].x as f64, c[j].y as f64);
            if (ay > y) != (by > y) && x < (bx - ax) * (y - ay) / (by - ay) + ax {
                hit = !hit;
            }
            j = i;
        }
    }
    hit
}

impl Session {
    fn to_world(&self, p: Pt) -> P2 {
        (p.x as f64 / self.scale + self.origin.0, p.y as f64 / self.scale + self.origin.1)
    }

    fn cell_at(&self, wp: Pos2) -> Option<usize> {
        let (x, y) = ((wp.x as f64 - self.origin.0) * self.scale, (wp.y as f64 - self.origin.1) * self.scale);
        self.cells.iter().position(|c| {
            c.state != State::Deleted
                && x >= c.bbox[0] as f64
                && x <= c.bbox[2] as f64
                && y >= c.bbox[1] as f64
                && y <= c.bbox[3] as f64
                && inside(&c.shape, x, y)
        })
    }

    /// Region yang dipilih + seluruh anggota grup gabungan yang tersentuh, tanpa duplikat.
    fn expand(&self, picked: &[usize]) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for &i in picked {
            match self.cells[i].state {
                State::Merged(g) => {
                    for (j, c) in self.cells.iter().enumerate() {
                        if c.state == State::Merged(g) && !out.contains(&j) {
                            out.push(j);
                        }
                    }
                }
                State::Free if !out.contains(&i) => out.push(i),
                _ => {}
            }
        }
        out
    }
}

/// Bangun sesi dari daftar saudara (urutan bawah → atas) yang sudah dipilih.
fn build(parent: Option<u64>, srcs: Vec<Node>, rev: u64) -> Option<Session> {
    let rings: Vec<Vec<Vec<P2>>> = srcs.iter().map(source_contours).collect();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in rings.iter().flatten().flatten() {
        if !p.0.is_finite() || !p.1.is_finite() {
            return None;
        }
        (x0, y0, x1, y1) = (x0.min(p.0), y0.min(p.1), x1.max(p.0), y1.max(p.1));
    }
    if x0 > x1 {
        return None;
    }
    let origin = ((x0 + x1) * 0.5, (y0 + y1) * 0.5);
    let extent = ((x1 - x0).max(y1 - y0) * 0.5).max(1.0);
    // Kisi tetap untuk semua operasi: pangkat dua terbesar yang masih muat di rentang aman.
    let mut scale = 1024.0;
    while extent * scale > (1u32 << 27) as f64 && scale > 1.0 / 1024.0 {
        scale *= 0.5;
    }
    let to_int = |p: &P2| Pt::new(((p.0 - origin.0) * scale).round() as i32, ((p.1 - origin.1) * scale).round() as i32);
    // Luas minimum region (0.2 satuan dunia persegi): serpihan numerik dibuang.
    let min_area = 0.2 * scale * scale;

    let mut cells: Vec<(Shape, Vec<usize>)> = Vec::new();
    let mut covered: Vec<Shape> = Vec::new();
    for (k, (ring, node)) in rings.iter().zip(&srcs).enumerate() {
        let contours: Vec<Contour> = ring.iter().map(|c| c.iter().map(to_int).collect()).collect();
        // Path satu kontur diisi non-zero (seperti saat digambar); dengan lubang: even-odd.
        let rule = if node.holes.is_empty() { FillRule::NonZero } else { FillRule::EvenOdd };
        let s: Vec<Shape> = Overlay::<i32>::from_subj(&contours).overlay(OverlayRule::Subject, rule);
        if s.is_empty() {
            continue;
        }
        let sb = s.iter().map(bbox).fold([i32::MAX, i32::MAX, i32::MIN, i32::MIN], |a, b| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]);
        let mut next = Vec::with_capacity(cells.len() + 4);
        for (shape, members) in cells.drain(..) {
            let cb = bbox(&shape);
            if cb[0] > sb[2] || sb[0] > cb[2] || cb[1] > sb[3] || sb[1] > cb[3] {
                next.push((shape, members));
                continue;
            }
            let one = [shape];
            for part in op(&one, &s, OverlayRule::Intersect) {
                let mut m = members.clone();
                m.push(k);
                next.push((part, m));
            }
            for part in op(&one, &s, OverlayRule::Difference) {
                next.push((part, members.clone()));
            }
        }
        for part in op(&s, &covered, OverlayRule::Difference) {
            next.push((part, vec![k]));
        }
        covered = op(&covered, &s, OverlayRule::Union);
        cells = next;
    }
    let cells: Vec<Cell> = cells
        .into_iter()
        .filter(|(s, _)| area(s) >= min_area)
        .map(|(shape, members)| Cell { bbox: bbox(&shape), owner: *members.last().unwrap_or(&0), shape, members, state: State::Free })
        .collect();
    if cells.is_empty() {
        return None;
    }
    let owned = srcs.iter().enumerate().map(|(k, n)| (n.id, k)).collect();
    Some(Session { parent, srcs, cells, groups: Vec::new(), owned, rev, origin, scale, hover: None, sweep: None })
}

/// Ganti node milik sesi di satu daftar saudara: node `keep` dibiarkan, sisanya diganti isi
/// `fresh[slot]` di posisi node pertama slot itu (urutan tumpukan terjaga).
fn rebuild<T: AsRef<Node>>(v: &mut Vec<T>, owned: &HashMap<u64, usize>, keep: &HashSet<u64>, fresh: &mut [Vec<Node>], wrap: impl Fn(Node) -> T) {
    let old = std::mem::take(v);
    let mut done = vec![false; fresh.len()];
    for t in old {
        let id = t.as_ref().id;
        match owned.get(&id) {
            Some(_) if keep.contains(&id) => v.push(t),
            Some(&slot) => {
                if !done[slot] {
                    done[slot] = true;
                    v.extend(fresh[slot].drain(..).map(&wrap));
                }
            }
            None => v.push(t),
        }
    }
    for list in fresh.iter_mut() {
        v.extend(list.drain(..).map(&wrap));
    }
}

impl App {
    /// Sumber sesi baru: seleksi (kalau ≥ 2 bentuk), atau bentuk di bawah kursor beserta
    /// semua bentuk saudara yang bertumpuk dengannya.
    fn builder_start(&mut self, seed: Option<u64>) {
        let picked: Vec<u64> = match seed {
            Some(id) => vec![id],
            None => self.sel.iter().copied().filter(|id| self.doc.find(*id).is_some_and(eligible)).collect(),
        };
        let Some(&first) = picked.first() else { return };
        if seed.is_none() && picked.len() < 2 {
            return;
        }
        let parent = self.doc.parent_of(first);
        let sibs: Vec<&Node> = match parent {
            None => self.doc.nodes.iter().map(|n| n.as_ref()).collect(),
            Some(pid) => match self.doc.find(pid) {
                Some(f) => f.children.iter().collect(),
                None => return,
            },
        };
        let mut chosen: Vec<bool> = sibs.iter().map(|n| eligible(n) && picked.contains(&n.id)).collect();
        if seed.is_some() {
            // Perluas ke semua bentuk yang kotak pembatasnya saling bertumpuk (berantai).
            loop {
                let mut grew = false;
                for i in 0..sibs.len() {
                    if chosen[i] || !eligible(sibs[i]) {
                        continue;
                    }
                    let b = sibs[i].aabb();
                    if (0..sibs.len()).any(|j| chosen[j] && overlaps(b, sibs[j].aabb())) {
                        chosen[i] = true;
                        grew = true;
                    }
                }
                if !grew {
                    break;
                }
            }
        }
        let srcs: Vec<Node> = sibs.iter().zip(&chosen).filter(|(_, c)| **c).map(|(n, _)| (*n).clone()).take(MAX_SOURCES).collect();
        if srcs.is_empty() {
            return;
        }
        self.ext.builder = build(parent, srcs, self.ext.rev);
        if let Some(s) = &self.ext.builder {
            self.sel = s.owned.keys().copied().collect();
        }
    }

    /// Interaksi pointer Shape Builder.
    pub(super) fn builder_pointer(&mut self, wp: Pos2, inp: &Inp, hovered: bool) {
        // Dokumen berubah di luar sesi (undo, panel, …): sesi tidak berlaku lagi.
        if self.ext.builder.as_ref().is_some_and(|s| s.rev != self.ext.rev) {
            self.ext.builder = None;
        }
        let sweeping = self.ext.builder.as_ref().is_some_and(|s| s.sweep.is_some());
        if !sweeping && hovered {
            let in_cell = self.ext.builder.as_ref().is_some_and(|s| s.cell_at(wp).is_some());
            if !in_cell {
                if self.ext.builder.is_none() {
                    self.builder_start(None);
                }
                let under = self.doc.hit(wp.x, wp.y, 0.0).filter(|id| self.doc.find(*id).is_some_and(eligible));
                if let Some(id) = under {
                    if !self.ext.builder.as_ref().is_some_and(|s| s.owned.contains_key(&id)) {
                        self.builder_start(Some(id));
                    }
                }
            }
        }
        let z = self.view.zoom;
        let Some(s) = self.ext.builder.as_mut() else { return };
        let cell = s.cell_at(wp);
        let mut finished = None;
        match &mut s.sweep {
            None => {
                s.hover = cell.filter(|_| hovered);
                if let (true, true, Some(c)) = (inp.pressed, hovered, cell) {
                    s.sweep = Some(Sweep { cells: vec![c], delete: inp.mods.alt, trail: vec![wp] });
                }
            }
            Some(sw) if inp.down => {
                // Semua region yang dilewati garis tarikan ikut terpilih.
                let last = *sw.trail.last().unwrap_or(&wp);
                let steps = (((wp - last).length() * z / 3.0).ceil() as usize).clamp(1, 96);
                let mut found = Vec::new();
                for i in 1..=steps {
                    let q = last + (wp - last) * (i as f32 / steps as f32);
                    found.push(q);
                }
                if (wp - last).length() * z > 1.0 {
                    sw.trail.push(wp);
                }
                let (mut cells, delete, trail) = (std::mem::take(&mut sw.cells), sw.delete, std::mem::take(&mut sw.trail));
                for q in found {
                    if let Some(c) = s.cell_at(q) {
                        if !cells.contains(&c) {
                            cells.push(c);
                        }
                    }
                }
                s.sweep = Some(Sweep { cells, delete, trail });
            }
            Some(_) => finished = s.sweep.take(),
        }
        if let Some(sw) = finished {
            self.builder_apply(&sw.cells, sw.delete);
        }
    }

    /// Gabungkan (atau hapus) region terpilih, lalu tulis ulang bentuk-bentuk yang terdampak.
    fn builder_apply(&mut self, picked: &[usize], delete: bool) {
        let Some(mut s) = self.ext.builder.take() else { return };
        let all = s.expand(picked);
        let Some(&first) = all.first() else {
            self.ext.builder = Some(s);
            return;
        };
        if !delete {
            // Tidak ada yang berubah: satu grup yang sudah jadi, atau satu bentuk utuh tanpa tumpukan.
            let same_group = matches!(s.cells[first].state, State::Merged(g) if all.iter().all(|i| s.cells[*i].state == State::Merged(g)));
            let k = s.cells[first].owner;
            let whole = all.len() == 1 && s.cells[first].members.len() == 1 && s.cells.iter().filter(|c| c.members.contains(&k)).count() == 1;
            if same_group || whole {
                self.ext.builder = Some(s);
                return;
            }
        }
        if delete {
            for &i in &all {
                s.cells[i].state = State::Deleted;
            }
        } else {
            // Gaya hasil mengikuti region tempat tarikan dimulai.
            let start = picked.first().copied().unwrap_or(first);
            let g = match s.cells[start].state {
                State::Merged(g) => g,
                _ => {
                    s.groups.push(s.cells[start].owner);
                    s.groups.len() - 1
                }
            };
            for &i in &all {
                s.cells[i].state = State::Merged(g);
            }
        }

        // Sumber yang salah satu regionnya sudah digabung/dihapus tidak utuh lagi.
        let count = s.srcs.len();
        let mut broken = vec![false; count];
        for c in s.cells.iter().filter(|c| c.state != State::Free) {
            for &k in &c.members {
                broken[k] = true;
            }
        }
        let pre = self.doc.clone();
        let mut fresh: Vec<Vec<Node>> = vec![Vec::new(); count];
        let emit = |doc: &mut Doc, want: State, owner: Option<usize>, slot: usize, fresh: &mut Vec<Vec<Node>>| {
            let parts: Vec<Shape> =
                s.cells.iter().filter(|c| c.state == want && owner.map_or(true, |k| c.owner == k)).map(|c| c.shape.clone()).collect();
            if parts.is_empty() {
                return;
            }
            let merged: Vec<Shape> = Overlay::<i32>::from_subj(&flat(&parts)).overlay(OverlayRule::Subject, FillRule::NonZero);
            for shape in merged {
                let cs: Vec<Vec<PenPt>> = shape
                    .iter()
                    .map(|c| fit_closed(&c.iter().map(|p| s.to_world(*p)).collect::<Vec<_>>()))
                    .filter(|c| c.len() >= 2)
                    .collect();
                if cs.is_empty() {
                    continue;
                }
                let tpl = &s.srcs[slot];
                let mut n = tpl.clone();
                n.id = doc.alloc_id();
                if tpl.kind != Kind::Path {
                    n.name = format!("Vector {}", n.id);
                }
                n.kind = Kind::Path;
                n.radius = 0.0;
                n.children.clear();
                set_geo(&mut n, &Geo { cs, closed: true });
                fresh[slot].push(n);
            }
        };
        for k in 0..count {
            if broken[k] {
                emit(&mut self.doc, State::Free, Some(k), k, &mut fresh);
            }
        }
        for g in 0..s.groups.len() {
            let slot = s.groups[g];
            emit(&mut self.doc, State::Merged(g), None, slot, &mut fresh);
        }
        let keep: HashSet<u64> = (0..count).filter(|k| !broken[*k]).map(|k| s.srcs[k].id).filter(|id| s.owned.contains_key(id)).collect();
        let mut owned: HashMap<u64, usize> = keep.iter().map(|id| (*id, s.owned[id])).collect();
        for (slot, list) in fresh.iter().enumerate() {
            owned.extend(list.iter().map(|n| (n.id, slot)));
        }
        match s.parent {
            None => rebuild(&mut self.doc.nodes, &s.owned, &keep, &mut fresh, Arc::new),
            Some(pid) => {
                if let Some(f) = self.doc.find_mut(pid) {
                    rebuild(&mut f.children, &s.owned, &keep, &mut fresh, |n| n);
                }
            }
        }
        s.owned = owned;
        s.hover = None;
        self.sel = s.owned.keys().copied().collect();
        self.push_undo(pre);
        s.rev = self.ext.rev;
        self.status = if delete { "Region removed.".into() } else { "Regions merged.".into() };
        self.ext.builder = Some(s);
    }

    pub(super) fn paint_builder(&self, painter: &egui::Painter, cursor: Option<Pos2>, alt: bool) {
        let Some(s) = &self.ext.builder else { return };
        let o = self.canvas_rect.min;
        let screen = |c: &Contour| -> Vec<Pos2> {
            c.iter()
                .map(|p| {
                    let (x, y) = s.to_world(*p);
                    self.view.to_screen(o, x as f32, y as f32)
                })
                .collect()
        };
        // Garis tipis semua region supaya kelihatan bagian mana saja yang bisa dipilih.
        for c in s.cells.iter().filter(|c| c.state == State::Free) {
            for ring in &c.shape {
                painter.add(egui::Shape::closed_line(screen(ring), Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.45))));
            }
        }
        let (picked, delete) = match &s.sweep {
            Some(sw) => (s.expand(&sw.cells), sw.delete),
            None => (s.hover.map(|h| s.expand(&[h])).unwrap_or_default(), alt),
        };
        let tint = if delete { Color32::from_rgb(240, 70, 70) } else { ACCENT };
        for &i in &picked {
            let rings: Vec<Vec<Pos2>> = s.cells[i].shape.iter().map(screen).collect();
            if let Some((verts, indices)) = vtext::fill_contours(&rings, vtext::FillRule::EvenOdd) {
                let mut mesh = egui::Mesh::default();
                let color = tint.gamma_multiply(if s.sweep.is_some() { 0.42 } else { 0.28 });
                mesh.vertices = verts.into_iter().map(|pos| egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color }).collect();
                mesh.indices = indices;
                painter.add(egui::Shape::mesh(mesh));
            }
            for ring in rings {
                painter.add(egui::Shape::closed_line(ring, Stroke::new(1.6_f32, tint)));
            }
        }
        if let Some(sw) = &s.sweep {
            let mut line: Vec<Pos2> = sw.trail.iter().map(|q| self.view.to_screen(o, q.x, q.y)).collect();
            line.extend(cursor);
            painter.add(egui::Shape::line(line, Stroke::new(1.5_f32, tint)));
        }
        // Tanda kecil di samping kursor: + gabung, − hapus.
        if let Some(p) = cursor.filter(|p| self.canvas_rect.contains(*p)) {
            let c = p + vec2(16.0, 18.0);
            let mut lines = vec![[c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)]];
            if !delete {
                lines.push([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)]);
            }
            for l in &lines {
                painter.line_segment(*l, Stroke::new(4.2_f32, Color32::from_rgb(28, 28, 30)));
            }
            for l in &lines {
                painter.line_segment(*l, Stroke::new(1.8_f32, Color32::WHITE));
            }
        }
    }
}

// ---------------------------------------------------------------- poligon → kurva bezier

fn sub(a: P2, b: P2) -> P2 {
    (a.0 - b.0, a.1 - b.1)
}
fn add(a: P2, b: P2) -> P2 {
    (a.0 + b.0, a.1 + b.1)
}
fn mul(a: P2, k: f64) -> P2 {
    (a.0 * k, a.1 * k)
}
fn dot(a: P2, b: P2) -> f64 {
    a.0 * b.0 + a.1 * b.1
}
fn len(a: P2) -> f64 {
    a.0.hypot(a.1)
}
fn unit(a: P2) -> P2 {
    let l = len(a);
    if l < 1e-12 {
        (0.0, 0.0)
    } else {
        (a.0 / l, a.1 / l)
    }
}

/// Ubah poligon tertutup (hasil boolean atas kurva yang diratakan) kembali jadi titik path:
/// sudut tajam dan sisi lurus dipertahankan, bagian melengkung dicocokkan dengan kurva bezier.
fn fit_closed(poly: &[P2]) -> Vec<PenPt> {
    // Buang titik yang nyaris berimpit (potongan sangat pendek di titik potong).
    let mut p: Vec<P2> = Vec::with_capacity(poly.len());
    for &q in poly {
        if p.last().map_or(true, |l| len(sub(q, *l)) >= 0.04) {
            p.push(q);
        }
    }
    while p.len() > 3 && len(sub(p[0], p[p.len() - 1])) < 0.04 {
        p.pop();
    }
    let m = p.len();
    let corner_pt = |q: P2| PenPt { x: q.0 as f32, y: q.1 as f32, hin: [0.0; 2], hout: [0.0; 2], r: None };
    if m < 3 {
        return p.into_iter().map(corner_pt).collect();
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for q in &p {
        (x0, y0, x1, y1) = (x0.min(q.0), y0.min(q.1), x1.max(q.0), y1.max(q.1));
    }
    let tol = ((x1 - x0).hypot(y1 - y0) * 0.0002).max(0.04);

    let seg = |i: usize| sub(p[(i + 1) % m], p[i]);
    let seg_len: Vec<f64> = (0..m).map(|i| len(seg(i))).collect();
    // Sudut tajam: arah berbelok lebih dari ambang.
    let corner: Vec<bool> = (0..m)
        .map(|i| {
            let (a, b) = (unit(seg((i + m - 1) % m)), unit(seg(i)));
            dot(a, b).clamp(-1.0, 1.0).acos() > CORNER
        })
        .collect();
    // Sisi lurus: diapit dua sudut, atau jauh lebih panjang dari kedua tetangganya
    // (langkah kurva yang diratakan panjangnya hampir seragam).
    let straight: Vec<bool> = (0..m)
        .map(|i| {
            let (prev, next) = (seg_len[(i + m - 1) % m], seg_len[(i + 1) % m]);
            (corner[i] && corner[(i + 1) % m]) || (seg_len[i] > 4.0 * prev && seg_len[i] > 4.0 * next)
        })
        .collect();
    let mut brk: Vec<bool> = (0..m).map(|i| corner[i] || straight[i] || straight[(i + m - 1) % m]).collect();
    if !brk.iter().any(|b| *b) {
        // Kurva tertutup mulus (mis. lingkaran): pecah jadi empat bagian.
        for k in 0..4 {
            brk[k * m / 4] = true;
        }
    }
    let first = brk.iter().position(|b| *b).unwrap_or(0);
    let breaks: Vec<usize> = (0..m).map(|i| (first + i) % m).filter(|i| brk[*i]).collect();

    // Arah singgung di titik pecah: mengikuti sisi lurus yang menempel, kalau tidak dari tetangga.
    // Di ujung kurva (sudut tajam) arahnya diekstrapolasi dari dua langkah pertama, supaya
    // tepat untuk busur yang langkahnya seragam.
    let tangent = |i: usize, forward: bool| -> P2 {
        let (before, after) = ((i + m - 1) % m, i);
        if !corner[i] {
            if straight[before] {
                return unit(seg(before));
            }
            if straight[after] {
                return unit(seg(after));
            }
            return unit(sub(p[(i + 1) % m], p[(i + m - 1) % m]));
        }
        let (d0, d1, inner) = if forward {
            (unit(seg(after)), unit(seg((i + 1) % m)), (i + 1) % m)
        } else {
            (unit(seg(before)), unit(seg((i + m - 2) % m)), before)
        };
        if brk[inner] {
            d0
        } else {
            unit(sub(mul(d0, 1.5), mul(d1, 0.5)))
        }
    };

    let mut out: Vec<PenPt> = Vec::new();
    // Per bagian: indeks titik awalnya di `out`, dan pegangan masuk untuk titik awal bagian berikutnya.
    let mut starts: Vec<usize> = Vec::with_capacity(breaks.len());
    let mut ends: Vec<Option<[f32; 2]>> = Vec::with_capacity(breaks.len());
    for (bi, &a) in breaks.iter().enumerate() {
        let b = breaks[(bi + 1) % breaks.len()];
        let count = if b > a { b - a } else { b + m - a };
        starts.push(out.len());
        if count == 1 {
            out.push(corner_pt(p[a]));
            ends.push(None);
            continue;
        }
        let run: Vec<P2> = (0..=count).map(|k| p[(a + k) % m]).collect();
        let (t1, t2) = (tangent(a, true), mul(tangent(b, false), -1.0));
        let mut curves: Vec<[P2; 4]> = Vec::new();
        fit_cubic(&run, t1, t2, tol, &mut curves, 0);
        for (ci, c) in curves.iter().enumerate() {
            let mut anchor = corner_pt(c[0]);
            if ci > 0 {
                let prev = curves[ci - 1];
                anchor.hin = [(prev[2].0 - c[0].0) as f32, (prev[2].1 - c[0].1) as f32];
            }
            anchor.hout = [(c[1].0 - c[0].0) as f32, (c[1].1 - c[0].1) as f32];
            out.push(anchor);
        }
        ends.push(curves.last().map(|l| [(l[2].0 - l[3].0) as f32, (l[2].1 - l[3].1) as f32]));
    }
    for (bi, h) in ends.into_iter().enumerate() {
        if let Some(h) = h {
            let at = starts[(bi + 1) % starts.len()];
            out[at].hin = h;
        }
    }
    out
}

fn bez(c: &[P2; 4], t: f64) -> P2 {
    let u = 1.0 - t;
    let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (k0 * c[0].0 + k1 * c[1].0 + k2 * c[2].0 + k3 * c[3].0, k0 * c[0].1 + k1 * c[1].1 + k2 * c[2].1 + k3 * c[3].1)
}

/// Pencocokan kurva Schneider: titik `d` didekati satu kurva kubik; kalau simpangannya
/// melebihi `tol`, dipecah di titik terburuk dan dicoba lagi per bagian.
fn fit_cubic(d: &[P2], t1: P2, t2: P2, tol: f64, out: &mut Vec<[P2; 4]>, depth: usize) {
    let n = d.len();
    let (first, last) = (d[0], d[n - 1]);
    if n == 2 {
        let k = len(sub(last, first)) / 3.0;
        out.push([first, add(first, mul(t1, k)), add(last, mul(t2, k)), last]);
        return;
    }
    // Parameter panjang tali busur.
    let mut u = vec![0.0; n];
    for i in 1..n {
        u[i] = u[i - 1] + len(sub(d[i], d[i - 1]));
    }
    let total = u[n - 1].max(1e-12);
    for v in &mut u {
        *v /= total;
    }
    let mut curve = generate(d, &u, t1, t2);
    let (mut err, mut split) = max_error(d, &curve, &u);
    if err <= tol * tol {
        out.push(curve);
        return;
    }
    if err <= tol * tol * 64.0 {
        for _ in 0..4 {
            for i in 1..n - 1 {
                u[i] = newton(&curve, d[i], u[i]);
            }
            curve = generate(d, &u, t1, t2);
            (err, split) = max_error(d, &curve, &u);
            if err <= tol * tol {
                out.push(curve);
                return;
            }
        }
    }
    if depth > 24 {
        out.push(curve);
        return;
    }
    let split = split.clamp(1, n - 2);
    let mut center = unit(sub(d[split - 1], d[split + 1]));
    if center == (0.0, 0.0) {
        center = unit(sub(d[split - 1], d[split]));
    }
    fit_cubic(&d[..=split], t1, center, tol, out, depth + 1);
    fit_cubic(&d[split..], mul(center, -1.0), t2, tol, out, depth + 1);
}

fn generate(d: &[P2], u: &[f64], t1: P2, t2: P2) -> [P2; 4] {
    let n = d.len();
    let (first, last) = (d[0], d[n - 1]);
    let (mut c00, mut c01, mut c11, mut x0, mut x1) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for i in 0..n {
        let t = u[i];
        let s = 1.0 - t;
        let (b0, b1, b2, b3) = (s * s * s, 3.0 * s * s * t, 3.0 * s * t * t, t * t * t);
        let (a1, a2) = (mul(t1, b1), mul(t2, b2));
        c00 += dot(a1, a1);
        c01 += dot(a1, a2);
        c11 += dot(a2, a2);
        let base = add(mul(first, b0 + b1), mul(last, b2 + b3));
        let rest = sub(d[i], base);
        x0 += dot(a1, rest);
        x1 += dot(a2, rest);
    }
    let det = c00 * c11 - c01 * c01;
    let chord = len(sub(last, first));
    let (mut al, mut ar) = if det.abs() > 1e-12 { ((x0 * c11 - x1 * c01) / det, (c00 * x1 - c01 * x0) / det) } else { (0.0, 0.0) };
    let eps = 1e-6 * chord.max(1e-9);
    if al < eps || ar < eps || !al.is_finite() || !ar.is_finite() {
        // Solusi kuadrat-terkecil tidak masuk akal: pakai tebakan standar sepertiga tali busur.
        al = chord / 3.0;
        ar = chord / 3.0;
    }
    [first, add(first, mul(t1, al)), add(last, mul(t2, ar)), last]
}

/// Simpangan kuadrat terbesar dan indeks titiknya.
fn max_error(d: &[P2], c: &[P2; 4], u: &[f64]) -> (f64, usize) {
    let mut best = (0.0, d.len() / 2);
    for i in 1..d.len() - 1 {
        let e = sub(bez(c, u[i]), d[i]);
        let e = dot(e, e);
        if e >= best.0 {
            best = (e, i);
        }
    }
    best
}

/// Satu langkah Newton-Raphson untuk parameter titik terdekat di kurva.
fn newton(c: &[P2; 4], p: P2, t: f64) -> f64 {
    let q = bez(c, t);
    let d1: [P2; 3] = [mul(sub(c[1], c[0]), 3.0), mul(sub(c[2], c[1]), 3.0), mul(sub(c[3], c[2]), 3.0)];
    let d2: [P2; 2] = [mul(sub(d1[1], d1[0]), 2.0), mul(sub(d1[2], d1[1]), 2.0)];
    let s = 1.0 - t;
    let q1 = add(add(mul(d1[0], s * s), mul(d1[1], 2.0 * s * t)), mul(d1[2], t * t));
    let q2 = add(mul(d2[0], s), mul(d2[1], t));
    let diff = sub(q, p);
    let (num, den) = (dot(diff, q1), dot(q1, q1) + dot(diff, q2));
    if den.abs() < 1e-12 {
        t
    } else {
        (t - num / den).clamp(0.0, 1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(id: u64, x: f32, y: f32, w: f32, h: f32) -> Node {
        let mut n = Node::new(id, Kind::Rect, x, y);
        (n.w, n.h) = (w, h);
        n.fill = Some([200, 0, 0, 255]);
        n
    }

    fn world_area(s: &Session, c: &Cell) -> f64 {
        area(&c.shape) / (s.scale * s.scale)
    }

    #[test]
    fn two_overlapping_rects_make_three_regions() {
        let s = build(None, vec![rect(1, 0.0, 0.0, 100.0, 100.0), rect(2, 50.0, 50.0, 100.0, 100.0)], 0).unwrap();
        assert_eq!(s.cells.len(), 3);
        let mut areas: Vec<f64> = s.cells.iter().map(|c| world_area(&s, c).round()).collect();
        areas.sort_by(|a, b| a.total_cmp(b));
        assert_eq!(areas, vec![2500.0, 7500.0, 7500.0]);
        let mid = s.cell_at(pos2(75.0, 75.0)).unwrap();
        assert_eq!(s.cells[mid].members, vec![0, 1]);
        assert_eq!(s.cells[mid].owner, 1);
        assert!(s.cell_at(pos2(200.0, 10.0)).is_none());
    }

    #[test]
    fn circle_fits_back_into_a_few_smooth_points() {
        let mut n = Node::new(1, Kind::Ellipse, 0.0, 0.0);
        (n.w, n.h) = (200.0, 200.0);
        let ring = source_contours(&n).remove(0);
        let pts = fit_closed(&ring);
        assert!(pts.len() >= 4 && pts.len() <= 8, "{} points", pts.len());
        assert!(pts.iter().all(|q| q.hin != [0.0; 2] && q.hout != [0.0; 2]));
        // Kurva hasil tetap berjarak ~100 dari pusat.
        for w in 0..pts.len() {
            let (a, b) = (pts[w], pts[(w + 1) % pts.len()]);
            let c = [
                (a.x as f64, a.y as f64),
                ((a.x + a.hout[0]) as f64, (a.y + a.hout[1]) as f64),
                ((b.x + b.hin[0]) as f64, (b.y + b.hin[1]) as f64),
                (b.x as f64, b.y as f64),
            ];
            for k in 0..=10 {
                let q = bez(&c, k as f64 / 10.0);
                let r = (q.0 - 100.0).hypot(q.1 - 100.0);
                assert!((r - 100.0).abs() < 0.3, "radius {r}");
            }
        }
    }

    #[test]
    fn rounded_rect_keeps_straight_sides() {
        let mut n = rect(1, 0.0, 0.0, 200.0, 100.0);
        n.radius = 20.0;
        let ring = source_contours(&n).remove(0);
        let pts = fit_closed(&ring);
        // 8 titik: awal dan akhir tiap lengkung pojok.
        assert_eq!(pts.len(), 8, "{:?}", pts.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>());
    }

    #[test]
    fn plain_polygon_stays_a_polygon() {
        let ring = vec![(0.0, 0.0), (100.0, 0.0), (100.0, 50.0), (50.0, 50.0), (50.0, 100.0), (0.0, 100.0)];
        let pts = fit_closed(&ring);
        assert_eq!(pts.len(), 6);
        assert!(pts.iter().all(|q| q.hin == [0.0; 2] && q.hout == [0.0; 2]));
    }
}
