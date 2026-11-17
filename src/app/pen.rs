//! Pen tool (menggambar path baru, menambah/menghapus titik, melanjutkan path terbuka)
//! dan mode edit titik untuk path yang sudah ada.

use super::render::{PathEdit, PenPt};
use super::*;
use crate::doc::{rotate, PathPoint};

/// Jarak (piksel layar) klik masih dianggap mengenai titik.
const HIT: f32 = 7.0;
const ZERO: [f32; 2] = [0.0; 2];

/// Titik kontur utama dalam koordinat dunia (tanpa rotasi node).
pub fn world_points(n: &Node) -> Vec<PenPt> {
    to_world(n, &n.points)
}

fn to_world(n: &Node, pts: &[PathPoint]) -> Vec<PenPt> {
    pts.iter()
        .map(|p| PenPt {
            x: n.x + p.x * n.w,
            y: n.y + p.y * n.h,
            hin: [p.hin[0] * n.w, p.hin[1] * n.h],
            hout: [p.hout[0] * n.w, p.hout[1] * n.h],
            r: p.r,
        })
        .collect()
}

/// Pasang titik dunia sebagai satu-satunya kontur node: kotak pembatas dihitung ulang,
/// titik dinormalisasi terhadapnya.
pub fn set_world_points(n: &mut Node, pts: &[PenPt]) {
    let closed = n.closed;
    let rotation = n.rotation;
    set_geo(n, &Geo { cs: vec![pts.to_vec()], closed });
    n.rotation = rotation;
}

/// Geometri path di ruang dunia: semua kontur, rotasi node sudah diterapkan ke titik-titiknya.
/// `cs[0]` = kontur utama (terbuka/tertutup menurut `closed`), sisanya lubang yang selalu tertutup.
pub struct Geo {
    pub cs: Vec<Vec<PenPt>>,
    pub closed: bool,
}

impl Geo {
    fn is_closed(&self, c: usize) -> bool {
        c > 0 || self.closed
    }

    fn get(&self, (c, i): (usize, usize)) -> Option<&PenPt> {
        self.cs.get(c).and_then(|v| v.get(i))
    }

    /// Titik yang kena di posisi layar `p`.
    fn hit_point(&self, s: impl Fn(f32, f32) -> Pos2, p: Pos2) -> Option<(usize, usize)> {
        let mut best: Option<((usize, usize), f32)> = None;
        for (c, pts) in self.cs.iter().enumerate() {
            for (i, q) in pts.iter().enumerate() {
                let d = (s(q.x, q.y) - p).length();
                if d <= HIT && best.map_or(true, |b| d < b.1) {
                    best = Some(((c, i), d));
                }
            }
        }
        best.map(|b| b.0)
    }

    /// Segmen terdekat dari titik dunia `wp`: (kontur, indeks segmen, t).
    fn hit_segment(&self, wp: Pos2, tol: f32) -> Option<(usize, usize, f32)> {
        let mut best: Option<(usize, usize, f32, f32)> = None;
        for (c, pts) in self.cs.iter().enumerate() {
            if let Some((seg, t, d)) = nearest_segment(pts, self.is_closed(c), wp, tol) {
                if best.map_or(true, |b| d < b.3) {
                    best = Some((c, seg, t, d));
                }
            }
        }
        best.map(|(c, seg, t, _)| (c, seg, t))
    }
}

pub fn geo(n: &Node) -> Geo {
    let mut cs = vec![world_points(n)];
    cs.extend(n.holes.iter().map(|h| to_world(n, h)));
    if n.rotation != 0.0 {
        let (cx, cy) = n.center();
        let rot = n.rotation;
        for q in cs.iter_mut().flatten() {
            (q.x, q.y) = rotate(q.x, q.y, cx, cy, rot);
            let (ix, iy) = rotate(q.hin[0], q.hin[1], 0.0, 0.0, rot);
            let (ox, oy) = rotate(q.hout[0], q.hout[1], 0.0, 0.0, rot);
            (q.hin, q.hout) = ([ix, iy], [ox, oy]);
        }
    }
    Geo { cs, closed: n.closed }
}

