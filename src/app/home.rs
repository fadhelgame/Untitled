//! Halaman awal (daftar file terbaru), auto-save, dan konfirmasi saat menutup.

use super::{theme, App, Drag, ACCENT, CANVAS_BG};
use crate::doc::Doc;
use crate::export;
use eframe::egui::{self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Key, Modifiers, Rect, Sense, Stroke, StrokeKind};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_RECENT: usize = 40;
/// Jeda setelah perubahan terakhir sebelum file ditulis otomatis.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(1500);
/// Ukuran maksimum thumbnail (px).
const THUMB: (f32, f32) = (560.0, 320.0);

pub(super) fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APPDATA") {
        return PathBuf::from(p).join("Kanvas");
    }
    if let Some(p) = std::env::var_os("HOME") {
        return PathBuf::from(p).join(".config").join("kanvas");
    }
    std::env::temp_dir().join("kanvas")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn thumb_path(path: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.to_string_lossy().to_lowercase().hash(&mut h);
    data_dir().join("thumbs").join(format!("{:016x}.png", h.finish()))
}

fn ago(secs: u64) -> String {
    let unit = |n: u64, name: &str| format!("{n} {name}{} ago", if n == 1 { "" } else { "s" });
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => unit(secs / 60, "minute"),
        3600..=86_399 => unit(secs / 3600, "hour"),
        86_400..=2_591_999 => unit(secs / 86_400, "day"),
        2_592_000..=31_535_999 => unit(secs / 2_592_000, "month"),
        _ => unit(secs / 31_536_000, "year"),
    }
}

/// Tulis lewat file sementara lalu ganti nama, supaya file lama tidak rusak kalau penulisan terputus.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Write the document to `path`. The JSON is streamed into a temporary file that then replaces
/// the old one, so a large document is never held in memory twice and an interrupted save
/// leaves the old file intact. Safe to call from any thread.
pub(super) fn save_doc(doc: &Doc, path: &Path) -> Result<(), String> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let write = || -> Result<(), String> {
        let file = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
        let mut out = std::io::BufWriter::with_capacity(1 << 20, file);
        serde_json::to_writer(&mut out, doc).map_err(|e| e.to_string())?;
        drop(out.into_inner().map_err(|e| e.to_string())?);
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    };
    write().inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Read a document from `path`. Safe to call from any thread.
pub(super) fn read_doc(path: &Path) -> Result<Doc, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut doc: Doc = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    // New ids must not collide with the ids in the file.
    doc.next_id = doc.next_id.max(doc.max_node_id());
    doc.fix_pages();
    Ok(doc)
}

/// A save running on another thread.
struct Saving {
    path: PathBuf,
    /// `Files::version` of the document being written.
    version: u64,
    rx: mpsc::Receiver<Result<(), String>>,
}

