//! Minimal Wallpaper Engine .tex reader.
//! Supports common TEXV0005 / TEXI0001 + TEXB0003 FreeImage-embedded images.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum TexError {
    #[error("invalid tex: {0}")]
    Invalid(String),
    #[error("unsupported tex format")]
    Unsupported,
    #[error("image decode: {0}")]
    Image(String),
}

/// Decode a WE .tex blob to RGBA8 + dimensions when possible.
pub fn decode_tex_to_rgba(data: &[u8]) -> Result<(u32, u32, Vec<u8>), TexError> {
    if data.len() < 40 {
        return Err(TexError::Invalid("too short".into()));
    }
    // magic is 9 bytes including null
    let m1 = &data[0..8];
    if m1 != b"TEXV0005" {
        return Err(TexError::Invalid(format!("container {:?}", m1)));
    }
    // skip null at 8
    let m2 = &data[9..17];
    if m2 != b"TEXI0001" {
        return Err(TexError::Invalid(format!("info {:?}", m2)));
    }
    // after TEXI0001 null at 17
    let mut o = 18usize;
    let _format = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    let _flags = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    let _tex_w = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    let _tex_h = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    let width = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    let height = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    o += 4;
    o += 4; // unknown

    // Container magic TEXB000x
    if o + 9 > data.len() {
        return Err(TexError::Invalid("no container".into()));
    }
    let cont = &data[o..o + 8];
    o += 9; // include null

    // Try FreeImage embedded (TEXB0003/0004): often raw image file bytes after header fields
    if cont == b"TEXB0003" || cont == b"TEXB0004" {
        let fif = u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        o += 4;
        if cont == b"TEXB0004" {
            o += 4; // isVideoMp4
        }
        // imageCount
        let _image_count = u32::from_le_bytes(data.get(o..o + 4).unwrap_or(&[0; 4]).try_into().unwrap_or([0; 4]));
        // Heuristic: search for JPEG/PNG magic in remaining
        if let Some(img) = find_and_decode_image(&data[o..]) {
            return Ok(img);
        }
        // FIF: 2=JPEG, 13=PNG, 0=unknown (raw?)
        let _ = fif;
        return Err(TexError::Unsupported);
    }

    if cont == b"TEXB0002" || cont == b"TEXB0001" {
        // raw mip data — try image search anyway
        if let Some(img) = find_and_decode_image(&data[o..]) {
            return Ok(img);
        }
        // raw RGBA?
        let need = (width as usize).saturating_mul(height as usize).saturating_mul(4);
        if width > 0 && height > 0 && data.len() >= o + need {
            return Ok((width, height, data[o..o + need].to_vec()));
        }
        return Err(TexError::Unsupported);
    }

    // Fallback: scan whole file for embedded image
    if let Some(img) = find_and_decode_image(data) {
        return Ok(img);
    }
    let _ = (width, height);
    Err(TexError::Unsupported)
}

fn find_and_decode_image(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    // JPEG
    if let Some(i) = find_subsequence(data, &[0xFF, 0xD8, 0xFF]) {
        if let Ok(img) = image::load_from_memory(&data[i..]) {
            let rgba = img.to_rgba8();
            return Some((rgba.width(), rgba.height(), rgba.into_raw()));
        }
    }
    // PNG
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
