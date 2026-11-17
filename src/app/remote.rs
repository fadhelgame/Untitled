//! Remote control: perintah JSON lewat soket lokal (127.0.0.1), dipakai server MCP `kanvas-mcp`
//! supaya asisten AI bisa membaca dan mengubah dokumen yang sedang terbuka.
//!
//! Protokol: satu objek JSON per baris. Baris pertama harus `{"token": …}` (token acak yang
//! ditulis ke `remote.json` di folder data aplikasi); setelah itu `{"cmd": …, "args": {…}}`,
//! dijawab `{"ok": true, "result": …}` atau `{"ok": false, "error": …}`.
//!
//! Konvensi koordinat di sisi remote mengikuti Figma: posisi anak frame relatif terhadap pojok
//! kiri-atas frame induknya (di dokumen sendiri semuanya koordinat dunia).

use super::home::data_dir;
use super::pen::{set_world_points, world_points};
use super::render::{self, PenPt};
use super::{blend, App, Drag};
use crate::doc::{b64, Doc, Gradient, Kind, Node, Stop};
use crate::export;
use crate::layout::{self, Align, AutoLayout, Children, Constraint, Dir, Sizing};
use crate::fonts::CatalogState;
use eframe::egui;
use serde_json::{json, Map, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Batas tunggu unduhan font sebelum teks diukur / gambar dirender dengan font cadangan.
const FONT_WAIT: Duration = Duration::from_secs(8);
/// Di atas jumlah ini `get_document` hanya mengirim node level atas (tanpa anak).
const DEEP_LIMIT: usize = 400;

type FontKey = (String, u16, bool);

struct Request {
    cmd: String,
    args: Value,
    reply: mpsc::Sender<Value>,
}

/// Pekerjaan yang jawabannya menunggu font selesai diunduh.
enum After {
    /// Ukur ulang teks lalu laporkan node (`key` = "created" / "updated").
    Nodes { key: &'static str, ids: Vec<u64>, warnings: Vec<String> },
    Shot { id: Option<u64>, max: f32 },
    Export { id: Option<u64>, path: PathBuf, scale: f32 },
}

enum Outcome {
    Now(Value),
    Later(After, Vec<FontKey>),
}

struct Waiting {
    reply: mpsc::Sender<Value>,
    after: After,
    fonts: Vec<FontKey>,
    deadline: Instant,
}

pub(super) struct Remote {
    rx: mpsc::Receiver<Request>,
    waiting: Vec<Waiting>,
    info: PathBuf,
    token: String,
}

fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    // `RandomState` diberi kunci acak dari OS; cukup untuk token sesi lokal.
    (0..2).map(|_| format!("{:016x}", std::collections::hash_map::RandomState::new().build_hasher().finish())).collect()
}

impl Remote {
    pub fn start(ctx: &egui::Context) -> Option<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        let token = token();
        let dir = data_dir();
        let info = dir.join("remote.json");
        let _ = std::fs::create_dir_all(&dir);
        let body = json!({ "port": port, "token": token, "pid": std::process::id() });
        std::fs::write(&info, body.to_string()).ok()?;
        let (tx, rx) = mpsc::channel();
        let (ctx, secret) = (ctx.clone(), token.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (tx, ctx, secret) = (tx.clone(), ctx.clone(), secret.clone());
                std::thread::spawn(move || serve(stream, &secret, tx, ctx));
            }
        });
        Some(Remote { rx, waiting: Vec::new(), info, token })
    }
}

impl Drop for Remote {
    fn drop(&mut self) {
        // Hapus hanya kalau file itu masih milik sesi ini (bukan instance Kanvas lain).
        if std::fs::read_to_string(&self.info).is_ok_and(|s| s.contains(&self.token)) {
            let _ = std::fs::remove_file(&self.info);
        }
    }
}

fn serve(stream: TcpStream, token: &str, tx: mpsc::Sender<Request>, ctx: egui::Context) {
    let Ok(mut out) = stream.try_clone() else { return };
    let _ = stream.set_nodelay(true);
    let mut lines = BufReader::new(stream).lines();
    let authed = lines
        .next()
        .and_then(Result::ok)
        .and_then(|l| serde_json::from_str::<Value>(&l).ok())
        .is_some_and(|v| v["token"].as_str() == Some(token));
    if !authed {
        let _ = writeln!(out, "{}", fail("bad token"));
        return;
    }
    if writeln!(out, "{}", json!({ "ok": true })).is_err() {
        return;
    }
    for line in lines {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(mut v) => {
                let (reply, answer) = mpsc::channel();
                let req = Request { cmd: v["cmd"].as_str().unwrap_or_default().to_owned(), args: v["args"].take(), reply };
                if tx.send(req).is_err() {
                    return;
                }
                ctx.request_repaint();
                answer
                    .recv_timeout(Duration::from_secs(60))
                    .unwrap_or_else(|_| fail("Kanvas didn't answer in time (it may be busy, showing a dialog, or minimized)."))
            }
            Err(e) => fail(&format!("invalid JSON: {e}")),
        };
        if writeln!(out, "{reply}").is_err() {
            return;
        }
    }
}

fn ok(result: Value) -> Value {
    json!({ "ok": true, "result": result })
}

fn fail(error: &str) -> Value {
    json!({ "ok": false, "error": error })
}

// ---------------------------------------------------------------- JSON → nilai

fn num(key: &str, v: &Value) -> Result<f32, String> {
    v.as_f64().map(|x| x as f32).filter(|x| x.is_finite()).ok_or_else(|| format!("`{key}` must be a number"))
}

fn flag(key: &str, v: &Value) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| format!("`{key}` must be true or false"))
}

fn string(key: &str, v: &Value) -> Result<String, String> {
    v.as_str().map(str::to_owned).ok_or_else(|| format!("`{key}` must be a string"))
}

fn id_list(v: Option<&Value>) -> Result<Vec<u64>, String> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(v) => v
            .as_array()
            .and_then(|a| a.iter().map(Value::as_u64).collect::<Option<Vec<u64>>>())
            .ok_or_else(|| "`ids` must be an array of node ids".to_owned()),
    }
}

fn color(v: &Value) -> Result<[u8; 4], String> {
    if let Some(a) = v.as_array() {
        let ch: Option<Vec<u8>> = a.iter().map(|x| x.as_u64().filter(|x| *x <= 255).map(|x| x as u8)).collect();
        return match ch.as_deref() {
            Some([r, g, b]) => Ok([*r, *g, *b, 255]),
            Some([r, g, b, a]) => Ok([*r, *g, *b, *a]),
            _ => Err("a color array must be [r, g, b] or [r, g, b, a] with values 0–255".into()),
        };
    }
    let s = v.as_str().ok_or("a color must be a string like \"#RRGGBB\" or \"#RRGGBBAA\"")?.trim();
    match s.to_ascii_lowercase().as_str() {
        "white" => return Ok([255, 255, 255, 255]),
        "black" => return Ok([0, 0, 0, 255]),
        "transparent" => return Ok([0, 0, 0, 0]),
        _ => {}
    }
    let h = s.trim_start_matches('#');
    let bad = || format!("invalid color \"{s}\"; use \"#RRGGBB\" or \"#RRGGBBAA\"");
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(bad());
    }
    let digits: Vec<u8> = h.bytes().map(|b| (b as char).to_digit(16).unwrap_or(0) as u8).collect();
    match digits.len() {
        3 | 4 => Ok([digits[0] * 17, digits[1] * 17, digits[2] * 17, digits.get(3).map_or(255, |d| d * 17)]),
        6 | 8 => {
            let pair = |i: usize| digits[i] * 16 + digits[i + 1];
            Ok([pair(0), pair(2), pair(4), if digits.len() == 8 { pair(6) } else { 255 }])
        }
        _ => Err(bad()),
    }
}

fn hex(c: [u8; 4]) -> String {
    match c[3] {
        255 => format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2]),
        a => format!("#{:02x}{:02x}{:02x}{a:02x}", c[0], c[1], c[2]),
    }
}

/// `fill`: null, warna, atau objek gradasi. Hasil: (fill, gradient) untuk node.
fn paint(v: &Value) -> Result<(Option<[u8; 4]>, Option<Gradient>), String> {
    let Some(g) = v.as_object() else {
        return if v.is_null() { Ok((None, None)) } else { Ok((Some(color(v)?), None)) };
    };
    let radial = match g.get("type").and_then(Value::as_str).unwrap_or("linear") {
        "linear" => false,
        "radial" => true,
        other => return Err(format!("unknown gradient type \"{other}\"; use \"linear\" or \"radial\"")),
    };
    let angle = g.get("angle").map(|a| num("angle", a)).transpose()?.unwrap_or(90.0);
    let list = g.get("stops").and_then(Value::as_array).filter(|l| l.len() >= 2).ok_or("a gradient needs `stops` with at least 2 entries")?;
    let mut stops = Vec::with_capacity(list.len());
    for s in list {
        let pos = num("pos", s.get("pos").unwrap_or(&Value::Null))?.clamp(0.0, 1.0);
        stops.push(Stop { pos, color: color(s.get("color").unwrap_or(&Value::Null))? });
    }
    let first = stops[0].color;
    Ok((Some(first), Some(Gradient { radial, angle, stops, line: None, radial_line: None })))
}

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Frame => "frame",
        Kind::Rect => "rect",
        Kind::Ellipse => "ellipse",
        Kind::Line => "line",
        Kind::Text => "text",
        Kind::Path => "path",
        Kind::Image => "image",
    }
}

