//! WE particle emitters / initializers / operators / renderers (CPU).
//!
//! Particle-local and authored scene positions are Y-up. Convert only the
//! final projected position/rotation to top-left viewport coordinates.

use super::model::*;
use super::parse::parse_particle_doc;
use crate::assets::AssetResolver;
use crate::transform::{origin_to_camera, screen_to_camera};
use std::time::Instant;

#[derive(Clone, Debug)]
pub struct WeParticle {
    /// Local-space position (y-up, relative to system origin before scale/rot).
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub size: f32,
    /// Authored size before sizechange (for startvalue→endvalue lerp).
    pub base_size: f32,
    pub alpha: f32,
    /// Authored alpha before alphafade (includes instance override).
    pub base_alpha: f32,
    pub color: [f32; 3],
    pub base_color: [f32; 3],
    pub life: f32,
    pub max_life: f32,
    pub age: f32,
    /// Sprite Z rotation (radians), from rotationrandom + angularmovement.
    pub rotation: f32,
    pub angular_vel: f32,
    /// oscillateposition: per-axis frequency / scale / phase (WE semantics).
    pub osc_freq: [f32; 3],
    pub osc_scale: [f32; 3],
    pub osc_phase: [f32; 3],
    /// sizechange: fractions of lifetime (0..1), not absolute seconds.
    pub size_start: f32,
    pub size_end: f32,
    pub size_t0: f32,
    pub size_t1: f32,
    /// colorchange multipliers (LWE multiplies initial color by fade(start→end)).
    pub color_start_mul: [f32; 3],
    pub color_end_mul: [f32; 3],
    pub color_t0: f32,
    pub color_t1: f32,
    pub has_color_change: bool,
    /// Random desync for sequence mode; unused for randomframe.
    pub frame_phase: f32,
    /// Locked TEXS frame index when animation_mode == RandomFrame.
    pub locked_frame: Option<usize>,
    pub alive: bool,
}

#[derive(Debug)]
pub struct WeParticleSystem {
    pub name: String,
    /// Camera-space origin of the particle object (center, y-up).
    pub origin_cam: [f32; 3],
    pub scale: [f32; 3],
    /// Screen-space angle Z (radians); applied as -angle when transforming.
    pub angle_z: f32,
    pub maxcount: u32,
    pub particles: Vec<WeParticle>,
    /// Multiplier from scene `instanceoverride.alpha` (applied at draw).
    pub alpha_mul: f32,
    /// "additive" | "translucent" from material.
    pub blending: String,
    /// Sprite texture name from the material (empty → procedural dot).
    pub texture_name: String,
    /// Decoded sprite, loaded at scene load.
    pub texture: Option<crate::tex::DecodedTex>,
    /// Multiplier for particle RGB (ember overbright).
    pub overbright: f32,
    animation_mode: ParticleAnimationMode,
    sequence_multiplier: f32,
    renderer: ParticleRendererDoc,
    emitters: Vec<EmitterDoc>,
    initializers: Vec<InitializerDoc>,
    operators: Vec<OperatorDoc>,
    /// Instance overrides (rate/speed/size/count/…).
    ov_rate: f32,
    ov_speed: f32,
    ov_size: f32,
    ov_count: f32,
    ov_lifetime: f32,
    ov_colorn: [f32; 3],
    emit_acc: f32,
    rng: u64,
    start_delay: f32,
    elapsed: f32,
    /// Host cursor in camera space (for controlpointattract / CP1 ≈ pointer).
    cursor_cam: Option<[f32; 2]>,
}

impl WeParticleSystem {
    /// `origin_screen` is world screen-space origin (after parent resolve).
    pub fn from_doc(
        name: &str,
        origin_screen: [f32; 3],
        scale: [f32; 3],
        angle_z: f32,
        ortho_w: f32,
        ortho_h: f32,
        doc: ParticleDoc,
        ov: &super::model::ParticleInstanceOverride,
    ) -> Self {
        let origin_cam = origin_to_camera(origin_screen, ortho_w, ortho_h);
        let maxcount = ((doc.maxcount as f32) * ov.count.max(0.01)).clamp(1.0, 5000.0) as u32;
        let mut sys = Self {
            name: name.to_string(),
            origin_cam,
            scale,
            angle_z,
            maxcount,
            particles: Vec::with_capacity(maxcount as usize),
            alpha_mul: ov.alpha.clamp(0.0, 4.0),
            blending: doc.blending,
            texture_name: doc.texture,
            texture: None,
            overbright: doc.overbright.max(0.0),
            animation_mode: doc.animation_mode,
            sequence_multiplier: doc.sequence_multiplier.max(0.01),
            renderer: doc.renderer,
            emitters: doc.emitters,
            initializers: doc.initializers,
            operators: doc.operators,
            ov_rate: ov.rate.max(0.0),
            ov_speed: ov.speed.max(0.0),
            ov_size: ov.size.max(0.01),
            ov_count: ov.count.max(0.01),
            ov_lifetime: ov.lifetime.max(0.01),
            ov_colorn: ov.colorn,
            emit_acc: 0.0,
            rng: Instant::now().elapsed().as_nanos() as u64 ^ 0xA076_1D64_78BD_642F,
            start_delay: doc.starttime.max(0.0),
            elapsed: 0.0,
            cursor_cam: None,
        };
        // WE `starttime` pre-runs the system so snow/smoke already fill the scene.
        let pretick = doc.starttime.clamp(0.0, 60.0);
        if pretick > 0.0 {
            let dt = 1.0 / 30.0;
            let steps = ((pretick / dt) as u32).min(30 * 45);
            for _ in 0..steps {
                sys.tick(dt);
            }
        } else {
            let warm = (maxcount as f32 * 0.35) as usize;
            for _ in 0..warm {
                if let Some(mut p) = sys.create_particle() {
                    let age = sys.next_f() * p.max_life * 0.85;
                    p.age = age;
                    p.life = (p.max_life - age).max(0.01);
                    p.pos[0] += p.vel[0] * age * 0.5;
                    p.pos[1] += p.vel[1] * age * 0.5;
                    sys.particles.push(p);
                }
            }
        }
        sys
    }

