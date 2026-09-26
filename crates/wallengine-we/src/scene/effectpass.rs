//! Generic WE effect passes: load `effects/<name>/effect.json`, resolve each
//! pass's material → shader, translate the GLSL, and bind constants the way WE
//! does (uniform annotations `// {"material":"speed",...}` map scene.json
//! `constantshadervalues` keys onto uniform names).
//!
//! This is what makes still artwork move in WE (shake, waterwaves, ripple,
//! caustics, iris …) — the renderer runs these as fragment passes over the
//! layer's rendered texture.

use crate::assets::AssetResolver;
use crate::glsl::{preprocess_we_glsl, reconcile_stage_varyings, ShaderStage};
use crate::scene::model::{EffectValue, WeEffectInstance};
use serde_json::Value;
use std::collections::HashMap;

/// One compiled-ready effect pass.
#[derive(Debug, Clone)]
pub struct EffectPass {
    pub name: String,
    /// GLES 3.00 sources.
    pub vert: String,
    pub frag: String,
    /// Uniform name → scalar/vector value (already resolved from material
    /// defaults + scene.json `constantshadervalues`).
    pub uniforms: HashMap<String, EffectValue>,
    /// Extra sampler textures by slot index (1..=7); slot 0 is the layer itself.
    pub textures: HashMap<u32, String>,
    /// "normal" | "additive" | "translucent"
    pub blending: String,
    pub combos: HashMap<String, i32>,
    /// Named FBO this pass renders into; None = the effect's output.
    pub target: Option<String>,
    /// (sampler index, buffer name) — "previous" is the incoming layer texture.
    pub binds: Vec<(u32, String)>,
}

/// All passes for one effect instance on a layer.
#[derive(Debug, Clone)]
pub struct LoadedEffect {
    pub file: String,
    pub name: String,
    pub passes: Vec<EffectPass>,
    /// Intermediate buffers: (name, downscale factor).
    pub fbos: Vec<(String, f32)>,
}

/// Parse `// [COMBO] {"combo":"BLENDMODE","default":9,…}` (and `[COMBO_OFF]`)
/// lines. WE always predefines every declared combo; omitting the material
/// override falls back to the annotation default — not zero. Zero is wrong for
/// image-blending combos (BLENDMODE==0 replaces the layer with rays only →
/// black screen with only particle systems visible).
fn combo_annotation_defaults(src: &str) -> HashMap<String, i32> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let t = line.trim_start();
        let rest = if let Some(r) = t.strip_prefix("// [COMBO]") {
            r
        } else if let Some(r) = t.strip_prefix("//[COMBO]") {
            r
        } else if let Some(r) = t.strip_prefix("// [COMBO_OFF]") {
            r
        } else if let Some(r) = t.strip_prefix("//[COMBO_OFF]") {
            r
        } else {
            continue;
        };
        let json = rest.trim();
        let Ok(Value::Object(meta)) = serde_json::from_str::<Value>(json) else {
            continue;
        };
        let Some(name) = meta.get("combo").and_then(|v| v.as_str()) else {
            continue;
        };
        let dflt = meta
            .get("default")
            .and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_f64().map(|f| f as i64))
                    .or_else(|| v.as_u64().map(|u| u as i64))
            })
            .unwrap_or(0);
        out.insert(name.to_uppercase(), dflt as i32);
    }
    out
}

/// Sampler annotations: `uniform sampler2D g_TextureN; // {...,"combo":"MASK"}`
/// → (slot, combo name). WE turns the combo on when that slot is actually
/// bound; leaving it off makes masked effects apply to the whole layer.
fn sampler_combos(src: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for line in src.lines() {
        let t = line.trim();
        if !t.starts_with("uniform sampler2D") {
            continue;
        }
        let Some(cpos) = t.find("//") else { continue };
        let Ok(Value::Object(meta)) = serde_json::from_str::<Value>(t[cpos + 2..].trim()) else {
            continue;
        };
        let Some(combo) = meta.get("combo").and_then(|v| v.as_str()) else {
            continue;
        };
        // g_TextureN → slot N
        let Some(decl_end) = t.find(';') else { continue };
        let name = t[..decl_end].split_whitespace().last().unwrap_or("");
        let Some(idx) = name.strip_prefix("g_Texture").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        out.push((idx, combo.to_uppercase()));
    }
    out
}

