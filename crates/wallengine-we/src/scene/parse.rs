//! Parse WE scene.json + linked materials / particle docs.
//! Applies project.json user properties and parent-chain transforms.

use super::model::*;
use crate::assets::AssetResolver;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

pub fn parse_scene_file(scene_path: &Path, assets: &AssetResolver) -> Result<WeSceneDoc, String> {
    parse_scene_file_with_props(scene_path, assets, &HashMap::new())
}

/// Parse scene, applying `project.json` `general.properties` user bindings.
pub fn parse_scene_file_with_props(
    scene_path: &Path,
    assets: &AssetResolver,
    props: &HashMap<String, Value>,
) -> Result<WeSceneDoc, String> {
    let text = std::fs::read_to_string(scene_path).map_err(|e| e.to_string())?;
    let root: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    parse_scene_value(&root, assets, props)
}

pub fn parse_scene_value(
    root: &Value,
    assets: &AssetResolver,
    props: &HashMap<String, Value>,
) -> Result<WeSceneDoc, String> {
    let version = root
        .get("version")
        .and_then(|v| v.as_i64())
        .unwrap_or(1) as i32;

    let general = root.get("general");
    let ortho = general
        .and_then(|g| g.get("orthogonalprojection"))
        .cloned()
        .unwrap_or(Value::Null);
    let ortho_width = json_f32(ortho.get("width"), 1920.0).max(1.0);
    let ortho_height = json_f32(ortho.get("height"), 1080.0).max(1.0);

    let clear = parse_vec3_or_default(
        general.and_then(|g| g.get("clearcolor")),
        [0.0, 0.0, 0.0],
    );
    let camera_center = parse_vec3_or_default(
        root.get("camera").and_then(|c| c.get("center")),
        [0.0, 0.0, 0.0],
    );

    let arr = root
        .get("objects")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    // Pass 1: every object (groups needed for parent transforms).
    let mut all: Vec<WeObject> = Vec::with_capacity(arr.len());
    for obj in &arr {
        all.push(parse_object(obj, assets, props, [ortho_width, ortho_height]));
    }

    // Snapshot the hierarchy with *local* transforms and each object's **own**
    // visibility (not inherited — `resolve_world` ANDs the chain per tick, so a
    // script re-showing a parent correctly reveals its children).
    let graph: Vec<GraphNode> = all
        .iter()
        .map(|o| GraphNode {
            id: o.id,
            name: o.name.clone(),
            parent: o.parent,
            local_origin: o.origin,
            origin_base: o.origin,
            local_scale: o.scale,
            local_angles: o.angles,
            visible: o.visible,
            alpha: o.alpha,
            alpha_base: o.alpha,
            alpha_anim: o.alpha_anim.clone(),
            origin_anim: o.origin_anim.clone(),
            anim_rate: 1.0,
        })
        .collect();

    // Hide descendants of invisible groups for the *initial* pose.
    inherit_parent_visibility(&mut all);

    // Parent-chain → world origin/scale/angle.
    resolve_parent_transforms(&mut all);

    // Load every drawable object regardless of current visibility: scripts
    // routinely reveal statically-hidden layers (flipbook frames, variant
    // sets, intro cards). Visibility is resolved per tick, not at parse.
    let objects: Vec<WeObject> = all
        .into_iter()
        .filter(|o| {
            matches!(
                o.kind,
                WeObjectKind::Image { .. }
                    | WeObjectKind::Particle { .. }
                    | WeObjectKind::Text { .. }
            )
        })
        .collect();

    log::info!(
        "scene parse: {} drawable layers (after props + parent visibility)",
        objects.len()
    );

    Ok(WeSceneDoc {
        version,
        ortho_width,
        ortho_height,
        clear_color: clear,
        camera_center,
        objects,
        graph,
        raw_objects: arr,
    })
}

/// Load `general.properties` from project.json if present.
pub fn load_project_properties(wallpaper_dir: &Path) -> HashMap<String, Value> {
    let pj = wallpaper_dir.join("project.json");
    let Ok(text) = std::fs::read_to_string(&pj) else {
        return HashMap::new();
    };
    let Ok(raw) = serde_json::from_str::<Value>(&text) else {
        return HashMap::new();
    };
    let mut out = HashMap::new();
    if let Some(Value::Object(map)) = raw
        .pointer("/general/properties")
        .cloned()
        .or_else(|| raw.get("general").and_then(|g| g.get("properties")).cloned())
    {
        for (k, v) in map {
            // Store the whole property object; resolvers read `.value`.
            out.insert(k, v);
        }
    }
    out
}

