//! Workshop scene editor: fork packages + mutate scene.json with field preserve.
//!
//! Working trees live under [`crate::wallengine_projects_dir`]. While editing we
//! keep a **directory** form (no `scene.pkg`) so walld always reads live
//! `scene.json` instead of the cached package unpack.

use crate::pkg::ensure_unpacked;
use crate::project::{Project, WallpaperType};
use crate::wallengine_projects_dir;
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

// ── Paths / fork ────────────────────────────────────────────────────────────

/// Create a local editable fork of a workshop (or other) wallpaper directory.
///
/// Copies the unpacked package tree + `project.json` / previews. Drops
/// `scene.pkg` so subsequent edits to `scene.json` are what walld loads.
pub fn fork_wallpaper(source_dir: &Path, source_id: &str) -> Result<ForkedProject, String> {
    if !source_dir.is_dir() {
        return Err(format!("not a directory: {}", source_dir.display()));
    }
    let pj = source_dir.join("project.json");
    if !pj.is_file() {
        return Err("source has no project.json".into());
    }
    let project = Project::load(&pj)?;
    if project.wallpaper_type != WallpaperType::Scene
        && !source_dir.join("scene.pkg").is_file()
        && !source_dir.join("scene.json").is_file()
    {
        return Err("only Scene wallpapers can be forked for editing".into());
    }

    let unpacked = ensure_unpacked(source_dir, source_id).map_err(|e| e.to_string())?;
    if !unpacked.join("scene.json").is_file() {
        return Err("scene.json missing after unpack — not a scene package".into());
    }

    let fork_id = unique_fork_id(source_id);
    let dest = wallengine_projects_dir().join(&fork_id);
    if dest.exists() {
        return Err(format!("fork path already exists: {}", dest.display()));
    }
    fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    copy_dir_filtered(&unpacked, &dest)?;

    // Prefer loose files from the original workshop folder too (project, preview).
    for name in [
        "project.json",
        "preview.jpg",
        "preview.png",
        "preview.gif",
        "preview.webp",
    ] {
        let src = source_dir.join(name);
        if src.is_file() {
            let _ = fs::copy(&src, dest.join(name));
        }
    }
    // Never keep scene.pkg in an editable fork.
    let pkg = dest.join("scene.pkg");
    if pkg.is_file() {
        let _ = fs::remove_file(&pkg);
    }

    rewrite_project_json(&dest, source_id, &project)?;

    Ok(ForkedProject {
        id: fork_id,
        dir: dest,
        forked_from: source_id.to_string(),
        title: format!("{} (edit)", project.title),
    })
}

/// Open an existing project directory as an editable scene (must have scene.json).
pub fn open_project_dir(dir: &Path) -> Result<EditableScene, String> {
    EditableScene::load(dir)
}

#[derive(Debug, Clone)]
pub struct ForkedProject {
    pub id: String,
    pub dir: PathBuf,
    pub forked_from: String,
    pub title: String,
}

fn unique_fork_id(source_id: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let safe: String = source_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    // A workshop id is normally numeric, but local/imported projects are not
    // required to be. Keep our generated directory well below NAME_MAX even
    // when a caller supplies an entire title or URL as its id.
    let safe = truncate_component(&safe, 120);
    let base = format!("fork_{safe}_{ts}");
    let root = wallengine_projects_dir();
    if !root.join(&base).exists() {
        return base;
    }
    for n in 1..1000 {
        let id = format!("{base}_{n}");
        if !root.join(&id).exists() {
            return id;
        }
    }
    format!("{base}_{ts}")
}

fn rewrite_project_json(dest: &Path, source_id: &str, original: &Project) -> Result<(), String> {
    let path = dest.join("project.json");
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let obj = raw
        .as_object_mut()
        .ok_or_else(|| "project.json root is not an object".to_string())?;

    let title = format!("{} (edit)", original.title);
    obj.insert("title".into(), json!(title));
    obj.insert("type".into(), json!("scene"));
    obj.insert("file".into(), json!("scene.json"));
    obj.insert("forked_from".into(), json!(source_id));
    // Keep workshopid for prop defaults / attribution; clear if confusing.
    if !obj.contains_key("workshopid") {
        obj.insert("workshopid".into(), json!(source_id));
    }

    let out = serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?;
    fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(())
}

fn copy_dir_filtered(src: &Path, dest: &Path) -> Result<(), String> {
    fn walk(src: &Path, dest: &Path) -> Result<(), String> {
        fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        let rd = fs::read_dir(src).map_err(|e| e.to_string())?;
        for ent in rd.flatten() {
            let name = ent.file_name();
            let name_str = name.to_string_lossy();
            // Skip package archive and cache markers.
            if name_str == "scene.pkg" || name_str == ".pkg_version" {
                continue;
            }
            let from = ent.path();
            let to = dest.join(&name);
            let meta = ent.metadata().map_err(|e| e.to_string())?;
            if meta.is_dir() {
                walk(&from, &to)?;
            } else if meta.is_file() {
                if let Some(parent) = to.parent() {
                    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                }
                fs::copy(&from, &to).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    }
    walk(src, dest)
}

// ── Editable scene document ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Image,
    Particle,
    Text,
    Sound,
    Group,
    Other,
}

impl LayerKind {
    pub fn as_label(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Particle => "particle",
            Self::Text => "text",
            Self::Sound => "sound",
            Self::Group => "group",
            Self::Other => "other",
        }
    }
}

#[derive(Debug, Clone)]
pub struct LayerSummary {
    pub index: usize,
    pub id: i64,
    pub name: String,
    pub kind: LayerKind,
    pub visible: bool,
    pub parent: Option<i64>,
    pub has_effects: bool,
    pub has_particle_override: bool,
    pub scripted: bool,
    pub animated: bool,
    /// Depth in parent chain (0 = root).
    pub depth: u32,
}

#[derive(Debug, Clone)]
pub struct EffectSummary {
    pub index: usize,
    pub name: String,
    pub file: String,
    pub visible: bool,
    pub constants: Vec<ConstantSummary>,
}

#[derive(Debug, Clone)]
pub struct ConstantSummary {
    pub pass: usize,
    pub key: String,
    pub value: EffectConstValue,
}

#[derive(Debug, Clone)]
pub enum EffectConstValue {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    Text(String),
    Other,
}

/// One editable field on a particle system JSON (or instance override).
#[derive(Debug, Clone)]
pub struct ParticleField {
    /// Stable draft key (`pd:maxcount`, `pd:em0:rate`, `pd:in1:min.x`, …).
    pub key: String,
    /// Section header shown once per group change.
    pub section: String,
    /// UI label.
    pub label: String,
    pub kind: ParticleFieldKind,
}

#[derive(Debug, Clone)]
pub enum ParticleFieldKind {
    Float { value: f32, lo: f32, hi: f32 },
    Text { value: String },
}

/// A known schema key that is missing from a particle block — offered as "+ add".
#[derive(Debug, Clone)]
pub struct ParticleAddableKey {
    /// Full `pd:…` key used by [`EditableScene::add_particle_key`].
    pub key: String,
    pub section: String,
    /// Field name as stored in JSON (`fadeouttime`).
    pub field: String,
    pub label: String,
    /// Default JSON value written when the key is added.
    pub default: Value,
    /// Slot id for freeform add draft (`sys`, `em0`, `op1`, …).
    pub slot: String,
}

#[derive(Debug, Clone)]
struct EditSnapshot {
    root: Value,
    particle_docs: HashMap<String, Value>,
    particle_dirty: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct EditableScene {
    /// Project directory (contains scene.json + assets).
    pub dir: PathBuf,
    pub scene_path: PathBuf,
    pub project_path: PathBuf,
    /// Full scene.json root — unknown fields preserved.
    pub root: Value,
    pub dirty: bool,
    pub title: String,
    pub id: String,
    pub forked_from: Option<String>,
    /// Cached particle JSON docs keyed by package-relative path.
    particle_docs: HashMap<String, Value>,
    /// Particle paths with unsaved mutations.
    particle_dirty: HashSet<String>,
    undo: Vec<EditSnapshot>,
    redo: Vec<EditSnapshot>,
}

const MAX_UNDO: usize = 64;

impl EditableScene {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let scene_path = dir.join("scene.json");
        if !scene_path.is_file() {
            // Try unpacked package in place.
            if dir.join("scene.pkg").is_file() {
                let id = dir
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "edit".into());
                let unpacked = ensure_unpacked(dir, &id).map_err(|e| e.to_string())?;
                return Self::load(&unpacked);
            }
            return Err(format!("no scene.json in {}", dir.display()));
        }
        let text = fs::read_to_string(&scene_path).map_err(|e| e.to_string())?;
        let root: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;

