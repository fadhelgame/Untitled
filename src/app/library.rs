//! Library: kumpulan gambar milik pengguna (logo, foto, grafis) yang selalu tersedia di semua
//! dokumen. File-nya disalin ke folder `library` di folder data aplikasi, jadi tidak perlu lagi
//! mencari di folder komputer: buka jendela Library, lalu tarik gambarnya ke canvas.
//!
//! Thumbnail dibuat di thread lain; hasilnya dikirim lewat channel dan diambil tiap frame.

use super::*;
use std::path::Path;
use std::sync::mpsc;

/// Sisi kotak thumbnail di kisi (poin layar).
const TILE: f32 = 90.0;
/// Tinggi baris nama di bawah thumbnail.
const LABEL: f32 = 20.0;
/// Sisi terpanjang thumbnail (piksel).
const THUMB: u32 = 192;
const EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "webp", "gif", "bmp", "svg"];

struct Entry {
    /// Nama file di folder library (dengan ekstensi).
    name: String,
    tex: Option<egui::TextureHandle>,
    failed: bool,
}

pub(super) struct Library {
    pub open: bool,
    /// Folder tempat file library disimpan.
    dir: PathBuf,
    filter: String,
    focus: bool,
    entries: Vec<Entry>,
    scanned: bool,
    /// Item yang sedang ditarik ke canvas.
    drag: Option<usize>,
    /// Kotak jendela di layar: file yang dijatuhkan di sini masuk ke library, bukan ke canvas.
    rect: Option<Rect>,
    tx: mpsc::Sender<(String, Option<egui::ColorImage>)>,
    rx: mpsc::Receiver<(String, Option<egui::ColorImage>)>,
}

enum Act {
    Browse,
    SaveSelection,
    Place(usize, Option<Pos2>),
    Remove(usize),
}

fn supported(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// Nama tampilan: nama file tanpa ekstensi.
fn stem(name: &str) -> &str {
    name.rsplit_once('.').map_or(name, |s| s.0)
}

/// Thumbnail dengan rasio aslinya, sisi terpanjang `THUMB`.
fn thumbnail(bytes: &[u8]) -> Option<egui::ColorImage> {
    if render::is_svg(bytes) {
        use resvg::tiny_skia::{Pixmap, Transform};
        let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
        let size = tree.size();
        let scale = THUMB as f32 / size.width().max(size.height()).max(0.001);
        let (w, h) = ((size.width() * scale).ceil().max(1.0) as u32, (size.height() * scale).ceil().max(1.0) as u32);
        let mut pixmap = Pixmap::new(w, h)?;
        resvg::render(&tree, Transform::from_scale(scale, scale), &mut pixmap.as_mut());
        return Some(egui::ColorImage::from_rgba_premultiplied([w as usize, h as usize], pixmap.data()));
    }
    let img = image::load_from_memory(bytes).ok()?.thumbnail(THUMB, THUMB).to_rgba8();
    Some(egui::ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], &img))
}

impl Library {
    pub(super) fn new(dir: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel();
        Library { open: false, dir, filter: String::new(), focus: false, entries: Vec::new(), scanned: false, drag: None, rect: None, tx, rx }
    }

    /// Masih ada thumbnail yang ditunggu.
    pub(super) fn busy(&self) -> bool {
        self.entries.iter().any(|e| e.tex.is_none() && !e.failed)
    }

