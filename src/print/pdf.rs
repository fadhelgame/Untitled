//! A small reader for the PDF files this app writes (`pdf-writer` with pages from `svg2pdf`),
//! and the rewrite of such a file from RGB to CMYK.
//!
//! `svg2pdf` only knows RGB: colours are set in an ICC based sRGB colour space (`r g b scn`),
//! gradients are shadings with functions of three components, pictures are `/DeviceRGB` image
//! streams. As the producer is known, the file is regular enough to be read back object by
//! object and written again with every one of these in `/DeviceCMYK`. This is not a general
//! PDF reader: it knows classic cross-reference tables, direct `/Length` values, and the Flate
//! and DCT filters, which is all that the two writers use.

use super::Colour;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Null,
    Bool(bool),
    Num(f64),
    /// Without the leading slash, escapes (`#20`) left as they are.
    Name(Vec<u8>),
    /// As written, delimiters included: `(text)` or `<hex>`.
    Str(Vec<u8>),
    Arr(Vec<Val>),
    Dict(Dict),
    Ref(u32),
}

pub type Dict = Vec<(Vec<u8>, Val)>;

#[derive(Clone, Debug, PartialEq)]
pub struct Object {
    pub id: u32,
    pub val: Val,
    /// The bytes between `stream` and `endstream`, still encoded.
    pub stream: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct File {
    pub objects: Vec<Object>,
    pub trailer: Dict,
}

pub fn get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Val> {
    dict.iter().find(|(k, _)| k == key.as_bytes()).map(|(_, v)| v)
}

fn set(dict: &mut Dict, key: &str, val: Val) {
    match dict.iter_mut().find(|(k, _)| k == key.as_bytes()) {
        Some(pair) => pair.1 = val,
        None => dict.push((key.as_bytes().to_vec(), val)),
    }
}

fn remove(dict: &mut Dict, key: &str) {
    dict.retain(|(k, _)| k != key.as_bytes());
}

fn name(s: &str) -> Val {
    Val::Name(s.as_bytes().to_vec())
}

impl Val {
    pub fn dict(&self) -> Option<&Dict> {
        match self {
            Val::Dict(d) => Some(d),
            _ => None,
        }
    }

    pub fn num(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }

    pub fn is_name(&self, s: &str) -> bool {
        matches!(self, Val::Name(n) if n == s.as_bytes())
    }

    /// The numbers of an array of numbers.
    pub fn nums(&self) -> Option<Vec<f64>> {
        match self {
            Val::Arr(a) => a.iter().map(|v| v.num()).collect(),
            _ => None,
        }
    }

    /// Call `f` on this value and on everything in it. What `f` puts in place of a value is
    /// walked as well.
    fn walk(&mut self, f: &mut dyn FnMut(&mut Val)) {
        f(self);
        match self {
            Val::Arr(a) => a.iter_mut().for_each(|v| v.walk(f)),
            Val::Dict(d) => d.iter_mut().for_each(|(_, v)| v.walk(f)),
            _ => {}
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Val::Null => out.extend(b"null"),
            Val::Bool(b) => out.extend(if *b { &b"true"[..] } else { &b"false"[..] }),
            Val::Num(n) => out.extend(number(*n).as_bytes()),
            Val::Name(n) => {
                out.push(b'/');
                out.extend(n);
            }
            Val::Str(s) => out.extend(s),
            Val::Arr(a) => {
                out.push(b'[');
                for (i, v) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(b' ');
                    }
                    v.write(out);
                }
                out.push(b']');
            }
            Val::Dict(d) => {
                out.extend(b"<<");
                for (k, v) in d {
                    out.push(b'/');
                    out.extend(k);
                    out.push(b' ');
                    v.write(out);
                    out.push(b'\n');
                }
                out.extend(b">>");
            }
            Val::Ref(id) => out.extend(format!("{id} 0 R").as_bytes()),
        }
    }
}

/// A number as PDF wants it: no exponent, and the digits of the `f32` it was written from
/// (the matrix of a page is 1 / its size: five decimals would move its far edge).
fn number(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        return format!("{}", n as i64);
    }
    format!("{}", n as f32)
}

