//! WE per-object color blend modes — CPU port of the official
//! `assets/shaders/common_blending.h` `ApplyBlending` (A = backdrop, B = layer,
//! opacity = layer alpha). Used by the software harnesses; the GL path maps the
//! same modes onto fixed-function blending in walld's renderer.

/// Modes walld's GL renderer can express with fixed-function blending.
/// Layers using other modes must be skipped in the GL path — alpha-blending
/// them as normal draws an opaque sheet over the scene (e.g. day/night
/// Overlay filters), which is far worse than omitting the tint.
pub fn gl_blend_mode_supported(mode: i32) -> bool {
    matches!(mode, 0 | 1 | 2 | 4 | 5 | 6 | 7 | 9 | 10 | 20 | 31)
}

fn soft_light(a: f32, b: f32) -> f32 {
    if b < 0.5 {
        2.0 * a * b + a * a * (1.0 - 2.0 * b)
    } else {
        a.sqrt() * (2.0 * b - 1.0) + 2.0 * a * (1.0 - b)
    }
}

fn overlay(a: f32, b: f32) -> f32 {
    if a < 0.5 {
        2.0 * a * b
    } else {
        1.0 - 2.0 * (1.0 - a) * (1.0 - b)
    }
}

fn color_dodge(a: f32, b: f32) -> f32 {
    if b >= 1.0 { 1.0 } else { (a / (1.0 - b)).min(1.0) }
}

fn color_burn(a: f32, b: f32) -> f32 {
    if b <= 0.0 { 0.0 } else { (1.0 - (1.0 - a) / b).max(0.0) }
}

/// `mix(A, Blend(A,B), opacity)` per channel. Unknown modes fall back to normal.
pub fn apply_color_blend(mode: i32, base: [f32; 3], blend: [f32; 3], opacity: f32) -> [f32; 3] {
    let o = opacity.clamp(0.0, 1.0);
    let mut out = [0.0f32; 3];
    for i in 0..3 {
        let a = base[i];
        let b = blend[i];
        let v = match mode {
            1 => a.min(b),                          // Darken
            2 => a * b,                             // Multiply
            3 => color_burn(a, b),                  // Color burn
            4 | 20 => (a + b - 1.0).max(0.0),       // Subtract (linear burn)
            5 => a.min(b),                          // min
            6 => a.max(b),                          // Lighten
            7 => 1.0 - (1.0 - a) * (1.0 - b),       // Screen
            8 => color_dodge(a, b),                 // Color dodge
            9 | 31 => (a + b).min(1.0),             // Add (linear dodge)
            10 => a.max(b),                         // max
            11 => overlay(a, b),                    // Overlay
            12 => soft_light(a, b),                 // Soft light
            13 => overlay(b, a),                    // Hard light
            18 => (a - b).abs(),                    // Difference
            19 => a + b - 2.0 * a * b,              // Exclusion
            24 => (a + b) * 0.5,                    // Average
            25 => 1.0 - (1.0 - a - b).abs(),        // Negation
            32 => a + a * b,                        // a*(1+b)
            _ => b,                                 // Normal
        };
        // Modes 5/10 in the official table ignore opacity (plain min/max).
        out[i] = if mode == 5 || mode == 10 {
            v
        } else {
            (a + (v - a) * o).clamp(0.0, 1.0)
        };
    }
    out
}