    /// Host cursor in camera space (y-up, centered). Used as control point 1.
    pub fn set_cursor_cam(&mut self, cam: Option<[f32; 2]>) {
        self.cursor_cam = cam;
    }

    pub fn load(
        assets: &AssetResolver,
        name: &str,
        path: &str,
        origin_screen: [f32; 3],
        scale: [f32; 3],
        angle_z: f32,
        ortho_w: f32,
        ortho_h: f32,
        ov: &super::model::ParticleInstanceOverride,
    ) -> Result<Self, String> {
        let doc = parse_particle_doc(assets, path)?;
        let mut sys = Self::from_doc(
            name,
            origin_screen,
            scale,
            angle_z,
            ortho_w,
            ortho_h,
            doc,
            ov,
        );
        sys.load_texture(assets);
        Ok(sys)
    }

    /// Expand live particles into textured-quad vertices in normalized
    /// viewport space: x y u v alpha r g b, 6 vertices per particle.
    ///
    /// Matches LWE model matrix `T * R(angle) * S` and WE genericparticle:
    /// `size` is what LWE stores after `sizerandom` (`authored * ov / 2`);
    /// shader expands with `size * (uv - 0.5)` so screen half-extent =
    /// `size * 0.5 * scale * fit`.
    pub fn sprite_vertices(
        &self,
        ortho_w: f32,
        ortho_h: f32,
        vw: f32,
        vh: f32,
    ) -> Vec<f32> {
        let (fit_s, _, _) = crate::transform::cover_fit(ortho_w, ortho_h, vw, vh);
        let sx = self.scale[0].abs().max(0.01);
        let sy = self.scale[1].abs().max(0.01);
        // Overbright once in the fragment shader (LWE: a_Color raw, g_Overbright).
        let frames = self
            .texture
            .as_ref()
            .map(|t| t.frames.as_slice())
            .unwrap_or(&[]);
        let trail = self.renderer.name.contains("trail");
        let mut out = Vec::new();
        for p in self.alive() {
            let cam = self.local_to_camera(p.pos);
            let c = crate::transform::camera_to_viewport_uv(
                cam[0], cam[1], ortho_w, ortho_h, vw, vh,
            );
            // WE shader: offset = size * (uv-0.5) → half-extent = size/2, then model scale.
            let mut half_w = (p.size * 0.5 * sx * fit_s).max(0.5);
            let mut half_h = (p.size * 0.5 * sy * fit_s).max(0.5);
            // Sprite rotation: particle spin + system angle. Spritetrail aligns
            // to velocity (WE: texture points "up" along travel).
            let mut rot = -(p.rotation + self.angle_z);
            if trail {
                let vcam = self.local_vel_to_camera(p.vel);
                let speed = (vcam[0] * vcam[0] + vcam[1] * vcam[1]).sqrt();
                if speed > 1e-3 {
                    // atan2: camera y-up → screen-ish orientation.
                    rot = std::f32::consts::FRAC_PI_2 - vcam[1].atan2(vcam[0]);
                    // Stretch along motion by length * speed, clamped.
                    let stretch = (speed * self.renderer.length)
                        .clamp(self.renderer.minlength, self.renderer.maxlength.max(0.01));
                    // `length` is dimensionless in many systems (~0.003); when
                    // stretch is tiny, keep a minimum 1× size aspect.
                    let base = half_h.max(half_w);
                    let along = (base * stretch.max(1.0)).max(base);
                    half_h = along;
                    half_w = base;
                }
            }
            let hx = half_w / vw.max(1.0);
            let hy = half_h / vh.max(1.0);
            let (ca, sa) = (rot.cos(), rot.sin());
            let corner = |dx: f32, dy: f32| -> (f32, f32) {
                (
                    c[0] + (dx * hx) * ca - (dy * hy) * sa,
                    c[1] + (dx * hx) * sa + (dy * hy) * ca,
                )
            };
            let (r, g, b) = (
                p.color[0].min(8.0),
                p.color[1].min(8.0),
                p.color[2].min(8.0),
            );
            let a = p.alpha.clamp(0.0, 1.0);
            let (u0, v0, du, dv) = self.particle_uv(p, frames);
            let quad = [
                ((-1.0, -1.0), (0.0, 0.0)),
                ((1.0, -1.0), (1.0, 0.0)),
                ((1.0, 1.0), (1.0, 1.0)),
                ((-1.0, -1.0), (0.0, 0.0)),
                ((1.0, 1.0), (1.0, 1.0)),
                ((-1.0, 1.0), (0.0, 1.0)),
            ];
            for ((dx, dy), (u, v)) in quad {
                let (x, y) = corner(dx, dy);
                out.extend_from_slice(&[x, y, u0 + u * du, v0 + v * dv, a, r, g, b]);
            }
        }
        out
    }

