//! Play WE content through walld (default) or linux-wallpaperengine (optional).

use crate::lwe_binary;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PlayBackend {
    #[default]
    Walld,
    /// External `linux-wallpaperengine` binary when available.
    Lwe,
    None,
}

impl PlayBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::Walld => "walld",
            Self::Lwe => "LWE",
            Self::None => "—",
        }
    }
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
    /// Preferred engine (walld or LWE).
    pub backend: PlayBackend,
}

#[derive(Debug, Clone)]
pub struct RuntimeStatus {
    pub playing: bool,
    pub backend: PlayBackend,
    pub title: String,
    pub detail: String,
    /// walld daemon is reachable.
    pub engine_ready: bool,
    /// linux-wallpaperengine binary found on PATH / known locations.
    pub lwe_ready: bool,
    /// Per-monitor wallpaper path from `walld ctl status` (empty when offline).
    pub monitor_paths: Vec<(String, String)>,
}

pub fn stop_all() {
    let walld = walld_binary();
    let _ = Command::new(&walld).args(["ctl", "we_stop"]).output();
    kill_lwe();
}

pub fn play(req: &PlayRequest) -> Result<RuntimeStatus, PlayerError> {
    match req.backend {
        PlayBackend::Lwe => play_lwe(req),
        PlayBackend::Walld | PlayBackend::None => play_walld(req),
    }
}

fn play_walld(req: &PlayRequest) -> Result<RuntimeStatus, PlayerError> {
    let walld = walld_binary();
    if !walld_alive(&walld) {
        ensure_walld(&walld)?;
    }
    // Stop LWE so it doesn't fight walld for the same outputs.
    kill_lwe();
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
        lwe_ready: lwe_binary().is_some(),
        monitor_paths: parse_monitor_paths_from_status(&status_line()),
    })
}

fn play_lwe(req: &PlayRequest) -> Result<RuntimeStatus, PlayerError> {
    let lwe = lwe_binary().ok_or_else(|| {
        PlayerError::Msg(
            "linux-wallpaperengine not found — install LWE or switch Engine to walld".into(),
        )
    })?;
    // Stop walld WE surfaces so LWE can own the outputs.
    let walld = walld_binary();
    let _ = Command::new(&walld).args(["ctl", "we_stop"]).output();
    // Kill previous LWE instances (must work — otherwise they stack).
    kill_lwe();

    let screens: Vec<String> = if req.monitors.is_empty() {
        discover_monitors()
    } else {
        req.monitors.clone()
    };
    if screens.is_empty() {
        return Err(PlayerError::Msg(
            "no monitors for LWE — is hyprctl available?".into(),
        ));
    }

    // LWE takes a workshop id or package path; prefer numeric id, else path.
    let bg_arg = if req.workshop_id.chars().all(|c| c.is_ascii_digit())
        && !req.workshop_id.is_empty()
    {
        req.workshop_id.clone()
    } else {
        req.wallpaper_dir.to_string_lossy().into_owned()
    };

    let mut pids = Vec::new();
    for screen in &screens {
        let mut cmd = Command::new(&lwe);
        cmd.arg("--screen-root")
            .arg(screen)
            .arg("--bg")
            .arg(&bg_arg)
            .arg("--fps")
            .arg(req.fps.to_string())
            .arg("--scaling")
            .arg("fill");
        if req.silent {
            cmd.arg("--silent");
        }
        // Detach so wallstudio doesn't block; reap in a helper thread so we
        // don't leave zombies when the process exits.
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let child = cmd
            .spawn()
            .map_err(|e| PlayerError::Msg(format!("spawn LWE on {screen}: {e}")))?;
        pids.push(child.id());
        reap_in_background(child);
    }
    write_lwe_pids(&pids);

    Ok(RuntimeStatus {
        playing: true,
        backend: PlayBackend::Lwe,
        title: req.workshop_id.clone(),
        detail: format!("LWE · {} screen(s)", screens.len()),
        engine_ready: walld_alive(&walld),
        lwe_ready: true,
        monitor_paths: screens
            .into_iter()
            .map(|s| (s, req.wallpaper_dir.to_string_lossy().into_owned()))
            .collect(),
    })
}

pub fn status_snapshot() -> RuntimeStatus {
    let walld = walld_binary();
    let lwe_ready = lwe_binary().is_some();
    let lwe_running = lwe_process_running();
    let out = Command::new(&walld).args(["ctl", "status"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let line = String::from_utf8_lossy(&o.stdout).trim().to_string();
            let walld_playing =
                line.contains("we=") || line.contains("video=") || line.contains("web=") || line.contains("scene=");
            let playing = walld_playing || lwe_running;
            let backend = if lwe_running && !walld_playing {
                PlayBackend::Lwe
            } else if walld_playing {
                PlayBackend::Walld
            } else {
                PlayBackend::None
            };
            RuntimeStatus {
                playing,
                backend,
                title: extract_we_title(&line).unwrap_or_default(),
                detail: if lwe_running && !walld_playing {
                    "LWE running".into()
                } else {
                    line.clone()
                },
                engine_ready: true,
                lwe_ready,
                monitor_paths: parse_monitor_paths_from_status(&line),
            }
        }
        _ => RuntimeStatus {
            playing: lwe_running,
            backend: if lwe_running {
                PlayBackend::Lwe
            } else {
                PlayBackend::None
            },
            title: String::new(),
            detail: if lwe_running {
                "LWE running · walld offline".into()
            } else {
                "walld offline".into()
            },
            engine_ready: false,
            lwe_ready,
            monitor_paths: Vec::new(),
        },
    }
}

