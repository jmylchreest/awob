//! Real text rendering via cosmic-text + fontdb.
//!
//! [`TextRenderer`] holds a [`cosmic_text::FontSystem`] (which loads system
//! fonts from fontdb at construction) and a [`cosmic_text::SwashCache`] for
//! rasterised glyph reuse. Each `draw` call lays out the run, rasterises the
//! glyphs through swash, and alpha-blends them into the destination pixmap
//! at the requested origin.
//!
//! Font specifiers in theme files take the form `"Family Name <size> <weight>"`
//! (e.g. `"Inter 14 500"`). Family is everything up to the trailing
//! integer pair; size is required, weight is optional.

use cosmic_text::{Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, Weight};
use tiny_skia::Pixmap;

use crate::colour::Colour;

// Reset after a completed draw, so a large run is never truncated. These bound
// retained glyph images/outlines; they do not bound transient shaping/raster
// allocations or FontSystem's internal font and shaping caches.
const GLYPH_CACHE_BYTES: usize = 4 * 1024 * 1024;
const GLYPH_CACHE_ENTRIES: usize = 4096;

pub struct TextRenderer {
    pub font_system: FontSystem,
    pub swash_cache: SwashCache,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRenderer {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
        }
    }

    pub(crate) fn clear_glyph_cache(&mut self) {
        self.swash_cache = SwashCache::new();
    }

    fn trim_glyph_cache(&mut self) {
        let images = &self.swash_cache.image_cache;
        let outlines = &self.swash_cache.outline_command_cache;
        let entries = images.len().saturating_add(outlines.len());
        let image_bytes = images
            .values()
            .map(|image| {
                std::mem::size_of::<(cosmic_text::CacheKey, Option<cosmic_text::SwashImage>)>()
                    + image.as_ref().map_or(0, |image| image.data.capacity())
            })
            .sum::<usize>();
        let outline_bytes = outlines
            .values()
            .map(|commands| {
                std::mem::size_of::<(cosmic_text::CacheKey, Option<Box<[cosmic_text::Command]>>)>()
                    + commands.as_deref().map_or(0, std::mem::size_of_val)
            })
            .sum::<usize>();
        if entries > GLYPH_CACHE_ENTRIES
            || image_bytes.saturating_add(outline_bytes) > GLYPH_CACHE_BYTES
        {
            self.clear_glyph_cache();
        }
    }

    /// Returns (width_px, height_px) for the text laid out with the given font.
    pub fn measure(&mut self, text: &str, font_spec: &FontSpec) -> (f32, f32) {
        let buffer = self.shape(text, font_spec);
        Self::measure_shaped(&buffer)
    }

    pub(crate) fn measure_shaped(buffer: &Buffer) -> (f32, f32) {
        let mut max_w: f32 = 0.0;
        let mut max_y: f32 = 0.0;
        for run in buffer.layout_runs() {
            max_w = max_w.max(run.line_w);
            max_y = max_y.max(run.line_top + run.line_height);
        }
        (max_w, max_y)
    }

    /// Render `text` with `colour` into `pm`, with the text's top-left corner
    /// at (`x`, `y`). Pixels outside `pm` are clipped.
    pub fn draw(
        &mut self,
        pm: &mut Pixmap,
        x: f32,
        y: f32,
        text: &str,
        font_spec: &FontSpec,
        colour: Colour,
    ) {
        let mut buffer = self.shape(text, font_spec);
        self.draw_shaped(pm, x, y, &mut buffer, colour);
    }

    pub(crate) fn draw_shaped(
        &mut self,
        pm: &mut Pixmap,
        x: f32,
        y: f32,
        buffer: &mut Buffer,
        colour: Colour,
    ) {
        let cosmic_color = cosmic_text::Color::rgba(colour.r, colour.g, colour.b, colour.a);
        let pm_w = pm.width() as i32;
        let pm_h = pm.height() as i32;
        let stride = pm_w * 4;
        let pm_data = pm.data_mut();

        let ox = x.round() as i32;
        let oy = y.round() as i32;
        buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            cosmic_color,
            |gx, gy, gw, gh, gcolor| {
                let (cr, cg, cb, ca) = (gcolor.r(), gcolor.g(), gcolor.b(), gcolor.a());
                if ca == 0 {
                    return;
                }
                let max_dx = gw.max(1) as i32;
                let max_dy = gh.max(1) as i32;
                for dy in 0..max_dy {
                    let py = oy + gy + dy;
                    if py < 0 || py >= pm_h {
                        continue;
                    }
                    for dx in 0..max_dx {
                        let px = ox + gx + dx;
                        if px < 0 || px >= pm_w {
                            continue;
                        }
                        let idx = (py * stride + px * 4) as usize;
                        // Source-over alpha blend, premultiplied math.
                        let s_a = ca as u32;
                        let inv = 255 - s_a;
                        let dr = pm_data[idx] as u32;
                        let dg = pm_data[idx + 1] as u32;
                        let db = pm_data[idx + 2] as u32;
                        let da = pm_data[idx + 3] as u32;
                        // cosmic_text gives straight (non-premultiplied) RGBA.
                        // Convert to premultiplied for compositing.
                        let s_r = (cr as u32 * s_a) / 255;
                        let s_g = (cg as u32 * s_a) / 255;
                        let s_b = (cb as u32 * s_a) / 255;
                        pm_data[idx] = (s_r + dr * inv / 255) as u8;
                        pm_data[idx + 1] = (s_g + dg * inv / 255) as u8;
                        pm_data[idx + 2] = (s_b + db * inv / 255) as u8;
                        pm_data[idx + 3] = (s_a + da * inv / 255) as u8;
                    }
                }
            },
        );
        self.trim_glyph_cache();
    }

    pub(crate) fn shape(&mut self, text: &str, spec: &FontSpec) -> Buffer {
        let metrics = Metrics::new(spec.size, spec.size * 1.25);
        let mut buffer = Buffer::new(&mut self.font_system, metrics);
        // CSS generic family names map to cosmic-text's generic enum
        // variants. This is what makes `font="monospace 13"` actually
        // resolve to the system's default monospace face (DejaVu Sans
        // Mono on most Linux distros) rather than searching for a
        // family literally named "monospace" and falling through to the
        // default sans face.
        let family = match spec.family.to_ascii_lowercase().as_str() {
            "monospace" => Family::Monospace,
            "serif" => Family::Serif,
            "sans-serif" | "sans serif" => Family::SansSerif,
            "cursive" => Family::Cursive,
            "fantasy" => Family::Fantasy,
            _ => Family::Name(&spec.family),
        };
        let attrs = Attrs::new()
            .family(family)
            .weight(Weight(spec.weight))
            .style(if spec.italic {
                Style::Italic
            } else {
                Style::Normal
            });
        buffer.set_size(Some(10_000.0), Some(spec.size * 4.0));
        buffer.set_text(text, &attrs, Shaping::Advanced, None);
        buffer.shape_until_scroll(&mut self.font_system, false);
        buffer
    }
}

