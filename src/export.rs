//! Export: dokumen -> SVG, lalu PNG/JPG lewat resvg (satu jalur render untuk semuanya).
//! Teks diekspor sebagai outline glyph, jadi hasilnya sama persis dengan canvas
//! (paragraf, gaya per-huruf, spasi) dan tidak bergantung pada font di komputer lain.

use crate::app::blend;
use crate::app::render::{is_svg, layout_text, text_left};
use crate::doc::{b64, Asset, Doc, Kind, Node, Shadow};
use crate::fonts::FontManager;
use eframe::egui;
use std::fmt::Write;
use std::path::Path;

type Assets = std::collections::BTreeMap<u64, std::sync::Arc<Asset>>;

fn paint(attr: &str, c: Option<[u8; 4]>) -> String {
    match c {
        Some([r, g, b, a]) => {
            format!(r##"{attr}="#{r:02x}{g:02x}{b:02x}" {attr}-opacity="{:.3}""##, a as f32 / 255.0)
        }
        None => format!(r#"{attr}="none""#),
    }
}

fn stroke(n: &Node) -> String {
    match n.stroke {
        Some(_) if n.stroke_w > 0.0 => format!(r#"{} stroke-width="{}""#, paint("stroke", n.stroke), n.stroke_w),
        _ => String::new(),
    }
}

/// Atribut fill; gradasi ditulis sebagai <defs> di depan elemen.
fn fill(out: &mut String, n: &Node) -> String {
    let (Some(_), Some(g)) = (n.fill, &n.gradient) else { return paint("fill", n.fill) };
    let id = format!("g{}", n.id);
    let stops: String = g
        .sorted()
        .iter()
        .map(|s| {
            let [r, gr, b, a] = s.color;
            format!(
                r##"<stop offset="{:.4}" stop-color="#{r:02x}{gr:02x}{b:02x}" stop-opacity="{:.3}"/>"##,
                s.pos.clamp(0.0, 1.0),
                a as f32 / 255.0
            )
        })
        .collect();
    if g.radial {
        let [cx, cy, ex, ey] = g.line();
        let r = ((ex - cx).powi(2) + (ey - cy).powi(2)).sqrt();
        let _ = write!(out, r#"<defs><radialGradient id="{id}" cx="{cx:.4}" cy="{cy:.4}" r="{r:.4}">{stops}</radialGradient></defs>"#);
    } else {
        // Sama dengan tekstur di canvas: garis lewat pusat kotak, dari tepi ke tepi.
        let [x1, y1, x2, y2] = g.line();
        let _ = write!(
            out,
            r#"<defs><linearGradient id="{id}" x1="{x1:.4}" y1="{y1:.4}" x2="{x2:.4}" y2="{y2:.4}">{stops}</linearGradient></defs>"#
        );
    }
    format!(r#"fill="url(#{id})""#)
}

fn mime(bytes: &[u8]) -> &'static str {
    if is_svg(bytes) {
        return "image/svg+xml";
    }
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => "image/png",
        [0xFF, 0xD8, ..] => "image/jpeg",
        [b'G', b'I', b'F', ..] => "image/gif",
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => "image/webp",
        [b'B', b'M', ..] => "image/bmp",
        _ => "image/png",
    }
}

struct Outline<'a> {
    d: &'a mut String,
    x: f32,
    y: f32,
    s: f32,
}

impl ttf_parser::OutlineBuilder for Outline<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let _ = write!(self.d, "M{:.2} {:.2}", self.x + x * self.s, self.y - y * self.s);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let _ = write!(self.d, "L{:.2} {:.2}", self.x + x * self.s, self.y - y * self.s);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (s, ox, oy) = (self.s, self.x, self.y);
        let _ = write!(self.d, "Q{:.2} {:.2} {:.2} {:.2}", ox + x1 * s, oy - y1 * s, ox + x * s, oy - y * s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (s, ox, oy) = (self.s, self.x, self.y);
        let _ = write!(
            self.d,
            "C{:.2} {:.2} {:.2} {:.2} {:.2} {:.2}",
            ox + x1 * s,
            oy - y1 * s,
            ox + x2 * s,
            oy - y2 * s,
            ox + x * s,
            oy - y * s
        );
    }
    fn close(&mut self) {
        self.d.push('Z');
    }
}

/// Teks -> path: layout yang sama dengan canvas, lalu outline tiap glyph dari file font.
fn text_svg(out: &mut String, n: &Node, fonts: &mut FontManager, ctx: &egui::Context) {
    if n.text.is_empty() {
        return;
    }
    // Layout di ukuran dunia (dibatasi supaya atlas glyph tidak membengkak), lalu diskalakan balik.
    let f = (200.0 / n.max_size()).min(1.0);
    let layout = layout_text(ctx, n, &n.text, fonts, f, 1.0, true);
    let galley = &layout.galley;
    let off_x = text_left(n.wrap, n.align, n.w, galley, f);
    // Satu <path> per warna berurutan.
    let mut runs: Vec<(egui::Color32, String)> = Vec::new();
    for row in &galley.rows {
        for g in &row.glyphs {
            let Some(section) = galley.job.sections.get(g.section_index as usize) else { continue };
            let format = &section.format;
            let face_of = |family: &egui::FontFamily| {
                let data = fonts.face_data(family)?;
                let face = ttf_parser::Face::parse(data, 0).ok()?;
                face.glyph_index(g.chr).map(|gid| (face, gid))
            };
            // Glyph tidak ada di font pilihan (mis. font belum terunduh): pakai font bawaan.
            let Some((face, gid)) = face_of(&format.font_id.family).or_else(|| face_of(&egui::FontFamily::Proportional)) else {
                continue;
            };
            let scale = format.font_id.size / face.units_per_em() as f32 / f;
            let mut d = String::new();
            let mut builder = Outline { d: &mut d, x: n.x + (g.pos.x - off_x) / f, y: n.y + (g.pos.y - layout.pad_top) / f, s: scale };
            if face.outline_glyph(gid, &mut builder).is_none() || d.is_empty() {
                continue;
            }
            match runs.last_mut() {
                Some((color, path)) if *color == format.color => path.push_str(&d),
                _ => runs.push((format.color, d)),
            }
        }
    }
    for (color, d) in runs {
        let _ = write!(out, r#"<path d="{d}" {}/>"#, paint("fill", Some(color.to_srgba_unmultiplied())));
    }
}

/// Daftar saudara (urutan tumpukan). Clipping mask: alas ditulis sekali dalam grup ber-id, lalu
/// dipakai ulang sebagai mask alpha untuk layer yang di-clip kepadanya.
fn list_svg<T: AsRef<Node>>(out: &mut String, nodes: &[T], assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) {
    let mut i = 0;
    while i < nodes.len() {
        let (j, k) = crate::doc::clip_run(nodes, i);
        if k == j {
            for n in &nodes[i..j] {
                node_svg(out, n.as_ref(), assets, fonts, ctx);
            }
        } else {
            let id = nodes[i].as_ref().id;
            let _ = write!(out, r#"<g id="b{id}">"#);
            let mut area: Option<(f32, f32, f32, f32)> = None;
            for n in &nodes[i..j] {
                let n = n.as_ref();
                node_svg(out, n, assets, fonts, ctx);
                // Stroke dan glyph bisa keluar sedikit dari kotak pembatas.
                let (b, pad) = (n.aabb(), n.stroke_w.max(0.0) + if n.kind == Kind::Text { n.max_size() } else { 0.0 });
                let b = (b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad);
                area = Some(area.map_or(b, |a| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))));
            }
            let (x0, y0, x1, y1) = area.unwrap_or_default();
            let _ = write!(
                out,
                r##"</g><mask id="m{id}" style="mask-type:alpha" maskUnits="userSpaceOnUse" x="{x0}" y="{y0}" width="{}" height="{}"><use href="#b{id}"/></mask><g mask="url(#m{id})">"##,
                x1 - x0,
                y1 - y0
            );
            for n in &nodes[j..k] {
                node_svg(out, n.as_ref(), assets, fonts, ctx);
            }
            out.push_str("</g>");
        }
        i = k;
    }
}

/// Cara drop shadow ikut digambar.
#[derive(Clone, Copy, PartialEq)]
enum Fx {
    /// Objek beserta bayangannya (export).
    Full,
    /// Hanya bayangannya (tekstur bayangan di canvas).
    ShadowOnly,
}

fn node_svg(out: &mut String, n: &Node, assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) {
    node_fx(out, n, assets, fonts, ctx, Fx::Full);
}

/// Jarak aman di sekeliling objek supaya bayangannya tidak terpotong.
fn shadow_pad(n: &Node, s: &Shadow) -> f32 {
    s.pad() + n.stroke_w.max(0.0) + if n.kind == Kind::Text { n.max_size() } else { 0.0 }
}

/// Tulis `<filter>` drop shadow untuk node (keluarannya hanya bayangan, tanpa objeknya);
/// hasilnya atribut `filter` untuk elemen yang dibayangi.
/// Filter dipasang di dalam grup rotasi, jadi arah bayangan ikut berputar bersama objek.
fn shadow_filter(out: &mut String, n: &Node, s: &Shadow) -> String {
    let (x0, y0, x1, y1) = n.bounds();
    let pad = shadow_pad(n, s);
    let [r, g, b, a] = s.color;
    let _ = write!(
        out,
        r#"<filter id="sh{}" filterUnits="userSpaceOnUse" x="{}" y="{}" width="{}" height="{}" color-interpolation-filters="sRGB">"#,
        n.id,
        x0 - pad,
        y0 - pad,
        x1 - x0 + pad * 2.0,
        y1 - y0 + pad * 2.0
    );
    // Blur 0 = bayangan tajam: tanpa feGaussianBlur.
    let source = if s.blur >= 0.01 {
        let _ = write!(out, r#"<feGaussianBlur in="SourceAlpha" stdDeviation="{}"/>"#, s.blur * 0.5);
        String::new()
    } else {
        r#" in="SourceAlpha""#.to_owned()
    };
    let _ = write!(
        out,
        r##"<feOffset{source} dx="{}" dy="{}" result="b"/><feFlood flood-color="#{r:02x}{g:02x}{b:02x}" flood-opacity="{:.3}"/><feComposite in2="b" operator="in"/>"##,
        s.x,
        s.y,
        a as f32 / 255.0
    );
    out.push_str("</filter>");
    format!(r#" filter="url(#sh{})""#, n.id)
}

/// Untuk tekstur bayangan: objek yang bentuknya pasti kotak penuh cukup diwakili satu `<rect>`
/// (tanpa menanam gambar atau isi frame). Hasil: sudut bulatnya.
fn solid_box(n: &Node, assets: &Assets) -> Option<f32> {
    let (x0, y0, x1, y1) = n.bounds();
    match n.kind {
        Kind::Frame if n.fill.is_some_and(|c| c[3] == 255) => Some(0.0),
        // JPEG tidak punya transparansi.
        Kind::Image if n.image.and_then(|id| assets.get(&id)).is_some_and(|a| mime(&a.bytes) == "image/jpeg") => {
            Some(n.radius.min((x1 - x0) * 0.5).min((y1 - y0) * 0.5).max(0.0))
        }
        _ => None,
    }
}

fn node_fx(out: &mut String, n: &Node, assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context, fx: Fx) {
    if !n.visible {
        return;
    }
    let shadow = n.shadow.filter(|s| s.color[3] > 0);
    if fx == Fx::ShadowOnly && shadow.is_none() {
        return;
    }
    // Opacity, rotasi, dan blend mode dibungkus dalam satu grup.
    let mut attrs = String::new();
    if n.opacity < 1.0 {
        let _ = write!(attrs, r#" opacity="{:.3}""#, n.opacity.max(0.0));
    }
    if n.rotation != 0.0 {
        let (cx, cy) = n.center();
        let _ = write!(attrs, r#" transform="rotate({} {cx} {cy})""#, n.rotation);
    }
    if n.blend != 0 && fx == Fx::Full {
        let _ = write!(attrs, r#" style="mix-blend-mode:{}""#, blend::CSS[(n.blend as usize).min(blend::CSS.len() - 1)]);
    }
    let filter = shadow.map(|s| shadow_filter(out, n, &s));
    if !attrs.is_empty() {
        let _ = write!(out, "<g{attrs}>");
    }
    match (&filter, fx) {
        (None, _) => node_inner(out, n, assets, fonts, ctx),
        (Some(filter), Fx::ShadowOnly) => {
            let _ = write!(out, "<g{filter}>");
            match solid_box(n, assets) {
                Some(r) => {
                    let (x0, y0, x1, y1) = n.bounds();
                    let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{}" height="{}" rx="{r}"/>"#, x1 - x0, y1 - y0);
                }
                None => node_inner(out, n, assets, fonts, ctx),
            }
            out.push_str("</g>");
        }
        // Objek ditulis sekali lalu dipakai dua kali: salinan berfilter untuk bayangan, dan objek
        // aslinya di atasnya tanpa filter. Dengan begitu hanya bayangan yang menjadi bitmap di PDF;
        // objeknya sendiri tetap vektor.
        (Some(filter), Fx::Full) => {
            let _ = write!(out, r#"<defs><g id="n{}">"#, n.id);
            node_inner(out, n, assets, fonts, ctx);
            let _ = write!(out, r##"</g></defs><use href="#n{id}"{filter}/><use href="#n{id}"/>"##, id = n.id);
        }
    }
    if !attrs.is_empty() {
        out.push_str("</g>");
    }
}

/// SVG yang hanya berisi bayangan satu node, untuk tekstur bayangan di canvas.
/// Hasil: (svg, area dunia yang dicakup).
pub fn shadow_svg(n: &Node, assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) -> Option<(String, (f32, f32, f32, f32))> {
    let s = n.shadow.filter(|s| s.color[3] > 0 && n.visible)?;
    let pad = shadow_pad(n, &s);
    let b = n.aabb();
    let region = (b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad);
    let (w, h) = (region.2 - region.0, region.3 - region.1);
    let mut out = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{} {} {w} {h}">"#, region.0, region.1);
    node_fx(&mut out, n, assets, fonts, ctx, Fx::ShadowOnly);
    out.push_str("</svg>");
    Some((out, region))
}

fn node_inner(out: &mut String, n: &Node, assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) {
    let (x0, y0, x1, y1) = n.bounds();
    let (w, h) = (x1 - x0, y1 - y0);
    let r = n.radius.min(w * 0.5).min(h * 0.5).max(0.0);
    match n.kind {
        Kind::Frame => {
            let fill = fill(out, n);
            let _ = write!(
                out,
                r#"<clipPath id="c{id}"><rect x="{x0}" y="{y0}" width="{w}" height="{h}"/></clipPath><rect x="{x0}" y="{y0}" width="{w}" height="{h}" {fill}/><g clip-path="url(#c{id})">"#,
                id = n.id
            );
            list_svg(out, &n.children, assets, fonts, ctx);
            out.push_str("</g>");
            if n.stroke.is_some() && n.stroke_w > 0.0 {
                let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{w}" height="{h}" fill="none" {}/>"#, stroke(n));
            }
        }
        Kind::Rect => {
            let fill = fill(out, n);
            let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{w}" height="{h}" rx="{r}" {fill} {}/>"#, stroke(n));
        }
        Kind::Ellipse => {
            let fill = fill(out, n);
            let _ = write!(
                out,
                r#"<ellipse cx="{}" cy="{}" rx="{}" ry="{}" {fill} {}/>"#,
                x0 + w * 0.5,
                y0 + h * 0.5,
                w * 0.5,
                h * 0.5,
                stroke(n)
            );
        }
        Kind::Line => {
            let _ = write!(
                out,
                r#"<line x1="{}" y1="{}" x2="{}" y2="{}" {} stroke-linecap="round"/>"#,
                n.x,
                n.y,
                n.x + n.w,
                n.y + n.h,
                stroke(n)
            );
        }
        Kind::Path => {
            if n.points.is_empty() {
                return;
            }
            let at = |x: f32, y: f32| (n.x + x * n.w, n.y + y * n.h);
            let mut d = String::new();
            let mut sub = |points: &[crate::doc::PathPoint], closed: bool| {
                let Some(first) = points.first() else { return };
                let (sx, sy) = at(first.x, first.y);
                let _ = write!(d, "M{sx:.2} {sy:.2}");
                let count = if closed { points.len() } else { points.len() - 1 };
                for i in 0..count {
                    let (a, b) = (points[i], points[(i + 1) % points.len()]);
                    let (c1, c2, p) = (at(a.x + a.hout[0], a.y + a.hout[1]), at(b.x + b.hin[0], b.y + b.hin[1]), at(b.x, b.y));
                    let _ = write!(d, "C{:.2} {:.2} {:.2} {:.2} {:.2} {:.2}", c1.0, c1.1, c2.0, c2.1, p.0, p.1);
                }
                if closed {
                    d.push('Z');
                }
            };
            sub(&n.points, n.closed);
            for h in &n.holes {
                sub(h, true);
            }
            let fill = if n.closed { fill(out, n) } else { r#"fill="none""#.to_owned() };
            let _ = write!(
                out,
                r#"<path d="{d}" {fill} fill-rule="evenodd" {} stroke-linejoin="round" stroke-linecap="round"/>"#,
                stroke(n)
            );
        }
        Kind::Image => {
            let Some(asset) = n.image.and_then(|id| assets.get(&id)) else { return };
            let clip = if r > 0.0 {
                let _ = write!(
                    out,
                    r#"<clipPath id="c{}"><rect x="{x0}" y="{y0}" width="{w}" height="{h}" rx="{r}"/></clipPath>"#,
                    n.id
                );
                format!(r#" clip-path="url(#c{})""#, n.id)
            } else {
                String::new()
            };
            // Color overlay: warna fill mengganti RGB gambar, alpha-nya dipertahankan.
            let overlay = match n.fill {
                Some([cr, cg, cb, ca]) => {
                    let _ = write!(
                        out,
                        r##"<filter id="o{}" x="0" y="0" width="1" height="1" color-interpolation-filters="sRGB"><feFlood flood-color="#{cr:02x}{cg:02x}{cb:02x}" flood-opacity="{:.3}"/><feComposite in2="SourceGraphic" operator="in"/></filter>"##,
                        n.id,
                        ca as f32 / 255.0
                    );
                    format!(r#" filter="url(#o{})""#, n.id)
                }
                None => String::new(),
            };
            let _ = write!(
                out,
                r#"<image x="{x0}" y="{y0}" width="{w}" height="{h}" preserveAspectRatio="none"{clip}{overlay} href="data:{};base64,{}"/>"#,
                mime(&asset.bytes),
                b64::encode(&asset.bytes)
            );
            if n.stroke.is_some() && n.stroke_w > 0.0 {
                let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{w}" height="{h}" rx="{r}" fill="none" {}/>"#, stroke(n));
            }
        }
        Kind::Text => text_svg(out, n, fonts, ctx),
    }
}

/// SVG untuk satu node (mis. frame terpilih) atau seluruh dokumen. Hasil: (svg, lebar, tinggi).
pub fn to_svg(doc: &Doc, only: Option<u64>, fonts: &mut FontManager, ctx: &egui::Context) -> Option<(String, f32, f32)> {
    let assets = &doc.assets;
    let target = only.and_then(|id| doc.find(id));
    let (x0, y0, x1, y1) = match target {
        Some(n) => {
            // Stroke garis/path melebar keluar dari kotak pembatas.
            let pad = if matches!(n.kind, Kind::Line | Kind::Path) && n.stroke.is_some() { n.stroke_w * 0.5 } else { 0.0 };
            // Bayangan ikut masuk hasil export, kecuali untuk frame (ukuran halaman harus tetap).
            let pad = pad + n.shadow.filter(|s| s.color[3] > 0 && n.kind != Kind::Frame).map_or(0.0, |s| s.pad());
            let b = n.aabb();
            (b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad)
        }
        None => doc.bounds()?,
    };
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let mut out = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{x0} {y0} {w} {h}">"#
    );
    match target {
        Some(n) => node_svg(&mut out, n, assets, fonts, ctx),
        None => list_svg(&mut out, &doc.nodes, assets, fonts, ctx),
    }
    out.push_str("</svg>");
    Some((out, w, h))
}

/// Render SVG ke PNG atau JPG pada skala tertentu (1x, 2x, 0.5x, …).
fn rasterize(svg: &str, w: f32, h: f32, scale: f32) -> Result<resvg::tiny_skia::Pixmap, String> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).map_err(|e| e.to_string())?;
    let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    if pw == 0 || ph == 0 || pw > 16384 || ph > 16384 {
        return Err(format!("export size {pw}×{ph}px is out of range (max 16384px)"));
    }
    let mut pixmap = tiny_skia::Pixmap::new(pw, ph).ok_or("couldn't create the bitmap")?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    Ok(pixmap)
}

/// Render SVG ke PNG di memori (dipakai remote control untuk tangkapan desain). Hasil: (png, lebar, tinggi).
pub fn render_png(svg: &str, w: f32, h: f32, scale: f32) -> Result<(Vec<u8>, u32, u32), String> {
    let pixmap = rasterize(svg, w, h, scale)?;
    let png = pixmap.encode_png().map_err(|e| e.to_string())?;
    Ok((png, pixmap.width(), pixmap.height()))
}

pub fn save_raster(svg: &str, w: f32, h: f32, scale: f32, jpg: bool, path: &Path) -> Result<(), String> {
    let pixmap = rasterize(svg, w, h, scale)?;
    let (pw, ph) = (pixmap.width(), pixmap.height());
    if !jpg {
        return pixmap.save_png(path).map_err(|e| e.to_string());
    }
    // JPG tidak punya transparansi: ratakan di atas putih (data pixmap premultiplied).
    let rgb: Vec<u8> = pixmap
        .data()
        .chunks_exact(4)
        .flat_map(|p| {
            let rest = 255 - p[3] as u16;
            [(p[0] as u16 + rest).min(255) as u8, (p[1] as u16 + rest).min(255) as u8, (p[2] as u16 + rest).min(255) as u8]
        })
        .collect();
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    image::codecs::jpeg::JpegEncoder::new_with_quality(std::io::BufWriter::new(file), 92)
        .encode(&rgb, pw, ph, image::ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())
}

/// Simpan beberapa SVG sebagai satu PDF, satu halaman per SVG: (svg, lebar, tinggi).
/// Isinya tetap vektor (teks sudah berupa outline), dan 1 satuan dokumen = 1 pt, sehingga frame
/// dari preset kertas (A4 = 595×842) keluar dengan ukuran kertas yang tepat.
pub fn save_pdf(pages: &[(String, f32, f32)], path: &Path) -> Result<(), String> {
    use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref};
    use resvg::usvg;
    let mut alloc = Ref::new(1);
    let (catalog_id, tree_id) = (alloc.bump(), alloc.bump());
    let mut pdf = Pdf::new();
    let mut kids = Vec::new();
    let name = Name(b"S");
    for (svg, w, h) in pages {
        let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).map_err(|e| e.to_string())?;
        // Bagian yang memakai filter (bayangan, color overlay) menjadi bitmap; 3× cukup tajam untuk cetak.
        let options = svg2pdf::ConversionOptions { raster_scale: 3.0, ..Default::default() };
        let (chunk, root) = svg2pdf::to_chunk(&tree, options).map_err(|e| e.to_string())?;
        // Nomor objek tiap halaman dibuat ulang supaya tidak bertabrakan di dalam satu file.
        let mut map = std::collections::HashMap::new();
        let chunk = chunk.renumber(|old| *map.entry(old).or_insert_with(|| alloc.bump()));
        let root = *map.get(&root).ok_or("the page couldn't be converted")?;
        let (page_id, content_id) = (alloc.bump(), alloc.bump());
        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, *w, *h));
        page.parent(tree_id);
        page.contents(content_id);
        page.resources().x_objects().pair(name, root);
        page.finish();
        // Hasil konversi berukuran 1×1; dibentangkan ke ukuran halaman.
        let mut content = Content::new();
        content.transform([*w, 0.0, 0.0, *h, 0.0, 0.0]).x_object(name);
        pdf.stream(content_id, &content.finish());
        pdf.extend(&chunk);
        kids.push(page_id);
    }
    pdf.catalog(catalog_id).pages(tree_id);
    pdf.pages(tree_id).kids(kids.iter().copied()).count(kids.len() as i32);
    std::fs::write(path, pdf.finish()).map_err(|e| e.to_string())
}

/// Urutan halaman untuk export PDF: frame level atas yang terlihat, dibaca seperti teks
/// (baris demi baris dari atas, tiap baris dari kiri ke kanan).
pub fn pdf_pages(doc: &Doc) -> Vec<u64> {
    let mut frames: Vec<(u64, (f32, f32, f32, f32))> =
        doc.nodes.iter().filter(|n| n.kind == Kind::Frame && n.visible).map(|n| (n.id, n.aabb())).collect();
    frames.sort_by(|a, b| a.1 .1.total_cmp(&b.1 .1));
    let mut rows: Vec<Vec<(u64, (f32, f32, f32, f32))>> = Vec::new();
    for frame in frames {
        match rows.last_mut() {
            // Masih satu baris kalau tepi atasnya belum melewati tengah frame pertama di baris itu.
            Some(row) if frame.1 .1 < (row[0].1 .1 + row[0].1 .3) * 0.5 => row.push(frame),
            _ => rows.push(vec![frame]),
        }
    }
    for row in &mut rows {
        row.sort_by(|a, b| a.1 .0.total_cmp(&b.1 .0));
    }
    rows.into_iter().flatten().map(|f| f.0).collect()
}

/// Simpan tangkapan layar RGBA (dipakai mode --bench).
pub fn save_rgba(path: &Path, w: u32, h: u32, rgba: &[u8]) -> Result<(), String> {
    let mut pixmap = resvg::tiny_skia::Pixmap::new(w, h).ok_or("couldn't create the bitmap")?;
    if pixmap.data().len() != rgba.len() {
        return Err("screenshot size mismatch".into());
    }
    pixmap.data_mut().copy_from_slice(rgba);
    pixmap.save_png(path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::doc::{Gradient, PathPoint, Stop};

    fn sample() -> Doc {
        let mut doc = Doc::default();
        let mut f = Node::new(doc.alloc_id(), Kind::Frame, 0.0, 0.0);
        f.w = 200.0;
        f.h = 100.0;
        f.fill = Some([255, 255, 255, 255]);
        let mut r = Node::new(doc.alloc_id(), Kind::Rect, 20.0, 20.0);
        r.w = 60.0;
        r.h = 40.0;
        r.fill = Some([255, 0, 0, 255]);
        r.radius = 8.0;
        let mut t = Node::new(doc.alloc_id(), Kind::Text, 100.0, 20.0);
        t.text = "Halo <Kanvas> & \"teman\"".into();
        t.fill = Some([0, 0, 0, 255]);
        f.children.push(r);
        f.children.push(t);
        doc.insert(f, None);
        doc
    }

    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |_| {});
        ctx
    }

    #[test]
    fn save_open_and_hit_test() {
        let doc = sample();
        let json = serde_json::to_vec(&doc).unwrap();
        let back: Doc = serde_json::from_slice(&json).unwrap();
        assert_eq!(back.count(), 3);
        assert_eq!(back.nodes[0].children, doc.nodes[0].children);
        // Anak frame didahulukan dari frame-nya; di luar semua = None.
        assert_eq!(back.hit(30.0, 30.0, 1.0), Some(2));
        assert_eq!(back.hit(190.0, 90.0, 1.0), Some(1));
        assert_eq!(back.hit(500.0, 500.0, 1.0), None);
        assert_eq!(back.parent_of(2), Some(1));
    }

    #[test]
    fn export_png_with_text_outlines() {
        let doc = sample();
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let (svg, w, h) = to_svg(&doc, Some(1), &mut fonts, &ctx).unwrap();
        assert_eq!((w, h), (200.0, 100.0));
        // Teks jadi outline: tidak ada elemen <text>, ada path glyph.
        assert!(!svg.contains("<text"));
        assert!(svg.contains(r##"<path d="M"##));
        let path = std::env::temp_dir().join("kanvas_test_export.png");
        save_raster(&svg, w, h, 2.0, false, &path).unwrap();
        let png = resvg::tiny_skia::Pixmap::load_png(&path).unwrap();
        assert_eq!((png.width(), png.height()), (400, 200));
        // Piksel di tengah kotak merah.
        let px = png.pixel(80, 70).unwrap();
        assert_eq!((px.red(), px.green(), px.blue(), px.alpha()), (255, 0, 0, 255));
        // Sudut frame putih.
        let px = png.pixel(390, 190).unwrap();
        assert_eq!((px.red(), px.green(), px.blue()), (255, 255, 255));
        // Ada piksel gelap di area teks (x 100..200 dunia, y 20..40 dunia).
        let dark = (200..400u32)
            .flat_map(|x| (40..80u32).map(move |y| (x, y)))
            .filter(|&(x, y)| png.pixel(x, y).unwrap().red() < 80)
            .count();
        assert!(dark > 50, "text outlines should be visible, got {dark} dark pixels");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn export_clipping_mask_follows_base_alpha() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut doc = Doc::default();
        // Latar putih 200×100 supaya ukuran export tetap.
        let mut bg = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        (bg.w, bg.h, bg.fill) = (200.0, 100.0, Some([255, 255, 255, 255]));
        doc.insert(bg, None);
        // Alas: kotak 100×100 dengan gradasi hitam pekat (kiri) → transparan (kanan).
        let mut base = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        (base.w, base.h, base.fill) = (100.0, 100.0, Some([0, 0, 0, 255]));
        base.gradient = Some(Gradient {
            radial: false,
            angle: 0.0,
            stops: vec![Stop { pos: 0.0, color: [0, 0, 0, 255] }, Stop { pos: 1.0, color: [0, 0, 0, 0] }],
            line: None,
            radial_line: None,
        });
        doc.insert(base, None);
        // Isi: kotak merah selebar dokumen, di-clip ke alas.
        let mut red = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        (red.w, red.h, red.fill, red.clip) = (200.0, 100.0, Some([255, 0, 0, 255]), true);
        doc.insert(red, None);
        let (svg, w, h) = to_svg(&doc, None, &mut fonts, &ctx).unwrap();
        let (png, ..) = render_png(&svg, w, h, 1.0).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        let (left, mid, right) = (img.get_pixel(3, 50), img.get_pixel(50, 50), img.get_pixel(150, 50));
        assert!(left[0] > 235 && left[1] < 25, "inside the opaque end: red, got {left:?}");
        // Alas 50% hitam di atas putih = abu 128, lalu merah 50% di atasnya.
        assert!((180..200).contains(&mid[0]) && (55..75).contains(&mid[1]), "half-transparent base: half red, got {mid:?}");
        assert_eq!(right.0, [255, 255, 255, 255], "outside the base: nothing drawn");
    }

    #[test]
    fn export_gradient_path_and_jpg() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut doc = Doc::default();
        // Persegi (sebagai path) dengan gradasi kiri (merah) → kanan (biru).
        let mut n = Node::new(doc.alloc_id(), Kind::Path, 0.0, 0.0);
        n.w = 100.0;
        n.h = 100.0;
        let p = |x, y| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2] };
        n.points = vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)];
        n.closed = true;
        n.fill = Some([0, 0, 0, 255]);
        n.gradient = Some(Gradient {
            radial: false,
            angle: 0.0,
            stops: vec![Stop { pos: 0.0, color: [255, 0, 0, 255] }, Stop { pos: 1.0, color: [0, 0, 255, 255] }],
            line: None,
            radial_line: None,
        });
        doc.insert(n, None);
        let (svg, w, h) = to_svg(&doc, None, &mut fonts, &ctx).unwrap();
        assert!(svg.contains("<linearGradient"));
        let path = std::env::temp_dir().join("kanvas_test_export.jpg");
        save_raster(&svg, w, h, 1.0, true, &path).unwrap();
        let img = image::open(&path).unwrap().to_rgb8();
        assert_eq!((img.width(), img.height()), (100, 100));
        let (left, right) = (img.get_pixel(5, 50), img.get_pixel(94, 50));
        assert!(left[0] > 200 && left[2] < 60, "left should be red, got {left:?}");
        assert!(right[2] > 200 && right[0] < 60, "right should be blue, got {right:?}");
        let _ = std::fs::remove_file(path);
    }
}
