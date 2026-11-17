use crate::doc::{Doc, Kind, Node, LAYER_COLORS};
use crate::export;
use crate::fonts::{weight_name, CatalogState, FontManager};
use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Key, Modifiers, PointerButton, Pos2, Rect,
    Sense, Stroke, StrokeKind, Vec2,
};
use std::collections::HashSet;
use std::path::PathBuf;

mod assist;
mod autolayout;
pub(crate) mod blend;
pub(crate) mod builder;
mod canvas;
mod components;
mod constraints;
mod corners;
mod crop;
mod find;
mod home;
mod icons;
mod library;
mod pacer;
mod pages;
mod panel;
pub(crate) mod pen;
pub(crate) mod perf;
mod photos;
mod picker;
mod polygon;
mod presets;
pub mod rembg;
mod remote;
mod rulers;
mod stroke;
mod styles;
mod svg;
pub(crate) mod render;
mod theme;
mod vtext;
mod wand;

const ACCENT: Color32 = Color32::from_rgb(13, 153, 255);
const CANVAS_BG: Color32 = Color32::from_rgb(24, 24, 26);
const MIN_ZOOM: f32 = 0.01;
const MAX_ZOOM: f32 = 64.0;
const UNDO_LIMIT: usize = 200;
const CLIP_TAG: &str = "kanvas_clip";
/// Pojok kiri-atas frame asal objek yang disalin, untuk Paste in Place ke frame lain.
const CLIP_ORIGIN: &str = "kanvas_origin";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Scale,
    Wand,
    Hand,
    Frame,
    Rect,
    Ellipse,
    Line,
    Polygon,
    Pen,
    ShapeBuilder,
    Text,
}

/// Urutan di toolbar bawah: (tool, nama, shortcut).
const TOOLS: [(Tool, &str, &str); 12] = [
    (Tool::Select, "Move", "V"),
    (Tool::Scale, "Scale", "K"),
    (Tool::Wand, "Magic wand", "Y"),
    (Tool::Hand, "Hand", "H / hold Space"),
    (Tool::Frame, "Frame", "F"),
    (Tool::Rect, "Rectangle", "R"),
    (Tool::Ellipse, "Ellipse", "O"),
    (Tool::Line, "Line", "L"),
    (Tool::Polygon, "Polygon / star", "Shift+P"),
    (Tool::Pen, "Pen", "P"),
    (Tool::ShapeBuilder, "Shape builder", "Shift+M"),
    (Tool::Text, "Text", "T"),
];

/// Posisi 8 pegangan sebagai pecahan kotak pembatas (searah jarum jam dari kiri-atas).
const HANDLE_F: [(f32, f32); 8] =
    [(0.0, 0.0), (0.5, 0.0), (1.0, 0.0), (1.0, 0.5), (1.0, 1.0), (0.5, 1.0), (0.0, 1.0), (0.0, 0.5)];

fn draw_icon(p: &egui::Painter, tool: Tool, c: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
    let line = |a: Pos2, b: Pos2| {
        p.line_segment([a, b], s);
    };
    match tool {
        Tool::Select => {
            p.add(egui::Shape::closed_line(vec![at(-5.5, -8.0), at(6.5, 0.5), at(0.5, 1.8), at(-2.5, 7.5)], s));
        }
        Tool::Scale => {
            p.rect_stroke(Rect::from_min_max(at(-8.0, -1.0), at(1.0, 8.0)), CornerRadius::same(1), s, StrokeKind::Middle);
            line(at(2.0, -2.0), at(8.0, -8.0));
            line(at(3.0, -8.0), at(8.0, -8.0));
            line(at(8.0, -8.0), at(8.0, -3.0));
        }
        Tool::Wand => {
            // Tongkat miring dengan kilau di ujungnya.
            line(at(-7.5, 8.0), at(1.0, -0.5));
            for (dx, dy) in [(0.0, -3.6), (0.0, 3.6), (-3.6, 0.0), (3.6, 0.0)] {
                line(at(4.0 + dx * 0.45, -3.5 + dy * 0.45), at(4.0 + dx, -3.5 + dy));
            }
            p.circle_filled(at(-4.5, -6.0), 1.0, color);
            p.circle_filled(at(7.0, 4.5), 1.0, color);
        }
        Tool::Hand => {
            // Telapak + empat jari, digambar sederhana.
            p.rect_stroke(Rect::from_min_max(at(-5.5, -1.0), at(6.0, 8.0)), CornerRadius::same(4), s, StrokeKind::Middle);
            for (x, top) in [(-3.6, -6.0), (-0.7, -8.0), (2.2, -7.0), (5.0, -4.5)] {
                line(at(x, top), at(x, -1.0));
            }
        }
        Tool::Frame => {
            for d in [-4.0, 4.0] {
                line(at(d, -8.0), at(d, 8.0));
                line(at(-8.0, d), at(8.0, d));
            }
        }
        Tool::Rect => {
            p.rect_stroke(Rect::from_center_size(c, vec2(15.0, 15.0)), CornerRadius::same(2), s, StrokeKind::Middle);
        }
        Tool::Ellipse => {
            p.circle_stroke(c, 7.5, s);
        }
        Tool::Line => line(at(-7.0, 7.0), at(7.0, -7.0)),
        Tool::Polygon => {
            // Segi lima.
            let pts: Vec<Pos2> = (0..5)
                .map(|i| {
                    let a = (-90.0_f32 + 72.0 * i as f32).to_radians();
                    at(8.0 * a.cos(), 8.0 * a.sin() + 0.8)
                })
                .collect();
            p.add(egui::Shape::closed_line(pts, s));
        }
        Tool::Pen => {
            // Mata pena.
            p.add(egui::Shape::closed_line(vec![at(0.0, -8.5), at(5.5, 1.0), at(2.5, 8.0), at(-2.5, 8.0), at(-5.5, 1.0)], s));
            line(at(0.0, -8.5), at(0.0, 0.5));
            p.circle_filled(at(0.0, 1.8), 1.6, color);
        }
        Tool::ShapeBuilder => {
            // Dua bentuk bertumpuk yang sedang disatukan, dengan tanda plus.
            p.circle_stroke(at(-2.8, 2.2), 5.4, s);
            p.rect_stroke(Rect::from_min_max(at(-1.5, -7.5), at(7.5, 1.5)), CornerRadius::same(1), s, StrokeKind::Middle);
            p.circle_filled(at(1.0, -1.0), 1.7, color);
        }
        Tool::Text => {
            line(at(-6.5, -7.0), at(6.5, -7.0));
            line(at(0.0, -7.0), at(0.0, 7.5));
            line(at(-6.5, -7.0), at(-6.5, -4.5));
            line(at(6.5, -7.0), at(6.5, -4.5));
            line(at(-2.5, 7.5), at(2.5, 7.5));
        }
    }
}

/// Skala proporsional terhadap titik jangkar, termasuk stroke, radius, ukuran font, dan anak.
fn scale_node(n: &mut Node, s: f32, ax: f32, ay: f32) {
    n.x = ax + (n.x - ax) * s;
    n.y = ay + (n.y - ay) * s;
    n.w *= s;
    n.h *= s;
    n.stroke_w *= s;
    n.dash *= s;
    n.gap *= s;
    if let Some(shadow) = &mut n.shadow {
        shadow.x *= s;
        shadow.y *= s;
        shadow.blur *= s;
    }
    n.radius *= s;
    for q in n.points.iter_mut().chain(n.holes.iter_mut().flatten()) {
        q.r = q.r.map(|r| r * s);
    }
    n.blur *= s;
    if let Some(l) = &mut n.layout {
        l.gap *= s;
        l.row_gap *= s;
        l.pad = l.pad.map(|p| p * s);
    }
    n.font_size = (n.font_size * s).max(1.0);
    for span in &mut n.spans {
        span.style.size = (span.style.size * s).max(1.0);
    }
    for c in &mut n.children {
        scale_node(c, s, ax, ay);
    }
}

/// Putar node sebesar `deg` derajat terhadap titik `pivot` (memutar beberapa objek bersama):
/// pusat node ikut berpindah mengelilingi pivot, dan rotasinya sendiri bertambah.
fn spin_node(n: &mut Node, (px, py): (f32, f32), deg: f32) {
    use crate::doc::rotate;
    let (cx, cy) = n.center();
    if n.kind == Kind::Line {
        // Garis disimpan sebagai dua ujung: ujungnya yang diputar, tanpa nilai rotasi.
        let turn = |x: f32, y: f32| {
            let (x, y) = rotate(x, y, cx, cy, n.rotation);
            rotate(x, y, px, py, deg)
        };
        let (a, b) = (turn(n.x, n.y), turn(n.x + n.w, n.y + n.h));
        (n.x, n.y, n.w, n.h, n.rotation) = (a.0, a.1, b.0 - a.0, b.1 - a.1, 0.0);
        return;
    }
    let (nx, ny) = rotate(cx, cy, px, py, deg);
    n.translate(nx - cx, ny - cy);
    let rot = (n.rotation + deg + 180.0).rem_euclid(360.0) - 180.0;
    n.rotation = (rot * 10.0).round() / 10.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinning_a_selection_orbits_the_pivot() {
        let mut a = Node::new(1, Kind::Rect, 100.0, -10.0);
        (a.w, a.h, a.rotation) = (40.0, 20.0, 30.0);
        spin_node(&mut a, (0.0, 0.0), 90.0);
        // Pusat (120, 0) berputar 90° searah jarum jam di layar → (0, 120); ukuran tetap.
        let (cx, cy) = a.center();
        assert!(cx.abs() < 1e-3 && (cy - 120.0).abs() < 1e-3, "center at ({cx}, {cy})");
        assert_eq!((a.w, a.h, a.rotation), (40.0, 20.0, 120.0));
        let mut line = Node::new(2, Kind::Line, 10.0, 0.0);
        (line.w, line.h) = (10.0, 0.0);
        spin_node(&mut line, (0.0, 0.0), 90.0);
        assert!(line.x.abs() < 1e-3 && (line.y - 10.0).abs() < 1e-3 && line.w.abs() < 1e-3 && (line.h - 10.0).abs() < 1e-3);
        assert_eq!(line.rotation, 0.0);
    }

    #[test]
    fn stretch_keeps_layout_inside_the_selection_box() {
        // Kotak seleksi (0,0)-(200,100) ditarik jadi (10,20)-(410,70): sx = 2, sy = 0.5.
        let mut a = Node::new(1, Kind::Rect, 0.0, 0.0);
        (a.w, a.h) = (50.0, 100.0);
        let mut b = Node::new(2, Kind::Image, 150.0, 60.0);
        (b.w, b.h) = (50.0, 40.0);
        let mut label = Node::new(3, Kind::Text, 80.0, 40.0);
        (label.w, label.h) = (40.0, 20.0);
        for n in [&mut a, &mut b, &mut label] {
            n.stretch((0.0, 0.0), (10.0, 20.0), (2.0, 0.5));
        }
        assert_eq!((a.x, a.y, a.w, a.h), (10.0, 20.0, 100.0, 50.0));
        assert_eq!((b.x, b.y, b.w, b.h), (310.0, 50.0, 100.0, 20.0));
        // Teks lebar-otomatis: ukuran tetap, pusatnya ikut berpindah.
        assert_eq!((label.w, label.h), (40.0, 20.0));
        assert_eq!(label.center(), (210.0, 45.0));
    }
}

#[derive(Clone, Copy)]
pub(crate) struct View {
    pan: Vec2,
    zoom: f32,
}

impl View {
    fn to_screen(&self, o: Pos2, x: f32, y: f32) -> Pos2 {
        pos2(o.x + self.pan.x + x * self.zoom, o.y + self.pan.y + y * self.zoom)
    }
    fn to_world(&self, o: Pos2, p: Pos2) -> Pos2 {
        pos2((p.x - o.x - self.pan.x) / self.zoom, (p.y - o.y - self.pan.y) / self.zoom)
    }
}

enum Drag {
    None,
    Pan,
    /// `dup`: Alt sedang ditahan, yang digeser adalah salinan; isinya seleksi asli (untuk dibatalkan
    /// kalau Alt dilepas sebelum mouse dilepas).
    Move { start: Pos2, applied: Vec2, moved: bool, dup: Option<Vec<u64>> },
    Resize { id: u64, handle: u8, orig: (f32, f32, f32, f32), start: Pos2 },
    Scale { id: u64, handle: u8, orig: Node },
    /// Resize/scale beberapa objek sekaligus lewat kotak seleksi. `bounds` = kotak asal,
    /// `orig` = node asli; `scale` = Scale tool (proporsional, termasuk stroke dan font).
    MultiResize { handle: u8, bounds: (f32, f32, f32, f32), start: Pos2, orig: Vec<Node>, scale: bool },
    TextBox { start: Pos2, cur: Pos2 },
    Rotate { id: u64, center: Pos2, start: f32, orig: f32 },
    /// Memutar beberapa objek (mis. satu group) bersama terhadap `center`. `orig` = node asli,
    /// `angle` = sudut putar saat ini (derajat) sejak mulai menarik.
    MultiRotate { center: Pos2, start: f32, orig: Vec<Node>, angle: f32 },
    Marquee { start: Pos2, base: Vec<u64> },
    Create { id: u64, start: Pos2 },
    /// Menarik pegangan gradasi di canvas: 0/1 = ujung garis, 2+i = stop ke-i.
    Gradient { id: u64, part: usize, orig: crate::doc::Gradient },
    /// Mode crop gambar: menarik pegangan crop, pegangan gambar, atau gambarnya. `geom` = keadaan awal.
    Crop { part: crop::Part, start: Pos2, geom: crop::Geom },
    /// Menarik widget sudut sebuah vector. `at` = (kontur, titik); `single` = hanya sudut itu (Alt);
    /// `r0` / `s0` = jari-jari dan posisi pointer sepanjang garis bagi saat mulai menarik.
    Corner { id: u64, at: (usize, usize), single: bool, r0: f32, s0: f32 },
}

struct Inp {
    pos: Option<Pos2>,
    pressed: bool,
    down: bool,
    mid_pressed: bool,
    mid_down: bool,
    delta: Vec2,
    mods: Modifiers,
    space: bool,
    zoom: f32,
    scroll: Vec2,
}

struct Bench {
    out: PathBuf,
    frame: u32,
    samples: Vec<f32>,
    shot: bool,
    phase: u8,
    mark: u32,
    /// Event pointer buatan untuk menguji interaksi mouse: (frame, event).
    inject: Vec<(u32, egui::Event)>,
    /// Id node yang dipakai skenario uji yang berjalan lintas frame.
    ids: Vec<u64>,
    /// Jarak waktu antar frame (ms) selama fase zoom/pan: menunjukkan refresh rate yang benar-benar dicapai.
    intervals: Vec<f32>,
    last: Option<std::time::Instant>,
    /// `KANVAS_PERF`: time the canvas instead (see `perf`).
    perf: perf::State,
}

pub struct App {
    doc: Doc,
    view: View,
    tool: Tool,
    sel: Vec<u64>,
    drag: Drag,
    pre_drag: Option<Doc>,
    undo: Vec<Doc>,
    redo: Vec<Doc>,
    fonts: FontManager,
    editing: Option<u64>,
    edit_frames: u32,
    edit_pre: Option<Doc>,
    path: Option<PathBuf>,
    dirty: bool,
    title: String,
    font_search: String,
    expanded: HashSet<u64>,
    /// Group yang namanya sedang diedit di panel layer: (id group, teks, fokus sudah diminta).
    group_rename: Option<(u64, String, bool)>,
    /// Layer yang sedang ditarik di panel layer (untuk dipindah urutan / induk / group-nya).
    layer_drag: Option<Vec<u64>>,
    status: String,
    last_prop: (u64, f64),
    canvas_rect: Rect,
    fit_pending: bool,
    labels: Vec<(u64, Rect)>,
    drawn: usize,
    frame_ms: f32,
    /// Waktu menunggu refresh layar pada frame sebelumnya (lihat `pacer`); bukan beban kerja.
    pace_ms: f32,
    last_zoom: (f32, f64),
    show_ui: bool,
    font_focus: bool,
    ext: render::Ext,
    bench: Option<Bench>,
    /// Halaman awal (daftar file terbaru) sedang ditampilkan.
    home: bool,
    files: home::Files,
    /// Remove background (Codex) yang sedang berjalan.
    rembg: Option<rembg::Job>,
    /// Export yang sedang berjalan di thread lain; hasilnya teks untuk status bar.
    exporting: Option<std::sync::mpsc::Receiver<String>>,
    /// Remote control lewat soket lokal (untuk server MCP `kanvas-mcp`).
    remote: Option<remote::Remote>,
    /// Pengaturan magic wand.
    wand: wand::Wand,
    /// Jendela pencarian icon (Iconify).
    icons: icons::Icons,
    /// Jendela pencarian foto stok (Openverse).
    photos: photos::Photos,
    /// Penggaris dan guide yang sedang ditarik.
    rulers: rulers::Rulers,
    /// Pengaturan tool Polygon / star.
    poly: polygon::Options,
    /// Daftar halaman: tampilan tiap halaman dan nama yang sedang diedit.
    pages: pages::Pages,
    /// Jendela Library: gambar milik pengguna yang tersedia di semua dokumen.
    library: library::Library,
    /// Mode crop gambar yang sedang aktif.
    crop: Option<crop::Session>,
    /// The About window is open.
    about: bool,
    /// The Components window, and what instances were last synced against.
    components: components::Components,
    /// Where the item being dragged will land in an auto layout: a line, in world coordinates.
    drop_mark: Option<[Pos2; 2]>,
    /// The Find and replace window and its matches.
    find: find::Find,
}

/// Warna dokumen → warna egui. Alpha dikalikan di ruang gamma, sama dengan cara egui (dan hasil
/// export) mencampur warna; `from_rgba_unmultiplied` mengalikan di ruang linear sehingga warna
/// semi-transparan tampak terlalu terang dan gradasi ke transparan tidak menyatu.
fn col(c: [u8; 4]) -> Color32 {
    let m = |v: u8| ((v as u16 * c[3] as u16 + 127) / 255) as u8;
    Color32::from_rgba_premultiplied(m(c[0]), m(c[1]), m(c[2]), c[3])
}

// ---------------------------------------------------------------- render

/// Titik pegangan resize dalam koordinat dunia.
/// Dengan Scale tool semua jenis objek (termasuk teks dan garis) punya 8 pegangan.
fn handles_local(n: &Node, scale: bool) -> Vec<(u8, f32, f32)> {
    let boxed = |n: &Node| {
        let (x0, y0, x1, y1) = n.bounds();
        HANDLE_F.iter().enumerate().map(|(i, (fx, fy))| (i as u8, x0 + (x1 - x0) * fx, y0 + (y1 - y0) * fy)).collect()
    };
    match n.kind {
        _ if scale => boxed(n),
        // Teks: tinggi mengikuti isi, jadi tanpa pegangan tengah-atas/bawah. A paragraph has
        // them: dragging one gives it a height of its own (`Node::fixed_h`).
        Kind::Text if n.wrap || n.fixed_h => boxed(n),
        Kind::Text => {
            let all: Vec<(u8, f32, f32)> = boxed(n);
            all.into_iter().filter(|h| h.0 != 1 && h.0 != 5).collect()
        }
        Kind::Line => vec![(0, n.x, n.y), (1, n.x + n.w, n.y + n.h)],
        _ => boxed(n),
    }
}

