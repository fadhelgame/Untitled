//! Menggambar node ke canvas: bentuk, gradasi, gambar, path, teks bergaya, rotasi, dan blend mode.

use super::blend;
use super::picker::Picker;
use super::vtext;
use super::{col, View};
use crate::doc::{Asset, Doc, Gradient, Kind, Node};
use crate::fonts::FontManager;
use eframe::egui::{
    self, pos2, vec2, Color32, ColorImage, CornerRadius, FontId, Pos2, Rect, Shape, Stroke, StrokeKind, TextureHandle,
    TextureId,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{mpsc, Arc};

/// Sisi terpanjang tekstur gambar di GPU (file aslinya tetap utuh untuk export).
const MAX_IMAGE_SIDE: u32 = 2048;
const GRADIENT_SIDE: usize = 256;
/// Ukuran glyph terbesar (piksel layar) yang masih diambil dari atlas bitmap egui.
/// Sengaja kecil: luas glyph tumbuh kuadratik, dan atlas 2048px yang penuh dikosongkan lalu
/// diunggah ulang seluruhnya oleh egui (terasa sebagai sendatan saat zoom).
const GLYPH_ATLAS_MAX: f32 = 64.0;
const UV_FULL: Rect = Rect { min: pos2(0.0, 0.0), max: pos2(1.0, 1.0) };

/// Titik path dalam koordinat dunia; pegangan bezier relatif terhadap titiknya.
#[derive(Clone, Copy, PartialEq)]
pub struct PenPt {
    pub x: f32,
    pub y: f32,
    pub hin: [f32; 2],
    pub hout: [f32; 2],
    /// Jari-jari sudut khusus titik ini (lihat `PathPoint::r`).
    pub r: Option<f32>,
}

/// Mode edit titik untuk satu path.
pub struct PathEdit {
    pub id: u64,
    /// Titik terpilih: (kontur, indeks titik). Kontur 0 = kontur utama, sisanya lubang.
    pub sel: Vec<(usize, usize)>,
    /// (kontur, indeks titik, bagian: 0 titik, 1 pegangan masuk, 2 pegangan keluar)
    pub drag: Option<(usize, usize, u8)>,
    pub pre: Option<Doc>,
    pub changed: bool,
    /// Kotak seleksi titik: posisi layar saat mulai ditarik.
    pub marquee: Option<Pos2>,
}

/// Kandidat snapping yang dikumpulkan saat mulai menggeser: (nilai, batas bawah, batas atas).
pub struct Snap {
    pub orig: (f32, f32, f32, f32),
    pub xs: Vec<(f32, f32, f32)>,
    pub ys: Vec<(f32, f32, f32)>,
}

/// Garis bantu yang sedang aktif (koordinat dunia).
pub struct Guide {
    pub vertical: bool,
    pub at: f32,
    pub lo: f32,
    pub hi: f32,
}

/// Seleksi awal saat masuk mode edit teks.
pub enum EditPending {
    /// Pilih seluruh teks (masuk lewat Enter).
    All,
    /// Taruh caret di posisi layar ini (masuk lewat klik dua kali).
    CaretAt(Pos2),
}

/// Kunci tekstur gambar: (id aset, versi siluet putih untuk color overlay).
type TexKey = (u64, bool);
type Decoded = (TexKey, Result<ColorImage, String>);

/// Sisi terpanjang potongan SVG yang dirasterkan ulang untuk zoom besar.
const DETAIL_MAX: f32 = 4096.0;

/// Potongan SVG yang dirasterkan ulang pada resolusi layar (SVG tetap tajam saat di-zoom).
/// `uv` = bagian gambar yang dicakup (0..1), `wpx`/`hpx` = ukuran piksel gambar utuh pada skala itu.
struct Detail {
    key: TexKey,
    uv: Rect,
    wpx: f32,
    hpx: f32,
    tex: TextureHandle,
    frame: u64,
}

/// Tekstur bayangan satu rangkaian (satu node, atau anggota group yang berbayangan bersama);
/// kuncinya id node pertama. `nodes` = keadaan node saat dirender (untuk tahu kapan harus
/// dirender ulang), `scale` = piksel tekstur per satuan dokumen, `region` = area dunia yang dicakup.
struct ShadowTex {
    nodes: Vec<Node>,
    scale: f32,
    region: (f32, f32, f32, f32),
    /// Jumlah font yang bisa dipakai saat dirender: font yang baru selesai diunduh mengubah
    /// tata letak teks, jadi bayangannya harus dirender ulang.
    fonts: usize,
    tex: TextureHandle,
    frame: u64,
}

type ShadowDone = (Vec<Node>, usize, f32, (f32, f32, f32, f32), Result<ColorImage, String>);

/// Tekstur satu rangkaian layer blur (satu node, atau anggota group yang buram bersama), sudah
/// dalam keadaan buram. Arti field-nya sama dengan `ShadowTex`; kuncinya id node pertama.
struct BlurTex {
    nodes: Vec<Node>,
    scale: f32,
    region: (f32, f32, f32, f32),
    fonts: usize,
    tex: TextureHandle,
    frame: u64,
}

type BlurDone = (Vec<Node>, usize, f32, (f32, f32, f32, f32), Result<ColorImage, String>);

struct DetailReq {
    node: u64,
    key: TexKey,
    uv: Rect,
    wpx: f32,
    hpx: f32,
}

/// State tambahan untuk fitur di luar inti editor.
pub struct Ext {
    pub frame: u64,
    pub pen: Option<Vec<PenPt>>,
    pub pen_drag: bool,
    /// Path yang sedang dilanjutkan dengan pen (bukan path baru).
    pub pen_cont: Option<u64>,
    /// Tarikan pen hanya mengubah pegangan keluar (pegangan masuk dibiarkan).
    pub pen_keep_in: bool,
    /// Posisi layar dua klik pen terakhir (untuk mengenali klik ganda sungguhan).
    pub pen_clicks: [Pos2; 2],
    /// Riwayat titik selama menggambar dengan pen (undo/redo per titik, sebelum path jadi objek).
    pub pen_undo: Vec<Vec<PenPt>>,
    pub pen_redo: Vec<Vec<PenPt>>,
    pub path_edit: Option<PathEdit>,
    /// Sesi Shape Builder yang sedang aktif.
    pub builder: Option<super::builder::Session>,
    /// Naik setiap kali dokumen berubah (untuk membatalkan cache yang bergantung padanya).
    pub rev: u64,
    pub snap: Option<Snap>,
    pub guides: Vec<Guide>,
    /// Seleksi karakter terakhir di editor teks (indeks karakter).
    pub edit_sel: Option<(usize, usize)>,
    pub edit_refocus: bool,
    /// Titik awal seleksi (tempat mouse ditekan) dan apakah sedang menyeret.
    pub edit_anchor: usize,
    pub edit_drag: bool,
    /// Klik terakhir di editor teks: (posisi, waktu, hitungan klik beruntun).
    pub edit_click: (Pos2, f64, u8),
    /// Aksi yang menunggu sampai editor siap (setelah frame pertama).
    pub edit_pending: Option<EditPending>,
    /// Posisi caret dan seleksi terakhir + waktunya, untuk kedipan caret.
    pub edit_blink: (usize, Option<(usize, usize)>, f64),
    pub picker: Picker,
    grads: HashMap<u64, (TextureHandle, u64)>,
    tex: HashMap<TexKey, (TextureHandle, u64)>,
    decoding: HashSet<TexKey>,
    /// Decode yang gagal: (frame saat gagal, berapa kali sudah dicoba). Dicoba lagi beberapa kali
    /// dengan jeda, supaya satu kegagalan tidak membuat gambar kosong selamanya.
    decode_failed: HashMap<TexKey, (u64, u8)>,
    /// Pesan untuk status bar (mis. gambar yang tidak bisa dibaca).
    pub notice: Option<String>,
    img_tx: mpsc::Sender<Decoded>,
    img_rx: mpsc::Receiver<Decoded>,
    blend_gl: blend::Shared,
    glyphs: vtext::Cache,
    /// Aset mana yang berupa SVG.
    svg: HashMap<u64, bool>,
    /// Potongan tajam per node gambar SVG, yang sedang dirender, dan aset yang gagal dirender.
    details: HashMap<u64, Detail>,
    detail_busy: HashSet<u64>,
    detail_failed: HashSet<u64>,
    det_tx: mpsc::Sender<(DetailReq, Result<ColorImage, String>)>,
    det_rx: mpsc::Receiver<(DetailReq, Result<ColorImage, String>)>,
    /// Tekstur drop shadow per node, yang sedang dirender, dan yang gagal dirender.
    shadows: HashMap<u64, ShadowTex>,
    shadow_busy: HashSet<u64>,
    shadow_failed: HashSet<u64>,
    sh_tx: mpsc::Sender<ShadowDone>,
    sh_rx: mpsc::Receiver<ShadowDone>,
    /// Tekstur layer blur per rangkaian, yang sedang dirender, dan yang gagal dirender.
    blurs: HashMap<u64, BlurTex>,
    blur_busy: HashSet<u64>,
    blur_failed: HashSet<u64>,
    bl_tx: mpsc::Sender<BlurDone>,
    bl_rx: mpsc::Receiver<BlurDone>,
    pub export_scale: f32,
    pub export_fmt: usize,
    pub v_plain: bool,
    pub paste_seen: bool,
}

impl Default for Ext {
    fn default() -> Self {
        let (img_tx, img_rx) = mpsc::channel();
        let (det_tx, det_rx) = mpsc::channel();
        let (sh_tx, sh_rx) = mpsc::channel();
        let (bl_tx, bl_rx) = mpsc::channel();
        Ext {
            blurs: HashMap::new(),
            blur_busy: HashSet::new(),
            blur_failed: HashSet::new(),
            bl_tx,
            bl_rx,
            svg: HashMap::new(),
            details: HashMap::new(),
            detail_busy: HashSet::new(),
            detail_failed: HashSet::new(),
            det_tx,
            det_rx,
            shadows: HashMap::new(),
            shadow_busy: HashSet::new(),
            shadow_failed: HashSet::new(),
            sh_tx,
            sh_rx,
            frame: 0,
            pen: None,
            pen_drag: false,
            pen_cont: None,
            pen_keep_in: false,
            pen_clicks: [pos2(-1e6, -1e6), pos2(1e6, 1e6)],
            pen_undo: Vec::new(),
            pen_redo: Vec::new(),
            path_edit: None,
            builder: None,
            rev: 0,
            snap: None,
            guides: Vec::new(),
            edit_sel: None,
            edit_refocus: false,
            edit_anchor: 0,
            edit_drag: false,
            edit_click: (Pos2::ZERO, -1.0, 0),
            edit_pending: None,
            edit_blink: (0, None, 0.0),
            picker: Picker::default(),
            grads: HashMap::new(),
            tex: HashMap::new(),
            decoding: HashSet::new(),
            decode_failed: HashMap::new(),
            notice: None,
            img_tx,
            img_rx,
            blend_gl: blend::Shared::default(),
            glyphs: vtext::Cache::default(),
            export_scale: 1.0,
            export_fmt: 0,
            v_plain: false,
            paste_seen: false,
        }
    }
}

impl Ext {
    /// Images, shadows and blurs still being prepared on other threads.
    pub fn busy(&self) -> usize {
        self.decoding.len() + self.detail_busy.len() + self.shadow_busy.len() + self.blur_busy.len()
    }

    /// Mesh vector untuk teks yang di layar lebih besar dari batas atlas glyph (`px` = tinggi
    /// huruf terbesar di layar). `None`: pakai jalur bitmap biasa.
    pub fn vector_text(&mut self, ctx: &egui::Context, fonts: &FontManager, galley: &egui::Galley, origin: Pos2, k: f32, px: f32, clip: Rect) -> Option<egui::Mesh> {
        if px <= GLYPH_ATLAS_MAX {
            return None;
        }
        vtext::mesh(&mut self.glyphs, fonts, galley, origin, k, Some(clip), ctx.pixels_per_point())
    }

    /// Awal tiap frame: terima gambar yang selesai di-decode, buang tekstur yang lama tak terpakai.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        while let Ok((id, result)) = self.img_rx.try_recv() {
            self.decoding.remove(&id);
            if let Err(e) = &result {
                let tries = self.decode_failed.get(&id).map_or(0, |f| f.1) + 1;
                self.decode_failed.insert(id, (self.frame, tries));
                self.notice = Some(format!("Couldn't read an image: {e}"));
            }
            if let Ok(image) = result {
                let options = egui::TextureOptions {
                    mipmap_mode: Some(egui::TextureFilter::Linear),
                    ..egui::TextureOptions::LINEAR
                };
                let handle = ctx.load_texture(format!("image-{}-{}", id.0, id.1), image, options);
                self.tex.insert(id, (handle, self.frame));
                self.decode_failed.remove(&id);
            }
        }
        while let Ok((req, result)) = self.det_rx.try_recv() {
            self.detail_busy.remove(&req.node);
            match result {
                Ok(image) => {
                    let tex = ctx.load_texture(format!("svg-detail-{}", req.node), image, egui::TextureOptions::LINEAR);
                    let detail = Detail { key: req.key, uv: req.uv, wpx: req.wpx, hpx: req.hpx, tex, frame: self.frame };
                    self.details.insert(req.node, detail);
                }
                Err(_) => {
                    self.detail_failed.insert(req.key.0);
                }
            }
        }
        while let Ok((nodes, fonts, scale, region, result)) = self.sh_rx.try_recv() {
            let Some(key) = nodes.first().map(|n| n.id) else { continue };
            self.shadow_busy.remove(&key);
            match result {
                Ok(image) => {
                    let tex = ctx.load_texture(format!("shadow-{key}"), image, egui::TextureOptions::LINEAR);
                    self.shadows.insert(key, ShadowTex { nodes, scale, region, fonts, tex, frame: self.frame });
                }
                Err(_) => {
                    self.shadow_failed.insert(key);
                }
            }
        }
        while let Ok((nodes, fonts, scale, region, result)) = self.bl_rx.try_recv() {
            let Some(key) = nodes.first().map(|n| n.id) else { continue };
            self.blur_busy.remove(&key);
            match result {
                Ok(image) => {
                    let tex = ctx.load_texture(format!("blur-{key}"), image, egui::TextureOptions::LINEAR);
                    self.blurs.insert(key, BlurTex { nodes, scale, region, fonts, tex, frame: self.frame });
                }
                Err(_) => {
                    self.blur_failed.insert(key);
                }
            }
        }
        let frame = self.frame;
        self.blurs.retain(|_, b| b.frame + 300 > frame);
        self.details.retain(|_, d| d.frame + 300 > frame);
        self.shadows.retain(|_, s| s.frame + 300 > frame);
        if self.tex.len() > 6 {
            self.tex.retain(|_, e| e.1 + 1200 > frame);
        }
        self.grads.retain(|_, e| e.1 + 600 > frame);
    }
}

