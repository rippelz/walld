//! Simple CPU particle systems for wallpaper layers.

use crate::doc::ParticlePreset;
use std::time::Instant;

#[derive(Clone, Copy, Debug)]
pub struct Particle {
    pub x: f32,
    pub y: f32,
    pub vx: f32,
    pub vy: f32,
    pub size: f32,
    pub alpha: f32,
}

#[derive(Debug)]
pub struct ParticleSystem {
    pub preset: ParticlePreset,
    pub speed: f32,
    pub opacity: f32,
    pub particles: Vec<Particle>,
    rng: u64,
}

impl ParticleSystem {
    pub fn new(preset: ParticlePreset, count: u32, speed: f32, opacity: f32) -> Self {
        let mut sys = Self {
            preset,
            speed: speed.clamp(0.05, 5.0),
            opacity: opacity.clamp(0.0, 1.0),
            particles: Vec::with_capacity(count as usize),
            rng: Instant::now().elapsed().as_nanos() as u64 ^ 0x9E37_79B9_7F4A_7C15,
        };
        for _ in 0..count {
            let p = sys.spawn(true);
            sys.particles.push(p);
        }
        sys
    }

    fn next_f32(&mut self) -> f32 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let u = x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 33;
        (u as f32) / (u32::MAX as f32)
    }

    fn spawn(&mut self, anywhere: bool) -> Particle {
        let x = self.next_f32();
        let y = if anywhere {
            self.next_f32()
        } else {
            -0.05 - self.next_f32() * 0.1
        };
        match self.preset {
            ParticlePreset::Snow => {
                let size = 0.002 + self.next_f32() * 0.006;
                let vy = (0.03 + self.next_f32() * 0.08) * self.speed;
                let vx = (self.next_f32() - 0.5) * 0.04 * self.speed;
                let alpha = 0.35 + self.next_f32() * 0.55;
                Particle {
                    x,
                    y,
                    vx,
                    vy,
                    size,
                    alpha,
                }
            }
            ParticlePreset::Dust => {
                let size = 0.001 + self.next_f32() * 0.003;
                let vy = (self.next_f32() - 0.5) * 0.02 * self.speed;
                let vx = (self.next_f32() - 0.5) * 0.02 * self.speed;
                let alpha = 0.15 + self.next_f32() * 0.35;
                Particle {
                    x,
                    y,
                    vx,
                    vy,
                    size,
                    alpha,
                }
            }
        }
    }

    /// Advance simulation. Coordinates are normalized 0..1 across the viewport.
    pub fn tick(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.05);
        let n = self.particles.len();
        let mut respawn = Vec::new();
        for i in 0..n {
            let p = &mut self.particles[i];
            p.x += p.vx * dt;
            p.y += p.vy * dt;
            if matches!(self.preset, ParticlePreset::Snow) {
                p.x += (p.y * 6.0).sin() * 0.01 * dt * self.speed;
            }
            let dead = p.y > 1.08 || p.y < -0.15 || p.x < -0.1 || p.x > 1.1;
            if dead {
                respawn.push(i);
            }
        }
        for i in respawn {
            let np = self.spawn(false);
            self.particles[i] = np;
        }
    }
}
