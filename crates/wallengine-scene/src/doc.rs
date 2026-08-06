use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// On-disk scene document (schema v1).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SceneDocument {
    #[serde(default = "default_schema")]
    pub schema: u32,
    #[serde(default)]
    pub name: String,
    /// RGBA clear color 0..1
    #[serde(default = "default_clear")]
    pub clear: [f32; 4],
    #[serde(default)]
    pub layers: Vec<LayerDoc>,
    #[serde(default)]
    pub transition: TransitionDoc,
}

fn default_schema() -> u32 {
    1
}
fn default_clear() -> [f32; 4] {
    [0.0, 0.0, 0.0, 1.0]
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayerDoc {
    Image {
        #[serde(default)]
        id: String,
        path: String,
        #[serde(default)]
        fit: FitMode,
        #[serde(default = "one_f32")]
        opacity: f32,
    },
    Color {
        #[serde(default)]
        id: String,
        /// RGBA 0..1
        color: [f32; 4],
        #[serde(default = "one_f32")]
        opacity: f32,
    },
    Particles {
        #[serde(default)]
        id: String,
        #[serde(default)]
        preset: ParticlePreset,
        #[serde(default = "default_count")]
        count: u32,
        #[serde(default = "default_speed")]
        speed: f32,
        #[serde(default = "one_f32")]
        opacity: f32,
    },
}

fn one_f32() -> f32 {
    1.0
}
fn default_count() -> u32 {
    600
}
fn default_speed() -> f32 {
    0.4
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FitMode {
    #[default]
    Cover,
    Contain,
    Fill,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ParticlePreset {
    #[default]
    Snow,
    Dust,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransitionDoc {
    #[serde(default)]
    pub mode: TransitionMode,
    #[serde(default = "default_ms")]
    pub ms: u32,
}

impl Default for TransitionDoc {
    fn default() -> Self {
        Self {
            mode: TransitionMode::Wipe,
            ms: 480,
        }
    }
}

fn default_ms() -> u32 {
    480
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransitionMode {
    Snap,
    #[default]
    Wipe,
}

impl SceneDocument {
    pub fn load_file(path: &Path) -> Result<(Self, PathBuf), String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        let doc: SceneDocument =
            serde_json::from_str(&text).map_err(|e| format!("parse {}: {e}", path.display()))?;
        if doc.schema != 1 {
            return Err(format!("unsupported scene schema {}", doc.schema));
        }
        let base = path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        Ok((doc, base))
    }

    /// True if any layer needs continuous animation.
    pub fn is_animated(&self) -> bool {
        self.layers.iter().any(|l| matches!(l, LayerDoc::Particles { .. }))
    }

    pub fn resolve_path(base: &Path, p: &str) -> PathBuf {
        let p = p.trim();
        if let Some(rest) = p.strip_prefix("~/") {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/home/beebit".into());
            return PathBuf::from(home).join(rest);
        }
        let path = PathBuf::from(p);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    }
}
