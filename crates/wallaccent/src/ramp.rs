//! Turn one wallpaper colour into the handful of shades a desktop needs.

use wallengine_we::accent as cmath;
use wallengine_we::{hsv_to_rgb, rgb_to_hsv};

/// The surface most accents sit on across this desktop (near-black bars,
/// terminals and popups). Accents are fitted for contrast against it.
const CHROME: [f32; 3] = [0.06, 0.06, 0.07];

/// Contrast an accent must reach against dark chrome. Matches wallstudio's
/// own threshold so the app and the desktop agree on what "readable" means.
pub const MIN_CONTRAST: f32 = 4.6;

/// How bright a tinted chrome surface is allowed to get. Matched to the
/// themes' own bar backgrounds (`rgba(16, 14, 24)` / `rgba(22, 19, 31)`) so a
/// recolor changes the *hue* of the bar and never its darkness.
const SURFACE_V: f32 = 0.094;
const SURFACE_HI_V: f32 = 0.122;

/// Chroma ceiling for those surfaces. Past roughly this much saturation a
/// near-black stops reading as "dark chrome" and starts reading as a colour.
const SURFACE_S_MAX: f32 = 0.45;

/// Every shade the target writers need, derived from one base colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ramp {
    /// The wallpaper's colour, untouched — reported, never rendered.
    pub base: [f32; 3],
    /// Primary accent: bar highlights, cursor, active borders.
    pub accent: [f32; 3],
    /// Lighter companion: hover, gradient top, urgent.
    pub accent_hi: [f32; 3],
    /// Darker companion: gradient bottom, selection fills.
    pub accent_lo: [f32; 3],
    /// Muted, low-chroma version for inactive borders.
    pub accent_muted: [f32; 3],
    /// Text that stays readable *on* `accent`.
    pub on_accent: [f32; 3],
    /// Bar background: the wallpaper's hue at near-black. Tinted, still dark.
    pub surface: [f32; 3],
    /// The raised companion of `surface` — menus, popups, tooltips.
    pub surface_hi: [f32; 3],
}

impl Ramp {
    /// Derive from a raw wallpaper colour.
    ///
    /// `strength` (0..1) scales how saturated the result is allowed to be;
    /// 1.0 keeps the wallpaper's own chroma, lower values calm it down.
    pub fn derive(base: [f32; 3], strength: f32) -> Ramp {
        let strength = strength.clamp(0.0, 1.0);
        let (h, s, v) = rgb_to_hsv(base[0], base[1], base[2]);

        // Wallpaper colours are routinely near-black, near-white or grey.
        // Pull them into a band that can actually act as an accent.
        let near_grey = s < 0.08;
        let (s0, v0) = if near_grey {
            (0.0, v.clamp(0.62, 0.86))
        } else {
            (s.clamp(0.35, 0.90) * (0.45 + 0.55 * strength), v.clamp(0.62, 0.95))
        };
        // The bar's own background carries the wallpaper's hue now, so it —
        // not a fixed near-black — is what an accent has to be readable on.
        // Deriving it from the base hue (which survives the fit unchanged)
        // keeps that from being circular.
        let s_surface = (s0 * 0.5).min(SURFACE_S_MAX);
        let surface = hsv_to_rgb(h, s_surface, SURFACE_V);
        // A hue can land the tinted surface *darker* than plain chrome; fit
        // against whichever is lighter so both stay legible.
        let against = if cmath::relative_luminance(surface) >= cmath::relative_luminance(CHROME) {
            surface
        } else {
            CHROME
        };
        let (s1, v1) = cmath::fit_contrast(h, s0, v0, against, MIN_CONTRAST);

        let accent = hsv_to_rgb(h, s1, v1);
        Ramp {
            base,
            accent,
            accent_hi: hsv_to_rgb(h, s1 * 0.82, (v1 + 0.12).min(1.0)),
            accent_lo: hsv_to_rgb(h, (s1 * 1.05).min(1.0), v1 * 0.42),
            accent_muted: hsv_to_rgb(h, s1 * 0.30, (v1 * 0.30).max(0.10)),
            on_accent: cmath::readable_on(accent),
            surface,
            surface_hi: hsv_to_rgb(h, s_surface * 0.92, SURFACE_HI_V),
        }
    }

