//! Persist the active Wallpaper Engine layout across walld restarts / reboots.
//!
//! Classic static walls live in `hyprpaper.conf`. WE packages are chosen at
//! runtime via wallstudio / `walld ctl we`, so without this file they vanish
//! every boot and the desktop falls back to the static config only.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSlot {
    /// Output names (`DP-1`) or `["*"]` for every display.
    pub monitors: Vec<String>,
    /// Absolute path to the WE package directory.
    pub path: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WeSession {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub slots: Vec<SessionSlot>,
}

fn default_version() -> u32 {
    VERSION
}

pub fn session_path() -> PathBuf {
    match std::env::var("XDG_CONFIG_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d).join("walld/session.json"),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".config/walld/session.json")
        }
    }
}

pub fn load() -> WeSession {
    let path = session_path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return WeSession::default();
    };
    match serde_json::from_str::<WeSession>(&text) {
        Ok(mut s) => {
            // Drop dead paths so a removed workshop item doesn't brick boot.
            s.slots.retain(|sl| sl.path.is_dir());
            s
        }
        Err(e) => {
            log::warn!("session: bad {}: {e}", path.display());
            WeSession::default()
        }
    }
}

pub fn save(session: &WeSession) -> Result<(), String> {
    let path = session_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(session).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

pub fn clear() {
    let path = session_path();
    let _ = std::fs::remove_file(path);
}

/// Build a session snapshot from live slots.
pub fn from_live_slots(slots: impl IntoIterator<Item = (Vec<String>, PathBuf)>) -> WeSession {
    let mut out = WeSession {
        version: VERSION,
        slots: Vec::new(),
    };
    for (monitors, path) in slots {
        if monitors.is_empty() {
            continue;
        }
        out.slots.push(SessionSlot { monitors, path });
    }
    out
}

/// Monitor list for IPC (`*` or `DP-1,DP-2`).
pub fn mon_arg(monitors: &[String]) -> String {
    if monitors.is_empty() || monitors.iter().any(|m| m == "*") {
        "*".into()
    } else {
        monitors.join(",")
    }
}

#[allow(dead_code)]
pub fn path() -> PathBuf {
    session_path()
}