/// A file being read and parsed on another thread.
struct Loading {
    path: PathBuf,
    rx: mpsc::Receiver<Result<Doc, String>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct Recent {
    path: PathBuf,
    /// Terakhir dibuka/disimpan (detik Unix); menentukan urutan.
    time: u64,
    /// Waktu ubah file di disk; None = file sudah tidak ada.
    #[serde(skip)]
    edited: Option<u64>,
}

pub(super) struct Files {
    ctx: egui::Context,
    recent: Vec<Recent>,
    thumbs: HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// Thumbnail file yang terbuka belum mengikuti isi terakhir.
    stale: bool,
    pub last_change: Instant,
    /// Goes up with every change to the document; tells whether a finished background save
    /// still matches what is on screen.
    pub version: u64,
    saving: Option<Saving>,
    /// Save as soon as possible (Ctrl+S), without waiting for the auto-save pause.
    asap: bool,
    loading: Option<Loading>,
    /// Pesan dialog "simpan dulu?" saat menutup aplikasi.
    prompt: Option<String>,
    closing: bool,
    /// Pesan kesalahan yang ditampilkan di halaman awal.
    note: String,
}

impl Files {
    pub fn new(ctx: &egui::Context) -> Self {
        let recent = std::fs::read(data_dir().join("recent.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Vec<Recent>>(&b).ok())
            .unwrap_or_default();
        let mut files = Files {
            ctx: ctx.clone(),
            recent,
            thumbs: HashMap::new(),
            stale: false,
            last_change: Instant::now(),
            version: 0,
            saving: None,
            asap: false,
            loading: None,
            prompt: None,
            closing: false,
            note: String::new(),
        };
        files.refresh();
        files
    }

    fn refresh(&mut self) {
        for r in &mut self.recent {
            r.edited = std::fs::metadata(&r.path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
        }
    }

    fn store(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(bytes) = serde_json::to_vec_pretty(&self.recent) {
            let _ = write_atomic(&dir.join("recent.json"), &bytes);
        }
    }

    /// Naikkan file ke urutan teratas daftar terbaru.
    fn touch(&mut self, path: &Path) {
        let time = now();
        self.recent.retain(|r| r.path != path);
        self.recent.insert(0, Recent { path: path.to_owned(), time, edited: Some(time) });
        for old in self.recent.drain(MAX_RECENT.min(self.recent.len())..) {
            let _ = std::fs::remove_file(thumb_path(&old.path));
        }
        self.store();
    }

    fn forget(&mut self, path: &Path) {
        self.recent.retain(|r| r.path != path);
        self.thumbs.remove(path);
        let _ = std::fs::remove_file(thumb_path(path));
        self.store();
    }

    fn texture(&mut self, path: &Path) -> Option<egui::TextureHandle> {
        if let Some(t) = self.thumbs.get(path) {
            return t.clone();
        }
        let tex = image::open(thumb_path(path)).ok().map(|img| {
            let img = img.to_rgba8();
            let size = [img.width() as usize, img.height() as usize];
            let color = egui::ColorImage::from_rgba_unmultiplied(size, &img);
            self.ctx.load_texture(format!("thumb:{}", path.display()), color, egui::TextureOptions::LINEAR)
        });
        self.thumbs.insert(path.to_owned(), tex.clone());
        tex
    }
}

enum Act {
    None,
    New,
    Open,
    Back,
    Load(PathBuf),
    Forget(PathBuf),
}

impl App {
    // ------------------------------------------------------------ simpan

    /// Tulis dokumen ke `path` dan catat sebagai file terbaru.
    /// Blocks until the file is written; an unfinished background save is waited for first, so
    /// two writers never share the temporary file.
    pub(super) fn write_doc(&mut self, path: &Path) -> Result<(), String> {
        self.save_wait();
        save_doc(&self.doc, path)?;
        self.files.asap = false;
        self.path = Some(path.to_owned());
        self.dirty = false;
        self.files.stale = true;
        self.files.touch(path);
        Ok(())
    }

    /// Dipanggil setelah file berhasil dibuka.
    pub(super) fn opened(&mut self, path: &Path) {
        self.files.stale = !thumb_path(path).exists();
        self.files.touch(path);
        self.files.note.clear();
        self.home = false;
    }

    /// Simpan otomatis file yang sudah punya lokasi, sesaat setelah pengguna berhenti mengedit.
    /// The file is written on another thread (see `start_save`), so a large document does not
    /// freeze the canvas; only one save runs at a time.
    pub(super) fn autosave(&mut self, ctx: &egui::Context) {
        if !self.dirty || self.bench.is_some() || self.files.saving.is_some() {
            return;
        }
        let Some(path) = self.path.clone() else { return };
        if !self.files.asap {
            let busy = !matches!(self.drag, Drag::None) || self.editing.is_some() || ctx.input(|i| i.pointer.any_down());
            if busy {
                return;
            }
            let idle = self.files.last_change.elapsed();
            if idle < AUTOSAVE_DELAY {
                ctx.request_repaint_after(AUTOSAVE_DELAY - idle);
                return;
            }
        }
        self.files.asap = false;
        self.start_save(path);
    }

    /// Ctrl+S on a file that already has a location: save in the background right away.
    pub(super) fn save_soon(&mut self) {
        if self.dirty {
            self.files.asap = true;
            self.status = "Saving…".into();
        } else {
            self.status = "All changes saved.".into();
        }
    }

    /// Write a snapshot of the document on another thread. The snapshot is cheap: layers and
    /// images are shared through `Arc`.
    fn start_save(&mut self, path: PathBuf) {
        let (tx, rx) = mpsc::channel();
        let (doc, ctx, to) = (self.doc.clone(), self.files.ctx.clone(), path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(save_doc(&doc, &to));
            ctx.request_repaint();
        });
        self.files.saving = Some(Saving { path, version: self.files.version, rx });
    }

    /// Each frame: pick up the result of a finished background save.
    pub(super) fn save_poll(&mut self) {
        let Some(job) = &self.files.saving else { return };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("the save was interrupted".into()),
        };
        if let Some(job) = self.files.saving.take() {
            self.saved(job, result);
        }
    }

    /// Block until the background save (if any) has finished.
    pub(super) fn save_wait(&mut self) {
        if let Some(job) = self.files.saving.take() {
            let result = job.rx.recv().unwrap_or_else(|_| Err("the save was interrupted".into()));
            self.saved(job, result);
        }
    }

    fn saved(&mut self, job: Saving, result: Result<(), String>) {
        // The user moved on to another file in the meantime: nothing to update here.
        if self.path.as_deref() != Some(job.path.as_path()) {
            return;
        }
        match result {
            Ok(()) => {
                // Changes made while the file was being written are saved by the next round.
                if job.version == self.files.version {
                    self.dirty = false;
                    self.status = "All changes saved.".into();
                }
                self.files.stale = true;
                self.files.touch(&job.path);
            }
            Err(e) => {
                self.status = format!("Auto-save failed: {e}");
                // Coba lagi setelah jeda berikutnya.
                self.files.last_change = Instant::now();
                self.files.ctx.request_repaint_after(AUTOSAVE_DELAY);
            }
        }
    }

    // ------------------------------------------------------------ buka

    /// Open a file. It is read and parsed on another thread (a document with many images takes
    /// a while); `loading_screen` is shown meanwhile and `load_poll` installs the result.
    pub(super) fn load(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        if self.bench.is_some() {
            // The bench scripts frames by number: the document has to be there from frame one.
            self.load_now(path);
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (ctx, from) = (self.files.ctx.clone(), path.clone());
        std::thread::spawn(move || {
            let _ = tx.send(read_doc(&from));
            ctx.request_repaint();
        });
        self.files.loading = Some(Loading { path, rx });
    }

    /// Open a file and wait for it. Result false = it could not be opened (see `status`).
    pub(super) fn load_now(&mut self, path: PathBuf) -> bool {
        let path = std::path::absolute(&path).unwrap_or(path);
        self.files.loading = None;
        let result = read_doc(&path);
        self.finish_load(path, result)
    }

    fn finish_load(&mut self, path: PathBuf, result: Result<Doc, String>) -> bool {
        match result {
            Ok(doc) => {
                self.doc = doc;
                self.status = format!("Opened: {}", path.display());
                self.opened(&path);
                self.path = Some(path);
                self.reset_session();
                true
            }
            Err(e) => {
                self.status = format!("Couldn't open the file: {e}");
                false
            }
        }
    }

    pub(super) fn loading(&self) -> bool {
        self.files.loading.is_some()
    }

    /// Each frame: install the document once the background read has finished.
    pub(super) fn load_poll(&mut self) {
        let Some(job) = &self.files.loading else { return };
        let result = match job.rx.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => Err("reading the file was interrupted".into()),
        };
        let Some(job) = self.files.loading.take() else { return };
        if !self.finish_load(job.path, result) {
            self.files.note = self.status.clone();
            self.files.refresh();
        }
    }

    /// Shown instead of the editor while a file is being opened. Result true = it was shown.
    pub(super) fn loading_screen(&mut self, ctx: &egui::Context) -> bool {
        let Some(job) = &self.files.loading else { return false };
        let name = job.path.file_stem().map_or("file".into(), |n| n.to_string_lossy().into_owned());
        egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS_BG)).show(ctx, |ui| {
            ui.add_space(ui.available_height() * 0.42);
            ui.vertical_centered(|ui| {
                ui.add(egui::Spinner::new().size(26.0));
                ui.add_space(10.0);
                ui.label(egui::RichText::new(format!("Opening {name}…")).size(15.0));
            });
        });
        true
    }

