//! Icon resolution + rasterisation.
//!
//! Resolves a value passed as an element's `src` attribute through, in
//! order:
//!
//! 1. `data:image/svg+xml,...` or `data:image/png;base64,...` inline data.
//! 2. An absolute path (`.svg` / `.png`).
//! 3. A freedesktop icon name. Looked up first in the active theme's
//!    `icons/` directory (passed via [`IconResolver::with_theme_dir`]),
//!    then in the system icon themes and legacy pixmaps dirs via
//!    [`paths::find_icon_file`] (preferred theme, then Adwaita, hicolor).
//!
//! Cached rasterisations are keyed by `(input, target_w, target_h)` and
//! evicted by least recent use (64 entries, 4 MiB including source keys).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use tiny_skia::{Pixmap, Transform};

use crate::paths;

const MAX_INLINE_BYTES: usize = 256 * 1024;
const MAX_ON_DISK_BYTES: u64 = 1024 * 1024;
const CACHE_CAP: usize = 64;
const CACHE_BYTES: usize = 4 * 1024 * 1024;

/// Standard freedesktop icon name used for "missing icon" fallbacks. The
/// resolver tries this name through the normal lookup chain (theme dir +
/// system icon themes) before falling back to the embedded SVG, so theme
/// authors can override the fallback by shipping their own
/// `image-missing-symbolic.svg`.
const FALLBACK_ICON_NAME: &str = "image-missing-symbolic";

/// Embedded last-resort fallback. Rendered when neither the requested icon
/// nor `image-missing-symbolic` could be found anywhere on the system.
/// Symbolic so the icon resolver tints it to `$fg`.
const EMBEDDED_FALLBACK_SVG: &[u8] = include_bytes!("../embedded/image-missing-symbolic.svg");

#[derive(Debug, thiserror::Error)]
pub enum IconError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported icon format")]
    UnsupportedFormat,
    #[error("svg: {0}")]
    Svg(String),
    #[error("png: {0}")]
    Png(String),
    #[error("payload exceeds size limit")]
    TooLarge,
    #[error("not found: {0}")]
    NotFound(String),
}

struct CacheEntry {
    key: (String, u32, u32),
    pixmap: Pixmap,
    was_symbolic: bool,
}

impl CacheEntry {
    fn bytes(&self) -> usize {
        icon_bytes(&self.key, &self.pixmap)
    }
}

fn icon_bytes(key: &(String, u32, u32), pixmap: &Pixmap) -> usize {
    key.0
        .capacity()
        .saturating_add(pixmap.data().len())
        .saturating_add(std::mem::size_of::<CacheEntry>())
}

#[derive(Default)]
pub struct IconResolver {
    cache: VecDeque<CacheEntry>,
    bytes: usize,
    theme_dir: Option<PathBuf>,
}

