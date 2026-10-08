//! awob wob-FIFO compatibility shim.
//!
//! Creates a wob-format named pipe (default `$XDG_RUNTIME_DIR/wob.sock`)
//! and translates `<value> [<max>] [<style>]` lines into awob IPC sends.
//! Drop-in for existing `echo 50 > $WOB_SOCK` scripts.

use std::io::{BufRead, BufReader};
use std::os::fd::AsFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use awob_client::{Client, Send};
use clap::Parser;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::stat::Mode;
use nix::unistd::Uid;
use nix::unistd::mkfifo;

#[derive(Parser, Debug)]
#[command(version, about = "awob — wob FIFO compatibility listener")]
struct Cli {
    /// FIFO path. Defaults to $XDG_RUNTIME_DIR/wob.sock.
    #[arg(long)]
    fifo: Option<PathBuf>,

    /// Override the daemon socket path.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Event name attached to every send. Defaults to "wob".
    #[arg(long, default_value = "wob")]
    event: String,

    /// Stable source ID. Defaults to "wob-fifo-<pid>".
    #[arg(long)]
    source: Option<String>,
}

fn default_fifo_path() -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(runtime).join("wob.sock"))
}

const MAX_LINE_BYTES: usize = 4096;

fn validate_fifo(meta: &std::fs::Metadata) -> std::io::Result<()> {
    if !meta.file_type().is_fifo()
        || meta.uid() != Uid::effective().as_raw()
        || meta.mode() & 0o7777 != 0o600
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "FIFO must be owned by the current user, mode 0600, and not a symlink",
        ));
    }
    Ok(())
}

fn ensure_fifo(path: &std::path::Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => return validate_fifo(&meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match mkfifo(path, Mode::S_IRUSR | Mode::S_IWUSR) {
        Ok(()) | Err(nix::errno::Errno::EEXIST) => {}
        Err(e) => return Err(e.into()),
    }
    validate_fifo(&std::fs::symlink_metadata(path)?)
}

fn open_fifo(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW).bits())
        .open(path)?;
    validate_fifo(&file.metadata()?)?;
    let flags = OFlag::from_bits_truncate(fcntl(&file, FcntlArg::F_GETFL)?);
    fcntl(&file, FcntlArg::F_SETFL(flags & !OFlag::O_NONBLOCK))?;
    // A nonblocking open does not wait for a writer. Poll once so an unused
    // FIFO sleeps rather than repeatedly reopening after immediate EOF.
    loop {
        match poll(
            &mut [PollFd::new(file.as_fd(), PollFlags::POLLIN)],
            PollTimeout::NONE,
        ) {
            Ok(_) => return Ok(file),
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[derive(Debug, PartialEq)]
enum LineStatus {
    Line,
    Discarded,
    Eof,
}

fn read_bounded_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> std::io::Result<LineStatus> {
    line.clear();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok(if oversized {
                LineStatus::Discarded
            } else if line.is_empty() {
                LineStatus::Eof
            } else {
                LineStatus::Line
            });
        }
        let end = available.iter().position(|&b| b == b'\n');
        let count = end.unwrap_or(available.len());
        if !oversized {
            if count > MAX_LINE_BYTES - line.len() {
                oversized = true;
                line.clear();
            } else {
                line.extend_from_slice(&available[..count]);
            }
        }
        reader.consume(count + usize::from(end.is_some()));
        if end.is_some() {
            return Ok(if oversized {
                LineStatus::Discarded
            } else {
                LineStatus::Line
            });
        }
    }
}

