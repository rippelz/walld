//! Boot stills — last-known-good wallpaper frames for instant next login.
//!
//! When a wallpaper is applied (classic image or WE after first real frames),
//! we write a per-monitor PNG under `$XDG_CACHE_HOME/walld/boot/`. On the next
//! walld start those PNGs paint *before* WE/session load, so the desktop never
//! shows Hyprland's default bg while the heavy content spins up.

use std::path::{Path, PathBuf};

use crate::image::Image;

const MAX_EDGE: u32 = 1920;

pub fn cache_dir() -> PathBuf {
    match std::env::var("XDG_CACHE_HOME") {
        Ok(d) if !d.is_empty() => PathBuf::from(d).join("walld/boot"),
        _ => {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache/walld/boot")
        }
    }
}

/// Safe filename for a monitor name (`DP-1` → `DP-1.png`).
pub fn path_for_monitor(name: &str) -> PathBuf {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = if safe.is_empty() {
        "unknown".into()
    } else {
        safe
    };
    cache_dir().join(format!("{safe}.png"))
}

pub fn path_if_exists(name: &str) -> Option<PathBuf> {
    let p = path_for_monitor(name);
    if p.is_file() {
        Some(p)
    } else {
        None
    }
}

/// Downscale if needed and write RGBA8 top-down to the boot still path.
pub fn save_rgba(monitor: &str, width: u32, height: u32, rgba: &[u8]) -> Result<PathBuf, String> {
    if width == 0 || height == 0 {
        return Err("empty frame".into());
    }
    if rgba.len() < (width as usize) * (height as usize) * 4 {
        return Err("rgba buffer too small".into());
    }
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;

    let path = path_for_monitor(monitor);
    // Must end in `.png` — image crate sniffs format from the extension.
    let tmp = path.with_file_name(format!(
        ".{}.boot-writing.png",
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("mon")
    ));

    let (w, h, pixels) = maybe_downscale(width, height, rgba);
    let img = ::image::RgbaImage::from_raw(w, h, pixels)
        .ok_or_else(|| "bad rgba dimensions".to_string())?;
    img.save(&tmp)
        .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    log::info!(
        "boot-still: saved {} ({}×{}) → {}",
        monitor,
        w,
        h,
        path.display()
    );
    Ok(path)
}

/// Save from an already-decoded classic wallpaper image.
pub fn save_image(monitor: &str, img: &Image) -> Result<PathBuf, String> {
    save_rgba(monitor, img.width, img.height, &img.rgba)
}

/// Copy/re-encode a file on disk into the boot still for `monitor`.
pub fn save_from_path(monitor: &str, src: &Path) -> Result<PathBuf, String> {
    let img = crate::image::decode_file(src)?;
    save_image(monitor, &img)
}

fn maybe_downscale(width: u32, height: u32, rgba: &[u8]) -> (u32, u32, Vec<u8>) {
    let long = width.max(height);
    if long <= MAX_EDGE {
        return (width, height, rgba.to_vec());
    }
    let scale = MAX_EDGE as f32 / long as f32;
    let nw = ((width as f32) * scale).round().max(1.0) as u32;
    let nh = ((height as f32) * scale).round().max(1.0) as u32;
    let src = match ::image::RgbaImage::from_raw(width, height, rgba.to_vec()) {
        Some(i) => i,
        None => return (width, height, rgba.to_vec()),
    };
    let resized =
        ::image::imageops::resize(&src, nw, nh, ::image::imageops::FilterType::Triangle);
    (nw, nh, resized.into_raw())
}