#[derive(Debug, Clone)]
pub struct FontSpec {
    pub family: String,
    pub size: f32,
    pub weight: u16,
    pub italic: bool,
}

impl Default for FontSpec {
    fn default() -> Self {
        Self {
            family: "sans-serif".into(),
            size: 14.0,
            weight: 400,
            italic: false,
        }
    }
}

impl FontSpec {
    /// Parse a CSS-flavoured font shorthand. Examples:
    ///
    /// * `"Inter 14"` → family Inter, size 14, weight 400, upright
    /// * `"Inter 14 500"` → family Inter, size 14, weight 500
    /// * `"Inter 14 italic"` / `"Inter italic 14"` → italic upright
    /// * `"DejaVu Sans 12 700 italic"` → multi-word family + size + weight + style
    /// * Aliases for weight: `thin=100`, `light=300`, `regular=400`,
    ///   `medium=500`, `semibold=600`, `bold=700`, `black=900`.
    pub fn parse(s: &str) -> Self {
        let mut size: f32 = 14.0;
        let mut weight: u16 = 400;
        let mut italic = false;

        let mut family_parts: Vec<&str> = Vec::new();
        for token in s.split_whitespace() {
            let lower = token.to_ascii_lowercase();
            if lower == "italic" || lower == "oblique" {
                italic = true;
                continue;
            }
            if lower == "normal" || lower == "upright" {
                italic = false;
                continue;
            }
            if let Some(w) = weight_alias(&lower) {
                weight = w;
                continue;
            }
            if let Ok(n) = token.parse::<u16>() {
                if (100..=900).contains(&n) && (n % 100 == 0 || n == 350 || n == 950) {
                    weight = n;
                    continue;
                }
                size = n as f32;
                continue;
            }
            if let Ok(f) = token.parse::<f32>() {
                size = f;
                continue;
            }
            family_parts.push(token);
        }
        let family = if family_parts.is_empty() {
            "sans-serif".into()
        } else {
            family_parts.join(" ")
        };
        Self {
            family,
            size,
            weight,
            italic,
        }
    }
}