fn prop_value<'a>(props: &'a HashMap<String, Value>, key: &str) -> Option<&'a Value> {
    props.get(key).and_then(|p| {
        if let Some(v) = p.get("value") {
            Some(v)
        } else {
            Some(p)
        }
    })
}

fn parse_object(
    obj: &Value,
    assets: &AssetResolver,
    props: &HashMap<String, Value>,
    ortho: [f32; 2],
) -> WeObject {
    let id = obj.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let visible = resolve_bool(obj.get("visible"), props, true);
    let origin = resolve_origin(obj.get("origin"), props, ortho);
    let size = resolve_vec2(obj.get("size"), props, [0.0, 0.0]);
    let scale = resolve_scale(obj.get("scale"), props);
    let angles = resolve_vec3(obj.get("angles"), props, [0.0, 0.0, 0.0]);
    let parent = obj.get("parent").and_then(|v| {
        if v.is_null() {
            None
        } else {
            v.as_i64().or_else(|| v.as_u64().map(|u| u as i64))
        }
    });
    let attachment = obj
        .get("attachment")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "null")
        .map(|s| s.to_string());
    let alignment = obj
        .get("alignment")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let effects = parse_effects(obj.get("effects"), props);
    let particle_override = parse_particle_override(obj.get("instanceoverride"));
    let color_blend_mode = obj
        .get("colorBlendMode")
        .and_then(|v| v.as_i64().or_else(|| v.get("value").and_then(|x| x.as_i64())))
        .unwrap_or(0) as i32;
    let alpha = resolve_f32(obj.get("alpha"), props, 1.0);
    let alpha_scripted = obj
        .get("alpha")
        .map(|v| v.get("script").is_some())
        .unwrap_or(false);
    let brightness = resolve_f32(obj.get("brightness"), props, 1.0);
    let color = obj
        .get("color")
        .map(|v| resolve_vec3(Some(v), props, [1.0, 1.0, 1.0]));
    let alpha_anim = obj.get("alpha").and_then(crate::scene::timeline::Timeline::parse);
    let origin_anim = obj.get("origin").and_then(crate::scene::timeline::Timeline::parse);
    let angle_time = obj
        .get("angles")
        .and_then(|v| v.get("script"))
        .and_then(|v| v.as_str())
        .and_then(crate::scene::timeline::AngleTimeBinding::parse);

    let kind = if let Some(t) = obj.get("text") {
        WeObjectKind::Text {
            font: obj
                .get("font")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            pointsize: resolve_f32(obj.get("pointsize"), props, 32.0),
            literal: t.as_str().map(|s| s.to_string()),
            script: t
                .get("script")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string()),
        }
    } else if let Some(p) = obj.get("particle").and_then(|v| v.as_str()) {
        if !p.is_empty() && p != "null" {
            WeObjectKind::Particle {
                path: p.to_string(),
            }
        } else {
            WeObjectKind::Other
        }
    } else if let Some(img) = obj.get("image").and_then(|v| v.as_str()) {
        if !img.is_empty() && img != "null" {
            match resolve_image_material(assets, img) {
                Some(resolved) => WeObjectKind::Image {
                    model: img.to_string(),
                    material: resolved.material,
                    texture: resolved.texture,
                    puppet: resolved.puppet,
                    crop_offset: resolved.crop_offset,
                    solidlayer: resolved.solidlayer,
                    fullscreen: resolved.fullscreen,
                    composelayer: resolved.composelayer,
                },
                None => {
                    log::debug!("scene object «{name}»: unresolved image {img}");
                    WeObjectKind::Other
                }
            }
        } else {
            WeObjectKind::Other
        }
    } else {
        WeObjectKind::Other
    };

    WeObject {
        id,
        name,
        visible,
        origin,
        size,
        scale,
        angles,
        parent,
        attachment,
        alignment,
        kind,
        color_blend_mode,
        alpha,
        alpha_scripted,
        brightness,
        color,
        alpha_anim,
        origin_anim,
        angle_time,
        effects,
        particle_override,
    }
}

