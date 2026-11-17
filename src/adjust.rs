//! Non-destructive image adjustments: exposure, contrast, saturation, temperature, tint,
//! highlights and shadows. The values live on the node; the pixels of the asset are never
//! changed. The canvas and the exports both run the same `Curves` over the decoded pixels, so
//! they show the same picture.
//!
//! The maths, on sRGB-encoded values in 0..1, in this order:
//! 1. per channel (a 256-entry table): white balance (temperature, tint) and exposure as gains
//!    in linear light, with a soft shoulder instead of clipping where a gain brightens, then
//!    contrast as an S-curve around middle grey;
//! 2. highlights and shadows: the luminance is bent in the bright or the dark half and the
//!    pixel follows it, keeping its hue;
//! 3. saturation: the distance of each channel from the luminance is scaled, and never so far
//!    that a channel would clip (so hues do not shift).

use serde::{Deserialize, Serialize};

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// Every value runs from -100 to +100; 0 leaves the image as it is.
#[derive(Clone, Copy, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct Adjust {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub exposure: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub contrast: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub saturation: f32,
    /// Negative = cooler (blue), positive = warmer (yellow).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub temperature: f32,
    /// Negative = green, positive = magenta.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub tint: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub highlights: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub shadows: f32,
}

/// The values in the order of `Adjust::values`: (name in files and the remote API, label).
pub const FIELDS: [(&str, &str); 7] = [
    ("exposure", "Exposure"),
    ("contrast", "Contrast"),
    ("saturation", "Saturation"),
    ("temperature", "Temperature"),
    ("tint", "Tint"),
    ("highlights", "Highlights"),
    ("shadows", "Shadows"),
];

/// Luminance of an sRGB-encoded colour (Rec. 709 weights).
const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

impl Adjust {
    pub fn values(&self) -> [f32; 7] {
        [self.exposure, self.contrast, self.saturation, self.temperature, self.tint, self.highlights, self.shadows]
    }

    /// Values out of range are brought into -100..=100; anything that is not a number is 0.
    pub fn from_values(v: [f32; 7]) -> Self {
        let v = v.map(|x| if x.is_finite() { x.clamp(-100.0, 100.0) + 0.0 } else { 0.0 });
        Adjust { exposure: v[0], contrast: v[1], saturation: v[2], temperature: v[3], tint: v[4], highlights: v[5], shadows: v[6] }
    }

    pub fn is_identity(&self) -> bool {
        self.values().iter().all(|v| *v == 0.0)
    }

    /// What to keep on a node: nothing when the image is left as it is.
    pub fn normalized(self) -> Option<Self> {
        let a = Self::from_values(self.values());
        (!a.is_identity()).then_some(a)
    }

    /// Identifies these values (for caches).
    pub fn key(&self) -> [u32; 7] {
        Self::from_values(self.values()).values().map(f32::to_bits)
    }

    /// The tables for these values. Make them once per image, not per pixel.
    pub fn curves(&self) -> Curves {
        Curves::new(self)
    }

    /// One pixel, straight alpha; the alpha is left alone. (For a single colour; images go
    /// through `curves` or `apply_rgba`, which make the tables once.)
    #[allow(dead_code)]
    pub fn apply(&self, rgba: [u8; 4]) -> [u8; 4] {
        if self.is_identity() {
            return rgba;
        }
        self.curves().pixel(rgba)
    }

