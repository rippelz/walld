//! Play WE content exclusively through walld (in-process engine).
//! No mpvpaper, no linux-wallpaperengine.

use crate::project::WallpaperType;
use crate::walld_binary;
use std::path::PathBuf;
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("{0}")]
    Msg(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayBackend {
    Walld,
    None,
}

#[derive(Debug, Clone)]
pub struct PlayRequest {
    pub wallpaper_dir: PathBuf,
    pub workshop_id: String,
    pub wallpaper_type: WallpaperType,
    /// Empty = all monitors (*)
    pub monitors: Vec<String>,
    pub silent: bool,
    pub fps: u32,
}

#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    pub playing: bool,
    pub backend: PlayBackend,
    pub title: String,
    pub detail: String,
    /// Always true for product messaging (engine is walld).
    pub engine_ready: bool,
}

pub fn stop_all() {
    let walld = walld_binary();
    let _ = Command::new(&walld).args(["ctl", "we_stop"]).output();
}

pub fn play(req: &PlayRequest) -> Result<RuntimeStatus, PlayerError> {
    let walld = walld_binary();
    // ensure daemon
    if !walld_alive(&walld) {
        ensure_walld(&walld)?;
    }
    let mon = if req.monitors.is_empty() {
        "*".to_string()
    } else {
        req.monitors.join(",")
    };
    let path = req.wallpaper_dir.to_string_lossy();
    let out = Command::new(&walld)
        .args(["ctl", "we", &mon, path.as_ref()])
        .output()
        .map_err(|e| PlayerError::Msg(format!("walld ctl we: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return Err(PlayerError::Msg(if !stdout.is_empty() {
            stdout
        } else if !stderr.is_empty() {
            stderr
        } else {
            "walld we failed".into()
        }));
    }
    Ok(RuntimeStatus {
        playing: true,
        backend: PlayBackend::Walld,
        title: req.workshop_id.clone(),
        detail: stdout,
        engine_ready: true,
    })
}

pub fn status_snapshot() -> RuntimeStatus {
    let walld = walld_binary();
    let out = Command::new(&walld).args(["ctl", "status"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let playing = line.contains("we=") || line.contains("video=") || line.contains("scene=");
            RuntimeStatus {
                playing,
                backend: if playing {
                    PlayBackend::Walld
                } else {
                    PlayBackend::None
                },
                title: extract_we_title(&line).unwrap_or_default(),
                detail: line,
                engine_ready: true,
            }
        }
        _ => RuntimeStatus {
            playing: false,
            backend: PlayBackend::None,
            title: String::new(),
            detail: "walld offline".into(),
            engine_ready: false,
        },
    }
}

fn extract_we_title(status: &str) -> Option<String> {
    for part in status.split_whitespace() {
        for prefix in ["we=", "video=", "scene="] {
            if let Some(rest) = part.strip_prefix(prefix) {
                let name = rest.split('(').next()?.trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

fn walld_alive(bin: &std::path::Path) -> bool {
    Command::new(bin)
        .args(["ctl", "ping"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn ensure_walld(bin: &std::path::Path) -> Result<(), PlayerError> {
    // start daemon if missing
    let _ = Command::new(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| PlayerError::Msg(format!("start walld: {e}")))?;
    for _ in 0..40 {
        if walld_alive(bin) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err(PlayerError::Msg("walld failed to start".into()))
}

pub fn discover_monitors() -> Vec<String> {
    let out = Command::new("hyprctl")
        .args(["monitors", "-j"])
        .output()
        .ok();
    let Some(out) = out else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) else {
        return Vec::new();
    };
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("name")?.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

pub fn detect_backends() -> (bool, bool) {
    // Product: only walld. Kept for UI compatibility as (engine, _) 
    (true, true)
}
