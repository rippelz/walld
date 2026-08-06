//! Runtime orchestration: play WE wallpapers on Hyprland.
//!
//! Backends:
//! - **Video** → mpvpaper per output (or all)
//! - **Scene** → linux-wallpaperengine when available (full fidelity)
//! - Stops walld layer surfaces while WE content owns the background

use crate::project::WallpaperType;
use crate::{lwe_binary, mpvpaper_binary, walld_binary, we_assets_dir};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PlayerError {
    #[error("{0}")]
    Msg(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayBackend {
    LinuxWallpaperEngine,
    MpvPaper,
    None,
}

#[derive(Debug, Clone)]
pub struct PlayRequest {
    pub wallpaper_dir: PathBuf,
    pub workshop_id: String,
    pub wallpaper_type: WallpaperType,
    /// Empty = all monitors
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
    pub lwe_available: bool,
    pub mpvpaper_available: bool,
}

static CHILDREN: Mutex<Vec<Child>> = Mutex::new(Vec::new());
static LAST: Mutex<Option<String>> = Mutex::new(None);

pub fn detect_backends() -> (bool, bool) {
    (lwe_binary().is_some(), mpvpaper_binary().is_some())
}

pub fn stop_all() {
    if let Ok(mut kids) = CHILDREN.lock() {
        for c in kids.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
        kids.clear();
    }
    // also kill stragglers by name
    let _ = Command::new("pkill").args(["-x", "linux-wallpaperengine"]).status();
    let _ = Command::new("pkill").args(["-x", "mpvpaper"]).status();
    // restore walld surfaces if daemon is up
    let walld = walld_binary();
    let _ = Command::new(&walld).args(["ctl", "start"]).output();
    if let Ok(mut last) = LAST.lock() {
        *last = None;
    }
}

pub fn play(req: &PlayRequest) -> Result<RuntimeStatus, PlayerError> {
    stop_all();
    // hide walld so it doesn't cover WE / video layers
    let walld = walld_binary();
    let _ = Command::new(&walld).args(["ctl", "stop"]).output();

    let (lwe_ok, mpv_ok) = detect_backends();
    let monitors = if req.monitors.is_empty() {
        discover_monitors()
    } else {
        req.monitors.clone()
    };

    match req.wallpaper_type {
        WallpaperType::Video => play_video(req, &monitors, mpv_ok),
        WallpaperType::Scene | WallpaperType::Unknown => {
            if lwe_ok {
                play_lwe(req, &monitors)
            } else if mpv_ok {
                // fallback: if folder has an mp4 use it
                if let Some(mp4) = find_video_in(&req.wallpaper_dir) {
                    let mut r = req.clone();
                    r.wallpaper_type = WallpaperType::Video;
                    // shadow by rewriting play for that file
                    return play_video_file(&mp4, &monitors, req.silent);
                }
                Err(PlayerError::Msg(
                    "linux-wallpaperengine not installed — required for Scene wallpapers. \
                     Install: yay -S linux-wallpaperengine-git  (or build to ~/.local/bin)"
                        .into(),
                ))
            } else {
                Err(PlayerError::Msg(
                    "no scene backend (linux-wallpaperengine) and no mpvpaper for video fallback"
                        .into(),
                ))
            }
        }
        WallpaperType::Web | WallpaperType::Application => Err(PlayerError::Msg(
            "Web/Application wallpapers need CEF-backed linux-wallpaperengine".into(),
        )),
    }
}

fn play_lwe(req: &PlayRequest, monitors: &[String]) -> Result<RuntimeStatus, PlayerError> {
    let bin = lwe_binary().ok_or_else(|| PlayerError::Msg("LWE missing".into()))?;
    let assets = we_assets_dir();
    let mut cmd = Command::new(&bin);
    cmd.arg("--assets-dir").arg(&assets);
    if req.silent {
        cmd.arg("--silent");
    }
    if req.fps > 0 {
        cmd.arg("--fps").arg(req.fps.to_string());
    }
    cmd.arg("--scaling").arg("fill");
    // multi-monitor: either screen-span or per-screen same bg
    if monitors.len() > 1 {
        for m in monitors {
            cmd.arg("--screen-root").arg(m);
            cmd.arg("--bg").arg(&req.wallpaper_dir);
        }
    } else if let Some(m) = monitors.first() {
        cmd.arg("--screen-root").arg(m);
        cmd.arg(&req.wallpaper_dir);
    } else {
        cmd.arg(&req.wallpaper_dir);
    }
    cmd.stdout(Stdio::null()).stderr(Stdio::null());
    let child = cmd
        .spawn()
        .map_err(|e| PlayerError::Msg(format!("spawn LWE: {e}")))?;
    if let Ok(mut kids) = CHILDREN.lock() {
        kids.push(child);
    }
    if let Ok(mut last) = LAST.lock() {
        *last = Some(req.workshop_id.clone());
    }
    Ok(RuntimeStatus {
        playing: true,
        backend: PlayBackend::LinuxWallpaperEngine,
        title: req.workshop_id.clone(),
        detail: format!("LWE · {}", req.wallpaper_dir.display()),
        lwe_available: true,
        mpvpaper_available: mpvpaper_binary().is_some(),
    })
}

fn play_video(req: &PlayRequest, monitors: &[String], mpv_ok: bool) -> Result<RuntimeStatus, PlayerError> {
    if !mpv_ok {
        return Err(PlayerError::Msg(
            "mpvpaper not found — install mpvpaper for video wallpapers".into(),
        ));
    }
    let file = find_video_in(&req.wallpaper_dir)
        .ok_or_else(|| PlayerError::Msg("no video file in wallpaper folder".into()))?;
    play_video_file(&file, monitors, req.silent)
}

fn play_video_file(file: &Path, monitors: &[String], silent: bool) -> Result<RuntimeStatus, PlayerError> {
    let bin = mpvpaper_binary().ok_or_else(|| PlayerError::Msg("mpvpaper missing".into()))?;
    let mons = if monitors.is_empty() {
        discover_monitors()
    } else {
        monitors.to_vec()
    };
    if mons.is_empty() {
        return Err(PlayerError::Msg("no monitors detected".into()));
    }
    let mut opts = String::from("no-audio loop-file=inf hwdec=auto-safe panscan=1.0 vo=gpu");
    if !silent {
        // still mute by default for wallpapers unless user wants audio — keep mute for safety
        opts = format!("no-audio {opts}");
    }
    for m in &mons {
        let mut cmd = Command::new(&bin);
        cmd.args(["-f", "-o", &opts, m, &file.to_string_lossy()]);
        cmd.stdout(Stdio::null()).stderr(Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| PlayerError::Msg(format!("mpvpaper: {e}")))?;
        if let Ok(mut kids) = CHILDREN.lock() {
            kids.push(child);
        }
    }
    Ok(RuntimeStatus {
        playing: true,
        backend: PlayBackend::MpvPaper,
        title: file
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default(),
        detail: format!("mpvpaper · {} monitor(s)", mons.len()),
        lwe_available: lwe_binary().is_some(),
        mpvpaper_available: true,
    })
}

fn find_video_in(dir: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(dir).ok()?;
    for ent in rd.flatten() {
        let p = ent.path();
        let ext = p.extension()?.to_string_lossy().to_ascii_lowercase();
        if matches!(ext.as_str(), "mp4" | "webm" | "mkv" | "mov") {
            return Some(p);
        }
    }
    None
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

pub fn status_snapshot() -> RuntimeStatus {
    let (lwe, mpv) = detect_backends();
    let playing = Command::new("pgrep")
        .args(["-x", "linux-wallpaperengine"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
        || Command::new("pgrep")
            .args(["-x", "mpvpaper"])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    let backend = if Command::new("pgrep")
        .args(["-x", "linux-wallpaperengine"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        PlayBackend::LinuxWallpaperEngine
    } else if Command::new("pgrep")
        .args(["-x", "mpvpaper"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        PlayBackend::MpvPaper
    } else {
        PlayBackend::None
    };
    let id = LAST.lock().ok().and_then(|g| g.clone()).unwrap_or_default();
    RuntimeStatus {
        playing,
        backend,
        title: id,
        detail: if playing {
            "runtime active".into()
        } else {
            "idle".into()
        },
        lwe_available: lwe,
        mpvpaper_available: mpv,
    }
}