/// Parse `uniform <type> g_Name; // {"material":"key","default":...}` lines,
/// returning material-key → (uniform name, default value).
fn uniform_annotations(src: &str) -> HashMap<String, (String, Option<EffectValue>)> {
    let mut out = HashMap::new();
    for line in src.lines() {
        let t = line.trim();
        if !t.starts_with("uniform ") {
            continue;
        }
        let Some(decl_end) = t.find(';') else { continue };
        let decl = &t[..decl_end];
        // uniform <type> <name>  (drop array suffix)
        let name = decl
            .split_whitespace()
            .last()
            .map(|n| n.split('[').next().unwrap_or(n).to_string());
        let Some(uname) = name else { continue };
        let Some(cpos) = t.find("//") else { continue };
        let json_part = t[cpos + 2..].trim();
        let Ok(Value::Object(meta)) = serde_json::from_str::<Value>(json_part) else {
            continue;
        };
        let Some(key) = meta.get("material").and_then(|v| v.as_str()) else {
            continue;
        };
        let dflt = meta.get("default").map(json_to_effect_value);
        out.insert(key.to_string(), (uname, dflt));
    }
    out
}

fn json_to_effect_value(v: &Value) -> EffectValue {
    // Share the same rules as scene parse (incl. scripted constants).
    crate::scene::model::parse_effect_value(v)
}