    pub fn hex(c: [f32; 3]) -> String {
        cmath::to_hex(c)
    }

    /// Blend every shade toward `other` by `t` (0 = self, 1 = other).
    /// Powers the smooth recolor: waybar steps through these on its way
    /// from the old accent to the new one.
    pub fn lerp(&self, other: &Ramp, t: f32) -> Ramp {
        fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
            [
                a[0] + (b[0] - a[0]) * t,
                a[1] + (b[1] - a[1]) * t,
                a[2] + (b[2] - a[2]) * t,
            ]
        }
        let t = t.clamp(0.0, 1.0);
        Ramp {
            base: mix(self.base, other.base, t),
            accent: mix(self.accent, other.accent, t),
            accent_hi: mix(self.accent_hi, other.accent_hi, t),
            accent_lo: mix(self.accent_lo, other.accent_lo, t),
            accent_muted: mix(self.accent_muted, other.accent_muted, t),
            on_accent: mix(self.on_accent, other.on_accent, t),
            surface: mix(self.surface, other.surface, t),
            surface_hi: mix(self.surface_hi, other.surface_hi, t),
        }
    }

    /// `rrggbbaa` (no `#`) — Hyprland's `rgba()` and mako both want this.
    pub fn hex8(c: [f32; 3], alpha: f32) -> String {
        format!(
            "{}{:02x}",
            cmath::to_hex(c).trim_start_matches('#').to_lowercase(),
            (alpha.clamp(0.0, 1.0) * 255.0).round() as u8
        )
    }

    /// `r, g, b` decimal channels — for CSS `rgba(...)`.
    pub fn rgb_csv(c: [f32; 3]) -> String {
        format!(
            "{}, {}, {}",
            (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
            (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accent_is_readable_on_dark_chrome() {
        for base in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.078, 0.102, 0.149], // real RDR2 scheme colour
            [0.0, 0.0, 0.6],       // deep blue: the hard case
            [0.5, 0.5, 0.5],
            [1.0, 0.117, 0.235],   // vampire red
        ] {
            for strength in [0.0, 0.5, 1.0] {
                let r = Ramp::derive(base, strength);
                let c = cmath::contrast_ratio(r.accent, CHROME);
                assert!(c >= MIN_CONTRAST, "{base:?}@{strength} → {c}");
                let on = cmath::contrast_ratio(r.on_accent, r.accent);
                assert!(on >= 4.5, "label on {base:?} → {on}");
            }
        }
    }

    /// The bar background may take the wallpaper's colour, but a "dark" bar
    /// that drifts light is worse than one that never tinted at all.
    #[test]
    fn surfaces_stay_dark_and_carry_the_hue() {
        for hue in [0.0, 60.0, 140.0, 210.0, 300.0] {
            for strength in [0.0, 0.5, 1.0] {
                let r = Ramp::derive(hsv_to_rgb(hue, 0.85, 0.55), strength);
                for (name, c) in [("surface", r.surface), ("surface_hi", r.surface_hi)] {
                    let (h, s, v) = rgb_to_hsv(c[0], c[1], c[2]);
                    assert!(v <= SURFACE_HI_V + 1e-3, "{name} @{hue} too light: {v}");
                    assert!(s <= SURFACE_S_MAX + 1e-3, "{name} @{hue} too saturated: {s}");
                    assert!(s > 0.05, "{name} @{hue} lost the tint: {s}");
                    let d = (h - hue).abs().min(360.0 - (h - hue).abs());
                    assert!(d < 2.0, "{name} hue {hue} drifted to {h}");
                }
                // Menus read as raised against the bar behind them.
                let lum = cmath::relative_luminance;
                assert!(lum(r.surface_hi) > lum(r.surface), "menu should be lighter");
            }
        }
    }

    /// The accent sits on the tinted bar now, so that is what it must clear.
    #[test]
    fn accent_is_readable_on_its_own_tinted_surface() {
        for base in [
            [0.0, 0.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.078, 0.102, 0.149],
            [0.0, 0.0, 0.6],
            [0.5, 0.5, 0.5],
            [1.0, 0.117, 0.235],
        ] {
            for strength in [0.0, 0.5, 1.0] {
                let r = Ramp::derive(base, strength);
                let c = cmath::contrast_ratio(r.accent, r.surface);
                assert!(c >= MIN_CONTRAST, "{base:?}@{strength} on surface → {c}");
            }
        }
    }

    #[test]
    fn grey_wallpapers_leave_the_bar_neutral() {
        let r = Ramp::derive([0.5, 0.5, 0.5], 1.0);
        let (_, s, v) = rgb_to_hsv(r.surface[0], r.surface[1], r.surface[2]);
        assert!(s < 0.02, "grey gained a tint: {s}");
        assert!(v <= SURFACE_V + 1e-3, "grey bar drifted light: {v}");
    }

    #[test]
    fn hue_survives_derivation() {
        for hue in [0.0, 60.0, 140.0, 210.0, 300.0] {
            let base = hsv_to_rgb(hue, 0.8, 0.5);
            let r = Ramp::derive(base, 1.0);
            let (h2, _, _) = rgb_to_hsv(r.accent[0], r.accent[1], r.accent[2]);
            let d = (h2 - hue).abs().min(360.0 - (h2 - hue).abs());
            assert!(d < 2.0, "hue {hue} drifted to {h2}");
        }
    }

    #[test]
    fn companions_are_ordered_by_brightness() {
        let r = Ramp::derive([0.9, 0.2, 0.3], 1.0);
        let lum = cmath::relative_luminance;
        assert!(lum(r.accent_hi) > lum(r.accent), "hi should be lighter");
        assert!(lum(r.accent_lo) < lum(r.accent), "lo should be darker");
        assert!(lum(r.accent_muted) < lum(r.accent_lo) + 0.05);
    }

    #[test]
    fn grey_wallpapers_stay_grey() {
        let r = Ramp::derive([0.5, 0.5, 0.5], 1.0);
        let (_, s, _) = rgb_to_hsv(r.accent[0], r.accent[1], r.accent[2]);
        assert!(s < 0.02, "grey gained chroma: {s}");
    }

    #[test]
    fn strength_calms_saturation() {
        let vivid = Ramp::derive([1.0, 0.0, 0.0], 1.0);
        let calm = Ramp::derive([1.0, 0.0, 0.0], 0.0);
        let sat = |c: [f32; 3]| rgb_to_hsv(c[0], c[1], c[2]).1;
        let (a, b) = (sat(calm.accent), sat(vivid.accent));
        assert!(a < b, "calm {a} should be less saturated than vivid {b}");
    }

    #[test]
    fn formatters_are_well_formed() {
        let c = [1.0, 0.0, 0.5];
        assert_eq!(Ramp::hex(c), "#FF0080");
        assert_eq!(Ramp::hex8(c, 1.0), "ff0080ff");
        assert_eq!(Ramp::hex8(c, 0.0), "ff008000");
        assert_eq!(Ramp::rgb_csv(c), "255, 0, 128");
    }

    #[test]
    fn lerp_walks_the_whole_ramp() {
        let a = Ramp::derive([1.0, 0.0, 0.0], 1.0);
        let b = Ramp::derive([0.0, 0.2, 1.0], 1.0);
        let m = a.lerp(&b, 0.5);
        for (x, (p, q)) in m.accent.iter().zip(a.accent.iter().zip(b.accent.iter())) {
            assert!((x - (p + q) / 2.0).abs() < 1e-6);
        }
        // Endpoints are exact; nothing runs past either end.
        let s = a.lerp(&b, 0.0);
        assert_eq!(s.accent, a.accent);
        let e = a.lerp(&b, 1.0);
        assert_eq!(e.accent, b.accent);
    }
}
