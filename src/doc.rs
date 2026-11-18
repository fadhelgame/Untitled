//! Model dokumen. Node level atas disimpan sebagai `Arc<Node>` supaya snapshot
//! undo berbagi memori untuk frame yang tidak berubah (ratusan frame tetap murah).

use crate::layout::{AutoLayout, Constraint, Sizing};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Kind {
    Frame,
    Rect,
    Ellipse,
    Line,
    Text,
    Path,
    Image,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Frame => "Frame",
            Kind::Rect => "Rectangle",
            Kind::Ellipse => "Ellipse",
            Kind::Line => "Line",
            Kind::Text => "Text",
            Kind::Path => "Vector",
            Kind::Image => "Image",
        }
    }
}

fn default_font() -> String {
    "Inter".to_owned()
}
fn default_weight() -> u16 {
    400
}
fn default_size() -> f32 {
    16.0
}
fn yes() -> bool {
    true
}
fn one() -> f32 {
    1.0
}

/// Base64 untuk menyimpan byte gambar di dalam file JSON.
pub mod b64 {
    use serde::{Deserialize, Deserializer, Serializer};
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    pub fn encode(data: &[u8]) -> String {
        let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
        for chunk in data.chunks(3) {
            let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
            let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
            out.push(T[(n >> 18) as usize & 63] as char);
            out.push(T[(n >> 12) as usize & 63] as char);
            out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
            out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
        }
        out
    }

    pub fn decode(s: &str) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(s.len() / 4 * 3);
        let (mut acc, mut bits) = (0u32, 0u32);
        for c in s.bytes() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' | b'\n' | b'\r' => continue,
                _ => return None,
            };
            acc = acc << 6 | v as u32;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
            }
        }
        Some(out)
    }

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        decode(&s).ok_or_else(|| serde::de::Error::custom("invalid base64"))
    }
}

/// File gambar asli (PNG/JPEG/…); dibagi lewat `Arc` antar node dan snapshot undo.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Asset {
    #[serde(with = "b64")]
    pub bytes: Vec<u8>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Stop {
    /// 0..1 sepanjang gradasi.
    pub pos: f32,
    pub color: [u8; 4],
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Gradient {
    pub radial: bool,
    /// Derajat, di ruang kotak pembatas objek. 0 = kiri→kanan, 90 = atas→bawah.
    pub angle: f32,
    pub stops: Vec<Stop>,
    /// Garis gradasi linear (x1, y1, x2, y2) di ruang kotak pembatas 0..1; menggantikan `angle` kalau ada.
    #[serde(default)]
    pub line: Option<[f32; 4]>,
    /// Gradasi radial: pusat (x1, y1) dan titik tepi t = 1 (x2, y2), di ruang kotak pembatas 0..1.
    /// Kosong = pusat kotak, jari-jari setengah kotak.
    #[serde(default)]
    pub radial_line: Option<[f32; 4]>,
}

impl Gradient {
    pub fn from_color(c: [u8; 4]) -> Self {
        Gradient {
            radial: false,
            angle: 90.0,
            stops: vec![Stop { pos: 0.0, color: c }, Stop { pos: 1.0, color: [c[0], c[1], c[2], 0] }],
            line: None,
            radial_line: None,
        }
    }

    /// Ubah garis gradasi untuk mode yang sedang aktif (linear atau radial).
    pub fn set_line(&mut self, line: [f32; 4]) {
        if self.radial {
            self.radial_line = Some(line);
        } else {
            self.line = Some(line);
        }
    }

    /// Garis gradasi di ruang kotak pembatas (0..1): titik awal dan akhir.
    /// Radial: pusat dan titik tepi (t = 1).
    pub fn line(&self) -> [f32; 4] {
        if self.radial {
            return self.radial_line.unwrap_or([0.5, 0.5, 1.0, 0.5]);
        }
        self.line.unwrap_or_else(|| {
            // Dari sudut: garis lewat pusat kotak, dari tepi ke tepi.
            let a = self.angle.to_radians();
            let (c, s) = (a.cos(), a.sin());
            let len = (c.abs() + s.abs()).max(1e-4);
            let (dx, dy) = (c / len * 0.5, s / len * 0.5);
            [0.5 - dx, 0.5 - dy, 0.5 + dx, 0.5 + dy]
        })
    }

    pub fn sorted(&self) -> Vec<Stop> {
        let mut s = self.stops.clone();
        s.sort_by(|a, b| a.pos.total_cmp(&b.pos));
        s
    }

    /// Warna (non-premultiplied) di posisi t.
    pub fn sample(sorted: &[Stop], t: f32) -> [u8; 4] {
        let Some(first) = sorted.first() else { return [0, 0, 0, 0] };
        if t <= first.pos {
            return first.color;
        }
        for w in sorted.windows(2) {
            if t <= w[1].pos {
                let span = (w[1].pos - w[0].pos).max(1e-6);
                let k = ((t - w[0].pos) / span).clamp(0.0, 1.0);
                let mut c = [0u8; 4];
                for i in 0..4 {
                    c[i] = (w[0].color[i] as f32 + (w[1].color[i] as f32 - w[0].color[i] as f32) * k).round() as u8;
                }
                return c;
            }
        }
        sorted[sorted.len() - 1].color
    }
}

/// Gaya huruf untuk sebagian teks.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Style {
    pub font: String,
    pub weight: u16,
    pub italic: bool,
    pub size: f32,
    pub fill: [u8; 4],
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
}

/// Rentang karakter [start, end) (indeks karakter, bukan byte) dengan gaya sendiri.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub style: Style,
}

/// Titik path. Posisi dan pegangan bezier dinormalisasi terhadap kotak pembatas node (0..1),
/// jadi memindah/mengubah ukuran node tidak perlu menyentuh titik.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct PathPoint {
    pub x: f32,
    pub y: f32,
    pub hin: [f32; 2],
    pub hout: [f32; 2],
    /// Jari-jari sudut khusus titik ini (satuan dunia). `None` = ikut `Node::radius`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<f32>,
}

#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct Node {
    pub id: u64,
    pub name: String,
    pub kind: Kind,
    /// Koordinat dunia (absolut). Untuk `Line`, w/h boleh negatif (vektor arah).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    #[serde(default)]
    pub fill: Option<[u8; 4]>,
    /// Kalau ada (dan `fill` aktif), fill digambar sebagai gradasi.
    #[serde(default)]
    pub gradient: Option<Gradient>,
    #[serde(default)]
    pub stroke: Option<[u8; 4]>,
    #[serde(default)]
    pub stroke_w: f32,
    #[serde(default)]
    pub radius: f32,
    #[serde(default)]
    pub text: String,
    #[serde(default = "default_font")]
    pub font: String,
    #[serde(default = "default_weight")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
    #[serde(default = "default_size")]
    pub font_size: f32,
    /// Persen dari ukuran font; 0 = otomatis (metrik font).
    #[serde(default)]
    pub line_height: f32,
    /// Persen dari ukuran font.
    #[serde(default)]
    pub letter_spacing: f32,
    /// 0 kiri, 1 tengah, 2 kanan, 3 justify (paragraph text only; otherwise like left).
    #[serde(default)]
    pub align: u8,
    /// Teks paragraf: lebar tetap (`w`), baris membungkus otomatis.
    #[serde(default)]
    pub wrap: bool,
    /// Gaya per rentang karakter; di luar rentang berlaku gaya node.
    #[serde(default)]
    pub spans: Vec<Span>,
    /// Underline / strikethrough of the node's own style (ranges carry theirs in `Style`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
    /// List: 0 none, 1 bullets, 2 numbered. Every paragraph gets a marker and a hanging indent;
    /// the markers are drawn, they are not part of `text`.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub list: u8,
    /// Where the lines sit in a box of fixed height (`fixed_h`): 0 top, 1 middle, 2 bottom.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub valign: u8,
    /// Text: the height `h` was set by hand and no longer follows the lines. Lines that do not
    /// fit run out of the box.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fixed_h: bool,
    /// Text on a path: the id of the node (a path, line, ellipse or rectangle of the same page)
    /// this text runs along, as one line with its baseline on that shape. See `textpath.rs`.
    /// The box of the text is then the box around its glyphs, kept up to date by the canvas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on_path: Option<u64>,
    /// How far along the path the text is moved from where its alignment puts it (world units).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub path_offset: f32,
    /// The text stands on the other side of the path and runs the other way.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub path_side: bool,
    #[serde(default)]
    pub points: Vec<PathPoint>,
    #[serde(default)]
    pub closed: bool,
    /// Kontur tertutup tambahan (lubang / bagian terpisah) di ruang kotak pembatas yang sama.
    /// Isian memakai aturan even-odd, jadi kontur di dalam kontur utama menjadi lubang.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub holes: Vec<Vec<PathPoint>>,
    /// Kunci ke `Doc::assets`.
    #[serde(default)]
    pub image: Option<u64>,
    /// Crop gambar: bagian gambar yang tampil di kotak node, sebagai (u0, v0, u1, v1) dalam 0..1
    /// terhadap gambar utuh. Kosong = seluruh gambar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<[f32; 4]>,
    /// Image adjustments (exposure, contrast, …), applied when the image is drawn and exported;
    /// the asset keeps its pixels. See `adjust.rs`. Read it through `Node::adjustment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adjust: Option<crate::adjust::Adjust>,
    /// Derajat, searah jarum jam, terhadap pusat kotak pembatas.
    #[serde(default)]
    pub rotation: f32,
    /// Mirrored left-right / top-bottom about the centre of the box, before `rotation` applies.
    /// Paths and lines never carry these: flipping one mirrors its points instead (`Node::mirror`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flip_h: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flip_v: bool,
    /// Indeks ke daftar blend mode (0 = Normal).
    #[serde(default)]
    pub blend: u8,
    #[serde(default = "one")]
    pub opacity: f32,
    #[serde(default = "yes")]
    pub visible: bool,
    /// Terkunci: tidak bisa dipilih atau digeser dari canvas (masih bisa dipilih di panel layer).
    /// Frame yang terkunci ikut mengunci isinya.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// Warna penanda di panel layer: indeks ke `LAYER_COLORS` (0 = tanpa warna). Tidak memengaruhi gambar.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub tag: u8,
    /// Id group (0 = tidak digroup). Node dengan id group yang sama dipilih dan diubah bersama.
    #[serde(default)]
    pub group: u64,
    /// Clipping mask: node ini hanya tampil di dalam bentuk (alpha) layer tepat di bawahnya.
    #[serde(default)]
    pub clip: bool,
    /// Stroke putus-putus: panjang garis dan jarak antar garis (0 = garis utuh; jarak 0 = sama
    /// dengan panjang garis).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dash: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub gap: f32,
    /// Posisi stroke terhadap tepi bentuk: 0 dalam, 1 tengah, 2 luar. `None` = bawaan jenis
    /// objeknya (kotak, frame, gambar: dalam; lainnya: tengah).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_align: Option<u8>,
    /// Ends of an open stroke and of each dash: 0 butt, 1 round, 2 square.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub cap: u8,
    /// Corners of a stroke: 0 miter (cut off to a bevel where the corner is sharper than the
    /// miter limit of 4 allows, as in SVG), 1 round, 2 bevel.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub join: u8,
    /// Drop shadow di belakang objek.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    /// Layer blur: seluruh objek (beserta bayangannya, dan isi frame) diburamkan. Satuan dokumen,
    /// kebiasaan CSS/Figma (simpangan baku = setengah nilai ini); 0 = tidak buram.
    /// Anggota satu group yang nilainya sama diburamkan sebagai satu kesatuan (lihat `blur_run`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blur: f32,
    /// Inner shadow: a shadow that falls inside the shape, over its fill and under its stroke
    /// (in a frame: under what the frame holds). Same units and blur convention as `shadow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inner_shadow: Option<Shadow>,
    /// Background blur: whatever is painted behind the node shows blurred inside its shape
    /// ("frosted glass"), under its own fill. Same convention as `blur`; 0 = none.
    /// See `Node::backdrop` for where it applies.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bg_blur: f32,
    /// Color style yang dipakai fill / stroke, dan text style (id ke `Doc::colors` / `Doc::type_styles`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_style: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_style: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_style: Option<u64>,
    /// Main component: instances of it (frames whose `instance_of` is this id) follow its
    /// changes. See `component.rs`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub component: bool,
    /// Instance: the id of the main component this frame is a copy of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_of: Option<u64>,
    /// Inside an instance: the id of the node in the main component that this one mirrors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<u64>,
    /// In a frame: how this node keeps its place when the frame is resized. See `layout.rs`.
    #[serde(default, skip_serializing_if = "Constraint::is_default")]
    pub constrain_h: Constraint,
    #[serde(default, skip_serializing_if = "Constraint::is_default")]
    pub constrain_v: Constraint,
    /// A frame that places its children itself: in a row, a column or wrapping rows. See `layout.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<AutoLayout>,
    /// How wide and high this node is: as set, as large as what it holds (a frame with `layout`),
    /// or as much as the auto layout of the frame around it has left.
    #[serde(default, skip_serializing_if = "Sizing::is_default")]
    pub size_h: Sizing,
    #[serde(default, skip_serializing_if = "Sizing::is_default")]
    pub size_v: Sizing,
    /// In a frame with `layout`: left out of it, placed by hand and by its constraints.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub absolute: bool,
    #[serde(default)]
    pub children: Vec<Node>,
}

/// Drop shadow: bayangan di belakang objek. Offset dan blur dalam satuan dokumen;
/// blur mengikuti kebiasaan CSS/Figma (simpangan baku = setengah nilai blur).
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Shadow {
    pub x: f32,
    pub y: f32,
    pub blur: f32,
    pub color: [u8; 4],
}

impl Default for Shadow {
    fn default() -> Self {
        Shadow { x: 0.0, y: 4.0, blur: 12.0, color: [0, 0, 0, 64] }
    }
}

impl Shadow {
    /// Seberapa jauh bayangan bisa melebar keluar dari objeknya, ke arah mana pun.
    pub fn pad(&self) -> f32 {
        self.x.abs().max(self.y.abs()) * 1.5 + self.blur.max(0.0) * 1.5 + 1.0
    }
}

impl Node {
    /// Posisi stroke yang berlaku: 0 dalam, 1 tengah, 2 luar. Hanya kotak, frame, gambar, dan
    /// elips yang bisa memilih; garis dan path selalu di tengah.
    pub fn stroke_pos(&self) -> u8 {
        match self.kind {
            Kind::Rect | Kind::Frame | Kind::Image => self.stroke_align.unwrap_or(0).min(2),
            Kind::Ellipse => self.stroke_align.unwrap_or(1).min(2),
            _ => 1,
        }
    }

    /// Seberapa jauh garis tengah stroke masuk dari tepi bentuk (negatif = keluar).
    pub fn stroke_inset(&self) -> f32 {
        match self.stroke_pos() {
            0 => self.stroke_w * 0.5,
            2 => -self.stroke_w * 0.5,
            _ => 0.0,
        }
    }

    /// The image adjustments that are applied, if any: those of an image that change something.
    pub fn adjustment(&self) -> Option<crate::adjust::Adjust> {
        self.adjust.filter(|a| self.kind == Kind::Image && !a.is_identity())
    }

