//! Print-ready export: bleed, crop marks, the page boxes of a PDF, and CMYK output.
//!
//! What is decided here:
//! - The document has no physical size of its own: `Print::ppi` says how many document px make
//!   an inch on paper. 72 (the default) makes 1 px one PDF point, which is what the Paper
//!   presets are drawn in (A4 = 595 × 842); a design made at 300 dpi pixel sizes (A4 = 2480 ×
//!   3508) is exported with 300.
//! - Bleed extends the exported area of a frame on all four sides (see `export::to_svg_bleed`):
//!   the frame's own fill is drawn again for the larger rectangle, and what the frame holds is
//!   clipped to that larger rectangle instead of to the frame. Other top-level layers are not
//!   shown.
//! - Crop marks sit in a margin around the bleed: 0.25 pt lines at the four corners, starting
//!   as far from the trim edge as the bleed is wide (3 mm at least) and 5 mm long.
//! - CMYK: `svg2pdf` writes RGB; the finished file is rewritten by `pdf::to_cmyk`. Without an
//!   ICC profile the numbers come from the plain formula (`naive`); with a CMYK profile they
//!   come from that profile (relative colorimetric), and the profile is embedded as the output
//!   intent. Either way neutral colours of vector content (r = g = b: black and grey text,
//!   lines, fills, gradient stops) are black ink only.

pub mod pdf;

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// PDF points per millimetre.
pub const MM: f32 = 72.0 / 25.4;
/// Resolution of a page that is flattened to one image.
pub const FLATTEN_DPI: f32 = 300.0;
/// Width of a crop mark, in points.
pub const MARK_WIDTH: f32 = 0.25;

/// The print settings of an export. They describe the job the document is made for, so they
/// are saved with it (`Doc::print`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Print {
    /// Bleed on every side, in millimetres.
    pub bleed_mm: f32,
    /// Crop marks at the four corners, in a margin around the bleed.
    pub marks: bool,
    /// PDF: everything in CMYK instead of RGB.
    pub cmyk: bool,
    /// PDF: every page as one image of `FLATTEN_DPI` instead of vectors.
    pub flatten: bool,
    /// PDF, CMYK: the ICC profile of the press, embedded as the output intent.
    pub icc: Option<PathBuf>,
    /// Document px per inch of paper.
    pub ppi: f32,
}

impl Default for Print {
    fn default() -> Self {
        Self { bleed_mm: 0.0, marks: false, cmyk: false, flatten: false, icc: None, ppi: 72.0 }
    }
}

impl Print {
    /// Nothing is set: such settings are left out of the file.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    fn resolution(&self) -> f32 {
        if self.ppi.is_finite() && self.ppi > 0.0 {
            self.ppi.clamp(1.0, 4800.0)
        } else {
            72.0
        }
    }

    pub fn bleed(&self) -> f32 {
        if self.bleed_mm.is_finite() {
            self.bleed_mm.clamp(0.0, 100.0)
        } else {
            0.0
        }
    }

    /// The bleed in document px.
    pub fn bleed_px(&self) -> f32 {
        self.bleed() * self.resolution() / 25.4
    }

    /// Document px per millimetre of paper.
    pub fn px_per_mm(&self) -> f32 {
        self.resolution() / 25.4
    }

    /// PDF points per document px.
    pub fn pt_per_px(&self) -> f32 {
        72.0 / self.resolution()
    }

    /// The bleed, in document px, of an export that is `w` × `h` px with the bleed included.
    /// (A layer smaller than twice the bleed has no trim size left: no bleed then.)
    pub fn page_bleed(&self, w: f32, h: f32) -> f32 {
        let bleed = self.bleed_px();
        if w > bleed * 2.0 + 1.0 && h > bleed * 2.0 + 1.0 {
            bleed
        } else {
            0.0
        }
    }

    /// The size on paper of `w` × `h` document px, as text: "210 × 297 mm + 3 mm bleed".
    pub fn hint(&self, w: f32, h: f32) -> String {
        let mm = |px: f32| {
            let v = px / self.px_per_mm();
            if (v - v.round()).abs() < 0.25 {
                format!("{}", v.round())
            } else {
                format!("{v:.1}")
            }
        };
        let mut s = format!("{} × {} mm", mm(w), mm(h));
        if self.bleed() > 0.0 {
            s += &format!(" + {} mm bleed", (self.bleed() * 100.0).round() / 100.0);
        }
        s
    }
}

