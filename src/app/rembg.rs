//! Remove background secara lokal: model segmentasi ISNet (ONNX) dijalankan langsung di aplikasi
//! lewat ONNX Runtime. Model (±170 MB) diunduh sekali saat pertama dipakai lalu disimpan di folder
//! data aplikasi; sesudah itu fiturnya jalan offline dalam hitungan detik.
//!
//! Kalau jalur lokal gagal (mis. model tidak bisa diunduh), dicoba cara lama lewat Codex CLI
//! (`codex exec`): gambar ditulis ke folder sementara dan Codex diminta menghasilkan `output.png`.

use super::*;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

pub(super) enum Msg {
    /// Kabar kemajuan untuk status bar (unduhan model, dsb.).
    Status(String),
    Done(Result<Vec<u8>, String>),
}

/// Pekerjaan yang sedang berjalan: (id node, penerima pesan dari thread pekerja).
pub(super) type Job = (u64, mpsc::Receiver<Msg>);

/// ISNet "general use" (Apache-2.0), berkas ONNX yang sama dengan yang dipakai proyek rembg.
const MODEL_URL: &str = "https://github.com/danielgatis/rembg/releases/download/v0.0.0/isnet-general-use.onnx";
const MODEL_FILE: &str = "isnet-general-use.onnx";
/// Ukuran berkas yang benar; dipakai untuk mengenali unduhan yang terpotong.
const MODEL_BYTES: u64 = 178_648_008;
/// Sisi masukan model (piksel).
const SIDE: u32 = 1024;

const PROMPT: &str = "Remove the background from the image file `input.png` in the current directory. \
Save the result as `output.png` in the same directory: a PNG with an alpha channel, exactly the same pixel \
dimensions as the input, the main subject kept unchanged and the background fully transparent. \
Use whatever image tool you have available. Do not ask questions and do not modify any other file.";

impl App {
    pub(super) fn rembg_running(&self, id: u64) -> bool {
        self.rembg.as_ref().is_some_and(|j| j.0 == id)
    }

    /// Mulai remove background untuk node gambar; jalan di thread lain supaya UI tidak macet.
    pub(super) fn rembg_start(&mut self, id: u64, ctx: &egui::Context) {
        if self.rembg.is_some() {
            self.status = "A background removal is already running.".into();
            return;
        }
        let asset = self.doc.find(id).and_then(|n| n.image).and_then(|a| self.doc.assets.get(&a)).cloned();
        let Some(asset) = asset else { return };
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let report = |s: String| {
                let _ = tx.send(Msg::Status(s));
                ctx.request_repaint();
            };
            let result = remove_local(&asset.bytes, &report).or_else(|local| {
                report(format!("Local model unavailable ({local}). Trying Codex…"));
                run_codex(&asset.bytes, id).map_err(|codex| format!("{local} Codex fallback: {codex}"))
            });
            let _ = tx.send(Msg::Done(result));
            ctx.request_repaint();
        });
        self.rembg = Some((id, rx));
        self.status = "Removing background…".into();
    }

    /// Tiap frame: teruskan kabar kemajuan, dan pasang hasilnya ke node kalau sudah selesai.
    pub(super) fn rembg_poll(&mut self) {
        loop {
            let Some((id, rx)) = &self.rembg else { return };
            let id = *id;
            let result = match rx.try_recv() {
                Ok(Msg::Status(s)) => {
                    self.status = s;
                    continue;
                }
                Ok(Msg::Done(r)) => r,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => Err("the worker stopped unexpectedly".into()),
            };
            self.rembg = None;
            match result {
                Ok(bytes) if self.doc.find(id).is_some() => {
                    let pre = self.doc.clone();
                    let asset = self.doc.add_asset(bytes);
                    if let Some(n) = self.doc.find_mut(id) {
                        n.image = Some(asset);
                    }
                    self.push_undo(pre);
                    self.status = "Background removed.".into();
                }
                Ok(_) => self.status = "Background removed, but the image no longer exists.".into(),
                Err(e) => self.status = format!("Remove background failed: {e}"),
            }
            return;
        }
    }
}