    /// A buffer of RGBA pixels with straight alpha.
    pub fn apply_rgba(&self, buf: &mut [u8]) {
        if !self.is_identity() {
            self.curves().run(buf, false);
        }
    }
}

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// Step 1 for one channel (0 red, 1 green, 2 blue): `x` and the result are sRGB-encoded, 0..1.
fn tone(a: &Adjust, channel: usize, x: f32) -> f32 {
    let (e, c, t, n) = (a.exposure / 100.0, a.contrast / 100.0, a.temperature / 100.0, a.tint / 100.0);
    let mut x = x.clamp(0.0, 1.0);
    // White balance: a gain per channel, in stops. Warm takes blue (and a little green) away
    // and lifts red; cool takes red away and lifts blue; magenta takes green away, green takes
    // red and blue away. What is taken away tints white too, as a change of light does.
    let (warm, cool, magenta, green) = (t.max(0.0), (-t).max(0.0), n.max(0.0), (-n).max(0.0));
    let balance = match channel {
        0 => 0.20 * warm - 0.45 * cool + 0.10 * magenta - 0.25 * green,
        1 => -0.10 * warm - 0.15 * cool - 0.35 * magenta + 0.15 * green,
        _ => -0.50 * warm + 0.25 * cool + 0.10 * magenta - 0.25 * green,
    };
    // Exposure: two stops either way.
    if balance != 0.0 || e != 0.0 {
        // Brighter: the gain holds in the shadows and rolls off towards white instead of
        // clipping. Darker: a plain gain, as with a camera.
        let gain = |lin: f32, stops: f32| if stops > 0.0 { 1.0 - (1.0 - lin).powf(2f32.powf(stops)) } else { lin * 2f32.powf(stops) };
        x = to_srgb(gain(gain(to_linear(x), balance), 2.0 * e).clamp(0.0, 1.0));
    }
    if c > 0.0 {
        // An S-curve through middle grey: steeper in the middle, flat towards both ends.
        let p = 1.0 + 1.2 * c;
        x = if x < 0.5 { 0.5 * (2.0 * x).powf(p) } else { 1.0 - 0.5 * (2.0 * (1.0 - x)).powf(p) };
    } else if c < 0.0 {
        // Less contrast: everything moves towards middle grey (a fifth is left at -100).
        x = 0.5 + (x - 0.5) * (1.0 + 0.8 * c);
    }
    x.clamp(0.0, 1.0)
}

/// Bump with its peak (1) at a third and no effect at 0 and 1: where the shadows are. The
/// highlights use its mirror image.
fn bump(y: f32) -> f32 {
    6.75 * y * (1.0 - y) * (1.0 - y)
}

/// Steps 2 and 3 for one pixel, on sRGB-encoded values in 0..1. `sh` and `hi`: how far the
/// luminance moves at the peak of the shadows / highlights, `sat`: -1..1.
#[inline(always)]
fn finish(mut c: [f32; 3], sh: f32, hi: f32, sat: f32) -> [f32; 3] {
    if sh != 0.0 || hi != 0.0 {
        let y = LUMA[0] * c[0] + LUMA[1] * c[1] + LUMA[2] * c[2];
        let to = y + sh * bump(y) + hi * bump(1.0 - y);
        if y > 1e-5 && to < y {
            // Darker: scaled, which keeps the hue and the saturation.
            let k = (to / y).max(0.0);
            c = c.map(|v| v * k);
        } else if y > 1e-5 && y < 1.0 - 1e-5 && to > y {
            // Brighter: halfway between scaling (which would clip strong colours) and moving
            // towards white (which would wash them out). Both land on the luminance wanted.
            let (k, w) = (to / y, ((1.0 - to) / (1.0 - y)).max(0.0));
            c = c.map(|v| 0.5 * ((v * k).min(1.0) + 1.0 - (1.0 - v) * w));
        }
    }
    if sat != 0.0 {
        let y = LUMA[0] * c[0] + LUMA[1] * c[1] + LUMA[2] * c[2];
        let mut k = 1.0 + sat;
        if sat > 0.0 {
            // No further than where the first channel would reach 0 or 1.
            let (lo, hi) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
            if hi - y > 1e-5 {
                k = k.min((1.0 - y) / (hi - y));
            }
            if y - lo > 1e-5 {
                k = k.min(y / (y - lo));
            }
            k = k.max(1.0);
        }
        c = c.map(|v| y + (v - y) * k);
    }
    c
}

fn byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// The amplitudes of step 2. The side that could fold the curve over (darker shadows crush
/// towards black, brighter highlights towards white) gets the smaller one, so the luminance
/// stays monotonic for every combination of the two.
fn amplitudes(a: &Adjust) -> (f32, f32) {
    let (s, h) = (a.shadows / 100.0, a.highlights / 100.0);
    (s * if s > 0.0 { 0.22 } else { 0.14 }, h * if h > 0.0 { 0.14 } else { 0.22 })
}

/// An `Adjust` made ready to run over pixels.
pub struct Curves {
    identity: bool,
    /// Step 1 per channel, as a value in 0..1 and rounded to a byte.
    tone: Box<[[f32; 256]; 3]>,
    tone8: Box<[[u8; 256]; 3]>,
    sh: f32,
    hi: f32,
    sat: f32,
}

/// Buffers from this size on are split over several threads.
const THREADED_FROM: usize = 1 << 18;