// ---------------------------------------------------------------- app

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, bench: Option<PathBuf>, open: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        theme::apply(&cc.egui_ctx);
        // Mode `--bench` memakai folder library sendiri supaya library pengguna tidak tersentuh.
        let library_dir = bench.as_ref().map_or_else(|| home::data_dir().join("library"), |out| out.with_file_name("bench-library"));
        let mut app = App {
            doc: Doc::default(),
            view: View { pan: vec2(80.0, 80.0), zoom: 1.0 },
            tool: Tool::Select,
            sel: Vec::new(),
            drag: Drag::None,
            pre_drag: None,
            undo: Vec::new(),
            redo: Vec::new(),
            fonts: FontManager::new(&cc.egui_ctx),
            editing: None,
            edit_frames: 0,
            edit_pre: None,
            path: None,
            dirty: false,
            title: String::new(),
            font_search: String::new(),
            expanded: HashSet::new(),
            group_rename: None,
            layer_drag: None,
            status: "Ready. Press F to create a frame.".to_owned(),
            last_prop: (0, 0.0),
            canvas_rect: Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 700.0)),
            fit_pending: false,
            labels: Vec::new(),
            drawn: 0,
            frame_ms: 0.0,
            pace_ms: 0.0,
            last_zoom: (1.0, 0.0),
            show_ui: true,
            wand: Default::default(),
            font_focus: false,
            ext: Default::default(),
            bench: bench.map(|out| Bench { out, frame: 0, samples: Vec::new(), shot: false, phase: 0, mark: 0, inject: Vec::new(), ids: Vec::new(), intervals: Vec::new(), last: None, perf: Default::default() }),
            home: false,
            files: home::Files::new(&cc.egui_ctx),
            rembg: None,
            exporting: None,
            remote: None,
            icons: Default::default(),
            photos: Default::default(),
            rulers: Default::default(),
            poly: Default::default(),
            pages: Default::default(),
            library: library::Library::new(library_dir),
            crop: None,
            about: false,
            components: Default::default(),
            drop_mark: None,
            find: Default::default(),
        };
        app.home = app.bench.is_none();
        if app.bench.is_none() {
            app.remote = remote::Remote::start(&cc.egui_ctx);
        }
        if let Some(p) = open {
            app.load(p);
        }
        app
    }

    // ------------------------------------------------------------ undo

    fn push_undo(&mut self, pre: Doc) {
        self.undo.push(pre);
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.touch();
    }

    /// Tandai dokumen berubah; auto-save menunggu sampai pengguna berhenti sebentar.
    fn touch(&mut self) {
        self.dirty = true;
        self.ext.rev += 1;
        self.files.version += 1;
        self.files.last_change = std::time::Instant::now();
    }

    /// Undo dari pengguna (Ctrl+Z / menu): saat menggambar dengan pen, mundur satu titik dulu.
    fn undo_cmd(&mut self) {
        if self.pen_can_undo() {
            self.pen_undo();
        } else {
            self.do_undo();
        }
    }

    fn redo_cmd(&mut self) {
        if self.pen_can_redo() {
            self.pen_redo();
        } else {
            self.do_redo();
        }
    }

    fn do_undo(&mut self) {
        if let Some(d) = self.undo.pop() {
            let page = self.doc.page_id();
            self.redo.push(std::mem::replace(&mut self.doc, d));
            self.page_changed(page);
            self.after_history();
        }
    }

    fn do_redo(&mut self) {
        if let Some(d) = self.redo.pop() {
            let page = self.doc.page_id();
            self.undo.push(std::mem::replace(&mut self.doc, d));
            self.page_changed(page);
            self.after_history();
        }
    }

    fn after_history(&mut self) {
        let doc = &self.doc;
        self.sel.retain(|id| doc.find(*id).is_some());
        // The restored document is already consistent; an undone change is not one to follow.
        self.components.rebase();
        self.touch();
    }

    // ------------------------------------------------------------ view

    fn zoom_at(&mut self, p: Pos2, factor: f32) {
        let o = self.canvas_rect.min;
        let w = self.view.to_world(o, p);
        self.view.zoom = (self.view.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        self.view.pan = vec2(p.x - o.x - w.x * self.view.zoom, p.y - o.y - w.y * self.view.zoom);
    }

    fn zoom_to(&mut self, b: (f32, f32, f32, f32), max_zoom: f32) {
        let size = self.canvas_rect.size();
        let (bw, bh) = ((b.2 - b.0).max(1.0), (b.3 - b.1).max(1.0));
        let zoom = ((size.x - 80.0).max(50.0) / bw).min((size.y - 80.0).max(50.0) / bh).clamp(MIN_ZOOM, max_zoom);
        self.view.zoom = zoom;
        self.view.pan = vec2(
            size.x * 0.5 - (b.0 + bw * 0.5) * zoom,
            size.y * 0.5 - (b.1 + bh * 0.5) * zoom,
        );
    }

    fn fit_all(&mut self) {
        match self.doc.bounds() {
            Some(b) => self.zoom_to(b, 1.0),
            None => self.view = View { pan: vec2(80.0, 80.0), zoom: 1.0 },
        }
    }

    // ------------------------------------------------------------ selection helpers

    /// Id terpilih tanpa isi frame yang juga terpilih, di kedalaman mana pun (supaya tidak digeser dua kali).
    fn roots(&self) -> Vec<u64> {
        // Satu objek (kasus paling sering, tiap frame saat menggeser): tidak perlu mencari induknya.
        if self.sel.len() < 2 {
            return self.sel.clone();
        }
        self.doc.roots_of(&self.sel)
    }

    /// Move dan Scale sama-sama bisa memilih dan menggeser objek.
    fn pointer_tool(&self) -> bool {
        matches!(self.tool, Tool::Select | Tool::Scale)
    }

    fn align(&mut self, mode: u8) {
        // The buttons beside the alignment ones.
        match mode {
            6 | 7 => return self.distribute(mode == 6),
            8 => return self.tidy_up(),
            _ => {}
        }
        if self.refuse_pinned() || self.refuse_auto_items() {
            return;
        }
        let ids = self.roots();
        // Satu objek: rata terhadap frame induknya. Banyak objek: terhadap kotak seleksi.
        let target = if ids.len() == 1 {
            self.doc.parent_of(ids[0]).and_then(|p| self.doc.find(p)).map(|n| n.bounds())
        } else {
            ids.iter()
                .filter_map(|id| self.doc.find(*id).map(|n| n.bounds()))
                .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
        };
        let Some(t) = target else { return };
        let pre = self.doc.clone();
        for id in ids {
            if let Some(n) = self.doc.find_mut(id) {
                let b = n.bounds();
                let (dx, dy) = match mode {
                    0 => (t.0 - b.0, 0.0),
                    1 => (t.2 - b.2, 0.0),
                    2 => ((t.0 + t.2 - b.0 - b.2) * 0.5, 0.0),
                    3 => (0.0, t.1 - b.1),
                    4 => (0.0, t.3 - b.3),
                    _ => (0.0, (t.1 + t.3 - b.1 - b.3) * 0.5),
                };
                n.translate(dx.round(), dy.round());
            }
        }
        self.push_undo(pre);
    }

    /// Alt+Shift+H / Alt+Shift+V: equal gaps between the selected objects (a group is one).
    fn distribute(&mut self, horizontal: bool) {
        if self.sel.is_empty() || self.sel_locked() || self.refuse_pinned() || self.refuse_auto_items() {
            return;
        }
        let pre = self.doc.clone();
        if self.doc.distribute(&self.sel, horizontal) {
            self.push_undo(pre);
        } else {
            self.status = "Select three or more objects to distribute the spacing between them.".into();
        }
    }

    /// Ctrl+Alt+T: straighten a row or a column, or snap a loose layout into a grid.
    fn tidy_up(&mut self) {
        if self.sel.is_empty() || self.sel_locked() || self.refuse_pinned() || self.refuse_auto_items() {
            return;
        }
        let pre = self.doc.clone();
        if self.doc.tidy(&self.sel) {
            self.push_undo(pre);
        } else {
            self.status = "Select two or more objects to tidy up.".into();
        }
    }

    /// Shift+H / Shift+V: mirror the selection about the centre of the box around it.
    fn flip(&mut self, horizontal: bool) {
        if self.sel.is_empty() || self.sel_locked() {
            return;
        }
        let pre = self.doc.clone();
        if self.sel_pinned() {
            // A part of an instance stays where the main has it: only what it shows is mirrored.
            let stuck = |n: &Node| n.source.is_none() || matches!(n.kind, Kind::Path | Kind::Line | Kind::Frame);
            if self.sel.iter().any(|id| self.doc.find(*id).is_some_and(stuck)) {
                self.status = "Inside an instance only shapes, images and text can be flipped, each in place. Flip the whole instance, or edit the main component.".into();
                return;
            }
            for id in self.sel.clone() {
                if let Some(n) = self.doc.find_mut(id) {
                    n.mirror_look(horizontal);
                }
            }
        } else if !self.doc.flip(&self.sel, horizontal) {
            return;
        }
        self.push_undo(pre);
    }

    fn edit_selected(&mut self, f: impl Fn(&mut Node)) {
        if self.sel.is_empty() {
            return;
        }
        let pre = self.doc.clone();
        for id in self.sel.clone() {
            if let Some(n) = self.doc.find_mut(id) {
                f(n);
            }
        }
        self.push_undo(pre);
    }

    /// Underline (`strike`: strikethrough) on or off: for the characters selected in the text
    /// editor, or else for every selected text as a whole. It goes on unless all of it has it.
    fn toggle_decoration(&mut self, strike: bool) {
        let range = self.ext.edit_sel.filter(|(a, b)| a != b && self.editing.is_some());
        let ids: Vec<u64> = match self.editing {
            Some(id) => vec![id],
            None => self.sel.clone(),
        };
        let texts: Vec<u64> = ids.into_iter().filter(|id| self.doc.find(*id).is_some_and(|n| n.kind == Kind::Text)).collect();
        if texts.is_empty() {
            return;
        }
        let (a, b) = range.unwrap_or((0, 0));
        let on = !texts.iter().filter_map(|id| self.doc.find(*id)).all(|n| n.decorated(a, b, strike));
        let pre = self.doc.clone();
        for id in texts {
            if let Some(n) = self.doc.find_mut(id) {
                if n.decorated(a, b, strike) != on {
                    n.toggle_decoration(a, b, strike);
                }
            }
        }
        self.push_undo(pre);
        self.touch();
        if self.editing.is_some() {
            self.ext.edit_refocus = true;
        }
    }

    fn zoom_100(&mut self) {
        let c = self.canvas_rect.center();
        let f = 1.0 / self.view.zoom;
        self.zoom_at(c, f);
    }

    fn zoom_selection(&mut self) {
        let b = self
            .sel
            .iter()
            .filter_map(|id| self.doc.find(*id).map(|n| n.bounds()))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
        if let Some(b) = b {
            self.zoom_to(b, 8.0);
        }
    }

    fn step_order(&mut self, up: bool) {
        if self.sel.is_empty() || self.refuse_pinned() {
            return;
        }
        let pre = self.doc.clone();
        self.doc.reorder_step(&self.sel, up);
        self.push_undo(pre);
    }

    /// Layer ini terkunci, sendiri atau lewat salah satu frame yang memuatnya.
    fn locked(&self, id: u64) -> bool {
        self.doc.lineage(id).iter().any(|n| n.locked)
    }

    /// Ada layer terkunci di dalam seleksi (seleksinya dibuat dari panel layer).
    fn sel_locked(&self) -> bool {
        self.sel.iter().any(|id| self.locked(*id))
    }

    /// Ctrl+Shift+L: kunci seleksi; kalau semuanya sudah terkunci, buka kuncinya.
    fn toggle_lock(&mut self) {
        if self.sel.is_empty() {
            return;
        }
        let lock = self.sel.iter().any(|id| self.doc.find(*id).is_some_and(|n| !n.locked));
        self.edit_selected(|n| n.locked = lock);
        self.status = if lock { "Locked. Unlock from the Layers panel or with Ctrl+Shift+L.".into() } else { "Unlocked.".into() };
    }

    /// What a click at this spot selects. Inside a top-level frame that is its direct child under
    /// the pointer (a nested frame as a whole); when the selection already sits inside a nested
    /// frame, the children of that frame (then of the nested frames around it) come first.
    fn hit(&self, p: Pos2, wp: Pos2, tol: f32) -> Option<u64> {
        let label = || self.labels.iter().rev().find(|(id, r)| r.contains(p) && !self.locked(*id)).map(|(id, _)| *id);
        if let Some(first) = self.sel.first() {
            let around = self.doc.ancestors(*first);
            // The last one is the top-level frame, which the usual rule below covers, unless it
            // is an instance: a plain click picks that as a whole.
            let whole = around.last().is_some_and(|top| self.doc.find(*top).is_some_and(|n| n.instance_of.is_some()));
            for scope in around.iter().take(around.len().saturating_sub(!whole as usize)) {
                if let Some(id) = self.doc.hit_within(*scope, wp.x, wp.y, tol) {
                    return Some(id);
                }
            }
        }
        match self.doc.hit(wp.x, wp.y, tol) {
            // Seperti Figma: badan frame yang berisi objek tidak bisa dipegang, hanya labelnya.
            Some(id) if self.body_locked(id) => label(),
            Some(id) => Some(id),
            None => label(),
        }
    }

    /// Frame level atas yang sudah berisi objek: badannya tidak memilih/menggeser frame (pakai label
    /// namanya). Frame yang terlalu kecil di layar untuk punya label tetap bisa dipegang dari badannya,
    /// begitu juga frame di dalam frame (tidak punya label).
    fn body_locked(&self, id: u64) -> bool {
        self.doc.nodes.iter().find(|n| n.id == id).is_some_and(|n| {
            n.kind == Kind::Frame && n.instance_of.is_none() && n.children.iter().any(|c| c.visible) && n.w * self.view.zoom > 36.0
        })
    }

    fn handle_at(&self, p: Pos2) -> Option<(u64, u8)> {
        if self.sel.len() != 1 || !self.pointer_tool() || self.sel_locked() || self.sel_pinned() || self.crop.is_some() {
            return None;
        }
        let n = self.doc.find(self.sel[0])?;
        let o = self.canvas_rect.min;
        handles(n, self.tool == Tool::Scale).into_iter().find_map(|(i, x, y)| {
            let hp = self.view.to_screen(o, x, y);
            ((hp.x - p.x).abs() <= 6.0 && (hp.y - p.y).abs() <= 6.0).then_some((n.id, i))
        })
    }

    /// Kotak pembatas seleksi kalau yang terpilih lebih dari satu objek (untuk pegangan resize bersama).
    fn sel_box(&self) -> Option<(f32, f32, f32, f32)> {
        if self.sel.len() < 2 || !self.pointer_tool() || self.editing.is_some() || self.ext.path_edit.is_some() {
            return None;
        }
        let ids = self.roots();
        if ids.len() < 2 {
            return None;
        }
        ids.iter()
            .filter_map(|id| self.doc.find(*id).map(|n| n.aabb()))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)))
    }

    fn multi_handle_at(&self, p: Pos2) -> Option<u8> {
        if self.sel_locked() || self.sel_pinned() {
            return None;
        }
        let (x0, y0, x1, y1) = self.sel_box()?;
        let o = self.canvas_rect.min;
        HANDLE_F.iter().enumerate().find_map(|(i, (fx, fy))| {
            let hp = self.view.to_screen(o, x0 + (x1 - x0) * fx, y0 + (y1 - y0) * fy);
            ((hp.x - p.x).abs() <= 6.0 && (hp.y - p.y).abs() <= 6.0).then_some(i as u8)
        })
    }

    // ------------------------------------------------------------ group

    /// Node ini beserta semua anggota group-nya (atau hanya node ini kalau tidak digroup).
    fn with_group(&self, id: u64) -> Vec<u64> {
        match self.doc.find(id).map(|n| n.group) {
            Some(g) if g != 0 => self.doc.group_members(&[g]),
            _ => vec![id],
        }
    }

    fn selected_groups(&self) -> Vec<u64> {
        let mut groups: Vec<u64> =
            self.sel.iter().filter_map(|id| self.doc.find(*id)).map(|n| n.group).filter(|g| *g != 0).collect();
        groups.sort_unstable();
        groups.dedup();
        groups
    }

    /// Id group kalau seleksi persis berisi semua anggota satu group.
    fn selected_whole_group(&self) -> Option<u64> {
        let groups = self.selected_groups();
        let [g] = groups[..] else { return None };
        let members = self.doc.group_members(&[g]);
        (members.len() == self.sel.len() && members.iter().all(|id| self.sel.contains(id))).then_some(g)
    }

    /// Ganti nama group; ketikan beruntun digabung jadi satu langkah undo.
    fn set_group_name(&mut self, g: u64, name: String, now: f64) {
        if self.doc.group_name(g) == name {
            return;
        }
        if self.last_prop.0 != g || now - self.last_prop.1 > 0.8 {
            let pre = self.doc.clone();
            self.push_undo(pre);
        }
        self.last_prop = (g, now);
        self.doc.group_names.insert(g, name);
        self.touch();
    }

    fn group_selection(&mut self) {
        let ids = self.roots();
        if ids.len() < 2 {
            self.status = "Select two or more objects to group.".into();
            return;
        }
        if self.refuse_pinned() {
            return;
        }
        let pre = self.doc.clone();
        self.doc.group(&ids);
        self.sel = ids;
        self.push_undo(pre);
        self.status = format!("Grouped {} objects. Ctrl+Shift+G to ungroup.", self.sel.len());
    }

    fn ungroup_selection(&mut self) {
        let groups = self.selected_groups();
        if groups.is_empty() || self.refuse_pinned() {
            return;
        }
        let pre = self.doc.clone();
        self.doc.ungroup(&groups);
        self.push_undo(pre);
        self.status = "Ungrouped.".into();
    }

    /// Objek terpilih yang bisa ikut clipping mask (semua kecuali frame).
    fn clip_targets(&self) -> Vec<u64> {
        self.sel.iter().copied().filter(|id| self.doc.find(*id).is_some_and(|n| n.kind != Kind::Frame)).collect()
    }

    /// Buat clipping mask dari seleksi, atau lepas kalau semuanya sudah di-clip.
    fn toggle_clip(&mut self) {
        let ids = self.clip_targets();
        if ids.is_empty() {
            self.status = "Select a layer (not a frame) to make a clipping mask.".into();
            return;
        }
        if self.refuse_pinned() {
            return;
        }
        let pre = self.doc.clone();
        if ids.iter().all(|id| self.doc.find(*id).is_some_and(|n| n.clip)) {
            self.doc.release_clip(&ids);
            self.status = "Clipping mask released.".into();
        } else if self.doc.make_clip(&ids) {
            self.status = "Clipping mask created: the layer shows only inside the layer below it.".into();
        } else {
            self.status = "Nothing to clip to: put the layer above another layer first.".into();
            return;
        }
        self.push_undo(pre);
    }

    fn marquee_pick(&self, a: Pos2, b: Pos2) -> Vec<u64> {
        let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
        let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
        let inter = |n: &Node| {
            let q = n.bounds();
            n.visible && !n.locked && q.0 <= x1 && q.2 >= x0 && q.1 <= y1 && q.3 >= y0
        };
        // Marquee sepenuhnya di dalam satu frame: pilih isi frame itu.
        let inside = self.doc.nodes.iter().rev().find(|n| {
            let q = n.bounds();
            n.kind == Kind::Frame && n.visible && q.0 <= x0 && q.1 <= y0 && q.2 >= x1 && q.3 >= y1
        });
        let picked: Vec<&Node> = match inside {
            // Frame terkunci: isinya tidak ikut terpilih.
            Some(f) if f.locked => Vec::new(),
            Some(f) => f.children.iter().filter(|c| inter(c)).collect(),
            None => self.doc.nodes.iter().map(|n| &**n).filter(|n| inter(n)).collect(),
        };
        let mut ids: Vec<u64> = picked.iter().map(|n| n.id).collect();
        // Menyentuh satu anggota group memilih seluruh group.
        let mut groups: Vec<u64> = picked.iter().map(|n| n.group).filter(|g| *g != 0).collect();
        groups.sort_unstable();
        groups.dedup();
        for id in self.doc.group_members(&groups) {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        ids
    }

    // ------------------------------------------------------------ editing ops

    fn delete_selection(&mut self) {
        if self.sel.is_empty() || self.refuse_pinned() {
            return;
        }
        let pre = self.doc.clone();
        for id in std::mem::take(&mut self.sel) {
            self.doc.remove(id);
        }
        self.push_undo(pre);
    }

    /// Salinan node terpilih dalam urutan tumpukan dokumen (bawah ke atas), bukan urutan klik:
    /// salin, duplikat dan tempel menyisipkan satu per satu ke atas, jadi urutan ini yang
    /// menjaga objek atas tetap di atas.
    fn selected_nodes(&self) -> Vec<Node> {
        let mut ids = self.roots();
        self.doc.sort_by_stack(&mut ids);
        ids.into_iter().filter_map(|id| self.doc.find(id).cloned()).collect()
    }

    /// Isi clipboard untuk seleksi saat ini: node-nodenya plus pojok frame asalnya.
    fn clip_json(&self) -> Option<String> {
        let nodes = self.selected_nodes();
        if nodes.is_empty() {
            return None;
        }
        // Semua objek dari satu frame yang sama: catat pojok frame itu.
        let mut parents: Vec<Option<u64>> = self.roots().into_iter().map(|id| self.doc.parent_of(id)).collect();
        parents.dedup();
        let origin = match parents[..] {
            [Some(p)] => self.doc.find(p).map(|f| {
                let b = f.bounds();
                [b.0, b.1]
            }),
            _ => None,
        };
        serde_json::to_string(&serde_json::json!({ CLIP_TAG: nodes, CLIP_ORIGIN: origin })).ok()
    }

    fn copy(&self, ctx: &egui::Context) {
        if let Some(json) = self.clip_json() {
            ctx.copy_text(json);
        }
    }

    fn cut(&mut self, ctx: &egui::Context) {
        self.copy(ctx);
        self.delete_selection();
    }

    /// Posisi pointer kalau sedang di atas canvas kosong (bukan di atas panel, menu, atau jendela).
    fn canvas_pointer(&self, ctx: &egui::Context) -> Option<Pos2> {
        let p = ctx.input(|i| i.pointer.latest_pos())?;
        let bare = ctx.layer_id_at(p).is_some_and(|l| l.order == egui::Order::Background);
        (bare && self.canvas_rect.contains(p)).then_some(p)
    }

    /// Frame tujuan Paste in Place: frame di bawah pointer, kalau tidak ada frame dari seleksi.
    fn paste_frame(&self, at: Option<Pos2>) -> Option<u64> {
        let hovered = at.and_then(|p| {
            let w = self.view.to_world(self.canvas_rect.min, p);
            self.doc.frame_at(w.x, w.y, &[])
        });
        hovered.or_else(|| {
            let id = *self.sel.first()?;
            match self.doc.find(id)?.kind {
                Kind::Frame => Some(id),
                _ => self.doc.parent_of(id),
            }
        })
    }

    /// Tempel isi clipboard. `at` = posisi pointer di canvas (layar); tanpa itu, tengah layar.
    /// Biasa: objek dipusatkan di `at`. `in_place`: posisi yang sama seperti aslinya, relatif
    /// terhadap frame tujuan (frame di bawah pointer atau frame terpilih).
    fn paste_text(&mut self, text: &str, in_place: bool, at: Option<Pos2>) {
        let mut value = serde_json::from_str::<serde_json::Value>(text).ok();
        let origin = value.as_ref().and_then(|v| v.get(CLIP_ORIGIN)).and_then(|o| serde_json::from_value::<[f32; 2]>(o.clone()).ok());
        let parsed = value
            .as_mut()
            .and_then(|v| v.get_mut(CLIP_TAG).map(serde_json::Value::take))
            .and_then(|v| serde_json::from_value::<Vec<Node>>(v).ok());
        let c = self.view.to_world(self.canvas_rect.min, at.unwrap_or(self.canvas_rect.center()));
        match parsed {
            Some(nodes) if nodes.is_empty() => {}
            Some(nodes) if in_place => {
                let target = self.paste_frame(at);
                let corner = target.and_then(|id| self.doc.find(id)).map(|f| f.bounds());
                match (origin, corner) {
                    (Some(o), Some(b)) => {
                        self.status = "Pasted in place, at the same position in this frame.".into();
                        self.add_nodes(nodes, vec2(b.0 - o[0], b.1 - o[1]), target);
                    }
                    _ => {
                        self.status = "Pasted in place.".into();
                        self.add_nodes(nodes, Vec2::ZERO, None);
                    }
                }
            }
            Some(nodes) => {
                // Pusat kotak pembatas semua objek jatuh di posisi pointer.
                let b = nodes.iter().map(|n| n.aabb()).fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |a, q| (a.0.min(q.0), a.1.min(q.1), a.2.max(q.2), a.3.max(q.3)));
                let d = vec2((c.x - (b.0 + b.2) * 0.5).round(), (c.y - (b.1 + b.3) * 0.5).round());
                self.add_nodes(nodes, d, None);
            }
            // SVG markup: editable layers.
            None if self.paste_svg(text, at) => {}
            None => {
                // Teks biasa dari clipboard: jadikan node teks di posisi pointer.
                let mut n = Node::new(0, Kind::Text, c.x.round(), c.y.round());
                n.text = text.chars().take(5000).collect();
                n.name = "Text".into();
                n.fill = Some(if self.doc.frame_at(c.x, c.y, &[]).is_some() { [0, 0, 0, 255] } else { [235, 235, 235, 255] });
                n.w = 100.0;
                n.h = 20.0;
                self.add_nodes(vec![n], Vec2::ZERO, None);
            }
        }
    }

    /// Sisipkan salinan node, digeser `offset`. `parent` memaksa frame induknya; tanpa itu
    /// induknya frame terdalam yang memuat titik tengah node. Frame hanya masuk ke frame yang
    /// lebih besar darinya (lihat `Doc::frame_for`).
    fn add_nodes(&mut self, mut nodes: Vec<Node>, offset: Vec2, parent: Option<u64>) {
        if nodes.is_empty() {
            return;
        }
        let pre = self.doc.clone();
        self.doc.regroup(&mut nodes);
        self.sel.clear();
        for mut n in nodes {
            let source = n.id;
            // A copy of a main component is an instance of it (see `Doc::settle_copy`).
            if !self.doc.settle_copy(&mut n) {
                self.doc.reassign_ids(&mut n);
            }
            n.translate(offset.x, offset.y);
            let (cx, cy) = n.center();
            let roomy = |p: &u64| !self.doc.in_instance(*p) && (n.kind != Kind::Frame || self.doc.find(*p).is_some_and(|f| n.w < f.w && n.h < f.h));
            let parent = parent.filter(roomy).or_else(|| self.doc.frame_for(&n, cx, cy, &[]));
            // Where it would come to contain itself, it goes to the top level instead.
            let parent = parent.filter(|_| self.may_enter(&n, parent));
            self.sel.push(n.id);
            // In an auto layout it is an item: right after the one it was copied from, or else
            // where it was put.
            let at = parent.and_then(|p| self.doc.find(p)).filter(|f| f.is_auto()).map(|f| match f.children.iter().position(|c| c.id == source) {
                Some(i) => i + 1,
                None => crate::layout::insert_index(f, &[], cx, cy),
            });
            match at {
                Some(at) => self.doc.insert_at(n, parent, at),
                None => self.doc.insert(n, parent),
            }
        }
        self.push_undo(pre);
    }

    fn nudge(&mut self, dx: f32, dy: f32) {
        let ids = self.roots();
        if ids.is_empty() || self.sel_locked() || self.refuse_pinned() || self.swap_items(dx, dy) {
            return;
        }
        let pre = self.doc.clone();
        for id in ids {
            if let Some(n) = self.doc.find_mut(id) {
                n.translate(dx, dy);
            }
        }
        self.push_undo(pre);
    }

    fn reorder(&mut self, to_front: bool) {
        if self.sel.is_empty() || self.refuse_pinned() {
            return;
        }
        let pre = self.doc.clone();
        self.doc.reorder(&self.sel, to_front);
        self.push_undo(pre);
    }

    fn start_text_edit(&mut self, id: u64, pre: Doc) {
        self.editing = Some(id);
        self.edit_frames = 0;
        self.edit_pre = Some(pre);
        self.sel = vec![id];
        if self.doc.find(id).is_some_and(|n| n.flips() != (false, false)) {
            self.status = "This text is flipped: it reads the normal way while you edit it, and is mirrored again when you are done.".into();
        }
    }

    fn finish_text_edit(&mut self) {
        let Some(id) = self.editing.take() else { return };
        let pre = self.edit_pre.take();
        let empty = self.doc.find(id).map_or(true, |n| n.text.trim().is_empty());
        if empty && self.doc.pinned(id) {
            // A part of an instance can't be removed: emptied, it gets its text back.
            if let (Some(old), Some(n)) = (pre.as_ref().and_then(|d| d.find(id)).cloned(), self.doc.find_mut(id)) {
                *n = old;
            }
        } else if empty {
            self.doc.remove(id);
            self.sel.retain(|s| *s != id);
        }
        if let Some(pre) = pre {
            let changed = pre.find(id).map(|n| &n.text) != self.doc.find(id).map(|n| &n.text);
            if changed {
                self.push_undo(pre);
            }
        }
        self.tool = Tool::Select;
    }

    // ------------------------------------------------------------ file ops

    /// Boleh meninggalkan dokumen ini? File yang sudah punya lokasi disimpan otomatis;
    /// hanya dokumen yang belum pernah disimpan yang perlu konfirmasi.
    fn confirm_discard(&mut self) -> bool {
        if self.path.is_some() {
            return self.settle();
        }
        if !self.dirty {
            return true;
        }
        rfd::MessageDialog::new()
            .set_title("Kanvas")
            .set_description("You have unsaved changes. Continue without saving?")
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
    }

    fn reset_session(&mut self) {
        self.sel.clear();
        self.undo.clear();
        self.redo.clear();
        self.editing = None;
        self.drag = Drag::None;
        self.dirty = false;
        self.fit_pending = true;
        self.components.rebase();
        self.pages.reset();
        self.find.reset();
        self.warm_fonts();
    }

    /// Ask for every font the document uses as soon as it is opened. Fonts are otherwise loaded
    /// when their text is first drawn, and each arrival rebuilds the font atlas: zooming into a
    /// document for the first time stuttered once per font.
    fn warm_fonts(&mut self) {
        let fonts = &mut self.fonts;
        self.doc.walk_all(|n| {
            if n.kind == Kind::Text {
                for (_, _, st) in n.segments(n.text.chars().count()) {
                    fonts.resolve(&st.font, st.weight, st.italic);
                }
            }
        });
    }

    fn new_doc(&mut self) {
        if self.confirm_discard() {
            self.doc = Doc::default();
            self.path = None;
            self.reset_session();
            self.status = "New document.".into();
        }
    }

    fn open_dialog(&mut self) {
        if !self.confirm_discard() {
            return;
        }
        if let Some(p) = rfd::FileDialog::new().add_filter("Kanvas", &["kanvas"]).pick_file() {
            self.load(p);
        }
    }

    /// Hasil true = dokumen tersimpan (false juga kalau dialog lokasi dibatalkan).
    fn save(&mut self, save_as: bool) -> bool {
        if !save_as && self.path.is_some() && self.bench.is_none() {
            // The file already has a location: written in the background, like auto-save.
            self.save_soon();
            return true;
        }
        let path = match (&self.path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new().add_filter("Kanvas", &["kanvas"]).set_file_name("Untitled.kanvas").save_file(),
        };
        let Some(path) = path else { return false };
        match self.write_doc(&path) {
            Ok(()) => {
                self.status = format!("Saved: {}", path.display());
                true
            }
            Err(e) => {
                self.status = format!("Couldn't save: {e}");
                false
            }
        }
    }

    /// Isi dokumen dengan 500 frame (±8.000 objek) untuk menguji performa.
    fn stress_test(&mut self) {
        const FONTS: [&str; 6] = ["Inter", "Poppins", "Playfair Display", "Space Grotesk", "Roboto Mono", "Lora"];
        const COLORS: [[u8; 4]; 8] = [
            [13, 153, 255, 255],
            [255, 99, 71, 255],
            [20, 174, 92, 255],
            [151, 71, 255, 255],
            [255, 205, 41, 255],
            [242, 78, 30, 255],
            [0, 181, 206, 255],
            [255, 36, 189, 255],
        ];
        let pre = self.doc.clone();
        let by = self.doc.bounds().map_or(0.0, |b| b.3 + 300.0);
        let text = |doc: &mut Doc, s: &str, x: f32, y: f32, size: f32, weight: u16, font: &str, fill: [u8; 4]| {
            let mut t = Node::new(doc.alloc_id(), Kind::Text, x, y);
            t.name = s.lines().next().unwrap_or("Text").to_owned();
            t.text = s.to_owned();
            t.font = font.to_owned();
            t.weight = weight;
            t.font_size = size;
            t.fill = Some(fill);
            t.w = s.lines().map(|l| l.chars().count()).max().unwrap_or(1) as f32 * size * 0.55;
            t.h = s.lines().count().max(1) as f32 * size * 1.2;
            t
        };
        let shape = |doc: &mut Doc, kind: Kind, x: f32, y: f32, w: f32, h: f32, fill: [u8; 4], radius: f32| {
            let mut n = Node::new(doc.alloc_id(), kind, x, y);
            n.w = w;
            n.h = h;
            n.fill = Some(fill);
            n.radius = radius;
            n
        };
        for i in 0..500usize {
            let (c, r) = ((i % 25) as f32, (i / 25) as f32);
            let (x, y) = (c * 470.0, by + r * 964.0);
            let accent = COLORS[i % COLORS.len()];
            let font = FONTS[i % FONTS.len()];
            let mut f = Node::new(self.doc.alloc_id(), Kind::Frame, x, y);
            f.name = format!("Screen {}", i + 1);
            f.w = 390.0;
            f.h = 844.0;
            f.fill = Some([255, 255, 255, 255]);
            let d = &mut self.doc;
            f.children.push(shape(d, Kind::Rect, x, y, 390.0, 104.0, accent, 0.0));
            f.children.push(text(d, &format!("Screen {}", i + 1), x + 24.0, y + 40.0, 26.0, 700, font, [255, 255, 255, 255]));
            f.children.push(shape(d, Kind::Ellipse, x + 318.0, y + 28.0, 48.0, 48.0, [255, 255, 255, 230], 0.0));
            for k in 0..3 {
                let cy = y + 132.0 + k as f32 * 196.0;
                f.children.push(shape(d, Kind::Rect, x + 24.0, cy, 342.0, 176.0, [242, 243, 245, 255], 16.0));
                f.children.push(text(d, &format!("Card {} · {}", k + 1, font), x + 44.0, cy + 22.0, 18.0, 700, font, [20, 20, 20, 255]));
                f.children.push(text(
                    d,
                    "Infinite canvas, hundreds of frames,\nstill light and lag-free.",
                    x + 44.0,
                    cy + 58.0,
                    14.0,
                    400,
                    font,
                    [90, 90, 90, 255],
                ));
            }
            let mut line = Node::new(d.alloc_id(), Kind::Line, x + 24.0, y + 728.0);
            line.w = 342.0;
            line.stroke = Some([220, 220, 220, 255]);
            line.stroke_w = 1.0;
            f.children.push(line);
            f.children.push(shape(d, Kind::Rect, x + 24.0, y + 752.0, 342.0, 56.0, accent, 28.0));
            f.children.push(text(d, "Continue", x + 152.0, y + 770.0, 17.0, 700, font, [255, 255, 255, 255]));
            self.doc.insert(f, None);
        }
        self.push_undo(pre);
        self.fit_pending = true;
        self.status = "Added 500 frames. Try zooming and panning.".into();
    }

    // ------------------------------------------------------------ shortcuts

    fn shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = Modifiers::COMMAND;
        // Ctrl+Shift+O (outline stroke) before Ctrl+O, whose pattern would match it as well.
        let (outline, save_as, save, open, new) = ctx.input_mut(|i| {
            (
                i.consume_key(cmd | Modifiers::SHIFT, Key::O),
                i.consume_key(cmd | Modifiers::SHIFT, Key::S),
                i.consume_key(cmd, Key::S),
                i.consume_key(cmd, Key::O),
                i.consume_key(cmd, Key::N),
            )
        });
        if save_as {
            self.save(true);
        } else if save {
            self.save(false);
        }
        if open {
            self.open_dialog();
        }
        if new {
            self.new_doc();
        }
        if outline && !ctx.wants_keyboard_input() && self.editing.is_none() && self.ext.picker.eyedrop.is_none() {
            self.outline_stroke();
        }

        // Eyedropper aktif: Esc membatalkan, shortcut lain ditahan supaya layar tetap sama dengan tangkapannya.
        if self.ext.picker.eyedrop.is_some() {
            if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                self.ext.picker.eyedrop = None;
            }
            return;
        }

        // Ctrl+F find, Ctrl+H replace: also while typing (in a text on the canvas, or in the
        // window itself, to move between its fields), so they are taken before any text field
        // sees them (where Ctrl+H would delete a character). Without Shift or Alt only, which
        // `consume_key` does not tell apart: Ctrl+Shift+H is show / hide.
        let (find, replace) = ctx.input_mut(|i| {
            let mut take = |key: Key| {
                let before = i.events.len();
                i.events.retain(|e| !matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && m.command && !m.shift && !m.alt));
                i.events.len() != before
            };
            (take(Key::F), take(Key::H))
        });
        if find || replace {
            self.open_find(replace);
        }

        // Ctrl+U underline, Ctrl+Shift+X strikethrough: also while a text is being edited (then
        // for the selected characters), so they are taken before the editor sees them (where
        // Ctrl+U would delete to the start of the line). Ctrl+Shift+X arrives as a cut event.
        if self.editing.is_some() || !ctx.wants_keyboard_input() {
            let (underline, strike) = ctx.input_mut(|i| {
                let underline = i.consume_key(cmd, Key::U);
                let cut_keys = i.modifiers.command && i.modifiers.shift && !i.modifiers.alt;
                let before = i.events.len();
                if cut_keys {
                    i.events.retain(|e| !matches!(e, egui::Event::Cut));
                }
                (underline, i.events.len() != before || i.consume_key(cmd | Modifiers::SHIFT, Key::X))
            });
            if underline {
                self.toggle_decoration(false);
            }
            if strike {
                self.toggle_decoration(true);
            }
        }

        // Sisanya hanya berlaku kalau tidak sedang mengetik.
        if ctx.wants_keyboard_input() || self.editing.is_some() {
            return;
        }
        let (component, detach, clip_mask, redo, undo, dup, all, ungroup, group) = ctx.input_mut(|i| {
            (
                i.consume_key(cmd | Modifiers::ALT, Key::K),
                i.consume_key(cmd | Modifiers::ALT, Key::B),
                // Sebelum Ctrl+G: pola tanpa Alt juga cocok dengan Ctrl+Alt+G.
                i.consume_key(cmd | Modifiers::ALT, Key::G),
                i.consume_key(cmd | Modifiers::SHIFT, Key::Z) || i.consume_key(cmd, Key::Y),
                i.consume_key(cmd, Key::Z),
                i.consume_key(cmd, Key::D),
                i.consume_key(cmd, Key::A),
                i.consume_key(cmd | Modifiers::SHIFT, Key::G),
                i.consume_key(cmd, Key::G),
            )
        });
        if component {
            self.create_component();
        } else if detach {
            self.detach_selection();
        }
        if clip_mask {
            self.toggle_clip();
        } else if ungroup {
            self.ungroup_selection();
        } else if group {
            self.group_selection();
        }
        if (redo || undo) && self.crop.is_some() {
            // Mode crop: undo membatalkan crop yang sedang dikerjakan.
            self.finish_crop(false);
        } else if redo {
            self.redo_cmd();
        } else if undo {
            self.undo_cmd();
        }
        if dup {
            let nodes = self.selected_nodes();
            self.add_nodes(nodes, vec2(24.0, 24.0), None);
        }
        if all && self.ext.path_edit.is_some() {
            self.path_edit_select_all();
        } else if all {
            self.sel = self.doc.nodes.iter().filter(|n| !n.locked).map(|n| n.id).collect();
        }
        // Salin / potong / tempel datang sebagai event, bukan tombol.
        let mut clip: Vec<(u8, String)> = Vec::new();
        // Ctrl+Shift+V juga datang sebagai event Paste: Shift yang membedakan Paste in Place.
        let in_place = ctx.input(|i| i.modifiers.command && i.modifiers.shift);
        ctx.input(|i| {
            for e in &i.events {
                match e {
                    egui::Event::Copy => clip.push((0, String::new())),
                    egui::Event::Cut => clip.push((1, String::new())),
                    egui::Event::Paste(t) => clip.push((2, t.clone())),
                    _ => {}
                }
            }
        });
        for (kind, text) in clip {
            match kind {
                0 => self.copy(ctx),
                1 => {
                    self.copy(ctx);
                    self.delete_selection();
                }
                _ => {
                    let at = self.canvas_pointer(ctx);
                    self.paste_text(&text, in_place, at);
                }
            }
        }

        // Shortcut mengikuti kebiasaan Figma.
        let (keys, mods) = ctx.input(|i| {
            let keys: Vec<Key> = i
                .events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, .. } => Some(*key),
                    _ => None,
                })
                .collect();
            (keys, i.modifiers)
        });
        let (c, s, a) = (mods.command, mods.shift, mods.alt);
        let plain = !c && !s && !a;
        let step = if s { 10.0 } else { 1.0 };
        let center = self.canvas_rect.center();
        for key in keys {
            match key {
                // Pen: Enter atau Esc menyelesaikan path yang sedang digambar
                Key::Enter | Key::Escape if self.ext.pen.is_some() => self.finish_pen(false),
                Key::Backspace | Key::Delete if self.ext.pen.is_some() => self.pen_backspace(),
                // Mode edit titik path
                Key::Enter | Key::Escape if self.ext.path_edit.is_some() => self.ext.path_edit = None,
                Key::Backspace | Key::Delete if self.ext.path_edit.is_some() => self.path_edit_delete(),
                // Mode crop: Enter menerapkan, Esc membatalkan
                Key::Enter if self.crop.is_some() => self.finish_crop(true),
                Key::Escape if self.crop.is_some() => self.finish_crop(false),
                Key::Backspace | Key::Delete if self.crop.is_some() => {}
                // Tool
                Key::V if plain => self.tool = Tool::Select,
                Key::K if plain => self.tool = Tool::Scale,
                Key::Y if plain => self.tool = Tool::Wand,
                Key::H if plain => self.tool = Tool::Hand,
                Key::F if plain => self.tool = Tool::Frame,
                // Shift+R penggaris dan guide
                Key::R if s && !c && !a => self.toggle_rulers(),
                Key::R if plain => self.tool = Tool::Rect,
                Key::O if plain => self.tool = Tool::Ellipse,
                Key::L if plain => self.tool = Tool::Line,
                Key::T if plain => self.tool = Tool::Text,
                Key::P if s && !c && !a => self.tool = Tool::Polygon,
                Key::P if plain => self.tool = Tool::Pen,
                Key::M if s && !c && !a => self.tool = Tool::ShapeBuilder,
                Key::I if plain => self.toggle_eyedropper(),
                // Rata: Alt+A/D kiri/kanan, Alt+W/S atas/bawah, Alt+H/V tengah
                // Alt+Shift+H/V jarak sama rata, Ctrl+Alt+T rapikan, Shift+H/V balik
                // Shift+A auto layout, Alt+Shift+A lepas
                Key::A if a && s && !c => self.remove_auto_layout(),
                Key::A if s && !c && !a => self.add_auto_layout(),
                Key::H if a && s && !c => self.distribute(true),
                Key::V if a && s && !c => self.distribute(false),
                Key::T if c && a => self.tidy_up(),
                Key::H if s && !c && !a => self.flip(true),
                Key::V if s && !c && !a => self.flip(false),
                Key::A if a && !c => self.align(0),
                Key::D if a && !c => self.align(1),
                Key::H if a && !c => self.align(2),
                Key::W if a && !c => self.align(3),
                Key::S if a && !c => self.align(4),
                Key::V if a && !c => self.align(5),
                // Shift+X tukar fill dan stroke
                Key::X if s && !c && !a => self.edit_selected(|n| {
                    std::mem::swap(&mut n.fill, &mut n.stroke);
                    if n.stroke.is_some() && n.stroke_w <= 0.0 {
                        n.stroke_w = 1.0;
                    }
                }),
                // Ctrl+Shift+H tampil/sembunyi
                Key::H if c && s => self.edit_selected(|n| n.visible = !n.visible),
                // Ctrl+Shift+L kunci/buka kunci
                Key::L if c && s => self.toggle_lock(),
                // Ctrl+\ tampil/sembunyikan panel
                Key::Backslash if c => self.show_ui = !self.show_ui,
                // Shift+Enter: pilih frame yang memuat seleksi (satu tingkat ke luar)
                Key::Enter if s && !c && !a && !self.sel.is_empty() => {
                    let mut parents: Vec<u64> = self.sel.iter().filter_map(|id| self.doc.parent_of(*id)).collect();
                    parents.sort_unstable();
                    parents.dedup();
                    if !parents.is_empty() {
                        self.sel = parents;
                    }
                }
                // Enter: edit teks terpilih
                Key::Enter if plain && self.sel.len() == 1 => {
                    let id = self.sel[0];
                    if self.doc.find(id).is_some_and(|n| n.kind == Kind::Text) {
                        let pre = self.doc.clone();
                        self.start_text_edit(id, pre);
                        // Seperti Figma: Enter masuk mode edit dengan seluruh teks terseleksi.
                        self.ext.edit_pending = Some(render::EditPending::All);
                    } else if self.doc.find(id).is_some_and(|n| n.kind == Kind::Path) {
                        // The points of a part of an instance are those of the main.
                        if !self.refuse_pinned() {
                            self.start_path_edit(id);
                        }
                    } else if let Some(f) = self.doc.find(id).filter(|n| n.kind == Kind::Frame) {
                        // Frame: Enter memilih isinya (satu tingkat ke dalam), seperti Figma.
                        let inside: Vec<u64> = f.children.iter().filter(|c| c.visible && !c.locked).map(|c| c.id).collect();
                        if !inside.is_empty() && !self.locked(id) {
                            self.expanded.insert(id);
                            self.sel = inside;
                        }
                    } else {
                        // Gambar: Enter masuk mode crop.
                        self.start_crop(id);
                    }
                }
                Key::Delete | Key::Backspace => self.delete_selection(),
                // Esc closes the Find and replace window first; the selection stays.
                Key::Escape if self.find_escape() => {}
                Key::Escape => {
                    self.sel.clear();
                    self.tool = Tool::Select;
                }
                // Mode edit titik: panah menggeser titik terpilih
                // Mode crop: panah menggeser gambar di balik kotak crop
                Key::ArrowLeft if !c && !a && self.crop_nudge(-step, 0.0) => {}
                Key::ArrowRight if !c && !a && self.crop_nudge(step, 0.0) => {}
                Key::ArrowUp if !c && !a && self.crop_nudge(0.0, -step) => {}
                Key::ArrowDown if !c && !a && self.crop_nudge(0.0, step) => {}
                Key::ArrowLeft if !c && !a && self.path_edit_nudge(-step, 0.0) => {}
                Key::ArrowRight if !c && !a && self.path_edit_nudge(step, 0.0) => {}
                Key::ArrowUp if !c && !a && self.path_edit_nudge(0.0, -step) => {}
                Key::ArrowDown if !c && !a && self.path_edit_nudge(0.0, step) => {}
                Key::ArrowLeft if !c && !a => self.nudge(-step, 0.0),
                Key::ArrowRight if !c && !a => self.nudge(step, 0.0),
                Key::ArrowUp if !c && !a => self.nudge(0.0, -step),
                Key::ArrowDown if !c && !a => self.nudge(0.0, step),
                // Zoom: Shift+0 / Ctrl+0 100%, Shift+1 semua, Shift+2 seleksi, + dan -
                Key::Num0 if s || c => self.zoom_100(),
                Key::Num1 | Key::Exclamationmark if s => self.fit_all(),
                Key::Num2 if s => self.zoom_selection(),
                // Angka: opacity 10% … 90%, 0 = 100%
                Key::Num0 | Key::Num1 | Key::Num2 | Key::Num3 | Key::Num4 | Key::Num5 | Key::Num6 | Key::Num7 | Key::Num8 | Key::Num9
                    if plain =>
                {
                    let digit = key.name().parse::<f32>().unwrap_or(0.0);
                    let value = if digit == 0.0 { 1.0 } else { digit / 10.0 };
                    self.edit_selected(|n| n.opacity = value);
                }
                Key::Plus | Key::Equals if !a => self.zoom_at(center, 1.25),
                Key::Minus if !a => self.zoom_at(center, 0.8),
                // Urutan: ] / [ paling depan/belakang, Ctrl+] / Ctrl+[ satu tingkat
                Key::CloseBracket if c => self.step_order(true),
                Key::OpenBracket if c => self.step_order(false),
                Key::CloseBracket if !a => self.reorder(true),
                Key::OpenBracket if !a => self.reorder(false),
                Key::PageDown if plain => self.step_page(true),
                Key::PageUp if plain => self.step_page(false),
                _ => {}
            }
        }
    }

    // ------------------------------------------------------------ panels

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        egui::menu::bar(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui.button("Home").on_hover_text("Recent files").clicked() {
                self.go_home();
            }
            ui.menu_button("File", |ui| {
                if ui.add(egui::Button::new("New").shortcut_text("Ctrl+N")).clicked() {
                    ui.close_menu();
                    self.new_doc();
                }
                if ui.add(egui::Button::new("Open…").shortcut_text("Ctrl+O")).clicked() {
                    ui.close_menu();
                    self.open_dialog();
                }
                if ui.add(egui::Button::new("Save").shortcut_text("Ctrl+S")).clicked() {
                    ui.close_menu();
                    self.save(false);
                }
                if ui.add(egui::Button::new("Save as…").shortcut_text("Ctrl+Shift+S")).clicked() {
                    ui.close_menu();
                    self.save(true);
                }
                ui.separator();
                if ui.button("Import…").on_hover_text("Place images on the canvas. An SVG becomes editable layers.").clicked() {
                    ui.close_menu();
                    self.import_dialog(false);
                }
                if ui.button("Place SVG as image…").on_hover_text("Keep an SVG as one picture instead of converting it to layers.").clicked() {
                    ui.close_menu();
                    self.import_dialog(true);
                }
            });
            ui.menu_button("Edit", |ui| {
                let can_undo = self.pen_can_undo() || !self.undo.is_empty();
                if ui.add_enabled(can_undo, egui::Button::new("Undo").shortcut_text("Ctrl+Z")).clicked() {
                    ui.close_menu();
                    self.undo_cmd();
                }
                let can_redo = self.pen_can_redo() || !self.redo.is_empty();
                if ui.add_enabled(can_redo, egui::Button::new("Redo").shortcut_text("Ctrl+Shift+Z")).clicked() {
                    ui.close_menu();
                    self.redo_cmd();
                }
                ui.separator();
                let has_sel = !self.sel.is_empty();
                if ui.add_enabled(has_sel, egui::Button::new("Cut").shortcut_text("Ctrl+X")).clicked() {
                    ui.close_menu();
                    self.cut(ui.ctx());
                }
                if ui.add_enabled(has_sel, egui::Button::new("Copy").shortcut_text("Ctrl+C")).clicked() {
                    ui.close_menu();
                    self.copy(ui.ctx());
                }
                if ui.add(egui::Button::new("Paste").shortcut_text("Ctrl+V")).clicked() {
                    ui.close_menu();
                    self.paste_clipboard(ui.ctx(), false);
                }
                if ui.add(egui::Button::new("Paste in Place").shortcut_text("Ctrl+Shift+V")).clicked() {
                    ui.close_menu();
                    self.paste_clipboard(ui.ctx(), true);
                }
                ui.separator();
                if ui.add(egui::Button::new("Duplicate").shortcut_text("Ctrl+D")).clicked() {
                    ui.close_menu();
                    let nodes = self.selected_nodes();
                    self.add_nodes(nodes, vec2(24.0, 24.0), None);
                }
                if ui.add(egui::Button::new("Delete").shortcut_text("Del")).clicked() {
                    ui.close_menu();
                    self.delete_selection();
                }
                ui.separator();
                if ui.add(egui::Button::new("Find…").shortcut_text("Ctrl+F")).clicked() {
                    ui.close_menu();
                    self.open_find(false);
                }
                if ui.add(egui::Button::new("Find and replace…").shortcut_text("Ctrl+H")).clicked() {
                    ui.close_menu();
                    self.open_find(true);
                }
                ui.separator();
                if ui.add(egui::Button::new("Group").shortcut_text("Ctrl+G")).clicked() {
                    ui.close_menu();
                    self.group_selection();
                }
                if ui.add(egui::Button::new("Ungroup").shortcut_text("Ctrl+Shift+G")).clicked() {
                    ui.close_menu();
                    self.ungroup_selection();
                }
                ui.separator();
                if ui.add_enabled(has_sel, egui::Button::new("Flip horizontal").shortcut_text("Shift+H")).clicked() {
                    ui.close_menu();
                    self.flip(true);
                }
                if ui.add_enabled(has_sel, egui::Button::new("Flip vertical").shortcut_text("Shift+V")).clicked() {
                    ui.close_menu();
                    self.flip(false);
                }
                let several = self.sel.len() > 1;
                if ui.add_enabled(several, egui::Button::new("Distribute horizontal spacing").shortcut_text("Alt+Shift+H")).clicked() {
                    ui.close_menu();
                    self.distribute(true);
                }
                if ui.add_enabled(several, egui::Button::new("Distribute vertical spacing").shortcut_text("Alt+Shift+V")).clicked() {
                    ui.close_menu();
                    self.distribute(false);
                }
                if ui.add_enabled(several, egui::Button::new("Tidy up").shortcut_text("Ctrl+Alt+T")).clicked() {
                    ui.close_menu();
                    self.tidy_up();
                }
                if ui.add_enabled(has_sel, egui::Button::new("Outline stroke").shortcut_text("Ctrl+Shift+O")).clicked() {
                    ui.close_menu();
                    self.outline_stroke();
                }
                ui.separator();
                if ui.add_enabled(has_sel, egui::Button::new("Add auto layout").shortcut_text("Shift+A")).clicked() {
                    ui.close_menu();
                    self.add_auto_layout();
                }
                let auto = self.sel.iter().any(|id| self.doc.find(*id).is_some_and(|n| n.is_auto()));
                if ui.add_enabled(auto, egui::Button::new("Remove auto layout").shortcut_text("Alt+Shift+A")).clicked() {
                    ui.close_menu();
                    self.remove_auto_layout();
                }
                ui.separator();
                if ui.add_enabled(has_sel, egui::Button::new("Create component").shortcut_text("Ctrl+Alt+K")).clicked() {
                    ui.close_menu();
                    self.create_component();
                }
                let instance = self.sel.iter().any(|id| self.doc.find(*id).is_some_and(|n| n.is_instance()));
                if ui.add_enabled(instance, egui::Button::new("Detach instance").shortcut_text("Ctrl+Alt+B")).clicked() {
                    ui.close_menu();
                    self.detach_selection();
                }
                ui.separator();
                if ui.add(egui::Button::new("Bring to front").shortcut_text("]")).clicked() {
                    ui.close_menu();
                    self.reorder(true);
                }
                if ui.add(egui::Button::new("Send to back").shortcut_text("[")).clicked() {
                    ui.close_menu();
                    self.reorder(false);
                }
            });
            ui.menu_button("View", |ui| {
                if ui.add(egui::Button::new("Zoom 100%").shortcut_text("Shift+0")).clicked() {
                    ui.close_menu();
                    self.zoom_100();
                }
                if ui.add(egui::Button::new("Zoom to selection").shortcut_text("Shift+2")).clicked() {
                    ui.close_menu();
                    self.zoom_selection();
                }
                if ui.add(egui::Button::new("Show/hide UI").shortcut_text("Ctrl+\\")).clicked() {
                    ui.close_menu();
                    self.show_ui = !self.show_ui;
                }
                if ui.add(egui::Button::new("Zoom to fit").shortcut_text("Shift+1")).clicked() {
                    ui.close_menu();
                    self.fit_all();
                }
                if ui.add(egui::Button::new(if self.rulers.show { "Hide rulers and guides" } else { "Show rulers and guides" }).shortcut_text("Shift+R")).clicked() {
                    ui.close_menu();
                    self.toggle_rulers();
                }
                ui.separator();
                if ui.button("Stress test: add 500 frames").clicked() {
                    ui.close_menu();
                    self.stress_test();
                }
            });
            if ui.button("About").clicked() {
                self.about = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(format!("{:.0}%", self.view.zoom * 100.0));
            });
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(&self.status);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!(
                    "{} frames · {} layers · {} drawn · {:.1} ms/frame · {} fonts loaded",
                    self.doc.frame_count(),
                    self.doc.count(),
                    self.drawn,
                    self.frame_ms,
                    self.fonts.loaded_count()
                ));
            });
        });
    }

    fn layers(&mut self, ui: &mut egui::Ui) {
        self.pages_ui(ui);
        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label(theme::title("Layers").size(15.0));
        ui.add_space(2.0);
        ui.separator();
        ui.add_space(2.0);
        // Daftar datar (frame + isi frame yang dibuka, di kedalaman mana pun), digambar virtual: hanya
        // baris yang terlihat. Anggota group yang berurutan (seinduk) tampil di bawah satu baris
        // group yang bisa dibuka/tutup.
        #[derive(Clone, Copy)]
        enum Row<'a> {
            /// (node, frame induk, kedalaman indentasi)
            Node(&'a Node, Option<u64>, u8),
            /// (id group, kedalaman indentasi, anggota paling depan, frame induk)
            Group(u64, u8, u64, Option<u64>),
        }
        // Satu daftar saudara dari depan ke belakang; frame yang dibuka diikuti isinya. Baris group
        // disisipkan di awal tiap rangkaian, dan anggotanya hanya tampil kalau group-nya dibuka.
        fn list<'a, T: AsRef<Node>>(rows: &mut Vec<Row<'a>>, nodes: &'a [T], parent: Option<u64>, depth: u8, expanded: &HashSet<u64>) {
            let mut last = 0u64;
            for n in nodes.iter().rev().map(|n| n.as_ref()) {
                let g = n.group;
                if g != last {
                    last = g;
                    if g != 0 {
                        rows.push(Row::Group(g, depth, n.id, parent));
                    }
                }
                if g != 0 && !expanded.contains(&g) {
                    continue;
                }
                let depth = depth.saturating_add((g != 0) as u8);
                rows.push(Row::Node(n, parent, depth));
                if n.kind == Kind::Frame && expanded.contains(&n.id) {
                    list(rows, &n.children, Some(n.id), depth.saturating_add(1), expanded);
                }
            }
        }
        let mut rows: Vec<Row> = Vec::with_capacity(self.doc.nodes.len());
        list(&mut rows, &self.doc.nodes, None, 0, &self.expanded);
        let row_h = 30.0;
        let mut click: Option<(u64, bool, bool)> = None; // (id, shift, double)
        let mut click_group: Option<(u64, bool)> = None; // (id group, shift)
        let mut toggle: Option<u64> = None;
        let mut lock_toggle: Option<u64> = None;
        let mut hide_toggle: Option<u64> = None;
        let mut menu: Option<u8> = None;
        let targets = self.clip_targets();
        let can_clip = !targets.is_empty();
        let clipped = can_clip && targets.iter().all(|id| self.doc.find(*id).is_some_and(|n| n.clip));
        let grouped = !self.selected_groups().is_empty();
        let detachable = self.sel.iter().any(|id| self.doc.find(*id).is_some_and(|n| n.is_instance()));
        // Warna penanda yang sama di seluruh pilihan (untuk menandai bulatan aktif di menu).
        let sel_tag = {
            let mut tags = self.sel.iter().filter_map(|id| self.doc.find(*id)).map(|n| n.tag);
            tags.next().filter(|t| tags.all(|u| u == *t))
        };
        // Menu klik kanan baris layer; hasilnya dijalankan setelah daftar selesai digambar.
        let layer_menu = |ui: &mut egui::Ui, menu: &mut Option<u8>| {
            let mut item = |ui: &mut egui::Ui, enabled: bool, label: &str, key: &str, action: u8| {
                let button = egui::Button::new(label).shortcut_text(key);
                if ui.add_enabled(enabled, button).clicked() {
                    *menu = Some(action);
                    ui.close_menu();
                }
            };
            ui.set_min_width(230.0);
            let label = if clipped { "Release clipping mask" } else { "Create clipping mask" };
            item(ui, can_clip, label, "Ctrl+Alt+G", 0);
            ui.separator();
            item(ui, true, "Group", "Ctrl+G", 1);
            item(ui, grouped, "Ungroup", "Ctrl+Shift+G", 2);
            item(ui, true, "Outline stroke", "Ctrl+Shift+O", 8);
            ui.separator();
            item(ui, true, "Create component", "Ctrl+Alt+K", 6);
            item(ui, detachable, "Detach instance", "Ctrl+Alt+B", 7);
            ui.separator();
            item(ui, true, "Bring to front", "]", 3);
            item(ui, true, "Send to back", "[", 4);
            ui.separator();
            item(ui, true, "Delete", "Del", 5);
            ui.separator();
            // Warna penanda layer: deretan bulatan; yang pertama = tanpa warna.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.label("Color");
                ui.add_space(4.0);
                for (i, (name, _)) in LAYER_COLORS.iter().enumerate() {
                    let (r, resp) = ui.allocate_exact_size(vec2(20.0, 20.0), Sense::click());
                    let resp = resp.on_hover_text(*name);
                    if resp.hovered() {
                        ui.painter().rect_filled(r, CornerRadius::same(5), ui.visuals().widgets.hovered.weak_bg_fill);
                    }
                    let c = r.center();
                    if i == 0 {
                        let s = Stroke::new(1.3_f32, ui.visuals().weak_text_color());
                        ui.painter().circle_stroke(c, 6.0, s);
                        ui.painter().line_segment([c + vec2(-4.2, 4.2), c + vec2(4.2, -4.2)], s);
                    } else {
                        ui.painter().circle_filled(c, 6.0, tag_color(i as u8));
                    }
                    if sel_tag == Some(i as u8) {
                        ui.painter().circle_stroke(c, 8.5, Stroke::new(1.5_f32, ui.visuals().text_color()));
                    }
                    if resp.clicked() {
                        *menu = Some(10 + i as u8);
                        ui.close_menu();
                    }
                }
            });
        };

        let mut rename = self.group_rename.take();
        let mut renamed: Option<(u64, String)> = None;
        // Tarik-lepas layer: tujuan lepas dihitung dari baris di bawah pointer.
        struct Drop {
            parent: Option<u64>,
            anchor: Option<(u64, bool)>,
            group: u64,
            /// Frame / group yang dibuka setelah dilepas, supaya hasilnya terlihat.
            open: Option<u64>,
        }
        let drag_pos = if self.layer_drag.is_some() { ui.input(|i| i.pointer.latest_pos()) } else { None };
        // Yang ditarik tidak bisa masuk ke dirinya sendiri atau ke dalam isinya.
        let dragged: &[u64] = self.layer_drag.as_deref().unwrap_or_default();
        let enters = |parent: Option<u64>| parent.map_or(true, |p| !dragged.iter().any(|d| *d == p || self.doc.is_descendant(p, *d)));
        let mut drag_start: Option<(Vec<u64>, bool)> = None; // (id yang ditarik, sudah terpilih)
        let mut target: Option<Drop> = None;
        let mut mark: Option<(Rect, bool)> = None; // (garis sisip / baris tujuan, masuk ke dalam?)
        ui.spacing_mut().item_spacing.y = 0.0;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, rows.len(), |ui, range| {
            let at_end = range.end == rows.len();
            let mut last_bottom = ui.cursor().top();
            for i in range {
                last_bottom = ui.cursor().top() + row_h;
                let (n, parent, depth) = match rows[i] {
                    Row::Node(n, parent, depth) => (n, parent, depth),
                    Row::Group(g, depth, front, parent) => {
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                        let members = self.doc.group_members(&[g]);
                        let selected = !members.is_empty() && members.iter().all(|id| self.sel.contains(id));
                        let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                        // Group berwarna kalau semua anggotanya memakai warna penanda yang sama.
                        let tag = {
                            let mut tags = members.iter().filter_map(|id| self.doc.find(*id)).map(|n| n.tag);
                            tags.next().filter(|t| tags.all(|u| u == *t)).unwrap_or(0)
                        };
                        tag_row(&painter, rect, tag);
                        if selected {
                            painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), ACCENT.gamma_multiply(0.35));
                        } else if resp.hovered() {
                            painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), ui.visuals().widgets.hovered.weak_bg_fill);
                        }
                        let x = rect.left() + 10.0 + 26.0 * depth as f32;
                        let weak = ui.visuals().weak_text_color();
                        let open = self.expanded.contains(&g);
                        if resp.drag_started() {
                            drag_start = Some((members.clone(), selected));
                        }
                        if let Some(p) = drag_pos.filter(|p| rect.contains(*p) && enters(parent)) {
                            // Tepi atas baris group: di atas group (di luarnya). Selebihnya: masuk ke group.
                            let above = p.y < rect.top() + row_h * 0.3;
                            target = Some(Drop { parent, anchor: Some((front, true)), group: if above { 0 } else { g }, open: (!above).then_some(g) });
                            mark = Some(if above { (Rect::from_min_max(pos2(x, rect.top()), rect.right_top()), false) } else { (rect, true) });
                        }
                        painter.text(pos2(x + 5.0, rect.center().y), Align2::CENTER_CENTER, if open { "⏷" } else { "⏵" }, FontId::proportional(12.5), weak);
                        // Ikon group: kotak bergaris putus-putus.
                        let icon = Rect::from_center_size(pos2(x + 23.0, rect.center().y), vec2(11.0, 11.0));
                        let c = [icon.left_top(), icon.right_top(), icon.right_bottom(), icon.left_bottom(), icon.left_top()];
                        painter.extend(egui::Shape::dashed_line(&c, Stroke::new(1.2_f32, weak), 2.5, 2.0));
                        if rename.as_ref().is_some_and(|r| r.0 == g) {
                            // Sedang mengganti nama: kotak teks di tempat nama. Enter / klik di luar = simpan, Esc = batal.
                            let (_, text, focused) = rename.as_mut().unwrap();
                            let area = Rect::from_min_max(pos2(x + 34.0, rect.top() + 3.0), pos2(rect.right() - 4.0, rect.bottom() - 3.0));
                            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(area));
                            let edit = child.add(egui::TextEdit::singleline(text).font(FontId::proportional(14.0)).desired_width(area.width()));
                            if !*focused {
                                *focused = true;
                                edit.request_focus();
                            }
                            if edit.lost_focus() {
                                if !ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                    renamed = Some((g, text.trim().to_owned()));
                                }
                                rename = None;
                            }
                            continue;
                        }
                        painter.text(pos2(x + 40.0, rect.center().y), Align2::LEFT_CENTER, self.doc.group_name(g), FontId::proportional(14.0), ui.visuals().text_color());
                        // Klik kanan pada group yang belum terpilih: pilih dulu, lalu menu berlaku untuknya.
                        if resp.secondary_clicked() && !selected {
                            click_group = Some((g, false));
                        }
                        resp.context_menu(|ui| layer_menu(ui, &mut menu));
                        if resp.clicked() {
                            let px = resp.interact_pointer_pos().map_or(f32::MAX, |p| p.x);
                            if px < x + 14.0 {
                                toggle = Some(g);
                            } else if resp.double_clicked() {
                                rename = Some((g, self.doc.group_name(g).to_owned(), false));
                            } else {
                                click_group = Some((g, ui.input(|i| i.modifiers.shift)));
                            }
                        }
                        continue;
                    }
                };
                let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), row_h), Sense::click_and_drag());
                let selected = self.sel.contains(&n.id);
                let painter = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
                tag_row(&painter, rect, n.tag);
                if selected {
                    painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), ACCENT.gamma_multiply(0.35));
                } else if resp.hovered() {
                    painter.rect_filled(rect.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), ui.visuals().widgets.hovered.weak_bg_fill);
                }
                let indent = 10.0 + 26.0 * depth as f32;
                let mut x = rect.left() + indent;
                let is_frame = n.kind == Kind::Frame;
                if resp.drag_started() {
                    drag_start = Some((if selected { self.sel.clone() } else { vec![n.id] }, selected));
                }
                if let Some(p) = drag_pos.filter(|p| rect.contains(*p)) {
                    let t = (p.y - rect.top()) / row_h;
                    if is_frame && enters(Some(n.id)) && t > 0.25 && (t < 0.75 || self.expanded.contains(&n.id)) {
                        // Bagian tengah baris frame: masuk ke frame, paling depan.
                        target = Some(Drop { parent: Some(n.id), anchor: None, group: 0, open: Some(n.id) });
                        mark = Some((rect, true));
                    } else if enters(parent) {
                        let above = t < 0.5;
                        // Di bawah anggota terakhir sebuah group: pointer di kiri indentasinya = keluar dari group.
                        // (Anggota berikutnya bisa terpisah beberapa baris kalau anggota ini frame yang dibuka.)
                        let more = n.group != 0
                            && rows[i + 1..].iter().find_map(|r| match r {
                                Row::Node(m, p2, _) if *p2 == parent => Some(m.group == n.group),
                                Row::Group(_, _, _, p2) if *p2 == parent => Some(false),
                                _ => None,
                            }) == Some(true);
                        let out = n.group != 0 && !above && !more && p.x < rect.left() + indent;
                        target = Some(Drop { parent, anchor: Some((n.id, above)), group: if out { 0 } else { n.group }, open: None });
                        let (lx, ly) = (rect.left() + indent - if out { 26.0 } else { 0.0 }, if above { rect.top() } else { rect.bottom() });
                        mark = Some((Rect::from_min_max(pos2(lx, ly), pos2(rect.right(), ly)), false));
                    }
                }
                if n.clip {
                    // Panah siku: layer ini di-clip ke layer di bawahnya.
                    let (c, s) = (pos2(x + 4.0, rect.center().y), Stroke::new(1.3_f32, ACCENT));
                    painter.line_segment([c + vec2(-3.0, -5.0), c + vec2(-3.0, 3.0)], s);
                    painter.line_segment([c + vec2(-3.0, 3.0), c + vec2(4.0, 3.0)], s);
                    painter.line_segment([c + vec2(1.5, 0.5), c + vec2(4.0, 3.0)], s);
                    painter.line_segment([c + vec2(1.5, 5.5), c + vec2(4.0, 3.0)], s);
                    // Layer biasa: panah menempati kolom panah buka/tutup, supaya ikon tetap sejajar.
                    if is_frame {
                        x += 15.0;
                    }
                }
                let chevron = x;
                if is_frame {
                    let open = self.expanded.contains(&n.id);
                    painter.text(
                        pos2(x + 5.0, rect.center().y),
                        Align2::CENTER_CENTER,
                        if open { "⏷" } else { "⏵" },
                        FontId::proportional(12.5),
                        ui.visuals().weak_text_color(),
                    );
                }
                // Kolom panah selalu disisihkan, jadi ikon dan nama sejajar di semua baris (seperti baris group).
                x += 18.0;
                let tag = match n.kind {
                    Kind::Frame => "#",
                    Kind::Rect => "▭",
                    Kind::Ellipse => "○",
                    Kind::Line => "/",
                    Kind::Text => "T",
                    Kind::Path => "✎",
                    Kind::Image => "▣",
                };
                // Mains and instances: their own icon and colour, as on the canvas.
                let linked = n.is_linked();
                if linked {
                    components::component_icon(&painter, pos2(x + 5.0, rect.center().y), n.component, components::PURPLE);
                } else {
                    painter.text(pos2(x + 5.0, rect.center().y), Align2::CENTER_CENTER, tag, FontId::proportional(13.0), ui.visuals().weak_text_color());
                }
                let color = match (n.visible, linked) {
                    (true, true) => Color32::from_rgb(190, 150, 255),
                    (true, false) => ui.visuals().text_color(),
                    (false, _) => ui.visuals().weak_text_color(),
                };
                let weight = if is_frame { FontId::new(14.0, theme::strong()) } else { FontId::proportional(14.0) };
                painter.text(pos2(x + 22.0, rect.center().y), Align2::LEFT_CENTER, &n.name, weight, color);
                // Gembok di ujung kanan baris: selalu tampil kalau terkunci, samar saat baris disorot.
                let lock_zone = Rect::from_min_max(pos2(rect.right() - 28.0, rect.top()), rect.max);
                if n.locked || resp.hovered() {
                    let tint = if n.locked { ui.visuals().text_color() } else { ui.visuals().weak_text_color().gamma_multiply(0.6) };
                    lock_icon(&painter, lock_zone.center(), n.locked, tint);
                }
                // The eye beside it: always shown on a hidden layer, faint while the row is hovered.
                let eye_zone = Rect::from_min_max(pos2(rect.right() - 52.0, rect.top()), pos2(rect.right() - 28.0, rect.bottom()));
                if !n.visible || resp.hovered() {
                    let tint = if n.visible { ui.visuals().weak_text_color().gamma_multiply(0.6) } else { ui.visuals().text_color() };
                    eye_icon(&painter, eye_zone.center(), n.visible, tint);
                }
                // Klik kanan pada layer yang belum terpilih: pilih dulu, lalu menu berlaku untuknya.
                if resp.secondary_clicked() && !selected {
                    click = Some((n.id, false, false));
                }
                resp.context_menu(|ui| layer_menu(ui, &mut menu));
                if resp.clicked() || resp.double_clicked() {
                    let px = resp.interact_pointer_pos().map_or(f32::MAX, |p| p.x);
                    if resp.interact_pointer_pos().is_some_and(|p| lock_zone.contains(p)) {
                        lock_toggle = Some(n.id);
                    } else if resp.interact_pointer_pos().is_some_and(|p| eye_zone.contains(p)) {
                        hide_toggle = Some(n.id);
                    } else if is_frame && px < chevron + 16.0 {
                        toggle = Some(n.id);
                    } else {
                        click = Some((n.id, ui.input(|i| i.modifiers.shift), resp.double_clicked()));
                    }
                }
            }
            if let Some(p) = drag_pos {
                let clip = ui.clip_rect();
                // Dilepas di area kosong di bawah daftar: jadi layer paling bawah.
                if target.is_none() && at_end && p.y >= last_bottom && clip.contains(p) {
                    if let Some(first) = self.doc.nodes.first() {
                        target = Some(Drop { parent: None, anchor: Some((first.id, false)), group: 0, open: None });
                        mark = Some((Rect::from_min_max(pos2(clip.left() + 10.0, last_bottom), pos2(clip.right(), last_bottom)), false));
                    }
                }
                if let Some((r, into)) = mark {
                    if into {
                        ui.painter().rect_stroke(r.shrink2(vec2(0.0, 1.0)), CornerRadius::same(8), Stroke::new(1.5_f32, ACCENT), StrokeKind::Inside);
                    } else {
                        ui.painter().hline(r.x_range(), r.top(), Stroke::new(2.0_f32, ACCENT));
                        ui.painter().circle_filled(r.left_top(), 3.0, ACCENT);
                    }
                }
                // Dekat tepi atas/bawah daftar: gulir otomatis.
                if clip.x_range().contains(p.x) {
                    let dy = if p.y < clip.top() + 24.0 { 10.0 } else if p.y > clip.bottom() - 24.0 { -10.0 } else { 0.0 };
                    if dy != 0.0 {
                        ui.scroll_with_delta(vec2(0.0, dy));
                        ui.ctx().request_repaint();
                    }
                }
            }
        });
        if let Some((ids, selected)) = drag_start {
            if !selected {
                self.sel = ids.clone();
            }
            self.layer_drag = Some(ids);
        }
        if let Some(ids) = self.layer_drag.take() {
            let (down, esc) = ui.input(|i| (i.pointer.primary_down(), i.key_pressed(egui::Key::Escape)));
            if down && !esc {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                // Label kecil yang mengikuti pointer: nama layer, atau jumlahnya.
                if let Some(p) = ui.input(|i| i.pointer.latest_pos()) {
                    let text = match ids[..] {
                        [id] => self.doc.find(id).map_or(String::new(), |n| n.name.clone()),
                        _ => format!("{} layers", ids.len()),
                    };
                    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("layer_drag")));
                    let galley = painter.layout_no_wrap(text, FontId::proportional(13.0), Color32::WHITE);
                    let r = Rect::from_min_size(p + vec2(14.0, 8.0), galley.size() + vec2(16.0, 8.0));
                    painter.rect_filled(r, CornerRadius::same(6), ACCENT);
                    painter.galley(r.min + vec2(8.0, 4.0), galley, Color32::WHITE);
                }
                self.layer_drag = Some(ids);
            } else if let Some(d) = target.filter(|_| !esc) {
                let pre = self.doc.clone();
                // Parts of an instance stay where the main has them, and nothing moves into one.
                let moved: Vec<Node> = self.doc.roots_of(&ids).iter().filter_map(|id| self.doc.find(*id).cloned()).collect();
                let allowed = if moved.iter().any(|n| n.source.is_some()) {
                    self.status = "Layers of an instance follow the main component: reorder them there, or detach the instance (Ctrl+Alt+B).".into();
                    false
                } else {
                    moved.iter().all(|n| self.may_enter(n, d.parent))
                };
                if allowed && self.doc.move_nodes(&ids, d.parent, d.anchor, d.group) {
                    if let Some(open) = d.open {
                        self.expanded.insert(open);
                    }
                    self.status = if ids.len() == 1 { "Layer moved.".into() } else { format!("{} layers moved.", ids.len()) };
                    self.sel = ids;
                    self.push_undo(pre);
                }
            }
        }
        if let Some(id) = hide_toggle {
            let pre = self.doc.clone();
            if let Some(n) = self.doc.find_mut(id) {
                n.visible = !n.visible;
            }
            self.push_undo(pre);
        }
        if let Some(id) = lock_toggle {
            let pre = self.doc.clone();
            if let Some(n) = self.doc.find_mut(id) {
                n.locked = !n.locked;
            }
            self.push_undo(pre);
        }
        if let Some(id) = toggle {
            if !self.expanded.remove(&id) {
                self.expanded.insert(id);
            }
        }
        self.group_rename = rename;
        if let Some((g, name)) = renamed {
            if !name.is_empty() {
                let now = ui.input(|i| i.time);
                self.set_group_name(g, name, now);
            }
        }
        if let Some((g, shift)) = click_group {
            let members = self.doc.group_members(&[g]);
            if shift {
                for id in members {
                    if !self.sel.contains(&id) {
                        self.sel.push(id);
                    }
                }
            } else {
                self.sel = members;
            }
        }
        if let Some((id, shift, double)) = click {
            if shift {
                if !self.sel.contains(&id) {
                    self.sel.push(id);
                }
            } else {
                self.sel = vec![id];
            }
            if double {
                if let Some(b) = self.doc.find(id).map(|n| n.bounds()) {
                    self.zoom_to(b, 4.0);
                }
            }
        }
        match menu {
            Some(0) => self.toggle_clip(),
            Some(1) => self.group_selection(),
            Some(2) => self.ungroup_selection(),
            Some(3) => self.reorder(true),
            Some(4) => self.reorder(false),
            Some(5) => self.delete_selection(),
            Some(6) => self.create_component(),
            Some(7) => self.detach_selection(),
            Some(8) => self.outline_stroke(),
            Some(c @ 10..=18) => {
                self.edit_selected(|n| n.tag = c - 10);
                self.status = if c == 10 { "Layer color removed.".into() } else { format!("Layer color: {}.", LAYER_COLORS[(c - 10) as usize].0) };
            }
            _ => {}
        }
    }

    // ------------------------------------------------------------ canvas

    // ------------------------------------------------------------ pointer

    // ------------------------------------------------------------ about

    /// A short card about the app: what it is, the version that is running, who made it.
    fn about_window(&mut self, ctx: &egui::Context) {
        if !self.about {
            return;
        }
        let modal = egui::Modal::new(egui::Id::new("kanvas_about")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label(theme::title("Kanvas").size(24.0));
                ui.label(egui::RichText::new(concat!("Version ", env!("CARGO_PKG_VERSION"))).color(ACCENT));
                ui.add_space(10.0);
                ui.label("A light, native design editor: an infinite canvas, frames, vector tools, images and Google Fonts — fast, offline, and yours.");
                ui.add_space(10.0);
                ui.weak("Made by Fadhel");
                ui.hyperlink_to("github.com/fadhelgame/Untitled", "https://github.com/fadhelgame/Untitled");
                ui.add_space(12.0);
                if ui.add(egui::Button::new("Close").min_size(vec2(84.0, 28.0))).clicked() {
                    self.about = false;
                }
            });
        });
        if modal.should_close() {
            self.about = false;
        }
    }

    // ------------------------------------------------------------ bench

    fn run_bench(&mut self, ctx: &egui::Context) {
        let Some(mut b) = self.bench.take() else { return };
        b.frame += 1;
        ctx.request_repaint();
        if std::env::var_os("KANVAS_PERF").is_some() {
            self.run_perf(ctx, &mut b);
            self.bench = Some(b);
            return;
        }
        let now = std::time::Instant::now();
        if let (Some(last), 11..=189) = (b.last, b.frame) {
            b.intervals.push((now - last).as_secs_f32() * 1000.0);
        }
        b.last = Some(now);
        let center = self.canvas_rect.center();
        match b.frame {
            1 => self.stress_test(),
            2..=9 => {}
            10..=99 => {
                b.samples.push(self.frame_ms);
                self.zoom_at(center, 1.05);
            }
            100..=189 => {
                b.samples.push(self.frame_ms);
                self.view.pan += vec2(-14.0, -9.0);
                self.zoom_at(center, 1.0 / 1.05);
            }
            190 => {
                self.fit_all();
                // Pilih satu teks supaya panel font (dengan preview) ikut terlihat di tangkapan layar.
                self.bench_demo();
                // `KANVAS_BENCH_ICONS=<kata>`: buka jendela icon dan cari, supaya ikut di tangkapan layar.
                if let Ok(q) = std::env::var("KANVAS_BENCH_ICONS") {
                    self.icons.open = true;
                    self.icons.query = q;
                    self.icons.search(ctx);
                }
                // `KANVAS_BENCH_CROP=<derajat>`: klik dua kali gambar demo (mode crop), tarik pegangan
                // crop kiri-atas ke dalam, lalu geser gambarnya. Gambarnya diputar sebesar nilai itu.
                // With `KANVAS_BENCH_CROP_FLIP` the image is flipped left-right first.
                if let Ok(rot) = std::env::var("KANVAS_BENCH_CROP") {
                    let id = self.doc.nodes.iter().flat_map(|n| n.children.iter()).find(|c| c.name == "Image").map(|c| c.id);
                    if let Some(n) = id.and_then(|id| self.doc.find_mut(id)) {
                        n.rotation = rot.parse().unwrap_or(0.0);
                        n.flip_h = std::env::var_os("KANVAS_BENCH_CROP_FLIP").is_some();
                        let (b0, c, o) = (n.aabb(), n.center(), self.canvas_rect.min);
                        let h0 = handles(n, false)[0];
                        self.zoom_to((b0.0 - 60.0, b0.1 - 60.0, b0.2 + 60.0, b0.3 + 60.0), 8.0);
                        let mid = self.view.to_screen(o, c.0, c.1);
                        let from = self.view.to_screen(o, h0.1, h0.2);
                        let to = from + (mid - from) * 0.5;
                        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                        let mv = egui::Event::PointerMoved;
                        b_inject(&mut b.inject, &[(200, mv(mid)), (202, button(mid, true)), (203, button(mid, false)), (205, button(mid, true)), (206, button(mid, false))]);
                        b_inject(&mut b.inject, &[(214, mv(from)), (216, button(from, true)), (218, mv(from + (to - from) * 0.5)), (220, mv(to)), (222, button(to, false))]);
                        let (p0, p1) = (mid + vec2(20.0, 20.0), mid + vec2(-25.0, 5.0));
                        b_inject(&mut b.inject, &[(230, mv(p0)), (232, button(p0, true)), (234, mv(p1)), (236, button(p1, false)), (240, mv(from))]);
                    }
                }
                // `KANVAS_BENCH_OVERLAY=1`: perbesar gambar demo; color overlay dipasang di frame 240,
                // setelah tekstur biasanya sudah tampil (seperti menambah Fill dari panel).
                // Nilainya boleh id node (untuk dokumen yang dibuka lewat argumen) sebagai ganti gambar demo.
                if let Ok(v) = std::env::var("KANVAS_BENCH_OVERLAY") {
                    let demo = self.doc.nodes.iter().flat_map(|n| n.children.iter()).find(|c| c.name == "Image").map(|c| c.id);
                    let id = v.parse::<u64>().ok().filter(|id| self.doc.find(*id).is_some()).or(demo);
                    if let Some(b0) = id.and_then(|id| self.doc.find(id)).map(|n| n.aabb()) {
                        self.sel = id.into_iter().collect();
                        self.zoom_to((b0.0 - 60.0, b0.1 - 60.0, b0.2 + 60.0, b0.3 + 60.0), 8.0);
                    }
                }
                // `KANVAS_BENCH_NESTED=1`: frames inside frames (see `bench_nested`), then with the mouse:
                // a click in the card (selects the card as a whole), a double-click on the badge and
                // one on its stripe (one level in each time), the loose rect dragged into the card,
                // and a frame drawn inside the outer frame. What got selected is printed to stderr.
                if std::env::var("KANVAS_BENCH_NESTED").is_ok() {
                    b.ids = self.bench_nested();
                    let (o, base) = (self.canvas_rect.min, self.doc.find(b.ids[0]).map_or((0.0, 0.0), |n| (n.x, n.y)));
                    let at = |x: f32, y: f32| self.view.to_screen(o, base.0 + x, base.1 + y);
                    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                    let mv = egui::Event::PointerMoved;
                    let click = |f: u32, p: Pos2| [(f, mv(p)), (f + 2, button(p, true)), (f + 3, button(p, false))];
                    let (in_card, on_badge, on_stripe) = (at(110.0, 410.0), at(330.0, 290.0), at(200.0, 338.0));
                    b.inject.extend(click(200, in_card));
                    b.inject.extend(click(340, on_badge).into_iter().chain(click(344, on_badge)));
                    b.inject.extend(click(480, on_stripe).into_iter().chain(click(484, on_stripe)));
                    let (from, to) = (at(700.0, 200.0), at(180.0, 425.0));
                    b.inject.extend([(620, mv(from)), (622, button(from, true))]);
                    b.inject.extend((1..=6u32).map(|s| (622 + s, mv(from + (to - from) * (s as f32 / 6.0)))));
                    b.inject.push((630, button(to, false)));
                    let (from, to) = (at(560.0, 330.0), at(740.0, 440.0));
                    b.inject.extend([(702, mv(from)), (704, button(from, true)), (706, mv(from + (to - from) * 0.5)), (708, mv(to)), (710, button(to, false))]);
                }
                // `KANVAS_BENCH_COMPONENT=1`: a main component and two instances (see `bench_component`;
                // one twice the size, one with a label and fill of its own). The main is restyled at
                // frames 220 and 226, as a user would. Then with the mouse: a click on the large
                // instance (selects it as a whole), a double-click (the part under the pointer), and
                // a drag of that part (must not move). Last, the remote commands: an instance made
                // with a size of its own, edits that must be refused, the main recoloured.
                // The outcome is printed to stderr.
                // `=window` also opens the Components window.
                if let Ok(mode) = std::env::var("KANVAS_BENCH_COMPONENT") {
                    b.ids = self.bench_component();
                    self.components.open = mode == "window";
                    let (o, c) = (self.canvas_rect.min, self.doc.find(b.ids[3]).map_or((0.0, 0.0), |n| n.center()));
                    let at = self.view.to_screen(o, c.0, c.1);
                    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                    let mv = egui::Event::PointerMoved;
                    let click = |f: u32, p: Pos2| [(f, mv(p)), (f + 2, button(p, true)), (f + 3, button(p, false))];
                    b.inject.extend(click(300, at));
                    b.inject.extend(click(440, at).into_iter().chain(click(444, at)));
                    let to = at + vec2(90.0, 60.0);
                    b.inject.extend([(580, mv(at)), (582, button(at, true)), (584, mv(at + (to - at) * 0.5)), (586, mv(to)), (590, button(to, false)), (596, mv(to + vec2(0.0, 200.0)))]);
                }
                // `KANVAS_BENCH_FLIP=1`: an image, a text, a gradient shape and a vector, each beside
                // its flipped copies (see `bench_flip`). The frame is also exported, to compare.
                if std::env::var("KANVAS_BENCH_FLIP").is_ok() {
                    self.bench_flip();
                }
                // `KANVAS_BENCH_DISTRIBUTE=1`: a row distributed and a loose grid tidied up (see
                // `bench_distribute`), then a box dragged to just short of equal spacing and held
                // there, so the snap and its markers are in the screenshot.
                // `=panel` leaves out the drag and keeps the row selected, to show the panel.
                if let Ok(mode) = std::env::var("KANVAS_BENCH_DISTRIBUTE") {
                    let (from, to, id) = self.bench_distribute();
                    b.ids = vec![id];
                    if mode == "panel" {
                        let row = self.doc.parent_of(id).and_then(|p| self.doc.find(p)).map(|p| p.children.iter().filter(|c| c.name == "Row").map(|c| c.id).collect());
                        self.sel = row.unwrap_or_default();
                    }
                    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                    let mv = egui::Event::PointerMoved;
                    if mode != "panel" {
                        b.inject.extend([(200, mv(from)), (202, button(from, true))]);
                        b.inject.extend((1..=8u32).map(|s| (202 + s, mv(from + (to - from) * (s as f32 / 8.0)))));
                    }
                }
                // `KANVAS_BENCH_CONSTRAINTS=1`: a frame with one layer per constraint (see
                // `bench_constraints`), enlarged by 200×100 with a drag of its bottom right handle;
                // then one layer is selected, to show the Constraints section. The boxes are printed
                // to stderr. `=mixed` selects two layers instead; `=remote` goes on to resize the frame
                // and set constraints with remote commands.
                if std::env::var("KANVAS_BENCH_CONSTRAINTS").is_ok() {
                    b.ids = self.bench_constraints();
                    let (o, c) = (self.canvas_rect.min, self.doc.find(b.ids[0]).map_or((0.0, 0.0), |n| (n.x + n.w, n.y + n.h)));
                    let (from, to) = (self.view.to_screen(o, c.0, c.1), self.view.to_screen(o, c.0 + 200.0, c.1 + 100.0));
                    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                    let mv = egui::Event::PointerMoved;
                    b.inject.extend([(200, mv(from)), (202, button(from, true))]);
                    b.inject.extend((1..=8u32).map(|s| (202 + s, mv(from + (to - from) * (s as f32 / 8.0)))));
                    b.inject.push((212, button(to, false)));
                }
                // `KANVAS_BENCH_AUTOLAYOUT=1`: a card, an instance of a button with a longer label
                // and a row of boxes (see `bench_autolayout`). Later, with the mouse: box A is
                // dragged past box C; then the arrow keys, undo, and remote commands. The boxes are
                // printed to stderr beside what they should be. `=drag` stops with the box in the air; `=multi` ends with the two buttons selected.
                if std::env::var("KANVAS_BENCH_AUTOLAYOUT").is_ok() {
                    b.ids = self.bench_autolayout();
                }
                // `KANVAS_BENCH_TYPO=1`: underline, strikethrough, justified text, lists and boxes
                // of fixed height (see `bench_typo`). The frame is also exported, to compare.
                // `=edit` / `=edit-list`: the justified paragraph / the numbered list is opened in
                // the text editor with a click in its third line and a drag to its fourth, so the
                // caret and the selection are in the screenshot. `=keys`: Ctrl+U on the selected list;
                // `=edit-keys`: on the characters selected in the editor (more modes further down).
                if let Ok(mode) = std::env::var("KANVAS_BENCH_TYPO") {
                    let ids = self.bench_typo();
                    b.ids.clone_from(&ids);
                    let id = if mode == "edit-list" { ids[3] } else { ids[1] };
                    if mode.starts_with("edit") {
                        self.sel = vec![id];
                        let pre = self.doc.clone();
                        self.start_text_edit(id, pre);
                        if let Some(n) = self.doc.find(id) {
                            let o = self.canvas_rect.min;
                            let (from, to) = (self.view.to_screen(o, n.x + n.w * 0.62, n.y + 52.0), self.view.to_screen(o, n.x + n.w * 0.3, n.y + 74.0));
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let mv = egui::Event::PointerMoved;
                            b.inject.extend([(215, mv(from)), (218, button(from, true)), (222, mv(from + (to - from) * 0.5)), (226, mv(to)), (230, button(to, false))]);
                        }
                    }
                    // `=zoom`: close to the large line, where it is drawn from outlines. `=panel`: the
                    // numbered list selected and the panel scrolled down to its Typography section.
                    if mode == "zoom" {
                        if let Some(b) = self.doc.find(ids[4]).map(|n| n.bounds()) {
                            self.zoom_to((b.0 - 120.0, b.1 - 20.0, b.2 + 20.0, b.3 + 20.0), 8.0);
                        }
                        self.sel.clear();
                    }
                    if mode == "panel" {
                        self.sel = vec![ids[3]];
                        let p = pos2(self.canvas_rect.right() + 170.0, self.canvas_rect.center().y);
                        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -640.0), modifiers: Default::default() };
                        b.inject.extend([(215, egui::Event::PointerMoved(p)), (218, wheel)]);
                    }
                    if mode == "edit-keys" {
                        let key = |pressed| egui::Event::Key { key: Key::U, physical_key: None, pressed, repeat: false, modifiers: Modifiers::COMMAND };
                        b.inject.extend([(238, key(true)), (240, key(false))]);
                    }
                    if mode == "keys" {
                        self.sel = vec![ids[2]];
                        let key = |pressed| egui::Event::Key { key: Key::U, physical_key: None, pressed, repeat: false, modifiers: Modifiers::COMMAND };
                        b.inject.extend([(220, key(true)), (222, key(false))]);
                    }
                }
                // `KANVAS_BENCH_FIND=<n>`: texts with the word "kanvas" in them (see `bench_find`),
                // the Find and replace window open on that word, and <n> steps to the next match.
                // Other modes (`replace`, `one`, `all`, `selection`, `keys`, `esc`, `far`, `many`)
                // are described at `bench_find_frame`, which drives them on later frames.
                if let Ok(mode) = std::env::var("KANVAS_BENCH_FIND") {
                    self.bench_find(&mode);
                }
                // `KANVAS_BENCH_SVG=<mode>`: an SVG imported as editable layers beside the same SVG as
                // a bitmap, icons both ways, and stroked shapes before and after Outline stroke
                // (see `bench_svg` for the modes).
                if let Ok(mode) = std::env::var("KANVAS_BENCH_SVG") {
                    self.bench_svg(&mode, &mut b.inject);
                }
                // `KANVAS_BENCH_CAPS=1`: strokes with every cap and join, solid and dashed, beside
                // the export of the same frame (see `bench_caps`). `=view:x0,y0,x1,y1`: that part.
                // `=panel`: a dashed path is selected and the panel scrolled to its Stroke section.
                if let Ok(mode) = std::env::var("KANVAS_BENCH_CAPS") {
                    self.bench_caps(ctx, &mode, &b.out, &mut b.inject);
                }
                // `KANVAS_BENCH_SHADOW=1`: beri drop shadow ke isi frame demo.
                if std::env::var("KANVAS_BENCH_SHADOW").is_ok() {
                    let ids: Vec<u64> = self.doc.nodes.iter().filter(|n| n.name == "Feature demo").flat_map(|n| n.children.iter().map(|c| c.id)).collect();
                    for id in ids {
                        if let Some(n) = self.doc.find_mut(id) {
                            n.shadow = Some(crate::doc::Shadow { x: 6.0, y: 10.0, blur: 14.0, color: [0, 0, 0, 120] });
                        }
                    }
                    // Gulir panel properti ke bawah supaya bagian Drop shadow ikut di tangkapan layar.
                    let at = pos2(ctx.screen_rect().right() - 150.0, 500.0);
                    let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -900.0), modifiers: Default::default() };
                    b.inject.extend([(200, egui::Event::PointerMoved(at)), (204, wheel.clone()), (212, wheel)]);
                }
                // `KANVAS_BENCH_GROUPFX=<derajat>`: group dua isi frame demo, beri bayangan bersama,
                // lalu putar group-nya dengan menarik dari luar sudut kotak seleksi.
                if let Ok(deg) = std::env::var("KANVAS_BENCH_GROUPFX") {
                    let kids: Vec<u64> = self.doc.nodes.iter().filter(|n| n.name == "Feature demo").flat_map(|n| n.children.iter().map(|c| c.id)).collect();
                    if kids.len() > 2 {
                        self.sel = vec![kids[1], kids[2]];
                        self.group_selection();
                        self.edit_selected(|n| n.shadow = Some(crate::doc::Shadow { x: 10.0, y: 14.0, blur: 10.0, color: [0, 0, 0, 150] }));
                        if let Some(b0) = self.sel_box() {
                            self.zoom_to((b0.0 - 80.0, b0.1 - 80.0, b0.2 + 80.0, b0.3 + 80.0), 8.0);
                            let o = self.canvas_rect.min;
                            let mid = self.view.to_screen(o, (b0.0 + b0.2) * 0.5, (b0.1 + b0.3) * 0.5);
                            let from = self.view.to_screen(o, b0.2, b0.1) + vec2(12.0, -12.0);
                            let turn = |f: f32| {
                                let (x, y) = crate::doc::rotate(from.x, from.y, mid.x, mid.y, deg.parse::<f32>().unwrap_or(30.0) * f);
                                pos2(x, y)
                            };
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let mv = egui::Event::PointerMoved;
                            b_inject(&mut b.inject, &[(214, mv(from)), (216, button(from, true)), (218, mv(turn(0.5))), (220, mv(turn(1.0))), (222, button(turn(1.0), false))]);
                        }
                    }
                }
                // `KANVAS_BENCH_BLUR=1`: layer blur di frame demo — satu kotak, satu group (elips +
                // bintang, buram sebagai satu gambar), teks, alas clipping mask, dan objek ber-blend.
                if std::env::var("KANVAS_BENCH_BLUR").is_ok() {
                    let kids: Vec<(u64, String)> =
                        self.doc.nodes.iter().filter(|n| n.name == "Feature demo").flat_map(|n| n.children.iter().map(|c| (c.id, c.name.clone()))).collect();
                    if kids.len() > 2 {
                        self.sel = vec![kids[1].0, kids[2].0];
                        self.group_selection();
                        for (id, name) in &kids {
                            let blur = match name.as_str() {
                                "Linear gradient" => 10.0,
                                "Headline" => 3.0,
                                "Mask base" => 8.0,
                                "Multiply" => 12.0,
                                _ if *id == kids[1].0 || *id == kids[2].0 => 14.0,
                                _ => 0.0,
                            };
                            if let Some(n) = self.doc.find_mut(*id) {
                                n.blur = blur;
                            }
                        }
                        let at = pos2(ctx.screen_rect().right() - 150.0, 500.0);
                        let wheel = egui::Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: vec2(0.0, -900.0), modifiers: Default::default() };
                        b.inject.extend([(200, egui::Event::PointerMoved(at)), (204, wheel.clone()), (212, wheel)]);
                    }
                }
                // `KANVAS_BENCH_LOCK=1`: kunci isi frame demo dan buka frame-nya di panel layer.
                if std::env::var("KANVAS_BENCH_LOCK").is_ok() {
                    let demo: Vec<(u64, Vec<u64>)> = self.doc.nodes.iter().filter(|n| n.name == "Feature demo").map(|n| (n.id, n.children.iter().map(|c| c.id).collect())).collect();
                    for (frame, ids) in demo {
                        self.expanded.insert(frame);
                        for id in ids {
                            if let Some(n) = self.doc.find_mut(id) {
                                n.locked = true;
                            }
                        }
                    }
                }
                // `KANVAS_BENCH_ROUND=<radius>`: bulatkan sudut semua vector di frame demo dan pilih yang pertama.
                if let Ok(r) = std::env::var("KANVAS_BENCH_ROUND") {
                    let ids: Vec<u64> = self.doc.nodes.iter().filter(|n| n.name == "Feature demo").flat_map(|n| n.children.iter()).filter(|c| c.kind == Kind::Path).map(|c| c.id).collect();
                    for id in &ids {
                        if let Some(n) = self.doc.find_mut(*id) {
                            n.radius = r.parse().unwrap_or(12.0);
                        }
                    }
                    self.sel = ids.into_iter().take(1).collect();
                }
                // `KANVAS_BENCH_CORNER=<piksel>`: pilih vector pertama di frame demo, lalu tarik widget
                // sudut pertamanya ke dalam sejauh itu. Akhiran `,hold`: tombol tidak dilepas.
                if let Ok(spec) = std::env::var("KANVAS_BENCH_CORNER") {
                    let id = self.doc.nodes.iter().filter(|n| n.name == "Feature demo").flat_map(|n| n.children.iter()).find(|c| if spec.contains("rect") { c.name == "Rotated" } else { c.kind == Kind::Path }).map(|c| c.id);
                    if let Some(b0) = id.and_then(|id| self.doc.find(id)).map(|n| n.aabb()) {
                        self.sel = id.into_iter().collect();
                        self.zoom_to((b0.0 - 60.0, b0.1 - 60.0, b0.2 + 60.0, b0.3 + 60.0), 8.0);
                        // Tarik widget sudut pertama menuju pusat vector.
                        let px: f32 = spec.split(',').next().and_then(|s| s.parse().ok()).unwrap_or(30.0);
                        if let Some(p) = self.bench_corner() {
                            let mid = self.view.to_screen(self.canvas_rect.min, (b0.0 + b0.2) * 0.5, (b0.1 + b0.3) * 0.5);
                            let to = p + (mid - p).normalized() * px;
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let mv = egui::Event::PointerMoved;
                            b_inject(&mut b.inject, &[(200, mv(p)), (202, button(p, true)), (204, mv(p + (to - p) * 0.5)), (206, mv(to))]);
                            if !spec.ends_with("hold") {
                                b_inject(&mut b.inject, &[(208, button(to, false)), (210, mv(to + vec2(60.0, 60.0)))]);
                            }
                        }
                    }
                }
                // `KANVAS_BENCH_TAGS=x,y`: buka frame demo di panel layer, group dua isinya, beri warna
                // penanda ke beberapa layer, lalu klik kanan di (x,y) supaya menu warnanya ikut tampil.
                if let Ok(spec) = std::env::var("KANVAS_BENCH_TAGS") {
                    if let Some((frame, kids)) = self.doc.nodes.iter().find(|n| n.name == "Feature demo").map(|n| (n.id, n.children.iter().map(|c| c.id).collect::<Vec<_>>())) {
                        self.expanded.insert(frame);
                        if kids.len() > 2 {
                            self.sel = vec![kids[1], kids[2]];
                            self.group_selection();
                            self.sel.clear();
                        }
                        for (i, id) in kids.iter().enumerate() {
                            if let Some(n) = self.doc.find_mut(*id) {
                                n.tag = [1, 4, 4, 0, 6, 0, 2][i % 7];
                            }
                        }
                        let v: Vec<f32> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                        if let [x, y] = v[..] {
                            let button = |pressed| egui::Event::PointerButton { pos: pos2(x, y), button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() };
                            b_inject(&mut b.inject, &[(200, egui::Event::PointerMoved(pos2(x, y))), (202, button(true)), (204, button(false))]);
                        }
                    }
                }
                // `KANVAS_BENCH_LAYERDRAG=x1,y1,x2,y2[,hold]`: buka frame demo di panel layer, group dua
                // isinya, lalu tarik dari (x1,y1) ke (x2,y2). Dengan `hold` tombol tidak dilepas,
                // supaya penanda tujuan ikut di tangkapan layar.
                if let Ok(spec) = std::env::var("KANVAS_BENCH_LAYERDRAG") {
                    if let Some((frame, kids)) = self.doc.nodes.iter().find(|n| n.name == "Feature demo").map(|n| (n.id, n.children.iter().map(|c| c.id).collect::<Vec<_>>())) {
                        self.expanded.insert(frame);
                        if kids.len() >= 3 {
                            let g = self.doc.group(&kids[kids.len() - 3..kids.len() - 1]);
                            self.expanded.insert(g);
                        }
                    }
                    let v: Vec<f32> = spec.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if let [x1, y1, x2, y2] = v[..] {
                        let (from, to) = (pos2(x1, y1), pos2(x2, y2));
                        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                        b.inject.extend([(200, egui::Event::PointerMoved(from)), (202, button(from, true))]);
                        for s in 1..=6u32 {
                            b.inject.push((202 + s * 2, egui::Event::PointerMoved(from + (to - from) * (s as f32 / 6.0))));
                        }
                        if !spec.contains("hold") {
                            b.inject.push((218, button(to, false)));
                        }
                    }
                }
                // `KANVAS_BENCH_PAGES=1`: tiga halaman (yang kedua diberi nama dan satu kotak), lalu
                // kembali ke halaman pertama. `=2`: berhenti di halaman kedua.
                if let Ok(mode) = std::env::var("KANVAS_BENCH_PAGES") {
                    self.add_page();
                    self.rename_page(1, "Kalender meja");
                    let id = self.doc.alloc_id();
                    let mut n = Node::new(id, Kind::Rect, 100.0, 100.0);
                    (n.w, n.h, n.fill) = (400.0, 300.0, Some([13, 153, 255, 255]));
                    self.doc.insert(n, None);
                    self.add_page();
                    self.go_page(if mode == "2" { 1 } else { 0 });
                }
                // `KANVAS_BENCH_LIBRARY=<file;file;…>`: buka Library, tambahkan file-file itu, dan
                // (dengan `KANVAS_BENCH_LIBRARY_DROP=x,y`) tarik item pertama ke titik layar itu.
                if let Ok(list) = std::env::var("KANVAS_BENCH_LIBRARY") {
                    self.library.open = true;
                    let paths: Vec<PathBuf> = list.split(';').filter(|s| !s.is_empty()).map(PathBuf::from).collect();
                    self.library_add_files(ctx, &paths);
                    let v: Vec<f32> = std::env::var("KANVAS_BENCH_LIBRARY_DROP").unwrap_or_default().split(',').filter_map(|s| s.trim().parse().ok()).collect();
                    if let [x, y] = v[..] {
                        let (from, to) = (self.canvas_rect.left_top() + vec2(80.0, 190.0), pos2(x, y));
                        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                        b.inject.extend([(230, egui::Event::PointerMoved(from)), (232, button(from, true))]);
                        for s in 1..=6u32 {
                            b.inject.push((232 + s * 2, egui::Event::PointerMoved(from + (to - from) * (s as f32 / 6.0))));
                        }
                        b.inject.push((248, button(to, false)));
                    }
                }
                // `KANVAS_BENCH_RULERS=1`: tampilkan penggaris dengan dua guide di frame demo.
                if std::env::var("KANVAS_BENCH_RULERS").is_ok() {
                    self.rulers.show = true;
                    if let Some((x, y)) = self.doc.nodes.iter().find(|n| n.name == "Feature demo").map(|n| (n.x, n.y)) {
                        self.doc.guides.push(crate::doc::RulerGuide { vertical: true, at: x + 40.0 });
                        self.doc.guides.push(crate::doc::RulerGuide { vertical: false, at: y + 280.0 });
                        // Uji tarik: guide horizontal baru dari penggaris atas, lalu geser guide vertikal.
                        let r = self.canvas_rect;
                        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                        let mv = egui::Event::PointerMoved;
                        let (p0, p1) = (pos2(r.center().x, r.top() + 8.0), pos2(r.center().x, r.top() + 60.0));
                        b_inject(&mut b.inject, &[(200, mv(p0)), (202, button(p0, true)), (204, mv(p0 + (p1 - p0) * 0.5)), (206, mv(p1)), (208, button(p1, false))]);
                        let gx = self.view.to_screen(r.min, x + 40.0, 0.0).x;
                        let (c, d) = (pos2(gx, r.top() + 120.0), pos2(gx + 200.0, r.top() + 120.0));
                        b_inject(&mut b.inject, &[(220, mv(c)), (222, button(c, true)), (224, mv(c + (d - c) * 0.5)), (226, mv(d)), (228, button(d, false)), (230, mv(r.center()))]);
                    }
                }
                // `KANVAS_BENCH_STYLES=1`: color style dari elips terpilih, dipakai dua objek lain,
                // lalu warnanya diubah; text style dari judul. `=2`: tanpa seleksi (daftar style tampil).
                if let Ok(mode) = std::env::var("KANVAS_BENCH_STYLES") {
                    use styles::StyleAction;
                    if let Some(id) = self.sel.first().copied() {
                        self.style_action(id, StyleAction::CreateColor { stroke: false });
                    }
                    if let Some(style) = self.doc.colors.first().map(|s| s.id) {
                        let by_name = |doc: &Doc, name: &str| doc.nodes.iter().flat_map(|n| n.children.iter()).find(|c| c.name == name).map(|c| c.id);
                        for name in ["Rotated", "Linear gradient"] {
                            if let Some(id) = by_name(&self.doc, name) {
                                self.style_action(id, StyleAction::LinkColor { stroke: false, style });
                            }
                        }
                        self.doc.colors[0].color = [20, 160, 90, 255];
                        self.doc.sync_color_style(style);
                        if let Some(id) = by_name(&self.doc, "Paragraph") {
                            self.style_action(id, StyleAction::CreateType);
                        }
                    }
                    if mode == "2" {
                        self.sel.clear();
                    }
                }
                // `KANVAS_BENCH_STROKE=1`: stroke putus-putus / posisi stroke di isi frame demo,
                // plus satu bintang dan satu segi enam dari tool Polygon.
                if std::env::var("KANVAS_BENCH_STROKE").is_ok() {
                    let demo: Vec<(u64, f32, f32, Vec<u64>)> = self.doc.nodes.iter().filter(|n| n.name == "Feature demo").map(|n| (n.id, n.x, n.y, n.children.iter().map(|c| c.id).collect())).collect();
                    for (frame, fx, fy, ids) in demo {
                        for (i, id) in ids.into_iter().enumerate() {
                            if let Some(n) = self.doc.find_mut(id) {
                                if n.kind != Kind::Text {
                                    n.stroke = Some([20, 20, 20, 255]);
                                    n.stroke_w = 6.0;
                                    n.stroke_align = Some((i % 3) as u8);
                                    if i % 2 == 0 {
                                        n.dash = 14.0;
                                        n.gap = 8.0;
                                    }
                                }
                            }
                        }
                        for (star, x) in [(true, 760.0), (false, 960.0)] {
                            let id = self.doc.alloc_id();
                            let mut n = Node::new(id, Kind::Path, fx + x, fy + 560.0);
                            (n.w, n.h, n.closed) = (160.0, 160.0, true);
                            n.points = polygon::points(&polygon::Options { sides: if star { 5 } else { 6 }, star, inner: 0.45 });
                            n.fill = Some([255, 196, 0, 255]);
                            n.stroke = Some([20, 20, 20, 255]);
                            n.stroke_w = 4.0;
                            self.doc.insert(n, Some(frame));
                        }
                    }
                }
                if let Ok(q) = std::env::var("KANVAS_BENCH_PHOTOS") {
                    self.photos.open = true;
                    self.photos.query = q;
                    self.photos.search(ctx);
                }
                let target = self.doc.nodes.get(30).map(|n| n.bounds());
                let _ = target;
            }
            _ => {
                // `KANVAS_BENCH_TEXTUNDO=1`: ketik di satu teks, pindah ke teks lain, ketik, lalu Ctrl+Z.
                // Yang ter-undo harus ketikan di teks kedua saja; hasilnya dicetak ke stderr.
                if std::env::var("KANVAS_BENCH_TEXTUNDO").is_ok() {
                    let find = |app: &Self, name: &str| app.doc.nodes.iter().flat_map(|n| n.children.iter()).find(|c| c.name == name).map(|c| c.id);
                    match b.frame {
                        200 => {
                            b.ids = [find(self, "Headline"), find(self, "Paragraph")].into_iter().flatten().collect();
                            if let Some(id) = b.ids.first().copied() {
                                let before: Vec<String> = b.ids.iter().filter_map(|id| self.doc.find(*id)).map(|n| n.text.clone()).collect();
                                eprintln!("TEXTUNDO before: {before:?}");
                                self.start_text_edit(id, self.doc.clone());
                            }
                            let key = egui::Event::Key { key: Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers { ctrl: true, command: true, ..Default::default() } };
                            b.inject.extend([(206, egui::Event::Text("AAA".into())), (228, egui::Event::Text("BBB".into())), (236, key)]);
                        }
                        214 => self.finish_text_edit(),
                        220 => {
                            if let Some(id) = b.ids.get(1).copied() {
                                self.start_text_edit(id, self.doc.clone());
                            }
                        }
                        232 | 244 => {
                            let now: Vec<String> = b.ids.iter().filter_map(|id| self.doc.find(*id)).map(|n| n.text.clone()).collect();
                            eprintln!("TEXTUNDO frame {}: {now:?}", b.frame);
                        }
                        _ => {}
                    }
                }
                // `KANVAS_BENCH_PASTE=1`: frame kedua di samping frame demo; salin satu isi frame demo,
                // arahkan pointer ke frame kedua, lalu Paste dan Paste in Place. Hasilnya ke stderr.
                if std::env::var("KANVAS_BENCH_PASTE").is_ok() {
                    match b.frame {
                        200 => {
                            let demo = self.doc.nodes.iter().find(|n| n.name == "Feature demo").map(|n| (n.id, n.bounds(), n.children.first().map(|c| c.id)));
                            if let Some((demo, q, Some(child))) = demo {
                                let id = self.doc.alloc_id();
                                let mut f = Node::new(id, Kind::Frame, q.2 + 300.0, q.1 + 150.0);
                                (f.w, f.h, f.name) = (q.2 - q.0, q.3 - q.1, "Second".into());
                                f.fill = Some([255, 255, 255, 255]);
                                self.doc.insert(f, None);
                                self.fit_all();
                                b.ids = vec![demo, id, child];
                                let at = self.view.to_screen(self.canvas_rect.min, q.2 + 300.0 + (q.2 - q.0) * 0.7, q.1 + 150.0 + (q.3 - q.1) * 0.6);
                                b.inject.push((203, egui::Event::PointerMoved(at)));
                            }
                        }
                        208 if b.ids.len() == 3 => {
                            let (demo, second, child) = (b.ids[0], b.ids[1], b.ids[2]);
                            let corner = |app: &Self, id: u64| app.doc.find(id).map(|n| (n.bounds().0, n.bounds().1)).unwrap_or_default();
                            self.sel = vec![child];
                            let json = self.clip_json().unwrap_or_default();
                            let at = self.canvas_pointer(ctx);
                            let world = at.map(|p| self.view.to_world(self.canvas_rect.min, p));
                            let (src, f1, f2) = (corner(self, child), corner(self, demo), corner(self, second));
                            self.paste_text(&json, false, at);
                            let got = self.sel.first().and_then(|id| self.doc.find(*id)).map(|n| (n.aabb(), self.doc.parent_of(n.id) == Some(second)));
                            eprintln!("PASTE pointer world {world:?}; pasted aabb + in second frame: {got:?}");
                            self.sel = vec![child];
                            self.paste_text(&json, true, at);
                            let got = self.sel.first().map(|id| (corner(self, *id), self.doc.parent_of(*id) == Some(second)));
                            eprintln!("PASTE in place: source offset {:?}; pasted {:?}", (src.0 - f1.0, src.1 - f1.1), got.map(|(p, ok)| ((p.0 - f2.0, p.1 - f2.1), ok)));
                            // Pointer di luar frame mana pun, tanpa seleksi frame lain: posisi yang sama persis.
                            self.sel = vec![child];
                            self.paste_text(&json, true, None);
                            let got = self.sel.first().map(|id| (corner(self, *id), self.doc.parent_of(*id) == Some(demo)));
                            eprintln!("PASTE in place, no pointer: source {src:?}; pasted + in demo frame: {got:?}");
                        }
                        _ => {}
                    }
                }
                if b.frame == 240 && std::env::var("KANVAS_BENCH_OVERLAY").is_ok() {
                    self.edit_selected(|n| n.fill = Some([0, 102, 227, 255]));
                }
                if b.frame == 250 && b.ids.len() == 1 && std::env::var_os("KANVAS_BENCH_DISTRIBUTE").is_some() {
                    let left = self.doc.find(b.ids[0]).zip(self.doc.parent_of(b.ids[0]).and_then(|p| self.doc.find(p))).map(|(n, p)| n.x - p.x);
                    eprintln!("DISTRIBUTE dragged box at {left:?} (want 320: 50 from its neighbour, like the two beside it), {} markers", self.ext.gaps.len());
                }
                if let (6, Ok(mode)) = (b.ids.len(), std::env::var("KANVAS_BENCH_CONSTRAINTS")) {
                    let ids = b.ids.clone();
                    let boxes = |app: &Self| -> Vec<String> {
                        let Some(f) = app.doc.find(ids[0]) else { return Vec::new() };
                        let head = format!("frame {}x{}", f.w, f.h);
                        std::iter::once(head).chain(f.children.iter().map(|c| format!("{} ({:.2}, {:.2}) {:.2}x{:.2}", c.name, c.x - f.x, c.y - f.y, c.w, c.h))).collect()
                    };
                    match b.frame {
                        206 => eprintln!("CONSTRAINTS halfway: {:?}", boxes(self)),
                        250 => {
                            eprintln!("CONSTRAINTS after the drag: {:?}\nCONSTRAINTS want: frame 800x400, Sidebar (20, 20) 80x360, Header (120, 20) 660x40, Badge (410, 180) 80x40, Button (700, 340) 80x40, Picture (160, 106.67) 160x80", boxes(self));
                            self.sel = if mode == "mixed" { vec![ids[1], ids[4]] } else { vec![ids[4]] };
                        }
                        252 if mode == "remote" => {
                            use serde_json::json;
                            let (frame, badge, button) = (ids[0], ids[3], ids[4]);
                            let run = |app: &mut Self, cmd: &str, args: serde_json::Value| {
                                let answer = app.remote_bench(cmd, &args);
                                eprintln!("CONSTRAINTS remote {cmd} {args} -> {}", answer.chars().take(330).collect::<String>());
                            };
                            self.do_undo();
                            eprintln!("CONSTRAINTS drag undone: {:?}", boxes(self));
                            run(self, "update_nodes", json!({ "nodes": [{ "id": frame, "w": 900, "h": 450 }] }));
                            eprintln!("CONSTRAINTS remote 900x450: {:?} (want Sidebar 80x410, Header 760x40, Badge (460, 205), Button (800, 390), Picture (180, 120) 180x90)", boxes(self));
                            run(self, "update_nodes", json!({ "nodes": [{ "id": frame, "w": 600, "h": 300, "keep_children": true }] }));
                            eprintln!("CONSTRAINTS remote back to 600x300, children kept: {:?} (want the same boxes)", boxes(self));
                            run(self, "update_nodes", json!({ "nodes": [{ "id": badge, "constraints": { "horizontal": "right", "vertical": "middle" } }] }));
                            run(self, "update_nodes", json!({ "nodes": [{ "id": badge, "constraints": { "horizontal": "left_right" } }] }));
                            run(self, "create_nodes", json!({ "parent": frame, "nodes": [{ "kind": "rect", "name": "Made", "x": 10, "y": 10, "w": 20, "h": 20, "constraints": { "horizontal": "scale", "vertical": "bottom" } }] }));
                            run(self, "get_document", json!({ "ids": [badge, button] }));
                            let made = self.doc.find(frame).and_then(|f| f.children.last()).map(|c| (c.constrain_h, c.constrain_v));
                            eprintln!("CONSTRAINTS remote made: {made:?} (want Scale, Max); badge: {:?} (want Stretch, Center)", self.doc.find(badge).map(|c| (c.constrain_h, c.constrain_v)));
                            self.sel = vec![button];
                        }
                        _ => {}
                    }
                }
                let auto = b.ids.len() == 11 && std::env::var_os("KANVAS_BENCH_AUTOLAYOUT").is_some();
                if auto {
                    let ids = b.ids.clone();
                    let (card, title, buttons, primary, cancel, chips, pill, inst, label, boxes, first) = (ids[0], ids[1], ids[2], ids[3], ids[4], ids[5], ids[6], ids[7], ids[8], ids[9], ids[10]);
                    // The box of a node, from the corner of the frame it is in.
                    let at = |app: &Self, id: u64| {
                        let o = app.doc.parent_of(id).and_then(|p| app.doc.find(p)).map_or((0.0, 0.0), |p| (p.x, p.y));
                        app.doc.find(id).map_or((0.0, 0.0, 0.0, 0.0), |n| (n.x - o.0, n.y - o.1, n.w, n.h))
                    };
                    let row = |app: &Self, id: u64| app.doc.find(id).map(|f| f.children.iter().map(|c| format!("{} at {:.0}", c.name, c.x - f.x)).collect::<Vec<_>>()).unwrap_or_default();
                    let near = |a: f32, b: f32| (a - b).abs() < 0.01;
                    let hold = std::env::var("KANVAS_BENCH_AUTOLAYOUT").is_ok_and(|m| m == "drag");
                    match b.frame {
                        264 => {
                            let (c, t, r, p, k, ch) = (at(self, card), at(self, title), at(self, buttons), at(self, primary), at(self, cancel), at(self, chips));
                            let text = |app: &Self, frame: u64| app.doc.find(frame).and_then(|f| f.children.first()).map_or((0.0, 0.0), |c| (c.w, c.h));
                            eprintln!("AUTOLAYOUT card {c:?}, title {t:?}, buttons {r:?}, fill button {p:?}, hug button {k:?}, chips {ch:?}");
                            eprintln!(
                                "AUTOLAYOUT card hugs its height ({} = 24 + {} + 16 + {} + 16 + {} + 24): {}",
                                c.3,
                                t.3,
                                r.3,
                                ch.3,
                                near(c.3, 24.0 + t.3 + 16.0 + r.3 + 16.0 + ch.3 + 24.0) && c.2 == 360.0
                            );
                            eprintln!("AUTOLAYOUT rows fill the card (312 wide at 24): {}", (r.0, r.2, ch.0, ch.2) == (24.0, 312.0, 24.0, 312.0) && near(r.1, 24.0 + t.3 + 16.0));
                            let (pt, kt) = (text(self, primary), text(self, cancel));
                            eprintln!(
                                "AUTOLAYOUT hug button is its label and padding ({} = 32 + {}), fill button takes the rest ({} = 312 - 12 - {}), its label in the middle: {}",
                                k.2,
                                kt.0,
                                p.2,
                                k.2,
                                near(k.2, 32.0 + kt.0) && near(k.3, 20.0 + kt.1) && near(p.2, 300.0 - k.2) && near(k.0, p.2 + 12.0) && near(at(self, self.doc.find(primary).map_or(0, |f| f.children[0].id)).0, (p.2 - pt.0) * 0.5)
                            );
                            let chip: Vec<(f32, f32, f32, f32)> = self.doc.find(chips).map(|f| f.children.iter().map(|c| (c.x - f.x, c.y - f.y, c.w, c.h)).collect()).unwrap_or_default();
                            let mut rows: Vec<f32> = chip.iter().map(|c| c.1).collect();
                            rows.dedup();
                            let fits = chip.iter().all(|c| c.0 + c.2 <= 312.01) && chip.windows(2).all(|w| w[1].1 > w[0].1 || near(w[1].0, w[0].0 + w[0].2 + 8.0));
                            eprintln!("AUTOLAYOUT chips {chip:?}: {} rows, each within 312 and 8 apart: {}", rows.len(), fits && rows.len() > 1 && near(ch.3, chip.last().map_or(0.0, |c| c.1 + c.3)));
                            let (m, i, l) = (at(self, pill), at(self, inst), at(self, label));
                            let own = self.doc.find(label).map(|n| n.text.clone()).unwrap_or_default();
                            eprintln!("AUTOLAYOUT main {}x{}, instance {}x{} with \"{own}\" {}x{} at ({}, {}): grows around its label: {}", m.2, m.3, i.2, i.3, l.2, l.3, l.0, l.1, i.2 > m.2 + 50.0 && near(i.2, 36.0 + l.2) && (l.0, l.1) == (18.0, 10.0));
                            eprintln!("AUTOLAYOUT boxes {:?} in {:?} (want A at 10, B at 80, C at 150 in a frame of 220 x 60)", row(self, boxes), at(self, boxes));
                            // Drag box A to the right of box C (A is selected: a click would pick the row).
                            self.sel = vec![first];
                            let (o, f) = (self.canvas_rect.min, self.doc.find(boxes).map_or((0.0, 0.0), |f| (f.x, f.y)));
                            let (from, to) = (self.view.to_screen(o, f.0 + 40.0, f.1 + 30.0), self.view.to_screen(o, f.0 + 200.0, f.1 + 36.0));
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let mv = egui::Event::PointerMoved;
                            b.inject.extend([(270, mv(from)), (272, button(from, true))]);
                            b.inject.extend((1..=8u32).map(|s| (272 + s, mv(from + (to - from) * (s as f32 / 8.0)))));
                            if !hold {
                                b.inject.push((286, button(to, false)));
                            }
                        }
                        277 => eprintln!("AUTOLAYOUT halfway through the drag: {:?}, mark {:?}", row(self, boxes), self.drop_mark.is_some()),
                        284 => eprintln!("AUTOLAYOUT at the end of the drag, button still down: {:?} (want B at 10, C at 80, A in the air), frame {:?}", row(self, boxes), at(self, boxes)),
                        300 if !hold => {
                            eprintln!("AUTOLAYOUT after the drag: {:?} (want B at 10, C at 80, A at 150), selected {:?}, undo steps {}", row(self, boxes), self.sel == [first], self.undo.len());
                            self.do_undo();
                        }
                        302 if !hold => {
                            eprintln!("AUTOLAYOUT drag undone: {:?} (want A at 10, B at 80, C at 150)", row(self, boxes));
                            self.do_redo();
                        }
                        304 if !hold => {
                            // The arrow keys: A back to the front, one place at a time; up and down do nothing in a row.
                            self.sel = vec![first];
                            self.nudge(-1.0, 0.0);
                            self.nudge(0.0, 1.0);
                        }
                        306 if !hold => {
                            eprintln!("AUTOLAYOUT arrow left: {:?} (want B at 10, A at 80, C at 150)", row(self, boxes));
                            // A duplicate lands right after its original; a paste goes by where it is put.
                            let nodes = self.selected_nodes();
                            self.add_nodes(nodes, vec2(24.0, 24.0), None);
                        }
                        308 if !hold => {
                            eprintln!("AUTOLAYOUT duplicate of A: {:?} (want B at 10, A at 80, A at 150, C at 220)", row(self, boxes));
                            self.do_undo();
                            // Resizing by a handle fixes that side: the fill button stops filling.
                            self.sel = vec![primary];
                            self.edit_selected(|n| n.absolute = true);
                        }
                        310 if !hold => {
                            eprintln!("AUTOLAYOUT fill button taken out of the layout: buttons {:?}, hug button {:?} (want it at 0)", at(self, buttons), at(self, cancel));
                            self.do_undo();
                        }
                        312 if !hold => {
                            use serde_json::json;
                            let stage = self.doc.parent_of(card).unwrap_or_default();
                            let run = |app: &mut Self, cmd: &str, args: serde_json::Value| {
                                let answer = app.remote_bench(cmd, &args);
                                eprintln!("AUTOLAYOUT remote {cmd} {args} -> {}", answer.chars().take(420).collect::<String>());
                            };
                            run(self, "create_nodes", json!({ "parent": stage, "nodes": [{ "kind": "frame", "name": "Remote row", "x": 460, "y": 360, "fill": "#e8ecf2", "layout": { "gap": 10, "padding": [10, 10], "cross_align": "center" }, "children": [
                                { "kind": "rect", "name": "One", "w": 50, "h": 30, "x": 400, "fill": "#0d99ff" },
                                { "kind": "rect", "name": "Two", "w": 50, "h": 20, "sizing": { "horizontal": "fill" }, "fill": "#14ae5c" },
                            ] }] }));
                            let made = self.doc.find(stage).and_then(|s| s.children.iter().find(|c| c.name == "Remote row")).map(|c| (c.id, c.children[1].id)).unwrap_or_default();
                            eprintln!("AUTOLAYOUT remote row: {:?} {:?} (want 130 x 50 hugging: One at 10, Two at 70)", at(self, made.0), row(self, made.0));
                            run(self, "update_nodes", json!({ "nodes": [{ "id": made.0, "w": 300 }, { "id": made.1, "x": 3 }] }));
                            eprintln!("AUTOLAYOUT remote row 300 wide: {:?}, Two {:?} (want Two at 70, 220 wide, 15 down)", at(self, made.0), at(self, made.1));
                            run(self, "update_nodes", json!({ "nodes": [{ "id": made.0, "layout": { "direction": "vertical", "gap": 4, "width": "hug" } }] }));
                            eprintln!("AUTOLAYOUT remote column: {:?}, Two {:?} (want 240 x 74: it hugs now, around Two as wide as it was, with One in the middle)", at(self, made.0), at(self, made.1));
                            run(self, "get_document", json!({ "ids": [made.0] }));
                            run(self, "update_nodes", json!({ "nodes": [{ "id": made.0, "layout": null }] }));
                            eprintln!("AUTOLAYOUT remote layout removed: {:?}, layout {:?}, Two {:?}", at(self, made.0), self.doc.find(made.0).map(|n| (n.layout.is_some(), n.size_h, n.size_v)), self.doc.find(made.1).map(|n| (n.size_h, n.x)));
                            // The right handle of the hug button dragged 30 px outward.
                            self.sel = vec![cancel];
                            let (o, n) = (self.canvas_rect.min, self.doc.find(cancel).map_or((0.0, 0.0), |n| (n.x + n.w, n.y + n.h * 0.5)));
                            let from = self.view.to_screen(o, n.0, n.1);
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let mv = egui::Event::PointerMoved;
                            b.inject.extend([(316, mv(from)), (318, button(from, true))]);
                            b.inject.extend((1..=6u32).map(|s| (318 + s, mv(from + vec2(5.0 * s as f32, 0.0)))));
                            b.inject.push((328, button(from + vec2(30.0, 0.0), false)));
                        }
                        322 | 332 if !hold => {
                            let sizes = self.doc.find(cancel).map(|n| (n.size_h, n.size_v));
                            eprintln!("AUTOLAYOUT hug button {} by its handle: {:?} {sizes:?}, fill button {:?}, zoom {} (want Fixed, Hug; the two still 300 wide together: {})", if b.frame == 322 { "being resized" } else { "resized" }, at(self, cancel), at(self, primary), self.view.zoom, near(at(self, cancel).2 + at(self, primary).2, 300.0));
                            if b.frame == 332 {
                                self.do_undo();
                            }
                        }
                        334 if !hold => {
                            // Typing in the label of the hug button: the row follows while the text is being edited.
                            eprintln!("AUTOLAYOUT resize undone: hug button {:?} {:?}", at(self, cancel), self.doc.find(cancel).map(|n| n.size_h));
                            let text = self.doc.find(cancel).map_or(0, |f| f.children[0].id);
                            self.start_text_edit(text, self.doc.clone());
                            b.inject.push((338, egui::Event::Text(" all".into())));
                        }
                        346 | 352 if !hold => {
                            let label = self.doc.find(cancel).map(|f| (f.children[0].text.clone(), f.children[0].w));
                            let ok = label.as_ref().is_some_and(|l| near(at(self, cancel).2, 32.0 + l.1)) && near(at(self, cancel).2 + at(self, primary).2, 300.0);
                            eprintln!("AUTOLAYOUT {}: label {label:?}, hug button {:?}, fill button {:?}, card {:?}: the button hugs it and the other one gives way: {ok}", if b.frame == 346 { "while typing" } else { "typing done" }, at(self, cancel), at(self, primary), at(self, card));
                            if b.frame == 346 {
                                self.finish_text_edit();
                            } else {
                                self.sel = if std::env::var("KANVAS_BENCH_AUTOLAYOUT").is_ok_and(|m| m == "multi") { vec![primary, cancel] } else { vec![card] };
                            }
                        }
                        _ => {}
                    }
                }
                let component = b.ids.len() == 6 && std::env::var_os("KANVAS_BENCH_COMPONENT").is_some();
                if component {
                    let (main_rect, main_label, wide, own) = (b.ids[1], b.ids[2], b.ids[3], b.ids[4]);
                    let look = |app: &Self, id: u64| -> Vec<String> {
                        let parts = app.doc.find(id).map(|n| n.children.iter().collect::<Vec<_>>()).unwrap_or_default();
                        parts.iter().map(|c| format!("{} fill {:?} radius {} text {:?} size {} at ({:.0}, {:.0}) {:.0}x{:.0}", c.name, c.fill.map(|f| [f[0], f[1], f[2]]), c.radius, c.text, c.font_size, c.x, c.y, c.w, c.h)).collect()
                    };
                    let names = |app: &Self| app.sel.iter().filter_map(|id| app.doc.find(*id)).map(|n| (n.name.clone(), n.is_instance(), n.source.is_some())).collect::<Vec<_>>();
                    match b.frame {
                        215 => eprintln!("COMPONENT before: large {:?}\nCOMPONENT before: own {:?}", look(self, wide), look(self, own)),
                        // The edits of the main, through the path that keys and menus take.
                        220 => {
                            self.sel = vec![main_rect];
                            self.edit_selected(|n| (n.fill, n.radius) = (Some([255, 120, 40, 255]), 36.0));
                        }
                        226 => {
                            self.sel = vec![main_label];
                            self.edit_selected(|n| (n.text, n.font_size, n.fill) = ("Sign up".into(), 30.0, Some([30, 30, 60, 255])));
                            self.sel.clear();
                        }
                        250 => eprintln!("COMPONENT after: large {:?} (want orange, radius 36, \"Sign up\", size 30, dark text)\nCOMPONENT after: own {:?} (want its green and \"Buy now\" kept; radius 36, size 30, dark text)", look(self, wide), look(self, own)),
                        // Ctrl+D on the main gives an instance; then undo and redo of the edit of the main.
                        256 => {
                            self.sel = vec![b.ids[0]];
                            let nodes = self.selected_nodes();
                            self.add_nodes(nodes, vec2(24.0, 24.0), None);
                            let copy = self.sel.first().and_then(|id| self.doc.find(*id)).map(|n| (n.is_instance(), n.instance_of == Some(b.ids[0]), n.children.len()));
                            eprintln!("COMPONENT duplicate of the main: {copy:?} (want (true, true, 2))");
                        }
                        258 | 260 => self.do_undo(),
                        262 => {
                            eprintln!("COMPONENT label edit undone: large {:?}, own {:?} (want \"Button\" size 26, \"Buy now\" size 26)", look(self, wide).last(), look(self, own).last());
                            self.do_redo();
                        }
                        266 => eprintln!("COMPONENT and redone: large {:?} (want \"Sign up\" size 30)", look(self, wide).last()),
                        310 => eprintln!("COMPONENT click on the large instance: {:?} (want [(Button, true, false)])", names(self)),
                        455 => eprintln!("COMPONENT double-click on it: {:?} (want one part: (_, false, true))", names(self)),
                        575 | 600 => {
                            let at = self.sel.first().and_then(|id| self.doc.find(*id)).map(|n| (n.x, n.y));
                            eprintln!("COMPONENT part {} the drag: {at:?}; parent {:?}; status: {}", if b.frame == 575 { "before" } else { "after" }, self.sel.first().and_then(|id| self.doc.parent_of(*id)) == Some(wide), self.status);
                        }
                        606 => {
                            use serde_json::json;
                            let (main, part) = (b.ids[0], self.doc.find(wide).and_then(|n| n.children.first()).map_or(0, |c| c.id));
                            let (x, y) = self.doc.find(own).map_or((0.0, 0.0), |n| (n.x + 300.0, n.y));
                            let mut run = |cmd: &str, args: serde_json::Value| {
                                let answer = self.remote_bench(cmd, &args);
                                eprintln!("COMPONENT remote {cmd} {args} -> {}", answer.chars().take(260).collect::<String>());
                            };
                            run("create_nodes", json!({ "nodes": [{ "kind": "instance", "instance_of": main, "x": x, "y": y, "w": 330, "name": "Wide button" }] }));
                            run("create_nodes", json!({ "parent": wide, "nodes": [{ "kind": "rect" }] }));
                            run("create_nodes", json!({ "parent": main, "nodes": [{ "kind": "instance", "instance_of": main }] }));
                            run("update_nodes", json!({ "nodes": [{ "id": part, "x": 5 }] }));
                            run("update_nodes", json!({ "nodes": [{ "id": part, "opacity": 0.9 }] }));
                            run("delete_nodes", json!({ "ids": [part] }));
                            run("arrange", json!({ "action": "front", "ids": [part] }));
                            run("update_nodes", json!({ "nodes": [{ "id": main_rect, "fill": "#0aa88a" }] }));
                            run("get_document", json!({ "ids": [part] }));
                        }
                        610 => {
                            let made = self.doc.nodes.iter().find(|n| n.name == "Wide button").map(|n| (n.is_instance(), n.w, look(self, n.id)));
                            eprintln!("COMPONENT remote instance: {made:?} (want true, 330 wide, teal, the background as wide and the label in its middle)");
                            self.sel = vec![own];
                        }
                        _ => {}
                    }
                }
                let nested = b.ids.len() == 5 && std::env::var_os("KANVAS_BENCH_NESTED").is_some();
                if nested {
                    let names = |app: &Self| app.sel.iter().filter_map(|id| app.doc.find(*id)).map(|n| n.name.clone()).collect::<Vec<_>>();
                    let parent = |app: &Self, id: u64| app.doc.parent_of(id).and_then(|p| app.doc.find(p)).map(|n| n.name.clone());
                    match b.frame {
                        210 => eprintln!("NESTED click in the card: {:?} (want [Card])", names(self)),
                        355 => eprintln!("NESTED double-click on the badge: {:?} (want [Badge])", names(self)),
                        495 => eprintln!("NESTED double-click on the stripe: {:?} (want [Stripe])", names(self)),
                        640 => eprintln!("NESTED loose rect dragged over the card: {:?} in {:?} (want [Loose] in Card)", names(self), parent(self, b.ids[4])),
                        700 => self.tool = Tool::Frame,
                        720 => {
                            let made = self.sel.first().and_then(|id| self.doc.find(*id)).map(|n| (n.kind, n.w, n.h));
                            eprintln!("NESTED frame drawn in the outer frame: {made:?} in {:?} (want a Frame in Nested demo)", self.sel.first().and_then(|id| parent(self, *id)));
                        }
                        _ => {}
                    }
                }
                if let Ok(mode) = std::env::var("KANVAS_BENCH_FIND") {
                    self.bench_find_frame(ctx, &mode, b.frame, &mut b.inject);
                }
                if std::env::var_os("KANVAS_BENCH_SVG").is_some() && (250..=255).contains(&b.frame) {
                    eprintln!("SVG frame {}: {:.2} ms", b.frame, self.frame_ms);
                    if b.frame == 250 {
                        let sel: Vec<(String, Kind)> = self.sel.iter().filter_map(|id| self.doc.find(*id)).map(|n| (n.name.clone(), n.kind)).collect();
                        eprintln!("SVG selection {sel:?}; status: {}", self.status);
                    }
                }
                if std::env::var_os("KANVAS_BENCH_CAPS").is_some() && (250..=255).contains(&b.frame) {
                    eprintln!("CAPS frame {}: {:.2} ms", b.frame, self.frame_ms);
                }
                if let Ok(mode) = std::env::var("KANVAS_BENCH_TYPO") {
                    if (250..=255).contains(&b.frame) {
                        eprintln!("TYPO frame {}: {:.2} ms", b.frame, self.frame_ms);
                    }
                    let paragraph = b.ids.get(1).and_then(|id| self.doc.find(*id)).map(|n| (n.x, n.y, n.w, n.h, n.fixed_h));
                    match (mode.as_str(), b.frame, paragraph) {
                        // `=edit-keys`: after Ctrl+U the selected characters are struck through too.
                        ("edit-keys", 245, _) => self.toggle_decoration(true),
                        // `=resize`: the bottom handle of the justified paragraph is dragged down by
                        // 60, which gives it a fixed height; then its lines go to the bottom.
                        ("resize", 215, Some((x, y, w, h, _))) => {
                            let o = self.canvas_rect.min;
                            let (from, to) = (self.view.to_screen(o, x + w * 0.5, y + h), self.view.to_screen(o, x + w * 0.5, y + h + 60.0));
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let mv = egui::Event::PointerMoved;
                            b.inject.extend([(218, mv(from)), (220, button(from, true)), (223, mv(from + (to - from) * 0.5)), (226, mv(to)), (230, button(to, false))]);
                            eprintln!("TYPO paragraph before the drag: h {h:.1}");
                        }
                        ("resize", 240, Some((_, _, _, h, fixed))) => {
                            eprintln!("TYPO paragraph after the drag: h {h:.1}, fixed height {fixed} (want 60 more, true)");
                            self.edit_selected(|n| n.valign = 2);
                        }
                        _ => {}
                    }
                }
                let settled = b.frame > if nested { 760 } else if component { 640 } else if auto { 356 } else { 260 };
                let ready = !self.fonts.any_loading() && !self.icons.busy() && !self.photos.busy() && !self.library.busy() && settled && (self.fonts.preview_count() >= 8 || b.frame > 700);
                if !b.shot && (ready || b.frame > 1500) {
                    b.shot = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    if let Some(id) = self.doc.nodes.iter().find(|n| n.name == "Flip demo").map(|n| n.id) {
                        if let Some((svg, w, h)) = export::to_svg(&self.doc, Some(id), &mut self.fonts, ctx) {
                            let _ = export::save_raster(&svg, w, h, 1.0, false, &b.out.with_file_name("bench-flip-export.png"));
                        }
                    }
                    if let Some(id) = self.doc.nodes.iter().find(|n| n.name == "Typography demo").map(|n| n.id) {
                        if let Some((svg, w, h)) = export::to_svg(&self.doc, Some(id), &mut self.fonts, ctx) {
                            let _ = export::save_raster(&svg, w, h, 1.0, false, &b.out.with_file_name("bench-typo-export.png"));
                        }
                    }
                    // Export frame demo juga, untuk dibandingkan dengan tampilan canvas.
                    if std::env::var("KANVAS_BENCH_BLUR").is_ok() {
                        if let Some(id) = self.doc.nodes.iter().find(|n| n.name == "Feature demo").map(|n| n.id) {
                            if let Some((svg, w, h)) = export::to_svg(&self.doc, Some(id), &mut self.fonts, ctx) {
                                let _ = export::save_raster(&svg, w, h, 1.0, false, &b.out.with_file_name("bench-blur-export.png"));
                            }
                        }
                    }
                }
                if b.phase == 1 {
                    if b.frame == b.mark + 84 {
                        // Selesai mengedit, lalu tarik pegangan kanan-tengah kotak teks ke kiri.
                        self.finish_text_edit();
                        if let Some(n) = self.sel.first().and_then(|id| self.doc.find(*id)) {
                            let o = self.canvas_rect.min;
                            let from = self.view.to_screen(o, n.x + n.w, n.y + n.h * 0.5);
                            let to = from - vec2(n.w * 0.45 * self.view.zoom, 0.0);
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let f = b.frame;
                            b.inject.extend([(f + 2, egui::Event::PointerMoved(from)), (f + 4, button(from, true))]);
                            for s in 1..=4u32 {
                                b.inject.push((f + 4 + s, egui::Event::PointerMoved(from + (to - from) * (s as f32 / 4.0))));
                            }
                            b.inject.push((f + 10, button(to, false)));
                        }
                    }
                    if b.frame == b.mark + 100 {
                        if let Some(i) = self.first_ready_icon() {
                            self.add_icon(i);
                        }
                        if let Some(i) = self.first_ready_photo() {
                            self.add_photo(i, ctx);
                        }
                    }
                    if b.frame >= b.mark + 115 && !self.photos.busy() {
                        b.phase = 2;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
                    }
                }
                let image = ctx.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                });
                if let Some(img) = image {
                    let rgba: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                    if b.phase == 2 {
                        let _ = export::save_rgba(&b.out.with_file_name("bench-edit.png"), img.size[0] as u32, img.size[1] as u32, &rgba);
                        self.dirty = false;
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                        return;
                    }
                    let shot = export::save_rgba(&b.out, img.size[0] as u32, img.size[1] as u32, &rgba);
                    // Export frame demo juga, supaya jalur export bisa diperiksa dari gambar hasilnya.
                    if let Some(demo) = self.doc.nodes.iter().find(|n| n.name == "Feature demo").map(|n| n.id) {
                        if let Some((svg, w, h)) = export::to_svg(&self.doc, Some(demo), &mut self.fonts, ctx) {
                            let _ = export::save_raster(&svg, w, h, 1.0, false, &b.out.with_file_name("bench-export.png"));
                        }
                        let ids: Vec<u64> = std::iter::once(demo).chain(export::pdf_pages(&self.doc).into_iter().filter(|id| *id != demo).take(3)).collect();
                        let pages: Vec<(String, f32, f32)> = ids.iter().filter_map(|id| export::to_svg(&self.doc, Some(*id), &mut self.fonts, ctx)).collect();
                        let _ = export::save_pdf(&pages, &b.out.with_file_name("bench-export.pdf"));
                        // Batch export: halaman yang sama sebagai file terpisah, dengan satu nama kembar.
                        let mut items: Vec<(String, String, f32, f32)> = ids
                            .iter()
                            .zip(&pages)
                            .map(|(id, (svg, w, h))| (self.doc.find(*id).map(|n| n.name.clone()).unwrap_or_default(), svg.clone(), *w, *h))
                            .collect();
                        items.extend(items.first().cloned());
                        let dir = b.out.with_file_name("bench-batch");
                        let _ = std::fs::create_dir_all(&dir);
                        let (n, err) = export::save_batch(&items, &dir, 0, 0.5);
                        eprintln!("bench batch export: {n} of {} files, error: {err:?}", items.len());
                    }
                    let mut s = b.samples.clone();
                    s.sort_by(|a, b| a.total_cmp(b));
                    let avg = s.iter().sum::<f32>() / s.len().max(1) as f32;
                    let p95 = s.get(s.len() * 95 / 100).copied().unwrap_or(0.0);
                    let mut iv = b.intervals.clone();
                    iv.sort_by(|a, b| a.total_cmp(b));
                    let interval = iv.get(iv.len() / 2).copied().unwrap_or(0.0);
                    let report = format!(
                        "frames={} objects={} doc_frames={} avg_ms={:.2} p95_ms={:.2} max_ms={:.2} interval_ms={:.2} fps={:.0} fonts_loaded={} screenshot={:?}\n",
                        s.len(),
                        self.doc.count(),
                        self.doc.frame_count(),
                        avg,
                        p95,
                        s.last().copied().unwrap_or(0.0),
                        interval,
                        if interval > 0.0 { 1000.0 / interval } else { 0.0 },
                        self.fonts.loaded_count(),
                        shot
                    );
                    let _ = std::fs::write(b.out.with_extension("txt"), report);
                    // Fase kedua: buka editor teks pada judul, seleksi dua huruf, lalu tangkap layar lagi.
                    let headline = self.doc.nodes.iter().flat_map(|n| n.children.iter()).find(|c| c.name == "Headline").map(|c| c.id);
                    if let Some(id) = headline {
                        b.phase = 1;
                        b.mark = b.frame;
                        if let Some(n) = self.doc.find(id) {
                            let o = self.canvas_rect.min;
                            let row = n.y + n.h * 0.25;
                            let p1 = self.view.to_screen(o, n.x + n.w * 0.3, row);
                            let p2 = self.view.to_screen(o, n.x + n.w * 0.52, row);
                            let p3 = self.view.to_screen(o, n.x + 2.0, row);
                            let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
                            let f = b.frame;
                            let mv = egui::Event::PointerMoved;
                            b.inject.extend([(f + 8, mv(p1)), (f + 10, button(p1, true)), (f + 11, button(p1, false)), (f + 13, button(p1, true)), (f + 14, button(p1, false))]);
                            b.inject.extend([(f + 40, mv(p2)), (f + 42, button(p2, true)), (f + 44, button(p2, false)), (f + 60, egui::Event::Text("X".into()))]);
                            b.inject.extend([(f + 62, mv(p3)), (f + 63, button(p3, true))]);
                            for s in 1..=6u32 {
                                b.inject.push((f + 63 + s, mv(p3 + (p2 - p3) * (s as f32 / 6.0))));
                            }
                            b.inject.extend([(f + 71, button(p2, false)), (f + 80, egui::Event::Text("Y".into()))]);
                        }
                        self.bench = Some(b);
                        return;
                    }
                    self.dirty = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    return;
                }
            }
        }
        self.bench = Some(b);
    }
}

