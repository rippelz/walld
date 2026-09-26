//! Runtime presentation controls for WE content (Wallpaper Engine–style).
//!
//! Applied by walld while a package is playing. Persisted per workshop id under
//! `~/.config/walld/present/<id>.json`.
//!
//! When the same package spans multiple monitors, **visual** fields (flip, zoom,
//! fit, pan) can differ per display via `monitors` in the JSON / runtime map.
//! Playback fields (pause, rate, mute) stay shared — one decoder, one clock.

use crate::config::FitMode;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WePresent {
    #[serde(default)]
    pub paused: bool,
    /// Playback rate / timescale (1.0 = real-time).
    #[serde(default = "default_rate")]
    pub rate: f32,
    /// Reserved for future audio path; wallstudio uses it as mute intent.
    #[serde(default = "default_true")]
    pub mute: bool,
    #[serde(default)]
    pub fit: PresentFit,
    /// Extra zoom on top of fit (1.0 = fit only). Higher = zoom in.
    #[serde(default = "default_zoom")]
    pub zoom: f32,
    /// Normalized UV pan, roughly −1…1.
    #[serde(default)]
    pub offset_x: f32,
    #[serde(default)]
    pub offset_y: f32,
    #[serde(default)]
    pub flip_h: bool,
    #[serde(default)]
    pub flip_v: bool,
}

fn default_rate() -> f32 {
    1.0
}
fn default_true() -> bool {
    true
}
fn default_zoom() -> f32 {
    1.0
}

impl Default for WePresent {
    fn default() -> Self {
        Self {
            paused: false,
            rate: 1.0,
            mute: true,
            fit: PresentFit::Cover,
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            flip_h: false,
            flip_v: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum PresentFit {
    #[default]
    Cover,
    Contain,
    Fill,
}

impl PresentFit {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Cover => "cover",
            Self::Contain => "contain",
            Self::Fill => "fill",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "cover" => Some(Self::Cover),
            "contain" | "fit" => Some(Self::Contain),
            "fill" | "stretch" => Some(Self::Fill),
            _ => None,
        }
    }

    pub fn to_fit_mode(self) -> FitMode {
        match self {
            Self::Cover => FitMode::Cover,
            Self::Contain => FitMode::Contain,
            Self::Fill => FitMode::Fill,
        }
    }
}

/// On-disk present file: shared base + optional per-monitor visual overrides.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PresentFile {
    #[serde(flatten)]
    pub base: WePresent,
    /// Visual-only overrides keyed by output name (`DP-1`, `DP-2`, …).
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub monitors: HashMap<String, WePresent>,
}

/// Keys that affect layout/mirror (safe to differ per display).
pub fn is_visual_key(key: &str) -> bool {
    matches!(
        key,
        "fit"
            | "zoom"
            | "scale"
            | "offset"
            | "position"
            | "offset_x"
            | "pos_x"
            | "x"
            | "offset_y"
            | "pos_y"
            | "y"
            | "flip_h"
            | "fliph"
            | "mirror"
            | "flip_v"
            | "flipv"
    )
}

/// Keys that drive the shared decoder / clock.
pub fn is_playback_key(key: &str) -> bool {
    matches!(
        key,
        "pause" | "paused" | "rate" | "timescale" | "speed" | "mute"
    )
}

impl WePresent {
    pub fn clamp(&mut self) {
        self.rate = self.rate.clamp(0.05, 4.0);
        self.zoom = self.zoom.clamp(0.25, 4.0);
        self.offset_x = self.offset_x.clamp(-1.0, 1.0);
        self.offset_y = self.offset_y.clamp(-1.0, 1.0);
    }

    pub fn status_line(&self) -> String {
        format!(
            "ok present paused={} rate={:.3} mute={} fit={} zoom={:.3} offset={:.3},{:.3} flip_h={} flip_v={}",
            self.paused as u8,
            self.rate,
            self.mute as u8,
            self.fit.as_str(),
            self.zoom,
            self.offset_x,
            self.offset_y,
            self.flip_h as u8,
            self.flip_v as u8,
        )
    }

    /// Copy layout/mirror fields from `other` (playback fields unchanged).
    pub fn copy_visual_from(&mut self, other: &WePresent) {
        self.fit = other.fit;
        self.zoom = other.zoom;
        self.offset_x = other.offset_x;
        self.offset_y = other.offset_y;
        self.flip_h = other.flip_h;
        self.flip_v = other.flip_v;
    }

