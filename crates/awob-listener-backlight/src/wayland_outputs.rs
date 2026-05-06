//! One-shot Wayland `wl_output` probe.
//!
//! Returns a map keyed by `wl_output.name` (`"eDP-1"`, `"HDMI-A-1"`)
//! with each output's `make` + `model` + `description`. Best-effort:
//! returns empty on any failure so the caller's fallback wins.

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
    in_progress: HashMap<u32, OutputInfo>,
    finalised: Vec<OutputInfo>,
    expected: usize,
}

/// Probe Wayland outputs. Best-effort: silently returns empty on any
/// failure so the caller's fallback path always wins.
pub fn probe(timeout: Duration) -> HashMap<String, OutputInfo> {
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let display = conn.display();
    let mut event_queue: EventQueue<ProbeState> = conn.new_event_queue();
    let qh = event_queue.handle();
    let _registry = display.get_registry(&qh, ());

    let mut state = ProbeState::default();
    // First roundtrip: registry globals, output bindings.
    let _ = event_queue.roundtrip(&mut state);

    // Drive the queue until every output fires `done` or timeout elapses.
    let deadline = Instant::now() + timeout;
    while state.finalised.len() < state.expected && Instant::now() < deadline {
        if event_queue.dispatch_pending(&mut state).is_err() {
            break;
        }
        if event_queue.flush().is_err() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
        if event_queue.roundtrip(&mut state).is_err() {
            break;
        }
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
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = ev
            && interface == wl_output::WlOutput::interface().name
        {
            // Bind v4 if the compositor offers it; v4 added the `name`
            // event which is exactly what we want. Older versions still
            // populate make/model via the `geometry` event.
            let bound_v = version.clamp(1, 4);
            let output = registry.bind::<wl_output::WlOutput, _, _>(name, bound_v, qh, ());
            let id = output.id().protocol_id();
            state.in_progress.insert(id, OutputInfo::default());
            state.expected += 1;
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
