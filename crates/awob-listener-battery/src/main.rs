//! awob battery listener (sysfs + udev).
//!
//! Tracks all `/sys/class/power_supply/*` battery and AC supplies and
//! emits OSD events on every change. Subscribes to udev's `power_supply`
//! subsystem so kernel uevents — AC plug, battery state transition,
//! capacity tick — fire instantly. Falls back to a 60-second periodic
//! re-read so a missed uevent or warm-boot still surfaces a fresh OSD.
//!
//! Why not UPower? UPower polls hardware on a timer for some events
//! (notably AC plug), which makes "battery just started charging" take
//! up to 30 seconds to surface. Reading the kernel's sysfs files
//! directly via uevents removes that lag and drops the zbus dependency.
//!
//! Multi-battery: capacities are weighted by `energy_full` (or
//! `charge_full` when energy isn't available — common on phones /
//! tablets) and aggregated. The reported state is the dominant one
//! across the batteries — Charging if any are charging, otherwise
//! Discharging if any are discharging, otherwise Full or Unknown.

use std::collections::HashSet;
use std::os::fd::AsFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use awob_client::{Client, Send};
use clap::Parser;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};

const LISTENER_ID: &str = "awob-listener-battery";
const SYSFS_ROOT: &str = "/sys/class/power_supply";
/// Re-read everything every minute regardless of uevents — a backstop
/// against any missed events. The kernel does fire uevents reliably for
/// the changes we care about, but a periodic refresh costs nothing and
/// keeps the OSD in sync with reality even after a suspend / resume.
const RESCAN_INTERVAL: Duration = Duration::from_secs(60);

/// After any power_supply uevent, poll sysfs at this cadence for
/// `BURST_DURATION` to catch lagged state transitions. Why:
///
/// On AC plug the kernel fires two events in sequence — first for the
/// AC adapter (`ACAD/online=1`), then for the battery (`BAT*/status
/// =Charging`). On some hardware (Dell, ThinkPad, Framework) the
/// battery driver polls its embedded controller on a timer, so the
/// BAT event can lag the AC event by 1–10 seconds. A single read at
/// AC-event time observes the *old* battery state. Polling every
/// second for `BURST_DURATION` catches the transition reliably.
///
/// Outside burst mode we fall back to the cheap `RESCAN_INTERVAL`
/// backstop. Once 60 s elapses without an event, we resume the
/// 60 s rescan rhythm.
const BURST_POLL_INTERVAL: Duration = Duration::from_secs(1);
const BURST_DURATION: Duration = Duration::from_secs(5);

#[derive(Parser, Debug)]
#[command(version, about = "awob — battery listener (sysfs + udev)")]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Stable source suffix. Final source = "battery-<source>". Defaults
    /// to "battery" so restarts re-use the same source.
    #[arg(long)]
    source: Option<String>,

    /// Comma-separated list of states to fire OSDs on. Recognised:
    /// `charging`, `discharging`, `empty`, `fully-charged`,
    /// `pending-charge`, `pending-discharge`, `unknown`, or `all`.
    ///
    /// Default `charging,discharging,empty,fully-charged` — the
    /// "interesting" transitions. `pending-discharge` (AC plugged
    /// but battery not absorbing — e.g. full at 100%, or the
    /// kernel's charge-threshold gate is in effect) is captured by
    /// the cache but doesn't fire by default. Pass `--states all`
    /// to surface every state change.
    #[arg(long, default_value = "charging,discharging,empty,fully-charged")]
    states: String,

    /// Comma-separated list of capacity bands that fire an OSD when
    /// entered. Recognised band names: `empty` (0–5 %), `caution`
    /// (6–20 %), `low` (21–50 %), `good` (51–80 %), `full`
    /// (81–100 %). Special values: `all` (every band fires) and
    /// `none` (only state transitions fire).
    ///
    /// Default `empty,caution` — drains past the warning bands surface
    /// an OSD; the rest of a discharge is silent. State transitions
    /// (Charging↔Discharging↔FullyCharged) always fire regardless of
    /// this filter — they're the events the user wants to see.
    #[arg(long, default_value = "empty,caution")]
    alert_bands: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatteryState {
    Unknown,
    Charging,
    Discharging,
    Empty,
    FullyCharged,
    PendingCharge,
    PendingDischarge,
}

