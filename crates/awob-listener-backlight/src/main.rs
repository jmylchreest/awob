//! awob backlight listener.
//!
//! Watches a sysfs backlight node (`/sys/class/backlight/<dev>/brightness`)
//! via three additive wake sources — inotify + udev + adaptive sysfs
//! polling — all feeding one mpsc channel. Auto-discovers a backlight
//! device by scanning `/sys/class/backlight` if `--device` isn't
//! passed.
//!
//! Mirrors `awob-listener-keyboard-backlight`. Display backlights are
//! almost always changed via userspace `write()` (brightnessctl etc),
//! so inotify is the hot path; udev + polling cover the rare path
//! where firmware writes the cached sysfs value without firing
//! `kernfs_notify()` (no machine has been observed doing this for the
//! display backlight, but the kbd-backlight half taught us not to
//! assume).
//!
//! Reads `max_brightness` from the same directory and forwards it as
//! the send's `max`, so themes that show absolute values can do so
//! faithfully.

mod wayland_outputs;

use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use awob_client::listener::{ChangeFilter, wait_for_resource};
use awob_client::{Client, Send};
use clap::Parser;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use notify::{Event, EventKind, RecursiveMode, Watcher};

#[derive(Parser, Debug)]
#[command(version, about = "awob — backlight listener")]
struct Cli {
    /// Backlight device name under /sys/class/backlight (e.g. `intel_backlight`).
    /// If not given, the first device found is used.
    #[arg(long)]
    device: Option<String>,

    /// Override the daemon socket path.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Override the source ID. Default is `backlight-<sanitised-device>` —
    /// stable across restarts (no PID suffix).
    #[arg(long)]
    source: Option<String>,

    /// Friendly label shown in the OSD's `app` field. Overrides the
    /// auto-detected name. Resolution order when unset:
    ///
    /// 1. `wl_output.make` + `wl_output.model` for the connector this
    ///    backlight controls (e.g. `"BOE NE135A1M-NY1"`),
    /// 2. heuristic from the connector type (`"Display"` for `eDP-*`,
    ///    `"External Display"` for HDMI/DP),
    /// 3. the raw sysfs device name.
    #[arg(long)]
    label: Option<String>,

    /// Polling interval in milliseconds for the sysfs brightness
    /// re-read. inotify covers most display backlights (because every
    /// brightness change is a userspace write — brightnessctl etc),
    /// but on hardware where the firmware updates sysfs directly the
    /// poll is the only signal. This is the worst-case OSD latency on
    /// such hardware.
    #[arg(long, default_value_t = 250, value_parser = clap::value_parser!(u64).range(100..=2000))]
    poll_interval: u64,
}

fn discover_device() -> Option<PathBuf> {
    let root = Path::new("/sys/class/backlight");
    let entries = std::fs::read_dir(root).ok()?;
    for ent in entries.flatten() {
        let p = ent.path();
        if p.join("brightness").exists() {
            return Some(p);
        }
    }
    None
}

fn read_u32(p: &Path) -> std::io::Result<u32> {
    let s = std::fs::read_to_string(p)?;
    s.trim()
        .parse()
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", p.display())))
}

/// Re-scan cadence when no backlight is present yet. Headless servers
/// and desktops with no DPMS-aware GPU never gain a backlight; the
/// 60 s rescan keeps the listener cheap there while still catching
/// the rare case where a driver loads late (e.g. hybrid graphics).
const NO_DEVICE_RESCAN: Duration = Duration::from_secs(60);