/// A page laid out for print, in one unit (points for a PDF, px for an image), origin in a
/// corner. As everything is symmetric it reads the same with y up or down.
#[derive(Clone, Debug, PartialEq)]
pub struct Sheet {
    /// The whole page: trim size, bleed, and the margin of the marks.
    pub media: (f32, f32),
    /// Where the paper is cut: x0, y0, x1, y1.
    pub trim: [f32; 4],
    /// The trim box grown by the bleed.
    pub bleed: [f32; 4],
    /// The crop marks, each a line x0, y0, x1, y1.
    pub marks: Vec<[f32; 4]>,
}

/// The sheet for a trim size of `w` × `h` with `bleed` around it. `mm`: units per millimetre.
pub fn sheet(w: f32, h: f32, bleed: f32, marks: bool, mm: f32) -> Sheet {
    let bleed = bleed.max(0.0);
    // The marks keep clear of the bleed, and of the trim edge by 3 mm at least.
    let (gap, len) = (bleed.max(3.0 * mm), 5.0 * mm);
    let margin = if marks { gap + len } else { bleed };
    let trim = [margin, margin, margin + w, margin + h];
    let mut lines = Vec::new();
    if marks {
        for (x, sx) in [(trim[0], -1.0), (trim[2], 1.0)] {
            for (y, sy) in [(trim[1], -1.0), (trim[3], 1.0)] {
                lines.push([x + sx * gap, y, x + sx * (gap + len), y]);
                lines.push([x, y + sy * gap, x, y + sy * (gap + len)]);
            }
        }
    }
    Sheet {
        media: (w + margin * 2.0, h + margin * 2.0),
        trim,
        bleed: [trim[0] - bleed, trim[1] - bleed, trim[2] + bleed, trim[3] + bleed],
        marks: lines,
    }
}

