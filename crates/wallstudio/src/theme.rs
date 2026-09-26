//! wallstudio's colour system.
//!
//! Every chrome colour is derived from a single accent. That accent is either
//! fixed (the classic gold) or taken from the wallpaper you're looking at —
//! Wallpaper Engine's `schemecolor`, falling back to the dominant colour of
//! the preview image. See [`crate::settings::Appearance`].
//!
//! The active palette is process-global: iced style closures are `Fn` values
//! evaluated at draw time and can't borrow `App`, so threading a palette
//! through every one of them would mean touching several hundred call sites
//! for no behavioural gain. `set_active` is called from `App::update`, reads
//! happen on the same UI thread while drawing.

use iced::{Background, Color, Gradient};
use std::sync::RwLock;
use wallengine_we::accent as cmath;
use wallengine_we::{hsv_to_rgb, rgb_to_hsv};

/// A full set of chrome colours derived from one accent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg: Color,
    pub panel: Color,
    pub panel2: Color,
    pub line: Color,
    pub fg: Color,
    pub dim: Color,
    pub mute: Color,
    /// Primary accent — buttons, active tabs, highlights.
    pub accent: Color,
    /// Hover / lighter end of the accent gradient.
    pub accent_hi: Color,
    /// Darker end of the accent gradient; also borders on active chips.
    pub accent_lo: Color,
    /// Tinted fill behind selected rows and tiles.
    pub accent_bg: Color,
    /// Semantic colours — never follow the wallpaper.
    pub ok: Color,
    pub err: Color,
    /// Editor selection fill / outline.
    pub sel: Color,
    pub sel_border: Color,
    /// Draw accent surfaces as gradients instead of flat fills.
    pub gradients: bool,
    /// Corner radius for buttons, chips and panels.
    pub radius: f32,
}

/// The untinted greys wallstudio has always used.
const NEUTRAL_BG: Color = Color::from_rgb(0.07, 0.07, 0.08);
const NEUTRAL_PANEL: Color = Color::from_rgb(0.10, 0.10, 0.11);
const NEUTRAL_PANEL2: Color = Color::from_rgb(0.12, 0.12, 0.13);
const NEUTRAL_LINE: Color = Color::from_rgb(0.20, 0.20, 0.22);
const NEUTRAL_FG: Color = Color::from_rgb(0.90, 0.90, 0.88);
const NEUTRAL_DIM: Color = Color::from_rgb(0.52, 0.52, 0.50);
const NEUTRAL_MUTE: Color = Color::from_rgb(0.38, 0.38, 0.36);

/// wallstudio's original accent (Wallpaper Engine gold).
///
/// Written as 8-bit channels because settings round-trip through `#RRGGBB`;
/// an exactly-representable value reloads as the same colour it was saved as.
pub const DEFAULT_ACCENT: Color = Color::from_rgb8(0xDB, 0xB8, 0x7A);

const OK: Color = Color::from_rgb(0.55, 0.72, 0.50);
const ERR: Color = Color::from_rgb(0.82, 0.38, 0.35);

impl Palette {
    /// Classic wallstudio look: gold accent, untinted greys, no gradients.
    pub const NEUTRAL: Palette = Palette {
        bg: NEUTRAL_BG,
        panel: NEUTRAL_PANEL,
        panel2: NEUTRAL_PANEL2,
        line: NEUTRAL_LINE,
        fg: NEUTRAL_FG,
        dim: NEUTRAL_DIM,
        mute: NEUTRAL_MUTE,
        accent: DEFAULT_ACCENT,
        accent_hi: Color::from_rgb(0.92, 0.80, 0.55),
        accent_lo: Color::from_rgb(0.40, 0.32, 0.18),
        accent_bg: Color::from_rgb(0.12, 0.11, 0.09),
        ok: OK,
        err: ERR,
        sel: Color::from_rgb(0.18, 0.22, 0.32),
        sel_border: Color::from_rgb(0.35, 0.55, 0.95),
        gradients: false,
        radius: 0.0,
    };

