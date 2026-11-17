//! Teks vector: outline glyph dari file font di-tessellate jadi mesh segitiga, sehingga teks
//! besar tetap tajam di zoom berapa pun (atlas glyph egui berupa bitmap dan dibatasi ukurannya).

use crate::fonts::FontManager;
use eframe::egui::{self, vec2, Color32, FontFamily, Mesh, Pos2, Rect, Vec2};
use lyon_tessellation::math::point;
use lyon_tessellation::path::iterator::PathIterator;
use lyon_tessellation::path::{Path, PathEvent};
pub use lyon_tessellation::FillRule;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use std::collections::HashMap;
use std::sync::Arc;

/// Satu glyph dalam satuan em (1.0 = ukuran font), y ke bawah, titik asal di baseline.
pub struct GlyphMesh {
    verts: Vec<[f32; 2]>,
    idx: Vec<u32>,
    /// Kontur: (titik, arah keluar di titik itu) untuk tepi anti-alias.
    contours: Vec<Vec<([f32; 2], [f32; 2])>>,
    /// (min x, min y, max x, max y)
    bounds: [f32; 4],
}

/// Kunci: (font, huruf, tingkat detail).
#[derive(Default)]
pub struct Cache {
    map: HashMap<(FontFamily, char, u8), Option<Arc<GlyphMesh>>>,
}

/// Tingkat detail kurva untuk glyph setinggi `px` piksel: (tingkat, ukuran terbesar tingkat itu).
fn level(px: f32) -> (u8, f32) {
    match px {
        p if p <= 512.0 => (0, 512.0),
        p if p <= 2048.0 => (1, 2048.0),
        p if p <= 8192.0 => (2, 8192.0),
        _ => (3, 32768.0),
    }
}

/// The same for text on a path, which is drawn from outlines at every size: small glyphs get
/// by with far fewer points.
fn level_fine(px: f32) -> (u8, f32) {
    match px {
        p if p <= 20.0 => (8, 20.0),
        p if p <= 64.0 => (9, 64.0),
        p if p <= 192.0 => (10, 192.0),
        p => level(p),
    }
}

struct Outline {
    b: lyon_tessellation::path::path::Builder,
    open: bool,
}

impl ttf_parser::OutlineBuilder for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        if self.open {
            self.b.end(true);
        }
        self.b.begin(point(x, -y));
        self.open = true;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        if self.open {
            self.b.line_to(point(x, -y));
        }
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        if self.open {
            self.b.quadratic_bezier_to(point(x1, -y1), point(x, -y));
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        if self.open {
            self.b.cubic_bezier_to(point(x1, -y1), point(x2, -y2), point(x, -y));
        }
    }
    fn close(&mut self) {
        if self.open {
            self.b.end(true);
            self.open = false;
        }
    }
}

