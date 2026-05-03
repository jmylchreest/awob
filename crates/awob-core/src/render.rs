//! tiny-skia rasterisation pass over a parsed [`Theme`].
//!
//! Resolves [`AttrValue`]s against [`Bindings`] and draws the result into a
//! `tiny_skia::Pixmap`. The pixmap layout is BGRA premultiplied, suitable for
//! handing to a Wayland `wl_shm` ARGB8888 buffer (Wayland's "ARGB8888" matches
//! tiny-skia's BGRA-on-little-endian byte order).
//!
//! Visual polish in this initial implementation:
//! * Real: rect (rounded), bar, fill colours, palette/style merging, layout.
//! * Placeholder: text and image render as flat coloured rects sized to their
//!   bounding box. Adding cosmic-text + resvg/png is a follow-up — the scene
//!   tree, layout, and colour pipeline are already in place to plug them in.
//!
//! See `awob-renderer` decision for the architectural choice.

use std::path::PathBuf;

use tiny_skia::{
    BlendMode, Color as SkColor, FillRule, Paint, PathBuilder, Pixmap, Rect, Transform,
};

use crate::bindings::Bindings;
use crate::colour::Colour;
use crate::expr::ExprError;
use crate::icon::IconResolver;
use crate::scene::*;
use crate::text::{FontSpec, TextRenderer};
use crate::theme::Theme;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("expression: {0}")]
    Expr(#[from] ExprError),
    #[error("attribute `{0}` could not resolve: {1}")]
    Attr(String, String),
    #[error("renderer: {0}")]
    Other(String),
}

/// Stateful renderer holding a text shaper, icon cache, and shadow-mask
/// cache. Construct once per daemon process and call [`Renderer::render`]
/// per-send — the heavy state is reused across renders.
pub struct Renderer {
    text: TextRenderer,
    icons: IconResolver,
    shadows: crate::shadow::ShadowCache,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            text: TextRenderer::new(),
            icons: IconResolver::new(),
            shadows: crate::shadow::ShadowCache::new(),
        }
    }

    /// Tell the icon resolver where the active theme lives so it can look
    /// up `<theme>/icons/<name>.svg` before falling back to system themes.
    pub fn set_theme_dir(&mut self, dir: Option<PathBuf>) {
        self.icons.set_theme_dir(dir);
    }

    /// Render the theme's scene against the given bindings into a pixmap.
    ///
    /// The pixmap is sized to the theme's surface dimensions and fully
    /// cleared (transparent) before drawing.
    pub fn render(&mut self, theme: &Theme, bindings: &Bindings) -> Result<Pixmap, RenderError> {
        let w = theme.surface.width.max(1);
        let h = theme.surface.height.max(1);
        let mut pixmap = Pixmap::new(w, h)
            .ok_or_else(|| RenderError::Other(format!("Pixmap::new({w},{h}) failed")))?;
        pixmap.fill(SkColor::TRANSPARENT);

        let frame = Frame {
            w: w as f32,
            h: h as f32,
        };

        let elements = sorted_by_z(&theme.scene.elements);
        for element in elements {
            self.draw_element(element, &frame, bindings, &mut pixmap)?;
        }

        Ok(pixmap)
    }
}

/// Convenience function for one-off rendering (tests). Each call creates a
/// fresh Renderer — for production the daemon constructs a [`Renderer`] once
/// and reuses it.
pub fn render_to_pixmap(theme: &Theme, bindings: &Bindings) -> Result<Pixmap, RenderError> {
    Renderer::new().render(theme, bindings)
}

fn sorted_by_z(elements: &[Element]) -> Vec<&Element> {
    let mut v: Vec<&Element> = elements.iter().collect();
    v.sort_by_key(|e| e.z());
    v
}

#[derive(Clone, Copy)]
struct Frame {
    w: f32,
    h: f32,
}

#[derive(Clone, Copy, Debug)]
struct Box2 {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Renderer {
    fn draw_element(
        &mut self,
        el: &Element,
        frame: &Frame,
        b: &Bindings,
        pm: &mut Pixmap,
    ) -> Result<(), RenderError> {
        match el {
            Element::Rect(r) => self.draw_rect_with_shadow(r, frame, b, pm),
            Element::Bar(r) => draw_bar(r, frame, b, pm),
            Element::Text(t) => self.draw_text(t, frame, b, pm),
            Element::Image(i) => self.draw_image(i, frame, b, pm),
        }
    }

