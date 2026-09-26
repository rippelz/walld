//! Pick a representative accent colour out of a wallpaper preview image.
//!
//! Used when a wallpaper doesn't declare a Wallpaper Engine `schemecolor`.
//! A plain average is useless here (every wallpaper averages to mud), so
//! pixels are binned by hue and weighted towards vivid mid-tones — the colour
//! a person would name if asked "what colour is this wallpaper?".

use image::RgbaImage;
use std::path::Path;

/// Longest edge the preview is sampled at. Small on purpose: this runs for
/// every wallpaper in the library and the answer doesn't get better with more
/// pixels.
const SAMPLE_EDGE: u32 = 64;

/// Hue bins (15° each).
const BINS: usize = 24;

/// Below this saturation a pixel is "grey" and can't vote for a hue.
const GREY_SAT: f32 = 0.12;

/// Frames sampled from an animated preview. Animated previews very often fade
/// in from black, so frame 0 alone is a trap.
const MAX_GIF_FRAMES: usize = 48;

/// Below this brightness the result isn't a usable accent — the caller should
/// fall back rather than theme the UI black.
const MIN_USABLE_VALUE: f32 = 0.03;

/// Dominant colour of the image at `path`, as RGB in 0..1.
///
/// Returns `None` when the file can't be decoded, or when it has no colour
/// worth using (a fully black or fully transparent preview).
pub fn dominant_color(path: &Path) -> Option<[f32; 3]> {
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    {
        if let Some(c) = dominant_color_gif(path) {
            return Some(c);
        }
        // Fall through: a single-frame GIF still decodes as a still image.
    }
    let img = image::open(path).ok()?;
    let small = shrink(&img);
    dominant_color_rgba(&small)
}

/// Accumulate every sampled frame of an animated preview into one histogram.
/// Dark fade frames contribute almost nothing thanks to the tone weighting, so
/// the answer reflects the preview at full brightness.
fn dominant_color_gif(path: &Path) -> Option<[f32; 3]> {
    use image::AnimationDecoder;
    let file = std::fs::File::open(path).ok()?;
    let decoder = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file)).ok()?;
    let mut hist = Histogram::default();
    let mut seen = 0usize;
    for (i, frame) in decoder.into_frames().take(MAX_GIF_FRAMES).enumerate() {
        let Ok(frame) = frame else { break };
        // Every third frame is plenty; neighbouring frames are near-identical.
        if i % 3 != 0 {
            continue;
        }
        let img = image::DynamicImage::ImageRgba8(frame.into_buffer());
        hist.add(&shrink(&img));
        seen += 1;
    }
    (seen > 0).then(|| hist.dominant()).flatten()
}

fn shrink(img: &image::DynamicImage) -> RgbaImage {
    img.resize(SAMPLE_EDGE, SAMPLE_EDGE, image::imageops::FilterType::Triangle)
        .to_rgba8()
}

/// Dominant colour of already-decoded pixels (see [`dominant_color`]).
pub fn dominant_color_rgba(img: &RgbaImage) -> Option<[f32; 3]> {
    let mut hist = Histogram::default();
    hist.add(img);
    hist.dominant()
}

/// Hue-binned colour votes, accumulated over one or more frames.
#[derive(Default)]
struct Histogram {
    /// Per hue bin: total weight and weight-scaled RGB sums.
    bin_weight: [f64; BINS],
    bin_rgb: [[f64; 3]; BINS],
    /// Greyscale fallback accumulators (weighted by how mid-tone a pixel is).
    grey_weight: f64,
    grey_rgb: [f64; 3],
}

