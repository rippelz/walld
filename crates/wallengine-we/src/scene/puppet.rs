//! Wallpaper Engine puppet mesh (.mdl) loader.
//!
//! Puppet layers store a mesh of body parts with UVs into a texture atlas.
//! Drawing the atlas as a single quad shows every part in its sheet layout
//! (decapitated characters, floating hair). We load the mesh and draw it
//! with per-vertex positions in camera space — matching LWE `CImage::loadPuppetMesh`.
//!
//! Also parses **attachment points** (`MDATA0001` / named sockets like `orb`)
//! so child layers (`"attachment": "orb"`) parent to the correct rest pose
//! on the puppet. MDLS/MDLA skeletal playback lives in `skeletal`.

use crate::assets::AssetResolver;
use std::collections::HashMap;
use std::path::Path;

const POSITION_OFFSET: usize = 0;
const MESH_HEADER_SIZE: usize = 8; // two u32s
const MARKER_SIZE: usize = 9; // "MDLV0023\0"

/// MDLV mesh vertex strides seen in the wild. The editor emits 80-byte verts
/// (UV at 72) for simple puppets and 84-byte verts (UV at 76) once extra
/// channels (tangents/weights) are present — e.g. Spirit Blossom Ahri's base
/// mesh. The stride is the first u32 of the mesh block header.
const KNOWN_STRIDES: &[usize] = &[80, 84, 88, 92, 96, 100, 104, 112, 120, 128];

/// UV floats sit 8 bytes before the end of the vertex record.
fn uv_offset(stride: usize) -> usize {
    stride - 8
}

#[derive(Debug, Clone, Default)]
pub struct PuppetMesh {
    /// Interleaved: x,y,z,u,v per vertex. Positions are **y-up, image-center relative** (pixels).
    pub vertices: Vec<f32>,
    pub indices: Vec<u16>,
    /// Named attachment sockets (mesh-local, y-up, center-relative pixels).
    /// Used by child scene objects with `"attachment": "<name>"`.
    pub attachments: HashMap<String, [f32; 2]>,
    pub(crate) skeleton: Option<super::skeletal::Skeleton>,
}

impl PuppetMesh {
    pub fn load(assets: &AssetResolver, path: &str) -> Result<Self, String> {
        let data = assets
            .read_bytes(path)
            .or_else(|_| {
                let p = if path.starts_with("models/") {
                    path.to_string()
                } else {
                    format!("models/{path}")
                };
                assets.read_bytes(&p)
            })
            .map_err(|e| format!("puppet {path}: {e}"))?;
        Self::parse(&data, path)
    }