    fn draw_text(
        &mut self,
        t: &TextEl,
        frame: &Frame,
        b: &Bindings,
        pm: &mut Pixmap,
    ) -> Result<(), RenderError> {
        let label = t.value.render(b)?;
        if label.is_empty() {
            return Ok(());
        }
        let font_spec = match &t.font {
            Some(f) => FontSpec::parse(f),
            None => FontSpec::default(),
        };
        let (text_w, text_h) = self.text.measure(&label, &font_spec);
        let bb_x = resolve_x(&t.common, frame, b, text_w)?;
        let bb_y = resolve_y(&t.common, frame, b, text_h)?;
        let colour = t
            .colour
            .as_ref()
            .and_then(|a| try_render_colour(a, b))
            .unwrap_or(Colour::rgb(0xff, 0xff, 0xff));
        self.text.draw(pm, bb_x, bb_y, &label, &font_spec, colour);
        Ok(())
    }

    fn draw_image(
        &mut self,
        i: &ImageEl,
        frame: &Frame,
        b: &Bindings,
        pm: &mut Pixmap,
    ) -> Result<(), RenderError> {
        let bb = resolve_box(&i.common, &i.size, frame, b)?;
        let src = i.src.render(b)?;
        let bb_w = bb.w.round().max(1.0) as u32;
        let bb_h = bb.h.round().max(1.0) as u32;
        if let Some((mut icon_pm, was_symbolic)) = self.icons.resolve_with_meta(&src, bb_w, bb_h) {
            // Colour resolution:
            //   1. `colour="auto"` / `colour="none"` — never tint; preserve
            //      whatever colours the source image carries.
            //   2. Explicit colour expression (e.g. `colour="$fg"`,
            //      `colour="#ff00aa"`) — flat-tint to that colour.
            //   3. Unset + symbolic source — auto-tint to theme's `$fg` so
            //      dark Adwaita symbolic SVGs are legible.
            //   4. Unset + non-symbolic — preserve original colours so
            //      multicolour app icons render faithfully.
            let mut tint_to: Option<Colour> = None;
            let mut explicit_disable = false;
            if let Some(attr) = &i.colour {
                let raw = attr.render(b).unwrap_or_default();
                let trimmed = raw.trim().to_ascii_lowercase();
                if trimmed == "auto" || trimmed == "none" {
                    explicit_disable = true;
                } else {
                    tint_to = try_render_colour(attr, b);
                }
            }
            if !explicit_disable && tint_to.is_none() && was_symbolic {
                tint_to = Some(
                    b.palette
                        .get("fg")
                        .copied()
                        .unwrap_or(Colour::rgb(0xff, 0xff, 0xff)),
                );
            }
            if let Some(c) = tint_to {
                crate::icon::tint_pixmap(&mut icon_pm, c);
            }
            blit_pixmap(pm, &icon_pm, bb.x, bb.y);
            return Ok(());
        }
        let colour = b
            .palette
            .get("fg")
            .copied()
            .unwrap_or(Colour::rgb(0xff, 0xff, 0xff));
        fill_rounded_rect(pm, bb, bb.w.min(bb.h) / 4.0, with_alpha(colour, 0.25));
        Ok(())
    }
}

fn blit_pixmap(dst: &mut Pixmap, src: &Pixmap, x: f32, y: f32) {
    let dst_w = dst.width() as i32;
    let dst_h = dst.height() as i32;
    let stride = dst_w * 4;
    let src_w = src.width() as i32;
    let src_h = src.height() as i32;
    let ox = x.round() as i32;
    let oy = y.round() as i32;
    let dst_data = dst.data_mut();
    let src_data = src.data();
    for sy in 0..src_h {
        let py = oy + sy;
        if py < 0 || py >= dst_h {
            continue;
        }
        for sx in 0..src_w {
            let px = ox + sx;
            if px < 0 || px >= dst_w {
                continue;
            }
            let s_idx = ((sy * src_w + sx) * 4) as usize;
            let d_idx = (py * stride + px * 4) as usize;
            let s_a = src_data[s_idx + 3] as u32;
            if s_a == 0 {
                continue;
            }
            let inv = 255 - s_a;
            for c in 0..3 {
                let s = src_data[s_idx + c] as u32;
                let d = dst_data[d_idx + c] as u32;
                dst_data[d_idx + c] = (s + d * inv / 255) as u8;
            }
            let d_a = dst_data[d_idx + 3] as u32;
            dst_data[d_idx + 3] = (s_a + d_a * inv / 255) as u8;
        }
    }
}

impl Renderer {
    /// Render a `rect` element including its drop-shadow (if any). Shadow
    /// goes down first so the rect's fill paints over it; the shadow mask
    /// is cached on `self.shadows` keyed by `(w, h, radius, blur)`.
    fn draw_rect_with_shadow(
        &mut self,
        r: &RectEl,
        frame: &Frame,
        b: &Bindings,
        pm: &mut Pixmap,
    ) -> Result<(), RenderError> {
        if let Some(shadow_attr) = &r.shadow {
            if let Ok(s) = shadow_attr.render(b) {
                if let Some(spec) = crate::shadow::parse(&s) {
                    let bb = resolve_box(&r.common, &r.size, frame, b)?;
                    let radius = r
                        .radius
                        .as_ref()
                        .map(|a| a.render_number(b))
                        .transpose()?
                        .unwrap_or(0.0) as f32;
                    self.draw_shadow(pm, bb, radius, spec);
                }
            }
        }
        draw_rect(r, frame, b, pm)
    }