impl Histogram {
    fn add(&mut self, img: &RgbaImage) {
        let Histogram {
            bin_weight,
            bin_rgb,
            grey_weight,
            grey_rgb,
        } = self;
        for px in img.pixels() {
            let a = px[3] as f32 / 255.0;
            if a < 0.25 {
                continue;
            }
            let r = px[0] as f32 / 255.0;
            let g = px[1] as f32 / 255.0;
            let b = px[2] as f32 / 255.0;
            let (h, s, v) = rgb_to_hsv(r, g, b);
            // Favour mid-tones: near-black and blown-out pixels say little
            // about a wallpaper's colour identity.
            let tone = (-((v - 0.6) * (v - 0.6)) / (2.0 * 0.25 * 0.25)).exp();
            let w = (a * tone) as f64;
            if w <= 0.0 {
                continue;
            }
            if s < GREY_SAT {
                *grey_weight += w;
                grey_rgb[0] += (r as f64) * w;
                grey_rgb[1] += (g as f64) * w;
                grey_rgb[2] += (b as f64) * w;
                continue;
            }
            // Vivid pixels count for more, but not so much that a handful of
            // neon specks beat the actual subject.
            let cw = w * (s as f64).powf(1.5);
            let bin = ((h / 360.0 * BINS as f32) as usize).min(BINS - 1);
            bin_weight[bin] += cw;
            bin_rgb[bin][0] += (r as f64) * cw;
            bin_rgb[bin][1] += (g as f64) * cw;
            bin_rgb[bin][2] += (b as f64) * cw;
        }
    }

    fn dominant(&self) -> Option<[f32; 3]> {
        let (bin_weight, bin_rgb) = (&self.bin_weight, &self.bin_rgb);
        // Merge each bin with its neighbours so a hue straddling a bin edge
        // (very common — skies, skin, foliage) isn't split in half.
        let mut best = None::<(usize, f64)>;
        for i in 0..BINS {
            let l = (i + BINS - 1) % BINS;
            let r = (i + 1) % BINS;
            let score = bin_weight[i] + 0.5 * (bin_weight[l] + bin_weight[r]);
            if bin_weight[i] > 0.0 && best.is_none_or(|(_, s)| score > s) {
                best = Some((i, score));
            }
        }

        let colored_total: f64 = bin_weight.iter().sum();
        let picked = best.and_then(|(i, _)| {
            // A few stray coloured pixels in an otherwise grey image shouldn't
            // hijack the accent.
            if colored_total < 0.02 * (colored_total + self.grey_weight) {
                return None;
            }
            let l = (i + BINS - 1) % BINS;
            let r = (i + 1) % BINS;
            let w = bin_weight[i] + bin_weight[l] + bin_weight[r];
            (w > 0.0).then(|| {
                [
                    ((bin_rgb[i][0] + bin_rgb[l][0] + bin_rgb[r][0]) / w) as f32,
                    ((bin_rgb[i][1] + bin_rgb[l][1] + bin_rgb[r][1]) / w) as f32,
                    ((bin_rgb[i][2] + bin_rgb[l][2] + bin_rgb[r][2]) / w) as f32,
                ]
            })
        });
        let picked = picked.or_else(|| {
            (self.grey_weight > 0.0).then(|| {
                [
                    (self.grey_rgb[0] / self.grey_weight) as f32,
                    (self.grey_rgb[1] / self.grey_weight) as f32,
                    (self.grey_rgb[2] / self.grey_weight) as f32,
                ]
            })
        })?;
        // An all-black preview (or one that never leaves its fade-in) has no
        // accent to give; say so instead of theming the UI black.
        let (_, _, v) = rgb_to_hsv(picked[0], picked[1], picked[2]);
        (v >= MIN_USABLE_VALUE).then_some(picked)
    }
}

// ── colour maths shared by wallstudio's chrome and wallaccent's desktop ─────