/// Mode `--bench`: jadwalkan event pointer buatan.
fn b_inject(queue: &mut Vec<(u32, egui::Event)>, events: &[(u32, egui::Event)]) {
    queue.extend(events.iter().cloned());
}

/// Warna penanda layer dari indeksnya (lihat `LAYER_COLORS`).
fn tag_color(tag: u8) -> Color32 {
    let [r, g, b] = LAYER_COLORS[(tag as usize).min(LAYER_COLORS.len() - 1)].1;
    Color32::from_rgb(r, g, b)
}

/// Latar baris layer yang diberi warna penanda: sapuan tipis selebar baris dan garis tegas di tepi kiri.
fn tag_row(p: &egui::Painter, rect: Rect, tag: u8) {
    if tag == 0 {
        return;
    }
    let (r, c) = (rect.shrink2(vec2(0.0, 1.0)), tag_color(tag));
    p.rect_filled(r, CornerRadius::same(8), c.gamma_multiply(0.2));
    p.rect_filled(Rect::from_min_max(r.min + vec2(0.0, 4.0), pos2(r.left() + 3.0, r.bottom() - 4.0)), CornerRadius::same(2), c);
}

/// A small eye for the layer panel: open (the layer is shown), or struck through (hidden).
fn eye_icon(p: &egui::Painter, c: Pos2, visible: bool, color: Color32) {
    let s = Stroke::new(1.3_f32, color);
    // Two arcs meeting in the corners of the eye.
    let lid = |sign: f32| -> Vec<Pos2> { (0..=8).map(|i| i as f32 / 8.0).map(|t| pos2(c.x - 6.5 + 13.0 * t, c.y - sign * 4.2 * (std::f32::consts::PI * t).sin())).collect() };
    p.add(egui::Shape::line(lid(1.0), s));
    p.add(egui::Shape::line(lid(-1.0), s));
    p.circle_filled(c, 1.8, color);
    if !visible {
        p.line_segment([c + vec2(-6.0, 5.5), c + vec2(6.0, -5.5)], Stroke::new(1.5_f32, color));
    }
}

