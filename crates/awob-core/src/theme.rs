//! KDL theme parsing.
//!
//! Reads a `scene.kdl` file into a [`Theme`] object: surface settings, the
//! palette, named styles, the element tree, and transition declarations.
//!
//! Keeps parsing strict but conservative: unknown attributes on known elements
//! become a `ParseWarning`, returned alongside the parsed theme. Unknown
//! top-level blocks are an error.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::bindings::{Bindings, Value};
use crate::colour::{Colour, ColourError};
use crate::expr::ExprError;
use crate::scene::*;

#[derive(Debug, Clone)]
pub struct Theme {
    pub surface: Surface,
    pub palette: HashMap<String, Colour>,
    pub styles: Vec<Style>,
    pub scene: Scene,
    pub warnings: Vec<String>,
    /// Absolute paths of every file inlined into this theme via `import`,
    /// in order of first occurrence. The daemon's hot-reload watcher
    /// subscribes to these in addition to the main scene file.
    pub imported_files: Vec<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("kdl parse: {0}")]
    Kdl(String),
    #[error("expected `{expected}` at node `{found}`")]
    UnexpectedNode { expected: String, found: String },
    #[error("missing required attribute `{0}` on element `{1}`")]
    MissingAttr(String, String),
    #[error("expression in `{at}`: {source}")]
    Expr { at: String, source: ExprError },
    #[error("colour `{name}`: {source}")]
    Colour { name: String, source: ColourError },
    #[error("unknown element type: {0}")]
    UnknownElement(String),
    #[error("unknown top-level node: {0}")]
    UnknownTopLevel(String),
    #[error("import `{path}`: {source}")]
    Import {
        path: String,
        source: std::io::Error,
    },
    #[error("circular import detected: {0}")]
    CircularImport(String),
}

impl From<kdl::KdlError> for ThemeError {
    fn from(e: kdl::KdlError) -> Self {
        ThemeError::Kdl(e.to_string())
    }
}

/// Parse a theme that does NOT use `import` (no filesystem context required).
/// Convenient for unit tests and embedded themes parsed from string literals.
pub fn parse(src: &str) -> Result<Theme, ThemeError> {
    parse_with_base(src, None)
}

/// Parse a theme, resolving any `import "path"` directives relative to
/// `base_dir`. Imported files are read from disk and their top-level blocks
/// merged into the result. Cycles are rejected.
pub fn parse_with_base(src: &str, base_dir: Option<&Path>) -> Result<Theme, ThemeError> {
    let mut acc = ThemeAccumulator::default();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    parse_into(src, base_dir, &mut seen, &mut acc)?;
    acc.into_theme()
}

#[derive(Default)]
struct ThemeAccumulator {
    surface: Surface,
    palette: HashMap<String, Colour>,
    styles: Vec<Style>,
    scene: Scene,
    warnings: Vec<String>,
    imported_files: Vec<PathBuf>,
}

impl ThemeAccumulator {
    fn into_theme(self) -> Result<Theme, ThemeError> {
        Ok(Theme {
            surface: self.surface,
            palette: self.palette,
            styles: self.styles,
            scene: self.scene,
            warnings: self.warnings,
            imported_files: self.imported_files,
        })
    }
}

fn parse_into(
    src: &str,
    base_dir: Option<&Path>,
    seen: &mut HashSet<PathBuf>,
    acc: &mut ThemeAccumulator,
) -> Result<(), ThemeError> {
    let doc: KdlDocument = src.parse::<KdlDocument>()?;
    for node in doc.nodes() {
        match node.name().value() {
            "import" => {
                let rel = node_string(node)
                    .ok_or_else(|| ThemeError::Kdl("import requires a path string".into()))?;
                let resolved = match base_dir {
                    Some(d) => d.join(&rel),
                    None => PathBuf::from(&rel),
                };
                let abs = std::fs::canonicalize(&resolved).map_err(|e| ThemeError::Import {
                    path: resolved.display().to_string(),
                    source: e,
                })?;
                if !seen.insert(abs.clone()) {
                    return Err(ThemeError::CircularImport(abs.display().to_string()));
                }
                let content = std::fs::read_to_string(&abs).map_err(|e| ThemeError::Import {
                    path: abs.display().to_string(),
                    source: e,
                })?;
                acc.imported_files.push(abs.clone());
                let imported_base = abs.parent().map(|p| p.to_path_buf());
                parse_into(&content, imported_base.as_deref(), seen, acc)?;
            }
            "surface" => parse_surface(node, &mut acc.surface, &mut acc.warnings)?,
            "palette" => parse_palette(node, &mut acc.palette)?,
            "styles" => parse_styles(node, &mut acc.styles, &mut acc.warnings)?,
            "scene" => parse_scene(node, &mut acc.scene, &mut acc.warnings)?,
            other => return Err(ThemeError::UnknownTopLevel(other.to_string())),
        }
    }
    Ok(())
}