    /// The inner shadow that is drawn, if any: a shape that has an inside.
    pub fn inner(&self) -> Option<Shadow> {
        let has_inside = match self.kind {
            Kind::Line => false,
            Kind::Path => self.closed,
            _ => true,
        };
        self.inner_shadow.filter(|s| has_inside && s.color[3] > 0)
    }

    /// The background blur that is drawn (0 = none). Rectangles, ellipses, closed paths, images
    /// and frames have it, in their shape. A layer with a layer blur is one blurred picture
    /// already and has none.
    pub fn backdrop(&self) -> f32 {
        let shaped = match self.kind {
            Kind::Rect | Kind::Ellipse | Kind::Frame | Kind::Image => true,
            Kind::Path => self.closed,
            Kind::Line | Kind::Text => false,
        };
        if shaped && self.blur <= 0.0 && self.bg_blur > 0.0 {
            self.bg_blur
        } else {
            0.0
        }
    }

    /// Seberapa jauh layer blur melebar keluar dari objeknya (3 simpangan baku).
    pub fn blur_pad(&self) -> f32 {
        if self.blur > 0.0 {
            self.blur * 1.5 + 1.0
        } else {
            0.0
        }
    }
}

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

/// Warna penanda layer (nama, RGB). Indeks 0 = tanpa warna.
pub const LAYER_COLORS: [(&str, [u8; 3]); 9] = [
    ("None", [0, 0, 0]),
    ("Red", [242, 72, 72]),
    ("Orange", [255, 150, 50]),
    ("Yellow", [255, 205, 41]),
    ("Green", [20, 174, 92]),
    ("Blue", [13, 153, 255]),
    ("Purple", [151, 71, 255]),
    ("Pink", [255, 102, 196]),
    ("Gray", [140, 140, 150]),
];

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Rangkaian layer blur mulai dari `nodes[i]`: indeks akhirnya (eksklusif). Node yang diburamkan
/// dan menjadi anggota group digabung dengan anggota berikutnya yang nilai blur-nya sama, supaya
/// group itu buram sebagai satu gambar (bukan tiap anggota sendiri-sendiri).
pub fn blur_run<T: AsRef<Node>>(nodes: &[T], i: usize) -> usize {
    let n = nodes[i].as_ref();
    let mut j = i + 1;
    if n.blur > 0.0 && n.group != 0 {
        while nodes.get(j).map(|m| m.as_ref()).is_some_and(|m| m.group == n.group && m.blur == n.blur) {
            j += 1;
        }
    }
    j
}

/// Rangkaian drop shadow mulai dari `nodes[i]`: indeks akhirnya (eksklusif). Anggota group yang
/// berurutan dengan bayangan yang sama membentuk satu bayangan bersama (siluet gabungan), jadi
/// bayangan satu anggota tidak jatuh di atas anggota lain. Anggota ber-layer-blur tidak ikut.
pub fn shadow_run<T: AsRef<Node>>(nodes: &[T], i: usize) -> usize {
    let n = nodes[i].as_ref();
    let mut j = i + 1;
    if n.group != 0 && n.blur <= 0.0 && n.shadow.is_some_and(|s| s.color[3] > 0) {
        while nodes.get(j).map(|m| m.as_ref()).is_some_and(|m| m.group == n.group && m.blur <= 0.0 && m.shadow == n.shadow) {
            j += 1;
        }
    }
    j
}

impl AsRef<Node> for Node {
    fn as_ref(&self) -> &Node {
        self
    }
}

/// Rangkaian clipping mask mulai dari `nodes[i]` (urutan tumpukan, bawah → atas):
/// alas = `nodes[i..j]`, layer yang di-clip ke alas itu = `nodes[j..k]` (kosong kalau tidak ada).
/// Alas berupa satu node, atau seluruh anggota group yang berurutan. Frame tidak ikut clipping.
pub fn clip_run<T: AsRef<Node>>(nodes: &[T], i: usize) -> (usize, usize) {
    let n = nodes[i].as_ref();
    let mut j = i + 1;
    if n.kind == Kind::Frame || n.clip {
        return (j, j);
    }
    let free = |m: &Node| m.kind != Kind::Frame;
    if n.group != 0 {
        while nodes.get(j).map(|m| m.as_ref()).is_some_and(|m| m.group == n.group && !m.clip && free(m)) {
            j += 1;
        }
    }
    let mut k = j;
    while nodes.get(k).map(|m| m.as_ref()).is_some_and(|m| m.clip && free(m)) {
        k += 1;
    }
    (j, k)
}

/// Buat clipping mask di satu daftar saudara. Satu objek/group terpilih: di-clip ke layer di
/// bawahnya. Beberapa terpilih: yang paling bawah jadi alas, sisanya dipindah tepat di atasnya
/// dan di-clip. Hasil `false`: tidak ada yang bisa dijadikan alas.
fn clip_siblings<T: AsRef<Node>>(v: &mut Vec<T>, ids: &[u64], node: impl Fn(&mut T) -> &mut Node) -> bool {
    let picked: Vec<usize> = (0..v.len()).filter(|&i| ids.contains(&v[i].as_ref().id) && v[i].as_ref().kind != Kind::Frame).collect();
    let Some(&first) = picked.first() else { return false };
    let g = v[first].as_ref().group;
    let first_id = v[first].as_ref().id;
    let in_base = move |n: &Node| n.id == first_id || (g != 0 && n.group == g);
    if picked.iter().all(|&i| in_base(v[i].as_ref())) {
        // Satu unit: butuh layer bukan-frame di bawahnya.
        if first == 0 || v[first - 1].as_ref().kind == Kind::Frame {
            return false;
        }
        for i in picked {
            node(&mut v[i]).clip = true;
        }
        return true;
    }
    let base_ids: Vec<u64> = picked.iter().map(|&i| v[i].as_ref()).filter(|n| in_base(n)).map(|n| n.id).collect();
    let mut moved = Vec::new();
    for &i in picked.iter().rev() {
        if !base_ids.contains(&v[i].as_ref().id) {
            let mut t = v.remove(i);
            node(&mut t).clip = true;
            moved.push(t);
        }
    }
    moved.reverse();
    let at = v.iter().rposition(|t| base_ids.contains(&t.as_ref().id)).map_or(0, |i| i + 1);
    v.splice(at..at, moved);
    true
}

/// Potong `range` jadi rangkaian berurutan; `joins(i)`: item `i` menyambung rangkaian item `i - 1`.
fn runs(range: std::ops::Range<usize>, joins: impl Fn(usize) -> bool) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    for i in range {
        match out.last_mut() {
            Some(r) if joins(i) => r.end = i + 1,
            _ => out.push(i..i + 1),
        }
    }
    out
}

/// Urutan baru setelah item terpilih digeser: paling depan/belakang, atau (`step`) satu tingkat.
/// Urutan relatif sesama yang terpilih, dan sesama yang tidak terpilih, tetap.
fn shifted(sel: &[bool], up: bool, step: bool) -> Vec<usize> {
    let mut order: Vec<usize> = (0..sel.len()).collect();
    if !step {
        order.sort_by_key(|i| sel[*i] == up);
    } else if up {
        for i in (0..sel.len().saturating_sub(1)).rev() {
            if sel[order[i]] && !sel[order[i + 1]] {
                order.swap(i, i + 1);
            }
        }
    } else {
        for i in 1..sel.len() {
            if sel[order[i]] && !sel[order[i - 1]] {
                order.swap(i, i - 1);
            }
        }
    }
    order
}

/// Ubah urutan tumpukan node `ids` dalam satu daftar saudara. Group (beserta layer yang di-clip
/// ke alasnya) bergerak sebagai satu kesatuan: isinya tidak berubah urutan, dan layer lain
/// melompati seluruh group, tidak menyelip ke dalamnya. Kalau hanya sebagian anggota group yang
/// dipilih, anggota itu bergeser di dalam group-nya saja.
fn restack<T: AsRef<Node>>(list: &mut Vec<T>, ids: &[u64], up: bool, step: bool) {
    // (terpilih, group, clip)
    let meta: Vec<(bool, u64, bool)> = list.iter().map(|t| t.as_ref()).map(|n| (ids.contains(&n.id), n.group, n.clip)).collect();
    if !meta.iter().any(|m| m.0) {
        return;
    }
    let units = runs(0..meta.len(), |i| meta[i].2 || (meta[i].1 != 0 && meta[i].1 == meta[i - 1].1));
    // Satu kesatuan ikut pindah utuh kalau alasnya (seluruh anggota group, atau node tunggal) terpilih.
    let whole: Vec<bool> = units
        .iter()
        .map(|u| {
            let g = meta[u.start].1;
            u.clone().filter(|i| *i == u.start || (g != 0 && meta[*i].1 == g)).all(|i| meta[i].0)
        })
        .collect();
    let mut order: Vec<usize> = Vec::with_capacity(meta.len());
    for ui in shifted(&whole, up, step) {
        let u = units[ui].clone();
        if whole[ui] {
            order.extend(u);
            continue;
        }
        // Sebagian saja yang terpilih: geser di antara sesama anggota group (atau sesama layer ter-clip).
        for r in runs(u, |i| meta[i].1 == meta[i - 1].1 && (meta[i].1 != 0 || (meta[i].2 && meta[i - 1].2))) {
            let sel: Vec<bool> = r.clone().map(|i| meta[i].0).collect();
            order.extend(shifted(&sel, up, step).into_iter().map(|j| r.start + j));
        }
    }
    if order.iter().enumerate().all(|(i, j)| i == *j) {
        return;
    }
    let mut slots: Vec<Option<T>> = std::mem::take(list).into_iter().map(Some).collect();
    *list = order.into_iter().filter_map(|i| slots[i].take()).collect();
}

/// Kumpulkan elemen yang cocok supaya berurutan, di posisi elemen cocok yang paling depan.
fn gather<T>(v: &mut Vec<T>, is: impl Fn(&T) -> bool) {
    let Some(last) = v.iter().rposition(&is) else { return };
    let (mut picked, mut rest) = (Vec::new(), Vec::with_capacity(v.len()));
    let mut at = 0;
    for (i, item) in std::mem::take(v).into_iter().enumerate() {
        if is(&item) {
            picked.push(item);
        } else {
            if i < last {
                at += 1;
            }
            rest.push(item);
        }
    }
    rest.splice(at..at, picked);
    *v = rest;
}

/// The front-most visible, unlocked node of a sibling list under a point. A frame under the
/// point gives way to what it holds, up to `depth` levels further down (0 = the frame itself);
/// a hidden or locked frame hides or locks everything in it. An instance at the top level is
/// picked as a whole by a plain click (`depth` 1), like a nested frame.
fn hit_list<'a, T: AsRef<Node>>(doc: &Doc, list: &'a [T], x: f32, y: f32, tol: f32, depth: usize) -> Option<&'a Node> {
    for n in list.iter().rev().map(|n| n.as_ref()) {
        if !n.visible || n.locked {
            continue;
        }
        if n.kind == Kind::Frame {
            if n.contains(x, y, 0.0) {
                let inner = if depth > 0 && !(depth == 1 && n.instance_of.is_some()) { hit_list(doc, &n.children, x, y, tol, depth - 1) } else { None };
                return Some(inner.unwrap_or(n));
            }
        } else if if n.kind == Kind::Text && n.on_path.is_some() { doc.hits_path_text(n, x, y, tol) } else { n.contains(x, y, tol) } {
            return Some(n);
        }
    }
    None
}

/// Putar titik (x, y) terhadap (cx, cy) sebesar `deg` derajat (searah jarum jam di layar).
pub fn rotate(x: f32, y: f32, cx: f32, cy: f32, deg: f32) -> (f32, f32) {
    let (s, c) = deg.to_radians().sin_cos();
    let (dx, dy) = (x - cx, y - cy);
    (cx + dx * c - dy * s, cy + dx * s + dy * c)
}

fn dist_to_segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON { 0.0 } else { (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0) };
    let (cx, cy) = (a.0 + dx * t, a.1 + dy * t);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

impl Node {
    pub fn new(id: u64, kind: Kind, x: f32, y: f32) -> Self {
        Node {
            id,
            name: format!("{} {}", kind.label(), id),
            kind,
            x,
            y,
            w: 0.0,
            h: 0.0,
            fill: None,
            gradient: None,
            stroke: None,
            stroke_w: 1.0,
            radius: 0.0,
            text: String::new(),
            font: default_font(),
            weight: 400,
            italic: false,
            font_size: 16.0,
            line_height: 0.0,
            letter_spacing: 0.0,
            align: 0,
            wrap: false,
            spans: Vec::new(),
            underline: false,
            strike: false,
            list: 0,
            valign: 0,
            fixed_h: false,
            on_path: None,
            path_offset: 0.0,
            path_side: false,
            points: Vec::new(),
            closed: false,
            holes: Vec::new(),
            image: None,
            crop: None,
            adjust: None,
            rotation: 0.0,
            flip_h: false,
            flip_v: false,
            blend: 0,
            opacity: 1.0,
            visible: true,
            locked: false,
            tag: 0,
            group: 0,
            clip: false,
            dash: 0.0,
            gap: 0.0,
            stroke_align: None,
            cap: 0,
            join: 0,
            shadow: None,
            blur: 0.0,
            inner_shadow: None,
            bg_blur: 0.0,
            fill_style: None,
            stroke_style: None,
            type_style: None,
            component: false,
            instance_of: None,
            source: None,
            constrain_h: Constraint::Min,
            constrain_v: Constraint::Min,
            layout: None,
            size_h: Sizing::Fixed,
            size_v: Sizing::Fixed,
            absolute: false,
            children: Vec::new(),
        }
    }

