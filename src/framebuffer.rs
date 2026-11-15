use std::io::Cursor;

use image::{ImageDecoder, Limits, RgbaImage};

use crate::protocol::{Bounds, Error, FrameUpdate, PixelFormat, Rectangle, Result, ServerInit};

/// Client allocation limit, independent of the server's screen dimensions.
pub const MAX_FRAMEBUFFER: usize = 64 * 1024 * 1024;
/// Aggregate decode/composite budget, including overlapping or corrupt rectangles.
/// Allows four maximum-sized framebuffer passes per update by default.
pub const DEFAULT_FRAME_PIXEL_BUDGET: u64 = MAX_FRAMEBUFFER as u64;

#[derive(Clone, Debug)]
pub struct Framebuffer {
    init: ServerInit,
    /// Native orientation, tightly packed RGBA, always opaque.
    rgba: Vec<u8>,
    content_revision: u64,
    content_updated_at: Option<std::time::Instant>,
    frame_pixel_budget: u64,
}
#[derive(Debug)]
pub struct AppliedFrame {
    pub id: u32,
    pub quality: u8,
    pub applied: usize,
    pub skipped: usize,
    pub dirty: Option<Bounds>,
}
impl Framebuffer {
    pub fn new(init: ServerInit) -> Result<Self> {
        init.pixel_format.validate()?;
        if init.width == 0 || init.height == 0 || init.rotation > 3 {
            return Err(Error::Invalid("screen dimensions or rotation"));
        }
        let len = image_bytes(init.width, init.height)?;
        let mut rgba = vec![0; len];
        for pixel in rgba.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        Ok(Self {
            init,
            rgba,
            content_revision: 0,
            content_updated_at: None,
            frame_pixel_budget: DEFAULT_FRAME_PIXEL_BUDGET,
        })
    }
    /// Rectangles beyond this per-update budget are skipped, not decoded. The
    /// valid prefix is retained and the session still ACKs the update. Zero skips all.
    pub fn set_frame_pixel_budget(&mut self, pixels: u64) {
        self.frame_pixel_budget = pixels;
    }
    pub fn configuration(&self) -> &ServerInit {
        &self.init
    }
    /// Changes only when at least one rectangle was successfully composited.
    pub fn content_revision(&self) -> u64 {
        self.content_revision
    }
    /// Time of the most recent successfully composited pixels, not UI repaint time.
    pub fn content_updated_at(&self) -> Option<std::time::Instant> {
        self.content_updated_at
    }
    pub fn native_rgba(&self) -> &[u8] {
        &self.rgba
    }
    pub fn apply(&mut self, frame: &FrameUpdate<'_>) -> AppliedFrame {
        self.apply_unless(frame, || false)
            .expect("uncancelled frame")
    }
    /// Cancellation is checked between rectangles; a cancelled session need not ACK
    /// the partial frame. This prevents a huge rectangle count from delaying Stop.
    pub(crate) fn apply_unless(
        &mut self,
        frame: &FrameUpdate<'_>,
        cancelled: impl Fn() -> bool,
    ) -> Option<AppliedFrame> {
        let mut report = AppliedFrame {
            id: frame.id,
            quality: frame.quality,
            applied: 0,
            skipped: 0,
            dirty: None,
        };
        let mut remaining = self.frame_pixel_budget;
        for (index, rect) in frame.rectangles.iter().enumerate() {
            if cancelled() {
                return None;
            }
            let b = rect.bounds;
            let valid = b.width > 0
                && b.height > 0
                && u32::from(b.x) + u32::from(b.width) <= u32::from(self.init.width)
                && u32::from(b.y) + u32::from(b.height) <= u32::from(self.init.height);
            let pixels = if valid {
                let cost = u64::from(b.width) * u64::from(b.height);
                if cost > remaining {
                    report.skipped += frame.rectangles.len() - index;
                    break;
                }
                // Charge attempts too: malformed compressed data can be expensive.
                remaining -= cost;
                decode_rectangle(rect, self.init.pixel_format).ok()
            } else {
                None
            };
            let Some(pixels) = pixels else {
                report.skipped += 1;
                continue;
            };
            for row in 0..usize::from(b.height) {
                let dest = ((usize::from(b.y) + row) * usize::from(self.init.width)
                    + usize::from(b.x))
                    * 4;
                let src = row * usize::from(b.width) * 4;
                let len = usize::from(b.width) * 4;
                self.rgba[dest..dest + len].copy_from_slice(&pixels[src..src + len]);
            }
            report.applied += 1;
            report.dirty = Some(match report.dirty {
                None => b,
                Some(old) => {
                    let x = old.x.min(b.x);
                    let y = old.y.min(b.y);
                    let right = (old.x + old.width).max(b.x + b.width);
                    let bottom = (old.y + old.height).max(b.y + b.height);
                    Bounds {
                        x,
                        y,
                        width: right - x,
                        height: bottom - y,
                    }
                }
            });
        }
        if report.dirty.is_some() {
            self.content_revision = self.content_revision.wrapping_add(1);
            self.content_updated_at = Some(std::time::Instant::now());
        }
        Some(report)
    }
    /// Full upright device image, independent of any UI crop, scaling or overlays.
    pub fn upright_snapshot(&self) -> RgbaImage {
        let (w, h) = self.init.upright_size();
        let mut out = RgbaImage::new(u32::from(w), u32::from(h));
        let nw = u32::from(self.init.width);
        let nh = u32::from(self.init.height);
        for y in 0..nh {
            for x in 0..nw {
                let (dx, dy) = match self.init.rotation {
                    0 => (x, y),
                    1 => (nh - 1 - y, x),
                    2 => (nw - 1 - x, nh - 1 - y),
                    3 => (y, nw - 1 - x),
                    _ => unreachable!("rotation validated at construction"),
                };
                let offset = (y as usize * nw as usize + x as usize) * 4;
                out.put_pixel(
                    dx,
                    dy,
                    image::Rgba(self.rgba[offset..offset + 4].try_into().unwrap()),
                );
            }
        }
        out
    }
}
fn image_bytes(w: u16, h: u16) -> Result<usize> {
    usize::from(w)
        .checked_mul(usize::from(h))
        .and_then(|n| n.checked_mul(4))
        .filter(|&n| n <= MAX_FRAMEBUFFER)
        .ok_or(Error::Invalid("decoded image exceeds 64 MiB"))
}

