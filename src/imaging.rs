// SPDX-License-Identifier: GPL-3.0-or-later
//! Looking at rendered frames: a small preview for the UI and a check for frames that came out
//! black, empty or with broken pixels (NaN), which a renderer reports as a success.

use std::io::Cursor;
use std::path::Path;

use anyhow::{Result, bail};
use image::{DynamicImage, ImageFormat};

pub struct Inspection {
    pub thumb_jpeg: Option<Vec<u8>>,
    pub warning: Option<String>,
}

const THUMB_WIDTH: u32 = 320;

/// Reads a frame. Formats the image library can't read get no preview and no warning.
pub fn inspect(path: &Path) -> Result<Inspection> {
    let meta = std::fs::metadata(path)?;
    if meta.len() == 0 {
        bail!("the file is empty");
    }
    let format = match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("exr" | "cxr") => ImageFormat::OpenExr,
        Some("png") => ImageFormat::Png,
        Some("jpg" | "jpeg") => ImageFormat::Jpeg,
        _ => return Ok(Inspection { thumb_jpeg: None, warning: None }),
    };
    let img = image::ImageReader::with_format(std::io::BufReader::new(std::fs::File::open(path)?), format);
    let img = match img.decode() {
        Ok(i) => i,
        Err(e) => bail!("the image cannot be read: {e}"),
    };
    Ok(look_at(&img, format == ImageFormat::OpenExr))
}

pub fn look_at(img: &DynamicImage, linear: bool) -> Inspection {
    let rgb = img.to_rgb32f();
    let (mut max, mut sum, mut bad) = (0f32, 0f64, 0usize);
    let count = (rgb.width() * rgb.height()) as usize;
    for p in rgb.pixels() {
        for c in p.0 {
            if !c.is_finite() {
                bad += 1;
            } else {
                max = max.max(c);
                sum += c as f64;
            }
        }
    }
    let mean = if count > 0 { sum / (count as f64 * 3.0) } else { 0.0 };
    let warning = if bad > 0 {
        Some(format!("{bad} broken pixel values (NaN or infinite)"))
    } else if max <= 0.002 {
        Some("the frame is black".into())
    } else if mean < 0.0005 {
        Some("the frame is almost black".into())
    } else {
        None
    };
    let small = img.thumbnail(THUMB_WIDTH, THUMB_WIDTH * 4);
    let mut small = small.to_rgb32f();
    if linear {
        // A simple view transform so linear EXRs don't look dark: soft roll-off, then sRGB gamma.
        for p in small.pixels_mut() {
            for c in p.0.iter_mut() {
                let v = if c.is_finite() { c.max(0.0) } else { 0.0 };
                *c = (v / (1.0 + v)).powf(1.0 / 2.2).min(1.0);
            }
        }
    }
    let rgb8 = DynamicImage::ImageRgb32F(small).to_rgb8();
    let mut jpeg = Cursor::new(Vec::new());
    let thumb = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80).encode_image(&rgb8).ok().map(|_| jpeg.into_inner());
    Inspection { thumb_jpeg: thumb, warning }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Rgb, RgbImage};

    #[test]
    fn black_frames_are_noticed() {
        let black = DynamicImage::ImageRgb8(RgbImage::new(64, 32));
        let i = look_at(&black, false);
        assert_eq!(i.warning.as_deref(), Some("the frame is black"));
        assert!(i.thumb_jpeg.is_some());
    }

    #[test]
    fn normal_frames_pass() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_fn(64, 32, |x, y| Rgb([x as u8 * 4, y as u8 * 8, 128])));
        assert!(look_at(&img, false).warning.is_none());
    }
}