impl BatteryState {
    /// Parse the kernel's `status` sysfs string. Reference:
    /// `Documentation/ABI/testing/sysfs-class-power`.
    fn from_sysfs(s: &str) -> Self {
        match s.trim() {
            "Charging" => Self::Charging,
            "Discharging" => Self::Discharging,
            "Full" => Self::FullyCharged,
            "Not charging" => Self::PendingDischarge,
            "Empty" => Self::Empty,
            _ => Self::Unknown,
        }
    }
    fn slug(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Charging => "charging",
            Self::Discharging => "discharging",
            Self::Empty => "empty",
            Self::FullyCharged => "fully-charged",
            Self::PendingCharge => "pending-charge",
            Self::PendingDischarge => "pending-discharge",
        }
    }
    fn parse_slug(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "charging" => Self::Charging,
            "discharging" => Self::Discharging,
            "empty" => Self::Empty,
            "fully-charged" | "fullycharged" | "full" => Self::FullyCharged,
            "pending-charge" => Self::PendingCharge,
            "pending-discharge" => Self::PendingDischarge,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

/// One battery's instantaneous state, sourced from sysfs.
#[derive(Debug, Clone)]
struct BatteryReading {
    capacity: f64,
    state: BatteryState,
    /// Capacity weight for multi-battery aggregation. Falls back to 1.0
    /// when the kernel doesn't expose `energy_full` / `charge_full`.
    weight: f64,
}

fn parse_state_filter(arg: &str) -> HashSet<BatteryState> {
    let mut out = HashSet::new();
    for token in arg.split(',') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        if t.eq_ignore_ascii_case("all") {
            for v in [
                BatteryState::Charging,
                BatteryState::Discharging,
                BatteryState::Empty,
                BatteryState::FullyCharged,
                BatteryState::PendingCharge,
                BatteryState::PendingDischarge,
                BatteryState::Unknown,
            ] {
                out.insert(v);
            }
            continue;
        }
        if let Some(s) = BatteryState::parse_slug(t) {
            out.insert(s);
        } else {
            tracing::info!("unknown state `{t}` in --states");
        }
    }
    out
}

fn read_string(p: &Path) -> Option<String> {
    std::fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
}

fn read_f64(p: &Path) -> Option<f64> {
    read_string(p)?.parse::<f64>().ok()
}

/// Re-scan cadence when no devices are present yet. Runs forever in the
/// background of an otherwise-idle desktop where no battery exists,
/// catching hot-plug additions cheaply (one [`std::fs::read_dir`] per
/// minute). 60 s rather than something faster because batteries
/// don't appear/disappear meaningfully often.
const NO_DEVICE_RESCAN: Duration = Duration::from_secs(60);

/// Block until at least one battery is discoverable. On a desktop with
/// no battery this loops forever at [`NO_DEVICE_RESCAN`] cadence; on a
/// laptop the first scan succeeds immediately.
///
/// Logs the "no batteries" state once at INFO so it's visible in the
/// journal without spamming on every retry. Subsequent quiet retries
/// stay at DEBUG.
fn wait_for_batteries() -> Vec<PathBuf> {
    let bs = discover_batteries();
    if !bs.is_empty() {
        return bs;
    }
    tracing::info!(
        "no batteries under {SYSFS_ROOT} (type=Battery); will rescan every {}s for hot-plug",
        NO_DEVICE_RESCAN.as_secs()
    );
    loop {
        std::thread::sleep(NO_DEVICE_RESCAN);
        let bs = discover_batteries();
        if !bs.is_empty() {
            tracing::info!("battery appeared; resuming");
            return bs;
        }
        tracing::debug!(
            "still no batteries; rescanning in {}s",
            NO_DEVICE_RESCAN.as_secs()
        );
    }
}

/// Return every `power_supply` device of `type=Battery` on this system.
fn discover_batteries() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(SYSFS_ROOT) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let t = read_string(&p.join("type")).unwrap_or_default();
            t == "Battery"
        })
        .collect();
    out.sort();
    out
}

/// Read one battery's state from sysfs.
fn read_battery(dir: &Path) -> Option<BatteryReading> {
    let capacity = read_f64(&dir.join("capacity"))?;
    let state = read_string(&dir.join("status"))
        .map(|s| BatteryState::from_sysfs(&s))
        .unwrap_or(BatteryState::Unknown);
    // Prefer energy (Wh-equivalent) for weight; fall back to charge
    // (Ah-equivalent); last resort 1.0 for equal-weight average.
    let weight = read_f64(&dir.join("energy_full"))
        .or_else(|| read_f64(&dir.join("charge_full")))
        .unwrap_or(1.0);
    Some(BatteryReading {
        capacity,
        state,
        weight,
    })
}

