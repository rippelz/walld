//! WeScene runtime: loads a WE package into drawable image layers + particles + effects.

use super::model::*;
use super::parse::{
    load_project_properties, parse_particle_doc, parse_scene_file_with_props,
};
use super::particles::WeParticleSystem;
use super::puppet::PuppetMesh;
use crate::assets::{AssetResolver, TextureCache};
use crate::pkg::ensure_unpacked;
use crate::tex::DecodedTex;
use crate::transform::origin_to_camera;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

impl ImageDraw {
    /// Keep animation frame windows in the same content-relative position
    /// after the effect executor removes TEX storage padding.
    pub fn rebase_texture_uv(&mut self, from: (f32, f32), to: (f32, f32)) {
        for (i, ratio) in [to.0 / from.0.max(f32::EPSILON), to.1 / from.1.max(f32::EPSILON)].into_iter().enumerate() {
            self.uv_scale[i] *= ratio;
            self.uv_offset[i] *= ratio;
        }
    }

    /// Dedicated shaders are fallbacks for effects absent from the successful
    /// generic chain, never a second application of the same operation.
    pub fn suppress_applied_effects(&mut self, applied: &[String]) {
        if self.waterflow.as_ref().is_some_and(|e| applied.contains(&e.file)) {
            self.has_waterflow = false;
        }
        if self.opacity.as_ref().is_some_and(|e| applied.contains(&e.file)) {
            self.has_opacity = false;
        }
        if self.colorkey_file.as_ref().is_some_and(|f| applied.contains(f)) {
            self.colorkey = None;
        }
    }
}

/// One effect applied on an image layer (waterflow first).
#[derive(Debug, Clone)]
pub struct SceneEffect {
    /// Source effect file, used to avoid applying a GPU effect again as a fallback.
    pub file: String,
    pub kind: EffectKind,
    pub strength: f32,
    pub speed: f32,
    pub phasescale: f32,
    pub feather: f32,
    /// Flow mask texture name (materials/… or masks/…)
    pub mask_tex: Option<String>,
    /// Phase texture name
    pub phase_tex: Option<String>,
    pub colorkey: Option<ColorkeyParams>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectKind {
    Waterflow,
    /// Opacity mask: albedo.a *= mask.r (WE effects/opacity)
    Opacity,
    /// Chroma key (WE effects/colorkey): keyed color becomes transparent.
    Colorkey,
    /// Placeholder for unhandled effects — layer still draws base image
    Unknown,
}

/// WE effects/colorkey parameters (official shader formula).
#[derive(Debug, Clone, Copy)]
pub struct ColorkeyParams {
    pub color: [f32; 3],
    pub alpha: f32,
    pub fuzziness: f32,
    pub tolerance: f32,
}

/// CPU-side image layer ready for GPU upload + ortho draw.
#[derive(Debug)]
pub struct ImageLayer {
    pub name: String,
    /// **Camera space** origin (center, y-up) — already converted from authored scene space.
    pub origin: [f32; 3],
    pub size: [f32; 2],
    pub scale: [f32; 3],
    /// Authored Y-up angles in radians.
    pub angles: [f32; 3],
    pub alignment: String,
    pub texture_name: String,
    pub rgba: Option<DecodedTex>,
    pub effects: Vec<SceneEffect>,
    /// Mask / phase textures decoded for waterflow
    pub mask_rgba: Option<DecodedTex>,
    pub phase_rgba: Option<DecodedTex>,
    /// Puppet mesh (body-part atlas). When set, draw triangles instead of a unit quad.
    pub puppet: Option<PuppetMesh>,
    /// Model `cropoffset` (fraction of size) — shifts puppet mesh pivot.
    pub crop_offset: [f32; 2],
    /// Named attachment on the **parent** puppet this layer is glued to.
    pub attachment: Option<String>,
    /// Generic WE effect passes (shake/waterwaves/ripple/…) in author order,
    /// run as fragment passes over the layer's rendered texture.
    pub effect_passes: Vec<crate::scene::effectpass::LoadedEffect>,
    pub solidlayer: bool,
    /// util/composelayer: capture what's already drawn in this layer's screen
    /// rect, run effect_passes, draw the result (TV censor / pixelate bars).
    pub composelayer: bool,
    /// WE per-object blend against the backdrop (common_blending.h table; 0 = normal).
    pub color_blend_mode: i32,
    /// Static per-object opacity from scene.json (0 hides the layer at load).
    pub alpha: f32,
    /// Timeline animations played by `tick` (alpha c0; origin c0/c1 screen px).
    pub alpha_anim: Option<crate::scene::timeline::Timeline>,
    pub origin_anim: Option<crate::scene::timeline::Timeline>,
    /// Loaded camera-space origin and the origin timeline's frame-0 sample,
    /// so animated origins move as deltas from their authored position.
    pub origin_base: [f32; 3],
    pub origin_anim_base: [f32; 2],
    /// engine.timeOfDay clock-hand binding from an angles script (native
    /// fallback used only when the SceneScript host is unavailable).
    pub angle_time: Option<crate::scene::timeline::AngleTimeBinding>,
    /// Scene graph node backing this layer.
    pub node_id: i64,
    /// False for layers whose load-time geometry was normalized (clamped
    /// solid/fullscreen layers) — the graph must not overwrite them.
    pub graph_driven: bool,
    /// Effective visibility after scripts (chain-ANDed each tick).
    pub visible: bool,
    /// Scene object order (for interleaved particle draw).
    pub scene_order: u32,
    /// Tint applied at draw (dynamic bars / scripted color). None = white.
    pub script_color: Option<[f32; 3]>,
}

/// A WE text layer (clock/date/…): restrung + rerasterized by `tick`.
pub struct TextLayerRuntime {
    pub name: String,
    pub node_id: i64,
    /// True when the text property is script-driven; the SceneScript host
    /// supplies the string each tick (native templates are the fallback).
    pub scripted: bool,
    pub visible: bool,
    /// Center in camera space.
    pub origin: [f32; 2],
    pub kind: crate::scene::text::TextKind,
    pub pointsize: f32,
    pub color: [f32; 3],
    pub alpha: f32,
    pub scene_order: u32,
    pub font: ab_glyph::FontVec,
    pub current: String,
    pub rgba: Option<DecodedTex>,
    /// Bumped whenever `rgba` changes so renderers re-upload.
    pub generation: u64,
}

/// Draw request for a text layer.
#[derive(Debug, Clone)]
pub struct TextDraw {
    pub text_index: usize,
    pub origin: [f32; 2],
    pub size: [f32; 2],
    pub alpha: f32,
    pub generation: u64,
    pub scene_order: u32,
}

/// Draw request for the GPU (normalized by runtime after tick).
#[derive(Debug, Clone)]
pub struct ImageDraw {
    pub layer_index: usize,
    /// Center in **camera space** (y-up)
    pub origin: [f32; 2],
    pub size: [f32; 2],
    /// Authored Y-up Z rotation in radians
    pub angle_z: f32,
    /// UV window into the texture buffer (spritesheet frame × content rect).
    /// `uv = vUV * uv_scale + uv_offset`.
    pub uv_scale: [f32; 2],
    pub uv_offset: [f32; 2],
    pub has_waterflow: bool,
    pub waterflow: Option<SceneEffect>,
    pub has_opacity: bool,
    pub opacity: Option<SceneEffect>,
    pub colorkey: Option<ColorkeyParams>,
    pub colorkey_file: Option<String>,
    pub has_puppet: bool,
    pub color_blend_mode: i32,
    /// Static per-object opacity (multiply into the draw's opacity).
    pub alpha: f32,
    pub scene_order: u32,
    /// Capture framebuffer region + effect stack (util/composelayer).
    pub composelayer: bool,
}

#[derive(Debug, Clone)]
pub struct ParticleDraw {
    pub system_index: usize,
    pub scene_order: u32,
}

/// Unified scene draw item so images and particles can interleave in author order.
#[derive(Debug, Clone)]
pub enum SceneDrawItem {
    Image(ImageDraw),
    Particle(ParticleDraw),
    Text(TextDraw),
}

pub struct WeSceneRuntime {
    pub title: String,
    pub id: String,
    pub ortho_width: f32,
    pub ortho_height: f32,
    pub clear_color: [f32; 3],
    pub camera_center: [f32; 3],
    pub images: Vec<ImageLayer>,
    pub texts: Vec<TextLayerRuntime>,
    pub particles: Vec<WeParticleSystem>,
    /// Maps particle system index → scene object order (for interleaving).
    pub particle_orders: Vec<u32>,
    /// Maps particle system index → graph node id.
    pub particle_nodes: Vec<i64>,
    /// Effective per-system visibility after scripts.
    pub particle_visible: Vec<bool>,
    /// Authoring hierarchy with local transforms (mutated by scripts/timelines).
    pub graph: Vec<GraphNode>,
    graph_index: HashMap<i64, usize>,
    /// SceneScript executor (None when the scene has no scripts).
    script_host: Option<crate::scene::script::ScriptHost>,
    /// Current audio spectrum (0..1 bins) fed to audio-reactive scripts and
    /// effects; empty/zero while nothing is playing.
    pub audio_spectrum: Vec<f32>,
    /// Cursor position in wallpaper UV 0..1 (WE `g_PointerPosition` / `input.cursorPosition`).
    /// Updated each frame by the host from the compositor cursor.
    pub cursor_uv: [f32; 2],
    pub assets: AssetResolver,
    pub package_root: PathBuf,
    time: f32,
    /// Click-debug samples (viewport UV 0..1, y-down) from the debug overlay.
    pub debug_clicks: Vec<DebugClick>,
}

/// One user click in debug mode, in both viewport and ortho/screen spaces.
#[derive(Debug, Clone, Copy)]
pub struct DebugClick {
    /// Viewport pixel (top-left origin).
    pub view_x: f32,
    pub view_y: f32,
    pub view_w: f32,
    pub view_h: f32,
    /// Ortho screen space (0,0 top-left of canvas).
    pub ortho_x: f32,
    pub ortho_y: f32,
    /// Camera space (center, y-up).
    pub cam_x: f32,
    pub cam_y: f32,
}

impl WeSceneRuntime {
    pub fn load(wallpaper_dir: &Path, workshop_id: &str, title: &str) -> Result<Self, String> {
        // Steam can prune workshop folders that walld still references (or that
        // were never installed to begin with); the unpacked cache outlives them.
        let dir_buf;
        let wallpaper_dir = if !wallpaper_dir.join("scene.pkg").is_file()
            && !wallpaper_dir.join("scene.json").is_file()
        {
            let cached = crate::we_cache_dir().join(workshop_id);
            if cached.join("scene.pkg").is_file() || cached.join("scene.json").is_file() {
                dir_buf = cached;
                &dir_buf
            } else {
                wallpaper_dir
            }
        } else {
            wallpaper_dir
        };
        let package_root = if wallpaper_dir.join("scene.pkg").is_file() {
            ensure_unpacked(wallpaper_dir, workshop_id).map_err(|e| e.to_string())?
        } else {
            wallpaper_dir.to_path_buf()
        };
        let assets = AssetResolver::new(&package_root);
        let scene_path = package_root.join("scene.json");
        if !scene_path.is_file() {
            return Err("scene.json missing".into());
        }
        // Defaults from project.json + user overrides in ~/.config/walld/props/.
        let mut props = crate::props::load_merged_properties(wallpaper_dir, workshop_id);
        if props.is_empty() {
            props = crate::props::load_merged_properties(&package_root, workshop_id);
        }
        if props.is_empty() {
            props = load_project_properties(wallpaper_dir);
        }
        let doc = parse_scene_file_with_props(&scene_path, &assets, &props)?;
        Self::from_doc(doc, assets, package_root, workshop_id, title, &props)
    }