/// `#RRGGBB` for an RGB triple in 0..1.
pub fn to_hex(rgb: [f32; 3]) -> String {
    format!(
        "#{:02X}{:02X}{:02X}",
        (rgb[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (rgb[2].clamp(0.0, 1.0) * 255.0).round() as u8,
    )
}

/// Parse `#RRGGBB`, `RRGGBB` or `#RGB`. `None` when it isn't a colour.
pub fn from_hex(s: &str) -> Option<[f32; 3]> {
    let h = s.trim().trim_start_matches('#');
    let dup = |c: char| -> Option<u8> {
        let d = c.to_digit(16)? as u8;
        Some(d * 16 + d)
    };
    let (r, g, b) = match h.len() {
        3 => {
            let mut it = h.chars();
            (dup(it.next()?)?, dup(it.next()?)?, dup(it.next()?)?)
        }
        6 => (
            u8::from_str_radix(&h[0..2], 16).ok()?,
            u8::from_str_radix(&h[2..4], 16).ok()?,
            u8::from_str_radix(&h[4..6], 16).ok()?,
        ),
        _ => return None,
    };
    Some([r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0])
}

/// WCAG relative luminance.
pub fn relative_luminance(rgb: [f32; 3]) -> f32 {
    fn lin(u: f32) -> f32 {
        let u = u.clamp(0.0, 1.0);
        if u <= 0.04045 {
            u / 12.92
        } else {
            ((u + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * lin(rgb[0]) + 0.7152 * lin(rgb[1]) + 0.0722 * lin(rgb[2])
}

/// WCAG contrast ratio between two colours (1.0 – 21.0).
pub fn contrast_ratio(a: [f32; 3], b: [f32; 3]) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Brighten (then desaturate) a hue until it reads against `against`.
///
/// HSV value is a poor proxy for perceived brightness: a fully saturated blue
/// at value 1.0 is dimmer than a mid grey. Deep-blue wallpapers are common, so
/// walk the colour toward white until it clears `min_ratio`.
pub fn fit_contrast(h: f32, s: f32, v: f32, against: [f32; 3], min_ratio: f32) -> (f32, f32) {
    let (mut s, mut v) = (s, v);
    for _ in 0..48 {
        if contrast_ratio(hsv_to_rgb(h, s, v), against) >= min_ratio {
            break;
        }
        if v < 0.98 {
            v = (v + 0.03).min(0.98);
        } else if s > 0.02 {
            s -= 0.04;
        } else {
            break;
        }
    }
    (s.clamp(0.0, 1.0), v.clamp(0.0, 1.0))
}

/// Near-black or near-white — whichever is legible on `bg`.
pub fn readable_on(bg: [f32; 3]) -> [f32; 3] {
    if contrast_ratio(bg, [0.0, 0.0, 0.0]) >= contrast_ratio(bg, [1.0, 1.0, 1.0]) {
        [0.06, 0.06, 0.07]
    } else {
        [0.97, 0.97, 0.97]
    }
}

/// Blend `a` toward `b` by `t` (clamped 0..1).
pub fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        a[0] + (b[0] - a[0]) * t,
        a[1] + (b[1] - a[1]) * t,
        a[2] + (b[2] - a[2]) * t,
    ]
}

/// RGB (0..1) → HSV with hue in degrees 0..360.
pub fn rgb_to_hsv(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d <= f32::EPSILON {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    let s = if max <= f32::EPSILON { 0.0 } else { d / max };
    (h, s, max)
}

/// HSV (hue in degrees) → RGB in 0..1.
pub fn hsv_to_rgb(h: f32, s: f32, v: f32) -> [f32; 3] {
    let h = h.rem_euclid(360.0);
    let s = s.clamp(0.0, 1.0);
    let v = v.clamp(0.0, 1.0);
    let c = v * s;
    let x = c * (1.0 - (((h / 60.0) % 2.0) - 1.0).abs());
    let m = v - c;
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgba, RgbaImage};

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        RgbaImage::from_pixel(w, h, Rgba(c))
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(from_hex("#DBB87A").map(to_hex).as_deref(), Some("#DBB87A"));
        assert_eq!(from_hex("dbb87a").map(to_hex).as_deref(), Some("#DBB87A"));
        assert_eq!(from_hex("#f0a").map(to_hex).as_deref(), Some("#FF00AA"));
        assert_eq!(from_hex("#12345"), None);
        assert_eq!(from_hex("#zzzzzz"), None);
        assert_eq!(from_hex(""), None);
    }

    #[test]
    fn contrast_matches_known_pairs() {
        // Black on white is the WCAG maximum.
        let c = contrast_ratio([0.0, 0.0, 0.0], [1.0, 1.0, 1.0]);
        assert!((c - 21.0).abs() < 0.01, "{c}");
        assert!((contrast_ratio([0.5, 0.5, 0.5], [0.5, 0.5, 0.5]) - 1.0).abs() < 1e-4);
    }

    #[test]
    fn fit_contrast_rescues_dark_hues() {
        let panel = [0.10, 0.10, 0.11];
        // Deep navy: value clamping alone can't make this readable.
        let (s, v) = fit_contrast(240.0, 1.0, 0.2, panel, 4.6);
        let out = hsv_to_rgb(240.0, s, v);
        assert!(contrast_ratio(out, panel) >= 4.6, "{out:?}");
        let (h2, _, _) = rgb_to_hsv(out[0], out[1], out[2]);
        assert!((h2 - 240.0).abs() < 1.0, "hue drifted to {h2}");
        // Something already bright is left alone.
        let (s3, v3) = fit_contrast(50.0, 0.5, 0.9, panel, 4.6);
        assert!((s3 - 0.5).abs() < 1e-6 && (v3 - 0.9).abs() < 1e-6);
    }

    #[test]
    fn readable_on_picks_the_legible_side() {
        assert!(readable_on([0.95, 0.9, 0.2])[0] < 0.2, "dark text on yellow");
        assert!(readable_on([0.1, 0.1, 0.3])[0] > 0.8, "light text on navy");
    }

    #[test]
    fn hsv_roundtrip() {
        for (r, g, b) in [
            (0.2, 0.4, 0.9),
            (0.9, 0.1, 0.1),
            (0.5, 0.5, 0.5),
            (0.0, 0.0, 0.0),
            (1.0, 1.0, 1.0),
            (0.3, 0.7, 0.2),
        ] {
            let (h, s, v) = rgb_to_hsv(r, g, b);
            let back = hsv_to_rgb(h, s, v);
            assert!(
                (back[0] - r).abs() < 1e-4
                    && (back[1] - g).abs() < 1e-4
                    && (back[2] - b).abs() < 1e-4,
                "{r},{g},{b} → {h},{s},{v} → {back:?}"
            );
        }
    }

    #[test]
    fn solid_color_image_returns_that_color() {
        let img = solid(16, 16, [51, 102, 204, 255]);
        let [r, g, b] = dominant_color_rgba(&img).expect("dominant");
        assert!((r - 0.2).abs() < 0.02, "r={r}");
        assert!((g - 0.4).abs() < 0.02, "g={g}");
        assert!((b - 0.8).abs() < 0.02, "b={b}");
    }

    #[test]
    fn vivid_minority_beats_dark_grey_majority() {
        // 90% near-black background, 10% saturated orange subject: the accent
        // people would name is the orange.
        let mut img = solid(20, 10, [10, 10, 12, 255]);
        for y in 0..10 {
            for x in 18..20 {
                img.put_pixel(x, y, Rgba([230, 130, 30, 255]));
            }
        }
        let [r, g, b] = dominant_color_rgba(&img).expect("dominant");
        assert!(r > g && g > b, "expected orange-ish, got {r},{g},{b}");
        assert!(r > 0.6, "expected a bright accent, got r={r}");
    }

    #[test]
    fn greyscale_image_returns_grey() {
        let img = solid(16, 16, [128, 128, 130, 255]);
        let [r, g, b] = dominant_color_rgba(&img).expect("dominant");
        let (_, s, _) = rgb_to_hsv(r, g, b);
        assert!(s < 0.15, "expected grey, got sat {s}");
    }

    #[test]
    fn stray_pixels_do_not_hijack_a_grey_wallpaper() {
        // One neon pixel in 1600 grey ones must not become the theme colour.
        let mut img = solid(40, 40, [140, 140, 140, 255]);
        img.put_pixel(0, 0, Rgba([255, 0, 255, 255]));
        let [r, g, b] = dominant_color_rgba(&img).expect("dominant");
        let (_, s, _) = rgb_to_hsv(r, g, b);
        assert!(s < 0.15, "expected grey, got {r},{g},{b}");
    }

    #[test]
    fn fully_transparent_image_has_no_accent() {
        assert_eq!(dominant_color_rgba(&solid(8, 8, [255, 0, 0, 0])), None);
    }

    #[test]
    fn hue_straddling_a_bin_edge_stays_one_color() {
        // Two hues either side of a 15° bin boundary should merge, not split
        // and lose to an unrelated smaller cluster.
        let mut img = solid(30, 10, [12, 12, 12, 255]);
        for y in 0..10 {
            for x in 0..5 {
                img.put_pixel(x, y, Rgba([220, 60, 60, 255])); // ~0°
            }
            for x in 5..10 {
                img.put_pixel(x, y, Rgba([220, 110, 60, 255])); // ~19°
            }
            for x in 10..14 {
                img.put_pixel(x, y, Rgba([60, 90, 220, 255])); // blue cluster
            }
        }
        let [r, g, b] = dominant_color_rgba(&img).expect("dominant");
        assert!(r > b, "expected the merged warm cluster, got {r},{g},{b}");
    }
}
