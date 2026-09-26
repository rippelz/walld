//! Config: global walld options + per-monitor wallpapers.
//!
//! Two files:
//!   ~/.config/walld/config          — walld's own options (transition, wipe_ms, ...)
//!   ~/.config/hypr/hyprpaper.conf   — per-monitor wallpaper{} blocks, hyprpaper-compatible,
//!                                     so the existing `theme` merge keeps working unchanged.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FitMode {
    Cover,
    Contain,
    Fill,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transition {
    Snap,
    Wipe,
}

#[derive(Clone, Debug)]
pub struct WallpaperCfg {
    pub monitor: String,
    pub path: PathBuf,
    pub fit: FitMode,
}

#[derive(Clone, Debug)]
pub struct WalldConfig {
    pub transition: Transition,
    pub wipe_ms: u32,
    /// Feather half-width of the wipe edge, in screen pixels.
    pub wipe_feather_px: f32,
    pub hyprpaper_conf: PathBuf,
    /// Optional scene file (wallpaper engine mode). When set, overrides per-monitor images.
    pub scene: Option<PathBuf>,
    /// Max FPS for animated scenes.
    pub scene_fps: u32,
    /// Longest-edge cap for decoded video textures, in pixels.
    /// 0 = follow the largest connected display (the default).
    pub video_max_edge: u32,
    /// Recolor the desktop (wallaccent) whenever the visible wallpaper changes.
    /// Default on: the point is that the accent follows every switch, live.
    pub accent_on_change: bool,
}

impl Default for WalldConfig {
    fn default() -> Self {
        WalldConfig {
            transition: Transition::Wipe,
            wipe_ms: 480,
            wipe_feather_px: 80.0,
            hyprpaper_conf: default_hyprpaper_conf(),
            scene: None,
            // 60 matches typical WE video sources; particles still fine at 60.
            scene_fps: 60,
            video_max_edge: 0,
            accent_on_change: true,
        }
    }
}

pub fn default_hyprpaper_conf() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
    PathBuf::from(home).join(".config/hypr/hyprpaper.conf")
}

pub fn walld_conf_path() -> PathBuf {
    match std::env::var("XDG_CONFIG_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d).join("walld/config"),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
            PathBuf::from(home).join(".config/walld/config")
        }
    }
}

fn expand(path: &str) -> PathBuf {
    let p = path.trim();
    if let Some(rest) = p.strip_prefix("~/") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

/// Parse a hyprpaper-format config into wallpaper entries (later blocks win).
pub fn read_hyprpaper_conf(path: &Path) -> Vec<WallpaperCfg> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("config: cannot read {}: {e}", path.display());
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    let mut monitor: Option<String> = None;
    let mut wpath: Option<PathBuf> = None;
    let mut fit = FitMode::Cover;

    let flush = |out: &mut Vec<WallpaperCfg>, monitor: &mut Option<String>, wpath: &mut Option<PathBuf>, fit: &mut FitMode| {
        if let (Some(m), Some(p)) = (monitor.take(), wpath.take()) {
            out.push(WallpaperCfg { monitor: m, path: p, fit: *fit });
        }
        *fit = FitMode::Cover;
    };

    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line == "wallpaper" || line == "wallpaper {" || line == "{" {
            flush(&mut out, &mut monitor, &mut wpath, &mut fit);
            continue;
        }
        if line == "}" {
            flush(&mut out, &mut monitor, &mut wpath, &mut fit);
            continue;
        }
        if line.starts_with("preload") {
            // hyprpaper preload= lines: nothing for walld to do (it caches on demand).
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            let k = k.trim();
            let v = v.trim();
            match k {
                "monitor" => monitor = Some(v.to_string()),
                "path" => wpath = Some(expand(v)),
                "fit_mode" => {
                    fit = match v {
                        "contain" => FitMode::Contain,
                        "fill" => FitMode::Fill,
                        _ => FitMode::Cover,
                    }
                }
                _ => {}
            }
        }
    }
    flush(&mut out, &mut monitor, &mut wpath, &mut fit);
    out
}

/// Parse ~/.config/walld/config (simple `key = value`).
pub fn load_global() -> WalldConfig {
    let mut cfg = WalldConfig::default();
    let path = walld_conf_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return cfg, // optional file; defaults are fine
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        let v = v.trim();
        match k {
            "transition" => {
                cfg.transition = match v {
                    "snap" => Transition::Snap,
                    "wipe" | "diagonal" => Transition::Wipe,
                    other => {
                        log::warn!("config: unknown transition '{other}', keeping wipe");
                        Transition::Wipe
                    }
                }
            }
            "wipe_ms" => {
                if let Ok(n) = v.parse::<u32>() {
                    cfg.wipe_ms = n.clamp(50, 5000);
                }
            }
            "wipe_feather_px" => {
                if let Ok(f) = v.parse::<f32>() {
                    cfg.wipe_feather_px = f.clamp(1.0, 500.0);
                }
            }
            "hyprpaper_conf" => cfg.hyprpaper_conf = expand(v),
            "scene" => cfg.scene = Some(expand(v)),
            "scene_fps" => {
                if let Ok(n) = v.parse::<u32>() {
                    cfg.scene_fps = n.clamp(5, 120);
                }
            }
            "video_max_edge" => {
                if let Ok(n) = v.parse::<u32>() {
                    // 0 keeps the display-matched default; anything else is a
                    // deliberate cap and gets clamped to something decodable.
                    cfg.video_max_edge = if n == 0 { 0 } else { n.clamp(640, 7680) };
                }
            }
            "accent_on_change" => {
                cfg.accent_on_change = matches!(v, "true" | "1" | "yes" | "on");
            }
            _ => log::warn!("config: unknown key '{k}'"),
        }
    }
    cfg
}
