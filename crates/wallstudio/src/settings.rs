//! Persistent wallstudio settings (`~/.config/wallstudio/settings.json`).
//!
//! Distinct from `session.json`, which remembers transient browsing state
//! (search text, active filters, selected display). Everything here is a
//! deliberate choice the user made in the Settings tab and expects to survive
//! restarts unchanged.
//!
//! Loading is deliberately lenient: unknown keys are ignored and missing or
//! out-of-range values fall back to the default, so a hand-edited or older
//! file can never stop wallstudio from starting.

use crate::theme::{self, Palette};
use iced::Color;
use serde_json::{json, Value};
use std::path::PathBuf;

/// Where the accent colour comes from when it follows the wallpaper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccentSource {
    /// Wallpaper Engine `schemecolor`, falling back to the preview image.
    #[default]
    Auto,
    /// Only the author-declared `schemecolor`.
    Scheme,
    /// Only the dominant colour of the preview image.
    Preview,
}

impl AccentSource {
    pub const ALL: [AccentSource; 3] = [Self::Auto, Self::Scheme, Self::Preview];

    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Auto (scheme → preview)",
            Self::Scheme => "WE scheme color only",
            Self::Preview => "Preview image colors",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Scheme => "scheme",
            Self::Preview => "preview",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "scheme" => Self::Scheme,
            "preview" => Self::Preview,
            _ => Self::Auto,
        }
    }
}

impl std::fmt::Display for AccentSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Which wallpaper the accent tracks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AccentFollow {
    /// The tile you're browsing — the theme previews as you move.
    #[default]
    Selected,
    /// The wallpaper actually on the desktop.
    Playing,
}

impl AccentFollow {
    pub const ALL: [AccentFollow; 2] = [Self::Selected, Self::Playing];

    pub fn label(self) -> &'static str {
        match self {
            Self::Selected => "Selected wallpaper",
            Self::Playing => "Playing wallpaper",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Playing => "playing",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "playing" => Self::Playing,
            _ => Self::Selected,
        }
    }
}

impl std::fmt::Display for AccentFollow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// How much of the library animates its GIF previews.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewAnim {
    /// Every visible tile animates (prettiest, most CPU).
    #[default]
    All,
    /// Only the selected tile and the detail panel animate.
    Selected,
    /// Static thumbnails everywhere (cheapest).
    Off,
}

impl PreviewAnim {
    pub const ALL: [PreviewAnim; 3] = [Self::All, Self::Selected, Self::Off];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All tiles",
            Self::Selected => "Selected tile only",
            Self::Off => "Off (static)",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Selected => "selected",
            Self::Off => "off",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "selected" => Self::Selected,
            "off" => Self::Off,
            _ => Self::All,
        }
    }
}

impl std::fmt::Display for PreviewAnim {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Video decode resolution cap handed to walld.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VideoCap {
    /// Match the largest connected display (walld's own default).
    #[default]
    Native,
    P1080,
    P1440,
    P2160,
}

impl VideoCap {
    pub const ALL: [VideoCap; 4] = [Self::Native, Self::P1080, Self::P1440, Self::P2160];

    pub fn label(self) -> &'static str {
        match self {
            Self::Native => "Native (match display)",
            Self::P1080 => "Cap 1080p (1920px)",
            Self::P1440 => "Cap 1440p (2560px)",
            Self::P2160 => "Cap 4K (3840px)",
        }
    }

    /// Longest-edge cap in pixels; 0 means "let walld decide".
    pub fn max_edge(self) -> u32 {
        match self {
            Self::Native => 0,
            Self::P1080 => 1920,
            Self::P1440 => 2560,
            Self::P2160 => 3840,
        }
    }

    pub fn from_max_edge(px: u32) -> Self {
        match px {
            1920 => Self::P1080,
            2560 => Self::P1440,
            3840 => Self::P2160,
            _ => Self::Native,
        }
    }
}

impl std::fmt::Display for VideoCap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Theme / chrome settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Appearance {
    /// Derive the UI colour from the wallpaper instead of `fixed_accent`.
    pub dynamic_accent: bool,
    pub source: AccentSource,
    pub follow: AccentFollow,
    /// 0 = neutral greys, 1 = fully tinted chrome.
    pub tint: f32,
    /// Accent surfaces are drawn as gradients.
    pub gradients: bool,
    /// Corner rounding in px.
    pub radius: f32,
    /// Used when `dynamic_accent` is off (or nothing can be derived).
    pub fixed_accent: Color,
}

impl Default for Appearance {
    fn default() -> Self {
        Self {
            dynamic_accent: true,
            source: AccentSource::Auto,
            follow: AccentFollow::Selected,
            tint: 0.55,
            gradients: true,
            radius: 3.0,
            fixed_accent: theme::DEFAULT_ACCENT,
        }
    }
}