    fn from_doc(
        doc: WeSceneDoc,
        assets: AssetResolver,
        package_root: PathBuf,
        id: &str,
        title: &str,
        user_props: &HashMap<String, serde_json::Value>,
    ) -> Result<Self, String> {
        let mut cache = TextureCache::new();
        let mut images = Vec::new();
        let mut texts: Vec<TextLayerRuntime> = Vec::new();
        let mut particles = Vec::new();
        let mut particle_orders = Vec::new();
        let mut particle_nodes: Vec<i64> = Vec::new();
        let mut scene_order = 0u32;
        // In scripted scenes any layer can be shown later (cross-layer
        // getLayer().alpha writes), so alpha-0 layers must still load.
        let scene_scripted = doc.raw_objects.iter().any(|o| {
            [
                "origin", "angles", "scale", "alpha", "visible", "text", "color", "size",
            ]
            .iter()
            .any(|p| o.get(*p).map(|v| v.get("script").is_some()).unwrap_or(false))
        });

        for obj in &doc.objects {
            match &obj.kind {
                WeObjectKind::Image {
                    texture,
                    material,
                    puppet,
                    crop_offset,
                    solidlayer,
                    fullscreen,
                    composelayer,
                    ..
                } => {
                    let tex_name = texture.clone();
                    let mut rgba = if *solidlayer || *composelayer || tex_name.is_empty() {
                        None
                    } else {
                        cache
                            .get_or_load(&assets, &tex_name)
                            .ok()
                            .cloned()
                            .or_else(|| {
                                let stem = Path::new(material)
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_default();
                                cache.get_or_load(&assets, &stem).ok().cloned()
                            })
                    };

                    let mut size = obj.size;
                    if (size[0] <= 0.0 || size[1] <= 0.0) && rgba.is_some() {
                        let t = rgba.as_ref().unwrap();
                        size = [t.content_width as f32, t.content_height as f32];
                    }
                    if *fullscreen || (*solidlayer && (size[0] <= 0.0 || size[1] <= 0.0)) {
                        size = [doc.ortho_width, doc.ortho_height];
                    }
                    if size[0] <= 0.0 {
                        size = [doc.ortho_width, doc.ortho_height];
                    }

                    // Statically hidden layers can be skipped only when no
                    // script could ever reveal them.
                    if obj.alpha <= 0.004 && !scene_scripted {
                        log::debug!("skip layer «{}» (object alpha 0)", obj.name);
                        continue;
                    }

                    // solidlayer without albedo: WE fills it with the object color.
                    // Only fill when the author gave it a color and real geometry;
                    // colorless or zero-size solidlayers are UI/FBO/effect targets
                    // (media panels, day/night filters) and drawing them would
                    // obliterate the scene.
                    if *solidlayer && rgba.is_none() {
                        match obj.color {
                            Some([r, g, b])
                                if obj.effects.is_empty()
                                    && obj.size[0] > 0.0
                                    && obj.size[1] > 0.0 =>
                            {
                                rgba = Some(make_solid_tex(
                                    (r.clamp(0.0, 1.0) * 255.0) as u8,
                                    (g.clamp(0.0, 1.0) * 255.0) as u8,
                                    (b.clamp(0.0, 1.0) * 255.0) as u8,
                                    255,
                                ));
                            }
                            _ => {
                                log::debug!(
                                    "skip solidlayer «{}» (no color/geometry; needs FBO pipeline)",
                                    obj.name
                                );
                                continue;
                            }
                        }
                    }

                    let mut effects = map_effects(&obj.effects);

                    // Classic WE water-flow templates put the motion on the
                    // material itself (`shader: flowimage`, textures:
                    // [albedo, flowmask]) instead of attaching effects/waterflow.
                    // Promote that to a real Waterflow effect so the dedicated
                    // draw path runs with the right mask + speed/amount.
                    let mut material_flow = false;
                    if !*composelayer && !*solidlayer {
                        if let Some(flow) =
                            crate::scene::effectpass::material_is_flow_kit(&assets, material)
                        {
                            if !effects.iter().any(|e| e.kind == EffectKind::Waterflow) {
                                log::info!(
                                    "layer «{}»: material flow kit «{}» → waterflow (mask={}, speed={}, amount={})",
                                    obj.name,
                                    flow.shader,
                                    flow.mask_tex,
                                    flow.speed,
                                    flow.amount
                                );
                                effects.push(SceneEffect {
                                    file: material.clone(),
                                    kind: EffectKind::Waterflow,
                                    strength: flow.amount,
                                    speed: flow.speed,
                                    phasescale: 2.0,
                                    feather: 0.4,
                                    mask_tex: Some(flow.mask_tex),
                                    phase_tex: None,
                                    colorkey: None,
                                });
                                material_flow = true;
                            }
                        }
                    }

                    let mut mask_rgba = None;
                    let mut phase_rgba = None;
                    for ef in &effects {
                        if matches!(ef.kind, EffectKind::Waterflow | EffectKind::Opacity) {
                            if let Some(ref m) = ef.mask_tex {
                                mask_rgba = assets.load_tex(m).ok().or_else(|| {
                                    assets
                                        .load_tex(&format!(
                                            "masks/{}",
                                            Path::new(m).file_name()?.to_str()?
                                        ))
                                        .ok()
                                });
                            }
                        }
                        if ef.kind == EffectKind::Waterflow {
                            if let Some(ref p) = ef.phase_tex {
                                phase_rgba = assets.load_tex(p).ok().or_else(|| {
                                    assets.load_tex("effects/waterflowphase").ok()
                                });
                            }
                            if phase_rgba.is_none() {
                                phase_rgba = assets
                                    .load_tex("effects/waterflowphase")
                                    .ok()
                                    .or_else(|| Some(make_noise_phase(32)));
                            }
                        }
                    }

                    // Compose layers have no package texture — content is the
                    // framebuffer region captured at draw time.
                    if rgba.is_none() && !*composelayer {
                        log::warn!(
                            "image layer «{}» texture «{}» failed to decode",
                            obj.name,
                            tex_name
                        );
                        continue;
                    }

                    // Alignment shifts the pivot (LWE CImage::updateScenePosition).
                    let mut origin_screen = obj.origin;
                    // Clamp absurd solidlayer scales (e.g. 5×5 * 1000 used as UV hacks).
                    let mut scale = obj.scale;
                    if *solidlayer || *fullscreen {
                        let sw = size[0] * scale[0].abs();
                        let sh = size[1] * scale[1].abs();
                        if sw > doc.ortho_width * 4.0 || sh > doc.ortho_height * 4.0 {
                            size = [doc.ortho_width, doc.ortho_height];
                            scale = [1.0, 1.0, 1.0];
                        }
                    }
                    let scaled = [size[0] * scale[0].abs(), size[1] * scale[1].abs()];
                    apply_alignment(&mut origin_screen, &obj.alignment, scaled);
                    // cropoffset is UV/mesh space (not a world-space origin shift).
                    let _ = crop_offset;

                    let origin_cam =
                        origin_to_camera(origin_screen, doc.ortho_width, doc.ortho_height);

                    let puppet_mesh = puppet
                        .as_ref()
                        .and_then(|p| match PuppetMesh::load(&assets, p) {
                            Ok(mut m) => {
                                if let Some(raw) = doc.raw_objects.iter().find(|v| v.get("id").and_then(|v| v.as_i64()) == Some(obj.id)) {
                                    m.set_animation_layers(&raw["animationlayers"]);
                                }
                                Some(m)
                            },
                            Err(e) => {
                                log::warn!("puppet «{}»: {e}", obj.name);
                                None
                            }
                        });

                    // Bake per-object tint × brightness into the albedo once so
                    // every draw path (quad, puppet, waterflow, soft) gets it.
                    if !*solidlayer && !*composelayer {
                        let c = obj.color.unwrap_or([1.0, 1.0, 1.0]);
                        let tint = [
                            c[0] * obj.brightness,
                            c[1] * obj.brightness,
                            c[2] * obj.brightness,
                        ];
                        if tint.iter().any(|c| (c - 1.0).abs() > 0.004) {
                            if let Some(t) = rgba.as_mut() {
                                for px in t.rgba.chunks_exact_mut(4) {
                                    for ch in 0..3 {
                                        px[ch] = (px[ch] as f32 * tint[ch].clamp(0.0, 4.0))
                                            .min(255.0) as u8;
                                    }
                                }
                            }
                        }
                    }

                    // Keep full TV-censor stack (pixelate + VHS static + pulse).
                    // LWE runs VHS here; dropping it left only soft pixelation.
                    let mut effect_passes: Vec<_> = obj
                        .effects
                        .iter()
                        .filter(|e| e.visible)
                        .filter_map(|e| crate::scene::effectpass::load_effect(&assets, e))
                        .collect();
                    // Non-generic material shaders (custom package shaders that
                    // aren't a flow kit already promoted above) run as a base
                    // effect pass so the layer isn't a still.
                    if !*composelayer && !*solidlayer && !material_flow {
                        if let Some(base) =
                            crate::scene::effectpass::load_material_as_effect(&assets, material)
                        {
                            effect_passes.insert(0, base);
                        }
                    }
                    if *composelayer {
                        log::info!(
                            "compose layer «{}» size={:.0}×{:.0} effect_passes={} files={:?}",
                            obj.name,
                            size[0] * scale[0].abs(),
                            size[1] * scale[1].abs(),
                            effect_passes.len(),
                            effect_passes.iter().map(|e| e.file.as_str()).collect::<Vec<_>>()
                        );
                    }

                    images.push(ImageLayer {
                        name: obj.name.clone(),
                        origin: origin_cam,
                        size,
                        scale,
                        angles: obj.angles,
                        alignment: obj.alignment.clone(),
                        texture_name: tex_name,
                        rgba,
                        effects,
                        mask_rgba,
                        phase_rgba,
                        puppet: puppet_mesh,
                        crop_offset: *crop_offset,
                        attachment: obj.attachment.clone(),
                        effect_passes,
                        solidlayer: *solidlayer,
                        composelayer: *composelayer,
                        color_blend_mode: obj.color_blend_mode,
                        alpha: obj.alpha.clamp(0.0, 1.0),
                        alpha_anim: obj.alpha_anim.clone(),
                        origin_anim: obj.origin_anim.clone(),
                        origin_base: origin_cam,
                        origin_anim_base: obj
                            .origin_anim
                            .as_ref()
                            .map(|t| {
                                [
                                    t.sample(0, 0.0).unwrap_or(0.0),
                                    t.sample(1, 0.0).unwrap_or(0.0),
                                ]
                            })
                            .unwrap_or([0.0, 0.0]),
                        angle_time: obj.angle_time,
                        node_id: obj.id,
                        graph_driven: !(*solidlayer || *fullscreen || *composelayer),
                        visible: obj.visible,
                        scene_order,
                        script_color: None,
                    });
                    scene_order += 1;
                }
                WeObjectKind::Particle { path } => {
                    let ov = &obj.particle_override;
                    match WeParticleSystem::load(
                        &assets,
                        &obj.name,
                        path,
                        obj.origin,
                        obj.scale,
                        obj.angles[2],
                        doc.ortho_width,
                        doc.ortho_height,
                        ov,
                    ) {
                        Ok(sys) => {
                            log::info!(
                                "particle «{}» from {path} max={} rate*={} speed*={}",
                                obj.name,
                                sys.maxcount,
                                ov.rate,
                                ov.speed
                            );
                            particles.push(sys);
                            particle_orders.push(scene_order);
                            particle_nodes.push(obj.id);
                            scene_order += 1;
                        }
                        Err(e) => {
                            let alt = if path.starts_with("particles/") {
                                path.clone()
                            } else {
                                format!("particles/{path}")
                            };
                            match parse_particle_doc(&assets, &alt).map(|d| {
                                WeParticleSystem::from_doc(
                                    &obj.name,
                                    obj.origin,
                                    obj.scale,
                                    obj.angles[2],
                                    doc.ortho_width,
                                    doc.ortho_height,
                                    d,
                                    ov,
                                )
                            }) {
                                Ok(mut sys) => {
                                    sys.load_texture(&assets);
                                    particles.push(sys);
                                    particle_orders.push(scene_order);
                                    particle_nodes.push(obj.id);
                                    scene_order += 1;
                                }
                                Err(_) => log::warn!("particle «{}» {path}: {e}", obj.name),
                            }
                        }
                    }
                }
                WeObjectKind::Text {
                    font,
                    pointsize,
                    literal,
                    script,
                } => {
                    if obj.alpha <= 0.004 && !scene_scripted {
                        continue;
                    }
                    // Template classification is only the fallback for when the
                    // SceneScript host can't run; scripted text layers always
                    // load (the script supplies the string each tick).
                    let kind = match crate::scene::text::classify(
                        literal.as_deref(),
                        script.as_deref(),
                        &obj.name,
                    ) {
                        Some(k) => k,
                        None if script.is_some() => {
                            crate::scene::text::TextKind::Static(String::new())
                        }
                        None => continue,
                    };
                    // Font: exact path, then basename under fonts/, then a
                    // system fallback (covers WE built-ins like systemfont_arial).
                    let font_bytes = assets
                        .read_bytes(font)
                        .ok()
                        .or_else(|| {
                            let base = Path::new(font).file_name()?.to_str()?;
                            assets.read_bytes(&format!("fonts/{base}")).ok()
                        })
                        .or_else(|| {
                            [
                                "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
                                "/usr/share/fonts/TTF/DejaVuSans.ttf",
                                "/usr/share/fonts/dejavu/DejaVuSans.ttf",
                                "/usr/share/fonts/noto/NotoSans-Regular.ttf",
                            ]
                            .iter()
                            .find_map(|p| std::fs::read(p).ok())
                        });
                    let Some(bytes) = font_bytes else {
                        log::warn!("text «{}»: font {font} not found", obj.name);
                        continue;
                    };
                    let Ok(fv) = ab_glyph::FontVec::try_from_vec(bytes) else {
                        log::warn!("text «{}»: font {font} failed to parse", obj.name);
                        continue;
                    };
                    let origin_cam =
                        origin_to_camera(obj.origin, doc.ortho_width, doc.ortho_height);
                    texts.push(TextLayerRuntime {
                        name: obj.name.clone(),
                        node_id: obj.id,
                        scripted: script.is_some(),
                        visible: obj.visible,
                        origin: [origin_cam[0], origin_cam[1]],
                        kind,
                        pointsize: *pointsize,
                        color: obj.color.unwrap_or([1.0, 1.0, 1.0]),
                        alpha: obj.alpha.clamp(0.0, 1.0),
                        scene_order,
                        font: fv,
                        current: String::new(),
                        rgba: None,
                        generation: 0,
                    });
                    scene_order += 1;
                }
                WeObjectKind::Other => {}
            }
        }

        if images.is_empty() {
            // last-resort: any tex in materials
            if let Ok(tex) = find_largest_tex(&assets, &package_root) {
                images.push(ImageLayer {
                    name: "fallback".into(),
                    origin: origin_to_camera(
                        [doc.ortho_width * 0.5, doc.ortho_height * 0.5, 0.0],
                        doc.ortho_width,
                        doc.ortho_height,
                    ),
                    size: [tex.width as f32, tex.height as f32],
                    scale: [1.0, 1.0, 1.0],
                    angles: [0.0, 0.0, 0.0],
                    alignment: String::new(),
                    texture_name: "fallback".into(),
                    rgba: Some(tex),
                    effects: Vec::new(),
                    mask_rgba: None,
                    phase_rgba: None,
                    puppet: None,
                    crop_offset: [0.0, 0.0],
                    attachment: None,
                    effect_passes: Vec::new(),
                    solidlayer: false,
                    composelayer: false,
                    color_blend_mode: 0,
                    alpha: 1.0,
                    alpha_anim: None,
                    origin_anim: None,
                    origin_base: [0.0, 0.0, 0.0],
                    origin_anim_base: [0.0, 0.0],
                    angle_time: None,
                    node_id: -1,
                    graph_driven: false,
                    visible: true,
                    scene_order: 0,
                    script_color: None,
                });
            }
        }

        if images.is_empty() {
            return Err("no drawable image layers in scene".into());
        }

        // Apply puppet attachment sockets: child origin is relative to the
        // named point on the parent puppet, not the parent layer center.
        let mut graph = doc.graph;
        apply_puppet_attachments(&mut graph, &images);

        // Attached limb puppets need bone skinning (animationlayers / MDLA) to
        // pose correctly. Drawing the bind-pose mesh alone dismembers the
        // character (e.g. Spirit Blossom Ahri's floating arm). Until we skin
        // those meshes, hide attached puppet *children* — parent base puppets
        // usually already include a static hand/limb, and the bubble (non-
        // puppet) still follows the `orb` socket.
        for img in &mut images {
            if img.attachment.is_some() && img.puppet.is_some() {
                log::info!(
                    "puppet «{}»: attached limb deferred (needs bone anim) — hidden",
                    img.name
                );
                img.puppet = None;
                img.visible = false;
                if let Some(gi) = graph.iter().position(|n| n.id == img.node_id) {
                    graph[gi].visible = false;
                }
            }
        }

        log::info!(
            "WeScene «{title}»: ortho {}x{} images={} particles={} effects={}",
            doc.ortho_width,
            doc.ortho_height,
            images.len(),
            particles.len(),
            images.iter().map(|i| i.effects.len()).sum::<usize>(),
        );

        let script_host = crate::scene::script::ScriptHost::new(
            &graph,
            &doc.raw_objects,
            user_props,
            [doc.ortho_width, doc.ortho_height],
        );
        let graph_index: HashMap<i64, usize> = graph
            .iter()
            .enumerate()
            .map(|(i, n)| (n.id, i))
            .collect();
        let particle_visible: Vec<bool> = particle_nodes
            .iter()
            .map(|id| {
                graph
                    .iter()
                    .find(|n| n.id == *id)
                    .map(|n| n.visible)
                    .unwrap_or(true)
            })
            .collect();

        // Re-resolve image origins so attachment offsets take effect at load.
        let (ow, oh) = (doc.ortho_width, doc.ortho_height);
        let mut images = images;
        for img in &mut images {
            if img.node_id < 0 || !img.graph_driven {
                continue;
            }
            if let Some(w) = resolve_world(&graph, &graph_index, img.node_id) {
                let mut origin_screen = w.origin;
                let scaled = [
                    img.size[0] * w.scale[0].abs(),
                    img.size[1] * w.scale[1].abs(),
                ];
                apply_alignment(&mut origin_screen, &img.alignment, scaled);
                let cam = origin_to_camera(origin_screen, ow, oh);
                img.origin = cam;
                img.origin_base = cam;
                img.scale = w.scale;
                img.angles[2] = w.angle_z;
            }
        }

        Ok(Self {
            title: title.to_string(),
            id: id.to_string(),
            ortho_width: doc.ortho_width,
            ortho_height: doc.ortho_height,
            clear_color: doc.clear_color,
            camera_center: doc.camera_center,
            images,
            texts,
            particles,
            particle_orders,
            particle_nodes,
            particle_visible,
            graph,
            graph_index,
            script_host,
            audio_spectrum: Vec::new(),
            cursor_uv: [0.5, 0.5],
            assets,
            package_root,
            time: 0.0,
            debug_clicks: Vec::new(),
        })
    }

