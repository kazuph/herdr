//! Changed-rectangle transport for large raw images.
//!
//! A browser pane repaints its whole picture many times a second while only a
//! sliver changes. A viewer that cannot read the server's image files would
//! otherwise receive every frame whole; with a patch it receives only the
//! tiles that changed since the frame it already holds, PNG-encoded, and
//! rebuilds the full frame locally.

use base64::Engine;
use serde::{Deserialize, Serialize};

use crate::protocol::endpoint_wire::{SurfaceGraphicsAssetKey, SurfaceGraphicsFormat};

/// Change detection grid, in pixels.
const TILE: u32 = 64;
/// Send the image whole when this share of its tiles changed.
const WHOLE_FRAME_RATIO: f64 = 0.6;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GraphicsPatch {
    /// The rebuilt image.
    pub key: SurfaceGraphicsAssetKey,
    /// The image the viewer already holds, which the rectangles update.
    pub base: SurfaceGraphicsAssetKey,
    pub rects: Vec<GraphicsPatchRect>,
}

impl GraphicsPatch {
    /// The whole image as one rectangle, for a viewer with no usable base.
    pub(crate) fn whole(key: &SurfaceGraphicsAssetKey, pixels: &[u8]) -> Option<Self> {
        bytes_per_pixel(key.format)?;
        if pixels.len() as u64 != key.data_len {
            return None;
        }
        let png = encode_png(key.format, key.image_width, key.image_height, pixels)?;
        Some(Self {
            key: key.clone(),
            base: key.clone(),
            rects: vec![GraphicsPatchRect {
                x: 0,
                y: 0,
                width: key.image_width,
                height: key.image_height,
                png: base64::engine::general_purpose::STANDARD.encode(png),
            }],
        })
    }

    /// Carries the full image rather than changes to a base.
    pub(crate) fn is_whole(&self) -> bool {
        self.base == self.key
    }
}

/// Images at least this large travel as patches to a viewer that accepts them.
pub(crate) const PATCH_MIN_BYTES: u64 = 512 * 1024;