    /// Kotak pembatas ternormalisasi: (x0, y0, x1, y1).
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let (x0, x1) = if self.w < 0.0 { (self.x + self.w, self.x) } else { (self.x, self.x + self.w) };
        let (y0, y1) = if self.h < 0.0 { (self.y + self.h, self.y) } else { (self.y, self.y + self.h) };
        (x0, y0, x1, y1)
    }

    /// Kotak gambar utuh (sebelum crop) di ruang node tanpa rotasi; sama dengan `bounds` kalau
    /// tidak di-crop.
    pub fn image_rect(&self) -> (f32, f32, f32, f32) {
        let (x0, y0, x1, y1) = self.bounds();
        match self.crop {
            Some([u0, v0, u1, v1]) if u1 - u0 > 1e-4 && v1 - v0 > 1e-4 => {
                let (w, h) = ((x1 - x0) / (u1 - u0), (y1 - y0) / (v1 - v0));
                let (ix, iy) = (x0 - u0 * w, y0 - v0 * h);
                (ix, iy, ix + w, iy + h)
            }
            _ => (x0, y0, x1, y1),
        }
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w * 0.5, self.y + self.h * 0.5)
    }

    /// Kotak pembatas di layar setelah rotasi.
    pub fn aabb(&self) -> (f32, f32, f32, f32) {
        let (x0, y0, x1, y1) = self.bounds();
        if self.rotation == 0.0 {
            return (x0, y0, x1, y1);
        }
        let (cx, cy) = self.center();
        let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)].map(|(x, y)| rotate(x, y, cx, cy, self.rotation));
        corners.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)))
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        for c in &mut self.children {
            c.translate(dx, dy);
        }
    }

    /// Regangkan node mengikuti kotak seleksi: kotak asal berpojok `from` dipetakan ke pojok
    /// `to` dengan skala (sx, sy). Stroke, radius, dan ukuran font tidak berubah (itu tugas Scale tool).
    pub fn stretch(&mut self, from: (f32, f32), to: (f32, f32), (sx, sy): (f32, f32)) {
        let map = |x: f32, y: f32| (to.0 + (x - from.0) * sx, to.1 + (y - from.1) * sy);
        if self.kind == Kind::Line && self.rotation == 0.0 {
            (self.x, self.y) = map(self.x, self.y);
            self.w *= sx;
            self.h *= sy;
        } else {
            let (cx, cy) = self.center();
            let (cx, cy) = map(cx, cy);
            // Node berputar: sumbu lokalnya ikut teregang sesuai arahnya.
            let (sin, cos) = self.rotation.to_radians().sin_cos();
            let (kw, kh) = ((sx * cos).hypot(sy * sin), (sx * sin).hypot(sy * cos));
            match self.kind {
                // Tinggi teks mengikuti isinya; teks lebar-otomatis hanya berpindah tempat.
                Kind::Text => {
                    if self.wrap {
                        self.w = (self.w * kw).max(1.0);
                    }
                    // A height set by hand is stretched like that of any other box.
                    if self.fixed_h {
                        self.h = (self.h * kh).max(1.0);
                    }
                }
                Kind::Line => {
                    self.w *= kw;
                    self.h *= kh;
                }
                _ => {
                    self.w = (self.w * kw).max(1.0);
                    self.h = (self.h * kh).max(1.0);
                }
            }
            self.x = cx - self.w * 0.5;
            self.y = cy - self.h * 0.5;
        }
        for c in &mut self.children {
            c.stretch(from, to, (sx, sy));
        }
    }

    // ---------------------------------------------------------------- path

    /// Path diratakan jadi polyline dalam koordinat dunia. Untuk path tertutup,
    /// titik pertama tidak diulang di akhir. Hanya kontur utama; lihat `contours`.
    pub fn path_points(&self, segs: usize) -> Vec<(f32, f32)> {
        self.flatten(&self.points, self.closed, segs)
    }

    /// Semua kontur (utama + lubang) sebagai polyline dunia.
    pub fn contours(&self, segs: usize) -> Vec<Vec<(f32, f32)>> {
        let mut out = vec![self.path_points(segs)];
        out.extend(self.holes.iter().map(|h| self.flatten(h, true, segs)));
        out
    }

    /// Titik kontur setelah sudut tajam dibulatkan sebesar `radius` (satuan dunia), seperti live
    /// corners: titik aslinya tidak berubah, pembulatan hanya diterapkan saat digambar / diekspor.
    /// Yang dibulatkan: titik tanpa pegangan di antara dua ruas lurus. Tiap sudut paling jauh
    /// memakan setengah ruas kalau ujung ruas satunya juga dibulatkan (seluruh ruas kalau tidak).
    /// Titik boleh punya jari-jari sendiri (`PathPoint::r`).
    /// Hapus jari-jari khusus per titik, sehingga semua sudut kembali mengikuti `radius`.
    pub fn clear_corner_radii(&mut self) {
        for q in self.points.iter_mut().chain(self.holes.iter_mut().flatten()) {
            q.r = None;
        }
    }

    pub fn rounded_points(&self, points: &[PathPoint], closed: bool) -> Vec<PathPoint> {
        let n = points.len();
        let (w, h) = (self.w, self.h);
        let radius = |i: usize| points[i].r.unwrap_or(self.radius).max(0.0);
        if self.kind != Kind::Path || n < 3 || w.abs() < 1e-3 || h.abs() < 1e-3 || (0..n).all(|i| radius(i) <= 0.0) {
            return points.to_vec();
        }
        let zero = [0.0f32; 2];
        let corner = |i: usize| {
            (closed || (i > 0 && i + 1 < n)) && points[i].hin == zero && points[i].hout == zero && points[(i + n - 1) % n].hout == zero && points[(i + 1) % n].hin == zero
        };
        let mut out = Vec::with_capacity(n * 2);
        for i in 0..n {
            let p = points[i];
            if !corner(i) || radius(i) <= 0.0 {
                out.push(p);
                continue;
            }
            let (ia, ib) = ((i + n - 1) % n, (i + 1) % n);
            let (a, b) = (points[ia], points[ib]);
            let (v1, v2) = (((a.x - p.x) * w, (a.y - p.y) * h), ((b.x - p.x) * w, (b.y - p.y) * h));
            let (l1, l2) = (v1.0.hypot(v1.1), v2.0.hypot(v2.1));
            if l1 < 1e-4 || l2 < 1e-4 {
                out.push(p);
                continue;
            }
            let (d1, d2) = ((v1.0 / l1, v1.1 / l1), (v2.0 / l2, v2.1 / l2));
            let cos = (d1.0 * d2.0 + d1.1 * d2.1).clamp(-1.0, 1.0);
            // Hampir lurus atau berbalik arah: tidak ada sudut yang bisa dibulatkan.
            if 1.0 + cos < 1e-4 || 1.0 - cos < 1e-4 {
                out.push(p);
                continue;
            }
            let tan_half = ((1.0 - cos) / (1.0 + cos)).sqrt();
            let limit = |len: f32, j: usize| if corner(j) && radius(j) > 0.0 { len * 0.5 } else { len };
            // Jarak titik singgung dari sudut, lalu jari-jari yang benar-benar muat.
            let t = (radius(i) / tan_half).min(limit(l1, ia)).min(limit(l2, ib));
            let r = t * tan_half;
            // Busur didekati satu bezier kubik.
            let k = 4.0 / 3.0 * ((std::f32::consts::PI - cos.acos()) / 4.0).tan() * r;
            out.push(PathPoint { x: p.x + d1.0 * t / w, y: p.y + d1.1 * t / h, hin: zero, hout: [-d1.0 * k / w, -d1.1 * k / h], r: None });
            out.push(PathPoint { x: p.x + d2.0 * t / w, y: p.y + d2.1 * t / h, hin: [-d2.0 * k / w, -d2.1 * k / h], hout: zero, r: None });
        }
        out
    }

    /// All contours as world polylines that stay within `tol` of the curves: every curve gets
    /// as many pieces as its own size asks for (at most `max`), so a path of many small curves,
    /// such as outlined text, does not get the point count of one large curve for each of them.
    pub fn contours_within(&self, tol: f32, max: usize) -> Vec<Vec<(f32, f32)>> {
        let steps = |p: [(f32, f32); 4]| {
            let dev = |a: (f32, f32), b: (f32, f32), c: (f32, f32)| ((a.0 - 2.0 * b.0 + c.0) * self.w).hypot((a.1 - 2.0 * b.1 + c.1) * self.h);
            ((0.75 * dev(p[0], p[1], p[2]).max(dev(p[1], p[2], p[3])) / tol.max(1e-6)).sqrt().ceil() as usize).clamp(2, max.max(2))
        };
        let mut out = vec![self.flatten_by(&self.points, self.closed, &steps)];
        out.extend(self.holes.iter().map(|h| self.flatten_by(h, true, &steps)));
        out
    }

    fn flatten(&self, points: &[PathPoint], closed: bool, segs: usize) -> Vec<(f32, f32)> {
        self.flatten_by(points, closed, &|_| segs.max(2))
    }

    /// `curved`: into how many pieces a curve is cut, from its four control points (in the
    /// space of the box, 0..1).
    fn flatten_by(&self, points: &[PathPoint], closed: bool, curved: &dyn Fn([(f32, f32); 4]) -> usize) -> Vec<(f32, f32)> {
        let points = &self.rounded_points(points, closed)[..];
        let n = points.len();
        let mut out = Vec::new();
        if n == 0 {
            return out;
        }
        let at = |x: f32, y: f32| (self.x + x * self.w, self.y + y * self.h);
        out.push(at(points[0].x, points[0].y));
        let count = if closed { n } else { n - 1 };
        for i in 0..count {
            let (a, b) = (points[i], points[(i + 1) % n]);
            let last = closed && i == n - 1;
            let straight = a.hout == [0.0, 0.0] && b.hin == [0.0, 0.0];
            let (c1, c2) = ((a.x + a.hout[0], a.y + a.hout[1]), (b.x + b.hin[0], b.y + b.hin[1]));
            let steps = if straight { 1 } else { curved([(a.x, a.y), c1, c2, (b.x, b.y)]) };
            for s in 1..=steps {
                if last && s == steps {
                    break;
                }
                let t = s as f32 / steps as f32;
                let u = 1.0 - t;
                let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                out.push(at(
                    k0 * a.x + k1 * c1.0 + k2 * c2.0 + k3 * b.x,
                    k0 * a.y + k1 * c1.1 + k2 * c2.1 + k3 * b.y,
                ));
            }
        }
        out
    }

    pub fn contains(&self, px: f32, py: f32, tol: f32) -> bool {
        // Node yang dirotasi: uji di ruang lokalnya (titik diputar balik).
        let (px, py) = if self.rotation != 0.0 {
            let (cx, cy) = self.center();
            rotate(px, py, cx, cy, -self.rotation)
        } else {
            (px, py)
        };
        let (x0, y0, x1, y1) = self.bounds();
        match self.kind {
            Kind::Line => {
                dist_to_segment(px, py, (self.x, self.y), (self.x + self.w, self.y + self.h)) <= tol + self.stroke_w * 0.5
            }
            Kind::Ellipse => {
                let (rx, ry) = ((x1 - x0) * 0.5, (y1 - y0) * 0.5);
                if rx <= 0.0 || ry <= 0.0 {
                    return false;
                }
                let (cx, cy) = (x0 + rx, y0 + ry);
                ((px - cx) / rx).powi(2) + ((py - cy) / ry).powi(2) <= 1.0
            }
            Kind::Path => {
                let pad = tol + self.stroke_w;
                if px < x0 - pad || px > x1 + pad || py < y0 - pad || py > y1 + pad {
                    return false;
                }
                let contours = self.contours(10);
                if contours[0].len() < 2 {
                    return false;
                }
                if self.closed && self.fill.is_some() {
                    // Uji titik-dalam-poligon (even-odd) atas semua kontur.
                    let mut inside = false;
                    for pts in &contours {
                        let m = pts.len();
                        if m == 0 {
                            continue;
                        }
                        let mut j = m - 1;
                        for i in 0..m {
                            let (a, b) = (pts[i], pts[j]);
                            if (a.1 > py) != (b.1 > py) && px < (b.0 - a.0) * (py - a.1) / (b.1 - a.1) + a.0 {
                                inside = !inside;
                            }
                            j = i;
                        }
                    }
                    if inside {
                        return true;
                    }
                }
                let limit = tol + self.stroke_w * 0.5;
                contours.iter().enumerate().any(|(c, pts)| {
                    let m = pts.len();
                    if m < 2 {
                        return false;
                    }
                    let segs = if self.closed || c > 0 { m } else { m - 1 };
                    (0..segs).any(|i| dist_to_segment(px, py, pts[i], pts[(i + 1) % m]) <= limit)
                })
            }
            _ => px >= x0 && px <= x1 && py >= y0 && py <= y1,
        }
    }

    pub fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }

    // ---------------------------------------------------------------- tree

    /// Visit this node and everything below it, parents before children, in stacking order.
    pub fn walk<'a>(&'a self, f: &mut impl FnMut(&'a Node)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }

    /// Like `walk`, with each node open for changes.
    pub fn walk_mut(&mut self, f: &mut impl FnMut(&mut Node)) {
        f(self);
        for c in &mut self.children {
            c.walk_mut(f);
        }
    }

    /// Does this node, or anything below it, satisfy `pred`?
    pub fn any(&self, pred: &impl Fn(&Node) -> bool) -> bool {
        pred(self) || self.children.iter().any(|c| c.any(pred))
    }

    /// This node or one below it, by id.
    pub fn find(&self, id: u64) -> Option<&Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|c| c.find(id))
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut Node> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter_mut().find_map(|c| c.find_mut(id))
    }

    /// Is `id` somewhere below this node (not the node itself)?
    pub fn has_descendant(&self, id: u64) -> bool {
        self.children.iter().any(|c| c.id == id || c.has_descendant(id))
    }

    /// Is one of `ids` somewhere below this node?
    pub fn has_any_descendant(&self, ids: &[u64]) -> bool {
        self.children.iter().any(|c| ids.contains(&c.id) || c.has_any_descendant(ids))
    }

    /// Child indices leading from this node down to `id`, appended to `path`.
    fn path_to(&self, id: u64, path: &mut Vec<usize>) -> bool {
        if self.id == id {
            return true;
        }
        for (i, c) in self.children.iter().enumerate() {
            path.push(i);
            if c.path_to(id, path) {
                return true;
            }
            path.pop();
        }
        false
    }

    /// Does some child list at or below this node satisfy `pred`?
    fn any_list(&self, pred: &impl Fn(&[Node]) -> bool) -> bool {
        !self.children.is_empty() && (pred(&self.children) || self.children.iter().any(|c| c.any_list(pred)))
    }

    /// Run `f` on every non-empty child list at or below this node.
    fn each_list_mut(&mut self, f: &mut impl FnMut(&mut Vec<Node>)) {
        if !self.children.is_empty() {
            f(&mut self.children);
            for c in &mut self.children {
                c.each_list_mut(f);
            }
        }
    }

    // ---------------------------------------------------------------- gaya teks

    pub fn base_style(&self) -> Style {
        Style {
            font: self.font.clone(),
            weight: self.weight,
            italic: self.italic,
            size: self.font_size,
            fill: self.fill.unwrap_or([0, 0, 0, 255]),
            underline: self.underline,
            strike: self.strike,
        }
    }

    pub fn apply_style(&mut self, s: &Style) {
        self.font = s.font.clone();
        self.weight = s.weight;
        self.italic = s.italic;
        self.font_size = s.size;
        self.fill = Some(s.fill);
        self.underline = s.underline;
        self.strike = s.strike;
    }

    /// Underline (`strike`: strikethrough) on or off for the whole text: the node's own style
    /// and every styled range.
    pub fn set_decoration(&mut self, strike: bool, on: bool) {
        if strike {
            self.strike = on;
        } else {
            self.underline = on;
        }
        for s in &mut self.spans {
            if strike {
                s.style.strike = on;
            } else {
                s.style.underline = on;
            }
        }
        let base = self.base_style();
        self.spans.retain(|s| s.style != base);
    }

    /// Is every character of [a, b) underlined (`strike`: struck through)? An empty range asks
    /// about the whole text.
    pub fn decorated(&self, a: usize, b: usize, strike: bool) -> bool {
        let len = self.text.chars().count();
        let (a, b) = if a >= b { (0, len) } else { (a.min(len), b.min(len)) };
        let has = |s: &Style| if strike { s.strike } else { s.underline };
        if a >= b {
            return has(&self.base_style());
        }
        self.segments(len).iter().filter(|(s, e, _)| *s < b && *e > a).all(|(_, _, st)| has(st))
    }

    /// Toggle underline (`strike`: strikethrough) for the characters [a, b), or for the whole
    /// text when the range is empty. It goes on unless all of it had it already.
    pub fn toggle_decoration(&mut self, a: usize, b: usize, strike: bool) {
        let on = !self.decorated(a, b, strike);
        if a >= b {
            self.set_decoration(strike, on);
        } else {
            self.style_range(a, b, |s| if strike { s.strike = on } else { s.underline = on });
        }
    }

    /// The list marker of every paragraph of `text` (split on `\n`): a bullet, or its number.
    /// An empty paragraph has none and does not take a number.
    pub fn list_labels(&self, text: &str) -> Vec<Option<String>> {
        let mut number = 0;
        text.split('\n')
            .map(|p| match self.list {
                0 => None,
                _ if p.is_empty() => None,
                1 => Some("\u{2022}".to_owned()),
                _ => {
                    number += 1;
                    Some(format!("{number}."))
                }
            })
            .collect()
    }

    /// How far the lines of a list stand in from the left of the box (document units): the room
    /// for the marker, wider when the numbers have more digits. 0 without a list.
    pub fn list_indent(&self, text: &str) -> f32 {
        match self.list {
            0 => 0.0,
            1 => self.font_size * 1.5,
            _ => {
                let count = text.split('\n').filter(|p| !p.is_empty()).count().max(1);
                let digits = count.to_string().len() as f32;
                self.font_size * (0.9 + 0.6 * digits)
            }
        }
    }

    /// Justified: a paragraph whose lines are stretched to the width of the box.
    pub fn justified(&self) -> bool {
        self.wrap && self.align == 3
    }

    /// How far down the lines start in a box of fixed height, for lines `content` high in all.
    /// Negative when they are higher than the box and not aligned to its top: they run out of it.
    pub fn text_dy(&self, content: f32) -> f32 {
        if !self.fixed_h {
            return 0.0;
        }
        match self.valign {
            1 => (self.h - content) * 0.5,
            2 => self.h - content,
            _ => 0.0,
        }
    }

    /// How far the lines of a text in a box of fixed height run out of that box, above or
    /// below it (whichever is more), for lines `content` high in all. 0: they fit.
    pub fn text_spill(&self, content: f32) -> f32 {
        if !self.fixed_h {
            return 0.0;
        }
        let dy = self.text_dy(content);
        (-dy).max(dy + content - self.h).max(0.0)
    }

    pub fn style_at(&self, i: usize) -> Style {
        self.spans
            .iter()
            .find(|s| s.start <= i && i < s.end)
            .map(|s| s.style.clone())
            .unwrap_or_else(|| self.base_style())
    }

    /// Ukuran font terbesar yang dipakai (gaya node atau salah satu rentang).
    pub fn max_size(&self) -> f32 {
        self.spans.iter().map(|s| s.style.size).fold(self.font_size, f32::max).max(1.0)
    }

    /// Potongan berurutan tanpa tumpang-tindih yang menutup [0, len): (awal, akhir, gaya).
    pub fn segments(&self, len: usize) -> Vec<(usize, usize, Style)> {
        let base = self.base_style();
        let mut spans: Vec<&Span> = self.spans.iter().collect();
        spans.sort_by_key(|s| s.start);
        let mut out = Vec::with_capacity(spans.len() * 2 + 1);
        let mut cursor = 0;
        for s in spans {
            let (start, end) = (s.start.max(cursor).min(len), s.end.min(len));
            if start >= end {
                continue;
            }
            if start > cursor {
                out.push((cursor, start, base.clone()));
            }
            out.push((start, end, s.style.clone()));
            cursor = end;
        }
        if cursor < len {
            out.push((cursor, len, base));
        }
        out
    }

    /// Ubah gaya karakter [a, b) saja; sisanya tidak tersentuh.
    pub fn style_range(&mut self, a: usize, b: usize, f: impl Fn(&mut Style)) {
        let len = self.text.chars().count();
        let (a, b) = (a.min(len), b.min(len));
        if a >= b {
            return;
        }
        let base = self.base_style();
        let mut spans: Vec<Span> = Vec::new();
        let mut push = |start: usize, end: usize, style: Style| {
            if start >= end || style == base {
                return;
            }
            // Gabung dengan rentang sebelumnya kalau gayanya sama dan bersambung.
            if let Some(last) = spans.last_mut() {
                if last.end == start && last.style == style {
                    last.end = end;
                    return;
                }
            }
            spans.push(Span { start, end, style });
        };
        for (s, e, style) in self.segments(len) {
            push(s, e.min(a), style.clone());
            let (ms, me) = (s.max(a), e.min(b));
            if ms < me {
                let mut changed = style.clone();
                f(&mut changed);
                push(ms, me, changed);
            }
            push(s.max(b), e, style);
        }
        self.spans = spans;
    }

    /// Geser rentang gaya mengikuti perubahan teks (ketik/hapus di tengah).
    pub fn sync_spans(&mut self, old: &str, new: &str) {
        if self.spans.is_empty() {
            return;
        }
        let (o, n): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
        let prefix = o.iter().zip(&n).take_while(|(a, b)| a == b).count();
        let max_suffix = o.len().min(n.len()) - prefix;
        let suffix = o.iter().rev().zip(n.iter().rev()).take(max_suffix).take_while(|(a, b)| a == b).count();
        let old_end = o.len() - suffix;
        let inserted = n.len() - suffix - prefix;
        let shift = |i: usize| i + n.len() - o.len();
        for s in &mut self.spans {
            if s.start >= old_end {
                s.start = shift(s.start);
            } else if s.start > prefix {
                s.start = prefix + inserted;
            }
            if s.end >= old_end {
                s.end = shift(s.end);
            } else if s.end > prefix {
                s.end = prefix;
            }
        }
        self.spans.retain(|s| s.start < s.end);
    }

    /// Replace the characters [start, end) (character indices) of the text with `with`, and
    /// move the styled ranges along. The new characters take the style of the first character
    /// they replace; a range that lay wholly inside what was replaced is gone. Unlike
    /// `sync_spans`, which has to guess one edit from two texts, this knows exactly what
    /// changed, so several replacements in one text can be applied one after the other (from
    /// the last to the first, so the indices of the earlier ones stay valid).
    pub fn replace_range(&mut self, start: usize, end: usize, with: &str) {
        let len = self.text.chars().count();
        let (a, b) = (start.min(len), end.min(len));
        if a > b {
            return;
        }
        let byte = |i: usize| self.text.char_indices().nth(i).map_or(self.text.len(), |(at, _)| at);
        let (from, to) = (byte(a), byte(b));
        self.text.replace_range(from..to, with);
        let added = with.chars().count();
        // Where a boundary of a styled range ends up.
        let place = |i: usize| {
            if i <= a {
                i
            } else if i < b {
                a + added
            } else {
                i - (b - a) + added
            }
        };
        for s in &mut self.spans {
            (s.start, s.end) = (place(s.start), place(s.end));
        }
        self.spans.retain(|s| s.start < s.end);
    }
}

