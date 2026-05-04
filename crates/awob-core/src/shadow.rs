//! Soft drop-shadow rendering.
//!
//! Shadows are declared in scene files via the `shadow` attribute on a
//! `rect` element, with CSS `box-shadow`-flavoured syntax:
//!
//! ```kdl
//! rect ... shadow="<offset-x> <offset-y> <blur-radius> <colour>"
//! ```
//!
//! e.g. `shadow="0 8 24 rgba(0,0,0,0.4)"` — soft black drop, no horizontal
//! offset, 8 px down, 24 px blur.
//!
//! ## How it works
//!
//! At first sight of a (rect-size, corner-radius, blur-radius) tuple we
//! rasterise a binary alpha mask of the rounded rect into a buffer that's
//! padded by `blur_radius * 2` on every side, then blur that mask in-place
//! with a separable Gaussian. The result — an alpha-only buffer — is
//! cached by `(w, h, radius, blur)` so subsequent frames just blit it into
//! the destination pixmap, multiplying by the shadow's colour at composite
//! time. Colour isn't part of the cache key because tinting is cheap.
//!
//! Performance: typical OSD shadow is a 360×64 rect, blur=24 → mask is
//! 408×112 bytes (~46 KB). Computing it once per theme change is sub-ms.

use std::collections::HashMap;

use crate::colour::Colour;

#[derive(Debug, Clone, Copy)]
pub struct ShadowSpec {
    pub offset_x: f32,
    pub offset_y: f32,
    pub blur_radius: f32,
    pub colour: Colour,
}

/// Maximum accepted blur radius, in pixels. The mask buffer scales as
/// `(w + 4·blur)²` bytes, so unbounded values from theme files are an
/// OOM vector. 256 px is well past the visible Gaussian falloff for
/// any sensible OSD.
pub const MAX_BLUR_RADIUS: f32 = 256.0;

/// Parse `"<offset-x> <offset-y> <blur-radius> <colour>"`.
///
/// Whitespace inside the colour token (e.g. `rgba(0, 0, 0, 0.4)`) is
/// tolerated by stripping spaces from everything after the first three
/// numeric tokens before handing it to [`Colour::parse`].
///
/// `blur_radius` is clamped to `[0, MAX_BLUR_RADIUS]` to bound mask
/// allocation. NaN is rejected.
pub fn parse(s: &str) -> Option<ShadowSpec> {
    let mut tokens = s.split_whitespace();
    let offset_x: f32 = tokens.next()?.parse().ok()?;
    let offset_y: f32 = tokens.next()?.parse().ok()?;
    let raw_blur: f32 = tokens.next()?.parse().ok()?;
    if raw_blur.is_nan() {
        return None;
    }
    let blur_radius = raw_blur.clamp(0.0, MAX_BLUR_RADIUS);
    let colour_str: String = tokens.collect::<String>();
    let colour = Colour::parse(&colour_str).ok()?;
    Some(ShadowSpec {
        offset_x,
        offset_y,
        blur_radius,
        colour,
    })
}

/// `(width, height, corner_radius, blur_radius)` → cached mask.
type MaskKey = (u32, u32, u32, u32);
/// `(mask_w, mask_h, alpha-bytes)`.
type MaskEntry = (u32, u32, Vec<u8>);

/// Cache of pre-blurred shadow masks. The renderer keeps one of these and
/// hits it once per shadowed rect per render — empty cost on repeat
/// renders of the same theme.
#[derive(Default)]
pub struct ShadowCache {
    masks: HashMap<MaskKey, MaskEntry>,
}

impl ShadowCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_compute(&mut self, w: u32, h: u32, radius: u32, blur: u32) -> (u32, u32, &[u8]) {
        // Belt-and-braces clamp: parse() already caps blur, but ShadowSpec
        // has public fields and a misbehaving caller could feed us a huge
        // value. Mask buffer is (w + 4·blur)² bytes, so this bounds the
        // worst-case allocation even if the parser is bypassed.
        let blur = blur.min(MAX_BLUR_RADIUS as u32);
        let key = (w, h, radius, blur);
        let entry = self
            .masks
            .entry(key)
            .or_insert_with(|| compute_mask(w, h, radius, blur));
        (entry.0, entry.1, &entry.2)
    }
}

/// Padding around the rect inside the mask buffer. `blur_radius * 2` is
/// the visual extent of a Gaussian — beyond that, the kernel weight is
/// effectively zero. We use that as the safe envelope.
pub fn shadow_padding(blur: u32) -> u32 {
    (blur * 2).max(1)
}

fn compute_mask(w: u32, h: u32, radius: u32, blur: u32) -> (u32, u32, Vec<u8>) {
    let pad = shadow_padding(blur);
    let mw = w + 2 * pad;
    let mh = h + 2 * pad;
    let mut buf = vec![0u8; (mw as usize) * (mh as usize)];
    rasterise_rounded_rect_alpha(&mut buf, mw, mh, pad, w, h, radius);
    if blur > 0 {
        let sigma = (blur as f32 / 2.0).max(0.5);
        blur_alpha(&mut buf, mw as usize, mh as usize, sigma);
    }
    (mw, mh, buf)
}