fn is_space(c: u8) -> bool {
    matches!(c, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delimiter(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    /// Skip white space and comments.
    fn space(&mut self) {
        while let Some(&c) = self.b.get(self.i) {
            if is_space(c) {
                self.i += 1;
            } else if c == b'%' {
                while self.b.get(self.i).is_some_and(|c| !matches!(c, 10 | 13)) {
                    self.i += 1;
                }
            } else {
                break;
            }
        }
    }

    fn at(&self, word: &[u8]) -> bool {
        self.b[self.i.min(self.b.len())..].starts_with(word)
    }

    fn expect(&mut self, word: &str) -> Result<(), String> {
        self.space();
        if !self.at(word.as_bytes()) {
            return Err(format!("PDF: `{word}` expected at byte {}", self.i));
        }
        self.i += word.len();
        Ok(())
    }

    /// A run of regular characters (a number, a keyword, the text of a name).
    fn word(&mut self) -> &'a [u8] {
        let start = self.i;
        while self.b.get(self.i).is_some_and(|c| !is_space(*c) && !is_delimiter(*c)) {
            self.i += 1;
        }
        &self.b[start..self.i]
    }

    fn integer(&mut self) -> Option<u32> {
        self.space();
        let start = self.i;
        let word = self.word();
        let n = std::str::from_utf8(word).ok().and_then(|w| w.parse().ok());
        if n.is_none() {
            self.i = start;
        }
        n
    }

    /// The end of a string that starts at `self.i`: `(…)` with nesting and escapes, or `<…>`.
    fn string(&mut self) -> Result<Val, String> {
        let start = self.i;
        if self.b[self.i] == b'<' {
            while self.b.get(self.i).is_some_and(|c| *c != b'>') {
                self.i += 1;
            }
        } else {
            let mut depth = 0;
            loop {
                match self.b.get(self.i) {
                    Some(b'\\') => self.i += 1,
                    Some(b'(') => depth += 1,
                    Some(b')') => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    Some(_) => {}
                    None => return Err("PDF: a string does not end".into()),
                }
                self.i += 1;
            }
        }
        self.i += 1;
        self.b.get(start..self.i).map(|s| Val::Str(s.to_vec())).ok_or_else(|| "PDF: a string does not end".to_owned())
    }

    fn value(&mut self) -> Result<Val, String> {
        self.space();
        let Some(&c) = self.b.get(self.i) else { return Err("PDF: the file ends inside an object".into()) };
        match c {
            b'/' => {
                self.i += 1;
                Ok(Val::Name(self.word().to_vec()))
            }
            b'(' => self.string(),
            b'<' if self.at(b"<<") => {
                self.i += 2;
                let mut dict = Dict::new();
                loop {
                    self.space();
                    if self.at(b">>") {
                        self.i += 2;
                        return Ok(Val::Dict(dict));
                    }
                    match self.value()? {
                        Val::Name(key) => dict.push((key, self.value()?)),
                        _ => return Err(format!("PDF: a dictionary key is not a name, at byte {}", self.i)),
                    }
                }
            }
            b'<' => self.string(),
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.space();
                    if self.at(b"]") {
                        self.i += 1;
                        return Ok(Val::Arr(items));
                    }
                    items.push(self.value()?);
                }
            }
            _ => {
                let start = self.i;
                let word = std::str::from_utf8(self.word()).unwrap_or_default();
                match word {
                    "true" => return Ok(Val::Bool(true)),
                    "false" => return Ok(Val::Bool(false)),
                    "null" => return Ok(Val::Null),
                    _ => {}
                }
                let Ok(n) = word.parse::<f64>() else { return Err(format!("PDF: unexpected `{word}` at byte {start}")) };
                // `12 0 R` is a reference, not three values.
                if word.bytes().all(|c| c.is_ascii_digit()) {
                    let after = self.i;
                    if self.integer().is_some() {
                        self.space();
                        if self.at(b"R") && self.b.get(self.i + 1).is_none_or(|c| is_space(*c) || is_delimiter(*c)) {
                            self.i += 1;
                            return Ok(Val::Ref(n as u32));
                        }
                    }
                    self.i = after;
                }
                Ok(Val::Num(n))
            }
        }
    }
}

