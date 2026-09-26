//! Typed models for Wallpaper Engine scene.json / materials / effects / particles.

use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct WeSceneDoc {
    pub version: i32,
    pub ortho_width: f32,
    pub ortho_height: f32,
    pub clear_color: [f32; 3],
    pub camera_center: [f32; 3],
    pub objects: Vec<WeObject>,
    /// Full object hierarchy with *local* (parent-relative) transforms,
    /// captured before parent resolution. Drives runtime re-resolution when
    /// scripts/timelines move groups.
    pub graph: Vec<GraphNode>,
    /// Raw scene.json objects — the SceneScript host reads scripted
    /// properties and their `scriptproperties` bindings from here.
    pub raw_objects: Vec<serde_json::Value>,
}

/// One node of the authoring hierarchy with local transform state.
#[derive(Debug, Clone)]
pub struct GraphNode {
    pub id: i64,
    pub name: String,
    pub parent: Option<i64>,
    pub local_origin: [f32; 3],
    /// Authored origin before timeline (required for `"relative": true` anims).
    pub origin_base: [f32; 3],
    pub local_scale: [f32; 3],
    /// Radians.
    pub local_angles: [f32; 3],
    pub visible: bool,
    pub alpha: f32,
    pub alpha_base: f32,
    /// Timeline animations on this node's properties.
    pub alpha_anim: Option<super::timeline::Timeline>,
    pub origin_anim: Option<super::timeline::Timeline>,
    /// Playback-rate multiplier (scripts set it via getAnimation().rate).
    pub anim_rate: f32,
}

#[derive(Debug, Clone)]
pub struct WeObject {
    pub id: i64,
    pub name: String,
    pub visible: bool,
    /// Local origin (parent-relative when `parent` is set). Scene space: (0,0) bottom-left of ortho, Y-up.
    pub origin: [f32; 3],
    pub size: [f32; 2],
    pub scale: [f32; 3],
    /// Radians (WE scene.json stores radians for static angles).
    pub angles: [f32; 3],
    /// Parent object id, if any (WE layer hierarchy).
    pub parent: Option<i64>,
    /// Named attachment socket on the parent puppet (`"orb"`, …).
    /// Child origin is relative to that socket, not the parent layer center.
    pub attachment: Option<String>,
    /// Alignment string: "top", "center", "topleft", …
    pub alignment: String,
    pub kind: WeObjectKind,
    /// WE per-object blend against the backdrop (common_blending.h table; 0 = normal).
    pub color_blend_mode: i32,
    /// Static layer opacity (SceneScript may animate it in WE; 0 = hidden).
    pub alpha: f32,
    /// True when `alpha` is script-driven — the layer must load even at 0.
    pub alpha_scripted: bool,
    /// Albedo multiplier.
    pub brightness: f32,
    /// Tint / solidlayer fill color. None when the object declares no color
    /// (colorless solidlayers are UI/FBO targets, not drawable fills).
    pub color: Option<[f32; 3]>,
    /// Timeline animation on `alpha` (c0), played by the runtime.
    pub alpha_anim: Option<super::timeline::Timeline>,
    /// Timeline animation on `origin` (c0/c1 screen px), played by the runtime.
    pub origin_anim: Option<super::timeline::Timeline>,
    /// Clock-hand binding parsed from an `angles` script (engine.timeOfDay).
    pub angle_time: Option<super::timeline::AngleTimeBinding>,
    pub effects: Vec<WeEffectInstance>,
    /// Particle instance overrides from scene.json (`instanceoverride`).
    pub particle_override: ParticleInstanceOverride,
}

/// Per-instance particle tweaks stored on the scene object (not the particle JSON).
#[derive(Debug, Clone)]
pub struct ParticleInstanceOverride {
    pub rate: f32,
    pub speed: f32,
    pub size: f32,
    pub alpha: f32,
    pub count: f32,
    pub lifetime: f32,
    pub colorn: [f32; 3],
}

impl Default for ParticleInstanceOverride {
    fn default() -> Self {
        Self {
            rate: 1.0,
            speed: 1.0,
            size: 1.0,
            alpha: 1.0,
            count: 1.0,
            lifetime: 1.0,
            colorn: [1.0, 1.0, 1.0],
        }
    }
}

/// World-space transform after walking the parent chain.
#[derive(Debug, Clone, Copy)]
pub struct ResolvedTransform {
    pub origin: [f32; 3],
    pub scale: [f32; 3],
    /// Accumulated Z rotation (radians).
    pub angle_z: f32,
}