        let id = dir
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "project".into());
        let project_path = dir.join("project.json");
        let (title, forked_from) = if project_path.is_file() {
            let p = Project::load(&project_path)?;
            let raw = fs::read_to_string(&project_path).ok();
            let forked = raw
                .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                .and_then(|v| {
                    v.get("forked_from")
                        .and_then(|x| x.as_str())
                        .map(|s| s.to_string())
                });
            (p.title, forked)
        } else {
            ("Untitled".into(), None)
        };

        Ok(Self {
            dir: dir.to_path_buf(),
            scene_path,
            project_path,
            root,
            dirty: false,
            title,
            id,
            forked_from,
            particle_docs: HashMap::new(),
            particle_dirty: HashSet::new(),
            undo: Vec::new(),
            redo: Vec::new(),
        })
    }

    pub fn save(&mut self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.root).map_err(|e| e.to_string())?;
        // WE often uses no trailing newline issues; pretty is fine for edit forks.
        fs::write(&self.scene_path, text).map_err(|e| e.to_string())?;
        // Persist particle system JSONs that were edited (or restored via undo).
        // Always write every dirty path; after undo/redo we mark all cached
        // docs dirty so disk matches memory.
        let dirty_paths: Vec<String> = self.particle_dirty.iter().cloned().collect();
        for rel in dirty_paths {
            let Some(doc) = self.particle_docs.get(&rel) else {
                continue;
            };
            let path =
                resolve_particle_path(&self.dir, &rel).unwrap_or_else(|_| self.dir.join(&rel));
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            let text = serde_json::to_string_pretty(doc).map_err(|e| e.to_string())?;
            fs::write(&path, text).map_err(|e| e.to_string())?;
        }
        self.particle_dirty.clear();
        self.dirty = false;
        Ok(())
    }

    pub fn ortho(&self) -> (f32, f32) {
        let g = self.root.get("general");
        let o = g.and_then(|g| g.get("orthogonalprojection"));
        let w = o
            .and_then(|o| o.get("width"))
            .and_then(|v| v.as_f64())
            .unwrap_or(1920.0) as f32;
        let h = o
            .and_then(|o| o.get("height"))
            .and_then(|v| v.as_f64())
            .unwrap_or(1080.0) as f32;
        (w.max(1.0), h.max(1.0))
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn snapshot(&self) -> EditSnapshot {
        EditSnapshot {
            root: self.root.clone(),
            particle_docs: self.particle_docs.clone(),
            particle_dirty: self.particle_dirty.clone(),
        }
    }

    fn restore(&mut self, snap: EditSnapshot) {
        self.root = snap.root;
        self.particle_docs = snap.particle_docs;
        // Force every cached particle doc to rewrite on next save so disk
        // matches the restored memory (undo must reverse a prior save).
        self.particle_dirty = self.particle_docs.keys().cloned().collect();
        self.particle_dirty.extend(snap.particle_dirty);
        self.dirty = true;
    }

    fn push_undo(&mut self) {
        self.undo.push(self.snapshot());
        if self.undo.len() > MAX_UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    pub fn undo(&mut self) -> bool {
        let Some(prev) = self.undo.pop() else {
            return false;
        };
        self.redo.push(self.snapshot());
        self.restore(prev);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(next) = self.redo.pop() else {
            return false;
        };
        self.undo.push(self.snapshot());
        self.restore(next);
        true
    }

    fn objects_mut(&mut self) -> Option<&mut Vec<Value>> {
        self.root.get_mut("objects")?.as_array_mut()
    }

    fn objects(&self) -> &[Value] {
        self.root
            .get("objects")
            .and_then(|v| v.as_array())
            .map(|a| a.as_slice())
            .unwrap_or(&[])
    }

    pub fn object(&self, index: usize) -> Option<&Value> {
        self.objects().get(index)
    }

    pub fn layers(&self) -> Vec<LayerSummary> {
        let objs = self.objects();
        let id_to_parent: Vec<(i64, Option<i64>)> = objs
            .iter()
            .map(|o| {
                let id = o.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let parent = o.get("parent").and_then(|v| {
                    if v.is_null() {
                        None
                    } else {
                        v.as_i64().or_else(|| v.as_u64().map(|u| u as i64))
                    }
                });
                (id, parent)
            })
            .collect();
        let mut parent_map: std::collections::HashMap<i64, Option<i64>> =
            id_to_parent.into_iter().collect();

        objs.iter()
            .enumerate()
            .map(|(index, o)| {
                let id = o.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
                let name = o
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("(unnamed)")
                    .to_string();
                let parent = o.get("parent").and_then(|v| {
                    if v.is_null() {
                        None
                    } else {
                        v.as_i64().or_else(|| v.as_u64().map(|u| u as i64))
                    }
                });
                let visible = read_bool(o.get("visible"), true);
                let kind = classify_layer(o);
                let has_effects = o
                    .get("effects")
                    .and_then(|v| v.as_array())
                    .map(|a| !a.is_empty())
                    .unwrap_or(false);
                let has_particle_override = o.get("instanceoverride").is_some();
                let scripted = is_scripted_object(o);
                let animated = is_animated_object(o);
                let depth = depth_of(id, &mut parent_map);
                LayerSummary {
                    index,
                    id,
                    name,
                    kind,
                    visible,
                    parent,
                    has_effects,
                    has_particle_override,
                    scripted,
                    animated,
                    depth,
                }
            })
            .collect()
    }

    pub fn effects_on(&self, layer_index: usize) -> Vec<EffectSummary> {
        let Some(obj) = self.object(layer_index) else {
            return Vec::new();
        };
        let Some(Value::Array(arr)) = obj.get("effects") else {
            return Vec::new();
        };
        arr.iter()
            .enumerate()
            .map(|(index, ef)| {
                let name = ef
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let file = ef
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let visible = read_bool(ef.get("visible"), true);
                let mut constants = Vec::new();
                if let Some(Value::Array(passes)) = ef.get("passes") {
                    for (pi, p) in passes.iter().enumerate() {
                        if let Some(Value::Object(cobj)) = p.get("constantshadervalues") {
                            for (key, val) in cobj {
                                constants.push(ConstantSummary {
                                    pass: pi,
                                    key: key.clone(),
                                    value: parse_const_value(val),
                                });
                            }
                        }
                    }
                }
                EffectSummary {
                    index,
                    name,
                    file,
                    visible,
                    constants,
                }
            })
            .collect()
    }

    // ── Mutations ───────────────────────────────────────────────────────────

    pub fn set_visible(&mut self, index: usize, visible: bool) -> Result<(), String> {
        self.push_undo();
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        // Force a plain JSON bool. WE often stores
        // `{ "user": "prop", "value": true }` or condition objects — writing only
        // into `.value` is ignored by the runtime while the user binding wins.
        force_static_bool(obj, "visible", visible);
        self.dirty = true;
        Ok(())
    }

    pub fn set_name(&mut self, index: usize, name: &str) -> Result<(), String> {
        self.push_undo();
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        if let Some(m) = obj.as_object_mut() {
            m.insert("name".into(), json!(name));
        }
        Ok(())
    }

    pub fn set_origin(&mut self, index: usize, v: [f32; 3]) -> Result<(), String> {
        self.push_undo();
        self.set_origin_raw(index, v)
    }

    /// Mutate origin without pushing undo (for multi-select / drag).
    pub fn set_origin_raw(&mut self, index: usize, v: [f32; 3]) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        write_vec3_field(obj, "origin", v);
        self.dirty = true;
        Ok(())
    }

    pub fn set_scale(&mut self, index: usize, v: [f32; 3]) -> Result<(), String> {
        self.push_undo();
        self.set_scale_raw(index, v)
    }

    pub fn set_scale_raw(&mut self, index: usize, v: [f32; 3]) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        write_vec3_field(obj, "scale", v);
        self.dirty = true;
        Ok(())
    }

    /// `degrees` — converted to radians for disk (WE static angles).
    pub fn set_angles_degrees(&mut self, index: usize, deg: [f32; 3]) -> Result<(), String> {
        self.push_undo();
        self.set_angles_degrees_raw(index, deg)
    }

    pub fn set_angles_degrees_raw(&mut self, index: usize, deg: [f32; 3]) -> Result<(), String> {
        let rad = [
            deg[0].to_radians(),
            deg[1].to_radians(),
            deg[2].to_radians(),
        ];
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        write_vec3_field(obj, "angles", rad);
        self.dirty = true;
        Ok(())
    }

    pub fn set_alpha(&mut self, index: usize, alpha: f32) -> Result<(), String> {
        self.push_undo();
        self.set_alpha_raw(index, alpha)
    }

    pub fn set_alpha_raw(&mut self, index: usize, alpha: f32) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        write_f32_field(obj, "alpha", alpha.clamp(0.0, 1.0));
        self.dirty = true;
        Ok(())
    }

    /// Snapshot for a single undo that covers a multi-object drag.
    pub fn snapshot_undo(&mut self) {
        self.push_undo();
    }

    pub fn set_brightness(&mut self, index: usize, brightness: f32) -> Result<(), String> {
        self.push_undo();
        self.set_brightness_raw(index, brightness)
    }

    pub fn set_brightness_raw(&mut self, index: usize, brightness: f32) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        write_f32_field(obj, "brightness", brightness.max(0.0));
        self.dirty = true;
        Ok(())
    }

    pub fn set_color(&mut self, index: usize, rgb: [f32; 3]) -> Result<(), String> {
        self.push_undo();
        self.set_color_raw(index, rgb)
    }

    pub fn set_color_raw(&mut self, index: usize, rgb: [f32; 3]) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        write_vec3_field(obj, "color", rgb);
        self.dirty = true;
        Ok(())
    }

    pub fn set_particle_override_f32(
        &mut self,
        index: usize,
        key: &str,
        value: f32,
    ) -> Result<(), String> {
        self.push_undo();
        self.set_particle_override_f32_raw(index, key, value)
    }

    /// Mutate instance override without pushing undo (multi-select / drag).
    pub fn set_particle_override_f32_raw(
        &mut self,
        index: usize,
        key: &str,
        value: f32,
    ) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        let map = obj
            .as_object_mut()
            .ok_or("object is not a map")?
            .entry("instanceoverride")
            .or_insert_with(|| json!({}));
        let m = map
            .as_object_mut()
            .ok_or("instanceoverride is not an object")?;
        m.insert(key.to_string(), json!(value));
        self.dirty = true;
        Ok(())
    }

    pub fn set_particle_override_color(
        &mut self,
        index: usize,
        rgb: [f32; 3],
    ) -> Result<(), String> {
        self.push_undo();
        self.set_particle_override_color_raw(index, rgb)
    }

    pub fn set_particle_override_color_raw(
        &mut self,
        index: usize,
        rgb: [f32; 3],
    ) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        let map = obj
            .as_object_mut()
            .ok_or("object is not a map")?
            .entry("instanceoverride")
            .or_insert_with(|| json!({}));
        let m = map
            .as_object_mut()
            .ok_or("instanceoverride is not an object")?;
        m.insert(
            "colorn".into(),
            json!(format!("{:.6} {:.6} {:.6}", rgb[0], rgb[1], rgb[2])),
        );
        self.dirty = true;
        Ok(())
    }

    pub fn read_particle_override_colorn(&self, index: usize) -> [f32; 3] {
        self.object(index)
            .and_then(|o| o.get("instanceoverride"))
            .and_then(|o| o.get("colorn").or_else(|| o.get("color")))
            .map(|v| read_vec3(Some(v), [1.0, 1.0, 1.0]))
            .unwrap_or([1.0, 1.0, 1.0])
    }

    // ── Particle system JSON (particles/*.json) ─────────────────────────────

    /// Load the particle document for a layer into the edit cache (idempotent).
    pub fn ensure_particle_doc(&mut self, index: usize) -> Result<Option<String>, String> {
        let Some(rel) = self.read_particle_path(index) else {
            return Ok(None);
        };
        let rel = normalize_particle_rel(&rel);
        if self.particle_docs.contains_key(&rel) {
            return Ok(Some(rel));
        }
        let path = resolve_particle_path(&self.dir, &rel)?;
        let text = fs::read_to_string(&path).map_err(|e| format!("read particle «{rel}»: {e}"))?;
        let doc: Value =
            serde_json::from_str(&text).map_err(|e| format!("parse particle «{rel}»: {e}"))?;
        self.particle_docs.insert(rel.clone(), doc);
        Ok(Some(rel))
    }

    /// Enumerate every editable field on the particle system JSON for `index`.
    ///
    /// Call [`ensure_particle_doc`] first so the cache is warm (view is immutable).
    pub fn particle_fields(&self, index: usize) -> Vec<ParticleField> {
        let Some(rel) = self.read_particle_path(index) else {
            return Vec::new();
        };
        let rel = normalize_particle_rel(&rel);
        let Some(doc) = self.particle_docs.get(&rel) else {
            return Vec::new();
        };
        let Some(root) = doc.as_object() else {
            return Vec::new();
        };
        let mut out = Vec::new();

        // Top-level system props.
        const TOP: &[&str] = &[
            "maxcount",
            "starttime",
            "sequencemultiplier",
            "animationmode",
            "material",
            "flags",
        ];
        for key in TOP {
            if let Some(v) = root.get(*key) {
                push_value_fields(
                    &mut out,
                    "SYSTEM",
                    pretty_label(key),
                    &format!("pd:{key}"),
                    v,
                    key,
                );
            }
        }

        // Renderer array
        if let Some(Value::Array(arr)) = root.get("renderer") {
            for (i, item) in arr.iter().enumerate() {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sprite");
                let section = format!("RENDERER {i} · {name}");
                if let Some(obj) = item.as_object() {
                    for (k, v) in obj {
                        if k == "id" {
                            continue;
                        }
                        push_value_fields(
                            &mut out,
                            &section,
                            pretty_label(k),
                            &format!("pd:rd{i}:{k}"),
                            v,
                            k,
                        );
                    }
                }
            }
        }

        // Emitters
        if let Some(Value::Array(arr)) = root.get("emitter") {
            for (i, item) in arr.iter().enumerate() {
                let name = item
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("emitter");
                let section = format!("EMITTER {i} · {name}");
                if let Some(obj) = item.as_object() {
                    for (k, v) in obj {
                        if k == "id" {
                            continue;
                        }
                        push_value_fields(
                            &mut out,
                            &section,
                            pretty_label(k),
                            &format!("pd:em{i}:{k}"),
                            v,
                            k,
                        );
                    }
                }
            }
        }

        // Initializers
        if let Some(Value::Array(arr)) = root.get("initializer") {
            for (i, item) in arr.iter().enumerate() {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("init");
                let section = format!("INITIALIZER {i} · {name}");
                if let Some(obj) = item.as_object() {
                    for (k, v) in obj {
                        if k == "id" {
                            continue;
                        }
                        push_value_fields(
                            &mut out,
                            &section,
                            pretty_label(k),
                            &format!("pd:in{i}:{k}"),
                            v,
                            k,
                        );
                    }
                }
            }
        }

        // Operators
        if let Some(Value::Array(arr)) = root.get("operator") {
            for (i, item) in arr.iter().enumerate() {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("op");
                let section = format!("OPERATOR {i} · {name}");
                if let Some(obj) = item.as_object() {
                    for (k, v) in obj {
                        if k == "id" {
                            continue;
                        }
                        push_value_fields(
                            &mut out,
                            &section,
                            pretty_label(k),
                            &format!("pd:op{i}:{k}"),
                            v,
                            k,
                        );
                    }
                }
            }
        }

        // Control points (only those with non-null useful fields)
        if let Some(Value::Array(arr)) = root.get("controlpoint") {
            for (i, item) in arr.iter().enumerate() {
                let section = format!("CONTROL POINT {i}");
                if let Some(obj) = item.as_object() {
                    let mut any = false;
                    for (k, v) in obj {
                        if matches!(k.as_str(), "id") || v.is_null() {
                            continue;
                        }
                        // Skip empty-looking offsets
                        if k == "offset" {
                            if let Some(s) = v.as_str() {
                                if s.trim().is_empty() || s == "0 0 0" {
                                    // still editable — include
                                }
                            }
                        }
                        any = true;
                        push_value_fields(
                            &mut out,
                            &section,
                            pretty_label(k),
                            &format!("pd:cp{i}:{k}"),
                            v,
                            k,
                        );
                    }
                    let _ = any;
                }
            }
        }

        // Children systems (event spawns etc.) — surface scalar/vector props.
        if let Some(Value::Array(arr)) = root.get("children") {
            for (i, item) in arr.iter().enumerate() {
                let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("child");
                let section = format!("CHILD {i} · {name}");
                if let Some(obj) = item.as_object() {
                    for (k, v) in obj {
                        if matches!(k.as_str(), "id") || v.is_null() {
                            continue;
                        }
                        push_value_fields(
                            &mut out,
                            &section,
                            pretty_label(k),
                            &format!("pd:ch{i}:{k}"),
                            v,
                            k,
                        );
                    }
                }
            }
        }

        out
    }

    /// Known keys missing from each particle block (for "+ add" buttons).
    pub fn particle_addable_keys(&self, index: usize) -> Vec<ParticleAddableKey> {
        let Some(rel) = self.read_particle_path(index) else {
            return Vec::new();
        };
        let rel = normalize_particle_rel(&rel);
        let Some(doc) = self.particle_docs.get(&rel) else {
            return Vec::new();
        };
        let Some(root) = doc.as_object() else {
            return Vec::new();
        };
        let mut out = Vec::new();

        // System top-level
        for (field, def) in known_system_keys() {
            if !root.contains_key(field) || root.get(field).map(|v| v.is_null()).unwrap_or(false) {
                out.push(ParticleAddableKey {
                    key: format!("pd:{field}"),
                    section: "SYSTEM".into(),
                    field: field.to_string(),
                    label: pretty_label(field),
                    default: def,
                    slot: "sys".into(),
                });
            }
        }

        push_addable_for_array(
            &mut out,
            root,
            "renderer",
            "rd",
            "RENDERER",
            known_renderer_keys,
        );
        push_addable_for_array(
            &mut out,
            root,
            "emitter",
            "em",
            "EMITTER",
            known_emitter_keys,
        );
        push_addable_for_array(
            &mut out,
            root,
            "initializer",
            "in",
            "INITIALIZER",
            known_initializer_keys,
        );
        push_addable_for_array(
            &mut out,
            root,
            "operator",
            "op",
            "OPERATOR",
            known_operator_keys,
        );
        push_addable_for_array(
            &mut out,
            root,
            "controlpoint",
            "cp",
            "CONTROL POINT",
            known_controlpoint_keys,
        );
        push_addable_for_array(&mut out, root, "children", "ch", "CHILD", known_child_keys);

        out
    }

    /// Slots present in the particle doc (for freeform "add key" rows).
    pub fn particle_slots(&self, index: usize) -> Vec<(String, String)> {
        // (slot, section title)
        let Some(rel) = self.read_particle_path(index) else {
            return Vec::new();
        };
        let rel = normalize_particle_rel(&rel);
        let Some(doc) = self.particle_docs.get(&rel) else {
            return Vec::new();
        };
        let Some(root) = doc.as_object() else {
            return Vec::new();
        };
        let mut out = vec![("sys".into(), "SYSTEM".into())];
        for (arr_key, prefix, title) in [
            ("renderer", "rd", "RENDERER"),
            ("emitter", "em", "EMITTER"),
            ("initializer", "in", "INITIALIZER"),
            ("operator", "op", "OPERATOR"),
            ("controlpoint", "cp", "CONTROL POINT"),
            ("children", "ch", "CHILD"),
        ] {
            if let Some(Value::Array(arr)) = root.get(arr_key) {
                for (i, item) in arr.iter().enumerate() {
                    let name = item.get("name").and_then(|v| v.as_str()).unwrap_or("");
                    let section = if name.is_empty() {
                        format!("{title} {i}")
                    } else {
                        format!("{title} {i} · {name}")
                    };
                    out.push((format!("{prefix}{i}"), section));
                }
            }
        }
        out
    }

    /// Insert a missing key with the given default value. `key` is a `pd:…` path.
    pub fn add_particle_key(
        &mut self,
        index: usize,
        key: &str,
        default: Value,
    ) -> Result<(), String> {
        self.push_undo();
        let rel = self
            .ensure_particle_doc(index)?
            .ok_or_else(|| "layer has no particle path".to_string())?;
        let doc = self
            .particle_docs
            .get_mut(&rel)
            .ok_or("particle doc missing from cache")?;
        insert_particle_key(doc, key, default)?;
        self.particle_dirty.insert(rel);
        self.dirty = true;
        Ok(())
    }

    /// Freeform: add `field` under slot (`sys`, `em0`, `op1`, …) with a guessed default.
    pub fn add_particle_key_freeform(
        &mut self,
        index: usize,
        slot: &str,
        field: &str,
    ) -> Result<(), String> {
        let field = field.trim();
        if field.is_empty() {
            return Err("key name is empty".into());
        }
        if field == "id" {
            return Err("cannot add reserved key «id»".into());
        }
        if !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err("key must be alphanumeric/underscore".into());
        }
        let pd_key = if slot == "sys" {
            format!("pd:{field}")
        } else {
            format!("pd:{slot}:{field}")
        };
        let default = guess_default_for_field(field);
        self.add_particle_key(index, &pd_key, default)
    }

    /// Set a float particle-doc field by draft key (`pd:…`). Pushes undo.
    pub fn set_particle_field_f32(
        &mut self,
        index: usize,
        key: &str,
        value: f32,
    ) -> Result<(), String> {
        self.push_undo();
        self.set_particle_field_f32_raw(index, key, value)
    }

    pub fn set_particle_field_f32_raw(
        &mut self,
        index: usize,
        key: &str,
        value: f32,
    ) -> Result<(), String> {
        let rel = self
            .ensure_particle_doc(index)?
            .ok_or_else(|| "layer has no particle path".to_string())?;
        let doc = self
            .particle_docs
            .get_mut(&rel)
            .ok_or("particle doc missing from cache")?;
        apply_particle_key(doc, key, ParticleWrite::Float(value))?;
        self.particle_dirty.insert(rel);
        self.dirty = true;
        Ok(())
    }

    /// Set a text particle-doc field by draft key. Pushes undo.
    pub fn set_particle_field_text(
        &mut self,
        index: usize,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        self.push_undo();
        self.set_particle_field_text_raw(index, key, value)
    }

    pub fn set_particle_field_text_raw(
        &mut self,
        index: usize,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        let rel = self
            .ensure_particle_doc(index)?
            .ok_or_else(|| "layer has no particle path".to_string())?;
        let doc = self
            .particle_docs
            .get_mut(&rel)
            .ok_or("particle doc missing from cache")?;
        apply_particle_key(doc, key, ParticleWrite::Text(value.to_string()))?;
        self.particle_dirty.insert(rel);
        self.dirty = true;
        Ok(())
    }

    pub fn set_effect_visible(
        &mut self,
        layer: usize,
        effect: usize,
        visible: bool,
    ) -> Result<(), String> {
        self.push_undo();
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(layer))
            .ok_or("bad layer index")?;
        let ef = obj
            .get_mut("effects")
            .and_then(|v| v.as_array_mut())
            .and_then(|a| a.get_mut(effect))
            .ok_or("bad effect index")?;
        force_static_bool(ef, "visible", visible);
        self.dirty = true;
        Ok(())
    }

    pub fn set_effect_constant_f32(
        &mut self,
        layer: usize,
        effect: usize,
        pass: usize,
        key: &str,
        value: f32,
    ) -> Result<(), String> {
        self.push_undo();
        self.set_effect_constant_f32_raw(layer, effect, pass, key, value)
    }

    pub fn set_effect_constant_f32_raw(
        &mut self,
        layer: usize,
        effect: usize,
        pass: usize,
        key: &str,
        value: f32,
    ) -> Result<(), String> {
        let c = self.effect_constants_mut(layer, effect, pass)?;
        if let Some(existing) = c.get(key) {
            if let Some(m) = existing.as_object() {
                let mut nm = m.clone();
                nm.insert("value".into(), json!(value));
                c.insert(key.to_string(), Value::Object(nm));
                self.dirty = true;
                return Ok(());
            }
        }
        c.insert(key.to_string(), json!(value));
        self.dirty = true;
        Ok(())
    }

    /// Write one component of a vec effect constant (string `"x y z"` or array).
    pub fn set_effect_constant_comp_raw(
        &mut self,
        layer: usize,
        effect: usize,
        pass: usize,
        key: &str,
        axis: usize,
        value: f32,
    ) -> Result<(), String> {
        let c = self.effect_constants_mut(layer, effect, pass)?;
        let current = c.get(key).cloned().unwrap_or(json!("0 0 0"));
        let (is_carrier, inner) = match &current {
            Value::Object(m) if m.contains_key("value") => {
                (true, m.get("value").cloned().unwrap_or(json!("0 0 0")))
            }
            other => (false, other.clone()),
        };
        let mut comps: Vec<f32> = match &inner {
            Value::String(s) => {
                let mut p: Vec<f32> = s
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                while p.len() <= axis {
                    p.push(0.0);
                }
                p
            }
            Value::Array(a) => {
                let mut p: Vec<f32> = a
                    .iter()
                    .filter_map(|x| x.as_f64().map(|f| f as f32))
                    .collect();
                while p.len() <= axis {
                    p.push(0.0);
                }
                p
            }
            Value::Number(n) => {
                let f = n.as_f64().unwrap_or(0.0) as f32;
                let mut p = vec![f; axis + 1];
                p[axis] = value;
                p
            }
            _ => {
                let mut p = vec![0.0; axis + 1];
                p[axis] = value;
                p
            }
        };
        if axis < comps.len() {
            comps[axis] = value;
        }
        let new_inner = match &inner {
            Value::Array(_) => Value::Array(comps.iter().map(|c| json!(c)).collect()),
            _ => {
                let s = comps
                    .iter()
                    .map(|c| {
                        if (*c - c.round()).abs() < 1e-4 {
                            format!("{}", *c as i64)
                        } else {
                            format!("{c}")
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                json!(s)
            }
        };
        if is_carrier {
            if let Value::Object(m) = current {
                let mut nm = m;
                nm.insert("value".into(), new_inner);
                c.insert(key.to_string(), Value::Object(nm));
            }
        } else {
            c.insert(key.to_string(), new_inner);
        }
        self.dirty = true;
        Ok(())
    }

    pub fn set_effect_constant_text_raw(
        &mut self,
        layer: usize,
        effect: usize,
        pass: usize,
        key: &str,
        value: &str,
    ) -> Result<(), String> {
        let c = self.effect_constants_mut(layer, effect, pass)?;
        if let Some(existing) = c.get(key) {
            if let Some(m) = existing.as_object() {
                let mut nm = m.clone();
                nm.insert("value".into(), json!(value));
                c.insert(key.to_string(), Value::Object(nm));
                self.dirty = true;
                return Ok(());
            }
        }
        c.insert(key.to_string(), json!(value));
        self.dirty = true;
        Ok(())
    }

    fn effect_constants_mut(
        &mut self,
        layer: usize,
        effect: usize,
        pass: usize,
    ) -> Result<&mut Map<String, Value>, String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(layer))
            .ok_or("bad layer index")?;
        let ef = obj
            .get_mut("effects")
            .and_then(|v| v.as_array_mut())
            .and_then(|a| a.get_mut(effect))
            .ok_or("bad effect index")?;
        {
            let passes = ef
                .as_object_mut()
                .ok_or("effect is not an object")?
                .entry("passes".to_string())
                .or_insert_with(|| json!([]));
            let arr = passes.as_array_mut().ok_or("passes is not an array")?;
            while arr.len() <= pass {
                arr.push(json!({}));
            }
            let p = arr[pass].as_object_mut().ok_or("pass is not an object")?;
            p.entry("constantshadervalues".to_string())
                .or_insert_with(|| json!({}));
        }
        ef.get_mut("passes")
            .and_then(|v| v.as_array_mut())
            .and_then(|a| a.get_mut(pass))
            .and_then(|p| p.get_mut("constantshadervalues"))
            .and_then(|v| v.as_object_mut())
            .ok_or_else(|| "constantshadervalues missing".to_string())
    }

    pub fn set_image_model(&mut self, index: usize, model_path: &str) -> Result<(), String> {
        self.push_undo();
        self.set_image_model_raw(index, model_path)
    }

    pub fn set_image_model_raw(&mut self, index: usize, model_path: &str) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        if let Some(m) = obj.as_object_mut() {
            m.insert("image".into(), json!(model_path));
        }
        self.dirty = true;
        Ok(())
    }

    pub fn set_text_literal(&mut self, index: usize, text: &str) -> Result<(), String> {
        self.push_undo();
        self.set_text_literal_raw(index, text)
    }

    pub fn set_text_literal_raw(&mut self, index: usize, text: &str) -> Result<(), String> {
        let obj = self
            .objects_mut()
            .and_then(|a| a.get_mut(index))
            .ok_or("bad layer index")?;
        if let Some(m) = obj.as_object_mut() {
            // Preserve object carrier if present.
            match m.get("text") {
                Some(Value::Object(existing)) => {
                    let mut nm = existing.clone();
                    nm.insert("value".into(), json!(text));
                    m.insert("text".into(), Value::Object(nm));
                }
                _ => {
                    m.insert("text".into(), json!(text));
                }
            }
        }
        self.dirty = true;
        Ok(())
    }

    pub fn delete_layer(&mut self, index: usize) -> Result<(), String> {
        self.push_undo();
        self.delete_layer_raw(index)
    }

    /// Remove layer and clear `parent` refs pointing at its id.
    pub fn delete_layer_raw(&mut self, index: usize) -> Result<(), String> {
        let arr = self.objects_mut().ok_or("no objects")?;
        if index >= arr.len() {
            return Err("bad layer index".into());
        }
        let removed_id = arr[index].get("id").and_then(|v| v.as_i64());
        arr.remove(index);
        if let Some(rid) = removed_id {
            for o in arr.iter_mut() {
                if let Some(m) = o.as_object_mut() {
                    let parent_match = m
                        .get("parent")
                        .and_then(|v| v.as_i64().or_else(|| v.as_u64().map(|u| u as i64)))
                        == Some(rid);
                    if parent_match {
                        m.insert("parent".into(), Value::Null);
                    }
                }
            }
        }
        self.dirty = true;
        Ok(())
    }

    pub fn duplicate_layer(&mut self, index: usize) -> Result<usize, String> {
        self.push_undo();
        self.duplicate_layer_raw(index)
    }

    pub fn duplicate_layer_raw(&mut self, index: usize) -> Result<usize, String> {
        let arr = self.objects_mut().ok_or("no objects")?;
        if index >= arr.len() {
            return Err("bad layer index".into());
        }
        let mut clone = arr[index].clone();
        let max_id = arr
            .iter()
            .filter_map(|o| o.get("id").and_then(|v| v.as_i64()))
            .max()
            .unwrap_or(0);
        if let Some(m) = clone.as_object_mut() {
            m.insert("id".into(), json!(max_id + 1));
            if let Some(name) = m.get("name").and_then(|v| v.as_str()) {
                m.insert("name".into(), json!(format!("{name} copy")));
            }
        }
        let new_index = index + 1;
        arr.insert(new_index, clone);
        self.dirty = true;
        Ok(new_index)
    }

    pub fn move_layer(&mut self, from: usize, to: usize) -> Result<(), String> {
        if from == to {
            return Ok(());
        }
        self.push_undo();
        self.move_layer_raw(from, to)
    }

    pub fn move_layer_raw(&mut self, from: usize, to: usize) -> Result<(), String> {
        if from == to {
            return Ok(());
        }
        let arr = self.objects_mut().ok_or("no objects")?;
        if from >= arr.len() || to >= arr.len() {
            return Err("bad layer index".into());
        }
        let item = arr.remove(from);
        arr.insert(to, item);
        self.dirty = true;
        Ok(())
    }

    // ── Field readers for inspector ─────────────────────────────────────────

    pub fn read_origin(&self, index: usize) -> [f32; 3] {
        self.object(index)
            .map(|o| read_vec3(o.get("origin"), [0.0, 0.0, 0.0]))
            .unwrap_or([0.0; 3])
    }

    pub fn read_scale(&self, index: usize) -> [f32; 3] {
        self.object(index)
            .map(|o| read_vec3(o.get("scale"), [1.0, 1.0, 1.0]))
            .unwrap_or([1.0; 3])
    }

    pub fn read_angles_degrees(&self, index: usize) -> [f32; 3] {
        let r = self
            .object(index)
            .map(|o| read_vec3(o.get("angles"), [0.0, 0.0, 0.0]))
            .unwrap_or([0.0; 3]);
        [r[0].to_degrees(), r[1].to_degrees(), r[2].to_degrees()]
    }

    pub fn read_alpha(&self, index: usize) -> f32 {
        self.object(index)
            .map(|o| read_f32(o.get("alpha"), 1.0))
            .unwrap_or(1.0)
    }

    pub fn read_brightness(&self, index: usize) -> f32 {
        self.object(index)
            .map(|o| read_f32(o.get("brightness"), 1.0))
            .unwrap_or(1.0)
    }

    pub fn read_color(&self, index: usize) -> Option<[f32; 3]> {
        self.object(index)
            .and_then(|o| o.get("color"))
            .map(|v| read_vec3(Some(v), [1.0, 1.0, 1.0]))
    }

    pub fn read_image_model(&self, index: usize) -> Option<String> {
        self.object(index)
            .and_then(|o| o.get("image"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    pub fn read_particle_path(&self, index: usize) -> Option<String> {
        self.object(index)
            .and_then(|o| o.get("particle"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    pub fn read_particle_override_f32(&self, index: usize, key: &str) -> f32 {
        self.object(index)
            .and_then(|o| o.get("instanceoverride"))
            .and_then(|o| o.get(key))
            .and_then(|v| v.as_f64())
            .unwrap_or(1.0) as f32
    }

    pub fn read_text_literal(&self, index: usize) -> Option<String> {
        self.object(index).and_then(|o| {
            let t = o.get("text")?;
            t.as_str().map(|s| s.to_string()).or_else(|| {
                t.get("value")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
        })
    }

    /// List relative asset paths under the project (for browser).
    pub fn list_assets(&self, max: usize) -> Vec<String> {
        let mut out = Vec::new();
        fn walk(base: &Path, dir: &Path, out: &mut Vec<String>, max: usize) {
            if out.len() >= max {
                return;
            }
            let Ok(rd) = fs::read_dir(dir) else {
                return;
            };
            for ent in rd.flatten() {
                if out.len() >= max {
                    return;
                }
                let p = ent.path();
                if p.is_dir() {
                    walk(base, &p, out, max);
                } else if p.is_file() {
                    if let Ok(rel) = p.strip_prefix(base) {
                        out.push(rel.to_string_lossy().replace('\\', "/"));
                    }
                }
            }
        }
        walk(&self.dir, &self.dir, &mut out, max);
        out.sort();
        out
    }

    /// Import an external image into `materials/imported/` and return relative path.
    pub fn import_image(&mut self, src: &Path) -> Result<String, String> {
        if !src.is_file() {
            return Err(format!("not a file: {}", src.display()));
        }
        let stem = src
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "import".into());
        let ext = src
            .extension()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "png".into());
        let ext = safe_extension(&ext);
        let safe: String = stem
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let rel_dir = PathBuf::from("materials/imported");
        let abs_dir = self.dir.join(&rel_dir);
        fs::create_dir_all(&abs_dir).map_err(|e| e.to_string())?;
        let mut name = imported_filename(&safe, &ext, "");
        let mut n = 1u32;
        while abs_dir.join(&name).exists() {
            name = imported_filename(&safe, &ext, &format!("_{n}"));
            n += 1;
        }
        let dest = abs_dir.join(&name);
        fs::copy(src, &dest).map_err(|e| e.to_string())?;
        let rel = format!("materials/imported/{name}");
        self.dirty = true;
        Ok(rel)
    }
}

// A component may be at most 255 bytes on common Linux filesystems. Leave a
// little headroom for a collision suffix and temporary-file extensions. The
// source title remains in project.json, so shortening this on-disk import name
// does not lose what the user sees in WallStudio.
const MAX_GENERATED_COMPONENT_BYTES: usize = 220;

fn truncate_component(value: &str, max_bytes: usize) -> String {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    let out = value[..end].trim_matches('_');
    if out.is_empty() {
        "import".into()
    } else {
        out.into()
    }
}

fn safe_extension(extension: &str) -> String {
    let ext: String = extension
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(16)
        .collect();
    if ext.is_empty() {
        "bin".into()
    } else {
        ext
    }
}

fn imported_filename(stem: &str, extension: &str, suffix: &str) -> String {
    let reserved = extension.len() + suffix.len() + 1; // dot before extension
    let max_stem = MAX_GENERATED_COMPONENT_BYTES
        .saturating_sub(reserved)
        .max(1);
    format!(
        "{}{}.{}",
        truncate_component(stem, max_stem),
        suffix,
        extension
    )
}

// ── JSON field helpers ──────────────────────────────────────────────────────

fn classify_layer(o: &Value) -> LayerKind {
    if o.get("image")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty() && s != "null")
    {
        return LayerKind::Image;
    }
    if o.get("particle")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.is_empty() && s != "null")
    {
        return LayerKind::Particle;
    }
    if o.get("text").is_some() {
        return LayerKind::Text;
    }
    if o.get("sound").is_some() {
        return LayerKind::Sound;
    }
    // Groups often have no drawable fields but have children referencing them.
    if o.get("image").is_none()
        && o.get("particle").is_none()
        && o.get("text").is_none()
        && o.get("sound").is_none()
    {
        return LayerKind::Group;
    }
    LayerKind::Other
}

fn is_scripted_object(o: &Value) -> bool {
    for p in [
        "origin", "angles", "scale", "alpha", "visible", "text", "color", "size",
    ] {
        if o.get(p).and_then(|v| v.get("script")).is_some() {
            return true;
        }
    }
    false
}

fn is_animated_object(o: &Value) -> bool {
    for p in ["origin", "angles", "scale", "alpha"] {
        if o.get(p).and_then(|v| v.get("animation")).is_some() {
            return true;
        }
    }
    false
}

fn depth_of(id: i64, parents: &mut std::collections::HashMap<i64, Option<i64>>) -> u32 {
    let mut d = 0u32;
    let mut cur = id;
    let mut guard = 0u32;
    while let Some(Some(p)) = parents.get(&cur).copied() {
        d += 1;
        cur = p;
        guard += 1;
        if guard > 64 {
            break;
        }
    }
    d
}

fn read_bool(v: Option<&Value>, default: bool) -> bool {
    match v {
        None => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_i64().unwrap_or(default as i64) != 0,
        Some(Value::Object(o)) => o
            .get("value")
            .map(|x| read_bool(Some(x), default))
            .unwrap_or(default),
        _ => default,
    }
}

fn read_f32(v: Option<&Value>, default: f32) -> f32 {
    match v {
        None => default,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default as f64) as f32,
        Some(Value::String(s)) => s
            .split_whitespace()
            .next()
            .and_then(|x| x.parse().ok())
            .unwrap_or(default),
        Some(Value::Object(o)) => {
            if let Some(val) = o.get("value") {
                return read_f32(Some(val), default);
            }
            default
        }
        _ => default,
    }
}

fn read_vec3(v: Option<&Value>, default: [f32; 3]) -> [f32; 3] {
    match v {
        None => default,
        Some(Value::String(s)) => parse_vec3_str(s).unwrap_or(default),
        Some(Value::Array(a)) => [
            a.first()
                .and_then(|x| x.as_f64())
                .unwrap_or(default[0] as f64) as f32,
            a.get(1)
                .and_then(|x| x.as_f64())
                .unwrap_or(default[1] as f64) as f32,
            a.get(2)
                .and_then(|x| x.as_f64())
                .unwrap_or(default[2] as f64) as f32,
        ],
        Some(Value::Object(o)) => {
            if let Some(val) = o.get("value") {
                return read_vec3(Some(val), default);
            }
            // {x,y,z} form
            if o.contains_key("x") || o.contains_key("y") {
                return [
                    o.get("x")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(default[0] as f64) as f32,
                    o.get("y")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(default[1] as f64) as f32,
                    o.get("z")
                        .and_then(|x| x.as_f64())
                        .unwrap_or(default[2] as f64) as f32,
                ];
            }
            default
        }
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(0.0) as f32;
            [f, f, f]
        }
        _ => default,
    }
}

fn parse_vec3_str(s: &str) -> Option<[f32; 3]> {
    let mut it = s.split_whitespace().filter_map(|x| x.parse::<f32>().ok());
    Some([
        it.next()?,
        it.next().unwrap_or(0.0),
        it.next().unwrap_or(0.0),
    ])
}

fn parse_const_value(v: &Value) -> EffectConstValue {
    match v {
        Value::Number(n) => EffectConstValue::Float(n.as_f64().unwrap_or(0.0) as f32),
        Value::String(s) => {
            let parts: Vec<f32> = s
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            match parts.len() {
                0 => EffectConstValue::Text(s.clone()),
                1 => EffectConstValue::Float(parts[0]),
                2 => EffectConstValue::Vec2([parts[0], parts[1]]),
                3 => EffectConstValue::Vec3([parts[0], parts[1], parts[2]]),
                _ => EffectConstValue::Vec4([
                    parts[0],
                    parts[1],
                    parts.get(2).copied().unwrap_or(0.0),
                    parts.get(3).copied().unwrap_or(0.0),
                ]),
            }
        }
        Value::Object(o) => {
            if let Some(val) = o.get("value") {
                return parse_const_value(val);
            }
            EffectConstValue::Other
        }
        Value::Array(a) => {
            let parts: Vec<f32> = a
                .iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect();
            match parts.len() {
                1 => EffectConstValue::Float(parts[0]),
                2 => EffectConstValue::Vec2([parts[0], parts[1]]),
                3 => EffectConstValue::Vec3([parts[0], parts[1], parts[2]]),
                n if n >= 4 => EffectConstValue::Vec4([parts[0], parts[1], parts[2], parts[3]]),
                _ => EffectConstValue::Other,
            }
        }
        _ => EffectConstValue::Other,
    }
}

fn write_bool_field(obj: &mut Value, key: &str, value: bool) {
    let Some(m) = obj.as_object_mut() else {
        return;
    };
    if let Some(Value::Object(existing)) = m.get(key) {
        let mut nm = existing.clone();
        nm.insert("value".into(), json!(value));
        m.insert(key.into(), Value::Object(nm));
    } else {
        m.insert(key.into(), json!(value));
    }
}

/// Replace a bool field with a plain JSON boolean (no user/script carrier).
/// Required so editor visibility/effect toggles actually stick on reload.
fn force_static_bool(obj: &mut Value, key: &str, value: bool) {
    let Some(m) = obj.as_object_mut() else {
        return;
    };
    m.insert(key.into(), Value::Bool(value));
}

fn write_f32_field(obj: &mut Value, key: &str, value: f32) {
    let Some(m) = obj.as_object_mut() else {
        return;
    };
    match m.get(key) {
        Some(Value::Object(existing)) => {
            let mut nm = existing.clone();
            nm.insert("value".into(), json!(value));
            m.insert(key.into(), Value::Object(nm));
        }
        Some(Value::String(_)) => {
            m.insert(key.into(), json!(format!("{value:.6}")));
        }
        _ => {
            m.insert(key.into(), json!(value));
        }
    }
}

fn write_vec3_field(obj: &mut Value, key: &str, v: [f32; 3]) {
    let Some(m) = obj.as_object_mut() else {
        return;
    };
    let formatted = format!("{:.6} {:.6} {:.6}", v[0], v[1], v[2]);
    match m.get(key) {
        Some(Value::Object(existing)) => {
            let mut nm = existing.clone();
            // Prefer string value like WE; keep animation/script siblings.
            if nm.contains_key("value") {
                // Match previous value type if possible.
                match nm.get("value") {
                    Some(Value::String(_)) => {
                        nm.insert("value".into(), json!(formatted));
                    }
                    Some(Value::Array(_)) => {
                        nm.insert("value".into(), json!([v[0], v[1], v[2]]));
                    }
                    _ => {
                        nm.insert("value".into(), json!(formatted));
                    }
                }
            } else {
                nm.insert("value".into(), json!(formatted));
            }
            m.insert(key.into(), Value::Object(nm));
        }
        Some(Value::Array(_)) => {
            m.insert(key.into(), json!([v[0], v[1], v[2]]));
        }
        _ => {
            // Default WE string form.
            m.insert(key.into(), json!(formatted));
        }
    }
}

// ── Particle JSON field helpers ─────────────────────────────────────────────

fn known_system_keys() -> Vec<(&'static str, Value)> {
    vec![
        ("maxcount", json!(100)),
        ("starttime", json!(0.0)),
        ("sequencemultiplier", json!(1.0)),
        ("animationmode", json!("sequence")),
        ("material", json!("")),
        ("flags", json!(0)),
    ]
}

fn known_renderer_keys(name: &str) -> Vec<(&'static str, Value)> {
    let _ = name;
    vec![
        ("name", json!("sprite")),
        ("length", json!(0.0)),
        ("minlength", json!(0.0)),
        ("maxlength", json!(1.0)),
    ]
}

fn known_emitter_keys(name: &str) -> Vec<(&'static str, Value)> {
    let mut v = vec![
        ("name", json!(name)),
        ("rate", json!(10.0)),
        ("origin", json!("0 0 0")),
        ("directions", json!("1 1 1")),
        ("distancemin", json!(0.0)),
        ("distancemax", json!(32.0)),
        ("sign", json!("0 0 0")),
        ("speedmin", json!(0.0)),
        ("speedmax", json!(0.0)),
        ("delay", json!(0.0)),
        ("duration", json!(0.0)),
        ("flags", json!(0)),
        ("controlpoint", json!(0)),
    ];
    if name.contains("sphere") || name.contains("box") {
        v.push(("instantaneous", json!(0)));
        v.push(("minperiodicdelay", json!(0.0)));
        v.push(("maxperiodicdelay", json!(0.0)));
        v.push(("minperiodicduration", json!(0.0)));
        v.push(("maxperiodicduration", json!(0.0)));
        v.push(("maxtoemitperperiod", json!(0)));
    }
    v
}

fn known_initializer_keys(name: &str) -> Vec<(&'static str, Value)> {
    let mut v = vec![("name", json!(name))];
    match name {
        "lifetimerandom" | "sizerandom" | "alpharandom" => {
            v.extend([
                ("min", json!(0.0)),
                ("max", json!(1.0)),
                ("exponent", json!(1.0)),
            ]);
        }
        "velocityrandom" | "rotationrandom" | "angularvelocityrandom" => {
            v.extend([("min", json!("0 0 0")), ("max", json!("0 0 0"))]);
        }
        "colorrandom" => {
            v.extend([
                ("min", json!("255 255 255")),
                ("max", json!("255 255 255")),
                ("exponent", json!(1.0)),
            ]);
        }
        "hsvcolorrandom" => {
            v.extend([
                ("huemin", json!(0.0)),
                ("huemax", json!(360.0)),
                ("saturationmin", json!(1.0)),
                ("valuemin", json!(1.0)),
            ]);
        }
        "turbulentvelocityrandom" => {
            v.extend([
                ("scale", json!(0.1)),
                ("offset", json!(0.0)),
                ("speedmin", json!(50.0)),
                ("speedmax", json!(100.0)),
                ("phasemin", json!(0.0)),
                ("phasemax", json!(1.0)),
                ("timescale", json!(1.0)),
                ("forward", json!("0 1 0")),
                ("right", json!("1 0 0")),
            ]);
        }
        "positionoffsetrandom" => {
            v.extend([
                ("distance", json!(0.0)),
                ("scale", json!(1.0)),
                ("timescale", json!(1.0)),
            ]);
        }
        _ => {
            v.extend([("min", json!(0.0)), ("max", json!(1.0))]);
        }
    }
    v
}

fn known_operator_keys(name: &str) -> Vec<(&'static str, Value)> {
    let mut v = vec![("name", json!(name))];
    match name {
        "alphafade" => {
            v.extend([("fadeintime", json!(0.1)), ("fadeouttime", json!(0.7))]);
        }
        "alphachange" => {
            v.extend([
                ("startvalue", json!(1.0)),
                ("endvalue", json!(0.0)),
                ("starttime", json!(0.0)),
                ("endtime", json!(1.0)),
            ]);
        }
        "sizechange" => {
            v.extend([
                ("startvalue", json!(1.0)),
                ("endvalue", json!(0.0)),
                ("starttime", json!(0.0)),
                ("endtime", json!(1.0)),
            ]);
        }
        "colorchange" => {
            v.extend([
                ("startvalue", json!("1 1 1")),
                ("endvalue", json!("1 1 1")),
                ("starttime", json!(0.0)),
                ("endtime", json!(1.0)),
            ]);
        }
        "movement" => {
            v.extend([
                ("drag", json!(0.0)),
                ("gravity", json!("0 0 0")),
                ("flags", json!(0)),
            ]);
        }
        "angularmovement" => {
            v.extend([("drag", json!(0.0)), ("force", json!("0 0 0"))]);
        }
        "oscillatealpha" | "oscillatesize" => {
            v.extend([
                ("frequencymin", json!(1.0)),
                ("frequencymax", json!(1.0)),
                ("scalemin", json!(0.0)),
                ("scalemax", json!(1.0)),
                ("phasemin", json!(0.0)),
                ("phasemax", json!(1.0)),
                ("blendinstart", json!(0.0)),
                ("blendinend", json!(0.0)),
            ]);
        }
        "oscillateposition" => {
            v.extend([
                ("frequencymin", json!("1 1 1")),
                ("frequencymax", json!("1 1 1")),
                ("scalemin", json!("0 0 0")),
                ("scalemax", json!("1 1 1")),
                ("phasemin", json!("0 0 0")),
                ("phasemax", json!("1 1 1")),
                ("mask", json!("1 1 1")),
                ("blendinstart", json!(0.0)),
                ("blendinend", json!(0.0)),
            ]);
        }
        "turbulence" => {
            v.extend([
                ("scale", json!(1.0)),
                ("speedmin", json!(0.0)),
                ("speedmax", json!(0.0)),
                ("timescale", json!(1.0)),
                ("phasemin", json!(0.0)),
                ("phasemax", json!(1.0)),
                ("mask", json!("1 1 1")),
                ("blendinstart", json!(0.0)),
                ("blendinend", json!(0.0)),
                ("blendoutstart", json!(1.0)),
                ("blendoutend", json!(1.0)),
            ]);
        }
        "controlpointattract" => {
            v.extend([
                ("controlpoint", json!(0)),
                ("origin", json!("0 0 0")),
                ("scale", json!(1.0)),
                ("threshold", json!(0.0)),
                ("flags", json!(0)),
                ("blendinstart", json!(0.0)),
            ]);
        }
        "vortex" | "vortex_v2" => {
            v.extend([
                ("distanceinner", json!(0.0)),
                ("distanceouter", json!(100.0)),
                ("speedinner", json!(0.0)),
                ("speedouter", json!(0.0)),
                ("flags", json!(0)),
                ("controlpoint", json!(0)),
                ("axis", json!("0 0 1")),
            ]);
        }
        "capvelocity" => {
            v.extend([
                ("maxspeed", json!(100.0)),
                ("blendinstart", json!(0.0)),
                ("blendinend", json!(0.0)),
            ]);
        }
        "boids" => {
            v.extend([
                ("separationfactor", json!(1.0)),
                ("alignmentfactor", json!(1.0)),
                ("cohesionfactor", json!(1.0)),
                ("neighborthreshold", json!(50.0)),
                ("flags", json!(0)),
            ]);
        }
        _ => {
            // Generic useful knobs
            v.extend([
                ("flags", json!(0)),
                ("blendinstart", json!(0.0)),
                ("blendinend", json!(0.0)),
            ]);
        }
    }
    v
}

fn known_controlpoint_keys(_name: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("offset", json!("0 0 0")),
        ("flags", json!(0)),
        ("parentcontrolpoint", json!(0)),
    ]
}