fn parse_surface(
    n: &KdlNode,
    s: &mut Surface,
    warnings: &mut Vec<String>,
) -> Result<(), ThemeError> {
    if let Some(children) = n.children() {
        for child in children.nodes() {
            let name = child.name().value();
            match name {
                "width" => {
                    if let Some(v) = node_int(child) {
                        s.width = v as u32
                    }
                }
                "height" => {
                    if let Some(v) = node_int(child) {
                        s.height = v as u32
                    }
                }
                "anchor" => {
                    if let Some(v) = node_string(child) {
                        s.anchor = Anchor::parse(&v)
                            .ok_or_else(|| ThemeError::Kdl(format!("unknown anchor `{v}`")))?;
                    }
                }
                "offset" => {
                    let xs: Vec<i64> = child
                        .entries()
                        .iter()
                        .filter_map(|e| match e.value() {
                            KdlValue::Integer(i) => Some(*i as i64),
                            KdlValue::Float(f) => Some(*f as i64),
                            _ => None,
                        })
                        .collect();
                    if xs.len() == 2 {
                        let (x, y) = (xs[0], xs[1]);
                        let (he, ve) = s.anchor.edges();
                        match he {
                            Edge::Start => s.margin.left = x.max(0) as u32,
                            Edge::End => s.margin.right = (-x).max(0) as u32,
                            Edge::Center => {}
                        }
                        match ve {
                            Edge::Start => s.margin.top = y.max(0) as u32,
                            Edge::End => s.margin.bottom = (-y).max(0) as u32,
                            Edge::Center => {}
                        }
                    }
                }
                "margin" => {
                    let mut m = Margin::default();
                    let mut pos = Vec::new();
                    for e in child.entries() {
                        if let Some(name) = e.name().map(|n| n.value()) {
                            let v = e.value().as_integer().unwrap_or(0).max(0) as u32;
                            match name {
                                "top" => m.top = v,
                                "right" => m.right = v,
                                "bottom" => m.bottom = v,
                                "left" => m.left = v,
                                _ => {}
                            }
                        } else if let KdlValue::Integer(v) = e.value() {
                            pos.push((*v).max(0) as u32);
                        }
                    }
                    match pos.len() {
                        1 => {
                            m.top = pos[0];
                            m.right = pos[0];
                            m.bottom = pos[0];
                            m.left = pos[0];
                        }
                        2 => {
                            m.top = pos[0];
                            m.bottom = pos[0];
                            m.right = pos[1];
                            m.left = pos[1];
                        }
                        4 => {
                            m.top = pos[0];
                            m.right = pos[1];
                            m.bottom = pos[2];
                            m.left = pos[3];
                        }
                        _ => {}
                    }
                    s.margin = m;
                }
                "timeout" => {
                    if let Some(ms) = parse_duration_ms(child) {
                        // Backwards compat: a single `timeout` sets the show
                        // duration; fade_in/fade_out keep their defaults.
                        s.show = Duration::from_millis(ms);
                    }
                }
                "fade-in" | "fade_in" => {
                    if let Some(ms) = parse_duration_ms(child) {
                        s.fade_in = Duration::from_millis(ms);
                    }
                }
                "show" => {
                    if let Some(ms) = parse_duration_ms(child) {
                        s.show = Duration::from_millis(ms);
                    }
                }
                "fade-out" | "fade_out" => {
                    if let Some(ms) = parse_duration_ms(child) {
                        s.fade_out = Duration::from_millis(ms);
                    }
                }
                "transition" | "value-transition" | "value_transition" => {
                    if let Some(ms) = parse_duration_ms(child) {
                        s.transition = Duration::from_millis(ms);
                    }
                }
                other => warnings.push(format!("unknown surface attr `{other}`")),
            }
        }
    }
    Ok(())
}