/// `kanvas --rembg <masukan> <keluaran.png>`: jalankan remove background tanpa membuka jendela.
pub fn cli(input: &Path, output: &Path) -> Result<(), String> {
    let bytes = std::fs::read(input).map_err(|e| e.to_string())?;
    let png = remove_local(&bytes, &|s| eprintln!("{s}"))?;
    std::fs::write(output, png).map_err(|e| e.to_string())
}

// ------------------------------------------------------------ model lokal

/// Session ONNX disimpan setelah dipakai pertama kali, jadi pemakaian berikutnya tidak memuat
/// ulang model dari disk.
static SESSION: Mutex<Option<ort::session::Session>> = Mutex::new(None);

fn remove_local(bytes: &[u8], report: &dyn Fn(String)) -> Result<Vec<u8>, String> {
    let mut source = image::load_from_memory(bytes).map_err(|e| e.to_string())?.to_rgba8();
    let (w, h) = source.dimensions();

    let mut guard = SESSION.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        let path = ensure_model(report)?;
        report("Loading background model…".into());
        *guard = Some(load_session(&path)?);
    }
    let session = guard.as_mut().unwrap();
    report("Removing background…".into());

    // Masukan: RGB 1024×1024 (tanpa menjaga rasio, sama seperti saat model dilatih), nilai 0..1
    // dikurangi 0,5, susunan CHW.
    let small = image::imageops::resize(&source, SIDE, SIDE, image::imageops::FilterType::Triangle);
    let plane = (SIDE * SIDE) as usize;
    let mut input = vec![0f32; 3 * plane];
    for (i, px) in small.pixels().enumerate() {
        for c in 0..3 {
            input[c * plane + i] = px[c] as f32 / 255.0 - 0.5;
        }
    }
    let tensor = ort::value::Tensor::from_array(([1usize, 3, SIDE as usize, SIDE as usize], input)).map_err(|e| e.to_string())?;
    let outputs = session.run(ort::inputs![tensor]).map_err(|e| e.to_string())?;
    let (_, mask) = outputs[0].try_extract_tensor::<f32>().map_err(|e| e.to_string())?;
    if mask.len() != plane {
        return Err("the model returned a mask of an unexpected size".into());
    }

    // Keluaran dinormalkan ke 0..1 (min–max), dibesarkan ke ukuran asli, lalu dikalikan ke alpha.
    let (lo, hi) = mask.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    let span = (hi - lo).max(1e-6);
    let gray: Vec<u8> = mask.iter().map(|&v| ((v - lo) / span * 255.0).round() as u8).collect();
    let gray = image::GrayImage::from_raw(SIDE, SIDE, gray).ok_or("bad mask buffer")?;
    let alpha = image::imageops::resize(&gray, w, h, image::imageops::FilterType::Triangle);
    for (px, a) in source.pixels_mut().zip(alpha.pixels()) {
        px[3] = ((px[3] as u32 * a[0] as u32 + 127) / 255) as u8;
    }

    let mut png = Vec::new();
    source.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).map_err(|e| e.to_string())?;
    Ok(png)
}

fn load_session(path: &Path) -> Result<ort::session::Session, String> {
    let env = ort::init().with_name("kanvas").build().map_err(|e| e.to_string())?;
    let loaded = ort::session::Session::builder(&env).and_then(|mut b| b.commit_from_file(path));
    loaded.map_err(|e| {
        // Berkas rusak: buang supaya percobaan berikutnya mengunduh ulang.
        let _ = std::fs::remove_file(path);
        format!("couldn't load the background model ({e})")
    })
}