fn known_child_keys(_name: &str) -> Vec<(&'static str, Value)> {
    vec![
        ("name", json!("")),
        ("type", json!("static")),
        ("origin", json!("0 0 0")),
        ("angles", json!("0 0 0")),
        ("scale", json!("1 1 1")),
        ("maxcount", json!(10)),
        ("probability", json!(1.0)),
        ("flags", json!(0)),
    ]
}

fn push_addable_for_array(
    out: &mut Vec<ParticleAddableKey>,
    root: &Map<String, Value>,
    arr_key: &str,
    prefix: &str,
    title: &str,
    known: fn(&str) -> Vec<(&'static str, Value)>,
) {
    let Some(Value::Array(arr)) = root.get(arr_key) else {
        return;
    };
    for (i, item) in arr.iter().enumerate() {
        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let section = if name.is_empty() {
            format!("{title} {i}")
        } else {
            format!("{title} {i} · {name}")
        };
        let slot = format!("{prefix}{i}");
        let Some(obj) = item.as_object() else {
            continue;
        };
        for (field, def) in known(&name) {
            if field == "name" {
                continue; // always present for named blocks
            }
            if obj.contains_key(field) && !obj.get(field).map(|v| v.is_null()).unwrap_or(false) {
                continue;
            }
            out.push(ParticleAddableKey {
                key: format!("pd:{slot}:{field}"),
                section: section.clone(),
                field: field.to_string(),
                label: pretty_label(field),
                default: def,
                slot: slot.clone(),
            });
        }
    }
}

fn guess_default_for_field(field: &str) -> Value {
    let f = field.to_ascii_lowercase();
    match f.as_str() {
        "name" | "material" | "type" | "animationmode" => json!(""),
        "scale" => json!("1 1 1"),
        "origin" | "directions" | "gravity" | "offset" | "forward" | "right" | "axis" | "mask"
        | "angles" | "sign" => json!("0 0 0"),
        "min" | "max" if f == "min" || f == "max" => json!(0.0),
        k if k.contains("color") => json!("255 255 255"),
        _ => json!(0.0),
    }
}

fn insert_particle_key(doc: &mut Value, key: &str, default: Value) -> Result<(), String> {
    let rest = key
        .strip_prefix("pd:")
        .ok_or_else(|| format!("not a particle key: {key}"))?;
    // No component suffix on add — always whole field.
    let (parent, field) = locate_particle_slot(doc, rest)?;
    if parent.contains_key(&field) && !parent.get(&field).map(|v| v.is_null()).unwrap_or(false) {
        return Err(format!("key «{field}» already exists"));
    }
    parent.insert(field, default);
    Ok(())
}

fn normalize_particle_rel(path: &str) -> String {
    let p = path.trim().trim_start_matches("./").replace('\\', "/");
    if p.starts_with("particles/") {
        p
    } else if p.is_empty() {
        p
    } else {
        format!("particles/{p}")
    }
}

fn resolve_particle_path(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let candidates = [
        dir.join(rel),
        dir.join("particles")
            .join(rel.trim_start_matches("particles/")),
        dir.join(rel.trim_start_matches("particles/")),
    ];
    for c in &candidates {
        if c.is_file() {
            return Ok(c.clone());
        }
    }
    Err(format!(
        "particle file not found: {rel} (looked under {})",
        dir.display()
    ))
}

fn pretty_label(key: &str) -> String {
    match key {
        "maxcount" => "Max count".into(),
        "starttime" => "Start time".into(),
        "sequencemultiplier" => "Sequence ×".into(),
        "animationmode" => "Animation".into(),
        "distancemin" => "Distance min".into(),
        "distancemax" => "Distance max".into(),
        "speedmin" => "Speed min".into(),
        "speedmax" => "Speed max".into(),
        "fadeintime" => "Fade in".into(),
        "fadeouttime" => "Fade out".into(),
        "startvalue" => "Start value".into(),
        "endvalue" => "End value".into(),
        "endtime" => "End time".into(),
        "colorn" => "Color".into(),
        "phasemin" => "Phase min".into(),
        "phasemax" => "Phase max".into(),
        "frequencymin" => "Freq min".into(),
        "frequencymax" => "Freq max".into(),
        "scalemin" => "Scale min".into(),
        "scalemax" => "Scale max".into(),
        "minlength" => "Min length".into(),
        "maxlength" => "Max length".into(),
        "blendinstart" => "Blend in start".into(),
        "blendinend" => "Blend in end".into(),
        "blendoutstart" => "Blend out start".into(),
        "blendoutend" => "Blend out end".into(),
        "parentcontrolpoint" => "Parent CP".into(),
        "controlpoint" => "Control point".into(),
        "maxtoemitperperiod" => "Max / period".into(),
        "minperiodicdelay" => "Period delay min".into(),
        "maxperiodicdelay" => "Period delay max".into(),
        "minperiodicduration" => "Period dur min".into(),
        "maxperiodicduration" => "Period dur max".into(),
        other => {
            let mut s = String::new();
            for (i, c) in other.chars().enumerate() {
                if c == '_' {
                    s.push(' ');
                } else if i == 0 {
                    s.push(c.to_ascii_uppercase());
                } else {
                    s.push(c);
                }
            }
            s
        }
    }
}

fn particle_field_range(key: &str, value: f32) -> (f32, f32) {
    let k = key.to_ascii_lowercase();
    // Axis components inherit from parent field name (passed as full key tip).
    let base = k.split('.').next().unwrap_or(&k);
    match base {
        "rate" => (0.0, 200.0),
        "maxcount" | "count" | "maxtoemitperperiod" => (0.0, 2000.0),
        "starttime" | "endtime" | "delay" | "duration" => (0.0, 30.0),
        "sequencemultiplier" => (0.0, 16.0),
        "drag" => (0.0, 5.0),
        "alpha" | "fadeintime" | "fadeouttime" | "probability" | "exponent" => (0.0, 1.0),
        "startvalue" | "endvalue" if value.abs() <= 2.0 => (0.0, 2.0),
        "length" | "minlength" | "maxlength" => (0.0, value.abs().max(2.0) * 2.0),
        "flags" | "sign" | "controlpoint" | "parentcontrolpoint" | "id" => (-16.0, 64.0),
        "speedmin" | "speedmax" | "speed" => {
            let a = value.abs().max(50.0);
            (-a * 2.0, a * 2.0)
        }
        "min" | "max" if value.abs() > 20.0 && value.abs() <= 255.0 => (0.0, 255.0),
        "origin" | "directions" | "gravity" | "offset" | "forward" | "right" | "axis" => {
            let a = value.abs().max(100.0);
            (-a * 2.0, a * 2.0)
        }
        "distancemin" | "distancemax" | "distance" | "distanceinner" | "distanceouter"
        | "threshold" | "ringradius" | "ringwidth" | "ringpulldistance" => {
            let a = value.abs().max(32.0);
            (0.0, a * 3.0)
        }
        "huemin" | "huemax" => (0.0, 360.0),
        "saturationmin" | "valuemin" => (0.0, 1.0),
        _ => {
            let a = value.abs();
            if a <= 1.0 {
                (-1.0, 1.0)
            } else if a <= 5.0 {
                (-5.0, 5.0)
            } else if a <= 20.0 {
                (-20.0, 20.0)
            } else if a <= 100.0 {
                (-a * 2.0, a * 2.0)
            } else if a <= 255.0 {
                (0.0, 255.0)
            } else {
                (-a * 2.0, a * 2.0)
            }
        }
    }
}

fn push_value_fields(
    out: &mut Vec<ParticleField>,
    section: &str,
    label: String,
    key_base: &str,
    v: &Value,
    field_name: &str,
) {
    // Unwrap { "value": … } carriers.
    let v = match v {
        Value::Object(m) if m.contains_key("value") => m.get("value").unwrap_or(v),
        other => other,
    };
    if v.is_null() {
        // Still expose animationmode etc. as empty text so user can set them.
        if matches!(field_name, "animationmode" | "material" | "name" | "type") {
            out.push(ParticleField {
                key: key_base.to_string(),
                section: section.to_string(),
                label,
                kind: ParticleFieldKind::Text {
                    value: String::new(),
                },
            });
        }
        return;
    }

    // Vector string "x y z" or array of 2–4 numbers → per-component floats.
    if let Some(vec) = coerce_vec(v) {
        if vec.len() >= 2 {
            let axes = ["x", "y", "z", "w"];
            for (i, comp) in vec.iter().enumerate().take(4) {
                let axis = axes.get(i).copied().unwrap_or("?");
                let (lo, hi) = particle_field_range(field_name, *comp);
                out.push(ParticleField {
                    key: format!("{key_base}.{axis}"),
                    section: section.to_string(),
                    label: format!("{label} {axis}"),
                    kind: ParticleFieldKind::Float {
                        value: *comp,
                        lo,
                        hi,
                    },
                });
            }
            return;
        }
    }

    if let Some(f) = coerce_f32(v) {
        let (lo, hi) = particle_field_range(field_name, f);
        out.push(ParticleField {
            key: key_base.to_string(),
            section: section.to_string(),
            label,
            kind: ParticleFieldKind::Float { value: f, lo, hi },
        });
        return;
    }

    if let Some(s) = coerce_text(v) {
        out.push(ParticleField {
            key: key_base.to_string(),
            section: section.to_string(),
            label,
            kind: ParticleFieldKind::Text { value: s },
        });
    }
}

fn coerce_f32(v: &Value) -> Option<f32> {
    match v {
        Value::Number(n) => n.as_f64().map(|f| f as f32),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::String(s) => {
            let t = s.trim();
            if t.contains(char::is_whitespace) {
                // multi-component — not a scalar
                return None;
            }
            t.parse().ok()
        }
        Value::Object(m) => m.get("value").and_then(coerce_f32),
        _ => None,
    }
}

fn coerce_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Object(m) => m.get("value").and_then(coerce_text),
        _ => None,
    }
}