/// Block until a backlight device appears (either at the explicit
/// `--device` name or via auto-discovery). Logs once at INFO and quiet
/// retries at DEBUG so a no-display headless box doesn't spam the
/// journal.
fn wait_for_device(explicit: Option<&str>) -> PathBuf {
    wait_for_resource(
        || match explicit {
            Some(name) => {
                let p = Path::new("/sys/class/backlight").join(name);
                p.join("brightness").exists().then_some(p)
            }
            None => discover_device(),
        },
        "backlight",
        NO_DEVICE_RESCAN,
    )
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
        .unwrap_or_else(|| "backlight".into());
    // Stable source — same device, same source across restarts. No PID
    // suffix so the daemon sees the same source on respawn (no spurious
    // duplicate-listener warning).
    let source = cli
        .source
        .unwrap_or_else(|| format!("backlight-{}", sanitise_device(&device_name)));

    // Friendly label. If user passed --label, use it. Otherwise probe
    // Wayland outputs and match the backlight's connector to one. Fall
    // back to a heuristic from the connector name, then to the raw sysfs
    // device name.
    let connector = read_connector(&dir);
    let label = match cli.label {
        Some(l) => l,
        None => derive_label(&device_name, connector.as_deref()),
    };
    tracing::info!(
        "device={device_name} source={source} \
         connector={connector:?} label={label:?}"
    );

    let max = read_u32(&max_path).unwrap_or(100) as f64;

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

    // Source 2 — udev `change` events on the backlight subsystem,
    // filtered to our device. Catches drivers that fire
    // `kobject_uevent()` without writing the sysfs file from userspace.
    let want_device = device_name.clone();
    let udev_tx = tx.clone();
    std::thread::Builder::new()
        .name("udev-backlight".into())
        .spawn(move || {
            if let Err(e) = run_udev(&want_device, udev_tx) {
                tracing::debug!("udev monitor exited: {e}");
            }
        })?;

    // Source 3 — polling fall-through, expressed as the recv_timeout
    // below. Backstop for hardware where neither inotify nor udev
    // fires (rare for display backlights — most are driven by
    // userspace tools that always fire inotify).
    let poll_interval = Duration::from_millis(cli.poll_interval);

    let mut filter: ChangeFilter<(), u32> = ChangeFilter::new();
    let debounce = Duration::from_millis(40);
    loop {
        match rx.recv_timeout(poll_interval) {
            Ok(()) => {
                // Debounce a burst (sysfs can fire several events per change).
                std::thread::sleep(debounce);
                while rx.try_recv().is_ok() {}
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // No wake; polling path.
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let current = read_u32(&brightness_path).unwrap_or(0);
        if filter.changed((), &current) {
            let _ = send_to_daemon(
                &cli.socket,
                &source,
                &device_name,
                &label,
                current as f64,
                max,
            );
        }
    }
    Ok(())
}

/// Subscribe to udev `change` events on the `backlight` subsystem and
/// forward each one matching our device into the wake channel.
/// Returns when the channel is dropped (main loop exited) or on a
/// fatal udev error.
fn run_udev(want_device: &str, tx: mpsc::Sender<()>) -> Result<(), Box<dyn std::error::Error>> {
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("backlight")?
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
            if ev.sysname().to_string_lossy() == want_device && tx.send(()).is_err() {
                return Ok(());
            }
        }
    }
}

/// Read the connector name from the backlight's `device` symlink.
/// e.g. `/sys/class/backlight/amdgpu_bl1/device → ../../card1-eDP-1`
/// returns `Some("eDP-1")`.
fn read_connector(backlight_dir: &Path) -> Option<String> {
    let target = std::fs::read_link(backlight_dir.join("device")).ok()?;
    let last = target.file_name()?.to_str()?;
    // strip the leading `cardN-` if present
    if let Some(rest) = last.strip_prefix(|c: char| c == 'c').and_then(|s| {
        s.strip_prefix("ard")
            .and_then(|t| t.split_once('-'))
            .map(|(_, name)| name)
    }) {
        Some(rest.to_string())
    } else {
        Some(last.to_string())
    }
}

fn derive_label(device_name: &str, connector: Option<&str>) -> String {
    let connector = match connector {
        Some(c) => c,
        None => return device_name.to_string(),
    };

    // 1. Wayland wl_output: connector name should match `wl_output.name`.
    let outputs = wayland_outputs::probe(Duration::from_millis(300));
    if let Some(info) = outputs.get(connector) {
        let make = info.make.trim();
        let model = info.model.trim();
        match (make.is_empty(), model.is_empty()) {
            (false, false) => return format!("{make} {model}"),
            (false, true) => return make.to_string(),
            (true, false) => return model.to_string(),
            (true, true) => {} // fall through
        }
        let desc = info.description.trim();
        if !desc.is_empty() {
            return desc.to_string();
        }
    }

    // 2. Heuristic from connector type.
    if connector.starts_with("eDP") || connector.starts_with("LVDS") || connector.starts_with("DSI")
    {
        "Display".to_string()
    } else if connector.starts_with("HDMI")
        || connector.starts_with("DP-")
        || connector.starts_with("VGA")
    {
        "External Display".to_string()
    } else {
        // 3. Raw sysfs device name as last resort.
        device_name.to_string()
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
    // Per-device listener_id: same device across runs collapses to one
    // tracked instance; multiple devices on the same machine each get their
    // own listener_id so the daemon treats them as independent.
    let listener_id = format!("awob-listener-backlight-{}", sanitise_device(device));
    let s = Send::new("brightness", value)
        .listener_id(listener_id)
        .source(source)
        .max(max)
        .icon("display-brightness")
        .app(label)
        // Brightness changes are key-press driven and user-interactive,
        // so this OSD should hot-swap a battery bar that happens to be
        // on screen rather than queueing behind it.
        .preempt(true);
    c.send(s.build())
}

/// `chromeos::kbd_backlight` → `chromeos__kbd_backlight`. Stable identifier
/// from a sysfs device name, suitable for use in a listener_id where `::`
/// would be confusing in logs.
fn sanitise_device(name: &str) -> String {
    name.replace("::", "__").replace(' ', "_")
}

fn main() -> ExitCode {
    awob_client::init_tracing("info");
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "awob-listener-backlight starting"
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