fn resolve_f32(v: Option<&Value>, props: &HashMap<String, Value>, default: f32) -> f32 {
    match v {
        None => default,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default as f64) as f32,
        Some(Value::String(s)) => s.trim().parse().unwrap_or(default),
        Some(Value::Object(o)) => {
            // Timeline animations play from the first keyframe; the sibling
            // `value` is just the editor's last state (often a stale 0).
            if let Some(kf) = anim_first_value(o, "c0") {
                return kf;
            }
            if let Some(Value::String(user)) = o.get("user") {
                if let Some(pv) = prop_value(props, user) {
                    return resolve_f32(Some(pv), props, default);
                }
            }
            if let Some(val) = o.get("value") {
                return resolve_f32(Some(val), props, default);
            }
            default
        }
        _ => default,
    }
}

/// First keyframe value of channel `chan` ("c0"/"c1"/"c2") in a WE timeline
/// animation object, if present.
fn anim_first_value(o: &serde_json::Map<String, Value>, chan: &str) -> Option<f32> {
    o.get("animation")?
        .get(chan)?
        .as_array()?
        .first()?
        .get("value")?
        .as_f64()
        .map(|v| v as f32)
}

fn parse_particle_override(v: Option<&Value>) -> ParticleInstanceOverride {
    let mut o = ParticleInstanceOverride::default();
    let Some(Value::Object(map)) = v else {
        return o;
    };
    if let Some(n) = map.get("rate").and_then(|x| x.as_f64()) {
        o.rate = n as f32;
    }
    if let Some(n) = map.get("speed").and_then(|x| x.as_f64()) {
        o.speed = n as f32;
    }
    if let Some(n) = map.get("size").and_then(|x| x.as_f64()) {
        o.size = n as f32;
    }
    if let Some(n) = map.get("alpha").and_then(|x| x.as_f64()) {
        o.alpha = n as f32;
    }
    if let Some(n) = map.get("count").and_then(|x| x.as_f64()) {
        o.count = n as f32;
    }
    if let Some(n) = map.get("lifetime").and_then(|x| x.as_f64()) {
        o.lifetime = n as f32;
    }
    if let Some(c) = map.get("colorn").or_else(|| map.get("color")) {
        o.colorn = parse_vec3_or_default(Some(c), [1.0, 1.0, 1.0]);
    }
    o
}

struct ResolvedImage {
    material: String,
    texture: String,
    puppet: Option<String>,
    crop_offset: [f32; 2],
    solidlayer: bool,
    fullscreen: bool,
    composelayer: bool,
}

fn resolve_bool(v: Option<&Value>, props: &HashMap<String, Value>, default: bool) -> bool {
    match v {
        None => default,
        Some(Value::Bool(b)) => *b,
        Some(Value::Object(o)) => {
            // Combo condition: `"user": {"condition": "1", "name": "timevarying"}`
            // Visible when property `name` equals `condition` (string compare).
            if let Some(Value::Object(user)) = o.get("user") {
                if let (Some(Value::String(name)), Some(cond)) =
                    (user.get("name"), user.get("condition"))
                {
                    let cond_s = match cond {
                        Value::String(s) => s.clone(),
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => if *b { "1" } else { "0" }.into(),
                        _ => String::new(),
                    };
                    if let Some(pv) = prop_value(props, name) {
                        let pv_s = match pv {
                            Value::String(s) => s.clone(),
                            Value::Number(n) => n.to_string(),
                            Value::Bool(b) => if *b { "true" } else { "false" }.into(),
                            _ => String::new(),
                        };
                        return pv_s == cond_s;
                    }
                    // No property → fall through to stored default value.
                }
            }
            if let Some(Value::String(user)) = o.get("user") {
                if let Some(pv) = prop_value(props, user) {
                    return match pv {
                        Value::Bool(b) => *b,
                        Value::Number(n) => n.as_f64().unwrap_or(0.0) != 0.0,
                        Value::String(s) => {
                            s == "true" || s == "1" || s.eq_ignore_ascii_case("yes")
                        }
                        _ => o
                            .get("value")
                            .and_then(|x| x.as_bool())
                            .unwrap_or(default),
                    };
                }
            }
            o.get("value")
                .and_then(|x| x.as_bool())
                .unwrap_or(default)
        }
        _ => default,
    }
}

