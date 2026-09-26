//! Editor live preview driven by **walld's GPU WE pipeline** (same as wallpaper).
//!
//! walld renders offscreen with full effect shaders and writes
//! `$XDG_RUNTIME_DIR/walld-editor-preview.png`. This module only loads/reloads
//! via IPC and polls the PNG — no CPU soft-raster.

use crate::walld_binary;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

#[derive(Clone)]
pub struct PreviewFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    pub generation: u64,
}

/// Live GPU preview handle. Drop → `walld ctl we_editor stop`.
pub struct SoftPreview {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
    last_seen: u64,
    dir: PathBuf,
}

struct Shared {
    frame: Mutex<Option<PreviewFrame>>,
    generation: AtomicU64,
    playing: AtomicBool,
    error: Mutex<Option<String>>,
    frames: AtomicU64,
    stop: AtomicBool,
    /// Path currently loaded in walld editor slot.
    dir: Mutex<PathBuf>,
    reload_req: AtomicBool,
}

impl SoftPreview {
    pub fn start(dir: &Path, _id: &str, _title: &str) -> Result<Self, String> {
        let dir = dir.to_path_buf();
        // Load into walld's dedicated editor slot — desktop WE keeps playing
        // on its monitors; this only feeds the offscreen live preview.
        let out = walld_ctl(&["we_editor", "load", &dir.to_string_lossy(), "720"])?;
        if !out.starts_with("ok") {
            return Err(out);
        }

        let shared = Arc::new(Shared {
            frame: Mutex::new(None),
            generation: AtomicU64::new(0),
            playing: AtomicBool::new(true),
            error: Mutex::new(None),
            frames: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            dir: Mutex::new(dir.clone()),
            reload_req: AtomicBool::new(false),
        });
        let shared_w = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("we-gpu-preview".into())
            .spawn(move || poll_loop(shared_w))
            .map_err(|e| e.to_string())?;

        Ok(Self {
            shared,
            join: Some(join),
            last_seen: 0,
            dir,
        })
    }

    pub fn set_playing(&self, playing: bool) {
        self.shared.playing.store(playing, Ordering::Relaxed);
        // walld still animates; pause just freezes UI updates.
        let _ = playing;
    }

    pub fn is_playing(&self) -> bool {
        self.shared.playing.load(Ordering::Relaxed)
    }

    pub fn request_reload(&self) {
        self.shared.reload_req.store(true, Ordering::Release);
    }

    pub fn last_error(&self) -> Option<String> {
        self.shared.error.lock().ok().and_then(|g| g.clone())
    }

    pub fn frame_count(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }

    pub fn take_if_new(&mut self) -> Option<PreviewFrame> {
        let gen = self.shared.generation.load(Ordering::Acquire);
        if gen == 0 || gen == self.last_seen {
            return None;
        }
        let frame = self.shared.frame.lock().ok()?.clone()?;
        self.last_seen = frame.generation;
        Some(frame)
    }

    pub fn current_frame(&self) -> Option<PreviewFrame> {
        self.shared.frame.lock().ok()?.clone()
    }

    pub fn width_height(&self) -> (u32, u32) {
        self.shared
            .frame
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|f| (f.width, f.height)))
            .unwrap_or((0, 0))
    }

    pub fn project_dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for SoftPreview {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let _ = walld_ctl(&["we_editor", "stop"]);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn poll_loop(shared: Arc<Shared>) {
    let mut last_walld_gen = 0u64;
    while !shared.stop.load(Ordering::Acquire) {
        if shared.reload_req.swap(false, Ordering::AcqRel) {
            if let Ok(dir) = shared.dir.lock() {
                match walld_ctl(&[
                    "we_editor",
                    "reload",
                ]) {
                    Ok(s) if s.starts_with("ok") => {
                        if let Ok(mut e) = shared.error.lock() {
                            *e = None;
                        }
                    }
                    Ok(s) => {
                        // fallback: load path again
                        let _ = walld_ctl(&[
                            "we_editor",
                            "load",
                            &dir.to_string_lossy(),
                            "720",
                        ]);
                        if let Ok(mut e) = shared.error.lock() {
                            *e = Some(s);
                        }
                    }
                    Err(e) => {
                        if let Ok(mut err) = shared.error.lock() {
                            *err = Some(e);
                        }
                    }
                }
            }
        }

        if !shared.playing.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(100));
            continue;
        }

        match walld_ctl(&["we_editor", "status"]) {
            Ok(line) => {
                let gen = parse_status_u64(&line, "gen=").unwrap_or(0);
                let path = parse_status_path(&line);
                if gen > last_walld_gen {
                    if let Some(p) = path {
                        if let Ok(img) = image::open(&p) {
                            let rgba = img.to_rgba8();
                            let (w, h) = rgba.dimensions();
                            let local_gen =
                                shared.generation.fetch_add(1, Ordering::AcqRel) + 1;
                            if let Ok(mut slot) = shared.frame.lock() {
                                *slot = Some(PreviewFrame {
                                    width: w,
                                    height: h,
                                    rgba: rgba.into_raw(),
                                    generation: local_gen,
                                });
                            }
                            shared.frames.fetch_add(1, Ordering::Relaxed);
                            last_walld_gen = gen;
                        }
                    }
                }
            }
            Err(e) => {
                if let Ok(mut err) = shared.error.lock() {
                    *err = Some(e);
                }
            }
        }
        thread::sleep(Duration::from_millis(40));
    }
}

fn walld_ctl(args: &[&str]) -> Result<String, String> {
    let bin = walld_binary();
    let mut cmd = Command::new(&bin);
    cmd.arg("ctl");
    for a in args {
        cmd.arg(a);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("walld ctl: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !out.status.success() {
        return Err(if !stdout.is_empty() {
            stdout
        } else if !stderr.is_empty() {
            stderr
        } else {
            "walld ctl failed".into()
        });
    }
    Ok(stdout)
}

fn parse_status_u64(line: &str, key: &str) -> Option<u64> {
    line.split_whitespace()
        .find_map(|p| p.strip_prefix(key)?.parse().ok())
}

fn parse_status_path(line: &str) -> Option<PathBuf> {
    line.split_whitespace()
        .find_map(|p| p.strip_prefix("path=").map(PathBuf::from))
}
