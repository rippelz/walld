//! MDLS0004 local bind transforms and MDLA0006 sampled bone animation.
//!
//! Vertices are in model bind space. Skinning uses animated_world *
//! inverse(bind_world), with parent transforms composed before the child.
//! Each MDLA track contains (frame_count + 1) TRS samples, including the
//! closing sample. Track headers are encoding/byte length, not bone IDs.

use glam::{EulerRot, Mat4, Quat, Vec3};
use serde_json::Value;

#[derive(Debug, Clone, Copy)]
struct Pose {
    translation: Vec3,
    rotation: Quat,
    scale: Vec3,
}

impl Pose {
    fn matrix(self) -> Mat4 {
        Mat4::from_scale_rotation_translation(self.scale, self.rotation, self.translation)
    }

    fn mix(self, other: Self, amount: f32) -> Self {
        Self {
            translation: self.translation.lerp(other.translation, amount),
            rotation: self.rotation.slerp(other.rotation, amount).normalize(),
            scale: self.scale.lerp(other.scale, amount),
        }
    }
}

#[derive(Debug, Clone)]
struct Bone {
    parent: Option<usize>,
    bind: Pose,
    inverse_bind: Mat4,
}

#[derive(Debug, Clone)]
struct Clip {
    id: u32,
    fps: f32,
    length: f32,
    looped: bool,
    tracks: Vec<Vec<Pose>>,
}

impl Clip {
    fn sample(&self, bone: usize, seconds: f32) -> Pose {
        let frame = seconds * self.fps;
        let frame = if self.looped {
            frame.rem_euclid(self.length)
        } else {
            frame.clamp(0.0, self.length)
        };
        let track = &self.tracks[bone];
        let first = (frame.floor() as usize).min(track.len() - 1);
        let next = (first + 1).min(track.len() - 1);
        track[first].mix(track[next], frame.fract())
    }
}

#[derive(Debug, Clone)]
struct Layer {
    clip: usize,
    time: f32,
    rate: f32,
    blend: f32,
    additive: bool,
}

#[derive(Debug, Clone)]
struct Influence {
    bones: [usize; 4],
    weights: [f32; 4],
}

#[derive(Debug, Clone)]
pub(crate) struct Skeleton {
    bones: Vec<Bone>,
    clips: Vec<Clip>,
    layers: Vec<Layer>,
    rest: Vec<Vec3>,
    influences: Vec<Influence>,
}

