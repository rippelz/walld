//! Wallpaper Engine user properties (`project.json` → `general.properties`).
//!
//! Defaults live in the workshop package. User overrides are stored under
//! `~/.config/walld/props/<workshop-id>.json` and merged at scene load.

use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One editable property, ready for UI binding.
#[derive(Debug, Clone)]
pub struct PropDef {
    pub key: String,
    pub label: String,
    pub kind: PropKind,
    pub order: i64,
    /// Current effective value (default merged with user override).
    pub value: PropValue,
    pub index: Option<i64>,
}

#[derive(Debug, Clone)]
pub enum PropKind {
    Bool,
    Slider {
        min: f64,
        max: f64,
        step: f64,
        fraction: bool,
    },
    Color,
    Text,
    Combo {
        options: Vec<(String, String)>, // (label, value)
    },
    /// Section header from WE `type: group` (not editable).
    Group,
    /// group / label / file / scenetexture — shown as read-only or skipped
    Other(String),
}

#[derive(Debug, Clone)]
pub enum PropValue {
    Bool(bool),
    Number(f64),
    Text(String),
    /// RGB in 0..1
    Color([f32; 3]),
}

impl PropValue {
    pub fn as_json(&self) -> Value {
        match self {
            PropValue::Bool(b) => Value::Bool(*b),
            PropValue::Number(n) => json!(n),
            PropValue::Text(s) => Value::String(s.clone()),
            PropValue::Color([r, g, b]) => Value::String(format!("{r} {g} {b}")),
        }
    }
}

/// Directory for per-wallpaper override JSON files.
pub fn props_override_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".config")
        });
    base.join("walld").join("props")
}

pub fn override_path_for_id(workshop_id: &str) -> PathBuf {
    let safe: String = workshop_id
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    props_override_dir().join(format!("{safe}.json"))
}

/// Load raw property objects from project.json (`general.properties`).
pub fn load_raw_properties(wallpaper_dir: &Path) -> HashMap<String, Value> {
    crate::scene::load_project_properties(wallpaper_dir)
}

/// Load user overrides as flat key → value JSON.
pub fn load_overrides(workshop_id: &str) -> HashMap<String, Value> {
    let path = override_path_for_id(workshop_id);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let Ok(Value::Object(map)) = serde_json::from_str(&text) else {
        return HashMap::new();
    };
    map.into_iter().collect()
}