pub fn decode_rectangle(rect: &Rectangle<'_>, format: PixelFormat) -> Result<Vec<u8>> {
    format.validate()?;
    let b = rect.bounds;
    if b.width == 0 || b.height == 0 {
        return Err(Error::Invalid("empty rectangle"));
    }
    let rgba_len = image_bytes(b.width, b.height)?;
    let bpp = format.bytes_per_pixel();
    let expected = rgba_len / 4 * bpp;
    let packed = match rect.codec {
        0 => {
            if rect.encoded.len() != expected {
                return Err(Error::Invalid("raw rectangle length"));
            }
            return packed_to_rgba(rect.encoded, format, rect.drop_bits);
        }
        1 => {
            let mut decoded = vec![0; expected];
            let n = lz4_flex::block::decompress_into(rect.encoded, &mut decoded)
                .map_err(|_| Error::Invalid("LZ4 block"))?;
            if n != expected {
                return Err(Error::Invalid("LZ4 decoded length"));
            }
            decoded
        }
        2 => {
            let mut decoder =
                zstd::bulk::Decompressor::new().map_err(|_| Error::Invalid("Zstd decoder"))?;
            decoder
                .set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(26))
                .map_err(|_| Error::Invalid("Zstd window limit"))?;
            let decoded = decoder
                .decompress(rect.encoded, expected)
                .map_err(|_| Error::Invalid("Zstd rectangle"))?;
            if decoded.len() != expected {
                return Err(Error::Invalid("Zstd decoded length"));
            }
            decoded
        }
        3 => {
            if rect.encoded.len() != bpp {
                return Err(Error::Invalid("solid pixel length"));
            }
            let pixel = packed_to_rgba(rect.encoded, format, rect.drop_bits)?;
            return Ok(pixel.repeat(rgba_len / 4));
        }
        4 => {
            // Support standard 32-bit RGB/BGR layouts. Other JPEG layouts need
            // explicit compatibility testing before they can be enabled.
            if format.0 != [32, 16, 8, 8, 8, 0, 8] && format.0 != [32, 0, 8, 8, 8, 16, 8] {
                return Err(Error::Invalid("JPEG pixel layout is unverified"));
            }
            let mut decoder = image::codecs::jpeg::JpegDecoder::new(Cursor::new(rect.encoded))
                .map_err(|_| Error::Invalid("JPEG header"))?;
            if decoder.dimensions() != (u32::from(b.width), u32::from(b.height)) {
                return Err(Error::Invalid("JPEG dimensions"));
            }
            let mut limits = Limits::default();
            limits.max_image_width = Some(u32::from(b.width));
            limits.max_image_height = Some(u32::from(b.height));
            limits.max_alloc = Some(MAX_FRAMEBUFFER as u64);
            decoder
                .set_limits(limits)
                .map_err(|_| Error::Invalid("JPEG allocation limit"))?;
            let rgb = image::DynamicImage::from_decoder(decoder)
                .map_err(|_| Error::Invalid("JPEG decode"))?
                .to_rgb8();
            let mut packed = Vec::with_capacity(expected);
            for pixel in rgb.pixels() {
                let word = (u32::from(pixel[0]) << format.0[1])
                    | (u32::from(pixel[1]) << format.0[3])
                    | (u32::from(pixel[2]) << format.0[5]);
                packed.extend_from_slice(&word.to_le_bytes());
            }
            packed
        }
        _ => return Err(Error::Invalid("rectangle codec")),
    };
    packed_to_rgba(&packed, format, rect.drop_bits)
}

