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

/// Schedule an extra read this far after every uevent. On AC plug the
/// kernel often fires the `power_supply` uevent for the AC adapter
/// (`ACAD/online`) BEFORE the battery driver has updated `BAT*/status`
/// to `"Charging"`. A single read at uevent time can therefore observe
/// the *old* battery state. The follow-up refresh catches the lagged
/// transition without making us poll continuously.
const EVENT_FOLLOWUP: Duration = Duration::from_millis(800);

#[derive(Parser, Debug)]
#[command(version, about = "awob — battery listener (sysfs + udev)")]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Stable source suffix. Final source = "battery-<source>". Defaults
    /// to "battery" so restarts re-use the same source.
    #[arg(long)]
    source: Option<String>,

    /// Comma-separated list of states to react to. Default `all`.
    /// Recognised: `charging`, `discharging`, `empty`, `fully-charged`,
    /// `pending-charge`, `pending-discharge`, `unknown`, or `all`.
    #[arg(long, default_value = "all")]
    states: String,
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
            eprintln!("awob-listener-battery: unknown state `{t}` in --states");
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

fn pick_icon(pct: f64, state: BatteryState) -> &'static str {
    let charging = matches!(state, BatteryState::Charging | BatteryState::FullyCharged);
    let bucket = match pct as i32 {
        v if v >= 80 => "full",
        v if v >= 50 => "good",
        v if v >= 25 => "low",
        v if v >= 10 => "caution",
        _ => "empty",
    };
    if charging {
        match bucket {
            "full" => "battery-full-charged",
            "good" => "battery-good-charging",
            "low" => "battery-low-charging",
            "caution" => "battery-caution-charging",
            _ => "battery-empty-charging",
        }
    } else {
        match bucket {
            "full" => "battery-full",
            "good" => "battery-good",
            "low" => "battery-low",
            "caution" => "battery-caution",
            _ => "battery-empty",
        }
    }
}

fn pick_style(pct: f64, state: BatteryState) -> &'static str {
    if matches!(state, BatteryState::Charging | BatteryState::FullyCharged) {
        return "normal";
    }
    if pct < 10.0 {
        "critical"
    } else if pct < 25.0 {
        "warn"
    } else {
        "normal"
    }
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
    eprintln!(
        "awob-listener-battery: source={source} states={} (sysfs + udev)",
        cli.states
    );

    let batteries = discover_batteries();
    if batteries.is_empty() {
        eprintln!(
            "awob-listener-battery: no batteries under {SYSFS_ROOT} \
             (type=Battery); nothing to watch"
        );
        return Ok(());
    }
    for b in &batteries {
        eprintln!("  battery: {}", b.display());
    }

    // Subscribe to power_supply uevents. Captures AC plug, battery
    // state transitions, and capacity threshold crossings instantly.
    let monitor = udev::MonitorBuilder::new()?
        .match_subsystem("power_supply")?
        .listen()?;
    let monitor_fd = monitor.as_fd();

    let mut last: Option<(BatteryState, i32)> = None;
    let mut last_rescan = Instant::now();
    // When set, do an extra read at this instant — see `EVENT_FOLLOWUP`.
    let mut followup_at: Option<Instant> = None;

    // Initial fire so the OSD has current state at startup.
    refresh_and_send(
        &batteries,
        &cli.socket,
        &source,
        &state_filter,
        &mut last,
        true,
    );

    loop {
        // Wait for the soonest of: (a) a uevent on the udev socket,
        // (b) the cascade follow-up after a recent uevent, (c) the
        // periodic rescan backstop.
        let now = Instant::now();
        let until_followup = followup_at
            .map(|t| t.saturating_duration_since(now))
            .unwrap_or(Duration::MAX);
        let until_rescan = RESCAN_INTERVAL.saturating_sub(now.duration_since(last_rescan));
        let wait = until_followup.min(until_rescan);
        let timeout_ms: i32 = wait.as_millis().min(i32::MAX as u128) as i32;
        let mut pfds = [PollFd::new(monitor_fd, PollFlags::POLLIN)];
        let timeout = PollTimeout::try_from(timeout_ms).unwrap_or(PollTimeout::NONE);
        match poll(&mut pfds, timeout) {
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => continue,
            Err(e) => return Err(format!("poll: {e}").into()),
        }

        // Drain any pending udev events. We don't actually inspect
        // them — the trigger alone is enough to prompt a refresh.
        let mut got_event = false;
        for _ in monitor.iter() {
            got_event = true;
        }

        let now = Instant::now();
        let followup_due = followup_at.map(|t| now >= t).unwrap_or(false);
        let due_for_rescan = now.duration_since(last_rescan) >= RESCAN_INTERVAL;

        if got_event || followup_due || due_for_rescan {
            refresh_and_send(
                &batteries,
                &cli.socket,
                &source,
                &state_filter,
                &mut last,
                false,
            );
            last_rescan = now;
            // After any uevent, schedule a follow-up read to catch a
            // possibly-lagged BAT/status transition. A follow-up that
            // already fired clears the slot.
            followup_at = if got_event {
                Some(now + EVENT_FOLLOWUP)
            } else {
                None
            };
        }
    }
}

/// Read every battery, aggregate, and fire an OSD if either the
/// dominant state changed or capacity moved by ≥1%. `force=true`
/// fires unconditionally (used at startup so the initial state hits
/// the screen even if nothing's changed since the last daemon boot).
fn refresh_and_send(
    batteries: &[PathBuf],
    socket: &Option<PathBuf>,
    source: &str,
    state_filter: &HashSet<BatteryState>,
    last: &mut Option<(BatteryState, i32)>,
    force: bool,
) {
    let readings: Vec<BatteryReading> = batteries.iter().filter_map(|p| read_battery(p)).collect();
    let Some((pct, state)) = aggregate(&readings) else {
        return;
    };
    if !state_filter.contains(&state) {
        return;
    }
    let pct_int = pct.round() as i32;
    let changed = match *last {
        Some((prev_state, prev_pct)) => prev_state != state || (prev_pct - pct_int).abs() >= 1,
        None => true,
    };
    if !changed && !force {
        return;
    }
    *last = Some((state, pct_int));
    if let Err(e) = fire(socket, source, pct, state) {
        eprintln!("awob-listener-battery: send: {e}");
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("awob-listener-battery: {e}");
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
        assert_eq!(pick_icon(20.0, BatteryState::Discharging), "battery-caution");
        assert_eq!(pick_icon(5.0, BatteryState::Discharging), "battery-empty");
        assert_eq!(pick_icon(95.0, BatteryState::Charging), "battery-full-charged");
        assert_eq!(pick_icon(60.0, BatteryState::Charging), "battery-good-charging");
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
            BatteryReading { capacity: 50.0, state: BatteryState::Discharging, weight: 80.0 },
            BatteryReading { capacity: 90.0, state: BatteryState::FullyCharged, weight: 20.0 },
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
        assert_eq!(BatteryState::from_sysfs("Discharging"), BatteryState::Discharging);
        assert_eq!(BatteryState::from_sysfs("Full"), BatteryState::FullyCharged);
        assert_eq!(BatteryState::from_sysfs("Not charging"), BatteryState::PendingDischarge);
        assert_eq!(BatteryState::from_sysfs("Empty"), BatteryState::Empty);
        assert_eq!(BatteryState::from_sysfs("nonsense"), BatteryState::Unknown);
    }
}
