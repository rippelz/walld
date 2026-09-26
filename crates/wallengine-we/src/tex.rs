//! Full Wallpaper Engine `.tex` decoder (TEXV0005 / TEXI0001 / TEXB000x).
//!
//! Supports LZ4-compressed mipmaps and pixel formats:
//! ARGB8888, RGB888, RGB565, DXT1/3/5, RG88, R8. FreeImage-embedded
//! JPEG/PNG/etc. are decoded via the `image` crate. Always returns RGBA8.
//! Video textures (isVideoMp4 / VIDEO flag) decode a poster frame and expose
//! the raw MP4 path for continuous playback by the host.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TexError {
    #[error("invalid tex: {0}")]
    Invalid(String),
    #[error("unsupported tex format {0}")]
    Unsupported(String),
    #[error("lz4: {0}")]
    Lz4(String),
    #[error("image decode: {0}")]
    Image(String),
}

/// Texture pixel format (matches WE / linux-wallpaperengine enum values).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum TexFormat {
    Argb8888 = 0,
    Rgb888 = 1,
    Rgb565 = 2,
    Dxt5 = 4,
    Dxt3 = 6,
    Dxt1 = 7,
    Rg88 = 8,
    R8 = 9,
    Rg1616f = 10,
    R16f = 11,
    Bc7 = 12,
    Rgba1010102 = 13,
    Rgba16161616f = 14,
    Rgb161616f = 15,
    Unknown = 0xFFFF_FFFF,
}

impl TexFormat {
    fn from_u32(v: u32) -> Self {
        match v {
            0 => Self::Argb8888,
            1 => Self::Rgb888,
            2 => Self::Rgb565,
            4 => Self::Dxt5,
            6 => Self::Dxt3,
            7 => Self::Dxt1,
            8 => Self::Rg88,
            9 => Self::R8,
            10 => Self::Rg1616f,
            11 => Self::R16f,
            12 => Self::Bc7,
            13 => Self::Rgba1010102,
            14 => Self::Rgba16161616f,
            15 => Self::Rgb161616f,
            _ => Self::Unknown,
        }
    }
}

/// Flags (subset of WE TextureFlags).
pub mod flags {
    pub const NO_INTERPOLATION: u32 = 1;
    pub const CLAMP_UVS: u32 = 2;
    pub const IS_GIF: u32 = 4;
    pub const CLAMP_UVS_BORDER: u32 = 8;
    pub const VIDEO: u32 = 32;
    pub const ALPHA_CHANNEL_PRIORITY: u32 = 524_288;
}

/// Decoded WE texture ready for GPU upload as RGBA8.
#[derive(Debug, Clone)]
pub struct DecodedTex {
    /// GPU buffer width (mip / decoded image width).
    pub width: u32,
    pub height: u32,
    /// Logical content size from TEX header (may be smaller than buffer — NPOT padding).
    pub content_width: u32,
    pub content_height: u32,
    /// Texture (power-of-two) width in memory when known.
    pub texture_width: u32,
    pub texture_height: u32,
    pub format: TexFormat,
    pub flags: u32,
    /// FreeImage format id, or `None` if raw pixel data.
    pub free_image: Option<i32>,
    /// RGBA8 pixels (width * height * 4).
    pub rgba: Vec<u8>,
    /// Spritesheet frames (TEXS): UV rects (u0, v0, du, dv) in buffer space,
    /// in frame order. Empty when the texture is not animated.
    pub frames: Vec<[f32; 4]>,
    /// Per-frame display time in seconds (same order as `frames`).
    pub frame_times: Vec<f32>,
    /// When this `.tex` embeds an MP4, path to the extracted file for continuous
    /// playback. `rgba` is only the first-frame poster until the host streams it.
    pub video_path: Option<PathBuf>,
}

impl DecodedTex {
    /// UV scale for WE shaders: sample only the content rect inside a padded buffer.
    /// Matches LWE `realWidth/textureWidth` texcoord limits.
    pub fn content_uv_scale(&self) -> (f32, f32) {
        let bw = self.width.max(1) as f32;
        let bh = self.height.max(1) as f32;
        (
            (self.content_width.max(1) as f32 / bw).clamp(0.0, 1.0),
            (self.content_height.max(1) as f32 / bh).clamp(0.0, 1.0),
        )
    }
}