#[derive(Debug, Clone)]
pub enum WeObjectKind {
    Image {
        /// models/foo.json
        model: String,
        /// materials/…tex stem resolved from material
        material: String,
        /// texture name from first pass
        texture: String,
        /// Optional puppet mesh path (models/…_puppet.mdl)
        puppet: Option<String>,
        /// Model crop offset (pixels), applied like LWE
        crop_offset: [f32; 2],
        /// solidlayer / fullscreen from model
        solidlayer: bool,
        fullscreen: bool,
        /// util/composelayer — samples the framebuffer (what's drawn so far)
        /// in this layer's screen rect, then runs its effect stack (pixelate/
        /// vhs/censor bars, etc.) and draws the result in place.
        composelayer: bool,
    },
    Particle {
        /// particles/….json
        path: String,
    },
    Text {
        /// fonts/foo.ttf (package-relative)
        font: String,
        pointsize: f32,
        /// Static string, when the text isn't script-driven.
        literal: Option<String>,
        /// SceneScript source driving the text, if any.
        script: Option<String>,
    },
    /// Sound / unknown — skipped for now
    Other,
}

#[derive(Debug, Clone)]
pub struct WeEffectInstance {
    pub file: String,
    pub name: String,
    pub visible: bool,
    pub passes: Vec<WeEffectPassInstance>,
}

#[derive(Debug, Clone)]
pub struct WeEffectPassInstance {
    pub textures: Vec<Option<String>>,
    pub constants: HashMap<String, EffectValue>,
    pub combos: HashMap<String, i32>,
}

#[derive(Debug, Clone)]
pub enum EffectValue {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Vec4([f32; 4]),
    String(String),
    /// SceneScript-driven constant (e.g. SharedNoise → g_Noise / g_NoiseTime).
    /// `default` is the authored fallback; `script` is re-evaluated each frame.
    Scripted { default: f32, script: String },
}

impl EffectValue {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Self::Float(f) => Some(*f),
            Self::Vec2(v) => Some(v[0]),
            Self::Vec3(v) => Some(v[0]),
            Self::Vec4(v) => Some(v[0]),
            Self::String(s) => s.split_whitespace().next()?.parse().ok(),
            Self::Scripted { default, .. } => Some(*default),
        }
    }

    /// Resolve scripted constants against the live scene graph.
    ///
    /// Ender Pink Girl / many portrait wallpapers drive rotate2d/3d from a
    /// SharedNoise layer's angles each frame. Pure angle values sit near a
    /// cos() peak for long stretches (almost frozen). For time-like uniforms
    /// (`g_NoiseTime`, `g_Noise`) we therefore use **wall-clock time + angle
    /// offset** so motion stays continuous (like stock shake's `g_Time`) while
    /// SharedNoise still adds organic drift — matching WE's lively preview.
    pub fn resolve_for_draw(&self, graph: &[GraphNode], time: f32, uname: &str) -> Self {
        match self {
            Self::Scripted { default, script } => {
                let angle = eval_layer_angle_script(script, graph).unwrap_or(*default);
                let v = if is_time_like_noise_uniform(uname) {
                    // degrees from SharedNoise are O(1..10); scale into phase.
                    time + angle * 0.15
                } else {
                    angle
                };
                Self::Float(v)
            }
            other => other.clone(),
        }
    }

    pub fn as_vec2(&self) -> Option<[f32; 2]> {
        match self {
            Self::Vec2(v) => Some(*v),
            Self::Float(f) => Some([*f, *f]),
            Self::String(s) => parse_vec2(s),
            Self::Scripted { default, .. } => Some([*default, *default]),
            _ => None,
        }
    }
    pub fn as_vec3(&self) -> Option<[f32; 3]> {
        match self {
            Self::Vec3(v) => Some(*v),
            Self::Vec4(v) => Some([v[0], v[1], v[2]]),
            Self::Float(f) => Some([*f, *f, *f]),
            Self::String(s) => parse_vec3(s),
            Self::Scripted { default, .. } => Some([*default, *default, *default]),
            _ => None,
        }
    }
}

fn is_time_like_noise_uniform(uname: &str) -> bool {
    matches!(
        uname,
        "g_NoiseTime" | "g_Noise" | "g_NoiseValue" | "u_Noise" | "u_NoiseTime"
    )
}

