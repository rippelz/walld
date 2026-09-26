//! Shared effect execution for the daemon and headless fidelity checks.
use crate::render::Renderer;
use glow::HasContext;
use std::collections::{HashMap, HashSet};
use wallengine_we::scene::{effectpass::LoadedEffect, GraphNode};

#[derive(Clone, Copy, Debug)]
pub struct EffectTexture {
    pub tex: glow::Texture,
    pub w: u32,
    pub h: u32,
    pub uv_scale: (f32, f32),
}

#[derive(Clone, Copy)]
struct Target {
    fbo: glow::Framebuffer,
    image: EffectTexture,
}

#[derive(Default)]
pub struct EffectTargets {
    pool: HashMap<(u32, u32), Vec<Target>>,
    warned: HashSet<String>,
}

pub struct EffectOutput {
    pub image: EffectTexture,
    /// Only complete effects are recorded, so failed shaders retain fallbacks.
    pub applied: Vec<String>,
}

impl EffectTargets {
    fn acquire(
        &mut self,
        renderer: &Renderer,
        w: u32,
        h: u32,
        protected: &[glow::Texture],
    ) -> Option<Target> {
        let targets = self.pool.entry((w, h)).or_default();
        if let Some(t) = targets.iter().find(|t| !protected.contains(&t.image.tex)) {
            return Some(*t);
        }
        let (fbo, tex) = renderer.create_target(w, h)?;
        let t = Target {
            fbo,
            image: EffectTexture {
                tex,
                w,
                h,
                uv_scale: (1.0, 1.0),
            },
        };
        targets.push(t);
        Some(t)
    }

    pub fn clear(&mut self, renderer: &Renderer) {
        self.warned.clear();
        for targets in self.pool.drain().map(|(_, v)| v) {
            for t in targets {
                renderer.delete_target(t.fbo, t.image.tex);
            }
        }
    }

    /// `previous` is frozen at the entrance to EACH effect. The pool excludes
    /// every sampled texture and that frozen input from output allocation.
    /// Two unconditional ping-pong buffers cannot satisfy this when an effect
    /// has several passes and later reuses its original input.
    #[allow(clippy::too_many_arguments)]
    pub fn run(
        &mut self,
        renderer: &Renderer,
        input: EffectTexture,
        effects: &[LoadedEffect],
        programs: &[Option<glow::Program>],
        graph: &[GraphNode],
        time: f32,
        pointer: [f32; 2],
        resolve_texture: impl Fn(&str) -> Option<EffectTexture>,
    ) -> EffectOutput {
        // WE runs effects on a rendered layer, not its power-of-two TEX
        // storage. Remove storage padding once, before normalized effect UVs
        // are compared with separately padded mask UVs.
        let mut src = input;
        if input.uv_scale != (1.0, 1.0) {
            let w = (input.w as f32 * input.uv_scale.0).round().max(1.0) as u32;
            let h = (input.h as f32 * input.uv_scale.1).round().max(1.0) as u32;
            if let Some(t) = self.acquire(renderer, w, h, &[input.tex]) {
                if renderer
                    .copy_texture_content(input.tex, t.fbo, w, h)
                    .is_ok()
                {
                    src = t.image;
                }
            }
        }
        let layer_size = (src.w, src.h);
        let mut applied = Vec::new();
        let mut pi = 0;
        for effect in effects {
            let start = pi;
            pi += effect.passes.len();
            // A partial multipass effect may sample unwritten intermediate
            // buffers. Skip it atomically, preserving the preceding result.
            let Some(progs) = programs.get(start..pi) else {
                continue;
            };
            if progs.iter().any(Option::is_none) {
                continue;
            }
            let previous = src;
            let mut buffers = HashMap::new();
            let mut protected = vec![input.tex, previous.tex];
            let mut valid = true;
            for (name, scale) in &effect.fbos {
                let w = ((layer_size.0 as f32 / scale).round() as u32).max(1);
                let h = ((layer_size.1 as f32 / scale).round() as u32).max(1);
                if let Some(target) = self.acquire(renderer, w, h, &protected) {
                    protected.push(target.image.tex);
                    buffers.insert(name.clone(), target);
                } else {
                    valid = false;
                    break;
                }
            }
            if !valid {
                continue;
            }
            // Validate the complete plan before drawing anything. A missing
            // declared buffer must never silently become the layer output.
            for (p, prog) in effect.passes.iter().zip(progs) {
                if p.target.as_ref().is_some_and(|n| !buffers.contains_key(n))
                    || p.binds
                        .iter()
                        .any(|(_, n)| n != "previous" && !buffers.contains_key(n))
                    || p.textures.iter().any(|(slot, n)| {
                        !p.binds.iter().any(|(idx, _)| idx == slot)
                            && !buffers.contains_key(n)
                            && resolve_texture(n).is_none()
                            && unsafe {
                                renderer
                                    .gl
                                    .get_uniform_location(
                                        prog.unwrap(),
                                        &format!("g_Texture{slot}"),
                                    )
                                    .is_some()
                            }
                    })
                {
                    valid = false;
                    break;
                }
            }
            if !valid {
                if self.warned.insert(effect.file.clone()) {
                    log::warn!(
                        "effect {} skipped: unresolved texture or render target",
                        effect.file
                    );
                }
                continue;
            }
            for (p, prog) in effect.passes.iter().zip(progs) {
                let mut samplers = HashMap::from([(0, src)]);
                for (slot, name) in &p.textures {
                    if let Some(t) = buffers
                        .get(name)
                        .map(|t| t.image)
                        .or_else(|| resolve_texture(name))
                    {
                        samplers.insert(*slot, t);
                    }
                }
                // Explicit buffer binds outrank material/instance textures.
                for (slot, name) in &p.binds {
                    let t = if name == "previous" {
                        previous
                    } else {
                        buffers[name].image
                    };
                    samplers.insert(*slot, t);
                }
                let mut avoid = protected.clone();
                avoid.extend(samplers.values().map(|t| t.tex));
                let named = p.target.as_ref().map(|n| buffers[n]);
                let target = if let Some(t) =
                    named.filter(|t| !samplers.values().any(|s| s.tex == t.image.tex))
                {
                    Some(t)
                } else {
                    let (w, h) = named.map(|t| (t.image.w, t.image.h)).unwrap_or(layer_size);
                    self.acquire(renderer, w, h, &avoid)
                };
                let Some(target) = target else {
                    valid = false;
                    break;
                };
                let source = samplers[&0];
                let extra: Vec<_> = samplers
                    .iter()
                    .filter(|(slot, _)| **slot != 0)
                    .map(|(slot, t)| (*slot, t.tex, t.w, t.h, t.uv_scale))
                    .collect();
                let uniforms: Vec<_> = p
                    .uniforms
                    .iter()
                    .map(|(name, v)| (name.clone(), v.resolve_for_draw(graph, time, name)))
                    .collect();
                renderer.run_effect_pass(
                    prog.unwrap(),
                    target.fbo,
                    target.image.w,
                    target.image.h,
                    source.tex,
                    source.w,
                    source.h,
                    source.uv_scale,
                    &extra,
                    &uniforms,
                    time,
                    pointer,
                );
                if let Some(name) = &p.target {
                    // Even read/write-to-the-same-name passes get distinct GL
                    // attachments; the new version becomes visible afterwards.
                    buffers.insert(name.clone(), target);
                    protected.push(target.image.tex);
                } else {
                    src = target.image;
                }
            }
            if valid {
                applied.push(effect.file.clone());
            } else {
                src = previous;
            }
        }
        EffectOutput {
            image: src,
            applied,
        }
    }
}