/// Tulis geometri dunia ke node. Rotasi node jadi 0 (sudah "dipanggang" ke titik-titiknya).
pub fn set_geo(n: &mut Node, g: &Geo) {
    let raw = |pts: &Vec<PenPt>| pts.iter().map(|q| PathPoint { x: q.x, y: q.y, hin: q.hin, hout: q.hout, r: q.r }).collect::<Vec<_>>();
    // Sementara w = h = 1 di (0,0), sehingga titik "ternormalisasi" = koordinat dunia.
    (n.x, n.y, n.w, n.h) = (0.0, 0.0, 1.0, 1.0);
    n.rotation = 0.0;
    n.points = g.cs.first().map(raw).unwrap_or_default();
    n.holes = g.cs.iter().skip(1).map(raw).collect();
    n.closed = g.closed;
    // Kotak pembatas dihitung dari titik asli, bukan dari sudut yang sudah dibulatkan.
    let radius = std::mem::take(&mut n.radius);
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in n.contours(16).into_iter().flatten() {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    n.radius = radius;
    if x0 > x1 {
        return;
    }
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    for q in n.points.iter_mut().chain(n.holes.iter_mut().flatten()) {
        q.x = (q.x - x0) / w;
        q.y = (q.y - y0) / h;
        q.hin = [q.hin[0] / w, q.hin[1] / h];
        q.hout = [q.hout[0] / w, q.hout[1] / h];
    }
    (n.x, n.y, n.w, n.h) = (x0, y0, w, h);
}

/// Titik pada kurva segmen a→b di parameter t.
fn bezier(a: &PenPt, b: &PenPt, t: f32) -> (f32, f32) {
    let u = 1.0 - t;
    let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (
        k0 * a.x + k1 * (a.x + a.hout[0]) + k2 * (b.x + b.hin[0]) + k3 * b.x,
        k0 * a.y + k1 * (a.y + a.hout[1]) + k2 * (b.y + b.hin[1]) + k3 * b.y,
    )
}

/// Segmen terdekat dari titik `p`: (indeks segmen, t, jarak), kalau jaraknya dalam toleransi.
fn nearest_segment(pts: &[PenPt], closed: bool, p: Pos2, tol: f32) -> Option<(usize, f32, f32)> {
    let n = pts.len();
    if n < 2 {
        return None;
    }
    let count = if closed { n } else { n - 1 };
    let mut best: Option<(usize, f32, f32)> = None;
    for i in 0..count {
        let (a, b) = (&pts[i], &pts[(i + 1) % n]);
        for s in 1..48 {
            let t = s as f32 / 48.0;
            let (x, y) = bezier(a, b, t);
            let d = ((x - p.x).powi(2) + (y - p.y).powi(2)).sqrt();
            if d <= tol && best.map_or(true, |b| d < b.2) {
                best = Some((i, t, d));
            }
        }
    }
    best
}

/// Sisipkan titik di segmen `seg` pada parameter t tanpa mengubah bentuk kurva (de Casteljau).
fn split(pts: &mut Vec<PenPt>, seg: usize, t: f32) {
    let n = pts.len();
    let (ia, ib) = (seg, (seg + 1) % n);
    let (a, b) = (pts[ia], pts[ib]);
    let lerp = |p: (f32, f32), q: (f32, f32)| (p.0 + (q.0 - p.0) * t, p.1 + (q.1 - p.1) * t);
    let straight = a.hout == ZERO && b.hin == ZERO;
    let (p0, p1, p2, p3) = ((a.x, a.y), (a.x + a.hout[0], a.y + a.hout[1]), (b.x + b.hin[0], b.y + b.hin[1]), (b.x, b.y));
    let (q0, q1, q2) = (lerp(p0, p1), lerp(p1, p2), lerp(p2, p3));
    let (r0, r1) = (lerp(q0, q1), lerp(q1, q2));
    let m = lerp(r0, r1);
    let mut new = PenPt { x: m.0, y: m.1, hin: ZERO, hout: ZERO, r: None };
    if !straight {
        pts[ia].hout = [q0.0 - p0.0, q0.1 - p0.1];
        pts[ib].hin = [q2.0 - p3.0, q2.1 - p3.1];
        new.hin = [r0.0 - m.0, r0.1 - m.1];
        new.hout = [r1.0 - m.0, r1.1 - m.1];
    }
    pts.insert(seg + 1, new);
}

/// Ganti titik sudut ↔ halus. Pegangan halus otomatis sejajar garis antara dua titik tetangga.
fn toggle_smooth(pts: &mut [PenPt], i: usize, closed: bool) {
    let n = pts.len();
    if pts[i].hin != ZERO || pts[i].hout != ZERO {
        pts[i].hin = ZERO;
        pts[i].hout = ZERO;
        return;
    }
    let prev = if i > 0 { Some(pts[i - 1]) } else if closed { Some(pts[n - 1]) } else { None };
    let next = if i + 1 < n { Some(pts[i + 1]) } else if closed { Some(pts[0]) } else { None };
    let (a, b) = (prev.unwrap_or(pts[i]), next.unwrap_or(pts[i]));
    let (dx, dy) = ((b.x - a.x) / 6.0, (b.y - a.y) / 6.0);
    pts[i].hin = [-dx, -dy];
    pts[i].hout = [dx, dy];
}

/// Kunci vektor ke kelipatan 45°.
fn constrain45(dx: f32, dy: f32) -> (f32, f32) {
    let step = std::f32::consts::FRAC_PI_4;
    let angle = (dy.atan2(dx) / step).round() * step;
    let len = (dx * dx + dy * dy).sqrt();
    (len * angle.cos(), len * angle.sin())
}

/// Apakah Caps Lock sedang menyala (kursor presisi).
pub(super) fn caps_lock() -> bool {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetKeyState(vk: i32) -> i16;
        }
        const VK_CAPITAL: i32 = 0x14;
        // SAFETY: GetKeyState hanya membaca status tombol milik thread ini.
        unsafe { GetKeyState(VK_CAPITAL) & 1 != 0 }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Apa yang akan dilakukan klik pen di posisi kursor sekarang.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum PenHover {
    /// Titik baru (memulai path atau menambah segmen).
    New,
    /// Menutup path yang sedang digambar.
    Close,
    /// Klik titik terakhir: buang pegangan keluarnya (jadi sudut).
    Corner,
    /// Lanjutkan path terbuka dari ujungnya.
    Continue { id: u64, start: bool },
    Delete { id: u64, c: usize, i: usize },
    Add { id: u64, c: usize, seg: usize, t: f32 },
    /// Alt: ganti sudut ↔ halus.
    Convert { id: u64, c: usize, i: usize },
}

enum EditAction {
    None,
    Write,
    WriteUndo(Doc),
    Release(Option<Doc>),
}

impl App {
    // ------------------------------------------------------------ menggambar

    pub(super) fn pen_hover(&self, p: Pos2, alt: bool) -> PenHover {
        let (view, o) = (self.view, self.canvas_rect.min);
        let s = |x: f32, y: f32| view.to_screen(o, x, y);
        if let Some(pts) = &self.ext.pen {
            if pts.len() >= 2 && (s(pts[0].x, pts[0].y) - p).length() <= 8.0 {
                return PenHover::Close;
            }
            if pts.last().is_some_and(|l| (s(l.x, l.y) - p).length() <= HIT) {
                return PenHover::Corner;
            }
            return PenHover::New;
        }
        // Belum menggambar: path yang terpilih bisa ditambah/dikurangi titiknya atau dilanjutkan.
        let wp = view.to_world(o, p);
        for &id in self.sel.iter().rev() {
            let Some(n) = self.doc.find(id).filter(|n| n.kind == Kind::Path && n.visible) else { continue };
            let g = geo(n);
            if let Some((c, i)) = g.hit_point(s, p) {
                let end = c == 0 && !g.closed && (i == 0 || i + 1 == g.cs[0].len());
                return if alt {
                    PenHover::Convert { id, c, i }
                } else if end {
                    PenHover::Continue { id, start: i == 0 }
                } else {
                    PenHover::Delete { id, c, i }
                };
            }
            if let Some((c, seg, t)) = g.hit_segment(wp, 6.0 / view.zoom) {
                return PenHover::Add { id, c, seg, t };
            }
        }
        PenHover::New
    }

    pub(super) fn pen_press(&mut self, p: Pos2, wp: Pos2, mods: Modifiers) {
        self.ext.pen_keep_in = false;
        self.ext.pen_clicks = [self.ext.pen_clicks[1], p];
        let hover = self.pen_hover(p, mods.alt);
        // Aksi baru membatalkan redo; menambah titik/sudut dicatat supaya bisa di-undo per langkah.
        self.ext.pen_redo.clear();
        if matches!(hover, PenHover::New | PenHover::Corner) {
            self.ext.pen_undo.push(self.ext.pen.clone().unwrap_or_default());
        }
        match hover {
            PenHover::Close => self.finish_pen(true),
            PenHover::Corner => {
                if let Some(last) = self.ext.pen.as_mut().and_then(|v| v.last_mut()) {
                    last.hout = ZERO;
                }
            }
            PenHover::New => {
                let pts = self.ext.pen.get_or_insert_with(Vec::new);
                let (mut x, mut y) = (wp.x.round(), wp.y.round());
                if let (true, Some(last)) = (mods.shift, pts.last()) {
                    // Shift: kunci arah segmen ke kelipatan 45°.
                    let (dx, dy) = constrain45(x - last.x, y - last.y);
                    (x, y) = ((last.x + dx).round(), (last.y + dy).round());
                }
                pts.push(PenPt { x, y, hin: ZERO, hout: ZERO, r: None });
                self.ext.pen_drag = true;
            }
            PenHover::Continue { id, start } => {
                let Some(n) = self.doc.find(id) else { return };
                let mut pts = geo(n).cs.swap_remove(0);
                if start {
                    // Dilanjutkan dari titik awal: arah path dibalik supaya ujung itu jadi titik terakhir.
                    pts.reverse();
                    for q in &mut pts {
                        std::mem::swap(&mut q.hin, &mut q.hout);
                    }
                }
                self.ext.pen = Some(pts);
                self.ext.pen_cont = Some(id);
                // Tarik untuk membuat pegangan keluar tanpa mengubah kurva yang sudah ada.
                self.ext.pen_drag = true;
                self.ext.pen_keep_in = true;
                self.status = "Continuing the path: click to add points, Enter to finish.".into();
            }
            PenHover::Delete { id, c, i } => {
                self.remove_points(id, &[(c, i)]);
                self.status = "Anchor point removed.".into();
            }
            PenHover::Add { id, c, seg, t } => {
                self.edit_geo(id, |g| split(&mut g.cs[c], seg, t));
                self.status = "Anchor point added.".into();
            }
            PenHover::Convert { id, c, i } => {
                self.edit_geo(id, |g| {
                    let closed = g.is_closed(c);
                    toggle_smooth(&mut g.cs[c], i, closed);
                });
            }
        }
    }

    /// Ubah geometri satu path sebagai satu langkah undo.
    fn edit_geo(&mut self, id: u64, f: impl FnOnce(&mut Geo)) {
        let pre = self.doc.clone();
        let Some(n) = self.doc.find_mut(id) else { return };
        let mut g = geo(n);
        f(&mut g);
        set_geo(n, &g);
        self.push_undo(pre);
    }

    /// Tahan dan tarik setelah klik pen: membuat lengkungan.
    /// Alt = pegangan patah (hanya pegangan keluar), Shift = kelipatan 45°, Space = geser titiknya.
    pub(super) fn pen_drag_update(&mut self, wp: Pos2, inp: &Inp) {
        if !self.ext.pen_drag {
            return;
        }
        if !inp.down {
            self.ext.pen_drag = false;
            return;
        }
        let z = self.view.zoom;
        let keep_in = self.ext.pen_keep_in || inp.mods.alt;
        let Some(last) = self.ext.pen.as_mut().and_then(|v| v.last_mut()) else { return };
        if inp.space {
            last.x += inp.delta.x / z;
            last.y += inp.delta.y / z;
            return;
        }
        let (mut hx, mut hy) = (wp.x - last.x, wp.y - last.y);
        if inp.mods.shift {
            (hx, hy) = constrain45(hx, hy);
        }
        if (hx * hx + hy * hy).sqrt() * z > 3.0 {
            last.hout = [hx, hy];
            if !keep_in {
                last.hin = [-hx, -hy];
            }
        }
    }

    /// Backspace saat menggambar: buang titik terakhir.
    pub(super) fn pen_backspace(&mut self) {
        self.ext.pen_drag = false;
        self.ext.pen_redo.clear();
        if let Some(pts) = &mut self.ext.pen {
            self.ext.pen_undo.push(pts.clone());
            pts.pop();
            if pts.is_empty() {
                self.ext.pen = None;
                self.ext.pen_cont = None;
            }
        }
    }

    /// Ada langkah pen yang bisa di-undo / di-redo (menggambar belum selesai).
    pub(super) fn pen_can_undo(&self) -> bool {
        self.tool == Tool::Pen && self.ext.pen.is_some()
    }

    pub(super) fn pen_can_redo(&self) -> bool {
        self.tool == Tool::Pen && !self.ext.pen_redo.is_empty()
    }

    /// Undo saat menggambar: kembali ke keadaan sebelum titik terakhir. Tanpa riwayat
    /// (baru mulai melanjutkan path) = batal menggambar.
    pub(super) fn pen_undo(&mut self) {
        self.ext.pen_drag = false;
        let Some(cur) = self.ext.pen.take() else { return };
        match self.ext.pen_undo.pop() {
            Some(prev) => {
                self.ext.pen_redo.push(cur);
                if !prev.is_empty() {
                    self.ext.pen = Some(prev);
                }
            }
            None => self.ext.pen_redo.clear(),
        }
        if self.ext.pen.is_none() && self.ext.pen_redo.is_empty() {
            self.ext.pen_cont = None;
        }
    }

    pub(super) fn pen_redo(&mut self) {
        self.ext.pen_drag = false;
        let Some(next) = self.ext.pen_redo.pop() else { return };
        self.ext.pen_undo.push(self.ext.pen.take().unwrap_or_default());
        self.ext.pen = Some(next);
    }

    /// Ubah titik-titik pen jadi node path (atau tulis balik ke path yang dilanjutkan).
    pub(super) fn finish_pen(&mut self, close: bool) {
        self.ext.pen_drag = false;
        self.ext.pen_undo.clear();
        self.ext.pen_redo.clear();
        let cont = self.ext.pen_cont.take();
        let Some(mut pts) = self.ext.pen.take() else { return };
        pts.dedup_by(|b, a| (a.x - b.x).abs() < 0.5 && (a.y - b.y).abs() < 0.5);
        if pts.len() < 2 {
            return;
        }
        let closed = close && pts.len() >= 3;
        let pre = self.doc.clone();
        if let Some(n) = cont.and_then(|id| self.doc.find_mut(id)) {
            let id = n.id;
            set_geo(n, &Geo { cs: vec![pts], closed });
            self.push_undo(pre);
            self.sel = vec![id];
            self.tool = Tool::Select;
            self.status = "Path updated.".into();
            return;
        }
        let id = self.doc.alloc_id();
        let mut n = Node::new(id, Kind::Path, 0.0, 0.0);
        n.name = format!("Vector {id}");
        n.closed = closed;
        set_world_points(&mut n, &pts);
        let parent = self.doc.frame_at(pts[0].x, pts[0].y, &[]);
        n.stroke = Some(if parent.is_some() { [0, 0, 0, 255] } else { [235, 235, 235, 255] });
        n.stroke_w = 2.0;
        self.doc.insert(n, parent);
        self.push_undo(pre);
        self.sel = vec![id];
        self.tool = Tool::Select;
        self.status = "Path created. Double-click it (or press Enter) to edit its points; press P to add or remove anchors.".into();
    }

    /// Pratinjau path yang sedang digambar: garis, titik, pegangan, dan segmen ke kursor.
    pub(super) fn paint_pen(&self, painter: &egui::Painter, cursor: Option<Pos2>, shift: bool) {
        let Some(pts) = &self.ext.pen else { return };
        let o = self.canvas_rect.min;
        let s = |x: f32, y: f32| self.view.to_screen(o, x, y);
        let stroke = Stroke::new(1.5_f32, ACCENT);
        let curve = |a: &PenPt, b: &PenPt, stroke: Stroke| {
            let points = [s(a.x, a.y), s(a.x + a.hout[0], a.y + a.hout[1]), s(b.x + b.hin[0], b.y + b.hin[1]), s(b.x, b.y)];
            egui::epaint::CubicBezierShape::from_points_stroke(points, false, Color32::TRANSPARENT, stroke)
        };
        for w in pts.windows(2) {
            painter.add(curve(&w[0], &w[1], stroke));
        }
        // Dekat titik pertama: tandai bahwa klik akan menutup path.
        let closing = cursor.is_some_and(|c| pts.len() >= 2 && (s(pts[0].x, pts[0].y) - c).length() <= 8.0);
        if let (Some(last), Some(c), false) = (pts.last(), cursor, self.ext.pen_drag) {
            let wp = self.view.to_world(o, c);
            let (mut x, mut y) = (wp.x, wp.y);
            if shift {
                let (dx, dy) = constrain45(x - last.x, y - last.y);
                (x, y) = (last.x + dx, last.y + dy);
            }
            let target = if closing { pts[0] } else { PenPt { x, y, hin: ZERO, hout: ZERO, r: None } };
            painter.add(curve(last, &target, Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.6))));
        }
        for (i, q) in pts.iter().enumerate() {
            let c = s(q.x, q.y);
            for h in [q.hout, q.hin] {
                if h != ZERO {
                    let hp = s(q.x + h[0], q.y + h[1]);
                    painter.line_segment([c, hp], Stroke::new(1.0_f32, ACCENT.gamma_multiply(0.7)));
                    painter.circle_filled(hp, 3.0, ACCENT);
                }
            }
            let first = i == 0 && pts.len() >= 2;
            let size = if first && closing { 12.0 } else if first { 9.0 } else { 7.0 };
            let sq = Rect::from_center_size(c, vec2(size, size));
            painter.rect_filled(sq, CornerRadius::same(1), if first && closing { ACCENT } else { Color32::WHITE });
            painter.rect_stroke(sq, CornerRadius::same(1), stroke, StrokeKind::Inside);
        }
    }

    /// Pen aktif tapi belum menggambar: tampilkan titik path terpilih, supaya jelas mana yang
    /// bisa dihapus (klik titik), ditambah (klik segmen), atau dilanjutkan (klik ujung).
    pub(super) fn paint_pen_anchors(&self, painter: &egui::Painter) {
        if self.ext.pen.is_some() {
            return;
        }
        let o = self.canvas_rect.min;
        for n in self.sel.iter().filter_map(|id| self.doc.find(*id)).filter(|n| n.kind == Kind::Path && n.visible) {
            for q in geo(n).cs.iter().flatten() {
                let sq = Rect::from_center_size(self.view.to_screen(o, q.x, q.y), vec2(7.0, 7.0));
                painter.rect_filled(sq, CornerRadius::same(1), Color32::WHITE);
                painter.rect_stroke(sq, CornerRadius::same(1), Stroke::new(1.5_f32, ACCENT), StrokeKind::Inside);
            }
        }
    }

    /// Kursor pen: mata pena dengan ujung di posisi pointer, plus tanda kecil sesuai aksi klik.
    pub(super) fn paint_pen_cursor(&self, painter: &egui::Painter, p: Pos2, hover: PenHover) {
        let (sin, cos) = (-std::f32::consts::FRAC_PI_4).sin_cos();
        let at = |x: f32, y: f32| pos2(p.x + x * cos - y * sin, p.y + x * sin + y * cos);
        let dark = Color32::from_rgb(28, 28, 30);
        let nib = vec![at(0.0, 0.0), at(5.0, 10.0), at(3.4, 17.0), at(-3.4, 17.0), at(-5.0, 10.0)];
        painter.add(egui::Shape::convex_polygon(nib, dark, Stroke::new(1.4_f32, Color32::WHITE)));
        painter.line_segment([at(0.0, 2.5), at(0.0, 9.0)], Stroke::new(1.2_f32, Color32::WHITE));
        painter.circle_filled(at(0.0, 10.5), 1.5, Color32::WHITE);

        // Tanda di kanan-bawah pena.
        let c = p + vec2(19.0, 19.0);
        let lines: Vec<[Pos2; 2]> = match hover {
            PenHover::Add { .. } => vec![[c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], [c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)]],
            PenHover::Delete { .. } => vec![[c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)]],
            PenHover::Continue { .. } => vec![[c + vec2(-3.5, 3.5), c + vec2(3.5, -3.5)]],
            PenHover::Convert { .. } | PenHover::Corner => {
                vec![[c + vec2(-4.0, 3.0), c + vec2(0.0, -3.5)], [c + vec2(0.0, -3.5), c + vec2(4.0, 3.0)]]
            }
            PenHover::Close => {
                painter.circle_stroke(c, 3.6, Stroke::new(4.2_f32, dark));
                painter.circle_stroke(c, 3.6, Stroke::new(1.8_f32, Color32::WHITE));
                Vec::new()
            }
            PenHover::New if self.ext.pen.is_none() => {
                // Bintang kecil: klik memulai path baru.
                let d = 3.2;
                vec![[c - vec2(d, d), c + vec2(d, d)], [c - vec2(d, -d), c + vec2(d, -d)]]
            }
            PenHover::New => Vec::new(),
        };
        // Inti putih dengan tepi gelap: terbaca di canvas gelap maupun frame putih.
        for l in &lines {
            painter.line_segment(*l, Stroke::new(4.2_f32, dark));
        }
        for l in &lines {
            painter.line_segment(*l, Stroke::new(1.8_f32, Color32::WHITE));
        }
    }

    // ------------------------------------------------------------ edit titik

    pub(super) fn start_path_edit(&mut self, id: u64) {
        let Some(n) = self.doc.find_mut(id) else { return };
        if n.kind != Kind::Path {
            return;
        }
        if n.rotation != 0.0 {
            // Rotasi "dipanggang" ke titik-titiknya supaya pengeditan berlangsung di ruang dunia.
            let g = geo(n);
            set_geo(n, &g);
        }
        self.sel = vec![id];
        self.ext.path_edit = Some(PathEdit { id, sel: Vec::new(), drag: None, pre: None, changed: false, marquee: None });
        self.status =
            "Editing points: drag points/handles (Alt breaks a handle), Shift-click or drag a box to select several, click a segment to add a point, double-click a point to toggle smooth/corner, Del removes, Esc to finish."
                .into();
    }

    /// Interaksi pointer saat mode edit titik aktif.
    pub(super) fn path_edit_pointer(&mut self, p: Pos2, wp: Pos2, inp: &Inp, hovered: bool) {
        let Some(id) = self.ext.path_edit.as_ref().map(|e| e.id) else { return };
        let Some(node) = self.doc.find(id) else {
            self.ext.path_edit = None;
            return;
        };
        let mut g = geo(node);
        let (view, o) = (self.view, self.canvas_rect.min);
        let s = |x: f32, y: f32| view.to_screen(o, x, y);
        let snapshot = if inp.pressed && hovered { Some(self.doc.clone()) } else { None };
        let shift = inp.mods.shift;

        let action = {
            let Some(pe) = self.ext.path_edit.as_mut() else { return };
            let mut action = EditAction::None;
            if let (Some(snapshot), None) = (snapshot, pe.drag) {
                let mut hit = None;
                // Pegangan milik titik terpilih didahulukan, lalu titik, lalu segmen.
                if let [(c, i)] = pe.sel[..] {
                    if let Some(q) = g.get((c, i)) {
                        for (part, h) in [(1u8, q.hin), (2u8, q.hout)] {
                            if h != ZERO && (s(q.x + h[0], q.y + h[1]) - p).length() <= HIT {
                                hit = Some((c, i, part));
                            }
                        }
                    }
                }
                if hit.is_none() {
                    hit = g.hit_point(s, p).map(|(c, i)| (c, i, 0));
                }
                match hit {
                    Some((c, i, part)) => {
                        let mut drag = true;
                        if part == 0 {
                            let at = pe.sel.iter().position(|x| *x == (c, i));
                            match (shift, at) {
                                // Shift-klik: tambah/keluarkan titik dari seleksi.
                                (true, Some(k)) => {
                                    pe.sel.remove(k);
                                    drag = false;
                                }
                                (true, None) => pe.sel.push((c, i)),
                                (false, None) => pe.sel = vec![(c, i)],
                                (false, Some(_)) => {}
                            }
                        }
                        if drag {
                            pe.drag = Some((c, i, part));
                            pe.pre = Some(snapshot);
                            pe.changed = false;
                        }
                    }
                    None => match g.hit_segment(wp, 6.0 / view.zoom).filter(|_| !shift) {
                        Some((c, seg, t)) => {
                            split(&mut g.cs[c], seg, t);
                            pe.sel = vec![(c, seg + 1)];
                            pe.drag = Some((c, seg + 1, 0));
                            pe.pre = None;
                            pe.changed = false;
                            action = EditAction::WriteUndo(snapshot);
                        }
                        None => {
                            // Area kosong: mulai kotak seleksi titik.
                            if !shift {
                                pe.sel.clear();
                            }
                            pe.marquee = Some(p);
                        }
                    },
                }
            }
            if let Some((c, i, part)) = pe.drag.filter(|(c, i, _)| g.get((*c, *i)).is_some()) {
                if inp.down {
                    if inp.delta != Vec2::ZERO {
                        match part {
                            0 => {
                                let (dx, dy) = (inp.delta.x / view.zoom, inp.delta.y / view.zoom);
                                for &(c2, i2) in &pe.sel {
                                    if let Some(q) = g.cs.get_mut(c2).and_then(|v| v.get_mut(i2)) {
                                        q.x += dx;
                                        q.y += dy;
                                    }
                                }
                            }
                            1 => {
                                let q = &mut g.cs[c][i];
                                q.hin = [wp.x - q.x, wp.y - q.y];
                                // Alt: patahkan pegangan (tidak dicerminkan).
                                if !inp.mods.alt {
                                    q.hout = [-q.hin[0], -q.hin[1]];
                                }
                            }
                            _ => {
                                let q = &mut g.cs[c][i];
                                q.hout = [wp.x - q.x, wp.y - q.y];
                                if !inp.mods.alt {
                                    q.hin = [-q.hout[0], -q.hout[1]];
                                }
                            }
                        }
                        pe.changed = true;
                        if matches!(action, EditAction::None) {
                            action = EditAction::Write;
                        }
                    }
                } else {
                    pe.drag = None;
                    let pre = pe.pre.take().filter(|_| pe.changed);
                    if matches!(action, EditAction::None) {
                        action = EditAction::Release(pre);
                    }
                }
            } else if !inp.down {
                pe.drag = None;
                if let Some(start) = pe.marquee.take() {
                    let r = Rect::from_two_pos(start, p);
                    if r.width() > 2.0 || r.height() > 2.0 {
                        for (c, pts) in g.cs.iter().enumerate() {
                            for (i, q) in pts.iter().enumerate() {
                                if r.contains(s(q.x, q.y)) && !pe.sel.contains(&(c, i)) {
                                    pe.sel.push((c, i));
                                }
                            }
                        }
                    }
                }
            }
            action
        };

        match action {
            EditAction::None => {}
            EditAction::Write => {
                if let Some(n) = self.doc.find_mut(id) {
                    set_geo(n, &g);
                }
            }
            EditAction::WriteUndo(pre) => {
                if let Some(n) = self.doc.find_mut(id) {
                    set_geo(n, &g);
                }
                self.push_undo(pre);
            }
            EditAction::Release(pre) => {
                if let Some(pre) = pre {
                    self.push_undo(pre);
                }
            }
        }
    }

    /// Klik dua kali: di titik = ganti sudut ↔ halus; di luar path = selesai mengedit.
    pub(super) fn path_edit_double(&mut self, p: Pos2) {
        let Some(id) = self.ext.path_edit.as_ref().map(|e| e.id) else { return };
        let Some(node) = self.doc.find(id) else { return };
        let g = geo(node);
        let (view, o) = (self.view, self.canvas_rect.min);
        let wp = view.to_world(o, p);
        let Some((c, i)) = g.hit_point(|x, y| view.to_screen(o, x, y), p) else {
            if g.hit_segment(wp, 8.0 / view.zoom).is_none() {
                self.ext.path_edit = None;
            }
            return;
        };
        self.edit_geo(id, |g| {
            let closed = g.is_closed(c);
            toggle_smooth(&mut g.cs[c], i, closed);
        });
        if let Some(pe) = &mut self.ext.path_edit {
            pe.sel = vec![(c, i)];
            pe.drag = None;
        }
    }

    /// Hapus titik-titik dari path. Kontur utama yang tinggal < 2 titik menghapus path-nya;
    /// lubang yang tinggal < 3 titik ikut dibuang.
    fn remove_points(&mut self, id: u64, which: &[(usize, usize)]) {
        let Some(node) = self.doc.find(id) else { return };
        let mut g = geo(node);
        let mut which: Vec<(usize, usize)> = which.iter().copied().filter(|x| g.get(*x).is_some()).collect();
        if which.is_empty() {
            return;
        }
        which.sort_unstable_by(|a, b| b.cmp(a));
        which.dedup();
        for (c, i) in which {
            g.cs[c].remove(i);
        }
        let pre = self.doc.clone();
        if g.cs[0].len() < 2 {
            self.doc.remove(id);
            self.sel.retain(|x| *x != id);
            self.ext.path_edit = None;
        } else if let Some(node) = self.doc.find_mut(id) {
            if g.cs[0].len() < 3 {
                g.closed = false;
            }
            let mut c = 0;
            g.cs.retain(|pts| {
                c += 1;
                c == 1 || pts.len() >= 3
            });
            set_geo(node, &g);
        }
        self.push_undo(pre);
    }

    /// Hapus titik terpilih (Del / Backspace di mode edit titik).
    pub(super) fn path_edit_delete(&mut self) {
        let Some((id, sel)) = self.ext.path_edit.as_ref().map(|e| (e.id, e.sel.clone())) else { return };
        self.remove_points(id, &sel);
        if let Some(pe) = &mut self.ext.path_edit {
            pe.sel.clear();
            pe.drag = None;
        }
    }

    /// Geser titik terpilih dengan tombol panah. Hasil `false`: tidak ada titik terpilih.
    pub(super) fn path_edit_nudge(&mut self, dx: f32, dy: f32) -> bool {
        let Some((id, sel)) = self.ext.path_edit.as_ref().map(|e| (e.id, e.sel.clone())) else { return false };
        if sel.is_empty() {
            return false;
        }
        self.edit_geo(id, |g| {
            for (c, i) in sel {
                if let Some(q) = g.cs.get_mut(c).and_then(|v| v.get_mut(i)) {
                    q.x += dx;
                    q.y += dy;
                }
            }
        });
        true
    }

    /// Ctrl+A di mode edit titik: pilih semua titik.
    pub(super) fn path_edit_select_all(&mut self) {
        let Some(id) = self.ext.path_edit.as_ref().map(|e| e.id) else { return };
        let Some(node) = self.doc.find(id) else { return };
        let all: Vec<(usize, usize)> =
            std::iter::once(&node.points).chain(node.holes.iter()).enumerate().flat_map(|(c, v)| (0..v.len()).map(move |i| (c, i))).collect();
        if let Some(pe) = &mut self.ext.path_edit {
            pe.sel = all;
        }
    }

    pub(super) fn paint_path_edit(&self, painter: &egui::Painter, cursor: Option<Pos2>) {
        let Some(pe) = &self.ext.path_edit else { return };
        let Some(node) = self.doc.find(pe.id) else { return };
        let o = self.canvas_rect.min;
        let s = |x: f32, y: f32| self.view.to_screen(o, x, y);
        let g = geo(node);
        for q in pe.sel.iter().filter_map(|x| g.get(*x)) {
            for h in [q.hin, q.hout] {
                if h != ZERO {
                    let hp = s(q.x + h[0], q.y + h[1]);
                    painter.line_segment([s(q.x, q.y), hp], Stroke::new(1.0_f32, ACCENT));
                    painter.circle_filled(hp, 4.0, Color32::WHITE);
                    painter.circle_stroke(hp, 4.0, Stroke::new(1.5_f32, ACCENT));
                }
            }
        }
        for (c, pts) in g.cs.iter().enumerate() {
            for (i, q) in pts.iter().enumerate() {
                let selected = pe.sel.contains(&(c, i));
                let sq = Rect::from_center_size(s(q.x, q.y), vec2(8.0, 8.0));
                painter.rect_filled(sq, CornerRadius::same(1), if selected { ACCENT } else { Color32::WHITE });
                painter.rect_stroke(sq, CornerRadius::same(1), Stroke::new(1.5_f32, ACCENT), StrokeKind::Inside);
            }
        }
        if let (Some(start), Some(c)) = (pe.marquee, cursor) {
            let mr = Rect::from_two_pos(start, c);
            painter.rect_filled(mr, CornerRadius::ZERO, ACCENT.gamma_multiply(0.12));
            painter.rect_stroke(mr, CornerRadius::ZERO, Stroke::new(1.0_f32, ACCENT), StrokeKind::Inside);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(x: f32, y: f32) -> PenPt {
        PenPt { x, y, hin: ZERO, hout: ZERO, r: None }
    }

    #[test]
    fn geo_round_trips_holes_and_bakes_rotation() {
        let mut n = Node::new(1, Kind::Path, 0.0, 0.0);
        let outer = vec![pt(0.0, 0.0), pt(100.0, 0.0), pt(100.0, 50.0), pt(0.0, 50.0)];
        let hole = vec![pt(40.0, 20.0), pt(60.0, 20.0), pt(60.0, 30.0)];
        set_geo(&mut n, &Geo { cs: vec![outer, hole], closed: true });
        assert_eq!((n.x, n.y, n.w, n.h), (0.0, 0.0, 100.0, 50.0));
        assert_eq!(n.holes.len(), 1);
        // Lubang tidak ikut terisi.
        n.fill = Some([0, 0, 0, 255]);
        assert!(n.contains(10.0, 10.0, 0.0));
        assert!(!n.contains(55.0, 22.0, 0.0));

        n.rotation = 90.0;
        let g = geo(&n);
        set_geo(&mut n, &g);
        assert_eq!(n.rotation, 0.0);
        assert!((n.w - 50.0).abs() < 1e-3 && (n.h - 100.0).abs() < 1e-3);
    }

    #[test]
    fn split_keeps_the_curve_shape() {
        let mut pts = vec![
            PenPt { x: 0.0, y: 0.0, hin: ZERO, hout: [30.0, -40.0], r: None },
            PenPt { x: 100.0, y: 0.0, hin: [-30.0, -40.0], hout: ZERO, r: None },
        ];
        let before = bezier(&pts[0], &pts[1], 0.5);
        split(&mut pts, 0, 0.5);
        assert_eq!(pts.len(), 3);
        assert!((pts[1].x - before.0).abs() < 1e-3 && (pts[1].y - before.1).abs() < 1e-3);
        // Seperempat kurva lama = tengah paruh pertama.
        let old = (
            0.421875 * 0.0 + 0.421875 * 30.0 + 0.140625 * 70.0 + 0.015625 * 100.0,
            0.421875 * -40.0 + 0.140625 * -40.0,
        );
        let new = bezier(&pts[0], &pts[1], 0.5);
        assert!((new.0 - old.0).abs() < 1e-3 && (new.1 - old.1).abs() < 1e-3);
    }
}