impl Skeleton {
    pub(super) fn parse(
        data: &[u8],
        vertex_offset: usize,
        stride: usize,
        vertices: &[f32],
    ) -> Result<Option<Self>, String> {
        let Some(start) = data.windows(9).position(|w| w == b"MDLS0004\0") else {
            return Ok(None);
        };
        let Some(anim_start) = data.windows(9).position(|w| w == b"MDLA0006\0") else {
            return Ok(None);
        };
        // Only this vertex layout is currently decoded for bone influences.
        // Other layouts retain their static mesh instead of interpreting UVs
        // or tangent data as weights.
        if stride != 80 {
            return Err(format!("unsupported skinned vertex stride {stride}"));
        }
        let mut r = Reader {
            data,
            pos: start + 9,
        };
        let skeleton_end = r.u32()? as usize;
        if skeleton_end <= r.pos || skeleton_end > data.len() || skeleton_end > anim_start {
            return Err("invalid skeleton section boundary".into());
        }
        r.data = &data[..skeleton_end];
        let count = r.u32()? as usize;
        if count == 0 || count > 4096 {
            return Err("invalid bone count".into());
        }
        let mut bones = Vec::with_capacity(count);
        let mut bind_matrices = Vec::with_capacity(count);
        for _ in 0..count {
            r.take(1)?; // flags
            r.u32()?; // bone type
            let parent = match r.u32()? {
                u32::MAX => None,
                p if (p as usize) < count => Some(p as usize),
                _ => return Err("invalid parent bone".into()),
            };
            let bytes = r.u32()? as usize;
            if bytes != 64 {
                return Err("unsupported bind transform size".into());
            }
            let mut columns = [0.0; 16];
            for v in &mut columns {
                *v = r.f32()?;
            }
            let matrix = Mat4::from_cols_array(&columns);
            if matrix.determinant().abs() < 1e-8 {
                return Err("singular bind transform".into());
            }
            let (scale, rotation, translation) = matrix.to_scale_rotation_translation();
            r.string()?; // optional bone name
            bones.push(Bone {
                parent,
                bind: Pose {
                    translation,
                    rotation,
                    scale,
                },
                inverse_bind: Mat4::IDENTITY,
            });
            bind_matrices.push(matrix);
        }
        let bind_world = world_matrices(&bones, &bind_matrices)?;
        for (bone, world) in bones.iter_mut().zip(bind_world) {
            bone.inverse_bind = world.inverse();
        }

        let mut r = Reader {
            data,
            pos: anim_start + 9,
        };
        let animation_end = r.u32()? as usize;
        if animation_end <= r.pos || animation_end > data.len() {
            return Err("invalid animation section boundary".into());
        }
        r.data = &data[..animation_end];
        let clip_count = r.u32()? as usize;
        if clip_count > 4096 {
            return Err("invalid clip count".into());
        }
        let mut clips = Vec::with_capacity(clip_count);
        for _ in 0..clip_count {
            let id = r.u32()?;
            r.u32()?; // flags
            r.string()?; // display name; scene layers select by ID
            let looped = r.string()? == "loop";
            let fps = r.f32()?;
            let frames = r.u32()? as usize;
            r.u32()?; // flags
            let track_count = r.u32()? as usize;
            if fps <= 0.0 || frames == 0 || frames > 1_000_000 || track_count != count {
                return Err("invalid animation dimensions".into());
            }
            let mut tracks = Vec::with_capacity(count);
            for _ in 0..track_count {
                let encoding = r.u32()?;
                let bytes = r.u32()? as usize;
                if encoding != 0 || bytes != (frames + 1) * 36 {
                    return Err(format!(
                        "unsupported animation track encoding {encoding} / {bytes} bytes"
                    ));
                }
                // Check the full track before allocating based on untrusted counts.
                if bytes > r.data.len().saturating_sub(r.pos) {
                    return Err("truncated animation track".into());
                }
                let mut track = Vec::with_capacity(frames + 1);
                for _ in 0..=frames {
                    let translation = r.vec3()?;
                    let angles = r.vec3()?;
                    let scale = r.vec3()?;
                    let rotation = Quat::from_euler(EulerRot::XYZ, angles.x, angles.y, angles.z);
                    track.push(Pose {
                        translation,
                        rotation,
                        scale,
                    });
                }
                tracks.push(track);
            }
            // MDLA0006's reserved per-clip tail precedes the next clip ID.
            // Do not scan for names: arbitrary UTF-8 names and multiple clips
            // must retain their exact record boundaries.
            r.take(35)?;
            clips.push(Clip {
                id,
                fps,
                length: frames as f32,
                looped,
                tracks,
            });
        }

        let rest: Vec<Vec3> = vertices
            .chunks_exact(5)
            .map(|v| Vec3::new(v[0], v[1], v[2]))
            .collect();
        let mut influences = Vec::with_capacity(rest.len());
        for i in 0..rest.len() {
            let mut r = Reader {
                data,
                pos: vertex_offset + i * stride + 40,
            };
            let mut indices = [0; 4];
            for idx in &mut indices {
                *idx = r.u32()? as usize;
            }
            let mut weights = [0.0; 4];
            for (idx, weight) in indices.iter().zip(&mut weights) {
                *weight = r.f32()?;
                if *weight < 0.0 || (*weight > 0.0 && *idx >= count) {
                    return Err("invalid vertex bone influence".into());
                }
            }
            influences.push(Influence {
                bones: indices,
                weights,
            });
        }
        Ok(Some(Self {
            bones,
            clips,
            layers: Vec::new(),
            rest,
            influences,
        }))
    }