fn coerce_vec(v: &Value) -> Option<Vec<f32>> {
    match v {
        Value::String(s) => {
            let parts: Vec<f32> = s
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            if parts.len() >= 2 {
                Some(parts)
            } else {
                None
            }
        }
        Value::Array(a) => {
            let parts: Vec<f32> = a.iter().filter_map(coerce_f32).collect();
            if parts.len() >= 2 {
                Some(parts)
            } else {
                None
            }
        }
        Value::Object(m) => m.get("value").and_then(coerce_vec),
        _ => None,
    }
}

enum ParticleWrite {
    Float(f32),
    Text(String),
}

/// Apply a `pd:…` key write into the particle JSON root.
fn apply_particle_key(doc: &mut Value, key: &str, write: ParticleWrite) -> Result<(), String> {
    let rest = key
        .strip_prefix("pd:")
        .ok_or_else(|| format!("not a particle key: {key}"))?;

    // Component suffix .x/.y/.z/.w
    let (path_str, component) = {
        if let Some((base, axis)) = rest.rsplit_once('.') {
            match axis {
                "x" => (base, Some(0usize)),
                "y" => (base, Some(1)),
                "z" => (base, Some(2)),
                "w" => (base, Some(3)),
                _ => (rest, None),
            }
        } else {
            (rest, None)
        }
    };

    // Locate parent object + field name.
    let (parent, field) = locate_particle_slot(doc, path_str)?;

    match write {
        ParticleWrite::Text(s) => {
            if component.is_some() {
                return Err("cannot write text into a vector component".into());
            }
            write_json_text(parent, field, &s);
        }
        ParticleWrite::Float(f) => {
            if let Some(axis) = component {
                write_json_vec_comp(parent, field, axis, f)?;
            } else {
                write_json_f32(parent, field, f);
            }
        }
    }
    Ok(())
}

