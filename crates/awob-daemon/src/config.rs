//! `awob.toml` parsing.
//!
//! Optional config file at `$XDG_CONFIG_HOME/awob/awob.toml` (or whatever
//! `--config` points to) lets you set the active theme, theme directory,
//! socket path, and a list of listeners the daemon should supervise.
//!
//! Example:
//! ```toml
//! theme = "default"
//! themes_dir = "~/.config/awob/themes"
//!
//! # Auto-discovery is on by default; this opts out specific listeners.
//! [supervisor]
//! disable = ["upower"]
//!
//! # Explicit listeners are merged with auto-discovered ones (explicit
//! # entries with the same name win). Listeners that need args belong
//! # here rather than relying on auto-discovery.
//! [[listeners]]
//! name = "wob-fifo"
//! command = "awob-listener-wob"
//! args = ["--fifo", "$XDG_RUNTIME_DIR/wob.sock"]
//! ```
//!
//! All keys are optional. CLI flags take precedence over file values.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AwobConfig {
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub themes_dir: Option<String>,
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub supervisor: SupervisorConfig,
    #[serde(default)]
    pub listeners: Vec<ListenerConfig>,
}

/// Auto-discovery + opt-out controls for the listener supervisor.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SupervisorConfig {
    /// When `true` (the default), the daemon walks the known-listener
    /// registry on startup and spawns each one whose binary is on `PATH`
    /// (or sits next to the daemon binary). Explicit `[[listeners]]`
    /// entries still take precedence and override auto entries with the
    /// same `name`.
    #[serde(default = "default_auto")]
    pub auto: bool,
    /// Names of auto-discovered listeners to skip. Match the `name`
    /// strings in `KNOWN_LISTENERS` (e.g. `"pipewire"`, `"upower"`).
    /// Has no effect on explicit `[[listeners]]` entries.
    #[serde(default)]
    pub disable: Vec<String>,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            auto: default_auto(),
            disable: Vec::new(),
        }
    }
}

fn default_auto() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenerConfig {
    /// Human-readable name used in logs and the supervisor's tracking map.
    pub name: String,
    /// Path to the binary or just the name (resolved via `$PATH`).
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    /// Restart policy when the listener exits. Default `always`.
    #[serde(default)]
    pub restart: RestartPolicy,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    Always,
    OnFailure,
    Never,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("toml parse: {0}")]
    Toml(#[from] toml::de::Error),
}

impl AwobConfig {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let contents = std::fs::read_to_string(path)?;
        Ok(toml::from_str(&contents)?)
    }

    /// Try the default path (`$XDG_CONFIG_HOME/awob/awob.toml`). Returns
    /// `Ok(None)` if it doesn't exist (not an error).
    pub fn load_default() -> Result<Option<Self>, ConfigError> {
        let path = match awob_core::paths::awob_config_file() {
            Some(p) => p,
            None => return Ok(None),
        };
        if !path.exists() {
            return Ok(None);
        }
        Self::load(&path).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal() {
        let cfg: AwobConfig = toml::from_str(
            r#"
            theme = "tinct"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.theme.as_deref(), Some("tinct"));
        assert!(cfg.listeners.is_empty());
    }

    #[test]
    fn parses_listeners() {
        let cfg: AwobConfig = toml::from_str(
            r#"
            [[listeners]]
            name = "pipewire"
            command = "awob-listener-pipewire"

            [[listeners]]
            name = "wob"
            command = "awob-listener-wob"
            args = ["--fifo", "/tmp/wob.sock"]
            restart = "on-failure"
        "#,
        )
        .unwrap();
        assert_eq!(cfg.listeners.len(), 2);
        assert_eq!(cfg.listeners[0].name, "pipewire");
        assert_eq!(cfg.listeners[1].args, vec!["--fifo", "/tmp/wob.sock"]);
        assert_eq!(cfg.listeners[1].restart, RestartPolicy::OnFailure);
    }

    #[test]
    fn rejects_unknown_keys() {
        let err = toml::from_str::<AwobConfig>(
            r#"
            theme = "default"
            unknown_key = "oops"
        "#,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("unknown") || msg.contains("unknown_key"),
            "got: {msg}"
        );
    }
}
