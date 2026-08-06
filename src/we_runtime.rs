//! Load Wallpaper Engine workshop packages into walld content modes.

use crate::video::VideoDecoder;
use std::path::{Path, PathBuf};
use wallengine_we::pkg::ensure_unpacked;
use wallengine_we::project::{Project, WallpaperType};
use wallengine_we::tex::decode_tex_to_rgba;

#[derive(Debug, Clone)]
pub struct WeImageLayer {
    pub path_hint: String,
    /// Decoded pixels ready for upload (consumed once).
    pub rgba: Option<(u32, u32, Vec<u8>)>,
    pub origin: (f32, f32),
    pub size: (f32, f32),
    pub scale: (f32, f32),
    pub visible: bool,
}

pub enum WeContent {
    Video {
        title: String,
        path: PathBuf,
        decoder: VideoDecoder,
    },
    /// 2D image stack from a WE scene (best-effort full-scene path without external engines).
    Scene {
        title: String,
        id: String,
        layers: Vec<WeImageLayer>,
        /// Optional particle snow if WE particle names suggest snow.
        snow: bool,
    },
}

impl WeContent {
    pub fn title(&self) -> &str {
        match self {
            Self::Video { title, .. } | Self::Scene { title, .. } => title,
        }
    }

    pub fn kind_tag(&self) -> &'static str {
        match self {
            Self::Video { .. } => "video",
            Self::Scene { .. } => "we",
        }
    }
}

pub fn load_we_dir(dir: &Path) -> Result<WeContent, String> {
    let pj = dir.join("project.json");
    if !pj.is_file() {
        // maybe path is workshop id under default workshop
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
                    decoder: VideoDecoder::start(&v, 30)?,
                })
            } else {
                load_scene(dir, &project, &id)
            }
        }
        WallpaperType::Web | WallpaperType::Application => Err(
            "Web/Application Wallpaper Engine types are not supported yet in walld".into(),
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
    let decoder = VideoDecoder::start(&path, 30)?;
    Ok(WeContent::Video {
        title: project.title.clone(),
        path,
        decoder,
    })
}

