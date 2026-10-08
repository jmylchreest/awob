//! Unix-socket IPC server for the awob daemon.
//!
//! Wire format: JSON-lines of [`Request`] / [`Response`] over a stream socket
//! at `$XDG_RUNTIME_DIR/awob.sock`. The socket and its parent directory are
//! locked to the running user (mode 700). Connections are short-lived: the
//! client sends one or more requests, the daemon replies one-for-one, and
//! either side may hang up at any time.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use awob_protocol::{DEFAULT_SOCKET_NAME, Request, Response};

/// Maximum size of a single JSON-line request, in bytes. Real requests
/// are well under 1 KB; 64 KB leaves headroom for unusually long
/// `theme_dir` paths or future fields without letting a misbehaving
/// local client exhaust the daemon's RAM.
const MAX_LINE_BYTES: usize = 64 * 1024;
const MESSAGE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONNECTIONS: usize = 64;

pub struct ConnectionLimiter {
    active: Arc<AtomicUsize>,
}

impl ConnectionLimiter {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn try_acquire(&self) -> Option<ConnectionPermit> {
        let mut count = self.active.load(Ordering::Relaxed);
        loop {
            if count >= MAX_CONNECTIONS {
                return None;
            }
            match self.active.compare_exchange_weak(
                count,
                count + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(current) => count = current,
            }
        }
        Some(ConnectionPermit {
            active: Arc::clone(&self.active),
        })
    }
}