impl File {
    pub fn parse(bytes: &[u8]) -> Result<File, String> {
        let mut r = Reader { b: bytes, i: 0 };
        let mut file = File::default();
        loop {
            r.space();
            if r.at(b"xref") {
                let Some(at) = bytes[r.i..].windows(7).position(|w| w == b"trailer") else { return Err("PDF: no trailer".into()) };
                r.i += at + 7;
                match r.value()? {
                    Val::Dict(d) => file.trailer = d,
                    _ => return Err("PDF: the trailer is not a dictionary".into()),
                }
                return Ok(file);
            }
            let Some(id) = r.integer() else { return Err(format!("PDF: an object expected at byte {}", r.i)) };
            r.integer().ok_or("PDF: a generation number expected")?;
            r.expect("obj")?;
            let val = r.value()?;
            r.space();
            let mut stream = None;
            if r.at(b"stream") {
                r.i += 6;
                if r.at(b"\r") {
                    r.i += 1;
                }
                if r.at(b"\n") {
                    r.i += 1;
                }
                let len = val.dict().and_then(|d| get(d, "Length")).and_then(|l| l.num()).ok_or("PDF: a stream without a direct /Length")? as usize;
                let data = bytes.get(r.i..r.i + len).ok_or("PDF: a stream runs past the end of the file")?;
                stream = Some(data.to_vec());
                r.i += len;
                r.expect("endstream")?;
            }
            r.expect("endobj")?;
            file.objects.push(Object { id, val, stream });
        }
    }