// ---------------------------------------------------------------- gambar & SVG

pub fn is_svg(bytes: &[u8]) -> bool {
    // SVG itu dokumen teks: harus diawali `<` (setelah BOM/spasi). Tanpa cek ini, file biner yang
    // metadatanya memuat SVG (mis. PNG dari ChatGPT dengan chunk C2PA `caBX`) ikut terdeteksi.
    let body = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let start = body.iter().position(|b| !b.is_ascii_whitespace()).unwrap_or(body.len());
    let body = &body[start..];
    if body.first() != Some(&b'<') {
        return false;
    }
    let head = String::from_utf8_lossy(&body[..body.len().min(4096)]);
    head.contains("<svg")
}

fn svg_tree(bytes: &[u8]) -> Result<resvg::usvg::Tree, String> {
    let mut opt = resvg::usvg::Options::default();
    // Memuat font sistem itu lambat; hanya perlu kalau SVG-nya memang berisi teks.
    if bytes.windows(5).any(|w| w == b"<text") {
        opt.fontdb_mut().load_system_fonts();
    }
    resvg::usvg::Tree::from_data(bytes, &opt).map_err(|e| e.to_string())
}

pub fn decode_image(bytes: &[u8]) -> Result<ColorImage, String> {
    if is_svg(bytes) {
        // SVG dirasterkan cukup besar supaya tetap tajam saat di-zoom.
        let tree = svg_tree(bytes)?;
        let size = tree.size();
        let longest = size.width().max(size.height()).max(1.0);
        let scale = (longest * 4.0).clamp(512.0, MAX_IMAGE_SIDE as f32) / longest;
        let (w, h) = ((size.width() * scale).ceil().max(1.0) as u32, (size.height() * scale).ceil().max(1.0) as u32);
        let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("couldn't create the bitmap")?;
        resvg::render(&tree, resvg::tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        return Ok(ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()));
    }
    let mut img = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    if img.width().max(img.height()) > MAX_IMAGE_SIDE {
        img = img.resize(MAX_IMAGE_SIDE, MAX_IMAGE_SIDE, image::imageops::FilterType::Triangle);
    }
    let rgba = img.to_rgba8();
    Ok(ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()))
}

/// Rasterkan sebagian SVG: `uv` = bagian yang diambil (0..1), `wpx`×`hpx` = ukuran piksel
/// gambar utuh pada skala yang diminta.
pub fn render_svg_region(bytes: &[u8], uv: Rect, wpx: f32, hpx: f32) -> Result<ColorImage, String> {
    use resvg::tiny_skia::{Pixmap, Transform};
    let tree = svg_tree(bytes)?;
    let size = tree.size();
    let (w, h) = ((uv.width() * wpx).ceil().max(1.0) as u32, (uv.height() * hpx).ceil().max(1.0) as u32);
    let mut pixmap = Pixmap::new(w, h).ok_or("couldn't create the bitmap")?;
    let transform = Transform::from_scale(wpx / size.width(), hpx / size.height()).post_translate(-uv.min.x * wpx, -uv.min.y * hpx);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Ok(ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()))
}

/// Ukuran alami gambar (piksel, atau satuan dokumen untuk SVG).
pub fn image_size(bytes: &[u8]) -> Option<(f32, f32)> {
    if is_svg(bytes) {
        let size = svg_tree(bytes).ok()?.size();
        return Some((size.width(), size.height()));
    }
    let (w, h) = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok()?;
    Some((w as f32, h as f32))
}

// ---------------------------------------------------------------- gradasi

fn gradient_image(g: &Gradient) -> ColorImage {
    let stops = g.sorted();
    let [x1, y1, x2, y2] = g.line();
    let (dx, dy) = (x2 - x1, y2 - y1);
    let d2 = (dx * dx + dy * dy).max(1e-8);
    let n = GRADIENT_SIDE;
    let mut pixels = Vec::with_capacity(n * n);
    for y in 0..n {
        for x in 0..n {
            let (u, v) = ((x as f32 + 0.5) / n as f32, (y as f32 + 0.5) / n as f32);
            let t = if g.radial {
                (((u - x1).powi(2) + (v - y1).powi(2)) / d2).sqrt()
            } else {
                ((u - x1) * dx + (v - y1) * dy) / d2
            };
            pixels.push(col(Gradient::sample(&stops, t)));
        }
    }
    ColorImage { size: [n, n], pixels }
}

fn gradient_texture(ctx: &egui::Context, ext: &mut Ext, g: &Gradient) -> TextureId {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (g.radial, g.line().map(f32::to_bits)).hash(&mut h);
    for s in &g.stops {
        (s.pos.to_bits(), s.color).hash(&mut h);
    }
    let key = h.finish();
    let frame = ext.frame;
    if let Some(e) = ext.grads.get_mut(&key) {
        e.1 = frame;
        return e.0.id();
    }
    let handle = ctx.load_texture(format!("gradient-{key}"), gradient_image(g), egui::TextureOptions::LINEAR);
    let id = handle.id();
    ext.grads.insert(key, (handle, frame));
    id
}

pub struct PaintCx<'a> {
    pub ctx: &'a egui::Context,
    pub fonts: &'a mut FontManager,
    pub ext: &'a mut Ext,
    pub assets: &'a BTreeMap<u64, Arc<Asset>>,
    pub view: View,
    pub o: Pos2,
    /// Area dunia yang terlihat; node di luar ini dilewati (culling).
    pub vis: (f32, f32, f32, f32),
    pub editing: Option<u64>,
    /// Zoom sedang berubah: pakai ukuran glyph bertingkat supaya atlas font tidak membengkak.
    pub moving: bool,
    pub measures: Vec<(u64, f32, f32)>,
    pub drawn: usize,
    /// Kalau ada: mesh node ditampung di sini, bukan digambar (dipakai clipping mask).
    pub capture: Option<Vec<egui::Mesh>>,
    /// Node berikutnya tidak menggambar bayangannya sendiri: sudah digambar bersama group-nya.
    pub skip_shadow: bool,
}