    /// Derive a full palette from one base colour.
    ///
    /// * `tint` (0..1) — how far the greys drift toward the accent's hue.
    ///   0 keeps wallstudio's original neutral chrome.
    /// * `gradients` — accent surfaces become two-stop gradients.
    /// * `radius` — corner rounding for buttons, chips and panels.
    pub fn derive(base: Color, tint: f32, gradients: bool, radius: f32) -> Palette {
        let tint = tint.clamp(0.0, 1.0);
        let (h, s, v) = rgb_to_hsv(base.r, base.g, base.b);

        // Wallpaper scheme colours are frequently near-black, near-white or
        // fully desaturated. Pull them into a band that stays legible against
        // dark chrome without losing the wallpaper's identity.
        let near_grey = s < 0.08;
        let (sa, va) = if near_grey {
            (0.0, v.clamp(0.62, 0.86))
        } else {
            (s.clamp(0.35, 0.85), v.clamp(0.62, 0.95))
        };
        let (sa, va) = fit_for_contrast(h, sa, va);
        let accent = from_hsv(h, sa, va);
        let accent_hi = from_hsv(h, sa * 0.86, (va + 0.10).min(1.0));
        let accent_lo = from_hsv(h, sa * 0.92, va * 0.45);

        // Tint target: same hue, but as dark as the chrome it replaces, so
        // panels gain colour without gaining brightness.
        let deep = from_hsv(h, if near_grey { 0.0 } else { sa * 0.55 }, 0.14);

        let bg = mix(NEUTRAL_BG, deep, 0.70 * tint);
        let panel = mix(NEUTRAL_PANEL, deep, 0.60 * tint);
        let panel2 = mix(NEUTRAL_PANEL2, lighten(deep, 0.06), 0.60 * tint);
        let line = mix(NEUTRAL_LINE, from_hsv(h, sa * 0.45, 0.30), 0.55 * tint);
        let fg = mix(NEUTRAL_FG, from_hsv(h, sa * 0.12, 0.95), 0.55 * tint);
        let dim = mix(NEUTRAL_DIM, from_hsv(h, sa * 0.20, 0.56), 0.50 * tint);
        let mute = mix(NEUTRAL_MUTE, from_hsv(h, sa * 0.22, 0.42), 0.50 * tint);
        // Selection fill always carries the accent, even at tint 0 — that's
        // what "selected" reads as in the original theme too.
        let accent_bg = mix(
            Palette::NEUTRAL.accent_bg,
            from_hsv(h, if near_grey { 0.0 } else { sa * 0.70 }, 0.20),
            0.65 + 0.35 * tint,
        );

        Palette {
            bg,
            panel,
            panel2,
            line,
            fg,
            dim,
            mute,
            accent,
            accent_hi,
            accent_lo,
            accent_bg,
            ok: OK,
            err: ERR,
            sel: mix(Palette::NEUTRAL.sel, deep, 0.5 * tint),
            sel_border: Palette::NEUTRAL.sel_border,
            gradients,
            radius: radius.clamp(0.0, 16.0),
        }
    }

    /// Accent fill for primary buttons — a gradient when enabled.
    pub fn accent_fill(&self) -> Background {
        self.gradient(self.accent_hi, self.accent_lo_mid(), self.accent)
    }

    /// Brighter variant of [`Palette::accent_fill`] for hover / pressed.
    pub fn accent_fill_hover(&self) -> Background {
        self.gradient(
            lighten(self.accent_hi, 0.06),
            lighten(self.accent, 0.06),
            lighten(self.accent, 0.06),
        )
    }

    /// Subtle tinted fill behind selected tiles and rows.
    pub fn accent_bg_fill(&self) -> Background {
        self.gradient(self.accent_bg, self.panel2, self.accent_bg)
    }

    /// Panel wash — a barely-there vertical gradient when enabled.
    pub fn panel_fill(&self, base: Color) -> Background {
        if !self.gradients {
            return Background::Color(base);
        }
        self.gradient(lighten(base, 0.018), base, base)
    }

    /// Midpoint between accent and its dark end — keeps button gradients from
    /// going muddy at the bottom.
    fn accent_lo_mid(&self) -> Color {
        mix(self.accent, self.accent_lo, 0.45)
    }

    /// Two-stop vertical gradient, or `flat` when gradients are disabled.
    fn gradient(&self, top: Color, bottom: Color, flat: Color) -> Background {
        if !self.gradients {
            return Background::Color(flat);
        }
        Background::Gradient(Gradient::Linear(
            iced::gradient::Linear::new(std::f32::consts::FRAC_PI_2)
                .add_stop(0.0, top)
                .add_stop(1.0, bottom),
        ))
    }
}

impl Default for Palette {
    fn default() -> Self {
        Palette::NEUTRAL
    }
}

/// Linear interpolation between two colours (`t` clamped to 0..1).
pub fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    Color::from_rgb(
        a.r + (b.r - a.r) * t,
        a.g + (b.g - a.g) * t,
        a.b + (b.b - a.b) * t,
    )
}

/// Push a colour toward white by `k`.
pub fn lighten(c: Color, k: f32) -> Color {
    mix(c, Color::WHITE, k.clamp(0.0, 1.0))
}

