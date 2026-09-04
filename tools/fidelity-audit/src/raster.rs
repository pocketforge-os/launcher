//! Minimal RGBA raster: decode a PNG (via the `png` crate, same pin as
//! `pf-shell`), crop to a component box, and compute per-component perceptual
//! deltas. Used by the color-sample and crop comparators. No `image` crate.

use std::collections::HashMap;
use std::fs::File;
use std::path::Path;

use crate::facts::BBox;

/// A decoded RGBA image, row-major, 4 bytes/pixel.
#[derive(Debug, Clone)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// An integer pixel rectangle clamped to an image.
#[derive(Debug, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Raster {
    /// Decode a PNG file to RGBA. Handles RGB and RGBA color types (padding RGB
    /// alpha to 255), matching the shell's own decode path.
    ///
    /// # Errors
    /// Returns an error on IO or decode failure, or an unsupported color type.
    pub fn load(path: &Path) -> Result<Self, String> {
        let decoder = png::Decoder::new(
            File::open(path).map_err(|e| format!("open png {}: {e}", path.display()))?,
        );
        let mut reader = decoder
            .read_info()
            .map_err(|e| format!("read png info {}: {e}", path.display()))?;
        let mut buf = vec![0; reader.output_buffer_size()];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| format!("decode png {}: {e}", path.display()))?;
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf[..info.buffer_size()].to_vec(),
            png::ColorType::Rgb => buf[..info.buffer_size()]
                .chunks_exact(3)
                .flat_map(|p| [p[0], p[1], p[2], 255])
                .collect(),
            other => {
                return Err(format!(
                    "unsupported png color type {other:?}: {}",
                    path.display()
                ));
            }
        };
        Ok(Self {
            width: info.width,
            height: info.height,
            rgba,
        })
    }

    /// Clamp a mockup-space bbox to an integer pixel rect inside the image.
    #[must_use]
    pub fn clamp_rect(&self, bbox: &BBox) -> Rect {
        let x0 = bbox.x.floor().clamp(0.0, f64::from(self.width));
        let y0 = bbox.y.floor().clamp(0.0, f64::from(self.height));
        let x1 = (bbox.x + bbox.w).ceil().clamp(0.0, f64::from(self.width));
        let y1 = (bbox.y + bbox.h).ceil().clamp(0.0, f64::from(self.height));
        Rect {
            x: x0 as u32,
            y: y0 as u32,
            w: (x1 - x0).max(0.0) as u32,
            h: (y1 - y0).max(0.0) as u32,
        }
    }

    fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * self.width + x) * 4) as usize;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Mean absolute error over RGB channels within a rect, normalised to
    /// `[0, 1]` (0 == identical). Both rasters must share dimensions; the rect is
    /// clamped to the shared bounds.
    #[must_use]
    pub fn crop_mae(&self, other: &Raster, rect: Rect) -> f64 {
        let w = rect.w.min(self.width.saturating_sub(rect.x));
        let h = rect.h.min(self.height.saturating_sub(rect.y));
        if w == 0 || h == 0 {
            return 0.0;
        }
        let mut total = 0u64;
        for dy in 0..h {
            for dx in 0..w {
                let a = self.pixel(rect.x + dx, rect.y + dy);
                let b = other.pixel(rect.x + dx, rect.y + dy);
                for c in 0..3 {
                    total += u64::from(a[c].abs_diff(b[c]));
                }
            }
        }
        let channels = u64::from(w) * u64::from(h) * 3;
        total as f64 / (channels as f64 * 255.0)
    }

    /// The dominant "foreground" color inside a rect: the most frequent color
    /// that is not the crop's background (estimated as the modal color). Returns
    /// `None` for an empty rect. Used by the (opt-in) color comparator.
    #[must_use]
    pub fn dominant_foreground(&self, rect: Rect) -> Option<[u8; 4]> {
        let w = rect.w.min(self.width.saturating_sub(rect.x));
        let h = rect.h.min(self.height.saturating_sub(rect.y));
        if w == 0 || h == 0 {
            return None;
        }
        let mut counts: HashMap<[u8; 4], u32> = HashMap::new();
        for dy in 0..h {
            for dx in 0..w {
                *counts
                    .entry(self.pixel(rect.x + dx, rect.y + dy))
                    .or_default() += 1;
            }
        }
        // The background is the modal color; the foreground is the next most
        // frequent color that differs from it beyond anti-aliasing noise.
        let background = counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .map_or([0, 0, 0, 0], |(c, _)| *c);
        counts
            .iter()
            .filter(|(c, _)| channel_distance(**c, background) > 24)
            .max_by_key(|(_, n)| **n)
            .map(|(c, _)| *c)
            .or(Some(background))
    }

    /// Write a scaled per-pixel RGB delta image for a rect (for the crop
    /// artifact). Pixels outside the rect are transparent.
    ///
    /// # Errors
    /// Returns an error on IO or encode failure.
    pub fn write_delta(&self, other: &Raster, rect: Rect, path: &Path) -> Result<(), String> {
        let mut out = vec![0u8; (self.width * self.height * 4) as usize];
        let w = rect.w.min(self.width.saturating_sub(rect.x));
        let h = rect.h.min(self.height.saturating_sub(rect.y));
        for dy in 0..h {
            for dx in 0..w {
                let px = rect.x + dx;
                let py = rect.y + dy;
                let a = self.pixel(px, py);
                let b = other.pixel(px, py);
                let i = ((py * self.width + px) * 4) as usize;
                for c in 0..3 {
                    out[i + c] = a[c].abs_diff(b[c]).saturating_mul(3);
                }
                out[i + 3] = 255;
            }
        }
        let file =
            File::create(path).map_err(|e| format!("create delta {}: {e}", path.display()))?;
        let mut encoder = png::Encoder::new(file, self.width, self.height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder
            .write_header()
            .and_then(|mut w| w.write_image_data(&out))
            .map_err(|e| format!("encode delta {}: {e}", path.display()))
    }
}

fn channel_distance(a: [u8; 4], b: [u8; 4]) -> u32 {
    (0..3).map(|c| u32::from(a[c].abs_diff(b[c]))).sum()
}
