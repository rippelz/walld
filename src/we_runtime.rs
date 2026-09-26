//! Load Wallpaper Engine workshop packages into walld content modes.
//! Scenes go through the full WeSceneRuntime pipeline (TEX, ortho, particles, effects).

use crate::video::VideoDecoder;
use std::path::{Path, PathBuf};
use wallengine_we::project::{Project, WallpaperType};
use wallengine_we::scene::WeSceneRuntime;

pub enum WeContent {
    Video {
        title: String,
        path: PathBuf,
        decoder: VideoDecoder,
    },
    /// Full WE scene runtime (images + particles + effects).
    Scene {
        runtime: WeSceneRuntime,
    },
}

impl WeContent {
    pub fn title(&self) -> &str {
        match self {
            Self::Video { title, .. } => title,
            Self::Scene { runtime } => &runtime.title,
        }
    }

    pub fn kind_tag(&self) -> &'static str {
        match self {
            Self::Video { decoder, .. } if decoder.backend == crate::video::VideoBackend::Web => "web",
            Self::Video { .. } => "video",
            Self::Scene { .. } => "we",
        }
    }
}

pub fn load_we_dir(dir: &Path) -> Result<WeContent, String> {
    let pj = dir.join("project.json");
    if !pj.is_file() {
        return Err(format!("no project.json in {}", dir.display()));
    }
    let project = Project::load(&pj)?;
    let id = dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "we".into());

    match project.wallpaper_type {
        WallpaperType::Video => load_video(dir, &project),
        WallpaperType::Scene | WallpaperType::Unknown => {
            if dir.join("scene.pkg").is_file() || dir.join("scene.json").is_file() {
                load_scene(dir, &project, &id)
            } else if let Some(v) = find_video(dir) {
                Ok(WeContent::Video {
                    title: project.title.clone(),
                    path: v.clone(),
                    decoder: VideoDecoder::start(&v, 60)?,
                })
            } else {
                load_scene(dir, &project, &id)
            }
        }
        WallpaperType::Web => load_web(dir, &project),
        WallpaperType::Application => Err(
            "Wallpaper Engine Application projects are not supported by walld".into(),
        ),
    }
}

fn load_video(dir: &Path, project: &Project) -> Result<WeContent, String> {
    let path = if !project.file.is_empty() {
        let p = dir.join(&project.file);
        if p.is_file() {
            p
        } else {
            find_video(dir).ok_or_else(|| "video file missing".to_string())?
        }
    } else {
        find_video(dir).ok_or_else(|| "video file missing".to_string())?
    };
    let decoder = VideoDecoder::start(&path, 60)?;
    Ok(WeContent::Video {
        title: project.title.clone(),
        path,
        decoder,
    })
}

fn load_web(dir: &Path, project: &Project) -> Result<WeContent, String> {
    let path = if !project.file.is_empty() {
        dir.join(&project.file)
    } else {
        dir.join("index.html")
    };
    let decoder = VideoDecoder::start_web(&path, 15, 1920)?;
    Ok(WeContent::Video {
        title: project.title.clone(),
        path,
        decoder,
    })
}

fn load_scene(dir: &Path, project: &Project, id: &str) -> Result<WeContent, String> {
    let runtime = WeSceneRuntime::load(dir, id, &project.title)?;
    Ok(WeContent::Scene { runtime })
}

fn find_video(dir: &Path) -> Option<PathBuf> {
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
