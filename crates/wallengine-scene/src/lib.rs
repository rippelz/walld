//! Scene documents + lightweight runtime state for wallengine.
//!
//! Pure data / CPU simulation — GPU upload stays in the walld renderer.

mod doc;
mod particles;
mod runtime;

pub use doc::*;
pub use particles::{Particle, ParticleSystem};
pub use runtime::{LayerGpuHint, SceneRuntime, TickResult};