/// Decode a WE `.tex` blob to RGBA8 (first image, top mipmap).
pub fn decode_tex_to_rgba(data: &[u8]) -> Result<(u32, u32, Vec<u8>), TexError> {
    let t = decode_tex(data)?;
    Ok((t.width, t.height, t.rgba))
}

/// Full decode with metadata.
pub fn decode_tex(data: &[u8]) -> Result<DecodedTex, TexError> {
    let mut r = Reader::new(data);
    let m1 = r.bytes(9)?;
    if &m1[..8] != b"TEXV0005" {
        return Err(TexError::Invalid(format!("container {:?}", &m1[..8.min(m1.len())])));
    }
    let m2 = r.bytes(9)?;
    if &m2[..8] != b"TEXI0001" {
        return Err(TexError::Invalid(format!("info {:?}", &m2[..8.min(m2.len())])));
    }

    let format = TexFormat::from_u32(r.u32()?);
    let flags = r.u32()?;
    let texture_width = r.u32()?;
    let texture_height = r.u32()?;
    let width = r.u32()?;
    let height = r.u32()?;
    let _unk = r.u32()?;

    let cont = r.bytes(9)?;
    let cont8 = &cont[..8];
    let image_count = r.u32()?;
    if image_count == 0 {
        return Err(TexError::Invalid("imageCount=0".into()));
    }

    let mut free_image: i32 = -1; // FIF_UNKNOWN
    let mut is_video_mp4 = false;
    // LWE demotes non-mp4 TEXB0004 to TEXB0003 for mipmap layout.
    let mut mip_layout_b4 = false;
    // TEXB0001 stores only (width, height, size) per mip — no compression
    // fields. Later containers (0002+) add compression + size pair.
    let mut simple_mip = false;

    match cont8 {
        b"TEXB0004" => {
            free_image = r.u32()? as i32;
            is_video_mp4 = r.u32()? == 1;
            if free_image == -1 && is_video_mp4 {
                free_image = 35; // treat as video container
            }
            mip_layout_b4 = is_video_mp4;
        }
        b"TEXB0003" => {
            free_image = r.u32()? as i32;
        }
        b"TEXB0002" => {}
        b"TEXB0001" => {
            simple_mip = true;
        }
        _ => {
            // Fallback: scan for embedded image
            if let Some(img) = find_and_decode_image(data) {
                return Ok(DecodedTex {
                    width: img.0,
                    height: img.1,
                    content_width: img.0,
                    content_height: img.1,
                    texture_width: img.0,
                    texture_height: img.1,
                    format,
                    flags,
                    free_image: None,
                    rgba: img.2,
                    frames: Vec::new(),
                    frame_times: Vec::new(),
                    video_path: None,
                });
            }
            return Err(TexError::Invalid(format!(
                "unknown container {:?}",
                String::from_utf8_lossy(cont8)
            )));
        }
    }

    // Decode every image's top mipmap. Multi-image GIFs (e.g. Ranni) store
    // several 8K atlases; TEXS frame.number selects which image a frame uses.
    // Only loading image 0 makes later frames re-sample the first atlas → a
    // ~2s visual loop of 25 cells instead of the full 112-frame animation.
    let mut images: Vec<(u32, u32, Vec<u8>)> = Vec::with_capacity(image_count as usize);
    for img_i in 0..image_count {
        let mipmap_count = r.u32()?;
        if mipmap_count == 0 {
            return Err(TexError::Invalid(format!("image {img_i}: no mipmaps")));
        }
        let mip = parse_mipmap(&mut r, mip_layout_b4 && img_i == 0, simple_mip)?;
        // Skip remaining mips of this image.
        for _ in 1..mipmap_count {
            let _ = parse_mipmap(&mut r, mip_layout_b4 && img_i == 0, simple_mip)?;
        }
        let mw = if mip.width > 0 { mip.width } else { width };
        let mh = if mip.height > 0 { mip.height } else { height };
        let raw = mip.data;

        if img_i == 0 && (is_video_mp4 || flags & flags::VIDEO != 0) {
            let video_path = cache_video_mp4(&raw);
            let (vw, vh, rgba) = decode_mp4_first_frame(&raw)
                .ok_or_else(|| TexError::Unsupported("embedded mp4 texture".into()))?;
            return Ok(DecodedTex {
                width: vw,
                height: vh,
                content_width: if width > 0 { width.min(vw) } else { vw },
                content_height: if height > 0 { height.min(vh) } else { vh },
                texture_width: vw,
                texture_height: vh,
                format,
                flags,
                free_image: None,
                rgba,
                frames: Vec::new(),
                frame_times: Vec::new(),
                video_path,
            });
        }

        let (ow, oh, rgba) = if free_image != -1 {
            let img = image::load_from_memory(&raw)
                .map_err(|e| TexError::Image(e.to_string()))?
                .to_rgba8();
            (img.width(), img.height(), img.into_raw())
        } else {
            let rgba = raw_to_rgba(format, flags, mw, mh, &raw)?;
            (mw, mh, rgba)
        };
        images.push((ow, oh, rgba));
    }

    if images.is_empty() {
        return Err(TexError::Invalid("no images decoded".into()));
    }

    let raw_frames = parse_texs_raw(data);
    // Multi-image animated: pack every TEXS frame's cell into one sequential
    // atlas so the existing frame_uv() draw path keeps working.
    if images.len() > 1 && !raw_frames.is_empty() {
        if let Some(packed) = pack_texs_frames(&images, &raw_frames) {
            log::info!(
                "tex multi-image GIF: {} images → packed {} frames into {}x{}",
                images.len(),
                packed.frames.len(),
                packed.width,
                packed.height
            );
            return Ok(packed);
        }
    }

    let (out_w, out_h, rgba) = images.into_iter().next().unwrap();
    let content_w = if free_image != -1 {
        out_w
    } else if width > 0 {
        width.min(out_w)
    } else {
        out_w
    };
    let content_h = if free_image != -1 {
        out_h
    } else if height > 0 {
        height.min(out_h)
    } else {
        out_h
    };
    let (frames, frame_times) = if raw_frames.is_empty() {
        (Vec::new(), Vec::new())
    } else {
        // Single-image: only frames that target image 0 (or any if all same).
        let bw = out_w.max(1) as f32;
        let bh = out_h.max(1) as f32;
        let mut frames = Vec::new();
        let mut times = Vec::new();
        for f in &raw_frames {
            if f.image > 0 {
                continue;
            }
            frames.push([
                f.x / bw,
                f.y / bh,
                (f.w / bw).clamp(0.0, 1.0),
                (f.h / bh).clamp(0.0, 1.0),
            ]);
            times.push(f.time);
        }
        if frames.is_empty() {
            // Fall back to all frames mapped onto image 0 (legacy).
            for f in &raw_frames {
                frames.push([
                    f.x / bw,
                    f.y / bh,
                    (f.w / bw).clamp(0.0, 1.0),
                    (f.h / bh).clamp(0.0, 1.0),
                ]);
                times.push(f.time);
            }
        }
        (frames, times)
    };

    Ok(DecodedTex {
        width: out_w,
        height: out_h,
        content_width: content_w,
        content_height: content_h,
        frames,
        frame_times,
        texture_width: if texture_width > 0 {
            texture_width
        } else {
            out_w
        },
        texture_height: if texture_height > 0 {
            texture_height
        } else {
            out_h
        },
        format,
        flags,
        free_image: if free_image == -1 {
            None
        } else {
            Some(free_image)
        },
        rgba,
        video_path: None,
    })
}