/// Resolve `maxcount` / `em0:rate` / `in2:min` → (parent object map, field key).
fn locate_particle_slot<'a>(
    doc: &'a mut Value,
    path: &str,
) -> Result<(&'a mut Map<String, Value>, String), String> {
    // Group prefixes: emN, inN, opN, rdN, cpN, chN
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        i += 1;
    }
    let mut j = i;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        j += 1;
    }

    let root = doc
        .as_object_mut()
        .ok_or("particle root is not an object")?;

    if i > 0 && j > i && (j == bytes.len() || bytes[j] == b':') {
        let prefix = &path[..i];
        let idx: usize = path[i..j]
            .parse()
            .map_err(|_| format!("bad index in {path}"))?;
        let field_rest = if j < path.len() {
            path[j..].strip_prefix(':').unwrap_or(&path[j..])
        } else {
            return Err(format!("missing field after {prefix}{idx}"));
        };
        let arr_key = match prefix {
            "em" => "emitter",
            "in" => "initializer",
            "op" => "operator",
            "rd" => "renderer",
            "cp" => "controlpoint",
            "ch" => "children",
            _ => {
                // Not a group — treat whole path as top-level field.
                return Ok((root, path.to_string()));
            }
        };
        let arr = root
            .get_mut(arr_key)
            .and_then(|v| v.as_array_mut())
            .ok_or_else(|| format!("missing array «{arr_key}»"))?;
        let item = arr
            .get_mut(idx)
            .ok_or_else(|| format!("{arr_key}[{idx}] out of range"))?;
        let map = item
            .as_object_mut()
            .ok_or_else(|| format!("{arr_key}[{idx}] is not an object"))?;
        return Ok((map, field_rest.to_string()));
    }

    // Top-level field
    Ok((root, path.to_string()))
}