fn status_line() -> String {
    let walld = walld_binary();
    Command::new(&walld)
        .args(["ctl", "status"])
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_default()
}

/// Parse `DP-1=/path/to/id (we)` pairs from walld status.
pub fn parse_monitor_paths_from_status(status: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for part in status.split_whitespace() {
        // name=/abs/path or name=/abs/path(we) or name=/abs/path (we)
        let Some((name, rest)) = part.split_once('=') else {
            continue;
        };
        if name == "we" || name == "video" || name == "scene" || name == "ok" {
            continue;
        }
        // Drop trailing "(we)" / "(video)" if glued.
        let path = rest
            .split('(')
            .next()
            .unwrap_or(rest)
            .trim_end_matches(')')
            .to_string();
        if path.starts_with('/') || path.starts_with('~') {
            out.push((name.to_string(), path));
        }
    }
    out
}

/// Workshop id (last path component) for a monitor's active wallpaper, if any.
pub fn monitor_wallpaper_id(status: &RuntimeStatus, monitor: &str) -> Option<String> {
    let path = if monitor.is_empty() {
        status.monitor_paths.first().map(|(_, p)| p.as_str())
    } else {
        status
            .monitor_paths
            .iter()
            .find(|(n, _)| n == monitor)
            .map(|(_, p)| p.as_str())
    }?;
    std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

fn lwe_process_running() -> bool {
    !lwe_pids().is_empty()
}

/// Linux `comm` is only 15 chars, so `pkill -x linux-wallpaperengine` never
/// matches (`linux-wallpaper` is what shows up). Kill by full cmdline instead,
/// and also by any PIDs we recorded when spawning.
fn kill_lwe() {
    // Prefer our recorded PIDs (precise; no collateral).
    for pid in read_lwe_pids() {
        let _ = Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .output();
    }
    // Full-cmdline match: works even if another tool started LWE.
    // Pattern is the binary name as a path component or argv0.
    let _ = Command::new("pkill")
        .args(["-f", "linux-wallpaperengine"])
        .output();
    // Give them a moment to exit cleanly, then force leftovers.
    std::thread::sleep(std::time::Duration::from_millis(80));
    for pid in read_lwe_pids() {
        if pid_alive(pid) {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .output();
        }
    }
    let _ = Command::new("pkill")
        .args(["-9", "-f", "linux-wallpaperengine"])
        .output();
    clear_lwe_pids();
}

fn reap_in_background(mut child: std::process::Child) {
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

fn lwe_pidfile() -> PathBuf {
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("wallstudio-lwe.pids")
}

fn write_lwe_pids(pids: &[u32]) {
    let body = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = std::fs::write(lwe_pidfile(), body);
}

fn read_lwe_pids() -> Vec<u32> {
    let Ok(text) = std::fs::read_to_string(lwe_pidfile()) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect()
}

fn clear_lwe_pids() {
    let _ = std::fs::remove_file(lwe_pidfile());
}

fn pid_alive(pid: u32) -> bool {
    PathBuf::from(format!("/proc/{pid}")).exists()
}

/// Live LWE PIDs via /proc (cmdline contains the binary name).
fn lwe_pids() -> Vec<u32> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return out;
    };
    for ent in rd.flatten() {
        let name = ent.file_name();
        let name = name.to_string_lossy();
        if !name.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let pid: u32 = match name.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        let cmdline = std::fs::read(ent.path().join("cmdline")).unwrap_or_default();
        // cmdline is NUL-separated; search raw bytes for the binary name.
        if cmdline.windows(b"linux-wallpaperengine".len()).any(|w| w == b"linux-wallpaperengine")
        {
            out.push(pid);
        }
    }
    out
}

fn extract_we_title(status: &str) -> Option<String> {
    for part in status.split_whitespace() {
        for prefix in ["we=", "video=", "web=", "scene="] {
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

#[derive(Debug, Clone)]
pub struct MonitorInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
}

impl MonitorInfo {
    pub fn label(&self) -> String {
        format!("{}  {}×{}", self.name, self.width, self.height)
    }

    /// Aspect ratio for tiny display glyph (width/height).
    pub fn aspect(&self) -> f32 {
        if self.height == 0 {
            16.0 / 9.0
        } else {
            self.width as f32 / self.height as f32
        }
    }
}

pub fn discover_monitors_info() -> Vec<MonitorInfo> {
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
                .filter_map(|m| {
                    Some(MonitorInfo {
                        name: m.get("name")?.as_str()?.to_string(),
                        width: m.get("width")?.as_u64()? as u32,
                        height: m.get("height")?.as_u64()? as u32,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn discover_monitors() -> Vec<String> {
    discover_monitors_info()
        .into_iter()
        .map(|m| m.name)
        .collect()
}

pub fn detect_backends() -> (bool, bool) {
    (true, lwe_binary().is_some())
}