/// One TEXS frame before UV conversion.
struct RawTexsFrame {
    /// Which image in a multi-image TEX this frame samples (`number` field).
    image: u32,
    time: f32,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

/// Pack multi-image TEXS frames into a single atlas (one cell per frame, in
/// order). Avoids binding multiple 8K textures while preserving full playback.
fn pack_texs_frames(
    images: &[(u32, u32, Vec<u8>)],
    raw: &[RawTexsFrame],
) -> Option<DecodedTex> {
    if raw.is_empty() || images.is_empty() {
        return None;
    }
    // Cell size from the first frame (typically gifWidth×gifHeight).
    let mut cell_w = raw[0].w.round().max(1.0) as u32;
    let mut cell_h = raw[0].h.round().max(1.0) as u32;
    let n = raw.len() as u32;
    let cols = (n as f32).sqrt().ceil().max(1.0) as u32;
    let rows = ((n + cols - 1) / cols).max(1);
    // Stay under common GLES max texture size (16384).
    const MAX_TEX: u32 = 16384;
    let need = (cols * cell_w).max(rows * cell_h);
    if need > MAX_TEX {
        let scale = MAX_TEX as f32 / need as f32;
        cell_w = ((cell_w as f32 * scale).floor() as u32).max(2) & !1;
        cell_h = ((cell_h as f32 * scale).floor() as u32).max(2) & !1;
    }
    let atlas_w = cols * cell_w;
    let atlas_h = rows * cell_h;

    let mut atlas = vec![0u8; (atlas_w as usize) * (atlas_h as usize) * 4];
    let mut frames = Vec::with_capacity(raw.len());
    let mut times = Vec::with_capacity(raw.len());

    for (i, f) in raw.iter().enumerate() {
        let col = (i as u32) % cols;
        let row = (i as u32) / cols;
        let dx = col * cell_w;
        let dy = row * cell_h;
        let img_i = f.image as usize;
        if img_i >= images.len() {
            continue;
        }
        let (iw, ih, src) = &images[img_i];
        blit_rgba(
            src,
            *iw,
            *ih,
            f.x.round().max(0.0) as u32,
            f.y.round().max(0.0) as u32,
            f.w.round().max(1.0) as u32,
            f.h.round().max(1.0) as u32,
            &mut atlas,
            atlas_w,
            atlas_h,
            dx,
            dy,
            cell_w,
            cell_h,
        );
        let aw = atlas_w as f32;
        let ah = atlas_h as f32;
        frames.push([
            dx as f32 / aw,
            dy as f32 / ah,
            cell_w as f32 / aw,
            cell_h as f32 / ah,
        ]);
        times.push(if f.time > 0.0 { f.time } else { 1.0 / 30.0 });
    }
    if frames.is_empty() {
        return None;
    }
    Some(DecodedTex {
        width: atlas_w,
        height: atlas_h,
        content_width: atlas_w,
        content_height: atlas_h,
        texture_width: atlas_w,
        texture_height: atlas_h,
        format: TexFormat::Argb8888,
        flags: 0,
        free_image: None,
        rgba: atlas,
        frames,
        frame_times: times,
        video_path: None,
    })
}

/// Copy (and optionally nearest-scale) a rectangle from src into dst.
fn blit_rgba(
    src: &[u8],
    sw: u32,
    sh: u32,
    sx: u32,
    sy: u32,
    src_w: u32,
    src_h: u32,
    dst: &mut [u8],
    dw: u32,
    dh: u32,
    dx: u32,
    dy: u32,
    dst_w: u32,
    dst_h: u32,
) {
    let sw = sw.max(1) as usize;
    let sh = sh.max(1) as usize;
    let dw = dw.max(1) as usize;
    for row in 0..dst_h {
        let syi = sy + (row as u64 * src_h as u64 / dst_h as u64) as u32;
        if syi >= sh as u32 {
            continue;
        }
        let dyi = dy + row;
        if dyi >= dh {
            break;
        }
        for col in 0..dst_w {
            let sxi = sx + (col as u64 * src_w as u64 / dst_w as u64) as u32;
            if sxi >= sw as u32 {
                continue;
            }
            let dxi = dx + col;
            if dxi >= dw as u32 {
                break;
            }
            let si = ((syi as usize) * sw + sxi as usize) * 4;
            let di = ((dyi as usize) * dw + dxi as usize) * 4;
            if si + 4 <= src.len() && di + 4 <= dst.len() {
                dst[di..di + 4].copy_from_slice(&src[si..si + 4]);
            }
        }
    }
}

/// Persist an embedded MP4 under the wallengine cache and return its path.
/// Content-addressed so reloads reuse the file instead of rewriting ~40MB blobs.
fn cache_video_mp4(mp4: &[u8]) -> Option<PathBuf> {
    if mp4.is_empty() {
        return None;
    }
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a 64
    for &b in mp4 {
        h ^= b as u64;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    let dir = crate::we_cache_dir().join("video_tex");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{h:016x}.mp4"));
    if !path.is_file() {
        // Write via temp + rename so a crashed write can't leave a half file
        // that we would skip forever under the is_file short-circuit.
        let tmp = dir.join(format!("{h:016x}.mp4.part"));
        std::fs::write(&tmp, mp4).ok()?;
        std::fs::rename(&tmp, &path).ok()?;
    }
    Some(path)
}

struct Mipmap {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

/// Decode the first frame of an embedded mp4 blob via the system ffmpeg
/// (same backend walld's video mode uses). Returns (w, h, rgba8).
/// Goes through a temp file: piping both ends deadlocks on large frames.
fn decode_mp4_first_frame(mp4: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if std::env::var("WALLD_TEX_DEBUG").is_ok() {
        let _ = std::fs::write("/tmp/walld-tex-debug.mp4", mp4);
    }
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("walld-tex-{}.mp4", std::process::id()));
    std::fs::write(&tmp, mp4).ok()?;
    let probe = std::process::Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v:0",
            "-show_entries", "stream=width,height",
            "-of", "csv=p=0:s=x",
        ])
        .arg(&tmp)
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&probe.stdout);
    let mut it = s.trim().split('x');
    let w = it.next()?.parse::<u32>().ok()?;
    let h = it.next()?.parse::<u32>().ok()?;
    if w == 0 || h == 0 {
        let _ = std::fs::remove_file(&tmp);
        return None;
    }
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-v", "error",
            "-i",
        ])
        .arg(&tmp)
        .args([
            "-frames:v", "1",
            "-f", "rawvideo",
            "-pix_fmt", "rgba",
            "pipe:1",
        ])
        .output()
        .ok()?;
    let _ = std::fs::remove_file(&tmp);
    if !out.status.success() || out.stdout.len() < (w as usize) * (h as usize) * 4 {
        return None;
    }
    Some((w, h, out.stdout))
}

