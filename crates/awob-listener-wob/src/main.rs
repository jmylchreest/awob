//! awob wob-FIFO compatibility shim.
//!
//! Creates a wob-format named pipe (default `$XDG_RUNTIME_DIR/wob.sock`) and
//! translates incoming `<value> [<max>] [<style>]` lines into awob IPC sends.
//! Intended as a drop-in replacement for `tail -f $WOB_SOCK | wob` style
//! consumers — your existing scripts that `echo 50 > $XDG_RUNTIME_DIR/wob.sock`
//! keep working unmodified.
//!
//! See workspace decision `awob-wob-compat`.

use std::io::{BufRead, BufReader};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use awob_client::{Client, Send};
use clap::Parser;
use nix::sys::stat::Mode;
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

fn ensure_fifo(path: &std::path::Path) -> std::io::Result<()> {
    if let Ok(meta) = std::fs::metadata(path) {
        let ft = meta.file_type();
        use std::os::unix::fs::FileTypeExt;
        if ft.is_fifo() {
            return Ok(());
        }
        std::fs::remove_file(path)?;
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    mkfifo(path, Mode::S_IRUSR | Mode::S_IWUSR)
        .map_err(|e| std::io::Error::other(format!("mkfifo: {e}")))?;
    Ok(())
}

fn parse_line(s: &str) -> Option<(f64, Option<f64>, Option<String>)> {
    let mut it = s.split_whitespace();
    let value: f64 = it.next()?.parse().ok()?;
    let mut max = None;
    let mut style = None;
    for tok in it {
        if let Ok(n) = tok.parse::<f64>() {
            if max.is_none() {
                max = Some(n);
                continue;
            }
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
    tracing::info!("awob-listener-wob: fifo={}", fifo.display());

    let source = cli
        .source
        .unwrap_or_else(|| format!("wob-fifo-{}", std::process::id()));
    tracing::info!("awob-listener-wob: source={source}");

    loop {
        let f = match std::fs::OpenOptions::new().read(true).open(&fifo) {
            Ok(f) => f,
            Err(e) => {
                tracing::info!("awob-listener-wob: open fifo: {e}");
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
        };
        // Reading an empty FIFO returns EOF when all writers close. Loop and reopen.
        let _fd = f.as_raw_fd();
        let reader = BufReader::new(f);
        for line in reader.lines() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::info!("read: {e}");
                    break;
                }
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let Some((value, max, style)) = parse_line(trimmed) else {
                tracing::info!("awob-listener-wob: bad line `{trimmed}`");
                continue;
            };
            let mut s = Send::new(&cli.event, value)
                .listener_id("awob-listener-wob")
                .source(&source)
                // wob FIFO writers are user-driven scripts. The
                // expectation when something writes a volume value is
                // that it shows up immediately, not queued.
                .preempt(true);
            if let Some(m) = max {
                s = s.max(m);
            }
            if let Some(st) = style {
                s = s.style(st);
            }
            match cli.socket.clone() {
                Some(p) => match Client::connect_to(&p) {
                    Ok(mut c) => {
                        if let Err(e) = c.send(s.build()) {
                            tracing::info!("send: {e}");
                        }
                    }
                    Err(e) => tracing::info!("connect: {e}"),
                },
                None => match Client::connect() {
                    Ok(mut c) => {
                        if let Err(e) = c.send(s.build()) {
                            tracing::info!("send: {e}");
                        }
                    }
                    Err(e) => tracing::info!("connect: {e}"),
                },
            }
        }
    }
}

fn main() -> ExitCode {
    awob_client::init_tracing("info");
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "awob-listener-wob starting");
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::info!("awob-listener-wob: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_line;

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
}