fn build(data: &[u8], chr: char, max_px: f32) -> Option<GlyphMesh> {
    let face = ttf_parser::Face::parse(data, 0).ok()?;
    let gid = face.glyph_index(chr)?;
    let upm = face.units_per_em() as f32;
    let mut outline = Outline { b: Path::builder(), open: false };
    face.outline_glyph(gid, &mut outline)?;
    if outline.open {
        outline.b.end(true);
    }
    let path = outline.b.build();
    // Simpangan kurva paling besar ±0.2 piksel pada ukuran terbesar tingkat ini.
    let tol = (0.2 / max_px * upm).max(1e-3);
    let s = 1.0 / upm;

    let mut buf: VertexBuffers<[f32; 2], u32> = VertexBuffers::new();
    let options = FillOptions::tolerance(tol).with_fill_rule(FillRule::NonZero);
    FillTessellator::new()
        .tessellate_path(
            &path,
            &options,
            &mut BuffersBuilder::new(&mut buf, |v: FillVertex| {
                let p = v.position();
                [p.x * s, p.y * s]
            }),
        )
        .ok()?;
    if buf.indices.is_empty() || buf.vertices.iter().any(|v| !v[0].is_finite() || !v[1].is_finite()) {
        return None;
    }

    let mut raw: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    for ev in path.iter().flattened(tol) {
        match ev {
            PathEvent::Begin { at } => cur = vec![[at.x * s, at.y * s]],
            PathEvent::Line { to, .. } => {
                let p = [to.x * s, to.y * s];
                if cur.last() != Some(&p) {
                    cur.push(p);
                }
            }
            PathEvent::End { .. } => {
                if cur.len() > 1 && cur.first() == cur.last() {
                    cur.pop();
                }
                if cur.len() >= 3 {
                    raw.push(std::mem::take(&mut cur));
                }
            }
            _ => {}
        }
    }
    // Arah putar kontur luar menentukan sisi mana yang "luar" (TrueType dan CFF berlawanan);
    // lubang huruf berputar sebaliknya sehingga rumus yang sama tetap menunjuk menjauhi tinta.
    let area: f32 = raw
        .iter()
        .map(|c| (0..c.len()).map(|i| c[i][0] * c[(i + 1) % c.len()][1] - c[(i + 1) % c.len()][0] * c[i][1]).sum::<f32>())
        .sum();
    let side = if area >= 0.0 { 1.0 } else { -1.0 };
    let normal = |a: [f32; 2], b: [f32; 2]| vec2(b[1] - a[1], a[0] - b[0]).normalized() * side;
    let contours = raw
        .iter()
        .map(|c| {
            let n = c.len();
            (0..n)
                .map(|i| {
                    let (n1, n2) = (normal(c[(i + n - 1) % n], c[i]), normal(c[i], c[(i + 1) % n]));
                    let m = n1 + n2;
                    // Miter: panjang 1/cos(setengah sudut), dibatasi supaya sudut lancip tidak menjulur.
                    let len2 = m.length_sq();
                    let m = if len2 < 0.16 { m.normalized() * 2.5 } else { m * (2.0 / len2) };
                    let m = if m.is_finite() { m } else { n2 };
                    (c[i], [m.x, m.y])
                })
                .collect()
        })
        .collect();

    let mut bounds = [f32::INFINITY, f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY];
    for v in &buf.vertices {
        bounds = [bounds[0].min(v[0]), bounds[1].min(v[1]), bounds[2].max(v[0]), bounds[3].max(v[1])];
    }
    Some(GlyphMesh { verts: buf.vertices, idx: buf.indices, contours, bounds })
}

/// Triangulasi poligon (boleh cekung atau berpotongan sendiri, aturan non-zero seperti SVG).
/// Hasil: (titik, indeks segitiga); titik bisa bertambah di perpotongan.
pub fn fill_polygon(pts: &[Pos2]) -> Option<(Vec<Pos2>, Vec<u32>)> {
    fill_contours(&[pts], FillRule::NonZero)
}

/// Triangulasi beberapa kontur tertutup sekaligus (even-odd: kontur di dalam jadi lubang).
pub fn fill_contours<C: AsRef<[Pos2]>>(contours: &[C], rule: FillRule) -> Option<(Vec<Pos2>, Vec<u32>)> {
    let mut b = Path::builder();
    let mut any = false;
    for c in contours {
        let pts = c.as_ref();
        if pts.len() < 3 {
            continue;
        }
        if pts.iter().any(|p| !p.x.is_finite() || !p.y.is_finite()) {
            return None;
        }
        b.begin(point(pts[0].x, pts[0].y));
        for p in &pts[1..] {
            b.line_to(point(p.x, p.y));
        }
        b.end(true);
        any = true;
    }
    if !any {
        return None;
    }
    let mut buf: VertexBuffers<Pos2, u32> = VertexBuffers::new();
    let options = FillOptions::tolerance(0.1).with_fill_rule(rule);
    FillTessellator::new()
        .tessellate_path(
            &b.build(),
            &options,
            &mut BuffersBuilder::new(&mut buf, |v: FillVertex| {
                let p = v.position();
                egui::pos2(p.x, p.y)
            }),
        )
        .ok()?;
    (!buf.indices.is_empty() && buf.vertices.iter().all(|p| p.x.is_finite() && p.y.is_finite())).then_some((buf.vertices, buf.indices))
}

