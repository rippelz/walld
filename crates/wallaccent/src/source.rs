//! Work out which wallpaper is on screen and what colour it is.

use std::path::{Path, PathBuf};
use wallengine_we::{dominant_color, status_snapshot, Project};

/// Where a colour came from — shown by `wallaccent status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Wallpaper Engine's author-declared `schemecolor`.
    Scheme,
    /// Sampled from the wallpaper (or its preview) image.
    Image,
    /// Pinned by the user with `wallaccent set`.
    Pinned,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Self::Scheme => "WE scheme color",
            Self::Image => "wallpaper image",
            Self::Pinned => "pinned",
        }
    }
}

/// Which colours a wallpaper is allowed to contribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Prefer {
    /// Scheme colour, falling back to the image.
    Auto,
    /// Only the author's `schemecolor`.
    Scheme,
    /// Always sample the image.
    Image,
}

#[derive(Debug, Clone)]
pub struct Found {
    pub monitor: String,
    pub path: PathBuf,
    pub color: [f32; 3],
    pub origin: Origin,
    /// Human name for the wallpaper (WE title, or the file stem).
    pub title: String,
}

/// The wallpaper currently on `monitor` (empty = first one walld reports).
///
/// walld is the source of truth; `hyprpaper.conf` is the fallback for a
/// session where the daemon isn't up yet (e.g. straight after login).
pub fn current_wallpaper(monitor: &str) -> Option<(String, PathBuf)> {
    let st = status_snapshot();
    let mut pick: Option<(String, PathBuf)> = None;
    for (mon, path) in &st.monitor_paths {
        if path.trim().is_empty() {
            continue;
        }
        if !monitor.is_empty() && mon != monitor {
            continue;
        }
        pick = Some((mon.clone(), PathBuf::from(path)));
        break;
    }
    pick.or_else(|| hyprpaper_wallpaper(monitor))
}

/// `monitor = …` / `path = …` pairs out of the hyprpaper-format config.
fn hyprpaper_wallpaper(monitor: &str) -> Option<(String, PathBuf)> {
    let conf = home().join(".config/hypr/hyprpaper.conf");
    let text = std::fs::read_to_string(conf).ok()?;
    let mut cur_mon = String::new();
    let mut first: Option<(String, PathBuf)> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        match k {
            "monitor" => cur_mon = v.to_string(),
            "path" if !v.is_empty() => {
                let p = expand(v);
                if !monitor.is_empty() && cur_mon == monitor {
                    return Some((cur_mon.clone(), p));
                }
                first.get_or_insert((cur_mon.clone(), p));
            }
            _ => {}
        }
    }
    first
}

/// Colour for whatever `path` points at: a Wallpaper Engine package directory,
/// a file inside one, or a plain image.
pub fn color_for(path: &Path, prefer: Prefer) -> Option<(([f32; 3], Origin), String)> {
    if let Some(dir) = we_package_dir(path) {
        let project = Project::load(&dir.join("project.json")).ok();
        let title = project
            .as_ref()
            .map(|p| p.title.clone())
            .unwrap_or_else(|| file_label(&dir));
        if prefer != Prefer::Image {
            if let Some(rgb) = project.as_ref().and_then(|p| p.scheme_color()) {
                return Some(((rgb, Origin::Scheme), title));
            }
            if prefer == Prefer::Scheme {
                return None;
            }
        }
        let preview = project.as_ref().and_then(|p| p.preview_path(&dir));
        let rgb = dominant_color(&preview?)?;
        return Some(((rgb, Origin::Image), title));
    }
    if prefer == Prefer::Scheme {
        return None;
    }
    // A plain image wallpaper (the classic hyprpaper path).
    if path.is_file() {
        if let Some(rgb) = dominant_color(path) {
            return Some(((rgb, Origin::Image), file_label(path)));
        }
        // Video wallpapers can't be decoded here; a sibling still often can.
        if let Some(still) = sibling_still(path) {
            if let Some(rgb) = dominant_color(&still) {
                return Some(((rgb, Origin::Image), file_label(path)));
            }
        }
    }
    None
}

/// Walk up from `path` to the directory holding a `project.json`.
fn we_package_dir(path: &Path) -> Option<PathBuf> {
    let mut cur = if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(|p| p.to_path_buf())
    };
    // Two levels is enough: `<pkg>/scene.pkg` or `<pkg>/`.
    for _ in 0..2 {
        let dir = cur?;
        if dir.join("project.json").is_file() {
            return Some(dir);
        }
        cur = dir.parent().map(|p| p.to_path_buf());
    }
    None
}

/// `wallpaper.mp4` → `wallpaper.jpg`/`.png`/… next to it.
fn sibling_still(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?;
    let dir = path.parent()?;
    for ext in ["jpg", "jpeg", "png", "webp", "gif"] {
        let p = dir.join(stem).with_extension(ext);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn file_label(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()))
}

pub fn expand(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(p),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_handles_tilde() {
        assert_eq!(expand("~/x/y"), home().join("x/y"));
        assert_eq!(expand("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn finds_the_package_dir_from_a_file_inside_it() {
        let dir = std::env::temp_dir().join(format!("wallaccent-pkg-{}", std::process::id()));
        let pkg = dir.join("123456");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("project.json"), "{}").unwrap();
        std::fs::write(pkg.join("scene.pkg"), "x").unwrap();

        assert_eq!(we_package_dir(&pkg).as_deref(), Some(pkg.as_path()));
        assert_eq!(
            we_package_dir(&pkg.join("scene.pkg")).as_deref(),
            Some(pkg.as_path())
        );
        // A plain wallpaper folder is not a package.
        assert_eq!(we_package_dir(&dir), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sibling_still_finds_a_video_thumbnail() {
        let dir = std::env::temp_dir().join(format!("wallaccent-vid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let vid = dir.join("clip.mp4");
        std::fs::write(&vid, "x").unwrap();
        assert_eq!(sibling_still(&vid), None);
        let still = dir.join("clip.png");
        std::fs::write(&still, "x").unwrap();
        assert_eq!(sibling_still(&vid), Some(still));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scheme_only_refuses_to_sample_images() {
        let dir = std::env::temp_dir().join(format!("wallaccent-scheme-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A package with no schemecolor and no preview.
        std::fs::write(dir.join("project.json"), r#"{"title":"T"}"#).unwrap();
        assert!(color_for(&dir, Prefer::Scheme).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