/// Load every pass of an effect instance attached to a layer.
pub fn load_effect(assets: &AssetResolver, inst: &WeEffectInstance) -> Option<LoadedEffect> {
    let doc = assets.read_json(&inst.file).ok()?;
    let pass_defs = doc.get("passes")?.as_array()?.clone();
    let mut passes = Vec::new();

    for (i, pd) in pass_defs.iter().enumerate() {
        let mat_path = pd.get("material").and_then(|v| v.as_str()).unwrap_or("");
        if mat_path.is_empty() {
            return None;
        }
        let Ok(mat) = assets.read_json(mat_path) else {
            log::warn!("effect {}: material {mat_path} missing", inst.file);
            return None;
        };
        let Some(mpass) = mat.get("passes").and_then(|p| p.as_array()).and_then(|a| a.first())
        else {
            return None;
        };
        let shader = mpass.get("shader").and_then(|v| v.as_str()).unwrap_or("");
        if shader.is_empty() {
            return None;
        }
        let target = pd
            .get("target")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let binds: Vec<(u32, String)> = pd
            .get("bind")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|b| {
                        let name = b.get("name")?.as_str()?.to_string();
                        let idx = b.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                        Some((idx, name))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let blending = mpass
            .get("blending")
            .and_then(|v| v.as_str())
            .unwrap_or("normal")
            .to_string();

        // Combos: material defaults + the scene.json instance overrides.
        let mut combos: HashMap<String, i32> = HashMap::new();
        if let Some(Value::Object(c)) = mpass.get("combos") {
            for (k, v) in c {
                if let Some(n) = v.as_i64() {
                    combos.insert(k.to_uppercase(), n as i32);
                }
            }
        }
        if let Some(inst_pass) = inst.passes.get(i) {
            for (k, v) in &inst_pass.combos {
                combos.insert(k.to_uppercase(), *v);
            }
        }

        let (Ok(vsrc), Ok(fsrc)) = (
            assets.load_shader(shader, "vert"),
            assets.load_shader(shader, "frag"),
        ) else {
            log::warn!("effect {}: shader {shader} missing", inst.file);
            return None;
        };

        // Shader [COMBO] annotation defaults (lowest priority after "undefined → 0").
        // Material / instance combos already in `combos` win over these.
        for (k, v) in combo_annotation_defaults(&vsrc)
            .into_iter()
            .chain(combo_annotation_defaults(&fsrc))
        {
            combos.entry(k).or_insert(v);
        }

        // Material-key → uniform mapping comes from both stages' annotations.
        let mut ann = uniform_annotations(&vsrc);
        ann.extend(uniform_annotations(&fsrc));

        // Start from annotation defaults, then material constants, then the
        // scene.json instance's constantshadervalues (highest priority).
        let mut uniforms: HashMap<String, EffectValue> = HashMap::new();
        for (uname, dflt) in ann.values() {
            if let Some(d) = dflt {
                uniforms.insert(uname.clone(), d.clone());
            }
        }
        let apply = |key: &str, val: EffectValue, uniforms: &mut HashMap<String, EffectValue>| {
            if let Some((uname, _)) = ann.get(key) {
                uniforms.insert(uname.clone(), val);
            }
        };
        if let Some(Value::Object(c)) = mpass.get("constantshadervalues") {
            for (k, v) in c {
                apply(k, json_to_effect_value(v), &mut uniforms);
            }
        }
        if let Some(inst_pass) = inst.passes.get(i) {
            for (k, v) in &inst_pass.constants {
                apply(k, v.clone(), &mut uniforms);
            }
        }

        // Textures: annotation defaults (e.g. particle/halo_6) → material →
        // instance overrides (slot 0 = layer albedo).
        let mut textures: HashMap<u32, String> = HashMap::new();
        for line in vsrc.lines().chain(fsrc.lines()) {
            let t = line.trim();
            if !t.starts_with("uniform sampler2D") {
                continue;
            }
            let Some(decl_end) = t.find(';') else { continue };
            let name = t[..decl_end].split_whitespace().last().unwrap_or("");
            let Some(idx) = name
                .strip_prefix("g_Texture")
                .and_then(|n| n.parse::<u32>().ok())
            else {
                continue;
            };
            if idx == 0 {
                continue;
            }
            let Some(cpos) = t.find("//") else { continue };
            let Ok(Value::Object(meta)) = serde_json::from_str::<Value>(t[cpos + 2..].trim())
            else {
                continue;
            };
            if let Some(def) = meta.get("default").and_then(|v| v.as_str()) {
                if !def.is_empty() {
                    textures.insert(idx, def.to_string());
                }
            }
        }
        if let Some(arr) = mpass.get("textures").and_then(|v| v.as_array()) {
            for (slot, t) in arr.iter().enumerate() {
                if let Some(s) = t.as_str() {
                    if !s.is_empty() && slot > 0 {
                        textures.insert(slot as u32, s.to_string());
                    }
                }
            }
        }
        if let Some(inst_pass) = inst.passes.get(i) {
            for (slot, t) in inst_pass.textures.iter().enumerate() {
                if let Some(s) = t {
                    if !s.is_empty() && slot > 0 {
                        textures.insert(slot as u32, s.clone());
                    }
                }
            }
        }

        // Enable combos whose sampler slot is actually bound (WE does this when
        // you attach a mask). Without it, e.g. waterwaves' MASK block is
        // compiled out and the distortion covers the entire layer.
        let mut combos = combos;
        for (slot, combo) in sampler_combos(&vsrc)
            .into_iter()
            .chain(sampler_combos(&fsrc))
        {
            combos.insert(combo, i32::from(slot == 0 || textures.contains_key(&slot) || binds.iter().any(|(idx, _)| *idx == slot)));
        }

        let mut vert = preprocess_we_glsl(&vsrc, ShaderStage::Vertex, shader, assets, &combos);
        let mut frag = preprocess_we_glsl(&fsrc, ShaderStage::Fragment, shader, assets, &combos);
        // Workshop rotate2d etc.: vert `out vec2 v_TexCoord` vs frag `in vec3`.
        reconcile_stage_varyings(&mut vert, &mut frag);

        passes.push(EffectPass {
            name: format!("{}#{i}", inst.name),
            vert,
            frag,
            uniforms,
            textures,
            blending,
            combos,
            target,
            binds,
        });
    }

    if passes.is_empty() {
        return None;
    }
    let fbos: Vec<(String, f32)> = doc
        .get("fbos")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|f| {
                    let name = f.get("name")?.as_str()?.to_string();
                    let scale = f.get("scale").and_then(|s| s.as_f64()).unwrap_or(1.0) as f32;
                    Some((name, scale.max(1.0)))
                })
                .collect()
        })
        .unwrap_or_default();

    Some(LoadedEffect {
        file: inst.file.clone(),
        name: inst.name.clone(),
        passes,
        fbos,
    })
}