/// Parse the common SceneScript pattern:
/// `return thisScene.getLayer("SharedNoise").angles.x;`
/// Returns the layer angle in **degrees** (WE script convention).
fn eval_layer_angle_script(script: &str, graph: &[GraphNode]) -> Option<f32> {
    // getLayer("Name") or getLayer('Name')
    let name = {
        let s = script;
        let key = "getLayer(";
        let i = s.find(key)?;
        let rest = &s[i + key.len()..];
        let rest = rest.trim_start();
        let quote = rest.chars().next()?;
        if quote != '"' && quote != '\'' {
            return None;
        }
        let rest = &rest[1..];
        let end = rest.find(quote)?;
        &rest[..end]
    };
    let comp = if script.contains(".angles.x") || script.contains(".angles['x']") {
        0
    } else if script.contains(".angles.y") || script.contains(".angles['y']") {
        1
    } else if script.contains(".angles.z") || script.contains(".angles['z']") {
        2
    } else {
        return None;
    };
    let node = graph.iter().find(|n| n.name == name)?;
    // Graph stores radians; scripts expose degrees.
    Some(node.local_angles[comp].to_degrees())
}

#[derive(Debug, Clone)]
pub struct MaterialDoc {
    pub passes: Vec<MaterialPass>,
}

#[derive(Debug, Clone)]
pub struct MaterialPass {
    pub textures: Vec<Option<String>>,
    pub shader: Option<String>,
    pub blending: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ParticleAnimationMode {
    /// Play TEXS frames stretched across each particle's lifetime (WE default).
    #[default]
    Sequence,
    /// Pick one random sheet cell at birth and hold it (leaves, multi-variant sprites).
    RandomFrame,
}

#[derive(Debug, Clone)]
pub struct ParticleRendererDoc {
    /// "sprite" | "spritetrail" | …
    pub name: String,
    /// Trail length factor (spritetrail): stretch along velocity.
    pub length: f32,
    pub minlength: f32,
    pub maxlength: f32,
}

impl Default for ParticleRendererDoc {
    fn default() -> Self {
        Self {
            name: "sprite".into(),
            length: 0.0,
            minlength: 0.0,
            maxlength: 1.0,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ParticleDoc {
    pub material: String,
    pub maxcount: u32,
    pub starttime: f32,
    pub emitters: Vec<EmitterDoc>,
    pub initializers: Vec<InitializerDoc>,
    pub operators: Vec<OperatorDoc>,
    /// Material blend: "additive" | "translucent" | "normal"
    pub blending: String,
    /// Material overbright (embers often 1.0+)
    pub overbright: f32,
    /// Sprite texture from the material's first pass (e.g. "particle/chromaticdot").
    pub texture: String,
    /// TEXS flipbook playback (`animationmode`).
    pub animation_mode: ParticleAnimationMode,
    /// How many sequence loops fit into one particle lifetime (WE sequence multiplier).
    pub sequence_multiplier: f32,
    pub renderer: ParticleRendererDoc,
}

#[derive(Debug, Clone)]
pub struct EmitterDoc {
    pub name: String,
    pub rate: f32,
    pub origin: [f32; 3],
    pub directions: [f32; 3],
    /// Per-axis shell/box extents. Scalar JSON (`32`) becomes `[32,32,32]`;
    /// vec strings like `"960 540 0"` (common for full-canvas boxrandom) keep
    /// each axis independent — previously collapsed to the scalar default 32.
    pub distancemin: [f32; 3],
    pub distancemax: [f32; 3],
    /// Per-axis sign clamp: 1 = force +, -1 = force −, 0 = both.
    pub sign: [i32; 3],
    /// Radial speed range (outward from spawn point) when nonzero.
    pub speedmin: f32,
    pub speedmax: f32,
}

#[derive(Debug, Clone)]
pub struct InitializerDoc {
    pub name: String,
    pub min: EffectValue,
    pub max: EffectValue,
    pub extras: HashMap<String, EffectValue>,
}

#[derive(Debug, Clone)]
pub struct OperatorDoc {
    pub name: String,
    pub params: HashMap<String, EffectValue>,
}

pub fn parse_vec2(s: &str) -> Option<[f32; 2]> {
    let mut it = s.split_whitespace();
    Some([it.next()?.parse().ok()?, it.next()?.parse().ok()?])
}

pub fn parse_vec3(s: &str) -> Option<[f32; 3]> {
    let mut it = s.split_whitespace();
    Some([
        it.next()?.parse().ok()?,
        it.next()?.parse().ok()?,
        // 2-component strings (sizes) are valid; z defaults to 0.
        it.next().and_then(|t| t.parse().ok()).unwrap_or(0.0),
    ])
}

pub fn parse_vec3_or_default(v: Option<&Value>, default: [f32; 3]) -> [f32; 3] {
    match v {
        Some(Value::String(s)) => parse_vec3(s).unwrap_or(default),
        Some(Value::Object(o)) => {
            if let Some(Value::String(s)) = o.get("value") {
                parse_vec3(s).unwrap_or(default)
            } else {
                default
            }
        }
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(0.0) as f32;
            [f, f, f]
        }
        _ => default,
    }
}

pub fn parse_vec2_or_default(v: Option<&Value>, default: [f32; 2]) -> [f32; 2] {
    match v {
        Some(Value::String(s)) => parse_vec2(s).unwrap_or(default),
        Some(Value::Object(o)) => {
            if let Some(Value::String(s)) = o.get("value") {
                parse_vec2(s).unwrap_or(default)
            } else {
                default
            }
        }
        _ => default,
    }
}

pub fn parse_bool_visible(v: Option<&Value>) -> bool {
    match v {
        None => true,
        Some(Value::Bool(b)) => *b,
        Some(Value::Object(o)) => o
            .get("value")
            .and_then(|x| x.as_bool())
            .unwrap_or(true),
        _ => true,
    }
}

pub fn parse_effect_value(v: &Value) -> EffectValue {
    match v {
        Value::Number(n) => EffectValue::Float(n.as_f64().unwrap_or(0.0) as f32),
        Value::String(s) => {
            let parts: Vec<f32> = s
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            match parts.len() {
                0 => EffectValue::String(s.clone()),
                1 => EffectValue::Float(parts[0]),
                2 => EffectValue::Vec2([parts[0], parts[1]]),
                3 => EffectValue::Vec3([parts[0], parts[1], parts[2]]),
                _ => EffectValue::Vec4([parts[0], parts[1], parts[2], parts[3]]),
            }
        }
        Value::Bool(b) => EffectValue::Float(if *b { 1.0 } else { 0.0 }),
        // User-linked / scripted constants:
        // `{ "user": "xray", "value": 0.2 }`
        // `{ "script": "return thisScene.getLayer(\"SharedNoise\").angles.x;", "value": 0 }`
        Value::Object(o) => {
            let default = o
                .get("value")
                .map(parse_effect_value)
                .and_then(|v| v.as_f32())
                .unwrap_or(0.0);
            if let Some(script) = o.get("script").and_then(|s| s.as_str()) {
                if !script.is_empty() {
                    return EffectValue::Scripted {
                        default,
                        script: script.to_string(),
                    };
                }
            }
            if let Some(val) = o.get("value") {
                parse_effect_value(val)
            } else {
                EffectValue::Float(default)
            }
        }
        _ => EffectValue::String(v.to_string()),
    }
}

pub fn json_f32(v: Option<&Value>, default: f32) -> f32 {
    match v {
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default as f64) as f32,
        Some(Value::String(s)) => {
            // First token: `"960 540 0"` → 960 (callers that need the full
            // vector should use `parse_vec3_or_default` / `json_vec3_extent`).
            s.split_whitespace()
                .next()
                .and_then(|t| t.parse().ok())
                .unwrap_or(default)
        }
        _ => default,
    }
}

/// Scalar or vec3 extent (emitter distancemin/max).
pub fn json_vec3_extent(v: Option<&Value>, default: f32) -> [f32; 3] {
    match v {
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(default as f64) as f32;
            [f, f, f]
        }
        Some(Value::String(s)) => {
            if let Some(v3) = parse_vec3(s) {
                // One component only → uniform.
                if s.split_whitespace().count() == 1 {
                    [v3[0], v3[0], v3[0]]
                } else {
                    v3
                }
            } else {
                [default, default, default]
            }
        }
        Some(Value::Array(a)) => {
            let x = a.first().and_then(|v| v.as_f64()).unwrap_or(default as f64) as f32;
            let y = a.get(1).and_then(|v| v.as_f64()).unwrap_or(x as f64) as f32;
            let z = a.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            [x, y, z]
        }
        _ => [default, default, default],
    }
}

pub fn json_u32(v: Option<&Value>, default: u32) -> u32 {
    match v {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(default as u64) as u32,
        Some(Value::String(s)) => s.parse().unwrap_or(default),
        _ => default,
    }
}