    /// Host-fed pointer in 0..1 wallpaper space (y-down, matching WE screen UV).
    pub fn set_cursor_uv(&mut self, u: f32, v: f32) {
        self.cursor_uv = [u.clamp(0.0, 1.0), v.clamp(0.0, 1.0)];
    }

    /// Materialize a layer created by `thisScene.createLayer(templatePath)`.
    /// Used by Jett-style audio visualizers (full-pixel / half-pixel bars).
    /// Only creates drawables for known bar templates — never invents assets.
    fn materialize_dynamic_layer(
        &mut self,
        st: &crate::scene::script::NodeScriptState,
        template: &str,
    ) {
        if self.graph_index.contains_key(&st.id) {
            return;
        }
        let path = template.replace('\\', "/");
        // Resolve model → material → texture via the package assets.
        let model_val = self
            .assets
            .read_json(&path)
            .or_else(|_| {
                // Scripts often pass `models/full-pixel.json` relative to the
                // workshop dependency folder; also try workshop/… prefixes.
                let stem = Path::new(&path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(&path);
                self.assets
                    .read_json(&format!("models/workshop/3007895627/{stem}"))
                    .or_else(|_| self.assets.read_json(&format!("models/{stem}")))
            });
        let Ok(model) = model_val else {
            log::warn!("createLayer «{template}»: model not found — bar skipped");
            return;
        };
        let mat_path = model
            .get("material")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let mut tex_name = String::new();
        let mut size = [1.0_f32, 2.0];
        if let Ok(mat) = self.assets.read_json(&mat_path) {
            if let Some(passes) = mat.get("passes").and_then(|v| v.as_array()) {
                if let Some(texs) = passes.first().and_then(|p| p.get("textures")) {
                    if let Some(t) = texs.as_array().and_then(|a| a.first()).and_then(|v| v.as_str())
                    {
                        tex_name = t.to_string();
                    }
                }
            }
        }
        // Model size from scene object templates if present.
        if let Some(sz) = model.get("size").and_then(|v| v.as_str()) {
            let parts: Vec<f32> = sz
                .split_whitespace()
                .filter_map(|s| s.parse().ok())
                .collect();
            if parts.len() >= 2 {
                size = [parts[0], parts[1]];
            }
        }
        // full-pixel / half-pixel models only declare material; size is on the
        // scene template objects (1×2).
        if size[0] <= 0.0 {
            size[0] = 1.0;
        }
        if size[1] <= 0.0 {
            size[1] = 2.0;
        }

        let mut cache = TextureCache::new();
        let mut rgba = if tex_name.is_empty() {
            solid_white_tex()
        } else {
            cache
                .get_or_load(&self.assets, &tex_name)
                .ok()
                .cloned()
                .unwrap_or_else(solid_white_tex)
        };
        // Bake script color into albedo (GPU draw has no separate tint).
        // Near-black parent colors (common on visualizer roots) would make
        // static min-height bars invisible — lift to white so LWE-parity
        // static bars remain visible.
        let tint = normalize_bar_color(st.color);
        tint_rgba(&mut rgba, tint);

        // half-pixel grows upward only → bottom-center pivot like WE.
        let alignment = if path.to_ascii_lowercase().contains("half") {
            "bottom".to_string()
        } else {
            "center".to_string()
        };

        let scene_order = self
            .images
            .iter()
            .map(|i| i.scene_order)
            .max()
            .unwrap_or(0)
            .saturating_add(1);

        self.graph.push(GraphNode {
            id: st.id,
            name: path.clone(),
            parent: None, // bars use absolute screen origin from the script
            local_origin: st.o,
            origin_base: st.o,
            local_scale: st.s,
            local_angles: [
                st.an[0].to_radians(),
                st.an[1].to_radians(),
                st.an[2].to_radians(),
            ],
            visible: st.v,
            alpha: st.a.clamp(0.0, 1.0),
            alpha_base: st.a.clamp(0.0, 1.0),
            alpha_anim: None,
            origin_anim: None,
            anim_rate: 1.0,
        });
        self.graph_index.insert(st.id, self.graph.len() - 1);

        let origin_cam = origin_to_camera(st.o, self.ortho_width, self.ortho_height);
        self.images.push(ImageLayer {
            name: format!("dyn:{path}"),
            origin: origin_cam,
            size,
            scale: st.s,
            angles: [
                st.an[0].to_radians(),
                st.an[1].to_radians(),
                st.an[2].to_radians(),
            ],
            alignment,
            texture_name: tex_name,
            rgba: Some(rgba),
            effects: Vec::new(),
            mask_rgba: None,
            phase_rgba: None,
            puppet: None,
            crop_offset: [0.0, 0.0],
            attachment: None,
            effect_passes: Vec::new(),
            solidlayer: false,
            composelayer: false,
            color_blend_mode: 0,
            alpha: st.a.clamp(0.0, 1.0),
            alpha_anim: None,
            origin_anim: None,
            origin_base: origin_cam,
            origin_anim_base: [0.0, 0.0],
            angle_time: None,
            node_id: st.id,
            graph_driven: true,
            visible: st.v,
            scene_order,
            script_color: Some(tint),
        });
        log::info!(
            "createLayer → dyn id={} template=«{path}» size={size:?} (audio bar)",
            st.id
        );
    }

    /// Push live video times into SceneScript handles (name, time, duration, playing).
    pub fn sync_script_video_times(&mut self, layers: &[(String, f32, f32, bool)]) {
        if let Some(host) = &mut self.script_host {
            host.sync_video_times(layers);
        }
    }

    /// Drain play/pause/seek commands scripts issued via getVideoTexture().
    pub fn drain_script_video_commands(
        &mut self,
    ) -> Vec<crate::scene::script::VideoScriptCommand> {
        self.script_host
            .as_mut()
            .map(|h| h.drain_video_commands())
            .unwrap_or_default()
    }

    pub fn tick(&mut self, dt: f32) {
        self.time += dt;
        // Cursor → camera for control-point operators (CP1 ≈ pointer).
        let cursor_cam = {
            let sx = self.cursor_uv[0] * self.ortho_width;
            let sy = self.cursor_uv[1] * self.ortho_height;
            let c = crate::transform::screen_to_camera(sx, sy, self.ortho_width, self.ortho_height);
            Some(c)
        };
        for p in &mut self.particles {
            p.set_cursor_cam(cursor_cam);
            p.tick(dt);
        }
        for img in &mut self.images {
            if let Some(mesh) = &mut img.puppet {
                let rate = self.graph_index.get(&img.node_id)
                    .map(|&i| self.graph[i].anim_rate).unwrap_or(1.0);
                mesh.tick(dt * rate);
            }
        }
        let t = self.time;
        let tod = crate::scene::text::time_of_day();

        // 1. Timeline animations → graph locals (rate set by scripts).
        // WE `"relative": true` means keyframes are **offsets** from the static
        // authored value. Absolute mode replaces. Applying absolute values to a
        // relative timeline (common on time-of-day scenery) flings parents by
        // thousands of pixels and dismembers every child.
        for node in &mut self.graph {
            let ts = t * node.anim_rate.max(0.0);
            if let Some(anim) = &node.alpha_anim {
                if let Some(a) = anim.sample(0, ts) {
                    node.alpha = if anim.relative {
                        (node.alpha_base + a).clamp(0.0, 1.0)
                    } else {
                        a.clamp(0.0, 1.0)
                    };
                }
            }
            if let Some(anim) = &node.origin_anim {
                let rel = anim.relative;
                let base = node.origin_base;
                if let Some(x) = anim.sample(0, ts) {
                    node.local_origin[0] = if rel { base[0] + x } else { x };
                }
                if let Some(y) = anim.sample(1, ts) {
                    node.local_origin[1] = if rel { base[1] + y } else { y };
                }
                if let Some(z) = anim.sample(2, ts) {
                    node.local_origin[2] = if rel { base[2] + z } else { z };
                }
            }
        }

        // 2. SceneScripts → graph locals + live text strings.
        //
        // Many WE SceneScripts (SharedNoise head sway, etc.) advance by a fixed
        // amount *per update call*, not `* engine.frametime`. Wallpaper Engine
        // typically fires ~60 updates/sec; if we only call once per 30 Hz draw
        // frame those scripts run at half speed. Sub-step so wall-clock rate
        // matches a 60 Hz host (capped so a hitch doesn't explode work).
        //
        // Dynamic layers from `thisScene.createLayer` (Jett-style audio bars)
        // are materialized into the graph + image list on first sighting so
        // scripts can drive their origin/scale/angles like LWE does.
        let mut script_texts: HashMap<i64, String> = HashMap::new();
        if let Some(host) = &mut self.script_host {
            // Match WE's ~60 script updates/sec so frame-based SharedNoise /
            // head-sway scripts (which ignore frametime) run at full speed.
            const SCRIPT_HZ: f32 = 60.0;
            let steps = ((dt * SCRIPT_HZ).round() as i32).clamp(1, 12) as u32;
            let sub_dt = dt / steps as f32;
            let mut last_states: Vec<crate::scene::script::NodeScriptState> = Vec::new();
            for step in 0..steps {
                let t_sub = t - dt + sub_dt * (step + 1) as f32;
                last_states =
                    host.tick(sub_dt, tod, t_sub, &self.audio_spectrum, self.cursor_uv);
            }
            for st in last_states {
                if !self.graph_index.contains_key(&st.id) {
                    // New dynamic layer from createLayer.
                    if let Some(ref tmpl) = st.template {
                        self.materialize_dynamic_layer(&st, tmpl);
                    } else {
                        continue;
                    }
                }
                let Some(&i) = self.graph_index.get(&st.id) else {
                    continue;
                };
                let n = &mut self.graph[i];
                n.local_origin = st.o;
                n.local_angles = [
                    st.an[0].to_radians(),
                    st.an[1].to_radians(),
                    st.an[2].to_radians(),
                ];
                n.local_scale = st.s;
                n.alpha = st.a.clamp(0.0, 1.0);
                n.visible = st.v;
                n.anim_rate = st.r;
                if let Some(txt) = st.t {
                    script_texts.insert(st.id, txt);
                }
                // Keep script_color in sync (normalized so near-black stays visible).
                if let Some(img) = self.images.iter_mut().find(|im| im.node_id == st.id) {
                    if st.color.is_some() || img.script_color.is_none() {
                        img.script_color = Some(normalize_bar_color(st.color));
                    }
                }
            }
        }

        // 3. Re-resolve world transforms into the drawables.
        let (ow, oh) = (self.ortho_width, self.ortho_height);
        for img in &mut self.images {
            // Fallback layer uses node_id=-1 and is not graph-driven.
            if !self.graph_index.contains_key(&img.node_id) {
                continue;
            }
            let Some(w) = resolve_world(&self.graph, &self.graph_index, img.node_id) else {
                continue;
            };
            img.visible = w.visible;
            img.alpha = w.alpha;
            if img.graph_driven {
                let mut origin_screen = w.origin;
                let scaled = [
                    img.size[0] * w.scale[0].abs(),
                    img.size[1] * w.scale[1].abs(),
                ];
                apply_alignment(&mut origin_screen, &img.alignment, scaled);
                img.origin = origin_to_camera(origin_screen, ow, oh);
                img.scale = w.scale;
                img.angles[2] = w.angle_z;
            }
            // Native clock-hand fallback when scripts can't run.
            if self.script_host.is_none() {
                if let Some(binding) = img.angle_time {
                    img.angles[2] = binding.angle_deg(tod).to_radians();
                }
            }
        }
        for (i, sys) in self.particles.iter_mut().enumerate() {
            let Some(&nid) = self.particle_nodes.get(i) else {
                continue;
            };
            let Some(w) = resolve_world(&self.graph, &self.graph_index, nid) else {
                continue;
            };
            if let Some(v) = self.particle_visible.get_mut(i) {
                *v = w.visible;
            }
            // Full world transform — scale/angle were stuck at load-time local
            // values, so parented systems and oversized scene scales never
            // grew the emission volume or sprite diameter.
            let cam = origin_to_camera(w.origin, ow, oh);
            sys.origin_cam = cam;
            sys.scale = w.scale;
            sys.angle_z = w.angle_z;
        }
        for txt in &mut self.texts {
            if let Some(w) = resolve_world(&self.graph, &self.graph_index, txt.node_id) {
                txt.visible = w.visible;
                txt.alpha = w.alpha;
                let cam = origin_to_camera(w.origin, ow, oh);
                txt.origin = [cam[0], cam[1]];
            }
            let s = if txt.scripted {
                script_texts
                    .get(&txt.node_id)
                    .cloned()
                    .unwrap_or_else(|| crate::scene::text::render_string(&txt.kind))
            } else {
                crate::scene::text::render_string(&txt.kind)
            };
            if s != txt.current {
                txt.current = s;
                txt.rgba = crate::scene::text::rasterize(
                    &txt.font,
                    &txt.current,
                    txt.pointsize,
                    txt.color,
                );
                txt.generation += 1;
            }
        }
    }

    pub fn time(&self) -> f32 {
        self.time
    }

    pub fn is_animated(&self) -> bool {
        // A SceneScript can change visibility/transform/text on any frame
        // (flipbook characters, cycling variants), so a scripted scene must
        // keep ticking — otherwise it freezes on the pre-script t=0 pose.
        // Effect passes (shake/waterripple/godrays/…) animate still artwork —
        // any loaded pass must keep the scene ticking.
        self.script_host.is_some()
            || !self.particles.is_empty()
            || !self.texts.is_empty()
            || self.images.iter().any(|i| {
                !i.effect_passes.is_empty()
                    || i.effects
                        .iter()
                        .any(|e| e.kind == EffectKind::Waterflow)
                    || i.puppet.is_some()
                    || i.rgba.as_ref().map(|t| !t.frames.is_empty()).unwrap_or(false)
                    || i.rgba.as_ref().and_then(|t| t.video_path.as_ref()).is_some()
                    || i.alpha_anim.is_some()
                    || i.origin_anim.is_some()
                    || i.angle_time.is_some()
            })
    }

    /// Layer indices whose albedo is an embedded video texture.
    pub fn video_layers(&self) -> impl Iterator<Item = (usize, &std::path::Path)> {
        self.images.iter().enumerate().filter_map(|(i, layer)| {
            layer
                .rgba
                .as_ref()
                .and_then(|t| t.video_path.as_deref())
                .map(|p| (i, p))
        })
    }

    pub fn image_draws(&self) -> Vec<ImageDraw> {
        self.images
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.visible)
            .map(|(i, layer)| {
                let w = layer.size[0] * layer.scale[0];
                let h = layer.size[1] * layer.scale[1];
                let wf = layer
                    .effects
                    .iter()
                    .find(|e| e.kind == EffectKind::Waterflow)
                    .cloned();
                let opacity = layer
                    .effects
                    .iter()
                    .find(|e| e.kind == EffectKind::Opacity)
                    .cloned();
                let colorkey = layer
                    .effects
                    .iter()
                    .find(|e| e.kind == EffectKind::Colorkey)
                    .and_then(|e| e.colorkey);
                let (uv_scale, uv_offset) = frame_uv(layer, self.time);
                ImageDraw {
                    layer_index: i,
                    origin: [layer.origin[0], layer.origin[1]],
                    size: [w, h],
                    angle_z: layer.angles[2],
                    uv_scale,
                    uv_offset,
                    has_waterflow: wf.is_some(),
                    waterflow: wf,
                    has_opacity: opacity.is_some(),
                    opacity,
                    colorkey,
                    colorkey_file: layer.effects.iter().find(|e| e.kind == EffectKind::Colorkey).map(|e| e.file.clone()),
                    has_puppet: layer.puppet.is_some(),
                    color_blend_mode: layer.color_blend_mode,
                    alpha: layer.alpha,
                    scene_order: layer.scene_order,
                    composelayer: layer.composelayer,
                }
            })
            .collect()
    }

