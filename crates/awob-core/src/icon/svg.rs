//! SVG image resolvers share one budget across the complete image tree.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use super::{IconError, MAX_INLINE_BYTES, file_format, read_icon_file};
use crate::limits;

const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
const MAX_IMAGES: usize = 64;
const MAX_DEPTH: usize = 4;

#[derive(Default)]
struct Budget {
    bytes: AtomicUsize,
    images: AtomicUsize,
    decoded: AtomicUsize,
    rejected: AtomicBool,
}

fn charge(counter: &AtomicUsize, amount: usize, limit: usize) -> Result<(), IconError> {
    let mut used = counter.load(Ordering::Relaxed);
    loop {
        let total = used
            .checked_add(amount)
            .filter(|&total| total <= limit)
            .ok_or(IconError::TooLarge)?;
        match counter.compare_exchange_weak(used, total, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return Ok(()),
            Err(current) => used = current,
        }
    }
}

pub(super) fn parse(bytes: &[u8]) -> Result<usvg::Tree, IconError> {
    let budget = Arc::new(Budget::default());
    charge(&budget.bytes, bytes.len(), MAX_TOTAL_BYTES)?;
    let tree = parse_tree(bytes, &budget, 0)?;
    if budget.rejected.load(Ordering::Relaxed) {
        return Err(IconError::TooLarge);
    }
    Ok(tree)
}

fn parse_tree(bytes: &[u8], budget: &Arc<Budget>, depth: usize) -> Result<usvg::Tree, IconError> {
    if depth > MAX_DEPTH {
        return Err(IconError::TooLarge);
    }
    // from_data also accepts SVGZ. Requiring plain UTF-8 avoids decompression
    // before the SVG source byte budget can be enforced.
    let text = std::str::from_utf8(bytes).map_err(|_| IconError::UnsupportedFormat)?;
    let data_budget = Arc::clone(budget);
    let path_budget = Arc::clone(budget);
    let options = usvg::Options {
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_data: Box::new(move |mime, data, _| {
                resolve(&data_budget, || {
                    charge(&data_budget.images, 1, MAX_IMAGES)?;
                    if data.len() > MAX_INLINE_BYTES {
                        return Err(IconError::TooLarge);
                    }
                    image(mime, data, &data_budget, depth)
                })
            }),
            resolve_string: Box::new(move |href, _| {
                resolve(&path_budget, || {
                    charge(&path_budget.images, 1, MAX_IMAGES)?;
                    let path = std::path::Path::new(href);
                    let mime = file_format(path)?;
                    let data = Arc::new(read_icon_file(path)?);
                    image(mime, data, &path_budget, depth)
                })
            }),
        },
        ..usvg::Options::default()
    };
    let result = if depth == 0 {
        usvg::Tree::from_str(text, &options)
    } else {
        // Retain SVG's existing rule: nested SVG images cannot reference
        // external files. Inline images still use our shared-budget callback.
        usvg::Tree::from_data_nested(bytes, &options)
    };
    result.map_err(|e| IconError::Svg(e.to_string()))
}

fn resolve(
    budget: &Budget,
    load: impl FnOnce() -> Result<usvg::ImageKind, IconError>,
) -> Option<usvg::ImageKind> {
    if budget.rejected.load(Ordering::Relaxed) {
        return None;
    }
    match load() {
        Ok(image) => Some(image),
        Err(_) => {
            // usvg normally omits a bad image and keeps rendering. Reject the
            // complete icon instead of caching an incomplete representation.
            budget.rejected.store(true, Ordering::Relaxed);
            None
        }
    }
}

