//! awob keyboard-backlight listener.
//!
//! Watches a `/sys/class/leds/<kbd>/brightness` node via `notify` and emits
//! an OSD on every change. Auto-discovers a keyboard-backlight LED by
//! pattern-matching against device names containing `kbd` or `keyboard`.
//!
//! Same `notify`-driven event loop as `awob-listener-backlight` — the kernel
//! signals on every write to the sysfs file, no polling. Reads
//! `max_brightness` from the same directory and forwards it as the send's
//! `max`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use awob_client::{Client, Send};
use clap::Parser;
use notify::{Event, EventKind, RecursiveMode, Watcher};

/// Per-device listener_id pattern. Each keyboard is its own logical
/// listener so two keyboards don't collide in the daemon's history map and
/// don't trigger duplicate-listener warnings.
fn listener_id_for(device: &str) -> String {
    format!(
        "awob-listener-keyboard-backlight-{}",
        sanitise_device(device)
    )
}

fn sanitise_device(name: &str) -> String {
    name.replace("::", "__").replace(' ', "_")
}

#[derive(Parser, Debug)]
#[command(version, about = "awob — keyboard-backlight listener")]
struct Cli {
    /// LED device name under /sys/class/leds (e.g. `tpacpi::kbd_backlight`,
    /// `chromeos::kbd_backlight`). If unset, the first device whose name
    /// contains `kbd` or `keyboard` is used.
    #[arg(long)]
    device: Option<String>,

    #[arg(long)]
    socket: Option<PathBuf>,

    /// Stable per-process suffix on the source ID.
    #[arg(long)]
    source: Option<String>,

    /// Friendly label shown in the OSD's `app` field. Overrides the
    /// auto-generated name. Default is `"Keyboard"` for a single keyboard,
    /// or `"Keyboard 1"` / `"Keyboard 2"` if multiple `*kbd*` LEDs are
    /// present (numbered by sorted sysfs path so the index is stable
    /// across reboots).
    #[arg(long)]
    label: Option<String>,
}

/// All `*kbd*` / `*keyboard*` LED device dirs, sorted by path so the
/// index of a given device is stable across reboots.
fn all_keyboard_devices() -> Vec<PathBuf> {
    let root = Path::new("/sys/class/leds");
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.join("brightness").exists())
        .filter(|p| {
            let n = p
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            n.contains("kbd") || n.contains("keyboard")
        })
        .collect();
    out.sort();
    out
}

fn friendly_label(device_path: &Path, override_label: Option<&String>) -> String {
    if let Some(s) = override_label {
        return s.clone();
    }
    let all = all_keyboard_devices();
    if all.len() <= 1 {
        return "Keyboard".into();
    }
    let idx = all.iter().position(|p| p == device_path).unwrap_or(0);
    format!("Keyboard {}", idx + 1)
}

fn discover_device() -> Option<PathBuf> {
    all_keyboard_devices().into_iter().next()
}

fn read_u32(p: &Path) -> std::io::Result<u32> {
    let s = std::fs::read_to_string(p)?;
    s.trim()
        .parse()
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", p.display())))
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let dir = match cli.device {
        Some(name) => Path::new("/sys/class/leds").join(name),
        None => discover_device()
            .ok_or("no keyboard backlight device found under /sys/class/leds (looked for *kbd*/*keyboard*)")?,
    };
    let brightness_path = dir.join("brightness");
    let max_path = dir.join("max_brightness");
    if !brightness_path.exists() {
        return Err(format!("brightness file not found at {}", brightness_path.display()).into());
    }
    let device_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "kbd-backlight".into());
    let source = cli
        .source
        .clone()
        .unwrap_or_else(|| sanitise_device(&device_name));
    let label = friendly_label(&dir, cli.label.as_ref());

    eprintln!(
        "awob-listener-keyboard-backlight: device={device_name} source={source} label={label:?}"
    );

    let max = read_u32(&max_path).unwrap_or(100) as f64;
    let initial = read_u32(&brightness_path).unwrap_or(0) as f64;
    let _ = send_to_daemon(&cli.socket, &source, &device_name, &label, initial, max);

    let (tx, rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res {
            if matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                let _ = tx.send(());
            }
        }
    })?;
    watcher.watch(&brightness_path, RecursiveMode::NonRecursive)?;

    let mut last = initial;
    let debounce = Duration::from_millis(40);
    loop {
        if rx.recv().is_err() {
            break;
        }
        std::thread::sleep(debounce);
        while rx.try_recv().is_ok() {}
        let current = read_u32(&brightness_path).unwrap_or(last as u32) as f64;
        if (current - last).abs() < f64::EPSILON {
            continue;
        }
        last = current;
        let _ = send_to_daemon(&cli.socket, &source, &device_name, &label, current, max);
    }
    Ok(())
}

fn send_to_daemon(
    socket: &Option<PathBuf>,
    source: &str,
    device: &str,
    label: &str,
    value: f64,
    max: f64,
) -> awob_client::Result<()> {
    let mut c = match socket {
        Some(p) => Client::connect_to(p)?,
        None => Client::connect()?,
    };
    let s = Send::new("keyboard-backlight", value)
        .listener_id(listener_id_for(device))
        .source(source)
        .max(max)
        .icon("input-keyboard")
        .app(label)
        // Keyboard-backlight is fn-key driven; treat as interactive.
        .preempt(true);
    c.send(s.build())
}

fn main() -> ExitCode {
    awob_client::init_tracing("info");
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "awob-listener-keyboard-backlight starting");
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::info!("awob-listener-keyboard-backlight: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_kbd_backlight_when_present() {
        // This test is environment-dependent; just exercise the function.
        let _ = discover_device();
    }
}