impl IconResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_theme_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.set_theme_dir(dir);
        self
    }

    pub fn set_theme_dir(&mut self, dir: Option<PathBuf>) {
        if self.theme_dir != dir {
            self.theme_dir = dir;
            self.clear_cache();
        }
    }

    /// Resolve and rasterize an icon to the requested size. Returns `None`
    /// if anything fails — callers fall back to a placeholder rect.
    pub fn resolve(&mut self, src: &str, w: u32, h: u32) -> Option<Pixmap> {
        Some(self.resolve_with_meta(src, w, h)?.0)
    }

    /// Same as [`Self::resolve`] but also returns whether the source was
    /// detected as a symbolic icon (path in a `symbolic/` dir or
    /// `-symbolic` suffix). The caller uses this to apply theme-foreground
    /// tinting automatically.
    pub fn resolve_with_meta(&mut self, src: &str, w: u32, h: u32) -> Option<(Pixmap, bool)> {
        if src.is_empty() || crate::limits::pixels(w, h).is_err() {
            return None;
        }
        if let Some(index) = self
            .cache
            .iter()
            .position(|entry| entry.key.0 == src && entry.key.1 == w && entry.key.2 == h)
        {
            let hit = self
                .cache
                .remove(index)
                .expect("index came from this cache");
            let result = (hit.pixmap.clone(), hit.was_symbolic);
            self.cache.push_back(hit);
            return Some(result);
        }
        let (pm, sym) = self.rasterise_with_meta(src, w, h).ok()?;
        let key = (src.to_owned(), w, h);
        // An oversized raster still renders, but do not duplicate it just to
        // discover that it cannot fit the retained cache.
        if icon_bytes(&key, &pm) <= CACHE_BYTES {
            self.insert_cache(key, pm.clone(), sym);
        }
        Some((pm, sym))
    }

    pub(crate) fn clear_cache(&mut self) {
        self.cache = VecDeque::new();
        self.bytes = 0;
    }

    fn insert_cache(&mut self, key: (String, u32, u32), value: Pixmap, was_symbolic: bool) {
        let bytes = icon_bytes(&key, &value);
        if bytes > CACHE_BYTES {
            return;
        }
        while self.bytes + bytes > CACHE_BYTES || self.cache.len() >= CACHE_CAP {
            if let Some(old) = self.cache.pop_front() {
                self.bytes -= old.bytes();
            }
        }
        self.bytes += bytes;
        self.cache.push_back(CacheEntry {
            key,
            pixmap: value,
            was_symbolic,
        });
    }

    fn rasterise_with_meta(&self, src: &str, w: u32, h: u32) -> Result<(Pixmap, bool), IconError> {
        if src.starts_with("data:") {
            // data: URIs aren't tinted automatically — caller's responsibility.
            return self.rasterise_data_uri(src, w, h).map(|p| (p, false));
        }
        if src.starts_with('/') {
            let path = Path::new(src);
            let sym = is_symbolic_path(path);
            return self.rasterise_path(path, w, h).map(|p| (p, sym));
        }
        if let Some(theme_dir) = &self.theme_dir {
            for ext in ["svg", "png"] {
                let p = theme_dir.join("icons").join(format!("{src}.{ext}"));
                if p.exists() {
                    let sym = is_symbolic_path(&p);
                    return self.rasterise_path(&p, w, h).map(|x| (x, sym));
                }
            }
        }
        if let Some(p) = paths::find_icon_file(src, w) {
            let sym = is_symbolic_path(&p);
            return self.rasterise_path(&p, w, h).map(|x| (x, sym));
        }

        // The requested icon couldn't be resolved. Give the theme a chance
        // to supply its own missing-icon glyph by recursing on the
        // canonical name; if that *also* fails, fall through to the
        // embedded SVG. Recursion is bounded — we only ever recurse with
        // `FALLBACK_ICON_NAME`, never deeper.
        if src != FALLBACK_ICON_NAME
            && let Ok((pm, _)) = self.rasterise_with_meta(FALLBACK_ICON_NAME, w, h)
        {
            return Ok((pm, true));
        }
        rasterise_svg(EMBEDDED_FALLBACK_SVG, w, h).map(|p| (p, true))
    }

    fn rasterise_path(&self, path: &Path, w: u32, h: u32) -> Result<Pixmap, IconError> {
        let meta = std::fs::metadata(path)?;
        if meta.len() > MAX_ON_DISK_BYTES {
            return Err(IconError::TooLarge);
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext.to_ascii_lowercase().as_str() {
            "svg" => {
                let bytes = std::fs::read(path)?;
                rasterise_svg(&bytes, w, h)
            }
            "png" => {
                let file = std::fs::File::open(path)?;
                rasterise_png(file, w, h)
            }
            _ => Err(IconError::UnsupportedFormat),
        }
    }

    fn rasterise_data_uri(&self, src: &str, w: u32, h: u32) -> Result<Pixmap, IconError> {
        let comma = src.find(',').ok_or(IconError::UnsupportedFormat)?;
        let header = &src[..comma];
        let payload = &src[comma + 1..];
        let bytes = if header.contains(";base64") {
            base64_decode(payload).map_err(|_| IconError::UnsupportedFormat)?
        } else {
            payload.as_bytes().to_vec()
        };
        if bytes.len() > MAX_INLINE_BYTES {
            return Err(IconError::TooLarge);
        }
        if header.contains("svg") {
            rasterise_svg(&bytes, w, h)
        } else if header.contains("png") {
            rasterise_png(std::io::Cursor::new(bytes), w, h)
        } else {
            Err(IconError::UnsupportedFormat)
        }
    }
}