/// Aggregate every battery into a single (capacity %, dominant state)
/// pair. With one battery this is a passthrough; with multiple, the
/// capacity is weighted by full-charge size so a tiny secondary cell
/// can't drag the headline number around.
fn aggregate(readings: &[BatteryReading]) -> Option<(f64, BatteryState)> {
    if readings.is_empty() {
        return None;
    }
    let total_weight: f64 = readings.iter().map(|r| r.weight).sum();
    let weighted_sum: f64 = readings.iter().map(|r| r.capacity * r.weight).sum();
    let pct = if total_weight > 0.0 {
        weighted_sum / total_weight
    } else {
        readings.iter().map(|r| r.capacity).sum::<f64>() / readings.len() as f64
    };
    // Dominant-state rule: any Charging dominates (the user just plugged
    // in, that's the news). Otherwise prefer Discharging > Full > others.
    let priority = |s: BatteryState| match s {
        BatteryState::Charging => 0,
        BatteryState::Discharging => 1,
        BatteryState::FullyCharged => 2,
        BatteryState::PendingCharge | BatteryState::PendingDischarge => 3,
        BatteryState::Empty => 4,
        BatteryState::Unknown => 5,
    };
    let state = readings
        .iter()
        .map(|r| r.state)
        .min_by_key(|s| priority(*s))
        .unwrap_or(BatteryState::Unknown);
    Some((pct, state))
}

/// One severity band in the capacity range. `upper` is inclusive: a
/// reading of `pct ≤ upper` falls into this band (after testing the
/// previous bands in order). Bands cascade so the first match wins.
///
/// Band names double as the unit of alert filtering (`--alert-bands`)
/// and as the OSD style override during discharge — `pick_style`,
/// `pick_icon`, and `refresh_and_send` all consult the same table.
struct Band {
    name: &'static str,
    upper: i32,
    /// OSD style applied when discharging. Charging always uses
    /// `"normal"` regardless — a charging battery isn't a problem.
    style: &'static str,
    /// Freedesktop icon name for discharging.
    icon: &'static str,
    /// Freedesktop icon name for charging.
    icon_charging: &'static str,
}

const BANDS: &[Band] = &[
    Band {
        name: "empty",
        upper: 5,
        style: "critical",
        icon: "battery-empty",
        icon_charging: "battery-empty-charging",
    },
    Band {
        name: "caution",
        upper: 20,
        style: "warn",
        icon: "battery-caution",
        icon_charging: "battery-caution-charging",
    },
    Band {
        name: "low",
        upper: 50,
        style: "normal",
        icon: "battery-low",
        icon_charging: "battery-low-charging",
    },
    Band {
        name: "good",
        upper: 80,
        style: "normal",
        icon: "battery-good",
        icon_charging: "battery-good-charging",
    },
    Band {
        name: "full",
        upper: 100,
        style: "normal",
        icon: "battery-full",
        icon_charging: "battery-full-charged",
    },
];

/// Resolve a percentage to its band. Caps at the topmost band so
/// out-of-range readings (e.g. 102 % during a calibration tick)
/// don't panic.
fn band_for(pct: i32) -> &'static Band {
    BANDS
        .iter()
        .find(|b| pct <= b.upper)
        .unwrap_or(BANDS.last().unwrap())
}

fn pick_icon(pct: f64, state: BatteryState) -> &'static str {
    let band = band_for(pct as i32);
    if matches!(state, BatteryState::Charging | BatteryState::FullyCharged) {
        band.icon_charging
    } else {
        band.icon
    }
}

fn pick_style(pct: f64, state: BatteryState) -> &'static str {
    if matches!(state, BatteryState::Charging | BatteryState::FullyCharged) {
        return "normal";
    }
    band_for(pct as i32).style
}