    fn draw_shadow(
        &mut self,
        pm: &mut Pixmap,
        bb: Box2,
        radius: f32,
        spec: crate::shadow::ShadowSpec,
    ) {
        let w = bb.w.round().max(0.0) as u32;
        let h = bb.h.round().max(0.0) as u32;
        if w == 0 || h == 0 || spec.colour.a == 0 {
            return;
        }
        let radius_u = radius.round().max(0.0) as u32;
        let blur_u = spec.blur_radius.round().max(0.0) as u32;
        let pad = crate::shadow::shadow_padding(blur_u) as f32;
        let (mw, mh, mask) = self.shadows.get_or_compute(w, h, radius_u, blur_u);
        // Owned copy lets us drop the cache borrow before mutably touching `pm`.
        let owned: Vec<u8> = mask.to_vec();
        blit_shadow_mask(
            pm,
            &owned,
            mw,
            mh,
            bb.x + spec.offset_x - pad,
            bb.y + spec.offset_y - pad,
            spec.colour,
        );
    }
}

fn blit_shadow_mask(
    dst: &mut Pixmap,
    mask: &[u8],
    mw: u32,
    mh: u32,
    x: f32,
    y: f32,
    colour: Colour,
) {
    let dst_w = dst.width() as i32;
    let dst_h = dst.height() as i32;
    let stride = dst_w * 4;
    let dst_data = dst.data_mut();
    let ox = x.round() as i32;
    let oy = y.round() as i32;
    let cr = colour.r as u32;
    let cg = colour.g as u32;
    let cb = colour.b as u32;
    let ca = colour.a as u32;
    if ca == 0 {
        return;
    }
    for my in 0..mh as i32 {
        let py = oy + my;
        if py < 0 || py >= dst_h {
            continue;
        }
        let row_idx = (my as u32 * mw) as usize;
        for mx in 0..mw as i32 {
            let px = ox + mx;
            if px < 0 || px >= dst_w {
                continue;
            }
            let m = mask[row_idx + mx as usize] as u32;
            if m == 0 {
                continue;
            }
            let eff = m * ca / 255;
            if eff == 0 {
                continue;
            }
            let inv = 255 - eff;
            let d_idx = (py * stride + px * 4) as usize;
            dst_data[d_idx] = ((cr * eff / 255) + (dst_data[d_idx] as u32) * inv / 255) as u8;
            dst_data[d_idx + 1] =
                ((cg * eff / 255) + (dst_data[d_idx + 1] as u32) * inv / 255) as u8;
            dst_data[d_idx + 2] =
                ((cb * eff / 255) + (dst_data[d_idx + 2] as u32) * inv / 255) as u8;
            dst_data[d_idx + 3] = (eff + (dst_data[d_idx + 3] as u32) * inv / 255) as u8;
        }
    }
}

fn draw_rect(r: &RectEl, frame: &Frame, b: &Bindings, pm: &mut Pixmap) -> Result<(), RenderError> {
    let bb = resolve_box(&r.common, &r.size, frame, b)?;
    let radius = r
        .radius
        .as_ref()
        .map(|a| a.render_number(b))
        .transpose()?
        .unwrap_or(0.0) as f32;
    let fill = r
        .fill
        .as_ref()
        .and_then(|a| try_render_colour(a, b))
        .unwrap_or(Colour::TRANSPARENT);
    fill_rounded_rect(pm, bb, radius, fill);
    if let Some(stroke_attr) = &r.stroke {
        if let Some(stroke_color) = try_render_colour(stroke_attr, b) {
            let sw = r
                .stroke_width
                .as_ref()
                .map(|a| a.render_number(b))
                .transpose()?
                .unwrap_or(1.0) as f32;
            stroke_rounded_rect(pm, bb, radius, sw, stroke_color);
        }
    }
    Ok(())
}

fn draw_bar(r: &BarEl, frame: &Frame, b: &Bindings, pm: &mut Pixmap) -> Result<(), RenderError> {
    let bb = resolve_box(&r.common, &r.size, frame, b)?;
    let value = r.value.render_number(b)?;
    let max = r
        .max
        .as_ref()
        .map(|a| a.render_number(b))
        .transpose()?
        .unwrap_or_else(|| b.get("max").as_number().unwrap_or(100.0));
    let min = r
        .min
        .as_ref()
        .map(|a| a.render_number(b))
        .transpose()?
        .unwrap_or(0.0);
    let span = (max - min).max(f64::EPSILON);
    let progress = ((value - min) / span).clamp(0.0, 1.0);
    let radius = r
        .radius
        .as_ref()
        .map(|a| a.render_number(b))
        .transpose()?
        .unwrap_or(0.0) as f32;
    let fill_color = r
        .fill
        .as_ref()
        .and_then(|a| try_render_colour(a, b))
        .or_else(|| accent_from_bindings(b))
        .unwrap_or(Colour::rgb(0xba, 0xea, 0x96));

    // Cell-mode: discrete blocks separated by gaps. Mutually exclusive
    // with the wedge — the visual idiom doesn't combine cleanly, so cell
    // mode just animates filled-count smoothly via a fractional last cell.
    if let Some(cells_attr) = &r.cells {
        let n = cells_attr.render_number(b)?.max(1.0) as u32;
        let gap = r
            .gap
            .as_ref()
            .map(|a| a.render_number(b))
            .transpose()?
            .unwrap_or(2.0) as f32;
        return draw_bar_cells(bb, radius, fill_color, n, gap, progress as f32, pm);
    }

    // Continuous-fill mode: render the settled bar, then overlay a
    // transition wedge for growing values.
    let filled = Box2 {
        w: bb.w * progress as f32,
        ..bb
    };
    fill_rounded_rect(pm, filled, radius, fill_color);

    // Wedge overlay. Only drawn when growing — `from < value` — so the
    // wedge represents new territory being claimed by the bar. For
    // shrinking values we just let the bar contract; no ghost above the
    // current level.
    let from_val = r
        .from
        .as_ref()
        .and_then(|a| a.render_number(b).ok())
        .unwrap_or(value);
    if from_val < value {
        let from_progress = ((from_val - min) / span).clamp(0.0, 1.0);
        if from_progress < progress {
            let t = b
                .get("transitionProgress")
                .as_number()
                .unwrap_or(1.0)
                .clamp(0.0, 1.0) as f32;
            // Peak tint, default 80% darker. Lerps to 0 over the transition
            // so the wedge fades into the bar by the time it settles. A
            // strong default makes the delta visible at a glance; theme
            // authors can soften with `transition="-20%"` (etc.) on the bar.
            let peak_tint = r
                .transition
                .as_ref()
                .and_then(|a| a.render(b).ok())
                .and_then(|s| parse_percentage(&s))
                .unwrap_or(-0.80);
            let live_tint = peak_tint * (1.0 - t);
            let delta_color = tint(fill_color, live_tint);
            let wedge_x = bb.x + bb.w * from_progress as f32;
            let wedge_w = bb.w * (progress - from_progress) as f32;
            // The wedge sits on top of the already-painted bar. Its right
            // edge inherits the bar's rounded end naturally because the
            // wedge box ends at `progress`, the same place the bar does.
            // The left edge is at `from_progress` mid-bar — drawn flat so
            // it abuts the settled region cleanly.
            let wedge_box = Box2 {
                x: wedge_x,
                w: wedge_w,
                ..bb
            };
            fill_partial_rounded_rect(pm, wedge_box, radius, delta_color, false, true);
        }
    }
    Ok(())
}

/// Render a bar as `n` discrete cells. Cells are equal-width, separated
/// by `gap` pixels. The cell at the progress boundary renders at a
/// fractional width so the animation stays smooth (no integer-stepping
/// flicker as `progress` interpolates).
fn draw_bar_cells(
    bb: Box2,
    radius: f32,
    fill: Colour,
    n: u32,
    gap: f32,
    progress: f32,
    pm: &mut Pixmap,
) -> Result<(), RenderError> {
    if n == 0 || bb.w <= 0.0 || bb.h <= 0.0 {
        return Ok(());
    }
    let n_f = n as f32;
    // Total gap width = gap * (n - 1), but when n == 1 there are no gaps.
    let gaps_total = gap * (n_f - 1.0).max(0.0);
    let cell_w = ((bb.w - gaps_total) / n_f).max(0.0);
    let filled_f = progress * n_f;
    let full_cells = filled_f.floor() as u32;
    let partial_frac = filled_f - full_cells as f32;
    for i in 0..n {
        let x = bb.x + (i as f32) * (cell_w + gap);
        let w = if i < full_cells {
            cell_w
        } else if i == full_cells && partial_frac > 0.0 {
            cell_w * partial_frac
        } else {
            continue;
        };
        let cell = Box2 {
            x,
            y: bb.y,
            w,
            h: bb.h,
        };
        fill_rounded_rect(pm, cell, radius, fill);
    }
    Ok(())
}

/// Apply a signed percentage tint to a colour. `amount > 0` lerps toward
/// white; `amount < 0` lerps toward black; `0` is a no-op. Magnitudes
/// outside `[-1, 1]` are clamped. Alpha is preserved.
fn tint(c: Colour, amount: f32) -> Colour {
    let a = amount.clamp(-1.0, 1.0);
    if a == 0.0 {
        return c;
    }
    let target = if a > 0.0 { 255.0 } else { 0.0 };
    let mag = a.abs();
    let blend = |x: u8| -> u8 {
        let v = (x as f32) + (target - x as f32) * mag;
        v.clamp(0.0, 255.0) as u8
    };
    Colour {
        r: blend(c.r),
        g: blend(c.g),
        b: blend(c.b),
        a: c.a,
    }
}

/// Parse `-20%`, `20%`, `-0.2`, `0.2` etc. into a fractional value in
/// `[-1, 1]`. Returns `None` if the string isn't a recognisable percentage
/// or fraction.
fn parse_percentage(s: &str) -> Option<f32> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix('%') {
        return num.trim().parse::<f32>().ok().map(|n| n / 100.0);
    }
    s.parse::<f32>().ok()
}

