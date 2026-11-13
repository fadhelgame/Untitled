//! Pencarian foto stok: jendela kecil yang mencari di Openverse (foto berlisensi Creative Commons
//! dan domain publik, tanpa API key), menampilkan thumbnail, dan menaruh foto yang diklik ke canvas.
//!
//! Hanya foto yang boleh dipakai komersial dan boleh diubah yang dicari. Nama pembuat dan lisensinya
//! ikut disimpan di nama layer, karena kebanyakan lisensi CC mewajibkan atribusi.
//!
//! Tanpa akun, Openverse membatasi 20 pencarian per menit dan 200 per hari. Semua akses jaringan
//! jalan di thread lain; hasilnya dikirim lewat channel dan diambil tiap frame.

use super::*;
use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const API: &str = "https://api.openverse.org/v1/images/";
/// Hasil per halaman (batas untuk pemakaian tanpa akun).
const PAGE: usize = 20;
/// Ukuran kotak thumbnail di kisi (poin layar).
const TILE: Vec2 = vec2(124.0, 93.0);
/// Pengunduh thumbnail yang jalan bersamaan.
const WORKERS: usize = 4;
const MAX_THUMB: u64 = 2 * 1024 * 1024;
/// Batas unduhan foto ukuran penuh.
const MAX_PHOTO: u64 = 40 * 1024 * 1024;
/// Foto yang lebih besar dari ini dikecilkan dulu supaya file dokumen tidak membengkak.
const MAX_SIDE: u32 = 2560;

struct Entry {
    title: String,
    creator: String,
    /// Mis. "CC BY-SA 2.0".
    license: String,
    url: String,
    thumb: String,
    size: (u32, u32),
    tex: Option<egui::TextureHandle>,
    failed: bool,
}

impl Entry {
    /// Atribusi singkat: judul, pembuat, lisensi.
    fn credit(&self) -> String {
        let by = if self.creator.is_empty() { String::new() } else { format!(" by {}", self.creator) };
        format!("{}{by} ({})", self.title, self.license)
    }
}

enum Msg {
    /// Satu halaman hasil pencarian, plus jumlah halaman yang tersedia.
    Found(u64, Result<(Vec<Entry>, u32), String>),
    Thumb(u64, usize, Option<egui::ColorImage>),
    /// Foto ukuran penuh selesai diunduh: (byte file, atribusi).
    Photo(Result<(Vec<u8>, String), String>),
}

pub(super) struct Photos {
    pub open: bool,
    pub query: String,
    /// Minta fokus ke kotak pencarian di frame berikutnya.
    pub focus: bool,
    searching: bool,
    /// Foto ukuran penuh sedang diunduh.
    downloading: bool,
    note: String,
    entries: Vec<Entry>,
    /// Kata kunci dan halaman terakhir dari hasil yang sedang tampil.
    shown: String,
    page: u32,
    pages: u32,
    /// Nomor pencarian yang sedang berlaku; pekerja pencarian lama berhenti saat nomornya berubah.
    gen: Arc<AtomicU64>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
}

impl Default for Photos {
    fn default() -> Self {
        let (tx, rx) = mpsc::channel();
        Photos {
            open: false,
            query: String::new(),
            focus: false,
            searching: false,
            downloading: false,
            note: String::new(),
            entries: Vec::new(),
            shown: String::new(),
            page: 0,
            pages: 0,
            gen: Default::default(),
            tx,
            rx,
        }
    }
}

impl Photos {
    /// Masih ada pencarian, thumbnail, atau unduhan foto yang ditunggu.
    pub fn busy(&self) -> bool {
        self.searching || self.downloading || self.entries.iter().any(|e| e.tex.is_none() && !e.failed)
    }

    /// Pencarian baru dari kotak pencarian.
    pub fn search(&mut self, ctx: &egui::Context) {
        let query = self.query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.gen.fetch_add(1, Ordering::SeqCst);
        self.entries.clear();
        self.shown = query;
        self.pages = 0;
        self.request(1, ctx);
    }