/// Gembok kecil untuk panel layer: tertutup (terkunci) atau terbuka.
fn lock_icon(p: &egui::Painter, c: Pos2, locked: bool, color: Color32) {
    let s = Stroke::new(1.4_f32, color);
    let body = Rect::from_center_size(c + vec2(0.0, 2.5), vec2(10.0, 7.5));
    if locked {
        p.rect_filled(body, CornerRadius::same(2), color);
    } else {
        p.rect_stroke(body, CornerRadius::same(2), s, StrokeKind::Inside);
    }
    // Lengkung gembok; versi terbuka terangkat di sisi kanan.
    let lift = if locked { 0.0 } else { -2.5 };
    let (l, r, top) = (c.x - 3.0, c.x + 3.0, c.y - 5.5);
    p.add(egui::Shape::line(
        vec![pos2(l, body.top()), pos2(l, top + 1.5), pos2(c.x - 1.5, top), pos2(c.x + 1.5, top), pos2(r, top + 1.5), pos2(r, body.top() + lift)],
        s,
    ));
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(4.0);
    ui.label(theme::title(title));
}

fn sub(ui: &mut egui::Ui, title: &str) {
    ui.label(egui::RichText::new(title).size(11.5).weak());
}

/// Kolom angka dengan label di dalamnya (mis. "X  120").
fn field(ui: &mut egui::Ui, width: f32, label: &str, v: &mut f32) {
    ui.add_sized(
        [width, 28.0],
        egui::DragValue::new(v).speed(1.0).max_decimals(2).prefix(format!("{label}  ")),
    );
}

