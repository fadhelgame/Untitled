//! Model dokumen. Node level atas disimpan sebagai `Arc<Node>` supaya snapshot
//! undo berbagi memori untuk frame yang tidak berubah (ratusan frame tetap murah).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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
    /// 0 kiri, 1 tengah, 2 kanan.
    #[serde(default)]
    pub align: u8,
    /// Teks paragraf: lebar tetap (`w`), baris membungkus otomatis.
    #[serde(default)]
    pub wrap: bool,
    /// Gaya per rentang karakter; di luar rentang berlaku gaya node.
    #[serde(default)]
    pub spans: Vec<Span>,
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
    /// Derajat, searah jarum jam, terhadap pusat kotak pembatas.
    #[serde(default)]
    pub rotation: f32,
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
    /// Drop shadow di belakang objek.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shadow: Option<Shadow>,
    /// Layer blur: seluruh objek (beserta bayangannya, dan isi frame) diburamkan. Satuan dokumen,
    /// kebiasaan CSS/Figma (simpangan baku = setengah nilai ini); 0 = tidak buram.
    /// Anggota satu group yang nilainya sama diburamkan sebagai satu kesatuan (lihat `blur_run`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub blur: f32,
    /// Color style yang dipakai fill / stroke, dan text style (id ke `Doc::colors` / `Doc::type_styles`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_style: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stroke_style: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub type_style: Option<u64>,
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

    /// Seberapa jauh layer blur melebar keluar dari objeknya (3 simpangan baku).
    pub fn blur_pad(&self) -> f32 {
        if self.blur > 0.0 {
            self.blur * 1.5 + 1.0
        } else {
            0.0
        }
    }
}

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
            points: Vec::new(),
            closed: false,
            holes: Vec::new(),
            image: None,
            rotation: 0.0,
            blend: 0,
            opacity: 1.0,
            visible: true,
            locked: false,
            group: 0,
            clip: false,
            dash: 0.0,
            gap: 0.0,
            stroke_align: None,
            shadow: None,
            blur: 0.0,
            fill_style: None,
            stroke_style: None,
            type_style: None,
            children: Vec::new(),
        }
    }

    /// Kotak pembatas ternormalisasi: (x0, y0, x1, y1).
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let (x0, x1) = if self.w < 0.0 { (self.x + self.w, self.x) } else { (self.x, self.x + self.w) };
        let (y0, y1) = if self.h < 0.0 { (self.y + self.h, self.y) } else { (self.y, self.y + self.h) };
        (x0, y0, x1, y1)
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

    fn flatten(&self, points: &[PathPoint], closed: bool, segs: usize) -> Vec<(f32, f32)> {
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
            let steps = if straight { 1 } else { segs.max(2) };
            let (c1, c2) = ((a.x + a.hout[0], a.y + a.hout[1]), (b.x + b.hin[0], b.y + b.hin[1]));
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

    // ---------------------------------------------------------------- gaya teks

    pub fn base_style(&self) -> Style {
        Style {
            font: self.font.clone(),
            weight: self.weight,
            italic: self.italic,
            size: self.font_size,
            fill: self.fill.unwrap_or([0, 0, 0, 255]),
        }
    }

    pub fn apply_style(&mut self, s: &Style) {
        self.font = s.font.clone();
        self.weight = s.weight;
        self.italic = s.italic;
        self.font_size = s.size;
        self.fill = Some(s.fill);
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
        }
    }

    pub fn matches(&self, n: &Node) -> bool {
        self.font == n.font
            && self.weight == n.weight
            && self.italic == n.italic
            && self.size == n.font_size
            && self.line_height == n.line_height
            && self.letter_spacing == n.letter_spacing
    }

    pub fn apply(&self, n: &mut Node) {
        n.font = self.font.clone();
        n.weight = self.weight;
        n.italic = self.italic;
        n.font_size = self.size;
        n.line_height = self.line_height;
        n.letter_spacing = self.letter_spacing;
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

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Doc {
    pub nodes: Vec<Arc<Node>>,
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

    /// Ubah node yang memenuhi `uses` (level atas dan anak frame). Frame yang tidak tersentuh
    /// tidak disalin, jadi snapshot undo tetap berbagi memori.
    fn update_nodes(&mut self, uses: impl Fn(&Node) -> bool, mut apply: impl FnMut(&mut Node)) {
        for top in &mut self.nodes {
            if uses(top) || top.children.iter().any(&uses) {
                let top = Arc::make_mut(top);
                if uses(top) {
                    apply(top);
                }
                for c in top.children.iter_mut().filter(|c| uses(c)) {
                    apply(c);
                }
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
        let uses = |n: &Node| (n.fill_style == Some(id)) as usize + (n.stroke_style == Some(id)) as usize;
        self.nodes.iter().map(|n| uses(n) + n.children.iter().map(uses).sum::<usize>()).sum()
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

    /// Posisi node: (indeks level atas, indeks anak jika node adalah anak frame).
    pub fn locate(&self, id: u64) -> Option<(usize, Option<usize>)> {
        for (ti, n) in self.nodes.iter().enumerate() {
            if n.id == id {
                return Some((ti, None));
            }
            if let Some(ci) = n.children.iter().position(|c| c.id == id) {
                return Some((ti, Some(ci)));
            }
        }
        None
    }

    pub fn find(&self, id: u64) -> Option<&Node> {
        let (ti, ci) = self.locate(id)?;
        match ci {
            None => Some(&self.nodes[ti]),
            Some(ci) => self.nodes[ti].children.get(ci),
        }
    }

    pub fn find_mut(&mut self, id: u64) -> Option<&mut Node> {
        let (ti, ci) = self.locate(id)?;
        let top = Arc::make_mut(&mut self.nodes[ti]);
        match ci {
            None => Some(top),
            Some(ci) => top.children.get_mut(ci),
        }
    }

    pub fn parent_of(&self, id: u64) -> Option<u64> {
        match self.locate(id)? {
            (ti, Some(_)) => Some(self.nodes[ti].id),
            _ => None,
        }
    }

    pub fn remove(&mut self, id: u64) -> Option<Node> {
        match self.locate(id)? {
            (ti, None) => Some(Arc::unwrap_or_clone(self.nodes.remove(ti))),
            (ti, Some(ci)) => Some(Arc::make_mut(&mut self.nodes[ti]).children.remove(ci)),
        }
    }

    /// Sisipkan di urutan paling atas dari `parent` (atau level atas bila `None`).
    pub fn insert(&mut self, node: Node, parent: Option<u64>) {
        if let Some(p) = parent {
            if let Some((ti, None)) = self.locate(p) {
                if self.nodes[ti].kind == Kind::Frame && node.kind != Kind::Frame {
                    Arc::make_mut(&mut self.nodes[ti]).children.push(node);
                    return;
                }
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
        restack(&mut self.nodes, ids, up, step);
        for top in &mut self.nodes {
            // Frame yang ikut terpilih pindah beserta isinya; urutan isinya tidak diubah.
            if !ids.contains(&top.id) && top.children.iter().any(|c| ids.contains(&c.id)) {
                restack(&mut Arc::make_mut(top).children, ids, up, step);
            }
        }
    }

    /// Pindahkan node-node `ids` (tarik-lepas di panel layer) ke `parent` (frame, atau level atas
    /// bila `None`). `anchor` = (id saudara, di atasnya?): disisipkan tepat di atas/bawah saudara
    /// itu dalam urutan tumpukan; `None` = paling depan. Semua yang dipindah ikut `group` tujuan;
    /// kalau tujuannya bukan group, group yang terbawa utuh tetap jadi group. Urutan relatif yang
    /// dipindah tetap. Hasil `false`: tidak sah (frame ke dalam frame) atau tidak ada yang berubah.
    pub fn move_nodes(&mut self, ids: &[u64], parent: Option<u64>, anchor: Option<(u64, bool)>, group: u64) -> bool {
        let layout = |d: &Doc| -> Vec<(u64, u64, u64, bool)> {
            d.nodes.iter().flat_map(|n| std::iter::once((n.id, 0, n.group, n.clip)).chain(n.children.iter().map(|c| (c.id, n.id, c.group, c.clip)))).collect()
        };
        let before = layout(self);
        if let Some(p) = parent {
            let ok = !ids.contains(&p) && self.nodes.iter().any(|n| n.id == p && n.kind == Kind::Frame);
            if !ok || self.nodes.iter().any(|n| n.kind == Kind::Frame && ids.contains(&n.id)) {
                return false;
            }
        }
        // Posisi sisip dihitung di daftar tujuan tanpa node yang dipindah.
        let siblings: Vec<u64> = match parent {
            Some(p) => self.find(p).map(|f| f.children.iter().map(|c| c.id).collect()).unwrap_or_default(),
            None => self.nodes.iter().map(|n| n.id).collect(),
        };
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

        let mut moved: Vec<Node> = Vec::new();
        for top in std::mem::take(&mut self.nodes) {
            if ids.contains(&top.id) {
                moved.push(Arc::unwrap_or_clone(top));
                continue;
            }
            let mut top = top;
            if top.children.iter().any(|c| ids.contains(&c.id)) {
                let t = Arc::make_mut(&mut top);
                for c in std::mem::take(&mut t.children) {
                    if ids.contains(&c.id) {
                        moved.push(c);
                    } else {
                        t.children.push(c);
                    }
                }
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
        match parent.and_then(|p| self.nodes.iter_mut().find(|n| n.id == p)) {
            Some(f) => {
                Arc::make_mut(f).children.splice(at..at, moved);
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
    pub fn reassign_ids(&mut self, node: &mut Node) {
        node.id = self.alloc_id();
        for c in &mut node.children {
            self.reassign_ids(c);
        }
    }

    // ---------------------------------------------------------------- group

    /// Id semua node (level atas dan anak frame) yang group-nya ada di `groups`.
    pub fn group_members(&self, groups: &[u64]) -> Vec<u64> {
        let mut out = Vec::new();
        if groups.is_empty() {
            return out;
        }
        for n in &self.nodes {
            if n.group != 0 && groups.contains(&n.group) {
                out.push(n.id);
            }
            out.extend(n.children.iter().filter(|c| c.group != 0 && groups.contains(&c.group)).map(|c| c.id));
        }
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
        for top in &mut self.nodes {
            if top.children.iter().filter(|c| c.group == g).count() >= 2 {
                gather(&mut Arc::make_mut(top).children, |c| c.group == g);
            }
        }
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
        for top in &mut self.nodes {
            if top.children.iter().any(|c| ids.contains(&c.id)) {
                any |= clip_siblings(&mut Arc::make_mut(top).children, ids, |t| t);
            }
        }
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

    /// Frame level atas paling depan yang memuat titik ini.
    pub fn frame_at(&self, x: f32, y: f32, exclude: u64) -> Option<u64> {
        self.nodes
            .iter()
            .rev()
            .find(|n| n.kind == Kind::Frame && n.visible && n.id != exclude && n.contains(x, y, 0.0))
            .map(|n| n.id)
    }

    /// Node paling depan di titik ini (anak frame didahulukan dari frame-nya).
    pub fn hit(&self, x: f32, y: f32, tol: f32) -> Option<u64> {
        for n in self.nodes.iter().rev() {
            if !n.visible || n.locked {
                continue;
            }
            if n.kind == Kind::Frame {
                if n.contains(x, y, 0.0) {
                    for c in n.children.iter().rev() {
                        if c.visible && !c.locked && c.contains(x, y, tol) {
                            return Some(c.id);
                        }
                    }
                    return Some(n.id);
                }
            } else if n.contains(x, y, tol) {
                return Some(n.id);
            }
        }
        None
    }

    pub fn bounds(&self) -> Option<(f32, f32, f32, f32)> {
        let mut it = self.nodes.iter().map(|n| n.bounds());
        let first = it.next()?;
        Some(it.fold(first, |a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))))
    }

    pub fn count(&self) -> usize {
        self.nodes.iter().map(|n| n.count()).sum()
    }

    pub fn frame_count(&self) -> usize {
        self.nodes.iter().filter(|n| n.kind == Kind::Frame).count()
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

    #[test]
    fn base64_round_trip() {
        for data in [&b""[..], b"a", b"ab", b"abc", &[0u8, 255, 16, 128, 7]] {
            assert_eq!(b64::decode(&b64::encode(data)).unwrap(), data);
        }
    }

    #[test]
    fn closed_path_hit_test() {
        let mut n = Node::new(1, Kind::Path, 10.0, 10.0);
        n.w = 100.0;
        n.h = 100.0;
        let p = |x, y| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2] };
        n.points = vec![p(0.0, 0.0), p(1.0, 0.0), p(0.0, 1.0)];
        n.closed = true;
        n.fill = Some([0, 0, 0, 255]);
        assert!(n.contains(30.0, 30.0, 1.0));
        assert!(!n.contains(100.0, 100.0, 1.0));
        assert_eq!(n.path_points(8).len(), 3);
    }
}
