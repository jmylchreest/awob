//! Disk loader + hot-reload watcher for awob themes.
//!
//! Themes are directories containing at minimum `scene.kdl`. The loader looks
//! up theme dirs in this order:
//!
//! 1. `<themes_root>/<name>` — user-controlled themes dir (`--themes`).
//! 2. embedded fallback (the bundled `default` theme baked into the binary).
//!
//! Hot reload uses `notify` on the active theme directory; on any modify
//! event, the daemon re-parses and atomically swaps the active [`Theme`].

use std::path::{Path, PathBuf};

use awob_core::{Theme, ThemeError, parse_theme, parse_theme_with_base};

pub const EMBEDDED_DEFAULT_NAME: &str = "default";
/// Embedded default theme. Self-contained (no `import` directives) so it
/// works even when no themes directory is on disk. The on-disk
/// `themes/default/scene.kdl` uses `import "../_palettes/tinct.kdl"` to
/// demonstrate the shared-palette pattern.
pub const EMBEDDED_DEFAULT_KDL: &str = r##"
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

surface {
    width 360
    height 64
    anchor "bottom"
    offset 0 -56
    fade-in    "150ms"
    show       "2000ms"
    fade-out   "150ms"
    transition "300ms"
}

scene {
    rect z=0 \
        x=0 y=0 width="100%" height="100%" \
        radius=12 fill="$bg" \
        shadow="0 8 24 rgba(0,0,0,0.4)"

    image z=1 src="{$icon ?? icon($event)}" \
        x=14 y="center" width=22 height=22

    text z=1 value="{$app ?? label($event)}" \
        x=46 y=14 font="Inter 14 500" colour="$fg"

    rect z=1 x=46 y=42 width="100%-60" height=8 radius=999 fill="$track"

    bar z=2 x=46 y=42 width="100%-60" height=8 radius=999 \
        fill="$accent" \
        min=0 max="$max" value="$value" from="{$lastValue ?? $value}"
}
"##;

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("theme not found: {0}")]
    NotFound(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("theme parse: {0}")]
    Parse(#[from] ThemeError),
}

#[derive(Debug, Clone)]
pub struct LoadedTheme {
    pub name: String,
    pub theme: Theme,
    /// Directory the theme was loaded from. The icon resolver consults
    /// `<source_dir>/icons/<name>.{svg,png}` before falling back to
    /// system icon themes, which is how each theme can ship its own
    /// glyphs (including its preferred `image-missing-symbolic`).
    /// `None` for the embedded default (no on-disk dir to look in).
    pub source_dir: Option<PathBuf>,
    /// Absolute path of `scene.kdl` if loaded from disk. None when the
    /// embedded fallback was used.
    pub scene_path: Option<PathBuf>,
}

impl LoadedTheme {
    /// Every file the watcher should subscribe to: the scene.kdl plus every
    /// file the theme's `import` directives transitively pulled in.
    pub fn watch_paths(&self) -> Vec<PathBuf> {
        let mut v = Vec::new();
        if let Some(p) = &self.scene_path {
            v.push(p.clone());
        }
        for imp in &self.theme.imported_files {
            v.push(imp.clone());
        }
        v
    }
}

/// Load a theme by name, optionally applying a *force-palette overlay*
/// after the theme's own palette + styles parsing.
///
/// The overlay file may declare a `palette { … }` and / or `styles
/// { … }` block; those entries are merged into the loaded theme
/// using the existing "later wins, key by key" rule. The overlay's
/// path is added to `imported_files` so the daemon's hot-reload
/// watcher tracks it alongside any imports the theme declared itself.
///
/// Surface and scene blocks in the overlay are ignored — the force-
/// palette feature is colour-only by design.
pub fn load(
    themes_root: Option<&Path>,
    name: &str,
    force_palette: Option<&Path>,
) -> Result<LoadedTheme, LoadError> {
    let mut loaded = if let Some(root) = themes_root {
        let dir = root.join(name);
        let scene = dir.join("scene.kdl");
        if scene.exists() {
            let kdl = std::fs::read_to_string(&scene)?;
            let theme = parse_theme_with_base(&kdl, Some(&dir))?;
            let scene_abs = std::fs::canonicalize(&scene).unwrap_or(scene);
            LoadedTheme {
                name: name.into(),
                theme,
                source_dir: Some(dir),
                scene_path: Some(scene_abs),
            }
        } else if name == EMBEDDED_DEFAULT_NAME {
            load_embedded()?
        } else {
            return Err(LoadError::NotFound(name.to_string()));
        }
    } else if name == EMBEDDED_DEFAULT_NAME {
        load_embedded()?
    } else {
        return Err(LoadError::NotFound(name.to_string()));
    };

    if let Some(overlay_path) = force_palette {
        apply_force_palette(&mut loaded.theme, overlay_path)?;
    }
    Ok(loaded)
}

/// Load the embedded fallback theme without consulting disk. Used as a
/// last-resort at daemon cold-start when the configured theme can't be
/// found or fails to parse — we still want to come up with *something*
/// rendered, so the user can see the OSD and reach for `awob set-theme`
/// to recover. Compiled in, so this is effectively infallible.
pub fn load_embedded() -> Result<LoadedTheme, LoadError> {
    let theme = parse_theme(EMBEDDED_DEFAULT_KDL)?;
    Ok(LoadedTheme {
        name: EMBEDDED_DEFAULT_NAME.into(),
        theme,
        source_dir: None,
        scene_path: None,
    })
}

/// Read `overlay_path`, parse it as a partial theme (palette / styles
/// only — anything else in the file is loaded but ignored), merge the
/// palette and styles into `theme` last-wins-by-key, and append the
/// canonical path to `imported_files` so the watcher picks it up.
///
/// Style merge: if the overlay declares a style with a name that the
/// underlying theme already had, the overlay's version replaces it
/// outright. Names that didn't exist before are appended.
fn apply_force_palette(theme: &mut Theme, overlay_path: &Path) -> Result<(), LoadError> {
    let content = std::fs::read_to_string(overlay_path)?;
    // Use parse_theme_with_base so the overlay can itself `import`
    // further palettes if a user wants to compose. Base dir is the
    // overlay's own parent so relative imports resolve sensibly.
    let base = overlay_path.parent();
    let overlay = parse_theme_with_base(&content, base)?;
    theme.palette.extend(overlay.palette);
    for s in overlay.styles {
        if let Some(pos) = theme.styles.iter().position(|x| x.name == s.name) {
            theme.styles[pos] = s;
        } else {
            theme.styles.push(s);
        }
    }
    let abs = std::fs::canonicalize(overlay_path).unwrap_or_else(|_| overlay_path.to_path_buf());
    if !theme.imported_files.iter().any(|p| p == &abs) {
        theme.imported_files.push(abs);
    }
    // Imports that the overlay itself triggered are already in
    // overlay.imported_files via parse_theme_with_base — copy them too
    // so the watcher tracks the full chain.
    for imp in overlay.imported_files {
        if !theme.imported_files.iter().any(|p| p == &imp) {
            theme.imported_files.push(imp);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_embedded_default() {
        let t = load(None, EMBEDDED_DEFAULT_NAME, None).unwrap();
        assert_eq!(t.name, "default");
        assert!(t.scene_path.is_none());
        assert_eq!(t.theme.surface.width, 360);
    }

    #[test]
    fn unknown_theme_is_not_found() {
        let err = load(None, "no-such-theme", None).unwrap_err();
        assert!(matches!(err, LoadError::NotFound(_)));
    }
}