fn rasterise_rounded_rect_alpha(
    buf: &mut [u8],
    mw: u32,
    _mh: u32,
    pad: u32,
    w: u32,
    h: u32,
    radius: u32,
) {
    let x0 = pad as i32;
    let y0 = pad as i32;
    let x1 = (pad + w) as i32;
    let y1 = (pad + h) as i32;
    let r = radius.min(w / 2).min(h / 2) as i32;
    let r2 = (r * r) as f32;
    for y in y0..y1 {
        for x in x0..x1 {
            // Each corner: if we're inside the radius zone for this
            // corner, accept iff (dx,dy) from the *circle centre* is
            // within `r`.
            let in_left = x - x0 < r;
            let in_right = x1 - 1 - x < r;
            let in_top = y - y0 < r;
            let in_bottom = y1 - 1 - y < r;
            let inside = match (in_left, in_top, in_right, in_bottom) {
                (true, true, _, _) => {
                    let dx = (x0 + r - x) as f32;
                    let dy = (y0 + r - y) as f32;
                    dx * dx + dy * dy <= r2
                }
                (_, true, true, _) => {
                    let dx = (x - (x1 - 1 - r)) as f32;
                    let dy = (y0 + r - y) as f32;
                    dx * dx + dy * dy <= r2
                }
                (true, _, _, true) => {
                    let dx = (x0 + r - x) as f32;
                    let dy = (y - (y1 - 1 - r)) as f32;
                    dx * dx + dy * dy <= r2
                }
                (_, _, true, true) => {
                    let dx = (x - (x1 - 1 - r)) as f32;
                    let dy = (y - (y1 - 1 - r)) as f32;
                    dx * dx + dy * dy <= r2
                }
                _ => true,
            };
            if inside {
                buf[y as usize * mw as usize + x as usize] = 255;
            }
        }
    }
}

fn build_kernel(sigma: f32) -> Vec<f32> {
    let radius = (sigma * 3.0).ceil() as i32;
    let size = (radius * 2 + 1) as usize;
    let mut k = vec![0.0_f32; size];
    let mut sum = 0.0;
    for (i, slot) in k.iter_mut().enumerate() {
        let x = (i as i32 - radius) as f32;
        let v = (-(x * x) / (2.0 * sigma * sigma)).exp();
        *slot = v;
        sum += v;
    }
    for v in &mut k {
        *v /= sum;
    }
    k
}

fn blur_alpha(buf: &mut [u8], w: usize, h: usize, sigma: f32) {
    let kernel = build_kernel(sigma);
    let r = (kernel.len() / 2) as i32;
    let mut tmp = vec![0u8; buf.len()];
    // Horizontal pass: buf -> tmp.
    for y in 0..h {
        let row = y * w;
        for x in 0..w {
            let mut acc = 0.0_f32;
            for (i, k) in kernel.iter().enumerate() {
                let sx = (x as i32 + i as i32 - r).clamp(0, w as i32 - 1) as usize;
                acc += buf[row + sx] as f32 * k;
            }
            tmp[row + x] = acc.clamp(0.0, 255.0) as u8;
        }
    }
    // Vertical pass: tmp -> buf.
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0_f32;
            for (i, k) in kernel.iter().enumerate() {
                let sy = (y as i32 + i as i32 - r).clamp(0, h as i32 - 1) as usize;
                acc += tmp[sy * w + x] as f32 * k;
            }
            buf[y * w + x] = acc.clamp(0.0, 255.0) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple() {
        let s = parse("0 8 24 rgba(0,0,0,0.4)").unwrap();
        assert_eq!(s.offset_x, 0.0);
        assert_eq!(s.offset_y, 8.0);
        assert_eq!(s.blur_radius, 24.0);
        assert_eq!(s.colour.r, 0);
        assert!(s.colour.a > 0);
    }

    #[test]
    fn parse_tolerates_whitespace_in_rgba() {
        let s = parse("4 4 12 rgba(0, 0, 0, 0.5)").unwrap();
        assert_eq!(s.blur_radius, 12.0);
    }

    #[test]
    fn parse_hex_colour() {
        let s = parse("0 4 8 #112233").unwrap();
        assert_eq!(s.colour.r, 0x11);
        assert_eq!(s.colour.g, 0x22);
        assert_eq!(s.colour.b, 0x33);
    }

    #[test]
    fn cache_returns_same_dimensions_on_hit() {
        let mut cache = ShadowCache::new();
        let (mw1, mh1, _) = cache.get_or_compute(50, 30, 8, 16);
        let (mw2, mh2, _) = cache.get_or_compute(50, 30, 8, 16);
        assert_eq!(mw1, mw2);
        assert_eq!(mh1, mh2);
        assert_eq!(mw1, 50 + 2 * shadow_padding(16));
        assert_eq!(mh1, 30 + 2 * shadow_padding(16));
    }

    #[test]
    fn parse_clamps_huge_blur() {
        let s = parse("0 0 1000000 rgba(0,0,0,0.5)").unwrap();
        assert_eq!(s.blur_radius, MAX_BLUR_RADIUS);
    }

    #[test]
    fn parse_rejects_nan_blur() {
        assert!(parse("0 0 NaN rgba(0,0,0,0.5)").is_none());
    }

    #[test]
    fn parse_rejects_negative_blur_via_clamp() {
        let s = parse("0 0 -50 rgba(0,0,0,0.5)").unwrap();
        assert_eq!(s.blur_radius, 0.0);
    }

    #[test]
    fn mask_has_full_alpha_at_centre_when_unblurred() {
        let mut cache = ShadowCache::new();
        let (mw, _, mask) = cache.get_or_compute(40, 40, 0, 0);
        let pad = shadow_padding(0);
        let cx = (pad + 20) as usize;
        let cy = (pad + 20) as usize;
        assert_eq!(mask[cy * mw as usize + cx], 255);
    }
}
