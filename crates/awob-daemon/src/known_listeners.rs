//! Registry of awob's first-party listener binaries plus a helper for
//! resolving them on disk.
//!
//! Listeners that need arguments to start (e.g. `awob-listener-wob`'s
//! `--fifo`) are intentionally absent — they require explicit
//! `[[listeners]]` configuration where the user provides those args.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy)]
pub struct KnownListener {
    pub name: &'static str,
    pub binary: &'static str,
}

pub const KNOWN_LISTENERS: &[KnownListener] = &[
    KnownListener {
        name: "pipewire",
        binary: "awob-listener-pipewire",
    },
    KnownListener {
        name: "battery",
        binary: "awob-listener-battery",
    },
    KnownListener {
        name: "backlight",
        binary: "awob-listener-backlight",
    },
    KnownListener {
        name: "keyboard-backlight",
        binary: "awob-listener-keyboard-backlight",
    },
    KnownListener {
        name: "power-profile",
        binary: "awob-listener-power-profile",
    },
];

/// Resolve `binary_name` to an executable path. Checks the daemon's own
/// directory first (so dev workflows that run from `target/release` find
/// sibling listeners without modifying `PATH`), then `$PATH` entries in
/// order.
pub fn resolve_binary(binary_name: &str) -> Option<PathBuf> {
    if let Ok(daemon_path) = std::env::current_exe()
        && let Some(dir) = daemon_path.parent()
    {
        let candidate = dir.join(binary_name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    let path_env = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_env) {
        let candidate = dir.join(binary_name);
        if is_executable_file(&candidate) {
            return Some(candidate);
        }
    }
    None
}

#[cfg(unix)]
fn is_executable_file(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(p) {
        Ok(meta) if meta.is_file() => meta.permissions().mode() & 0o111 != 0,
        _ => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_non_empty() {
        // Sanity — we should always ship at least one auto-discoverable
        // listener. If this fires, something stripped the registry.
        assert!(!KNOWN_LISTENERS.is_empty());
    }

    #[test]
    fn registry_names_are_unique() {
        let names: Vec<_> = KNOWN_LISTENERS.iter().map(|k| k.name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(
            names.len(),
            sorted.len(),
            "duplicate listener name in registry"
        );
    }

    #[test]
    fn resolve_missing_binary_returns_none() {
        assert!(resolve_binary("definitely-not-a-real-binary-xyz123").is_none());
    }
}
