//! File-watcher for the active theme + every file it imported.
//! Coalesces events into a single `()` on the reload channel; debouncing
//! happens in the daemon worker.

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

    pub fn set_paths(&mut self, paths: &[PathBuf]) -> Vec<PathBuf> {
        for p in &self.watching {
            let _ = self.inner.unwatch(p);
        }
        self.watching.clear();

        // Watch parent dirs, not the files — editors do write-temp+rename
        // which would otherwise drop the inode and lose the watch.
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

    /// Reserved for future filtering — currently every covered event
    /// triggers reload and lets the parser sort relevance.
    #[allow(dead_code)]
    pub fn covers(&self, path: &Path) -> bool {
        self.watching.iter().any(|d| path.starts_with(d))
    }
}
