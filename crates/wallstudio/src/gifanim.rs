//! Animated GIF previews.
//!
//! iced's `image::Handle::from_path` renders a single frame, so animated
//! workshop previews looked like stills. Frames are decoded once, cached per
//! path, and advanced on wall-clock time so every tile animates at its own
//! authored rate without needing per-tile timers.

use iced::widget::image::Handle;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

struct Frames {
    handles: Vec<Handle>,
    /// Cumulative frame end times in seconds; last entry = loop length.
    ends: Vec<f32>,
}

/// iced builds views from `&App`, so decoding is cached behind a `RefCell`.
#[derive(Default)]
pub struct GifCache {
    cache: RefCell<HashMap<PathBuf, Option<Frames>>>,
    start: RefCell<Option<Instant>>,
}

impl GifCache {
    /// Current frame for `path`, decoding on first use. `None` when the file
    /// isn't an animated GIF — callers fall back to `Handle::from_path`.
    pub fn handle(&self, path: &Path) -> Option<Handle> {
        let start = *self.start.borrow_mut().get_or_insert_with(Instant::now);
        if !self.cache.borrow().contains_key(path) {
            let decoded = decode(path);
            self.cache.borrow_mut().insert(path.to_path_buf(), decoded);
        }
        let cache = self.cache.borrow();
        let frames = cache.get(path)?.as_ref()?;
        let total = *frames.ends.last()?;
        if total <= 0.0 || frames.handles.len() < 2 {
            return frames.handles.first().cloned();
        }
        let t = start.elapsed().as_secs_f32() % total;
        let idx = frames
            .ends
            .iter()
            .position(|&e| t < e)
            .unwrap_or(frames.handles.len() - 1);
        frames.handles.get(idx).cloned()
    }

    /// True once any animated GIF is loaded (drives the redraw tick).
    pub fn has_animation(&self) -> bool {
        self.cache
            .borrow()
            .values()
            .any(|f| f.as_ref().is_some_and(|f| f.handles.len() > 1))
    }

    /// Drop a cached GIF so the next `handle()` reloads from disk (after regen).
    pub fn invalidate(&self, path: &Path) {
        self.cache.borrow_mut().remove(path);
    }

    /// Clear all decoded previews (library refresh).
    pub fn clear(&self) {
        self.cache.borrow_mut().clear();
    }
}

fn decode(path: &Path) -> Option<Frames> {
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gif"))
    {
        return None;
    }
    use image::AnimationDecoder;
    let file = std::fs::File::open(path).ok()?;
    let decoder = image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file)).ok()?;

    let mut handles = Vec::new();
    let mut ends = Vec::new();
    let mut acc = 0.0f32;
    // Cap frames so a long preview can't balloon memory across a big library.
    for frame in decoder.into_frames().take(120) {
        let Ok(frame) = frame else { break };
        let (num, den) = frame.delay().numer_denom_ms();
        // GIF delays of 0/10ms mean "as fast as possible"; browsers clamp to 100ms.
        let ms = if den == 0 {
            100.0
        } else {
            num as f32 / den as f32
        };
        acc += (if ms < 20.0 { 100.0 } else { ms }) / 1000.0;
        ends.push(acc);
        let buf = frame.into_buffer();
        handles.push(Handle::from_rgba(buf.width(), buf.height(), buf.into_raw()));
    }
    (!handles.is_empty()).then_some(Frames { handles, ends })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real animated workshop preview must decode to multiple frames, and the
    /// displayed frame must advance with wall-clock time.
    #[test]
    fn animated_gif_advances_frames() {
        let Some(gif) = glob_first_preview_gif() else {
            eprintln!("no preview.gif in cache; skipping");
            return;
        };
        let frames = decode(&gif).expect("decodes");
        assert!(
            frames.handles.len() > 1,
            "expected an animated gif, got {} frame(s)",
            frames.handles.len()
        );

        let cache = GifCache::default();
        let first = cache.handle(&gif).expect("frame");
        let total = *frames.ends.last().unwrap();
        // Sleep past a frame boundary (but stay inside the loop when possible).
        let step = (frames.ends[0] + 0.01).min(total);
        std::thread::sleep(std::time::Duration::from_secs_f32(step));
        let later = cache.handle(&gif).expect("frame");
        assert_ne!(first.id(), later.id(), "frame did not advance");
        assert!(cache.has_animation());
    }

    /// Non-GIF previews return None so callers fall back to a static handle.
    #[test]
    fn non_gif_returns_none() {
        let cache = GifCache::default();
        assert!(cache.handle(Path::new("/tmp/definitely.png")).is_none());
    }

    fn glob_first_preview_gif() -> Option<PathBuf> {
        let base = dirs_cache()?.join("wallengine/we");
        for e in std::fs::read_dir(base).ok()?.flatten() {
            let p = e.path().join("preview.gif");
            if p.is_file() {
                return Some(p);
            }
        }
        None
    }

    fn dirs_cache() -> Option<PathBuf> {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
    }
}