    pub(super) fn set_layers(&mut self, value: &Value) {
        self.layers.clear();
        for layer in value.as_array().into_iter().flatten() {
            if !super::model::parse_bool_visible(layer.get("visible")) {
                continue;
            }
            let Some(id) = layer.get("animation").and_then(Value::as_u64) else {
                continue;
            };
            let Some(clip) = self.clips.iter().position(|c| c.id as u64 == id) else {
                log::warn!("puppet animation layer references missing clip {id}");
                continue;
            };
            let scalar = |key, default| {
                super::model::json_f32(layer.get(key).map(|v| v.get("value").unwrap_or(v)), default)
            };
            let rate = scalar("rate", 1.0);
            let blend = scalar("blend", 1.0).clamp(0.0, 1.0);
            if !rate.is_finite() || !blend.is_finite() {
                continue;
            }
            self.layers.push(Layer {
                clip,
                time: 0.0,
                rate,
                blend,
                additive: layer
                    .get("additive")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
    }

    pub(super) fn tick(&mut self, dt: f32, vertices: &mut [f32]) {
        if !dt.is_finite() {
            return;
        }
        for layer in &mut self.layers {
            let clip = &self.clips[layer.clip];
            layer.time += dt * layer.rate;
            if clip.looped {
                layer.time = layer.time.rem_euclid(clip.length / clip.fps);
            } else {
                layer.time = layer.time.clamp(0.0, clip.length / clip.fps);
            }
        }
        let locals: Vec<Mat4> = self
            .bones
            .iter()
            .enumerate()
            .map(|(i, bone)| {
                let mut pose = bone.bind;
                for layer in &self.layers {
                    let clip = &self.clips[layer.clip];
                    let sample = clip.sample(i, layer.time);
                    if layer.additive {
                        let reference = clip.sample(i, 0.0);
                        pose.translation +=
                            (sample.translation - reference.translation) * layer.blend;
                        let delta = reference.rotation.inverse() * sample.rotation;
                        pose.rotation =
                            (pose.rotation * Quat::IDENTITY.slerp(delta, layer.blend)).normalize();
                        let ratio = Vec3::new(
                            safe_ratio(sample.scale.x, reference.scale.x),
                            safe_ratio(sample.scale.y, reference.scale.y),
                            safe_ratio(sample.scale.z, reference.scale.z),
                        );
                        pose.scale *= Vec3::ONE.lerp(ratio, layer.blend);
                    } else {
                        pose = pose.mix(sample, layer.blend);
                    }
                }
                pose.matrix()
            })
            .collect();
        // Parent indices and cycles were validated during parsing.
        let Ok(world) = world_matrices(&self.bones, &locals) else {
            return;
        };
        let skin: Vec<Mat4> = world
            .iter()
            .zip(&self.bones)
            .map(|(w, b)| *w * b.inverse_bind)
            .collect();
        for ((vertex, rest), inf) in vertices
            .chunks_exact_mut(5)
            .zip(&self.rest)
            .zip(&self.influences)
        {
            let mut position = Vec3::ZERO;
            let mut total = 0.0;
            for (&index, &weight) in inf.bones.iter().zip(&inf.weights) {
                if weight > 0.0 {
                    position += skin[index].transform_point3(*rest) * weight;
                    total += weight;
                }
            }
            position = if total > 1e-6 {
                position / total
            } else {
                *rest
            };
            vertex[..3].copy_from_slice(&position.to_array());
        }
    }
}

fn safe_ratio(value: f32, reference: f32) -> f32 {
    if reference.abs() < 1e-6 {
        1.0
    } else {
        value / reference
    }
}

fn world_matrices(bones: &[Bone], locals: &[Mat4]) -> Result<Vec<Mat4>, String> {
    let mut world = vec![Mat4::IDENTITY; bones.len()];
    let mut ready = vec![false; bones.len()];
    for i in 0..bones.len() {
        let mut chain = Vec::new();
        let mut at = Some(i);
        while let Some(index) = at {
            if ready[index] {
                break;
            }
            if chain.len() >= bones.len() {
                return Err("cyclic bone hierarchy".into());
            }
            chain.push(index);
            at = bones[index].parent;
        }
        for index in chain.into_iter().rev() {
            world[index] = bones[index]
                .parent
                .map(|p| world[p])
                .unwrap_or(Mat4::IDENTITY)
                * locals[index];
            ready[index] = true;
        }
    }
    Ok(world)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.pos.checked_add(n).ok_or("section overflow")?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or("truncated puppet data")?;
        self.pos = end;
        Ok(bytes)
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, String> {
        let f = f32::from_le_bytes(self.take(4)?.try_into().unwrap());
        if f.is_finite() {
            Ok(f)
        } else {
            Err("non-finite puppet transform".into())
        }
    }
    fn vec3(&mut self) -> Result<Vec3, String> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }
    fn string(&mut self) -> Result<&'a str, String> {
        let len = self
            .data
            .get(self.pos..)
            .and_then(|b| b.iter().position(|&b| b == 0))
            .ok_or("unterminated puppet string")?;
        std::str::from_utf8(&self.take(len + 1)?[..len]).map_err(|_| "invalid puppet string".into())
    }
}