pub struct ConnectionPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Admission failure must not let a client block the accept loop indefinitely.
pub fn reject_connection(mut stream: UnixStream) {
    let _ = write_with_deadline(
        &mut stream,
        b"{\"type\":\"error\",\"message\":\"daemon busy: active connection limit reached\"}\n",
        Duration::from_millis(100),
    );
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("XDG_RUNTIME_DIR is not set")]
    NoRuntimeDir,
    #[error("another awob-daemon is already listening on {path}")]
    AlreadyRunning { path: PathBuf },
    #[error(
        "stale socket at {path} couldn't be removed ({source}); check the parent directory's write permissions (systemd unit `ReadWritePaths=` / `RuntimeDirectory=`)"
    )]
    StaleSocketUnlink {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to bind IPC socket {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub fn default_socket_path() -> Result<PathBuf, IpcError> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(IpcError::NoRuntimeDir)?;
    Ok(Path::new(&dir).join(DEFAULT_SOCKET_NAME))
}

pub struct Server {
    listener: UnixListener,
    path: PathBuf,
}

impl Server {
    pub fn bind(path: PathBuf) -> Result<Self, IpcError> {
        // Reuse-the-socket dance. If a file exists at the bind path we have
        // to figure out whether it's a *live* daemon (someone's actually
        // accept()-ing on the other end — we should bail) or a *stale*
        // file left behind by a previous instance that died without
        // cleanup (we should unlink and rebind). The probe is a connect();
        // success means live. Only errors we treat as stale are
        // ConnectionRefused (no one accept()ing) and NotFound (file gone
        // between exists() and connect()).
        if path.exists() {
            match UnixStream::connect(&path) {
                Ok(_) => return Err(IpcError::AlreadyRunning { path }),
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                    ) =>
                {
                    if let Err(unlink_err) = std::fs::remove_file(&path) {
                        return Err(IpcError::StaleSocketUnlink {
                            path,
                            source: unlink_err,
                        });
                    }
                }
                Err(e) => {
                    return Err(IpcError::Bind { path, source: e });
                }
            }
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let listener = UnixListener::bind(&path).map_err(|e| IpcError::Bind {
            path: path.clone(),
            source: e,
        })?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
        Ok(Self { listener, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn try_clone_listener(&self) -> Result<UnixListener, IpcError> {
        Ok(self.listener.try_clone()?)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Read all newline-delimited JSON requests from a stream and dispatch each
/// through `handler`, writing the [`Response`] back as a single JSON line.
pub fn serve_connection<H>(stream: UnixStream, handler: H) -> std::io::Result<()>
where
    H: FnMut(Request) -> Response,
{
    serve_connection_with_timeout(stream, handler, MESSAGE_TIMEOUT)
}

fn serve_connection_with_timeout<H>(
    stream: UnixStream,
    mut handler: H,
    timeout: Duration,
) -> std::io::Result<()>
where
    H: FnMut(Request) -> Response,
{
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = Vec::new();
    loop {
        line.clear();
        let n = read_request(&mut reader, &mut line, timeout)?;
        if n == 0 {
            return Ok(());
        }
        if n > MAX_LINE_BYTES {
            let response = Response::Error {
                message: format!("request exceeds {MAX_LINE_BYTES}-byte limit"),
            };
            let mut out = serde_json::to_vec(&response)?;
            out.push(b'\n');
            let _ = write_with_deadline(&mut writer, &out, timeout);
            return Ok(());
        }
        let text = std::str::from_utf8(&line)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let trimmed = text.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(request) => handler(request),
            Err(error) => Response::Error {
                message: format!("bad request: {error}"),
            },
        };
        let mut out = serde_json::to_vec(&response)?;
        out.push(b'\n');
        write_with_deadline(&mut writer, &out, timeout)?;
    }
}

fn deadline_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "IPC message deadline exceeded",
    )
}

fn remaining(deadline: Instant) -> std::io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(deadline_error)
}

fn normalize_timeout(error: std::io::Error) -> std::io::Error {
    if matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ) {
        deadline_error()
    } else {
        error
    }
}

fn read_request(
    reader: &mut BufReader<UnixStream>,
    line: &mut Vec<u8>,
    timeout: Duration,
) -> std::io::Result<usize> {
    // Idle listeners can wait indefinitely within the connection cap. Once any
    // bytes arrive, a peer cannot extend the deadline by slowly dripping bytes.
    let mut deadline = None;
    loop {
        let wait = deadline.map(remaining).transpose()?;
        reader.get_ref().set_read_timeout(wait)?;
        let available = match reader.fill_buf() {
            Ok(available) => available,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(normalize_timeout(error)),
        };
        if available.is_empty() {
            return Ok(line.len());
        }
        deadline.get_or_insert_with(|| Instant::now() + timeout);
        let newline = available.iter().position(|byte| *byte == b'\n');
        let wanted = newline.map_or(available.len(), |position| position + 1);
        let count = wanted.min(MAX_LINE_BYTES + 1 - line.len());
        line.extend_from_slice(&available[..count]);
        reader.consume(count);
        if line.len() > MAX_LINE_BYTES || (newline.is_some() && count == wanted) {
            return Ok(line.len());
        }
    }
}

fn write_with_deadline(
    stream: &mut UnixStream,
    mut bytes: &[u8],
    timeout: Duration,
) -> std::io::Result<()> {
    let deadline = Instant::now() + timeout;
    while !bytes.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "IPC write returned zero",
                ));
            }
            Ok(count) => bytes = &bytes[count..],
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(normalize_timeout(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn server_bind_drop_removes_socket() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("test.sock");
        {
            let s = Server::bind(p.clone()).unwrap();
            assert!(p.exists());
            assert_eq!(s.path(), p);
        }
        assert!(!p.exists());
    }

    #[test]
    fn serve_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rt.sock");
        let server = Server::bind(p.clone()).unwrap();
        let listener = server.try_clone_listener().unwrap();
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let s = incoming.unwrap();
                serve_connection(s, |req| match req {
                    Request::Hello { protocol } => Response::Hello {
                        protocol,
                        daemon_version: "test".into(),
                    },
                    _ => Response::Ok,
                })
                .ok();
            }
        });
        let mut client = UnixStream::connect(&p).unwrap();
        writeln!(client, r#"{{"type":"hello","protocol":0}}"#).unwrap();
        let mut r = BufReader::new(client);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(line.contains("\"hello\""));
        assert!(line.contains("\"daemon_version\":\"test\""));
    }

    #[test]
    fn serve_rejects_oversize_line() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("over.sock");
        let server = Server::bind(p.clone()).unwrap();
        let listener = server.try_clone_listener().unwrap();
        thread::spawn(move || {
            for incoming in listener.incoming() {
                let s = incoming.unwrap();
                serve_connection(s, |_req| Response::Ok).ok();
            }
        });
        let mut client = UnixStream::connect(&p).unwrap();
        // Send a payload that exceeds MAX_LINE_BYTES with no newline yet.
        let big = vec![b'x'; MAX_LINE_BYTES + 100];
        client.write_all(&big).unwrap();
        client.write_all(b"\n").unwrap();
        client.flush().unwrap();
        let mut r = BufReader::new(client);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert!(
            line.contains("exceeds"),
            "expected size-limit error, got: {line}"
        );
    }
    #[test]
    fn connection_capacity_is_released_on_drop() {
        let limiter = ConnectionLimiter::new();
        let mut permits: Vec<_> = (0..64).map(|_| limiter.try_acquire().unwrap()).collect();
        assert!(limiter.try_acquire().is_none());
        permits.pop();
        assert!(limiter.try_acquire().is_some());
        drop(permits);
        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn incomplete_request_has_total_deadline() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            serve_connection_with_timeout(
                server,
                |_| panic!("partial request dispatched"),
                Duration::from_millis(100),
            )
        });
        let began = Instant::now();
        while began.elapsed() < Duration::from_millis(300) {
            if client.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            worker.join().unwrap().unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn idle_persistent_connection_outlives_message_deadline() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            serve_connection_with_timeout(server, |_| Response::Ok, Duration::from_millis(30))
        });
        thread::sleep(Duration::from_millis(80));
        client.write_all(b"{\"type\":\"version\"}\n").unwrap();
        let mut reader = BufReader::new(client);
        let mut response = String::new();
        reader.read_line(&mut response).unwrap();
        assert!(response.contains("ok"));
        drop(reader);
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn response_write_has_total_deadline() {
        let (_client, mut server) = UnixStream::pair().unwrap();
        let response = vec![b'x'; 8 * 1024 * 1024];
        assert_eq!(
            write_with_deadline(&mut server, &response, Duration::from_millis(50))
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn pipelined_requests_and_eof_terminated_json_remain_supported() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || serve_connection(server, |_| Response::Ok));
        client
            .write_all(b"{\"type\":\"version\"}\n{\"type\":\"version\"}")
            .unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let lines: Vec<_> = BufReader::new(client)
            .lines()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|line| line.contains("ok")));
        worker.join().unwrap().unwrap();
    }

    #[test]
    fn processing_time_does_not_consume_response_write_budget() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            serve_connection_with_timeout(
                server,
                |_| {
                    thread::sleep(Duration::from_millis(60));
                    Response::Ok
                },
                Duration::from_millis(20),
            )
        });
        client.write_all(b"{\"type\":\"version\"}\n").unwrap();
        client.shutdown(std::net::Shutdown::Write).unwrap();
        let mut line = String::new();
        BufReader::new(client).read_line(&mut line).unwrap();
        assert!(line.contains("ok"));
        worker.join().unwrap().unwrap();
    }
}