/// `mask`: siluet putih (hanya alpha) yang diwarnai lewat tint, untuk color overlay.
fn image_texture(pc: &mut PaintCx, id: u64, mask: bool) -> Option<TextureId> {
    let frame = pc.ext.frame;
    let key = (id, mask);
    if let Some(e) = pc.ext.tex.get_mut(&key) {
        e.1 = frame;
        return Some(e.0.id());
    }
    // Yang pernah gagal dicoba lagi paling banyak tiga kali, dengan jeda.
    let waiting = pc.ext.decode_failed.get(&key).is_some_and(|(at, tries)| *tries >= 3 || at + 120 > frame);
    // Decode di thread lain supaya canvas tidak tersendat. `decoding` hanya ditandai kalau thread
    // benar-benar berjalan, dan thread selalu melapor (juga kalau decoder-nya panik); kalau tidak,
    // kuncinya tertahan selamanya dan gambarnya tidak pernah muncul.
    if !waiting && !pc.ext.decoding.contains(&key) {
        if let Some(asset) = pc.assets.get(&id).cloned() {
            pc.ext.decoding.insert(key);
            let (tx, ctx) = (pc.ext.img_tx.clone(), pc.ctx.clone());
            std::thread::spawn(move || {
                let image = std::panic::catch_unwind(|| {
                    let mut image = decode_image(&asset.bytes);
                    if let (true, Ok(image)) = (mask, &mut image) {
                        silhouette(image);
                    }
                    image
                })
                .unwrap_or_else(|_| Err("the decoder crashed".into()));
                let _ = tx.send((key, image));
                ctx.request_repaint();
            });
        }
    }
    None
}

fn silhouette(image: &mut ColorImage) {
    for p in &mut image.pixels {
        let a = p.a();
        *p = Color32::from_rgba_premultiplied(a, a, a, a);
    }
}

/// Untuk gambar SVG yang di layar lebih besar dari teksturnya: tekstur tajam untuk bagian yang
/// terlihat (dirender di thread lain, sementara itu tekstur biasa yang dipakai).
/// `whole`: harus mencakup seluruh gambar (dirotasi / bersudut bulat). Hasil: (tekstur, kotak layar).
fn svg_detail(pc: &mut PaintCx, n: &Node, r: Rect, clip: Rect, whole: bool) -> Option<(TextureId, Rect)> {
    let key: TexKey = (n.image?, n.fill.is_some());
    let base = pc.ext.tex.get(&key)?.0.size();
    let is_svg_asset = match pc.ext.svg.get(&key.0) {
        Some(v) => *v,
        None => {
            let v = pc.assets.get(&key.0).is_some_and(|a| is_svg(&a.bytes));
            pc.ext.svg.insert(key.0, v);
            v
        }
    };
    if !is_svg_asset || r.width() < 1.0 || r.height() < 1.0 {
        return None;
    }
    let ppp = pc.ctx.pixels_per_point();
    let (full_w, full_h) = (r.width() * ppp, r.height() * ppp);
    let beats_base = |w: f32, h: f32| w > base[0] as f32 * 1.15 || h > base[1] as f32 * 1.15;
    if !beats_base(full_w, full_h) {
        return None;
    }
    let vis = r.intersect(clip);
    if !whole && !vis.is_positive() {
        return None;
    }
    let unit = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    let to_uv = |q: Rect| Rect::from_min_max(((q.min - r.min) / r.size()).to_pos2(), ((q.max - r.min) / r.size()).to_pos2());
    // Yang diminta sedikit lebih luas dari yang terlihat, supaya geser sedikit tidak langsung merender ulang.
    let (need, want) = if whole { (unit, unit) } else { (to_uv(vis), to_uv(vis.expand2(vis.size() * 0.25).intersect(r))) };
    let shrink = (DETAIL_MAX / (want.width() * full_w).max(want.height() * full_h)).min(1.0);
    let (wpx, hpx) = (full_w * shrink, full_h * shrink);

    let frame = pc.ext.frame;
    let mut out = None;
    let mut fresh = false;
    if let Some(d) = pc.ext.details.get_mut(&n.id).filter(|d| d.key == key) {
        if d.uv.expand(1e-4).contains_rect(need) {
            d.frame = frame;
            let at = |uv: Pos2| r.min + uv.to_vec2() * r.size();
            out = Some((d.tex.id(), Rect::from_min_max(at(d.uv.min), at(d.uv.max))));
            let ok = |have: f32, wanted: f32| have >= wanted * 0.9 && have <= wanted * 2.0;
            fresh = ok(d.wpx, wpx) && ok(d.hpx, hpx);
        }
    }
    let idle = !pc.moving && !pc.ext.detail_busy.contains(&n.id) && !pc.ext.detail_failed.contains(&key.0);
    if !fresh && idle && beats_base(wpx, hpx) {
        if let Some(asset) = pc.assets.get(&key.0).cloned() {
            pc.ext.detail_busy.insert(n.id);
            let (tx, ctx) = (pc.ext.det_tx.clone(), pc.ctx.clone());
            let req = DetailReq { node: n.id, key, uv: want, wpx, hpx };
            std::thread::spawn(move || {
                let mut image = render_svg_region(&asset.bytes, req.uv, req.wpx, req.hpx);
                if let (true, Ok(image)) = (req.key.1, &mut image) {
                    silhouette(image);
                }
                let _ = tx.send((req, image));
                ctx.request_repaint();
            });
        }
    }
    out
}

/// Mode crop: gambar utuh digambar redup di atas dokumen, lalu bagian di dalam kotak crop
/// digambar penuh di atasnya, supaya terlihat apa yang terpotong.
pub fn paint_crop_ghost(pc: &mut PaintCx, painter: &egui::Painter, n: &Node) {
    let Some(tex) = n.image.and_then(|id| image_texture(pc, id, n.fill.is_some())) else { return };
    let (o, view) = (pc.o, pc.view);
    let rect = |(x0, y0, x1, y1): (f32, f32, f32, f32)| Rect::from_min_max(view.to_screen(o, x0, y0), view.to_screen(o, x1, y1));
    let (full, r) = (rect(n.image_rect()), rect(n.bounds()));
    let uv = n.crop.map_or(UV_FULL, |[u0, v0, u1, v1]| Rect::from_min_max(pos2(u0, v0), pos2(u1, v1)));
    let tint = n.fill.map_or(Color32::WHITE, col);
    let mut mesh = egui::Mesh::with_texture(tex);
    mesh.add_rect_with_uv(full, UV_FULL, tint.gamma_multiply(0.35));
    mesh.add_rect_with_uv(r, uv, tint);
    if n.rotation != 0.0 {
        mesh.rotate(egui::emath::Rot2::from_angle(n.rotation.to_radians()), r.center());
    }
    painter.add(Shape::mesh(mesh));
}

enum Source {
    None,
    Solid(Color32),
    Texture(TextureId, Color32),
}

fn source(pc: &mut PaintCx, n: &Node, alpha: f32) -> Source {
    let tint = Color32::WHITE.gamma_multiply(alpha);
    if n.kind == Kind::Image {
        // Fill pada gambar = color overlay: warnanya mengganti RGB, alpha gambar dipertahankan.
        let overlay = n.fill.map(|c| col(c).gamma_multiply(alpha));
        return match n.image.and_then(|id| image_texture(pc, id, overlay.is_some())) {
            Some(t) => Source::Texture(t, overlay.unwrap_or(tint)),
            // Siluet untuk color overlay belum siap: tampilkan gambar aslinya dulu, bukan kotak abu-abu.
            None => match n.image.filter(|_| overlay.is_some()).and_then(|id| pc.ext.tex.get(&(id, false))) {
                Some(e) => Source::Texture(e.0.id(), tint),
                None => Source::Solid(Color32::from_gray(70).gamma_multiply(alpha)),
            },
        };
    }
    match (n.fill, &n.gradient) {
        (None, _) => Source::None,
        (Some(_), Some(g)) => Source::Texture(gradient_texture(pc.ctx, pc.ext, g), tint),
        (Some(c), None) => Source::Solid(col(c).gamma_multiply(alpha)),
    }
}

/// Triangulasi poligon sederhana (boleh cekung) dengan ear clipping.
pub fn earclip(p: &[Pos2]) -> Vec<u32> {
    let n = p.len();
    if n < 3 {
        return Vec::new();
    }
    let cross = |a: Pos2, b: Pos2, c: Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let area: f32 = (0..n).map(|i| p[i].x * p[(i + 1) % n].y - p[(i + 1) % n].x * p[i].y).sum();
    let mut idx: Vec<usize> = if area > 0.0 { (0..n).collect() } else { (0..n).rev().collect() };
    let mut out = Vec::with_capacity((n - 2) * 3);
    'outer: while idx.len() > 3 {
        let m = idx.len();
        for i in 0..m {
            let (ia, ib, ic) = (idx[(i + m - 1) % m], idx[i], idx[(i + 1) % m]);
            let (a, b, c) = (p[ia], p[ib], p[ic]);
            if cross(a, b, c) <= 0.0 {
                continue; // sudut cekung, bukan "telinga"
            }
            let blocked = idx.iter().any(|&j| {
                j != ia && j != ib && j != ic && {
                    let q = p[j];
                    cross(a, b, q) >= 0.0 && cross(b, c, q) >= 0.0 && cross(c, a, q) >= 0.0
                }
            });
            if !blocked {
                out.extend([ia as u32, ib as u32, ic as u32]);
                idx.remove(i);
                continue 'outer;
            }
        }
        break; // poligon berpotongan sendiri: sisanya diisi kipas
    }
    for i in 1..idx.len().saturating_sub(1) {
        out.extend([idx[0] as u32, idx[i] as u32, idx[i + 1] as u32]);
    }
    out
}