fn fire(
    socket: &Option<PathBuf>,
    source: &str,
    pct: f64,
    state: BatteryState,
) -> awob_client::Result<()> {
    let mut c = match socket {
        Some(p) => Client::connect_to(p)?,
        None => Client::connect()?,
    };
    let icon = pick_icon(pct, state);
    let style = pick_style(pct, state);
    let app = format!("Battery: {}", state.slug());
    let s = Send::new("battery", pct)
        .listener_id(LISTENER_ID)
        .source(source)
        .icon(icon)
        .style(style)
        .app(app);
    c.send(s.build())
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let source = cli
        .source
        .clone()
        .map(|s| format!("battery-{s}"))
        .unwrap_or_else(|| "battery".into());
    let state_filter = parse_state_filter(&cli.states);
    let alert_bands = parse_alert_bands(&cli.alert_bands);
    tracing::info!(
        "source={source} states={} alert-bands={} (sysfs + udev)",
        cli.states,
        cli.alert_bands
    );

    let batteries = wait_for_batteries();
    for b in &batteries {
        tracing::info!("battery: {}", b.display());
    }

    // Subscribe to power_supply uevents. Captures AC plug, battery
    // state transitions, and capacity threshold crossings instantly.
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("power_supply")?
        .listen()?;
    let monitor_fd = monitor.as_fd();

    let mut last: Option<(BatteryState, &'static str)> = None;
    let mut last_rescan = Instant::now();
    // Per-battery cache of state + capacity, keyed by the kernel's
    // POWER_SUPPLY_NAME. Updated authoritatively by uevent properties
    // when battery events fire; refreshed from sysfs otherwise.
    // Aggregated for every send so multi-battery systems produce one
    // OSD per change.
    let mut cache: std::collections::HashMap<String, BatteryReading> =
        std::collections::HashMap::new();
    // Burst-poll window: extended on every uevent. While `Some` and
    // not yet elapsed we tighten the wait to BURST_POLL_INTERVAL and
    // re-read sysfs each tick — handles hardware where the battery
    // driver lags AC plug events.
    let mut burst_until: Option<Instant> = None;

    // Seed the cache from sysfs and fire the initial OSD.
    for path in &batteries {
        if let Some(r) = read_battery(path) {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            cache.insert(name, r);
        }
    }
    refresh_and_send(
        &cache,
        &cli.socket,
        &source,
        &state_filter,
        &alert_bands,
        &mut last,
    );

    loop {
        let now = Instant::now();
        let in_burst = burst_until.map(|t| now < t).unwrap_or(false);
        let until_rescan = RESCAN_INTERVAL.saturating_sub(now.duration_since(last_rescan));
        let wait = if in_burst {
            BURST_POLL_INTERVAL.min(burst_until.unwrap().saturating_duration_since(now))
        } else {
            until_rescan
        };
        let timeout_ms: i32 = wait.as_millis().min(i32::MAX as u128) as i32;
        let mut pfds = [PollFd::new(monitor_fd, PollFlags::POLLIN)];
        let timeout = PollTimeout::try_from(timeout_ms).unwrap_or(PollTimeout::NONE);
        match poll(&mut pfds, timeout) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(format!("poll: {e}").into()),
        }

        // Drain udev events. For each Battery-type event, update the
        // cache from event properties — that's the kernel-authoritative
        // state at announce time, with no sysfs race. Events for AC
        // adapters / USB-PD don't carry battery state so they only
        // serve to extend the burst window.
        let mut got_event = false;
        let mut got_battery_event = false;
        for ev in monitor.iter() {
            got_event = true;
            if update_cache_from_event(&mut cache, &ev) {
                got_battery_event = true;
            }
        }

        let now = Instant::now();
        if got_event {
            burst_until = Some(now + BURST_DURATION);
        }
        let still_in_burst = burst_until.map(|t| now < t).unwrap_or(false);
        let due_for_rescan = now.duration_since(last_rescan) >= RESCAN_INTERVAL;

        // Re-read sysfs as the safety net: catches state transitions
        // on hardware whose battery driver doesn't fire its own uevent
        // promptly after AC plug. Battery events update the cache
        // first (above), so this is a redundant write for those.
        let need_sysfs_refresh =
            !got_battery_event && (got_event || still_in_burst || due_for_rescan);
        if need_sysfs_refresh {
            for path in &batteries {
                if let Some(r) = read_battery(path) {
                    let name = path
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    cache.insert(name, r);
                }
            }
        }

        if got_event || still_in_burst || due_for_rescan {
            refresh_and_send(
                &cache,
                &cli.socket,
                &source,
                &state_filter,
                &alert_bands,
                &mut last,
            );
            last_rescan = now;
        }
        if !still_in_burst {
            burst_until = None;
        }
    }
}

