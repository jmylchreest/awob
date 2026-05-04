//! awob keyboard-backlight listener.
//!
//! Watches a `/sys/class/leds/<kbd>/brightness` node via three additive
//! mechanisms — whichever wakes first wins. Auto-discovers the LED by
//! pattern-matching `kbd` / `keyboard` device names.
//!
//! ## Why three?
//!
//! No single mechanism is reliable across the whole laptop ecosystem:
//!
//! * **inotify** fires when *userspace* writes the sysfs `brightness`
//!   file, e.g. brightnessctl from a Hyprland keybind. Microsecond
//!   latency, doesn't fire on firmware-driven changes.
//! * **udev** fires `change` events when the LED driver calls
//!   `kobject_uevent()`. Some EC drivers do, some don't. Millisecond
//!   latency where supported, silent otherwise.
//! * **250 ms polling** catches everything else — drivers that update
//!   the cached value without calling either notification primitive
//!   (Framework laptops with the chromeos EC are the classic case).
//!   Quarter-second worst-case latency, four-byte sysfs read per tick.
//!
//! All three feed the same `mpsc` wake channel; the main loop just
//! reads-and-compares on every wake regardless of source. The polling
//! interval acts as the `recv_timeout` so it's the natural fall-through
//! when neither inotify nor udev fired.

use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use awob_client::{Client, Send};
use clap::Parser;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
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

    /// Polling interval in milliseconds for the sysfs brightness
    /// re-read. This is the worst-case OSD latency on hardware where
    /// neither inotify nor udev fires (e.g. Framework chromeos EC).
    /// On hardware where they do fire, polling rarely matters —
    /// every wake is external.
    #[arg(long, default_value_t = 250, value_parser = clap::value_parser!(u64).range(100..=2000))]
    poll_interval: u64,
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

/// Re-scan cadence when no LED is present yet. Sits in the background
/// of a desktop without an internal keyboard backlight, catching a
/// hot-plugged USB keyboard whose driver registers an LED. 60 s
/// because keyboards don't appear/disappear often.
const NO_DEVICE_RESCAN: Duration = Duration::from_secs(60);

/// Block until a keyboard-backlight LED appears at the given device
/// name (or via auto-discovery). Logs the "no device" state once at
/// INFO and subsequent quiet retries at DEBUG.
fn wait_for_device(explicit: Option<&str>) -> PathBuf {
    let resolve = || match explicit {
        Some(name) => {
            let p = Path::new("/sys/class/leds").join(name);
            if p.join("brightness").exists() {
                Some(p)
            } else {
                None
            }
        }
        None => discover_device(),
    };
    if let Some(p) = resolve() {
        return p;
    }
    tracing::info!(
        "no keyboard backlight device found under /sys/class/leds (looked for *kbd*/*keyboard*); \
         will rescan every {}s for hot-plug",
        NO_DEVICE_RESCAN.as_secs()
    );
    loop {
        std::thread::sleep(NO_DEVICE_RESCAN);
        if let Some(p) = resolve() {
            tracing::info!("keyboard backlight appeared at {}; resuming", p.display());
            return p;
        }
        tracing::debug!(
            "still no keyboard backlight; rescanning in {}s",
            NO_DEVICE_RESCAN.as_secs()
        );
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let dir = wait_for_device(cli.device.as_deref());
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
    // Seed `last` from the current brightness without firing an OSD.
    // Listeners stay silent on startup (and supervisor respawn) — an
    // OSD only ever surfaces on a *change* against this baseline. Same
    // policy as awob-listener-battery.
    let initial = read_u32(&brightness_path).unwrap_or(0) as f64;

    // Source 1 — inotify on the sysfs file. Cheap, wakes the loop on
    // userspace writes (brightnessctl etc).
    let (tx, rx) = mpsc::channel::<()>();
    let inotify_tx = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res
            && matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_))
        {
            let _ = inotify_tx.send(());
        }
    })?;
    watcher.watch(&brightness_path, RecursiveMode::NonRecursive)?;

    // Source 2 — udev `change` events on the leds subsystem, filtered
    // to our device. Catches drivers that fire `kobject_uevent()`
    // without writing the sysfs file from userspace. Spawned on a
    // worker thread so the main loop just reads from the same channel.
    let want_device = device_name.clone();
    let udev_tx = tx.clone();
    std::thread::Builder::new()
        .name("udev-leds".into())
        .spawn(move || {
            if let Err(e) = run_udev(&want_device, udev_tx) {
                tracing::debug!("udev monitor exited: {e}");
            }
        })?;

    // Source 3 — polling fall-through, expressed as the recv_timeout
    // below. Backstop for drivers that update the cached sysfs value
    // without firing either inotify or udev (Framework laptops with
    // the chromeos EC are the canonical case).
    let poll_interval = Duration::from_millis(cli.poll_interval);

    let mut last = initial;
    let debounce = Duration::from_millis(40);
    loop {
        match rx.recv_timeout(poll_interval) {
            Ok(()) => {
                // Some source fired — coalesce a burst before re-reading.
                std::thread::sleep(debounce);
                while rx.try_recv().is_ok() {}
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No wake; polling path.
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

/// Subscribe to udev `change` events on the `leds` subsystem and
/// forward each one matching our device into the wake channel.
/// Returns when the channel is dropped (main loop exited) or on a
/// fatal udev error.
fn run_udev(want_device: &str, tx: mpsc::Sender<()>) -> Result<(), Box<dyn std::error::Error>> {
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("leds")?
        .listen()?;
    let monitor_fd = monitor.as_fd();
    loop {
        let mut pfds = [PollFd::new(monitor_fd, PollFlags::POLLIN)];
        match poll(&mut pfds, PollTimeout::NONE) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(format!("poll: {e}").into()),
        }
        for ev in monitor.iter() {
            // Only react to changes on our specific device. Some
            // drivers fire on related leds (capslock, numlock) which
            // share the subsystem but aren't the keyboard backlight.
            if ev.sysname().to_string_lossy() == want_device && tx.send(()).is_err() {
                return Ok(());
            }
        }
    }
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
