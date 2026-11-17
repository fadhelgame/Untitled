//! Pencarian icon: jendela kecil yang mencari di Iconify (kumpulan set icon open source, tanpa
//! API key), menampilkan hasilnya sebagai kisi thumbnail, dan menaruh icon yang diklik ke canvas
//! sebagai gambar SVG (tetap tajam di zoom berapa pun, bisa diwarnai ulang lewat Fill).
//!
//! Semua akses jaringan jalan di thread lain; hasilnya dikirim lewat channel dan diambil tiap frame.

use super::*;
use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const API: &str = "https://api.iconify.design";
/// Jumlah hasil per pencarian.
const LIMIT: usize = 96;
/// Sisi kotak thumbnail di kisi (poin layar).
const TILE: f32 = 52.0;
/// Sisi thumbnail yang dirasterkan (piksel); 2× isi kotak supaya tajam di layar HiDPI.
const THUMB: u32 = 72;
/// Pengunduh yang jalan bersamaan (satu permintaan per set icon).
const WORKERS: usize = 3;
/// Sisi terpanjang icon saat ditaruh di canvas (satuan dokumen).
const INSERT_SIDE: f32 = 96.0;
/// Batas ukuran jawaban untuk satu set icon.
const MAX_SET: u64 = 4 * 1024 * 1024;

struct Entry {
    /// `prefix:nama`, mis. `mdi:home`.
    id: String,
    /// Nama set dan lisensinya, untuk tooltip.
    set: String,
    svg: Option<Vec<u8>>,
    tex: Option<egui::TextureHandle>,
    failed: bool,
}

enum Msg {
    /// Hasil pencarian: daftar (id, keterangan set).
    Found(u64, Result<Vec<(String, String)>, String>),
    /// Satu icon selesai diunduh dan dirasterkan (None = gagal).
    Icon(u64, usize, Option<(Vec<u8>, egui::ColorImage)>),
}

pub(super) struct Icons {
    pub open: bool,
    pub query: String,
    /// Minta fokus ke kotak pencarian di frame berikutnya.
    pub focus: bool,
    searching: bool,
    note: String,
    entries: Vec<Entry>,
    /// Nomor pencarian yang sedang berlaku; pekerja pencarian lama berhenti saat nomornya berubah.
    gen: Arc<AtomicU64>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

impl Default for Icons {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Icons { open: false, query: String::new(), focus: false, searching: false, note: String::new(), entries: Vec::new(), gen: Default::default(), tx, rx }
    }
}

impl Icons {
    /// Masih ada pencarian atau thumbnail yang ditunggu.
    pub fn busy(&self) -> bool {
        self.searching || self.entries.iter().any(|e| e.tex.is_none() && !e.failed)
    }