    /// Halaman berikutnya dari pencarian yang sedang tampil.
    fn more(&mut self, ctx: &egui::Context) {
        if !self.searching && self.page < self.pages {
            self.request(self.page + 1, ctx);
        }
    }

    fn request(&mut self, page: u32, ctx: &egui::Context) {
        self.note.clear();
        self.searching = true;
        let gen = self.gen.load(Ordering::SeqCst);
        let (tx, ctx, query) = (self.tx.clone(), ctx.clone(), self.shown.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Found(gen, find(&query, page).map(|(entries, pages)| (entries, pages.max(page)))));
            ctx.request_repaint();
        });
        self.page = page;
    }

    fn fetch_thumbs(&self, queue: VecDeque<(usize, String)>, gen: u64, ctx: &egui::Context) {
        let queue = Arc::new(Mutex::new(queue));
        let agent = agent();
        for _ in 0..WORKERS {
            let (queue, agent, tx, ctx, current) = (queue.clone(), agent.clone(), self.tx.clone(), ctx.clone(), self.gen.clone());
            std::thread::spawn(move || loop {
                if current.load(Ordering::SeqCst) != gen {
                    return;
                }
                let Some((i, url)) = queue.lock().ok().and_then(|mut q| q.pop_front()) else { return };
                let image = download(&agent, &url, MAX_THUMB).ok().and_then(|bytes| thumbnail(&bytes));
                if tx.send(Msg::Thumb(gen, i, image)).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(15)).timeout(Duration::from_secs(60)).user_agent("kanvas/0.1").build()
}

fn find(query: &str, page: u32) -> Result<(Vec<Entry>, u32), String> {
    let response = agent()
        .get(API)
        .query("q", query)
        .query("page", &page.to_string())
        .query("page_size", &PAGE.to_string())
        // Hanya lisensi yang mengizinkan pemakaian komersial dan pengubahan.
        .query("license_type", "commercial,modification")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(429, _) => "too many searches. Openverse allows 20 per minute and 200 per day; try again later.".to_owned(),
            ureq::Error::Status(code, _) => format!("the photo service answered {code}"),
            other => format!("couldn't reach the photo service ({other})"),
        })?;
    let body = response.into_string().map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().trim().to_owned();
    let entries = json["results"]
        .as_array()
        .map(|a| a.as_slice())
        .unwrap_or_default()
        .iter()
        .filter_map(|r| {
            let (url, thumb) = (text(&r["url"]), text(&r["thumbnail"]));
            // Alamat dari server tidak dipercaya begitu saja: hanya HTTPS, thumbnail hanya dari Openverse.
            if !url.starts_with("https://") || !thumb.starts_with("https://api.openverse.org/") {
                return None;
            }
            let title = Some(text(&r["title"])).filter(|t| !t.is_empty()).unwrap_or_else(|| "Untitled photo".into());
            let dim = |key: &str| r[key].as_u64().unwrap_or(0) as u32;
            Some(Entry {
                title: title.chars().take(80).collect(),
                creator: text(&r["creator"]).chars().take(60).collect(),
                license: license_label(&text(&r["license"]), &text(&r["license_version"])),
                url,
                thumb,
                size: (dim("width"), dim("height")),
                tex: None,
                failed: false,
            })
        })
        .collect();
    Ok((entries, json["page_count"].as_u64().unwrap_or(1) as u32))
}

/// Kode lisensi Openverse ("by-sa", "2.0") → nama yang biasa dipakai ("CC BY-SA 2.0").
fn license_label(code: &str, version: &str) -> String {
    let name = match code {
        "pdm" => return "Public Domain".into(),
        "cc0" => "CC0".to_owned(),
        other => format!("CC {}", other.to_uppercase()),
    };
    if version.is_empty() {
        name
    } else {
        format!("{name} {version}")
    }
}

fn download(agent: &ureq::Agent, url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let response = agent.get(url).call().map_err(|e| match e {
        ureq::Error::Status(code, _) => format!("the server answered {code}"),
        other => other.to_string(),
    })?;
    let mut bytes = Vec::new();
    response.into_reader().take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err("the file is too large".into());
    }
    Ok(bytes)
}