impl Curves {
    pub fn new(a: &Adjust) -> Self {
        let a = Adjust::from_values(a.values());
        let mut tone_f = Box::new([[0f32; 256]; 3]);
        let mut tone8 = Box::new([[0u8; 256]; 3]);
        for ch in 0..3 {
            for i in 0..256 {
                tone_f[ch][i] = tone(&a, ch, i as f32 / 255.0);
                tone8[ch][i] = byte(tone_f[ch][i]);
            }
        }
        let (sh, hi) = amplitudes(&a);
        Curves { identity: a.is_identity(), tone: tone_f, tone8, sh, hi, sat: a.saturation / 100.0 }
    }

    /// Only step 1 is needed: a table lookup per channel.
    fn tables_only(&self) -> bool {
        self.sh == 0.0 && self.hi == 0.0 && self.sat == 0.0
    }

    #[inline(always)]
    fn rgb(&self, r: u8, g: u8, b: u8) -> [u8; 3] {
        let c = [self.tone[0][r as usize], self.tone[1][g as usize], self.tone[2][b as usize]];
        finish(c, self.sh, self.hi, self.sat).map(byte)
    }

    /// One pixel with straight alpha.
    pub fn pixel(&self, [r, g, b, a]: [u8; 4]) -> [u8; 4] {
        if self.identity {
            return [r, g, b, a];
        }
        if self.tables_only() {
            return [self.tone8[0][r as usize], self.tone8[1][g as usize], self.tone8[2][b as usize], a];
        }
        let [r, g, b] = self.rgb(r, g, b);
        [r, g, b, a]
    }

    fn chunk(&self, buf: &mut [u8], premultiplied: bool) {
        let tables = self.tables_only();
        for p in buf.chunks_exact_mut(4) {
            let a = p[3];
            if a == 0 {
                continue;
            }
            if premultiplied && a != 255 {
                // Out of the premultiplied form and back, both on the encoded values (as
                // tiny-skia stores them).
                let (af, half) = (a as u32, a as u32 / 2);
                let un = |v: u8| ((v as u32 * 255 + half) / af).min(255) as u8;
                let [r, g, b] = self.rgb(un(p[0]), un(p[1]), un(p[2]));
                let pre = |v: u8| ((v as u32 * af + 127) / 255) as u8;
                (p[0], p[1], p[2]) = (pre(r), pre(g), pre(b));
            } else if tables {
                (p[0], p[1], p[2]) = (self.tone8[0][p[0] as usize], self.tone8[1][p[1] as usize], self.tone8[2][p[2] as usize]);
            } else {
                let [r, g, b] = self.rgb(p[0], p[1], p[2]);
                (p[0], p[1], p[2]) = (r, g, b);
            }
        }
    }

    /// A buffer of RGBA pixels; the alpha is left alone. `premultiplied`: the colours are
    /// multiplied by the alpha (on the encoded values), otherwise the alpha is straight.
    /// Large buffers are worked on by several threads.
    pub fn run(&self, buf: &mut [u8], premultiplied: bool) {
        if self.identity {
            return;
        }
        let pixels = buf.len() / 4;
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(8);
        if pixels < THREADED_FROM || threads < 2 {
            return self.chunk(buf, premultiplied);
        }
        let per = pixels.div_ceil(threads) * 4;
        std::thread::scope(|s| {
            for part in buf.chunks_mut(per) {
                s.spawn(move || self.chunk(part, premultiplied));
            }
        });
    }