fn image(
    mime: &str,
    data: Arc<Vec<u8>>,
    budget: &Arc<Budget>,
    depth: usize,
) -> Result<usvg::ImageKind, IconError> {
    charge(&budget.bytes, data.len(), MAX_TOTAL_BYTES)?;
    match mime {
        "image/png" => {
            let mut reader = png::Decoder::new_with_limits(
                std::io::Cursor::new(&*data),
                png::Limits {
                    bytes: limits::PNG_SCRATCH_BYTES,
                },
            )
            .read_info()
            .map_err(|e| IconError::Png(e.to_string()))?;
            let pixels = limits::pixels(reader.info().width, reader.info().height)
                .map_err(|_| IconError::TooLarge)?;
            // resvg expands to RGBA8; the direct PNG decoder may use more bytes
            // for 16-bit input. Account for whichever representation is larger.
            let decoded = reader.output_buffer_size().max(pixels * 4);
            charge(&budget.decoded, decoded, limits::MAX_RGBA_BYTES)?;
            // Validate compressed rows without allocating a full output image.
            // resvg otherwise silently drops corrupt nested PNGs after parse.
            while reader
                .next_row()
                .map_err(|e| IconError::Png(e.to_string()))?
                .is_some()
            {}
            reader.finish().map_err(|e| IconError::Png(e.to_string()))?;
            Ok(usvg::ImageKind::PNG(data))
        }
        "image/svg+xml" => parse_tree(&data, budget, depth + 1).map(usvg::ImageKind::SVG),
        _ => Err(IconError::UnsupportedFormat),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut out, width, height);
            encoder.set_color(png::ColorType::Rgba);
            let mut writer = encoder.write_header().unwrap();
            {
                let mut stream = writer.stream_writer().unwrap();
                let row = [255, 0, 0, 255].repeat(width as usize);
                for _ in 0..height {
                    stream.write_all(&row).unwrap();
                }
                stream.finish().unwrap();
            }
            writer.finish().unwrap();
        }
        out
    }

    fn base64(bytes: &[u8]) -> String {
        const DIGITS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in bytes.chunks(3) {
            let value = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            for (index, shift) in [18, 12, 6, 0].iter().enumerate() {
                out.push(if index <= chunk.len() {
                    DIGITS[((value >> shift) & 63) as usize] as char
                } else {
                    '='
                });
            }
        }
        out
    }

    fn svg(href: &str, count: usize) -> String {
        let image = format!(r#"<image href="{href}" width="8" height="8"/>"#);
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8">{}</svg>"#,
            image.repeat(count)
        )
    }
    fn inline(mime: &str, data: &[u8]) -> String {
        format!("data:{mime};base64,{}", base64(data))
    }

    #[test]
    fn security_valid_nested_png_and_svg_preserve_pixels() {
        let png = inline("image/png", &png(1, 1));
        let nested = inline("image/svg+xml", svg(&png, 1).as_bytes());
        for href in [png, nested] {
            let pixmap = super::super::rasterise_svg(svg(&href, 1).as_bytes(), 8, 8).unwrap();
            assert_eq!(&pixmap.data()[..4], &[255, 0, 0, 255]);
        }
    }

    #[test]
    fn security_references_and_depth_share_root_budget() {
        let png = inline("image/png", &png(1, 1));
        assert!(parse(svg(&png, 64).as_bytes()).is_ok());
        assert!(parse(svg(&png, 65).as_bytes()).is_err());
        let mut nested = svg(&png, 1);
        for _ in 0..MAX_DEPTH {
            nested = svg(&inline("image/svg+xml", nested.as_bytes()), 1);
        }
        assert!(parse(nested.as_bytes()).is_ok());
        assert!(parse(svg(&inline("image/svg+xml", nested.as_bytes()), 1).as_bytes()).is_err());
    }

    #[test]
    fn security_aggregate_file_bytes_and_inline_bytes_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("padded.png");
        let mut data = png(1, 1);
        data.resize(700 * 1024, 0);
        std::fs::write(&path, &data).unwrap();
        let href = path.to_str().unwrap();
        assert!(parse(svg(href, 2).as_bytes()).is_ok());
        assert!(parse(svg(href, 3).as_bytes()).is_err());
        data.resize(MAX_INLINE_BYTES + 1, 0);
        assert!(parse(svg(&inline("image/png", &data), 1).as_bytes()).is_err());
    }

    #[test]
    fn security_decoded_png_budget_accumulates_across_references() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pixels.png");
        std::fs::write(&path, png(2048, 2048)).unwrap();
        assert!(parse(svg(path.to_str().unwrap(), 2).as_bytes()).is_ok());
        assert!(parse(svg(path.to_str().unwrap(), 3).as_bytes()).is_err());
    }

    #[test]
    fn security_forbidden_formats_and_bad_pixels_reject_whole_icon() {
        let image = png(1, 1);
        for mime in ["image/jpeg", "image/gif", "image/webp", "text/plain"] {
            assert!(parse(svg(&inline(mime, &image), 1).as_bytes()).is_err());
        }
        let corrupt = &image[..image.len() / 2];
        assert!(parse(svg(&inline("image/png", corrupt), 1).as_bytes()).is_err());
    }

    #[test]
    fn security_external_svg_and_existing_nested_file_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("vector.svg");
        std::fs::write(&path,r##"<svg xmlns="http://www.w3.org/2000/svg" width="8" height="8"><rect width="8" height="8" fill="#ff0000"/></svg>"##).unwrap();
        assert!(parse(svg(path.to_str().unwrap(), 1).as_bytes()).is_ok());
        // SVG specification disables external files inside nested SVG images.
        let nested = inline("image/svg+xml", svg("/not-present.png", 1).as_bytes());
        assert!(parse(svg(&nested, 1).as_bytes()).is_ok());
    }
}