    /// Images + particles + text in scene authoring order.
    pub fn scene_draw_list(&self) -> Vec<SceneDrawItem> {
        let mut items: Vec<(u32, SceneDrawItem)> = Vec::new();
        for d in self.image_draws() {
            items.push((d.scene_order, SceneDrawItem::Image(d)));
        }
        for (i, txt) in self.texts.iter().enumerate() {
            if !txt.visible {
                continue;
            }
            let Some(ref tex) = txt.rgba else { continue };
            items.push((
                txt.scene_order,
                SceneDrawItem::Text(TextDraw {
                    text_index: i,
                    origin: txt.origin,
                    size: [tex.width as f32, tex.height as f32],
                    alpha: txt.alpha,
                    generation: txt.generation,
                    scene_order: txt.scene_order,
                }),
            ));
        }
        for (i, _) in self.particles.iter().enumerate() {
            if !self.particle_visible.get(i).copied().unwrap_or(true) {
                continue;
            }
            let order = self.particle_orders.get(i).copied().unwrap_or(u32::MAX);
            items.push((
                order,
                SceneDrawItem::Particle(ParticleDraw {
                    system_index: i,
                    scene_order: order,
                }),
            ));
        }
        items.sort_by_key(|(o, _)| *o);
        items.into_iter().map(|(_, it)| it).collect()
    }

