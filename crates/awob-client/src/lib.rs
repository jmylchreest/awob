//! awob client SDK.
//!
//! Connect to the awob daemon and send events. Used by the `awob` CLI and by
//! every listener binary. Designed to be FFI-friendly: no lifetimes leak across
//! the API surface, errors are values, all ownership is explicit.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub use awob_protocol::{HistoryEntry, PROTOCOL_VERSION, Request, Response, SendPayload};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("XDG_RUNTIME_DIR is not set; cannot locate awob socket")]
    NoRuntimeDir,
    #[error("daemon socket not found at {0}")]
    SocketMissing(PathBuf),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("daemon returned error: {0}")]
    Daemon(String),
    #[error("unexpected response from daemon: {0:?}")]
    UnexpectedResponse(Response),
    #[error("daemon closed the connection without responding")]
    Disconnected,
    #[error("protocol version mismatch: client={client} daemon={daemon}")]
    VersionMismatch { client: u32, daemon: u32 },
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn default_socket_path() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(Error::NoRuntimeDir)?;
    Ok(Path::new(&dir).join(awob_protocol::DEFAULT_SOCKET_NAME))
}

pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    pub fn connect() -> Result<Self> {
        let path = default_socket_path()?;
        Self::connect_to(&path)
    }

    pub fn connect_to(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(Error::SocketMissing(path.to_path_buf()));
        }
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self { stream, reader })
    }

    fn request(&mut self, req: &Request) -> Result<Response> {
        let mut line = serde_json::to_vec(req)?;
        line.push(b'\n');
        self.stream.write_all(&line)?;
        self.stream.flush()?;
        let mut buf = String::new();
        let n = self.reader.read_line(&mut buf)?;
        if n == 0 {
            return Err(Error::Disconnected);
        }
        let resp: Response = serde_json::from_str(buf.trim_end())?;
        match resp {
            Response::Error { message } => Err(Error::Daemon(message)),
            other => Ok(other),
        }
    }

    /// Negotiate protocol version. Returns daemon version string on success.
    pub fn hello(&mut self) -> Result<String> {
        match self.request(&Request::Hello { protocol: PROTOCOL_VERSION })? {
            Response::Hello { protocol, daemon_version } => {
                if protocol != PROTOCOL_VERSION {
                    return Err(Error::VersionMismatch {
                        client: PROTOCOL_VERSION,
                        daemon: protocol,
                    });
                }
                Ok(daemon_version)
            }
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    pub fn send(&mut self, payload: SendPayload) -> Result<()> {
        match self.request(&Request::Send(payload))? {
            Response::Ok => Ok(()),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    pub fn query(&mut self, source: Option<String>) -> Result<Vec<HistoryEntry>> {
        match self.request(&Request::Query { source })? {
            Response::Query { entries } => Ok(entries),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    pub fn set_theme(&mut self, name: impl Into<String>) -> Result<()> {
        self.set_theme_with(name, false)
    }

    /// Set the active theme, optionally persisting the choice to
    /// `awob.toml` so it survives daemon restarts.
    pub fn set_theme_with(&mut self, name: impl Into<String>, persist: bool) -> Result<()> {
        match self.request(&Request::SetTheme { name: name.into(), persist })? {
            Response::Ok => Ok(()),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    /// List every theme the daemon can resolve, plus the embedded
    /// fallback. Each entry carries `active`, `source`, optional
    /// `description` from the theme's `manifest.toml`.
    pub fn theme_list(&mut self) -> Result<Vec<awob_protocol::ThemeInfo>> {
        match self.request(&Request::ThemeList)? {
            Response::ThemeList { themes } => Ok(themes),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    pub fn reload(&mut self) -> Result<()> {
        match self.request(&Request::Reload)? {
            Response::Ok => Ok(()),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    pub fn version(&mut self) -> Result<(String, u32)> {
        match self.request(&Request::Version)? {
            Response::Version { daemon_version, protocol } => Ok((daemon_version, protocol)),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }
}

#[derive(Debug)]
pub struct Send {
    inner: SendPayload,
}

impl Send {
    pub fn new(event: impl Into<String>, value: f64) -> Self {
        Self { inner: SendPayload::new(event, value) }
    }
    pub fn max(mut self, max: f64) -> Self { self.inner.max = max; self }
    pub fn source(mut self, s: impl Into<String>) -> Self { self.inner.source = Some(s.into()); self }
    pub fn listener_id(mut self, s: impl Into<String>) -> Self { self.inner.listener_id = Some(s.into()); self }
    pub fn style(mut self, s: impl Into<String>) -> Self { self.inner.style = Some(s.into()); self }
    pub fn accent(mut self, s: impl Into<String>) -> Self { self.inner.accent = Some(s.into()); self }
    pub fn app(mut self, s: impl Into<String>) -> Self { self.inner.app = Some(s.into()); self }
    pub fn icon(mut self, s: impl Into<String>) -> Self { self.inner.icon = Some(s.into()); self }
    pub fn timeout_ms(mut self, t: u32) -> Self { self.inner.timeout_ms = Some(t); self }
    /// Mark this send as user-interactive: it'll hot-swap the active OSD
    /// even if a different `(source, event)` is currently displayed.
    /// Right for volume/brightness/mic-mute key presses. Ambient sources
    /// (battery, network) leave this unset.
    pub fn preempt(mut self, preempt: bool) -> Self { self.inner.preempt = preempt; self }

    /// Fill in `listener_id` with the basename of the current executable
    /// (e.g. `"awob"`, `"awob-listener-pipewire"`) if it isn't already set.
    /// Listener binaries call this on every send so the daemon can detect
    /// duplicate listener instances.
    pub fn auto_listener_id(mut self) -> Self {
        if self.inner.listener_id.is_none() {
            if let Ok(p) = std::env::current_exe() {
                if let Some(n) = p.file_name().and_then(|s| s.to_str()) {
                    self.inner.listener_id = Some(n.to_string());
                }
            }
        }
        self
    }

    pub fn build(self) -> SendPayload { self.inner }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn spawn_mock(handler: impl Fn(Request) -> Response + std::marker::Send + 'static) -> PathBuf {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("awob.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let path_clone = path.clone();
        std::mem::forget(dir);
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let mut s = incoming.unwrap();
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                while r.read_line(&mut line).unwrap() > 0 {
                    let req: Request = serde_json::from_str(line.trim_end()).unwrap();
                    let resp = handler(req);
                    let mut out = serde_json::to_vec(&resp).unwrap();
                    out.push(b'\n');
                    s.write_all(&out).unwrap();
                    line.clear();
                }
            }
        });
        path_clone
    }

    #[test]
    fn send_round_trips_through_socket() {
        let sock = spawn_mock(|req| match req {
            Request::Send(p) => {
                assert_eq!(p.event, "volume");
                assert_eq!(p.value, 50.0);
                assert_eq!(p.max, 100.0);
                assert_eq!(p.source.as_deref(), Some("test"));
                Response::Ok
            }
            _ => Response::Error { message: "expected Send".into() },
        });
        let mut c = Client::connect_to(&sock).unwrap();
        c.send(Send::new("volume", 50.0).source("test").build()).unwrap();
    }

    #[test]
    fn hello_negotiates_version() {
        let sock = spawn_mock(|req| match req {
            Request::Hello { protocol } => Response::Hello {
                protocol,
                daemon_version: "0.0.1-test".into(),
            },
            _ => Response::Error { message: "expected Hello".into() },
        });
        let mut c = Client::connect_to(&sock).unwrap();
        assert_eq!(c.hello().unwrap(), "0.0.1-test");
    }

    #[test]
    fn daemon_error_propagates() {
        let sock = spawn_mock(|_| Response::Error { message: "no theme".into() });
        let mut c = Client::connect_to(&sock).unwrap();
        let err = c.send(Send::new("v", 1.0).build()).unwrap_err();
        assert!(matches!(err, Error::Daemon(m) if m == "no theme"));
    }
}