/// Origin: plain vec, user-linked, scripted, or timeline-animated.
///
/// Timeline animations (`"animation": { c0, c1, c2, options }`) are sampled at
/// frame 0 for static load (full clock-driven evaluation happens at runtime when
/// present). This avoids using a mid-timeline editor snapshot that parks the
/// moon / stars at the wrong place.
fn resolve_origin(v: Option<&Value>, props: &HashMap<String, Value>, ortho: [f32; 2]) -> [f32; 3] {
    let Some(v) = v else {
        return [0.0, 0.0, 0.0];
    };
    if let Value::Object(o) = v {
        // Timeline animation: sample frame 0 of c0/c1/c2 channels.
        if let Some(anim) = o.get("animation") {
            if let Some(sampled) = sample_animation_vec3(anim, 0.0) {
                return sampled;
            }
        }
        if o.contains_key("script") {
            if let Some(Value::Object(sp)) = o.get("scriptproperties") {
                // Prefer named position keys used by WE editor scripts.
                if sp.contains_key("xPosition")
                    || sp.contains_key("yPosition")
                    || sp.contains_key("zPosition")
                {
                    return [
                        resolve_script_num(sp.get("xPosition"), props, 0.0),
                        resolve_script_num(sp.get("yPosition"), props, 0.0),
                        resolve_script_num(sp.get("zPosition"), props, 0.0),
                    ];
                }
                // Unit-slider template: origin = (x, y) × canvas size.
                if sp.contains_key("x") && sp.contains_key("y") {
                    return [
                        resolve_script_num(sp.get("x"), props, 0.5) * ortho[0],
                        resolve_script_num(sp.get("y"), props, 0.5) * ortho[1],
                        0.0,
                    ];
                }
            }
            // Script present but no position props → static value field.
            if let Some(val) = o.get("value") {
                return resolve_vec3(Some(val), props, [0.0, 0.0, 0.0]);
            }
        }
    }
    resolve_vec3(Some(v), props, [0.0, 0.0, 0.0])
}

/// Sample a WE timeline animation at `frame` (can be fractional).
/// Channels c0/c1/c2 hold keyframed scalars for x/y/z.
fn sample_animation_vec3(anim: &Value, frame: f32) -> Option<[f32; 3]> {
    let obj = anim.as_object()?;
    let c0 = sample_channel(obj.get("c0")?, frame).unwrap_or(0.0);
    let c1 = sample_channel(obj.get("c1")?, frame).unwrap_or(0.0);
    let c2 = obj
        .get("c2")
        .and_then(|c| sample_channel(c, frame))
        .unwrap_or(0.0);
    Some([c0, c1, c2])
}

fn sample_channel(channel: &Value, frame: f32) -> Option<f32> {
    let keys = channel.as_array()?;
    if keys.is_empty() {
        return None;
    }
    // Key: { "frame": N, "value": F }
    let mut points: Vec<(f32, f32)> = Vec::new();
    for k in keys {
        let f = k.get("frame").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
        let val = k
            .get("value")
            .and_then(|v| {
                v.as_f64()
                    .map(|x| x as f32)
                    .or_else(|| v.as_str()?.parse().ok())
            })
            .unwrap_or(0.0);
        points.push((f, val));
    }
    points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    if frame <= points[0].0 {
        return Some(points[0].1);
    }
    if frame >= points[points.len() - 1].0 {
        return Some(points[points.len() - 1].1);
    }
    for w in points.windows(2) {
        let (f0, v0) = w[0];
        let (f1, v1) = w[1];
        if frame >= f0 && frame <= f1 {
            let t = if (f1 - f0).abs() < 1e-6 {
                0.0
            } else {
                (frame - f0) / (f1 - f0)
            };
            return Some(v0 + (v1 - v0) * t);
        }
    }
    Some(points[0].1)
}

