//! Asset resolution for Wallpaper Engine packages.
//!
//! Looks up files by relative path across:
//! 1. Unpacked scene package (or workshop folder)
//! 2. Steam Wallpaper Engine `assets/` directory
//! 3. Other workshop packages referenced by id (effects/workshop/…)

use crate::paths::{we_assets_dir, workshop_dir};
use crate::tex::{decode_tex, DecodedTex, TexError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tex: {0}")]
    Tex(#[from] TexError),
    #[error("{0}")]
    Other(String),
}

/// Resolves package-relative paths the way Wallpaper Engine does.
#[derive(Debug, Clone)]
pub struct AssetResolver {
    /// Unpacked scene root (has scene.json, materials/, …).
    pub package_root: PathBuf,
    /// Steam WE install assets (effects, shaders, particles, materials, …).
    pub we_assets: PathBuf,
    /// Workshop content root (…/431960).
    pub workshop: PathBuf,
    /// Optional extra search roots.
    pub extra: Vec<PathBuf>,
}

impl AssetResolver {
    pub fn new(package_root: impl Into<PathBuf>) -> Self {
        Self {
            package_root: package_root.into(),
            we_assets: we_assets_dir(),
            workshop: workshop_dir(),
            extra: Vec::new(),
        }
    }

    /// Candidate absolute paths for a relative asset name.
    pub fn candidates(&self, rel: &str) -> Vec<PathBuf> {
        let rel = rel.trim().trim_start_matches(['/', '\\']);
        if rel.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();

        // Direct under package
        out.push(self.package_root.join(rel));

        // materials/ prefix helpers
        if !rel.starts_with("materials/") {
            out.push(self.package_root.join("materials").join(rel));
        }

        // WE assets
        out.push(self.we_assets.join(rel));
        if !rel.starts_with("materials/") {
            out.push(self.we_assets.join("materials").join(rel));
        }

        // shaders live under assets/shaders
        if rel.starts_with("shaders/") {
            out.push(self.we_assets.join(rel));
        } else if rel.ends_with(".frag") || rel.ends_with(".vert") || rel.ends_with(".h") {
            out.push(self.we_assets.join("shaders").join(rel));
        }

        // effects/… often nested under assets/effects/<name>/…
        if let Some(rest) = rel.strip_prefix("effects/") {
            // assets/effects/waterflow/… embeds full tree
            let first = rest.split('/').next().unwrap_or("");
            if !first.is_empty() {
                out.push(self.we_assets.join("effects").join(first).join(rel));
                out.push(
                    self.we_assets
                        .join("effects")
                        .join(first)
                        .join(rest.trim_start_matches(|c| c != '/').trim_start_matches('/')),
                );
            }
            // effect.json path: effects/waterflow/effect.json
            out.push(self.we_assets.join(rel));
        }

        // Workshop cross-references: effects/workshop/<id>/… or particles/workshop/<id>/…
        if let Some(id) = extract_workshop_id(rel) {
            let wp = self.workshop.join(id);
            out.push(wp.join(rel));
            // strip leading kind
            for prefix in ["effects/workshop/", "particles/workshop/", "materials/workshop/", "models/workshop/"] {
                if let Some(rest) = rel.strip_prefix(prefix) {
                    // rest starts with id/
                    if let Some(after_id) = rest.strip_prefix(id).and_then(|s| s.strip_prefix('/')) {
                        out.push(wp.join(after_id));
                        out.push(wp.join("materials").join(after_id));
                    }
                    out.push(wp.join(rest));
                }
            }
        }

        for e in &self.extra {
            out.push(e.join(rel));
        }

        // de-dup while preserving order
        let mut seen = std::collections::HashSet::new();
        out.retain(|p| seen.insert(p.clone()));
        out
    }

    /// Resolve first existing file for `rel`.
    pub fn resolve(&self, rel: &str) -> Option<PathBuf> {
        for c in self.candidates(rel) {
            if c.is_file() {
                return Some(c);
            }
            // try with .tex if no extension
            if c.extension().is_none() {
                let tex = c.with_extension("tex");
                if tex.is_file() {
                    return Some(tex);
                }
                let json = c.with_extension("json");
                if json.is_file() {
                    return Some(json);
                }
            }
        }
        // Try stem.tex under materials
        let stem = Path::new(rel).file_stem().and_then(|s| s.to_str()).unwrap_or(rel);
        for base in [&self.package_root, &self.we_assets] {
            let p = base.join("materials").join(format!("{stem}.tex"));
            if p.is_file() {
                return Some(p);
            }
            // recursive shallow search by filename
            if let Some(found) = find_named(base, &format!("{stem}.tex"), 4) {
                return Some(found);
            }
        }
        None
    }

    pub fn read_bytes(&self, rel: &str) -> Result<Vec<u8>, AssetError> {
        let path = self
            .resolve(rel)
            .ok_or_else(|| AssetError::NotFound(rel.to_string()))?;
        Ok(std::fs::read(path)?)
    }

    pub fn read_string(&self, rel: &str) -> Result<String, AssetError> {
        let path = self
            .resolve(rel)
            .ok_or_else(|| AssetError::NotFound(rel.to_string()))?;
        Ok(std::fs::read_to_string(path)?)
    }

    pub fn read_json(&self, rel: &str) -> Result<serde_json::Value, AssetError> {
        let s = self.read_string(rel)?;
        serde_json::from_str(&s).map_err(|e| AssetError::Other(format!("{rel}: {e}")))
    }

    /// Load a texture by material stem or path (with or without `.tex`).
    pub fn load_tex(&self, name: &str) -> Result<DecodedTex, AssetError> {
        let rel = if name.ends_with(".tex") {
            name.to_string()
        } else if name.contains('/') {
            // materials/foo or masks/bar
            if name.ends_with(".json") {
                name.to_string()
            } else {
                format!("{name}.tex")
            }
        } else {
            format!("materials/{name}.tex")
        };

        // try several variants
        let variants = [
            rel.clone(),
            format!("materials/{name}.tex"),
            format!("materials/{name}"),
            format!("{name}.tex"),
            name.to_string(),
            // masks referenced without materials/ prefix
            format!("materials/masks/{}.tex", Path::new(name).file_name().and_then(|s| s.to_str()).unwrap_or(name)),
        ];
        for v in &variants {
            if let Some(path) = self.resolve(v) {
                let data = std::fs::read(&path)?;
                return Ok(decode_tex(&data)?);
            }
        }
        // last resort: search package materials tree
        if let Some(path) = find_named(&self.package_root.join("materials"), &format!("{}.tex", Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or(name)), 6) {
            let data = std::fs::read(&path)?;
            return Ok(decode_tex(&data)?);
        }
        Err(AssetError::NotFound(name.to_string()))
    }

    /// Read a WE shader source (`effects/waterflow` → shaders/effects/waterflow.frag).
    pub fn load_shader(&self, name: &str, stage: &str) -> Result<String, AssetError> {
        // name like "effects/waterflow"
        let rel = if name.ends_with(stage) {
            name.to_string()
        } else {
            format!("shaders/{name}.{stage}")
        };
        if let Ok(s) = self.read_string(&rel) {
            return Ok(s);
        }
        // effect-local: effects/waterflow/shaders/effects/waterflow.frag
        let base = name.trim_start_matches("shaders/");
        let effect = base.split('/').next().unwrap_or(base);
        let local = format!("effects/{effect}/shaders/{base}.{stage}");
        // Actually waterflow path is effects/waterflow/shaders/effects/waterflow.frag
        let candidates = [
            format!("effects/{effect}/shaders/{base}.{stage}"),
            format!("effects/{effect}/shaders/effects/{}.{}", Path::new(base).file_name().and_then(|s| s.to_str()).unwrap_or(base), stage),
            local,
            format!("shaders/{base}.{stage}"),
        ];
        for c in candidates {
            if let Some(p) = self.resolve(&c) {
                return Ok(std::fs::read_to_string(p)?);
            }
            // absolute under we_assets
            let p = self.we_assets.join(&c);
            if p.is_file() {
                return Ok(std::fs::read_to_string(p)?);
            }
        }
        Err(AssetError::NotFound(format!("{name}.{stage}")))
    }

    /// Include path for `#include "common.h"` etc.
    pub fn include_shader(&self, include_name: &str) -> Result<String, AssetError> {
        let name = include_name.trim().trim_matches('"');
        let candidates = [
            self.we_assets.join("shaders").join(name),
            self.we_assets.join(name),
            self.package_root.join("shaders").join(name),
        ];
        for p in candidates {
            if p.is_file() {
                return Ok(std::fs::read_to_string(p)?);
            }
        }
        Err(AssetError::NotFound(format!("include {name}")))
    }
}

/// Simple texture cache keyed by resolved path string.
#[derive(Default)]
pub struct TextureCache {
    map: HashMap<String, DecodedTex>,
}

impl TextureCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_load(
        &mut self,
        assets: &AssetResolver,
        name: &str,
    ) -> Result<&DecodedTex, AssetError> {
        if !self.map.contains_key(name) {
            let t = assets.load_tex(name)?;
            self.map.insert(name.to_string(), t);
        }
        Ok(self.map.get(name).unwrap())
    }
}

fn extract_workshop_id(rel: &str) -> Option<&str> {
    // …/workshop/1234567890/…
    let marker = "workshop/";
    let i = rel.find(marker)?;
    let rest = &rel[i + marker.len()..];
    let id = rest.split('/').next()?;
    if id.chars().all(|c| c.is_ascii_digit()) && id.len() >= 5 {
        Some(id)
    } else {
        None
    }
}

fn find_named(root: &Path, filename: &str, max_depth: u32) -> Option<PathBuf> {
    if max_depth == 0 || !root.is_dir() {
        return None;
    }
    let rd = std::fs::read_dir(root).ok()?;
    for ent in rd.flatten() {
        let p = ent.path();
        if p.is_file() {
            if p.file_name().and_then(|s| s.to_str()) == Some(filename) {
                return Some(p);
            }
        } else if p.is_dir() {
            if let Some(f) = find_named(&p, filename, max_depth - 1) {
                return Some(f);
            }
        }
    }
    None
}