    pub fn search(&mut self, ctx: &egui::Context) {
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let gen = self.gen.fetch_add(1, Ordering::SeqCst) + 1;
        self.entries.clear();
        self.note.clear();
        self.searching = true;
        let (tx, ctx) = (self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Found(gen, find(&query)));
            ctx.request_repaint();
        });
    }

    /// Tiap frame: ambil hasil pencarian dan thumbnail yang sudah selesai.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.rx.try_recv() {
            let current = self.gen.load(Ordering::SeqCst);
            match msg {
                Msg::Found(gen, result) if gen == current => {
                    self.searching = false;
                    match result {
                        Ok(found) if found.is_empty() => self.note = "No icons found. Try another word (in English).".into(),
                        Ok(found) => {
                            let queue: VecDeque<(usize, String)> = found.iter().enumerate().map(|(i, f)| (i, f.0.clone())).collect();
                            self.entries = found.into_iter().map(|(id, set)| Entry { id, set, svg: None, tex: None, failed: false }).collect();
                            self.fetch_all(queue, gen, ctx);
                        }
                        Err(e) => self.note = format!("Search failed: {e}"),
                    }
                }
                Msg::Icon(gen, i, icon) if gen == current => {
                    let Some(entry) = self.entries.get_mut(i) else { continue };
                    match icon {
                        Some((svg, image)) => {
                            entry.tex = Some(ctx.load_texture(format!("icon-{gen}-{i}"), image, egui::TextureOptions::LINEAR));
                            entry.svg = Some(svg);
                        }
                        None => entry.failed = true,
                    }
                }
                _ => {}
            }
        }
    }

    /// Unduh icon per set: satu permintaan untuk semua icon dari set yang sama, supaya jumlah
    /// permintaan kecil (layanan ini membatasi klien yang terlalu sering meminta).
    fn fetch_all(&self, queue: VecDeque<(usize, String)>, gen: u64, ctx: &egui::Context) {
        let mut sets: Vec<(String, Vec<(usize, String)>)> = Vec::new();
        for (i, id) in queue {
            let Some((prefix, name)) = id.split_once(':') else { continue };
            match sets.iter_mut().find(|s| s.0 == prefix) {
                Some(set) => set.1.push((i, name.to_owned())),
                None => sets.push((prefix.to_owned(), vec![(i, name.to_owned())])),
            }
        }
        let sets = Arc::new(Mutex::new(VecDeque::from(sets)));
        let agent = agent();
        for _ in 0..WORKERS {
            let (sets, agent, tx, ctx, current) = (sets.clone(), agent.clone(), self.tx.clone(), ctx.clone(), self.gen.clone());
            std::thread::spawn(move || loop {
                if current.load(Ordering::SeqCst) != gen {
                    return;
                }
                let Some((prefix, icons)) = sets.lock().ok().and_then(|mut q| q.pop_front()) else { return };
                let names: Vec<&str> = icons.iter().map(|i| i.1.as_str()).collect();
                let data = fetch_set(&agent, &prefix, &names);
                for (i, name) in &icons {
                    let icon = data.as_ref().and_then(|d| build_svg(d, name)).and_then(|svg| thumbnail(&svg).map(|image| (svg, image)));
                    if tx.send(Msg::Icon(gen, *i, icon)).is_err() {
                        return;
                    }
                }
                ctx.request_repaint();
            });
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent("kanvas/0.1").build()
}

/// Bagian nama icon dari server tidak dipercaya begitu saja: hanya huruf kecil, angka, dan tanda hubung.
fn valid_part(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn find(query: &str) -> Result<Vec<(String, String)>, String> {
    let response = agent()
        .get(&format!("{API}/search"))
        .query("query", query)
        .query("limit", &LIMIT.to_string())
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("the icon service answered {code}"),
            other => format!("couldn't reach the icon service ({other})"),
        })?;
    let body = response.into_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let sets = &json["collections"];
    let icons = json["icons"].as_array().map(|a| a.as_slice()).unwrap_or_default();
    Ok(icons
        .iter()
        .filter_map(|v| v.as_str())
        .filter(|id| id.split_once(':').is_some_and(|(p, n)| valid_part(p) && valid_part(n)))
        .map(|id| {
            let set = &sets[id.split_once(':').map_or("", |s| s.0)];
            let label = match (set["name"].as_str(), set["license"]["title"].as_str()) {
                (Some(name), Some(license)) => format!("{name} · {license}"),
                (Some(name), None) => name.to_owned(),
                _ => String::new(),
            };
            (id.to_owned(), label)
        })
        .collect())
}

/// Data beberapa icon dari satu set (format JSON Iconify: `icons`, `aliases`, ukuran bawaan).
fn fetch_set(agent: &ureq::Agent, prefix: &str, names: &[&str]) -> Option<serde_json::Value> {
    let response = agent.get(&format!("{API}/{prefix}.json")).query("icons", &names.join(",")).call().ok()?;
    let mut body = String::new();
    response.into_reader().take(MAX_SET).read_to_string(&mut body).ok()?;
    serde_json::from_str(&body).ok()
}