/// Render quality / performance knobs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quality {
    /// walld scene frame cap (`scene_fps` in `~/.config/walld/config`).
    pub scene_fps: u32,
    /// Video decode resolution cap (`video_max_edge`).
    pub video_cap: VideoCap,
    /// `--fps` handed to linux-wallpaperengine.
    pub lwe_fps: u32,
    pub preview_anim: PreviewAnim,
    /// Library preview animation rate (also the editor's soft-preview tick).
    pub preview_fps: u32,
    /// Target gallery tile width in px.
    pub tile_size: u32,
}

impl Default for Quality {
    fn default() -> Self {
        Self {
            scene_fps: 60,
            video_cap: VideoCap::Native,
            lwe_fps: 30,
            preview_anim: PreviewAnim::All,
            preview_fps: 12,
            tile_size: 220,
        }
    }
}

/// Behaviour that isn't about looks or speed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behaviour {
    /// Ask twice before unsubscribing or deleting.
    pub confirm_destructive: bool,
    /// Generate the desktop palette (bars, terminal text, KDE/Qt including
    /// Dolphin, GTK, notifications and launcher) from the wallpaper after
    /// playing it, by running `wallaccent`.
    pub desktop_accent: bool,
    /// Fade the recolor over ~0.8s instead of snapping. Mirrored into
    /// wallaccent's own config so walld-triggered recolors fade too.
    pub smooth_transition: bool,
    /// Poll for wallpapers Steam finished downloading while wallstudio is open.
    pub auto_refresh: bool,
    /// Fit mode applied to wallpapers with no saved layout yet.
    pub default_fit: crate::FitModeUi,
}

impl Default for Behaviour {
    fn default() -> Self {
        Self {
            confirm_destructive: true,
            desktop_accent: true,
            smooth_transition: true,
            auto_refresh: true,
            default_fit: crate::FitModeUi::Cover,
        }
    }
}

/// Everything the Settings tab edits.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub appearance: Appearance,
    pub quality: Quality,
    pub behaviour: Behaviour,
}

impl Settings {
    /// Palette for a wallpaper accent, or the fixed accent when dynamic
    /// theming is off / nothing could be derived.
    pub fn palette_for(&self, derived: Option<Color>) -> Palette {
        let a = &self.appearance;
        let base = if a.dynamic_accent {
            derived.unwrap_or(a.fixed_accent)
        } else {
            a.fixed_accent
        };
        Palette::derive(base, a.tint, a.gradients, a.radius)
    }

    /// Milliseconds between preview-animation ticks.
    pub fn anim_interval_ms(&self) -> u64 {
        let fps = self.quality.preview_fps.clamp(1, 60);
        (1000 / fps).max(8) as u64
    }

    pub fn to_json(&self) -> Value {
        let a = &self.appearance;
        let q = &self.quality;
        let b = &self.behaviour;
        json!({
            "appearance": {
                "dynamic_accent": a.dynamic_accent,
                "source": a.source.key(),
                "follow": a.follow.key(),
                "tint": a.tint,
                "gradients": a.gradients,
                "radius": a.radius,
                "fixed_accent": theme::to_hex(a.fixed_accent),
            },
            "quality": {
                "scene_fps": q.scene_fps,
                "video_max_edge": q.video_cap.max_edge(),
                "lwe_fps": q.lwe_fps,
                "preview_anim": q.preview_anim.key(),
                "preview_fps": q.preview_fps,
                "tile_size": q.tile_size,
            },
            "behaviour": {
                "confirm_destructive": b.confirm_destructive,
                "desktop_accent": b.desktop_accent,
                "smooth_transition": b.smooth_transition,
                "auto_refresh": b.auto_refresh,
                "default_fit": b.default_fit.as_str(),
            },
        })
    }

