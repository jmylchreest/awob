//! One-shot Wayland `wl_output` probe.
//!
//! Connects to `$WAYLAND_DISPLAY`, enumerates outputs, and returns a map
//! keyed by `wl_output.name` (the connector slug, e.g. `"eDP-1"`,
//! `"HDMI-A-1"`) with each output's `make` + `model` + `description`.
//!
//! Used at backlight-listener startup to translate the connector that
//! sysfs reports (`/sys/class/backlight/<dev>/device → card1-eDP-1`) into
//! a friendly label like `"BOE NE135A1M-NY1"`. Returns an empty map (and
//! the caller falls back to a heuristic) on:
//!
//! * `WAYLAND_DISPLAY` unset
//! * compositor hangs / refuses connect
//! * no outputs reported
//!
//! Connection is opened, dispatched to a short timeout, then dropped.
//! No long-running Wayland code in this listener.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use wayland_client::protocol::{wl_output, wl_registry};
use wayland_client::{Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child};

#[derive(Debug, Default, Clone)]
pub struct OutputInfo {
    pub name: String,
    pub make: String,
    pub model: String,
    pub description: String,
}

#[derive(Default)]
struct ProbeState {
    /// Outputs seen so far, indexed by their wl_output proxy ID.
    in_progress: HashMap<u32, OutputInfo>,
    /// Outputs whose `done` event has fired and which are now finalised.
    finalised: Vec<OutputInfo>,
    expected: usize,
}

/// Probe Wayland outputs. Best-effort: silently returns empty on any
/// failure so the caller's fallback path always wins.
pub fn probe(timeout: Duration) -> HashMap<String, OutputInfo> {
    let conn = match Connection::connect_to_env() { Ok(c) => c, Err(_) => return HashMap::new() };
    let display = conn.display();
    let mut event_queue: EventQueue<ProbeState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut state = ProbeState::default();
    // First roundtrip: registry globals, output bindings.
    let _ = event_queue.roundtrip(&mut state);

    // Drive the queue until every output has fired its `done` event, or
    // the timeout elapses. The pipewire / hyprland combination usually
    // finishes within a single dispatch but multi-output systems can
    // need a second.
    let deadline = Instant::now() + timeout;
    while state.finalised.len() < state.expected && Instant::now() < deadline {
        if event_queue.dispatch_pending(&mut state).is_err() { break; }
        if event_queue.flush().is_err() { break; }
        std::thread::sleep(Duration::from_millis(20));
        if event_queue.roundtrip(&mut state).is_err() { break; }
    }

    let mut out = HashMap::new();
    for info in state.finalised {
        if !info.name.is_empty() {
            out.insert(info.name.clone(), info);
        }
    }
    out
}

impl Dispatch<wl_registry::WlRegistry, ()> for ProbeState {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        ev: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, version } = ev {
            if interface == wl_output::WlOutput::interface().name {
                // Bind v4 if the compositor offers it; v4 added the `name`
                // event which is exactly what we want. Older versions still
                // populate make/model via the `geometry` event.
                let bound_v = version.min(4).max(1);
                let output = registry.bind::<wl_output::WlOutput, _, _>(name, bound_v, qh, ());
                let id = output.id().protocol_id();
                state.in_progress.insert(id, OutputInfo::default());
                state.expected += 1;
            }
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for ProbeState {
    fn event(
        state: &mut Self,
        output: &wl_output::WlOutput,
        ev: wl_output::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = output.id().protocol_id();
        let entry = state.in_progress.entry(id).or_default();
        match ev {
            wl_output::Event::Geometry { make, model, .. } => {
                entry.make = make;
                entry.model = model;
            }
            wl_output::Event::Name { name } => {
                entry.name = name;
            }
            wl_output::Event::Description { description } => {
                entry.description = description;
            }
            wl_output::Event::Done => {
                if let Some(info) = state.in_progress.remove(&id) {
                    state.finalised.push(info);
                }
            }
            _ => {}
        }
    }

    event_created_child!(ProbeState, wl_output::WlOutput, []);
}