impl Cache {
    fn glyph(&mut self, fonts: &FontManager, family: &FontFamily, chr: char, px: f32) -> Option<Arc<GlyphMesh>> {
        self.glyph_at(fonts, family, chr, level(px))
    }

    fn glyph_at(&mut self, fonts: &FontManager, family: &FontFamily, chr: char, (lv, max_px): (u8, f32)) -> Option<Arc<GlyphMesh>> {
        if self.map.len() > 6000 {
            self.map.clear();
        }
        self.map
            .entry((family.clone(), chr, lv))
            .or_insert_with(|| {
                // Glyph tidak ada di font pilihan (mis. font belum terunduh): pakai font bawaan.
                let from = |f: &FontFamily| fonts.face_data(f).and_then(|data| build(data, chr, max_px));
                from(family).or_else(|| from(&FontFamily::Proportional)).map(Arc::new)
            })
            .clone()
    }
}

/// Mesh vector untuk seluruh galley. `origin` = posisi layar titik (0, 0) galley, `k` = skala
/// dari satuan layout ke layar. `clip`: glyph di luar kotak ini dilewati.
/// `None` kalau ada glyph yang outline-nya tidak tersedia (pemanggil memakai jalur bitmap).
pub fn mesh(
    cache: &mut Cache,
    fonts: &FontManager,
    galley: &egui::Galley,
    origin: Pos2,
    k: f32,
    clip: Option<Rect>,
    pixels_per_point: f32,
) -> Option<Mesh> {
    let _span = super::perf::span(4);
    let mut mesh = Mesh::default();
    let feather = 1.0 / pixels_per_point.max(0.1);
    for row in &galley.rows {
        for g in &row.glyphs {
            if g.uv_rect.is_nothing() {
                continue; // spasi, ganti baris
            }
            let format = &galley.job.sections.get(g.section_index as usize)?.format;
            let size = format.font_id.size * k;
            let at = origin + g.pos.to_vec2() * k;
            let gm = cache.glyph(fonts, &format.font_id.family, g.chr, size)?;
            let b = gm.bounds;
            let ink = Rect::from_min_max(at + vec2(b[0], b[1]) * size, at + vec2(b[2], b[3]) * size);
            if clip.is_some_and(|c| !c.intersects(ink.expand(2.0))) {
                continue;
            }
            let color = format.color;
            let vertex = |pos: Pos2, color: Color32| egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color };
            let base = mesh.vertices.len() as u32;
            mesh.vertices.extend(gm.verts.iter().map(|v| vertex(at + vec2(v[0], v[1]) * size, color)));
            mesh.indices.extend(gm.idx.iter().map(|i| base + i));
            // Tepi anti-alias: pita selebar satu piksel fisik dari warna penuh ke transparan.
            for c in &gm.contours {
                let n = c.len() as u32;
                let base = mesh.vertices.len() as u32;
                for (p, m) in c {
                    let q = at + vec2(p[0], p[1]) * size;
                    mesh.vertices.push(vertex(q, color));
                    mesh.vertices.push(vertex(q + Vec2::from(*m) * feather, Color32::TRANSPARENT));
                }
                for i in 0..n {
                    let (a, b) = (base + 2 * i, base + 2 * ((i + 1) % n));
                    mesh.indices.extend([a, a + 1, b, b, a + 1, b + 1]);
                }
            }
        }
    }
    Some(mesh)
}