fn load_scene(dir: &Path, project: &Project, id: &str) -> Result<WeContent, String> {
    let unpacked = if dir.join("scene.pkg").is_file() {
        ensure_unpacked(dir, id).map_err(|e| e.to_string())?
    } else {
        dir.to_path_buf()
    };
    let scene_path = unpacked.join("scene.json");
    if !scene_path.is_file() {
        return Err("scene.json missing after unpack".into());
    }
    let scene: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&scene_path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;

    let mut snow = false;
    let mut layers = Vec::new();
    let objects = scene
        .get("objects")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for obj in &objects {
        if let Some(p) = obj.get("particle").and_then(|v| v.as_str()) {
            let pl = p.to_ascii_lowercase();
            if pl.contains("snow") || pl.contains("leaf") || pl.contains("ember") {
                snow = true;
            }
            continue;
        }
        let visible = obj
            .get("visible")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        if !visible {
            continue;
        }
        let image_ref = match obj.get("image").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() && s != "null" => s,
            _ => continue,
        };
        // resolve model json -> material -> tex name
        let rgba = resolve_image_pixels(&unpacked, image_ref);
        let origin = parse_vec2(obj.get("origin")).unwrap_or((0.0, 0.0));
        let size = parse_vec2(obj.get("size")).unwrap_or((1920.0, 1080.0));
        let scale = parse_vec2(obj.get("scale")).unwrap_or((1.0, 1.0));
        layers.push(WeImageLayer {
            path_hint: image_ref.to_string(),
            rgba,
            origin,
            size,
            scale,
            visible: true,
        });
    }

    // Prefer largest layer as primary (background)
    layers.sort_by(|a, b| {
        let aa = a.size.0 * a.size.1 * a.scale.0 * a.scale.1;
        let bb = b.size.0 * b.size.1 * b.scale.0 * b.scale.1;
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Keep only layers that decoded; if none, try any .tex in materials
    if layers.iter().all(|l| l.rgba.is_none()) {
        if let Some((w, h, px)) = find_any_tex(&unpacked) {
            layers.insert(
                0,
                WeImageLayer {
                    path_hint: "materials/*.tex".into(),
                    rgba: Some((w, h, px)),
                    origin: (w as f32 / 2.0, h as f32 / 2.0),
                    size: (w as f32, h as f32),
                    scale: (1.0, 1.0),
                    visible: true,
                },
            );
        }
    }

    let decoded = layers.iter().filter(|l| l.rgba.is_some()).count();
    if decoded == 0 {
        return Err(
            "could not decode any image layers from this scene (tex format unsupported or empty)"
                .into(),
        );
    }
    log::info!(
        "WE scene «{}»: {} objects → {} image layers ({} decoded), snow={snow}",
        project.title,
        objects.len(),
        layers.len(),
        decoded
    );

    Ok(WeContent::Scene {
        title: project.title.clone(),
        id: id.to_string(),
        layers,
        snow,
    })
}

fn resolve_image_pixels(root: &Path, image_ref: &str) -> Option<(u32, u32, Vec<u8>)> {
    // image_ref like models/foo.json
    let model_path = root.join(image_ref);
    let material_rel = if model_path.is_file() {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&model_path).ok()?).ok()?;
        v.get("material")?.as_str()?.to_string()
    } else {
        // maybe direct material
        image_ref.to_string()
    };
    let mat_path = root.join(&material_rel);
    let tex_stem = if mat_path.is_file() {
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&mat_path).ok()?).ok()?;
        // passes[0].textures[0]
        v.get("passes")
            .and_then(|p| p.as_array())
            .and_then(|a| a.first())
            .and_then(|p| p.get("textures"))
            .and_then(|t| t.as_array())
            .and_then(|a| a.first())
            .and_then(|t| t.as_str())
            .map(|s| s.to_string())
    } else {
        None
    };
    let tex_stem = tex_stem.unwrap_or_else(|| {
        Path::new(&material_rel)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    });
    // materials/NAME.tex or materials/foo/NAME.tex
    let candidates = [
        root.join(format!("materials/{tex_stem}.tex")),
        root.join(material_rel).with_extension("tex"),
        root.join(format!("{tex_stem}.tex")),
    ];
    for c in candidates {
        if c.is_file() {
            if let Ok(data) = std::fs::read(&c) {
                if let Ok(img) = decode_tex_to_rgba(&data) {
                    return Some(img);
                }
            }
        }
    }
    // walk materials for matching stem
    let mat_dir = root.join("materials");
    if let Ok(rd) = std::fs::read_dir(mat_dir) {
        for ent in rd.flatten() {
            let p = ent.path();
            if p.extension().and_then(|e| e.to_str()) == Some("tex") {
                if p.file_stem().map(|s| s.to_string_lossy()) == Some(tex_stem.as_str().into())
                    || p.file_name()
                        .map(|s| s.to_string_lossy().contains(&tex_stem))
                        .unwrap_or(false)
                {
                    if let Ok(data) = std::fs::read(&p) {
                        if let Ok(img) = decode_tex_to_rgba(&data) {
                            return Some(img);
                        }
                    }
                }
            }
        }
    }
    None
}

fn find_any_tex(root: &Path) -> Option<(u32, u32, Vec<u8>)> {
    let mut best: Option<(u64, u32, u32, Vec<u8>)> = None;
    fn walk(dir: &Path, best: &mut Option<(u64, u32, u32, Vec<u8>)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                walk(&p, best);
            } else if p.extension().and_then(|e| e.to_str()) == Some("tex") {
                if let Ok(data) = std::fs::read(&p) {
                    if let Ok((w, h, rgba)) = decode_tex_to_rgba(&data) {
                        let score = (w as u64) * (h as u64);
                        if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                            *best = Some((score, w, h, rgba));
                        }
                    }
                }
            }
        }
    }
    walk(root, &mut best);
    best.map(|(_, w, h, r)| (w, h, r))
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

fn parse_vec2(v: Option<&serde_json::Value>) -> Option<(f32, f32)> {
    let v = v?;
    if let Some(s) = v.as_str() {
        let mut it = s.split_whitespace();
        let x = it.next()?.parse().ok()?;
        let y = it.next()?.parse().ok()?;
        return Some((x, y));
    }
    if let Some(obj) = v.as_object() {
        // { "value": "1 1 1" }
        if let Some(s) = obj.get("value").and_then(|x| x.as_str()) {
            let mut it = s.split_whitespace();
            let x = it.next()?.parse().ok()?;
            let y = it.next()?.parse().ok()?;
            return Some((x, y));
        }
    }
    None
}
