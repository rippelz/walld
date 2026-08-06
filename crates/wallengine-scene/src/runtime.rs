//! CPU-side scene instance: resolves layers, ticks particles.

use crate::doc::{LayerDoc, SceneDocument};
use crate::particles::ParticleSystem;
use std::path::{Path, PathBuf};

/// Hint for the GPU renderer about what to draw for a layer.
#[derive(Debug)]
pub enum LayerGpuHint<'a> {
    Color {
        color: [f32; 4],
        opacity: f32,
    },
    Image {
        path: &'a Path,
        fit: crate::doc::FitMode,
        opacity: f32,
    },
    Particles {
        system: &'a ParticleSystem,
    },
}

pub struct SceneRuntime {
    pub doc: SceneDocument,
    pub base: PathBuf,
    /// Parallel to doc.layers: particle systems where applicable.
    pub particles: Vec<Option<ParticleSystem>>,
    /// Resolved image paths per layer (None if not image).
    pub image_paths: Vec<Option<PathBuf>>,
}

pub struct TickResult {
    pub needs_redraw: bool,
}

impl SceneRuntime {
    pub fn from_document(doc: SceneDocument, base: PathBuf) -> Self {
        let mut particles = Vec::with_capacity(doc.layers.len());
        let mut image_paths = Vec::with_capacity(doc.layers.len());
        for layer in &doc.layers {
            match layer {
                LayerDoc::Particles {
                    preset,
                    count,
                    speed,
                    opacity,
                    ..
                } => {
                    particles.push(Some(ParticleSystem::new(
                        *preset,
                        (*count).clamp(1, 5000),
                        *speed,
                        *opacity,
                    )));
                    image_paths.push(None);
                }
                LayerDoc::Image { path, .. } => {
                    particles.push(None);
                    image_paths.push(Some(SceneDocument::resolve_path(&base, path)));
                }
                LayerDoc::Color { .. } => {
                    particles.push(None);
                    image_paths.push(None);
                }
            }
        }
        Self {
            doc,
            base,
            particles,
            image_paths,
        }
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let (doc, base) = SceneDocument::load_file(path)?;
        Ok(Self::from_document(doc, base))
    }

    pub fn is_animated(&self) -> bool {
        self.doc.is_animated()
    }

    pub fn name(&self) -> &str {
        if self.doc.name.is_empty() {
            "unnamed"
        } else {
            &self.doc.name
        }
    }

    pub fn tick(&mut self, dt: f32) -> TickResult {
        let mut needs = false;
        for sys in self.particles.iter_mut().flatten() {
            sys.tick(dt);
            needs = true;
        }
        TickResult {
            needs_redraw: needs,
        }
    }

    pub fn layers(&self) -> impl Iterator<Item = LayerGpuHint<'_>> {
        self.doc
            .layers
            .iter()
            .enumerate()
            .filter_map(|(i, layer)| match layer {
                LayerDoc::Color { color, opacity, .. } => Some(LayerGpuHint::Color {
                    color: *color,
                    opacity: *opacity,
                }),
                LayerDoc::Image { fit, opacity, .. } => {
                    let path = self.image_paths[i].as_deref()?;
                    Some(LayerGpuHint::Image {
                        path,
                        fit: *fit,
                        opacity: *opacity,
                    })
                }
                LayerDoc::Particles { .. } => {
                    let system = self.particles[i].as_ref()?;
                    Some(LayerGpuHint::Particles { system })
                }
            })
    }
}