/// For PNG, JPG and SVG: the exported `svg` (`w` × `h` px, bleed included) on a sheet with
/// crop marks around it. Without marks it is returned as it is. The sheet is white, as paper
/// is: black marks on a see-through margin can't be seen where the picture is shown on a dark
/// ground. `scale`: pixels of the picture per px of the SVG (1 for an SVG file).
pub fn sheet_svg(svg: String, w: f32, h: f32, print: &Print, scale: f32) -> (String, f32, f32) {
    if !print.marks {
        return (svg, w, h);
    }
    let bleed = print.page_bleed(w, h);
    let s = sheet(w - bleed * 2.0, h - bleed * 2.0, bleed, true, print.px_per_mm());
    let (mw, mh) = s.media;
    // (A quarter of a point is less than a pixel at screen resolutions: one pixel of the
    // picture at least, however much smaller than the document that is made.)
    let width = (MARK_WIDTH / print.pt_per_px()).max(1.0 / if scale.is_finite() && scale > 0.0 { scale.min(1.0) } else { 1.0 });
    let mut out = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{mw}" height="{mh}" viewBox="0 0 {mw} {mh}">"#);
    out += &format!(r##"<rect width="{mw}" height="{mh}" fill="#ffffff"/>"##);
    out += &format!(r#"<g transform="translate({} {})">{svg}</g>"#, s.bleed[0], s.bleed[1]);
    out += r##"<path fill="none" stroke="#000000" stroke-width=""##;
    out += &format!(r#"{width}" d=""#);
    for [x0, y0, x1, y1] in &s.marks {
        out += &format!("M{x0} {y0}L{x1} {y1}");
    }
    out += r#""/></svg>"#;
    (out, mw, mh)
}

// ---------------------------------------------------------------- colour

/// RGB to CMYK by the plain formula, all components 0..1: as much black as the colour
/// allows (K = 1 − max), the rest in the three colours. Black is black ink only, greys are
/// black ink only, white is no ink. This knows nothing about a press, its inks or its paper:
/// the colours are as device dependent as the RGB numbers they come from.
pub fn naive([r, g, b]: [f32; 3]) -> [f32; 4] {
    let (r, g, b) = (r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
    let max = r.max(g).max(b);
    if max <= 0.0 {
        return [0.0, 0.0, 0.0, 1.0];
    }
    [(max - r) / max, (max - g) / max, (max - b) / max, 1.0 - max]
}

/// A CMYK ICC profile read from a file.
#[derive(Clone)]
pub struct Profile {
    pub bytes: Vec<u8>,
    /// Its description ("U.S. Web Coated (SWOP) v2"), or the file name.
    pub name: String,
}

/// The text of the `desc` tag of an ICC profile.
fn profile_description(icc: &[u8]) -> Option<String> {
    let int = |at: usize| icc.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
    let count = int(128)?.min(1024);
    let (at, size) = (0..count).map(|i| 132 + i * 12).find(|e| icc.get(*e..*e + 4) == Some(b"desc")).and_then(|e| Some((int(e + 4)?, int(e + 8)?)))?;
    let tag = icc.get(at..at.checked_add(size)?)?;
    let text = match tag.get(..4)? {
        // Version 2: an ASCII text with its length (the closing zero included).
        b"desc" => {
            let len = u32::from_be_bytes(tag.get(8..12)?.try_into().ok()?) as usize;
            String::from_utf8_lossy(tag.get(12..12 + len)?).into_owned()
        }
        // Version 4: texts by language, UTF-16; the first one.
        b"mluc" => {
            let field = |at: usize| tag.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize);
            let (len, start) = (field(20)?, field(24)?);
            let units: Vec<u16> = tag.get(start..start + len)?.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
            String::from_utf16_lossy(&units)
        }
        _ => return None,
    };
    let text = text.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_owned();
    (!text.is_empty()).then_some(text)
}

/// Read a CMYK ICC profile (.icc, .icm).
pub fn read_profile(path: &Path) -> Result<Profile, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("couldn't read the ICC profile: {e}"))?;
    if bytes.len() < 132 || &bytes[36..40] != b"acsp" {
        return Err("that file is not an ICC profile".into());
    }
    if &bytes[16..20] != b"CMYK" {
        return Err(format!("the ICC profile is for {}, not for CMYK", String::from_utf8_lossy(&bytes[16..20]).trim()));
    }
    let name = profile_description(&bytes).unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
    Ok(Profile { bytes, name })
}

/// How RGB becomes CMYK in one export.
pub struct Colour {
    /// sRGB to the CMYK of `profile`; `None`: the plain formula.
    cms: Option<Arc<moxcms::Transform8BitExecutor>>,
    pub profile: Option<Profile>,
}

impl Colour {
    /// The plain formula, no profile.
    pub fn naive() -> Self {
        Self { cms: None, profile: None }
    }

    /// Colours through `profile`: sRGB to its CMYK, relative colorimetric (the colour engine
    /// has no black point compensation, so the darkest shadows of a picture may close up).
    /// A profile that the engine can't use is still embedded, and the numbers are the plain
    /// ones.
    pub fn with_profile(profile: Profile) -> Self {
        let cms = moxcms::ColorProfile::new_from_slice(&profile.bytes).ok().and_then(|press| {
            let options = moxcms::TransformOptions { rendering_intent: moxcms::RenderingIntent::RelativeColorimetric, ..Default::default() };
            moxcms::ColorProfile::new_srgb().create_transform_8bit(moxcms::Layout::Rgb, &press, moxcms::Layout::Rgba, options).ok()
        });
        Self { cms, profile: Some(profile) }
    }

    /// Are the numbers made by the profile (not by the plain formula)?
    pub fn managed(&self) -> bool {
        self.cms.is_some()
    }

    /// The colour of vector content: a fill, a stroke, a gradient stop. Neutral colours are
    /// black ink only, whatever the profile says, so black text does not need four plates to
    /// register.
    pub fn vector(&self, rgb: [f32; 3]) -> [f32; 4] {
        let [r, g, b] = rgb.map(|c| c.clamp(0.0, 1.0));
        if (r - g).abs() < 0.002 && (g - b).abs() < 0.002 {
            return [0.0, 0.0, 0.0, 1.0 - g];
        }
        if let Some(cms) = &self.cms {
            let mut out = [0u8; 4];
            if cms.transform(&[r, g, b].map(|c| (c * 255.0).round() as u8), &mut out).is_ok() {
                return out.map(|c| c as f32 / 255.0);
            }
        }
        naive([r, g, b])
    }

    /// The pixels of a picture, RGB to CMYK, 8 bits each. (No special case for neutral
    /// pixels here: next to the nearly neutral ones around them it would show as a step.)
    pub fn pixels(&self, rgb: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; rgb.len() / 3 * 4];
        if let Some(cms) = &self.cms {
            if cms.transform(&rgb[..rgb.len() / 3 * 3], &mut out).is_ok() {
                return out;
            }
        }
        for (p, o) in rgb.chunks_exact(3).zip(out.chunks_exact_mut(4)) {
            let max = p[0].max(p[1]).max(p[2]) as u32;
            if max == 0 {
                o.copy_from_slice(&[0, 0, 0, 255]);
                continue;
            }
            let ink = |c: u8| (((max - c as u32) * 255 + max / 2) / max) as u8;
            o.copy_from_slice(&[ink(p[0]), ink(p[1]), ink(p[2]), 255 - max as u8]);
        }
        out
    }
}