/// Adds one glyph to `mesh`, turned: its origin (the left end of its baseline) at `at`, the
/// baseline running along the unit vector `tan`, `size` pixels high. For text on a path.
/// Result false: the glyph has no outline (a space, or no font has it).
#[allow(clippy::too_many_arguments)]
pub fn turned(cache: &mut Cache, fonts: &FontManager, mesh: &mut Mesh, family: &FontFamily, chr: char, size: f32, at: Pos2, tan: Vec2, color: Color32, feather: f32) -> bool {
    let Some(gm) = cache.glyph_at(fonts, family, chr, level_fine(size)) else { return false };
    // Glyph space has y down: its y axis is the baseline's normal, a quarter turn on from `tan`.
    let nrm = vec2(-tan.y, tan.x);
    let to = |v: [f32; 2]| at + (tan * v[0] + nrm * v[1]) * size;
    let turn = |v: [f32; 2]| tan * v[0] + nrm * v[1];
    let vertex = |pos: Pos2, color: Color32| egui::epaint::Vertex { pos, uv: egui::epaint::WHITE_UV, color };
    let base = mesh.vertices.len() as u32;
    mesh.vertices.extend(gm.verts.iter().map(|v| vertex(to(*v), color)));
    mesh.indices.extend(gm.idx.iter().map(|i| base + i));
    // The soft edge is centred on the outline, so small text is not made bolder by it. Its
    // inner half lies on the glyph, which hides it as long as the colour is opaque.
    let inner = if color.a() == 255 { 0.5 } else { 0.0 };
    for c in &gm.contours {
        let n = c.len() as u32;
        let base = mesh.vertices.len() as u32;
        for (p, m) in c {
            let (q, m) = (to(*p), turn(*m) * feather);
            mesh.vertices.push(vertex(q - m * inner, color));
            mesh.vertices.push(vertex(q + m * (1.0 - inner), Color32::TRANSPARENT));
        }
        for i in 0..n {
            let (a, b) = (base + 2 * i, base + 2 * ((i + 1) % n));
            mesh.indices.extend([a, a + 1, b, b, a + 1, b + 1]);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glyph_outline_becomes_a_mesh() {
        let defs = egui::FontDefinitions::default();
        let name = &defs.families[&FontFamily::Proportional][0];
        let data = &defs.font_data[name].font;
        // "o" punya lubang: dua kontur, dan luas mesh harus lebih kecil dari kotak pembatasnya.
        let g = build(data, 'o', 512.0).expect("glyph mesh");
        assert_eq!(g.contours.len(), 2);
        assert_eq!(g.idx.len() % 3, 0);
        let area: f32 = g
            .idx
            .chunks(3)
            .map(|t| {
                let (a, b, c) = (g.verts[t[0] as usize], g.verts[t[1] as usize], g.verts[t[2] as usize]);
                ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() * 0.5
            })
            .sum();
        let bbox = (g.bounds[2] - g.bounds[0]) * (g.bounds[3] - g.bounds[1]);
        assert!(area > bbox * 0.2 && area < bbox * 0.75, "area {area} bbox {bbox}");
        // Arah keluar kontur terluar harus menjauhi pusat glyph.
        let (cx, cy) = ((g.bounds[0] + g.bounds[2]) * 0.5, (g.bounds[1] + g.bounds[3]) * 0.5);
        let outer = g.contours.iter().max_by(|a, b| a.len().cmp(&b.len())).unwrap();
        let away = outer.iter().filter(|(p, m)| (p[0] - cx) * m[0] + (p[1] - cy) * m[1] > 0.0).count();
        assert!(away * 10 >= outer.len() * 9, "{away} of {} normals point outward", outer.len());
        // Tingkat detail lebih tinggi = kurva lebih halus.
        assert!(build(data, 'o', 8192.0).unwrap().verts.len() > g.verts.len());
        assert!(build(data, ' ', 512.0).is_none());
    }
}