fn resolve_script_num(v: Option<&Value>, props: &HashMap<String, Value>, default: f32) -> f32 {
    match v {
        None => default,
        Some(Value::Number(n)) => n.as_f64().unwrap_or(default as f64) as f32,
        Some(Value::Object(o)) => {
            if let Some(Value::String(user)) = o.get("user") {
                if let Some(pv) = prop_value(props, user) {
                    return json_f32(Some(pv), default);
                }
            }
            json_f32(o.get("value"), default)
        }
        Some(Value::String(s)) => s.parse().unwrap_or(default),
        _ => default,
    }
}

fn resolve_scale(v: Option<&Value>, props: &HashMap<String, Value>) -> [f32; 3] {
    match v {
        None => [1.0, 1.0, 1.0],
        Some(Value::Object(o)) => {
            if let Some(Value::String(user)) = o.get("user") {
                if let Some(pv) = prop_value(props, user) {
                    // Slider often stores a single float for uniform scale
                    if let Some(n) = pv.as_f64() {
                        let f = n as f32;
                        return [f, f, f];
                    }
                    if let Some(s) = pv.as_str() {
                        if let Some(v3) = parse_vec3(s) {
                            return v3;
                        }
                        if let Ok(f) = s.parse::<f32>() {
                            return [f, f, f];
                        }
                    }
                }
            }
            resolve_vec3(Some(&Value::Object(o.clone())), props, [1.0, 1.0, 1.0])
        }
        other => resolve_vec3(other, props, [1.0, 1.0, 1.0]),
    }
}

fn resolve_vec3(v: Option<&Value>, props: &HashMap<String, Value>, default: [f32; 3]) -> [f32; 3] {
    match v {
        None => default,
        Some(Value::String(s)) => parse_vec3(s).unwrap_or(default),
        Some(Value::Number(n)) => {
            let f = n.as_f64().unwrap_or(0.0) as f32;
            [f, f, f]
        }
        Some(Value::Object(o)) => {
            if o.get("animation").is_some() {
                return [
                    anim_first_value(o, "c0").unwrap_or(default[0]),
                    anim_first_value(o, "c1").unwrap_or(default[1]),
                    anim_first_value(o, "c2").unwrap_or(default[2]),
                ];
            }
            if let Some(Value::String(user)) = o.get("user") {
                if let Some(pv) = prop_value(props, user) {
                    return resolve_vec3(Some(pv), props, default);
                }
            }
            if let Some(val) = o.get("value") {
                return resolve_vec3(Some(val), props, default);
            }
            default
        }
        _ => default,
    }
}

fn resolve_vec2(v: Option<&Value>, props: &HashMap<String, Value>, default: [f32; 2]) -> [f32; 2] {
    let v3 = resolve_vec3(
        v,
        props,
        [default[0], default[1], 0.0],
    );
    [v3[0], v3[1]]
}

/// If an ancestor is hidden, hide this object (WE parent visibility inheritance).
fn inherit_parent_visibility(objects: &mut [WeObject]) {
    let id_to_idx: HashMap<i64, usize> = objects
        .iter()
        .enumerate()
        .filter(|(_, o)| o.id != 0)
        .map(|(i, o)| (o.id, i))
        .collect();
    let parents: Vec<Option<i64>> = objects.iter().map(|o| o.parent).collect();
    let base_vis: Vec<bool> = objects.iter().map(|o| o.visible).collect();

    for i in 0..objects.len() {
        let mut vis = base_vis[i];
        let mut cur = parents[i];
        let mut guard = 0;
        while let Some(pid) = cur {
            guard += 1;
            if guard > 32 {
                break;
            }
            if let Some(&pi) = id_to_idx.get(&pid) {
                if !base_vis[pi] {
                    vis = false;
                    break;
                }
                cur = parents[pi];
            } else {
                break;
            }
        }
        objects[i].visible = vis;
    }
}

/// Walk parent chains (LWE `CImage::resolveTransform`) and bake world origin/scale/angle.
fn resolve_parent_transforms(objects: &mut [WeObject]) {
    let locals: Vec<(i64, [f32; 3], [f32; 3], f32, Option<i64>)> = objects
        .iter()
        .map(|o| (o.id, o.origin, o.scale, o.angles[2], o.parent))
        .collect();

    let id_to_local: HashMap<i64, usize> = locals
        .iter()
        .enumerate()
        .filter(|(_, (id, ..))| *id != 0)
        .map(|(i, (id, ..))| (*id, i))
        .collect();

    for i in 0..objects.len() {
        let resolved = resolve_one(i, &locals, &id_to_local);
        objects[i].origin = resolved.origin;
        objects[i].scale = resolved.scale;
        objects[i].angles[2] = resolved.angle_z;
    }
}