fn parse_kind(s: &str) -> Option<Kind> {
    Some(match s.to_ascii_lowercase().as_str() {
        "frame" => Kind::Frame,
        "rect" | "rectangle" => Kind::Rect,
        "ellipse" | "circle" => Kind::Ellipse,
        "line" => Kind::Line,
        "text" => Kind::Text,
        "path" | "vector" => Kind::Path,
        "image" => Kind::Image,
        _ => return None,
    })
}

// ---------------------------------------------------------------- node → JSON

fn r2(v: f32) -> Value {
    json!((v as f64 * 100.0).round() / 100.0)
}

/// Ringkasan node (id, nama, posisi, ukuran) untuk jawaban create/update.
fn brief(n: &Node, o: (f32, f32)) -> Value {
    let mut v = json!({ "id": n.id, "kind": kind_name(n.kind), "name": n.name, "x": r2(n.x - o.0), "y": r2(n.y - o.1), "w": r2(n.w), "h": r2(n.h) });
    if !n.children.is_empty() {
        v["children"] = n.children.iter().map(|c| brief(c, (n.x, n.y))).collect();
    }
    v
}

const POSITIONED: &str = "x / y of an item of an auto-layout frame are set by that frame and were ignored (items follow the order of `children`; set `absolute: true` on a node to place it by hand).";

/// Was this node (as specified) given a place of its own, without being taken out of the layout?
fn placed_by_hand(spec: &Value) -> bool {
    (spec.get("x").is_some() || spec.get("y").is_some()) && spec.get("absolute").and_then(Value::as_bool) != Some(true)
}

fn parse_sizing(key: &str, v: &Value) -> Result<Sizing, String> {
    let name = string(key, v)?;
    Sizing::parse(&name).ok_or_else(|| format!("unknown `{key}` \"{name}\"; use fixed, hug (a frame with `layout`: as large as its items) or fill (an item of an auto-layout frame: the room that is left)"))
}

/// The auto layout of a frame as the remote API has it, with the frame's own sizes.
fn layout_json(l: &AutoLayout, n: &Node) -> Value {
    let mut v = json!({
        "direction": if l.dir == Dir::Horizontal { "horizontal" } else { "vertical" },
        "gap": r2(l.gap),
        "padding": l.pad.iter().map(|p| r2(*p)).collect::<Vec<_>>(),
        "align": l.align.key(),
        "cross_align": l.cross.key(),
        "width": n.size_h.key(),
        "height": n.size_v.key(),
    });
    if l.wrap {
        v["wrap"] = json!(true);
        v["row_gap"] = r2(l.row_gap);
    }
    v
}

/// Merge the keys of a `layout` object into the auto layout of frame `n` (a new one is a row
/// without gap or padding). Result: whether `width` / `height` were among them.
fn set_layout(n: &mut Node, v: &Value) -> Result<(bool, bool), String> {
    let m = v.as_object().ok_or("`layout` must be an object like {\"direction\": \"vertical\", \"gap\": 12, \"padding\": 16}, or null to remove the auto layout")?;
    let mut l = n.layout.clone().unwrap_or_default();
    let mut sized = (false, false);
    let place = |key: &str, v: &Value, between: bool| -> Result<Align, String> {
        let name = string(key, v)?;
        Align::parse(&name).filter(|a| between || *a != Align::Between).ok_or_else(|| format!("unknown `{key}` \"{name}\"; use start, center{}", if between { ", end or space_between" } else { " or end" }))
    };
    for (k, v) in m {
        match k.as_str() {
            "direction" => {
                l.dir = match string(k, v)?.as_str() {
                    "horizontal" => Dir::Horizontal,
                    "vertical" => Dir::Vertical,
                    other => return Err(format!("unknown direction \"{other}\"; use horizontal or vertical")),
                }
            }
            "wrap" => l.wrap = flag(k, v)?,
            // "auto" is how the Design panel names space between.
            "gap" if v.as_str() == Some("auto") => l.align = Align::Between,
            "gap" => l.gap = num(k, v)?.max(0.0),
            "row_gap" => l.row_gap = num(k, v)?.max(0.0),
            "padding" => {
                let side = |v: &Value| num("padding", v).map(|p| p.max(0.0));
                l.pad = match (v, v.as_array().map(Vec::as_slice)) {
                    (_, Some([h, v])) => [side(h)?, side(v)?, side(h)?, side(v)?],
                    (_, Some([a, b, c, d])) => [side(a)?, side(b)?, side(c)?, side(d)?],
                    (Value::Object(o), _) => {
                        let mut pad = l.pad;
                        for (name, v) in o {
                            let i = ["left", "top", "right", "bottom"].iter().position(|s| s == name).ok_or_else(|| format!("unknown side `{name}` in `padding`; use left, top, right and bottom"))?;
                            pad[i] = side(v)?;
                        }
                        pad
                    }
                    (Value::Number(_), _) => [side(v)?; 4],
                    _ => return Err("`padding` must be a number (all sides), [horizontal, vertical], [left, top, right, bottom] or {left, top, right, bottom}".into()),
                }
            }
            "align" => l.align = place(k, v, true)?,
            "cross_align" => l.cross = place(k, v, false)?,
            "width" => (n.size_h, sized.0) = (parse_sizing(k, v)?, true),
            "height" => (n.size_v, sized.1) = (parse_sizing(k, v)?, true),
            other => return Err(format!("unknown key `{other}` in `layout`; use direction, wrap, gap, row_gap, padding, align, cross_align, width and height")),
        }
    }
    n.layout = Some(l);
    Ok(sized)
}

/// Properti node; nilai bawaan dilewati supaya ringkas. `deep` = sertakan anak dan titik path.
fn describe(n: &Node, o: (f32, f32), deep: bool) -> Value {
    let mut m = Map::new();
    let mut put = |k: &str, v: Value| {
        m.insert(k.to_owned(), v);
    };
    put("id", json!(n.id));
    put("kind", json!(kind_name(n.kind)));
    put("name", json!(n.name));
    put("x", r2(n.x - o.0));
    put("y", r2(n.y - o.1));
    put("w", r2(n.w));
    put("h", r2(n.h));
    if let Some(c) = n.fill {
        put(
            "fill",
            match &n.gradient {
                Some(g) => json!({
                    "type": if g.radial { "radial" } else { "linear" },
                    "angle": r2(g.angle),
                    "stops": g.sorted().iter().map(|s| json!({ "pos": r2(s.pos), "color": hex(s.color) })).collect::<Vec<_>>(),
                }),
                None => json!(hex(c)),
            },
        );
    }
    if let Some(c) = n.stroke.filter(|_| n.stroke_w > 0.0) {
        put("stroke", json!(hex(c)));
        put("stroke_width", r2(n.stroke_w));
    }
    if n.radius != 0.0 {
        put("radius", r2(n.radius));
    }
    if let Some(s) = n.shadow {
        put("shadow", json!({ "x": r2(s.x), "y": r2(s.y), "blur": r2(s.blur), "color": hex(s.color) }));
    }
    if n.blur != 0.0 {
        put("blur", r2(n.blur));
    }
    if n.rotation != 0.0 {
        put("rotation", r2(n.rotation));
    }
    if n.flips().0 {
        put("flip_h", json!(true));
    }
    if n.flips().1 {
        put("flip_v", json!(true));
    }
    if n.opacity != 1.0 {
        put("opacity", r2(n.opacity));
    }
    if !n.visible {
        put("visible", json!(false));
    }
    if n.locked {
        put("locked", json!(true));
    }
    if n.tag != 0 {
        put("layer_color", json!(crate::doc::LAYER_COLORS[(n.tag as usize).min(crate::doc::LAYER_COLORS.len() - 1)].0.to_lowercase()));
    }
    if n.blend != 0 {
        put("blend", json!(blend::CSS[(n.blend as usize).min(blend::CSS.len() - 1)]));
    }
    if n.group != 0 {
        put("group", json!(n.group));
    }
    if !n.constrain_h.is_default() || !n.constrain_v.is_default() {
        put("constraints", json!({ "horizontal": n.constrain_h.key(true), "vertical": n.constrain_v.key(false) }));
    }
    match &n.layout {
        Some(l) => put("layout", layout_json(l, n)),
        None if n.size_h != Sizing::Fixed || n.size_v != Sizing::Fixed => put("sizing", json!({ "horizontal": n.size_h.key(), "vertical": n.size_v.key() })),
        None => {}
    }
    if n.absolute {
        put("absolute", json!(true));
    }
    if n.component {
        put("component", json!(true));
    }
    if let Some(main) = n.instance_of {
        put("instance_of", json!(main));
    }
    if let Some(source) = n.source {
        put("source", json!(source));
    }
    match n.kind {
        Kind::Text => {
            put("text", json!(n.text));
            put("font", json!(n.font));
            put("weight", json!(n.weight));
            put("font_size", r2(n.font_size));
            if n.italic {
                put("italic", json!(true));
            }
            if n.line_height != 0.0 {
                put("line_height", r2(n.line_height));
            }
            if n.letter_spacing != 0.0 {
                put("letter_spacing", r2(n.letter_spacing));
            }
            if n.align != 0 {
                put("align", json!(["left", "center", "right", "justify"][n.align.min(3) as usize]));
            }
            if n.wrap {
                put("wrap", json!(true));
            }
            if n.underline {
                put("underline", json!(true));
            }
            if n.strike {
                put("strikethrough", json!(true));
            }
            if n.list != 0 {
                put("list", json!(if n.list == 1 { "bullet" } else { "numbered" }));
            }
            if n.fixed_h {
                put("fixed_height", json!(true));
            }
            if n.valign != 0 {
                put("valign", json!(["top", "middle", "bottom"][n.valign.min(2) as usize]));
            }
            if !n.spans.is_empty() {
                let spans: Vec<Value> = n
                    .spans
                    .iter()
                    .map(|s| {
                        let mut span = json!({
                            "start": s.start, "end": s.end, "font": s.style.font, "weight": s.style.weight,
                            "italic": s.style.italic, "size": r2(s.style.size), "fill": hex(s.style.fill),
                        });
                        if s.style.underline != n.underline {
                            span["underline"] = json!(s.style.underline);
                        }
                        if s.style.strike != n.strike {
                            span["strikethrough"] = json!(s.style.strike);
                        }
                        span
                    })
                    .collect();
                put("spans", json!(spans));
            }
        }
        Kind::Path => {
            put("closed", json!(n.closed));
            if deep {
                let pair = |h: [f32; 2]| json!([r2(h[0]), r2(h[1])]);
                let points: Vec<Value> = world_points(n)
                    .iter()
                    .map(|p| {
                        let mut q = json!({ "x": r2(p.x - o.0), "y": r2(p.y - o.1) });
                        if p.hin != [0.0, 0.0] {
                            q["in"] = pair(p.hin);
                        }
                        if p.hout != [0.0, 0.0] {
                            q["out"] = pair(p.hout);
                        }
                        q
                    })
                    .collect();
                put("points", json!(points));
            } else {
                put("point_count", json!(n.points.len()));
            }
        }
        Kind::Frame => {
            if deep {
                put("children", n.children.iter().map(|c| describe(c, (n.x, n.y), true)).collect());
            } else {
                put("child_count", json!(n.children.len()));
            }
        }
        _ => {}
    }
    Value::Object(m)
}