/// Susun SVG utuh untuk satu icon dari data set-nya. Nama bisa berupa alias icon lain
/// (kadang dicerminkan); alias yang diputar dilewati.
fn build_svg(set: &serde_json::Value, name: &str) -> Option<Vec<u8>> {
    let mut name = name.to_owned();
    let (mut hflip, mut vflip) = (false, false);
    for _ in 0..6 {
        if let Some(icon) = set["icons"].get(&name) {
            let body = icon["body"].as_str()?;
            let num = |key: &str, default: f64| icon[key].as_f64().or_else(|| set[key].as_f64()).unwrap_or(default);
            let (left, top, w, h) = (num("left", 0.0), num("top", 0.0), num("width", 16.0), num("height", 16.0));
            if !(w > 0.0 && h > 0.0) {
                return None;
            }
            let body = if hflip || vflip {
                let (sx, tx) = if hflip { (-1, w + 2.0 * left) } else { (1, 0.0) };
                let (sy, ty) = if vflip { (-1, h + 2.0 * top) } else { (1, 0.0) };
                format!("<g transform=\"translate({tx} {ty}) scale({sx} {sy})\">{body}</g>")
            } else {
                body.to_owned()
            };
            let svg = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{w}\" height=\"{h}\" viewBox=\"{left} {top} {w} {h}\">{body}</svg>"
            );
            return Some(svg.into_bytes());
        }
        let alias = set["aliases"].get(&name)?;
        if alias["rotate"].as_i64().unwrap_or(0) != 0 {
            return None;
        }
        hflip ^= alias["hFlip"].as_bool().unwrap_or(false);
        vflip ^= alias["vFlip"].as_bool().unwrap_or(false);
        name = alias["parent"].as_str()?.to_owned();
    }
    None
}

/// Rasterkan SVG ke thumbnail persegi, icon di tengah.
fn thumbnail(svg: &[u8]) -> Option<egui::ColorImage> {
    use resvg::tiny_skia::{Pixmap, Transform};
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = THUMB as f32 / size.width().max(size.height()).max(0.001);
    let (dx, dy) = ((THUMB as f32 - size.width() * scale) * 0.5, (THUMB as f32 - size.height() * scale) * 0.5);
    let mut pixmap = Pixmap::new(THUMB, THUMB)?;
    resvg::render(&tree, Transform::from_scale(scale, scale).post_translate(dx, dy), &mut pixmap.as_mut());
    Some(egui::ColorImage::from_rgba_premultiplied([THUMB as usize, THUMB as usize], pixmap.data()))
}

/// Gambar tombol toolbar: empat bentuk kecil.
pub(super) fn toolbar_icon(p: &egui::Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
    p.circle_stroke(at(-4.5, -4.5), 3.2, s);
    p.rect_stroke(Rect::from_center_size(at(4.5, -4.5), vec2(6.4, 6.4)), CornerRadius::same(1), s, StrokeKind::Middle);
    p.add(egui::Shape::closed_line(vec![at(-4.5, 1.2), at(-8.0, 7.6), at(-1.0, 7.6)], s));
    p.add(egui::Shape::closed_line(vec![at(4.5, 0.8), at(8.0, 4.5), at(4.5, 8.2), at(1.0, 4.5)], s));
}

impl App {
    pub(super) fn toggle_icons(&mut self) {
        self.icons.open = !self.icons.open;
        self.icons.focus = self.icons.open;
    }