fn try_render_colour(a: &AttrValue, b: &Bindings) -> Option<Colour> {
    let s = a.render(b).ok()?;
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(c) = b.palette.get(s).copied() {
        return Some(c);
    }
    Colour::parse(s).ok()
}

fn accent_from_bindings(b: &Bindings) -> Option<Colour> {
    match b.get("accent") {
        crate::bindings::Value::Colour(c) => Some(c),
        crate::bindings::Value::String(s) => {
            if let Some(c) = b.palette.get(&s).copied() {
                return Some(c);
            }
            Colour::parse(&s).ok()
        }
        _ => None,
    }
}

fn with_alpha(c: Colour, mul: f32) -> Colour {
    Colour {
        r: c.r,
        g: c.g,
        b: c.b,
        a: ((c.a as f32) * mul).clamp(0.0, 255.0) as u8,
    }
}

fn fill_rounded_rect(pm: &mut Pixmap, bb: Box2, radius: f32, colour: Colour) {
    if bb.w <= 0.0 || bb.h <= 0.0 || colour.a == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color_rgba8(colour.r, colour.g, colour.b, colour.a);
    paint.anti_alias = true;
    paint.blend_mode = BlendMode::SourceOver;
    if radius <= 0.0 {
        if let Some(rect) = Rect::from_xywh(bb.x, bb.y, bb.w, bb.h) {
            pm.fill_rect(rect, &paint, Transform::identity(), None);
        }
        return;
    }
    let path = rounded_rect_path(bb, radius);
    if let Some(p) = path {
        pm.fill_path(&p, &paint, FillRule::EvenOdd, Transform::identity(), None);
    }
}