// ---------------------------------------------------------------- PDF

/// The most pixels a flattened page may have. (Its bitmap takes four bytes per pixel while it
/// is rendered and converted: half a gigabyte at this size.)
const FLATTEN_PIXELS: f32 = 120e6;

/// Pixels per document px of a flattened page of `w` × `h` px: `FLATTEN_DPI`, or less for a
/// page so large that its bitmap would not fit in memory (or in the renderer: 16000 px a side).
fn flatten_scale(w: f32, h: f32, print: &Print) -> f32 {
    let area = (w * h).max(1.0);
    (FLATTEN_DPI / print.resolution()).min(16000.0 / w.max(h)).min((FLATTEN_PIXELS / area).sqrt())
}

/// One page as a single image of `FLATTEN_DPI`, on white: the samples (RGB, or CMYK when
/// `colour` is given), the size in pixels, and the resolution it has if that had to be less.
fn flatten(svg: &str, w: f32, h: f32, print: &Print, colour: Option<&Colour>) -> Result<(Vec<u8>, u32, u32, Option<f32>), String> {
    let scale = flatten_scale(w, h, print);
    let lower = (scale < FLATTEN_DPI / print.resolution() * 0.999).then(|| scale * print.resolution());
    let pixmap = crate::export::rasterize(svg, w, h, scale)?;
    let (pw, ph) = (pixmap.width(), pixmap.height());
    // The bitmap is turned into the samples in place, a band of rows at a time: a second and a
    // third copy of a large page would not fit in memory.
    let mut data = pixmap.take();
    let band = (pw as usize * 64).max(1);
    let mut rgb = Vec::with_capacity(band * 3);
    let mut out = 0;
    for start in (0..data.len() / 4).step_by(band) {
        let end = (start + band).min(data.len() / 4);
        rgb.clear();
        for p in data[start * 4..end * 4].chunks_exact(4) {
            // (Premultiplied: what is missing to full cover is the white of the paper.)
            let rest = 255 - p[3] as u16;
            rgb.extend([(p[0] as u16 + rest).min(255) as u8, (p[1] as u16 + rest).min(255) as u8, (p[2] as u16 + rest).min(255) as u8]);
        }
        // (What is written never passes what is still to be read: at most four bytes a pixel.)
        match colour {
            Some(colour) => {
                let cmyk = colour.pixels(&rgb);
                data[out..out + cmyk.len()].copy_from_slice(&cmyk);
                out += cmyk.len();
            }
            None => {
                data[out..out + rgb.len()].copy_from_slice(&rgb);
                out += rgb.len();
            }
        }
    }
    data.truncate(out);
    data.shrink_to_fit();
    Ok((data, pw, ph, lower))
}

