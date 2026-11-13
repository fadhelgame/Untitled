//! Google Fonts on-demand: katalog diambil sekali (di-cache di disk), file font
//! baru diunduh saat pertama kali dipakai, lalu disimpan untuk pemakaian offline.
//! Untuk preview di daftar font, yang diunduh hanya potongan kecil (±5 KB) berisi
//! huruf-huruf nama font itu sendiri.

use eframe::egui::{self, FontData, FontDefinitions, FontFamily};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

const WORKERS: usize = 6;

pub struct Family {
    pub name: String,
    pub lower: String,
    pub category: String,
    /// (weight, italic)
    pub variants: Vec<(u16, bool)>,
}

pub enum CatalogState {
    Loading,
    Ready,
    Failed(String),
}

enum State {
    Loading,
    Ready { frame: u64 },
    Failed(String),
}

enum Msg {
    Catalog(Result<Vec<Family>, String>),
    Font { key: String, result: Result<Vec<u8>, String> },
}

struct Job {
    key: String,
    family: String,
    /// `None` = potongan preview; `Some((weight, italic))` = font lengkap.
    variant: Option<(u16, bool)>,
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    ready: Condvar,
}

pub struct FontManager {
    pub catalog: Vec<Family>,
    pub catalog_state: CatalogState,
    defs: FontDefinitions,
    state: HashMap<String, State>,
    queue: Arc<Queue>,
    tx: mpsc::Sender<Msg>,
    rx: mpsc::Receiver<Msg>,
    dir: PathBuf,
    frame: u64,
}

pub fn key(family: &str, weight: u16, italic: bool) -> String {
    format!("{family}:{weight}{}", if italic { "i" } else { "" })
}

pub fn weight_name(w: u16) -> &'static str {
    match w {
        100 => "Thin",
        200 => "ExtraLight",
        300 => "Light",
        400 => "Regular",
        500 => "Medium",
        600 => "SemiBold",
        700 => "Bold",
        800 => "ExtraBold",
        900 => "Black",
        _ => "Custom",
    }
}

pub fn cache_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(p).join("Kanvas");
    }
    if let Some(p) = std::env::var_os("HOME") {
        return PathBuf::from(p).join(".cache").join("kanvas");
    }
    std::env::temp_dir().join("kanvas")
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(40))
        .user_agent("kanvas/0.1")
        .build()
}

/// Nama family dari file dokumen tidak dipercaya begitu saja: hanya huruf, angka, spasi.
fn valid_family(name: &str) -> bool {
    !name.is_empty() && name.len() <= 80 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == ' ')
}

#[derive(Serialize, Deserialize)]
struct CachedFamily {
    n: String,
    c: String,
    v: Vec<String>,
}

#[derive(Deserialize)]
struct RawCatalog {
    #[serde(rename = "familyMetadataList")]
    list: Vec<RawFamily>,
}

#[derive(Deserialize)]
struct RawFamily {
    family: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    fonts: BTreeMap<String, serde::de::IgnoredAny>,
    #[serde(default)]
    popularity: Option<u32>,
}

fn to_families(list: Vec<CachedFamily>) -> Vec<Family> {
    list.into_iter()
        .map(|f| {
            let mut variants: Vec<(u16, bool)> = f
                .v
                .iter()
                .filter_map(|v| {
                    let italic = v.ends_with('i');
                    v.trim_end_matches('i').parse::<u16>().ok().map(|w| (w, italic))
                })
                .collect();
            variants.sort();
            if variants.is_empty() {
                variants.push((400, false));
            }
            Family { lower: f.n.to_lowercase(), name: f.n, category: f.c, variants }
        })
        .collect()
}