    /// Jendela pencarian icon.
    pub(super) fn icons_window(&mut self, ctx: &egui::Context) {
        if !self.icons.open {
            return;
        }
        let mut open = true;
        let mut go = false;
        let mut pick = None;
        egui::Window::new("Icons")
            .open(&mut open)
            .collapsible(false)
            .default_pos(self.canvas_rect.left_top() + vec2(16.0, 16.0))
            // Ukuran tetap: isi yang mengikuti lebar jendela membuat jendela egui terus melebar.
            .resizable(false)
            .fixed_size([412.0, 430.0])
            .show(ctx, |ui| {
                let icons = &mut self.icons;
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut icons.query)
                            .hint_text("Search icons: home, arrow, heart…")
                            .desired_width(ui.available_width() - 72.0),
                    );
                    if std::mem::take(&mut icons.focus) {
                        edit.request_focus();
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    go = ui.add(egui::Button::new("Search").min_size(vec2(64.0, 0.0))).clicked() || enter;
                });
                ui.add_space(4.0);
                if icons.searching {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Searching…");
                    });
                } else if !icons.note.is_empty() {
                    ui.weak(&icons.note);
                } else if !icons.entries.is_empty() && icons.entries.iter().all(|e| e.failed) {
                    ui.weak("Couldn't load these icons. The icon service may be busy; try again in a minute.");
                } else if icons.entries.is_empty() {
                    ui.weak("Search 200,000+ open-source icons from Iconify. Click one to add it to the canvas.");
                } else {
                    ui.weak("Click an icon to add it. Hover to see its set and license.");
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for (i, entry) in icons.entries.iter().enumerate() {
                            if entry.failed {
                                continue;
                            }
                            let (rect, resp) = ui.allocate_exact_size(vec2(TILE, TILE), Sense::click());
                            // Kotak terang: kebanyakan icon berwarna hitam (`currentColor`).
                            let fill = if resp.hovered() { Color32::WHITE } else { Color32::from_gray(232) };
                            ui.painter().rect_filled(rect, CornerRadius::same(10), fill);
                            if resp.hovered() {
                                ui.painter().rect_stroke(rect, CornerRadius::same(10), Stroke::new(2.0_f32, ACCENT), StrokeKind::Inside);
                            }
                            match &entry.tex {
                                Some(tex) => {
                                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                                    ui.painter().image(tex.id(), rect.shrink(8.0), uv, Color32::WHITE);
                                }
                                None => {
                                    ui.painter().circle_filled(rect.center(), 3.0, Color32::from_gray(180));
                                }
                            }
                            let resp = resp.on_hover_text(if entry.set.is_empty() { entry.id.clone() } else { format!("{}\n{}", entry.id, entry.set) });
                            if resp.clicked() && entry.svg.is_some() {
                                pick = Some(i);
                            }
                        }
                    });
                });
            });
        self.icons.open = open;
        if go {
            self.icons.search(ctx);
        }
        if let Some(i) = pick {
            self.add_icon(i);
        }
    }

    /// Taruh icon hasil pencarian ke tengah canvas.
    pub(super) fn add_icon(&mut self, index: usize) {
        let Some((id, Some(svg))) = self.icons.entries.get(index).map(|e| (e.id.clone(), e.svg.clone())) else { return };
        // As editable vectors, one group; an icon that cannot be converted comes in as an image.
        if self.add_svg(&svg, &id, None, Some(INSERT_SIDE)).is_ok() {
            self.status = format!("Added icon \"{id}\" as editable vectors. Recolor it with Fill or Stroke.");
            return;
        }
        if !self.add_image(svg, &id, None) {
            return;
        }
        // Ukuran asli icon biasanya hanya 24 satuan; perbesar supaya langsung terlihat.
        if let Some(n) = self.sel.first().copied().and_then(|id| self.doc.find_mut(id)) {
            let k = INSERT_SIDE / n.w.max(n.h).max(1.0);
            let (cx, cy) = (n.x + n.w * 0.5, n.y + n.h * 0.5);
            n.w = (n.w * k).round().max(1.0);
            n.h = (n.h * k).round().max(1.0);
            n.x = (cx - n.w * 0.5).round();
            n.y = (cy - n.h * 0.5).round();
        }
        self.status = format!("Added icon \"{id}\". Use Fill to recolor it.");
    }

    /// Mode `--bench`: icon pertama yang sudah siap, untuk menguji penyisipan.
    pub(super) fn first_ready_icon(&self) -> Option<usize> {
        self.icons.entries.iter().position(|e| e.svg.is_some())
    }
}
