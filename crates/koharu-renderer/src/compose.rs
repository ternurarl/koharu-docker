//! Pure-CPU src-over compositing for decoded image layers.
//!
//! The vello image atlas silently drops layers whose pixels exceed its
//! capacity, so full-page images are composited here on the CPU while vello
//! only draws text. Placement is scale plus translation in output pixel
//! coordinates; anything outside the canvas is clipped.

#![allow(dead_code)] // temporarily unused until the raster orchestration路 lands

use image::RgbaImage;

use crate::images::DecodedImage;

/// An RGBA8 canvas with a transparent base, composited src-over by hand.
pub(crate) struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    pub(crate) fn new(width: u32, height: u32) -> Self {
        let len = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        Self {
            width,
            height,
            pixels: vec![0; len],
        }
    }

    pub(crate) fn from_rgba(image: RgbaImage) -> Self {
        let (width, height) = image.dimensions();
        Self {
            width,
            height,
            pixels: image.into_raw(),
        }
    }

    pub(crate) fn into_rgba(self) -> RgbaImage {
        RgbaImage::from_raw(self.width, self.height, self.pixels)
            .expect("canvas buffer is exactly width*height*4 bytes")
    }

    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// Composites `src` onto the canvas with scale `(sx, sy)` and translation
    /// `(tx, ty)` in output pixel coordinates, multiplying `opacity` into the
    /// source alpha. Out-of-bounds destination pixels are clipped; out-of-
    /// bounds source samples are skipped. At `sx == sy == 1.0` with integral
    /// translation the mapping is per-pixel exact.
    pub(crate) fn overlay_decoded(
        &mut self,
        src: &DecodedImage,
        sx: f64,
        sy: f64,
        tx: f64,
        ty: f64,
        opacity: f32,
    ) {
        if !sx.is_finite()
            || !sy.is_finite()
            || !tx.is_finite()
            || !ty.is_finite()
            || sx <= 0.0
            || sy <= 0.0
            || !opacity.is_finite()
            || opacity <= 0.0
        {
            return;
        }
        let opacity = opacity.min(1.0);
        let src_width = src.width;
        let src_height = src.height;
        if src_width == 0 || src_height == 0 {
            return;
        }
        let expected = (src_width as usize).saturating_mul(src_height as usize) * 4;
        if src.pixels.len() < expected || self.pixels.len() < expected_canvas_bytes(self) {
            return;
        }
        let pixels = src.pixels.as_ref();

        if sx == 1.0 && sy == 1.0 && tx == tx.floor() && ty == ty.floor() {
            // Integral 1:1 placement: exact per-pixel copy with clipping.
            let origin_x = tx as i64;
            let origin_y = ty as i64;
            let x0 = origin_x.max(0);
            let y0 = origin_y.max(0);
            let x1 = origin_x
                .saturating_add(i64::from(src_width))
                .min(i64::from(self.width));
            let y1 = origin_y
                .saturating_add(i64::from(src_height))
                .min(i64::from(self.height));
            if x0 >= x1 || y0 >= y1 {
                return;
            }
            for y in y0..y1 {
                let src_y = (y - origin_y) as usize;
                for x in x0..x1 {
                    let src_x = (x - origin_x) as usize;
                    let offset = (src_y * src_width as usize + src_x) * 4;
                    blend_pixel(
                        self,
                        x as u32,
                        y as u32,
                        pixels[offset],
                        pixels[offset + 1],
                        pixels[offset + 2],
                        pixels[offset + 3],
                        opacity,
                    );
                }
            }
            return;
        }

        // General path: destination pixels whose center falls inside the
        // placed source rectangle, sampled nearest-neighbor.
        let span_x = sx * f64::from(src_width);
        let span_y = sy * f64::from(src_height);
        let x_start = (tx - 0.5).ceil().max(0.0) as u64;
        let y_start = (ty - 0.5).ceil().max(0.0) as u64;
        let x_end = ((tx + span_x - 0.5).ceil().max(0.0) as u64).min(u64::from(self.width));
        let y_end = ((ty + span_y - 0.5).ceil().max(0.0) as u64).min(u64::from(self.height));
        if x_start >= x_end || y_start >= y_end {
            return;
        }
        for y in y_start..y_end {
            let src_y = ((y as f64 + 0.5 - ty) / sy).floor();
            if !(0.0..f64::from(src_height)).contains(&src_y) {
                continue;
            }
            let src_y = src_y as usize;
            for x in x_start..x_end {
                let src_x = ((x as f64 + 0.5 - tx) / sx).floor();
                if !(0.0..f64::from(src_width)).contains(&src_x) {
                    continue;
                }
                let offset = (src_y * src_width as usize + src_x as usize) * 4;
                blend_pixel(
                    self,
                    x as u32,
                    y as u32,
                    pixels[offset],
                    pixels[offset + 1],
                    pixels[offset + 2],
                    pixels[offset + 3],
                    opacity,
                );
            }
        }
    }

    /// Composites `src` at `(0, 0)` at scale 1, clipping to both images.
    pub(crate) fn overlay_rgba_full(&mut self, src: &RgbaImage, opacity: f32) {
        if !opacity.is_finite() || opacity <= 0.0 {
            return;
        }
        let opacity = opacity.min(1.0);
        let (src_width, src_height) = src.dimensions();
        let width = src_width.min(self.width);
        let height = src_height.min(self.height);
        if width == 0 || height == 0 {
            return;
        }
        let raw = src.as_raw();
        for y in 0..height {
            for x in 0..width {
                let offset = (y as usize * src_width as usize + x as usize) * 4;
                blend_pixel(
                    self,
                    x,
                    y,
                    raw[offset],
                    raw[offset + 1],
                    raw[offset + 2],
                    raw[offset + 3],
                    opacity,
                );
            }
        }
    }
}

