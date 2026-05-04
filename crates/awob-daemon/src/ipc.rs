//! Unix-socket IPC server for the awob daemon.
//!
//! Wire format: JSON-lines of [`Request`] / [`Response`] over a stream socket
//! at `$XDG_RUNTIME_DIR/awob.sock`. The socket and its parent directory are
//! locked to the running user (mode 700). Connections are short-lived: the
//! client sends one or more requests, the daemon replies one-for-one, and
//! either side may hang up at any time.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use awob_protocol::{DEFAULT_SOCKET_NAME, Request, Response};

/// Maximum size of a single JSON-line request, in bytes. Real requests
/// are well under 1 KB; 64 KB leaves headroom for unusually long
/// `theme_dir` paths or future fields without letting a misbehaving
/// local client exhaust the daemon's RAM.
const MAX_LINE_BYTES: u64 = 64 * 1024;

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
pub fn serve_connection<H>(stream: UnixStream, mut handler: H) -> std::io::Result<()>
where
    H: FnMut(Request) -> Response,
{
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        // Bound each request to MAX_LINE_BYTES so a misbehaving client
        // can't pin daemon RAM by streaming an unterminated giant line.
        // We allow one extra byte so we can distinguish "exactly at the
        // cap" (legitimate) from "ran past the cap".
        let mut limited = reader.by_ref().take(MAX_LINE_BYTES + 1);
        let n = limited.read_line(&mut line)?;
        if n == 0 {
            return Ok(());
        }
        if (n as u64) > MAX_LINE_BYTES {
            // Oversize line: send a single error and close. We don't try
            // to resync because we'd have to drain an attacker-controlled
            // amount of data to find the next newline.
            let response = Response::Error {
                message: format!("request exceeds {MAX_LINE_BYTES}-byte limit"),
            };
            let mut out = serde_json::to_vec(&response)?;
            out.push(b'\n');
            let _ = writer.write_all(&out);
            let _ = writer.flush();
            return Ok(());
        }
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(trimmed) {
            Ok(req) => handler(req),
            Err(e) => Response::Error {
                message: format!("bad request: {e}"),
            },
        };
        let mut out = serde_json::to_vec(&response)?;
        out.push(b'\n');
        writer.write_all(&out)?;
        writer.flush()?;
    }
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
        let big = vec![b'x'; (MAX_LINE_BYTES as usize) + 100];
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
}