/// Color style: satu warna bernama. Objek yang memakainya menyimpan id-nya di `fill_style` /
/// `stroke_style`; warnanya disalin ke objek setiap kali style berubah.
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct ColorStyle {
    pub id: u64,
    pub name: String,
    pub color: [u8; 4],
}

/// Text style: font, ketebalan, ukuran, dan jarak. Warna teks tidak termasuk (itu color style).
#[derive(Clone, PartialEq, Debug, Serialize, Deserialize)]
pub struct TypeStyle {
    pub id: u64,
    pub name: String,
    pub font: String,
    pub weight: u16,
    pub italic: bool,
    pub size: f32,
    pub line_height: f32,
    pub letter_spacing: f32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub strike: bool,
}

impl TypeStyle {
    /// Gaya teks node ini (tanpa id dan nama).
    pub fn of(n: &Node) -> Self {
        TypeStyle {
            id: 0,
            name: String::new(),
            font: n.font.clone(),
            weight: n.weight,
            italic: n.italic,
            size: n.font_size,
            line_height: n.line_height,
            letter_spacing: n.letter_spacing,
            underline: n.underline,
            strike: n.strike,
        }
    }

    pub fn matches(&self, n: &Node) -> bool {
        self.font == n.font
            && self.weight == n.weight
            && self.italic == n.italic
            && self.size == n.font_size
            && self.line_height == n.line_height
            && self.letter_spacing == n.letter_spacing
            && self.underline == n.underline
            && self.strike == n.strike
    }

    pub fn apply(&self, n: &mut Node) {
        n.font = self.font.clone();
        n.weight = self.weight;
        n.italic = self.italic;
        n.font_size = self.size;
        n.line_height = self.line_height;
        n.letter_spacing = self.letter_spacing;
        n.underline = self.underline;
        n.strike = self.strike;
    }

    /// Ringkasan satu baris, mis. "Montserrat 24 · Bold".
    pub fn summary(&self) -> String {
        format!("{} {} · {}{}", self.font, self.size, crate::fonts::weight_name(self.weight), if self.italic { " Italic" } else { "" })
    }
}

/// Garis bantu (guide) di canvas: garis vertikal di x = `at`, atau horizontal di y = `at`.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct RulerGuide {
    pub vertical: bool,
    pub at: f32,
}

/// Satu halaman dokumen: punya layer dan guide sendiri; style, gambar dan nama group dipakai bersama.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Page {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub nodes: Vec<Arc<Node>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<RulerGuide>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Doc {
    /// Layer halaman aktif.
    pub nodes: Vec<Arc<Node>>,
    /// Halaman dokumen. Isi halaman aktif (`pages[page]`) ada di `nodes` / `guides` di atas, dan
    /// slotnya di sini kosong, jadi kode lain cukup bekerja dengan `nodes`. Daftar kosong =
    /// dokumen satu halaman (file lama); versi lama yang membuka file baru melihat halaman aktif.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<Page>,
    /// Indeks halaman aktif di `pages`.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub page: usize,
    #[serde(default)]
    pub next_id: u64,
    #[serde(default)]
    pub assets: BTreeMap<u64, Arc<Asset>>,
    /// Nama group per id group (ditampilkan di panel layer).
    #[serde(default)]
    pub group_names: BTreeMap<u64, String>,
    /// Garis bantu yang ditarik dari penggaris.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guides: Vec<RulerGuide>,
    /// Color styles: warna bernama yang dipakai bersama.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colors: Vec<ColorStyle>,
    /// Text styles: gaya teks bernama yang dipakai bersama.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_styles: Vec<TypeStyle>,
}

impl Doc {
    pub fn color_style(&self, id: u64) -> Option<&ColorStyle> {
        self.colors.iter().find(|s| s.id == id)
    }

    pub fn type_style(&self, id: u64) -> Option<&TypeStyle> {
        self.type_styles.iter().find(|s| s.id == id)
    }

    pub fn add_color_style(&mut self, color: [u8; 4]) -> u64 {
        let id = self.alloc_id();
        let name = format!("Color {}", self.colors.len() + 1);
        self.colors.push(ColorStyle { id, name, color });
        id
    }

    pub fn add_type_style(&mut self, from: &Node) -> u64 {
        let id = self.alloc_id();
        let name = format!("{} {}", from.font, from.font_size);
        self.type_styles.push(TypeStyle { id, name, ..TypeStyle::of(from) });
        id
    }

    /// Ubah node yang memenuhi `uses` (di kedalaman mana pun, semua halaman). Node level atas
    /// yang tidak tersentuh tidak disalin, jadi snapshot undo tetap berbagi memori.
    pub fn update_nodes(&mut self, uses: impl Fn(&Node) -> bool, mut apply: impl FnMut(&mut Node)) {
        for top in self.nodes.iter_mut().chain(self.pages.iter_mut().flat_map(|p| p.nodes.iter_mut())) {
            if top.any(&uses) {
                Arc::make_mut(top).walk_mut(&mut |n| {
                    if uses(n) {
                        apply(n);
                    }
                });
            }
        }
    }

    /// Visit every node of the current page, parents before children, in stacking order.
    pub fn walk<'a>(&'a self, mut f: impl FnMut(&'a Node)) {
        for n in &self.nodes {
            n.walk(&mut f);
        }
    }

    /// Visit every node of every page (the current page first).
    pub fn walk_all<'a>(&'a self, mut f: impl FnMut(&'a Node)) {
        for n in self.all_tops() {
            n.walk(&mut f);
        }
    }

    /// Change child lists below the top level of the current page: `edit` runs on each list
    /// (of a frame at any depth) that `touches` accepts. Top-level nodes without one stay shared.
    fn edit_lists(&mut self, touches: impl Fn(&[Node]) -> bool, mut edit: impl FnMut(&mut Vec<Node>)) {
        for top in &mut self.nodes {
            if top.any_list(&touches) {
                Arc::make_mut(top).each_list_mut(&mut |list| {
                    if touches(list) {
                        edit(list);
                    }
                });
            }
        }
    }

    /// Salin warna style ke semua objek yang memakainya.
    pub fn sync_color_style(&mut self, id: u64) {
        let Some(color) = self.color_style(id).map(|s| s.color) else { return };
        self.update_nodes(
            |n| n.fill_style == Some(id) || n.stroke_style == Some(id),
            |n| {
                if n.fill_style == Some(id) {
                    n.fill = Some(color);
                    n.gradient = None;
                }
                if n.stroke_style == Some(id) {
                    n.stroke = Some(color);
                    if n.stroke_w <= 0.0 {
                        n.stroke_w = 1.0;
                    }
                }
            },
        );
    }

    /// Salin gaya teks style ke semua teks yang memakainya.
    pub fn sync_type_style(&mut self, id: u64) {
        let Some(style) = self.type_style(id).cloned() else { return };
        self.update_nodes(|n| n.type_style == Some(id), |n| style.apply(n));
    }

    pub fn color_style_uses(&self, id: u64) -> usize {
        let mut count = 0;
        self.walk_all(|n| count += (n.fill_style == Some(id)) as usize + (n.stroke_style == Some(id)) as usize);
        count
    }

    /// Setelah style dihapus: lepaskan objek yang masih menunjuk ke style itu (tampilannya tetap).
    pub fn drop_missing_styles(&mut self) {
        let colors: Vec<u64> = self.colors.iter().map(|s| s.id).collect();
        let types: Vec<u64> = self.type_styles.iter().map(|s| s.id).collect();
        let gone = |list: &[u64], id: Option<u64>| id.is_some_and(|id| !list.contains(&id));
        let (c2, t2) = (colors.clone(), types.clone());
        self.update_nodes(
            move |n| gone(&colors, n.fill_style) || gone(&colors, n.stroke_style) || gone(&types, n.type_style),
            move |n| {
                let keep = |list: &[u64], id: Option<u64>| id.filter(|id| list.contains(id));
                n.fill_style = keep(&c2, n.fill_style);
                n.stroke_style = keep(&c2, n.stroke_style);
                n.type_style = keep(&t2, n.type_style);
            },
        );
    }

    // ---------------------------------------------------------------- pages

    /// Node level atas di semua halaman (halaman aktif dulu).
    pub fn all_tops(&self) -> impl Iterator<Item = &Node> {
        self.nodes.iter().chain(self.pages.iter().flat_map(|p| p.nodes.iter())).map(|n| &**n)
    }

