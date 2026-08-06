//! Wallpaper Engine scene.pkg (PKGV000x) archive.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PkgError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid package: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone)]
pub struct PkgEntry {
    pub name: String,
    pub offset: u32,
    pub size: u32,
}

#[derive(Debug)]
pub struct PkgArchive {
    pub version: String,
    pub entries: Vec<PkgEntry>,
    data: Vec<u8>,
    data_start: usize,
}

impl PkgArchive {
    pub fn open(path: &Path) -> Result<Self, PkgError> {
        let data = std::fs::read(path)?;
        if data.len() < 16 {
            return Err(PkgError::Invalid("too short".into()));
        }
        if &data[4..8] != b"PKGV" {
            return Err(PkgError::Invalid(format!(
                "bad magic {:?}",
                &data.get(0..12)
            )));
        }
        let version = String::from_utf8_lossy(&data[4..12]).into_owned();
        let nfiles = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
        let mut pos = 16usize;
        let mut entries = Vec::with_capacity(nfiles);
        for _ in 0..nfiles {
            if pos + 4 > data.len() {
                return Err(PkgError::Invalid("truncated TOC".into()));
            }
            let plen = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + plen + 8 > data.len() {
                return Err(PkgError::Invalid("truncated entry".into()));
            }
            let name = String::from_utf8_lossy(&data[pos..pos + plen]).into_owned();
            pos += plen;
            let offset = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
            let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap());
            pos += 8;
            entries.push(PkgEntry { name, offset, size });
        }
        Ok(Self {
            version,
            entries,
            data,
            data_start: pos,
        })
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let e = self.entries.iter().find(|e| e.name == name)?;
        let start = self.data_start + e.offset as usize;
        let end = start + e.size as usize;
        self.data.get(start..end)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|e| e.name.as_str())
    }

    /// Extract all files into `dest` (creates parent dirs).
    pub fn extract_all(&self, dest: &Path) -> Result<(), PkgError> {
        for e in &self.entries {
            let start = self.data_start + e.offset as usize;
            let end = start + e.size as usize;
            let blob = self
                .data
                .get(start..end)
                .ok_or_else(|| PkgError::Invalid(format!("bad range for {}", e.name)))?;
            let out = dest.join(&e.name);
            if let Some(parent) = out.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&out, blob)?;
        }
        Ok(())
    }

    pub fn extract_map(&self) -> HashMap<String, Vec<u8>> {
        let mut m = HashMap::new();
        for e in &self.entries {
            let start = self.data_start + e.offset as usize;
            let end = start + e.size as usize;
            if let Some(blob) = self.data.get(start..end) {
                m.insert(e.name.clone(), blob.to_vec());
            }
        }
        m
    }
}

/// Ensure package is unpacked under cache; returns directory with scene.json etc.
pub fn ensure_unpacked(wallpaper_dir: &Path, workshop_id: &str) -> Result<PathBuf, PkgError> {
    let pkg = wallpaper_dir.join("scene.pkg");
    if !pkg.is_file() {
        // already a folder wallpaper
        return Ok(wallpaper_dir.to_path_buf());
    }
    let dest = crate::we_cache_dir().join(workshop_id);
    let marker = dest.join(".pkg_version");
    let meta = std::fs::metadata(&pkg)?;
    let stamp = format!("{}:{}", meta.len(), meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0));
    if dest.join("scene.json").is_file() {
        if let Ok(old) = std::fs::read_to_string(&marker) {
            if old.trim() == stamp {
                return Ok(dest);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dest);
    std::fs::create_dir_all(&dest)?;
    let arch = PkgArchive::open(&pkg)?;
    arch.extract_all(&dest)?;
    // copy project.json / preview for convenience
    for name in ["project.json", "preview.jpg", "preview.png", "preview.gif"] {
        let src = wallpaper_dir.join(name);
        if src.is_file() {
            let _ = std::fs::copy(&src, dest.join(name));
        }
    }
    std::fs::write(marker, stamp)?;
    Ok(dest)
}