/// Pixels per document px for the parts of a vector PDF that have to be bitmaps.
fn raster_scale(ppi: f32) -> f32 {
    (216.0 / ppi).max((300.0 / ppi).min(1.0))
}

/// How the colours of a PDF were made, for the status line: "RGB", "CMYK" …
fn colour_note(colour: Option<&Colour>) -> String {
    match colour {
        None => "RGB".into(),
        Some(c) => match (&c.profile, c.managed()) {
            (Some(p), true) => format!("CMYK ({})", p.name),
            (Some(p), false) => format!("CMYK (plain conversion; {} embedded but not usable for conversion)", p.name),
            (None, _) => "CMYK (plain conversion, no ICC profile)".into(),
        },
    }
}

/// Several SVGs as one PDF, a page each: (svg, width, height) in document px, the bleed of
/// `print` included (see `export::to_svg_bleed`). The content stays vector (text is outlines
/// already) unless `print.flatten`. One document px is `print.pt_per_px()` points, so with the
/// default 72 px per inch a frame of a Paper preset (A4 = 595 × 842) comes out at that paper
/// size. With bleed or marks every page gets a TrimBox and a BleedBox; the MediaBox holds the
/// marks as well.
#[cfg(test)]
pub fn pdf_bytes(pages: &[(String, f32, f32)], print: &Print) -> Result<Vec<u8>, String> {
    build(pages, print, &mut |_| {}).map(|(bytes, _)| bytes)
}