/// Multiply every pixel's alpha (and RGB, since the input is
/// premultiplied) by `mul`. Used by the renderer's animation engine
/// to apply a per-element alpha modulation to icon pixmaps —
/// `tint_pixmap` only handles the colour replacement, not free
/// scaling, so this is the gap-filler for `pulse=true` on `image`
/// elements.
pub fn scale_pixmap_alpha(pm: &mut Pixmap, mul: f32) {
    let m = mul.clamp(0.0, 1.0);
    if (m - 1.0).abs() < f32::EPSILON {
        return;
    }
    let data = pm.data_mut();
    for px in data.as_chunks_mut::<4>().0 {
        px[0] = ((px[0] as f32) * m) as u8;
        px[1] = ((px[1] as f32) * m) as u8;
        px[2] = ((px[2] as f32) * m) as u8;
        px[3] = ((px[3] as f32) * m) as u8;
    }
}

/// Tint a pre-rasterised pixmap to a flat colour by replacing each pixel's
/// RGB with the given colour while preserving its existing alpha. The result
/// stays premultiplied (input already is).
pub fn tint_pixmap(pm: &mut Pixmap, colour: crate::colour::Colour) {
    let cr = colour.r as u32;
    let cg = colour.g as u32;
    let cb = colour.b as u32;
    let ca = colour.a as u32;
    let data = pm.data_mut();
    for px in data.as_chunks_mut::<4>().0 {
        let src_a = px[3] as u32;
        if src_a == 0 {
            continue;
        }
        // Effective alpha = pre-existing alpha scaled by tint alpha.
        let out_a = (src_a * ca / 255).min(255);
        // Premultiplied output.
        px[0] = (cr * out_a / 255) as u8;
        px[1] = (cg * out_a / 255) as u8;
        px[2] = (cb * out_a / 255) as u8;
        px[3] = out_a as u8;
    }
}

fn is_symbolic_path(p: &Path) -> bool {
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    if stem.ends_with("-symbolic") {
        return true;
    }
    let s = p.to_string_lossy();
    s.contains("/symbolic/")
}

fn rasterise_svg(bytes: &[u8], w: u32, h: u32) -> Result<Pixmap, IconError> {
    crate::limits::pixels(w, h).map_err(|_| IconError::TooLarge)?;
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_data(bytes, &opt).map_err(|e| IconError::Svg(e.to_string()))?;
    let mut pm = Pixmap::new(w, h).ok_or_else(|| IconError::Svg("pixmap alloc".into()))?;
    pm.fill(tiny_skia::Color::TRANSPARENT);
    let svg_size = tree.size();
    let sx = w as f32 / svg_size.width();
    let sy = h as f32 / svg_size.height();
    let scale = sx.min(sy);
    let dx = (w as f32 - svg_size.width() * scale) / 2.0;
    let dy = (h as f32 - svg_size.height() * scale) / 2.0;
    let transform = Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(&tree, transform, &mut pm.as_mut());
    Ok(pm)
}

fn rasterise_png<R: std::io::Read>(reader: R, w: u32, h: u32) -> Result<Pixmap, IconError> {
    crate::limits::pixels(w, h).map_err(|_| IconError::TooLarge)?;
    let decoder = png::Decoder::new_with_limits(
        reader,
        png::Limits {
            bytes: crate::limits::PNG_SCRATCH_BYTES,
        },
    );
    let mut reader = decoder
        .read_info()
        .map_err(|e| IconError::Png(e.to_string()))?;
    crate::limits::pixels(reader.info().width, reader.info().height)
        .map_err(|_| IconError::TooLarge)?;
    let output_bytes = reader.output_buffer_size();
    if output_bytes > crate::limits::MAX_RGBA_BYTES {
        return Err(IconError::TooLarge);
    }
    let mut buf = vec![0; output_bytes];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| IconError::Png(e.to_string()))?;
    let src_w = info.width;
    let src_h = info.height;
    let bytes_per_pixel = match info.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::Grayscale => 1,
        png::ColorType::GrayscaleAlpha => 2,
        _ => return Err(IconError::Png("unsupported color type".into())),
    };
    // Convert + box-scale into a (w, h) RGBA pixmap with nearest-neighbor.
    let mut pm = Pixmap::new(w, h).ok_or_else(|| IconError::Png("pixmap alloc".into()))?;
    let dst = pm.data_mut();
    let stride = w as usize * 4;
    for ty in 0..h {
        let sy = ((ty as f64 / h.max(1) as f64) * src_h as f64) as u32;
        let sy = sy.min(src_h - 1);
        for tx in 0..w {
            let sx = ((tx as f64 / w.max(1) as f64) * src_w as f64) as u32;
            let sx = sx.min(src_w - 1);
            let src_idx = (sy * src_w + sx) as usize * bytes_per_pixel;
            let dst_idx = ty as usize * stride + tx as usize * 4;
            let (r, g, b, a) = match info.color_type {
                png::ColorType::Rgba => (
                    buf[src_idx],
                    buf[src_idx + 1],
                    buf[src_idx + 2],
                    buf[src_idx + 3],
                ),
                png::ColorType::Rgb => (buf[src_idx], buf[src_idx + 1], buf[src_idx + 2], 255),
                png::ColorType::Grayscale => {
                    let g = buf[src_idx];
                    (g, g, g, 255)
                }
                png::ColorType::GrayscaleAlpha => {
                    let g = buf[src_idx];
                    (g, g, g, buf[src_idx + 1])
                }
                _ => unreachable!(),
            };
            // Premultiply.
            let a = a as u32;
            dst[dst_idx] = (r as u32 * a / 255) as u8;
            dst[dst_idx + 1] = (g as u32 * a / 255) as u8;
            dst[dst_idx + 2] = (b as u32 * a / 255) as u8;
            dst[dst_idx + 3] = a as u8;
        }
    }
    Ok(pm)
}