fn parse_line(s: &str) -> Option<(f64, Option<f64>, Option<String>)> {
    let mut it = s.split_whitespace();
    let value: f64 = it.next()?.parse().ok()?;
    let mut max = None;
    let mut style = None;
    for tok in it {
        if let Ok(n) = tok.parse::<f64>()
            && max.is_none()
        {
            max = Some(n);
            continue;
        }
        style = Some(tok.to_string());
        break;
    }
    Some((value, max, style))
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let fifo = cli
        .fifo
        .clone()
        .or_else(default_fifo_path)
        .ok_or("XDG_RUNTIME_DIR not set; pass --fifo")?;
    ensure_fifo(&fifo)?;
    tracing::info!("fifo={}", fifo.display());

    let source = cli
        .source
        .unwrap_or_else(|| format!("wob-fifo-{}", std::process::id()));
    tracing::info!("source={source}");

    let socket = cli.socket.as_deref();
    // Hold the connection across lines; reconnect lazily on send error.
    let mut client: Option<Client> = None;

    loop {
        let f = match open_fifo(&fifo) {
            Ok(f) => f,
            Err(e) => {
                tracing::info!("open fifo: {e}");
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        let mut reader = BufReader::new(f);
        let mut line = Vec::with_capacity(MAX_LINE_BYTES);
        loop {
            match read_bounded_line(&mut reader, &mut line) {
                Ok(LineStatus::Eof) => break,
                Ok(LineStatus::Discarded) => {
                    tracing::info!("discarded oversized FIFO line");
                    continue;
                }
                Ok(LineStatus::Line) => {}
                Err(e) => {
                    tracing::info!("read: {e}");
                    break;
                }
            }
            let Ok(line) = std::str::from_utf8(&line) else {
                tracing::info!("discarded non-UTF-8 FIFO line");
                continue;
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((value, max, style)) = parse_line(trimmed) else {
                tracing::info!("bad line `{trimmed}`");
                continue;
            };
            let mut s = Send::new(&cli.event, value)
                .listener_id("awob-listener-wob")
                .source(&source)
                // wob writers are user-driven; show immediately, don't queue.
                .preempt(true);
            if let Some(m) = max {
                s = s.max(m);
            }
            if let Some(st) = style {
                s = s.style(st);
            }
            if client.is_none() {
                match Client::connect_or_default(socket) {
                    Ok(c) => client = Some(c),
                    Err(e) => {
                        tracing::info!("connect: {e}");
                        continue;
                    }
                }
            }
            if let Some(c) = client.as_mut()
                && let Err(e) = c.send(s.build())
            {
                tracing::info!("send: {e}");
                client = None;
            }
        }
    }
}

fn main() -> ExitCode {
    awob_client::init_tracing("info");
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "awob-listener-wob starting"
    );
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::info!("{e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn just_value() {
        assert_eq!(parse_line("50"), Some((50.0, None, None)));
    }
    #[test]
    fn value_and_max() {
        assert_eq!(parse_line("50 200"), Some((50.0, Some(200.0), None)));
    }
    #[test]
    fn value_and_style() {
        assert_eq!(
            parse_line("50 normal"),
            Some((50.0, None, Some("normal".into())))
        );
    }
    #[test]
    fn value_max_style() {
        assert_eq!(
            parse_line("50 200 critical"),
            Some((50.0, Some(200.0), Some("critical".into())))
        );
    }
    #[test]
    fn bad() {
        assert!(parse_line("not a number").is_none());
    }
    #[test]
    fn oversized_line_is_drained_without_retaining_it() {
        let mut input = vec![b'x'; MAX_LINE_BYTES * 100];
        input.extend_from_slice(b"\n50\n");
        let mut reader = BufReader::with_capacity(97, input.as_slice());
        let mut line = Vec::with_capacity(MAX_LINE_BYTES);
        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineStatus::Discarded
        );
        assert!(line.capacity() <= MAX_LINE_BYTES);
        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineStatus::Line
        );
        assert_eq!(line, b"50");
        assert_eq!(
            read_bounded_line(&mut reader, &mut line).unwrap(),
            LineStatus::Eof
        );
    }

    #[test]
    fn exact_limit_and_eof_are_supported() {
        let input = vec![b'x'; MAX_LINE_BYTES];
        let mut line = Vec::with_capacity(MAX_LINE_BYTES);
        assert_eq!(
            read_bounded_line(&mut input.as_slice(), &mut line).unwrap(),
            LineStatus::Line
        );
        assert_eq!(line.len(), MAX_LINE_BYTES);
        let input = vec![b'x'; MAX_LINE_BYTES + 1];
        assert_eq!(
            read_bounded_line(&mut input.as_slice(), &mut line).unwrap(),
            LineStatus::Discarded
        );
    }

    #[test]
    fn existing_regular_files_and_symlinks_are_preserved_and_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("regular");
        std::fs::write(&regular, b"keep me").unwrap();
        assert!(ensure_fifo(&regular).is_err());
        assert_eq!(std::fs::read(&regular).unwrap(), b"keep me");
        let fifo = dir.path().join("fifo");
        ensure_fifo(&fifo).unwrap();
        ensure_fifo(&fifo).unwrap();
        let link = dir.path().join("link");
        symlink(&fifo, &link).unwrap();
        assert!(ensure_fifo(&link).is_err());
        assert!(open_fifo(&link).is_err());
        std::fs::set_permissions(&fifo, std::fs::Permissions::from_mode(0o622)).unwrap();
        assert!(ensure_fifo(&fifo).is_err());
        assert!(open_fifo(&fifo).is_err());
    }

    #[test]
    fn opened_fifo_delivers_writer_data() {
        use std::io::{Read, Write};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fifo");
        ensure_fifo(&path).unwrap();
        let writer_path = path.clone();
        let writer = std::thread::spawn(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .open(writer_path)
                .unwrap()
                .write_all(b"50\n")
                .unwrap();
        });
        let mut file = open_fifo(&path).unwrap();
        let mut data = String::new();
        file.read_to_string(&mut data).unwrap();
        assert_eq!(data, "50\n");
        writer.join().unwrap();
    }
}