fn parse_palette(n: &KdlNode, p: &mut HashMap<String, Colour>) -> Result<(), ThemeError> {
    if let Some(children) = n.children() {
        for child in children.nodes() {
            let name = child.name().value();
            let val = node_string(child)
                .ok_or_else(|| ThemeError::Kdl(format!("palette `{name}` needs a string value")))?;
            let colour = Colour::parse(&val).map_err(|e| ThemeError::Colour {
                name: name.into(),
                source: e,
            })?;
            p.insert(name.to_string(), colour);
        }
    }
    Ok(())
}

fn parse_styles(
    n: &KdlNode,
    styles: &mut Vec<Style>,
    warnings: &mut Vec<String>,
) -> Result<(), ThemeError> {
    if let Some(children) = n.children() {
        for child in children.nodes() {
            if child.name().value() != "style" {
                warnings.push(format!("unknown styles entry `{}`", child.name().value()));
                continue;
            }
            let name = child
                .entries()
                .iter()
                .find(|e| e.name().is_none())
                .and_then(|e| e.value().as_string().map(|s| s.to_string()))
                .ok_or_else(|| ThemeError::Kdl("style needs a name".into()))?;
            let mut overrides = Vec::new();
            for e in child.entries() {
                if let Some(key) = e.name().map(|n| n.value().to_string()) {
                    let raw = entry_value_to_string(e);
                    let av = AttrValue::parse(raw).map_err(|err| ThemeError::Expr {
                        at: format!("style `{name}`.`{key}`"),
                        source: err,
                    })?;
                    overrides.push((key, av));
                }
            }
            styles.push(Style { name, overrides });
        }
    }
    Ok(())
}

fn parse_scene(
    n: &KdlNode,
    scene: &mut Scene,
    warnings: &mut Vec<String>,
) -> Result<(), ThemeError> {
    if let Some(children) = n.children() {
        for child in children.nodes() {
            scene.elements.push(parse_element(child, warnings)?);
        }
    }
    scene.elements.sort_by_key(|e| e.z());
    Ok(())
}

fn parse_element(node: &KdlNode, _warnings: &mut Vec<String>) -> Result<Element, ThemeError> {
    let kind = node.name().value();
    let common = parse_common(node)?;

    match kind {
        "rect" => {
            let size = parse_size(node)?;
            Ok(Element::Rect(RectEl {
                common,
                size,
                fill: attr(node, "fill")?,
                stroke: attr(node, "stroke")?,
                stroke_width: attr(node, "stroke-width")?,
                radius: attr(node, "radius")?,
                shadow: attr(node, "shadow")?,
            }))
        }
        "text" => {
            // Accept either `colour` (preferred) or `colour` (American alias).
            let colour = match attr(node, "colour")? {
                Some(c) => Some(c),
                None => attr(node, "color")?,
            };
            Ok(Element::Text(TextEl {
                common,
                value: req_attr(node, "value", "text")?,
                font: attr_str(node, "font"),
                colour,
                max_width: attr(node, "max-width")?,
            }))
        }
        "image" => {
            let size = parse_size(node)?;
            // Accept `colour` (preferred), `colour` (American alias), or
            // `tint` (older-style alias from early drafts).
            let colour = match attr(node, "colour")? {
                Some(c) => Some(c),
                None => match attr(node, "color")? {
                    Some(c) => Some(c),
                    None => attr(node, "tint")?,
                },
            };
            Ok(Element::Image(ImageEl {
                common,
                size,
                src: req_attr(node, "src", "image")?,
                colour,
            }))
        }
        "bar" => {
            let size = parse_size(node)?;
            Ok(Element::Bar(BarEl {
                common,
                size,
                fill: attr(node, "fill")?,
                radius: attr(node, "radius")?,
                min: attr(node, "min")?,
                max: attr(node, "max")?,
                value: req_attr(node, "value", "bar")?,
                from: attr(node, "from")?,
                transition: attr(node, "transition")?,
                cells: attr(node, "cells")?,
                gap: attr(node, "gap")?,
            }))
        }
        other => Err(ThemeError::UnknownElement(other.to_string())),
    }
}

