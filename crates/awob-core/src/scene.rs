//! Scene AST: the parsed in-memory shape of a theme's `scene.kdl`.
//!
//! Element attributes are stored as un-evaluated [`AttrValue`]s so that the
//! same parsed scene can be rendered repeatedly against different bindings
//! (one per send) without re-parsing the file.

use std::time::Duration;

use crate::bindings::{Bindings, Value};
use crate::colour::Colour;
use crate::expr::{ExprError, Template};
use crate::length::Length;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Anchor {
    TopLeft,
    Top,
    TopRight,
    Left,
    Center,
    Right,
    BottomLeft,
    Bottom,
    BottomRight,
}

impl Anchor {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "top-left" | "topleft" => Anchor::TopLeft,
            "top" => Anchor::Top,
            "top-right" | "topright" => Anchor::TopRight,
            "left" => Anchor::Left,
            "center" | "centre" => Anchor::Center,
            "right" => Anchor::Right,
            "bottom-left" | "bottomleft" => Anchor::BottomLeft,
            "bottom" => Anchor::Bottom,
            "bottom-right" | "bottomright" => Anchor::BottomRight,
            _ => return None,
        })
    }

    /// Returns (horizontal, vertical) edge anchoring as `Edge`.
    pub fn edges(self) -> (Edge, Edge) {
        use Anchor::*;
        match self {
            TopLeft => (Edge::Start, Edge::Start),
            Top => (Edge::Center, Edge::Start),
            TopRight => (Edge::End, Edge::Start),
            Left => (Edge::Start, Edge::Center),
            Center => (Edge::Center, Edge::Center),
            Right => (Edge::End, Edge::Center),
            BottomLeft => (Edge::Start, Edge::End),
            Bottom => (Edge::Center, Edge::End),
            BottomRight => (Edge::End, Edge::End),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Start,
    Center,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Margin {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
}

#[derive(Debug, Clone)]
pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub anchor: Anchor,
    pub margin: Margin,
    pub fade_in: Duration,
    pub show: Duration,
    pub fade_out: Duration,
    /// Duration the bar takes to animate its value from `$lastValue` to
    /// `$value`. Sequenced *after* `fade_in` so the bar appears at the
    /// previous value, then visibly transitions, then settles. Configured
    /// in KDL as `transition "<ms>"` alongside the other surface phases.
    pub transition: Duration,
}

impl Surface {
    /// Total visible duration: fade_in + show + fade_out.
    pub fn total(&self) -> Duration {
        self.fade_in + self.show + self.fade_out
    }
    /// Backwards-compat alias kept for callers that still think in terms of
    /// a single timeout. Equivalent to `total()`.
    pub fn timeout(&self) -> Duration {
        self.total()
    }
}

impl Default for Surface {
    fn default() -> Self {
        Self {
            width: 360,
            height: 64,
            anchor: Anchor::Bottom,
            margin: Margin {
                bottom: 56,
                ..Default::default()
            },
            // Snappy fade-in so the bar appears nearly instantly when the
            // OSD is triggered.
            fade_in: Duration::from_millis(150),
            // Long enough to read the level after rapid adjustments stop,
            // short enough not to feel sticky when you change one thing
            // and walk away.
            show: Duration::from_millis(2000),
            // Symmetric snappy fade-out.
            fade_out: Duration::from_millis(150),
            // Bar value tween, sequenced *after* fade-in. 300ms is well
            // past the eye's "abrupt vs animated" threshold and gives the
            // delta-highlight wedge a clear moment to be seen.
            transition: Duration::from_millis(300),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Style {
    pub name: String,
    /// Override entries that get merged into the bindings *vars* map when this
    /// style is active. Each value is a parsed expression evaluated lazily.
    pub overrides: Vec<(String, AttrValue)>,
}

/// One scene file's complete parsed contents (minus surface/palette/styles
/// which live alongside on the [`Theme`][crate::theme::Theme] container).
#[derive(Debug, Clone, Default)]
pub struct Scene {
    pub elements: Vec<Element>,
}

#[derive(Debug, Clone)]
pub enum Element {
    Rect(RectEl),
    Text(TextEl),
    Image(ImageEl),
    Bar(BarEl),
}

impl Element {
    pub fn z(&self) -> i32 {
        match self {
            Element::Rect(e) => e.common.z,
            Element::Text(e) => e.common.z,
            Element::Image(e) => e.common.z,
            Element::Bar(e) => e.common.z,
        }
    }
}

/// Attribute value: any element attribute can be a literal length, a static
/// template, or an expression-bearing template. Stored as a [`Template`] so
/// that it can be rendered on each frame; the result is then re-parsed as a
/// length / colour / etc. depending on the destination type.
#[derive(Debug, Clone)]
pub struct AttrValue {
    pub raw: String,
    pub template: Template,
}

impl AttrValue {
    pub fn parse(raw: impl Into<String>) -> Result<Self, ExprError> {
        let raw = raw.into();
        let template = Template::parse(&raw)?;
        Ok(Self { raw, template })
    }
    pub fn render(&self, b: &Bindings) -> Result<String, ExprError> {
        self.template.render(b)
    }
    pub fn render_length(&self, b: &Bindings) -> Result<Length, ExprError> {
        let s = self.render(b)?;
        Length::parse(&s).map_err(|e| ExprError::Type(format!("length parse `{s}`: {e}")))
    }
    pub fn render_number(&self, b: &Bindings) -> Result<f64, ExprError> {
        let s = self.render(b)?;
        s.trim()
            .parse::<f64>()
            .map_err(|_| ExprError::Type(format!("number parse `{s}`")))
    }
    pub fn render_colour(&self, b: &Bindings) -> Result<Colour, ExprError> {
        let s = self.render(b)?;
        if let Some(c) = b.palette.get(&s).copied() {
            return Ok(c);
        }
        Colour::parse(&s).map_err(|e| ExprError::Type(format!("colour parse `{s}`: {e}")))
    }
    pub fn render_value(&self, b: &Bindings) -> Result<Value, ExprError> {
        if self.template.is_static() {
            return Ok(Value::String(self.raw.clone()));
        }
        Ok(Value::String(self.render(b)?))
    }
}

#[derive(Debug, Clone)]
pub struct Common {
    pub z: i32,
    pub anchor: Option<Anchor>,
    pub x: AttrValue,
    pub y: AttrValue,
}

#[derive(Debug, Clone)]
pub struct Sized {
    pub width: AttrValue,
    pub height: AttrValue,
}

#[derive(Debug, Clone)]
pub struct RectEl {
    pub common: Common,
    pub size: Sized,
    pub fill: Option<AttrValue>,
    pub stroke: Option<AttrValue>,
    pub stroke_width: Option<AttrValue>,
    pub radius: Option<AttrValue>,
    pub shadow: Option<AttrValue>,
}

#[derive(Debug, Clone)]
pub struct TextEl {
    pub common: Common,
    pub value: AttrValue,
    pub font: Option<String>,
    pub colour: Option<AttrValue>,
    pub max_width: Option<AttrValue>,
}

#[derive(Debug, Clone)]
pub struct ImageEl {
    pub common: Common,
    pub size: Sized,
    pub src: AttrValue,
    /// Foreground colour the image should be drawn in. Mirrors the `colour`
    /// attribute on `text` so theme writers think about it the same way:
    ///
    /// * Unset: symbolic icons (`symbolic/` directory or `-symbolic` suffix)
    ///   auto-tint to the theme's `$fg`. Non-symbolic icons keep their
    ///   original colours so multicolour app icons render correctly.
    /// * Explicit colour expression (`"$fg"`, `"$accent"`, `"#ff00aa"`):
    ///   forces the icon to be flat-tinted in that colour regardless of
    ///   whether it's symbolic.
    /// * Special value `"auto"` or `"none"`: never tint — preserve the
    ///   icon's original colours even if it's symbolic.
    pub colour: Option<AttrValue>,
}

#[derive(Debug, Clone)]
pub struct BarEl {
    pub common: Common,
    pub size: Sized,
    pub fill: Option<AttrValue>,
    pub radius: Option<AttrValue>,
    pub min: Option<AttrValue>,
    pub max: Option<AttrValue>,
    pub value: AttrValue,
    /// Anchor for the transition wedge. When `from < value`, the bar renders
    /// as a settled region (min → from) plus a wedge overlay (from → value)
    /// tinted per `transition`. Default theme expression is
    /// `{$lastValue ?? $value}`, which collapses to no wedge when there's
    /// no prior value to compare to.
    pub from: Option<AttrValue>,
    /// Signed percentage tint applied to `fill` to colour the transition
    /// wedge. Negative = darker, positive = brighter, `0` = identical to
    /// fill. The configured value is the *peak* tint at the start of the
    /// value transition; it lerps to `0` over the transition so the wedge
    /// fades to match the bar by the time it settles. Accepted forms:
    /// `"-80%"`, `"40%"`, `"-0.8"`, `"0.4"`. Default: `-80%`. KDL key:
    /// `transition`.
    pub transition: Option<AttrValue>,
    /// Render the bar as `N` discrete cells separated by gaps, instead of
    /// one continuous fill. Filled count is `progress * N`; the cell at
    /// the progress boundary renders a fractional width so the animation
    /// stays smooth. When unset, the bar uses the default continuous
    /// fill. KDL key: `cells`.
    pub cells: Option<AttrValue>,
    /// Pixel gap between cells in cell-mode. Ignored when `cells` is
    /// unset. Default `2`. KDL key: `gap`.
    pub gap: Option<AttrValue>,
}
