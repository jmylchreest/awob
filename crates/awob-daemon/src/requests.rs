//! Theme I/O is serialized separately from the state used by event requests.
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use awob_protocol::{PROTOCOL_VERSION, Request, Response};

use crate::{
    Shared, WithProtocolCheck, enumerate_themes, lock_or_recover, persist_theme_to_config,
    theme_loader, wayland,
};

pub(super) struct Requests {
    pub(super) shared: Mutex<Shared>,
    // All IPC and watcher theme mutations take this before shared, never the reverse.
    // Keep it through persistence so an older save cannot overtake a newer theme.
    pub(super) theme_operations: Mutex<()>,
}

pub(super) enum ThemeChange {
    Set { name: String, persist: bool },
    Reload,
    Palette(Option<PathBuf>),
}

#[derive(Debug, thiserror::Error)]
pub(super) enum ThemeChangeError {
    #[error(transparent)]
    Surface(#[from] wayland::SurfaceSendError),
    #[error("{0}")]
    Other(String),
}

impl Requests {
    pub(super) const fn new(shared: Shared) -> Self {
        Self {
            shared: Mutex::new(shared),
            theme_operations: Mutex::new(()),
        }
    }

    pub(super) fn handle(&self, request: Request) -> Response {
        let change = match request {
            Request::Hello { protocol } => {
                return Response::Hello {
                    protocol: PROTOCOL_VERSION,
                    daemon_version: env!("CARGO_PKG_VERSION").into(),
                }
                .with_protocol_check(protocol);
            }
            Request::Version => {
                return Response::Version {
                    daemon_version: env!("CARGO_PKG_VERSION").into(),
                    protocol: PROTOCOL_VERSION,
                };
            }
            Request::Send(payload) => {
                return lock_or_recover(&self.shared, "shared(send)").send(payload);
            }
            Request::Query { source } => {
                return lock_or_recover(&self.shared, "shared(query)").query(source);
            }
            Request::ThemeList => {
                let (roots, name) = {
                    let shared = lock_or_recover(&self.shared, "shared(theme-list)");
                    (shared.themes_roots.clone(), shared.theme.name.clone())
                };
                return Response::ThemeList {
                    themes: enumerate_themes(&roots, &name),
                };
            }
            Request::SetTheme { name, persist } => ThemeChange::Set { name, persist },
            Request::Reload => ThemeChange::Reload,
            Request::SetForcePalette { path } => ThemeChange::Palette(path.map(PathBuf::from)),
        };
        match self.change_theme(change) {
            Ok(_) => Response::Ok,
            Err(error) => Response::Error {
                message: error.to_string(),
            },
        }
    }

    pub(super) fn change_theme(&self, change: ThemeChange) -> Result<usize, ThemeChangeError> {
        self.change_theme_with(change, theme_loader::load, persist_theme_to_config)
    }

    pub(super) fn change_theme_with(
        &self,
        change: ThemeChange,
        load: impl FnOnce(
            &[PathBuf],
            &str,
            Option<&Path>,
        ) -> Result<theme_loader::LoadedTheme, theme_loader::LoadError>,
        persist: impl FnOnce(&Path, &str) -> std::io::Result<()>,
    ) -> Result<usize, ThemeChangeError> {
        let _operation = lock_or_recover(&self.theme_operations, "theme operation");
        let (roots, name, palette, config, save, context) = {
            let shared = lock_or_recover(&self.shared, "shared(theme snapshot)");
            let (name, palette, save, context) = match change {
                ThemeChange::Set { name, persist } => {
                    (name, shared.force_palette.clone(), persist, "set theme")
                }
                ThemeChange::Reload => (
                    shared.theme.name.clone(),
                    shared.force_palette.clone(),
                    false,
                    "reload",
                ),
                ThemeChange::Palette(path) => {
                    (shared.theme.name.clone(), path, false, "set force-palette")
                }
            };
            (
                shared.themes_roots.clone(),
                name,
                palette,
                shared.config_path.clone(),
                save,
                context,
            )
        };
        let theme = load(&roots, &name, palette.as_deref())
            .map_err(|error| ThemeChangeError::Other(format!("{context}: {error}")))?;
        let watched = theme.watch_paths().len();
        {
            let mut shared = lock_or_recover(&self.shared, "shared(theme publish)");
            // Admission and publication share one critical section with Send.
            shared.publish_theme(theme)?;
            shared.force_palette = palette;
        }
        if save {
            let path = config.ok_or_else(|| {
                ThemeChangeError::Other(
                    "theme set in memory but no awob.toml path is configured to persist to".into(),
                )
            })?;
            persist(&path, &name).map_err(|error| {
                ThemeChangeError::Other(format!(
                    "theme set in memory but persisting to {}: {error}",
                    path.display()
                ))
            })?;
        }
        Ok(watched)
    }
}
