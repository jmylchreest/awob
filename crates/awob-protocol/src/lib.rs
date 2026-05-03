//! awob wire protocol.
//!
//! IPC envelope sent over the daemon's Unix socket at `$XDG_RUNTIME_DIR/awob.sock`.
//! One JSON object per line.
//!
//! Wire format is intentionally minimal: a tagged enum for both [`Request`] and
//! [`Response`], with [`SendPayload`] carrying the full event description.
//! `event` is text-only metadata; `source` is the *only* key used for history
//! tracking on the daemon side — see the `awob-protocol-shape` decision.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 0;

pub const DEFAULT_SOCKET_NAME: &str = "awob.sock";

pub fn default_max() -> f64 {
    100.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello { protocol: u32 },
    Send(SendPayload),
    Query { source: Option<String> },
    SetTheme {
        name: String,
        /// When `true`, the daemon rewrites the active theme name into
        /// `awob.toml` so the choice survives restart. When `false`
        /// (default), the change is in-memory only.
        #[serde(default)]
        persist: bool,
    },
    Reload,
    Version,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error { message: String },
    Hello { protocol: u32, daemon_version: String },
    Query { entries: Vec<HistoryEntry> },
    Version { daemon_version: String, protocol: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendPayload {
    pub event: String,

    pub value: f64,

    #[serde(default = "default_max")]
    pub max: f64,

    /// Stable listener identity, e.g. `"awob-listener-pipewire"`. Defaults
    /// in `awob-client` to the basename of the current executable when not
    /// explicitly set. Multiple processes sending with the same
    /// `listener_id` but different `source` get a duplicate-listener
    /// warning logged by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listener_id: Option<String>,

    /// Per-process random suffix (typically 4-8 hex chars) that
    /// distinguishes one running instance of a listener from another.
    /// Together `(listener_id, source)` form the full unique session
    /// identifier. The daemon's per-source history map is keyed by
    /// `source` alone — see the `awob-protocol-shape` decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,

    /// CSS-syntax colour override applied for this send (e.g. "#ff00aa", "rgba(…)").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,

    /// `data:` URI, absolute path, or icon name. Resolved by the daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Force a one-off duration override (ms) for this surface display before dismiss.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,

    /// Whether this send may interrupt an OSD that's currently displaying a
    /// *different* `(source, event)` pair.
    ///
    /// * `true` — hot-swap: the daemon discards the visible OSD and renders
    ///   this one immediately. Right for interactive, user-initiated changes
    ///   (volume keys, brightness keys, mic-mute) where a stale battery bar
    ///   shouldn't be allowed to swallow the user's input.
    /// * `false` (default) — polite: if the active OSD is for a different
    ///   `(source, event)`, this send is queued and rendered after the
    ///   active cycle's fade-out. Right for ambient updates (battery,
    ///   network state) that shouldn't pre-empt whatever the user is
    ///   actively doing.
    ///
    /// Same `(source, event)` as the visible OSD is always treated as a
    /// continuity update regardless of this flag.
    #[serde(default)]
    pub preempt: bool,
}

impl SendPayload {
    pub fn new(event: impl Into<String>, value: f64) -> Self {
        Self {
            event: event.into(),
            value,
            max: default_max(),
            listener_id: None,
            source: None,
            style: None,
            accent: None,
            app: None,
            icon: None,
            timeout_ms: None,
            preempt: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub source: String,
    pub event: String,
    pub last_value: f64,
    pub last_max: f64,
    /// Seconds since the last update.
    pub age_seconds: f64,
    /// Stable listener identity (e.g. `"awob-listener-pipewire"`) if the
    /// sender set one. Useful for `awob query` output to distinguish which
    /// listener type produced each tracked source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listener_id: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn send_payload_default_max() {
        let p = SendPayload::new("volume", 50.0);
        assert_eq!(p.max, 100.0);
    }

    #[test]
    fn send_payload_omits_optional_fields() {
        let p = SendPayload::new("volume", 50.0);
        let json = serde_json::to_string(&p).unwrap();
        assert!(json.contains("\"event\":\"volume\""));
        assert!(json.contains("\"value\":50"));
        assert!(json.contains("\"max\":100"));
        assert!(!json.contains("source"));
        assert!(!json.contains("style"));
        assert!(!json.contains("accent"));
        assert!(!json.contains("app"));
        assert!(!json.contains("icon"));
        assert!(!json.contains("timeout_ms"));
    }

    #[test]
    fn send_payload_max_default_on_deserialize() {
        let p: SendPayload = serde_json::from_str(r#"{"event":"v","value":10}"#).unwrap();
        assert_eq!(p.max, 100.0);
    }

    #[test]
    fn request_send_round_trip() {
        let req = Request::Send(SendPayload {
            event: "volume".into(),
            value: 50.0,
            max: 200.0,
            listener_id: Some("awob-listener-pipewire".into()),
            source: Some("7a3f".into()),
            style: Some("normal".into()),
            accent: None,
            app: Some("Spotify".into()),
            icon: Some("audio-volume-high".into()),
            timeout_ms: Some(800),
            preempt: true,
        });
        let json = serde_json::to_string(&req).unwrap();
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn request_hello_round_trip() {
        let req = Request::Hello { protocol: 0 };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        let back: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn response_error_round_trip() {
        let r = Response::Error { message: "no theme".into() };
        let json = serde_json::to_string(&r).unwrap();
        let back: Response = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }
}