    /// Record a debug click. `view_*` are wallpaper surface pixels (top-left origin).
    pub fn push_debug_click(&mut self, view_x: f32, view_y: f32, view_w: f32, view_h: f32) {
        let (s, _ox, _oy) =
            crate::transform::cover_fit(self.ortho_width, self.ortho_height, view_w, view_h);
        // Invert cover mapping: screen = center + cam * s
        let cam_x = (view_x - view_w * 0.5) / s;
        let cam_y = (view_h * 0.5 - view_y) / s;
        let [ortho_x, ortho_y] =
            crate::transform::camera_to_screen(cam_x, cam_y, self.ortho_width, self.ortho_height);
        let click = DebugClick {
            view_x,
            view_y,
            view_w,
            view_h,
            ortho_x,
            ortho_y,
            cam_x,
            cam_y,
        };
        log::info!(
            "DEBUG CLICK view=({view_x:.1},{view_y:.1})/{view_w:.0}x{view_h:.0} \
             ortho=({ortho_x:.1},{ortho_y:.1}) cam=({cam_x:.1},{cam_y:.1}) \
             canvas={}x{} uv=({:.3},{:.3})",
            self.ortho_width,
            self.ortho_height,
            ortho_x / self.ortho_width.max(1.0),
            ortho_y / self.ortho_height.max(1.0),
        );
        // Also dump nearest image layer center for correlation
        let mut best: Option<(f32, &str, [f32; 2])> = None;
        for img in &self.images {
            let dx = img.origin[0] - cam_x;
            let dy = img.origin[1] - cam_y;
            let dist = (dx * dx + dy * dy).sqrt();
            if best.map(|(d, ..)| dist < d).unwrap_or(true) {
                best = Some((dist, img.name.as_str(), [img.origin[0], img.origin[1]]));
            }
        }
        if let Some((dist, name, origin)) = best {
            log::info!(
                "DEBUG nearest layer «{name}» cam=({:.1},{:.1}) dist={dist:.1}",
                origin[0],
                origin[1]
            );
        }
        self.debug_clicks.push(click);
        // Cap history
        if self.debug_clicks.len() > 32 {
            self.debug_clicks.remove(0);
        }
    }
}

/// UV window for this tick: content rect, or the current spritesheet frame
/// rect (TEXS animated textures) inside the buffer. `uv = vUV * scale + offset`.
fn frame_uv(layer: &ImageLayer, time: f32) -> ([f32; 2], [f32; 2]) {
    let Some(ref t) = layer.rgba else {
        return ([1.0, 1.0], [0.0, 0.0]);
    };
    let (cs0, cs1) = t.content_uv_scale();
    if t.frames.is_empty() {
        return ([cs0, cs1], [0.0, 0.0]);
    }
    // Advance frames by their display times (fallback 30 fps per frame).
    let default_dt = 1.0 / 30.0;
    let dt_at = |i: usize| t.frame_times.get(i).copied().filter(|d| *d > 0.0).unwrap_or(default_dt);
    let total: f32 = (0..t.frames.len()).map(dt_at).sum::<f32>().max(default_dt);
    let mut tt = time % total;
    let mut idx = t.frames.len() - 1;
    for i in 0..t.frames.len() {
        let d = dt_at(i);
        if tt < d {
            idx = i;
            break;
        }
        tt -= d;
    }
    let f = t.frames[idx];
    ([f[2], f[3]], [f[0], f[1]])
}

fn solid_white_tex() -> DecodedTex {
    DecodedTex {
        width: 1,
        height: 1,
        content_width: 1,
        content_height: 1,
        texture_width: 1,
        texture_height: 1,
        format: crate::tex::TexFormat::Argb8888,
        flags: 0,
        free_image: None,
        rgba: vec![255, 255, 255, 255],
        frames: Vec::new(),
        frame_times: Vec::new(),
        video_path: None,
    }
}

fn normalize_bar_color(c: Option<[f32; 3]>) -> [f32; 3] {
    let c = c.unwrap_or([1.0, 1.0, 1.0]);
    // Visualizer roots often ship `color: 0 0 0`; black bars vanish on light
    // backgrounds. Fall back to a bright accent (similar to WE scheme blue).
    if c[0] + c[1] + c[2] < 0.05 {
        [0.35, 0.65, 1.0]
    } else {
        [
            c[0].clamp(0.0, 1.0),
            c[1].clamp(0.0, 1.0),
            c[2].clamp(0.0, 1.0),
        ]
    }
}

fn tint_rgba(tex: &mut DecodedTex, tint: [f32; 3]) {
    if (tint[0] - 1.0).abs() < 0.004 && (tint[1] - 1.0).abs() < 0.004 && (tint[2] - 1.0).abs() < 0.004
    {
        return;
    }
    for px in tex.rgba.chunks_exact_mut(4) {
        for ch in 0..3 {
            px[ch] = (px[ch] as f32 * tint[ch]).min(255.0) as u8;
        }
    }
}

/// Bake parent-puppet attachment sockets into graph local origins.
///
/// Scene objects with `"attachment": "orb"` store origin relative to that
/// socket. Without this, children parent to the layer center and float off
/// (Ahri arm / bubble).
fn apply_puppet_attachments(graph: &mut [GraphNode], images: &[ImageLayer]) {
    let mut parent_atts: HashMap<i64, &HashMap<String, [f32; 2]>> = HashMap::new();
    for img in images {
        if let Some(p) = img.puppet.as_ref() {
            if !p.attachments.is_empty() {
                parent_atts.insert(img.node_id, &p.attachments);
            }
        }
    }
    if parent_atts.is_empty() {
        return;
    }
    for img in images {
        let Some(ref att_name) = img.attachment else {
            continue;
        };
        let Some(gi) = graph.iter().position(|n| n.id == img.node_id) else {
            continue;
        };
        let Some(pid) = graph[gi].parent else {
            continue;
        };
        let Some(atts) = parent_atts.get(&pid) else {
            log::debug!(
                "attachment «{att_name}» on «{}»: parent {pid} has no puppet sockets",
                img.name
            );
            continue;
        };
        let offset = atts.get(att_name.as_str()).copied().or_else(|| {
            atts.iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(att_name))
                .map(|(_, p)| *p)
        });
        let Some([mx, my]) = offset else {
            log::warn!(
                "attachment «{att_name}» on «{}»: socket missing (have {:?})",
                img.name,
                atts.keys().collect::<Vec<_>>()
            );
            continue;
        };
        // Mesh sockets and graph local origins are both Y-up.
        graph[gi].local_origin[0] += mx;
        graph[gi].local_origin[1] += my;
        graph[gi].origin_base = graph[gi].local_origin;
        log::info!(
            "puppet attach «{att_name}» → «{}» +({mx:.1},{:.1}) screen",
            img.name,
            my
        );
    }
}