/// Kolom nilai layer blur. Tidak bisa sampai 0: blur dilepas lewat tombol − di bagian Effects.
fn blur_field(ui: &mut egui::Ui, width: f32, v: &mut f32) -> egui::Response {
    ui.add_sized([width, 28.0], egui::DragValue::new(v).speed(0.25).range(0.1..=500.0).max_decimals(1).prefix("Blur  "))
}

fn icon_button(ui: &mut egui::Ui, selected: bool, tip: &str, draw: impl FnOnce(&egui::Painter, Pos2, Color32)) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 28.0), Sense::click());
    let bg = if selected {
        Color32::from_gray(88)
    } else if resp.hovered() && ui.is_enabled() {
        Color32::from_gray(64)
    } else {
        Color32::from_gray(48)
    };
    ui.painter().rect_filled(rect, CornerRadius::same(8), bg);
    let color = if ui.is_enabled() { Color32::from_gray(232) } else { Color32::from_gray(105) };
    draw(ui.painter(), rect.center(), color);
    resp.on_hover_text(tip).clicked()
}

/// Enam tombol rata; hasilnya mode untuk `App::align`.
fn align_buttons(ui: &mut egui::Ui) -> Option<u8> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (i, (mode, tip)) in [
            (0u8, "Align left  (Alt+A)"),
            (2, "Align horizontal centers  (Alt+H)"),
            (1, "Align right  (Alt+D)"),
            (3, "Align top  (Alt+W)"),
            (5, "Align vertical centers  (Alt+V)"),
            (4, "Align bottom  (Alt+S)"),
        ]
        .into_iter()
        .enumerate()
        {
            if i == 3 {
                ui.add_space(8.0);
            }
            if icon_button(ui, false, tip, |p, c, color| align_icon(p, c, mode, color)) {
                picked = Some(mode);
            }
        }
    });
    picked
}