/// Contrast an accent must reach against the panel behind it.
///
/// 4.6 rather than the 3:1 minimum for large text: below roughly this level an
/// accent lands in a luminance dead zone where *neither* a dark nor a light
/// label on top of it reaches 4.5:1, and accents are button fills here.
const MIN_ACCENT_CONTRAST: f32 = 4.6;

/// Brighten (then desaturate) a hue until it's legible on dark chrome.
fn fit_for_contrast(h: f32, s: f32, v: f32) -> (f32, f32) {
    let panel = [NEUTRAL_PANEL.r, NEUTRAL_PANEL.g, NEUTRAL_PANEL.b];
    cmath::fit_contrast(h, s, v, panel, MIN_ACCENT_CONTRAST)
}

fn from_hsv(h: f32, s: f32, v: f32) -> Color {
    let [r, g, b] = hsv_to_rgb(h, s, v);
    Color::from_rgb(r, g, b)
}

/// Near-black or near-white — whichever is readable on `bg`.
pub fn on_color(bg: Color) -> Color {
    let [r, g, b] = cmath::readable_on([bg.r, bg.g, bg.b]);
    Color::from_rgb(r, g, b)
}

/// `#RRGGBB` for a colour.
pub fn to_hex(c: Color) -> String {
    cmath::to_hex([c.r, c.g, c.b])
}

/// Parse `#RRGGBB` / `RRGGBB` / `#RGB`. `None` when it isn't a colour.
pub fn from_hex(s: &str) -> Option<Color> {
    cmath::from_hex(s).map(|[r, g, b]| Color::from_rgb(r, g, b))
}

static ACTIVE: RwLock<Palette> = RwLock::new(Palette::NEUTRAL);

/// Install the palette used by every style closure from now on.
pub fn set_active(p: Palette) {
    match ACTIVE.write() {
        Ok(mut slot) => *slot = p,
        // A panic in a style closure must not permanently freeze the theme.
        Err(poisoned) => *poisoned.into_inner() = p,
    }
}

/// The palette in force. Cheap: `Palette` is `Copy`.
pub fn active() -> Palette {
    match ACTIVE.read() {
        Ok(p) => *p,
        Err(poisoned) => *poisoned.into_inner(),
    }
}

/// Colour accessors for view code. Thin wrappers over [`active`] so call sites
/// read like the constants they replaced (`pal::accent()`).
pub mod pal {
    use super::{active, Palette};
    use iced::{Background, Color};