fn tessellate(ctx: &egui::Context, shapes: Vec<Shape>) -> Vec<egui::Mesh> {
    let clipped = shapes.into_iter().map(|shape| egui::epaint::ClippedShape { clip_rect: Rect::EVERYTHING, shape }).collect();
    ctx.tessellate(clipped, ctx.pixels_per_point())
        .into_iter()
        .filter_map(|p| match p.primitive {
            egui::epaint::Primitive::Mesh(m) => Some(m),
            egui::epaint::Primitive::Callback(_) => None,
        })
        .collect()
}

/// Isi bentuk non-kotak dengan tekstur: biarkan egui yang men-tessellate (dengan tepi halus),
/// lalu ganti UV-nya supaya tekstur terbentang di kotak pembatas.
fn textured(pc: &PaintCx, shape: Shape, r: Rect, tex: TextureId) -> Vec<Shape> {
    tessellate(pc.ctx, vec![shape])
        .into_iter()
        .map(|mut mesh| {
            mesh.texture_id = tex;
            for v in &mut mesh.vertices {
                v.uv = pos2((v.pos.x - r.left()) / r.width().max(1e-3), (v.pos.y - r.top()) / r.height().max(1e-3));
            }
            Shape::mesh(mesh)
        })
        .collect()
}

// ---------------------------------------------------------------- teks

/// LayoutJob untuk node teks: satu bagian per rentang gaya. `f` = piksel layout per satuan dunia.
pub fn text_job(n: &Node, text: &str, fonts: &mut FontManager, f: f32, alpha: f32, aligned: bool) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = if n.wrap { (n.w * f).max(1.0) } else { f32::INFINITY };
    if aligned {
        job.halign = match n.align {
            1 => egui::Align::Center,
            2 => egui::Align::RIGHT,
            _ => egui::Align::LEFT,
        };
    }
    let offsets: Vec<usize> = text.char_indices().map(|(i, _)| i).chain(std::iter::once(text.len())).collect();
    let len = offsets.len() - 1;
    let mut format = |st: &crate::doc::Style| {
        let size = (st.size * f).max(0.5);
        egui::TextFormat {
            font_id: FontId::new(size, fonts.resolve(&st.font, st.weight, st.italic)),
            color: col(st.fill).gamma_multiply(alpha),
            extra_letter_spacing: n.letter_spacing / 100.0 * size,
            line_height: (n.line_height > 0.0).then(|| n.line_height / 100.0 * size),
            ..Default::default()
        }
    };
    if len == 0 {
        job.append("", 0.0, format(&n.base_style()));
    }
    for (a, b, st) in n.segments(len) {
        let fmt = format(&st);
        // egui hanya memberi letter spacing di antara huruf dalam satu bagian. Tanpa ini,
        // huruf pertama tiap bagian (mis. satu huruf dengan font lain) tidak ikut merapat/merenggang.
        let at_line_start = a == 0 || text[..offsets[a]].ends_with('\n');
        // egui juga memberi letter spacing ke huruf pertama setelah ganti baris (dihitung dari huruf
        // terakhir baris sebelumnya), sehingga baris kedua dst. bergeser dari tepi kiri. Tiap baris
        // dijadikan bagian sendiri supaya semua baris mulai tepat di tepi yang sama.
        for (i, line) in text[offsets[a]..offsets[b]].split_inclusive('\n').enumerate() {
            let leading = if at_line_start || i > 0 { 0.0 } else { fmt.extra_letter_spacing };
            job.append(line, leading, fmt.clone());
        }
    }
    job
}

#[derive(Clone)]
pub struct TextLayout {
    pub galley: Arc<egui::Galley>,
    /// Ruang di atas baris pertama untuk glyph yang menjulang keluar kotak baris.
    /// Galley harus digambar di (posisi node − pad_top).
    pub pad_top: f32,
    /// Ukuran logis (baris × line height), tanpa pad.
    pub width: f32,
    pub height: f32,
}

/// Posisi x tepi kiri kotak teks di ruang galley (piksel layout; `f` = piksel layout per satuan dunia).
/// egui menaruh baris rata tengah/kanan di sekitar x = 0. Teks lebar-otomatis: kotaknya mulai di
/// baris terpanjang. Paragraf (lebar tetap `w`): x = 0 adalah tengah / tepi kanan kotaknya.
pub fn text_left(wrap: bool, align: u8, w: f32, galley: &egui::Galley, f: f32) -> f32 {
    match (wrap, align) {
        (false, _) => galley.rect.min.x,
        (true, 1) => -w * f * 0.5,
        (true, 2) => -w * f,
        (true, _) => 0.0,
    }
}

/// Ukuran font terbesar node (piksel layout) pada layout acuan.
const TEXT_REF: f32 = 128.0;

/// Galley yang susunannya tidak bergantung pada zoom.
///
/// egui membulatkan ukuran font dan posisi tiap glyph ke piksel, jadi layout langsung di ukuran
/// layar menghasilkan lebar teks, jarak huruf, dan pemenggalan baris yang berubah-ubah saat zoom.
/// Di sini susunan (baris + posisi glyph) selalu diambil dari layout acuan berukuran tetap lalu
/// diskalakan; layout di ukuran layar hanya dipakai untuk bitmap glyph-nya.
#[allow(clippy::too_many_arguments)]
fn stable_galley(ctx: &egui::Context, n: &Node, text: &str, fonts: &mut FontManager, job: egui::text::LayoutJob, f: f32, alpha: f32, aligned: bool) -> Arc<egui::Galley> {
    let f_ref = TEXT_REF / n.max_size().max(1e-3);
    let ref_job = text_job(n, text, fonts, f_ref, alpha, aligned);
    // Tanpa wrap: urutan glyph sama dengan acuan, pemenggalan baris mengikuti acuan.
    let mut flat = job.clone();
    flat.wrap.max_width = f32::INFINITY;
    let reference = fonts.reference_layout(ctx, ref_job);
    let render = ctx.fonts(|ft| ft.layout_job(flat));

    // Glyph layar berurutan, masing-masing dengan 4 vertex-nya (kalau terlihat).
    let mut src = Vec::new();
    for row in &render.rows {
        let mut vi = row.visuals.glyph_vertex_range.start;
        for gl in &row.glyphs {
            let quad = if gl.uv_rect.is_nothing() {
                None
            } else {
                vi += 4;
                row.visuals.mesh.vertices.get(vi - 4..vi)
            };
            src.push((gl, quad));
        }
    }
    let same = reference.rows.iter().flat_map(|r| &r.glyphs).map(|g| g.chr).eq(src.iter().map(|(g, _)| g.chr));
    if !same {
        return ctx.fonts(|ft| ft.layout_job(job));
    }

    let s = f / f_ref;
    let ppp = ctx.pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let scale = |r: Rect| Rect::from_min_max((r.min.to_vec2() * s).to_pos2(), (r.max.to_vec2() * s).to_pos2());
    let mut g = (*reference).clone();
    let mut src = src.into_iter();
    let (mut bounds, mut num_vertices, mut num_indices) = (Rect::NOTHING, 0, 0);
    for row in &mut g.rows {
        let mut mesh = egui::Mesh::default();
        for glyph in &mut row.glyphs {
            let Some((from, quad)) = src.next() else { break };
            let pos = pos2(snap(glyph.pos.x * s), snap(glyph.pos.y * s));
            if let Some(quad) = quad {
                let base = mesh.vertices.len() as u32;
                let d = pos - from.pos;
                mesh.vertices.extend(quad.iter().map(|v| egui::epaint::Vertex { pos: v.pos + d, ..*v }));
                mesh.indices.extend([base, base + 1, base + 2, base + 2, base + 1, base + 3]);
            }
            *glyph = egui::epaint::text::Glyph { pos, advance_width: glyph.advance_width * s, ..from.clone() };
        }
        row.rect = scale(row.rect);
        row.visuals.mesh_bounds = mesh.calc_bounds();
        row.visuals.glyph_index_start = 0;
        row.visuals.glyph_vertex_range = 0..mesh.vertices.len();
        bounds = bounds.union(row.visuals.mesh_bounds);
        num_vertices += mesh.vertices.len();
        num_indices += mesh.indices.len();
        row.visuals.mesh = mesh;
    }
    g.rect = scale(g.rect);
    g.mesh_bounds = bounds;
    g.num_vertices = num_vertices;
    g.num_indices = num_indices;
    g.job = Arc::new(job);
    Arc::new(g)
}

/// Finished text layouts, kept between frames (in `FontManager::layouts`).
///
/// Building one means laying the text out twice and rebuilding the mesh of every glyph; on a
/// canvas that stands still that used to be repeated for every text layer on every frame. An
/// entry is found again by the `LayoutJob` it was built from, which holds everything the result
/// depends on: text, fonts, sizes, colours, spacing, wrap width.
#[derive(Default)]
pub struct TextCache {
    map: HashMap<u64, (f32, TextLayout, u64)>,
    /// A galley out of egui's own cache. egui drops that cache together with the font atlas, so
    /// getting a different one back means the glyph positions in `map` point into an atlas that
    /// is gone.
    probe: Option<Arc<egui::Galley>>,
    pass: u64,
}

impl TextCache {
    /// Entries unused for this many frames are dropped.
    const KEEP: u64 = 240;

    fn key(job: &egui::text::LayoutJob, f: f32) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (job, f.to_bits()).hash(&mut h);
        h.finish()
    }

    /// Once per frame: empty the cache if the font atlas was rebuilt, drop what is no longer used.
    fn begin(&mut self, ctx: &egui::Context) {
        let pass = ctx.cumulative_pass_nr();
        if pass == self.pass && self.probe.is_some() {
            return;
        }
        self.pass = pass;
        let probe = ctx.fonts(|f| f.layout_job(egui::text::LayoutJob::simple_singleline(" ".into(), FontId::proportional(10.0), Color32::BLACK)));
        if !self.probe.as_ref().is_some_and(|old| Arc::ptr_eq(old, &probe)) {
            self.map.clear();
            super::perf::count(1);
        }
        self.probe = Some(probe);
        self.map.retain(|_, e| e.2 + Self::KEEP >= pass);
    }

    fn get(&mut self, key: u64, job: &egui::text::LayoutJob, f: f32) -> Option<TextLayout> {
        let (at, layout, used) = self.map.get_mut(&key)?;
        if *at != f || *layout.galley.job != *job {
            return None;
        }
        *used = self.pass;
        Some(layout.clone())
    }
}