fn base64_decode(s: &str) -> Result<Vec<u8>, ()> {
    // Tiny base64 decoder. Avoids pulling another crate just for icon URIs.
    const TBL: [i8; 256] = make_b64_table();
    let s = s.trim();
    let bytes = s.as_bytes();
    let len = bytes.len();
    if !len.is_multiple_of(4) {
        return Err(());
    }
    let mut out = Vec::with_capacity(len / 4 * 3);
    let mut i = 0;
    while i < len {
        let chunk: [i8; 4] = [
            TBL[bytes[i] as usize],
            TBL[bytes[i + 1] as usize],
            if bytes[i + 2] == b'=' {
                0
            } else {
                TBL[bytes[i + 2] as usize]
            },
            if bytes[i + 3] == b'=' {
                0
            } else {
                TBL[bytes[i + 3] as usize]
            },
        ];
        if chunk.iter().any(|&v| v < 0) {
            return Err(());
        }
        let n = ((chunk[0] as u32) << 18)
            | ((chunk[1] as u32) << 12)
            | ((chunk[2] as u32) << 6)
            | (chunk[3] as u32);
        out.push((n >> 16) as u8);
        if bytes[i + 2] != b'=' {
            out.push((n >> 8) as u8);
        }
        if bytes[i + 3] != b'=' {
            out.push(n as u8);
        }
        i += 4;
    }
    Ok(out)
}