fn fetch_catalog(dir: &Path) -> Result<Vec<Family>, String> {
    let path = dir.join("catalog.json");
    let read_cache = || -> Option<Vec<CachedFamily>> {
        let bytes = std::fs::read(&path).ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    // Cache masih segar (< 7 hari): tidak perlu jaringan.
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < Duration::from_secs(7 * 24 * 3600));
    if fresh {
        if let Some(c) = read_cache() {
            return Ok(to_families(c));
        }
    }
    let online = || -> Result<Vec<CachedFamily>, String> {
        let body = agent()
            .get("https://fonts.google.com/metadata/fonts")
            .call()
            .map_err(|e| e.to_string())?
            .into_string()
            .map_err(|e| e.to_string())?;
        let json = body.trim_start_matches(")]}'").trim_start();
        let mut raw: RawCatalog = serde_json::from_str(json).map_err(|e| e.to_string())?;
        raw.list.sort_by_key(|f| f.popularity.unwrap_or(u32::MAX));
        Ok(raw
            .list
            .into_iter()
            .filter(|f| valid_family(&f.family))
            .map(|f| CachedFamily {
                n: f.family,
                c: f.category.unwrap_or_default(),
                v: f.fonts.into_keys().collect(),
            })
            .collect())
    };
    match online() {
        Ok(list) => {
            let _ = std::fs::create_dir_all(dir);
            if let Ok(bytes) = serde_json::to_vec(&list) {
                let _ = std::fs::write(&path, bytes);
            }
            Ok(to_families(list))
        }
        // Offline: pakai cache lama kalau ada.
        Err(e) => read_cache().map(to_families).ok_or(e),
    }
}