fn align_icon(p: &egui::Painter, c: Pos2, mode: u8, color: Color32) {
    let (variant, vertical) = match mode {
        0 => (0, false),
        1 => (1, false),
        2 => (2, false),
        3 => (0, true),
        4 => (1, true),
        _ => (2, true),
    };
    let at = |a: f32, b: f32| if vertical { pos2(c.x + b, c.y + a) } else { pos2(c.x + a, c.y + b) };
    let (guide, bars): (f32, [(f32, f32); 2]) = match variant {
        0 => (-7.0, [(-5.0, 6.0), (-5.0, 1.0)]),
        1 => (7.0, [(-6.0, 5.0), (-1.0, 5.0)]),
        _ => (0.0, [(-6.5, 6.5), (-3.5, 3.5)]),
    };
    p.line_segment([at(guide, -7.5), at(guide, 7.5)], Stroke::new(1.2_f32, color));
    for (i, (a0, a1)) in bars.iter().enumerate() {
        let b0 = if i == 0 { -5.0 } else { 1.5 };
        p.rect_filled(Rect::from_two_pos(at(*a0, b0), at(*a1, b0 + 3.5)), CornerRadius::same(1), color);
    }
}

/// The buttons beside the alignment ones: distribute spacing (needs three units) and tidy up
/// (two). Result: the mode for `App::align`.
fn distribute_buttons(ui: &mut egui::Ui, units: usize) -> Option<u8> {
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for (mode, need, tip) in [
            (6u8, 3, "Distribute horizontal spacing  (Alt+Shift+H)"),
            (7, 3, "Distribute vertical spacing  (Alt+Shift+V)"),
            (8, 2, "Tidy up  (Ctrl+Alt+T)"),
        ] {
            if mode == 8 {
                ui.add_space(8.0);
            }
            ui.add_enabled_ui(units >= need, |ui| {
                if icon_button(ui, false, tip, |p, c, color| distribute_icon(p, c, mode, color)) {
                    picked = Some(mode);
                }
            });
        }
    });
    picked
}

