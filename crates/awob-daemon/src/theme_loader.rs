//! Disk loader + hot-reload watcher for awob themes.
//!
//! Themes are directories containing at minimum `scene.kdl`. The loader looks
//! up theme dirs in this order:
//!
//! 1. `<root>/<name>` for each search root in order — `--themes-dir` /
//!    config `themes_dir` first, then the XDG defaults (user config dir,
//!    `$XDG_DATA_HOME`, `$XDG_DATA_DIRS` — see
//!    [`awob_core::paths::theme_search_roots`]). First root containing
//!    the theme wins; imports resolve relative to that root only.
//! 2. embedded fallback (the bundled `default` theme baked into the binary).
//!
//! Hot reload uses `notify` on the active theme directory; on any modify
//! event, the daemon re-parses and atomically swaps the active [`Theme`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use awob_core::{Theme, ThemeError, theme::LoadBudget};

pub const EMBEDDED_DEFAULT_NAME: &str = "default";

/// Bundled default — same `themes/default/scene.kdl` we ship on disk.
/// Kept self-contained (no `import`s) so it parses without disk context.
const EMBEDDED_DEFAULT_SCENE: &str = include_str!("../../../themes/default/scene.kdl");

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
    pub theme: Arc<Theme>,
    /// Directory the theme was loaded from; the icon resolver searches
    /// `<source_dir>/icons/<name>.{svg,png}` before system icon themes.
    /// `None` for the embedded default.
    pub source_dir: Option<PathBuf>,
    pub scene_path: Option<PathBuf>,
}

impl LoadedTheme {
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

/// Load a theme by name; if `force_palette` is set, merge it last
/// (later-wins-by-key) and add it to the watch list. Overlay's surface
/// and scene blocks are ignored — colour-only by design.
///
/// The first root containing `<name>/scene.kdl` wins. A parse error in
/// that copy propagates rather than falling through to a shadowed copy
/// in a later root — silently loading a different file than the one
/// being edited would make theme development maddening.
pub fn load(
    themes_roots: &[PathBuf],
    name: &str,
    force_palette: Option<&Path>,
) -> Result<LoadedTheme, LoadError> {
    let mut budget = LoadBudget::default();
    let on_disk = themes_roots.iter().find_map(|root| {
        let dir = root.join(name);
        dir.join("scene.kdl").exists().then_some(dir)
    });
    let mut loaded = if let Some(dir) = on_disk {
        let scene = dir.join("scene.kdl");
        let theme = budget.load(&scene)?;
        let scene_abs = std::fs::canonicalize(&scene).unwrap_or(scene);
        LoadedTheme {
            name: name.into(),
            theme: Arc::new(theme),
            source_dir: Some(dir),
            scene_path: Some(scene_abs),
        }
    } else if name == EMBEDDED_DEFAULT_NAME {
        load_embedded_with_budget(&mut budget)?
    } else {
        return Err(LoadError::NotFound(name.to_string()));
    };

    if let Some(overlay_path) = force_palette {
        apply_force_palette(Arc::make_mut(&mut loaded.theme), overlay_path, &mut budget)?;
    }
    Ok(loaded)
}

/// Load the embedded fallback theme. Used at cold start when the
/// configured theme can't be loaded.
pub fn load_embedded() -> Result<LoadedTheme, LoadError> {
    load_embedded_with_budget(&mut LoadBudget::default())
}

fn load_embedded_with_budget(budget: &mut LoadBudget) -> Result<LoadedTheme, LoadError> {
    let theme = budget.parse(EMBEDDED_DEFAULT_SCENE, None)?;
    Ok(LoadedTheme {
        name: EMBEDDED_DEFAULT_NAME.into(),
        theme: Arc::new(theme),
        source_dir: None,
        scene_path: None,
    })
}

/// Merge an overlay's palette + styles into `theme` last-wins-by-key
/// and append its path to `imported_files`. Existing style names are
/// replaced outright; new names are appended.
fn apply_force_palette(
    theme: &mut Theme,
    overlay_path: &Path,
    budget: &mut LoadBudget,
) -> Result<(), LoadError> {
    let overlay = budget.load(overlay_path)?;
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
        let t = load(&[], EMBEDDED_DEFAULT_NAME, None).unwrap();
        assert_eq!(t.name, "default");
        assert!(t.scene_path.is_none());
        assert_eq!(t.theme.surface.width, 360);
    }

    #[test]
    fn unknown_theme_is_not_found() {
        let err = load(&[], "no-such-theme", None).unwrap_err();
        assert!(matches!(err, LoadError::NotFound(_)));
    }

    #[test]
    fn earlier_root_shadows_later() {
        let tmp = std::env::temp_dir().join(format!("awob-tl-shadow-{}", std::process::id()));
        let user = tmp.join("user");
        let system = tmp.join("system");
        for (root, width) in [(&user, 100u32), (&system, 200u32)] {
            let dir = root.join("mytheme");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("scene.kdl"),
                format!("surface {{ width {width}; height 40 }}\nscene {{ }}"),
            )
            .unwrap();
        }
        let roots = vec![user.clone(), system.clone()];
        let t = load(&roots, "mytheme", None).unwrap();
        assert_eq!(t.theme.surface.width, 100);
        assert_eq!(
            t.source_dir.as_deref(),
            Some(user.join("mytheme").as_path())
        );

        // Only present in the later root → found there.
        std::fs::remove_dir_all(user.join("mytheme")).unwrap();
        let t = load(&roots, "mytheme", None).unwrap();
        assert_eq!(t.theme.surface.width, 200);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
    #[test]
    fn palette_overlay_shares_root_and_import_byte_budget() {
        let dir =
            std::env::temp_dir().join(format!("awob-theme-overlay-budget-{}", std::process::id()));
        let theme_dir = dir.join("budget");
        std::fs::create_dir_all(&theme_dir).unwrap();
        let size = 1024 * 1024;
        let padded = |mut source: String| {
            source.extend(std::iter::repeat_n(' ', size - source.len()));
            source
        };
        std::fs::write(
            theme_dir.join("scene.kdl"),
            padded("import \"a.kdl\"\nimport \"b.kdl\"\nscene {}\n".into()),
        )
        .unwrap();
        for file in ["a.kdl", "b.kdl"] {
            std::fs::write(theme_dir.join(file), padded("palette {}\n".into())).unwrap();
        }
        let overlay = dir.join("overlay.kdl");
        std::fs::write(&overlay, padded("palette {}\n".into())).unwrap();
        assert!(load(std::slice::from_ref(&dir), "budget", Some(&overlay)).is_ok());
        std::fs::write(
            &overlay,
            padded("import \"extra.kdl\"\npalette {}\n".into()),
        )
        .unwrap();
        std::fs::write(dir.join("extra.kdl"), "palette {}\n").unwrap();
        assert!(matches!(
            load(std::slice::from_ref(&dir), "budget", Some(&overlay)),
            Err(LoadError::Parse(ThemeError::InputLimit(_)))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