    pub fn parse(data: &[u8], path: &str) -> Result<Self, String> {
        if data.len() < MARKER_SIZE {
            return Err(format!("puppet {path}: too short"));
        }
        let ver = std::str::from_utf8(&data[..8]).unwrap_or("");
        if ver != "MDLV0021" && ver != "MDLV0023" {
            return Err(format!("puppet {path}: unsupported header {ver}"));
        }

        // Find MDLS end marker (bounds search for mesh block).
        let mdls = data
            .windows(4)
            .position(|w| w == b"MDLS")
            .unwrap_or(data.len());

        let block = find_mesh_block(data, mdls)
            .ok_or_else(|| format!("puppet {path}: no mesh block"))?;

        let stride = block.stride;
        let uv_off = uv_offset(stride);
        let vertex_count = block.vertex_bytes / stride;
        let vertices_off = block.header_offset + MESH_HEADER_SIZE;
        let indices_off = vertices_off + block.vertex_bytes + 4;
        let index_count = block.index_bytes / 2;

        if vertices_off + block.vertex_bytes > data.len()
            || indices_off + block.index_bytes > data.len()
        {
            return Err(format!("puppet {path}: mesh out of bounds"));
        }

        let mut vertices = Vec::with_capacity(vertex_count * 5);
        for i in 0..vertex_count {
            let base = vertices_off + i * stride;
            let x = read_f32(data, base + POSITION_OFFSET);
            let y = read_f32(data, base + POSITION_OFFSET + 4);
            let z = read_f32(data, base + POSITION_OFFSET + 8);
            let u = read_f32(data, base + uv_off);
            let v = read_f32(data, base + uv_off + 4);
            vertices.extend_from_slice(&[x, y, z, u, v]);
        }

        let mut indices = Vec::with_capacity(index_count);
        for i in 0..index_count {
            let idx = read_u16(data, indices_off + i * 2);
            if idx as usize >= vertex_count {
                return Err(format!("puppet {path}: bad index {idx}"));
            }
            indices.push(idx);
        }

        let attachments = parse_attachments(data);
        log::info!(
            "puppet «{}»: verts={} indices={} attachments={}",
            Path::new(path)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or(path),
            vertex_count,
            indices.len(),
            attachments.len()
        );
        if !attachments.is_empty() {
            log::debug!("puppet attachments: {:?}", attachments.keys().collect::<Vec<_>>());
        }
        let skeleton = match super::skeletal::Skeleton::parse(data, vertices_off, stride, &vertices) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("puppet {path}: skeletal playback unavailable: {e}");
                None
            }
        };
        Ok(Self {
            skeleton,
            vertices,
            indices,
            attachments,
        })
    }

    /// Bind authored animation layers by clip ID, never by scene or asset name.
    pub fn set_animation_layers(&mut self, layers: &serde_json::Value) {
        if let Some(skeleton) = &mut self.skeleton {
            skeleton.set_layers(layers);
            skeleton.tick(0.0, &mut self.vertices);
        }
    }

    pub fn tick(&mut self, dt: f32) {
        if let Some(skeleton) = &mut self.skeleton {
            skeleton.tick(dt, &mut self.vertices);
        }
    }

    /// Attachment offset in authored parent-local space (Y-up).
    pub fn attachment_offset_screen(&self, name: &str) -> Option<[f32; 2]> {
        let p = self.attachments.get(name)?;
        Some([p[0], p[1]])
    }

    /// Expand to camera-space triangles: each vertex becomes (cam_x, cam_y, u, v).
    ///
    /// Matches LWE `updatePuppetPositionBuffer` + layer transform:
    /// mesh is y-up / center-relative image pixels; scale is the **resolved**
    /// world scale; `angle_z` is authored Y-up Z rotation, shared with quads.
    ///
    /// `crop_offset` is the model `cropoffset` (fraction of size, often ±0.5):
    /// shifts the mesh pivot inside the layer before scale/rotate.
    pub fn to_camera_tris(
        &self,
        origin_cam: [f32; 2],
        scale: [f32; 2],
        angle_z: f32,
    ) -> Vec<[f32; 4]> {
        self.to_camera_tris_crop(origin_cam, scale, angle_z, [0.0, 0.0], [0.0, 0.0])
    }

    pub fn to_camera_tris_crop(
        &self,
        origin_cam: [f32; 2],
        scale: [f32; 2],
        angle_z: f32,
        crop_offset: [f32; 2],
        size: [f32; 2],
    ) -> Vec<[f32; 4]> {
        // Mesh and scene geometry are both Y-up. Apply the same T * R * S
        // used by image quads; never reflect the layer origin a second time.
        let _ = (crop_offset, size);
        let ang = angle_z;
        let (ca, sa) = (ang.cos(), ang.sin());
        let mut out = Vec::with_capacity(self.indices.len());
        for &idx in &self.indices {
            let i = idx as usize * 5;
            let mx = self.vertices[i] * scale[0];
            let my = self.vertices[i + 1] * scale[1];
            let u = self.vertices[i + 3];
            let v = self.vertices[i + 4];
            let rx = mx * ca - my * sa;
            let ry = mx * sa + my * ca;
            out.push([origin_cam[0] + rx, origin_cam[1] + ry, u, v]);
        }
        out
    }
}

/// Parse named attachment sockets from the MDATA section after the mesh.
///
/// Layout (observed on MDLV0023 / Spirit Blossom Ahri):
/// `…MDATA0001\0` then records of null-terminated name + column-major mat4
/// (translation in elements 12,13). Names like `orb`, `aaaaaaaaa`, `Attachment`.
fn parse_attachments(data: &[u8]) -> HashMap<String, [f32; 2]> {
    let mut out = HashMap::new();
    // Prefer full marker; fall back to DAT0001 (overlaps "MDATA0001").
    let start = data
        .windows(9)
        .position(|w| w == b"MDATA0001")
        .map(|i| i + 9)
        .or_else(|| {
            data.windows(7)
                .position(|w| w == b"DAT0001")
                .map(|i| i + 7)
        });
    let Some(mut o) = start else {
        return out;
    };
    // Skip size u32 + count u16 when present (Ahri: size, count=3).
    if o + 6 <= data.len() {
        let maybe_count = read_u16(data, o + 4);
        if (1..64).contains(&maybe_count) {
            o += 6;
        }
    }
    // Scan remaining for name + identity-ish mat4 patterns.
    let end = data.len().saturating_sub(64 + 4);
    while o < end {
        // Optional u16 bone index before name (0x04, 0x0b, 0x0f on Ahri).
        let mut name_at = o;
        if o + 2 < end {
            let peek = read_u16(data, o);
            if peek < 256 {
                // Could be bone index; name may follow.
                name_at = o + 2;
            }
        }
        // Try null-terminated printable name at name_at or o.
        let mut found = None;
        for try_at in [name_at, o] {
            if try_at >= data.len() {
                continue;
            }
            let mut e = try_at;
            while e < data.len() && e - try_at < 48 && data[e] != 0 {
                let c = data[e];
                if !(c.is_ascii_alphanumeric() || c == b'_' || c == b' ' || c == b'-') {
                    break;
                }
                e += 1;
            }
            if e > try_at && e < data.len() && data[e] == 0 {
                let name = String::from_utf8_lossy(&data[try_at..e]).into_owned();
                if name.len() >= 2 && name.chars().any(|c| c.is_ascii_alphabetic()) {
                    found = Some((try_at, e, name));
                    break;
                }
            }
        }
        let Some((try_at, name_end, name)) = found else {
            o += 1;
            continue;
        };
        let mat_at = name_end + 1;
        if mat_at + 64 > data.len() {
            break;
        }
        // Column-major mat4: m00 at 0, translation at floats 12,13,14, m33 at 15.
        let m00 = read_f32(data, mat_at);
        let m11 = read_f32(data, mat_at + 20);
        let m22 = read_f32(data, mat_at + 40);
        let m33 = read_f32(data, mat_at + 60);
        let tx = read_f32(data, mat_at + 48);
        let ty = read_f32(data, mat_at + 52);
        let tz = read_f32(data, mat_at + 56);
        let looks_like_mat = m00.is_finite()
            && m11.is_finite()
            && m33.is_finite()
            && (m00 - 1.0).abs() < 0.15
            && (m11 - 1.0).abs() < 0.15
            && (m33 - 1.0).abs() < 0.15
            && tx.is_finite()
            && ty.is_finite()
            && tz.is_finite()
            && tx.abs() < 50_000.0
            && ty.abs() < 50_000.0;
        if looks_like_mat {
            // Keep first occurrence (rest pose).
            out.entry(name).or_insert([tx, ty]);
            o = mat_at + 64;
        } else {
            o = try_at + 1;
        }
        if out.len() > 64 {
            break;
        }
    }
    out
}