fn fetch_font(dir: &Path, family: &str, variant: Option<(u16, bool)>) -> Result<Vec<u8>, String> {
    if !valid_family(family) {
        return Err("invalid font name".into());
    }
    let file_base = family.replace(' ', "_");
    let fam = family.replace(' ', "+");
    let (path, css_url) = match variant {
        Some((weight, italic)) => (
            dir.join("fonts").join(format!("{file_base}-{weight}{}.ttf", if italic { "i" } else { "" })),
            if italic {
                format!("https://fonts.googleapis.com/css2?family={fam}:ital,wght@1,{weight}")
            } else {
                format!("https://fonts.googleapis.com/css2?family={fam}:wght@{weight}")
            },
        ),
        // Preview: hanya glyph yang dipakai untuk menulis nama font itu.
        None => (
            dir.join("previews").join(format!("{file_base}.ttf")),
            format!("https://fonts.googleapis.com/css2?family={fam}&text={}", family.replace(' ', "%20")),
        ),
    };
    if let Ok(bytes) = std::fs::read(&path) {
        if !bytes.is_empty() {
            return Ok(bytes);
        }
    }
    let agent = agent();
    let css = agent
        .get(&css_url)
        .call()
        .map_err(|e| format!("couldn't fetch the font: {e}"))?
        .into_string()
        .map_err(|e| e.to_string())?;
    let start = css.find("url(").ok_or("unrecognized Google Fonts response")? + 4;
    let len = css[start..].find(')').ok_or("unrecognized Google Fonts response")?;
    let font_url = css[start..start + len].trim_matches(|c| c == '"' || c == '\'');
    if !font_url.starts_with("https://fonts.gstatic.com/") {
        return Err("unrecognized font source".into());
    }
    let mut bytes = Vec::new();
    agent
        .get(font_url)
        .call()
        .map_err(|e| format!("couldn't download the font: {e}"))?
        .into_reader()
        .take(40 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if ttf_parser::Face::parse(&bytes, 0).is_err() {
        return Err("corrupt font file".into());
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, &bytes);
    Ok(bytes)
}

/// Nama family egui untuk judul UI (semibold).
pub const UI_STRONG: &str = "ui-strong";

/// Font antarmuka: sans-serif sistem (Segoe UI) di depan font bawaan egui, yang tetap
/// jadi cadangan untuk glyph ikon. Tanpa file sistem, UI memakai font bawaan saja.
fn ui_defs() -> FontDefinitions {
    let mut defs = FontDefinitions::default();
    let base = defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let dirs: Vec<std::path::PathBuf> = match std::env::var_os("WINDIR") {
        Some(w) => vec![std::path::Path::new(&w).join("Fonts")],
        None => vec!["/System/Library/Fonts".into(), "/usr/share/fonts/truetype/inter".into(), "/usr/share/fonts/inter".into()],
    };
    let mut load = |key: &str, files: &[&str]| -> bool {
        for (dir, file) in dirs.iter().flat_map(|d| files.iter().map(move |f| (d, f))) {
            if let Ok(bytes) = std::fs::read(dir.join(file)) {
                if ttf_parser::Face::parse(&bytes, 0).is_ok() {
                    defs.font_data.insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
                    return true;
                }
            }
        }
        false
    };
    let regular = load("ui-regular", &["segoeui.ttf", "Inter-Regular.ttf", "Inter-Regular.otf"]);
    let strong = load("ui-semibold", &["seguisb.ttf", "Inter-SemiBold.ttf", "Inter-SemiBold.otf"]);
    let mut body = base.clone();
    if regular {
        body.insert(0, "ui-regular".to_owned());
    }
    let mut head = body.clone();
    if strong {
        head.insert(0, "ui-semibold".to_owned());
    }
    defs.families.insert(FontFamily::Proportional, body);
    defs.families.insert(FontFamily::Name(UI_STRONG.into()), head);
    defs
}

impl FontManager {
    pub fn new(ctx: &egui::Context) -> Self {
        let defs = ui_defs();
        ctx.set_fonts(defs.clone());
        let (tx, rx) = mpsc::channel();
        let queue = Arc::new(Queue { jobs: Mutex::new(VecDeque::new()), ready: Condvar::new() });
        let dir = cache_dir();
        // Beberapa worker saja, supaya menggulir daftar font tidak membuat ratusan thread.
        for _ in 0..WORKERS {
            let (queue, tx, dir, ctx) = (queue.clone(), tx.clone(), dir.clone(), ctx.clone());
            std::thread::spawn(move || loop {
                let job = {
                    let mut jobs = queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        // Permintaan terbaru dulu: yang sedang terlihat di layar didahulukan.
                        if let Some(job) = jobs.pop_back() {
                            break job;
                        }
                        jobs = queue.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
                    }
                };
                let result = fetch_font(&dir, &job.family, job.variant);
                if tx.send(Msg::Font { key: job.key, result }).is_err() {
                    return;
                }
                ctx.request_repaint();
            });
        }
        let mut m = FontManager {
            catalog: Vec::new(),
            catalog_state: CatalogState::Loading,
            defs,
            state: HashMap::new(),
            queue,
            tx,
            rx,
            dir,
            frame: 0,
        };
        m.load_catalog(ctx);
        m
    }

    pub fn load_catalog(&mut self, ctx: &egui::Context) {
        self.catalog_state = CatalogState::Loading;
        let (tx, dir, ctx) = (self.tx.clone(), self.dir.clone(), ctx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(Msg::Catalog(fetch_catalog(&dir)));
            ctx.request_repaint();
        });
    }

    fn enqueue(&mut self, key: String, family: &str, variant: Option<(u16, bool)>) {
        self.state.insert(key.clone(), State::Loading);
        let mut jobs = self.queue.jobs.lock().unwrap_or_else(|e| e.into_inner());
        jobs.push_back(Job { key, family: family.to_owned(), variant });
        self.queue.ready.notify_one();
    }

    /// Dipanggil sekali di awal tiap frame: terima hasil unduhan dan daftarkan ke egui.
    pub fn poll(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        let mut changed = false;
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Catalog(Ok(list)) => {
                    self.catalog = list;
                    self.catalog_state = CatalogState::Ready;
                }
                Msg::Catalog(Err(e)) => self.catalog_state = CatalogState::Failed(e),
                Msg::Font { key, result: Err(e) } => {
                    self.state.insert(key, State::Failed(e));
                }
                Msg::Font { key, result: Ok(bytes) } => {
                    if ttf_parser::Face::parse(&bytes, 0).is_err() {
                        self.state.insert(key, State::Failed("corrupt font file".into()));
                        continue;
                    }
                    // Font bawaan egui jadi cadangan untuk glyph yang tidak ada (mis. emoji).
                    let mut chain = vec![key.clone()];
                    if let Some(def) = self.defs.families.get(&FontFamily::Proportional) {
                        chain.extend(def.iter().cloned());
                    }
                    self.defs.font_data.insert(key.clone(), Arc::new(FontData::from_owned(bytes)));
                    self.defs.families.insert(FontFamily::Name(key.as_str().into()), chain);
                    self.state.insert(key, State::Ready { frame: self.frame });
                    changed = true;
                }
            }
        }
        if changed {
            ctx.set_fonts(self.defs.clone());
        }
    }

    fn lookup(&mut self, k: String, family: &str, variant: Option<(u16, bool)>) -> Option<FontFamily> {
        match self.state.get(&k) {
            // `set_fonts` baru berlaku di frame berikutnya.
            Some(State::Ready { frame, .. }) if self.frame > *frame => Some(FontFamily::Name(k.as_str().into())),
            Some(_) => None,
            None => {
                self.enqueue(k, family, variant);
                None
            }
        }
    }

    /// Family egui untuk varian ini. Kalau belum ada, unduhan dimulai di latar
    /// belakang dan font bawaan dipakai sementara.
    pub fn resolve(&mut self, family: &str, weight: u16, italic: bool) -> FontFamily {
        self.lookup(key(family, weight, italic), family, Some((weight, italic)))
            .unwrap_or(FontFamily::Proportional)
    }

    /// Font kecil untuk menampilkan nama family dengan bentuk hurufnya sendiri.
    pub fn resolve_preview(&mut self, family: &str) -> Option<FontFamily> {
        self.lookup(format!("~{family}"), family, None)
    }

    /// Teks status untuk panel properti (None = siap dipakai).
    pub fn status(&self, family: &str, weight: u16, italic: bool) -> Option<String> {
        match self.state.get(&key(family, weight, italic)) {
            Some(State::Loading) | None => Some("downloading…".into()),
            Some(State::Failed(e)) => Some(format!("failed: {e}")),
            Some(State::Ready { .. }) => None,
        }
    }

    /// Unduhan varian ini sudah selesai: siap dipakai untuk layout, atau gagal.
    pub fn settled(&self, family: &str, weight: u16, italic: bool) -> bool {
        match self.state.get(&key(family, weight, italic)) {
            Some(State::Ready { frame }) => self.frame > *frame,
            Some(State::Failed(_)) => true,
            Some(State::Loading) | None => false,
        }
    }

    /// Masih ada font lengkap yang sedang diunduh (preview tidak dihitung).
    pub fn any_loading(&self) -> bool {
        self.state.iter().any(|(k, s)| k.contains(':') && matches!(s, State::Loading))
    }

    pub fn loaded_count(&self) -> usize {
        self.state.iter().filter(|(k, s)| k.contains(':') && matches!(s, State::Ready { .. })).count()
    }

    pub fn preview_count(&self) -> usize {
        self.state.iter().filter(|(k, s)| k.starts_with('~') && matches!(s, State::Ready { .. })).count()
    }

    pub fn family(&self, name: &str) -> Option<&Family> {
        self.catalog.iter().find(|f| f.name == name)
    }

    /// Byte file font untuk sebuah family egui (dipakai export untuk mengambil outline glyph).
    pub fn face_data(&self, family: &FontFamily) -> Option<&[u8]> {
        let name = match family {
            FontFamily::Name(key) => key.to_string(),
            other => self.defs.families.get(other)?.first()?.clone(),
        };
        self.defs.font_data.get(&name).map(|d| d.font.as_ref())
    }
}