/// Layout teks dengan baseline yang benar.
///
/// egui menaruh tiap glyph di *atas* kotak barisnya dan meratakan kotak huruf beda ukuran
/// di bagian bawah. Akibatnya huruf yang lebih besar turun dari baseline, dan dengan line
/// height khusus glyph keluar dari kotak baris (seleksi meleset, teks terpotong di editor).
/// Di sini tiap baris ditata ulang: semua glyph berbagi satu baseline yang berada di tengah
/// kotak baris, dan kotak baris (untuk seleksi/kursor) mengikuti tinggi huruf sebenarnya.
pub fn layout_text(ctx: &egui::Context, n: &Node, text: &str, fonts: &mut FontManager, f: f32, alpha: f32, aligned: bool) -> TextLayout {
    let _span = super::perf::span(3);
    let job = text_job(n, text, fonts, f, alpha, aligned);
    fonts.layouts.begin(ctx);
    let key = TextCache::key(&job, f);
    if let Some(layout) = fonts.layouts.get(key, &job, f) {
        return layout;
    }
    super::perf::count(0);
    let layout = build_layout(ctx, n, text, fonts, job, f, alpha, aligned);
    let pass = fonts.layouts.pass;
    fonts.layouts.map.insert(key, (f, layout.clone(), pass));
    layout
}

#[allow(clippy::too_many_arguments)]
fn build_layout(ctx: &egui::Context, n: &Node, text: &str, fonts: &mut FontManager, job: egui::text::LayoutJob, f: f32, alpha: f32, aligned: bool) -> TextLayout {
    let galley = stable_galley(ctx, n, text, fonts, job, f, alpha, aligned);
    let uniform = n.line_height <= 0.0 && galley.job.sections.windows(2).all(|w| w[0].format.font_id == w[1].format.font_id);
    let (width, height) = (galley.rect.width(), galley.rect.height());
    if uniform {
        return TextLayout { galley, pad_top: 0.0, width, height };
    }
    let ppp = ctx.pixels_per_point();
    let snap = |v: f32| (v * ppp).round() / ppp;
    let mut g = (*galley).clone();

    // Baseline bersama per baris: (baseline, ascent terbesar, descent terbesar).
    // Dua nilai terakhir: ascent/descent font yang paling banyak dipakai di baris itu, untuk
    // kotak seleksi/kursor (supaya satu huruf dekoratif tidak membuat sorotan jadi sangat tinggi).
    let plan: Vec<Option<(f32, f32, f32, f32, f32)>> = g
        .rows
        .iter()
        .map(|row| {
            if row.glyphs.is_empty() {
                return None;
            }
            let ascent = row.glyphs.iter().map(|gl| gl.font_ascent).fold(0.0, f32::max);
            let descent = row.glyphs.iter().map(|gl| gl.font_height - gl.font_ascent).fold(0.0, f32::max);
            let baseline = snap(row.rect.min.y + (row.rect.height() - (ascent + descent)) * 0.5 + ascent);
            let mut counts: Vec<(f32, f32, usize)> = Vec::new();
            for gl in &row.glyphs {
                let (a, d) = (gl.font_ascent, gl.font_height - gl.font_ascent);
                match counts.iter_mut().find(|c| c.0 == a && c.1 == d) {
                    Some(c) => c.2 += 1,
                    None => counts.push((a, d, 1)),
                }
            }
            let (main_a, main_d, _) = counts.into_iter().max_by_key(|c| c.2).unwrap_or((ascent, descent, 0));
            Some((baseline, ascent, descent, main_a, main_d))
        })
        .collect();
    let ink_top = plan.iter().flatten().map(|p| p.0 - p.1).fold(0.0, f32::min);
    let ink_bottom = plan.iter().flatten().map(|p| p.0 + p.2).fold(height, f32::max);
    let pad = snap(-ink_top);

    let mut bounds = Rect::NOTHING;
    for (row, p) in g.rows.iter_mut().zip(plan) {
        match p {
            Some((baseline, _, _, ascent, descent)) => {
                let mut vi = row.visuals.glyph_vertex_range.start;
                for glyph in &mut row.glyphs {
                    let dy = baseline - glyph.pos.y + pad;
                    glyph.pos.y += dy;
                    if !glyph.uv_rect.is_nothing() {
                        // Tiap glyph yang terlihat = 4 vertex berurutan di mesh baris.
                        if let Some(quad) = row.visuals.mesh.vertices.get_mut(vi..vi + 4) {
                            for v in quad {
                                v.pos.y += dy;
                            }
                        }
                        vi += 4;
                    }
                }
                row.rect.min.y = baseline - ascent + pad;
                row.rect.max.y = baseline + descent + pad;
            }
            None => row.rect = row.rect.translate(vec2(0.0, pad)),
        }
        row.visuals.mesh_bounds = row.visuals.mesh.calc_bounds();
        bounds = bounds.union(row.visuals.mesh_bounds);
    }
    g.rect.max.y = ink_bottom + pad;
    g.mesh_bounds = bounds;
    TextLayout { galley: Arc::new(g), pad_top: pad, width, height }
}

/// Ukuran layout glyph untuk teks setinggi `px` piksel di layar: (ukuran layout, faktor skala).
/// Glyph dirender paling besar `GLYPH_ATLAS_MAX` lalu diskalakan, supaya atlas font tetap kecil.
/// Selama zoom bergerak ukurannya bertingkat (dua tingkat per penggandaan zoom, diperkecil paling
/// jauh ke 0.71x): tiap ukuran baru menambah satu set glyph per font ke atlas.
/// Di atas ukuran itu canvas memakai mesh vector (`vtext`), jadi tidak ada bitmap yang diperbesar.
/// Ukuran layout selalu bilangan bulat piksel fisik: egui membulatkan ukuran font ke piksel,
/// jadi ukuran pecahan membuat glyph tidak sebesar yang diminta (teks tampak membesar-mengecil).
pub fn glyph_scale(px: f32, moving: bool, ppp: f32) -> (f32, f32) {
    let cap = px.clamp(1.0, GLYPH_ATLAS_MAX);
    let lay = if moving { (2f32.powf((cap.log2() * 2.0).ceil() / 2.0) * ppp).ceil() } else { (cap * ppp).round() };
    let lay = lay.max(1.0) / ppp;
    (lay, px / lay)
}

// ---------------------------------------------------------------- node

// ---------------------------------------------------------------- stroke putus-putus

/// Titik-titik keliling kotak bersudut bulat (searah jarum jam dari kiri-atas).
fn rounded_rect_points(r: Rect, radius: f32) -> Vec<Pos2> {
    let rad = radius.min(r.width() * 0.5).min(r.height() * 0.5).max(0.0);
    if rad < 0.5 {
        return vec![r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()];
    }
    let steps = ((rad * 0.5).ceil() as usize).clamp(4, 24);
    let corners = [
        (pos2(r.right() - rad, r.top() + rad), -90.0_f32),
        (pos2(r.right() - rad, r.bottom() - rad), 0.0),
        (pos2(r.left() + rad, r.bottom() - rad), 90.0),
        (pos2(r.left() + rad, r.top() + rad), 180.0),
    ];
    let mut pts = Vec::with_capacity(4 * (steps + 1));
    for (c, from) in corners {
        for i in 0..=steps {
            let a = (from + 90.0 * i as f32 / steps as f32).to_radians();
            pts.push(pos2(c.x + rad * a.cos(), c.y + rad * a.sin()));
        }
    }
    pts
}

fn ellipse_points(c: Pos2, radius: egui::Vec2) -> Vec<Pos2> {
    let steps = ((radius.x + radius.y).sqrt() * 4.0).clamp(16.0, 256.0) as usize;
    (0..steps)
        .map(|i| {
            let a = std::f32::consts::TAU * i as f32 / steps as f32;
            pos2(c.x + radius.x * a.cos(), c.y + radius.y * a.sin())
        })
        .collect()
}

/// Gambar garis putus-putus sepanjang `pts` (ukuran dash dalam satuan dokumen, diskalakan zoom).
fn add_dashed(sink: &mut Sink, mut pts: Vec<Pos2>, closed: bool, s: Stroke, n: &Node, z: f32) {
    if closed {
        if let Some(first) = pts.first().copied() {
            pts.push(first);
        }
    }
    let dash = n.dash * z;
    let gap = if n.gap > 0.0 { n.gap } else { n.dash } * z;
    // Terlalu rapat untuk terlihat sebagai putus-putus: gambar garis utuh saja.
    if dash + gap < 2.0 {
        sink.add(Shape::line(pts, s));
        return;
    }
    for shape in Shape::dashed_line(&pts, s, dash, gap) {
        sink.add(shape);
    }
}

// ---------------------------------------------------------------- drop shadow

/// Resolusi tekstur bayangan (piksel per satuan dokumen): tidak perlu lebih halus dari layar,
/// dan bayangan yang buram cukup dengan resolusi rendah.
fn shadow_scale(blur: f32, screen: f32, side: f32) -> f32 {
    screen.min(8.0 / blur.max(0.5)).min(2048.0 / side.max(1.0)).max(1e-3)
}

fn render_shadow(svg: &str, region: (f32, f32, f32, f32), scale: f32) -> Result<ColorImage, String> {
    use resvg::tiny_skia::{Pixmap, Transform};
    let tree = resvg::usvg::Tree::from_str(svg, &resvg::usvg::Options::default()).map_err(|e| e.to_string())?;
    let (w, h) = (((region.2 - region.0) * scale).ceil().max(1.0) as u32, ((region.3 - region.1) * scale).ceil().max(1.0) as u32);
    let mut pixmap = Pixmap::new(w, h).ok_or("couldn't create the bitmap")?;
    resvg::render(&tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()))
}