    /// Nama file gambar di folder library, urut abjad.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_file()) && supported(&e.path()))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        names
    }

    /// Baca ulang isi folder; thumbnail yang sudah ada dipakai lagi, sisanya dibuat di thread lain.
    fn scan(&mut self, ctx: &egui::Context) {
        self.scanned = true;
        let names = self.names();
        let mut old = std::mem::take(&mut self.entries);
        let mut todo = Vec::new();
        for name in names {
            match old.iter().position(|e| e.name == name) {
                Some(i) => self.entries.push(old.swap_remove(i)),
                None => {
                    todo.push(name.clone());
                    self.entries.push(Entry { name, tex: None, failed: false });
                }
            }
        }
        if todo.is_empty() {
            return;
        }
        let (dir, tx, ctx) = (self.dir.clone(), self.tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            for name in todo {
                let image = std::fs::read(dir.join(&name)).ok().and_then(|b| thumbnail(&b));
                if tx.send((name, image)).is_err() {
                    return;
                }
                ctx.request_repaint();
            }
        });
    }

    /// Tiap frame: ambil thumbnail yang sudah selesai.
    pub(super) fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((name, image)) = self.rx.try_recv() {
            let Some(entry) = self.entries.iter_mut().find(|e| e.name == name && e.tex.is_none()) else { continue };
            match image {
                Some(image) => entry.tex = Some(ctx.load_texture(format!("library-{name}"), image, egui::TextureOptions::LINEAR)),
                None => entry.failed = true,
            }
        }
    }

    /// Simpan satu gambar ke folder library dengan nama yang belum terpakai. Hasil: nama file-nya.
    fn store(&self, name: &str, bytes: &[u8]) -> Result<String, String> {
        if render::image_size(bytes).is_none() {
            return Err(format!("\"{name}\" isn't an image Kanvas can read"));
        }
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let clean: String = name.chars().map(|c| if c.is_control() || "\\/:*?\"<>|".contains(c) { '_' } else { c }).collect();
        let clean = clean.trim().trim_matches('.').to_owned();
        let (base, ext) = match clean.rsplit_once('.') {
            Some((b, e)) if !b.is_empty() && EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()) => (b.to_owned(), e.to_ascii_lowercase()),
            _ => (if clean.is_empty() { "Image".to_owned() } else { clean }, extension_of(bytes).to_owned()),
        };
        let mut file = format!("{base}.{ext}");
        let mut n = 2;
        while self.dir.join(&file).exists() {
            // Isi yang sama persis sudah ada: tidak perlu salinan kedua.
            if n == 2 && std::fs::read(self.dir.join(&file)).is_ok_and(|b| b == bytes) {
                return Ok(file);
            }
            file = format!("{base} {n}.{ext}");
            n += 1;
        }
        std::fs::write(self.dir.join(&file), bytes).map_err(|e| e.to_string())?;
        Ok(file)
    }

    /// Daftar isi library: (nama file, lokasi lengkap).
    pub(super) fn list(&self) -> Vec<(String, PathBuf)> {
        self.names().into_iter().map(|n| (n.clone(), self.dir.join(n))).collect()
    }

    /// Salin satu file ke library (dari remote); jendela Library membaca ulang di frame berikutnya.
    pub(super) fn add_file(&mut self, path: &Path) -> Result<String, String> {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let bytes = std::fs::read(path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
        let file = self.store(&name, &bytes)?;
        self.scanned = false;
        Ok(file)
    }
}

/// Ekstensi file untuk byte gambar ini.
fn extension_of(bytes: &[u8]) -> &'static str {
    if render::is_svg(bytes) {
        return "svg";
    }
    image::guess_format(bytes).ok().and_then(|f| f.extensions_str().first().copied()).unwrap_or("png")
}

/// Gambar tombol toolbar: map/folder dengan bintang kecil.
pub(super) fn toolbar_icon(p: &egui::Painter, c: Pos2, color: Color32) {
    let s = Stroke::new(1.6_f32, color);
    let at = |x: f32, y: f32| pos2(c.x + x, c.y + y);
    p.add(egui::Shape::closed_line(
        vec![at(-8.5, -6.0), at(-2.5, -6.0), at(-0.5, -3.5), at(8.5, -3.5), at(8.5, 7.0), at(-8.5, 7.0)],
        s,
    ));
    p.line_segment([at(0.0, -0.5), at(0.0, 4.5)], s);
    p.line_segment([at(-2.5, 2.0), at(2.5, 2.0)], s);
}

impl App {
    pub(super) fn toggle_library(&mut self) {
        self.library.open = !self.library.open;
        self.library.focus = self.library.open;
    }

    /// Tambah file ke library. Hasil: jumlah yang berhasil ditambahkan.
    pub(super) fn library_add_files(&mut self, ctx: &egui::Context, paths: &[PathBuf]) -> usize {
        let (mut added, mut error) = (0, None);
        for path in paths {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::read(path).map_err(|e| e.to_string()).and_then(|b| self.library.store(&name, &b)) {
                Ok(_) => added += 1,
                Err(e) => error = Some(e),
            }
        }
        self.library.scan(ctx);
        self.status = match (added, error) {
            (0, Some(e)) => format!("Couldn't add to the library: {e}."),
            (n, Some(e)) => format!("Added {n} to the library; skipped one: {e}."),
            (1, None) => "Added 1 item to the library.".into(),
            (n, None) => format!("Added {n} items to the library."),
        };
        added
    }

    /// Gambar terpilih di canvas: (nama layer, byte gambarnya).
    fn selected_images(&self) -> Vec<(String, std::sync::Arc<crate::doc::Asset>)> {
        self.sel
            .iter()
            .filter_map(|id| self.doc.find(*id))
            .filter_map(|n| Some((n.name.clone(), self.doc.assets.get(&n.image?)?.clone())))
            .collect()
    }

