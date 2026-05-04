//! awob power-profile listener.
//!
//! Watches `/sys/firmware/acpi/platform_profile` for changes — the
//! kernel-exposed attribute that `power-profiles-daemon` and
//! `tuned` proxy via D-Bus. Emits an OSD whenever the active profile
//! changes (`performance` / `balanced` / `low-power` / `quiet` /
//! `cool` / etc., depending on the platform driver).
//!
//! Why sysfs and not D-Bus? The kernel attribute is the source of
//! truth on every modern laptop that exposes a platform profile
//! (Framework via `amd_pmf`, ThinkPad via `thinkpad_acpi`, most
//! Intel/AMD laptops via firmware DPTF). PPD and tuned both proxy
//! it. Reading sysfs directly avoids the D-Bus hop and the zbus
//! dependency. On laptops without `platform_profile`, the listener
//! sits in its standard wait-for-device loop — same graceful
//! behaviour as the battery/backlight listeners on hardware they
//! don't apply to.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc;
use std::time::Duration;

use awob_client::listener::{ChangeFilter, wait_for_resource};
use awob_client::{Client, Send};
use clap::Parser;
use notify::{Event, EventKind, RecursiveMode, Watcher};

const PROFILE_PATH: &str = "/sys/firmware/acpi/platform_profile";
const CHOICES_PATH: &str = "/sys/firmware/acpi/platform_profile_choices";
const LISTENER_ID: &str = "awob-listener-power-profile";
const SOURCE: &str = "power-profile";

/// Re-scan cadence when the platform_profile attribute is absent (e.g.
/// older laptops or virtual machines without ACPI DPTF). Same 60 s
/// pattern the battery / backlight / kbd-backlight listeners use.
const NO_DEVICE_RESCAN: Duration = Duration::from_secs(60);

#[derive(Parser, Debug)]
#[command(
    version,
    about = "awob — power-profile listener (ACPI platform_profile)"
)]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Polling interval in milliseconds for the sysfs re-read backstop.
    /// inotify is the hot path on every kernel that calls `kernfs_notify`
    /// on the attribute (which is most of them); polling is the
    /// safety net.
    #[arg(long, default_value_t = 250, value_parser = clap::value_parser!(u64).range(100..=2000))]
    poll_interval: u64,
}

/// Block until `/sys/firmware/acpi/platform_profile` exists. On a
/// machine that doesn't expose it (older laptops, VMs) this loops
/// forever at NO_DEVICE_RESCAN cadence. Hot-plug isn't really a
/// thing for ACPI firmware, but the loop keeps shape consistent
/// with the other listeners.
fn wait_for_attribute() -> PathBuf {
    wait_for_resource(
        || {
            let p = PathBuf::from(PROFILE_PATH);
            p.exists().then_some(p)
        },
        "platform_profile",
        NO_DEVICE_RESCAN,
    )
}

fn read_profile(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Resolve a profile name (`performance` / `balanced` / `low-power` /
/// `quiet` / `cool` / vendor-specific oddities) into the OSD value,
/// label, icon, and style. Unknown profile names still fire — they
/// just get a generic icon and the raw name as label.
///
/// Returns `(value, label, icon, style)`. Value 0..1 is a coarse
/// performance level (low-power = 0.25, balanced = 0.5, performance
/// = 1.0) so themes can render a 3-step bar without parsing the
/// profile string. Label is owned because unknown profiles fall back
/// to the raw input name (which has a non-static lifetime).
fn presentation(profile: &str) -> (f64, String, &'static str, &'static str) {
    match profile {
        "performance" => (
            1.0,
            "Performance".into(),
            "power-profile-performance-symbolic",
            "warn",
        ),
        "balanced" => (
            0.5,
            "Balanced".into(),
            "power-profile-balanced-symbolic",
            "normal",
        ),
        "low-power" => (
            0.25,
            "Power Saver".into(),
            "power-profile-power-saver-symbolic",
            "low",
        ),
        "quiet" => (
            0.4,
            "Quiet".into(),
            "power-profile-balanced-symbolic",
            "normal",
        ),
        "cool" => (
            0.4,
            "Cool".into(),
            "power-profile-balanced-symbolic",
            "normal",
        ),
        _ => (
            0.5,
            profile.to_string(),
            "power-profile-balanced-symbolic",
            "normal",
        ),
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let path = wait_for_attribute();
    let choices = read_profile(Path::new(CHOICES_PATH)).unwrap_or_default();
    tracing::info!(
        "source={SOURCE} path={} choices={choices:?}",
        path.display()
    );

    // inotify on the attribute. Wakes the loop on writes from PPD,
    // tuned, the user echoing into it, ACPI hotkeys handled in
    // userspace, etc.
    let (tx, rx) = mpsc::channel::<()>();
    let inotify_tx = tx.clone();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
        if let Ok(ev) = res
            && matches!(ev.kind, EventKind::Modify(_) | EventKind::Create(_))
        {
            let _ = inotify_tx.send(());
        }
    })?;
    watcher.watch(&path, RecursiveMode::NonRecursive)?;

    let poll_interval = Duration::from_millis(cli.poll_interval);
    let mut filter: ChangeFilter<(), String> = ChangeFilter::new();
    let debounce = Duration::from_millis(40);
    loop {
        match rx.recv_timeout(poll_interval) {
            Ok(()) => {
                std::thread::sleep(debounce);
                while rx.try_recv().is_ok() {}
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        let current = read_profile(&path).unwrap_or_default();
        if current.is_empty() {
            continue;
        }
        if filter.changed((), &current)
            && let Err(e) = fire(&cli.socket, &current)
        {
            tracing::info!("send: {e}");
        }
    }
    Ok(())
}

fn fire(socket: &Option<PathBuf>, profile: &str) -> awob_client::Result<()> {
    let (value, label, icon, style) = presentation(profile);
    let mut c = match socket {
        Some(p) => Client::connect_to(p)?,
        None => Client::connect()?,
    };
    let s = Send::new("power-profile", value)
        .max(1.0)
        .listener_id(LISTENER_ID)
        .source(SOURCE)
        .icon(icon)
        .style(style)
        .app(&label)
        // Power-profile changes are user-driven (Fn-key, settings panel),
        // so hot-swap whatever's on screen.
        .preempt(true);
    c.send(s.build())
}

fn main() -> ExitCode {
    awob_client::init_tracing("info");
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "awob-listener-power-profile starting"
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
    fn presentation_known_profiles() {
        let (_v, label, icon, style) = presentation("performance");
        assert_eq!(label, "Performance");
        assert!(icon.contains("performance"));
        assert_eq!(style, "warn");

        let (_v, label, _icon, style) = presentation("balanced");
        assert_eq!(label, "Balanced");
        assert_eq!(style, "normal");

        let (_v, label, _icon, style) = presentation("low-power");
        assert_eq!(label, "Power Saver");
        assert_eq!(style, "low");
    }

    #[test]
    fn presentation_unknown_falls_through() {
        let (value, label, _icon, style) = presentation("eco");
        assert_eq!(label, "eco");
        assert_eq!(value, 0.5);
        assert_eq!(style, "normal");
    }
}