fn distribute_icon(p: &egui::Painter, c: Pos2, mode: u8, color: Color32) {
    if mode == 8 {
        // Tidy up: a small grid.
        for (ix, iy) in [(-1.0_f32, -1.0_f32), (0.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (0.0, 1.0), (1.0, 1.0)] {
            p.rect_filled(Rect::from_center_size(c + vec2(ix * 5.5, iy * 3.5), vec2(3.5, 3.5)), CornerRadius::same(1), color);
        }
        return;
    }
    // Two walls with a bar held halfway between them.
    let at = |a: f32, b: f32| if mode == 7 { pos2(c.x + b, c.y + a) } else { pos2(c.x + a, c.y + b) };
    for wall in [-7.0, 7.0] {
        p.line_segment([at(wall, -7.0), at(wall, 7.0)], Stroke::new(1.2_f32, color));
    }
    p.rect_filled(Rect::from_two_pos(at(-2.0, -4.5), at(2.0, 4.5)), CornerRadius::same(1), color);
}

/// Flip buttons. Result: `true` for left-right, `false` for top-bottom.
fn flip_buttons(ui: &mut egui::Ui) -> Option<bool> {
    let mut picked = None;
    ui.spacing_mut().item_spacing.x = 2.0;
    for (horizontal, tip) in [(true, "Flip horizontal  (Shift+H)"), (false, "Flip vertical  (Shift+V)")] {
        if icon_button(ui, false, tip, |p, c, color| flip_icon(p, c, horizontal, color)) {
            picked = Some(horizontal);
        }
    }
    picked
}

/// A triangle and its mirror image on either side of a dashed axis.
fn flip_icon(p: &egui::Painter, c: Pos2, horizontal: bool, color: Color32) {
    let at = |a: f32, b: f32| if horizontal { pos2(c.x + a, c.y + b) } else { pos2(c.x + b, c.y + a) };
    for i in 0..4 {
        let b = -7.0 + i as f32 * 4.0;
        p.line_segment([at(0.0, b), at(0.0, b + 2.0)], Stroke::new(1.0_f32, color));
    }
    let side = |s: f32| vec![at(s * 2.5, -5.0), at(s * 2.5, 5.0), at(s * 8.0, 5.0)];
    p.add(egui::Shape::closed_line(side(-1.0), Stroke::new(1.2_f32, color)));
    p.add(egui::Shape::convex_polygon(side(1.0), color, Stroke::NONE));
}

/// A letter with a line under it, or through it.
fn decoration_icon(p: &egui::Painter, c: Pos2, strike: bool, color: Color32) {
    p.text(c - vec2(0.0, 1.0), Align2::CENTER_CENTER, if strike { "S" } else { "U" }, FontId::proportional(13.0), color);
    let y = if strike { c.y - 0.5 } else { c.y + 7.5 };
    p.line_segment([pos2(c.x - 6.0, y), pos2(c.x + 6.0, y)], Stroke::new(1.2_f32, color));
}

/// Lines of text as a list: plain, with bullets, or with numbers.
fn list_icon(p: &egui::Painter, c: Pos2, mode: u8, color: Color32) {
    let rows: &[f32] = if mode == 2 { &[-4.0, 4.0] } else { &[-5.0, 0.0, 5.0] };
    for (i, dy) in rows.iter().enumerate() {
        let y = c.y + dy;
        match mode {
            1 => {
                p.circle_filled(pos2(c.x - 6.0, y), 1.4, color);
            }
            2 => {
                p.text(pos2(c.x - 6.0, y), Align2::CENTER_CENTER, format!("{}", i + 1), FontId::proportional(8.0), color);
            }
            _ => {}
        }
        let x0 = if mode == 0 { c.x - 7.0 } else { c.x - 2.0 };
        p.line_segment([pos2(x0, y), pos2(c.x + 7.0, y)], Stroke::new(1.4_f32, color));
    }
}

/// Two lines of text at the top, in the middle or at the bottom of their box.
fn valign_icon(p: &egui::Painter, c: Pos2, mode: u8, color: Color32) {
    let (edges, lines): (&[f32], [f32; 2]) = match mode {
        0 => (&[-7.0], [-3.0, 1.0]),
        1 => (&[-7.0, 7.0], [-2.0, 2.0]),
        _ => (&[7.0], [-1.0, 3.0]),
    };
    for dy in edges {
        p.line_segment([pos2(c.x - 8.0, c.y + dy), pos2(c.x + 8.0, c.y + dy)], Stroke::new(1.0_f32, color.gamma_multiply(0.6)));
    }
    for (dy, w) in lines.into_iter().zip([10.0_f32, 6.0]) {
        p.line_segment([pos2(c.x - 5.0, c.y + dy), pos2(c.x - 5.0 + w, c.y + dy)], Stroke::new(1.4_f32, color));
    }
}

fn text_align_icon(p: &egui::Painter, c: Pos2, mode: u8, color: Color32) {
    // Justified: every line as wide as the box.
    let widths = if mode == 3 { [14.0_f32; 4] } else { [14.0, 9.0, 14.0, 7.0] };
    for (i, w) in widths.into_iter().enumerate() {
        let y = c.y - 6.0 + i as f32 * 4.0;
        let x0 = match mode {
            1 => c.x - w * 0.5,
            2 => c.x + 7.0 - w,
            _ => c.x - 7.0,
        };
        p.line_segment([pos2(x0, y), pos2(x0 + w, y)], Stroke::new(1.4_f32, color));
    }
}


/// Titik pegangan dalam koordinat dunia, mengikuti rotasi node.
fn handles(n: &Node, scale: bool) -> Vec<(u8, f32, f32)> {
    let mut list = handles_local(n, scale);
    if n.rotation != 0.0 {
        let (cx, cy) = n.center();
        for h in &mut list {
            (h.1, h.2) = crate::doc::rotate(h.1, h.2, cx, cy, n.rotation);
        }
    }
    list
}