    /// Bereskan dokumen sebelum ditinggalkan: simpan perubahan dan perbarui thumbnail.
    /// Hasil false = penyimpanan gagal (pesannya ada di `status`).
    pub(super) fn settle(&mut self) -> bool {
        self.finish_text_edit();
        let Some(path) = self.path.clone() else { return true };
        // A background save that is about to finish may already hold everything.
        self.save_wait();
        if self.dirty {
            if let Err(e) = self.write_doc(&path) {
                self.status = format!("Couldn't save: {e}");
                return false;
            }
        }
        if self.files.stale {
            self.snapshot(&path);
        }
        true
    }

    fn snapshot(&mut self, path: &Path) {
        self.files.stale = false;
        let ctx = self.files.ctx.clone();
        let thumb = thumb_path(path);
        match export::to_svg(&self.doc, None, &mut self.fonts, &ctx) {
            Some((svg, w, h)) => {
                let scale = (THUMB.0 / w).min(THUMB.1 / h).min(4.0);
                if let Some(dir) = thumb.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = export::save_raster(&svg, w, h, scale, false, &thumb);
            }
            None => {
                let _ = std::fs::remove_file(&thumb);
            }
        }
        self.files.thumbs.remove(path);
    }

    // ------------------------------------------------------------ tutup

