//! Unix-socket IPC server for the awob daemon.
//!
//! Wire format: JSON-lines of [`Request`] / [`Response`] over a stream socket
//! at `$XDG_RUNTIME_DIR/awob.sock`. The socket and its parent directory are
//! restricted to the running user (socket mode 600, private parent). The client
//! sends one or more requests, the daemon replies one-for-one, and either side
//! may hang up at any time.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
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
    #[error("unsafe IPC path {path}: {reason}")]
    UnsafePath { path: PathBuf, reason: &'static str },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub fn default_socket_path() -> Result<PathBuf, IpcError> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").ok_or(IpcError::NoRuntimeDir)?;
    Ok(Path::new(&dir).join(DEFAULT_SOCKET_NAME))
}

pub use crate::socket_path::Server;

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
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;
    use std::thread;

    fn private_tempdir() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[test]
    fn socket_rejects_non_socket_and_symlink_without_removing_them() {
        use std::os::unix::fs::symlink;
        let dir = private_tempdir();
        let file = dir.path().join("file");
        std::fs::write(&file, "keep me").unwrap();
        assert!(Server::bind(file.clone()).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep me");
        let link = dir.path().join("link");
        symlink(&file, &link).unwrap();
        assert!(Server::bind(link.clone()).is_err());
        assert!(
            std::fs::symlink_metadata(link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let dangling = dir.path().join("dangling");
        symlink(dir.path().join("missing"), &dangling).unwrap();
        assert!(Server::bind(dangling.clone()).is_err());
        assert!(
            std::fs::symlink_metadata(dangling)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn socket_rejects_shared_or_symlink_parent() {
        use std::os::unix::fs::symlink;
        let dir = private_tempdir();
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(Server::bind(shared.join("a.sock")).is_err());
        assert_eq!(
            std::fs::metadata(&shared).unwrap().permissions().mode() & 0o777,
            0o755
        );
        let link = dir.path().join("link");
        symlink(dir.path(), &link).unwrap();
        assert!(Server::bind(link.join("a.sock")).is_err());
    }

    #[test]
    fn socket_creates_private_parent_and_restrictive_socket() {
        let dir = private_tempdir();
        let parent = dir.path().join("new").join("nested");
        let path = parent.join("a.sock");
        let server = Server::bind(path.clone()).unwrap();
        assert_eq!(
            std::fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 1);
        assert!(UnixStream::connect(server.path()).is_ok());
    }

    #[test]
    fn socket_recovers_stale_socket_but_preserves_live_one() {
        let dir = private_tempdir();
        let path = dir.path().join("a.sock");
        drop(UnixListener::bind(&path).unwrap());
        let server = Server::bind(path.clone()).unwrap();
        assert!(matches!(
            Server::bind(path.clone()),
            Err(IpcError::AlreadyRunning { .. })
        ));
        assert!(UnixStream::connect(server.path()).is_ok());
    }

    #[test]
    fn socket_drop_preserves_replacement_file_and_socket() {
        let dir = private_tempdir();
        let path = dir.path().join("a.sock");
        let server = Server::bind(path.clone()).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "replacement").unwrap();
        drop(server);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "replacement");
        std::fs::remove_file(&path).unwrap();
        let server = Server::bind(path.clone()).unwrap();
        std::fs::remove_file(&path).unwrap();
        let replacement = UnixListener::bind(&path).unwrap();
        drop(server);
        assert!(UnixStream::connect(&path).is_ok());
        drop(replacement);
    }

    #[test]
    fn socket_drop_uses_original_parent_after_directory_replacement() {
        let dir = private_tempdir();
        let parent = dir.path().join("original");
        let path = parent.join("a.sock");
        let server = Server::bind(path.clone()).unwrap();
        let moved = dir.path().join("moved");
        std::fs::rename(&parent, &moved).unwrap();
        std::fs::create_dir(&parent).unwrap();
        std::fs::write(&path, "replacement").unwrap();
        drop(server);
        assert!(!moved.join("a.sock").exists());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "replacement");
    }

    #[test]
    fn socket_concurrent_startup_keeps_one_listener_and_cleans_staging() {
        let dir = private_tempdir();
        let path = dir.path().join("a.sock");
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let path = path.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    Server::bind(path)
                })
            })
            .collect();
        let servers: Vec<_> = workers
            .into_iter()
            .filter_map(|worker| worker.join().unwrap().ok())
            .collect();
        assert_eq!(servers.len(), 1);
        assert!(UnixStream::connect(&path).is_ok());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        drop(servers);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn socket_binding_preserves_maximum_path_length() {
        use std::os::unix::ffi::OsStrExt;
        let dir = private_tempdir();
        let filename = "s".repeat(107 - dir.path().as_os_str().as_bytes().len() - 1);
        let path = dir.path().join(filename);
        let server = Server::bind(path).unwrap();
        assert!(UnixStream::connect(server.path()).is_ok());
    }

    #[test]
    fn server_bind_drop_removes_socket() {
        let dir = private_tempdir();
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
        let dir = private_tempdir();
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
        let dir = private_tempdir();
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