    fn particle_uv(&self, p: &WeParticle, frames: &[[f32; 4]]) -> (f32, f32, f32, f32) {
        if frames.is_empty() {
            return (0.0, 0.0, 1.0, 1.0);
        }
        let idx = match self.animation_mode {
            ParticleAnimationMode::RandomFrame => p
                .locked_frame
                .unwrap_or(0)
                .min(frames.len().saturating_sub(1)),
            ParticleAnimationMode::Sequence => {
                // WE: full sequence is stretched across the particle lifetime,
                // then sped up by sequence_multiplier (how many loops per life).
                let life = p.max_life.max(1e-4);
                let prog = (p.age / life) * self.sequence_multiplier + p.frame_phase;
                let f = prog.rem_euclid(1.0) * frames.len() as f32;
                (f.floor() as usize).min(frames.len() - 1)
            }
        };
        let f = frames[idx];
        (f[0], f[1], f[2], f[3])
    }

    fn local_vel_to_camera(&self, local: [f32; 3]) -> [f32; 2] {
        let sx = self.scale[0];
        let sy = self.scale[1];
        let lx = local[0] * sx;
        let ly = local[1] * sy;
        let ang = self.angle_z;
        let c = ang.cos();
        let s = ang.sin();
        [lx * c - ly * s, lx * s + ly * c]
    }

    /// True when the material blends additively (snow/embers/glow).
    pub fn is_additive(&self) -> bool {
        !self.blending.contains("translucent") && !self.blending.contains("normal")
    }

    /// Decode the sprite texture named by the material (best-effort).
    pub fn load_texture(&mut self, assets: &AssetResolver) {
        if self.texture_name.is_empty() {
            return;
        }
        let name = self.texture_name.clone();
        self.texture = assets.load_tex(&name).ok().or_else(|| {
            let base = std::path::Path::new(&name).file_name()?.to_str()?;
            assets
                .load_tex(&format!("particle/{base}"))
                .ok()
                .or_else(|| assets.load_tex(&format!("materials/{name}")).ok())
        });
        // genericparticle `ConvertTexture0Format`: RG88 → (R,R,R,G), R8 → (1,1,1,R).
        // LWE sets TEX0FORMAT combo; we expand at load so the simple psprite
        // shader matches WE (smoke RG88 soft-alpha, etc.).
        if let Some(tex) = self.texture.as_mut() {
            expand_particle_texture_format(tex);
        }
        if self.texture.is_none() {
            log::debug!("particle «{}»: texture {name} not found", self.name);
        }
    }