fn write_json_f32(parent: &mut Map<String, Value>, field: String, value: f32) {
    match parent.get(&field) {
        Some(Value::Object(existing)) if existing.contains_key("value") => {
            let mut nm = existing.clone();
            match nm.get("value") {
                Some(Value::String(_)) => {
                    nm.insert("value".into(), json!(format!("{value}")));
                }
                _ => {
                    nm.insert("value".into(), json!(value));
                }
            }
            parent.insert(field, Value::Object(nm));
        }
        Some(Value::String(_)) => {
            // Keep string form for scalar strings (some WE fields use "10").
            parent.insert(field, json!(format!("{value}")));
        }
        _ => {
            parent.insert(field, json!(value));
        }
    }
}

fn write_json_text(parent: &mut Map<String, Value>, field: String, value: &str) {
    match parent.get(&field) {
        Some(Value::Object(existing)) if existing.contains_key("value") => {
            let mut nm = existing.clone();
            nm.insert("value".into(), json!(value));
            parent.insert(field, Value::Object(nm));
        }
        Some(Value::Null) | None => {
            // animationmode null → set string (or null if empty)
            if value.is_empty() || value.eq_ignore_ascii_case("null") {
                parent.insert(field, Value::Null);
            } else {
                parent.insert(field, json!(value));
            }
        }
        _ => {
            parent.insert(field, json!(value));
        }
    }
}