    fn library_save_selection(&mut self, ctx: &egui::Context) {
        let (mut added, mut error) = (0, None);
        for (name, asset) in self.selected_images() {
            match self.library.store(&name, &asset.bytes) {
                Ok(_) => added += 1,
                Err(e) => error = Some(e),
            }
        }
        self.library.scan(ctx);
        self.status = match (added, error) {
            (0, Some(e)) => format!("Couldn't add to the library: {e}."),
            (0, None) => "Select an image on the canvas first.".into(),
            (1, _) => "Added the selected image to the library.".into(),
            (n, _) => format!("Added {n} images to the library."),
        };
    }

    /// File yang dijatuhkan di atas jendela Library masuk ke library. Hasil: sudah ditangani.
    pub(super) fn library_take_drop(&mut self, ctx: &egui::Context, files: &[egui::DroppedFile], pos: Option<Pos2>) -> bool {
        let over = self.library.open && pos.zip(self.library.rect).is_some_and(|(p, r)| r.contains(p));
        if !over || files.is_empty() {
            return false;
        }
        let paths: Vec<PathBuf> = files.iter().filter_map(|f| f.path.clone()).collect();
        self.library_add_files(ctx, &paths);
        true
    }

    /// Taruh item library ke canvas (`at` = posisi layar; default tengah canvas).
    pub(super) fn library_place(&mut self, index: usize, at: Option<Pos2>) -> bool {
        let Some(name) = self.library.entries.get(index).map(|e| e.name.clone()) else { return false };
        match std::fs::read(self.library.dir.join(&name)) {
            Ok(bytes) => {
                let label = stem(&name).to_owned();
                let ok = self.add_image(bytes, &label, at);
                if ok {
                    self.fit_placed();
                    self.status = format!("Added \"{label}\" from the library.");
                }
                ok
            }
            Err(e) => {
                self.status = format!("Couldn't read \"{name}\" from the library: {e}.");
                false
            }
        }
    }

    /// Gambar yang baru ditaruh tidak boleh menutupi semuanya: perkecil (rasio tetap) sampai
    /// paling besar 60% dari frame tempatnya, atau 60% dari bagian canvas yang terlihat.
    fn fit_placed(&mut self) {
        let Some(id) = self.sel.first().copied() else { return };
        let (lw, lh) = match self.doc.parent_of(id).and_then(|p| self.doc.find(p)) {
            Some(frame) => (frame.w * 0.6, frame.h * 0.6),
            None => {
                let visible = self.canvas_rect.size() / self.view.zoom;
                (visible.x * 0.6, visible.y * 0.6)
            }
        };
        if let Some(n) = self.doc.find_mut(id) {
            let k = (lw / n.w.max(1.0)).min(lh / n.h.max(1.0));
            if k < 1.0 {
                let (cx, cy) = (n.x + n.w * 0.5, n.y + n.h * 0.5);
                n.w = (n.w * k).round().max(1.0);
                n.h = (n.h * k).round().max(1.0);
                n.x = (cx - n.w * 0.5).round();
                n.y = (cy - n.h * 0.5).round();
            }
        }
    }

    fn library_remove(&mut self, ctx: &egui::Context, index: usize) {
        let Some(name) = self.library.entries.get(index).map(|e| e.name.clone()) else { return };
        let sure = rfd::MessageDialog::new()
            .set_title("Kanvas")
            .set_description(format!("Remove \"{}\" from the library? Designs that already use it keep their copy.", stem(&name)))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !sure {
            return;
        }
        self.status = match std::fs::remove_file(self.library.dir.join(&name)) {
            Ok(()) => format!("Removed \"{}\" from the library.", stem(&name)),
            Err(e) => format!("Couldn't remove \"{name}\": {e}."),
        };
        self.library.scan(ctx);
    }