fn stroke_rounded_rect(pm: &mut Pixmap, bb: Box2, radius: f32, width: f32, colour: Colour) {
    if bb.w <= 0.0 || bb.h <= 0.0 || colour.a == 0 || width <= 0.0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color_rgba8(colour.r, colour.g, colour.b, colour.a);
    paint.anti_alias = true;
    let mut stroke = tiny_skia::Stroke::default();
    stroke.width = width;
    if let Some(p) = rounded_rect_path(bb, radius) {
        pm.stroke_path(&p, &paint, &stroke, Transform::identity(), None);
    }
}

fn rounded_rect_path(bb: Box2, radius: f32) -> Option<tiny_skia::Path> {
    rounded_rect_path_sides(bb, radius, true, true)
}

/// Build a rounded-rect path where left/right corner pairs can independently
/// be flat or rounded. Used by the bar's delta wedge (rounded right, flat
/// left) so the wedge meets the settled bar region with a clean vertical
/// boundary instead of a rounded notch.
fn rounded_rect_path_sides(
    bb: Box2,
    radius: f32,
    round_left: bool,
    round_right: bool,
) -> Option<tiny_skia::Path> {
    let r = radius.min(bb.w / 2.0).min(bb.h / 2.0).max(0.0);
    let lr = if round_left { r } else { 0.0 };
    let rr = if round_right { r } else { 0.0 };
    let mut pb = PathBuilder::new();
    let (x, y, w, h) = (bb.x, bb.y, bb.w, bb.h);
    if lr <= 0.0 && rr <= 0.0 {
        pb.push_rect(Rect::from_xywh(x, y, w, h)?);
        return pb.finish();
    }
    pb.move_to(x + lr, y);
    pb.line_to(x + w - rr, y);
    if rr > 0.0 {
        pb.cubic_to(x + w - rr * 0.45, y, x + w, y + rr * 0.45, x + w, y + rr);
    }
    pb.line_to(x + w, y + h - rr);
    if rr > 0.0 {
        pb.cubic_to(
            x + w,
            y + h - rr * 0.45,
            x + w - rr * 0.45,
            y + h,
            x + w - rr,
            y + h,
        );
    }
    pb.line_to(x + lr, y + h);
    if lr > 0.0 {
        pb.cubic_to(x + lr * 0.45, y + h, x, y + h - lr * 0.45, x, y + h - lr);
    }
    pb.line_to(x, y + lr);
    if lr > 0.0 {
        pb.cubic_to(x, y + lr * 0.45, x + lr * 0.45, y, x + lr, y);
    }
    pb.close();
    pb.finish()
}