/// Persist a single override (creates dir as needed).
pub fn set_override(workshop_id: &str, key: &str, value: Value) -> Result<(), String> {
    let dir = props_override_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = override_path_for_id(workshop_id);
    let mut map = load_overrides(workshop_id);
    map.insert(key.to_string(), value);
    let obj: Map<String, Value> = map.into_iter().collect();
    let text = serde_json::to_string_pretty(&Value::Object(obj)).map_err(|e| e.to_string())?;
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

pub fn clear_overrides(workshop_id: &str) -> Result<(), String> {
    let path = override_path_for_id(workshop_id);
    if path.is_file() {
        std::fs::remove_file(&path).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Merge defaults from project.json with user overrides.
/// Returns the full property object map (with `.value` patched) for scene load.
pub fn load_merged_properties(wallpaper_dir: &Path, workshop_id: &str) -> HashMap<String, Value> {
    let mut props = load_raw_properties(wallpaper_dir);
    let overrides = load_overrides(workshop_id);
    for (k, v) in overrides {
        if let Some(Value::Object(obj)) = props.get_mut(&k) {
            obj.insert("value".into(), v);
        } else {
            // Unknown key — still store so scripts can see it.
            props.insert(
                k,
                json!({
                    "type": "text",
                    "value": v,
                }),
            );
        }
    }
    props
}

/// UI-facing list of properties (skips empty groups).
pub fn list_props(wallpaper_dir: &Path, workshop_id: &str) -> Vec<PropDef> {
    let raw = load_raw_properties(wallpaper_dir);
    let overrides = load_overrides(workshop_id);
    let mut out = Vec::new();
    for (key, spec) in &raw {
        let Some(Value::Object(obj)) = Some(spec) else {
            continue;
        };
        let ty = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        // Skip non-UI structural types (keep groups as section headers).
        if matches!(ty.as_str(), "label" | "" | "scenetexture") {
            continue;
        }
        let label = human_label(
            obj.get("text")
                .and_then(|v| v.as_str())
                .unwrap_or(key.as_str()),
            key,
        );
        let order = obj.get("order").and_then(|v| v.as_i64()).unwrap_or(1000);
        let index = obj.get("index").and_then(|v| v.as_i64());
        let default_val = obj.get("value").cloned().unwrap_or(Value::Null);
        let effective = overrides.get(key).cloned().unwrap_or(default_val);
        let kind = match ty.as_str() {
            "group" => PropKind::Group,
            "bool" | "checkbox" => PropKind::Bool,
            "slider" => PropKind::Slider {
                min: obj.get("min").and_then(|v| v.as_f64()).unwrap_or(0.0),
                max: obj.get("max").and_then(|v| v.as_f64()).unwrap_or(1.0),
                step: obj.get("step").and_then(|v| v.as_f64()).unwrap_or(0.01),
                fraction: obj
                    .get("fraction")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(true),
            },
            "color" => PropKind::Color,
            "text" | "textinput" => PropKind::Text,
            "combo" => {
                let options = parse_combo_options(obj);
                PropKind::Combo { options }
            }
            "file" => PropKind::Other("file".into()),
            other => PropKind::Other(other.to_string()),
        };
        let value = match &kind {
            PropKind::Group => PropValue::Text(String::new()),
            PropKind::Bool => PropValue::Bool(coerce_bool(&effective)),
            PropKind::Slider { .. } => PropValue::Number(coerce_f64(&effective).unwrap_or(0.0)),
            PropKind::Color => PropValue::Color(parse_color(&effective)),
            PropKind::Text | PropKind::Combo { .. } | PropKind::Other(_) => {
                PropValue::Text(match &effective {
                    Value::String(s) => s.clone(),
                    Value::Bool(b) => b.to_string(),
                    Value::Number(n) => n.to_string(),
                    other => other.to_string(),
                })
            }
        };
        out.push(PropDef {
            key: key.clone(),
            label,
            kind,
            order,
            value,
            index,
        });
    }
    out.sort_by(|a, b| {
        a.order
            .cmp(&b.order)
            .then_with(|| a.index.cmp(&b.index))
            .then_with(|| a.key.cmp(&b.key))
    });
    out
}

fn parse_combo_options(obj: &Map<String, Value>) -> Vec<(String, String)> {
    let mut options = Vec::new();
    if let Some(Value::Array(arr)) = obj.get("options") {
        for o in arr {
            if let Value::Object(m) = o {
                let label = m
                    .get("label")
                    .or_else(|| m.get("text"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("?")
                    .to_string();
                let value = m
                    .get("value")
                    .map(|v| match v {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => v.to_string(),
                    })
                    .unwrap_or_else(|| label.clone());
                options.push((human_label(&label, &value), value));
            } else if let Some(s) = o.as_str() {
                options.push((s.to_string(), s.to_string()));
            }
        }
    }
    options
}

fn parse_color(v: &Value) -> [f32; 3] {
    match v {
        Value::String(s) => {
            let p: Vec<f32> = s
                .split_whitespace()
                .filter_map(|x| x.parse().ok())
                .collect();
            [
                p.first().copied().unwrap_or(0.0),
                p.get(1).copied().unwrap_or(0.0),
                p.get(2).copied().unwrap_or(0.0),
            ]
        }
        Value::Array(a) => [
            a.first().and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
            a.get(1).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
            a.get(2).and_then(|x| x.as_f64()).unwrap_or(0.0) as f32,
        ],
        _ => [0.0, 0.0, 0.0],
    }
}

/// WE often stores bools as `0`/`1` or `"true"`/`"false"`, not JSON bools.
fn coerce_bool(v: &Value) -> bool {
    match v {
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
        Value::String(s) => matches!(
            s.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        _ => false,
    }
}

fn coerce_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// Turn WE `ui_editor_properties_foo_bar` keys into readable labels.
pub fn human_label(text: &str, fallback: &str) -> String {
    let t = strip_html(text).trim().to_string();
    if t.is_empty() {
        return title_case(fallback);
    }
    let stripped = t
        .strip_prefix("ui_editor_properties_")
        .or_else(|| t.strip_prefix("ui_browse_properties_"))
        .unwrap_or(t.as_str());
    if stripped.contains(' ') && !stripped.starts_with("ui_") {
        return stripped.to_string();
    }
    title_case(stripped)
}

/// Strip simple HTML tags/entities from WE property labels.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("<br/>", " ")
        .replace("<br>", " ")
}

fn title_case(s: &str) -> String {
    s.split(|c: char| c == '_' || c == '-')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                None => String::new(),
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a CLI/UI string into a JSON value for a property key.
pub fn parse_value_for_prop(spec: &Value, raw: &str) -> Result<Value, String> {
    let ty = spec
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("text")
        .to_ascii_lowercase();
    let s = raw.trim();
    match ty.as_str() {
        "bool" | "checkbox" => {
            let b = matches!(
                s.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            );
            Ok(Value::Bool(b))
        }
        "slider" => {
            let n: f64 = s.parse().map_err(|_| format!("not a number: {s}"))?;
            Ok(json!(n))
        }
        "color" => {
            // Accept "r g b" floats or #rrggbb
            if let Some(hex) = s.strip_prefix('#') {
                if hex.len() >= 6 {
                    let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0) as f32 / 255.0;
                    let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0) as f32 / 255.0;
                    let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0) as f32 / 255.0;
                    return Ok(Value::String(format!("{r} {g} {b}")));
                }
            }
            Ok(Value::String(s.to_string()))
        }
        _ => Ok(Value::String(s.to_string())),
    }
}