    /// Id node terbesar di semua halaman (untuk memastikan id baru tidak bentrok).
    pub fn max_node_id(&self) -> u64 {
        let mut max = 0;
        self.walk_all(|n| max = max.max(n.id));
        max
    }

    pub fn page_count(&self) -> usize {
        self.pages.len().max(1)
    }

    /// Id halaman aktif (0 untuk dokumen satu halaman yang belum punya daftar halaman).
    pub fn page_id(&self) -> u64 {
        self.pages.get(self.page).map_or(0, |p| p.id)
    }

    pub fn page_name(&self, i: usize) -> &str {
        self.pages.get(i).map_or("Page 1", |p| p.name.as_str())
    }

    /// Dokumen lama / baru belum punya daftar halaman: buat "Page 1" untuk isi yang ada.
    pub fn ensure_pages(&mut self) {
        if self.pages.is_empty() {
            let id = self.alloc_id();
            self.pages.push(Page { id, name: "Page 1".into(), ..Default::default() });
        }
        self.page = self.page.min(self.pages.len() - 1);
    }

    /// Setelah file dibuka: rapikan indeks halaman aktif.
    pub fn fix_pages(&mut self) {
        if !self.pages.is_empty() {
            self.page = self.page.min(self.pages.len() - 1);
        }
    }

    fn next_page_name(&self) -> String {
        let last = self.pages.iter().filter_map(|p| p.name.strip_prefix("Page ")?.parse::<u32>().ok()).max().unwrap_or(0);
        format!("Page {}", (last + 1).max(self.pages.len() as u32 + 1))
    }

    /// Pindah ke halaman `i`. Hasil: apakah halaman aktif berganti.
    pub fn switch_page(&mut self, i: usize) -> bool {
        self.ensure_pages();
        if i == self.page || i >= self.pages.len() {
            return false;
        }
        let cur = &mut self.pages[self.page];
        cur.nodes = std::mem::take(&mut self.nodes);
        cur.guides = std::mem::take(&mut self.guides);
        self.page = i;
        let new = &mut self.pages[i];
        self.nodes = std::mem::take(&mut new.nodes);
        self.guides = std::mem::take(&mut new.guides);
        true
    }

    /// Tambah halaman kosong di akhir dan pindah ke sana. Hasil: indeksnya.
    pub fn add_page(&mut self) -> usize {
        self.ensure_pages();
        let (id, name) = (self.alloc_id(), self.next_page_name());
        self.pages.push(Page { id, name, ..Default::default() });
        let i = self.pages.len() - 1;
        self.switch_page(i);
        i
    }

    /// Salin halaman `i` (dengan id node dan group baru) tepat setelahnya, lalu pindah ke salinan.
    pub fn duplicate_page(&mut self, i: usize) -> Option<usize> {
        self.ensure_pages();
        if i >= self.pages.len() {
            return None;
        }
        let (src, guides) = if i == self.page { (&self.nodes, &self.guides) } else { (&self.pages[i].nodes, &self.pages[i].guides) };
        let (mut nodes, guides): (Vec<Node>, Vec<RulerGuide>) = (src.iter().map(|n| (**n).clone()).collect(), guides.clone());
        self.regroup(&mut nodes);
        let mut map = HashMap::new();
        for n in &mut nodes {
            self.reassign_ids_into(n, &mut map);
        }
        nodes.iter_mut().for_each(|n| crate::textpath::relink(n, &map));
        let page = Page { id: self.alloc_id(), name: format!("{} copy", self.pages[i].name), nodes: nodes.into_iter().map(Arc::new).collect(), guides };
        self.pages.insert(i + 1, page);
        if self.page > i {
            self.page += 1;
        }
        self.switch_page(i + 1);
        Some(i + 1)
    }

    /// Hapus halaman `i` beserta isinya; halaman terakhir tidak bisa dihapus.
    pub fn delete_page(&mut self, i: usize) -> bool {
        if i >= self.pages.len() || self.pages.len() < 2 {
            return false;
        }
        if i == self.page {
            self.switch_page(if i == 0 { 1 } else { i - 1 });
        }
        self.pages.remove(i);
        if self.page > i {
            self.page -= 1;
        }
        let mut used = std::collections::BTreeSet::new();
        self.walk_all(|n| {
            used.insert(n.group);
        });
        self.group_names.retain(|g, _| used.contains(g));
        true
    }

    pub fn rename_page(&mut self, i: usize, name: &str) -> bool {
        self.ensure_pages();
        match self.pages.get_mut(i) {
            Some(p) if !name.trim().is_empty() && p.name != name.trim() => {
                p.name = name.trim().to_owned();
                true
            }
            _ => false,
        }
    }

    /// Pindahkan halaman `from` ke posisi `to` dalam daftar; halaman aktif tetap sama.
    pub fn move_page(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.pages.len() || to >= self.pages.len() {
            return false;
        }
        let active = self.page_id();
        let p = self.pages.remove(from);
        self.pages.insert(to, p);
        self.page = self.pages.iter().position(|p| p.id == active).unwrap_or(0);
        true
    }