    /// Saat jendela ditutup: file yang sudah punya lokasi disimpan otomatis,
    /// file yang belum pernah disimpan memunculkan dialog simpan.
    pub(super) fn close_flow(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested());
        if requested && !self.files.closing && self.bench.is_none() {
            if self.dirty && self.path.is_none() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.finish_text_edit();
                self.files.prompt = Some("This file hasn't been saved yet. Choose where to save it before closing?".into());
            } else if !self.settle() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.files.prompt = Some(format!("{} Save a copy somewhere else before closing?", self.status));
            }
        }
        let Some(message) = self.files.prompt.clone() else { return };
        let mut choice = 0;
        let modal = egui::Modal::new(egui::Id::new("kanvas_close_prompt")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.label(egui::RichText::new("Save your work?").strong().size(17.0));
            ui.add_space(6.0);
            ui.label(message);
            ui.add_space(14.0);
            ui.horizontal(|ui| {
                let save = egui::Button::new(egui::RichText::new("Save…").color(Color32::WHITE)).fill(ACCENT);
                if ui.add(save.min_size(vec2(84.0, 28.0))).clicked() {
                    choice = 1;
                }
                if ui.add(egui::Button::new("Don't save").min_size(vec2(84.0, 28.0))).clicked() {
                    choice = 2;
                }
                if ui.add(egui::Button::new("Cancel").min_size(vec2(84.0, 28.0))).clicked() {
                    choice = 3;
                }
            });
        });
        if choice == 0 && modal.should_close() {
            choice = 3;
        }
        match choice {
            1 => {
                // Dialog pilih lokasi; kalau dibatalkan, popup tetap terbuka.
                if self.save(true) {
                    self.settle();
                    self.quit(ctx);
                }
            }
            2 => self.quit(ctx),
            3 => self.files.prompt = None,
            _ => {}
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.save_wait();
        self.files.prompt = None;
        self.files.closing = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    // ------------------------------------------------------------ halaman awal

    pub(super) fn go_home(&mut self) {
        if !self.settle() {
            return;
        }
        self.files.refresh();
        self.files.note.clear();
        self.home = true;
    }

    pub(super) fn home_screen(&mut self, ctx: &egui::Context) {
        let mut act = Act::None;
        let (open, new) = ctx.input_mut(|i| (i.consume_key(Modifiers::COMMAND, Key::O), i.consume_key(Modifiers::COMMAND, Key::N)));
        if open {
            act = Act::Open;
        } else if new {
            act = Act::New;
        }
        let dropped = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).find(|p| p.extension().is_some_and(|e| e == "kanvas"))
        });
        if let Some(p) = dropped {
            act = Act::Load(p);
        }
        let has_doc = self.path.is_some() || self.dirty;
        let current = self.path.as_ref().and_then(|p| p.file_stem()).map_or("Untitled".into(), |n| n.to_string_lossy().into_owned());

        egui::SidePanel::left("home_side")
            .exact_width(244.0)
            .resizable(false)
            .frame(theme::gutter(theme::Edge::LeftFull))
            .show_separator_line(false)
            .show(ctx, |ui| theme::card(ui, |ui| {
                ui.add_space(4.0);
                ui.label(theme::title("Kanvas").size(19.0));
                ui.add_space(14.0);
                let w = ui.available_width();
                let _ = ui.add_sized([w, 30.0], egui::SelectableLabel::new(true, "Recents"));
                if has_doc {
                    ui.add_space(10.0);
                    ui.separator();
                    ui.add_space(6.0);
                    ui.weak("Open now");
                    let label: String = current.chars().take(24).collect();
                    if ui.add_sized([w, 30.0], egui::Button::new(format!("Back to {label}"))).clicked() {
                        act = Act::Back;
                    }
                }
            }));

        egui::CentralPanel::default().frame(egui::Frame::new().fill(CANVAS_BG).inner_margin(24.0)).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::title("Recents").size(21.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(egui::Button::new("Open file…").min_size(vec2(96.0, 30.0))).on_hover_text("Ctrl+O").clicked() {
                        act = Act::Open;
                    }
                    let new = egui::Button::new(egui::RichText::new("+ New design file").color(Color32::WHITE)).fill(ACCENT);
                    if ui.add(new.min_size(vec2(140.0, 30.0))).on_hover_text("Ctrl+N").clicked() {
                        act = Act::New;
                    }
                });
            });
            if !self.files.note.is_empty() {
                ui.add_space(4.0);
                ui.colored_label(Color32::from_rgb(255, 120, 100), &self.files.note);
            }
            ui.add_space(16.0);
            if self.files.recent.is_empty() {
                ui.add_space(ui.available_height() * 0.3);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("No recent files yet").size(16.0));
                    ui.weak("Create a new design file or open one — it will show up here.");
                });
                return;
            }
            let items: Vec<(PathBuf, Option<u64>)> = self.files.recent.iter().map(|r| (r.path.clone(), r.edited)).collect();
            let today = now();
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                let gap = 20.0;
                let avail = ui.available_width() - 8.0;
                let cols = ((avail + gap) / (270.0 + gap)).floor().max(1.0) as usize;
                let cw = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();
                let th = (cw * 0.56).round();
                ui.spacing_mut().item_spacing = vec2(gap, gap);
                for row in items.chunks(cols) {
                    ui.horizontal(|ui| {
                        for (path, edited) in row {
                            let tex = self.files.texture(path);
                            if let Some(a) = card(ui, vec2(cw, th + 58.0), th, path, *edited, today, tex) {
                                act = a;
                            }
                        }
                    });
                }
            });
        });

        match act {
            Act::None => {}
            Act::New => {
                if self.confirm_discard() {
                    self.doc = Doc::default();
                    self.path = None;
                    self.reset_session();
                    self.status = "New document.".into();
                    self.home = false;
                }
            }
            Act::Open => self.open_dialog(),
            Act::Back => self.home = false,
            Act::Forget(p) => self.files.forget(&p),
            Act::Load(p) => {
                if self.path.as_ref() == Some(&p) {
                    self.home = false;
                } else if self.confirm_discard() {
                    self.load(p);
                }
            }
        }
    }
}