fn weight_alias(s: &str) -> Option<u16> {
    Some(match s {
        "thin" | "hairline" => 100,
        "extralight" | "ultralight" => 200,
        "light" => 300,
        "regular" | "normal-weight" => 400,
        "medium" => 500,
        "semibold" | "demibold" => 600,
        "bold" => 700,
        "extrabold" | "ultrabold" => 800,
        "black" | "heavy" => 900,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_family_size_weight() {
        let f = FontSpec::parse("Inter 14 500");
        assert_eq!(f.family, "Inter");
        assert_eq!(f.size, 14.0);
        assert_eq!(f.weight, 500);
    }
    #[test]
    fn parse_family_size() {
        let f = FontSpec::parse("Inter 16");
        assert_eq!(f.family, "Inter");
        assert_eq!(f.size, 16.0);
        assert_eq!(f.weight, 400);
    }
    #[test]
    fn parse_multi_word_family() {
        let f = FontSpec::parse("DejaVu Sans 12 700");
        assert_eq!(f.family, "DejaVu Sans");
        assert_eq!(f.size, 12.0);
        assert_eq!(f.weight, 700);
    }
    #[test]
    fn parse_only_family() {
        let f = FontSpec::parse("Inter");
        assert_eq!(f.family, "Inter");
        assert_eq!(f.size, 14.0);
        assert_eq!(f.weight, 400);
        assert!(!f.italic);
    }
    #[test]
    fn parse_italic_anywhere() {
        let f = FontSpec::parse("Inter italic 14");
        assert_eq!(f.family, "Inter");
        assert_eq!(f.size, 14.0);
        assert!(f.italic);
        let f = FontSpec::parse("DejaVu Sans 12 oblique");
        assert_eq!(f.family, "DejaVu Sans");
        assert!(f.italic);
    }
    #[test]
    fn parse_weight_aliases() {
        assert_eq!(FontSpec::parse("Inter 14 bold").weight, 700);
        assert_eq!(FontSpec::parse("Inter 14 light").weight, 300);
        assert_eq!(FontSpec::parse("DejaVu medium 12").weight, 500);
    }

    #[test]
    fn shared_shape_matches_separate_measure_and_draw() {
        let mut renderer = TextRenderer::new();
        for label in ["Volume 75%", "mute", "مرحبا", "A\nsecond line", ""] {
            for font in ["sans-serif 14", "monospace 17 bold", "serif 12 italic"] {
                let spec = FontSpec::parse(font);
                let expected_size = renderer.measure(label, &spec);
                let mut expected = Pixmap::new(180, 50).unwrap();
                renderer.draw(
                    &mut expected,
                    -2.0,
                    3.0,
                    label,
                    &spec,
                    Colour::rgb(83, 149, 231),
                );
                let mut buffer = renderer.shape(label, &spec);
                assert_eq!(TextRenderer::measure_shaped(&buffer), expected_size);
                let mut actual = Pixmap::new(180, 50).unwrap();
                renderer.draw_shaped(
                    &mut actual,
                    -2.0,
                    3.0,
                    &mut buffer,
                    Colour::rgb(83, 149, 231),
                );
                assert_eq!(actual.data(), expected.data(), "{label:?}, {font}");
            }
        }
    }

    fn cache_key(glyph: u16) -> cosmic_text::CacheKey {
        cosmic_text::CacheKey::new(
            Default::default(),
            glyph,
            14.0,
            (0.0, 0.0),
            Weight(400),
            cosmic_text::CacheKeyFlags::empty(),
        )
        .0
    }

    #[test]
    fn glyph_cache_limits_include_missing_glyphs_and_payload_bytes() {
        let mut renderer = TextRenderer::new();
        for glyph in 0..=GLYPH_CACHE_ENTRIES as u16 {
            renderer
                .swash_cache
                .image_cache
                .insert(cache_key(glyph), None);
        }
        renderer.trim_glyph_cache();
        assert!(renderer.swash_cache.image_cache.is_empty());
        let mut image = cosmic_text::SwashImage::new();
        image.data = vec![0; GLYPH_CACHE_BYTES];
        renderer
            .swash_cache
            .image_cache
            .insert(cache_key(0), Some(image));
        renderer.trim_glyph_cache();
        assert!(renderer.swash_cache.image_cache.is_empty());

        renderer.swash_cache.outline_command_cache.insert(
            cache_key(0),
            vec![
                cosmic_text::Command::Close;
                GLYPH_CACHE_BYTES / std::mem::size_of::<cosmic_text::Command>() + 1
            ]
            .into_boxed_slice()
            .into(),
        );
        renderer.trim_glyph_cache();
        assert!(renderer.swash_cache.outline_command_cache.is_empty());
    }

    #[test]
    fn glyph_reset_preserves_fonts_and_drawn_pixels() {
        let mut renderer = TextRenderer::new();
        let spec = FontSpec::parse("sans-serif 14");
        let mut expected = Pixmap::new(180, 50).unwrap();
        renderer.draw(
            &mut expected,
            0.0,
            0.0,
            "Volume 75%",
            &spec,
            Colour::rgb(255, 255, 255),
        );
        let font_count = renderer.font_system.db().faces().count();
        renderer.clear_glyph_cache();
        assert!(renderer.swash_cache.image_cache.is_empty());
        assert_eq!(renderer.font_system.db().faces().count(), font_count);
        let mut actual = Pixmap::new(180, 50).unwrap();
        renderer.draw(
            &mut actual,
            0.0,
            0.0,
            "Volume 75%",
            &spec,
            Colour::rgb(255, 255, 255),
        );
        assert_eq!(actual.data(), expected.data());
    }
}