    pub fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Simpan byte gambar; gambar yang sama hanya disimpan sekali.
    pub fn add_asset(&mut self, bytes: Vec<u8>) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        let key = h.finish();
        self.assets.entry(key).or_insert_with(|| Arc::new(Asset { bytes }));
        key
    }

    /// Where a node is: the index among the top-level nodes, then the child index at each level
    /// below (so a top-level node has a path of length 1).
    pub fn locate(&self, id: u64) -> Option<Vec<usize>> {
        let mut path = Vec::new();
        for (ti, n) in self.nodes.iter().enumerate() {
            path.push(ti);
            if n.path_to(id, &mut path) {
                return Some(path);
            }
            path.pop();
        }
        None
    }

    /// The node at a path from `locate`.
    pub fn at(&self, path: &[usize]) -> Option<&Node> {
        let (first, rest) = path.split_first()?;
        let mut n: &Node = self.nodes.get(*first)?;
        for i in rest {
            n = n.children.get(*i)?;
        }
        Some(n)
    }

    /// Urutkan id menurut urutan tumpukan dokumen, dari paling bawah ke paling atas
    /// (induk sebelum isinya).
    pub fn sort_by_stack(&self, ids: &mut [u64]) {
        ids.sort_by_cached_key(|id| self.locate(*id).unwrap_or_else(|| vec![usize::MAX]));
    }

    pub fn find(&self, id: u64) -> Option<&Node> {
        self.nodes.iter().find_map(|n| n.find(id))
    }

    /// Only the top-level node that holds `id` is copied (when an undo snapshot shares it).
    pub fn find_mut(&mut self, id: u64) -> Option<&mut Node> {
        let ti = self.nodes.iter().position(|n| n.find(id).is_some())?;
        Arc::make_mut(&mut self.nodes[ti]).find_mut(id)
    }

    /// The node and the frames around it, outermost first (the node itself is last).
    pub fn lineage(&self, id: u64) -> Vec<&Node> {
        let Some(path) = self.locate(id) else { return Vec::new() };
        let mut out = Vec::with_capacity(path.len());
        let mut n: &Node = &self.nodes[path[0]];
        out.push(n);
        for i in &path[1..] {
            n = &n.children[*i];
            out.push(n);
        }
        out
    }

    /// Ids of the frames around a node, nearest first (empty for a top-level node).
    pub fn ancestors(&self, id: u64) -> Vec<u64> {
        let mut out: Vec<u64> = self.lineage(id).iter().map(|n| n.id).collect();
        out.pop();
        out.reverse();
        out
    }

    pub fn parent_of(&self, id: u64) -> Option<u64> {
        let path = self.locate(id)?;
        self.at(&path[..path.len() - 1]).map(|n| n.id)
    }

    /// The top-level node that holds `id` (the node itself when it is top-level).
    pub fn top_of(&self, id: u64) -> Option<u64> {
        self.nodes.iter().find(|n| n.find(id).is_some()).map(|n| n.id)
    }

    /// Is `id` somewhere inside `of` (at any depth)? A node is not its own descendant.
    pub fn is_descendant(&self, id: u64, of: u64) -> bool {
        self.find(of).is_some_and(|n| n.has_descendant(id))
    }

    /// Ids of the direct children of `parent`, or of the top level, in stacking order.
    pub fn child_ids(&self, parent: Option<u64>) -> Vec<u64> {
        match parent {
            Some(p) => self.find(p).map(|f| f.children.iter().map(|c| c.id).collect()).unwrap_or_default(),
            None => self.nodes.iter().map(|n| n.id).collect(),
        }
    }

    /// Drop the ids that sit inside another node of the list, so a frame and its content are
    /// handled once. The order of the rest is kept.
    pub fn roots_of(&self, ids: &[u64]) -> Vec<u64> {
        ids.iter().copied().filter(|id| !self.ancestors(*id).iter().any(|a| ids.contains(a))).collect()
    }

    pub fn remove(&mut self, id: u64) -> Option<Node> {
        let path = self.locate(id)?;
        let (last, up) = path.split_last()?;
        if up.is_empty() {
            return Some(Arc::unwrap_or_clone(self.nodes.remove(*last)));
        }
        let mut n = Arc::make_mut(&mut self.nodes[up[0]]);
        for i in &up[1..] {
            n = &mut n.children[*i];
        }
        Some(n.children.remove(*last))
    }

    /// Sisipkan di urutan paling atas dari `parent` (frame di kedalaman mana pun), atau di level
    /// atas bila `None` atau induknya bukan frame.
    pub fn insert(&mut self, node: Node, parent: Option<u64>) {
        if parent.is_some_and(|p| self.find(p).is_some_and(|f| f.kind == Kind::Frame)) {
            if let Some(f) = parent.and_then(|p| self.find_mut(p)) {
                f.children.push(node);
                return;
            }
        }
        self.nodes.push(Arc::new(node));
    }

    /// Bawa node-node ini ke paling depan / paling belakang di antara saudaranya (lihat `restack`).
    pub fn reorder(&mut self, ids: &[u64], to_front: bool) {
        self.restack(ids, to_front, false);
    }

    /// Naik/turun satu tingkat dalam urutan tumpukan.
    pub fn reorder_step(&mut self, ids: &[u64], up: bool) {
        self.restack(ids, up, true);
    }

    fn restack(&mut self, ids: &[u64], up: bool, step: bool) {
        // Frame yang ikut terpilih pindah beserta isinya; urutan isinya tidak diubah.
        fn inside(n: &mut Node, ids: &[u64], up: bool, step: bool) {
            if ids.contains(&n.id) || n.children.is_empty() {
                return;
            }
            restack(&mut n.children, ids, up, step);
            for c in &mut n.children {
                inside(c, ids, up, step);
            }
        }
        restack(&mut self.nodes, ids, up, step);
        for top in &mut self.nodes {
            if !ids.contains(&top.id) && top.has_any_descendant(ids) {
                inside(Arc::make_mut(top), ids, up, step);
            }
        }
    }

    /// Pindahkan node-node `ids` (tarik-lepas di panel layer) ke `parent` (frame di kedalaman mana
    /// pun, atau level atas bila `None`). `anchor` = (id saudara, di atasnya?): disisipkan tepat
    /// di atas/bawah saudara itu dalam urutan tumpukan; `None` = paling depan. Semua yang dipindah
    /// ikut `group` tujuan; kalau tujuannya bukan group, group yang terbawa utuh tetap jadi group.
    /// Urutan relatif yang dipindah tetap; node yang induknya ikut dipindah tetap di dalam induknya.
    /// Hasil `false`: tidak sah (tujuan bukan frame, atau frame ke dalam dirinya sendiri / isinya)
    /// atau tidak ada yang berubah.
    pub fn move_nodes(&mut self, ids: &[u64], parent: Option<u64>, anchor: Option<(u64, bool)>, group: u64) -> bool {
        fn rows(n: &Node, parent: u64, out: &mut Vec<(u64, u64, u64, bool)>) {
            out.push((n.id, parent, n.group, n.clip));
            for c in &n.children {
                rows(c, n.id, out);
            }
        }
        let layout = |d: &Doc| -> Vec<(u64, u64, u64, bool)> {
            let mut out = Vec::new();
            for n in &d.nodes {
                rows(n, 0, &mut out);
            }
            out
        };
        let ids = &self.roots_of(ids)[..];
        let before = layout(self);
        if let Some(p) = parent {
            let frame = self.find(p).is_some_and(|n| n.kind == Kind::Frame);
            if !frame || ids.contains(&p) || self.ancestors(p).iter().any(|a| ids.contains(a)) {
                return false;
            }
        }
        // Posisi sisip dihitung di daftar tujuan tanpa node yang dipindah.
        let siblings = self.child_ids(parent);
        let stay = |list: &[u64]| list.iter().filter(|id| !ids.contains(id)).count();
        let at = match anchor {
            Some((a, above)) => {
                let Some(i) = siblings.iter().position(|id| *id == a) else { return false };
                stay(&siblings[..i]) + (above && !ids.contains(&a)) as usize
            }
            None => stay(&siblings),
        };
        // Group yang semua anggotanya ikut pindah tetap utuh (kalau tujuannya bukan group lain).
        let mut whole: Vec<u64> = before.iter().filter(|r| r.2 != 0 && ids.contains(&r.0)).map(|r| r.2).collect();
        whole.dedup();
        whole.retain(|g| before.iter().filter(|r| r.2 == *g).all(|r| ids.contains(&r.0)));

        fn take(list: &mut Vec<Node>, ids: &[u64], moved: &mut Vec<Node>) {
            let mut i = 0;
            while i < list.len() {
                if ids.contains(&list[i].id) {
                    moved.push(list.remove(i));
                } else {
                    take(&mut list[i].children, ids, moved);
                    i += 1;
                }
            }
        }
        let mut moved: Vec<Node> = Vec::new();
        for top in std::mem::take(&mut self.nodes) {
            if ids.contains(&top.id) {
                moved.push(Arc::unwrap_or_clone(top));
                continue;
            }
            let mut top = top;
            if top.has_any_descendant(ids) {
                take(&mut Arc::make_mut(&mut top).children, ids, &mut moved);
            }
            self.nodes.push(top);
        }
        if moved.is_empty() {
            return false;
        }
        for n in &mut moved {
            n.group = if group != 0 || !whole.contains(&n.group) { group } else { n.group };
        }
        // Disisipkan di tengah rangkaian clipping mask: ikut di-clip. Di luar itu yang paling
        // bawah lepas dari clip (yang lain tetap, supaya alas + isinya pindah sebagai satu kesatuan).
        let above_clip = |next: Option<&Node>| at > 0 && next.is_some_and(|n| n.clip);
        let inside = match parent {
            Some(p) => above_clip(self.find(p).and_then(|f| f.children.get(at))),
            None => above_clip(self.nodes.get(at).map(|n| &**n)),
        };
        if inside {
            moved.iter_mut().filter(|n| n.kind != Kind::Frame).for_each(|n| n.clip = true);
        } else {
            moved[0].clip = false;
        }
        match parent.and_then(|p| self.find_mut(p)) {
            Some(f) => {
                f.children.splice(at..at, moved);
            }
            None => {
                self.nodes.splice(at..at, moved.into_iter().map(Arc::new));
            }
        }
        let after = layout(self);
        self.group_names.retain(|g, _| after.iter().any(|r| r.2 == *g));
        before != after
    }

    /// Beri id baru untuk node (dan anak-anaknya), dipakai saat tempel/duplikat.
    /// Give a copy (with everything in it) ids of its own. A text on a path inside it follows
    /// the copy of its path when that is inside too, and stays on its path otherwise.
    pub fn reassign_ids(&mut self, node: &mut Node) {
        let mut map = HashMap::new();
        self.reassign_ids_into(node, &mut map);
        crate::textpath::relink(node, &map);
    }

    /// The same for one of several copies that belong together: `map` collects old id to new
    /// id, for `textpath::relink` once all of them have their ids.
    pub fn reassign_ids_into(&mut self, node: &mut Node, map: &mut HashMap<u64, u64>) {
        let old = node.id;
        node.id = self.alloc_id();
        map.insert(old, node.id);
        for c in &mut node.children {
            self.reassign_ids_into(c, map);
        }
    }

    // ---------------------------------------------------------------- group

    /// Id semua node (di kedalaman mana pun) yang group-nya ada di `groups`.
    pub fn group_members(&self, groups: &[u64]) -> Vec<u64> {
        let mut out = Vec::new();
        if groups.is_empty() {
            return out;
        }
        self.walk(|n| {
            if n.group != 0 && groups.contains(&n.group) {
                out.push(n.id);
            }
        });
        out
    }

    /// Jadikan node-node ini satu group; anggota yang seinduk dirapatkan dalam urutan tumpukan.
    pub fn group(&mut self, ids: &[u64]) -> u64 {
        let g = self.alloc_id();
        let last = self.group_names.values().filter_map(|s| s.strip_prefix("Group ")?.parse::<u32>().ok()).max().unwrap_or(0);
        self.group_names.insert(g, format!("Group {}", last + 1));
        for id in ids {
            if let Some(n) = self.find_mut(*id) {
                n.group = g;
            }
        }
        gather(&mut self.nodes, |n| n.group == g);
        self.edit_lists(|list| list.iter().filter(|c| c.group == g).count() >= 2, |list| gather(list, |c| c.group == g));
        g
    }

    pub fn ungroup(&mut self, groups: &[u64]) {
        for id in self.group_members(groups) {
            if let Some(n) = self.find_mut(id) {
                n.group = 0;
            }
        }
        for g in groups {
            self.group_names.remove(g);
        }
    }

    /// Buat clipping mask dari node-node ini (lihat `clip_siblings`), per daftar saudara.
    /// Hasil: apakah ada yang berhasil di-clip.
    pub fn make_clip(&mut self, ids: &[u64]) -> bool {
        let mut any = clip_siblings(&mut self.nodes, ids, |t| Arc::make_mut(t));
        self.edit_lists(|list| list.iter().any(|c| ids.contains(&c.id)), |list| any |= clip_siblings(list, ids, |t| t));
        any
    }

    pub fn release_clip(&mut self, ids: &[u64]) {
        for id in ids {
            if let Some(n) = self.find_mut(*id) {
                n.clip = false;
            }
        }
    }

    pub fn group_name(&self, g: u64) -> &str {
        self.group_names.get(&g).map_or("Group", |s| s.as_str())
    }

    /// Salinan (tempel/duplikat) mendapat id group baru supaya tidak menyatu dengan aslinya.
    /// Group yang hanya tersalin satu anggotanya dilepas.
    pub fn regroup(&mut self, nodes: &mut [Node]) {
        fn each(nodes: &mut [Node], f: &mut impl FnMut(&mut Node)) {
            for n in nodes {
                f(n);
                each(&mut n.children, f);
            }
        }
        let mut count: BTreeMap<u64, usize> = BTreeMap::new();
        each(nodes, &mut |n: &mut Node| {
            if n.group != 0 {
                *count.entry(n.group).or_default() += 1;
            }
        });
        let mut map: BTreeMap<u64, u64> = BTreeMap::new();
        for (g, c) in count {
            let new = if c >= 2 { self.alloc_id() } else { 0 };
            if new != 0 {
                if let Some(name) = self.group_names.get(&g).cloned() {
                    self.group_names.insert(new, name);
                }
            }
            map.insert(g, new);
        }
        each(nodes, &mut |n: &mut Node| {
            if n.group != 0 {
                n.group = map[&n.group];
            }
        });
    }

    /// The deepest visible frame under this point; frames in `exclude` (and so everything in
    /// them) are passed over. Content outside its frame is clipped away, so a nested frame only
    /// counts where its parents show it. Instances are passed over as well: what they hold is
    /// decided by their main component, so nothing gets dropped into them.
    pub fn frame_at(&self, x: f32, y: f32, exclude: &[u64]) -> Option<u64> {
        fn deepest<'a, T: AsRef<Node>>(list: &'a [T], x: f32, y: f32, exclude: &[u64]) -> Option<&'a Node> {
            list.iter()
                .rev()
                .map(|n| n.as_ref())
                .find(|n| n.kind == Kind::Frame && n.visible && n.instance_of.is_none() && !exclude.contains(&n.id) && n.contains(x, y, 0.0))
                .map(|n| deepest(&n.children, x, y, exclude).unwrap_or(n))
        }
        deepest(&self.nodes, x, y, exclude).map(|n| n.id)
    }

    /// The frame `n` belongs in when it sits at this point: the deepest frame there, and for a
    /// frame the nearest one that is larger than it in both directions (so artboards dragged
    /// past each other do not nest).
    pub fn frame_for(&self, n: &Node, x: f32, y: f32, exclude: &[u64]) -> Option<u64> {
        let found = self.frame_at(x, y, exclude)?;
        if n.kind != Kind::Frame {
            return Some(found);
        }
        self.lineage(found).into_iter().rev().find(|f| n.w.abs() < f.w.abs() && n.h.abs() < f.h.abs()).map(|f| f.id)
    }

    /// Node paling depan di titik ini menurut aturan klik biasa: objek level atas, atau anak
    /// langsung frame level atas (frame di dalam frame terpilih utuh), kalau tidak frame-nya.
    pub fn hit(&self, x: f32, y: f32, tol: f32) -> Option<u64> {
        hit_list(self, &self.nodes, x, y, tol, 1).map(|n| n.id)
    }

    /// The deepest node under this point, however many frames it sits in (Ctrl+click).
    pub fn hit_deep(&self, x: f32, y: f32, tol: f32) -> Option<u64> {
        hit_list(self, &self.nodes, x, y, tol, usize::MAX).map(|n| n.id)
    }

    /// The front-most direct child of frame `scope` under this point (a nested frame counts as
    /// a whole). `None` when the point misses them all, lies outside the frame, or the frame or
    /// one around it is hidden, locked or clips the point away.
    pub fn hit_within(&self, scope: u64, x: f32, y: f32, tol: f32) -> Option<u64> {
        let line = self.lineage(scope);
        let open = line.iter().all(|f| f.kind == Kind::Frame && f.visible && !f.locked && f.contains(x, y, 0.0));
        let frame = line.last().filter(|_| open)?;
        hit_list(self, &frame.children, x, y, tol, 0).map(|n| n.id)
    }

    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut it = self.nodes.iter().map(|n| n.bounds());
        let first = it.next()?;
        Some(it.fold(first, |a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))))
    }

    pub fn count(&self) -> usize {
        self.nodes.iter().map(|n| n.count()).sum()
    }

    /// Frames on the current page, nested ones included.
    pub fn frame_count(&self) -> usize {
        let mut count = 0;
        self.walk(|n| count += (n.kind == Kind::Frame) as usize);
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_node(s: &str) -> Node {
        let mut n = Node::new(1, Kind::Text, 0.0, 0.0);
        n.text = s.into();
        n.fill = Some([0, 0, 0, 255]);
        n
    }

    #[test]
    fn lines_higher_than_a_fixed_box_run_out_where_they_are_not_anchored() {
        let mut n = text_node("a\nb\nc");
        (n.h, n.fixed_h) = (20.0, true);
        // 50 of lines in a box of 20: out at the bottom, half at each end, or out at the top.
        assert_eq!([0, 1, 2].map(|v| Node { valign: v, ..n.clone() }.text_spill(50.0)), [30.0, 15.0, 30.0]);
        assert_eq!((n.text_spill(20.0), n.text_spill(5.0), Node { fixed_h: false, ..n.clone() }.text_spill(50.0)), (0.0, 0.0, 0.0));
    }

    #[test]
    fn sort_by_stack_follows_layer_order_not_click_order() {
        let mut doc = Doc::default();
        let frame = doc.alloc_id();
        doc.insert(Node::new(frame, Kind::Frame, 0.0, 0.0), None);
        let ids: Vec<u64> = (0..3)
            .map(|_| {
                let id = doc.alloc_id();
                doc.insert(Node::new(id, Kind::Rect, 0.0, 0.0), Some(frame));
                id
            })
            .collect();
        let top = doc.alloc_id();
        doc.insert(Node::new(top, Kind::Rect, 0.0, 0.0), None);
        // Diklik dari atas ke bawah: hasilnya tetap bawah ke atas.
        let mut picked = vec![top, ids[2], ids[0], ids[1]];
        doc.sort_by_stack(&mut picked);
        assert_eq!(picked, vec![ids[0], ids[1], ids[2], top]);
    }

    #[test]
    fn clipping_mask_runs() {
        let mut doc = Doc::default();
        for _ in 0..4 {
            let id = doc.alloc_id();
            doc.insert(Node::new(id, Kind::Rect, 0.0, 0.0), None);
        }
        // Paling bawah tidak punya alas.
        assert!(!doc.make_clip(&[1]));
        // Satu objek: di-clip ke layer di bawahnya.
        assert!(doc.make_clip(&[2]));
        assert_eq!(clip_run(&doc.nodes, 0), (1, 2));
        // Dua objek terpilih: yang bawah (1) jadi alas, 4 dipindah tepat di atasnya.
        assert!(doc.make_clip(&[1, 4]));
        let order: Vec<(u64, bool)> = doc.nodes.iter().map(|n| (n.id, n.clip)).collect();
        assert_eq!(order, [(1, false), (4, true), (2, true), (3, false)]);
        assert_eq!(clip_run(&doc.nodes, 0), (1, 3));
        // Group sebagai alas: seluruh anggotanya jadi satu alas.
        doc.release_clip(&[2, 4]);
        doc.group(&[1, 4]);
        assert!(doc.make_clip(&[2]));
        assert_eq!(clip_run(&doc.nodes, 0), (2, 3));
    }

    #[test]
    fn groups_restack_as_one_unit() {
        let mut doc = Doc::default();
        for _ in 0..6 {
            let id = doc.alloc_id();
            doc.insert(Node::new(id, Kind::Rect, 0.0, 0.0), None);
        }
        let order = |d: &Doc| d.nodes.iter().map(|n| n.id).collect::<Vec<u64>>();
        doc.group(&[2, 3, 4]);
        assert_eq!(order(&doc), [1, 2, 3, 4, 5, 6]);
        // Urutan seleksi tidak berpengaruh: isi group tetap 2, 3, 4.
        doc.reorder(&[4, 2, 3], false);
        assert_eq!(order(&doc), [2, 3, 4, 1, 5, 6]);
        doc.reorder(&[3, 4, 2], true);
        assert_eq!(order(&doc), [1, 5, 6, 2, 3, 4]);
        doc.reorder_step(&[2, 3, 4], false);
        assert_eq!(order(&doc), [1, 5, 2, 3, 4, 6]);
        // Layer lain melompati seluruh group, tidak menyelip ke dalamnya.
        doc.reorder_step(&[5], true);
        assert_eq!(order(&doc), [1, 2, 3, 4, 5, 6]);
        doc.reorder_step(&[5], false);
        assert_eq!(order(&doc), [1, 5, 2, 3, 4, 6]);
        // Sebagian anggota saja: bergeser di dalam group-nya.
        doc.reorder(&[4], false);
        assert_eq!(order(&doc), [1, 5, 4, 2, 3, 6]);
        doc.reorder_step(&[4], true);
        assert_eq!(order(&doc), [1, 5, 2, 4, 3, 6]);
        doc.reorder(&[2], true);
        assert_eq!(order(&doc), [1, 5, 4, 3, 2, 6]);
        // Layer yang di-clip ke group ikut pindah bersama alasnya.
        assert!(doc.make_clip(&[6]));
        doc.reorder(&[4, 3, 2], false);
        assert_eq!(order(&doc), [4, 3, 2, 6, 1, 5]);
        // Beberapa layer sekaligus: urutan relatifnya tetap.
        doc.reorder(&[5, 1], false);
        assert_eq!(order(&doc), [1, 5, 4, 3, 2, 6]);
    }

    #[test]
    fn pages_keep_their_own_layers() {
        let mut doc = Doc::default();
        let a = doc.alloc_id();
        doc.insert(Node::new(a, Kind::Rect, 0.0, 0.0), None);
        doc.guides.push(RulerGuide { vertical: true, at: 10.0 });
        assert_eq!((doc.page_count(), doc.page_name(0)), (1, "Page 1"));
        // Halaman baru kosong; isi halaman pertama tersimpan dan kembali saat dibuka lagi.
        assert_eq!(doc.add_page(), 1);
        assert!(doc.nodes.is_empty() && doc.guides.is_empty());
        assert_eq!(doc.page_name(1), "Page 2");
        let b = doc.alloc_id();
        doc.insert(Node::new(b, Kind::Ellipse, 0.0, 0.0), None);
        assert!(doc.find(a).is_none());
        assert_eq!(doc.max_node_id(), b);
        // Simpan dan buka lagi: halaman aktif dan isi halaman lain tetap.
        let mut back: Doc = serde_json::from_slice(&serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!((back.page, back.nodes[0].id, back.pages[0].nodes[0].id), (1, b, a));
        assert!(back.switch_page(0));
        assert_eq!((back.nodes[0].id, back.guides.len()), (a, 1));
        // Style berlaku lintas halaman.
        let style = doc.add_color_style([1, 2, 3, 255]);
        doc.switch_page(0);
        doc.find_mut(a).unwrap().fill_style = Some(style);
        doc.switch_page(1);
        assert_eq!(doc.color_style_uses(style), 1);
        doc.colors[0].color = [9, 9, 9, 255];
        doc.sync_color_style(style);
        doc.switch_page(0);
        assert_eq!(doc.find(a).unwrap().fill, Some([9, 9, 9, 255]));
        // Duplikat: id baru, pindah ke salinan; hapus: kembali ke tetangganya.
        assert_eq!(doc.duplicate_page(0), Some(1));
        assert_eq!((doc.page, doc.page_count(), doc.page_name(1)), (1, 3, "Page 1 copy"));
        assert!(doc.nodes.len() == 1 && doc.nodes[0].id != a);
        assert!(doc.move_page(1, 2));
        assert_eq!((doc.page, doc.page_name(2)), (2, "Page 1 copy"));
        assert!(doc.delete_page(2));
        assert_eq!((doc.page, doc.nodes[0].id), (1, b));
        assert!(doc.delete_page(0));
        assert!(!doc.delete_page(0));
        assert_eq!((doc.page, doc.page_count(), doc.nodes[0].id), (0, 1, b));
        // File lama tanpa daftar halaman tetap terbuka sebagai satu halaman.
        // A node saved before strokes had caps and joins: butt ends and mitred corners, as the
        // canvas drew it then. Only values that were set are written.
        let plain = r#"{"id":1,"name":"L","kind":"Line","x":0,"y":0,"w":10,"h":0,"stroke":[0,0,0,255],"stroke_w":2}"#;
        let mut line: Node = serde_json::from_str(plain).unwrap();
        assert_eq!((line.cap, line.join), (0, 0));
        assert_eq!((Node::new(1, Kind::Path, 0.0, 0.0).cap, Node::new(1, Kind::Path, 0.0, 0.0).join), (0, 0));
        let json = serde_json::to_string(&line).unwrap();
        assert!(!json.contains("\"cap\"") && !json.contains("\"join\""), "{json}");
        (line.cap, line.join) = (1, 2);
        let json = serde_json::to_string(&line).unwrap();
        assert!(json.contains("\"cap\":1") && json.contains("\"join\":2"), "{json}");
        let back: Node = serde_json::from_str(&json).unwrap();
        assert_eq!((back.cap, back.join), (1, 2));
        let old: Doc = serde_json::from_str(r#"{"nodes":[]}"#).unwrap();
        assert_eq!((old.page_count(), old.page_id()), (1, 0));
        // The same for inner shadow and background blur: a file from before has neither, and
        // a node without them is written as it was then.
        assert_eq!((line.inner_shadow, line.bg_blur, line.inner(), line.backdrop()), (None, 0.0, None, 0.0));
        let mut rect = Node::new(2, Kind::Rect, 0.0, 0.0);
        let json = serde_json::to_string(&rect).unwrap();
        assert!(!json.contains("inner_shadow") && !json.contains("bg_blur"), "{json}");
        (rect.inner_shadow, rect.bg_blur) = (Some(Shadow { x: 1.0, y: 2.0, blur: 3.0, color: [4, 5, 6, 7] }), 12.0);
        let back: Node = serde_json::from_str(&serde_json::to_string(&rect).unwrap()).unwrap();
        assert_eq!((back.inner_shadow, back.bg_blur), (rect.inner_shadow, 12.0));
        assert_eq!((back.inner(), back.backdrop()), (rect.inner_shadow, 12.0));
        // A line has no inside; an open path neither.
        (line.inner_shadow, line.bg_blur) = (rect.inner_shadow, 12.0);
        assert_eq!((line.inner(), line.backdrop()), (None, 0.0));
        let mut path = Node::new(3, Kind::Path, 0.0, 0.0);
        (path.inner_shadow, path.bg_blur) = (rect.inner_shadow, 12.0);
        assert_eq!((path.inner(), path.backdrop()), (None, 0.0));
        path.closed = true;
        assert_eq!((path.inner(), path.backdrop()), (rect.inner_shadow, 12.0));
    }

    #[test]
    fn style_only_the_selected_range() {
        let mut n = text_node("Dokumentasi Kajian");
        n.style_range(12, 18, |s| s.font = "Lora".into());
        assert_eq!(n.spans.len(), 1);
        assert_eq!((n.spans[0].start, n.spans[0].end), (12, 18));
        assert_eq!(n.style_at(3).font, "Inter");
        assert_eq!(n.style_at(14).font, "Lora");
        // Ubah sebagian dari rentang itu: terpecah jadi dua gaya.
        n.style_range(15, 18, |s| s.weight = 700);
        assert_eq!(n.spans.len(), 2);
        assert_eq!((n.style_at(13).weight, n.style_at(16).weight), (400, 700));
        assert_eq!(n.style_at(16).font, "Lora");
        // Kembalikan ke gaya dasar: rentang hilang.
        n.style_range(12, 18, |s| {
            s.font = "Inter".into();
            s.weight = 400;
        });
        assert!(n.spans.is_empty());
    }

    #[test]
    fn underline_and_strikethrough_toggle_per_range_and_for_the_whole_text() {
        let mut n = text_node("Dokumentasi Kajian");
        // A range: on, and only there.
        n.toggle_decoration(12, 18, false);
        assert_eq!((n.spans.len(), n.spans[0].start, n.spans[0].end), (1, 12, 18));
        assert!(n.style_at(14).underline && !n.style_at(3).underline && !n.underline);
        assert!(n.decorated(12, 18, false) && !n.decorated(10, 18, false) && !n.decorated(12, 18, true));
        // Partly underlined: the whole range goes on first, then off.
        n.toggle_decoration(10, 18, false);
        assert!(n.decorated(10, 18, false));
        n.toggle_decoration(10, 18, false);
        assert!(n.spans.is_empty());
        // Strikethrough is separate, and both can be on one range.
        n.toggle_decoration(0, 4, true);
        n.toggle_decoration(2, 6, false);
        assert_eq!((n.style_at(1).strike, n.style_at(1).underline), (true, false));
        assert_eq!((n.style_at(3).strike, n.style_at(3).underline), (true, true));
        assert_eq!((n.style_at(5).strike, n.style_at(5).underline), (false, true));
        // The whole text (an empty range): it goes on for every range too, which keep what
        // else they have; then off again.
        n.style_range(8, 11, |s| s.weight = 700);
        n.toggle_decoration(0, 0, false);
        assert!(n.underline && n.decorated(0, 0, false) && n.style_at(9).underline && n.style_at(9).weight == 700);
        assert!(n.style_at(1).strike && !n.strike);
        n.toggle_decoration(0, 0, false);
        assert!(!n.underline && !n.style_at(3).underline && !n.style_at(9).underline);
        // A range of an underlined text can be taken out of it.
        let mut n = text_node("Dokumentasi Kajian");
        n.set_decoration(false, true);
        n.toggle_decoration(0, 11, false);
        assert!(n.underline && !n.style_at(4).underline && n.style_at(14).underline);
        assert!(!n.decorated(0, 0, false));
    }

    #[test]
    fn new_text_fields_are_left_out_of_files_until_used() {
        let n = text_node("Kanvas");
        let plain = serde_json::to_string(&n).unwrap();
        for key in ["underline", "strike", "\"list\"", "valign", "fixed_h"] {
            assert!(!plain.contains(key), "{key} is written for a text that does not use it");
        }
        // A file from before these fields existed, also with a styled range.
        let mut v: serde_json::Value = serde_json::from_str(&plain).unwrap();
        v["spans"] = serde_json::json!([{ "start": 0, "end": 2, "style": { "font": "Lora", "weight": 700, "italic": false, "size": 20.0, "fill": [0, 0, 0, 255] } }]);
        let old: Node = serde_json::from_value(v).unwrap();
        assert_eq!((old.underline, old.strike, old.list, old.valign, old.fixed_h), (false, false, 0, 0, false));
        assert_eq!((old.spans[0].style.underline, old.spans[0].style.strike), (false, false));
        assert!(!serde_json::to_string(&old).unwrap().contains("underline"));
        // What is set is written and read back.
        let mut n = text_node("Kanvas");
        (n.list, n.valign, n.fixed_h, n.align, n.strike) = (2, 1, true, 3, true);
        n.toggle_decoration(0, 3, false);
        let back: Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back, n);
        let style: TypeStyle = serde_json::from_str(r#"{"id":1,"name":"Body","font":"Inter","weight":400,"italic":false,"size":16.0,"line_height":0.0,"letter_spacing":0.0}"#).unwrap();
        assert!(!style.underline && !style.strike && style.matches(&text_node("x")));
    }

    #[test]
    fn list_markers_skip_empty_paragraphs() {
        let mut n = text_node("");
        let text = "one\n\ntwo\nthree\n";
        assert_eq!(n.list_labels(text), vec![None; 5]);
        assert_eq!(n.list_indent(text), 0.0);
        n.list = 1;
        let dot = Some("\u{2022}".to_owned());
        assert_eq!(n.list_labels(text), vec![dot.clone(), None, dot.clone(), dot, None]);
        assert_eq!(n.list_indent(text), 24.0);
        n.list = 2;
        let number = |i: u32| Some(format!("{i}."));
        assert_eq!(n.list_labels(text), vec![number(1), None, number(2), number(3), None]);
        // Two digits need more room than one.
        let short = n.list_indent(text);
        let long = n.list_indent(&"x\n".repeat(10));
        assert!(short >= 16.0 * 1.5 && long > short, "{short} {long}");
    }

    #[test]
    fn lines_are_placed_in_a_box_of_fixed_height() {
        let mut n = text_node("Kanvas");
        (n.h, n.valign) = (100.0, 1);
        // The height follows the lines: nothing to place.
        assert_eq!(n.text_dy(40.0), 0.0);
        n.fixed_h = true;
        assert_eq!(n.text_dy(40.0), 30.0);
        n.valign = 2;
        assert_eq!(n.text_dy(40.0), 60.0);
        // Higher than the box: out at the top.
        assert_eq!(n.text_dy(130.0), -30.0);
        n.valign = 0;
        assert_eq!(n.text_dy(130.0), 0.0);
        // Stretched with a selection, such a box changes height like any other.
        (n.wrap, n.w) = (true, 50.0);
        n.stretch((0.0, 0.0), (0.0, 0.0), (2.0, 3.0));
        assert_eq!((n.w, n.h), (100.0, 300.0));
        n.fixed_h = false;
        n.stretch((0.0, 0.0), (0.0, 0.0), (2.0, 3.0));
        assert_eq!((n.w, n.h), (200.0, 300.0));
    }

    #[test]
    fn spans_follow_text_edits() {
        let mut n = text_node("Dokumentasi Kajian");
        n.style_range(12, 18, |s| s.italic = true);
        // Sisip 5 huruf di depan: rentang bergeser.
        let new = "Buku Dokumentasi Kajian";
        n.sync_spans("Dokumentasi Kajian", new);
        n.text = new.into();
        assert_eq!((n.spans[0].start, n.spans[0].end), (17, 23));
        // Ketik di ujung rentang: gaya ikut memanjang.
        let newer = "Buku Dokumentasi Kajian!";
        n.sync_spans(new, newer);
        assert_eq!(n.spans[0].end, 24);
        // Hapus seluruh kata bergaya: rentang hilang.
        n.sync_spans(newer, "Buku Dokumentasi ");
        assert!(n.spans.is_empty());
    }

    #[test]
    fn grouping_gathers_members_and_copies_get_a_new_group() {
        let mut doc = Doc::default();
        let ids: Vec<u64> = (0..5)
            .map(|_| {
                let id = doc.alloc_id();
                doc.insert(Node::new(id, Kind::Rect, 0.0, 0.0), None);
                id
            })
            .collect();
        let g = doc.group(&[ids[0], ids[3]]);
        // Anggota dirapatkan di posisi anggota paling depan; urutan lainnya tetap.
        let order: Vec<u64> = doc.nodes.iter().map(|n| n.id).collect();
        assert_eq!(order, vec![ids[1], ids[2], ids[0], ids[3], ids[4]]);
        assert_eq!(doc.group_members(&[g]), vec![ids[0], ids[3]]);

        let mut copies: Vec<Node> = doc.group_members(&[g]).iter().map(|id| doc.find(*id).unwrap().clone()).collect();
        doc.regroup(&mut copies);
        assert!(copies[0].group != 0 && copies[0].group != g && copies[0].group == copies[1].group);
        let mut single = vec![doc.find(ids[0]).unwrap().clone()];
        doc.regroup(&mut single);
        assert_eq!(single[0].group, 0);

        doc.ungroup(&[g]);
        assert!(doc.group_members(&[g]).is_empty());
    }

    #[test]
    fn layers_move_between_groups_and_frames() {
        let mut doc = Doc::default();
        let frame = doc.alloc_id();
        doc.insert(Node::new(frame, Kind::Frame, 0.0, 0.0), None);
        let ids: Vec<u64> = (0..5)
            .map(|_| {
                let id = doc.alloc_id();
                doc.insert(Node::new(id, Kind::Rect, 0.0, 0.0), None);
                id
            })
            .collect();
        let order = |d: &Doc| -> Vec<u64> { d.nodes.iter().map(|n| n.id).collect() };
        let g = doc.group(&[ids[0], ids[1]]);
        // Ke dalam group, tepat di atas anggota paling depan.
        assert!(doc.move_nodes(&[ids[4]], None, Some((ids[1], true)), g));
        assert_eq!(order(&doc), vec![frame, ids[0], ids[1], ids[4], ids[2], ids[3]]);
        assert_eq!(doc.group_members(&[g]), vec![ids[0], ids[1], ids[4]]);
        // Keluar dari group: di bawah ids[3], tanpa group.
        assert!(doc.move_nodes(&[ids[0]], None, Some((ids[3], false)), 0));
        assert_eq!(order(&doc), vec![frame, ids[1], ids[4], ids[2], ids[0], ids[3]]);
        assert_eq!(doc.find(ids[0]).unwrap().group, 0);
        // Ke dalam frame; frame sendiri tidak bisa masuk frame.
        assert!(doc.move_nodes(&[ids[2], ids[3]], Some(frame), None, 0));
        assert_eq!(doc.nodes[0].children.iter().map(|c| c.id).collect::<Vec<_>>(), vec![ids[2], ids[3]]);
        assert!(!doc.move_nodes(&[frame], Some(frame), None, 0));
        // Seluruh group dipindah: tetap satu group.
        assert!(doc.move_nodes(&[ids[1], ids[4]], Some(frame), Some((ids[2], false)), 0));
        assert_eq!(doc.nodes[0].children.iter().map(|c| c.id).collect::<Vec<_>>(), vec![ids[1], ids[4], ids[2], ids[3]]);
        assert_eq!(doc.group_members(&[g]), vec![ids[1], ids[4]]);
        // Dilepas di tempat semula: tidak ada yang berubah.
        assert!(!doc.move_nodes(&[ids[2]], Some(frame), Some((ids[2], true)), 0));
        // Group yang ditinggal semua anggotanya ikut hilang namanya.
        let h = doc.group(&[ids[2], ids[3]]);
        assert!(doc.move_nodes(&[ids[1], ids[4]], Some(frame), Some((ids[3], true)), h));
        assert_eq!(doc.group_members(&[h]), vec![ids[2], ids[3], ids[1], ids[4]]);
        assert_eq!(doc.group_names.keys().copied().collect::<Vec<_>>(), vec![h]);
    }

    /// Top frame 1 (400×400) › frame 2 (at 100,100, 200×200) › frame 3 (at 150,150, 100×100) ›
    /// rect 4 (at 180,180, 200×40, sticking out of 3 and 2). Rect 5 in frame 1, rect 6 at top level.
    fn nested() -> Doc {
        let mut doc = Doc::default();
        let mut add = |kind: Kind, parent: Option<u64>, (x, y, w, h): (f32, f32, f32, f32)| {
            let id = doc.alloc_id();
            let mut n = Node::new(id, kind, x, y);
            (n.w, n.h, n.fill) = (w, h, Some([0, 0, 0, 255]));
            doc.insert(n, parent);
            id
        };
        add(Kind::Frame, None, (0.0, 0.0, 400.0, 400.0));
        add(Kind::Frame, Some(1), (100.0, 100.0, 200.0, 200.0));
        add(Kind::Frame, Some(2), (150.0, 150.0, 100.0, 100.0));
        add(Kind::Rect, Some(3), (180.0, 180.0, 200.0, 40.0));
        add(Kind::Rect, Some(1), (10.0, 10.0, 50.0, 50.0));
        add(Kind::Rect, None, (500.0, 0.0, 50.0, 50.0));
        doc
    }

    #[test]
    fn tree_api_reaches_any_depth() {
        let mut doc = nested();
        assert_eq!(doc.locate(4), Some(vec![0, 0, 0, 0]));
        assert_eq!(doc.locate(5), Some(vec![0, 1]));
        assert_eq!(doc.locate(6), Some(vec![1]));
        assert_eq!(doc.locate(99), None);
        assert_eq!(doc.at(&[0, 0, 0]).map(|n| n.id), Some(3));
        assert_eq!(doc.find(4).map(|n| n.kind), Some(Kind::Rect));
        assert_eq!((doc.parent_of(4), doc.parent_of(2), doc.parent_of(1), doc.parent_of(99)), (Some(3), Some(1), None, None));
        assert_eq!(doc.ancestors(4), vec![3, 2, 1]);
        assert!(doc.ancestors(6).is_empty());
        assert_eq!(doc.lineage(3).iter().map(|n| n.id).collect::<Vec<_>>(), vec![1, 2, 3]);
        assert_eq!((doc.top_of(4), doc.top_of(6)), (Some(1), Some(6)));
        assert!(doc.is_descendant(4, 1) && doc.is_descendant(4, 3) && !doc.is_descendant(1, 4) && !doc.is_descendant(3, 3));
        assert_eq!(doc.child_ids(Some(1)), vec![2, 5]);
        assert_eq!(doc.child_ids(None), vec![1, 6]);
        assert_eq!(doc.roots_of(&[4, 6, 2, 5]), vec![6, 2, 5]);
        assert_eq!((doc.count(), doc.frame_count(), doc.max_node_id()), (6, 3, 6));
        let mut seen = Vec::new();
        doc.walk(|n| seen.push(n.id));
        assert_eq!(seen, vec![1, 2, 3, 4, 5, 6]);
        // Parents sort below what they hold.
        let mut ids = vec![6, 4, 5, 1, 3, 2];
        doc.sort_by_stack(&mut ids);
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6]);

        // A change deep inside one frame copies that top-level node only.
        let snapshot = doc.clone();
        doc.find_mut(4).unwrap().x = 185.0;
        assert_eq!((doc.find(4).unwrap().x, snapshot.find(4).unwrap().x), (185.0, 180.0));
        assert!(!Arc::ptr_eq(&doc.nodes[0], &snapshot.nodes[0]) && Arc::ptr_eq(&doc.nodes[1], &snapshot.nodes[1]));

        // Styles reach every depth, and leave the other top-level nodes shared.
        let style = doc.add_color_style([1, 2, 3, 255]);
        doc.find_mut(4).unwrap().fill_style = Some(style);
        doc.find_mut(2).unwrap().stroke_style = Some(style);
        assert_eq!(doc.color_style_uses(style), 2);
        let snapshot = doc.clone();
        doc.colors[0].color = [9, 9, 9, 255];
        doc.sync_color_style(style);
        assert_eq!((doc.find(4).unwrap().fill, doc.find(2).unwrap().stroke), (Some([9, 9, 9, 255]), Some([9, 9, 9, 255])));
        assert!(Arc::ptr_eq(&doc.nodes[1], &snapshot.nodes[1]));
        doc.colors.clear();
        doc.drop_missing_styles();
        assert_eq!((doc.find(4).unwrap().fill_style, doc.find(2).unwrap().stroke_style), (None, None));

        // Moving a frame carries everything in it; a frame can be inserted into a frame.
        doc.find_mut(2).unwrap().translate(10.0, 0.0);
        assert_eq!((doc.find(3).unwrap().x, doc.find(4).unwrap().x), (160.0, 195.0));
        let taken = doc.remove(3).unwrap();
        assert_eq!((taken.children.len(), doc.find(4).is_none(), doc.count()), (1, true, 4));
        doc.insert(taken, Some(1));
        assert_eq!((doc.parent_of(3), doc.parent_of(4)), (Some(1), Some(3)));
        // Not a frame: goes to the top level.
        let id = doc.alloc_id();
        doc.insert(Node::new(id, Kind::Rect, 0.0, 0.0), Some(5));
        assert_eq!(doc.parent_of(id), None);

        // Saved and opened again, the tree is the same.
        let back: Doc = serde_json::from_slice(&serde_json::to_vec(&doc).unwrap()).unwrap();
        assert_eq!(back.locate(4), doc.locate(4));
        assert_eq!(back.nodes[0], doc.nodes[0]);
    }

    #[test]
    fn frames_move_into_frames_but_never_into_themselves() {
        let mut doc = nested();
        // Into itself, into its own content, into something that is not a frame.
        assert!(!doc.move_nodes(&[2], Some(2), None, 0));
        assert!(!doc.move_nodes(&[2], Some(3), None, 0));
        assert!(!doc.move_nodes(&[1], Some(3), None, 0));
        assert!(!doc.move_nodes(&[6], Some(5), None, 0));
        assert_eq!(doc.locate(4), Some(vec![0, 0, 0, 0]));
        // A nested frame out to the top level, below rect 6, with its content.
        let snapshot = doc.clone();
        assert!(doc.move_nodes(&[3], None, Some((6, false)), 0));
        assert_eq!(doc.child_ids(None), vec![1, 3, 6]);
        assert_eq!((doc.parent_of(4), doc.child_ids(Some(2))), (Some(3), vec![]));
        assert!(Arc::ptr_eq(&doc.nodes[2], &snapshot.nodes[1]));
        // And into another nested frame, above nothing: front-most.
        assert!(doc.move_nodes(&[3, 6], Some(2), None, 0));
        assert_eq!(doc.child_ids(Some(2)), vec![3, 6]);
        assert_eq!(doc.ancestors(4), vec![3, 2, 1]);
        // A frame picked together with its content moves once, content in place.
        assert!(doc.move_nodes(&[4, 3], Some(1), Some((2, false)), 0));
        assert_eq!((doc.child_ids(Some(1)), doc.child_ids(Some(3))), (vec![3, 2, 5], vec![4]));
        // Between two frames at different depths.
        assert!(doc.move_nodes(&[4], Some(2), Some((6, true)), 0));
        assert_eq!(doc.child_ids(Some(2)), vec![6, 4]);
    }

    #[test]
    fn clicks_pick_by_depth() {
        let mut doc = nested();
        // Inside the top frame: its direct child, a nested frame as a whole.
        assert_eq!(doc.hit(200.0, 200.0, 1.0), Some(2));
        assert_eq!(doc.hit(20.0, 20.0, 1.0), Some(5));
        assert_eq!(doc.hit(350.0, 350.0, 1.0), Some(1));
        assert_eq!(doc.hit(510.0, 10.0, 1.0), Some(6));
        assert_eq!(doc.hit(450.0, 450.0, 1.0), None);
        // One level in at a time.
        assert_eq!(doc.hit_within(2, 200.0, 200.0, 1.0), Some(3));
        assert_eq!(doc.hit_within(3, 200.0, 200.0, 1.0), Some(4));
        assert_eq!(doc.hit_within(3, 160.0, 160.0, 1.0), None);
        assert_eq!(doc.hit_within(2, 110.0, 110.0, 1.0), None);
        // The deepest object; where a frame clips it away, whatever shows there instead.
        assert_eq!(doc.hit_deep(200.0, 200.0, 1.0), Some(4));
        assert_eq!(doc.hit_deep(160.0, 160.0, 1.0), Some(3));
        assert_eq!(doc.hit_deep(270.0, 200.0, 1.0), Some(2));
        assert_eq!(doc.hit_deep(350.0, 200.0, 1.0), Some(1));
        assert_eq!(doc.hit_within(3, 270.0, 200.0, 1.0), None);
        // The deepest frame, for dropping things into; without the ones being dragged.
        assert_eq!(doc.frame_at(200.0, 200.0, &[]), Some(3));
        assert_eq!(doc.frame_at(200.0, 200.0, &[3]), Some(2));
        assert_eq!(doc.frame_at(200.0, 200.0, &[2]), Some(1));
        assert_eq!(doc.frame_at(270.0, 200.0, &[]), Some(2));
        assert_eq!(doc.frame_at(510.0, 10.0, &[]), None);
        // A frame only goes into a frame that is larger than it.
        let mut f = Node::new(0, Kind::Frame, 0.0, 0.0);
        (f.w, f.h) = (150.0, 90.0);
        assert_eq!(doc.frame_for(&f, 200.0, 200.0, &[]), Some(2));
        (f.w, f.h) = (150.0, 500.0);
        assert_eq!(doc.frame_for(&f, 200.0, 200.0, &[]), None);
        assert_eq!(doc.frame_for(doc.find(6).unwrap(), 200.0, 200.0, &[]), Some(3));
        // A locked or hidden frame takes everything in it along.
        doc.find_mut(3).unwrap().locked = true;
        assert_eq!(doc.hit_deep(200.0, 200.0, 1.0), Some(2));
        assert_eq!(doc.hit_within(3, 200.0, 200.0, 1.0), None);
        doc.find_mut(2).unwrap().visible = false;
        assert_eq!((doc.hit(200.0, 200.0, 1.0), doc.hit_deep(200.0, 200.0, 1.0)), (Some(1), Some(1)));
        assert_eq!(doc.frame_at(200.0, 200.0, &[]), Some(1));
    }

    #[test]
    fn stacking_groups_and_masks_inside_nested_frames() {
        let mut doc = nested();
        let ids: Vec<u64> = (0..4)
            .map(|_| {
                let id = doc.alloc_id();
                doc.insert(Node::new(id, Kind::Rect, 160.0, 160.0), Some(3));
                id
            })
            .collect();
        let (a, b, c, d) = (ids[0], ids[1], ids[2], ids[3]);
        assert_eq!(doc.child_ids(Some(3)), vec![4, a, b, c, d]);
        let snapshot = doc.clone();
        doc.reorder(&[a], true);
        assert_eq!(doc.child_ids(Some(3)), vec![4, b, c, d, a]);
        doc.reorder_step(&[d], false);
        assert_eq!(doc.child_ids(Some(3)), vec![4, b, d, c, a]);
        // A frame picked with its content moves as one; the content keeps its order.
        doc.reorder(&[2, a], false);
        assert_eq!((doc.child_ids(Some(1)), doc.child_ids(Some(3))), (vec![2, 5], vec![4, b, d, c, a]));
        doc.reorder(&[5, a], false);
        assert_eq!((doc.child_ids(Some(1)), doc.child_ids(Some(3))), (vec![5, 2], vec![a, 4, b, d, c]));
        assert!(Arc::ptr_eq(&doc.nodes[1], &snapshot.nodes[1]));
        // Grouping gathers the members where the front-most one is.
        let g = doc.group(&[a, d]);
        assert_eq!(doc.child_ids(Some(3)), vec![4, b, a, d, c]);
        assert_eq!(doc.group_members(&[g]), vec![a, d]);
        // The group moves as one unit among its siblings.
        doc.reorder(&[a, d], false);
        assert_eq!(doc.child_ids(Some(3)), vec![a, d, 4, b, c]);
        // A clipping mask on the layer below, three frames deep.
        assert!(doc.make_clip(&[4]));
        assert!(doc.find(4).unwrap().clip);
        assert_eq!(clip_run(&doc.find(3).unwrap().children, 0), (2, 3));
        doc.release_clip(&[4]);
        doc.ungroup(&[g]);
        assert!(doc.group_members(&[g]).is_empty() && !doc.find(4).unwrap().clip);
        // Copies of a nested frame get ids of their own all the way down.
        let mut copy = doc.find(2).unwrap().clone();
        doc.reassign_ids(&mut copy);
        let mut fresh = Vec::new();
        copy.walk(&mut |n| fresh.push(n.id));
        assert!(fresh.len() == 7 && fresh.iter().all(|id| doc.find(*id).is_none()));
    }

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"a", b"ab", b"abc", &[0u8, 255, 16, 128, 7]] {
            assert_eq!(b64::decode(&b64::encode(data)).unwrap(), data);
        }
    }

    #[test]
    fn path_radius_rounds_sharp_corners() {
        let p = |x: f32, y: f32| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        let mut n = Node::new(1, Kind::Path, 0.0, 0.0);
        (n.w, n.h, n.closed) = (100.0, 60.0, true);
        n.points = vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)];
        n.radius = 10.0;
        let r = n.rounded_points(&n.points, true);
        assert_eq!(r.len(), 8);
        // Sudut kiri-atas: titik singgung 10 satuan dari sudut di kedua sisi.
        assert!((r[0].x * 100.0).abs() < 1e-3 && (r[0].y * 60.0 - 10.0).abs() < 1e-3, "{:?}", r[0]);
        assert!((r[1].x * 100.0 - 10.0).abs() < 1e-3 && (r[1].y * 60.0).abs() < 1e-3, "{:?}", r[1]);
        // Titik hasil perataan di pojok kiri-atas berada di lingkaran sudut (pusat 10,10).
        let pts = n.path_points(16);
        assert!(pts.iter().all(|(x, y)| *x >= 10.0 || *y >= 10.0 || ((x - 10.0).hypot(y - 10.0) - 10.0).abs() < 0.05));
        // Jari-jari terlalu besar dibatasi setengah sisi terpendek.
        n.radius = 500.0;
        let r = n.rounded_points(&n.points, true);
        assert!((r[0].y * 60.0 - 30.0).abs() < 1e-3 && (r[1].x * 100.0 - 30.0).abs() < 1e-3);
        // Jari-jari per titik menang atas jari-jari node; 0 = sudut itu tetap tajam.
        n.radius = 10.0;
        n.points[2].r = Some(0.0);
        n.points[3].r = Some(20.0);
        let r = n.rounded_points(&n.points, true);
        assert_eq!(r.len(), 7);
        assert!((r[5].x * 100.0 - 20.0).abs() < 1e-3 && (r[6].y * 60.0 - 40.0).abs() < 1e-3, "{:?} {:?}", r[5], r[6]);
        n.clear_corner_radii();
        assert_eq!(n.rounded_points(&n.points, true).len(), 8);
        // Titik berpegangan (kurva) dan tetangganya tidak disentuh.
        n.points[0].hout = [0.1, 0.0];
        assert_eq!(n.rounded_points(&n.points, true).len(), 6);
    }

    #[test]
    fn closed_path_hit_test() {
        let mut n = Node::new(1, Kind::Path, 10.0, 10.0);
        n.w = 100.0;
        n.h = 100.0;
        let p = |x, y| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        n.points = vec![p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0)];
        n.closed = true;
        n.fill = Some([0, 0, 0, 255]);
        assert!(n.contains(30.0, 30.0, 1.0));
        assert!(!n.contains(100.0, 100.0, 1.0));
        assert_eq!(n.path_points(8).len(), 3);
    }
}