/// Gambar drop shadow satu rangkaian (di bawah node-nya): satu node, atau anggota group yang
/// berbayangan bersama (`shadow_run`). Bayangan dirasterkan di thread lain dari SVG yang sama
/// dengan jalur export, lalu disimpan sebagai tekstur; selama versi baru belum jadi, tekstur lama
/// tetap dipakai (digeser mengikuti node, jadi menggeser objek tetap mulus).
fn shadow_layer<T: AsRef<Node>>(pc: &mut PaintCx, painter: &egui::Painter, run: &[T], alpha: f32) {
    let n = run[0].as_ref();
    let Some(shadow) = n.shadow.filter(|s| s.color[3] > 0) else { return };
    if pc.capture.is_some() {
        return;
    }
    let frame = pc.ext.frame;
    let screen = pc.view.zoom * pc.ctx.pixels_per_point();
    let fonts = pc.fonts.usable_count();
    let fresh = pc.ext.shadows.get_mut(&n.id).is_some_and(|c| {
        c.frame = frame;
        let want = shadow_scale(shadow.blur, screen, (c.region.2 - c.region.0).max(c.region.3 - c.region.1));
        let same = c.nodes.len() == run.len() && c.nodes.iter().zip(run).all(|(a, b)| a == b.as_ref());
        same && c.fonts == fonts && c.scale >= want * 0.66 && c.scale <= want * 4.0
    });
    if !fresh && !pc.ext.shadow_busy.contains(&n.id) && !pc.ext.shadow_failed.contains(&n.id) {
        if let Some((svg, region)) = crate::export::shadow_svg(run, pc.assets, pc.fonts, pc.ctx) {
            let scale = shadow_scale(shadow.blur, screen, (region.2 - region.0).max(region.3 - region.1));
            pc.ext.shadow_busy.insert(n.id);
            let nodes: Vec<Node> = run.iter().map(|n| n.as_ref().clone()).collect();
            let (tx, ctx) = (pc.ext.sh_tx.clone(), pc.ctx.clone());
            std::thread::spawn(move || {
                let image = render_shadow(&svg, region, scale);
                let _ = tx.send((nodes, fonts, scale, region, image));
                ctx.request_repaint();
            });
        }
    }
    if let Some(c) = pc.ext.shadows.get(&n.id) {
        let (dx, dy) = (n.x - c.nodes[0].x, n.y - c.nodes[0].y);
        let r = Rect::from_min_max(
            pc.view.to_screen(pc.o, c.region.0 + dx, c.region.1 + dy),
            pc.view.to_screen(pc.o, c.region.2 + dx, c.region.3 + dy),
        );
        let tint = Color32::WHITE.gamma_multiply(alpha);
        painter.add(egui::epaint::RectShape::filled(r, CornerRadius::ZERO, tint).with_texture(c.tex.id(), UV_FULL));
    }
}

// ---------------------------------------------------------------- layer blur

/// Resolusi tekstur layer blur: seperti bayangan, gambar yang buram cukup dengan resolusi rendah.
fn blur_scale(blur: f32, screen: f32, side: f32) -> f32 {
    screen.min(8.0 / blur.max(0.5)).min(4096.0 / side.max(1.0)).max(1e-3)
}

/// Ukur teks lebar-otomatis yang tidak lewat `paint_node` (karena digambar sebagai tekstur buram),
/// supaya ukuran kotaknya tetap mengikuti isi dan font-nya.
fn measure_texts(pc: &mut PaintCx, n: &Node) {
    if !n.visible {
        return;
    }
    for c in &n.children {
        measure_texts(pc, c);
    }
    if n.kind != Kind::Text || n.text.is_empty() {
        return;
    }
    let (max, z) = (n.max_size(), pc.view.zoom);
    if max * z < 3.0 {
        return;
    }
    let (lay, k) = glyph_scale(max * z, pc.moving, pc.ctx.pixels_per_point());
    let layout = layout_text(pc.ctx, n, &n.text, pc.fonts, lay / max, 1.0, true);
    let width = if n.wrap { n.w } else { layout.width * k / z };
    pc.measures.push((n.id, width, layout.height * k / z));
}

/// Gambar satu rangkaian layer blur sebagai tekstur yang sudah buram. Seperti bayangan, teksturnya
/// dirasterkan di thread lain dari SVG jalur export; selama versi baru belum jadi, tekstur lama
/// tetap dipakai (digeser mengikuti node). Hasil `false`: belum ada tekstur sama sekali, atau
/// teksnya sedang diedit — pemanggil menggambar node-nya seperti biasa (tajam).
fn blur_layer<T: AsRef<Node>>(pc: &mut PaintCx, painter: &egui::Painter, run: &[T], alpha: f32) -> bool {
    let first = run[0].as_ref();
    if let Some(id) = pc.editing {
        if run.iter().map(|n| n.as_ref()).any(|n| n.id == id || n.children.iter().any(|c| c.id == id)) {
            return false;
        }
    }
    let Some(now) = crate::export::blur_region(run) else { return true };
    if now.2 < pc.vis.0 || now.0 > pc.vis.2 || now.3 < pc.vis.1 || now.1 > pc.vis.3 {
        return true;
    }
    for n in run {
        measure_texts(pc, n.as_ref());
    }
    let (key, frame) = (first.id, pc.ext.frame);
    let screen = pc.view.zoom * pc.ctx.pixels_per_point();
    let fonts = pc.fonts.usable_count();
    let same = |nodes: &[Node]| nodes.len() == run.len() && nodes.iter().zip(run).all(|(a, b)| a == b.as_ref());
    let fresh = pc.ext.blurs.get_mut(&key).is_some_and(|c| {
        c.frame = frame;
        let (dx, dy) = (first.x - c.nodes[0].x, first.y - c.nodes[0].y);
        if (dx != 0.0 || dy != 0.0) && c.nodes.len() == run.len() {
            // Hanya digeser: isi teksturnya tetap sama, cukup pindahkan areanya.
            let mut moved = c.nodes.clone();
            for n in &mut moved {
                n.translate(dx, dy);
            }
            if same(&moved) {
                c.nodes = moved;
                c.region = (c.region.0 + dx, c.region.1 + dy, c.region.2 + dx, c.region.3 + dy);
            }
        }
        let want = blur_scale(first.blur, screen, (c.region.2 - c.region.0).max(c.region.3 - c.region.1));
        same(&c.nodes) && c.fonts == fonts && c.scale >= want * 0.66 && c.scale <= want * 4.0
    });
    if !fresh && !pc.ext.blur_busy.contains(&key) && !pc.ext.blur_failed.contains(&key) {
        if let Some((svg, region)) = crate::export::blur_svg(run, pc.assets, pc.fonts, pc.ctx) {
            let scale = blur_scale(first.blur, screen, (region.2 - region.0).max(region.3 - region.1));
            pc.ext.blur_busy.insert(key);
            let nodes: Vec<Node> = run.iter().map(|n| n.as_ref().clone()).collect();
            let (tx, ctx) = (pc.ext.bl_tx.clone(), pc.ctx.clone());
            std::thread::spawn(move || {
                let image = render_shadow(&svg, region, scale);
                let _ = tx.send((nodes, fonts, scale, region, image));
                ctx.request_repaint();
            });
        }
    }
    let Some(c) = pc.ext.blurs.get(&key) else { return false };
    let (dx, dy) = (first.x - c.nodes[0].x, first.y - c.nodes[0].y);
    let r = Rect::from_min_max(
        pc.view.to_screen(pc.o, c.region.0 + dx, c.region.1 + dy),
        pc.view.to_screen(pc.o, c.region.2 + dx, c.region.3 + dy),
    );
    // Opacity tiap node sudah termasuk di tekstur; yang dikalikan di sini hanya milik induknya.
    let tint = Color32::WHITE.gamma_multiply(alpha);
    let shape: Shape = egui::epaint::RectShape::filled(r, CornerRadius::ZERO, tint).with_texture(c.tex.id(), UV_FULL).into();
    let mode = crate::export::run_blend(run);
    pc.drawn += 1;
    if pc.capture.is_none() && mode == 0 {
        painter.add(shape);
        return true;
    }
    let meshes = tessellate(pc.ctx, vec![shape]);
    match &mut pc.capture {
        Some(capture) => capture.extend(meshes),
        None => {
            painter.add(blend::callback(pc.ext.blend_gl.clone(), painter.clip_rect(), meshes, mode));
        }
    }
    true
}

/// Potongan daftar saudara: rangkaian ber-layer-blur digambar sebagai satu tekstur buram, sisanya
/// lewat `paint_node`. `after` dipanggil setelah tiap node digambar.
fn paint_run<T: AsRef<Node>>(pc: &mut PaintCx, painter: &egui::Painter, nodes: &[T], alpha: f32, after: &mut dyn FnMut(&Node)) {
    let mut i = 0;
    while i < nodes.len() {
        let end = crate::doc::blur_run(nodes, i);
        let run = &nodes[i..end];
        if !(run[0].as_ref().blur > 0.0 && blur_layer(pc, painter, run, alpha)) {
            let mut k = 0;
            while k < run.len() {
                // Anggota group yang berbayangan bersama: satu bayangan di bawah semuanya.
                let stop = crate::doc::shadow_run(run, k);
                let shared = stop > k + 1;
                if shared {
                    let seen = crate::export::shadow_region(&run[k..stop])
                        .is_some_and(|b| b.2 >= pc.vis.0 && b.0 <= pc.vis.2 && b.3 >= pc.vis.1 && b.1 <= pc.vis.3);
                    if seen {
                        shadow_layer(pc, painter, &run[k..stop], alpha);
                    }
                }
                for n in &run[k..stop] {
                    pc.skip_shadow = shared;
                    paint_node(pc, painter, n.as_ref(), alpha);
                }
                k = stop;
            }
        }
        for n in run {
            after(n.as_ref());
        }
        i = end;
    }
}

/// Penampung bentuk. Node biasa langsung masuk painter; node yang dirotasi atau memakai
/// blend mode dikumpulkan dulu supaya bisa di-tessellate, diputar, dan digambar khusus.
struct Sink<'p> {
    painter: &'p egui::Painter,
    buf: Option<Vec<Shape>>,
}

impl Sink<'_> {
    fn add(&mut self, shape: impl Into<Shape>) {
        match &mut self.buf {
            Some(buf) => buf.push(shape.into()),
            None => {
                self.painter.add(shape);
            }
        }
    }
}