    fn next_f(&mut self) -> f32 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let u = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33;
        (u as f32) / (u32::MAX as f32)
    }

    fn next_range(&mut self, min: f32, max: f32) -> f32 {
        min + (max - min) * self.next_f()
    }

    fn create_particle(&mut self) -> Option<WeParticle> {
        let emitter = self.emitters.first().cloned().unwrap_or(EmitterDoc {
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

        // Emitter offsets in particle-local y-up (matches walld camera y-up).
        // Snowflat origin (0, 650, 0) = above the system; smoke (0,0,0) = at chimney object.
        let mut local = emitter.origin;
        let dirs = emitter.directions;
        let mut random_pos = [0.0f32; 3];
        if emitter.name == "boxrandom" {
            for axis in 0..3 {
                let lo = emitter.distancemin[axis].min(emitter.distancemax[axis]);
                let hi = emitter.distancemin[axis]
                    .max(emitter.distancemax[axis])
                    .max(lo + 1e-4);
                let mut dist = self.next_range(lo, hi);
                if self.next_f() < 0.5 {
                    dist = -dist;
                }
                random_pos[axis] = dist * dirs[axis];
            }
        } else {
            // LWE ortho sphererandom: distanceMin.x / distanceMax.x only
            let dmin = emitter.distancemin[0];
            let dmax = emitter.distancemax[0].max(dmin + 0.01);
            let angle = self.next_f() * std::f32::consts::TAU;
            let radius = self.next_range(dmin * dmin, dmax * dmax).sqrt();
            random_pos = [
                radius * angle.cos() * dirs[0],
                radius * angle.sin() * dirs[1],
                self.next_range(-dmax, dmax) * dirs[2],
            ];
        }
        for axis in 0..3 {
            if emitter.sign[axis] == 1 {
                random_pos[axis] = random_pos[axis].abs();
            } else if emitter.sign[axis] == -1 {
                random_pos[axis] = -random_pos[axis].abs();
            }
        }
        for axis in 0..3 {
            local[axis] += random_pos[axis];
        }

        let mut vel = [0.0_f32, 0.0, 0.0];
        if emitter.speedmax > 0.0 || emitter.speedmin != 0.0 {
            let len = (random_pos[0] * random_pos[0]
                + random_pos[1] * random_pos[1]
                + random_pos[2] * random_pos[2])
                .sqrt();
            let dir = if len > 1e-4 {
                [random_pos[0] / len, random_pos[1] / len, random_pos[2] / len]
            } else {
                [0.0, 1.0, 0.0]
            };
            let speed = self.next_range(
                emitter.speedmin.min(emitter.speedmax),
                emitter.speedmin.max(emitter.speedmax),
            );
            for axis in 0..3 {
                vel[axis] += dir[axis] * speed;
            }
        }

        // LWE defaults before initializers
        let mut size = 20.0_f32 * self.ov_size;
        let mut color = [
            1.0_f32 * self.ov_colorn[0],
            1.0 * self.ov_colorn[1],
            1.0 * self.ov_colorn[2],
        ];
        let mut max_life = 1.0_f32 * self.ov_lifetime;
        let mut alpha = 1.0_f32 * self.alpha_mul;
        let mut rotation = 0.0_f32;
        let mut angular_vel = 0.0_f32;
        let mut osc_freq = [0.0f32; 3];
        let mut osc_scale = [0.0f32; 3];
        let mut osc_phase = [0.0f32; 3];

        let inits = self.initializers.clone();
        for init in &inits {
            match init.name.as_str() {
                "lifetimerandom" => {
                    // LWE: randomFloat(min,max) * lifetimeOverride
                    let a = init.min.as_f32().unwrap_or(1.0);
                    let b = init.max.as_f32().unwrap_or(a);
                    max_life = self.next_range(a.min(b), a.max(b)).max(0.05) * self.ov_lifetime;
                }
                "sizerandom" => {
                    // LWE: (min + pow(t,exp)*(max-min)) * sizeOverride / 2
                    let a = init.min.as_f32().unwrap_or(4.0);
                    let b = init.max.as_f32().unwrap_or(a);
                    let exponent = init
                        .extras
                        .get("exponent")
                        .and_then(|v| v.as_f32())
                        .filter(|e| *e > 0.0)
                        .unwrap_or(1.0);
                    let t = self.next_f().powf(exponent);
                    size = (a + t * (b - a)) * self.ov_size * 0.5;
                }
                "velocityrandom" => {
                    // y-up: negative Y = fall (WE). No LWE local Y flip (see module docs).
                    let lo = vec3_from_value(&init.min).unwrap_or([-10.0, -50.0, 0.0]);
                    let hi = vec3_from_value(&init.max).unwrap_or(lo);
                    vel = [
                        vel[0]
                            + self.next_range(lo[0].min(hi[0]), lo[0].max(hi[0])) * self.ov_speed,
                        vel[1]
                            + self.next_range(lo[1].min(hi[1]), lo[1].max(hi[1])) * self.ov_speed,
                        vel[2]
                            + self.next_range(lo[2].min(hi[2]), lo[2].max(hi[2])) * self.ov_speed,
                    ];
                }
                "colorrandom" => {
                    // LWE: randomVec3(min,max) * colorn  (per-channel)
                    let lo = color_unit_from_value(&init.min).unwrap_or([1.0, 1.0, 1.0]);
                    let hi = color_unit_from_value(&init.max).unwrap_or(lo);
                    color = [
                        self.next_range(lo[0].min(hi[0]), lo[0].max(hi[0])) * self.ov_colorn[0],
                        self.next_range(lo[1].min(hi[1]), lo[1].max(hi[1])) * self.ov_colorn[1],
                        self.next_range(lo[2].min(hi[2]), lo[2].max(hi[2])) * self.ov_colorn[2],
                    ];
                }
                "turbulentvelocityrandom" => {
                    // Direction = curl-ish noise clamped to a cone around `forward`,
                    // tilted by `offset` about `right` (WE semantics, y-up).
                    let mut forward = init
                        .extras
                        .get("forward")
                        .and_then(vec3_from_value)
                        .unwrap_or([0.0, 0.0, 0.0]);
                    if norm(&mut forward) < 1e-4 {
                        forward = [0.0, 1.0, 0.0];
                    }
                    let mut right = init
                        .extras
                        .get("right")
                        .and_then(vec3_from_value)
                        .unwrap_or([0.0, 0.0, 0.0]);
                    if norm(&mut right) < 1e-4 {
                        right = [1.0, 0.0, 0.0];
                    }
                    let scale = init
                        .extras
                        .get("scale")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.1);
                    let offset = init
                        .extras
                        .get("offset")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.0);
                    let smin = init
                        .extras
                        .get("speedmin")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(50.0);
                    let smax = init
                        .extras
                        .get("speedmax")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(smin);
                    let phase = init
                        .extras
                        .get("phasemin")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.0);
                    let phase_max = init
                        .extras
                        .get("phasemax")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(1.0);
                    // Smooth pseudo-curl noise sample (deterministic per particle).
                    let nph = self.next_range(phase.min(phase_max), phase.min(phase_max).max(phase_max));
                    let n = [
                        (nph * 12.9898).sin(),
                        (nph * 78.233).sin(),
                        (nph * 37.719).sin(),
                    ];
                    let mut dir = if n[0].abs() + n[1].abs() + n[2].abs() < 1e-4 {
                        forward
                    } else {
                        let mut d = n;
                        norm(&mut d);
                        d
                    };
                    // scale < 2 clamps the deviation angle from forward (scale/2 of π).
                    if scale < 2.0 {
                        let cosang = dot(dir, forward).clamp(-1.0, 1.0);
                        let ang = cosang.acos() / std::f32::consts::PI;
                        let max_angle = scale / 2.0;
                        if ang > max_angle && max_angle > 1e-4 {
                            let mut axis = cross(dir, forward);
                            if norm(&mut axis) > 1e-4 {
                                dir = rotate_around(dir, axis, (ang - max_angle) * std::f32::consts::PI);
                            }
                        }
                    }
                    // offset tilts the result about the right axis.
                    if offset.abs() > 1e-4 {
                        dir = rotate_around(dir, right, -offset);
                    }
                    // 2D ortho: no z drift.
                    dir[2] = 0.0;
                    norm(&mut dir);
                    let speed = self.next_range(smin.min(smax), smin.max(smax)) * self.ov_speed;
                    for axis in 0..3 {
                        vel[axis] += dir[axis] * speed;
                    }
                }
                "alpharandom" => {
                    // LWE: randomFloat * alphaOverride (alpha_mul already default)
                    let a = init.min.as_f32().unwrap_or(1.0);
                    let b = init.max.as_f32().unwrap_or(a);
                    alpha = self.next_range(a.min(b), a.max(b)) * self.alpha_mul;
                }
                "rotationrandom" => {
                    // Scalar or vec3; 2D uses Z. Empty → full 0..2π.
                    let lo = vec3_from_value(&init.min).unwrap_or([0.0, 0.0, 0.0]);
                    let hi = vec3_from_value(&init.max).unwrap_or([
                        std::f32::consts::TAU,
                        std::f32::consts::TAU,
                        std::f32::consts::TAU,
                    ]);
                    // Prefer Z; if only X authored (common), use X.
                    let (a, b) = if (hi[2] - lo[2]).abs() > 1e-6 || lo[2].abs() + hi[2].abs() > 0.0
                    {
                        (lo[2], hi[2])
                    } else {
                        (lo[0], hi[0])
                    };
                    rotation = self.next_range(a.min(b), a.max(b));
                }
                "angularvelocityrandom" => {
                    let lo = vec3_from_value(&init.min).unwrap_or([-1.0, -1.0, -1.0]);
                    let hi = vec3_from_value(&init.max).unwrap_or([1.0, 1.0, 1.0]);
                    let (a, b) = if (hi[2] - lo[2]).abs() > 1e-6 || lo[2].abs() + hi[2].abs() > 0.0
                    {
                        (lo[2], hi[2])
                    } else {
                        (lo[0], hi[0])
                    };
                    angular_vel = self.next_range(a.min(b), a.max(b));
                }
                _ => {}
            }
        }

        let mut size_start = 1.0f32;
        let mut size_end = 0.0f32; // WE default: shrink away when only starttime set
        let mut size_t0 = 0.0f32;
        let mut size_t1 = 1.0f32;
        // colorchange multipliers (LWE defaults start=end=1 → no change).
        let mut color_start_mul = [1.0f32, 1.0, 1.0];
        let mut color_end_mul = [1.0f32, 1.0, 1.0];
        let mut color_t0 = 0.0f32;
        let mut color_t1 = 1.0f32;
        let mut has_color_change = false;
        let ops = self.operators.clone();
        for op in &ops {
            match op.name.as_str() {
                "oscillateposition" | "oscillatesize" | "oscillatealpha" => {
                    // Shared birth rolls for frequency/scale/phase.
                    let fmin = op
                        .params
                        .get("frequencymin")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.5);
                    let fmax = op
                        .params
                        .get("frequencymax")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(fmin);
                    let smin = op
                        .params
                        .get("scalemin")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(if op.name == "oscillateposition" {
                            10.0
                        } else {
                            0.2
                        });
                    let smax = op
                        .params
                        .get("scalemax")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(smin);
                    let pmin = op
                        .params
                        .get("phasemin")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.0);
                    let pmax = op
                        .params
                        .get("phasemax")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(std::f32::consts::TAU);
                    for axis in 0..3 {
                        osc_freq[axis] = self.next_range(fmin.min(fmax), fmin.max(fmax));
                        osc_scale[axis] = self.next_range(smin.min(smax), smin.max(smax));
                        osc_phase[axis] = self.next_range(pmin.min(pmax), pmin.max(pmax));
                    }
                }
                "sizechange" => {
                    // Times are **lifetime fractions** (WE docs).
                    size_start = op
                        .params
                        .get("startvalue")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(1.0);
                    size_end = op
                        .params
                        .get("endvalue")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.0);
                    size_t0 = op
                        .params
                        .get("starttime")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.0)
                        .clamp(0.0, 1.0);
                    size_t1 = op
                        .params
                        .get("endtime")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0);
                    if size_t1 < size_t0 {
                        std::mem::swap(&mut size_t0, &mut size_t1);
                    }
                }
                "colorchange" => {
                    // LWE parses start/end via user-settings (raw vec3 multipliers,
                    // default 1 1 1) — NOT ColorBuilder /255. Multiplies initial.
                    has_color_change = true;
                    if let Some(v) = op.params.get("endvalue").and_then(color_mul_from_value) {
                        color_end_mul = v;
                    }
                    if let Some(v) = op.params.get("startvalue").and_then(color_mul_from_value) {
                        color_start_mul = v;
                    }
                    color_t0 = op
                        .params
                        .get("starttime")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(0.0)
                        .clamp(0.0, 1.0);
                    color_t1 = op
                        .params
                        .get("endtime")
                        .and_then(|v| v.as_f32())
                        .unwrap_or(1.0)
                        .clamp(0.0, 1.0);
                }
                _ => {}
            }
        }

        // LWE: initial.alpha already includes instance alpha (set above).
        let base_alpha = alpha.clamp(0.0, 1.0);
        // sizechange multiplies initial.size; apply startvalue at birth like LWE sizechange op
        let birth_size = (size * size_start).max(0.01);
        let n_frames = self
            .texture
            .as_ref()
            .map(|t| t.frames.len())
            .unwrap_or(0)
            .max(1);
        let (locked_frame, frame_phase) = match self.animation_mode {
            ParticleAnimationMode::RandomFrame => {
                (Some((self.next_f() * n_frames as f32) as usize % n_frames), 0.0)
            }
            ParticleAnimationMode::Sequence => (None, self.next_f()),
        };

        Some(WeParticle {
            pos: local,
            vel,
            size: birth_size,
            base_size: size,
            alpha: base_alpha,
            base_alpha,
            color,
            base_color: color,
            life: max_life,
            max_life,
            age: 0.0,
            rotation,
            angular_vel,
            osc_freq,
            osc_scale,
            osc_phase,
            size_start,
            size_end,
            size_t0,
            size_t1,
            color_start_mul,
            color_end_mul,
            color_t0,
            color_t1,
            has_color_change,
            frame_phase,
            locked_frame,
            alive: true,
        })
    }

    pub fn tick(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.05);
        self.elapsed += dt;

        let rate = self.emitters.first().map(|e| e.rate).unwrap_or(10.0) * self.ov_rate;
        self.emit_acc += rate * dt;
        while self.emit_acc >= 1.0 {
            self.emit_acc -= 1.0;
            if let Some(p) = self.create_particle() {
                if self.particles.len() < self.maxcount as usize {
                    self.particles.push(p);
                } else if let Some(i) = self.particles.iter().position(|p| !p.alive) {
                    self.particles[i] = p;
                }
            }
        }

        let ops = self.operators.clone();
        let cursor = self.cursor_cam;
        let origin = self.origin_cam;
        let n = self.particles.len();
        for i in 0..n {
            if !self.particles[i].alive {
                continue;
            }
            let mut gravity = [0.0_f32, 0.0, 0.0];
            let mut drag = 0.0_f32;
            let mut ang_force = 0.0_f32;
            let mut ang_drag = 0.0_f32;
            for op in &ops {
                match op.name.as_str() {
                    "movement" => {
                        // LWE: vel += gravity * dt * speedOverride (no local Y flip here).
                        if let Some(g) = op.params.get("gravity") {
                            let gv = vec3_from_value(g).unwrap_or([0.0, 0.0, 0.0]);
                            gravity = [gv[0], gv[1], gv[2]];
                        }
                        drag = op
                            .params
                            .get("drag")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(0.0);
                    }
                    "angularmovement" => {
                        // LWE: rotation += angularVel * dt * speed; angVel += force * dt * speed
                        if let Some(f) = op.params.get("force").and_then(vec3_from_value) {
                            ang_force = f[2];
                            if ang_force.abs() < 1e-6 {
                                ang_force = f[0];
                            }
                        }
                        ang_drag = op
                            .params
                            .get("drag")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(0.0);
                    }
                    "alphafade" => {
                        // WE: fadeintime / fadeouttime are **lifetime fractions**.
                        // fade-in: 0 → fade_in completes the ramp to full alpha.
                        // fade-out: fade_out is when fade-out **starts** (→ 1.0 dies).
                        let fade_in = op
                            .params
                            .get("fadeintime")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(0.1)
                            .clamp(0.0, 1.0);
                        let fade_out = op
                            .params
                            .get("fadeouttime")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(1.0)
                            .clamp(0.0, 1.0);
                        let p = &mut self.particles[i];
                        let life_frac = (p.age / p.max_life.max(1e-4)).clamp(0.0, 1.0);
                        let mut a = 1.0;
                        if fade_in > 1e-4 && life_frac < fade_in {
                            a *= life_frac / fade_in;
                        }
                        if fade_out < 1.0 - 1e-4 && life_frac > fade_out {
                            let span = (1.0 - fade_out).max(1e-4);
                            a *= ((1.0 - life_frac) / span).clamp(0.0, 1.0);
                        }
                        p.alpha = (p.base_alpha * a).clamp(0.0, 1.0);
                    }
                    "oscillateposition" => {
                        let mask = op
                            .params
                            .get("mask")
                            .and_then(vec3_from_value)
                            .unwrap_or([1.0, 0.0, 0.0]);
                        let p = &mut self.particles[i];
                        for axis in 0..3 {
                            let w = p.osc_freq[axis];
                            let mv = -p.osc_scale[axis]
                                * w
                                * (w * p.age + p.osc_phase[axis]).sin()
                                * dt;
                            p.pos[axis] += mv * mask[axis] * self.ov_speed;
                        }
                    }
                    "oscillatealpha" => {
                        let smin = op
                            .params
                            .get("scalemin")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(0.2);
                        let p = &mut self.particles[i];
                        let pulse =
                            0.5 + 0.5 * (p.age * p.osc_freq[0] * std::f32::consts::TAU + p.osc_phase[0]).sin();
                        p.alpha = (p.base_alpha * (smin + (1.0 - smin) * pulse)).clamp(0.0, 1.0);
                    }
                    "oscillatesize" => {
                        let smin = op
                            .params
                            .get("scalemin")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(0.5);
                        let smax = op
                            .params
                            .get("scalemax")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(1.0);
                        let p = &mut self.particles[i];
                        let pulse =
                            0.5 + 0.5 * (p.age * p.osc_freq[0] * std::f32::consts::TAU + p.osc_phase[0]).sin();
                        let mul = smin + (smax - smin) * pulse;
                        p.size = (p.base_size * mul).max(0.5);
                    }
                    "sizechange" => {
                        let p = &mut self.particles[i];
                        let life_frac = (p.age / p.max_life.max(1e-4)).clamp(0.0, 1.0);
                        let mul = fade_value(
                            life_frac,
                            p.size_t0,
                            p.size_t1,
                            p.size_start,
                            p.size_end,
                        );
                        p.size = (p.base_size * mul).max(0.5);
                    }
                    "colorchange" => {
                        // LWE: p.color = initial * fade(startMul, endMul) per channel.
                        let p = &mut self.particles[i];
                        if !p.has_color_change {
                            continue;
                        }
                        let life_frac = (p.age / p.max_life.max(1e-4)).clamp(0.0, 1.0);
                        p.color = [
                            p.base_color[0]
                                * fade_value(
                                    life_frac,
                                    p.color_t0,
                                    p.color_t1,
                                    p.color_start_mul[0],
                                    p.color_end_mul[0],
                                ),
                            p.base_color[1]
                                * fade_value(
                                    life_frac,
                                    p.color_t0,
                                    p.color_t1,
                                    p.color_start_mul[1],
                                    p.color_end_mul[1],
                                ),
                            p.base_color[2]
                                * fade_value(
                                    life_frac,
                                    p.color_t0,
                                    p.color_t1,
                                    p.color_start_mul[2],
                                    p.color_end_mul[2],
                                ),
                        ];
                    }
                    "turbulence" => {
                        let scale = op
                            .params
                            .get("scale")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(0.002);
                        let speed = op
                            .params
                            .get("speedmin")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(50.0);
                        let speed_max = op
                            .params
                            .get("speedmax")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(speed);
                        let mask = op
                            .params
                            .get("mask")
                            .and_then(vec3_from_value)
                            .unwrap_or([1.0, 1.0, 0.0]);
                        let timescale = op
                            .params
                            .get("timescale")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(1.0);
                        let p = &mut self.particles[i];
                        let spd = speed + (speed_max - speed) * 0.5;
                        let t = p.age * spd * 0.01 * timescale + p.osc_phase[0] * 10.0;
                        // Strength: WE scale is often 0.33..50; normalize softly.
                        let amp = if scale.abs() > 1.0 {
                            scale
                        } else {
                            scale * 1000.0
                        };
                        p.vel[0] += t.sin() * amp * mask[0] * dt;
                        p.vel[1] += (t * 1.3).cos() * amp * mask[1] * dt;
                    }
                    "controlpointattract" => {
                        // Pull/push toward control point (CP1 ≈ mouse when locked).
                        let cp_idx = op
                            .params
                            .get("controlpoint")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(0.0) as i32;
                        let scale = op
                            .params
                            .get("scale")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(100.0);
                        let threshold = op
                            .params
                            .get("threshold")
                            .and_then(|v| v.as_f32())
                            .unwrap_or(256.0)
                            .max(1.0);
                        let off = op
                            .params
                            .get("origin")
                            .and_then(vec3_from_value)
                            .unwrap_or([0.0, 0.0, 0.0]);
                        // CP0 = system origin; CP1+ = cursor when available.
                        let cp = if cp_idx <= 0 {
                            [origin[0] + off[0], origin[1] + off[1]]
                        } else if let Some(c) = cursor {
                            [c[0] + off[0], c[1] + off[1]]
                        } else {
                            [origin[0] + off[0], origin[1] + off[1]]
                        };
                        let pos = self.particles[i].pos;
                        let world = self.local_to_camera(pos);
                        let dx = cp[0] - world[0];
                        let dy = cp[1] - world[1];
                        let dist = (dx * dx + dy * dy).sqrt().max(1e-3);
                        if dist < threshold {
                            let fall = 1.0 - (dist / threshold);
                            let force = scale * fall * fall / dist;
                            let inv_s =
                                1.0 / self.scale[0].abs().max(self.scale[1].abs()).max(0.01);
                            let p = &mut self.particles[i];
                            p.vel[0] += dx * force * inv_s * dt;
                            p.vel[1] += dy * force * inv_s * dt;
                        }
                    }
                    _ => {}
                }
            }

            // LWE createMovementOperator / createAngularMovementOperator:
            // position += velocity * dt
            // velocity += gravity * dt * speedOverride
            // velocity *= max(0, 1 - drag*dt)
            // rotation += angularVelocity * dt * speedOverride
            // angularVelocity += force * dt * speedOverride
            let spd = self.ov_speed;
            let p = &mut self.particles[i];
            p.pos[0] += p.vel[0] * dt;
            p.pos[1] += p.vel[1] * dt;
            p.pos[2] += p.vel[2] * dt;
            p.vel[0] += gravity[0] * dt * spd;
            p.vel[1] += gravity[1] * dt * spd;
            p.vel[2] += gravity[2] * dt * spd;
            if drag.abs() > 1e-6 {
                let d = (1.0 - drag * dt).max(0.0);
                p.vel[0] *= d;
                p.vel[1] *= d;
                p.vel[2] *= d;
            }
            p.rotation += p.angular_vel * dt * spd;
            p.angular_vel += ang_force * dt * spd;
            if ang_drag.abs() > 1e-6 {
                p.angular_vel *= (1.0 - ang_drag * dt).max(0.0);
            }
            // wrap rotation like LWE
            let pi = std::f32::consts::PI;
            let tau = std::f32::consts::TAU;
            while p.rotation > pi {
                p.rotation -= tau;
            }
            while p.rotation < -pi {
                p.rotation += tau;
            }
            p.age += dt;
            p.life = p.max_life - p.age;
            if p.life <= 0.0 {
                p.alive = false;
            }
        }
    }

    /// Transform local particle → camera-space world position (T * R(a) * S * local).
    pub fn local_to_camera(&self, local: [f32; 3]) -> [f32; 3] {
        let sx = self.scale[0];
        let sy = self.scale[1];
        let sz = self.scale[2];
        let lx = local[0] * sx;
        let ly = local[1] * sy;
        let lz = local[2] * sz;
        let ang = self.angle_z;
        let c = ang.cos();
        let s = ang.sin();
        let rx = lx * c - ly * s;
        let ry = lx * s + ly * c;
        [
            self.origin_cam[0] + rx,
            self.origin_cam[1] + ry,
            self.origin_cam[2] + lz,
        ]
    }

    pub fn alive(&self) -> impl Iterator<Item = &WeParticle> {
        self.particles.iter().filter(|p| p.alive)
    }
}