pub fn expand_quantized(value: u32, bits: u8, drop: u8) -> u32 {
    let drop = drop.min(bits - 1);
    if drop == 0 {
        value
    } else {
        value | (value >> (bits - drop))
    }
}
fn expand_channel(value: u32, bits: u8) -> u8 {
    let mut aligned = value << (8 - bits);
    let mut shift = bits;
    while shift < 8 {
        aligned |= (value << (8 - bits)) >> shift;
        shift += bits;
    }
    aligned as u8
}
pub fn packed_to_rgba(bytes: &[u8], format: PixelFormat, drop: u8) -> Result<Vec<u8>> {
    format.validate()?;
    let bpp = format.bytes_per_pixel();
    if !bytes.len().is_multiple_of(bpp) {
        return Err(Error::Invalid("partial packed pixel"));
    }
    let length = (bytes.len() / bpp)
        .checked_mul(4)
        .filter(|&n| n <= MAX_FRAMEBUFFER)
        .ok_or(Error::Invalid("decoded pixels exceed 64 MiB"))?;
    let channels = [
        (format.0[1], format.0[2]),
        (format.0[3], format.0[4]),
        (format.0[5], format.0[6]),
    ];
    let mut out = Vec::with_capacity(length);
    for bytes in bytes.chunks_exact(bpp) {
        let mut le = [0; 4];
        le[..bpp].copy_from_slice(bytes);
        let mut pixel = u32::from_le_bytes(le);
        for (shift, bits) in channels {
            let value = (pixel >> shift) & ((1 << bits) - 1);
            pixel |= expand_quantized(value, bits, drop) << shift;
        }
        for (shift, bits) in channels {
            out.push(expand_channel((pixel >> shift) & ((1 << bits) - 1), bits));
        }
        out.push(255);
    }
    Ok(out)
}