fn flush(pc: &mut PaintCx, sink: &mut Sink, n: &Node, pivot: Pos2) {
    let Some(buf) = sink.buf.as_mut() else { return };
    if buf.is_empty() {
        return;
    }
    let mut meshes = tessellate(pc.ctx, std::mem::take(buf));
    if n.rotation != 0.0 {
        let rot = egui::emath::Rot2::from_angle(n.rotation.to_radians());
        for m in &mut meshes {
            m.rotate(rot, pivot);
        }
    }
    if let Some(capture) = &mut pc.capture {
        capture.extend(meshes);
    } else if n.blend == 0 {
        for m in meshes {
            sink.painter.add(Shape::mesh(m));
        }
    } else {
        sink.painter.add(blend::callback(pc.ext.blend_gl.clone(), sink.painter.clip_rect(), meshes, n.blend));
    }
}

/// Gambar daftar saudara (urutan tumpukan), termasuk clipping mask: layer ber-`clip` hanya tampil
/// di dalam alpha alasnya. `after` dipanggil setelah tiap node digambar.
pub fn paint_list<T: AsRef<Node>>(pc: &mut PaintCx, painter: &egui::Painter, nodes: &[T], alpha: f32, after: &mut dyn FnMut(&Node)) {
    let mut i = 0;
    while i < nodes.len() {
        let (j, k) = crate::doc::clip_run(nodes, i);
        paint_run(pc, painter, &nodes[i..j], alpha, after);
        if k > j {
            // Alas dan isi ditampung sebagai mesh, lalu digabung di GPU: isi × alpha alas.
            let outer = pc.capture.replace(Vec::new());
            paint_run(pc, painter, &nodes[i..j], 1.0, &mut |_| {});
            let mask = pc.capture.replace(Vec::new()).unwrap_or_default();
            paint_run(pc, painter, &nodes[j..k], alpha, &mut |_| {});
            let content = std::mem::replace(&mut pc.capture, outer).unwrap_or_default();
            if !mask.is_empty() && !content.is_empty() {
                painter.add(blend::masked(pc.ext.blend_gl.clone(), painter.clip_rect(), mask, content));
            }
        }
        i = k;
    }
}

