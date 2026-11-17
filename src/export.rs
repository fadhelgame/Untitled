//! Export: dokumen -> SVG, lalu PNG/JPG lewat resvg (satu jalur render untuk semuanya).
//! Teks diekspor sebagai outline glyph, jadi hasilnya sama persis dengan canvas
//! (paragraf, gaya per-huruf, spasi) dan tidak bergantung pada font di komputer lain.

use crate::app::blend;
use crate::app::render::{is_svg, layout_text, text_offset};
use crate::doc::{b64, Asset, Doc, Kind, Node, Shadow};
use crate::fonts::FontManager;
use eframe::egui;
use std::fmt::Write;
use std::path::Path;

type Assets = std::collections::BTreeMap<u64, std::sync::Arc<Asset>>;

/// Background blur in an export.
///
/// SVG has no portable way to blur what lies behind an element (neither resvg nor PDF knows a
/// backdrop filter), so `to_svg` works it out itself: for every node with a background blur, in
/// paint order, it renders what is painted before that node, blurs it, and embeds the result as
/// a picture clipped to the shape of the node, under the node's own fill. PNG, JPG, SVG and PDF
/// all get that same picture; it is a bitmap (`BACKDROP_SCALE` pixels per unit or more) even
/// where the rest of the file is vector.
#[derive(Default)]
struct Stage {
    /// The finished pictures, as SVG, by the id of their node.
    images: std::collections::HashMap<u64, String>,
    /// While what is behind a node is written out: that node, where writing ends...
    stop: Option<u64>,
    /// ...whether it has been reached...
    stopped: bool,
    /// ...and the area that is wanted (nodes outside of it are left out).
    cull: Option<(f32, f32, f32, f32)>,
}

thread_local! {
    static STAGE: std::cell::RefCell<Stage> = std::cell::RefCell::new(Stage::default());
}

fn stage<R>(f: impl FnOnce(&mut Stage) -> R) -> R {
    STAGE.with(|s| f(&mut s.borrow_mut()))
}

