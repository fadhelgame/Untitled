//! Server MCP (stdio) untuk Kanvas. Tiap panggilan tool diteruskan ke aplikasi Kanvas yang sedang
//! berjalan lewat soket lokal (lihat `src/app/remote.rs`). Kalau Kanvas belum terbuka, server ini
//! menjalankan `kanvas.exe` yang ada di folder yang sama.
//!
//! Daftarkan di klien MCP dengan perintah: `kanvas-mcp.exe` (tanpa argumen).

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, ErrorKind, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const TOOLS: &str = include_str!("../mcp_tools.json");
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];
const INSTRUCTIONS: &str = "Kanvas is a native design editor (infinite canvas, frames, shapes, text with Google Fonts, vector paths, images). \
These tools edit the document that is open in the user's Kanvas window, live — the app is started automatically if it isn't running. \
Typical flow: get_document to see what exists; create_nodes to add a frame with its children in one call (children use coordinates relative to the frame); \
screenshot the frame to check the result and fix spacing/overflow with update_nodes; repeat. Text sizes are returned by create/update, use them for alignment. \
Every change is a normal undo step in the app, and the user can edit by hand at any time, so re-read the document before relying on old ids or positions.";

/// Folder data aplikasi; harus sama dengan `data_dir` di `src/app/home.rs`.
fn data_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("APPDATA") {
        return PathBuf::from(p).join("Kanvas");
    }
    if let Some(p) = std::env::var_os("HOME") {
        return PathBuf::from(p).join(".config").join("kanvas");
    }
    std::env::temp_dir().join("kanvas")
}

struct Link {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
}

/// Kesalahan komunikasi; `retry` = aman dicoba lagi lewat sambungan baru (perintah belum dijalankan).
struct LinkError {
    message: String,
    retry: bool,
}

impl Link {
    fn exchange(&mut self, msg: &Value) -> Result<Value, LinkError> {
        let lost = |e: std::io::Error| LinkError { message: format!("lost the connection to Kanvas: {e}"), retry: true };
        writeln!(self.writer, "{msg}").map_err(lost)?;
        let mut line = String::new();
        match self.reader.read_line(&mut line) {
            Ok(0) => Err(LinkError { message: "Kanvas closed the connection".into(), retry: true }),
            Ok(_) => serde_json::from_str(&line).map_err(|e| LinkError { message: format!("unreadable answer from Kanvas: {e}"), retry: false }),
            // Batas waktu: perintah mungkin sudah berjalan, jangan diulang.
            Err(e) if matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {
                Err(LinkError { message: "Kanvas didn't answer in time (it may be busy, showing a dialog, or minimized).".into(), retry: false })
            }
            Err(e) => Err(lost(e)),
        }
    }
}

fn connect() -> Result<Link, String> {
    let bytes = std::fs::read(data_dir().join("remote.json")).map_err(|_| "Kanvas isn't running")?;
    let info: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let port = info["port"].as_u64().filter(|p| *p <= u16::MAX as u64).ok_or("bad remote.json")? as u16;
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(90)));
    let writer = stream.try_clone().map_err(|e| e.to_string())?;
    let mut link = Link { reader: BufReader::new(stream), writer };
    let hello = link.exchange(&json!({ "token": info["token"] })).map_err(|e| e.message)?;
    if hello["ok"] != json!(true) {
        return Err("Kanvas rejected the connection".into());
    }
    Ok(link)
}

/// Jalankan aplikasi Kanvas (file di sebelah server ini), terlepas dari proses ini.
fn launch() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let app = exe.with_file_name(if cfg!(windows) { "kanvas.exe" } else { "kanvas" });
    if !app.exists() {
        return Err(format!("Kanvas isn't running and {} doesn't exist. Start Kanvas manually.", app.display()));
    }
    let mut cmd = std::process::Command::new(&app);
    // Jangan wariskan stdin/stdout: itu saluran MCP.
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: aplikasi tetap hidup setelah server ini berhenti.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    cmd.spawn().map(|_| ()).map_err(|e| format!("couldn't start {}: {e}", app.display()))
}

fn ensure(slot: &mut Option<Link>) -> Result<&mut Link, String> {
    if slot.is_none() {
        let link = match connect() {
            Ok(link) => link,
            Err(_) => {
                launch()?;
                let start = Instant::now();
                loop {
                    std::thread::sleep(Duration::from_millis(300));
                    match connect() {
                        Ok(link) => break link,
                        Err(e) if start.elapsed() > Duration::from_secs(25) => return Err(format!("Kanvas was started but isn't answering: {e}")),
                        Err(_) => {}
                    }
                }
            }
        };
        *slot = Some(link);
    }
    slot.as_mut().ok_or_else(|| "no connection".to_owned())
}

fn call(slot: &mut Option<Link>, cmd: &str, args: &Value) -> Result<Value, String> {
    let msg = json!({ "cmd": cmd, "args": args });
    for attempt in 0..2 {
        match ensure(slot)?.exchange(&msg) {
            Ok(v) => return Ok(v),
            Err(e) => {
                *slot = None;
                // Sambungan lama putus (mis. Kanvas ditutup lalu dibuka lagi): sambung ulang sekali.
                if !e.retry || attempt == 1 {
                    return Err(e.message);
                }
            }
        }
    }
    Err("no connection".into())
}

fn tool_call(slot: &mut Option<Link>, tools: &Value, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or_default();
    let error = |text: String| json!({ "content": [{ "type": "text", "text": text }], "isError": true });
    if !tools.as_array().is_some_and(|t| t.iter().any(|t| t["name"] == json!(name))) {
        return error(format!("unknown tool \"{name}\""));
    }
    let args = match &params["arguments"] {
        Value::Null => json!({}),
        other => other.clone(),
    };
    match call(slot, name, &args) {
        Ok(mut v) if v["ok"] == json!(true) => {
            let mut content = vec![json!({ "type": "text", "text": v["result"].to_string() })];
            if let Value::String(data) = v["image"].take() {
                content.push(json!({ "type": "image", "data": data, "mimeType": "image/png" }));
            }
            json!({ "content": content })
        }
        Ok(v) => error(v["error"].as_str().unwrap_or("Kanvas reported an error").to_owned()),
        Err(e) => error(e),
    }
}

fn main() {
    let tools: Value = serde_json::from_str(TOOLS).expect("mcp_tools.json is valid JSON");
    let mut slot: Option<Link> = None;
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(msg) = serde_json::from_str::<Value>(line.trim_start_matches('\u{feff}')) else { continue };
        let Some(method) = msg["method"].as_str() else { continue };
        let result: Result<Value, (i64, &str)> = match method {
            "initialize" => {
                let asked = msg["params"]["protocolVersion"].as_str().unwrap_or_default();
                let version = VERSIONS.iter().find(|v| **v == asked).unwrap_or(&VERSIONS[0]);
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "kanvas", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tools })),
            "tools/call" => Ok(tool_call(&mut slot, &tools, &msg["params"])),
            _ => Err((-32601, "method not found")),
        };
        // Notifikasi (tanpa id) tidak dijawab.
        let Some(id) = msg.get("id").filter(|id| !id.is_null()) else { continue };
        let reply = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
        };
        if writeln!(out, "{reply}").and_then(|_| out.flush()).is_err() {
            break;
        }
    }
}