/// Lokasi model di disk; diunduh dulu kalau belum ada atau ukurannya salah.
fn ensure_model(report: &dyn Fn(String)) -> Result<PathBuf, String> {
    let dir = crate::fonts::cache_dir().join("models");
    let path = dir.join(MODEL_FILE);
    if std::fs::metadata(&path).is_ok_and(|m| m.len() == MODEL_BYTES) {
        return Ok(path);
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let part = dir.join(format!("{MODEL_FILE}.part"));
    let result = download(&part, report);
    if result.is_ok() {
        std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
        return Ok(path);
    }
    let _ = std::fs::remove_file(&part);
    result.map(|_| path)
}

fn download(part: &Path, report: &dyn Fn(String)) -> Result<(), String> {
    let mb = |b: u64| b / (1024 * 1024);
    report(format!("Downloading background model ({} MB, first time only)…", mb(MODEL_BYTES)));
    // Tanpa batas waktu total (berkasnya besar); cukup batas sambung dan batas tiap pembacaan.
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(30))
        .user_agent("kanvas/0.1")
        .build();
    let failed = |e: String| format!("couldn't download the background model ({e})");
    let response = agent.get(MODEL_URL).call().map_err(|e| failed(e.to_string()))?;
    let mut reader = response.into_reader();
    let mut file = std::fs::File::create(part).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; 256 * 1024];
    let (mut done, mut shown) = (0u64, 0u64);
    loop {
        let n = reader.read(&mut buf).map_err(|e| failed(e.to_string()))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        let pct = done * 100 / MODEL_BYTES;
        if pct > shown {
            shown = pct;
            report(format!("Downloading background model… {pct}% ({} / {} MB, first time only)", mb(done), mb(MODEL_BYTES)));
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    if done != MODEL_BYTES {
        return Err(failed("the download was incomplete".into()));
    }
    Ok(())
}

// ------------------------------------------------------------ cadangan: Codex CLI

fn run_codex(bytes: &[u8], id: u64) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join(format!("kanvas-rembg-{}-{id}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let result = run_in(&dir, bytes);
    let _ = std::fs::remove_dir_all(&dir);
    result
}

fn run_in(dir: &std::path::Path, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let source = image::load_from_memory(bytes).map_err(|e| e.to_string())?;
    source.save(dir.join("input.png")).map_err(|e| e.to_string())?;

    // Prompt dikirim lewat stdin (`-`) supaya tidak ada masalah kutip di command line.
    // `--approve-for-me`: perintah Codex ditinjau otomatis di sandbox workspace-write
    // (tanpa ini sesi `exec` di Windows hanya read-only dan tidak bisa menulis output.png).
    let args = ["exec", "--skip-git-repo-check", "--approve-for-me", "-"];
    // Urutan: PATH, lokasi installer resmi Windows, lalu `codex.cmd` (instalasi npm) lewat cmd.
    let mut programs: Vec<(std::path::PathBuf, &[&str])> = vec![("codex".into(), &[])];
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            programs.push((std::path::Path::new(&local).join("Programs/OpenAI/Codex/bin/codex.exe"), &[]));
        }
        programs.push(("cmd".into(), &["/C", "codex"]));
    }
    let mut child = None;
    for (program, prefix) in programs {
        let mut cmd = Command::new(program);
        cmd.args(prefix).args(args).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        if let Ok(c) = cmd.spawn() {
            child = Some(c);
            break;
        }
    }
    let mut child = child.ok_or("couldn't start Codex. Install the Codex CLI and run `codex login` first.")?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(PROMPT.as_bytes());
    }
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&out.stderr);

    let Ok(png) = std::fs::read(dir.join("output.png")) else {
        if stderr.contains("not recognized") || stderr.contains("not found") {
            return Err("Codex CLI isn't installed. Install it and run `codex login` first.".into());
        }
        let last = stderr.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("Codex produced no output.png");
        return Err(last.chars().take(160).collect());
    };
    let result = image::load_from_memory(&png).map_err(|e| format!("Codex returned an unreadable image ({e})"))?;
    if !result.color().has_alpha() {
        return Err("Codex returned an image without transparency.".into());
    }
    Ok(png)
}