fn card(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    thumb_h: f32,
    path: &Path,
    edited: Option<u64>,
    today: u64,
    tex: Option<egui::TextureHandle>,
) -> Option<Act> {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(path.display().to_string());
    if !ui.is_rect_visible(rect) {
        return None;
    }
    let p = ui.painter_at(rect);
    let round = CornerRadius::same(10);
    p.rect_filled(rect, round, Color32::from_rgb(44, 44, 44));
    let thumb = Rect::from_min_size(rect.min, vec2(size.x, thumb_h));
    p.rect_filled(thumb, CornerRadius { nw: 10, ne: 10, sw: 0, se: 0 }, Color32::from_rgb(22, 22, 22));
    let weak = ui.visuals().weak_text_color();
    match &tex {
        Some(t) => {
            let inner = thumb.shrink(14.0);
            let ts = t.size_vec2();
            let s = (inner.width() / ts.x).min(inner.height() / ts.y);
            let tint = if edited.is_some() { Color32::WHITE } else { Color32::from_gray(110) };
            p.image(t.id(), Rect::from_center_size(inner.center(), ts * s), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), tint);
        }
        None => {
            p.text(thumb.center(), Align2::CENTER_CENTER, "No preview", FontId::proportional(12.0), weak);
        }
    }
    let foot = Rect::from_min_max(pos2(rect.left(), thumb.bottom()), rect.max);
    let icon = Rect::from_center_size(pos2(foot.left() + 24.0, foot.center().y), vec2(18.0, 18.0));
    p.rect_filled(icon, CornerRadius::same(5), ACCENT);
    p.rect_stroke(icon.shrink(4.5), CornerRadius::same(2), Stroke::new(1.4_f32, Color32::WHITE), StrokeKind::Middle);
    let name = path.file_stem().map_or("Untitled".into(), |n| n.to_string_lossy().into_owned());
    let text = p.with_clip_rect(Rect::from_min_max(pos2(foot.left() + 44.0, foot.top()), pos2(foot.right() - 12.0, foot.bottom())));
    text.text(pos2(foot.left() + 44.0, foot.center().y - 9.0), Align2::LEFT_CENTER, name, FontId::proportional(13.5), ui.visuals().strong_text_color());
    let sub = match edited {
        Some(t) => format!("Edited {}", ago(today.saturating_sub(t))),
        None => "File not found".to_owned(),
    };
    text.text(pos2(foot.left() + 44.0, foot.center().y + 9.0), Align2::LEFT_CENTER, sub, FontId::proportional(11.5), weak);
    let border = if resp.hovered() { Stroke::new(1.5_f32, ACCENT) } else { Stroke::new(1.0_f32, Color32::from_gray(58)) };
    p.rect_stroke(rect, round, border, StrokeKind::Inside);

    let mut act = None;
    if resp.clicked() {
        act = Some(Act::Load(path.to_owned()));
    }
    resp.context_menu(|ui| {
        if ui.button("Open").clicked() {
            ui.close_menu();
            act = Some(Act::Load(path.to_owned()));
        }
        if ui.button("Remove from recents").clicked() {
            ui.close_menu();
            act = Some(Act::Forget(path.to_owned()));
        }
    });
    act
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Kind, Node};

    #[test]
    fn save_and_read_round_trip() {
        let mut doc = Doc::default();
        let image = doc.add_asset(vec![7; 3000]);
        let mut frame = Node::new(doc.alloc_id(), Kind::Frame, 0.0, 0.0);
        frame.children.push(Node::new(doc.alloc_id(), Kind::Rect, 10.0, 10.0));
        doc.insert(frame, None);
        let path = std::env::temp_dir().join(format!("kanvas_test_save_{}.kanvas", std::process::id()));
        save_doc(&doc, &path).unwrap();
        // Saving again replaces the file and leaves no temporary file behind.
        save_doc(&doc, &path).unwrap();
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        assert!(!Path::new(&tmp).exists());
        let back = read_doc(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(back.count(), 2);
        assert_eq!(back.nodes[0].children, doc.nodes[0].children);
        assert_eq!(back.assets[&image].bytes, vec![7; 3000]);
        assert!(read_doc(&path).is_err());
    }
}