    /// Jendela Library.
    pub(super) fn library_window(&mut self, ctx: &egui::Context) {
        if !self.library.open {
            self.library.rect = None;
            self.library.drag = None;
            return;
        }
        if !self.library.scanned {
            self.library.scan(ctx);
        }
        let mut open = true;
        let mut act: Option<Act> = None;
        let has_image = !self.selected_images().is_empty();
        let window = egui::Window::new("Library")
            .open(&mut open)
            .collapsible(false)
            .default_pos(self.canvas_rect.left_top() + vec2(16.0, 16.0))
            // Ukuran tetap: isi yang mengikuti lebar jendela membuat jendela egui terus melebar.
            .resizable(false)
            .fixed_size([412.0, 430.0])
            .show(ctx, |ui| {
                let lib = &mut self.library;
                ui.horizontal(|ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut lib.filter).hint_text("Filter by name…").desired_width(ui.available_width() - 104.0));
                    if std::mem::take(&mut lib.focus) {
                        edit.request_focus();
                    }
                    if ui.add(egui::Button::new("Add files…").min_size(vec2(96.0, 0.0))).clicked() {
                        act = Some(Act::Browse);
                    }
                });
                ui.add_space(4.0);
                if lib.entries.is_empty() {
                    ui.weak("Keep your own logos, photos and graphics here, ready in every document. Add files, or drop them onto this window.");
                } else {
                    ui.weak("Drag an item onto the canvas, or click it. Right-click to remove.");
                }
                if has_image && ui.add(egui::Button::new("Add the selected image to the library").min_size(vec2(ui.available_width(), 26.0))).clicked() {
                    act = Some(Act::SaveSelection);
                }
                ui.add_space(4.0);
                let filter = lib.filter.trim().to_lowercase();
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
                        for (i, entry) in lib.entries.iter().enumerate() {
                            if !filter.is_empty() && !entry.name.to_lowercase().contains(&filter) {
                                continue;
                            }
                            let (rect, resp) = ui.allocate_exact_size(vec2(TILE, TILE + LABEL), Sense::click_and_drag());
                            let tile = Rect::from_min_size(rect.min, vec2(TILE, TILE));
                            // Abu-abu sedang: logo putih maupun hitam sama-sama terlihat.
                            ui.painter().rect_filled(tile, CornerRadius::same(10), Color32::from_gray(if resp.hovered() { 150 } else { 128 }));
                            if resp.hovered() || lib.drag == Some(i) {
                                ui.painter().rect_stroke(tile, CornerRadius::same(10), Stroke::new(2.0_f32, ACCENT), StrokeKind::Inside);
                            }
                            match &entry.tex {
                                Some(tex) => {
                                    let (area, size) = (tile.shrink(8.0), tex.size_vec2());
                                    let k = (area.width() / size.x).min(area.height() / size.y);
                                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                                    ui.painter().image(tex.id(), Rect::from_center_size(area.center(), size * k), uv, Color32::WHITE);
                                }
                                None if entry.failed => {
                                    ui.painter().text(tile.center(), Align2::CENTER_CENTER, "?", FontId::proportional(22.0), Color32::from_gray(60));
                                }
                                None => {
                                    ui.painter().circle_filled(tile.center(), 3.0, Color32::from_gray(200));
                                }
                            }
                            let label = Rect::from_min_max(pos2(rect.left(), tile.bottom()), rect.max);
                            let galley = ui.painter().layout_no_wrap(stem(&entry.name).to_owned(), FontId::proportional(11.5), ui.visuals().text_color());
                            let x = if galley.size().x <= label.width() { label.center().x - galley.size().x * 0.5 } else { label.left() };
                            ui.painter().with_clip_rect(label).galley(pos2(x, label.center().y - galley.size().y * 0.5), galley, Color32::WHITE);
                            if resp.drag_started() && !entry.failed {
                                lib.drag = Some(i);
                            }
                            if resp.clicked() && !entry.failed {
                                act = Some(Act::Place(i, None));
                            }
                            resp.on_hover_text(&entry.name).context_menu(|ui| {
                                if ui.button("Remove from library").clicked() {
                                    act = Some(Act::Remove(i));
                                    ui.close_menu();
                                }
                            });
                        }
                    });
                });
            });
        self.library.rect = window.map(|w| w.response.rect);
        self.library.open = open;
        // Menarik item ke canvas: thumbnail mengikuti pointer, dilepas di canvas = ditaruh di situ.
        if let Some(i) = self.library.drag {
            let (down, pos, esc) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos(), i.key_pressed(Key::Escape)));
            if down && !esc {
                ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                if let Some((p, tex)) = pos.zip(self.library.entries.get(i).and_then(|e| e.tex.as_ref())) {
                    let size = tex.size_vec2();
                    let size = size * (72.0 / size.x.max(size.y).max(1.0));
                    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Tooltip, egui::Id::new("library_drag")));
                    let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                    painter.image(tex.id(), Rect::from_center_size(p, size), uv, Color32::from_white_alpha(200));
                }
            } else {
                self.library.drag = None;
                let over_window = pos.zip(self.library.rect).is_some_and(|(p, r)| r.contains(p));
                if let Some(p) = pos.filter(|p| !esc && !over_window && self.canvas_rect.contains(*p)) {
                    act = Some(Act::Place(i, Some(p)));
                }
            }
        }
        match act {
            Some(Act::Browse) => {
                if let Some(paths) = rfd::FileDialog::new().add_filter("Images", &EXTENSIONS).pick_files() {
                    self.library_add_files(ctx, &paths);
                }
            }
            Some(Act::SaveSelection) => self.library_save_selection(ctx),
            Some(Act::Place(i, at)) => {
                self.library_place(i, at);
            }
            Some(Act::Remove(i)) => self.library_remove(ctx, i),
            None => {}
        }
    }
}
