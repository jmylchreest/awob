//! awob client SDK.
//!
//! Connect to the awob daemon and send events. Used by the `awob` CLI and by
//! every listener binary. Designed to be FFI-friendly: no lifetimes leak across
//! the API surface, errors are values, all ownership is explicit.
//!
//! Also exposes [`init_tracing`] — a shared `tracing-subscriber` setup so the
//! daemon and every listener share one log format and the same `RUST_LOG`
//! semantics.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub mod listener;
mod transport;
use transport::{DeadlineWriter, MAX_RESPONSE_BYTES, REQUEST_TIMEOUT, read_response_line};

pub use awob_protocol::{HistoryEntry, PROTOCOL_VERSION, Request, Response, SendPayload};

/// Idempotent shared tracing subscriber. Honours `RUST_LOG` if set,
/// otherwise applies `default_directives`.
pub fn init_tracing(default_directives: &str) {
    use tracing_subscriber::{EnvFilter, fmt};
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_directives));
    let _ = fmt()
        .with_env_filter(filter)
        .with_target(true)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .compact()
        .with_writer(std::io::stderr)
        .try_init();
}

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
    /// A response line exceeded the SDK's bounded transport buffer.
    #[error("daemon response exceeds {limit}-byte limit")]
    ResponseTooLarge { limit: usize },
    #[error("protocol version mismatch: client={client} daemon={daemon}")]
    VersionMismatch { client: u32, daemon: u32 },
}

pub type Result<T> = std::result::Result<T, Error>;