fn write_json_vec_comp(
    parent: &mut Map<String, Value>,
    field: String,
    axis: usize,
    value: f32,
) -> Result<(), String> {
    let current = parent.get(&field).cloned().unwrap_or(json!("0 0 0"));
    // Unwrap value carrier
    let (is_carrier, inner) = match &current {
        Value::Object(m) if m.contains_key("value") => {
            (true, m.get("value").cloned().unwrap_or(json!("0 0 0")))
        }
        other => (false, other.clone()),
    };

    let mut comps: Vec<f32> = match &inner {
        Value::String(s) => {
            let mut p: Vec<f32> = s
                .split_whitespace()
                .filter_map(|t| t.parse().ok())
                .collect();
            while p.len() < axis + 1 {
                p.push(0.0);
            }
            p
        }
        Value::Array(a) => {
            let mut p: Vec<f32> = a.iter().filter_map(coerce_f32).collect();
            while p.len() < axis + 1 {
                p.push(0.0);
            }
            p
        }
        Value::Number(n) => {
            let f = n.as_f64().unwrap_or(0.0) as f32;
            let mut p = vec![f];
            while p.len() < axis + 1 {
                p.push(0.0);
            }
            p
        }
        _ => {
            let mut p = vec![0.0; axis + 1];
            p[axis] = value;
            p
        }
    };
    if axis >= comps.len() {
        comps.resize(axis + 1, 0.0);
    }
    comps[axis] = value;

    let new_inner = match &inner {
        Value::Array(_) => Value::Array(comps.iter().map(|c| json!(c)).collect()),
        _ => {
            // WE default: space-separated string
            let s = comps
                .iter()
                .map(|c| {
                    if (*c - c.round()).abs() < 1e-4 {
                        format!("{}", *c as i64)
                    } else {
                        format!("{c}")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            json!(s)
        }
    };

    if is_carrier {
        if let Value::Object(m) = current {
            let mut nm = m;
            nm.insert("value".into(), new_inner);
            parent.insert(field, Value::Object(nm));
        }
    } else {
        parent.insert(field, new_inner);
    }
    Ok(())
}

/// Whether a library entry can be opened in the scene editor.
pub fn is_editable_scene(dir: &Path, wallpaper_type: WallpaperType) -> bool {
    if wallpaper_type == WallpaperType::Scene {
        return dir.join("scene.pkg").is_file() || dir.join("scene.json").is_file();
    }
    dir.join("scene.pkg").is_file() || dir.join("scene.json").is_file()
}

/// True if this is a wallstudio-owned project (safe to rename/delete).
pub fn is_local_project(dir: &Path) -> bool {
    let root = wallengine_projects_dir();
    let root_c = root.canonicalize().unwrap_or(root);
    match dir.canonicalize() {
        Ok(c) => c.starts_with(&root_c),
        Err(_) => dir.starts_with(&root_c) || dir.starts_with(wallengine_projects_dir()),
    }
}

/// Rename the display title in project.json (library name).
pub fn rename_project_title(dir: &Path, new_title: &str) -> Result<(), String> {
    let title = new_title.trim();
    if title.is_empty() {
        return Err("title cannot be empty".into());
    }
    let path = dir.join("project.json");
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut raw: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let obj = raw
        .as_object_mut()
        .ok_or_else(|| "project.json root is not an object".to_string())?;
    obj.insert("title".into(), json!(title));
    let out = serde_json::to_string_pretty(&raw).map_err(|e| e.to_string())?;
    fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(())
}

/// Permanently delete a local wallstudio project directory.
/// Refuses Steam workshop / WE install paths.
pub fn delete_local_project(dir: &Path) -> Result<(), String> {
    if !is_local_project(dir) {
        return Err(
            "refusing to delete: only projects under ~/.local/share/wallengine/projects/ can be removed"
                .into(),
        );
    }
    if !dir.is_dir() {
        return Err(format!("not a directory: {}", dir.display()));
    }
    fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_import_names_leave_room_for_extensions_and_collisions() {
        let stem = "x".repeat(255);
        let plain = imported_filename(&stem, "png", "");
        let collision = imported_filename(&stem, "png", "_999999");
        assert!(plain.len() <= MAX_GENERATED_COMPONENT_BYTES);
        assert!(collision.len() <= MAX_GENERATED_COMPONENT_BYTES);
        assert!(collision.ends_with("_999999.png"));
        assert_eq!(
            safe_extension(".very-long_extension-name!"),
            "verylongextensio"
        );
    }

    #[test]
    fn fork_ids_are_bounded_when_given_nonstandard_source_ids() {
        let id = unique_fork_id(&"source/".repeat(100));
        assert!(id.len() < 160, "fork id is too long: {}", id.len());
        assert!(!id.contains('/'));
    }

    #[test]
    fn fork_and_mutate_workshop_if_present() {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        let src = PathBuf::from(home)
            .join(".local/share/Steam/steamapps/workshop/content/431960/1918995622");
        if !src.join("project.json").is_file() {
            eprintln!("skip: workshop sample not installed");
            return;
        }
        let fork = fork_wallpaper(&src, "1918995622").expect("fork");
        assert!(fork.dir.join("scene.json").is_file());
        assert!(!fork.dir.join("scene.pkg").is_file());

        let mut ed = open_project_dir(&fork.dir).expect("open");
        let n = ed.layers().len();
        assert!(n > 0, "expected layers");
        let _ = ed.set_visible(0, false);
        assert!(ed.dirty);
        ed.save().expect("save");
        assert!(!ed.dirty);

        // cleanup
        let _ = fs::remove_dir_all(&fork.dir);
    }

    #[test]
    fn particle_fields_and_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "walld_ptcl_edit_{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("particles/presets")).unwrap();
        let particle = json!({
            "material": "materials/presets/smoke1.json",
            "maxcount": 50,
            "starttime": 1.0,
            "sequencemultiplier": 2.0,
            "animationmode": null,
            "emitter": [{
                "name": "sphererandom",
                "rate": 10.0,
                "origin": "0 0 0",
                "directions": "1 1 0",
                "distancemin": 0,
                "distancemax": 64
            }],
            "initializer": [
                { "name": "lifetimerandom", "min": 3, "max": 4 },
                { "name": "velocityrandom", "min": "-50 -50 0", "max": "50 50 0" },
                { "name": "colorrandom", "min": "100 100 100", "max": "200 200 200" }
            ],
            "operator": [
                { "name": "movement", "drag": 0.2, "gravity": "0 0 0" },
                { "name": "alphafade", "fadeintime": 0.5 }
            ],
            "renderer": [{ "name": "sprite" }]
        });
        fs::write(
            dir.join("particles/presets/smoke1.json"),
            serde_json::to_string_pretty(&particle).unwrap(),
        )
        .unwrap();
        let scene = json!({
            "general": { "orthogonalprojection": { "width": 1920, "height": 1080 } },
            "objects": [{
                "id": 1,
                "name": "Smoke cloud",
                "particle": "particles/presets/smoke1.json",
                "origin": "100 200 0",
                "scale": "1 1 1",
                "angles": "0 0 0",
                "alpha": 1.0,
                "instanceoverride": { "rate": 1.12, "speed": 0.05, "size": 1.0, "alpha": 0.07, "count": 1.0, "lifetime": 1.0 }
            }]
        });
        fs::write(
            dir.join("scene.json"),
            serde_json::to_string_pretty(&scene).unwrap(),
        )
        .unwrap();
        fs::write(
            dir.join("project.json"),
            r#"{"title":"ptcl test","type":"scene","file":"scene.json"}"#,
        )
        .unwrap();

        let mut ed = open_project_dir(&dir).expect("open");
        ed.ensure_particle_doc(0).expect("load particle");
        let fields = ed.particle_fields(0);
        assert!(
            fields.len() > 15,
            "expected full field list, got {}",
            fields.len()
        );
        // Must include system + emitter + init + op fields
        assert!(fields.iter().any(|f| f.key == "pd:maxcount"));
        assert!(fields.iter().any(|f| f.key == "pd:em0:rate"));
        assert!(fields.iter().any(|f| f.key == "pd:in0:min"));
        assert!(fields.iter().any(|f| f.key == "pd:in1:min.x"));
        assert!(fields.iter().any(|f| f.key == "pd:op0:drag"));
        assert!(fields.iter().any(|f| f.key == "pd:op0:gravity.y"));

        ed.set_particle_field_f32(0, "pd:em0:rate", 42.0).unwrap();
        ed.set_particle_field_f32(0, "pd:op0:gravity.y", -80.0)
            .unwrap();
        ed.set_particle_field_text(0, "pd:animationmode", "sequence")
            .unwrap();
        ed.set_particle_override_color(0, [1.2, 0.8, 0.5]).unwrap();
        ed.save().unwrap();

        // Re-read disk
        let disk: Value = serde_json::from_str(
            &fs::read_to_string(dir.join("particles/presets/smoke1.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(disk["emitter"][0]["rate"].as_f64().unwrap(), 42.0);
        let grav = disk["operator"][0]["gravity"].as_str().unwrap();
        assert!(grav.contains("-80"), "gravity rewritten: {grav}");
        assert_eq!(disk["animationmode"].as_str().unwrap(), "sequence");

        let col = ed.read_particle_override_colorn(0);
        assert!((col[0] - 1.2).abs() < 1e-4);

        let _ = fs::remove_dir_all(&dir);
    }
}
