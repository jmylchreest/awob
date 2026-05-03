//! XDG / platform path helpers.
//!
//! Centralised so that any future port to nix-style profiles, BSD, macOS,
//! or Windows touches one file. Functions never panic and never expand from
//! untrusted user input; they only consult the standard environment
//! variables and well-known fallbacks.

use std::ffi::OsString;
use std::path::PathBuf;

const APP_NAME: &str = "awob";

fn env(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|v| !v.is_empty())
}

/// `$HOME`, with no fallback (returning `None` if unset is the only correct
/// answer — falling through to `/` would be unsafe).
pub fn home_dir() -> Option<PathBuf> {
    env("HOME").map(PathBuf::from)
}

/// `$XDG_CONFIG_HOME` if set and non-empty, else `$HOME/.config`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(p) = env("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(p));
    }
    home_dir().map(|h| h.join(".config"))
}

/// `$XDG_DATA_HOME` if set and non-empty, else `$HOME/.local/share`.
pub fn data_dir() -> Option<PathBuf> {
    if let Some(p) = env("XDG_DATA_HOME") {
        return Some(PathBuf::from(p));
    }
    home_dir().map(|h| h.join(".local").join("share"))
}

/// `$XDG_CACHE_HOME` if set and non-empty, else `$HOME/.cache`.
pub fn cache_dir() -> Option<PathBuf> {
    if let Some(p) = env("XDG_CACHE_HOME") {
        return Some(PathBuf::from(p));
    }
    home_dir().map(|h| h.join(".cache"))
}

/// `$XDG_STATE_HOME` if set and non-empty, else `$HOME/.local/state`.
pub fn state_dir() -> Option<PathBuf> {
    if let Some(p) = env("XDG_STATE_HOME") {
        return Some(PathBuf::from(p));
    }
    home_dir().map(|h| h.join(".local").join("state"))
}

/// `$XDG_RUNTIME_DIR`. Returns `None` if unset.
pub fn runtime_dir() -> Option<PathBuf> {
    env("XDG_RUNTIME_DIR").map(PathBuf::from)
}

/// `$XDG_DATA_DIRS` split on `:`, with the spec default
/// `/usr/local/share:/usr/share` if unset.
pub fn xdg_data_dirs() -> Vec<PathBuf> {
    let raw = env("XDG_DATA_DIRS")
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    raw.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// `$XDG_CONFIG_DIRS` split on `:`, with the spec default `/etc/xdg` if unset.
pub fn xdg_config_dirs() -> Vec<PathBuf> {
    let raw = env("XDG_CONFIG_DIRS")
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/etc/xdg".to_string());
    raw.split(':')
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect()
}

// --- awob-specific defaults ---

pub fn awob_config_dir() -> Option<PathBuf> {
    config_dir().map(|d| d.join(APP_NAME))
}

pub fn awob_config_file() -> Option<PathBuf> {
    awob_config_dir().map(|d| d.join("awob.toml"))
}

pub fn awob_themes_dir() -> Option<PathBuf> {
    awob_config_dir().map(|d| d.join("themes"))
}

pub fn awob_cache_dir() -> Option<PathBuf> {
    cache_dir().map(|d| d.join(APP_NAME))
}

pub fn awob_socket_path() -> Option<PathBuf> {
    runtime_dir().map(|d| d.join("awob.sock"))
}

// --- path expansion helpers (for config-file values) ---

/// Expand a leading `~` or `~/` to `$HOME`. Does not expand `~user/`.
pub fn expand_tilde(s: &str) -> PathBuf {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = home_dir() {
            return h.join(rest);
        }
    } else if s == "~" {
        if let Some(h) = home_dir() {
            return h;
        }
    }
    PathBuf::from(s)
}

