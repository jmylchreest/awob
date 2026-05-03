//! awob scene engine and renderer.
//!
//! Parses theme files (`scene.kdl`) into an in-memory [`Theme`] tree, evaluates
//! per-frame [`Bindings`] against the parsed scene, and produces rendered
//! pixmaps for the daemon to push to its Wayland surface.
//!
//! Renderer (tiny-skia) lives in `render.rs` and is implemented separately
//! from the scene parser so the parser can be unit-tested in isolation.

pub mod bindings;
pub mod colour;
pub mod expr;
pub mod icon;
pub mod length;
pub mod paths;
pub mod render;
pub mod scene;
pub mod shadow;
pub mod text;
pub mod theme;

pub use bindings::{Bindings, Value};
pub use colour::Colour;
pub use expr::{Expr, ExprError, Template};
pub use length::Length;
pub use scene::{
    Anchor, AttrValue, BarEl, Common, Edge, Element, ImageEl, Margin, RectEl, Scene, Sized, Style,
    Surface, TextEl,
};
pub use theme::{
    Theme, ThemeError, apply_style, parse as parse_theme, parse_with_base as parse_theme_with_base,
};

pub use tiny_skia::Pixmap;
