//! File-watcher for the active theme + every file it imported.
//!
//! Uses `notify` to subscribe to modify/create/remove events on the union
//! of `LoadedTheme::watch_paths()`. On any event we send a single coalesced
//! `()` over the reload channel — the daemon worker picks it up, debounces
//! ~80ms (compositor IDEs love to fire bursts of events on save), and
//! atomically reparses the theme.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

pub struct ThemeWatcher {
    inner: RecommendedWatcher,
    watching: Vec<PathBuf>,
}

impl ThemeWatcher {
    pub fn new(reload_tx: Sender<()>) -> notify::Result<Self> {
        let inner = notify::recommended_watcher(move |res: notify::Result<Event>| {
            if let Ok(ev) = res
                && matches!(
                    ev.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
                )
            {
                let _ = reload_tx.send(());
            }
        })?;
        Ok(Self {
            inner,
            watching: Vec::new(),
        })
    }

    /// Replace the watch set. Returns the new (filtered) watch list.
    pub fn set_paths(&mut self, paths: &[PathBuf]) -> Vec<PathBuf> {
        for p in &self.watching {
            let _ = self.inner.unwatch(p);
        }
        self.watching.clear();

        // Watch each file's parent directory rather than the file itself —
        // editors often replace files (write-temp + rename) which sends a
        // Remove for the inode even though the path content is fresh. Watching
        // the directory captures both forms reliably.
        let mut dirs: Vec<PathBuf> = Vec::new();
        for p in paths {
            if let Some(parent) = p.parent() {
                let parent = parent.to_path_buf();
                if !dirs.iter().any(|d| d == &parent) {
                    dirs.push(parent);
                }
            }
        }

        for dir in &dirs {
            if dir.exists() && self.inner.watch(dir, RecursiveMode::NonRecursive).is_ok() {
                self.watching.push(dir.clone());
            }
        }
        self.watching.clone()
    }

    /// Returns true iff `path` is inside one of the directories currently
    /// being watched, i.e. an event for this path is one we'd receive.
    /// Reserved for future filtering — we currently treat any covered event
    /// as a reload trigger and let the parser decide if the file is relevant.
    #[allow(dead_code)]
    pub fn covers(&self, path: &Path) -> bool {
        self.watching.iter().any(|d| path.starts_with(d))
    }
}