const fn make_b64_table() -> [i8; 256] {
    let mut t = [-1_i8; 256];
    let alpha = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut i = 0;
    while i < alpha.len() {
        t[alpha[i] as usize] = i as i8;
        i += 1;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_decode_roundtrip() {
        // "hello" -> "aGVsbG8="
        let r = base64_decode("aGVsbG8=").unwrap();
        assert_eq!(r, b"hello");
    }

    #[test]
    fn b64_decode_rejects_non_multiple_of_four() {
        // Non-multiple-of-4 lengths must error before the chunk loop
        // so we never index past the buffer.
        for s in ["a", "ab", "abc", "abcde"] {
            assert!(
                base64_decode(s).is_err(),
                "expected err for len={}",
                s.len()
            );
        }
    }

    #[test]
    fn b64_decode_empty_ok() {
        assert_eq!(base64_decode("").unwrap(), Vec::<u8>::new());
    }
    #[test]
    fn rasterize_simple_svg() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="32" height="32"><rect width="32" height="32" fill="red"/></svg>"#;
        let pm = rasterise_svg(svg, 32, 32).unwrap();
        assert_eq!(pm.width(), 32);
        assert_eq!(pm.height(), 32);
        let red_pixels = pm
            .data()
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[0] >= 200 && p[1] < 50 && p[2] < 50)
            .count();
        assert!(
            red_pixels > 100,
            "expected mostly red, got {red_pixels} red pixels"
        );
    }

    #[test]
    fn icon_budget_counts_source_bytes_and_promotes_hits() {
        let mut resolver = IconResolver::new();
        for n in 0..CACHE_CAP {
            resolver.insert_cache((format!("{n}"), 2, 2), Pixmap::new(2, 2).unwrap(), false);
        }
        resolver.resolve("0", 2, 2).unwrap();
        resolver.insert_cache(("new".into(), 2, 2), Pixmap::new(2, 2).unwrap(), false);
        assert!(resolver.cache.iter().any(|entry| entry.key.0 == "0"));
        assert!(!resolver.cache.iter().any(|entry| entry.key.0 == "1"));
        assert_eq!(resolver.cache.len(), CACHE_CAP);
        resolver.clear_cache();
        resolver.insert_cache(
            ("x".repeat(CACHE_BYTES), 2, 2),
            Pixmap::new(2, 2).unwrap(),
            false,
        );
        assert!(
            resolver.cache.is_empty(),
            "large source keys must not bypass the byte budget"
        );
        for n in 0..20 {
            resolver.insert_cache(
                (format!("{n}"), 512, 512),
                Pixmap::new(512, 512).unwrap(),
                false,
            );
            assert!(resolver.bytes <= CACHE_BYTES);
        }
        assert!(resolver.cache.len() < 20);
    }

    #[test]
    fn explicit_invalidation_refreshes_same_directory_icon() {
        let dir = tempfile::tempdir().unwrap();
        let icons = dir.path().join("icons");
        std::fs::create_dir(&icons).unwrap();
        let path = icons.join("custom.svg");
        let svg = r##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="2"><rect width="2" height="2" fill="red"/></svg>"##;
        std::fs::write(&path, svg).unwrap();
        let mut resolver = IconResolver::new().with_theme_dir(Some(dir.path().to_path_buf()));
        let red = resolver.resolve("custom", 2, 2).unwrap();
        std::fs::write(&path, svg.replace("red", "blue")).unwrap();
        resolver.set_theme_dir(Some(dir.path().to_path_buf()));
        assert_eq!(resolver.resolve("custom", 2, 2).unwrap().data(), red.data());
        resolver.clear_cache();
        let blue = resolver.resolve("custom", 2, 2).unwrap();
        assert_ne!(blue.data(), red.data());
        assert_eq!(&blue.data()[..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn oversized_icon_renders_without_entering_cache() {
        let mut resolver = IconResolver::new();
        let src = r##"data:image/svg+xml,<svg xmlns="http://www.w3.org/2000/svg" width="1025" height="1024"><rect width="1025" height="1024" fill="red"/></svg>"##;
        let pm = resolver.resolve(src, 1025, 1024).unwrap();
        assert_eq!(&pm.data()[..4], &[255, 0, 0, 255]);
        assert!(resolver.cache.is_empty());
        assert_eq!(resolver.bytes, 0);
    }
    #[test]
    fn security_png_limits_reject_headers_before_decoding_pixels() {
        // Real PNG headers followed by IDAT, without pixel data. TooLarge
        // proves rejection happens before next_frame or output allocation.
        let headers: &[&[u8]] = &[
            &[
                137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 32, 0, 0, 0,
                32, 0, 8, 6, 0, 0, 0, 114, 170, 202, 89, 0, 0, 0, 0, 73, 68, 65, 84,
            ],
            &[
                137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 32, 1, 0, 0, 0,
                1, 8, 6, 0, 0, 0, 153, 137, 75, 94, 0, 0, 0, 0, 73, 68, 65, 84,
            ],
            &[
                137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 32, 0, 0, 0, 4,
                0, 16, 6, 0, 0, 0, 25, 200, 11, 149, 0, 0, 0, 0, 73, 68, 65, 84,
            ],
        ];
        for header in headers {
            assert!(matches!(
                rasterise_png(std::io::Cursor::new(header), 8, 8),
                Err(IconError::TooLarge)
            ));
        }
    }

    #[test]
    fn security_icon_target_bounds_precede_asset_io() {
        let mut resolver = IconResolver::new();
        assert!(
            resolver
                .resolve("/path/that/must/not/be/opened.svg", 8193, 1)
                .is_none()
        );
        assert!(matches!(
            rasterise_svg(EMBEDDED_FALLBACK_SVG, 4096, 4096),
            Err(IconError::TooLarge)
        ));
    }
}
