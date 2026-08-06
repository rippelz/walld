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
}