fn expected_canvas_bytes(canvas: &Canvas) -> usize {
    (canvas.width as usize).saturating_mul(canvas.height as usize) * 4
}

/// Straight-alpha src-over of one source pixel with `opacity` folded into the
/// source alpha, writing into the canvas at `(x, y)` (already clipped by the
/// caller).
#[inline]
fn blend_pixel(canvas: &mut Canvas, x: u32, y: u32, sr: u8, sg: u8, sb: u8, sa: u8, opacity: f32) {
    let source_alpha = if opacity < 1.0 {
        (f64::from(sa) * f64::from(opacity)).round() as u32
    } else {
        u32::from(sa)
    };
    if source_alpha == 0 {
        return;
    }
    let offset = (y as usize * canvas.width as usize + x as usize) * 4;
    let dst = &mut canvas.pixels[offset..offset + 4];
    if source_alpha == 255 {
        dst[0] = sr;
        dst[1] = sg;
        dst[2] = sb;
        dst[3] = 255;
        return;
    }
    let da = u32::from(dst[3]);
    let inv = 255 - source_alpha;
    let out_alpha = source_alpha + (da * inv + 127) / 255;
    let denominator = 255 * out_alpha;
    let half = denominator / 2;
    let out_r =
        (u32::from(sr) * source_alpha * 255 + u32::from(dst[0]) * da * inv + half) / denominator;
    let out_g =
        (u32::from(sg) * source_alpha * 255 + u32::from(dst[1]) * da * inv + half) / denominator;
    let out_b =
        (u32::from(sb) * source_alpha * 255 + u32::from(dst[2]) * da * inv + half) / denominator;
    dst[0] = out_r.min(255) as u8;
    dst[1] = out_g.min(255) as u8;
    dst[2] = out_b.min(255) as u8;
    dst[3] = out_alpha.min(255) as u8;
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn decoded(width: u32, height: u32, rgba: [u8; 4]) -> DecodedImage {
        DecodedImage {
            width,
            height,
            encoded: Arc::from([0_u8; 1]),
            pixels: rgba
                .repeat((width * height) as usize)
                .into_boxed_slice()
                .into(),
        }
    }

    fn pixel(canvas: &Canvas, x: u32, y: u32) -> [u8; 4] {
        let offset = (y as usize * canvas.width as usize + x as usize) * 4;
        canvas.pixels[offset..offset + 4].try_into().unwrap()
    }

    #[test]
    fn one_to_one_overlay_is_exact() {
        let mut canvas = Canvas::new(2, 2);
        let src = decoded(2, 2, [10, 20, 30, 255]);
        canvas.overlay_decoded(&src, 1.0, 1.0, 0.0, 0.0, 1.0);
        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(pixel(&canvas, x, y), [10, 20, 30, 255]);
            }
        }
        // Exact src-over onto a partially transparent destination pixel.
        let mut canvas =
            Canvas::from_rgba(RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 255, 0])));
        let src = decoded(1, 1, [80, 90, 100, 128]);
        canvas.overlay_decoded(&src, 1.0, 1.0, 0.0, 0.0, 1.0);
        assert_eq!(pixel(&canvas, 0, 0), [80, 90, 100, 128]);
        let round = canvas.into_rgba();
        assert_eq!((round.width(), round.height()), (1, 1));
    }

    #[test]
    fn half_transparent_source_blends_src_over() {
        let mut canvas = Canvas::new(1, 1);
        // Opaque blue destination.
        canvas.pixels.copy_from_slice(&[0, 0, 255, 255]);
        let src = decoded(1, 1, [255, 0, 0, 128]);
        canvas.overlay_decoded(&src, 1.0, 1.0, 0.0, 0.0, 1.0);
        // out_a = 128 + round(255*127/255) = 255;
        // out_r = 255*128*255 / (255*255) = 128; out_b = 255*255*127 / (255*255) = 127.
        assert_eq!(pixel(&canvas, 0, 0), [128, 0, 127, 255]);
    }

    #[test]
    fn opacity_multiplies_source_alpha() {
        let mut canvas = Canvas::new(1, 1);
        let src = decoded(1, 1, [255, 255, 255, 255]);
        canvas.overlay_decoded(&src, 1.0, 1.0, 0.0, 0.0, 0.5);
        // sa_eff = round(255 * 0.5) = 128 over a transparent base.
        assert_eq!(pixel(&canvas, 0, 0), [255, 255, 255, 128]);

        let mut canvas = Canvas::new(1, 1);
        canvas.overlay_decoded(&src, 1.0, 1.0, 0.0, 0.0, 0.0);
        assert_eq!(pixel(&canvas, 0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn scaled_source_is_placed_at_translation() {
        let mut canvas = Canvas::new(4, 4);
        let src = decoded(1, 1, [200, 0, 0, 255]);
        canvas.overlay_decoded(&src, 2.0, 2.0, 1.0, 1.0, 1.0);
        // Dest centers in [1, 3) map into the single source texel.
        for y in 0..4 {
            for x in 0..4 {
                let inside = (1..3).contains(&x) && (1..3).contains(&y);
                let expected = if inside {
                    [200, 0, 0, 255]
                } else {
                    [0, 0, 0, 0]
                };
                assert_eq!(pixel(&canvas, x, y), expected, "at ({x}, {y})");
            }
        }
    }

    #[test]
    fn out_of_bounds_placement_is_clipped() {
        let mut canvas = Canvas::new(4, 4);
        let src = DecodedImage {
            width: 2,
            height: 2,
            encoded: Arc::from([0_u8; 1]),
            pixels: Arc::from(vec![
                255_u8, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 0, 255,
            ]),
        };
        // Shifted one pixel left/up: source spans dest [-1, 1) on each axis,
        // so only dest (0,0) samples an in-bounds source texel (1,1); dest
        // (1,0) samples source x=2 which is out of bounds and skipped.
        canvas.overlay_decoded(&src, 1.0, 1.0, -1.0, -1.0, 1.0);
        assert_eq!(pixel(&canvas, 0, 0), [255, 255, 0, 255]); // src (1,1)
        assert_eq!(pixel(&canvas, 1, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&canvas, 3, 3), [0, 0, 0, 0]);
        // Extending past the bottom-right edge is also clipped, never written
        // outside the buffer.
        let mut canvas = Canvas::new(2, 2);
        let src = decoded(2, 2, [9, 9, 9, 255]);
        canvas.overlay_decoded(&src, 1.0, 1.0, 1.0, 1.0, 1.0);
        assert_eq!(pixel(&canvas, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&canvas, 1, 1), [9, 9, 9, 255]);
    }

    #[test]
    fn overlay_rgba_full_clips_and_blends() {
        let mut canvas = Canvas::new(2, 2);
        let src = RgbaImage::from_pixel(3, 3, image::Rgba([0, 255, 0, 255]));
        canvas.overlay_rgba_full(&src, 1.0);
        for y in 0..2 {
            for x in 0..2 {
                assert_eq!(pixel(&canvas, x, y), [0, 255, 0, 255]);
            }
        }
        assert_eq!((canvas.width(), canvas.height()), (2, 2));
    }
}