fn parse_common(node: &KdlNode) -> Result<Common, ThemeError> {
    let z = attr_int(node, "z").unwrap_or(0) as i32;
    let anchor = attr_str(node, "anchor").as_deref().and_then(Anchor::parse);
    let x = attr(node, "x")?.unwrap_or_else(|| AttrValue::parse("0").unwrap());
    let y = attr(node, "y")?.unwrap_or_else(|| AttrValue::parse("0").unwrap());
    Ok(Common { z, anchor, x, y })
}

fn parse_size(node: &KdlNode) -> Result<Sized, ThemeError> {
    let width = attr(node, "width")?.unwrap_or_else(|| AttrValue::parse("0").unwrap());
    let height = attr(node, "height")?.unwrap_or_else(|| AttrValue::parse("0").unwrap());
    Ok(Sized { width, height })
}

// -- attribute helpers --

fn attr(node: &KdlNode, name: &str) -> Result<Option<AttrValue>, ThemeError> {
    for e in node.entries() {
        if let Some(n) = e.name() {
            if n.value() == name {
                let raw = entry_value_to_string(e);
                let av = AttrValue::parse(raw).map_err(|err| ThemeError::Expr {
                    at: format!("{}.{}", node.name().value(), name),
                    source: err,
                })?;
                return Ok(Some(av));
            }
        }
    }
    Ok(None)
}

fn req_attr(node: &KdlNode, name: &str, kind: &str) -> Result<AttrValue, ThemeError> {
    attr(node, name)?.ok_or_else(|| ThemeError::MissingAttr(name.into(), kind.into()))
}

fn attr_str(node: &KdlNode, name: &str) -> Option<String> {
    for e in node.entries() {
        if let Some(n) = e.name() {
            if n.value() == name {
                return Some(entry_value_to_string(e));
            }
        }
    }
    None
}

fn attr_int(node: &KdlNode, name: &str) -> Option<i64> {
    for e in node.entries() {
        if let Some(n) = e.name() {
            if n.value() == name {
                return e.value().as_integer().map(|i| i as i64);
            }
        }
    }
    None
}

fn entry_value_to_string(e: &kdl::KdlEntry) -> String {
    match e.value() {
        KdlValue::String(s) => s.to_string(),
        KdlValue::Integer(i) => i.to_string(),
        KdlValue::Float(f) => f.to_string(),
        KdlValue::Bool(b) => b.to_string(),
        KdlValue::Null => "null".to_string(),
    }
}

fn node_string(n: &KdlNode) -> Option<String> {
    n.entries()
        .iter()
        .find(|e| e.name().is_none())
        .map(entry_value_to_string)
}

fn node_int(n: &KdlNode) -> Option<i64> {
    n.entries()
        .iter()
        .find(|e| e.name().is_none())
        .and_then(|e| e.value().as_integer().map(|i| i as i64))
}

fn parse_duration_ms(n: &KdlNode) -> Option<u64> {
    let s = node_string(n)?;
    let s = s.trim();
    if let Some(ms) = s.strip_suffix("ms") {
        ms.trim().parse().ok()
    } else if let Some(secs) = s.strip_suffix('s') {
        secs.trim().parse::<f64>().ok().map(|f| (f * 1000.0) as u64)
    } else {
        s.parse().ok()
    }
}