fn thumbnail(bytes: &[u8]) -> Option<egui::ColorImage> {
    // 2× ukuran kotak supaya tajam di layar HiDPI.
    let small = image::load_from_memory(bytes).ok()?.thumbnail((TILE.x * 2.0) as u32, (TILE.y * 2.0) as u32).to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([small.width() as usize, small.height() as usize], small.as_raw()))
}

/// Unduh foto ukuran penuh; yang terlalu besar dikecilkan dan disimpan ulang sebagai JPEG.
fn fetch_photo(url: &str) -> Result<Vec<u8>, String> {
    let bytes = download(&agent(), url, MAX_PHOTO)?;
    let image = image::load_from_memory(&bytes).map_err(|_| "the file isn't an image Kanvas can read".to_owned())?;
    if image.width().max(image.height()) <= MAX_SIDE {
        return Ok(bytes);
    }
    let small = image.resize(MAX_SIDE, MAX_SIDE, image::imageops::FilterType::Triangle).to_rgb8();
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90).encode_image(&small).map_err(|e| e.to_string())?;
    Ok(out)
}

/// Gambar tombol toolbar: bingkai foto dengan gunung dan matahari.
pub(super) fn toolbar_icon(p: &egui::Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
    p.rect_stroke(Rect::from_center_size(c, vec2(17.0, 14.0)), CornerRadius::same(2), s, StrokeKind::Middle);
    p.circle_filled(at(-3.8, -2.6), 1.6, color);
    p.add(egui::Shape::line(vec![at(-8.0, 5.0), at(-2.5, 0.5), at(1.0, 3.5), at(4.0, 0.0), at(8.0, 4.0)], s));
}

impl App {
    pub(super) fn toggle_photos(&mut self) {
        self.photos.open = !self.photos.open;
        self.photos.focus = self.photos.open;
    }

    /// Tiap frame: ambil hasil pencarian, thumbnail, dan foto yang selesai diunduh.
    pub(super) fn photos_poll(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.photos.rx.try_recv() {
            let photos = &mut self.photos;
            let current = photos.gen.load(Ordering::SeqCst);
            match msg {
                Msg::Found(gen, result) if gen == current => {
                    photos.searching = false;
                    match result {
                        Ok((found, pages)) => {
                            photos.pages = pages;
                            let first = photos.entries.len();
                            let queue = found.iter().enumerate().map(|(i, e)| (first + i, e.thumb.clone())).collect();
                            photos.entries.extend(found);
                            if photos.entries.is_empty() {
                                photos.note = "No photos found. Try another word (in English).".into();
                            }
                            photos.fetch_thumbs(queue, gen, ctx);
                        }
                        Err(e) => photos.note = format!("Search failed: {e}"),
                    }
                }
                Msg::Thumb(gen, i, image) if gen == current => {
                    let Some(entry) = photos.entries.get_mut(i) else { continue };
                    match image {
                        Some(image) => entry.tex = Some(ctx.load_texture(format!("photo-{gen}-{i}"), image, egui::TextureOptions::LINEAR)),
                        None => entry.failed = true,
                    }
                }
                // Unduhan foto tetap dipasang walaupun pencarian sudah berganti.
                Msg::Photo(result) => {
                    photos.downloading = false;
                    match result {
                        Ok((bytes, credit)) => {
                            if self.add_image(bytes, &credit, None) {
                                self.status = format!("Added photo: {credit}. The credit is kept in the layer name.");
                            }
                        }
                        Err(e) => self.status = format!("Couldn't download the photo: {e}"),
                    }
                }
                _ => {}
            }
        }
    }