fn fonts_of(n: &Node, out: &mut Vec<FontKey>) {
    if n.kind == Kind::Text {
        let mut add = |font: &str, weight: u16, italic: bool| {
            if !out.iter().any(|f| f.0 == font && f.1 == weight && f.2 == italic) {
                out.push((font.to_owned(), weight, italic));
            }
        };
        add(&n.font, n.weight, n.italic);
        for s in &n.spans {
            add(&s.style.font, s.style.weight, s.style.italic);
        }
    }
    for c in &n.children {
        fonts_of(c, out);
    }
}

/// Catatan sampingan saat menerapkan properti.
#[derive(Default)]
struct Notes {
    warnings: Vec<String>,
    /// Ukuran alami gambar yang baru dipasang.
    natural: Option<(f32, f32)>,
}

// ---------------------------------------------------------------- app

impl App {
    /// Dipanggil tiap frame: jalankan perintah yang masuk dan selesaikan yang menunggu font.
    pub(super) fn remote_poll(&mut self, ctx: &egui::Context) {
        let Some(mut remote) = self.remote.take() else { return };
        // Pengguna sedang menyeret atau menggambar path: tunda supaya tidak saling menimpa.
        let busy = !matches!(self.drag, Drag::None) || self.ext.pen.is_some();
        if busy {
            ctx.request_repaint_after(Duration::from_millis(100));
        } else {
            while let Ok(req) = remote.rx.try_recv() {
                let outcome = self.remote_run(&req.cmd, &req.args);
                // Instances follow at once, so the next command reads a document that is in line.
                self.settle_doc();
                match outcome {
                    Ok(Outcome::Now(v)) => {
                        let _ = req.reply.send(ok(v));
                    }
                    Ok(Outcome::Later(after, fonts)) => {
                        remote.waiting.push(Waiting { reply: req.reply, after, fonts, deadline: Instant::now() + FONT_WAIT });
                    }
                    Err(e) => {
                        let _ = req.reply.send(fail(&e));
                    }
                }
            }
        }
        let now = Instant::now();
        for w in std::mem::take(&mut remote.waiting) {
            let ready = w.fonts.iter().all(|(f, wt, it)| self.fonts.settled(f, *wt, *it));
            if ready || now >= w.deadline {
                let answer = self.remote_finish(ctx, w.after, &w.fonts).unwrap_or_else(|e| fail(&e));
                let _ = w.reply.send(answer);
            } else {
                remote.waiting.push(w);
            }
        }
        if !remote.waiting.is_empty() {
            ctx.request_repaint_after(Duration::from_millis(30));
        }
        self.remote = Some(remote);
    }

    /// `--bench`: run one command the way `remote_poll` does, without the socket (which is off
    /// in that mode). Result: the answer as text; for created / updated nodes just their ids.
    pub(super) fn remote_bench(&mut self, cmd: &str, args: &Value) -> String {
        let outcome = self.remote_run(cmd, args);
        self.settle_doc();
        match outcome {
            Ok(Outcome::Now(v)) => v.to_string(),
            Ok(Outcome::Later(After::Nodes { ids, .. }, _)) => format!("ids {ids:?}"),
            Ok(Outcome::Later(..)) => "(answers later)".into(),
            Err(e) => format!("ERROR: {e}"),
        }
    }