/// Use AWOB_SOCKET when set and nonempty, otherwise $XDG_RUNTIME_DIR/awob.sock.
pub fn default_socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AWOB_SOCKET").filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
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

    /// Connect to `path` if `Some`, otherwise the default socket.
    pub fn connect_or_default(path: Option<&Path>) -> Result<Self> {
        match path {
            Some(p) => Self::connect_to(p),
            None => Self::connect(),
        }
    }

    fn request(&mut self, req: &Request) -> Result<Response> {
        self.request_line(&Self::encode_request(req)?)
            .map_err(|failure| failure.error)
    }

    fn encode_request(req: &Request) -> Result<Vec<u8>> {
        let mut line = serde_json::to_vec(req)?;
        line.push(b'\n');
        Ok(line)
    }

    fn request_line(&mut self, line: &[u8]) -> std::result::Result<Response, RequestFailure> {
        self.request_line_with_limits(line, Instant::now() + REQUEST_TIMEOUT, MAX_RESPONSE_BYTES)
    }

    fn request_line_with_limits(
        &mut self,
        line: &[u8],
        deadline: Instant,
        limit: usize,
    ) -> std::result::Result<Response, RequestFailure> {
        let result = (|| {
            write_request(&mut DeadlineWriter::new(&mut self.stream, deadline), line)?;
            // Any response failure is ambiguous: the request may already have run.
            let response = (|| {
                let bytes = read_response_line(&mut self.reader, deadline, limit)?;
                if bytes.is_empty() {
                    return Err(Error::Disconnected);
                }
                let text = std::str::from_utf8(&bytes)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
                match serde_json::from_str::<Response>(text.trim_end())? {
                    Response::Error { message } => Err(Error::Daemon(message)),
                    response => Ok(response),
                }
            })();
            response.map_err(|error| RequestFailure {
                error,
                retry_safe: false,
            })
        })();
        if result
            .as_ref()
            .is_err_and(|failure| !matches!(failure.error, Error::Daemon(_)))
        {
            // A partial or malformed response cannot be resynchronized safely.
            let _ = self.stream.shutdown(std::net::Shutdown::Both);
        }
        result
    }

    /// Negotiate protocol version. Returns daemon version string on success.
    pub fn hello(&mut self) -> Result<String> {
        match self.request(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })? {
            Response::Hello {
                protocol,
                daemon_version,
            } => {
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
        match self.request(&Request::SetTheme {
            name: name.into(),
            persist,
        })? {
            Response::Ok => Ok(()),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    pub fn theme_list(&mut self) -> Result<Vec<awob_protocol::ThemeInfo>> {
        match self.request(&Request::ThemeList)? {
            Response::ThemeList { themes } => Ok(themes),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }

    /// `Some(path)` installs the overlay; `None` clears it.
    pub fn set_force_palette(&mut self, path: Option<String>) -> Result<()> {
        match self.request(&Request::SetForcePalette { path })? {
            Response::Ok => Ok(()),
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
            Response::Version {
                daemon_version,
                protocol,
            } => Ok((daemon_version, protocol)),
            other => Err(Error::UnexpectedResponse(other)),
        }
    }
}

/// Lazy connection for long-running listeners, reused between events.
///
/// A disconnected socket is reconnected once only if no request bytes were
/// written. Ambiguous failures are returned without replaying the event; the
/// following event opens a new connection.
///
/// ```no_run
/// use awob_client::{ReconnectingClient, Send};
/// let mut client = ReconnectingClient::new(None);
/// client.send(Send::new("volume", 50.0).build())?;
/// client.send(Send::new("volume", 60.0).build())?;
/// # Ok::<(), awob_client::Error>(())
/// ```
pub struct ReconnectingClient {
    socket: Option<PathBuf>,
    client: Option<Client>,
}

impl ReconnectingClient {
    /// Remember an optional socket override; connect on the first event.
    ///
    /// ```
    /// let client = awob_client::ReconnectingClient::new(None);
    /// ```
    pub const fn new(socket: Option<PathBuf>) -> Self {
        Self {
            socket,
            client: None,
        }
    }

    /// Send one event, preserving at-most-once delivery across reconnects.
    ///
    /// Errors are returned to the caller. A failure after any bytes were written
    /// is never retried, since the daemon may have displayed the event already.
    ///
    /// ```no_run
    /// let mut client = awob_client::ReconnectingClient::new(None);
    /// client.send(awob_client::Send::new("brightness", 75.0).build())?;
    /// # Ok::<(), awob_client::Error>(())
    /// ```
    pub fn send(&mut self, payload: SendPayload) -> Result<()> {
        let line = Client::encode_request(&Request::Send(payload))?;
        let deadline = Instant::now() + REQUEST_TIMEOUT;
        if self.client.is_none() {
            self.client = Some(Client::connect_or_default(self.socket.as_deref())?);
        }
        let result = self
            .client
            .as_mut()
            .expect("connection initialized")
            .request_line_with_limits(&line, deadline, MAX_RESPONSE_BYTES);
        let result = match result {
            Err(failure) if failure.retry_safe => {
                self.client = None;
                let mut client = Client::connect_or_default(self.socket.as_deref())?;
                let result = client.request_line_with_limits(&line, deadline, MAX_RESPONSE_BYTES);
                self.client = Some(client);
                result
            }
            result => result,
        };
        let result = match result {
            Ok(Response::Ok) => Ok(()),
            Ok(other) => Err(Error::UnexpectedResponse(other)),
            Err(failure) => Err(failure.error),
        };
        if result.is_err() {
            self.client = None;
        }
        result
    }
}

struct RequestFailure {
    error: Error,
    retry_safe: bool,
}

/// Track writes instead of interpreting a socket error as evidence of delivery.
/// Even a partial JSON line makes retry unsafe; only zero-byte failures qualify.
fn write_request(writer: &mut impl Write, line: &[u8]) -> std::result::Result<(), RequestFailure> {
    let mut written = 0;
    while written < line.len() {
        let error = match writer.write(&line[written..]) {
            Ok(0) => std::io::Error::from(std::io::ErrorKind::WriteZero),
            Ok(n) => {
                written += n;
                continue;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => e,
        };
        return Err(RequestFailure {
            error: error.into(),
            retry_safe: written == 0,
        });
    }
    writer.flush().map_err(|error| RequestFailure {
        error: error.into(),
        retry_safe: false,
    })
}

#[derive(Debug)]
pub struct Send {
    inner: SendPayload,
}

impl Send {
    pub fn new(event: impl Into<String>, value: f64) -> Self {
        Self {
            inner: SendPayload::new(event, value),
        }
    }
    pub fn max(mut self, max: f64) -> Self {
        self.inner.max = max;
        self
    }
    pub fn source(mut self, s: impl Into<String>) -> Self {
        self.inner.source = Some(s.into());
        self
    }
    pub fn listener_id(mut self, s: impl Into<String>) -> Self {
        self.inner.listener_id = Some(s.into());
        self
    }
    pub fn style(mut self, s: impl Into<String>) -> Self {
        self.inner.style = Some(s.into());
        self
    }
    pub fn accent(mut self, s: impl Into<String>) -> Self {
        self.inner.accent = Some(s.into());
        self
    }
    pub fn app(mut self, s: impl Into<String>) -> Self {
        self.inner.app = Some(s.into());
        self
    }
    pub fn icon(mut self, s: impl Into<String>) -> Self {
        self.inner.icon = Some(s.into());
        self
    }
    pub fn timeout_ms(mut self, t: u32) -> Self {
        self.inner.timeout_ms = Some(t);
        self
    }
    /// Hot-swap the active OSD even when a different `(source, event)`
    /// is on screen. Use for user-interactive events; leave unset for
    /// ambient updates.
    pub fn preempt(mut self, preempt: bool) -> Self {
        self.inner.preempt = preempt;
        self
    }

    /// Set `listener_id` to the basename of the current executable when
    /// unset. Used by listener binaries so the daemon can detect duplicates.
    pub fn auto_listener_id(mut self) -> Self {
        if self.inner.listener_id.is_none()
            && let Ok(p) = std::env::current_exe()
            && let Some(n) = p.file_name().and_then(|s| s.to_str())
        {
            self.inner.listener_id = Some(n.to_string());
        }
        self
    }

    pub fn build(self) -> SendPayload {
        self.inner
    }
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
    fn socket_environment_precedence() {
        // Run each environment in a child process; changing this test runner's
        // environment would race other tests and requires unsafe in Rust 2024.
        if let Ok(expected) = std::env::var("AWOB_TEST_SOCKET_EXPECTED") {
            let explicit = std::env::var_os("AWOB_TEST_SOCKET_EXPLICIT").map(PathBuf::from);
            let client = Client::connect_or_default(explicit.as_deref());
            if expected == "missing" {
                assert!(matches!(client, Err(Error::SocketMissing(_))));
            } else {
                assert_eq!(client.unwrap().version().unwrap().0, expected);
            }
            return;
        }
        let runtime = spawn_mock(|_| Response::Version {
            daemon_version: "runtime".into(),
            protocol: PROTOCOL_VERSION,
        });
        let environment = spawn_mock(|_| Response::Version {
            daemon_version: "environment".into(),
            protocol: PROTOCOL_VERSION,
        });
        let explicit = spawn_mock(|_| Response::Version {
            daemon_version: "explicit".into(),
            protocol: PROTOCOL_VERSION,
        });
        for case in [
            "environment",
            "explicit",
            "empty",
            "unset",
            "no-runtime",
            "missing",
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "tests::socket_environment_precedence",
                    "--nocapture",
                ])
                .env("XDG_RUNTIME_DIR", runtime.parent().unwrap())
                .env("AWOB_SOCKET", &environment)
                .env_remove("AWOB_TEST_SOCKET_EXPLICIT");
            let expected = match case {
                "explicit" => {
                    command.env("AWOB_TEST_SOCKET_EXPLICIT", &explicit);
                    "explicit"
                }
                "empty" => {
                    command.env("AWOB_SOCKET", "");
                    "runtime"
                }
                "unset" => {
                    command.env_remove("AWOB_SOCKET");
                    "runtime"
                }
                "no-runtime" => {
                    command.env_remove("XDG_RUNTIME_DIR");
                    "environment"
                }
                "missing" => {
                    command.env("AWOB_SOCKET", environment.with_extension("missing"));
                    "missing"
                }
                _ => "environment",
            };
            let output = command
                .env("AWOB_TEST_SOCKET_EXPECTED", expected)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
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
            _ => Response::Error {
                message: "expected Send".into(),
            },
        });
        let mut c = Client::connect_to(&sock).unwrap();
        c.send(Send::new("volume", 50.0).source("test").build())
            .unwrap();
    }

    #[test]
    fn hello_negotiates_version() {
        let sock = spawn_mock(|req| match req {
            Request::Hello { protocol } => Response::Hello {
                protocol,
                daemon_version: "0.0.1-test".into(),
            },
            _ => Response::Error {
                message: "expected Hello".into(),
            },
        });
        let mut c = Client::connect_to(&sock).unwrap();
        assert_eq!(c.hello().unwrap(), "0.0.1-test");
    }

    #[test]
    fn daemon_error_propagates() {
        let sock = spawn_mock(|_| Response::Error {
            message: "no theme".into(),
        });
        let mut c = Client::connect_to(&sock).unwrap();
        let err = c.send(Send::new("v", 1.0).build()).unwrap_err();
        assert!(matches!(err, Error::Daemon(m) if m == "no theme"));
    }
    fn serve_events(
        listener: UnixListener,
        values: Vec<f64>,
        acknowledge: bool,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for expected in values {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).unwrap() > 0);
                let Request::Send(payload) = serde_json::from_str(&line).unwrap() else {
                    panic!("expected send");
                };
                assert_eq!(payload.value, expected);
                if acknowledge {
                    stream.write_all(b"{\"type\":\"ok\"}\n").unwrap();
                }
            }
        })
    }

    #[test]
    fn reconnecting_client_reuses_one_connection() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = serve_events(listener, vec![1.0, 2.0], true);
        let mut client = ReconnectingClient::new(Some(path));
        client.send(Send::new("v", 1.0).build()).unwrap();
        client.send(Send::new("v", 2.0).build()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn reconnecting_client_delivers_first_event_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let worker = serve_events(UnixListener::bind(&path).unwrap(), vec![1.0], true);
        let mut client = ReconnectingClient::new(Some(path.clone()));
        client.send(Send::new("v", 1.0).build()).unwrap();
        worker.join().unwrap(); // old peer is closed before the next event
        std::fs::remove_file(&path).unwrap();
        let worker = serve_events(UnixListener::bind(&path).unwrap(), vec![2.0], true);
        client.send(Send::new("v", 2.0).build()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn reconnecting_client_never_replays_unacknowledged_event() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let listener = UnixListener::bind(&path).unwrap();
        let worker = serve_events(listener.try_clone().unwrap(), vec![1.0], false);
        let mut client = ReconnectingClient::new(Some(path));
        assert!(matches!(
            client.send(Send::new("v", 1.0).build()),
            Err(Error::Disconnected)
        ));
        worker.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(listener.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
        listener.set_nonblocking(false).unwrap();
        let worker = serve_events(listener, vec![2.0], true);
        client.send(Send::new("v", 2.0).build()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn reconnecting_client_recovers_after_initial_connection_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("socket");
        let mut client = ReconnectingClient::new(Some(path.clone()));
        assert!(matches!(
            client.send(Send::new("v", 1.0).build()),
            Err(Error::SocketMissing(_))
        ));
        let worker = serve_events(UnixListener::bind(&path).unwrap(), vec![2.0], true);
        client.send(Send::new("v", 2.0).build()).unwrap();
        worker.join().unwrap();
    }

    struct FailingWriter {
        bytes_left: usize,
        fail_flush: bool,
    }

    impl Write for FailingWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes_left == 0 {
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            let written = bytes.len().min(self.bytes_left);
            self.bytes_left -= written;
            Ok(written)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_flush {
                Err(std::io::ErrorKind::BrokenPipe.into())
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn request_write_failure_is_retryable_only_before_first_byte() {
        for (bytes_left, fail_flush, retry_safe) in
            [(0, false, true), (3, false, false), (100, true, false)]
        {
            let mut writer = FailingWriter {
                bytes_left,
                fail_flush,
            };
            let failure = write_request(&mut writer, b"request\n").unwrap_err();
            assert_eq!(failure.retry_safe, retry_safe);
        }
    }
    fn client_pair() -> (Client, UnixStream) {
        let (stream, server) = UnixStream::pair().unwrap();
        let reader = BufReader::new(stream.try_clone().unwrap());
        (Client { stream, reader }, server)
    }

    #[test]
    fn oversized_response_closes_transport_without_allowing_replay() {
        let (mut client, mut server) = client_pair();
        let worker = thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let _ = server.write_all(&vec![b'x'; 2048]);
        });
        let failure = client
            .request_line_with_limits(
                b"{\"type\":\"version\"}\n",
                Instant::now() + Duration::from_secs(1),
                1024,
            )
            .err()
            .unwrap();
        assert!(matches!(
            failure.error,
            Error::ResponseTooLarge { limit: 1024 }
        ));
        assert!(!failure.retry_safe);
        assert!(client.stream.write_all(b"another request").is_err());
        worker.join().unwrap();
    }

    #[test]
    fn dripping_response_cannot_extend_total_deadline() {
        let (mut client, mut server) = client_pair();
        let worker = thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            for _ in 0..30 {
                if server.write_all(b" ").is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        let failure = client
            .request_line_with_limits(
                b"{\"type\":\"version\"}\n",
                Instant::now() + Duration::from_millis(50),
                MAX_RESPONSE_BYTES,
            )
            .err()
            .unwrap();
        assert!(
            matches!(failure.error, Error::Io(ref error) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        assert!(!failure.retry_safe);
        worker.join().unwrap();
    }

    #[test]
    fn response_larger_than_request_limit_is_accepted() {
        let (mut client, mut server) = client_pair();
        let version = "v".repeat(128 * 1024);
        let expected = version.clone();
        let worker = thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let mut response = serde_json::to_vec(&Response::Version {
                daemon_version: version,
                protocol: PROTOCOL_VERSION,
            })
            .unwrap();
            response.push(b'\n');
            server.write_all(&response).unwrap();
        });
        assert_eq!(client.version().unwrap().0, expected);
        worker.join().unwrap();
    }

    #[test]
    fn partial_request_write_timeout_is_not_replayable() {
        let (mut client, _server) = client_pair();
        let line = vec![b'x'; 8 * 1024 * 1024];
        let failure = client
            .request_line_with_limits(
                &line,
                Instant::now() + Duration::from_millis(50),
                MAX_RESPONSE_BYTES,
            )
            .err()
            .unwrap();
        assert!(
            matches!(failure.error, Error::Io(ref error) if error.kind() == std::io::ErrorKind::TimedOut)
        );
        assert!(!failure.retry_safe);
    }

    #[test]
    fn response_limit_includes_the_newline_and_accepts_exact_boundary() {
        let (mut client, mut server) = client_pair();
        let response = b"{\"type\":\"ok\"}\n";
        let worker = thread::spawn(move || {
            let mut request = String::new();
            BufReader::new(server.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            server.write_all(response).unwrap();
        });
        assert!(matches!(
            client.request_line_with_limits(
                b"request\n",
                Instant::now() + Duration::from_secs(1),
                response.len()
            ),
            Ok(Response::Ok)
        ));
        worker.join().unwrap();
    }
}