fn resolve_one(
    idx: usize,
    locals: &[(i64, [f32; 3], [f32; 3], f32, Option<i64>)],
    id_to_local: &HashMap<i64, usize>,
) -> ResolvedTransform {
    let mut chain = Vec::new();
    let mut cur = Some(idx);
    let mut guard = 0;
    while let Some(i) = cur {
        chain.push(i);
        guard += 1;
        if guard > 32 {
            break;
        }
        cur = locals[i]
            .4
            .and_then(|pid| id_to_local.get(&pid).copied());
    }

    let root = *chain.last().unwrap();
    let (_, ro, rs, ra, _) = locals[root];
    let mut resolved = ResolvedTransform {
        origin: ro,
        scale: rs,
        angle_z: ra,
    };

    for &ci in chain.iter().rev().skip(1) {
        let (_, lo, ls, la, _) = locals[ci];
        let ox = lo[0] * resolved.scale[0];
        let oy = lo[1] * resolved.scale[1];
        let (rx, ry) = rotate2(ox, oy, resolved.angle_z);
        resolved = ResolvedTransform {
            origin: [
                resolved.origin[0] + rx,
                resolved.origin[1] + ry,
                resolved.origin[2] + lo[2] * resolved.scale[2],
            ],
            scale: [
                ls[0] * resolved.scale[0],
                ls[1] * resolved.scale[1],
                ls[2] * resolved.scale[2],
            ],
            angle_z: resolved.angle_z + la,
        };
    }
    resolved
}

fn rotate2(x: f32, y: f32, angle: f32) -> (f32, f32) {
    let c = angle.cos();
    let s = angle.sin();
    (x * c - y * s, x * s + y * c)
}

fn resolve_image_material(assets: &AssetResolver, image_ref: &str) -> Option<ResolvedImage> {
    // Raw FBO name references aren't standalone layers.
    if image_ref.contains("_rt_") || image_ref.contains("FullFrameBuffer") {
        return None;
    }

    // Compose layer: samples the backbuffer in its screen rect + effect stack.
    // Must not be dropped — Jett's TV censor (pixelate/vhs/pulse) lives here.
    let is_compose = image_ref.contains("util/composelayer")
        || image_ref.ends_with("composelayer.json")
        || image_ref.ends_with("composelayer_depthtest.json");
    if is_compose {
        return Some(ResolvedImage {
            material: "materials/util/composelayer.json".into(),
            texture: String::new(),
            puppet: None,
            crop_offset: [0.0, 0.0],
            solidlayer: false,
            fullscreen: false,
            composelayer: true,
        });
    }

    let model_val = assets.read_json(image_ref).ok()?;
    let solidlayer = model_val
        .get("solidlayer")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || image_ref.contains("solidlayer");
    let fullscreen = model_val
        .get("fullscreen")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
        || image_ref.contains("fullscreenlayer");
    let puppet = model_val
        .get("puppet")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && *s != "null")
        .map(|s| s.to_string());
    let crop_offset = parse_vec2_or_default(model_val.get("cropoffset"), [0.0, 0.0]);

    let material = model_val
        .get("material")
        .and_then(|v| v.as_str())
        .unwrap_or(image_ref)
        .to_string();

    if material.contains("_rt_") || material.contains("FullFrameBuffer") {
        return None;
    }

    // solidlayer has no real texture — handled as a solid-color full/rect layer.
    if solidlayer {
        return Some(ResolvedImage {
            material,
            texture: String::new(),
            puppet,
            crop_offset,
            solidlayer: true,
            fullscreen,
            composelayer: false,
        });
    }

    let mat_val = assets.read_json(&material).ok();
    let texture = mat_val
        .as_ref()
        .and_then(|v| {
            v.get("passes")
                .and_then(|p| p.as_array())
                .and_then(|a| a.first())
                .and_then(|p| p.get("textures"))
                .and_then(|t| t.as_array())
                .and_then(|a| a.first())
                .and_then(|t| t.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| {
            Path::new(&material)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| material.clone())
        });

    if texture.starts_with("_rt_") {
        return None;
    }

    Some(ResolvedImage {
        material,
        texture,
        puppet,
        crop_offset,
        solidlayer,
        fullscreen,
        composelayer: false,
    })
}