/// Shift origin based on WE alignment (pivot is center by default).
/// World transform of a graph node (composed exactly like WE's parent chain).
struct WorldTransform {
    origin: [f32; 3],
    scale: [f32; 3],
    angle_z: f32,
    visible: bool,
    alpha: f32,
}

fn resolve_world(
    graph: &[GraphNode],
    index: &HashMap<i64, usize>,
    id: i64,
) -> Option<WorldTransform> {
    let mut chain: Vec<usize> = Vec::new();
    let mut cur = Some(id);
    while let Some(cid) = cur {
        let Some(&i) = index.get(&cid) else { break };
        if chain.contains(&i) || chain.len() > 64 {
            break; // cycle guard
        }
        chain.push(i);
        cur = graph[i].parent;
    }
    if chain.is_empty() {
        return None;
    }
    let mut w = WorldTransform {
        origin: [0.0; 3],
        scale: [1.0; 3],
        angle_z: 0.0,
        visible: true,
        alpha: 1.0,
    };
    for &i in chain.iter().rev() {
        let n = &graph[i];
        let ox = n.local_origin[0] * w.scale[0];
        let oy = n.local_origin[1] * w.scale[1];
        let c = w.angle_z.cos();
        let s = w.angle_z.sin();
        w.origin = [
            w.origin[0] + ox * c - oy * s,
            w.origin[1] + ox * s + oy * c,
            w.origin[2] + n.local_origin[2] * w.scale[2],
        ];
        w.scale = [
            n.local_scale[0] * w.scale[0],
            n.local_scale[1] * w.scale[1],
            n.local_scale[2] * w.scale[2],
        ];
        w.angle_z += n.local_angles[2];
        w.visible &= n.visible;
    }
    w.alpha = graph[chain[0]].alpha;
    Some(w)
}

