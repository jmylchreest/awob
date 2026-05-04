//! awob keyboard-backlight listener.
//!
//! Watches a `/sys/class/leds/<kbd>/brightness` node and emits an OSD on
//! every change. Auto-discovers a keyboard-backlight LED by pattern-
//! matching against device names containing `kbd` or `keyboard`.
//!
//! Why both polling and inotify? On hardware where userspace writes the
//! sysfs file (most laptops driven via brightnessctl-style hotkeys) inotify
//! fires immediately on the write, so we wake within microseconds.
//!
//! On hardware where the embedded controller / firmware handles the
//! brightness key directly (Framework laptops with the chromeos EC,
//! some Chromebooks, certain Apple keyboards) the kernel updates the
//! cached sysfs value but the LED driver never calls `kernfs_notify()`
//! — so no inotify event ever reaches us, and udev `change` events
//! aren't fired either. The 250 ms polling backstop catches those
//! cases at the cost of a four-byte sysfs read every quarter-second.
//! Cheap insurance.

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

    tracing::info!("device={device_name} source={source} label={label:?}");

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
    let poll_interval = Duration::from_millis(250);
    loop {
        match rx.recv_timeout(poll_interval) {
            Ok(()) => {
                // inotify fired — coalesce any rapid follow-up events
                // before re-reading sysfs.
                std::thread::sleep(debounce);
                while rx.try_recv().is_ok() {}
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No inotify; fall through to the same read-and-compare
                // path that the inotify branch uses. This is the
                // backstop for firmware-driven LED updates.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
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
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "awob-listener-keyboard-backlight starting"
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

    #[test]
    fn discovers_kbd_backlight_when_present() {
        // This test is environment-dependent; just exercise the function.
        let _ = discover_device();
    }
}