    /// Parse a settings document, filling in defaults for anything missing,
    /// unparsable or out of range.
    pub fn from_json(v: &Value) -> Self {
        let mut s = Settings::default();
        let f32_in = |sect: &Value, key: &str, lo: f32, hi: f32, dflt: f32| -> f32 {
            sect.get(key)
                .and_then(|x| x.as_f64())
                .map(|x| x as f32)
                .filter(|x| x.is_finite())
                .unwrap_or(dflt)
                .clamp(lo, hi)
        };
        let u32_in = |sect: &Value, key: &str, lo: u32, hi: u32, dflt: u32| -> u32 {
            sect.get(key)
                .and_then(|x| x.as_u64())
                .map(|x| x as u32)
                .unwrap_or(dflt)
                .clamp(lo, hi)
        };
        let bool_in = |sect: &Value, key: &str, dflt: bool| -> bool {
            sect.get(key).and_then(|x| x.as_bool()).unwrap_or(dflt)
        };

        if let Some(a) = v.get("appearance") {
            let d = Appearance::default();
            s.appearance = Appearance {
                dynamic_accent: bool_in(a, "dynamic_accent", d.dynamic_accent),
                source: a
                    .get("source")
                    .and_then(|x| x.as_str())
                    .map(AccentSource::parse)
                    .unwrap_or(d.source),
                follow: a
                    .get("follow")
                    .and_then(|x| x.as_str())
                    .map(AccentFollow::parse)
                    .unwrap_or(d.follow),
                tint: f32_in(a, "tint", 0.0, 1.0, d.tint),
                gradients: bool_in(a, "gradients", d.gradients),
                radius: f32_in(a, "radius", 0.0, 16.0, d.radius),
                fixed_accent: a
                    .get("fixed_accent")
                    .and_then(|x| x.as_str())
                    .and_then(theme::from_hex)
                    .unwrap_or(d.fixed_accent),
            };
        }
        if let Some(q) = v.get("quality") {
            let d = Quality::default();
            s.quality = Quality {
                scene_fps: u32_in(q, "scene_fps", 5, 120, d.scene_fps),
                video_cap: VideoCap::from_max_edge(u32_in(q, "video_max_edge", 0, 7680, 0)),
                lwe_fps: u32_in(q, "lwe_fps", 5, 144, d.lwe_fps),
                preview_anim: q
                    .get("preview_anim")
                    .and_then(|x| x.as_str())
                    .map(PreviewAnim::parse)
                    .unwrap_or(d.preview_anim),
                preview_fps: u32_in(q, "preview_fps", 1, 60, d.preview_fps),
                tile_size: u32_in(q, "tile_size", 140, 400, d.tile_size),
            };
        }
        if let Some(b) = v.get("behaviour") {
            let d = Behaviour::default();
            s.behaviour = Behaviour {
                confirm_destructive: bool_in(b, "confirm_destructive", d.confirm_destructive),
                desktop_accent: bool_in(b, "desktop_accent", d.desktop_accent),
                smooth_transition: bool_in(b, "smooth_transition", d.smooth_transition),
                auto_refresh: bool_in(b, "auto_refresh", d.auto_refresh),
                default_fit: b
                    .get("default_fit")
                    .and_then(|x| x.as_str())
                    .map(crate::FitModeUi::parse)
                    .unwrap_or(d.default_fit),
            };
        }
        s
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".config")
        });
    base.join("wallstudio")
}

pub fn settings_path() -> PathBuf {
    config_dir().join("settings.json")
}

pub fn load() -> Settings {
    let Ok(text) = std::fs::read_to_string(settings_path()) else {
        return Settings::default();
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => Settings::from_json(&v),
        Err(e) => {
            log::warn!("settings.json is not valid JSON ({e}); using defaults");
            Settings::default()
        }
    }
}