/// Expand `$VAR` and `${VAR}` references against the process environment.
/// Unknown variables are left as-is so misconfig is visible.
pub fn expand_env(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() {
            if bytes[i + 1] == b'{' {
                if let Some(end) = bytes[i + 2..].iter().position(|&c| c == b'}') {
                    let name = &s[i + 2..i + 2 + end];
                    out.push_str(&std::env::var(name).unwrap_or_else(|_| format!("${{{name}}}")));
                    i += 2 + end + 1;
                    continue;
                }
            } else if bytes[i + 1].is_ascii_alphabetic() || bytes[i + 1] == b'_' {
                let mut j = i + 1;
                while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                    j += 1;
                }
                let name = &s[i + 1..j];
                out.push_str(&std::env::var(name).unwrap_or_else(|_| format!("${name}")));
                i = j;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

pub fn expand_config_path(s: &str) -> PathBuf {
    expand_tilde(&expand_env(s))
}

// --- icon-theme path resolution ---

/// Roots to search for icon themes, in order of preference. Always includes
/// `$XDG_DATA_HOME/icons`, `$XDG_DATA_DIRS/icons`, and `/usr/share/icons`.
/// Duplicates are removed while preserving order.
pub fn icon_search_roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Some(d) = data_dir() {
        roots.push(d.join("icons"));
    }
    for d in xdg_data_dirs() {
        roots.push(d.join("icons"));
    }
    roots.push(PathBuf::from("/usr/share/icons"));
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    roots.retain(|p| seen.insert(p.clone()));
    roots
}

/// Preferred icon-theme names, in order. Sources consulted:
/// 1. `$AWOB_ICON_THEME` env override
/// 2. `$GTK_THEME` (older GTK env)
/// 3. `gsettings get org.gnome.desktop.interface icon-theme`
/// 4. `Adwaita` and `hicolor` as last-resort fallbacks (always appended)
pub fn preferred_icon_themes() -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    if let Some(t) = std::env::var_os("AWOB_ICON_THEME") {
        v.push(t.to_string_lossy().into_owned());
    }
    if let Some(t) = std::env::var_os("GTK_THEME") {
        v.push(t.to_string_lossy().into_owned());
    }
    if let Ok(out) = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.desktop.interface", "icon-theme"])
        .output()
    {
        let s = String::from_utf8_lossy(&out.stdout);
        let s = s.trim().trim_matches('\'').trim_matches('"');
        if !s.is_empty() && !v.iter().any(|n| n == s) {
            v.push(s.to_string());
        }
    }
    if !v.iter().any(|n| n == "Adwaita") {
        v.push("Adwaita".into());
    }
    if !v.iter().any(|n| n == "hicolor") {
        v.push("hicolor".into());
    }
    v
}

/// Subdirectory search order for a target pixel size: scalable + symbolic
/// first (vector formats scale freely), then bitmap directories sorted by
/// "closest >= size" then "largest < size".
pub fn icon_size_search_order(size: u32) -> Vec<String> {
    let mut v: Vec<String> = vec!["scalable".into(), "symbolic".into()];
    let bitmap_sizes: [u32; 11] = [16, 22, 24, 32, 36, 48, 64, 96, 128, 256, 512];
    let mut larger: Vec<u32> = bitmap_sizes
        .iter()
        .copied()
        .filter(|s| *s >= size)
        .collect();
    let smaller: Vec<u32> = bitmap_sizes
        .iter()
        .copied()
        .filter(|s| *s < size)
        .rev()
        .collect();
    larger.sort();
    let chosen: Vec<u32> = larger.iter().chain(smaller.iter()).copied().collect();
    for s in chosen {
        v.push(format!("{s}x{s}"));
    }
    v
}

/// Resolve a freedesktop icon name to a file path. Tries every preferred
/// theme under every search root, with `<name>` and `<name>-symbolic`
/// variants. Returns `None` if no match exists.
pub fn find_icon_file(name: &str, size: u32) -> Option<PathBuf> {
    let categories = [
        "status",
        "devices",
        "actions",
        "places",
        "apps",
        "categories",
        "mimetypes",
        "panel",
        "legacy",
    ];
    let names = [name.to_string(), format!("{name}-symbolic")];
    let subdirs = icon_size_search_order(size);
    for root in icon_search_roots() {
        for theme in preferred_icon_themes() {
            let theme_root = root.join(&theme);
            if !theme_root.exists() {
                continue;
            }
            for sub in &subdirs {
                for cat in &categories {
                    for n in &names {
                        for ext in ["svg", "png"] {
                            let p = theme_root.join(sub).join(cat).join(format!("{n}.{ext}"));
                            if p.exists() {
                                return Some(p);
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_data_dirs_default() {
        let v = xdg_data_dirs();
        assert!(!v.is_empty());
    }

    #[test]
    fn expand_tilde_basic() {
        unsafe {
            std::env::set_var("HOME", "/home/test");
        }
        assert_eq!(expand_tilde("~"), PathBuf::from("/home/test"));
        assert_eq!(expand_tilde("~/foo"), PathBuf::from("/home/test/foo"));
        assert_eq!(expand_tilde("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_tilde("rel"), PathBuf::from("rel"));
    }

    #[test]
    fn expand_env_basic() {
        unsafe {
            std::env::set_var("AWOB_TEST_VAR", "hello");
        }
        assert_eq!(expand_env("$AWOB_TEST_VAR/world"), "hello/world");
        assert_eq!(expand_env("${AWOB_TEST_VAR}/world"), "hello/world");
        assert_eq!(expand_env("no var here"), "no var here");
        assert_eq!(
            expand_env("$AWOB_DEFINITELY_UNSET_X"),
            "$AWOB_DEFINITELY_UNSET_X"
        );
    }
}