    pub fn get() -> Palette {
        active()
    }
    pub fn bg() -> Color {
        active().bg
    }
    pub fn panel() -> Color {
        active().panel
    }
    pub fn panel2() -> Color {
        active().panel2
    }
    pub fn line() -> Color {
        active().line
    }
    pub fn fg() -> Color {
        active().fg
    }
    pub fn dim() -> Color {
        active().dim
    }
    pub fn mute() -> Color {
        active().mute
    }
    pub fn accent() -> Color {
        active().accent
    }
    pub fn accent_hi() -> Color {
        active().accent_hi
    }
    pub fn accent_lo() -> Color {
        active().accent_lo
    }
    pub fn accent_bg() -> Color {
        active().accent_bg
    }
    pub fn ok() -> Color {
        active().ok
    }
    pub fn err() -> Color {
        active().err
    }
    pub fn sel() -> Color {
        active().sel
    }
    pub fn sel_border() -> Color {
        active().sel_border
    }
    /// Text colour that stays readable on top of the accent.
    pub fn on_accent() -> Color {
        super::on_color(active().accent)
    }
    pub fn radius() -> f32 {
        active().radius
    }
    pub fn accent_fill() -> Background {
        active().accent_fill()
    }
    pub fn accent_fill_hover() -> Background {
        active().accent_fill_hover()
    }
    pub fn accent_bg_fill() -> Background {
        active().accent_bg_fill()
    }
    pub fn panel_fill(base: Color) -> Background {
        active().panel_fill(base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_tint_reproduces_the_classic_greys() {
        let p = Palette::derive(DEFAULT_ACCENT, 0.0, false, 0.0);
        assert_eq!(p.bg, NEUTRAL_BG);
        assert_eq!(p.panel, NEUTRAL_PANEL);
        assert_eq!(p.panel2, NEUTRAL_PANEL2);
        assert_eq!(p.line, NEUTRAL_LINE);
        assert_eq!(p.fg, NEUTRAL_FG);
        // Gold is already inside the legibility band, so it survives intact.
        for (a, b) in [
            (p.accent.r, DEFAULT_ACCENT.r),
            (p.accent.g, DEFAULT_ACCENT.g),
            (p.accent.b, DEFAULT_ACCENT.b),
        ] {
            assert!((a - b).abs() < 1e-3, "{a} vs {b}");
        }
    }

    #[test]
    fn accent_is_always_readable_on_chrome() {
        // Every extreme a wallpaper can throw at us must still produce an
        // accent that can be seen against the panel behind it.
        for base in [
            Color::BLACK,
            Color::WHITE,
            Color::from_rgb(0.02, 0.02, 0.05),
            Color::from_rgb(0.078, 0.102, 0.149), // real RDR2 scheme colour
            Color::from_rgb(0.0, 0.0, 0.6),
            Color::from_rgb(0.5, 0.5, 0.5),
            Color::from_rgb(1.0, 0.0, 0.0),
        ] {
            for tint in [0.0, 0.5, 1.0] {
                let p = Palette::derive(base, tint, true, 4.0);
                let c = p.accent.relative_contrast(p.panel);
                assert!(c >= 3.0, "accent {base:?} tint {tint} → contrast {c}");
                let on = on_color(p.accent).relative_contrast(p.accent);
                assert!(on >= 4.5, "label on accent {base:?} → contrast {on}");
            }
        }
    }

    #[test]
    fn chrome_stays_dark_at_full_tint() {
        for base in [
            Color::WHITE,
            Color::from_rgb(1.0, 1.0, 0.0),
            Color::from_rgb(0.0, 1.0, 1.0),
        ] {
            let p = Palette::derive(base, 1.0, true, 0.0);
            assert!(
                p.bg.relative_luminance() < 0.05,
                "bg too bright for {base:?}"
            );
            assert!(
                p.panel.relative_luminance() < 0.06,
                "panel too bright for {base:?}"
            );
            // Body text must still stand out from the panel it sits on.
            assert!(p.fg.relative_contrast(p.panel) >= 7.0);
            assert!(p.dim.relative_contrast(p.panel) >= 2.5);
        }
    }

    #[test]
    fn tint_actually_shifts_the_greys() {
        let blue = Color::from_rgb(0.2, 0.4, 0.9);
        let p = Palette::derive(blue, 1.0, false, 0.0);
        assert!(
            p.panel.b > p.panel.r,
            "panel should lean blue: {:?}",
            p.panel
        );
        assert!(p.line.b > p.line.r);
        let flat = Palette::derive(blue, 0.0, false, 0.0);
        assert_eq!(flat.panel, NEUTRAL_PANEL);
    }

    #[test]
    fn near_grey_scheme_colors_stay_grey() {
        let p = Palette::derive(Color::from_rgb(0.5, 0.5, 0.5), 1.0, false, 0.0);
        let (_, s, _) = rgb_to_hsv(p.accent.r, p.accent.g, p.accent.b);
        assert!(s < 0.02, "grey accent gained saturation: {s}");
        assert!(p.accent.relative_contrast(p.panel) >= 3.0);
    }

    #[test]
    fn gradients_toggle_between_fills() {
        let flat = Palette::derive(DEFAULT_ACCENT, 0.5, false, 0.0);
        assert!(matches!(flat.accent_fill(), Background::Color(_)));
        let grad = Palette::derive(DEFAULT_ACCENT, 0.5, true, 0.0);
        assert!(matches!(grad.accent_fill(), Background::Gradient(_)));
        assert!(matches!(
            grad.panel_fill(grad.panel),
            Background::Gradient(_)
        ));
        assert!(matches!(flat.panel_fill(flat.panel), Background::Color(_)));
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(from_hex("#DBB87A").map(to_hex).as_deref(), Some("#DBB87A"));
        assert_eq!(from_hex("dbb87a").map(to_hex).as_deref(), Some("#DBB87A"));
        assert_eq!(from_hex("#f0a").map(to_hex).as_deref(), Some("#FF00AA"));
        assert_eq!(from_hex(""), None);
        assert_eq!(from_hex("#12345"), None);
        assert_eq!(from_hex("#zzzzzz"), None);
    }

    #[test]
    fn radius_is_clamped() {
        assert_eq!(
            Palette::derive(DEFAULT_ACCENT, 0.0, false, -5.0).radius,
            0.0
        );
        assert_eq!(
            Palette::derive(DEFAULT_ACCENT, 0.0, false, 99.0).radius,
            16.0
        );
    }

    #[test]
    fn active_palette_is_installable() {
        let blue = Palette::derive(Color::from_rgb(0.2, 0.4, 0.9), 0.8, true, 3.0);
        set_active(blue);
        assert_eq!(pal::accent(), blue.accent);
        assert_eq!(pal::radius(), 3.0);
        set_active(Palette::NEUTRAL);
        assert_eq!(pal::accent(), DEFAULT_ACCENT);
    }
}