    /// Jendela pencarian foto.
    pub(super) fn photos_window(&mut self, ctx: &egui::Context) {
        if !self.photos.open {
            return;
        }
        let mut open = true;
        let (mut go, mut more) = (false, false);
        let mut pick = None;
        egui::Window::new("Photos")
            .open(&mut open)
            .collapsible(false)
            .default_pos(self.canvas_rect.left_top() + vec2(40.0, 40.0))
            // Ukuran tetap: isi yang mengikuti lebar jendela membuat jendela egui terus melebar.
            .resizable(false)
            .fixed_size([412.0, 430.0])
            .show(ctx, |ui| {
                let photos = &mut self.photos;
                ui.horizontal(|ui| {
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut photos.query)
                            .hint_text("Search photos: mountain, coffee, office…")
                            .desired_width(ui.available_width() - 72.0),
                    );
                    if std::mem::take(&mut photos.focus) {
                        edit.request_focus();
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                    go = ui.add(egui::Button::new("Search").min_size(vec2(64.0, 0.0))).clicked() || enter;
                });
                ui.add_space(4.0);
                if photos.downloading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Downloading photo…");
                    });
                } else if photos.searching {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak("Searching…");
                    });
                } else if !photos.note.is_empty() {
                    ui.weak(&photos.note);
                } else if photos.entries.is_empty() {
                    ui.weak("Search free-to-use photos from Openverse. Click one to add it to the canvas.");
                } else {
                    ui.weak("Click a photo to add it. Hover to see its author and license.");
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for (i, entry) in photos.entries.iter().enumerate() {
                            if entry.failed {
                                continue;
                            }
                            let (rect, resp) = ui.allocate_exact_size(TILE, Sense::click());
                            ui.painter().rect_filled(rect, CornerRadius::same(6), Color32::from_gray(58));
                            if let Some(tex) = &entry.tex {
                                // Isi kotak penuh: potong sisi thumbnail yang berlebih.
                                let size = tex.size_vec2();
                                let k = (size.x / size.y) / (TILE.x / TILE.y);
                                let (cut_x, cut_y) = if k > 1.0 { ((1.0 - 1.0 / k) * 0.5, 0.0) } else { (0.0, (1.0 - k) * 0.5) };
                                let uv = Rect::from_min_max(pos2(cut_x, cut_y), pos2(1.0 - cut_x, 1.0 - cut_y));
                                ui.painter().image(tex.id(), rect, uv, Color32::WHITE);
                            }
                            if resp.hovered() {
                                ui.painter().rect_stroke(rect, CornerRadius::ZERO, Stroke::new(2.0_f32, ACCENT), StrokeKind::Inside);
                            }
                            let resp = resp.on_hover_text(format!("{}\n{} × {} px", entry.credit(), entry.size.0, entry.size.1));
                            if resp.clicked() && !photos.downloading {
                                pick = Some(i);
                            }
                        }
                    });
                    if !photos.entries.is_empty() && photos.page < photos.pages {
                        ui.add_space(6.0);
                        more = ui.add_enabled(!photos.searching, egui::Button::new("Load more").min_size(vec2(ui.available_width(), 28.0))).clicked();
                    }
                });
            });
        self.photos.open = open;
        if go {
            self.photos.search(ctx);
        }
        if more {
            self.photos.more(ctx);
        }
        if let Some(i) = pick {
            self.add_photo(i, ctx);
        }
    }

    /// Unduh foto hasil pencarian (di thread lain), lalu taruh di tengah canvas.
    pub(super) fn add_photo(&mut self, index: usize, ctx: &egui::Context) {
        let photos = &mut self.photos;
        let Some(entry) = photos.entries.get(index) else { return };
        if photos.downloading {
            return;
        }
        photos.downloading = true;
        let (tx, ctx, url, credit) = (photos.tx.clone(), ctx.clone(), entry.url.clone(), entry.credit());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Photo(fetch_photo(&url).map(|bytes| (bytes, credit))));
            ctx.request_repaint();
        });
        self.status = "Downloading photo…".into();
    }

    /// Mode `--bench`: foto pertama yang thumbnail-nya sudah siap, untuk menguji penyisipan.
    pub(super) fn first_ready_photo(&self) -> Option<usize> {
        self.photos.entries.iter().position(|e| e.tex.is_some())
    }
}