    fn remote_run(&mut self, cmd: &str, args: &Value) -> Result<Outcome, String> {
        // Perintah yang mengubah dokumen: akhiri dulu mode edit teks / titik path.
        if matches!(cmd, "create_nodes" | "update_nodes" | "delete_nodes" | "arrange" | "history" | "file" | "pages") {
            self.finish_text_edit();
            self.ext.path_edit = None;
        }
        match cmd {
            "get_document" => self.remote_get(args).map(Outcome::Now),
            "create_nodes" => self.remote_create(args),
            "update_nodes" => self.remote_update(args),
            "delete_nodes" => self.remote_delete(args).map(Outcome::Now),
            "arrange" => self.remote_arrange(args).map(Outcome::Now),
            "select_nodes" => self.remote_select(args).map(Outcome::Now),
            "history" => self.remote_history(args).map(Outcome::Now),
            "pages" => self.remote_pages(args).map(Outcome::Now),
            "library" => self.remote_library(args).map(Outcome::Now),
            "file" => self.remote_file(args).map(Outcome::Now),
            "list_fonts" => self.remote_fonts(args).map(Outcome::Now),
            "screenshot" => {
                let id = self.target(args)?;
                let max = args.get("max_size").map(|v| num("max_size", v)).transpose()?.unwrap_or(1024.0).clamp(64.0, 2048.0);
                let fonts = self.target_fonts(id);
                Ok(Outcome::Later(After::Shot { id, max }, fonts))
            }
            "export" => {
                let id = self.target(args)?;
                let path = PathBuf::from(string("path", args.get("path").unwrap_or(&Value::Null))?);
                if !path.is_absolute() {
                    return Err("`path` must be an absolute file path".into());
                }
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                if !matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "svg") {
                    return Err("`path` must end in .png, .jpg or .svg".into());
                }
                let scale = args.get("scale").map(|v| num("scale", v)).transpose()?.unwrap_or(1.0).clamp(0.1, 16.0);
                let fonts = self.target_fonts(id);
                Ok(Outcome::Later(After::Export { id, path, scale }, fonts))
            }
            other => Err(format!("unknown command \"{other}\"")),
        }
    }

    /// `id` opsional di args: node yang dirender (None = seluruh dokumen).
    fn target(&self, args: &Value) -> Result<Option<u64>, String> {
        match args.get("id") {
            None | Some(Value::Null) => Ok(None),
            Some(v) => {
                let id = v.as_u64().ok_or("`id` must be a node id")?;
                self.doc.find(id).map(|_| Some(id)).ok_or_else(|| format!("node {id} not found"))
            }
        }
    }

    /// Mulai unduh font yang dipakai target supaya hasil render memakai font aslinya.
    fn target_fonts(&mut self, id: Option<u64>) -> Vec<FontKey> {
        let mut fonts = Vec::new();
        match id.and_then(|id| self.doc.find(id)) {
            Some(n) => fonts_of(n, &mut fonts),
            None => self.doc.nodes.iter().for_each(|n| fonts_of(n, &mut fonts)),
        }
        for (f, w, i) in &fonts {
            self.fonts.resolve(f, *w, *i);
        }
        fonts
    }

    /// Mulai unduh font teks pada node-node ini; nama/varian yang tidak dikenal jadi peringatan.
    fn want_fonts(&mut self, ids: &[u64], warnings: &mut Vec<String>) -> Vec<FontKey> {
        let mut fonts = Vec::new();
        for n in ids.iter().filter_map(|id| self.doc.find(*id)) {
            fonts_of(n, &mut fonts);
        }
        for (font, weight, italic) in &fonts {
            if matches!(self.fonts.catalog_state, CatalogState::Ready) {
                match self.fonts.family(font) {
                    None => {
                        let lower = font.to_lowercase();
                        let word = lower.split(' ').next().unwrap_or_default();
                        let near: Vec<&str> =
                            self.fonts.catalog.iter().filter(|f| !word.is_empty() && f.lower.contains(word)).take(5).map(|f| f.name.as_str()).collect();
                        let hint = if near.is_empty() { "use list_fonts to search".to_owned() } else { format!("did you mean: {}?", near.join(", ")) };
                        warnings.push(format!("Font \"{font}\" isn't in Google Fonts, so a fallback font is shown ({hint})."));
                    }
                    Some(fam) if !fam.variants.contains(&(*weight, *italic)) => {
                        let have: Vec<String> = fam.variants.iter().map(|(w, i)| format!("{w}{}", if *i { " italic" } else { "" })).collect();
                        warnings.push(format!(
                            "\"{font}\" has no {weight}{} variant, so a fallback font is shown. Available: {}.",
                            if *italic { " italic" } else { "" },
                            have.join(", ")
                        ));
                    }
                    Some(_) => {}
                }
            }
            self.fonts.resolve(font, *weight, *italic);
        }
        fonts
    }

    /// Ukur teks dengan layout yang sama dengan canvas dan simpan ukurannya di node.
    fn measure_text(&mut self, ctx: &egui::Context, id: u64) {
        let Some(n) = self.doc.find(id).filter(|n| n.kind == Kind::Text && !n.text.is_empty()).cloned() else { return };
        let f = (200.0 / n.max_size()).min(1.0);
        let layout = render::layout_text(ctx, &n, &n.text, &mut self.fonts, f, 1.0, true);
        let (w, h) = (if n.wrap { n.w } else { (layout.width / f).max(2.0) }, (layout.height / f).max(1.0));
        if let Some(n) = self.doc.find_mut(id) {
            // (A height that was set by hand stays.)
            (n.w, n.h) = (w, if n.fixed_h { n.h } else { h });
        }
    }

    fn origin_of(&self, id: u64) -> (f32, f32) {
        self.doc.parent_of(id).and_then(|p| self.doc.find(p)).map_or((0.0, 0.0), |p| (p.x, p.y))
    }

    fn remote_finish(&mut self, ctx: &egui::Context, after: After, fonts: &[FontKey]) -> Result<Value, String> {
        match after {
            After::Nodes { key, ids, mut warnings } => {
                for (f, w, i) in fonts {
                    let known = warnings.iter().any(|x| x.contains(&format!("\"{f}\"")));
                    if let Some(state) = self.fonts.status(f, *w, *i).filter(|_| !known) {
                        warnings.push(format!("Font \"{f}\" {w}: {state} (text sizes were measured with a fallback font)."));
                    }
                }
                let mut texts = Vec::new();
                for n in ids.iter().filter_map(|id| self.doc.find(*id)) {
                    n.walk(&mut |c| texts.push(c.id));
                }
                for id in texts {
                    self.measure_text(ctx, id);
                }
                // A text that was measured has a new size for the auto layout it sits in, and
                // in a main component for its instances too: the answer has the boxes as they end up.
                self.components.recheck();
                self.settle_doc();
                let list: Vec<Value> = ids.iter().filter_map(|id| self.doc.find(*id).map(|n| brief(n, self.origin_of(*id)))).collect();
                let mut result = json!({ key: list });
                if !warnings.is_empty() {
                    result["warnings"] = json!(warnings);
                }
                Ok(ok(result))
            }
            After::Shot { id, max } => {
                let (svg, w, h) = export::to_svg(&self.doc, id, &mut self.fonts, ctx).ok_or("nothing to render: the document is empty")?;
                let scale = (max / w.max(h)).min(2.0);
                let (png, pw, ph) = export::render_png(&svg, w, h, scale)?;
                let mut answer = ok(json!({ "width": pw, "height": ph, "scale": r2(scale), "source_size": [r2(w), r2(h)] }));
                answer["image"] = json!(b64::encode(&png));
                Ok(answer)
            }
            After::Export { id, path, scale } => {
                let (svg, w, h) = export::to_svg(&self.doc, id, &mut self.fonts, ctx).ok_or("nothing to export: the document is empty")?;
                let ext = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
                match ext.as_str() {
                    "svg" => std::fs::write(&path, svg).map_err(|e| e.to_string())?,
                    other => export::save_raster(&svg, w, h, scale, other != "png", &path)?,
                }
                self.status = format!("Remote: exported {}", path.display());
                Ok(ok(json!({ "path": path, "width": r2(w * scale), "height": r2(h * scale) })))
            }
        }
    }

    // ------------------------------------------------------------ baca

    fn remote_get(&self, args: &Value) -> Result<Value, String> {
        let ids = id_list(args.get("ids"))?;
        let mut out = json!({
            "file": self.path,
            "unsaved_changes": self.dirty,
            "selection": self.sel,
        });
        if self.doc.page_count() > 1 {
            out["pages"] = self.pages_json();
            out["note_pages"] = json!("`nodes` are the layers of the current page only; use the pages tool to switch.");
        }
        if !self.doc.group_names.is_empty() {
            out["groups"] = self.doc.group_names.iter().map(|(g, name)| (g.to_string(), json!(name))).collect::<Map<_, _>>().into();
        }
        if ids.is_empty() {
            let deep = self.doc.count() <= DEEP_LIMIT;
            if !deep {
                out["note"] = json!("Large document: frame children are omitted (see child_count). Pass `ids` to read specific nodes in full.");
            }
            out["nodes"] = self.doc.nodes.iter().map(|n| describe(n, (0.0, 0.0), deep)).collect();
        } else {
            let mut nodes = Vec::with_capacity(ids.len());
            for id in ids {
                let n = self.doc.find(id).ok_or_else(|| format!("node {id} not found"))?;
                let mut v = describe(n, self.origin_of(id), true);
                if let Some(p) = self.doc.parent_of(id) {
                    v["parent"] = json!(p);
                }
                nodes.push(v);
            }
            out["nodes"] = json!(nodes);
        }
        Ok(out)
    }

    fn remote_fonts(&self, args: &Value) -> Result<Value, String> {
        match &self.fonts.catalog_state {
            CatalogState::Loading => return Err("the font catalog is still loading; try again in a moment".into()),
            CatalogState::Failed(e) => return Err(format!("the font catalog couldn't be loaded: {e}")),
            CatalogState::Ready => {}
        }
        let query = args.get("query").and_then(Value::as_str).unwrap_or_default().to_lowercase();
        let limit = args.get("limit").and_then(Value::as_u64).unwrap_or(30).clamp(1, 200) as usize;
        // Katalog sudah terurut dari yang paling populer.
        let fonts: Vec<Value> = self
            .fonts
            .catalog
            .iter()
            .filter(|f| query.is_empty() || f.lower.contains(&query) || f.category.to_lowercase().contains(&query))
            .take(limit)
            .map(|f| {
                let weights: Vec<u16> = f.variants.iter().filter(|v| !v.1).map(|v| v.0).collect();
                json!({ "name": f.name, "category": f.category, "weights": weights, "italic": f.variants.iter().any(|v| v.1) })
            })
            .collect();
        Ok(json!({ "fonts": fonts }))
    }

    // ------------------------------------------------------------ properti

    /// Terapkan properti dari `m` ke node. `o` = titik asal koordinat (pojok frame induk).
    fn apply(&mut self, n: &mut Node, m: &Map<String, Value>, o: (f32, f32), creating: bool, notes: &mut Notes) -> Result<(), String> {
        if let Some(v) = m.get("points") {
            if n.kind != Kind::Path {
                return Err("`points` only applies to path nodes".into());
            }
            let list = v.as_array().filter(|a| a.len() >= 2).ok_or("`points` must be an array of at least 2 points")?;
            let mut pts = Vec::with_capacity(list.len());
            for p in list {
                let handle = |key: &str| -> Result<[f32; 2], String> {
                    match p.get(key) {
                        None | Some(Value::Null) => Ok([0.0; 2]),
                        Some(h) => match h.as_array().map(Vec::as_slice) {
                            Some([dx, dy]) => Ok([num(key, dx)?, num(key, dy)?]),
                            _ => Err(format!("a point's `{key}` handle must be [dx, dy]")),
                        },
                    }
                };
                pts.push(PenPt {
                    x: o.0 + num("x", p.get("x").unwrap_or(&Value::Null))?,
                    y: o.1 + num("y", p.get("y").unwrap_or(&Value::Null))?,
                    hin: handle("in")?,
                    hout: handle("out")?,
                    r: None,
                });
            }
            set_world_points(n, &pts);
        }
        let had_layout = n.layout.is_some();
        for (k, v) in m {
            let k = k.as_str();
            match k {
                "id" | "points" | "spans" | "parent" => {}
                "layout" => {
                    if n.kind != Kind::Frame {
                        return Err("`layout` only applies to frames".into());
                    }
                    if n.instance_of.is_some() || n.source.is_some() {
                        return Err("the layout of an instance follows its main component; set `layout` there".into());
                    }
                    if v.is_null() {
                        n.layout = None;
                    } else {
                        set_layout(n, v)?;
                    }
                }
                "sizing" => {
                    let s = v.as_object().ok_or("`sizing` must be an object like {\"horizontal\": \"fill\", \"vertical\": \"fixed\"}")?;
                    for (axis, value) in s {
                        match axis.as_str() {
                            "horizontal" => n.size_h = parse_sizing(axis, value)?,
                            "vertical" => n.size_v = parse_sizing(axis, value)?,
                            other => return Err(format!("unknown key `{other}` in `sizing`; use horizontal and vertical")),
                        }
                    }
                }
                "absolute" => n.absolute = flag(k, v)?,
                "kind" | "children" | "instance_of" if creating => {}
                "instance_of" => return Err("`instance_of` can't be changed; create a new instance instead".into()),
                "component" => {
                    let on = flag(k, v)?;
                    if on && (n.kind != Kind::Frame || n.instance_of.is_some() || n.source.is_some()) {
                        return Err("only a frame that isn't an instance (or part of one) can be a main component; use arrange with action \"component\" to wrap other layers in one".into());
                    }
                    n.component = on;
                }
                "kind" => return Err("`kind` can't be changed; delete the node and create a new one".into()),
                "children" => return Err("`children` can't be set in update_nodes; use create_nodes with `parent`".into()),
                "name" => n.name = string(k, v)?,
                "x" => {
                    let dx = o.0 + num(k, v)? - n.x;
                    n.translate(dx, 0.0);
                }
                "y" => {
                    let dy = o.1 + num(k, v)? - n.y;
                    n.translate(0.0, dy);
                }
                "keep_children" if !creating => {
                    flag(k, v)?;
                }
                "constraints" => {
                    let c = v.as_object().ok_or("`constraints` must be an object like {\"horizontal\": \"left_right\", \"vertical\": \"top\"}")?;
                    for (axis, value) in c {
                        let horizontal = match axis.as_str() {
                            "horizontal" => true,
                            "vertical" => false,
                            other => return Err(format!("unknown key `{other}` in `constraints`; use horizontal and vertical")),
                        };
                        let name = string(axis, value)?;
                        let parsed = Constraint::parse(&name, horizontal).ok_or_else(|| {
                            let all: Vec<&str> = Constraint::ALL.iter().map(|c| c.key(horizontal)).collect();
                            format!("unknown {axis} constraint \"{name}\"; use one of: {}", all.join(", "))
                        })?;
                        if horizontal {
                            n.constrain_h = parsed;
                        } else {
                            n.constrain_v = parsed;
                        }
                    }
                }
                "w" => n.w = if n.kind == Kind::Line { num(k, v)? } else { num(k, v)?.max(1.0) },
                "h" => n.h = if n.kind == Kind::Line { num(k, v)? } else { num(k, v)?.max(1.0) },
                "fill" => (n.fill, n.gradient) = paint(v)?,
                "stroke" => {
                    n.stroke = if v.is_null() { None } else { Some(color(v)?) };
                    if n.stroke.is_some() && n.stroke_w <= 0.0 {
                        n.stroke_w = 1.0;
                    }
                }
                "stroke_width" => n.stroke_w = num(k, v)?.max(0.0),
                "radius" => n.radius = num(k, v)?.max(0.0),
                "shadow" => {
                    n.shadow = if v.is_null() {
                        None
                    } else {
                        let d = crate::doc::Shadow::default();
                        let f = |key: &str, default: f32| v.get(key).and_then(|x| x.as_f64()).map_or(default, |x| x as f32);
                        let color = match v.get("color") {
                            Some(c) => color(c)?,
                            None => d.color,
                        };
                        Some(crate::doc::Shadow { x: f("x", d.x), y: f("y", d.y), blur: f("blur", d.blur).max(0.0), color })
                    };
                }
                "blur" => n.blur = if v.is_null() { 0.0 } else { num(k, v)?.clamp(0.0, 500.0) },
                "rotation" => n.rotation = num(k, v)?,
                "flip_h" | "flip_v" => {
                    let on = flag(k, v)?;
                    // These mirror what a node shows in its box. A path or line has its points
                    // mirrored instead, and a frame takes its content along: both by `arrange`.
                    if matches!(n.kind, Kind::Path | Kind::Line | Kind::Frame) {
                        if on {
                            return Err(format!("`{k}` is a property of rects, ellipses, images and text; to flip a {}, use arrange with action \"{k}\"", n.kind.label().to_lowercase()));
                        }
                    } else if k == "flip_h" {
                        n.flip_h = on;
                    } else {
                        n.flip_v = on;
                    }
                }
                "opacity" => n.opacity = num(k, v)?.clamp(0.0, 1.0),
                "visible" => n.visible = flag(k, v)?,
                "locked" => n.locked = flag(k, v)?,
                "layer_color" => {
                    let name = if v.is_null() { "none".to_owned() } else { string(k, v)?.to_lowercase() };
                    let at = crate::doc::LAYER_COLORS.iter().position(|c| c.0.to_lowercase() == name);
                    n.tag = at.ok_or_else(|| format!("unknown layer color \"{name}\"; use one of: none, red, orange, yellow, green, blue, purple, pink, gray"))? as u8;
                }
                "blend" => {
                    let name = string(k, v)?.to_lowercase().replace('_', "-");
                    let at = blend::CSS.iter().position(|b| *b == name).or_else(|| blend::NAMES.iter().position(|b| b.to_lowercase() == name));
                    n.blend = at.ok_or_else(|| format!("unknown blend mode \"{name}\"; use one of: {}", blend::CSS.join(", ")))? as u8;
                }
                "text" => {
                    if n.kind != Kind::Text {
                        return Err("`text` only applies to text nodes".into());
                    }
                    let new = string(k, v)?;
                    let old = std::mem::take(&mut n.text);
                    if !m.contains_key("spans") {
                        n.sync_spans(&old, &new);
                    }
                    n.text = new;
                }
                "font" => n.font = string(k, v)?,
                "weight" => n.weight = v.as_u64().filter(|w| (1..=1000).contains(w)).ok_or("`weight` must be a number like 400 or 700")? as u16,
                "italic" => n.italic = flag(k, v)?,
                "font_size" => n.font_size = num(k, v)?.max(1.0),
                "line_height" => n.line_height = num(k, v)?.max(0.0),
                "letter_spacing" => n.letter_spacing = num(k, v)?,
                "align" => {
                    n.align = match string(k, v)?.as_str() {
                        "left" => 0,
                        "center" => 1,
                        "right" => 2,
                        "justify" => 3,
                        other => return Err(format!("unknown align \"{other}\"; use left, center, right or justify")),
                    }
                }
                "wrap" => n.wrap = flag(k, v)?,
                // For the whole text; `spans` (applied after these) can differ per range.
                "underline" => n.set_decoration(false, flag(k, v)?),
                "strikethrough" => n.set_decoration(true, flag(k, v)?),
                "list" => {
                    n.list = match if v.is_null() { "none".to_owned() } else { string(k, v)? }.as_str() {
                        "none" => 0,
                        "bullet" => 1,
                        "numbered" => 2,
                        other => return Err(format!("unknown list \"{other}\"; use none, bullet or numbered")),
                    }
                }
                "valign" => {
                    n.valign = match string(k, v)?.as_str() {
                        "top" => 0,
                        "middle" => 1,
                        "bottom" => 2,
                        other => return Err(format!("unknown valign \"{other}\"; use top, middle or bottom")),
                    }
                }
                "fixed_height" => {
                    if n.kind != Kind::Text {
                        return Err("`fixed_height` only applies to text nodes".into());
                    }
                    n.fixed_h = flag(k, v)?;
                }
                "closed" => n.closed = flag(k, v)?,
                "image_path" | "svg" => {
                    if n.kind != Kind::Image {
                        return Err(format!("`{k}` only applies to image nodes"));
                    }
                    let source = string(k, v)?;
                    let bytes = if k == "svg" {
                        source.into_bytes()
                    } else {
                        std::fs::read(&source).map_err(|e| format!("couldn't read \"{source}\": {e}"))?
                    };
                    let size = render::image_size(&bytes).filter(|s| s.0 > 0.0 && s.1 > 0.0);
                    notes.natural = Some(size.ok_or_else(|| format!("`{k}` isn't a readable image (PNG, JPEG, WebP, GIF, BMP or SVG)"))?);
                    n.image = Some(self.doc.add_asset(bytes));
                }
                other => return Err(format!("unknown property `{other}`")),
            }
        }
        // A size that is given is the size, unless the same call says how that side is sized;
        // a new auto layout hugs its items on the sides that were not given one.
        for (horizontal, size, axis, side) in [(true, "w", "horizontal", "width"), (false, "h", "vertical", "height")] {
            let said = m.get("sizing").is_some_and(|s| s.get(axis).is_some()) || m.get("layout").is_some_and(|l| l.get(side).is_some());
            if said {
                continue;
            }
            if m.contains_key(size) {
                n.set_sizing(horizontal, Sizing::Fixed);
            } else if n.layout.is_some() && !had_layout {
                n.set_sizing(horizontal, Sizing::Hug);
            }
        }
        if let Some(v) = m.get("spans") {
            if n.kind != Kind::Text {
                return Err("`spans` only applies to text nodes".into());
            }
            let list = v.as_array().ok_or("`spans` must be an array")?;
            let len = n.text.chars().count();
            n.spans.clear();
            for s in list {
                let at = |key: &str| s.get(key).and_then(Value::as_u64).map(|x| x as usize).ok_or_else(|| format!("a span needs a numeric `{key}`"));
                let (a, b) = (at("start")?, at("end")?);
                if a >= b || b > len {
                    return Err(format!("span {a}..{b} doesn't fit the text ({len} characters; `end` is exclusive)"));
                }
                let font = s.get("font").map(|v| string("font", v)).transpose()?;
                let weight = s.get("weight").and_then(Value::as_u64).map(|w| w.clamp(1, 1000) as u16);
                let italic = s.get("italic").and_then(Value::as_bool);
                let underline = s.get("underline").and_then(Value::as_bool);
                let strike = s.get("strikethrough").and_then(Value::as_bool);
                let size = s.get("size").map(|v| num("size", v)).transpose()?;
                let fill = s.get("fill").filter(|v| !v.is_null()).map(color).transpose()?;
                n.style_range(a, b, |st| {
                    if let Some(font) = &font {
                        st.font = font.clone();
                    }
                    st.weight = weight.unwrap_or(st.weight);
                    st.italic = italic.unwrap_or(st.italic);
                    st.underline = underline.unwrap_or(st.underline);
                    st.strike = strike.unwrap_or(st.strike);
                    st.size = size.unwrap_or(st.size).max(1.0);
                    st.fill = fill.unwrap_or(st.fill);
                });
            }
        }
        Ok(())
    }

    /// Bangun node baru dari spesifikasi JSON (belum dimasukkan ke dokumen).
    fn build(&mut self, spec: &Value, o: (f32, f32), child: bool, notes: &mut Notes) -> Result<Node, String> {
        let m = spec.as_object().ok_or("each node must be an object")?;
        let kind = m.get("kind").and_then(Value::as_str).ok_or("missing `kind`")?;
        if kind.eq_ignore_ascii_case("instance") {
            return self.build_instance(m, o, child, notes);
        }
        let kind = parse_kind(kind).ok_or_else(|| format!("unknown kind \"{kind}\"; use frame, rect, ellipse, line, text, path, image or instance"))?;
        if kind != Kind::Frame && m.contains_key("children") {
            return Err("only frames can have `children`".into());
        }
        let id = self.doc.alloc_id();
        let mut n = Node::new(id, kind, o.0, o.1);
        // Nilai awal yang wajar kalau properti tidak disebut.
        match kind {
            Kind::Frame => (n.w, n.h, n.fill) = (100.0, 100.0, Some([255, 255, 255, 255])),
            Kind::Rect | Kind::Ellipse => (n.w, n.h, n.fill) = (100.0, 100.0, Some([217, 217, 217, 255])),
            Kind::Line => (n.w, n.stroke) = (100.0, Some([0, 0, 0, 255])),
            Kind::Text => (n.fill, n.w, n.h) = (Some([0, 0, 0, 255]), 200.0, 20.0),
            Kind::Path | Kind::Image => {}
        }
        // Node level atas tanpa posisi: taruh di ruang kosong di kanan isi dokumen.
        if !child && kind != Kind::Path && !m.contains_key("x") && !m.contains_key("y") {
            if let Some(b) = self.doc.bounds() {
                (n.x, n.y) = ((b.2 + 100.0).round(), b.1.round());
            }
        }
        notes.natural = None;
        self.apply(&mut n, m, o, true, notes)?;
        match kind {
            Kind::Text => {
                if !m.contains_key("name") {
                    n.name = n.text.lines().next().unwrap_or("Text").chars().take(40).collect();
                }
                // Perkiraan kasar; diganti hasil ukur sebenarnya begitu font siap.
                if !n.wrap {
                    n.w = n.text.lines().map(|l| l.chars().count()).max().unwrap_or(1) as f32 * n.font_size * 0.55;
                }
                if !n.fixed_h {
                    n.h = n.text.lines().count().max(1) as f32 * n.font_size * 1.2;
                }
            }
            Kind::Path => {
                if n.points.len() < 2 {
                    return Err("a path needs `points`".into());
                }
                if n.fill.is_none() && n.stroke.is_none() {
                    (n.stroke, n.stroke_w) = (Some([0, 0, 0, 255]), 2.0);
                }
            }
            Kind::Image => {
                let (nw, nh) = notes.natural.ok_or("an image needs `image_path` (a file on disk) or `svg` (inline SVG markup)")?;
                match (m.contains_key("w"), m.contains_key("h")) {
                    (false, false) => (n.w, n.h) = (nw, nh),
                    (true, false) => n.h = n.w * nh / nw,
                    (false, true) => n.w = n.h * nw / nh,
                    (true, true) => {}
                }
            }
            Kind::Frame => {
                if let Some(children) = m.get("children") {
                    let list = children.as_array().ok_or("`children` must be an array")?;
                    for (i, c) in list.iter().enumerate() {
                        let c = self.build(c, (n.x, n.y), true, notes).map_err(|e| format!("children[{i}]: {e}"))?;
                        n.children.push(c);
                    }
                    if n.layout.is_some() && list.iter().any(placed_by_hand) && !notes.warnings.iter().any(|w| w == POSITIONED) {
                        notes.warnings.push(POSITIONED.into());
                    }
                }
            }
            _ => {}
        }
        Ok(n)
    }

    /// A new instance of the main component `instance_of` (not in the document yet).
    fn build_instance(&mut self, m: &Map<String, Value>, o: (f32, f32), child: bool, notes: &mut Notes) -> Result<Node, String> {
        let main = m.get("instance_of").and_then(Value::as_u64).ok_or("an instance needs `instance_of`: the id of a main component")?;
        if m.contains_key("children") {
            return Err("an instance gets its layers from the main component; `children` can't be given".into());
        }
        let mut n = self.doc.instantiate(main).ok_or_else(|| format!("node {main} isn't a main component (those have `component: true` in get_document)"))?;
        let free = !child && !m.contains_key("x") && !m.contains_key("y");
        let at = self.doc.bounds().filter(|_| free).map_or(o, |b| ((b.2 + 100.0).round(), b.1.round()));
        n.translate(at.0 - n.x, at.1 - n.y);
        notes.natural = None;
        self.apply(&mut n, m, o, true, notes)?;
        // Given a size of its own, its layers are laid out again.
        self.doc.fit_instance(&mut n);
        Ok(n)
    }

    /// May `node` go into frame `parent`? Not into an instance, and not where a component would contain itself.
    fn may_hold(&self, node: &Node, parent: Option<u64>) -> Result<(), String> {
        match parent {
            Some(p) if self.doc.in_instance(p) => Err(format!("frame {p} is an instance (or part of one): layers can't be added to it. Add them to its main component, or detach the instance first.")),
            Some(p) if self.doc.would_cycle(node, parent) => Err(format!("frame {p} belongs to a main component that this node holds an instance of: a component can't contain itself")),
            _ => Ok(()),
        }
    }

    /// Frame (di kedalaman mana pun) yang boleh jadi induk: hasilnya titik asalnya.
    fn parent_origin(&self, id: u64) -> Result<(f32, f32), String> {
        let f = self.doc.find(id).ok_or_else(|| format!("parent {id} not found"))?;
        if f.kind != Kind::Frame {
            return Err(format!("parent {id} isn't a frame"));
        }
        Ok((f.x, f.y))
    }

    /// Geser tampilan ke node-node ini kalau belum terlihat seluruhnya.
    fn reveal(&mut self, ids: &[u64]) {
        let bounds = ids
            .iter()
            .filter_map(|id| self.doc.find(self.doc.top_of(*id)?).map(Node::aabb))
            .reduce(|a, b| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)));
        let Some(b) = bounds else { return };
        let o = self.canvas_rect.min;
        let (tl, br) = (self.view.to_world(o, self.canvas_rect.min), self.view.to_world(o, self.canvas_rect.max));
        let inside = b.0 >= tl.x && b.1 >= tl.y && b.2 <= br.x && b.3 <= br.y;
        let tiny = (b.2 - b.0).max(b.3 - b.1) * self.view.zoom < 60.0;
        if !inside || tiny {
            self.zoom_to(b, 1.0);
        }
    }

    // ------------------------------------------------------------ ubah

    fn remote_create(&mut self, args: &Value) -> Result<Outcome, String> {
        let parent = args.get("parent").and_then(Value::as_u64).filter(|p| *p != 0);
        let o = match parent {
            Some(p) => self.parent_origin(p)?,
            None => (0.0, 0.0),
        };
        let specs = args.get("nodes").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or("`nodes` must be a non-empty array")?;
        let pre = self.doc.clone();
        let mut notes = Notes::default();
        if parent.and_then(|p| self.doc.find(p)).is_some_and(|p| p.is_auto()) && specs.iter().any(placed_by_hand) {
            notes.warnings.push(POSITIONED.into());
        }
        let mut ids = Vec::with_capacity(specs.len());
        for (i, spec) in specs.iter().enumerate() {
            match self.build(spec, o, parent.is_some(), &mut notes).and_then(|n| self.may_hold(&n, parent).map(|_| n)) {
                Ok(n) => {
                    ids.push(n.id);
                    self.doc.insert(n, parent);
                }
                Err(e) => {
                    self.doc = pre;
                    return Err(format!("nodes[{i}]: {e} (nothing was created)"));
                }
            }
        }
        self.push_undo(pre);
        let fonts = self.want_fonts(&ids, &mut notes.warnings);
        self.sel = ids.clone();
        self.home = false;
        self.reveal(&ids);
        let count: usize = ids.iter().filter_map(|id| self.doc.find(*id)).map(Node::count).sum();
        self.status = format!("Remote: added {count} layer{}.", if count == 1 { "" } else { "s" });
        Ok(Outcome::Later(After::Nodes { key: "created", ids, warnings: notes.warnings }, fonts))
    }

    fn remote_update(&mut self, args: &Value) -> Result<Outcome, String> {
        let specs = args.get("nodes").and_then(Value::as_array).filter(|a| !a.is_empty()).ok_or("`nodes` must be a non-empty array")?;
        let pre = self.doc.clone();
        let mut notes = Notes::default();
        let mut ids = Vec::with_capacity(specs.len());
        for (i, spec) in specs.iter().enumerate() {
            match self.update_one(spec, &mut notes) {
                Ok(id) => ids.push(id),
                Err(e) => {
                    self.doc = pre;
                    return Err(format!("nodes[{i}]: {e} (nothing was changed)"));
                }
            }
        }
        self.push_undo(pre);
        let fonts = self.want_fonts(&ids, &mut notes.warnings);
        self.home = false;
        self.reveal(&ids);
        self.status = format!("Remote: updated {} layer{}.", ids.len(), if ids.len() == 1 { "" } else { "s" });
        Ok(Outcome::Later(After::Nodes { key: "updated", ids, warnings: notes.warnings }, fonts))
    }

    fn update_one(&mut self, spec: &Value, notes: &mut Notes) -> Result<u64, String> {
        let m = spec.as_object().ok_or("each entry must be an object")?;
        let id = m.get("id").and_then(Value::as_u64).ok_or("missing `id`")?;
        let mut n = self.doc.find(id).cloned().ok_or_else(|| format!("node {id} not found"))?;
        let current = self.doc.parent_of(id);
        // `parent` disebut: pindah induk (null / 0 = level atas); x/y dibaca relatif ke induk baru.
        let target = match m.get("parent") {
            None => current,
            Some(p) => p.as_u64().filter(|p| *p != 0),
        };
        let o = match target {
            Some(p) => self.parent_origin(p)?,
            None => (0.0, 0.0),
        };
        if target.is_some_and(|p| p == id || self.doc.is_descendant(p, id)) {
            return Err("a frame can't be moved into itself or into something inside it".into());
        }
        if n.source.is_some() {
            let fixed = ["x", "y", "w", "h", "rotation", "name", "points", "closed", "wrap", "fixed_height", "parent", "component", "constraints", "layout", "sizing", "absolute"];
            if let Some(k) = m.keys().find(|k| fixed.contains(&k.as_str())) {
                return Err(format!("node {id} is part of an instance: `{k}` follows the main component (only its look and text can be changed). Edit the main component, or detach the instance."));
            }
        }
        if target != current {
            self.may_hold(&n, target)?;
        }
        let was = n.clone();
        self.apply(&mut n, m, o, false, notes)?;
        let item = !n.absolute && target.and_then(|p| self.doc.find(p)).is_some_and(|p| p.is_auto());
        if item && (m.contains_key("x") || m.contains_key("y")) && !notes.warnings.iter().any(|w| w == POSITIONED) {
            notes.warnings.push(POSITIONED.into());
        }
        if n.kind == Kind::Frame && (n.w, n.h) != (was.w, was.h) {
            // A resized frame lays out what it holds, unless asked to leave it where it is.
            let keep = m.get("keep_children").is_some_and(|v| v.as_bool() == Some(true));
            let to = (n.x, n.y, n.w, n.h);
            (n.w, n.h) = (was.w, was.h);
            layout::set_box(&mut n, to, if keep { Children::Keep } else { Children::Constrain });
            self.doc.refit(&mut n, &was);
        }
        if target != current {
            self.doc.remove(id);
            self.doc.insert(n, target);
        } else if let Some(slot) = self.doc.find_mut(id) {
            *slot = n;
        }
        Ok(id)
    }

    fn remote_delete(&mut self, args: &Value) -> Result<Value, String> {
        let ids = id_list(args.get("ids"))?;
        if let Some(id) = ids.iter().find(|id| self.doc.pinned(**id)) {
            return Err(format!("node {id} is part of an instance and can't be deleted on its own; delete the instance, detach it, or remove the layer from the main component (nothing was deleted)"));
        }
        let pre = self.doc.clone();
        let removed: Vec<u64> = ids.into_iter().filter(|id| self.doc.remove(*id).is_some()).collect();
        if removed.is_empty() {
            return Err("none of those ids exist".into());
        }
        let doc = &self.doc;
        self.sel.retain(|id| doc.find(*id).is_some());
        self.push_undo(pre);
        self.status = format!("Remote: deleted {} layer{}.", removed.len(), if removed.len() == 1 { "" } else { "s" });
        Ok(json!({ "deleted": removed }))
    }

    fn remote_arrange(&mut self, args: &Value) -> Result<Value, String> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or_default();
        let ids = id_list(args.get("ids"))?;
        if let Some(missing) = ids.iter().find(|id| self.doc.find(**id).is_none()) {
            return Err(format!("node {missing} not found"));
        }
        if ids.is_empty() {
            return Err("`ids` is empty".into());
        }
        if !matches!(action, "reset") {
            if let Some(id) = ids.iter().find(|id| self.doc.pinned(**id)) {
                return Err(format!("node {id} is part of an instance: its place, order and grouping follow the main component (to mirror what it shows, set flip_h / flip_v with update_nodes)"));
            }
        }
        if matches!(action, "distribute_h" | "distribute_v" | "tidy") {
            if let Some(id) = ids.iter().find(|id| self.doc.auto_item(**id)) {
                return Err(format!("node {id} is an item of an auto-layout frame, which places it: change the `layout` of that frame (gap, align) instead, or set `absolute: true` on the node"));
            }
        }
        let pre = self.doc.clone();
        let mut out = json!({ "ok": true });
        match action {
            "component" => {
                let id = self.doc.make_component(&ids)?;
                self.sel = vec![id];
                out = json!({ "component": id });
            }
            "detach" => {
                let done: Vec<u64> = ids.iter().copied().filter(|id| self.doc.detach(*id)).collect();
                if done.is_empty() {
                    return Err("none of those nodes is an instance".into());
                }
                out = json!({ "detached": done });
            }
            "reset" => {
                let done: Vec<u64> = ids.iter().copied().filter(|id| self.doc.reset_overrides(*id)).collect();
                out = json!({ "reset": done });
            }
            "front" | "back" => self.doc.reorder(&ids, action == "front"),
            "forward" | "backward" => self.doc.reorder_step(&ids, action == "forward"),
            "group" => {
                if ids.len() < 2 {
                    return Err("grouping needs at least two nodes".into());
                }
                let g = self.doc.group(&ids);
                if let Some(name) = args.get("name").and_then(Value::as_str) {
                    self.doc.group_names.insert(g, name.to_owned());
                }
                out = json!({ "group": g, "name": self.doc.group_name(g) });
            }
            "ungroup" => {
                let mut groups: Vec<u64> = ids.iter().filter_map(|id| self.doc.find(*id)).map(|n| n.group).filter(|g| *g != 0).collect();
                groups.sort_unstable();
                groups.dedup();
                self.doc.ungroup(&groups);
                out = json!({ "ungrouped": groups });
            }
            "distribute_h" | "distribute_v" => {
                if !self.doc.distribute(&ids, action == "distribute_h") {
                    return Err("distributing needs at least three nodes (the members of a group count as one)".into());
                }
            }
            "tidy" => {
                if !self.doc.tidy(&ids) {
                    return Err("tidying up needs at least two nodes (the members of a group count as one)".into());
                }
            }
            "flip_h" | "flip_v" => {
                self.doc.flip(&ids, action == "flip_h");
            }
            other => return Err(format!("unknown action \"{other}\"; use front, back, forward, backward, group, ungroup, component, detach, reset, distribute_h, distribute_v, tidy, flip_h or flip_v")),
        }
        self.push_undo(pre);
        self.status = format!("Remote: {action}.");
        Ok(out)
    }

    fn remote_select(&mut self, args: &Value) -> Result<Value, String> {
        let ids = id_list(args.get("ids"))?;
        self.sel = ids.into_iter().filter(|id| self.doc.find(*id).is_some()).collect();
        self.home = false;
        if args.get("zoom").and_then(Value::as_bool).unwrap_or(true) {
            if self.sel.is_empty() {
                self.fit_all();
            } else {
                self.zoom_selection();
            }
        }
        Ok(json!({ "selection": self.sel }))
    }

    fn remote_library(&mut self, args: &Value) -> Result<Value, String> {
        match args.get("action").and_then(Value::as_str).unwrap_or("list") {
            "list" => {}
            "add" => {
                let path = PathBuf::from(string("path", args.get("path").unwrap_or(&Value::Null))?);
                if !path.is_absolute() {
                    return Err("`path` must be an absolute file path".into());
                }
                let name = self.library.add_file(&path)?;
                self.status = format!("Remote: added \"{name}\" to the library.");
            }
            other => return Err(format!("unknown action \"{other}\"; use list or add")),
        }
        let items: Vec<Value> = self.library.list().into_iter().map(|(name, path)| json!({ "name": name, "path": path })).collect();
        Ok(json!({ "items": items }))
    }

    fn pages_json(&self) -> Value {
        (0..self.doc.page_count()).map(|i| json!({ "index": i, "name": self.doc.page_name(i), "current": i == self.doc.page })).collect()
    }

    fn remote_pages(&mut self, args: &Value) -> Result<Value, String> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("list");
        let count = self.doc.page_count();
        let index = || -> Result<usize, String> {
            match args.get("index").and_then(Value::as_u64) {
                Some(i) if (i as usize) < count => Ok(i as usize),
                Some(i) => Err(format!("page {i} doesn't exist; the document has {count} page(s), indexed from 0")),
                None => Ok(self.doc.page),
            }
        };
        let name = args.get("name").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        match action {
            "list" => {}
            "switch" => {
                let i = index()?;
                self.go_page(i);
            }
            "add" => {
                self.add_page();
                if let Some(name) = name {
                    self.doc.rename_page(self.doc.page, name);
                }
            }
            "rename" => {
                let i = index()?;
                self.rename_page(i, name.ok_or("`name` is required to rename a page")?);
            }
            "duplicate" => {
                let i = index()?;
                self.duplicate_page(i);
            }
            "delete" => {
                let i = index()?;
                if count < 2 {
                    return Err("the last page can't be deleted".into());
                }
                self.delete_page(i);
            }
            other => return Err(format!("unknown action \"{other}\"; use list, switch, add, rename, duplicate or delete")),
        }
        Ok(json!({ "pages": self.pages_json() }))
    }

    fn remote_history(&mut self, args: &Value) -> Result<Value, String> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or_default();
        let steps = args.get("steps").and_then(Value::as_u64).unwrap_or(1).clamp(1, 100);
        let mut done = 0;
        for _ in 0..steps {
            let stack = match action {
                "undo" => &self.undo,
                "redo" => &self.redo,
                other => return Err(format!("unknown action \"{other}\"; use undo or redo")),
            };
            if stack.is_empty() {
                break;
            }
            if action == "undo" {
                self.do_undo();
            } else {
                self.do_redo();
            }
            done += 1;
        }
        Ok(json!({ "steps_done": done, "undo_available": self.undo.len(), "redo_available": self.redo.len() }))
    }

    // ------------------------------------------------------------ file

    /// Boleh mengganti dokumen yang terbuka? File yang punya lokasi disimpan dulu; dokumen yang
    /// belum pernah disimpan tidak dibuang diam-diam.
    fn remote_leave(&mut self, args: &Value) -> Result<(), String> {
        if self.path.is_some() {
            return if self.settle() { Ok(()) } else { Err(self.status.clone()) };
        }
        if self.dirty && !args.get("discard").and_then(Value::as_bool).unwrap_or(false) {
            return Err("The open document has unsaved changes and was never saved. Save it first (action \"save\" with a path), or ask the user and pass discard: true.".into());
        }
        Ok(())
    }

    fn remote_file(&mut self, args: &Value) -> Result<Value, String> {
        let action = args.get("action").and_then(Value::as_str).unwrap_or("info");
        let path = args.get("path").and_then(Value::as_str).map(PathBuf::from);
        if path.as_ref().is_some_and(|p| !p.is_absolute()) {
            return Err("`path` must be an absolute file path".into());
        }
        match action {
            "info" => {}
            "new" => {
                self.remote_leave(args)?;
                self.doc = Doc::default();
                self.path = None;
                self.reset_session();
                self.home = false;
                self.status = "Remote: new document.".into();
            }
            "open" => {
                let path = path.ok_or("`path` is required to open a file")?;
                if self.path.as_deref() != Some(path.as_path()) {
                    self.remote_leave(args)?;
                    if !self.load_now(path) {
                        return Err(self.status.clone());
                    }
                }
                self.home = false;
            }
            "save" => {
                let mut path = path.or_else(|| self.path.clone()).ok_or("this document has no file yet; pass `path` (e.g. C:\\…\\design.kanvas)")?;
                if path.extension().is_none_or(|e| e != "kanvas") {
                    path.as_mut_os_string().push(".kanvas");
                }
                self.write_doc(Path::new(&path))?;
                self.status = format!("Remote: saved {}", path.display());
            }
            other => return Err(format!("unknown action \"{other}\"; use info, new, open or save")),
        }
        Ok(json!({
            "file": self.path,
            "unsaved_changes": self.dirty,
            "frames": self.doc.frame_count(),
            "layers": self.doc.count(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_parse_and_print() {
        assert_eq!(color(&json!("#ff8000")).unwrap(), [255, 128, 0, 255]);
        assert_eq!(color(&json!("F80")).unwrap(), [255, 136, 0, 255]);
        assert_eq!(color(&json!("#0000ff80")).unwrap(), [0, 0, 255, 128]);
        assert_eq!(color(&json!([1, 2, 3])).unwrap(), [1, 2, 3, 255]);
        assert_eq!(color(&json!("white")).unwrap(), [255, 255, 255, 255]);
        assert!(color(&json!("#12345")).is_err());
        assert!(color(&json!("#ggg")).is_err());
        assert!(color(&json!("#ééé")).is_err());
        assert_eq!(hex([255, 128, 0, 255]), "#ff8000");
        assert_eq!(hex([0, 0, 255, 128]), "#0000ff80");
    }

    #[test]
    fn fill_accepts_color_gradient_or_none() {
        assert_eq!(paint(&Value::Null).unwrap(), (None, None));
        assert_eq!(paint(&json!("#000")).unwrap(), (Some([0, 0, 0, 255]), None));
        let (fill, grad) = paint(&json!({ "type": "radial", "stops": [{ "pos": 0, "color": "#f00" }, { "pos": 1, "color": "#00f" }] })).unwrap();
        let grad = grad.unwrap();
        assert_eq!(fill, Some([255, 0, 0, 255]));
        assert!(grad.radial && grad.stops.len() == 2);
        assert!(paint(&json!({ "stops": [{ "pos": 0, "color": "#f00" }] })).is_err());
    }

    #[test]
    fn children_are_described_relative_to_their_frame() {
        let mut f = Node::new(1, Kind::Frame, 100.0, 50.0);
        (f.w, f.h) = (400.0, 300.0);
        let mut r = Node::new(2, Kind::Rect, 120.0, 80.0);
        (r.w, r.h, r.fill) = (10.0, 10.0, Some([255, 0, 0, 255]));
        f.children.push(r);
        let v = describe(&f, (0.0, 0.0), true);
        assert_eq!((v["x"].as_f64(), v["y"].as_f64()), (Some(100.0), Some(50.0)));
        let c = &v["children"][0];
        assert_eq!((c["x"].as_f64(), c["y"].as_f64()), (Some(20.0), Some(30.0)));
        assert_eq!(c["fill"], json!("#ff0000"));
        assert_eq!(describe(&f, (0.0, 0.0), false)["child_count"], json!(1));
    }

    #[test]
    fn constraints_are_described_where_they_are_set() {
        let mut n = Node::new(1, Kind::Rect, 0.0, 0.0);
        assert_eq!(describe(&n, (0.0, 0.0), true)["constraints"], Value::Null);
        (n.constrain_h, n.constrain_v) = (Constraint::Stretch, Constraint::Max);
        assert_eq!(describe(&n, (0.0, 0.0), true)["constraints"], json!({ "horizontal": "left_right", "vertical": "bottom" }));
        n.constrain_v = Constraint::Min;
        assert_eq!(describe(&n, (0.0, 0.0), true)["constraints"], json!({ "horizontal": "left_right", "vertical": "top" }));
    }

    #[test]
    fn auto_layout_is_read_and_described() {
        let mut f = Node::new(1, Kind::Frame, 0.0, 0.0);
        assert_eq!((describe(&f, (0.0, 0.0), true)["layout"].clone(), describe(&f, (0.0, 0.0), true)["sizing"].clone()), (Value::Null, Value::Null));
        // A new layout is a row; only what is given changes after that.
        assert_eq!(set_layout(&mut f, &json!({ "gap": 12, "padding": [16, 8], "cross_align": "center", "height": "hug" })), Ok((false, true)));
        let l = f.layout.clone().unwrap();
        assert_eq!((l.dir, l.gap, l.pad, l.align, l.cross, f.size_v), (Dir::Horizontal, 12.0, [16.0, 8.0, 16.0, 8.0], Align::Start, Align::Center, Sizing::Hug));
        assert_eq!(set_layout(&mut f, &json!({ "direction": "vertical", "gap": "auto", "padding": { "top": 4 }, "wrap": true, "row_gap": 6, "width": "fill" })), Ok((true, false)));
        let l = f.layout.clone().unwrap();
        assert_eq!((l.dir, l.gap, l.pad, l.align, l.wrap, l.row_gap, f.size_h), (Dir::Vertical, 12.0, [16.0, 4.0, 16.0, 8.0], Align::Between, true, 6.0, Sizing::Fill));
        assert_eq!(
            describe(&f, (0.0, 0.0), true)["layout"],
            json!({ "direction": "vertical", "wrap": true, "gap": 12.0, "row_gap": 6.0, "padding": [16.0, 4.0, 16.0, 8.0], "align": "space_between", "cross_align": "center", "width": "fill", "height": "hug" })
        );
        set_layout(&mut f, &json!({ "padding": 5, "align": "end" })).unwrap();
        assert_eq!((f.layout.as_ref().unwrap().pad, f.layout.as_ref().unwrap().align), ([5.0; 4], Align::End));
        set_layout(&mut f, &json!({ "padding": [1, 2, 3, 4] })).unwrap();
        assert_eq!(f.layout.as_ref().unwrap().pad, [1.0, 2.0, 3.0, 4.0]);
        for bad in [json!({ "direction": "diagonal" }), json!({ "cross_align": "space_between" }), json!({ "width": "wide" }), json!({ "padding": "lots" }), json!({ "spacing": 3 }), json!(7)] {
            assert!(set_layout(&mut f.clone(), &bad).is_err(), "{bad}");
        }
        // An item says how it is sized, and whether it is left out of the layout.
        let mut item = Node::new(2, Kind::Rect, 0.0, 0.0);
        (item.size_h, item.absolute) = (Sizing::Fill, true);
        let d = describe(&item, (0.0, 0.0), true);
        assert_eq!((d["sizing"].clone(), d["absolute"].clone(), d["layout"].clone()), (json!({ "horizontal": "fill", "vertical": "fixed" }), json!(true), Value::Null));
        assert!(placed_by_hand(&json!({ "x": 5 })) && !placed_by_hand(&json!({ "x": 5, "absolute": true })) && !placed_by_hand(&json!({ "w": 5 })));
    }

    #[test]
    fn components_and_instances_say_what_they_are() {
        let mut main = Node::new(1, Kind::Frame, 0.0, 0.0);
        main.component = true;
        let mut inst = Node::new(3, Kind::Frame, 0.0, 0.0);
        inst.instance_of = Some(1);
        let mut part = Node::new(4, Kind::Rect, 0.0, 0.0);
        part.source = Some(2);
        inst.children.push(part);
        let (m, i) = (describe(&main, (0.0, 0.0), true), describe(&inst, (0.0, 0.0), true));
        assert_eq!((&m["component"], &m["instance_of"]), (&json!(true), &Value::Null));
        assert_eq!((&i["component"], &i["instance_of"], &i["children"][0]["source"]), (&Value::Null, &json!(1), &json!(2)));
    }

    #[test]
    fn nested_frames_are_described_relative_to_their_own_parent() {
        let mut top = Node::new(1, Kind::Frame, 100.0, 50.0);
        let mut inner = Node::new(2, Kind::Frame, 150.0, 70.0);
        let mut r = Node::new(3, Kind::Rect, 155.0, 80.0);
        (top.w, top.h, inner.w, inner.h, r.w, r.h) = (400.0, 300.0, 200.0, 100.0, 10.0, 10.0);
        inner.children.push(r);
        top.children.push(inner);
        let at = |v: &Value| (v["x"].as_f64(), v["y"].as_f64());
        for v in [describe(&top, (0.0, 0.0), true), brief(&top, (0.0, 0.0))] {
            let inner = &v["children"][0];
            assert_eq!((inner["kind"].as_str(), at(inner)), (Some("frame"), (Some(50.0), Some(20.0))));
            assert_eq!(at(&inner["children"][0]), (Some(5.0), Some(10.0)));
        }
    }
}