fn fill_partial_rounded_rect(
    pm: &mut Pixmap,
    bb: Box2,
    radius: f32,
    colour: Colour,
    round_left: bool,
    round_right: bool,
) {
    if bb.w <= 0.0 || bb.h <= 0.0 || colour.a == 0 {
        return;
    }
    let mut paint = Paint::default();
    paint.set_color_rgba8(colour.r, colour.g, colour.b, colour.a);
    paint.anti_alias = true;
    paint.blend_mode = BlendMode::SourceOver;
    if let Some(p) = rounded_rect_path_sides(bb, radius, round_left, round_right) {
        pm.fill_path(&p, &paint, FillRule::EvenOdd, Transform::identity(), None);
    }
}

fn resolve_box(
    common: &Common,
    size: &Sized,
    frame: &Frame,
    b: &Bindings,
) -> Result<Box2, RenderError> {
    let w = size.width.render_length(b)?.resolve(frame.w as f64) as f32;
    let h = size.height.render_length(b)?.resolve(frame.h as f64) as f32;
    let x = resolve_x(common, frame, b, w)?;
    let y = resolve_y(common, frame, b, h)?;
    Ok(Box2 { x, y, w, h })
}

fn resolve_x(common: &Common, frame: &Frame, b: &Bindings, my_w: f32) -> Result<f32, RenderError> {
    let raw = common.x.render_length(b)?.resolve(frame.w as f64) as f32;
    let edge = common.anchor.map(|a| a.edges().0).unwrap_or(Edge::Start);
    Ok(match edge {
        Edge::Start => raw,
        Edge::Center => (frame.w - my_w) / 2.0 + raw,
        Edge::End => frame.w - raw - my_w,
    })
}

