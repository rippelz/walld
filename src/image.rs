//! Image decoding (worker-thread side). jpg/png/webp via the `image` crate.

use std::path::Path;

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// Tightly packed RGBA8 pixels, top-down.
    pub rgba: Vec<u8>,
}

pub fn decode_file(path: &Path) -> Result<Image, String> {
    let img = image::ImageReader::open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?
        .with_guessed_format()
        .map_err(|e| format!("guess format: {e}"))?
        .decode()
        .map_err(|e| format!("decode: {e}"))?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return Err(format!("bad dimensions {w}x{h}"));
    }
    let rgba = img.into_rgba8().into_raw();
    Ok(Image { width: w, height: h, rgba })
}