    /// The whole file again. Objects are numbered anew from 1, in the order they are in.
    pub fn write(&self) -> Vec<u8> {
        let ids: HashMap<u32, u32> = self.objects.iter().enumerate().map(|(i, o)| (o.id, i as u32 + 1)).collect();
        let renumber = |v: &Val| {
            let mut v = v.clone();
            v.walk(&mut |v| {
                if let Val::Ref(id) = v {
                    // (A reference to an object that is not there reads as null.)
                    *v = ids.get(id).map_or(Val::Null, |new| Val::Ref(*new));
                }
            });
            v
        };
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend(format!("{} 0 obj\n", i + 1).as_bytes());
            renumber(&o.val).write(&mut out);
            if let Some(data) = &o.stream {
                out.extend(b"\nstream\n");
                out.extend(data);
                out.extend(b"\nendstream");
            }
            out.extend(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", offsets.len() + 1).as_bytes());
        for at in offsets {
            out.extend(format!("{at:010} 00000 n\r\n").as_bytes());
        }
        let mut trailer = self.trailer.clone();
        set(&mut trailer, "Size", Val::Num(self.objects.len() as f64 + 1.0));
        out.extend(b"trailer\n");
        renumber(&Val::Dict(trailer)).write(&mut out);
        out.extend(format!("\nstartxref\n{xref}\n%%EOF").as_bytes());
        out
    }

    /// The decoded bytes of every stream that is page content (pages, form XObjects, tiling
    /// patterns), for looking at what a file paints.
    #[cfg(test)]
    pub fn contents(&self) -> Vec<Vec<u8>> {
        let pages = self.page_contents();
        self.objects.iter().filter(|o| is_content(o, &pages)).filter_map(|o| decode(o.val.dict()?, o.stream.as_ref()?).ok()).collect()
    }

    /// The streams that pages name as their content.
    fn page_contents(&self) -> HashSet<u32> {
        let mut found = HashSet::new();
        for d in self.objects.iter().filter_map(|o| o.val.dict()) {
            if !get(d, "Type").is_some_and(|t| t.is_name("Page")) {
                continue;
            }
            match get(d, "Contents") {
                Some(Val::Ref(id)) => {
                    found.insert(*id);
                }
                Some(Val::Arr(a)) => found.extend(a.iter().filter_map(|v| if let Val::Ref(id) = v { Some(*id) } else { None })),
                _ => {}
            }
        }
        found
    }
}

/// Is this stream a run of painting operators?
fn is_content(o: &Object, pages: &HashSet<u32>) -> bool {
    let Some(d) = o.val.dict() else { return false };
    o.stream.is_some() && (pages.contains(&o.id) || get(d, "Subtype").is_some_and(|s| s.is_name("Form")) || get(d, "PatternType").is_some_and(|p| p.num() == Some(1.0)))
}

/// The one filter of a stream: "" for none.
fn filter(dict: &Dict) -> Result<String, String> {
    let text = |v: &Val| match v {
        Val::Name(n) => Ok(String::from_utf8_lossy(n).into_owned()),
        _ => Err("PDF: a filter is not a name".to_owned()),
    };
    match get(dict, "Filter") {
        None | Some(Val::Null) => Ok(String::new()),
        Some(Val::Arr(a)) if a.is_empty() => Ok(String::new()),
        Some(Val::Arr(a)) if a.len() == 1 => text(&a[0]),
        Some(Val::Arr(_)) => Err("PDF: a stream with several filters".into()),
        Some(v) => text(v),
    }
}

/// The bytes of a stream without its Flate encoding.
pub fn decode(dict: &Dict, data: &[u8]) -> Result<Vec<u8>, String> {
    match filter(dict)?.as_str() {
        "" => Ok(data.to_vec()),
        "FlateDecode" if get(dict, "DecodeParms").is_none() => miniz_oxide::inflate::decompress_to_vec_zlib(data).map_err(|e| format!("PDF: a stream does not inflate ({e})")),
        other => Err(format!("PDF: can't read a stream encoded with {other}")),
    }
}

pub fn deflate(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// Put `data` in place of the stream of `dict`, Flate encoded.
fn replace_stream(dict: &mut Dict, stream: &mut Option<Vec<u8>>, data: &[u8]) {
    let packed = deflate(data);
    set(dict, "Filter", name("FlateDecode"));
    set(dict, "Length", Val::Num(packed.len() as f64));
    remove(dict, "DecodeParms");
    *stream = Some(packed);
}

/// A colour component for a content stream.
fn component(v: f32) -> String {
    let s = format!("{:.4}", v.clamp(0.0, 1.0));
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A content stream with its RGB colours as CMYK: `r g b rg` and `r g b scn` (three numbers:
/// a colour in an RGB space, as the spaces of one or four components take other counts and
/// patterns take a name) become `c m y k k`, which also selects `/DeviceCMYK`. The same for
/// the stroking operators. Everything else is copied as it is.
pub fn content_to_cmyk(src: &[u8], colour: &Colour) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(src.len() + src.len() / 8);
    // The operands since the last operator: (start, end, value if it is a number).
    let mut operands: Vec<(usize, usize, Option<f32>)> = Vec::new();
    let mut copied = 0;
    let mut r = Reader { b: src, i: 0 };
    loop {
        r.space();
        let start = r.i;
        let Some(&c) = src.get(r.i) else { break };
        match c {
            b'(' => {
                r.string()?;
                operands.push((start, r.i, None));
            }
            b'<' if !r.at(b"<<") => {
                r.string()?;
                operands.push((start, r.i, None));
            }
            b'/' => {
                r.i += 1;
                r.word();
                operands.push((start, r.i, None));
            }
            b'<' | b'>' => {
                r.i += 2;
                operands.push((start, r.i, None));
            }
            b'[' | b']' | b'{' | b'}' | b')' => {
                r.i += 1;
                operands.push((start, r.i, None));
            }
            _ => {
                let word = r.word();
                if let Some(n) = std::str::from_utf8(word).ok().and_then(|w| w.parse::<f32>().ok()) {
                    operands.push((start, r.i, Some(n)));
                    continue;
                }
                let stroke = match word {
                    b"rg" => Some(false),
                    b"RG" => Some(true),
                    b"sc" | b"scn" if operands.len() == 3 => Some(false),
                    b"SC" | b"SCN" if operands.len() == 3 => Some(true),
                    // Inline images carry bytes that are not operators; neither writer makes them.
                    b"BI" => return Err("PDF: inline images can't be converted to CMYK".into()),
                    _ => None,
                };
                let rgb: Vec<f32> = operands.iter().rev().take(3).rev().filter_map(|o| o.2).collect();
                if let (Some(stroke), [red, green, blue]) = (stroke, rgb.as_slice()) {
                    let first = operands[operands.len() - 3].0;
                    let [c, m, y, k] = colour.vector([*red, *green, *blue]);
                    out.extend(&src[copied..first]);
                    out.extend(format!("{} {} {} {} {}", component(c), component(m), component(y), component(k), if stroke { "K" } else { "k" }).as_bytes());
                    copied = r.i;
                }
                operands.clear();
            }
        }
    }
    out.extend(&src[copied..]);
    Ok(out)
}

/// The samples of an RGB image stream as CMYK, 8 bits each.
fn image_to_cmyk(dict: &Dict, data: &[u8], colour: &Colour) -> Result<Vec<u8>, String> {
    let int = |key: &str| get(dict, key).and_then(|v| v.num()).map(|n| n as usize).ok_or(format!("PDF: an image without /{key}"));
    let (w, h, bits) = (int("Width")?, int("Height")?, int("BitsPerComponent")?);
    let rgb = match filter(dict)?.as_str() {
        "DCTDecode" => image::load_from_memory_with_format(data, image::ImageFormat::Jpeg).map_err(|e| format!("PDF: a JPEG could not be read ({e})"))?.to_rgb8().into_raw(),
        _ => {
            let raw = decode(dict, data)?;
            match bits {
                8 => raw,
                // The high byte of every sample.
                16 => raw.chunks_exact(2).map(|s| s[0]).collect(),
                _ => return Err(format!("PDF: an image with {bits} bits per component")),
            }
        }
    };
    if rgb.len() != w * h * 3 {
        return Err("PDF: an image has the wrong number of samples".into());
    }
    Ok(colour.pixels(&rgb))
}

/// How many pieces a blend between two colours is cut into when it is not a straight line
/// in CMYK.
const BLEND_PIECES: usize = 16;

fn numbers(values: &[f32]) -> Val {
    Val::Arr(values.iter().map(|c| Val::Num((*c as f64 * 1e4).round() / 1e4)).collect())
}

/// The function of a blend from one RGB colour to another (what a gradient is between two of
/// its stops), in CMYK. Halfway between two colours in RGB is not halfway between their CMYK
/// numbers (black to red passes through dark red, not through grey pink), so unless the
/// straight line is close enough the blend is cut into `BLEND_PIECES` pieces, each a straight
/// line between two colours taken from the RGB blend.
pub fn blend_to_cmyk(from: [f32; 3], to: [f32; 3], colour: &Colour) -> Dict {
    let at = |t: f32| colour.vector([0, 1, 2].map(|i| from[i] + (to[i] - from[i]) * t));
    let piece = |a: [f32; 4], b: [f32; 4]| -> Dict {
        vec![
            (b"FunctionType".to_vec(), Val::Num(2.0)),
            (b"Domain".to_vec(), numbers(&[0.0, 1.0])),
            (b"Range".to_vec(), numbers(&[0.0, 1.0].repeat(4))),
            (b"C0".to_vec(), numbers(&a)),
            (b"C1".to_vec(), numbers(&b)),
            (b"N".to_vec(), Val::Num(1.0)),
        ]
    };
    let (a, b) = (at(0.0), at(1.0));
    let straight = [0.25, 0.5, 0.75].iter().all(|t| {
        let mid = at(*t);
        (0..4).all(|i| (mid[i] - (a[i] + (b[i] - a[i]) * t)).abs() < 0.02)
    });
    if straight {
        return piece(a, b);
    }
    let n = BLEND_PIECES;
    let step = |i: usize| i as f32 / n as f32;
    vec![
        (b"FunctionType".to_vec(), Val::Num(3.0)),
        (b"Domain".to_vec(), numbers(&[0.0, 1.0])),
        (b"Range".to_vec(), numbers(&[0.0, 1.0].repeat(4))),
        (b"Functions".to_vec(), Val::Arr((0..n).map(|i| Val::Dict(piece(at(step(i)), at(step(i + 1))))).collect())),
        (b"Bounds".to_vec(), numbers(&(1..n).map(step).collect::<Vec<f32>>())),
        (b"Encode".to_vec(), numbers(&[0.0, 1.0].repeat(n))),
    ]
}

/// The PDF `bytes` (written by this app) with everything that is RGB turned into CMYK:
/// colours in content streams, gradient stops, pictures, and the colour spaces of
/// transparency groups. Grey stays grey (`/DeviceGray`). The sRGB and grey ICC profiles that
/// `svg2pdf` embeds are left out, as nothing refers to them any more.
pub fn to_cmyk(bytes: &[u8], colour: &Colour) -> Result<Vec<u8>, String> {
    let mut file = File::parse(bytes)?;
    // The ICC profiles by their number of components.
    let profiles: HashMap<u32, f64> =
        file.objects.iter().filter(|o| o.stream.is_some()).filter_map(|o| Some((o.id, get(o.val.dict()?, "N")?.num()?))).collect();
    let mut unused = HashSet::new();
    for o in &mut file.objects {
        o.val.walk(&mut |v| {
            let Val::Arr(a) = v else { return };
            let [Val::Name(kind), Val::Ref(id)] = a.as_slice() else { return };
            if kind != b"ICCBased" {
                return;
            }
            let device = match profiles.get(id) {
                Some(n) if *n == 3.0 => "DeviceCMYK",
                Some(n) if *n == 1.0 => "DeviceGray",
                _ => return,
            };
            unused.insert(*id);
            *v = name(device);
        });
    }
    file.objects.retain(|o| !unused.contains(&o.id));

    let pages = file.page_contents();
    for o in &mut file.objects {
        let content = is_content(o, &pages);
        let Val::Dict(dict) = &mut o.val else { continue };
        // Gradients: the functions that give three components give four.
        if get(dict, "FunctionType").is_some() {
            let stop = |key: &str| get(dict, key).and_then(|v| v.nums()).filter(|c| c.len() == 3).map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]);
            if let (Some(from), Some(to)) = (stop("C0"), stop("C1")) {
                *dict = blend_to_cmyk(from, to, colour);
            }
            if get(dict, "Range").and_then(|v| v.nums()).is_some_and(|r| r.len() == 6) {
                set(dict, "Range", Val::Arr([0.0, 1.0].repeat(4).into_iter().map(Val::Num).collect()));
            }
        }
        let Some(data) = &o.stream else { continue };
        if get(dict, "Subtype").is_some_and(|s| s.is_name("Image")) {
            match get(dict, "ColorSpace") {
                Some(space) if space.is_name("DeviceRGB") => {
                    let cmyk = image_to_cmyk(dict, data, colour)?;
                    set(dict, "ColorSpace", name("DeviceCMYK"));
                    set(dict, "BitsPerComponent", Val::Num(8.0));
                    remove(dict, "Decode");
                    replace_stream(dict, &mut o.stream, &cmyk);
                }
                Some(space) if space.is_name("DeviceGray") || space.is_name("DeviceCMYK") => {}
                _ => return Err("PDF: an image in a colour space that can't be converted to CMYK".into()),
            }
        } else if content {
            let painted = content_to_cmyk(&decode(dict, data)?, colour)?;
            replace_stream(dict, &mut o.stream, &painted);
        }
    }
    // What still says RGB: the colour space of a shading or of a transparency group.
    for o in &mut file.objects {
        o.val.walk(&mut |v| {
            if v.is_name("DeviceRGB") {
                *v = name("DeviceCMYK");
            }
        });
    }
    Ok(file.write())
}