pub fn save(s: &Settings) -> Result<(), String> {
    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let text = serde_json::to_string_pretty(&s.to_json()).map_err(|e| e.to_string())?;
    let path = settings_path();
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let mut s = Settings::default();
        s.appearance.dynamic_accent = false;
        s.appearance.source = AccentSource::Preview;
        s.appearance.follow = AccentFollow::Playing;
        s.appearance.tint = 0.25;
        s.appearance.gradients = false;
        s.appearance.radius = 7.0;
        s.appearance.fixed_accent = Color::from_rgb8(0x33, 0x66, 0xCC);
        s.quality.scene_fps = 90;
        s.quality.video_cap = VideoCap::P1440;
        s.quality.lwe_fps = 24;
        s.quality.preview_anim = PreviewAnim::Selected;
        s.quality.preview_fps = 20;
        s.quality.tile_size = 300;
        s.behaviour.confirm_destructive = false;
        s.behaviour.desktop_accent = false;
        s.behaviour.smooth_transition = false;
        s.behaviour.auto_refresh = false;
        s.behaviour.default_fit = crate::FitModeUi::Contain;

        let back = Settings::from_json(&s.to_json());
        assert_eq!(back, s);
    }

    #[test]
    fn defaults_round_trip() {
        let d = Settings::default();
        assert_eq!(Settings::from_json(&d.to_json()), d);
    }

    #[test]
    fn empty_or_partial_documents_fall_back_to_defaults() {
        let d = Settings::default();
        assert_eq!(Settings::from_json(&json!({})), d);
        assert_eq!(Settings::from_json(&json!(null)), d);
        let partial = Settings::from_json(&json!({ "quality": { "scene_fps": 30 } }));
        assert_eq!(partial.quality.scene_fps, 30);
        assert_eq!(partial.quality.lwe_fps, d.quality.lwe_fps);
        assert_eq!(partial.appearance, d.appearance);
    }

    #[test]
    fn out_of_range_values_are_clamped_not_rejected() {
        let v = json!({
            "appearance": { "tint": 9.0, "radius": -3.0, "fixed_accent": "not a color" },
            "quality": { "scene_fps": 100000, "preview_fps": 0, "tile_size": 5 },
        });
        let s = Settings::from_json(&v);
        assert_eq!(s.appearance.tint, 1.0);
        assert_eq!(s.appearance.radius, 0.0);
        assert_eq!(s.appearance.fixed_accent, theme::DEFAULT_ACCENT);
        assert_eq!(s.quality.scene_fps, 120);
        assert_eq!(s.quality.preview_fps, 1);
        assert_eq!(s.quality.tile_size, 140);
    }

    #[test]
    fn unknown_enum_keys_fall_back() {
        let s = Settings::from_json(&json!({
            "appearance": { "source": "bogus", "follow": "bogus" },
            "quality": { "preview_anim": "bogus", "video_max_edge": 1234 },
            "behaviour": { "default_fit": "bogus" },
        }));
        assert_eq!(s.appearance.source, AccentSource::Auto);
        assert_eq!(s.appearance.follow, AccentFollow::Selected);
        assert_eq!(s.quality.preview_anim, PreviewAnim::All);
        // An unrecognised pixel cap means "don't cap".
        assert_eq!(s.quality.video_cap, VideoCap::Native);
        assert_eq!(s.behaviour.default_fit, crate::FitModeUi::Cover);
    }

    #[test]
    fn palette_follows_the_dynamic_toggle() {
        let mut s = Settings::default();
        let wallpaper = Color::from_rgb(0.2, 0.4, 0.9);
        let dynamic = s.palette_for(Some(wallpaper));
        assert!(dynamic.accent.b > dynamic.accent.r, "should be blue-ish");

        s.appearance.dynamic_accent = false;
        let fixed = s.palette_for(Some(wallpaper));
        assert_eq!(fixed.accent, s.palette_for(None).accent);
        assert!(fixed.accent.r > fixed.accent.b, "should stay gold");
    }

    #[test]
    fn palette_falls_back_when_nothing_can_be_derived() {
        let s = Settings::default();
        assert_eq!(s.palette_for(None).accent, s.palette_for(None).accent);
        assert!(s.palette_for(None).accent.r > s.palette_for(None).accent.b);
    }

    #[test]
    fn anim_interval_tracks_preview_fps() {
        let mut s = Settings::default();
        s.quality.preview_fps = 12;
        assert_eq!(s.anim_interval_ms(), 83);
        s.quality.preview_fps = 1;
        assert_eq!(s.anim_interval_ms(), 1000);
        s.quality.preview_fps = 60;
        assert_eq!(s.anim_interval_ms(), 16);
    }

    /// The whole point of this module: what you set survives a restart.
    /// Exercises the real `save`/`load` pair against a temp config dir.
    #[test]
    fn saves_and_reloads_from_disk() {
        let dir = std::env::temp_dir().join(format!(
            "wallstudio-settings-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let prev = std::env::var_os("XDG_CONFIG_HOME");
        // Safety: this is the only test that touches XDG_CONFIG_HOME.
        std::env::set_var("XDG_CONFIG_HOME", &dir);

        // Nothing on disk yet → defaults, and no file is conjured up.
        assert_eq!(load(), Settings::default());
        assert!(!settings_path().exists());

        let mut want = Settings::default();
        want.appearance.dynamic_accent = false;
        want.appearance.fixed_accent = Color::from_rgb8(0xFF, 0x3E, 0xA5);
        want.appearance.tint = 1.0;
        want.appearance.gradients = false;
        want.appearance.radius = 12.0;
        want.quality.scene_fps = 45;
        want.quality.video_cap = VideoCap::P1440;
        want.behaviour.default_fit = crate::FitModeUi::Contain;
        save(&want).expect("save");
        assert!(settings_path().starts_with(&dir));
        assert_eq!(load(), want);

        // A corrupt file must not stop wallstudio from starting.
        std::fs::write(settings_path(), "{ not json").unwrap();
        assert_eq!(load(), Settings::default());

        match prev {
            Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
            None => std::env::remove_var("XDG_CONFIG_HOME"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_cap_maps_both_ways() {
        for c in VideoCap::ALL {
            assert_eq!(VideoCap::from_max_edge(c.max_edge()), c);
        }
        assert_eq!(VideoCap::from_max_edge(0), VideoCap::Native);
    }
}