/// Apply a power_supply uevent to the cache. Returns `true` if the
/// event was for a Battery-type device whose state we extracted.
/// Non-battery events (AC adapters, USB-PD ports) return `false` —
/// they still extend the burst window from the caller's side, but
/// they don't update battery cache directly.
fn update_cache_from_event(
    cache: &mut std::collections::HashMap<String, BatteryReading>,
    event: &udev::Event,
) -> bool {
    let dev = event.device();
    let typ = dev
        .property_value("POWER_SUPPLY_TYPE")
        .and_then(|s| s.to_str().map(String::from));
    if typ.as_deref() != Some("Battery") {
        return false;
    }
    let Some(name) = dev
        .property_value("POWER_SUPPLY_NAME")
        .and_then(|s| s.to_str().map(String::from))
    else {
        return false;
    };
    let status = dev
        .property_value("POWER_SUPPLY_STATUS")
        .and_then(|s| s.to_str())
        .map(BatteryState::from_sysfs)
        .unwrap_or(BatteryState::Unknown);
    let capacity = dev
        .property_value("POWER_SUPPLY_CAPACITY")
        .and_then(|s| s.to_str())
        .and_then(|s| s.parse::<f64>().ok());

    // Preserve the existing weight (set from sysfs at startup).
    // Without it, multi-battery aggregation collapses to a flat mean.
    let weight = cache.get(&name).map(|r| r.weight).unwrap_or(1.0);
    cache.insert(
        name,
        BatteryReading {
            capacity: capacity
                .unwrap_or_else(|| cache.values().next().map(|r| r.capacity).unwrap_or(0.0)),
            state: status,
            weight,
        },
    );
    true
}

/// Capacity thresholds that fire an OSD when crossed in either
/// Parse the `--alert-bands` argument into a set of band names.
/// Recognises every band name from [`BANDS`] plus `all` (every band)
/// and `none` (only state transitions fire).
fn parse_alert_bands(arg: &str) -> HashSet<&'static str> {
    let mut out = HashSet::new();
    for token in arg.split(',') {
        let t = token.trim();
        if t.is_empty() {
            continue;
        }
        if t.eq_ignore_ascii_case("all") {
            for b in BANDS {
                out.insert(b.name);
            }
            continue;
        }
        if t.eq_ignore_ascii_case("none") {
            // Explicit "no bands" — caller will only see OSDs on
            // state transitions. Returning an empty set conveys that.
            out.clear();
            return out;
        }
        if let Some(b) = BANDS.iter().find(|b| b.name.eq_ignore_ascii_case(t)) {
            out.insert(b.name);
        } else {
            tracing::info!("unknown band `{t}` in --alert-bands");
        }
    }
    out
}