pub(crate) fn patchable(key: &SurfaceGraphicsAssetKey) -> bool {
    bytes_per_pixel(key.format).is_some() && key.data_len >= PATCH_MIN_BYTES
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GraphicsPatchRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Base64 PNG of the rectangle, in the image's pixel format.
    pub png: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct GraphicsPatchMiss {
    pub key: SurfaceGraphicsAssetKey,
}

fn bytes_per_pixel(format: SurfaceGraphicsFormat) -> Option<usize> {
    match format {
        SurfaceGraphicsFormat::Rgba => Some(4),
        SurfaceGraphicsFormat::Rgb => Some(3),
        SurfaceGraphicsFormat::Png => None,
    }
}

/// Both keys describe raw images of the same shape, so one can patch the other.
pub(crate) fn compatible(base: &SurfaceGraphicsAssetKey, key: &SurfaceGraphicsAssetKey) -> bool {
    base.source == key.source
        && base.format == key.format
        && base.image_width == key.image_width
        && base.image_height == key.image_height
        && base.data_len == key.data_len
        && bytes_per_pixel(key.format).is_some()
}

/// The rectangles of `next` that differ from `base`, PNG-encoded. `None` when
/// the image should be sent whole (shape mismatch or most of it changed).
pub(crate) fn diff(
    base_key: &SurfaceGraphicsAssetKey,
    base: &[u8],
    key: &SurfaceGraphicsAssetKey,
    next: &[u8],
) -> Option<GraphicsPatch> {
    let bpp = bytes_per_pixel(key.format)?;
    if !compatible(base_key, key) || base.len() != next.len() || next.len() as u64 != key.data_len
    {
        return None;
    }
    let (width, height) = (key.image_width, key.image_height);
    let (cols, rows) = (width.div_ceil(TILE), height.div_ceil(TILE));
    let stride = width as usize * bpp;
    let mut changed = vec![false; (cols * rows) as usize];
    let mut changed_count = 0usize;
    for ty in 0..rows {
        let y0 = ty * TILE;
        let y1 = (y0 + TILE).min(height);
        for tx in 0..cols {
            let x0 = (tx * TILE) as usize * bpp;
            let x1 = (((tx + 1) * TILE).min(width)) as usize * bpp;
            let differs = (y0..y1).any(|y| {
                let row = y as usize * stride;
                base[row + x0..row + x1] != next[row + x0..row + x1]
            });
            if differs {
                changed[(ty * cols + tx) as usize] = true;
                changed_count += 1;
            }
        }
    }
    if changed_count as f64 > WHOLE_FRAME_RATIO * changed.len() as f64 {
        return None;
    }
    // Horizontal runs of changed tiles per tile row, then runs with the same
    // span on consecutive rows merge into one rectangle.
    let mut open: Vec<(u32, u32, u32, u32)> = Vec::new(); // (tx0, tx1, ty0, ty1)
    let mut rects_tiles = Vec::new();
    for ty in 0..rows {
        let mut runs = Vec::new();
        let mut tx = 0;
        while tx < cols {
            if changed[(ty * cols + tx) as usize] {
                let start = tx;
                while tx < cols && changed[(ty * cols + tx) as usize] {
                    tx += 1;
                }
                runs.push((start, tx));
            } else {
                tx += 1;
            }
        }
        let mut next_open = Vec::new();
        for run in runs {
            if let Some(pos) = open
                .iter()
                .position(|o| o.0 == run.0 && o.1 == run.1 && o.3 == ty)
            {
                let mut grown = open.remove(pos);
                grown.3 = ty + 1;
                next_open.push(grown);
            } else {
                next_open.push((run.0, run.1, ty, ty + 1));
            }
        }
        rects_tiles.append(&mut open);
        open = next_open;
    }
    rects_tiles.append(&mut open);
    let mut rects = Vec::with_capacity(rects_tiles.len());
    for (tx0, tx1, ty0, ty1) in rects_tiles {
        let x = tx0 * TILE;
        let y = ty0 * TILE;
        let w = (tx1 * TILE).min(width) - x;
        let h = (ty1 * TILE).min(height) - y;
        let mut pixels = Vec::with_capacity(w as usize * h as usize * bpp);
        for row in y..y + h {
            let start = row as usize * stride + x as usize * bpp;
            pixels.extend_from_slice(&next[start..start + w as usize * bpp]);
        }
        let png = encode_png(key.format, w, h, &pixels)?;
        rects.push(GraphicsPatchRect {
            x,
            y,
            width: w,
            height: h,
            png: base64::engine::general_purpose::STANDARD.encode(png),
        });
    }
    Some(GraphicsPatch {
        key: key.clone(),
        base: base_key.clone(),
        rects,
    })
}

/// Rebuild the patched image from the base the viewer holds. `None` when the
/// patch does not fit the base; the caller asks for the image whole.
pub(crate) fn apply(base: Option<&[u8]>, patch: &GraphicsPatch) -> Option<Vec<u8>> {
    let bpp = bytes_per_pixel(patch.key.format)?;
    let mut out = if patch.is_whole() {
        vec![0; usize::try_from(patch.key.data_len).ok()?]
    } else {
        let base = base?;
        if !compatible(&patch.base, &patch.key) || base.len() as u64 != patch.base.data_len {
            return None;
        }
        base.to_vec()
    };
    let (width, height) = (patch.key.image_width, patch.key.image_height);
    let stride = width as usize * bpp;
    if out.len() != stride * height as usize {
        return None;
    }
    for rect in &patch.rects {
        if rect.width == 0
            || rect.height == 0
            || rect.x.checked_add(rect.width)? > width
            || rect.y.checked_add(rect.height)? > height
        {
            return None;
        }
        let png = base64::engine::general_purpose::STANDARD
            .decode(&rect.png)
            .ok()?;
        let pixels = decode_png(&png, patch.key.format, rect.width, rect.height)?;
        let row_bytes = rect.width as usize * bpp;
        for row in 0..rect.height as usize {
            let dst = (rect.y as usize + row) * stride + rect.x as usize * bpp;
            out[dst..dst + row_bytes].copy_from_slice(&pixels[row * row_bytes..(row + 1) * row_bytes]);
        }
    }
    Some(out)
}

/// PNG for one rectangle (or a whole image) of raw pixels.
pub(crate) fn encode_png(
    format: SurfaceGraphicsFormat,
    width: u32,
    height: u32,
    pixels: &[u8],
) -> Option<Vec<u8>> {
    let color = match format {
        SurfaceGraphicsFormat::Rgba => png::ColorType::Rgba,
        SurfaceGraphicsFormat::Rgb => png::ColorType::Rgb,
        SurfaceGraphicsFormat::Png => return None,
    };
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(color);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_compression(png::Compression::Fast);
    encoder.set_filter(png::FilterType::Sub);
    let mut writer = encoder.write_header().ok()?;
    writer.write_image_data(pixels).ok()?;
    writer.finish().ok()?;
    Some(out)
}

fn decode_png(
    png: &[u8],
    format: SurfaceGraphicsFormat,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let decoder = png::Decoder::new(png);
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buf).ok()?;
    let expected = match format {
        SurfaceGraphicsFormat::Rgba => png::ColorType::Rgba,
        SurfaceGraphicsFormat::Rgb => png::ColorType::Rgb,
        SurfaceGraphicsFormat::Png => return None,
    };
    if info.width != width
        || info.height != height
        || info.color_type != expected
        || info.bit_depth != png::BitDepth::Eight
    {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::endpoint_wire::{SurfaceGraphicsSource, SurfaceGraphicsTarget};

    fn key(fingerprint: u64, width: u32, height: u32) -> SurfaceGraphicsAssetKey {
        SurfaceGraphicsAssetKey {
            source: SurfaceGraphicsSource::Terminal {
                target: SurfaceGraphicsTarget::Pane {
                    pane_id: "p1".into(),
                },
                image_id: 7,
            },
            image_width: width,
            image_height: height,
            format: SurfaceGraphicsFormat::Rgba,
            data_len: u64::from(width * height * 4),
            data_fingerprint: fingerprint,
        }
    }

    fn image(width: u32, height: u32, seed: u8) -> Vec<u8> {
        (0..width * height * 4)
            .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
            .collect()
    }

    #[test]
    fn small_change_round_trips_through_a_patch() {
        let (w, h) = (300, 200);
        let base = image(w, h, 1);
        let mut next = base.clone();
        // Paint a 20x10 block at (150, 90).
        for y in 90..100 {
            for x in 150..170 {
                let i = (y * w + x) as usize * 4;
                next[i..i + 4].copy_from_slice(&[9, 8, 7, 255]);
            }
        }
        let patch = diff(&key(1, w, h), &base, &key(2, w, h), &next).expect("patch");
        assert!(!patch.rects.is_empty());
        let sent: usize = patch.rects.iter().map(|r| r.png.len()).sum();
        assert!(sent < next.len() / 10, "patch {sent} bytes for {}", next.len());
        assert_eq!(apply(Some(&base), &patch).expect("applies"), next);
    }

    #[test]
    fn whole_image_patch_rebuilds_without_a_base() {
        let (w, h) = (100, 40);
        let pixels = image(w, h, 5);
        let patch = GraphicsPatch::whole(&key(9, w, h), &pixels).expect("whole");
        assert!(patch.is_whole());
        assert_eq!(apply(None, &patch).expect("applies"), pixels);
    }

    #[test]
    fn identical_frames_need_no_rectangles() {
        let (w, h) = (130, 70);
        let base = image(w, h, 3);
        let patch = diff(&key(1, w, h), &base, &key(2, w, h), &base).expect("patch");
        assert!(patch.rects.is_empty());
        assert_eq!(apply(Some(&base), &patch).expect("applies"), base);
    }

    #[test]
    fn mostly_changed_or_reshaped_frames_go_whole() {
        let (w, h) = (128, 128);
        let base = image(w, h, 1);
        let other = image(w, h, 2);
        assert!(diff(&key(1, w, h), &base, &key(2, w, h), &other).is_none());
        let wider = image(w + 64, h, 1);
        assert!(diff(&key(1, w, h), &base, &key(2, w + 64, h), &wider).is_none());
    }

    #[test]
    fn a_patch_for_another_base_is_refused() {
        let (w, h) = (128, 64);
        let base = image(w, h, 1);
        let mut next = base.clone();
        next[0] = next[0].wrapping_add(1);
        let patch = diff(&key(1, w, h), &base, &key(2, w, h), &next).expect("patch");
        assert!(apply(Some(&base[..base.len() - 4]), &patch).is_none());
        assert!(apply(None, &patch).is_none());
    }
}