    /// As `run`, on this thread only (to compare with, and to time).
    #[cfg(test)]
    pub fn run_single(&self, buf: &mut [u8], premultiplied: bool) {
        if !self.identity {
            self.chunk(buf, premultiplied);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same maths without any table: every pixel from the formulas.
    fn reference(a: &Adjust, [r, g, b, alpha]: [u8; 4]) -> [u8; 4] {
        let a = Adjust::from_values(a.values());
        let c = [tone(&a, 0, r as f32 / 255.0), tone(&a, 1, g as f32 / 255.0), tone(&a, 2, b as f32 / 255.0)];
        let (sh, hi) = amplitudes(&a);
        let [r, g, b] = finish(c, sh, hi, a.saturation / 100.0).map(byte);
        [r, g, b, alpha]
    }

    fn with(i: usize, v: f32) -> Adjust {
        let mut values = [0.0; 7];
        values[i] = v;
        Adjust::from_values(values)
    }

    /// A coarse walk through the colour cube (and the corners).
    fn colours() -> impl Iterator<Item = [u8; 3]> {
        let steps = || (0..=255u32).step_by(15).map(|v| v as u8);
        steps().flat_map(move |r| steps().flat_map(move |g| steps().map(move |b| [r, g, b])))
    }

    const LOOKS: [[f32; 7]; 6] = [
        [35.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
        [0.0, 60.0, 0.0, 20.0, -10.0, 0.0, 0.0],
        [-20.0, 25.0, -30.0, 15.0, 5.0, -40.0, 35.0],
        [100.0, 100.0, 100.0, 100.0, 100.0, 100.0, 100.0],
        [-100.0, -100.0, -100.0, -100.0, -100.0, -100.0, -100.0],
        [0.0, 0.0, 70.0, 0.0, 0.0, 55.0, -65.0],
    ];

    #[test]
    fn identity_changes_nothing() {
        let a = Adjust::default();
        assert!(a.is_identity() && a.normalized().is_none());
        let curves = a.curves();
        for [r, g, b] in colours() {
            for alpha in [0, 1, 128, 255] {
                assert_eq!(a.apply([r, g, b, alpha]), [r, g, b, alpha]);
                assert_eq!(curves.pixel([r, g, b, alpha]), [r, g, b, alpha]);
            }
        }
        // Also when the tables are run without the shortcut: every table is the identity.
        for ch in 0..3 {
            for i in 0..256 {
                assert_eq!(curves.tone8[ch][i] as usize, i);
            }
        }
        let mut buf: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 256) as u8).collect();
        let before = buf.clone();
        a.apply_rgba(&mut buf);
        curves.run(&mut buf, true);
        assert_eq!(buf, before);
    }

    #[test]
    fn no_saturation_is_grey() {
        for extra in [0.0, 40.0] {
            let a = Adjust { saturation: -100.0, exposure: extra, shadows: extra, temperature: extra, ..Default::default() };
            let curves = a.curves();
            for [r, g, b] in colours() {
                let out = curves.pixel([r, g, b, 255]);
                assert!(out[0] == out[1] && out[1] == out[2], "{:?} -> {out:?}", [r, g, b]);
            }
        }
        // A grey stays the grey it was.
        let grey = Adjust { saturation: -100.0, ..Default::default() }.apply([90, 90, 90, 255]);
        assert_eq!(grey, [90, 90, 90, 255]);
    }

    #[test]
    fn exposure_and_contrast_are_monotonic() {
        for field in [0, 1] {
            for v in [-100.0, -60.0, -25.0, 25.0, 60.0, 100.0] {
                let curves = with(field, v).curves();
                for ch in 0..3 {
                    assert!(curves.tone8[ch].windows(2).all(|w| w[0] <= w[1]), "field {field} at {v}");
                }
            }
        }
        // More exposure never makes a tone darker; more contrast spreads the tones around
        // middle grey.
        let steps = [-100.0, -50.0, 0.0, 50.0, 100.0];
        for i in 0..256 {
            let exposed: Vec<u8> = steps.iter().map(|v| with(0, *v).curves().tone8[0][i]).collect();
            assert!(exposed.windows(2).all(|w| w[0] <= w[1]), "{i}: {exposed:?}");
            let contrast: Vec<u8> = steps.iter().map(|v| with(1, *v).curves().tone8[0][i]).collect();
            let spread = |w: &[u8]| if i < 128 { w[0] >= w[1] } else { w[0] <= w[1] };
            assert!(contrast.windows(2).all(spread), "{i}: {contrast:?}");
        }
        // Brighter exposure rolls off towards white instead of clipping: the brightest tones
        // are still told apart at +100.
        let top = with(0, 100.0).curves();
        assert!(top.tone8[0][200] < top.tone8[0][255]);
        assert_eq!((top.tone8[0][0], top.tone8[0][255]), (0, 255));
    }

    #[test]
    fn luminance_stays_monotonic_under_highlights_and_shadows() {
        for (h, s) in [(100.0, 100.0), (100.0, -100.0), (-100.0, 100.0), (-100.0, -100.0), (60.0, 0.0), (0.0, -60.0)] {
            let curves = Adjust { highlights: h, shadows: s, ..Default::default() }.curves();
            let ramp: Vec<u8> = (0..=255u8).map(|v| curves.pixel([v, v, v, 255])[0]).collect();
            assert!(ramp.windows(2).all(|w| w[0] <= w[1]), "highlights {h}, shadows {s}: {ramp:?}");
            assert_eq!((ramp[0], ramp[255]), (0, 255));
        }
        // Shadows up lifts the dark tones and leaves the bright ones nearly alone; highlights
        // down does the opposite.
        let up = Adjust { shadows: 100.0, ..Default::default() }.curves();
        assert!(up.pixel([60, 60, 60, 255])[0] > 80 && up.pixel([240, 240, 240, 255])[0] <= 243);
        let down = Adjust { highlights: -100.0, ..Default::default() }.curves();
        assert!(down.pixel([200, 200, 200, 255])[0] < 180 && down.pixel([20, 20, 20, 255])[0] >= 18);
    }

    #[test]
    fn alpha_is_untouched_and_extremes_stay_in_range() {
        for look in LOOKS {
            let a = Adjust::from_values(look);
            let curves = a.curves();
            for [r, g, b] in colours() {
                for alpha in [0, 3, 77, 200, 255] {
                    assert_eq!(curves.pixel([r, g, b, alpha])[3], alpha);
                }
            }
            // Premultiplied pixels stay valid: no channel above the alpha.
            let mut buf: Vec<u8> = Vec::new();
            for [r, g, b] in colours() {
                for alpha in [0u8, 1, 64, 255] {
                    let pre = |v: u8| ((v as u32 * alpha as u32 + 127) / 255) as u8;
                    buf.extend([pre(r), pre(g), pre(b), alpha]);
                }
            }
            let before = buf.clone();
            curves.run(&mut buf, true);
            for (p, q) in buf.chunks_exact(4).zip(before.chunks_exact(4)) {
                assert_eq!(p[3], q[3]);
                assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3], "{q:?} -> {p:?}");
            }
        }
        // Values out of range and things that are not numbers are tamed.
        let wild = Adjust { exposure: 1e9, contrast: f32::NAN, saturation: -1e9, tint: f32::INFINITY, ..Default::default() };
        let tamed = Adjust::from_values(wild.values());
        assert_eq!((tamed.exposure, tamed.contrast, tamed.saturation, tamed.tint), (100.0, 0.0, -100.0, 0.0));
        assert_eq!(wild.curves().pixel([10, 200, 90, 9])[3], 9);
        assert_eq!(wild.key(), tamed.key());
    }

    #[test]
    fn white_balance_leans_the_right_way() {
        let grey = [128, 128, 128, 255];
        let warm = with(3, 60.0).apply(grey);
        assert!(warm[0] > 128 && warm[2] < 128, "{warm:?}");
        let cool = with(3, -60.0).apply(grey);
        assert!(cool[0] < 128 && cool[2] > 128, "{cool:?}");
        let magenta = with(4, 60.0).apply(grey);
        assert!(magenta[1] < 128 && magenta[0] > 128 && magenta[2] > 128, "{magenta:?}");
        let green = with(4, -60.0).apply(grey);
        assert!(green[1] > 128 && green[0] < 128, "{green:?}");
        // Black stays black; white takes the colour of the light, without any channel folding.
        for a in [with(3, 100.0), with(3, -100.0), with(4, 100.0), with(4, -100.0)] {
            assert_eq!(a.apply([0, 0, 0, 255]), [0, 0, 0, 255]);
            let curves = a.curves();
            for ch in 0..3 {
                assert!(curves.tone8[ch].windows(2).all(|w| w[0] <= w[1]));
                assert!(curves.tone8[ch][255] > 200);
            }
        }
        let white = with(3, 100.0).apply([255, 255, 255, 255]);
        assert!(white[0] == 255 && white[1] < 255 && white[2] < white[1], "{white:?}");
    }

    #[test]
    fn more_saturation_keeps_the_hue_and_never_clips_a_channel_past_another() {
        let curves = with(2, 100.0).curves();
        for [r, g, b] in colours() {
            let out = curves.pixel([r, g, b, 255]);
            // The order of the channels (which decides the hue sector) is kept.
            let order = |c: [u8; 3]| [(c[0] as i32 - c[1] as i32).signum(), (c[1] as i32 - c[2] as i32).signum(), (c[0] as i32 - c[2] as i32).signum()];
            let (was, now) = (order([r, g, b]), order([out[0], out[1], out[2]]));
            assert!(was.iter().zip(now).all(|(a, b)| *a == 0 || *a == b), "{:?} -> {out:?}", [r, g, b]);
        }
        let more = curves.pixel([150, 120, 100, 255]);
        assert!(more[0] as i32 - more[2] as i32 > 50, "{more:?}");
    }

    #[test]
    fn tables_match_the_formulas() {
        for look in LOOKS.into_iter().chain((0..7).flat_map(|i| [with(i, -60.0).values(), with(i, 60.0).values()])) {
            let a = Adjust::from_values(look);
            let curves = a.curves();
            for [r, g, b] in colours() {
                let px = [r, g, b, 200];
                assert_eq!(curves.pixel(px), reference(&a, px), "{look:?} at {px:?}");
                assert_eq!(a.apply(px), reference(&a, px));
            }
        }
    }

    #[test]
    fn a_buffer_matches_pixel_by_pixel_on_any_number_of_threads() {
        // Larger than `THREADED_FROM`, and not a multiple of the thread count.
        let pixels = THREADED_FROM + 12_345;
        let mut state = 0x2545_f491u32;
        let src: Vec<u8> = (0..pixels * 4)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state >> 8) as u8
            })
            .collect();
        for look in [LOOKS[1], LOOKS[2]] {
            let a = Adjust::from_values(look);
            let curves = a.curves();
            let (mut threaded, mut single) = (src.clone(), src.clone());
            a.apply_rgba(&mut threaded);
            curves.run_single(&mut single, false);
            assert!(threaded == single);
            for (out, px) in threaded.chunks_exact(4).zip(src.chunks_exact(4)).step_by(97) {
                let px = [px[0], px[1], px[2], px[3]];
                // A pixel without alpha is skipped: its colour is not seen anyway.
                let want = if px[3] == 0 { px } else { reference(&a, px) };
                assert_eq!(out, want);
            }
        }
    }

    #[test]
    fn only_what_is_set_is_written_and_old_files_read_as_no_adjustment() {
        let a = Adjust { contrast: 20.0, tint: -5.0, ..Default::default() };
        let text = serde_json::to_string(&a).unwrap();
        assert_eq!(text, r#"{"contrast":20.0,"tint":-5.0}"#);
        assert_eq!(serde_json::from_str::<Adjust>(&text).unwrap(), a);
        assert_eq!(serde_json::from_str::<Adjust>("{}").unwrap(), Adjust::default());
        // On a node: left out of the file when there is none, and a file without it reads back.
        let mut n = crate::doc::Node::new(1, crate::doc::Kind::Image, 0.0, 0.0);
        let plain = serde_json::to_string(&n).unwrap();
        assert!(!plain.contains("adjust"));
        assert_eq!(serde_json::from_str::<crate::doc::Node>(&plain).unwrap().adjust, None);
        n.adjust = Some(a);
        let back: crate::doc::Node = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(back.adjust, Some(a));
        assert_eq!(back.adjustment(), Some(a));
        // All zero on a node counts as none.
        n.adjust = Some(Adjust::default());
        assert_eq!(n.adjustment(), None);
    }

    /// `cargo test --release -- --ignored --nocapture adjust_speed`: 12 megapixels.
    #[test]
    #[ignore]
    fn adjust_speed() {
        let pixels = 12_000_000;
        let src: Vec<u8> = (0..pixels * 4).map(|i| (i * 31 % 251) as u8 | if i % 4 == 3 { 255 } else { 0 }).collect();
        let looks = [("tables only (exposure, contrast, temperature, tint)", [30.0, 40.0, 0.0, 20.0, -10.0, 0.0, 0.0]), ("all seven", LOOKS[2])];
        for (name, look) in looks {
            let curves = Adjust::from_values(look).curves();
            let mut best = [f64::MAX; 2];
            for _ in 0..3 {
                let mut buf = src.clone();
                let t = std::time::Instant::now();
                curves.run_single(&mut buf, false);
                best[0] = best[0].min(t.elapsed().as_secs_f64() * 1000.0);
                let mut buf = src.clone();
                let t = std::time::Instant::now();
                curves.run(&mut buf, false);
                best[1] = best[1].min(t.elapsed().as_secs_f64() * 1000.0);
            }
            println!("12 MP, {name}: {:.1} ms on one thread, {:.1} ms threaded", best[0], best[1]);
        }
    }
}