fn resolve_y(common: &Common, frame: &Frame, b: &Bindings, my_h: f32) -> Result<f32, RenderError> {
    let raw = if matches!(common.y.template, _) && common.y.raw.trim() == "center" {
        return Ok((frame.h - my_h) / 2.0);
    } else {
        common.y.render_length(b)?.resolve(frame.h as f64) as f32
    };
    let edge = common.anchor.map(|a| a.edges().1).unwrap_or(Edge::Start);
    Ok(match edge {
        Edge::Start => raw,
        Edge::Center => (frame.h - my_h) / 2.0 + raw,
        Edge::End => frame.h - raw - my_h,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bindings::{Bindings, Value};
    use crate::theme::parse;

    const DEFAULT_KDL: &str = r##"
palette {
    bg     "rgba(28,28,35,0.85)"
    fg     "#f3e8d7"
    track  "rgba(255,255,255,0.08)"
    normal "#baea96"
}
styles { style "normal" accent="$normal" }
surface { width 360; height 64; anchor "bottom"; offset 0 -56 }
scene {
    rect z=0 x=0 y=0 width="100%" height="100%" fill="$bg" radius=12
    rect z=1 x=46 y=42 width="100%-60" height=8 radius=999 fill="$track"
    bar z=2 x=46 y=42 width="100%-60" height=8 radius=999 \
        fill="$accent" min=0 max="$max" value="$value"
}
"##;

    fn make_bindings(theme: &Theme) -> Bindings {
        let payload = awob_protocol::SendPayload {
            event: "volume".into(),
            value: 50.0,
            max: 100.0,
            listener_id: Some("awob-test".into()),
            source: Some("test".into()),
            style: Some("normal".into()),
            accent: None,
            app: None,
            icon: None,
            timeout_ms: None,
            preempt: false,
        };
        let mut b = crate::bindings::build(&payload, None, None, None);
        b.palette = theme.palette.clone();
        let _ = crate::theme::apply_style(theme, &mut b, "normal");
        // Bar reads accent from bindings; map style override to a literal colour.
        if let Value::String(name) = b.get("accent") {
            if let Some(c) = b.palette.get(&name).copied() {
                b.set("accent", Value::Colour(c));
            }
        }
        b
    }

    #[test]
    fn renders_default_theme_to_pixmap() {
        let theme = parse(DEFAULT_KDL).unwrap();
        let bindings = make_bindings(&theme);
        let pm = render_to_pixmap(&theme, &bindings).unwrap();
        assert_eq!(pm.width(), theme.surface.width);
        assert_eq!(pm.height(), theme.surface.height);
        let bytes = pm.data();
        let any_nonzero = bytes.chunks_exact(4).any(|px| px[3] != 0);
        assert!(any_nonzero, "pixmap should contain non-transparent pixels");
    }

    #[test]
    fn rect_with_zero_size_is_safe() {
        let theme = parse(DEFAULT_KDL).unwrap();
        let mut b = make_bindings(&theme);
        b.set("value", Value::Number(0.0));
        let pm = render_to_pixmap(&theme, &b).unwrap();
        assert_eq!(pm.width(), theme.surface.width);
    }
}