/// True for stock albedo shaders that the engine already draws as a plain
/// textured quad — no custom material pass needed.
pub fn is_generic_image_shader(shader: &str) -> bool {
    let s = shader.trim().to_ascii_lowercase();
    s.is_empty()
        || s.starts_with("genericimage")
        || s == "generic"
        || s.starts_with("genericparticle")
}

/// Load a layer's own material pass as a drawable effect.
///
/// Classic WE water-flow templates (and other authoring kits) put the motion
/// shader on the material itself (`"shader": "flowimage"`) with multi-texture
/// binds, rather than attaching an `effects/…` instance. Without this the
/// layer loads as a still and looks broken.
pub fn load_material_as_effect(
    assets: &AssetResolver,
    material: &str,
) -> Option<LoadedEffect> {
    let mat = assets.read_json(material).ok()?;
    let mpass = mat
        .get("passes")
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())?;
    let shader = mpass.get("shader").and_then(|v| v.as_str()).unwrap_or("");
    if is_generic_image_shader(shader) {
        return None;
    }

    // Inline: effect docs say `passes[].material`; material docs are the pass
    // itself. Build the pass directly from `mpass`.
    let (Ok(vsrc), Ok(fsrc)) = (
        assets.load_shader(shader, "vert"),
        assets.load_shader(shader, "frag"),
    ) else {
        log::warn!("material {material}: shader {shader} missing");
        return None;
    };

    let mut combos: HashMap<String, i32> = HashMap::new();
    if let Some(Value::Object(c)) = mpass.get("combos") {
        for (k, v) in c {
            if let Some(n) = v.as_i64() {
                combos.insert(k.to_uppercase(), n as i32);
            }
        }
    }
    for (k, v) in combo_annotation_defaults(&vsrc)
        .into_iter()
        .chain(combo_annotation_defaults(&fsrc))
    {
        combos.entry(k).or_insert(v);
    }

    let mut ann = uniform_annotations(&vsrc);
    ann.extend(uniform_annotations(&fsrc));
    let mut uniforms: HashMap<String, EffectValue> = HashMap::new();
    for (uname, dflt) in ann.values() {
        if let Some(d) = dflt {
            uniforms.insert(uname.clone(), d.clone());
        }
    }
    if let Some(Value::Object(c)) = mpass.get("constantshadervalues") {
        for (k, v) in c {
            if let Some((uname, _)) = ann.get(k.as_str()) {
                uniforms.insert(uname.clone(), json_to_effect_value(v));
            } else {
                // Material keys sometimes match uniform names directly
                // (g_FlowSpeed) or use PascalCase labels (Speed → g_FlowSpeed
                // via annotation only). Fall through when no annotation.
                let _ = (k, v);
            }
        }
    }

    let mut textures: HashMap<u32, String> = HashMap::new();
    if let Some(arr) = mpass.get("textures").and_then(|v| v.as_array()) {
        for (slot, t) in arr.iter().enumerate() {
            if let Some(s) = t.as_str() {
                if !s.is_empty() && slot > 0 {
                    textures.insert(slot as u32, s.to_string());
                }
            }
        }
    }
    for (slot, combo) in sampler_combos(&vsrc)
        .into_iter()
        .chain(sampler_combos(&fsrc))
    {
        combos.insert(combo, i32::from(slot == 0 || textures.contains_key(&slot)));
    }

    let mut vert = preprocess_we_glsl(&vsrc, ShaderStage::Vertex, shader, assets, &combos);
    let mut frag = preprocess_we_glsl(&fsrc, ShaderStage::Fragment, shader, assets, &combos);
    reconcile_stage_varyings(&mut vert, &mut frag);

    let blending = mpass
        .get("blending")
        .and_then(|v| v.as_str())
        .unwrap_or("normal")
        .to_string();

    log::info!(
        "material base shader «{shader}» on {material}: uniforms={} tex_slots={:?}",
        uniforms.len(),
        textures.keys().collect::<Vec<_>>()
    );

    Some(LoadedEffect {
        file: material.to_string(),
        name: format!("material:{shader}"),
        passes: vec![EffectPass {
            name: format!("material:{shader}"),
            vert,
            frag,
            uniforms,
            textures,
            blending,
            combos,
            target: None,
            binds: Vec::new(),
        }],
        fbos: Vec::new(),
    })
}

