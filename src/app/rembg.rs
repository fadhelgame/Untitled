//! Remove background lewat Codex CLI (`codex exec`): gambar ditulis ke folder sementara,
//! Codex diminta menghasilkan `output.png` transparan, lalu hasilnya menggantikan gambar di node.

use super::*;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::mpsc;

/// Pekerjaan yang sedang berjalan: (id node, penerima hasil PNG).
pub(super) type Job = (u64, mpsc::Receiver<Result<Vec<u8>, String>>);

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
            let _ = tx.send(run_codex(&asset.bytes, id));
            ctx.request_repaint();
        });
        self.rembg = Some((id, rx));
        self.status = "Removing background with Codex… this can take a minute.".into();
    }

    /// Tiap frame: ambil hasil kalau sudah selesai dan pasang ke node.
    pub(super) fn rembg_poll(&mut self) {
        let Some((id, rx)) = &self.rembg else { return };
        let id = *id;
        let result = match rx.try_recv() {
            Ok(r) => r,
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
    }
}

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
