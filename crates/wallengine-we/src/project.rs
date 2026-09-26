use serde_json::Value;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallpaperType {
    Scene,
    Video,
    Web,
    Application,
    Unknown,
}

impl WallpaperType {
    pub fn from_str_loose(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "scene" => Self::Scene,
            "video" => Self::Video,
            "web" => Self::Web,
            "application" | "app" => Self::Application,
            _ => Self::Unknown,
        }
    }

    pub fn as_label(self) -> &'static str {
        match self {
            Self::Scene => "Scene",
            Self::Video => "Video",
            Self::Web => "Web",
            Self::Application => "App",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Project {
    pub title: String,
    pub description: String,
    pub wallpaper_type: WallpaperType,
    /// Relative to wallpaper folder (scene.json, foo.mp4, …)
    pub file: String,
    pub preview: Option<String>,
    pub tags: Vec<String>,
    pub workshop_id: Option<String>,
    pub content_rating: Option<String>,
    pub raw: Value,
}

impl Project {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let title = raw
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("Untitled")
            .to_string();
        let description = raw
            .get("description")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let ty = raw
            .get("type")
            .and_then(|v| v.as_str())
            .map(WallpaperType::from_str_loose)
            .unwrap_or(WallpaperType::Unknown);
        let file = raw
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let preview = raw
            .get("preview")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let tags = raw
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let workshop_id = raw
            .get("workshopid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .or_else(|| {
                // folder name often is the id
                path.parent()
                    .and_then(|p| p.file_name())
                    .map(|s| s.to_string_lossy().into_owned())
            });
        let content_rating = raw
            .get("contentrating")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok(Self {
            title,
            description,
            wallpaper_type: ty,
            file,
            preview,
            tags,
            workshop_id,
            content_rating,
            raw,
        })
    }

    pub fn preview_path(&self, dir: &Path) -> Option<PathBuf> {
        if let Some(p) = &self.preview {
            let full = dir.join(p);
            if full.is_file() {
                return Some(full);
            }
        }
        for name in ["preview.jpg", "preview.png", "preview.gif", "preview.webp"] {
            let full = dir.join(name);
            if full.is_file() {
                return Some(full);
            }
        }
        None
    }

    pub fn content_path(&self, dir: &Path) -> PathBuf {
        if self.file.is_empty() {
            dir.to_path_buf()
        } else {
            dir.join(&self.file)
        }
    }

    pub fn is_scene_pkg(&self, dir: &Path) -> bool {
        dir.join("scene.pkg").is_file()
    }

    /// Wallpaper Engine's `schemecolor` property — the author's declared
    /// accent for this wallpaper (`general.properties.schemecolor.value`,
    /// "r g b" in 0..1). `None` when the wallpaper doesn't declare one.
    ///
    /// Pure white / pure black are treated as "no scheme color": WE ships them
    /// as filler defaults on wallpapers that never picked a real accent.
    pub fn scheme_color(&self) -> Option<[f32; 3]> {
        let v = self
            .raw
            .pointer("/general/properties/schemecolor/value")
            .or_else(|| self.raw.pointer("/general/properties/schemecolor"))?;
        let rgb = parse_rgb_triplet(v)?;
        if rgb.iter().all(|c| *c >= 0.999) || rgb.iter().all(|c| *c <= 0.001) {
            return None;
        }
        Some(rgb)
    }
}

/// Parse a WE colour value: `"0.5 0.25 1"` (or a `[r, g, b]` array).
pub fn parse_rgb_triplet(v: &Value) -> Option<[f32; 3]> {
    let parts: Vec<f32> = match v {
        Value::String(s) => s
            .split_whitespace()
            .filter_map(|x| x.parse::<f32>().ok())
            .collect(),
        Value::Array(a) => a.iter().filter_map(|x| x.as_f64().map(|f| f as f32)).collect(),
        _ => return None,
    };
    if parts.len() < 3 || parts.iter().any(|c| !c.is_finite()) {
        return None;
    }
    Some([
        parts[0].clamp(0.0, 1.0),
        parts[1].clamp(0.0, 1.0),
        parts[2].clamp(0.0, 1.0),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn project_with(raw: Value) -> Project {
        Project {
            title: "t".into(),
            description: String::new(),
            wallpaper_type: WallpaperType::Scene,
            file: String::new(),
            preview: None,
            tags: Vec::new(),
            workshop_id: None,
            content_rating: None,
            raw,
        }
    }

    #[test]
    fn scheme_color_reads_we_property() {
        let p = project_with(json!({
            "general": { "properties": { "schemecolor": {
                "type": "color", "value": "0.23922 0.42353 0.56471"
            }}}
        }));
        let [r, g, b] = p.scheme_color().expect("scheme color");
        assert!((r - 0.23922).abs() < 1e-5);
        assert!((g - 0.42353).abs() < 1e-5);
        assert!((b - 0.56471).abs() < 1e-5);
    }

    #[test]
    fn scheme_color_rejects_filler_white_and_black() {
        let white = project_with(json!({
            "general": { "properties": { "schemecolor": { "value": "1 1 1" }}}
        }));
        assert_eq!(white.scheme_color(), None);
        let black = project_with(json!({
            "general": { "properties": { "schemecolor": { "value": "0 0 0" }}}
        }));
        assert_eq!(black.scheme_color(), None);
    }

    #[test]
    fn scheme_color_absent_or_malformed_is_none() {
        assert_eq!(project_with(json!({})).scheme_color(), None);
        let bad = project_with(json!({
            "general": { "properties": { "schemecolor": { "value": "nope" }}}
        }));
        assert_eq!(bad.scheme_color(), None);
        let short = project_with(json!({
            "general": { "properties": { "schemecolor": { "value": "0.5 0.5" }}}
        }));
        assert_eq!(short.scheme_color(), None);
    }

    #[test]
    fn scheme_color_accepts_array_form_and_clamps() {
        let p = project_with(json!({
            "general": { "properties": { "schemecolor": { "value": [1.4, 0.5, -0.2] }}}
        }));
        assert_eq!(p.scheme_color(), Some([1.0, 0.5, 0.0]));
    }
}
