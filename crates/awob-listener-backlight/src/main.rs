//! awob backlight listener.
//!
//! Watches a sysfs backlight node (`/sys/class/backlight/<dev>/brightness`)
//! via `notify` and emits an OSD event on every change. Auto-discovers a
//! backlight device by scanning `/sys/class/backlight` if `--device` isn't
//! passed.
//!
//! Reads `max_brightness` from the same directory and forwards it as the
//! send's `max`, so themes that show absolute values can do so faithfully.

mod wayland_outputs;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use awob_client::{Client, Send};
use clap::Parser;
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

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let dir = match cli.device {
        Some(name) => Path::new("/sys/class/backlight").join(name),
        None => discover_device().ok_or("no backlight device found under /sys/class/backlight")?,
    };
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
    eprintln!(
        "awob-listener-backlight: device={device_name} source={source} \
         connector={connector:?} label={label:?}"
    );

    eprintln!(
        "awob-listener-backlight: device={} source={}",
        device_name, source
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
        // Debounce a burst of writes (sysfs can fire several events per change).
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
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "awob-listener-backlight starting");
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::info!("awob-listener-backlight: {e}");
            ExitCode::from(1)
        }
    }
}
