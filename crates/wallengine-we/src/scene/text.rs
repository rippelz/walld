//! WE text layers: glyph rasterization + local-time helpers.
//!
//! Text *content* comes from the scene's own SceneScripts, executed by
//! `scene::script`. Nothing here interprets a particular wallpaper's scripts.

use crate::tex::{DecodedTex, TexFormat};
use ab_glyph::{Font, FontVec, Glyph, ScaleFont};

/// Text content for a layer. WE drives text through SceneScripts, which the
/// script host executes for real; the engine only needs to carry a static
/// literal for the unscripted case.
#[derive(Debug, Clone, PartialEq)]
pub enum TextKind {
    Static(String),
}

/// A text layer is drawable when it has a script (the host supplies the string
/// each tick) or a non-empty literal.
pub fn classify(literal: Option<&str>, script: Option<&str>, _name: &str) -> Option<TextKind> {
    if script.is_some() {
        return Some(TextKind::Static(String::new()));
    }
    literal
        .filter(|s| !s.is_empty())
        .map(|s| TextKind::Static(s.to_string()))
}

/// Local wall-clock time (year, month 1-12, day, hour, min, sec, weekday 0=Sun).
pub fn local_now() -> (i32, u32, u32, u32, u32, u32, u32) {
    unsafe {
        let t = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        (
            tm.tm_year + 1900,
            (tm.tm_mon + 1) as u32,
            tm.tm_mday as u32,
            tm.tm_hour as u32,
            tm.tm_min as u32,
            tm.tm_sec as u32,
            tm.tm_wday as u32,
        )
    }
}

/// Fraction of the local day [0,1) — the `engine.timeOfDay` value.
pub fn time_of_day() -> f32 {
    let (_, _, _, h, m, s, _) = local_now();
    (h * 3600 + m * 60 + s) as f32 / 86400.0
}

/// Current string for a text kind.
pub fn render_string(kind: &TextKind) -> String {
    match kind {
        TextKind::Static(s) => s.clone(),
    }
}

/// Rasterize a single line into a tightly-cropped RGBA texture.
/// `color` is 0-1 RGB; glyph coverage becomes alpha.
pub fn rasterize(font: &FontVec, text: &str, px: f32, color: [f32; 3]) -> Option<DecodedTex> {
    if text.is_empty() {
        return None;
    }
    let scaled = font.as_scaled(px.max(4.0));
    // Layout glyphs on a baseline.
    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut caret = 0.0f32;
    let mut last: Option<ab_glyph::GlyphId> = None;
    for ch in text.chars() {
        if ch == '\n' {
            continue;
        }
        let id = scaled.glyph_id(ch);
        if let Some(prev) = last {
            caret += scaled.kern(prev, id);
        }
        let g = id.with_scale_and_position(px.max(4.0), ab_glyph::point(caret, scaled.ascent()));
        caret += scaled.h_advance(id);
        glyphs.push(g);
        last = Some(id);
    }
    let height = (scaled.ascent() - scaled.descent()).ceil().max(1.0);
    let width = caret.ceil().max(1.0);
    let (w, h) = (width as usize + 4, height as usize + 4);
    let mut buf = vec![0u8; w * h * 4];
    let (r, g, b) = (
        (color[0].clamp(0.0, 1.0) * 255.0) as u8,
        (color[1].clamp(0.0, 1.0) * 255.0) as u8,
        (color[2].clamp(0.0, 1.0) * 255.0) as u8,
    );
    for glyph in glyphs {
        if let Some(outlined) = font.outline_glyph(glyph) {
            let bounds = outlined.px_bounds();
            outlined.draw(|gx, gy, cov| {
                let x = bounds.min.x as i32 + gx as i32 + 2;
                let y = bounds.min.y as i32 + gy as i32 + 2;
                if x < 0 || y < 0 || x >= w as i32 || y >= h as i32 {
                    return;
                }
                let i = (y as usize * w + x as usize) * 4;
                let a = (cov * 255.0) as u8;
                if a > buf[i + 3] {
                    buf[i] = r;
                    buf[i + 1] = g;
                    buf[i + 2] = b;
                    buf[i + 3] = a;
                }
            });
        }
    }
    Some(DecodedTex {
        width: w as u32,
        height: h as u32,
        content_width: w as u32,
        content_height: h as u32,
        texture_width: w as u32,
        texture_height: h as u32,
        format: TexFormat::Argb8888,
        flags: 0,
        free_image: None,
        rgba: buf,
        frames: Vec::new(),
        frame_times: Vec::new(),
        video_path: None,
    })
}