/// LWE fadeValue: clamp-lerp between start/end values over [t0, t1].
fn fade_value(t: f32, t0: f32, t1: f32, v0: f32, v1: f32) -> f32 {
    if t <= t0 {
        v0
    } else if t >= t1 {
        v1
    } else {
        v0 + (v1 - v0) * ((t - t0) / (t1 - t0))
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

/// Normalize in place, returning the previous length.
fn norm(v: &mut [f32; 3]) -> f32 {
    let len = dot(*v, *v).sqrt();
    if len > 1e-8 {
        *v = [v[0] / len, v[1] / len, v[2] / len];
    }
    len
}

/// Rotate `v` about normalized `axis` by `angle` (Rodrigues).
fn rotate_around(v: [f32; 3], axis: [f32; 3], angle: f32) -> [f32; 3] {
    let c = angle.cos();
    let s = angle.sin();
    let cr = cross(axis, v);
    let d = dot(axis, v);
    [
        v[0] * c + cr[0] * s + axis[0] * d * (1.0 - c),
        v[1] * c + cr[1] * s + axis[1] * d * (1.0 - c),
        v[2] * c + cr[2] * s + axis[2] * d * (1.0 - c),
    ]
}

fn vec3_from_value(v: &EffectValue) -> Option<[f32; 3]> {
    match v {
        EffectValue::Vec3(c) => Some(*c),
        EffectValue::Vec4(c) => Some([c[0], c[1], c[2]]),
        EffectValue::Vec2(c) => Some([c[0], c[1], 0.0]),
        EffectValue::String(s) => parse_vec3(s),
        EffectValue::Float(f) => Some([*f, *f, *f]),
        EffectValue::Scripted { default, .. } => Some([*default, *default, *default]),
    }
}

fn color_from_value(v: &EffectValue) -> Option<[f32; 3]> {
    vec3_from_value(v)
}

/// ColorBuilder-style: integer 0–255 triples → unit RGB; already-unit floats stay.
fn color_unit_from_value(v: &EffectValue) -> Option<[f32; 3]> {
    let c = color_from_value(v)?;
    if c[0] > 1.0 || c[1] > 1.0 || c[2] > 1.0 {
        Some([c[0] / 255.0, c[1] / 255.0, c[2] / 255.0])
    } else {
        Some(c)
    }
}

/// colorchange start/end are raw multipliers (LWE `user()`, default 1). If an
/// author used 0–255 style by mistake, still accept it.
fn color_mul_from_value(v: &EffectValue) -> Option<[f32; 3]> {
    color_unit_from_value(v)
}

/// Apply genericparticle `ConvertTexture0Format` for particle sprites.
fn expand_particle_texture_format(tex: &mut crate::tex::DecodedTex) {
    use crate::tex::TexFormat;
    match tex.format {
        TexFormat::Rg88 => {
            // Shader: sample.rrrg → RGB = R, A = G.
            let n = (tex.width as usize).saturating_mul(tex.height as usize);
            if tex.rgba.len() < n * 4 {
                return;
            }
            for i in 0..n {
                let r = tex.rgba[i * 4];
                let g = tex.rgba[i * 4 + 1];
                tex.rgba[i * 4] = r;
                tex.rgba[i * 4 + 1] = r;
                tex.rgba[i * 4 + 2] = r;
                tex.rgba[i * 4 + 3] = g;
            }
            tex.format = TexFormat::Argb8888;
        }
        TexFormat::R8 => {
            // Shader: vec4(1, 1, 1, sample.r) — softness lives in alpha.
            let n = (tex.width as usize).saturating_mul(tex.height as usize);
            if tex.rgba.len() < n * 4 {
                return;
            }
            for i in 0..n {
                let r = tex.rgba[i * 4];
                tex.rgba[i * 4] = 255;
                tex.rgba[i * 4 + 1] = 255;
                tex.rgba[i * 4 + 2] = 255;
                tex.rgba[i * 4 + 3] = r;
            }
            tex.format = TexFormat::Argb8888;
        }
        _ => {}
    }
}

// silence unused import if screen_to_camera not used directly
#[allow(dead_code)]
fn _keep(x: f32, y: f32, w: f32, h: f32) -> [f32; 2] {
    screen_to_camera(x, y, w, h)
}