/// `pdf_bytes`, and how the colours were made (see `colour_note`).
/// `progress` is told the index of each page as its turn comes.
fn build(pages: &[(String, f32, f32)], print: &Print, progress: &mut dyn FnMut(usize)) -> Result<(Vec<u8>, String), String> {
    use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref, TextStr};
    use resvg::usvg;
    let colour = match (&print.icc, print.cmyk) {
        (Some(path), true) => Some(Colour::with_profile(read_profile(path)?)),
        (None, true) => Some(Colour::naive()),
        _ => None,
    };
    let scale = print.pt_per_px();
    let boxes = print.bleed() > 0.0 || print.marks;
    let mut alloc = Ref::new(1);
    let (catalog_id, tree_id, info_id) = (alloc.bump(), alloc.bump(), alloc.bump());
    let mut pdf = Pdf::new();
    let mut kids = Vec::new();
    let name = Name(b"S");
    // The resolution of the flattened pages, where a page was too large for `FLATTEN_DPI`.
    let mut lower: Option<f32> = None;
    for (at, (svg, w, h)) in pages.iter().enumerate() {
        progress(at);
        let bleed_px = print.page_bleed(*w, *h);
        let s = sheet((w - bleed_px * 2.0) * scale, (h - bleed_px * 2.0) * scale, bleed_px * scale, print.marks, MM);
        let art = if print.flatten {
            let (samples, pw, ph, less) = flatten(svg, *w, *h, print, colour.as_ref())?;
            lower = lower.or(less);
            let id = alloc.bump();
            let packed = pdf::deflate(&samples);
            drop(samples);
            let mut image = pdf.image_xobject(id, &packed);
            image.filter(Filter::FlateDecode);
            image.width(pw as i32).height(ph as i32).bits_per_component(8);
            match colour {
                Some(_) => image.color_space().device_cmyk(),
                None => image.color_space().device_rgb(),
            }
            image.finish();
            id
        } else {
            let tree = usvg::Tree::from_str(svg, &usvg::Options::default()).map_err(|e| e.to_string())?;
            // What uses a filter (shadows, colour overlays) becomes a bitmap: 3 pixels per px at
            // 72 px per inch (216 dpi, as before there were print settings); a finer document
            // gets as many pixels as it has, up to 300 dpi.
            let options = svg2pdf::ConversionOptions { raster_scale: raster_scale(print.resolution()), ..Default::default() };
            let (chunk, root) = svg2pdf::to_chunk(&tree, options).map_err(|e| e.to_string())?;
            // The objects of every page are numbered anew, so they don't collide in one file.
            let mut map = std::collections::HashMap::new();
            let chunk = chunk.renumber(|old| *map.entry(old).or_insert_with(|| alloc.bump()));
            pdf.extend(&chunk);
            *map.get(&root).ok_or("the page couldn't be converted")?
        };
        let (page_id, content_id) = (alloc.bump(), alloc.bump());
        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, s.media.0, s.media.1));
        if boxes {
            page.bleed_box(Rect::new(s.bleed[0], s.bleed[1], s.bleed[2], s.bleed[3]));
            page.trim_box(Rect::new(s.trim[0], s.trim[1], s.trim[2], s.trim[3]));
        }
        page.parent(tree_id);
        page.contents(content_id);
        page.resources().x_objects().pair(name, art);
        page.finish();
        // The art is 1 × 1: stretched over the bleed box.
        let mut content = Content::new();
        content.save_state();
        content.transform([s.bleed[2] - s.bleed[0], 0.0, 0.0, s.bleed[3] - s.bleed[1], s.bleed[0], s.bleed[1]]).x_object(name);
        content.restore_state();
        if !s.marks.is_empty() {
            content.set_line_width(MARK_WIDTH);
            // Registration black: on every plate, so the marks of the four inks lie on one
            // another.
            match colour {
                Some(_) => content.set_stroke_cmyk(1.0, 1.0, 1.0, 1.0),
                None => content.set_stroke_rgb(0.0, 0.0, 0.0),
            };
            for [x0, y0, x1, y1] in &s.marks {
                content.move_to(*x0, *y0).line_to(*x1, *y1);
            }
            content.stroke();
        }
        pdf.stream(content_id, &content.finish());
        kids.push(page_id);
    }
    let mut catalog = pdf.catalog(catalog_id);
    catalog.pages(tree_id);
    let profile = colour.as_ref().and_then(|c| c.profile.as_ref());
    let profile_id = profile.map(|_| alloc.bump());
    if let (Some(profile), Some(id)) = (profile, profile_id) {
        let mut intents = catalog.output_intents();
        let mut intent = intents.push();
        intent.subtype(pdf_writer::types::OutputIntentSubtype::PDFX);
        intent.output_condition_identifier(TextStr(&profile.name)).info(TextStr(&profile.name)).dest_output_profile(id);
    }
    catalog.finish();
    if let (Some(profile), Some(id)) = (profile, profile_id) {
        let packed = pdf::deflate(&profile.bytes);
        pdf.icc_profile(id, &packed).n(4).filter(Filter::FlateDecode);
    }
    pdf.pages(tree_id).kids(kids.iter().copied()).count(kids.len() as i32);
    pdf.document_info(info_id).producer(TextStr("Kanvas"));
    let bytes = pdf.finish();
    let bytes = match &colour {
        Some(colour) => pdf::to_cmyk(&bytes, colour)?,
        None => bytes,
    };
    let mut note = colour_note(colour.as_ref());
    if let Some(dpi) = lower {
        note += &format!(", flattened at {dpi:.0} dpi (the page is too large for {FLATTEN_DPI:.0})");
    }
    Ok((bytes, note))
}

/// Write the PDF of `pdf_bytes` to `path`. Returns how the colours were made ("RGB",
/// "CMYK (plain conversion, no ICC profile)", …), for the status line.
pub fn save_pdf(pages: &[(String, f32, f32)], print: &Print, path: &Path) -> Result<String, String> {
    save_pdf_with(pages, print, path, &mut |_| {})
}

/// `save_pdf` that tells `progress` the index of each page as its turn comes.
pub fn save_pdf_with(pages: &[(String, f32, f32)], print: &Print, path: &Path, progress: &mut dyn FnMut(usize)) -> Result<String, String> {
    let (bytes, colour) = build(pages, print, progress)?;
    std::fs::write(path, bytes).map_err(|e| e.to_string())?;
    Ok(colour)
}

#[cfg(test)]
mod tests;