fn apply_alignment(origin: &mut [f32; 3], alignment: &str, scaled_size: [f32; 2]) {
    let a = alignment.to_ascii_lowercase();
    if a.is_empty() || a == "center" || a == "centre" {
        return;
    }
    // Alignment offsets are expressed in authored scene coordinates.
    if a.contains("top") {
        origin[1] -= scaled_size[1] * 0.5;
    } else if a.contains("bottom") {
        origin[1] += scaled_size[1] * 0.5;
    }
    if a.contains("left") {
        origin[0] -= scaled_size[0] * 0.5;
    } else if a.contains("right") {
        origin[0] += scaled_size[0] * 0.5;
    }
}

fn make_solid_tex(r: u8, g: u8, b: u8, a: u8) -> DecodedTex {
    DecodedTex {
        width: 1,
        height: 1,
        content_width: 1,
        content_height: 1,
        texture_width: 1,
        texture_height: 1,
        format: crate::tex::TexFormat::Argb8888,
        flags: 0,
        free_image: None,
        rgba: vec![r, g, b, a],
        frames: Vec::new(),
        frame_times: Vec::new(),
        video_path: None,
    }
}

fn map_effects(effects: &[WeEffectInstance]) -> Vec<SceneEffect> {
    let mut out = Vec::new();
    for ef in effects {
        let file = ef.file.to_ascii_lowercase();
        let kind = if file.contains("waterflow") {
            EffectKind::Waterflow
        } else if file.contains("opacity") {
            EffectKind::Opacity
        } else if file.contains("colorkey") {
            EffectKind::Colorkey
        } else {
            EffectKind::Unknown
        };
        let pass = ef.passes.first();
        let constants = pass.map(|p| &p.constants);
        let colorkey = if kind == EffectKind::Colorkey {
            let color = constants
                .and_then(|c| c.get("color"))
                .and_then(|v| v.as_vec3())
                .unwrap_or([0.0, 1.0, 1.0]);
            Some(ColorkeyParams {
                color,
                alpha: constants
                    .and_then(|c| c.get("alpha"))
                    .and_then(|v| v.as_f32())
                    .unwrap_or(0.0),
                fuzziness: constants
                    .and_then(|c| c.get("fuzziness"))
                    .and_then(|v| v.as_f32())
                    .unwrap_or(0.0),
                tolerance: constants
                    .and_then(|c| c.get("tolerance"))
                    .and_then(|v| v.as_f32())
                    .unwrap_or(0.1),
            })
        } else {
            None
        };
        let strength = constants
            .and_then(|c| c.get("strength").or_else(|| c.get("alpha")))
            .and_then(|v| v.as_f32())
            .unwrap_or(1.0);
        let speed = constants
            .and_then(|c| c.get("speed"))
            .and_then(|v| v.as_f32())
            .unwrap_or(1.0);
        let phasescale = constants
            .and_then(|c| c.get("phasescale"))
            .and_then(|v| v.as_f32())
            .unwrap_or(2.0);
        let feather = constants
            .and_then(|c| c.get("feather"))
            .and_then(|v| v.as_f32())
            .unwrap_or(0.4);

        let mut mask_tex = None;
        let mut phase_tex = None;
        if let Some(p) = pass {
            // textures: [null, mask, phase]
            if let Some(Some(t)) = p.textures.get(1) {
                mask_tex = Some(t.clone());
            }
            if let Some(Some(t)) = p.textures.get(2) {
                phase_tex = Some(t.clone());
            }
        }

        out.push(SceneEffect {
            file: ef.file.clone(),
            kind,
            strength,
            speed,
            phasescale,
            feather,
            mask_tex,
            phase_tex,
            colorkey,
        });
    }
    out
}

