//! awob UPower listener.
//!
//! Subscribes to UPower's aggregate battery device (`DisplayDevice`) via the
//! system D-Bus and emits OSD events on configurable triggers:
//!
//! * `--states` (default `all`) — fire on transitions into the listed states
//! * `--warning-levels` (default `low,critical,action`) — fire on transitions
//!   into the listed warning levels
//!
//! Per the design discussion: an AC plug fires implicitly via the
//! `discharging → charging` state transition, so no special handling.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::ExitCode;

use awob_client::{Client, Send};
use clap::Parser;
use zbus::blocking::{Connection, Proxy};

const LISTENER_ID: &str = "awob-listener-upower";

#[derive(Parser, Debug)]
#[command(version, about = "awob — upower battery listener")]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Stable source suffix. Final source = "upower-<source>". Defaults to
    /// a per-process random hex.
    #[arg(long)]
    source: Option<String>,

    /// Comma-separated list of states to react to. Default `all`.
    /// Recognised: `charging`, `discharging`, `empty`, `fully-charged`,
    /// `pending-charge`, `pending-discharge`, or `all`.
    #[arg(long, default_value = "all")]
    states: String,

    /// Comma-separated list of warning levels that trigger an OSD.
    /// Recognised: `none`, `discharging`, `low`, `critical`, `action`.
    /// Default fires on transitions into "low" or below.
    #[arg(long = "warning-levels", default_value = "low,critical,action")]
    warning_levels: String,
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
    fn from_dbus(v: u32) -> Self {
        match v {
            1 => Self::Charging,
            2 => Self::Discharging,
            3 => Self::Empty,
            4 => Self::FullyCharged,
            5 => Self::PendingCharge,
            6 => Self::PendingDischarge,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarningLevel { Unknown, None, Discharging, Low, Critical, Action }

impl WarningLevel {
    fn from_dbus(v: u32) -> Self {
        match v {
            1 => Self::None,
            2 => Self::Discharging,
            3 => Self::Low,
            4 => Self::Critical,
            5 => Self::Action,
            _ => Self::Unknown,
        }
    }
    fn parse_slug(s: &str) -> Option<Self> {
        Some(match s.trim().to_ascii_lowercase().as_str() {
            "none" => Self::None,
            "discharging" => Self::Discharging,
            "low" => Self::Low,
            "critical" => Self::Critical,
            "action" => Self::Action,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

fn parse_state_filter(arg: &str) -> HashSet<BatteryState> {
    let mut out = HashSet::new();
    for token in arg.split(',') {
        let t = token.trim();
        if t.is_empty() { continue; }
        if t.eq_ignore_ascii_case("all") {
            for v in [
                BatteryState::Charging, BatteryState::Discharging, BatteryState::Empty,
                BatteryState::FullyCharged, BatteryState::PendingCharge,
                BatteryState::PendingDischarge, BatteryState::Unknown,
            ] { out.insert(v); }
            continue;
        }
        if let Some(s) = BatteryState::parse_slug(t) { out.insert(s); }
        else { eprintln!("awob-listener-upower: unknown state `{t}` in --states"); }
    }
    out
}

fn parse_warning_filter(arg: &str) -> HashSet<WarningLevel> {
    let mut out = HashSet::new();
    for token in arg.split(',') {
        let t = token.trim();
        if t.is_empty() { continue; }
        if let Some(w) = WarningLevel::parse_slug(t) { out.insert(w); }
        else { eprintln!("awob-listener-upower: unknown warning level `{t}` in --warning-levels"); }
    }
    out
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
            "low"  => "battery-low-charging",
            "caution" => "battery-caution-charging",
            _ => "battery-empty-charging",
        }
    } else {
        match bucket {
            "full" => "battery-full",
            "good" => "battery-good",
            "low"  => "battery-low",
            "caution" => "battery-caution",
            _ => "battery-empty",
        }
    }
}

fn pick_style(pct: f64, warning: WarningLevel) -> &'static str {
    match warning {
        WarningLevel::Action | WarningLevel::Critical => "critical",
        WarningLevel::Low => "warn",
        _ => if pct < 10.0 { "critical" }
             else if pct < 25.0 { "warn" }
             else { "normal" },
    }
}

fn fire(
    socket: &Option<PathBuf>,
    source: &str,
    pct: f64,
    state: BatteryState,
    warning: WarningLevel,
) -> awob_client::Result<()> {
    let mut c = match socket {
        Some(p) => Client::connect_to(p)?,
        None => Client::connect()?,
    };
    let icon = pick_icon(pct, state);
    let style = pick_style(pct, warning);
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
    // Stable source — there's one UPower aggregate device per machine, so
    // a fixed `upower` source identifies it uniquely. Restarts re-use the
    // same source (no respawn duplicate-listener warning); a custom value
    // can still be passed via `--source` if needed.
    let source = cli.source.clone()
        .map(|s| format!("upower-{s}"))
        .unwrap_or_else(|| "upower".into());
    eprintln!("awob-listener-upower: source={source}");

    let state_filter = parse_state_filter(&cli.states);
    let warning_filter = parse_warning_filter(&cli.warning_levels);
    eprintln!(
        "  filters: states={} warning-levels={}",
        cli.states, cli.warning_levels,
    );

    let conn = Connection::system()?;
    let proxy = Proxy::new(
        &conn,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.UPower.Device",
    )?;

    let mut last_state = BatteryState::from_dbus(proxy.get_property::<u32>("State")?);
    let mut last_warning = WarningLevel::from_dbus(proxy.get_property::<u32>("WarningLevel")?);
    let percentage: f64 = proxy.get_property("Percentage")?;
    eprintln!(
        "  initial: pct={percentage:.0} state={} warning={:?}",
        last_state.slug(), last_warning,
    );

    // Subscribe to PropertiesChanged on the Properties interface for this object.
    let props_proxy = Proxy::new(
        &conn,
        "org.freedesktop.UPower",
        "/org/freedesktop/UPower/devices/DisplayDevice",
        "org.freedesktop.DBus.Properties",
    )?;
    let signal_iter = props_proxy.receive_signal("PropertiesChanged")?;

    for msg in signal_iter {
        // Reread current values; the signal payload could be parsed but
        // the property cache is simpler and reliable.
        let pct: f64 = match proxy.get_property("Percentage") { Ok(v) => v, Err(_) => continue };
        let state = BatteryState::from_dbus(proxy.get_property::<u32>("State").unwrap_or(0));
        let warning = WarningLevel::from_dbus(proxy.get_property::<u32>("WarningLevel").unwrap_or(0));

        let state_changed = state != last_state;
        let warning_changed = warning != last_warning;

        let mut should_fire = false;
        if state_changed && state_filter.contains(&state) { should_fire = true; }
        if warning_changed && warning_filter.contains(&warning) { should_fire = true; }

        if should_fire {
            if let Err(e) = fire(&cli.socket, &source, pct, state, warning) {
                eprintln!("awob-listener-upower: send failed: {e}");
            }
        }
        last_state = state;
        last_warning = warning;
        let _ = msg; // silence unused
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => { eprintln!("awob-listener-upower: {e}"); ExitCode::from(1) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn icon_buckets() {
        assert_eq!(pick_icon(95.0, BatteryState::Discharging), "battery-full");
        assert_eq!(pick_icon(50.0, BatteryState::Discharging), "battery-good");
        assert_eq!(pick_icon(30.0, BatteryState::Discharging), "battery-low");
        assert_eq!(pick_icon(15.0, BatteryState::Discharging), "battery-caution");
        assert_eq!(pick_icon(5.0,  BatteryState::Discharging), "battery-empty");
        assert_eq!(pick_icon(50.0, BatteryState::Charging), "battery-good-charging");
    }
    #[test] fn style_thresholds() {
        assert_eq!(pick_style(50.0, WarningLevel::None), "normal");
        assert_eq!(pick_style(20.0, WarningLevel::None), "warn");
        assert_eq!(pick_style(5.0,  WarningLevel::None), "critical");
        // WarningLevel overrides percent
        assert_eq!(pick_style(80.0, WarningLevel::Critical), "critical");
        assert_eq!(pick_style(80.0, WarningLevel::Low), "warn");
    }
    #[test] fn parse_filters() {
        let s = parse_state_filter("charging,discharging");
        assert!(s.contains(&BatteryState::Charging));
        assert!(s.contains(&BatteryState::Discharging));
        assert!(!s.contains(&BatteryState::FullyCharged));

        let s = parse_state_filter("all");
        assert!(s.contains(&BatteryState::FullyCharged));

        let w = parse_warning_filter("low,critical,action");
        assert!(w.contains(&WarningLevel::Low));
        assert!(w.contains(&WarningLevel::Critical));
        assert!(w.contains(&WarningLevel::Action));
        assert!(!w.contains(&WarningLevel::None));
    }
}
