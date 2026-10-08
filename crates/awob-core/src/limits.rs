//! Per-allocation bounds for untrusted image and theme dimensions.
//!
//! These limits do not bound total process RSS, font caches or scene complexity.

/// Largest accepted axis for a raster allocation, in pixels.
pub const MAX_AXIS: u32 = 8192;
/// Largest accepted pixel count; a 4K surface fits within this budget.
pub const MAX_PIXELS: usize = 8 * 1024 * 1024;
/// Largest accepted decoded image/output buffer, in bytes.
pub const MAX_RGBA_BYTES: usize = MAX_PIXELS * 4;
/// PNG decoder-owned scratch budget, separate from the caller's output buffer.
pub(crate) const PNG_SCRATCH_BYTES: usize = 8 * 1024 * 1024;

/// A raster dimension or allocation exceeds the renderer's input budget.
#[derive(Debug, thiserror::Error)]
#[error("raster exceeds limits (axes 1..={MAX_AXIS}, pixels <= {MAX_PIXELS})")]
pub struct RasterLimit;

/// Validate dimensions and calculate their pixel count without overflowing.
///
/// ```
/// assert_eq!(awob_core::limits::pixels(3840, 2160).unwrap(), 8_294_400);
/// assert!(awob_core::limits::pixels(u32::MAX, 1).is_err());
/// ```
pub fn pixels(width: u32, height: u32) -> Result<usize, RasterLimit> {
    if width == 0 || height == 0 || width > MAX_AXIS || height > MAX_AXIS {
        return Err(RasterLimit);
    }
    let pixels = (width as usize)
        .checked_mul(height as usize)
        .ok_or(RasterLimit)?;
    if pixels > MAX_PIXELS {
        return Err(RasterLimit);
    }
    Ok(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries_and_overflow_are_rejected_without_allocating() {
        assert_eq!(pixels(8192, 1024).unwrap(), MAX_PIXELS);
        for (w, h) in [
            (0, 1),
            (1, 0),
            (8193, 1),
            (8192, 1025),
            (u32::MAX, u32::MAX),
        ] {
            assert!(pixels(w, h).is_err());
        }
    }
}