fn parse_effects(v: Option<&Value>, props: &HashMap<String, Value>) -> Vec<WeEffectInstance> {
    let Some(Value::Array(arr)) = v else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for ef in arr {
        let file = ef
            .get("file")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if file.is_empty() {
            continue;
        }
        let name = ef
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Resolve `{ "user": "fluidanimation", "value": true }` against props
        // so toggles in project.json / wallstudio actually gate effects.
        let visible = resolve_bool(ef.get("visible"), props, true);
        if !visible {
            continue;
        }
        let mut passes = Vec::new();
        if let Some(Value::Array(parr)) = ef.get("passes") {
            for p in parr {
                let mut textures = Vec::new();
                if let Some(Value::Array(tarr)) = p.get("textures") {
                    for t in tarr {
                        textures.push(t.as_str().map(|s| s.to_string()));
                    }
                }
                let mut constants = HashMap::new();
                if let Some(Value::Object(cobj)) = p.get("constantshadervalues") {
                    for (k, val) in cobj {
                        constants.insert(k.clone(), parse_effect_value(val));
                    }
                }
                let mut combos = HashMap::new();
                if let Some(Value::Object(cobj)) = p.get("combos") {
                    for (k, val) in cobj {
                        if let Some(n) = val.as_i64() {
                            combos.insert(k.clone(), n as i32);
                        } else if let Some(n) = val.as_u64() {
                            combos.insert(k.clone(), n as i32);
                        }
                    }
                }
                passes.push(WeEffectPassInstance {
                    textures,
                    constants,
                    combos,
                });
            }
        }
        out.push(WeEffectInstance {
            file,
            name,
            visible,
            passes,
        });
    }
    out
}

/// Emitter `sign` per-axis clamp ("1 0 -1" or array form).
fn parse_sign(v: Option<&Value>) -> [i32; 3] {
    let to_i = |x: &Value| -> i32 {
        match x {
            Value::Number(n) => n.as_f64().unwrap_or(0.0) as i32,
            Value::String(s) => s.parse::<f32>().map(|f| f as i32).unwrap_or(0),
            _ => 0,
        }
    };
    match v {
        Some(Value::String(s)) => {
            let n: Vec<i32> = s.split_whitespace().map(|p| p.parse::<f32>().map(|f| f as i32).unwrap_or(0)).collect();
            [*n.first().unwrap_or(&0), *n.get(1).unwrap_or(&0), *n.get(2).unwrap_or(&0)]
        }
        Some(Value::Array(a)) => {
            let n: Vec<i32> = a.iter().map(to_i).collect();
            [*n.first().unwrap_or(&0), *n.get(1).unwrap_or(&0), *n.get(2).unwrap_or(&0)]
        }
        Some(Value::Number(n)) => [n.as_f64().unwrap_or(0.0) as i32; 3],
        _ => [0, 0, 0],
    }
}