/// Parse the TEXS animation section.
///
/// Layout (LWE TextureParser): `TEXS000x\0`, u32 frameCount, [V3: gifW, gifH],
/// then per frame (32 bytes):
///   V1:    u32 number, f32 frametime, u32 x, y, w, _, _, h
///   V2/V3: u32 number, f32 frametime, f32 x, y, w, w2, h2, h
///
/// `number` is the **image index** in multi-image TEXes (GIF atlases), not the
/// sequential frame index. Pixel rects are in that image's coordinate space.
fn parse_texs_raw(data: &[u8]) -> Vec<RawTexsFrame> {
    let mut pos = 0usize;
    while pos + 9 <= data.len() {
        if &data[pos..pos + 6] != b"TEXS00" {
            pos += 1;
            continue;
        }
        let version = data[pos + 7];
        if version != b'1' && version != b'2' && version != b'3' {
            pos += 1;
            continue;
        }
        if pos + 13 > data.len() {
            return Vec::new();
        }
        let frame_count =
            u32::from_le_bytes(data[pos + 9..pos + 13].try_into().unwrap()) as usize;
        let mut off = pos + 13;
        if version == b'3' {
            off += 8; // gifWidth, gifHeight
        }
        if frame_count == 0 || frame_count > 4096 {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(frame_count);
        for _ in 0..frame_count {
            if off + 32 > data.len() {
                return Vec::new();
            }
            let image = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
            let t = f32::from_le_bytes(data[off + 4..off + 8].try_into().unwrap());
            let rd = |o: usize| {
                let bits = u32::from_le_bytes(data[off + o..off + o + 4].try_into().unwrap());
                if version == b'1' {
                    bits as f32
                } else {
                    f32::from_bits(bits)
                }
            };
            let (x, y, w, h) = (rd(8), rd(12), rd(16), rd(28));
            if w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0 {
                out.push(RawTexsFrame {
                    image,
                    time: t,
                    x,
                    y,
                    w,
                    h,
                });
            }
            off += 32;
        }
        return out;
    }
    Vec::new()
}

fn parse_mipmap(
    r: &mut Reader<'_>,
    layout_b4: bool,
    simple_mip: bool,
) -> Result<Mipmap, TexError> {
    if layout_b4 {
        let _a = r.u32()?;
        let _b = r.u32()?;
        let _json = r.cstring()?;
        let _c = r.u32()?;
    }
    let width = r.u32()?;
    let height = r.u32()?;

    // TEXB0001: width, height, size (u32), raw bytes — no compression header.
    // Older workshop packs (classic water-flow templates) still ship this.
    if simple_mip {
        let size = r.u32()? as usize;
        if size == 0 || size > 512 * 1024 * 1024 {
            return Err(TexError::Invalid(format!("bad TEXB0001 mip size {size}")));
        }
        let data = r.bytes(size)?.to_vec();
        return Ok(Mipmap {
            width,
            height,
            data,
        });
    }

    // TEXB0002/0003/0004 (non-mp4 uses 0003 layout): compression + sizes
    let compression = r.u32()?;
    let mut uncompressed_size = r.i32()?;
    let compressed_size = r.i32()?;
    if compression == 0 {
        uncompressed_size = compressed_size;
    }
    if uncompressed_size < 0 || compressed_size < 0 {
        return Err(TexError::Invalid("negative mipmap size".into()));
    }
    let data = if compression == 1 {
        let comp = r.bytes(compressed_size as usize)?;
        lz4_flex::decompress(&comp, uncompressed_size as usize)
            .map_err(|e| TexError::Lz4(e.to_string()))?
    } else {
        r.bytes(uncompressed_size as usize)?.to_vec()
    };
    Ok(Mipmap {
        width,
        height,
        data,
    })
}

fn raw_to_rgba(
    format: TexFormat,
    flags: u32,
    w: u32,
    h: u32,
    data: &[u8],
) -> Result<Vec<u8>, TexError> {
    let n = (w as usize).saturating_mul(h as usize);
    if n == 0 {
        return Err(TexError::Invalid("zero size".into()));
    }
    match format {
        TexFormat::Argb8888 => {
            // WE stores BGRA bytes in the ARGB8888 slot.
            if data.len() < n * 4 {
                return Err(TexError::Invalid("ARGB too short".into()));
            }
            let mut out = vec![0u8; n * 4];
            for i in 0..n {
                let b = data[i * 4];
                let g = data[i * 4 + 1];
                let r = data[i * 4 + 2];
                let a = data[i * 4 + 3];
                out[i * 4] = r;
                out[i * 4 + 1] = g;
                out[i * 4 + 2] = b;
                out[i * 4 + 3] = a;
            }
            Ok(out)
        }
        TexFormat::Rgb888 => {
            if data.len() < n * 3 {
                return Err(TexError::Invalid("RGB888 too short".into()));
            }
            let mut out = vec![0u8; n * 4];
            for i in 0..n {
                out[i * 4] = data[i * 3];
                out[i * 4 + 1] = data[i * 3 + 1];
                out[i * 4 + 2] = data[i * 3 + 2];
                out[i * 4 + 3] = 255;
            }
            Ok(out)
        }
        TexFormat::Rgb565 => {
            if data.len() < n * 2 {
                return Err(TexError::Invalid("RGB565 too short".into()));
            }
            let mut out = vec![0u8; n * 4];
            for i in 0..n {
                let p = u16::from_le_bytes([data[i * 2], data[i * 2 + 1]]);
                let r = ((p >> 11) & 0x1F) as u8;
                let g = ((p >> 5) & 0x3F) as u8;
                let b = (p & 0x1F) as u8;
                out[i * 4] = (r << 3) | (r >> 2);
                out[i * 4 + 1] = (g << 2) | (g >> 4);
                out[i * 4 + 2] = (b << 3) | (b >> 2);
                out[i * 4 + 3] = 255;
            }
            Ok(out)
        }
        TexFormat::Rg88 => {
            if data.len() < n * 2 {
                return Err(TexError::Invalid("RG88 too short".into()));
            }
            let alpha_priority = flags & flags::ALPHA_CHANNEL_PRIORITY != 0;
            let mut out = vec![0u8; n * 4];
            for i in 0..n {
                let r = data[i * 2];
                let g = data[i * 2 + 1];
                if alpha_priority {
                    // alpha in G (or R); keep both in RG for flow maps
                    out[i * 4] = r;
                    out[i * 4 + 1] = g;
                    out[i * 4 + 2] = 0;
                    out[i * 4 + 3] = g;
                } else {
                    out[i * 4] = r;
                    out[i * 4 + 1] = g;
                    out[i * 4 + 2] = 0;
                    out[i * 4 + 3] = 255;
                }
            }
            Ok(out)
        }
        TexFormat::R8 => {
            if data.len() < n {
                return Err(TexError::Invalid("R8 too short".into()));
            }
            let mut out = vec![0u8; n * 4];
            for i in 0..n {
                let v = data[i];
                out[i * 4] = v;
                out[i * 4 + 1] = v;
                out[i * 4 + 2] = v;
                out[i * 4 + 3] = if flags & flags::ALPHA_CHANNEL_PRIORITY != 0 {
                    v
                } else {
                    255
                };
            }
            Ok(out)
        }
        TexFormat::Dxt1 => decompress_dxt(w, h, data, DxtMode::Dxt1),
        TexFormat::Dxt3 => decompress_dxt(w, h, data, DxtMode::Dxt3),
        TexFormat::Dxt5 => decompress_dxt(w, h, data, DxtMode::Dxt5),
        other => Err(TexError::Unsupported(format!("{other:?}"))),
    }
}

#[derive(Clone, Copy)]
enum DxtMode {
    Dxt1,
    Dxt3,
    Dxt5,
}

fn decompress_dxt(w: u32, h: u32, data: &[u8], mode: DxtMode) -> Result<Vec<u8>, TexError> {
    let bw = w.div_ceil(4) as usize;
    let bh = h.div_ceil(4) as usize;
    let block_size = match mode {
        DxtMode::Dxt1 => 8,
        DxtMode::Dxt3 | DxtMode::Dxt5 => 16,
    };
    let need = bw * bh * block_size;
    if data.len() < need {
        return Err(TexError::Invalid(format!(
            "DXT too short: {} < {}",
            data.len(),
            need
        )));
    }
    let mut out = vec![0u8; (w as usize) * (h as usize) * 4];
    let mut off = 0usize;
    for by in 0..bh {
        for bx in 0..bw {
            let block = &data[off..off + block_size];
            off += block_size;
            let mut colors = [[0u8; 4]; 16];
            match mode {
                DxtMode::Dxt1 => decode_dxt1_block(block, &mut colors),
                DxtMode::Dxt3 => decode_dxt3_block(block, &mut colors),
                DxtMode::Dxt5 => decode_dxt5_block(block, &mut colors),
            }
            for py in 0..4 {
                for px in 0..4 {
                    let x = bx * 4 + px;
                    let y = by * 4 + py;
                    if x >= w as usize || y >= h as usize {
                        continue;
                    }
                    let i = (y * w as usize + x) * 4;
                    let c = colors[py * 4 + px];
                    out[i] = c[0];
                    out[i + 1] = c[1];
                    out[i + 2] = c[2];
                    out[i + 3] = c[3];
                }
            }
        }
    }
    Ok(out)
}

fn decode_rgb565(c: u16) -> [u8; 3] {
    let r = ((c >> 11) & 0x1F) as u8;
    let g = ((c >> 5) & 0x3F) as u8;
    let b = (c & 0x1F) as u8;
    [
        (r << 3) | (r >> 2),
        (g << 2) | (g >> 4),
        (b << 3) | (b >> 2),
    ]
}

fn decode_dxt1_colors(c0: u16, c1: u16) -> [[u8; 4]; 4] {
    let a = decode_rgb565(c0);
    let b = decode_rgb565(c1);
    let mut cols = [[0u8; 4]; 4];
    cols[0] = [a[0], a[1], a[2], 255];
    cols[1] = [b[0], b[1], b[2], 255];
    if c0 > c1 {
        cols[2] = [
            ((2 * a[0] as u16 + b[0] as u16) / 3) as u8,
            ((2 * a[1] as u16 + b[1] as u16) / 3) as u8,
            ((2 * a[2] as u16 + b[2] as u16) / 3) as u8,
            255,
        ];
        cols[3] = [
            ((a[0] as u16 + 2 * b[0] as u16) / 3) as u8,
            ((a[1] as u16 + 2 * b[1] as u16) / 3) as u8,
            ((a[2] as u16 + 2 * b[2] as u16) / 3) as u8,
            255,
        ];
    } else {
        cols[2] = [
            ((a[0] as u16 + b[0] as u16) / 2) as u8,
            ((a[1] as u16 + b[1] as u16) / 2) as u8,
            ((a[2] as u16 + b[2] as u16) / 2) as u8,
            255,
        ];
        cols[3] = [0, 0, 0, 0];
    }
    cols
}

fn decode_dxt1_block(block: &[u8], out: &mut [[u8; 4]; 16]) {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let cols = decode_dxt1_colors(c0, c1);
    let mut bits = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    for i in 0..16 {
        let idx = (bits & 3) as usize;
        bits >>= 2;
        out[i] = cols[idx];
    }
}

fn decode_dxt3_block(block: &[u8], out: &mut [[u8; 4]; 16]) {
    // alpha: 4 bits per pixel
    let mut alpha = [0u8; 16];
    for i in 0..8 {
        let b = block[i];
        alpha[i * 2] = (b & 0x0F) * 17;
        alpha[i * 2 + 1] = (b >> 4) * 17;
    }
    decode_dxt1_block(&block[8..16], out);
    for i in 0..16 {
        out[i][3] = alpha[i];
    }
}

fn decode_dxt5_block(block: &[u8], out: &mut [[u8; 4]; 16]) {
    let a0 = block[0];
    let a1 = block[1];
    let mut alpha_palette = [0u8; 8];
    alpha_palette[0] = a0;
    alpha_palette[1] = a1;
    if a0 > a1 {
        for i in 1..7 {
            alpha_palette[i + 1] =
                (((7 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 7) as u8;
        }
    } else {
        for i in 1..5 {
            alpha_palette[i + 1] =
                (((5 - i) as u16 * a0 as u16 + i as u16 * a1 as u16) / 5) as u8;
        }
        alpha_palette[6] = 0;
        alpha_palette[7] = 255;
    }
    // 6 bytes of 3-bit indices
    let mut bits: u64 = 0;
    for i in 0..6 {
        bits |= (block[2 + i] as u64) << (8 * i);
    }
    let mut alpha = [0u8; 16];
    for i in 0..16 {
        let idx = ((bits >> (3 * i)) & 7) as usize;
        alpha[i] = alpha_palette[idx];
    }
    decode_dxt1_block(&block[8..16], out);
    for i in 0..16 {
        out[i][3] = alpha[i];
    }
}

fn find_and_decode_image(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if let Some(i) = find_subsequence(data, &[0xFF, 0xD8, 0xFF]) {
        if let Ok(img) = image::load_from_memory(&data[i..]) {
            let rgba = img.to_rgba8();
            return Some((rgba.width(), rgba.height(), rgba.into_raw()));
        }
    }
    if let Some(i) = find_subsequence(data, &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        if let Ok(img) = image::load_from_memory(&data[i..]) {
            let rgba = img.to_rgba8();
            return Some((rgba.width(), rgba.height(), rgba.into_raw()));
        }
    }
    None
}

fn find_subsequence(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn ensure(&self, n: usize) -> Result<(), TexError> {
        if self.pos + n > self.data.len() {
            Err(TexError::Invalid("truncated".into()))
        } else {
            Ok(())
        }
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], TexError> {
        self.ensure(n)?;
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u32(&mut self) -> Result<u32, TexError> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn i32(&mut self) -> Result<i32, TexError> {
        Ok(self.u32()? as i32)
    }
    fn cstring(&mut self) -> Result<String, TexError> {
        let start = self.pos;
        while self.pos < self.data.len() && self.data[self.pos] != 0 {
            self.pos += 1;
        }
        if self.pos >= self.data.len() {
            return Err(TexError::Invalid("unterminated string".into()));
        }
        let s = String::from_utf8_lossy(&self.data[start..self.pos]).into_owned();
        self.pos += 1; // null
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn decode_workshop_samples() {
        let root = PathBuf::from("/tmp/we-inspect");
        if !root.is_dir() {
            return;
        }
        for ent in walkdir(&root) {
            if ent.extension().and_then(|e| e.to_str()) != Some("tex") {
                continue;
            }
            let data = std::fs::read(&ent).unwrap();
            match decode_tex(&data) {
                Ok(t) => {
                    assert_eq!(t.rgba.len(), (t.width * t.height * 4) as usize);
                    eprintln!("ok {} {}x{}", ent.display(), t.width, t.height);
                }
                Err(e) => panic!("{}: {e}", ent.display()),
            }
        }
    }

    /// TEXB0001 mipmaps are (width, height, size) with no compression fields.
    /// Classic water-flow template packs still ship this container; misparsing
    /// it as TEXB0002+ yields "negative mipmap size" and empty scenes.
    #[test]
    fn decode_texb0001_if_present() {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/home/beebit"));
        let base = home.join(".cache/wallengine/we/833227004/materials");
        for name in ["background.tex", "flowmask.tex"] {
            let path = base.join(name);
            if !path.is_file() {
                eprintln!("skip: {path:?} not installed");
                continue;
            }
            let data = std::fs::read(&path).expect("read");
            assert!(
                data.windows(8).any(|w| w == b"TEXB0001"),
                "{name} should be TEXB0001"
            );
            let t = decode_tex(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(t.width, 2048);
            assert_eq!(t.height, 2048);
            assert_eq!(t.content_width, 1920);
            assert_eq!(t.content_height, 1080);
            assert_eq!(t.rgba.len(), (2048 * 2048 * 4) as usize);
            // Content region (top 1080 rows) must not be fully transparent.
            let row_bytes = (t.width * 4) as usize;
            let opaque = t.rgba[..row_bytes * 1080]
                .chunks(4)
                .filter(|p| p[3] > 8)
                .count();
            assert!(opaque > 1000, "{name}: expected opaque content pixels");
        }
    }

    fn walkdir(p: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                let path = e.path();
                if path.is_dir() {
                    out.extend(walkdir(&path));
                } else {
                    out.push(path);
                }
            }
        }
        out
    }
}