pub fn paint_node(pc: &mut PaintCx, painter: &egui::Painter, n: &Node, alpha: f32) {
    let skip_shadow = std::mem::take(&mut pc.skip_shadow);
    if !n.visible {
        return;
    }
    // Opacity diturunkan ke anak (pendekatan; bukan group opacity sungguhan).
    let parent_alpha = alpha;
    let alpha = alpha * n.opacity.clamp(0.0, 1.0);
    let (x0, y0, x1, y1) = n.bounds();
    let cull = if n.rotation != 0.0 { n.aabb() } else { (x0, y0, x1, y1) };
    let pad = n.stroke_w.max(1.0) + if n.kind == Kind::Text { n.max_size() } else { 0.0 } + n.shadow.map_or(0.0, |s| s.pad());
    if cull.2 + pad < pc.vis.0 || cull.0 - pad > pc.vis.2 || cull.3 + pad < pc.vis.1 || cull.1 - pad > pc.vis.3 {
        return;
    }
    // Opacity node sudah termasuk di tekstur bayangan; yang dikalikan di sini hanya milik induknya.
    if !skip_shadow {
        shadow_layer(pc, painter, std::slice::from_ref(n), parent_alpha);
    }
    let z = pc.view.zoom;
    let r = Rect::from_min_max(pc.view.to_screen(pc.o, x0, y0), pc.view.to_screen(pc.o, x1, y1));
    let pivot = r.center();
    let stroke = n
        .stroke
        .filter(|_| n.stroke_w > 0.0)
        .map(|c| Stroke::new(n.stroke_w * z, col(c).gamma_multiply(alpha)));
    pc.drawn += 1;
    let special = n.rotation != 0.0 || n.blend != 0 || pc.capture.is_some();
    let mut sink = Sink { painter, buf: special.then(Vec::new) };
    match n.kind {
        Kind::Frame | Kind::Rect | Kind::Image => {
            let is_frame = n.kind == Kind::Frame;
            if !is_frame && r.width().max(r.height()) < 0.6 {
                return;
            }
            let rad = if is_frame {
                CornerRadius::ZERO
            } else {
                let rad = (n.radius * z).min(r.width() * 0.5).min(r.height() * 0.5).clamp(0.0, 255.0);
                CornerRadius::same(rad.round() as u8)
            };
            match source(pc, n, alpha) {
                Source::None => {}
                Source::Solid(c) => sink.add(Shape::rect_filled(r, rad, c)),
                Source::Texture(t, tint) => {
                    let whole = n.rotation != 0.0 || rad != CornerRadius::ZERO;
                    // Gambar yang di-crop: kotak node hanya menampilkan sebagian dari gambar utuh (`full`).
                    let (full, uv) = match n.crop.filter(|_| n.kind == Kind::Image) {
                        Some([u0, v0, u1, v1]) => {
                            let (ix0, iy0, ix1, iy1) = n.image_rect();
                            let full = Rect::from_min_max(pc.view.to_screen(pc.o, ix0, iy0), pc.view.to_screen(pc.o, ix1, iy1));
                            (full, Rect::from_min_max(pos2(u0, v0), pos2(u1, v1)))
                        }
                        None => (r, UV_FULL),
                    };
                    let detail =
                        if n.kind == Kind::Image { svg_detail(pc, n, full, r.intersect(painter.clip_rect()), whole) } else { None };
                    match detail {
                        Some((sharp, part)) => {
                            // Potongan tajam mencakup `part` dari gambar utuh; yang digambar hanya irisannya dengan kotak node.
                            let q = part.intersect(r);
                            if q.is_positive() {
                                let at = |p: Pos2| ((p - part.min) / part.size()).to_pos2();
                                let rad = if whole { rad } else { CornerRadius::ZERO };
                                sink.add(egui::epaint::RectShape::filled(q, rad, tint).with_texture(sharp, Rect::from_min_max(at(q.min), at(q.max))));
                            }
                        }
                        None => sink.add(egui::epaint::RectShape::filled(r, rad, tint).with_texture(t, uv)),
                    }
                }
            }
            // Frame yang terlalu kecil di layar: isinya tidak perlu digambar.
            if is_frame && r.width() >= 3.0 && r.height() >= 3.0 {
                flush(pc, &mut sink, n, pivot);
                let clip = r.intersect(painter.clip_rect());
                if clip.is_positive() {
                    let p = painter.with_clip_rect(clip);
                    paint_list(pc, &p, &n.children, alpha, &mut |_| {});
                }
            }
            if let Some(s) = stroke {
                let inset = n.stroke_inset() * z;
                if n.dash > 0.0 {
                    let pts = rounded_rect_points(r.shrink(inset), rad.nw as f32 - inset);
                    add_dashed(&mut sink, pts, true, s, n, z);
                } else {
                    let kind = if inset > 0.0 {
                        StrokeKind::Inside
                    } else if inset < 0.0 {
                        StrokeKind::Outside
                    } else {
                        StrokeKind::Middle
                    };
                    sink.add(Shape::rect_stroke(r, rad, s, kind));
                }
            }
        }
        Kind::Ellipse => {
            if r.width().max(r.height()) < 0.6 {
                return;
            }
            let ellipse = |fill: Color32| egui::epaint::EllipseShape::filled(r.center(), r.size() * 0.5, fill);
            match source(pc, n, alpha) {
                Source::None => {}
                Source::Solid(c) => sink.add(ellipse(c)),
                Source::Texture(t, tint) => {
                    for shape in textured(pc, ellipse(tint).into(), r, t) {
                        sink.add(shape);
                    }
                }
            }
            if let Some(s) = stroke {
                let radius = (r.size() * 0.5 - egui::Vec2::splat(n.stroke_inset() * z)).max(egui::Vec2::ZERO);
                if n.dash > 0.0 {
                    add_dashed(&mut sink, ellipse_points(r.center(), radius), true, s, n, z);
                } else {
                    sink.add(egui::epaint::EllipseShape::stroke(r.center(), radius, s));
                }
            }
        }
        Kind::Line => {
            if let Some(s) = stroke {
                let a = pc.view.to_screen(pc.o, n.x, n.y);
                let b = pc.view.to_screen(pc.o, n.x + n.w, n.y + n.h);
                if n.dash > 0.0 {
                    add_dashed(&mut sink, vec![a, b], false, s, n, z);
                } else {
                    sink.add(Shape::line_segment([a, b], s));
                }
            }
        }
        Kind::Path => {
            // Jumlah segmen per kurva mengikuti ukuran di layar (simpangan ±0.1 piksel), supaya
            // kurva tetap mulus saat di-zoom.
            let segs = ((r.width() + r.height()).sqrt() * 1.2).clamp(4.0, 256.0) as usize;
            let (view, o) = (pc.view, pc.o);
            let mut all: Vec<Vec<Pos2>> =
                n.contours(segs).into_iter().map(|c| c.into_iter().map(|(x, y)| view.to_screen(o, x, y)).collect()).collect();
            let pts = all.remove(0);
            let holes = all;
            if pts.len() < 2 {
                return;
            }
            if n.closed && pts.len() >= 3 {
                let (tex, color) = match source(pc, n, alpha) {
                    Source::None => (None, Color32::TRANSPARENT),
                    Source::Solid(c) => (None, c),
                    Source::Texture(t, tint) => (Some(t), tint),
                };
                if color != Color32::TRANSPARENT {
                    let mut mesh = egui::Mesh::default();
                    if let Some(t) = tex {
                        mesh.texture_id = t;
                    }
                    let (verts, indices) = if holes.is_empty() {
                        vtext::fill_polygon(&pts).unwrap_or_else(|| (pts.clone(), earclip(&pts)))
                    } else {
                        let mut cs: Vec<&[Pos2]> = vec![&pts];
                        cs.extend(holes.iter().map(|h| h.as_slice()));
                        vtext::fill_contours(&cs, vtext::FillRule::EvenOdd).unwrap_or_else(|| (pts.clone(), earclip(&pts)))
                    };
                    for p in &verts {
                        let uv = match tex {
                            Some(_) => pos2((p.x - r.left()) / r.width().max(1e-3), (p.y - r.top()) / r.height().max(1e-3)),
                            None => egui::epaint::WHITE_UV,
                        };
                        mesh.vertices.push(egui::epaint::Vertex { pos: *p, uv, color });
                    }
                    mesh.indices = indices;
                    sink.add(Shape::mesh(mesh));
                    if tex.is_none() && stroke.is_none() {
                        // Garis tipis sewarna untuk menghaluskan tepi isian.
                        sink.add(Shape::closed_line(pts.clone(), Stroke::new(1.0_f32, color)));
                        for h in &holes {
                            sink.add(Shape::closed_line(h.clone(), Stroke::new(1.0_f32, color)));
                        }
                    }
                }
            }
            if let Some(s) = stroke {
                if n.dash > 0.0 {
                    add_dashed(&mut sink, pts, n.closed, s, n, z);
                    for h in holes {
                        add_dashed(&mut sink, h, true, s, n, z);
                    }
                } else {
                    sink.add(if n.closed { Shape::closed_line(pts, s) } else { Shape::line(pts, s) });
                    for h in holes {
                        sink.add(Shape::closed_line(h, s));
                    }
                }
            }
        }
        Kind::Text => {
            if pc.editing == Some(n.id) || n.text.is_empty() {
                return;
            }
            let max = n.max_size();
            let px = max * z;
            if px < 3.0 {
                // Teks terlalu kecil untuk dibaca: cukup blok warna (tanpa layout glyph).
                let color = n.fill.map(col).unwrap_or(Color32::BLACK).gamma_multiply(alpha * 0.4);
                let bar = Rect::from_min_size(r.min, vec2((n.w * z).max(1.0), (n.h * z).max(1.0)));
                sink.add(Shape::rect_filled(bar.shrink2(vec2(0.0, bar.height() * 0.22)), CornerRadius::ZERO, color));
                flush(pc, &mut sink, n, pivot);
                return;
            }
            let (lay, k) = glyph_scale(px, pc.moving, pc.ctx.pixels_per_point());
            let layout = layout_text(pc.ctx, n, &n.text, pc.fonts, lay / max, alpha, true);
            // Teks lebar-otomatis rata tengah/kanan: baris galley berada di x negatif.
            let off = vec2(text_left(n.wrap, n.align, n.w, &layout.galley, lay / max), layout.pad_top);
            let width = if n.wrap { n.w } else { layout.width * k / z };
            pc.measures.push((n.id, width, layout.height * k / z));
            if px > GLYPH_ATLAS_MAX {
                // Teks besar: gambar outline glyph sebagai mesh vector supaya tetap tajam.
                let clip = (n.rotation == 0.0).then(|| painter.clip_rect());
                let ppp = pc.ctx.pixels_per_point();
                if let Some(mesh) = vtext::mesh(&mut pc.ext.glyphs, pc.fonts, &layout.galley, r.min - off * k, k, clip, ppp) {
                    if !mesh.indices.is_empty() {
                        sink.add(Shape::mesh(mesh));
                    }
                    flush(pc, &mut sink, n, pivot);
                    return;
                }
            }
            if (k - 1.0).abs() < 0.01 {
                sink.add(Shape::galley(r.min - off, layout.galley, Color32::BLACK));
            } else {
                let pos = (r.min.to_vec2() / k - off).to_pos2();
                let mut shape = Shape::Text(egui::epaint::TextShape::new(pos, layout.galley, Color32::BLACK));
                shape.transform(egui::emath::TSTransform::from_scaling(k));
                sink.add(shape);
            }
        }
    }
    flush(pc, &mut sink, n, pivot);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_files_open_as_images() {
        let svg = br##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60" viewBox="0 0 120 60">
  <rect width="120" height="60" fill="#ff0000"/>
  <circle cx="90" cy="30" r="20" fill="#0000ff"/>
</svg>"##;
        assert!(is_svg(svg));
        assert_eq!(image_size(svg), Some((120.0, 60.0)));
        let img = decode_image(svg).unwrap();
        // Dirasterkan lebih besar dari ukuran aslinya supaya tajam saat di-zoom.
        assert_eq!(img.size, [512, 256]);
        let at = |x: usize, y: usize| img.pixels[y * img.size[0] + x];
        assert_eq!(at(40, 128), Color32::from_rgb(255, 0, 0));
        assert_eq!(at(384, 128), Color32::from_rgb(0, 0, 255));
        // PNG biasa tetap lewat jalur bitmap.
        assert!(!is_svg(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn svg_region_renders_at_requested_scale() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="60" viewBox="0 0 120 60">
  <rect width="120" height="60" fill="#ff0000"/>
  <circle cx="90" cy="30" r="20" fill="#0000ff"/>
</svg>"##;
        // Separuh kanan gambar pada skala 20x: 1200×1200 piksel dari gambar utuh 2400×1200.
        let uv = Rect::from_min_max(pos2(0.5, 0.0), pos2(1.0, 1.0));
        let img = render_svg_region(svg, uv, 2400.0, 1200.0).unwrap();
        assert_eq!(img.size, [1200, 1200]);
        let at = |x: usize, y: usize| img.pixels[y * img.size[0] + x];
        // Pusat lingkaran (90, 30) → (600, 600); sudut kiri atas tetap merah.
        assert_eq!(at(600, 600), Color32::from_rgb(0, 0, 255));
        assert_eq!(at(20, 20), Color32::from_rgb(255, 0, 0));
        // Tepi lingkaran (radius 20 → 400 piksel) tajam: 3 piksel di dalam biru, 3 piksel di luar merah.
        assert_eq!(at(600 + 397, 600), Color32::from_rgb(0, 0, 255));
        assert_eq!(at(600 + 403, 600), Color32::from_rgb(255, 0, 0));
    }

    #[test]
    fn concave_polygon_fill_keeps_its_area() {
        let p: Vec<Pos2> = [(0.0, 0.0), (20.0, 0.0), (20.0, 10.0), (10.0, 10.0), (10.0, 20.0), (0.0, 20.0)].iter().map(|&(x, y)| pos2(x, y)).collect();
        let (v, idx) = vtext::fill_polygon(&p).unwrap();
        let area: f32 = idx
            .chunks(3)
            .map(|t| {
                let (a, b, c) = (v[t[0] as usize], v[t[1] as usize], v[t[2] as usize]);
                ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() * 0.5
            })
            .sum();
        assert!((area - 300.0).abs() < 0.01, "area {area}");
    }

    #[test]
    fn png_with_svg_in_metadata_is_not_svg() {
        // PNG dari ChatGPT membawa chunk C2PA (`caBX`) yang memuat ikon SVG di dekat awal file.
        let mut png = Vec::new();
        let img = image::RgbaImage::from_pixel(7, 5, image::Rgba([10, 20, 30, 255]));
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        // Sisipkan chunk tambahan tepat setelah IHDR (8 byte signature + 25 byte IHDR).
        let payload = br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#;
        let mut chunk = (payload.len() as u32).to_be_bytes().to_vec();
        chunk.extend_from_slice(b"caBX");
        chunk.extend_from_slice(payload);
        chunk.extend_from_slice(&[0; 4]); // CRC chunk tambahan tidak diperiksa decoder
        png.splice(33..33, chunk);
        assert!(!is_svg(&png));
        assert_eq!(image_size(&png), Some((7.0, 5.0)));
        assert!(decode_image(&png).is_ok());
    }

    #[test]
    fn letter_spacing_keeps_lines_aligned() {
        let ctx = egui::Context::default();
        let mut starts = Vec::new();
        let _ = ctx.run(Default::default(), |ctx| {
            let mut fonts = FontManager::new(ctx);
            let mut n = Node::new(1, Kind::Text, 0.0, 0.0);
            n.text = "Penyerahan\nBantuan\nSMP5".into();
            n.font_size = 60.0;
            n.letter_spacing = -6.0;
            let layout = layout_text(ctx, &n, &n.text, &mut fonts, 1.0, 1.0, true);
            starts = layout.galley.rows.iter().map(|r| r.glyphs[0].pos.x).collect();
        });
        assert_eq!(starts.len(), 3);
        assert!(starts.iter().all(|x| (x - starts[0]).abs() < 0.01), "line starts differ: {starts:?}");
    }

    #[test]
    fn text_layout_does_not_depend_on_zoom() {
        let ctx = egui::Context::default();
        // (lebar relatif, posisi huruf terakhir relatif, jumlah baris paragraf) per zoom.
        let mut seen = Vec::new();
        let _ = ctx.run(Default::default(), |ctx| {
            let mut fonts = FontManager::new(ctx);
            let mut n = Node::new(1, Kind::Text, 0.0, 0.0);
            n.text = "Malinau, 16-22 November 2026".into();
            n.font_size = 43.4;
            n.letter_spacing = -4.0;
            let mut para = n.clone();
            para.text = "Pekan olahraga provinsi kedua Kalimantan Utara di Malinau tahun ini".into();
            para.wrap = true;
            para.w = 600.0;
            for zoom in [0.31, 0.5, 0.72, 0.88, 1.08, 1.43, 1.97, 2.94] {
                let (lay, _) = glyph_scale(n.font_size * zoom, false, ctx.pixels_per_point());
                let f = lay / n.font_size;
                let line = layout_text(ctx, &n, &n.text, &mut fonts, f, 1.0, true);
                let last = line.galley.rows[0].glyphs.last().unwrap().pos.x;
                let rows = layout_text(ctx, &para, &para.text, &mut fonts, f, 1.0, true).galley.rows.len();
                seen.push((line.width / f, last / f, 1.0 / f, rows));
            }
        });
        let (w0, x0, _, rows0) = seen[0];
        for &(w, x, px, rows) in &seen {
            assert!((w - w0).abs() < 0.01, "width changes with zoom: {seen:?}");
            // Posisi glyph hanya boleh meleset sebatas pembulatan ke satu piksel layar.
            assert!((x - x0).abs() <= px, "glyph drifts with zoom: {seen:?}");
            assert_eq!(rows, rows0, "line breaks change with zoom: {seen:?}");
        }
    }

    #[test]
    fn earclip_handles_concave_polygons() {
        // Bentuk "L": 6 titik, harus jadi 4 segitiga yang menutup luas 3.
        let p: Vec<Pos2> = [(0.0, 0.0), (2.0, 0.0), (2.0, 1.0), (1.0, 1.0), (1.0, 2.0), (0.0, 2.0)].iter().map(|&(x, y)| pos2(x, y)).collect();
        let idx = earclip(&p);
        assert_eq!(idx.len(), 12);
        let area: f32 = idx
            .chunks(3)
            .map(|t| {
                let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
                ((b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)).abs() * 0.5
            })
            .sum();
        assert!((area - 3.0).abs() < 1e-4, "area {area}");
    }
}