struct MeshBlock {
    header_offset: usize,
    vertex_bytes: usize,
    index_bytes: usize,
    stride: usize,
}

/// Stride for one (vertex_bytes, index_bytes) pair: the header-declared stride
/// if it's a known one and divides evenly, else the first known stride that
/// yields a sane mesh (UVs in 0..=1 for the sampled verts).
fn detect_stride(data: &[u8], vertices_offset: usize, vertex_bytes: usize, declared: usize) -> Option<usize> {
    let candidates = std::iter::once(declared)
        .chain(KNOWN_STRIDES.iter().copied())
        .filter(|s| *s >= 20 && vertex_bytes % *s == 0 && vertex_bytes / *s > 0);
    for stride in candidates {
        let count = vertex_bytes / stride;
        let uv_off = uv_offset(stride);
        // Sanity by majority vote: with the right stride, positions are a
        // compact cluster and UVs land in [0,1] (a few may poke past the
        // atlas edge — padding). With a wrong stride the interleaved floats
        // decode as garbage (huge/non-finite) almost everywhere.
        let mut good = 0usize;
        let mut sampled = 0usize;
        for i in (0..count).step_by(1.max(count / 32)) {
            let b = vertices_offset + i * stride;
            if b + stride > data.len() {
                continue;
            }
            sampled += 1;
            let x = read_f32(data, b);
            let y = read_f32(data, b + 4);
            let u = read_f32(data, b + uv_off);
            let v = read_f32(data, b + uv_off + 4);
            if x.is_finite() && y.is_finite()
                && x.abs() < 100_000.0
                && y.abs() < 100_000.0
                && u.is_finite() && v.is_finite()
                && (-0.05..=1.05).contains(&u)
                && (-0.05..=1.05).contains(&v)
            {
                good += 1;
            }
        }
        if sampled > 0 && good * 10 >= sampled * 9 {
            return Some(stride);
        }
    }
    None
}

fn find_mesh_block(data: &[u8], mdls: usize) -> Option<MeshBlock> {
    let mut offset = MARKER_SIZE;
    while offset + MESH_HEADER_SIZE + 4 < mdls {
        // header: first u32 declares vertex stride, second is vertex_bytes
        if offset + 8 > data.len() {
            break;
        }
        let declared = read_u32(data, offset) as usize;
        let vertex_bytes = read_u32(data, offset + 4) as usize;
        let vertices_offset = offset + MESH_HEADER_SIZE;
        let index_len_offset = vertices_offset + vertex_bytes;

        if vertex_bytes == 0 || index_len_offset + 4 > mdls {
            offset += 1;
            continue;
        }
        let index_bytes = read_u32(data, index_len_offset) as usize;
        let indices_offset = index_len_offset + 4;
        let stride = detect_stride(data, vertices_offset, vertex_bytes, declared);
        if stride.is_none()
            || index_bytes == 0
            || index_bytes % 6 != 0 // u16 * 3 per triangle
            || indices_offset + index_bytes > mdls
        {
            offset += 1;
            continue;
        }
        return Some(MeshBlock {
            header_offset: offset,
            vertex_bytes,
            index_bytes,
            stride: stride.unwrap(),
        });
    }
    None
}

fn read_u32(data: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}

fn read_u16(data: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([data[off], data[off + 1]])
}

fn read_f32(data: &[u8], off: usize) -> f32 {
    f32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]])
}
