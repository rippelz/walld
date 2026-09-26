//! Generate animated `preview.gif` thumbnails from GPU editor frames.

use image::codecs::gif::{GifEncoder, Repeat};
use image::{Delay, Frame, RgbaImage};
use serde_json::{json, Value};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::walld_binary;

/// One RGBA frame for GIF assembly.
#[derive(Clone)]
pub struct GifFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Encode frames to an animated GIF (infinite loop).
///
/// `delay_ms` is the per-frame delay. Frames are resized so the longest edge
/// is at most `max_edge` (library thumbnails stay small).
pub fn encode_gif(
    frames: &[GifFrame],
    out: &Path,
    delay_ms: u32,
    max_edge: u32,
) -> Result<(), String> {
    if frames.is_empty() {
        return Err("no frames to encode".into());
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let file = File::create(out).map_err(|e| e.to_string())?;
    let mut enc = GifEncoder::new_with_speed(BufWriter::new(file), 10);
    enc.set_repeat(Repeat::Infinite)
        .map_err(|e| e.to_string())?;

    let delay = Delay::from_numer_denom_ms(delay_ms.max(20), 1);
    let max_edge = max_edge.max(64);

    for f in frames {
        let mut img = RgbaImage::from_raw(f.width, f.height, f.rgba.clone())
            .ok_or_else(|| "bad frame buffer".to_string())?;
        let (w, h) = img.dimensions();
        let long = w.max(h).max(1);
        if long > max_edge {
            let scale = max_edge as f32 / long as f32;
            let nw = ((w as f32 * scale).round() as u32).max(1);
            let nh = ((h as f32 * scale).round() as u32).max(1);
            img = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
        }
        // GIF encoder wants RGB for size sometimes but Frame from Rgba works.
        let frame = Frame::from_parts(img, 0, 0, delay);
        enc.encode_frame(frame).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Point `project.json` at `preview.gif` so the library picks it up.
pub fn set_project_preview_gif(project_dir: &Path) -> Result<(), String> {
    let path = project_dir.join("project.json");
    let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let obj = raw
        .as_object_mut()
        .ok_or_else(|| "project.json root is not an object".to_string())?;
    obj.insert("preview".into(), json!("preview.gif"));
    let out = serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?;
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(())
}

fn walld_ctl(args: &[&str]) -> Result<String, String> {
    let bin = walld_binary();
    let mut cmd = Command::new(&bin);
    cmd.arg("ctl");
    for a in args {
        cmd.arg(a);
    }
    let out = cmd.output().map_err(|e| format!("walld ctl: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
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

fn editor_preview_path() -> PathBuf {
    let run = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(run).join("walld-editor-preview.png")
}

fn parse_status_u64(line: &str, key: &str) -> Option<u64> {
    line.split_whitespace()
        .find_map(|p| p.strip_prefix(key)?.parse().ok())
}

/// Capture animated frames from the live walld editor GPU preview and write
/// `preview.gif` into the project folder.
///
/// Assumes `we_editor load` is already active for this project (normal editor session).
pub fn capture_and_write_preview_gif(
    project_dir: &Path,
    frame_count: u32,
    fps: f32,
    max_edge: u32,
) -> Result<PathBuf, String> {
    let frame_count = frame_count.clamp(4, 48);
    let fps = fps.clamp(4.0, 24.0);
    let delay_ms = (1000.0 / fps).round() as u32;
    let interval = Duration::from_millis(delay_ms as u64);

    let png_path = editor_preview_path();
    let mut frames: Vec<GifFrame> = Vec::with_capacity(frame_count as usize);
    let mut last_gen = 0u64;
    let mut spins = 0u32;
    let max_spins = frame_count * 8 + 20;

    while frames.len() < frame_count as usize && spins < max_spins {
        spins += 1;
        // Nudge a fresh frame (walld ticks while editor_preview_only is on).
        let _ = walld_ctl(&["we_editor", "status"]);
        let gen = walld_ctl(&["we_editor", "status"])
            .ok()
            .and_then(|s| parse_status_u64(&s, "gen="))
            .unwrap_or(0);

        if gen > last_gen && png_path.is_file() {
            if let Ok(img) = image::open(&png_path) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                if w > 0 && h > 0 {
                    frames.push(GifFrame {
                        width: w,
                        height: h,
                        rgba: rgba.into_raw(),
                    });
                    last_gen = gen;
                }
            }
        }
        thread::sleep(interval);
    }

    if frames.is_empty() {
        // Fallback: single still from current PNG
        if png_path.is_file() {
            if let Ok(img) = image::open(&png_path) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                frames.push(GifFrame {
                    width: w,
                    height: h,
                    rgba: rgba.into_raw(),
                });
            }
        }
    }
    if frames.is_empty() {
        return Err("no preview frames captured (is walld editor preview running?)".into());
    }

    // If we only got one frame, duplicate it a few times so library still
    // shows something (static GIF); better than nothing.
    if frames.len() == 1 {
        let f = frames[0].clone();
        for _ in 0..5 {
            frames.push(f.clone());
        }
    }

    let out = project_dir.join("preview.gif");
    // Write to temp then rename so readers don't see a half file.
    let tmp = project_dir.join("preview.gif.tmp");
    encode_gif(&frames, &tmp, delay_ms, max_edge)?;
    std::fs::rename(&tmp, &out).map_err(|e| e.to_string())?;
    set_project_preview_gif(project_dir)?;
    log::info!(
        "wrote preview.gif ({} frames) → {}",
        frames.len(),
        out.display()
    );
    Ok(out)
}

/// Fire-and-forget GIF regen on a background thread.
pub fn spawn_preview_gif_job(project_dir: PathBuf) {
    thread::Builder::new()
        .name("preview-gif".into())
        .spawn(move || {
            // Let the last scene save settle + a couple of walld frames render.
            thread::sleep(Duration::from_millis(400));
            match capture_and_write_preview_gif(&project_dir, 18, 10.0, 480) {
                Ok(p) => log::info!("auto preview gif ready: {}", p.display()),
                Err(e) => log::warn!("auto preview gif failed: {e}"),
            }
        })
        .ok();
}