/// Read every battery, aggregate, and fire an OSD if either:
///
/// * the dominant state changed (Discharging↔Charging↔FullyCharged), or
/// * capacity entered a new band whose name is in `alert_bands`.
///
/// "Discharging from 87 % to 21 %" is silent unless `low` and/or
/// `caution` are in `alert_bands` — only band entries we explicitly
/// asked about and state transitions surface as OSDs.
///
/// On the first call (`*last == None`) the function records the
/// current state and band as a baseline *without* firing. That keeps
/// the listener silent on startup / supervisor respawn — matching
/// pipewire / backlight / keyboard-backlight, which only fire on real
/// changes. Without this the user would see a "discharging at 73 %"
/// OSD every time the daemon was restarted, which is just noise.
fn refresh_and_send(
    cache: &std::collections::HashMap<String, BatteryReading>,
    socket: &Option<PathBuf>,
    source: &str,
    state_filter: &HashSet<BatteryState>,
    alert_bands: &HashSet<&'static str>,
    last: &mut Option<(BatteryState, &'static str)>,
) {
    let readings: Vec<BatteryReading> = cache.values().cloned().collect();
    let Some((pct, state)) = aggregate(&readings) else {
        return;
    };
    if !state_filter.contains(&state) {
        return;
    }
    let pct_int = pct.round() as i32;
    let band = band_for(pct_int);
    let fire_now = match *last {
        Some((prev_state, prev_band_name)) => {
            prev_state != state || (band.name != prev_band_name && alert_bands.contains(band.name))
        }
        // First observation: silently record the baseline.
        None => false,
    };
    *last = Some((state, band.name));
    if !fire_now {
        return;
    }
    if let Err(e) = fire(socket, source, pct, state) {
        tracing::info!("send: {e}");
    }
}

fn main() -> ExitCode {
    awob_client::init_tracing("info");
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "awob-listener-battery starting"
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
    fn icon_buckets() {
        assert_eq!(pick_icon(95.0, BatteryState::Discharging), "battery-full");
        assert_eq!(pick_icon(60.0, BatteryState::Discharging), "battery-good");
        assert_eq!(
            pick_icon(20.0, BatteryState::Discharging),
            "battery-caution"
        );
        assert_eq!(pick_icon(5.0, BatteryState::Discharging), "battery-empty");
        assert_eq!(
            pick_icon(95.0, BatteryState::Charging),
            "battery-full-charged"
        );
        assert_eq!(
            pick_icon(60.0, BatteryState::Charging),
            "battery-good-charging"
        );
    }

    #[test]
    fn style_priorities() {
        assert_eq!(pick_style(50.0, BatteryState::Charging), "normal");
        assert_eq!(pick_style(5.0, BatteryState::Charging), "normal");
        assert_eq!(pick_style(50.0, BatteryState::Discharging), "normal");
        assert_eq!(pick_style(20.0, BatteryState::Discharging), "warn");
        assert_eq!(pick_style(5.0, BatteryState::Discharging), "critical");
    }

    #[test]
    fn band_for_lookup() {
        assert_eq!(band_for(0).name, "empty");
        assert_eq!(band_for(5).name, "empty");
        assert_eq!(band_for(6).name, "caution");
        assert_eq!(band_for(20).name, "caution");
        assert_eq!(band_for(21).name, "low");
        assert_eq!(band_for(50).name, "low");
        assert_eq!(band_for(51).name, "good");
        assert_eq!(band_for(80).name, "good");
        assert_eq!(band_for(81).name, "full");
        assert_eq!(band_for(100).name, "full");
        // Out-of-range readings cap at the topmost band.
        assert_eq!(band_for(102).name, "full");
    }

    #[test]
    fn parse_alert_bands_defaults() {
        let s = parse_alert_bands("empty,caution");
        assert!(s.contains("empty"));
        assert!(s.contains("caution"));
        assert!(!s.contains("low"));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn parse_alert_bands_all() {
        let s = parse_alert_bands("all");
        assert_eq!(s.len(), BANDS.len());
    }

    #[test]
    fn parse_alert_bands_none() {
        let s = parse_alert_bands("none");
        assert!(s.is_empty());
        // `none` wins even if mixed with named bands — caller asked for silence.
        let s2 = parse_alert_bands("empty,none,caution");
        assert!(s2.is_empty());
    }

    #[test]
    fn parse_alert_bands_unknown_is_ignored() {
        let s = parse_alert_bands("empty,bogus,caution");
        assert!(s.contains("empty"));
        assert!(s.contains("caution"));
        assert_eq!(s.len(), 2);
    }

    #[test]
    fn aggregate_single() {
        let r = vec![BatteryReading {
            capacity: 73.0,
            state: BatteryState::Charging,
            weight: 50.0,
        }];
        let (pct, state) = aggregate(&r).unwrap();
        assert!((pct - 73.0).abs() < f64::EPSILON);
        assert_eq!(state, BatteryState::Charging);
    }

    #[test]
    fn aggregate_two_batteries_weighted() {
        // A 80-Wh battery at 50% + a 20-Wh battery at 90% should
        // weight toward 50% (the bigger one), not the simple mean.
        let r = vec![
            BatteryReading {
                capacity: 50.0,
                state: BatteryState::Discharging,
                weight: 80.0,
            },
            BatteryReading {
                capacity: 90.0,
                state: BatteryState::FullyCharged,
                weight: 20.0,
            },
        ];
        let (pct, state) = aggregate(&r).unwrap();
        // (50*80 + 90*20)/100 = 58
        assert!((pct - 58.0).abs() < 0.001, "got {pct}");
        // Discharging beats FullyCharged in priority.
        assert_eq!(state, BatteryState::Discharging);
    }

    #[test]
    fn from_sysfs_strings() {
        assert_eq!(BatteryState::from_sysfs("Charging"), BatteryState::Charging);
        assert_eq!(
            BatteryState::from_sysfs("Discharging"),
            BatteryState::Discharging
        );
        assert_eq!(BatteryState::from_sysfs("Full"), BatteryState::FullyCharged);
        assert_eq!(
            BatteryState::from_sysfs("Not charging"),
            BatteryState::PendingDischarge
        );
        assert_eq!(BatteryState::from_sysfs("Empty"), BatteryState::Empty);
        assert_eq!(BatteryState::from_sysfs("nonsense"), BatteryState::Unknown);
    }
}