/// Material looks like a dual-texture flow / water-distort kit (classic WE
/// water-flow template, package-local `flowimage`, etc.).
pub fn material_is_flow_kit(assets: &AssetResolver, material: &str) -> Option<FlowMaterialInfo> {
    let mat = assets.read_json(material).ok()?;
    let mpass = mat
        .get("passes")
        .and_then(|p| p.as_array())
        .and_then(|a| a.first())?;
    let shader = mpass
        .get("shader")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let textures: Vec<Option<String>> = mpass
        .get("textures")
        .and_then(|t| t.as_array())
        .map(|a| {
            a.iter()
                .map(|t| t.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();
    let mask = textures.get(1).and_then(|t| t.clone())?;
    let shader_flow = shader.contains("flow") || shader.contains("water");
    let mask_flow = {
        let m = mask.to_ascii_lowercase();
        m.contains("flow") || m.contains("mask") || m.contains("distort")
    };
    if !(shader_flow || mask_flow) {
        return None;
    }
    let constants = mpass.get("constantshadervalues");
    let speed = constants
        .and_then(|c| c.get("Speed").or_else(|| c.get("speed")))
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0) as f32;
    let amount = constants
        .and_then(|c| {
            c.get("Amount")
                .or_else(|| c.get("amount"))
                .or_else(|| c.get("strength"))
        })
        .and_then(|v| v.as_f64())
        .unwrap_or(1.0) as f32;
    Some(FlowMaterialInfo {
        mask_tex: mask,
        speed,
        amount,
        shader,
    })
}

/// Info extracted from a package-local flow material.
#[derive(Debug, Clone)]
pub struct FlowMaterialInfo {
    pub mask_tex: String,
    pub speed: f32,
    pub amount: f32,
    pub shader: String,
}

#[cfg(test)]
mod fidelity_tests {
    use super::*;
    #[test]
    fn sampler_presence_overrides_zero_combo_defaults_in_effects_and_materials() {
        let root = std::env::temp_dir().join(format!("walld-mask-combo-{}", std::process::id()));
        std::fs::create_dir_all(root.join("shaders")).unwrap();
        std::fs::write(root.join("shaders/test.vert"), "attribute vec3 a_Position;\nvoid main(){gl_Position=vec4(a_Position,1.0);}").unwrap();
        std::fs::write(root.join("shaders/test.frag"), r#"
// [COMBO] {"combo":"MASK","default":0}
uniform sampler2D g_Texture1; // {"combo":"MASK"}
void main(){gl_FragColor=vec4(1.0);}
"#).unwrap();
        std::fs::write(root.join("material.json"), r#"{"passes":[{"shader":"test","combos":{"MASK":0},"textures":[null,"mask"]}]}"#).unwrap();
        std::fs::write(root.join("effect.json"), r#"{"passes":[{"material":"material.json"}]}"#).unwrap();
        let assets = AssetResolver { package_root:root.clone(), we_assets:root.clone(), workshop:root.clone(), extra:vec![] };
        let instance = WeEffectInstance { file:"effect.json".into(), name:"test".into(), visible:true, passes:vec![] };
        assert_eq!(load_effect(&assets,&instance).unwrap().passes[0].combos["MASK"],1);
        assert_eq!(load_material_as_effect(&assets,"material.json").unwrap().passes[0].combos["MASK"],1);
        // An unbound sampler must not inherit a stale material combo either.
        std::fs::write(root.join("material.json"), r#"{"passes":[{"shader":"test","combos":{"MASK":1}}]}"#).unwrap();
        assert_eq!(load_effect(&assets,&instance).unwrap().passes[0].combos["MASK"],0);
        assert_eq!(load_material_as_effect(&assets,"material.json").unwrap().passes[0].combos["MASK"],0);
        // Named FBO binds count as real sampler inputs.
        std::fs::write(root.join("effect.json"), r#"{"passes":[{"material":"material.json","bind":[{"index":1,"name":"previous"}]}]}"#).unwrap();
        assert_eq!(load_effect(&assets,&instance).unwrap().passes[0].combos["MASK"],1);
        std::fs::remove_dir_all(root).unwrap();
    }
}