/// Pixels per document unit of an embedded blurred backdrop, at least.
const BACKDROP_SCALE: f32 = 2.0;

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
        Some(_) if n.stroke_w > 0.0 => {
            let dash = if n.dash > 0.0 {
                format!(r#" stroke-dasharray="{} {}""#, n.dash, if n.gap > 0.0 { n.gap } else { n.dash })
            } else {
                String::new()
            };
            // Ends and corners as the node has them (butt and miter are what SVG assumes).
            let cap = ["", r#" stroke-linecap="round""#, r#" stroke-linecap="square""#][n.cap.min(2) as usize];
            let join = ["", r#" stroke-linejoin="round""#, r#" stroke-linejoin="bevel""#][n.join.min(2) as usize];
            format!(r#"{} stroke-width="{}" stroke-miterlimit="{}"{dash}{cap}{join}"#, paint("stroke", n.stroke), n.stroke_w, crate::outline::MITER_LIMIT)
        }
        _ => String::new(),
    }
}

/// Path data of contours in world coordinates.
fn contour_d(contours: &[crate::outline::Contour]) -> String {
    let mut d = String::new();
    for c in contours {
        let n = c.pts.len();
        if n < 2 {
            continue;
        }
        let _ = write!(d, "M{:.3} {:.3}", c.pts[0].x, c.pts[0].y);
        for i in 0..if c.closed { n } else { n - 1 } {
            let (a, b) = (c.pts[i], c.pts[(i + 1) % n]);
            let _ = write!(d, "C{:.3} {:.3} {:.3} {:.3} {:.3} {:.3}", a.x + a.hout[0], a.y + a.hout[1], b.x + b.hin[0], b.y + b.hin[1], b.x, b.y);
        }
        if c.closed {
            d.push('Z');
        }
    }
    d
}

/// Stroke kotak sebagai elemen sendiri, digeser ke dalam/luar sesuai posisi stroke.
fn stroke_rect(out: &mut String, n: &Node, r: f32) {
    if n.stroke.is_none() || n.stroke_w <= 0.0 {
        return;
    }
    // Dashes around rounded corners start where the canvas starts them, not where an SVG
    // `<rect>` would.
    if n.dash > 0.0 && r > 0.0 {
        if let Some(lines) = crate::outline::stroke_lines(n) {
            let _ = write!(out, r#"<path d="{}" fill="none" {}/>"#, contour_d(&lines), stroke(n));
            return;
        }
    }
    let (x0, y0, x1, y1) = n.bounds();
    let i = n.stroke_inset();
    let (w, h) = ((x1 - x0 - i * 2.0).max(0.0), (y1 - y0 - i * 2.0).max(0.0));
    let _ = write!(
        out,
        r#"<rect x="{}" y="{}" width="{w}" height="{h}" rx="{}" fill="none" {}/>"#,
        x0 + i,
        y0 + i,
        // Square corners stay square wherever the stroke is, as on the canvas.
        if r > 0.0 { (r - i).max(0.0) } else { 0.0 },
        stroke(n)
    );
}

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
    // On a path: the glyphs where `textpath` puts them, as on the canvas.
    if let Some(guide) = n.on_path.and_then(|_| fonts.paths.get(&n.id).cloned()) {
        for part in crate::textpath::path_parts(ctx, n, &guide, fonts) {
            let mut all: Vec<crate::outline::Contour> = part.glyphs.into_iter().flatten().collect();
            all.extend(part.bars);
            let _ = write!(out, r#"<path d="{}" {}/>"#, contour_d(&all), paint("fill", Some(part.color.to_srgba_unmultiplied())));
        }
        return;
    }
    // Layout di ukuran dunia (dibatasi supaya atlas glyph tidak membengkak), lalu diskalakan balik.
    let f = (200.0 / n.max_size()).min(1.0);
    let layout = layout_text(ctx, n, &n.text, fonts, f, 1.0, true);
    // Where the galley's origin is in the document (a box of fixed height: by its vertical alignment).
    let off = text_offset(n, &layout, f) / f;
    let (ox, oy) = (n.x + off.x, n.y + off.y);
    // Satu <path> per warna berurutan.
    let mut runs: Vec<(egui::Color32, String)> = Vec::new();
    // The text, then the markers of its list (a galley of their own in the same space).
    for galley in std::iter::once(&layout.galley).chain(&layout.markers) {
        for g in galley.rows.iter().flat_map(|row| &row.glyphs) {
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
            let mut builder = Outline { d: &mut d, x: ox + g.pos.x / f, y: oy + g.pos.y / f, s: scale };
            if face.outline_glyph(gid, &mut builder).is_none() || d.is_empty() {
                continue;
            }
            match runs.last_mut() {
                Some((color, path)) if *color == format.color => path.push_str(&d),
                _ => runs.push((format.color, d)),
            }
        }
    }
    // Underlines and strikethroughs: the same rectangles the canvas draws.
    for line in layout.deco.iter() {
        let (r, color) = (line.rect, line.color);
        let d = format!("M{:.2} {:.2}H{:.2}V{:.2}H{:.2}Z", ox + r.min.x / f, oy + r.min.y / f, ox + r.max.x / f, oy + r.max.y / f, ox + r.min.x / f);
        match runs.iter_mut().rev().find(|run| run.0 == color) {
            Some((_, path)) => path.push_str(&d),
            None => runs.push((color, d)),
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
            run_svg(out, &nodes[i..j], assets, fonts, ctx);
        } else {
            let id = nodes[i].as_ref().id;
            let _ = write!(out, r#"<g id="b{id}">"#);
            let mut area: Option<(f32, f32, f32, f32)> = None;
            run_svg(out, &nodes[i..j], assets, fonts, ctx);
            for n in &nodes[i..j] {
                let n = n.as_ref();
                // Stroke, glyph, dan layer blur bisa keluar dari kotak pembatas.
                let (b, pad) = (n.aabb(), n.stroke_w.max(0.0) + n.blur_pad() + if n.kind == Kind::Text { n.max_size() } else { 0.0 });
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
            run_svg(out, &nodes[j..k], assets, fonts, ctx);
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
    /// Seperti `Full` tapi tanpa blend mode: blend dipasang pembungkusnya (grup layer blur).
    NoBlend,
    /// Objek tanpa bayangannya: bayangan digambar bersama untuk seluruh group.
    NoShadow,
    /// Hanya bentuknya (tanpa bayangan dan blend), sebagai sumber siluet bayangan group.
    Shape,
}

/// Potongan daftar saudara: node biasa ditulis apa adanya, rangkaian ber-layer-blur dibungkus
/// satu grup berfilter.
fn run_svg<T: AsRef<Node>>(out: &mut String, nodes: &[T], assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) {
    let mut i = 0;
    while i < nodes.len() {
        let end = crate::doc::blur_run(nodes, i);
        if nodes[i].as_ref().blur > 0.0 {
            blur_group(out, &nodes[i..end], assets, fonts, ctx, true);
            i = end;
            continue;
        }
        let end = crate::doc::shadow_run(nodes, i);
        if end > i + 1 {
            shadow_group(out, &nodes[i..end], assets, fonts, ctx, false);
        } else {
            node_fx(out, nodes[i].as_ref(), assets, fonts, ctx, Fx::Full);
        }
        i = end;
    }
}

/// Area dunia yang dicakup satu rangkaian layer blur, termasuk luberan blur, stroke, glyph, dan
/// bayangan. `None`: tidak ada anggota yang terlihat.
pub fn blur_region<T: AsRef<Node>>(nodes: &[T]) -> Option<(f32, f32, f32, f32)> {
    let mut area: Option<(f32, f32, f32, f32)> = None;
    for n in nodes.iter().map(|n| n.as_ref()).filter(|n| n.visible) {
        let text = if n.kind == Kind::Text { n.max_size() } else { 0.0 };
        let pad = n.blur_pad() + n.stroke_w.max(0.0) + text + n.shadow.filter(|s| s.color[3] > 0).map_or(0.0, |s| s.pad());
        let b = n.aabb();
        let b = (b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad);
        area = Some(area.map_or(b, |a| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))));
    }
    area
}

/// Blend mode satu rangkaian layer blur: dipakai kalau semua anggota yang terlihat memakai mode
/// yang sama (hasil buramnya lalu di-blend sebagai satu gambar); kalau berbeda-beda, 0 (Normal).
pub fn run_blend<T: AsRef<Node>>(nodes: &[T]) -> u8 {
    let mut modes = nodes.iter().map(|n| n.as_ref()).filter(|n| n.visible).map(|n| n.blend);
    let first = modes.next().unwrap_or(0);
    if modes.all(|m| m == first) {
        first
    } else {
        0
    }
}

/// Satu rangkaian layer blur: anggotanya digambar ke satu grup, lalu grup itu diburamkan.
/// `blend`: pasang blend mode bersama di grupnya (di canvas, blend dikerjakan GPU).
fn blur_group<T: AsRef<Node>>(out: &mut String, nodes: &[T], assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context, blend: bool) {
    let Some((x0, y0, x1, y1)) = blur_region(nodes) else { return };
    let first = nodes[0].as_ref();
    let mode = run_blend(nodes);
    let _ = write!(
        out,
        r#"<filter id="bl{id}" filterUnits="userSpaceOnUse" x="{x0}" y="{y0}" width="{}" height="{}" color-interpolation-filters="sRGB"><feGaussianBlur stdDeviation="{}"/></filter><g filter="url(#bl{id})""#,
        x1 - x0,
        y1 - y0,
        first.blur * 0.5,
        id = first.id
    );
    if mode != 0 && blend {
        let _ = write!(out, r#" style="mix-blend-mode:{}""#, blend::CSS[(mode as usize).min(blend::CSS.len() - 1)]);
    }
    out.push('>');
    for n in nodes {
        node_fx(out, n.as_ref(), assets, fonts, ctx, if mode != 0 { Fx::NoBlend } else { Fx::Full });
    }
    out.push_str("</g>");
}

/// SVG satu rangkaian layer blur (sudah buram), untuk tekstur di canvas. Blend mode bersama tidak
/// ikut: canvas yang mengerjakannya. Hasil: (svg, area dunia yang dicakup).
pub fn blur_svg<T: AsRef<Node>>(nodes: &[T], assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) -> Option<(String, (f32, f32, f32, f32))> {
    let region = blur_region(nodes)?;
    let (w, h) = (region.2 - region.0, region.3 - region.1);
    let mut out = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{} {} {w} {h}">"#, region.0, region.1);
    blur_group(&mut out, nodes, assets, fonts, ctx, false);
    out.push_str("</svg>");
    Some((out, region))
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
    filter_def(out, &format!("sh{}", n.id), (x0 - pad, y0 - pad, x1 + pad, y1 + pad), s)
}

/// `<filter>` drop shadow ber-id `id` yang mencakup `area`; hasilnya atribut `filter`-nya.
fn filter_def(out: &mut String, id: &str, (x0, y0, x1, y1): (f32, f32, f32, f32), s: &Shadow) -> String {
    let [r, g, b, a] = s.color;
    let _ = write!(
        out,
        r#"<filter id="{id}" filterUnits="userSpaceOnUse" x="{x0}" y="{y0}" width="{}" height="{}" color-interpolation-filters="sRGB">"#,
        x1 - x0,
        y1 - y0
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
    format!(r#" filter="url(#{id})""#)
}

/// Area dunia yang dicakup bayangan satu rangkaian (`shadow_run`), termasuk luberannya.
/// `None`: tidak ada bayangan, atau tidak ada anggota yang terlihat.
pub fn shadow_region<T: AsRef<Node>>(nodes: &[T]) -> Option<(f32, f32, f32, f32)> {
    let s = nodes.first()?.as_ref().shadow.filter(|s| s.color[3] > 0)?;
    let mut area: Option<(f32, f32, f32, f32)> = None;
    for n in nodes.iter().map(|n| n.as_ref()).filter(|n| n.visible) {
        let (b, pad) = (n.aabb(), shadow_pad(n, &s));
        let b = (b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad);
        area = Some(area.map_or(b, |a| (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))));
    }
    area
}

/// Anggota group dengan bayangan bersama: siluet gabungannya dibayangi sekali, di bawah semua
/// anggota. Arah bayangan mengikuti dokumen (tidak ikut rotasi tiap anggota).
/// `only`: tulis bayangannya saja (tekstur bayangan di canvas).
fn shadow_group<T: AsRef<Node>>(out: &mut String, nodes: &[T], assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context, only: bool) {
    let first = nodes[0].as_ref();
    let (Some(s), Some(area)) = (first.shadow, shadow_region(nodes)) else {
        for n in nodes {
            node_fx(out, n.as_ref(), assets, fonts, ctx, Fx::NoShadow);
        }
        return;
    };
    let filter = filter_def(out, &format!("gsf{}", first.id), area, &s);
    if only {
        let _ = write!(out, "<g{filter}>");
        for n in nodes {
            node_fx(out, n.as_ref(), assets, fonts, ctx, Fx::Shape);
        }
        out.push_str("</g>");
        return;
    }
    // A member with a background blur: its blurred backdrop must not darken the shadow, and
    // what is behind it includes the whole shadow. So the shadow is written from the shapes
    // alone, then the members over it.
    let staged = stage(|s| nodes.iter().map(|n| n.as_ref().id).any(|id| s.stop == Some(id) || s.images.contains_key(&id)));
    if staged {
        let _ = write!(out, "<g{filter}>");
        for n in nodes {
            node_fx(out, n.as_ref(), assets, fonts, ctx, Fx::Shape);
        }
        out.push_str("</g>");
        for n in nodes {
            node_fx(out, n.as_ref(), assets, fonts, ctx, Fx::NoShadow);
        }
        return;
    }
    // Anggota ditulis sekali lalu dipakai dua kali (lihat `node_fx`): salinan berfilter untuk
    // bayangan, dan aslinya di atasnya.
    let _ = write!(out, r#"<defs><g id="gs{}">"#, first.id);
    for n in nodes {
        node_fx(out, n.as_ref(), assets, fonts, ctx, Fx::NoShadow);
    }
    let _ = write!(out, r##"</g></defs><use href="#gs{id}"{filter}/><use href="#gs{id}"/>"##, id = first.id);
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

/// How far `n` can paint, with its stroke, glyphs, shadow and layer blur.
fn reach(n: &Node) -> (f32, f32, f32, f32) {
    let text = if n.kind == Kind::Text { n.max_size() } else { 0.0 };
    let pad = n.stroke_w.max(0.0) + n.blur_pad() + text + n.shadow.filter(|s| s.color[3] > 0).map_or(0.0, |s| s.pad());
    let b = n.aabb();
    (b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad)
}

fn touches(a: (f32, f32, f32, f32), b: (f32, f32, f32, f32)) -> bool {
    a.0 <= b.2 && a.2 >= b.0 && a.1 <= b.3 && a.3 >= b.1
}

fn node_fx(out: &mut String, n: &Node, assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context, fx: Fx) {
    if !n.visible {
        return;
    }
    // Background blur (see `Stage`): the picture of what is behind this node, if it has one, and
    // whether writing stops here because that picture is being made.
    let (mut stop, mut glass) = (false, None);
    if !matches!(fx, Fx::ShadowOnly | Fx::Shape) {
        let skip = stage(|s| {
            stop = s.stop == Some(n.id);
            glass = s.images.get(&n.id).cloned();
            s.stopped || (!stop && s.cull.is_some_and(|c| !touches(reach(n), c)))
        });
        if skip {
            return;
        }
    }
    let shadow = n.shadow.filter(|s| s.color[3] > 0 && !matches!(fx, Fx::NoShadow | Fx::Shape));
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
    let blended = n.blend != 0 && matches!(fx, Fx::Full | Fx::NoShadow);
    if blended {
        // The blurred backdrop is not part of what is blended: it goes under the group.
        if let Some(glass) = glass.take() {
            let _ = write!(out, "<g{attrs}>{glass}</g>");
        }
        let _ = write!(attrs, r#" style="mix-blend-mode:{}""#, blend::CSS[(n.blend as usize).min(blend::CSS.len() - 1)]);
    }
    if stop {
        // Everything behind this node has been written. Its own drop shadow is the last of
        // that (not when the node is blended: then its backdrop lies under the shadow too).
        if let Some(s) = shadow.filter(|_| !blended) {
            let filter = shadow_filter(out, n, &s);
            let _ = write!(out, "<g{attrs}><g{filter}>");
            match solid_box(n, assets) {
                Some(r) => {
                    let (x0, y0, x1, y1) = n.bounds();
                    let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{}" height="{}" rx="{r}"/>"#, x1 - x0, y1 - y0);
                }
                None => node_inner(out, n, assets, fonts, ctx),
            }
            out.push_str("</g></g>");
        }
        stage(|s| s.stopped = true);
        return;
    }
    let glass = glass.unwrap_or_default();
    let filter = shadow.map(|s| shadow_filter(out, n, &s));
    if !attrs.is_empty() {
        let _ = write!(out, "<g{attrs}>");
    }
    // Sumber siluet bayangan: kotak penuh cukup diwakili satu `<rect>`.
    let shape = |out: &mut String, fonts: &mut FontManager| match solid_box(n, assets) {
        Some(r) => {
            let (x0, y0, x1, y1) = n.bounds();
            let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{}" height="{}" rx="{r}"/>"#, x1 - x0, y1 - y0);
        }
        None => node_inner(out, n, assets, fonts, ctx),
    };
    match (&filter, fx) {
        (None, Fx::Shape) => shape(out, fonts),
        (None, _) => {
            out.push_str(&glass);
            node_inner(out, n, assets, fonts, ctx);
        }
        (Some(filter), Fx::ShadowOnly) => {
            let _ = write!(out, "<g{filter}>");
            shape(out, fonts);
            out.push_str("</g>");
        }
        // Objek ditulis sekali lalu dipakai dua kali: salinan berfilter untuk bayangan, dan objek
        // aslinya di atasnya tanpa filter. Dengan begitu hanya bayangan yang menjadi bitmap di PDF;
        // objeknya sendiri tetap vektor.
        (Some(filter), _) => {
            let _ = write!(out, r#"<defs><g id="n{}">"#, n.id);
            node_inner(out, n, assets, fonts, ctx);
            let _ = write!(out, r##"</g></defs><use href="#n{id}"{filter}/>{glass}<use href="#n{id}"/>"##, id = n.id);
        }
    }
    if !attrs.is_empty() {
        out.push_str("</g>");
    }
}

/// SVG yang hanya berisi bayangan satu rangkaian (satu node, atau anggota group yang berbayangan
/// bersama), untuk tekstur bayangan di canvas. Hasil: (svg, area dunia yang dicakup).
pub fn shadow_svg<T: AsRef<Node>>(nodes: &[T], assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) -> Option<(String, (f32, f32, f32, f32))> {
    let region = shadow_region(nodes)?;
    let (w, h) = (region.2 - region.0, region.3 - region.1);
    let mut out = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{} {} {w} {h}">"#, region.0, region.1);
    match nodes {
        [n] => node_fx(&mut out, n.as_ref(), assets, fonts, ctx, Fx::ShadowOnly),
        _ => shadow_group(&mut out, nodes, assets, fonts, ctx, true),
    }
    out.push_str("</svg>");
    Some((out, region))
}

/// Opens the group that mirrors a flipped node about the centre of its box (`Node::flips`).
/// It sits inside the rotation, and inside the drop shadow, which keeps its direction.
/// Result: whether a group was opened.
fn flip_open(out: &mut String, n: &Node) -> bool {
    let (h, v) = n.flips();
    if h || v {
        let (cx, cy) = n.center();
        let (tx, sx) = if h { (cx * 2.0, -1) } else { (0.0, 1) };
        let (ty, sy) = if v { (cy * 2.0, -1) } else { (0.0, 1) };
        let _ = write!(out, r#"<g transform="translate({tx} {ty}) scale({sx} {sy})">"#);
    }
    h || v
}

fn node_inner(out: &mut String, n: &Node, assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) {
    // What a frame holds has places and flips of its own: only the frame's own paint is mirrored.
    let flipped = n.kind != Kind::Frame && flip_open(out, n);
    node_body(out, n, assets, fonts, ctx);
    if flipped {
        out.push_str("</g>");
    }
}

fn node_body(out: &mut String, n: &Node, assets: &Assets, fonts: &mut FontManager, ctx: &egui::Context) {
    let (x0, y0, x1, y1) = n.bounds();
    let (w, h) = (x1 - x0, y1 - y0);
    let r = n.radius.min(w * 0.5).min(h * 0.5).max(0.0);
    match n.kind {
        Kind::Frame => {
            let fill = fill(out, n);
            let flipped = flip_open(out, n);
            let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{w}" height="{h}" {fill}/>"#);
            if flipped {
                out.push_str("</g>");
            }
            // The inner shadow of a frame lies on its fill, under what the frame holds.
            inner_fx(out, n, fonts, ctx, Inner::Plain);
            let _ = write!(out, r#"<clipPath id="c{id}"><rect x="{x0}" y="{y0}" width="{w}" height="{h}"/></clipPath><g clip-path="url(#c{id})">"#, id = n.id);
            list_svg(out, &n.children, assets, fonts, ctx);
            out.push_str("</g>");
            // (Not when only what is behind a node inside this frame is being written.)
            if !stage(|s| s.stopped) {
                stroke_rect(out, n, 0.0);
            }
        }
        Kind::Rect => {
            let fill = fill(out, n);
            let _ = write!(out, r#"<rect x="{x0}" y="{y0}" width="{w}" height="{h}" rx="{r}" {fill}/>"#);
            inner_fx(out, n, fonts, ctx, Inner::Flipped);
            stroke_rect(out, n, r);
        }
        Kind::Ellipse => {
            let fill = fill(out, n);
            let (cx, cy) = (x0 + w * 0.5, y0 + h * 0.5);
            let _ = write!(out, r#"<ellipse cx="{cx}" cy="{cy}" rx="{}" ry="{}" {fill}/>"#, w * 0.5, h * 0.5);
            inner_fx(out, n, fonts, ctx, Inner::Flipped);
            if n.stroke.is_some() && n.stroke_w > 0.0 {
                let i = n.stroke_inset();
                let _ = write!(
                    out,
                    r#"<ellipse cx="{cx}" cy="{cy}" rx="{}" ry="{}" fill="none" {}/>"#,
                    (w * 0.5 - i).max(0.0),
                    (h * 0.5 - i).max(0.0),
                    stroke(n)
                );
            }
        }
        Kind::Line => {
            let _ = write!(
                out,
                r#"<line x1="{}" y1="{}" x2="{}" y2="{}" {}/>"#,
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
            let d = path_d(n);
            let fill = if n.closed { fill(out, n) } else { r#"fill="none""#.to_owned() };
            if n.inner().is_some() {
                // The inner shadow goes between the fill and the stroke: two elements.
                let _ = write!(out, r#"<path d="{d}" {fill} fill-rule="evenodd"/>"#);
                inner_fx(out, n, fonts, ctx, Inner::Flipped);
                if n.stroke.is_some() && n.stroke_w > 0.0 {
                    let _ = write!(out, r#"<path d="{d}" fill="none" {}/>"#, stroke(n));
                }
                return;
            }
            let _ = write!(
                out,
                r#"<path d="{d}" {fill} fill-rule="evenodd" {}/>"#,
                stroke(n)
            );
        }
        Kind::Image => {
            let Some(asset) = n.image.and_then(|id| assets.get(&id)) else { return };
            // Gambar yang di-crop: gambar utuh ditaruh di kotaknya sendiri lalu dipotong ke kotak node.
            let (ix0, iy0, ix1, iy1) = n.image_rect();
            let clip = if r > 0.0 || n.crop.is_some() {
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
                r#"<image x="{ix0}" y="{iy0}" width="{}" height="{}" preserveAspectRatio="none"{clip}{overlay} href="data:{};base64,{}"/>"#,
                ix1 - ix0,
                iy1 - iy0,
                mime(&asset.bytes),
                b64::encode(&asset.bytes)
            );
            inner_fx(out, n, fonts, ctx, Inner::Flipped);
            stroke_rect(out, n, r);
        }
        Kind::Text => {
            text_svg(out, n, fonts, ctx);
            inner_fx(out, n, fonts, ctx, Inner::Flipped);
        }
    }
}

/// Path data of a path node, in the document: its contours with the rounded corners (`radius`)
/// as curves.
fn path_d(n: &Node) -> String {
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
    // Sudut yang dibulatkan (`radius`) ikut diekspor sebagai kurva.
    sub(&n.rounded_points(&n.points, n.closed), n.closed);
    for h in &n.holes {
        sub(&n.rounded_points(h, true), true);
    }
    d
}

/// The shape of `n` as SVG elements without a paint of their own (so: black), where the node
/// itself is drawn before it is turned or flipped. An inner shadow falls into this shape and a
/// background blur shows in it. A frame, an image: their box.
fn silhouette(n: &Node, fonts: &mut FontManager, ctx: &egui::Context) -> Option<String> {
    let (x0, y0, x1, y1) = n.bounds();
    let (w, h) = (x1 - x0, y1 - y0);
    let rect = |r: f32| format!(r#"<rect x="{x0}" y="{y0}" width="{w}" height="{h}" rx="{r}"/>"#);
    match n.kind {
        Kind::Frame => Some(rect(0.0)),
        Kind::Rect | Kind::Image => Some(rect(n.radius.min(w * 0.5).min(h * 0.5).max(0.0))),
        Kind::Ellipse => Some(format!(r#"<ellipse cx="{}" cy="{}" rx="{}" ry="{}"/>"#, x0 + w * 0.5, y0 + h * 0.5, w * 0.5, h * 0.5)),
        Kind::Path if n.closed && !n.points.is_empty() => Some(format!(r#"<path d="{}" fill-rule="evenodd" clip-rule="evenodd"/>"#, path_d(n))),
        Kind::Text => {
            let mut glyphs = String::new();
            text_svg(&mut glyphs, n, fonts, ctx);
            (!glyphs.is_empty()).then_some(glyphs)
        }
        Kind::Path | Kind::Line => None,
    }
}

/// The area the filter of an inner shadow works in: the box of the node and a little around it
/// (glyphs reach out of the box of their text).
pub fn inner_region(n: &Node) -> (f32, f32, f32, f32) {
    let (x0, y0, x1, y1) = n.bounds();
    let pad = 2.0 + if n.kind == Kind::Text { n.max_size() } else { 0.0 };
    (x0 - pad, y0 - pad, x1 + pad, y1 + pad)
}

/// The inner shadow of `n`, to be written right after its fill: the shadow colour wherever the
/// shape of the node, blurred and moved by the offset, does not cover the shape itself. `mode`:
/// see `Inner`.
/// Like a drop shadow, an inner shadow keeps its direction when the node is flipped (and turns
/// with it when it is rotated).
fn inner_fx(out: &mut String, n: &Node, fonts: &mut FontManager, ctx: &egui::Context, mode: Inner) {
    let Some(s) = n.inner() else { return };
    let Some(shape) = silhouette(n, fonts, ctx) else { return };
    let (flip_h, flip_v) = n.flips();
    let flipped = flip_h || flip_v;
    // The canvas turns and mirrors what it draws, this shadow with it: there the shadow is
    // made for the node as it is before that, with the offset mirrored beforehand.
    // In a file the filter works in the space of the document instead: the groups that turn
    // and flip the node are undone around it and applied to the shape inside it, and the
    // offset is turned by hand. (A renderer may not turn or mirror the offset of a filter that
    // is inside a turned or mirrored group; resvg does neither for half a turn or a flip.)
    let local = !matches!(mode, Inner::Flipped | Inner::Plain);
    let turned = !local && n.rotation != 0.0;
    let (dx, dy) = if local {
        (if flip_h { -s.x } else { s.x }, if flip_v { -s.y } else { s.y })
    } else {
        let (sin, cos) = n.rotation.to_radians().sin_cos();
        (s.x * cos - s.y * sin, s.x * sin + s.y * cos)
    };
    let (x0, y0, x1, y1) = if turned {
        let (a, pad) = (n.aabb(), 2.0 + if n.kind == Kind::Text { n.max_size() } else { 0.0 });
        (a.0 - pad, a.1 - pad, a.2 + pad, a.3 + pad)
    } else {
        inner_region(n)
    };
    let clipped = mode != Inner::Canvas;
    let (cx, cy) = n.center();
    if mode == Inner::Flipped && flipped {
        out.push_str("</g>");
    }
    if turned {
        let _ = write!(out, r#"<g transform="rotate({} {cx} {cy})">"#, -n.rotation);
    }
    let [r, g, b, a] = s.color;
    let _ = write!(
        out,
        r#"<filter id="is{id}" filterUnits="userSpaceOnUse" x="{x0}" y="{y0}" width="{}" height="{}" color-interpolation-filters="sRGB">"#,
        x1 - x0,
        y1 - y0,
        id = n.id
    );
    let source = if s.blur >= 0.01 {
        let _ = write!(out, r#"<feGaussianBlur in="SourceAlpha" stdDeviation="{}"/>"#, s.blur * 0.5);
        String::new()
    } else {
        r#" in="SourceAlpha""#.to_owned()
    };
    let _ = write!(
        out,
        r##"<feOffset{source} dx="{dx}" dy="{dy}" result="b"/><feFlood flood-color="#{r:02x}{g:02x}{b:02x}" flood-opacity="{:.3}"/><feComposite in2="b" operator="out"/>"##,
        a as f32 / 255.0
    );
    if clipped {
        out.push_str(r#"<feComposite in2="SourceAlpha" operator="in"/>"#);
    }
    let _ = write!(out, r#"</filter><g filter="url(#is{})">"#, n.id);
    if turned {
        let _ = write!(out, r#"<g transform="rotate({} {cx} {cy})">"#, n.rotation);
    }
    let mirrored = !local && flip_open(out, n);
    out.push_str(&shape);
    for _ in 0..1 + mirrored as usize + 2 * turned as usize {
        out.push_str("</g>");
    }
    if mode == Inner::Flipped && flipped {
        flip_open(out, n);
    }
}

/// Where an inner shadow is written (see `inner_fx`).
#[derive(Clone, Copy, PartialEq)]
enum Inner {
    /// In a file, inside the group that flips the node: that group is closed around the shadow.
    Flipped,
    /// In a file, outside of that group (the fill of a frame).
    Plain,
    /// For the canvas, the shadow colour also fills everything around the shape: the canvas
    /// draws the result with the mesh of the shape, so its edge is as sharp as that of the fill.
    Canvas,
    /// For the canvas, cut off at the shape (the glyphs of a text).
    CanvasCut,
}

/// SVG with nothing but the inner shadow of `n`, for its texture on the canvas: the node as it
/// is before it is turned and flipped (the canvas does both with its meshes). Result: the SVG,
/// the area it covers, and whether the shadow is cut off at the shape already (a text; for
/// other shapes see `inner_fx`).
pub fn inner_shadow_svg(n: &Node, fonts: &mut FontManager, ctx: &egui::Context) -> Option<(String, (f32, f32, f32, f32), bool)> {
    n.inner()?;
    let region = inner_region(n);
    let (w, h) = (region.2 - region.0, region.3 - region.1);
    let clipped = n.kind == Kind::Text;
    let mut out = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{} {} {w} {h}">"#, region.0, region.1);
    let start = out.len();
    inner_fx(&mut out, n, fonts, ctx, if clipped { Inner::CanvasCut } else { Inner::Canvas });
    if out.len() == start {
        return None;
    }
    out.push_str("</svg>");
    Some((out, region, clipped))
}

/// SVG untuk satu node (mis. frame terpilih) atau seluruh dokumen. Hasil: (svg, lebar, tinggi).
pub fn to_svg(doc: &Doc, only: Option<u64>, fonts: &mut FontManager, ctx: &egui::Context) -> Option<(String, f32, f32)> {
    to_svg_bleed(doc, only, 0.0, fonts, ctx)
}

/// A frame as it is exported with `bleed` around it: its own rectangle is larger by the bleed
/// on every side (its corners square, as the page is cut out of it), so its fill, gradient or
/// image is drawn for the larger area and what it holds is clipped there instead of at the
/// frame's edge. The layers inside keep their places.
fn bleed_frame(n: &Node, bleed: f32) -> Node {
    let mut f = n.clone();
    let grow = |at: &mut f32, size: &mut f32| {
        let sign = if *size < 0.0 { -1.0 } else { 1.0 };
        *at -= bleed * sign;
        *size += bleed * 2.0 * sign;
    };
    grow(&mut f.x, &mut f.w);
    grow(&mut f.y, &mut f.h);
    f.radius = 0.0;
    f
}

/// `to_svg` for print, with `bleed` document px around the exported area: the result is
/// larger by twice the bleed each way. A frame fills its bleed (see `bleed_frame`); layers
/// outside the frame are not shown. Around any other layer, or the whole document, the bleed
/// is only as full as the layers that reach into it.
pub fn to_svg_bleed(doc: &Doc, only: Option<u64>, bleed: f32, fonts: &mut FontManager, ctx: &egui::Context) -> Option<(String, f32, f32)> {
    let assets = &doc.assets;
    let bleed = if bleed.is_finite() { bleed.max(0.0) } else { 0.0 };
    // Texts on a path find their line here (see `text_svg`).
    fonts.paths = doc.guides();
    let target = only.and_then(|id| doc.find(id));
    let grown = target.filter(|n| bleed > 0.0 && n.kind == Kind::Frame).map(|n| bleed_frame(n, bleed));
    let (target, bleed) = match &grown {
        Some(frame) => (Some(frame), 0.0),
        None => (target, bleed),
    };
    let (x0, y0, x1, y1) = match target {
        Some(n) => {
            // Stroke garis/path melebar keluar dari kotak pembatas.
            // (A square end reaches out by half the width along the line as well.)
            let pad = if matches!(n.kind, Kind::Line | Kind::Path) && n.stroke.is_some() { n.stroke_w * if n.cap == 2 { 0.71 } else { 0.5 } } else { 0.0 };
            // Bayangan ikut masuk hasil export, kecuali untuk frame (ukuran halaman harus tetap).
            let pad = pad + n.shadow.filter(|s| s.color[3] > 0 && n.kind != Kind::Frame).map_or(0.0, |s| s.pad());
            // Begitu juga luberan layer blur.
            let pad = pad + if n.kind == Kind::Frame { 0.0 } else { n.blur_pad() };
            let b = n.aabb();
            (b.0 - pad, b.1 - pad, b.2 + pad, b.3 + pad)
        }
        None => doc.bounds()?,
    };
    let (x0, y0, x1, y1) = (x0 - bleed, y0 - bleed, x1 + bleed, y1 + bleed);
    let (w, h) = ((x1 - x0).max(1.0), (y1 - y0).max(1.0));
    let mut out = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{x0} {y0} {w} {h}">"#
    );
    let mut write = |out: &mut String, fonts: &mut FontManager| match target {
        Some(n) => run_svg(out, std::slice::from_ref(n), assets, fonts, ctx),
        None => list_svg(out, &doc.nodes, assets, fonts, ctx),
    };
    // Background blurs first (see `Stage`), bottom to top: one that lies over another shows
    // that one blurred as well.
    let mut glass = Vec::new();
    match target {
        Some(n) => glass_nodes(std::slice::from_ref(n), &mut glass),
        None => glass_nodes(&doc.nodes, &mut glass),
    }
    let _reset = StageReset;
    stage(|s| *s = Stage::default());
    for g in glass {
        if let Some(picture) = backdrop_image(g, (x0, y0, x0 + w, y0 + h), &mut write, fonts, ctx) {
            stage(|s| s.images.insert(g.id, picture));
        }
    }
    write(&mut out, fonts);
    out.push_str("</svg>");
    Some((out, w, h))
}

/// Leaves no pictures of one export behind for the next.
struct StageReset;

impl Drop for StageReset {
    fn drop(&mut self) {
        stage(|s| *s = Stage::default());
    }
}

/// The nodes that show a background blur, in the order they are painted. Not those inside a
/// layer blur (that is one blurred picture already) and not those that take part in a clipping
/// mask.
fn glass_nodes<'a, T: AsRef<Node>>(nodes: &'a [T], found: &mut Vec<&'a Node>) {
    let mut i = 0;
    while i < nodes.len() {
        let (j, k) = crate::doc::clip_run(nodes, i);
        if k == j {
            for n in nodes[i..j].iter().map(|n| n.as_ref()).filter(|n| n.visible && n.blur <= 0.0) {
                if n.backdrop() > 0.0 {
                    found.push(n);
                }
                glass_nodes(&n.children, found);
            }
        }
        i = k;
    }
}

/// Weights of a gaussian with standard deviation `sigma` (in pixels) out to three deviations
/// on each side; they add up to 1.
pub fn gaussian_kernel(sigma: f32) -> Vec<f32> {
    let radius = (sigma.max(0.0) * 3.0).ceil() as i32;
    if radius == 0 {
        return vec![1.0];
    }
    let weights: Vec<f32> = (-radius..=radius).map(|i| (-0.5 * (i as f32 / sigma).powi(2)).exp()).collect();
    let total: f32 = weights.iter().sum();
    weights.into_iter().map(|w| w / total).collect()
}

/// Blur RGBA pixels (premultiplied) in place. Past its edges the picture is taken to go on
/// with the pixels of the edge, so the blur does not fade out there.
pub fn gaussian_blur(data: &mut [u8], w: usize, h: usize, sigma: f32) {
    let kernel = gaussian_kernel(sigma);
    let radius = kernel.len() / 2;
    if radius == 0 || w == 0 || h == 0 || data.len() != w * h * 4 {
        return;
    }
    // Along x into `mid`, then along y back into `data`.
    let mut mid = vec![0.0_f32; data.len()];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0_f32; 4];
            for (i, k) in kernel.iter().enumerate() {
                let at = (y * w + (x + i).saturating_sub(radius).min(w - 1)) * 4;
                for c in 0..4 {
                    sum[c] += data[at + c] as f32 * k;
                }
            }
            mid[(y * w + x) * 4..][..4].copy_from_slice(&sum);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0_f32; 4];
            for (i, k) in kernel.iter().enumerate() {
                let at = ((y + i).saturating_sub(radius).min(h - 1) * w + x) * 4;
                for c in 0..4 {
                    sum[c] += mid[at + c] * k;
                }
            }
            for c in 0..4 {
                data[(y * w + x) * 4 + c] = sum[c].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// The blurred backdrop of `n` as SVG: a picture of what `write` paints before `n`, blurred,
/// clipped to the shape of `n`. To be written inside the group that turns the node, right under
/// its fill. `bounds`: the area being exported; nothing outside of it is seen, and the blur
/// treats its edge as the edge of the picture. `None`: there is nothing behind the node.
fn backdrop_image(
    n: &Node,
    bounds: (f32, f32, f32, f32),
    write: &mut dyn FnMut(&mut String, &mut FontManager),
    fonts: &mut FontManager,
    ctx: &egui::Context,
) -> Option<String> {
    use resvg::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
    let blur = n.backdrop();
    let shape = silhouette(n, fonts, ctx)?;
    let cut = |b: (f32, f32, f32, f32)| (b.0.max(bounds.0), b.1.max(bounds.1), b.2.min(bounds.2), b.3.min(bounds.3));
    let a = n.aabb();
    // Where the node shows, and how far around it the blur reaches (three deviations).
    let shown = cut((a.0 - 1.0, a.1 - 1.0, a.2 + 1.0, a.3 + 1.0));
    let pad = blur * 1.5;
    let (x0, y0, x1, y1) = cut((shown.0 - pad, shown.1 - pad, shown.2 + pad, shown.3 + pad));
    let (w, h) = (x1 - x0, y1 - y0);
    if !(shown.2 > shown.0 && shown.3 > shown.1) {
        return None;
    }
    // Blurred at a size where the deviation is 4 pixels (plenty to carry a gaussian, and quick
    // however large the radius), then enlarged smoothly to the size it is embedded at.
    let scale = (8.0 / blur).min(4.0).min(4096.0 / w.max(h));
    let (pw, ph) = ((w * scale).ceil().max(1.0) as u32, (h * scale).ceil().max(1.0) as u32);
    let mut svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="{x0} {y0} {w} {h}">"#);
    stage(|s| (s.stop, s.stopped, s.cull) = (Some(n.id), false, Some((x0, y0, x1, y1))));
    write(&mut svg, fonts);
    stage(|s| (s.stop, s.stopped, s.cull) = (None, false, None));
    svg.push_str("</svg>");
    let tree = resvg::usvg::Tree::from_str(&svg, &resvg::usvg::Options::default()).ok()?;
    let mut small = Pixmap::new(pw, ph)?;
    resvg::render(&tree, Transform::from_scale(scale, scale), &mut small.as_mut());
    if small.data().chunks_exact(4).all(|p| p[3] == 0) {
        return None;
    }
    gaussian_blur(small.data_mut(), pw as usize, ph as usize, blur * 0.5 * scale);
    let grow = (BACKDROP_SCALE / scale).min(4096.0 / pw.max(ph) as f32);
    let picture = if grow > 1.01 {
        let (bw, bh) = ((pw as f32 * grow).round().max(1.0) as u32, (ph as f32 * grow).round().max(1.0) as u32);
        let mut big = Pixmap::new(bw, bh)?;
        let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
        big.draw_pixmap(0, 0, small.as_ref(), &paint, Transform::from_scale(bw as f32 / pw as f32, bh as f32 / ph as f32), None);
        big
    } else {
        small
    };
    let png = picture.encode_png().ok()?;
    // (The picture covers whole pixels: a little more than the area that was asked for.)
    let (iw, ih) = (pw as f32 / scale, ph as f32 / scale);
    let back = if n.rotation != 0.0 {
        // The picture is in the document; the group it is written in turns with the node.
        let (cx, cy) = n.center();
        format!(r#" transform="rotate({} {cx} {cy})""#, -n.rotation)
    } else {
        String::new()
    };
    Some(format!(
        r##"<clipPath id="bc{id}">{shape}</clipPath><g clip-path="url(#bc{id})"><image x="{x0}" y="{y0}" width="{iw}" height="{ih}" preserveAspectRatio="none"{back} href="data:image/png;base64,{}"/></g>"##,
        b64::encode(&png),
        id = n.id
    ))
}

/// Render SVG ke PNG atau JPG pada skala tertentu (1x, 2x, 0.5x, …).
pub(crate) fn rasterize(svg: &str, w: f32, h: f32, scale: f32) -> Result<resvg::tiny_skia::Pixmap, String> {
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
/// Batch export: satu file per item `(nama, svg, w, h)` ke folder `dir`. `fmt`: 0 PNG, 1 JPG,
/// 2 SVG. Nama kembar diberi akhiran angka (tanpa membedakan huruf besar/kecil, seperti sistem
/// berkas Windows). Mengembalikan jumlah file yang berhasil dan error pertama, kalau ada.
pub fn save_batch(items: &[(String, String, f32, f32)], dir: &Path, fmt: usize, scale: f32) -> (usize, Option<String>) {
    let ext = ["png", "jpg", "svg"][fmt.min(2)];
    let suffix = if fmt < 2 && scale != 1.0 { format!("@{scale}x") } else { String::new() };
    let mut used = std::collections::HashSet::new();
    let (mut done, mut first_err) = (0, None);
    for (name, svg, w, h) in items {
        let safe: String = name.chars().map(|c| if c.is_alphanumeric() || " -_".contains(c) { c } else { '_' }).collect();
        let safe = if safe.trim().is_empty() { "untitled" } else { safe.trim() };
        let mut stem = safe.to_owned();
        let mut n = 2;
        while !used.insert(stem.to_lowercase()) {
            stem = format!("{safe}-{n}");
            n += 1;
        }
        let path = dir.join(format!("{stem}{suffix}.{ext}"));
        let result = match fmt {
            2 => std::fs::write(&path, svg).map_err(|e| e.to_string()),
            _ => save_raster(svg, *w, *h, scale, fmt == 1, &path),
        };
        match result {
            Ok(()) => done += 1,
            Err(e) => {
                first_err.get_or_insert(format!("{stem}: {e}"));
            }
        }
    }
    (done, first_err)
}

/// (See `print::pdf_bytes`; this is the export without any print settings.)
pub fn save_pdf(pages: &[(String, f32, f32)], path: &Path) -> Result<(), String> {
    crate::print::save_pdf(pages, &crate::print::Print::default(), path).map(|_| ())
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
    fn nested_frames_clip_their_content() {
        // White frame 1 (200×100) › blue frame 2 (at 50,20, 100×60) › red rect 3 (at 100,40,
        // 200×20) that sticks out of both.
        let mut doc = Doc::default();
        let mut add = |kind: Kind, parent: Option<u64>, (x, y, w, h): (f32, f32, f32, f32), fill: [u8; 4]| {
            let id = doc.alloc_id();
            let mut n = Node::new(id, kind, x, y);
            (n.w, n.h, n.fill) = (w, h, Some(fill));
            doc.insert(n, parent);
            id
        };
        add(Kind::Frame, None, (0.0, 0.0, 200.0, 100.0), [255, 255, 255, 255]);
        add(Kind::Frame, Some(1), (50.0, 20.0, 100.0, 60.0), [0, 0, 255, 255]);
        add(Kind::Rect, Some(2), (100.0, 40.0, 200.0, 20.0), [255, 0, 0, 255]);
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let rgb = |svg: &str, w: f32, h: f32, x: u32, y: u32| {
            let px = rasterize(svg, w, h, 1.0).unwrap().pixel(x, y).unwrap();
            (px.red(), px.green(), px.blue(), px.alpha())
        };
        let (svg, w, h) = to_svg(&doc, Some(1), &mut fonts, &ctx).unwrap();
        assert_eq!((w, h), (200.0, 100.0));
        assert_eq!(svg.matches("<clipPath").count(), 2);
        // Inside the nested frame: its fill, and the rect on top of it.
        assert_eq!(rgb(&svg, w, h, 60, 30), (0, 0, 255, 255));
        assert_eq!(rgb(&svg, w, h, 120, 50), (255, 0, 0, 255));
        // Past the nested frame's edge the rect is cut off: the outer frame shows.
        assert_eq!(rgb(&svg, w, h, 170, 50), (255, 255, 255, 255));
        // The nested frame is an export target of its own, at its own size.
        let (svg, w, h) = to_svg(&doc, Some(2), &mut fonts, &ctx).unwrap();
        assert_eq!((w, h), (100.0, 60.0));
        assert_eq!(rgb(&svg, w, h, 10, 10), (0, 0, 255, 255));
        assert_eq!(rgb(&svg, w, h, 90, 30), (255, 0, 0, 255));
        // The whole document: nothing of the rect outside the frames.
        let (svg, w, h) = to_svg(&doc, None, &mut fonts, &ctx).unwrap();
        assert_eq!((w, h), (200.0, 100.0));
        assert_eq!(rgb(&svg, w, h, 170, 50), (255, 255, 255, 255));
        // A hidden nested frame hides what is in it.
        doc.find_mut(2).unwrap().visible = false;
        let (svg, w, h) = to_svg(&doc, Some(1), &mut fonts, &ctx).unwrap();
        assert_eq!(rgb(&svg, w, h, 120, 50), (255, 255, 255, 255));
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
    fn flipped_images_and_text_export_mirrored() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        // An 80×40 image: the left half red, the right half blue, the top-right corner green.
        let mut pm = resvg::tiny_skia::Pixmap::new(80, 40).unwrap();
        for (i, px) in pm.pixels_mut().iter_mut().enumerate() {
            let (x, y) = (i % 80, i / 80);
            let c = if x >= 60 && y < 20 { (0, 255, 0) } else if x < 40 { (255, 0, 0) } else { (0, 0, 255) };
            *px = resvg::tiny_skia::PremultipliedColorU8::from_rgba(c.0, c.1, c.2, 255).unwrap();
        }
        let mut doc = Doc::default();
        let asset = doc.add_asset(pm.encode_png().unwrap());
        let mut img = Node::new(doc.alloc_id(), Kind::Image, 0.0, 0.0);
        (img.w, img.h, img.image) = (80.0, 40.0, Some(asset));
        let id = img.id;
        doc.insert(img, None);
        let rgb = |doc: &Doc, fonts: &mut FontManager, x: u32, y: u32| {
            let (svg, w, h) = to_svg(doc, None, fonts, &ctx).unwrap();
            let px = rasterize(&svg, w, h, 1.0).unwrap().pixel(x, y).unwrap();
            (px.red(), px.green(), px.blue())
        };
        const RED: (u8, u8, u8) = (255, 0, 0);
        const GREEN: (u8, u8, u8) = (0, 255, 0);
        const BLUE: (u8, u8, u8) = (0, 0, 255);
        assert_eq!((rgb(&doc, &mut fonts, 10, 30), rgb(&doc, &mut fonts, 50, 30), rgb(&doc, &mut fonts, 70, 10)), (RED, BLUE, GREEN));
        assert!(doc.flip(&[id], true));
        assert_eq!((rgb(&doc, &mut fonts, 10, 30), rgb(&doc, &mut fonts, 70, 30), rgb(&doc, &mut fonts, 10, 10)), (BLUE, RED, GREEN));
        assert!(doc.flip(&[id], false));
        assert_eq!((rgb(&doc, &mut fonts, 10, 10), rgb(&doc, &mut fonts, 10, 30), rgb(&doc, &mut fonts, 70, 10)), (BLUE, GREEN, RED));
        // Cropped to the right half of the picture, in a box of 40×40: the crop is mirrored in place.
        {
            let n = doc.find_mut(id).unwrap();
            (n.flip_v, n.w, n.crop) = (false, 40.0, Some([0.5, 0.0, 1.0, 1.0]));
        }
        assert_eq!((rgb(&doc, &mut fonts, 10, 10), rgb(&doc, &mut fonts, 30, 10), rgb(&doc, &mut fonts, 30, 30)), (GREEN, BLUE, BLUE));
        // A colour overlay and round corners go through the same mirror.
        {
            let n = doc.find_mut(id).unwrap();
            (n.fill, n.radius) = (Some([255, 255, 0, 255]), 6.0);
        }
        let (svg, ..) = to_svg(&doc, None, &mut fonts, &ctx).unwrap();
        assert!(svg.contains(r#"<g transform="translate(40 0) scale(-1 1)">"#), "{}", &svg[..svg.len().min(300)]);
        assert_eq!(rgb(&doc, &mut fonts, 20, 20), (255, 255, 0));

        // Text: an "L" has its foot on the right; mirrored, the foot is on the left.
        let mut doc = Doc::default();
        let mut bg = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        (bg.w, bg.h, bg.fill) = (100.0, 100.0, Some([255, 255, 255, 255]));
        doc.insert(bg, None);
        let mut t = Node::new(doc.alloc_id(), Kind::Text, 10.0, 10.0);
        (t.text, t.font_size, t.weight, t.fill, t.w, t.h) = ("L".into(), 80.0, 700, Some([0, 0, 0, 255]), 60.0, 80.0);
        let tid = t.id;
        doc.insert(t, None);
        // Dark pixels left and right of the middle of the text box, in its lower half.
        let ink = |doc: &Doc, fonts: &mut FontManager| {
            let (svg, w, h) = to_svg(doc, None, fonts, &ctx).unwrap();
            let pm = rasterize(&svg, w, h, 1.0).unwrap();
            let count = |xs: std::ops::Range<u32>| xs.flat_map(|x| (50..100u32).map(move |y| (x, y))).filter(|&(x, y)| pm.pixel(x, y).unwrap().red() < 80).count();
            (count(10..40), count(40..70))
        };
        let (left, right) = ink(&doc, &mut fonts);
        assert!(left > 0 && right > 0, "the letter should be there: {left} / {right}");
        assert!(doc.flip(&[tid], true));
        assert_eq!(ink(&doc, &mut fonts), (right, left), "was {left} / {right}");
        assert!(left != right, "an L is not symmetric");
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
    fn layer_blur_softens_edges_and_groups_blur_as_one() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut doc = Doc::default();
        let mut bg = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        (bg.w, bg.h, bg.fill) = (300.0, 100.0, Some([255, 255, 255, 255]));
        doc.insert(bg, None);
        // Dua kotak hitam yang saling menempel (x 50..100 dan 100..150), satu group, blur 20.
        for x in [50.0, 100.0] {
            let mut r = Node::new(doc.alloc_id(), Kind::Rect, x, 0.0);
            (r.w, r.h, r.fill, r.group, r.blur) = (50.0, 100.0, Some([0, 0, 0, 255]), 7, 20.0);
            doc.insert(r, None);
        }
        // Kotak hitam tajam sebagai pembanding.
        let mut sharp = Node::new(doc.alloc_id(), Kind::Rect, 220.0, 0.0);
        (sharp.w, sharp.h, sharp.fill) = (50.0, 100.0, Some([0, 0, 0, 255]));
        doc.insert(sharp, None);
        assert_eq!(crate::doc::blur_run(&doc.nodes, 1), 3);
        assert_eq!(crate::doc::blur_run(&doc.nodes, 3), 4);
        let (svg, w, h) = to_svg(&doc, None, &mut fonts, &ctx).unwrap();
        assert_eq!(svg.matches("<feGaussianBlur").count(), 1, "the group is blurred by one filter");
        let (png, ..) = render_png(&svg, w, h, 1.0).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        let gray = |x: u32| img.get_pixel(x, 50)[0];
        // Tepi luar group jadi gradasi: di tepinya setengah gelap, 10 px di luarnya masih kena.
        assert!((100..160).contains(&gray(50)), "group edge is half covered, got {}", gray(50));
        assert!((180..250).contains(&gray(40)), "blur spills outside, got {}", gray(40));
        // Sambungan di dalam group tetap pekat: kalau tiap anggota diburamkan sendiri-sendiri,
        // sambungan ini akan tembus pandang.
        assert!(gray(100) < 12, "seam inside the group stays solid, got {}", gray(100));
        // Objek tanpa blur tidak berubah.
        assert_eq!((gray(219), gray(221)), (255, 0));
        // Export satu node: kanvasnya dilebarkan supaya luberan blur tidak terpotong.
        let (_, w, _) = to_svg(&doc, Some(2), &mut fonts, &ctx).unwrap();
        assert_eq!(w, 50.0 + 2.0 * 31.0);
    }

    #[test]
    fn group_members_cast_one_shared_shadow() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut doc = Doc::default();
        let mut bg = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        (bg.w, bg.h, bg.fill) = (300.0, 200.0, Some([255, 255, 255, 255]));
        doc.insert(bg, None);
        // Kotak abu besar dan kotak hitam kecil di atasnya, satu group, bayangan tajam 20 px ke kanan.
        let shadow = Some(Shadow { x: 20.0, y: 0.0, blur: 0.0, color: [255, 0, 0, 255] });
        for (x, y, side, gray) in [(50.0, 50.0, 100.0, 200), (100.0, 80.0, 20.0, 0)] {
            let mut r = Node::new(doc.alloc_id(), Kind::Rect, x, y);
            (r.w, r.h, r.fill, r.group, r.shadow) = (side, side, Some([gray, gray, gray, 255]), 7, shadow);
            doc.insert(r, None);
        }
        assert_eq!(crate::doc::shadow_run(&doc.nodes, 0), 1);
        assert_eq!(crate::doc::shadow_run(&doc.nodes, 1), 3);
        let (svg, w, h) = to_svg(&doc, None, &mut fonts, &ctx).unwrap();
        assert_eq!(svg.matches("<feOffset").count(), 1, "the group casts one shadow");
        let (png, ..) = render_png(&svg, w, h, 1.0).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        let at = |x: u32, y: u32| img.get_pixel(x, y).0;
        // Bayangan siluet gabungan muncul di kanan kotak besar.
        assert_eq!(at(160, 100), [255, 0, 0, 255]);
        // Bayangan kotak kecil tidak jatuh di atas kotak besar (tepat di kanan kotak kecil).
        assert_eq!(at(130, 90), [200, 200, 200, 255]);
        assert_eq!(at(110, 90), [0, 0, 0, 255]);
        // Tekstur canvas: hanya bayangannya, mencakup seluruh group.
        let (only, region) = shadow_svg(&doc.nodes[1..3], &doc.assets, &mut fonts, &ctx).unwrap();
        assert!(region.0 < 50.0 && region.2 > 170.0);
        assert_eq!(only.matches("<rect").count(), 2);
        // Anggota yang bayangannya berbeda keluar dari bayangan bersama.
        doc.find_mut(3).unwrap().shadow = None;
        assert_eq!(crate::doc::shadow_run(&doc.nodes, 1), 2);
    }

    #[test]
    fn export_draws_the_caps_and_joins_of_the_node() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        // A line from (20, 20) to (80, 20), 10 wide, on a white frame of 100 × 40.
        let render = |cap: u8, fonts: &mut FontManager| {
            let mut doc = Doc::default();
            let mut frame = Node::new(doc.alloc_id(), Kind::Frame, 0.0, 0.0);
            (frame.w, frame.h, frame.fill) = (100.0, 40.0, Some([255, 255, 255, 255]));
            let mut line = Node::new(doc.alloc_id(), Kind::Line, 20.0, 20.0);
            (line.w, line.h, line.stroke, line.stroke_w, line.cap) = (60.0, 0.0, Some([0, 0, 255, 255]), 10.0, cap);
            frame.children.push(line);
            let id = frame.id;
            doc.insert(frame, None);
            let (svg, w, h) = to_svg(&doc, Some(id), fonts, &ctx).unwrap();
            let (png, _, _) = render_png(&svg, w, h, 1.0).unwrap();
            (svg, image::load_from_memory(&png).unwrap().to_rgb8())
        };
        let blue = |img: &image::RgbImage, x: u32, y: u32| img.get_pixel(x, y)[0] < 60;
        // Butt (what a document from before caps existed gets): nothing before the start.
        let (svg, img) = render(0, &mut fonts);
        assert!(!svg.contains("stroke-linecap") && !svg.contains("stroke-linejoin"), "{svg}");
        assert!(blue(&img, 21, 20) && !blue(&img, 18, 20));
        // Round: reaches 5 past the start on the axis, but not into the corner of that square.
        let (svg, img) = render(1, &mut fonts);
        assert!(svg.contains(r#"stroke-linecap="round""#));
        assert!(blue(&img, 16, 20) && !blue(&img, 15, 15) && !blue(&img, 13, 20));
        // Square: the corner too.
        let (svg, img) = render(2, &mut fonts);
        assert!(svg.contains(r#"stroke-linecap="square""#));
        assert!(blue(&img, 16, 20) && blue(&img, 15, 15) && blue(&img, 84, 24) && !blue(&img, 13, 20));

        // Joins of a path, and of a square whose stroke is outside (its corners stay square).
        let mut path = Node::new(1, Kind::Path, 0.0, 0.0);
        (path.w, path.h, path.stroke, path.stroke_w, path.join) = (10.0, 10.0, Some([0, 0, 0, 255]), 2.0, 2);
        let p = |x, y| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        path.points = vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0)];
        let mut out = String::new();
        node_body(&mut out, &path, &Default::default(), &mut fonts, &ctx);
        assert!(out.contains(r#"stroke-linejoin="bevel""#) && out.contains(r#"stroke-miterlimit="4""#) && !out.contains("linecap"), "{out}");
        let mut rect = Node::new(2, Kind::Rect, 0.0, 0.0);
        (rect.w, rect.h, rect.stroke, rect.stroke_w, rect.stroke_align) = (10.0, 10.0, Some([0, 0, 0, 255]), 4.0, Some(2));
        let mut out = String::new();
        node_body(&mut out, &rect, &Default::default(), &mut fonts, &ctx);
        assert!(out.contains(r#"x="-2" y="-2" width="14" height="14" rx="0""#), "{out}");
        // Dashes around rounded corners are written as a path that starts where the canvas does.
        (rect.radius, rect.dash, rect.stroke_align) = (3.0, 2.0, Some(1));
        let mut out = String::new();
        node_body(&mut out, &rect, &Default::default(), &mut fonts, &ctx);
        assert!(out.contains(r#"<path d="M7.000 0.000C"#) && out.contains(r#"stroke-dasharray="2 2""#), "{out}");
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
        let p = |x, y| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
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
    /// A white frame of 100 x 100 with a gray square (20..80) in it, set up by `edit`, exported.
    fn square_export(edit: &dyn Fn(&mut Node)) -> (String, image::RgbaImage) {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut doc = Doc::default();
        let mut frame = Node::new(doc.alloc_id(), Kind::Frame, 0.0, 0.0);
        (frame.w, frame.h, frame.fill) = (100.0, 100.0, Some([255, 255, 255, 255]));
        let mut r = Node::new(doc.alloc_id(), Kind::Rect, 20.0, 20.0);
        (r.w, r.h, r.fill) = (60.0, 60.0, Some([200, 200, 200, 255]));
        r.inner_shadow = Some(Shadow { x: 0.0, y: 10.0, blur: 0.0, color: [255, 0, 0, 255] });
        edit(&mut r);
        frame.children.push(r);
        let id = frame.id;
        doc.insert(frame, None);
        let (svg, w, h) = to_svg(&doc, Some(id), &mut fonts, &ctx).unwrap();
        let (png, ..) = render_png(&svg, w, h, 1.0).unwrap();
        (svg, image::load_from_memory(&png).unwrap().to_rgba8())
    }

    #[test]
    fn inner_shadow_falls_inside_the_shape() {
        const RED: [u8; 4] = [255, 0, 0, 255];
        const GRAY: [u8; 4] = [200, 200, 200, 255];
        const WHITE: [u8; 4] = [255, 255, 255, 255];
        // A sharp shadow moved down by 10: a band of 10 inside the top edge, nothing outside.
        let (svg, img) = square_export(&|_| {});
        let at = |img: &image::RgbaImage, x: u32, y: u32| img.get_pixel(x, y).0;
        assert_eq!(svg.matches(r#"<filter id="is"#).count(), 1);
        assert_eq!([at(&img, 50, 25), at(&img, 50, 35), at(&img, 50, 75), at(&img, 50, 15), at(&img, 50, 85)], [RED, GRAY, GRAY, WHITE, WHITE]);
        assert_eq!([at(&img, 22, 50), at(&img, 78, 50)], [GRAY, GRAY]);
        // Flipped, it keeps its direction; turned upside down, it turns with the shape.
        let (_, img) = square_export(&|r| r.flip_v = true);
        assert_eq!([at(&img, 50, 25), at(&img, 50, 75)], [RED, GRAY]);
        let (_, img) = square_export(&|r| r.rotation = 180.0);
        assert_eq!([at(&img, 50, 25), at(&img, 50, 75)], [GRAY, RED]);
        // Blurred: strongest at the edge, gone further in. With the drop shadow beside it.
        let (svg, img) = square_export(&|r| {
            r.inner_shadow = Some(Shadow { x: 0.0, y: 0.0, blur: 12.0, color: [255, 0, 0, 255] });
            r.shadow = Some(Shadow { x: 0.0, y: 0.0, blur: 8.0, color: [0, 0, 255, 255] });
        });
        let (edge, inside, middle, outside) = (at(&img, 50, 21), at(&img, 50, 27), at(&img, 50, 50), at(&img, 50, 17));
        assert!(edge[1] < 150 && edge[0] > 215, "red near the edge, got {edge:?}");
        assert!(inside[1] > edge[1] + 20 && inside[1] < 200, "fading inwards, got {inside:?} after {edge:?}");
        assert_eq!(middle, GRAY);
        assert!(outside[2] == 255 && outside[0] < 235, "the drop shadow outside is blue, got {outside:?}");
        assert!(svg.contains(r#"<filter id="is"#) && svg.contains(r#"<filter id="sh"#));
        // Under the stroke; a transparent shadow, a line and an open path have none.
        let (_, img) = square_export(&|r| (r.stroke, r.stroke_w) = (Some([0, 0, 0, 255]), 4.0));
        assert_eq!([at(&img, 50, 22), at(&img, 50, 27)], [[0, 0, 0, 255], RED]);
        let (svg, _) = square_export(&|r| r.inner_shadow = Some(Shadow { x: 0.0, y: 4.0, blur: 4.0, color: [0, 0, 0, 0] }));
        assert!(!svg.contains("<filter"));
        let (svg, _) = square_export(&|r| (r.kind, r.stroke, r.stroke_w) = (Kind::Line, Some([0, 0, 0, 255]), 2.0));
        assert!(!svg.contains("<filter"));

        // A ring (a path with a hole): the shadow follows both edges; fill, shadow and stroke
        // are three elements in that order.
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        let mut ring = Node::new(7, Kind::Path, 0.0, 0.0);
        let p = |x, y| PathPoint { x, y, hin: [0.0; 2], hout: [0.0; 2], r: None };
        (ring.w, ring.h, ring.closed, ring.fill, ring.stroke, ring.stroke_w) = (100.0, 100.0, true, Some([200, 200, 200, 255]), Some([0, 0, 0, 255]), 1.0);
        ring.points = vec![p(0.0, 0.0), p(1.0, 0.0), p(1.0, 1.0), p(0.0, 1.0)];
        ring.holes = vec![vec![p(0.4, 0.4), p(0.6, 0.4), p(0.6, 0.6), p(0.4, 0.6)]];
        ring.inner_shadow = Some(Shadow { x: 0.0, y: 10.0, blur: 0.0, color: [255, 0, 0, 255] });
        let mut out = String::new();
        node_body(&mut out, &ring, &Default::default(), &mut fonts, &ctx);
        let (fill_at, shadow_at, stroke_at) = (out.find("fill=\"#c8c8c8\"").unwrap(), out.find("<filter").unwrap(), out.find("stroke-width").unwrap());
        assert!(fill_at < shadow_at && shadow_at < stroke_at, "{out}");
        let svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">{out}</svg>"#);
        let (png, ..) = render_png(&svg, 100.0, 100.0, 1.0).unwrap();
        let img = image::load_from_memory(&png).unwrap().to_rgba8();
        // Below the top edge and below the hole: shadow. In the hole: nothing at all.
        assert_eq!([at(&img, 50, 5), at(&img, 50, 65), at(&img, 50, 30), at(&img, 50, 50)], [RED, RED, GRAY, [0, 0, 0, 0]]);

        // The texture of the canvas: the shadow alone, for shapes not cut off at the shape
        // (the mesh of the shape does that), for text cut off at the glyphs.
        let (only, region, clipped) = inner_shadow_svg(&ring, &mut fonts, &ctx).unwrap();
        assert!(!clipped && region == (-2.0, -2.0, 102.0, 102.0) && !only.contains("#c8c8c8") && !only.contains(r#"operator="in""#), "{only}");
        let mut text = Node::new(8, Kind::Text, 0.0, 0.0);
        (text.text, text.fill, text.inner_shadow) = ("Kanvas".into(), Some([0, 0, 0, 255]), ring.inner_shadow);
        let (only, _, clipped) = inner_shadow_svg(&text, &mut fonts, &ctx).unwrap();
        assert!(clipped && only.contains(r#"operator="in""#));
        text.inner_shadow = None;
        assert!(inner_shadow_svg(&text, &mut fonts, &ctx).is_none());
    }

    #[test]
    fn background_blur_shows_what_is_behind_blurred_in_the_shape() {
        let ctx = context();
        let mut fonts = FontManager::new(&ctx);
        // A white frame of 200 x 100 whose left half is black; over the middle a glass panel
        // without a fill (50..150 x 20..80), background blur 20.
        let mut doc = Doc::default();
        let mut frame = Node::new(doc.alloc_id(), Kind::Frame, 0.0, 0.0);
        (frame.w, frame.h, frame.fill) = (200.0, 100.0, Some([255, 255, 255, 255]));
        let mut black = Node::new(doc.alloc_id(), Kind::Rect, 0.0, 0.0);
        (black.w, black.h, black.fill) = (100.0, 100.0, Some([0, 0, 0, 255]));
        let mut glass = Node::new(doc.alloc_id(), Kind::Rect, 50.0, 20.0);
        (glass.w, glass.h, glass.bg_blur) = (100.0, 60.0, 20.0);
        let (frame_id, glass_id) = (frame.id, glass.id);
        frame.children.extend([black, glass]);
        doc.insert(frame, None);
        let render = |doc: &Doc, only: u64, fonts: &mut FontManager| {
            let (svg, w, h) = to_svg(doc, Some(only), fonts, &ctx).unwrap();
            let (png, ..) = render_png(&svg, w, h, 1.0).unwrap();
            (svg, image::load_from_memory(&png).unwrap().to_rgba8())
        };
        let (svg, img) = render(&doc, frame_id, &mut fonts);
        assert_eq!(svg.matches("<image").count(), 1);
        let gray = |img: &image::RgbaImage, x: u32, y: u32| img.get_pixel(x, y)[0];
        // Inside the panel the edge between black and white is a soft ramp...
        assert!((100..156).contains(&gray(&img, 100, 50)), "half way at the edge, got {}", gray(&img, 100, 50));
        assert!((15..70).contains(&gray(&img, 90, 50)) && (185..240).contains(&gray(&img, 110, 50)), "a ramp, got {} and {}", gray(&img, 90, 50), gray(&img, 110, 50));
        assert!(gray(&img, 55, 50) < 3 && gray(&img, 145, 50) > 252, "far from the edge it is as before");
        // ...and outside of it nothing changed.
        assert_eq!([gray(&img, 98, 10), gray(&img, 102, 10), gray(&img, 98, 90), gray(&img, 102, 90)], [0, 255, 0, 255]);
        assert!(img.pixels().all(|p| p[3] == 255));
        // The node's own fill lies over the blur; fully opaque, it hides it.
        doc.find_mut(glass_id).unwrap().fill = Some([255, 0, 0, 128]);
        let (_, img) = render(&doc, frame_id, &mut fonts);
        let (l, r) = (img.get_pixel(55, 50).0, img.get_pixel(145, 50).0);
        assert!((126..130).contains(&l[0]) && l[1] < 3 && (126..130).contains(&r[1]) && r[0] == 255, "red glass over black and white, got {l:?} {r:?}");
        // At the edge of what is exported the blur does not fade out: the panel moved to the
        // left edge shows plain black there.
        doc.find_mut(glass_id).unwrap().fill = None;
        doc.find_mut(glass_id).unwrap().x = 0.0;
        let (_, img) = render(&doc, frame_id, &mut fonts);
        assert_eq!(img.get_pixel(0, 50).0, [0, 0, 0, 255]);
        // A second panel on top sees the first one's blur; turned, its picture stays in place.
        let mut second = doc.find(glass_id).unwrap().clone();
        (second.id, second.x, second.rotation) = (doc.alloc_id(), 60.0, 90.0);
        doc.insert(second, Some(frame_id));
        let (svg, img) = render(&doc, frame_id, &mut fonts);
        assert_eq!(svg.matches("<image").count(), 2);
        // (The first panel, which ends at that edge, had made its dark side lighter there.)
        assert!((135..200).contains(&gray(&img, 100, 50)), "blurred twice at the edge, got {}", gray(&img, 100, 50));
        // Turned, the second panel stands upright (80..140 x 0..100): the ramp reaches the top
        // of the frame, still running from left to right, and ends at the panel's sides.
        let (l, r) = (gray(&img, 90, 3), gray(&img, 110, 3));
        assert!(l > 10 && l + 60 < r && r < 245, "a ramp from left to right, got {l} and {r}");
        assert_eq!([gray(&img, 70, 3), gray(&img, 150, 3)], [0, 255]);
        // Alone, with nothing behind it, a panel has nothing to blur. Neither has one that is
        // layer-blurred itself, nor a text.
        let (svg, _) = render(&doc, glass_id, &mut fonts);
        assert!(!svg.contains("<image"));
        let mut n = doc.find(glass_id).unwrap().clone();
        assert_eq!(n.backdrop(), 20.0);
        n.blur = 4.0;
        assert_eq!(n.backdrop(), 0.0);
        (n.blur, n.kind) = (0.0, Kind::Text);
        assert_eq!(n.backdrop(), 0.0);
        // Nothing of one export is left behind for the next.
        assert!(stage(|s| s.images.is_empty() && s.stop.is_none() && s.cull.is_none()));
    }

    #[test]
    fn gaussian_blur_keeps_what_is_even_and_spreads_an_edge() {
        let k = gaussian_kernel(2.0);
        assert_eq!(k.len(), 13);
        assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(k.iter().zip(k.iter().rev()).all(|(a, b)| a == b) && k[6] > k[5] && k[5] > k[0]);
        // The middle weight of a gaussian: 1 / (sigma * sqrt(2 pi)), a little more for the cut-off tails.
        assert!((k[6] - 0.1995).abs() < 0.002, "{}", k[6]);
        assert_eq!(gaussian_kernel(0.0), vec![1.0]);
        // One colour everywhere stays that colour, up to the edges.
        let (w, h) = (16, 9);
        let mut even = [40u8, 80, 120, 200].repeat(w * h);
        gaussian_blur(&mut even, w, h, 3.0);
        assert!(even.chunks_exact(4).all(|p| p == [40, 80, 120, 200]));
        // A step from 0 to 255 at x = 8: half way at the step, the ends untouched, the same on every row.
        let mut step: Vec<u8> = (0..w * h).flat_map(|i| [if i % w < 8 { 0 } else { 255 }; 4]).collect();
        gaussian_blur(&mut step, w, h, 1.5);
        let at = |x: usize, y: usize| step[(y * w + x) * 4];
        assert!((at(7, 4) as i32 + at(8, 4) as i32 - 255).abs() <= 1 && at(7, 4) > 60 && at(7, 4) < 127, "{} {}", at(7, 4), at(8, 4));
        assert_eq!((at(0, 4), at(15, 4)), (0, 255));
        assert!((0..h).all(|y| at(7, y) == at(7, 0)));
        // Sizes that do not fit are left alone.
        let mut odd = vec![1u8; 10];
        gaussian_blur(&mut odd, 2, 2, 3.0);
        assert_eq!(odd, vec![1u8; 10]);
    }
}