    /// Base present with optional per-monitor visual override applied.
    pub fn with_monitor_visual(&self, over: Option<&WePresent>) -> WePresent {
        let mut p = self.clone();
        if let Some(o) = over {
            p.copy_visual_from(o);
        }
        p
    }

    /// Apply a single key/value update. Value is the remainder of the IPC line.
    pub fn apply_kv(&mut self, key: &str, value: &str) -> Result<(), String> {
        let v = value.trim();
        match key {
            "pause" | "paused" => {
                self.paused = parse_bool(v)?;
            }
            "rate" | "timescale" | "speed" => {
                self.rate = v
                    .parse::<f32>()
                    .map_err(|_| format!("bad rate: {v}"))?
                    .clamp(0.05, 4.0);
            }
            "mute" => {
                self.mute = parse_bool(v)?;
            }
            "fit" => {
                self.fit = PresentFit::parse(v).ok_or_else(|| format!("bad fit: {v}"))?;
            }
            "zoom" | "scale" => {
                self.zoom = v
                    .parse::<f32>()
                    .map_err(|_| format!("bad zoom: {v}"))?
                    .clamp(0.25, 4.0);
            }
            "offset" | "position" => {
                let mut it = v.split_whitespace();
                let x: f32 = it
                    .next()
                    .ok_or("offset needs x y")?
                    .parse()
                    .map_err(|_| "bad offset x".to_string())?;
                let y: f32 = it
                    .next()
                    .ok_or("offset needs x y")?
                    .parse()
                    .map_err(|_| "bad offset y".to_string())?;
                self.offset_x = x.clamp(-1.0, 1.0);
                self.offset_y = y.clamp(-1.0, 1.0);
            }
            "offset_x" | "pos_x" | "x" => {
                self.offset_x = v
                    .parse::<f32>()
                    .map_err(|_| format!("bad offset_x: {v}"))?
                    .clamp(-1.0, 1.0);
            }
            "offset_y" | "pos_y" | "y" => {
                self.offset_y = v
                    .parse::<f32>()
                    .map_err(|_| format!("bad offset_y: {v}"))?
                    .clamp(-1.0, 1.0);
            }
            "flip_h" | "fliph" | "mirror" => {
                self.flip_h = parse_bool(v)?;
            }
            "flip_v" | "flipv" => {
                self.flip_v = parse_bool(v)?;
            }
            "reset" => {
                *self = WePresent::default();
            }
            other => return Err(format!("unknown present key '{other}'")),
        }
        Ok(())
    }

    pub fn present_dir() -> PathBuf {
        let base = std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
                PathBuf::from(home).join(".config")
            });
        base.join("walld").join("present")
    }

    pub fn path_for_id(id: &str) -> PathBuf {
        let safe: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        Self::present_dir().join(format!("{safe}.json"))
    }

    pub fn load_for_id(id: &str) -> Self {
        Self::load_file_for_id(id).base
    }

    pub fn load_file_for_id(id: &str) -> PresentFile {
        let path = Self::path_for_id(id);
        Self::load_file_path(&path).unwrap_or_default()
    }

    pub fn load_file_path(path: &Path) -> Option<PresentFile> {
        let text = std::fs::read_to_string(path).ok()?;
        // Prefer full file (base + monitors); fall back to bare WePresent.
        if let Ok(mut f) = serde_json::from_str::<PresentFile>(&text) {
            f.base.clamp();
            for p in f.monitors.values_mut() {
                p.clamp();
            }
            return Some(f);
        }
        let mut p: WePresent = serde_json::from_str(&text).ok()?;
        p.clamp();
        Some(PresentFile {
            base: p,
            monitors: HashMap::new(),
        })
    }

    pub fn load_path(path: &Path) -> Option<Self> {
        Some(Self::load_file_path(path)?.base)
    }

    pub fn save_for_id(&self, id: &str) -> Result<(), String> {
        self.save_file_for_id(id, &HashMap::new())
    }

    pub fn save_file_for_id(
        &self,
        id: &str,
        monitors: &HashMap<String, WePresent>,
    ) -> Result<(), String> {
        let dir = Self::present_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let path = Self::path_for_id(id);
        let file = PresentFile {
            base: self.clone(),
            monitors: monitors.clone(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| e.to_string())
    }
}

fn parse_bool(s: &str) -> Result<bool, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        other => Err(format!("expected bool, got '{other}'")),
    }
}