fn make_noise_phase(n: u32) -> DecodedTex {
    let mut rgba = vec![0u8; (n * n * 4) as usize];
    let mut rng = 0x1234_5678u64;
    for i in 0..(n * n) as usize {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let v = (rng & 0xFF) as u8;
        rgba[i * 4] = v;
        rgba[i * 4 + 1] = v;
        rgba[i * 4 + 2] = v;
        rgba[i * 4 + 3] = 255;
    }
    DecodedTex {
        width: n,
        height: n,
        content_width: n,
        content_height: n,
        texture_width: n,
        texture_height: n,
        format: crate::tex::TexFormat::Argb8888,
        flags: 0,
        free_image: None,
        rgba,
        frames: Vec::new(),
        frame_times: Vec::new(),
        video_path: None,
    }
}

fn find_largest_tex(assets: &AssetResolver, root: &Path) -> Result<DecodedTex, String> {
    let mut best: Option<(u64, DecodedTex)> = None;
    fn walk(dir: &Path, best: &mut Option<(u64, DecodedTex)>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for ent in rd.flatten() {
            let p = ent.path();
            if p.is_dir() {
                walk(&p, best);
            } else if p.extension().and_then(|e| e.to_str()) == Some("tex") {
                if let Ok(data) = std::fs::read(&p) {
                    if let Ok(t) = crate::tex::decode_tex(&data) {
                        let score = t.width as u64 * t.height as u64;
                        if best.as_ref().map(|b| score > b.0).unwrap_or(true) {
                            *best = Some((score, t));
                        }
                    }
                }
            }
        }
    }
    walk(&root.join("materials"), &mut best);
    walk(root, &mut best);
    if let Some((_, t)) = best {
        return Ok(t);
    }
    // Still nothing? Workshop packs always ship preview.jpg — better than a
    // hard fail when every .tex is an unsupported container (or empty pack).
    for name in ["preview.jpg", "preview.png", "preview.gif"] {
        let p = root.join(name);
        if !p.is_file() {
            continue;
        }
        if let Ok(data) = std::fs::read(&p) {
            if let Ok(img) = image::load_from_memory(&data) {
                let rgba = img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let _ = assets;
                return Ok(DecodedTex {
                    width: w,
                    height: h,
                    content_width: w,
                    content_height: h,
                    texture_width: w,
                    texture_height: h,
                    format: crate::tex::TexFormat::Argb8888,
                    flags: 0,
                    free_image: None,
                    rgba: rgba.into_raw(),
                    frames: Vec::new(),
                    frame_times: Vec::new(),
                    video_path: None,
                });
            }
        }
    }
    let _ = assets;
    Err("no tex".into())
}
