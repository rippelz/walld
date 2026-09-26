use crate::project::{Project, WallpaperType};
use crate::{wallengine_projects_dir, we_myprojects_dir, workshop_dir};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct WeEntry {
    pub id: String,
    pub dir: PathBuf,
    pub project: Project,
    pub preview: Option<PathBuf>,
    pub source: WeSource,
    pub has_scene_pkg: bool,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeSource {
    Workshop,
    MyProjects,
    LocalFolder,
}

impl WeSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Workshop => "Workshop",
            Self::MyProjects => "My projects",
            Self::LocalFolder => "Local",
        }
    }
}

pub fn scan_all() -> Vec<WeEntry> {
    let mut out = Vec::new();
    out.extend(scan_dir(&workshop_dir(), WeSource::Workshop));
    out.extend(scan_dir(&we_myprojects_dir(), WeSource::MyProjects));
    // wallstudio forks / editable WE package trees
    out.extend(scan_dir(
        &wallengine_projects_dir(),
        WeSource::LocalFolder,
    ));
    out.sort_by(|a, b| {
        // newer workshop ids first-ish, then title
        b.id.cmp(&a.id)
            .then_with(|| a.project.title.to_lowercase().cmp(&b.project.title.to_lowercase()))
    });
    out
}

fn scan_dir(root: &std::path::Path, source: WeSource) -> Vec<WeEntry> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for ent in rd.flatten() {
        let dir = ent.path();
        if !dir.is_dir() {
            continue;
        }
        let pj = dir.join("project.json");
        if !pj.is_file() {
            continue;
        }
        let Ok(project) = Project::load(&pj) else {
            continue;
        };
        let id = dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown".into());
        let preview = project.preview_path(&dir);
        let has_scene_pkg = dir.join("scene.pkg").is_file();
        let size_bytes = dir_size(&dir);
        // fix type inference for video if file is mp4
        let mut project = project;
        if project.wallpaper_type == WallpaperType::Unknown {
            if project.file.ends_with(".mp4") || project.file.ends_with(".webm") {
                project.wallpaper_type = WallpaperType::Video;
            } else if has_scene_pkg || project.file.ends_with("scene.json") {
                project.wallpaper_type = WallpaperType::Scene;
            }
        }
        out.push(WeEntry {
            id,
            dir,
            project,
            preview,
            source,
            has_scene_pkg,
            size_bytes,
        });
    }
    out
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    let Ok(rd) = std::fs::read_dir(path) else {
        return 0;
    };
    for ent in rd.flatten() {
        if let Ok(m) = ent.metadata() {
            if m.is_file() {
                total = total.saturating_add(m.len());
            }
        }
    }
    total
}

pub fn format_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    const GB: f64 = MB * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1} GB", b / GB)
    } else if b >= MB {
        format!("{:.0} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}