/// Apply a named style's override entries onto `bindings.vars`.
pub fn apply_style(theme: &Theme, b: &mut Bindings, style: &str) -> Result<(), ExprError> {
    if let Some(s) = theme.styles.iter().find(|s| s.name == style) {
        for (k, v) in &s.overrides {
            b.set(k, Value::String(v.render(b)?));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Inline copy of the default theme used for unit-testing — `include_str!`
    /// of the on-disk file would pick up its `import` directive which can't
    /// resolve relative paths in unit-test context.
    const DEFAULT_KDL: &str = r##"
palette {
    bg     "rgba(28,28,35,0.85)"
    fg     "#f3e8d7"
    track  "rgba(255,255,255,0.08)"
    low    "#8fdc55"
    normal "#baea96"
    warn   "#e89a49"
    crit   "#dc8855"
    muted  "#6e6e75"
}
styles {
    style "low"      accent="$low"
    style "normal"   accent="$normal"
    style "warn"     accent="$warn"
    style "critical" accent="$crit"
    style "muted"    accent="$crit" alpha="0.6"
}
surface { width 360; height 64; anchor "bottom"; offset 0 -56 }
scene {
    rect z=0 x=0 y=0 width="100%" height="100%" fill="$bg" radius=12
    image z=1 src="audio-volume-high" x=14 y="center" width=22 height=22
    text z=1 value="Volume" x=46 y=14 font="Inter 14 500" color="$fg"
    rect z=1 x=46 y=42 width="100%-60" height=8 radius=999 fill="$track"
    bar z=2 x=46 y=42 width="100%-60" height=8 radius=999 \
        fill="$accent" min=0 max="$max" value="$value" from="{$lastValue ?? $value}"
}
"##;

    #[test]
    fn parses_default_theme() {
        let t = parse(DEFAULT_KDL).expect("default theme should parse");
        assert_eq!(t.surface.width, 360);
        assert_eq!(t.surface.height, 64);
        assert_eq!(t.surface.anchor, Anchor::Bottom);
        assert!(!t.palette.is_empty());
        assert!(t.palette.contains_key("bg"));
        assert!(t.palette.contains_key("normal"));
        assert!(!t.styles.is_empty());
        let names: Vec<_> = t.styles.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"normal"));
        assert!(names.contains(&"critical"));
        assert!(!t.scene.elements.is_empty());
        assert!(
            t.imported_files.is_empty(),
            "default theme has no imports yet"
        );
    }

    #[test]
    fn import_inlines_palette_and_tracks_path() {
        let dir = tempfile::tempdir().unwrap();
        let palette_path = dir.path().join("colours.kdl");
        std::fs::write(
            &palette_path,
            r##"
        palette {
            bg     "#101020"
            fg     "#eeeeee"
            accent "#00aaff"
            normal "#00aaff"
        }
        styles {
            style "normal" accent="$accent"
        }
        "##,
        )
        .unwrap();
        let scene_path = dir.path().join("scene.kdl");
        std::fs::write(
            &scene_path,
            r##"
        import "colours.kdl"
        surface { width 200; height 40 }
        scene {
            rect z=0 x=0 y=0 width="100%" height="100%" fill="$bg"
        }
        "##,
        )
        .unwrap();
        let src = std::fs::read_to_string(&scene_path).unwrap();
        let t = parse_with_base(&src, Some(dir.path())).unwrap();
        assert_eq!(t.palette.get("bg").unwrap().to_string(), "#101020");
        assert_eq!(t.palette.get("fg").unwrap().to_string(), "#eeeeee");
        assert_eq!(t.styles.len(), 1);
        assert_eq!(t.imported_files.len(), 1);
        assert!(t.imported_files[0].ends_with("colours.kdl"));
    }

    #[test]
    fn import_circular_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.kdl");
        let b = dir.path().join("b.kdl");
        std::fs::write(&a, "import \"b.kdl\"\n").unwrap();
        std::fs::write(&b, "import \"a.kdl\"\n").unwrap();
        let src = std::fs::read_to_string(&a).unwrap();
        let err = parse_with_base(&src, Some(dir.path())).unwrap_err();
        assert!(matches!(err, ThemeError::CircularImport(_)));
    }

    #[test]
    fn surface_offset_negative_y_anchored_bottom_sets_bottom_margin() {
        let src = r#"
        surface {
            width 300
            height 60
            anchor "bottom"
            offset 0 -56
        }
        "#;
        let t = parse(src).unwrap();
        assert_eq!(t.surface.margin.bottom, 56);
        assert_eq!(t.surface.margin.top, 0);
    }

    #[test]
    fn unknown_top_level_errors() {
        let src = "wat { x 1; }";
        assert!(parse(src).is_err());
    }
}