pub fn parse_particle_doc(assets: &AssetResolver, path: &str) -> Result<ParticleDoc, String> {
    let val = assets
        .read_json(path)
        .or_else(|_| {
            let p = format!("particles/{}", path.trim_start_matches("particles/"));
            assets.read_json(&p)
        })
        .map_err(|e| e.to_string())?;

    let material = val
        .get("material")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let maxcount = json_u32(val.get("maxcount"), 100);
    let starttime = json_f32(val.get("starttime"), 0.0);
    let animation_mode = match val
        .get("animationmode")
        .and_then(|v| v.as_str())
        .unwrap_or("sequence")
        .to_ascii_lowercase()
        .as_str()
    {
        "randomframe" | "random" => ParticleAnimationMode::RandomFrame,
        _ => ParticleAnimationMode::Sequence,
    };
    let sequence_multiplier = json_f32(val.get("sequencemultiplier"), 1.0).max(0.01);
    let renderer = {
        let mut r = ParticleRendererDoc::default();
        if let Some(Value::Array(arr)) = val.get("renderer") {
            if let Some(first) = arr.first() {
                r.name = first
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sprite")
                    .to_ascii_lowercase();
                r.length = json_f32(first.get("length"), 0.0);
                r.minlength = json_f32(first.get("minlength"), 0.0);
                r.maxlength = json_f32(first.get("maxlength"), 1.0).max(r.minlength);
            }
        }
        r
    };

    // Resolve blend/overbright from particle material (additive snow vs translucent smoke).
    let mut blending = "additive".to_string();
    let mut overbright = 1.0f32;
    let mut texture = String::new();
    if !material.is_empty() {
        if let Ok(mat) = assets.read_json(&material) {
            if let Some(pass) = mat
                .get("passes")
                .and_then(|p| p.as_array())
                .and_then(|a| a.first())
            {
                if let Some(b) = pass.get("blending").and_then(|v| v.as_str()) {
                    blending = b.to_ascii_lowercase();
                }
                if let Some(t) = pass
                    .get("textures")
                    .and_then(|v| v.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str())
                {
                    if !t.is_empty() {
                        texture = t.to_string();
                    }
                }
                if let Some(c) = pass
                    .get("constantshadervalues")
                    .and_then(|v| v.as_object())
                {
                    if let Some(ob) = c
                        .get("ui_editor_properties_overbright")
                        .or_else(|| c.get("overbright"))
                    {
                        overbright = json_f32(Some(ob), 1.0);
                    }
                }
            }
        }
    }

    let mut emitters = Vec::new();
    if let Some(Value::Array(arr)) = val.get("emitter") {
        for e in arr {
            emitters.push(EmitterDoc {
                name: e
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sphererandom")
                    .to_string(),
                rate: json_f32(e.get("rate"), 10.0),
                origin: parse_vec3_or_default(e.get("origin"), [0.0, 0.0, 0.0]),
                directions: parse_vec3_or_default(e.get("directions"), [1.0, 1.0, 1.0]),
                distancemin: json_vec3_extent(e.get("distancemin"), 0.0),
                distancemax: json_vec3_extent(e.get("distancemax"), 32.0),
                sign: parse_sign(e.get("sign")),
                speedmin: json_f32(e.get("speedmin"), 0.0),
                speedmax: json_f32(e.get("speedmax"), 0.0),
            });
        }
    }
    if emitters.is_empty() {
        emitters.push(EmitterDoc {
            name: "sphererandom".into(),
            rate: 10.0,
            origin: [0.0, 0.0, 0.0],
            directions: [1.0, 1.0, 1.0],
            distancemin: [0.0, 0.0, 0.0],
            distancemax: [32.0, 32.0, 32.0],
            sign: [0, 0, 0],
            speedmin: 0.0,
            speedmax: 0.0,
        });
    }

    let mut initializers = Vec::new();
    if let Some(Value::Array(arr)) = val.get("initializer") {
        for init in arr {
            let name = init
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let min = init
                .get("min")
                .map(parse_effect_value)
                .unwrap_or(EffectValue::Float(0.0));
            let max = init
                .get("max")
                .map(parse_effect_value)
                .unwrap_or_else(|| min.clone());
            let mut extras = HashMap::new();
            if let Value::Object(obj) = init {
                for (k, v) in obj {
                    if matches!(k.as_str(), "name" | "min" | "max" | "id") {
                        continue;
                    }
                    extras.insert(k.clone(), parse_effect_value(v));
                }
            }
            initializers.push(InitializerDoc {
                name,
                min,
                max,
                extras,
            });
        }
    }

    let mut operators = Vec::new();
    if let Some(Value::Array(arr)) = val.get("operator") {
        for op in arr {
            let name = op
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mut params = HashMap::new();
            if let Value::Object(obj) = op {
                for (k, v) in obj {
                    if matches!(k.as_str(), "name" | "id") {
                        continue;
                    }
                    params.insert(k.clone(), parse_effect_value(v));
                }
            }
            operators.push(OperatorDoc { name, params });
        }
    }

    Ok(ParticleDoc {
        material,
        maxcount,
        starttime,
        emitters,
        initializers,
        operators,
        blending,
        overbright,
        texture,
        animation_mode,
        sequence_multiplier,
        renderer,
    })
}
