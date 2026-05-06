//! awob PipeWire listener — event-driven, no polling.
//!
//! The pipewire mainloop binds to `Audio/Sink` and `Audio/Source` nodes
//! and parses each `Props` change for `channelVolumes[]` (cubic curve)
//! and `mute`, pushing `(channel, value, muted)` over an mpsc to a worker
//! thread that does the blocking IPC send.
//!
//! Per-channel listener_ids
//! (`awob-listener-pipewire-{speaker,mic,app-out,app-in}`) keep the
//! daemon's history and duplicate-detector treating each as independent.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc;

use awob_client::listener::ChangeFilter;
use awob_client::{Client, Send};
use clap::Parser;
use pipewire as pw;
use pw::types::ObjectType;

/// Distinguish a physical-device node from an application stream node.
/// Both kinds are pipewire `Node` objects; their `media.class` and the
/// useful labelling fields differ.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NodeKind {
    Device,
    App,
}

/// Per-(channel, kind, node) listener_id. Folding the node hash into the
/// listener_id means the daemon's duplicate-listener detector treats every
/// node as its own logical listener.
///
/// Format:
/// * `awob-listener-pipewire-speaker-<hash>` — physical sink
/// * `awob-listener-pipewire-mic-<hash>`     — physical source
/// * `awob-listener-pipewire-app-out-<hash>` — app output stream (Spotify et al.)
/// * `awob-listener-pipewire-app-in-<hash>`  — app input stream (mic capture)
fn listener_id_for(channel: Channel, kind: NodeKind, source: &str) -> String {
    match (channel, kind) {
        (Channel::Speaker, NodeKind::Device) => format!("awob-listener-pipewire-speaker-{source}"),
        (Channel::Mic, NodeKind::Device) => format!("awob-listener-pipewire-mic-{source}"),
        (Channel::Speaker, NodeKind::App) => format!("awob-listener-pipewire-app-out-{source}"),
        (Channel::Mic, NodeKind::App) => format!("awob-listener-pipewire-app-in-{source}"),
    }
}

#[derive(Parser, Debug)]
#[command(
    version,
    about = "awob — pipewire listener (volume / mute -> OSD, event-driven)"
)]
struct Cli {
    #[arg(long)]
    socket: Option<PathBuf>,

    /// When muted, send `value=0` instead of the current level. Default is
    /// to send the actual level so the bar shows where you'd be unmuted,
    /// styled `muted` so the theme can render that differently.
    #[arg(long)]
    mute_volume_zero: bool,

    /// Skip every Audio/Sink — only mic / source events fire.
    #[arg(long)]
    no_speaker: bool,

    /// Skip every Audio/Source — only speaker / sink events fire.
    #[arg(long)]
    no_mic: bool,

    /// Also track per-app audio streams (`Stream/Output/Audio` for apps
    /// playing audio, `Stream/Input/Audio` for apps recording). Each app
    /// gets its own OSD with the app's name and icon. Default off.
    #[arg(long)]
    per_app: bool,

    /// When `--per-app` is enabled, restrict to streams whose
    /// `application.process.binary` matches one of the listed names. Pass
    /// repeatedly to allow multiple. Default: every app.
    #[arg(long = "per-app-binary")]
    per_app_binaries: Vec<String>,

    /// Forward events only from the current default sink/source (resolved
    /// via PipeWire's `default` Metadata object). Per-app streams ignore
    /// this filter.
    #[arg(long)]
    default_only: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Channel {
    Speaker,
    Mic,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AudioState {
    pub value: f64,
    pub muted: bool,
}

#[allow(
    clippy::too_many_arguments,
    reason = "OSD send-site naturally takes value/state/labels/icon — folding into a struct hurts call-site readability"
)]
pub fn emit_osd(
    socket: &Option<PathBuf>,
    source: &str,
    channel: Channel,
    kind: NodeKind,
    state: AudioState,
    mute_volume_zero: bool,
    app: &str,
    icon_override: Option<&str>,
) -> awob_client::Result<()> {
    let event = match channel {
        Channel::Speaker => "volume",
        Channel::Mic => "mic",
    };
    let listener_id = listener_id_for(channel, kind, source);
    let (default_icon, style) = pick_visuals(channel, state.value, state.muted);
    // App streams use the app's own icon; device streams use the
    // level-based audio-volume-* / microphone-* icons.
    let icon: String = match icon_override {
        Some(i) if !i.is_empty() => i.to_string(),
        _ => default_icon.to_string(),
    };
    let mut value = state.value;
    if state.muted && mute_volume_zero {
        value = 0.0;
    }
    let mut c = Client::connect_or_default(socket.as_deref())?;
    let s = Send::new(event, value)
        .max(1.0)
        .listener_id(listener_id)
        .source(source)
        .icon(icon)
        .style(style)
        .app(app)
        // PipeWire volume changes are dominated by user interaction
        // (volume keys, media-key pavucontrol scroll). Hot-swap whatever
        // ambient bar happens to be visible.
        .preempt(true);
    c.send(s.build())
}

/// `value` is 0..1 (or higher for boost). Returns `(icon, style)`.
fn pick_visuals(ch: Channel, value: f64, muted: bool) -> (&'static str, &'static str) {
    if muted {
        let icon = match ch {
            Channel::Speaker => "audio-volume-muted",
            Channel::Mic => "microphone-disabled",
        };
        return (icon, "muted");
    }
    let pct = (value * 100.0) as i32;
    let icon = match ch {
        Channel::Speaker => match pct {
            v if v >= 66 => "audio-volume-high",
            v if v >= 33 => "audio-volume-medium",
            _ => "audio-volume-low",
        },
        Channel::Mic => {
            if pct >= 33 {
                "microphone-sensitivity-high"
            } else {
                "microphone-sensitivity-low"
            }
        }
    };
    let style = if value >= 1.0 {
        "warn" // > 100 % boost
    } else if value >= 0.33 {
        "normal"
    } else {
        "low"
    };
    (icon, style)
}

/// One event the pipewire mainloop hands off to the I/O worker.
struct VolumeEvent {
    channel: Channel,
    kind: NodeKind,
    state: AudioState,
    /// Stable hex hash. For devices: hash of `node.name`. For apps: hash
    /// of `application.process.binary` so the same app gets the same
    /// source across restarts and bar animations track properly.
    source: String,
    /// Display label — `node.description ?? node.nick ?? node.name` for
    /// devices; `application.name ?? application.process.binary
    /// ?? node.name` for apps.
    app: String,
    /// Icon name to send. `Some` for app streams (using
    /// `application.icon-name` or the app's binary name); `None` for
    /// devices, in which case the listener picks a level-based volume icon.
    icon_override: Option<String>,
}

/// Stable, short hash of a node name. Used as the per-node `source` so
/// that:
///
/// * Restarts of the listener re-use the same source (no false-positive
///   duplicate-listener warnings on respawn).
/// * Different nodes get different sources so their histories tween
///   independently.
fn stable_node_hash(name: &str) -> String {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    h.write(name.as_bytes());
    format!("{:08x}", (h.finish() as u32))
}

/// Per-node identity decided once at bind time and copied into the param
/// closure. Carries everything `emit_osd()` needs to label the OSD without
/// re-querying the node properties on every event.
#[derive(Clone)]
struct NodeIdentity {
    source: String,
    app: String,
    icon: Option<String>,
    /// `node.name` from PipeWire — the symbolic device name
    /// (e.g. `alsa_output.pci-0000_00_1f.3.analog-stereo`). Used by
    /// `--default-only` filtering to compare against the current
    /// `default.audio.sink` / `default.audio.source` metadata values.
    /// `None` for app-stream nodes (apps don't participate in the
    /// default-device mechanic).
    node_name: Option<String>,
}

fn identity_for_device(props: &libspa::utils::dict::DictRef) -> Option<NodeIdentity> {
    let node_name = props.get("node.name")?.to_string();
    let app = props
        .get("node.description")
        .or_else(|| props.get("node.nick"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| node_name.clone());
    Some(NodeIdentity {
        source: stable_node_hash(&node_name),
        app,
        icon: None,
        node_name: Some(node_name),
    })
}

/// Identity for an app stream (`Stream/{Output,Input}/Audio`).
/// Source hash keys on `application.process.binary` (or `.name`, or
/// `node.name`) so the same app keeps a stable source. Icon prefers
/// `application.icon-name`. `binaries` is a case-insensitive substring
/// allowlist on `application.process.binary` — empty allows all.
fn identity_for_app(
    props: &libspa::utils::dict::DictRef,
    binaries: &[String],
) -> Option<NodeIdentity> {
    let binary = props
        .get("application.process.binary")
        .map(|s| s.to_string());
    if !binaries.is_empty() {
        let bin_lc = binary.as_deref().unwrap_or("").to_ascii_lowercase();
        if !binaries
            .iter()
            .any(|b| bin_lc.contains(&b.to_ascii_lowercase()))
        {
            return None;
        }
    }
    let app = props
        .get("application.name")
        .or_else(|| props.get("application.process.binary"))
        .or_else(|| props.get("node.name"))
        .map(|s| s.to_string())?;
    let icon = props
        .get("application.icon-name")
        .or_else(|| props.get("application.process.binary"))
        .map(|s| s.to_string());
    let key_for_hash = binary
        .clone()
        .or_else(|| props.get("application.name").map(|s| s.to_string()))
        .or_else(|| props.get("node.name").map(|s| s.to_string()))
        .unwrap_or_default();
    Some(NodeIdentity {
        source: stable_node_hash(&key_for_hash),
        app,
        icon,
        // None bypasses the --default-only filter (apps aren't devices).
        node_name: None,
    })
}

/// Worker thread that owns the awob-client connection and sends OSDs.
/// Pipewire's mainloop is single-threaded and would block if we did
/// blocking unix-socket I/O inside callbacks — the channel decouples them.
fn spawn_io_worker(
    rx: mpsc::Receiver<VolumeEvent>,
    socket: Option<PathBuf>,
    mute_volume_zero: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("awob-pw-io".into())
        .spawn(move || {
            while let Ok(ev) = rx.recv() {
                if let Err(e) = emit_osd(
                    &socket,
                    &ev.source,
                    ev.channel,
                    ev.kind,
                    ev.state,
                    mute_volume_zero,
                    &ev.app,
                    ev.icon_override.as_deref(),
                ) {
                    tracing::info!("send failed: {e}");
                }
            }
        })
        .expect("spawn pipewire io worker")
}

/// Parse a `Props` SPA pod into `(mean(channelVolumes), mute)`.
///
/// The pod is a `SPA_TYPE_OBJECT_Props` containing properties keyed by
/// `SPA_PROP_*` constants from `spa/param/audio/raw.h`. Verified live
/// against pipewire 1.6 — keys are:
///
/// * `SPA_PROP_volume`         = `0x10003` — single overall volume (f32, cubic)
/// * `SPA_PROP_mute`           = `0x10004` — bool
/// * `SPA_PROP_channelVolumes` = `0x10008` — array of f32 (cubic per channel)
///
/// Pipewire emits multiple Props pods per node (one with volume info,
/// another with hardware/format info). Pods that don't carry any volume
/// property are skipped by returning `None`.
fn parse_props_pod(pod: &libspa::pod::Pod) -> Option<AudioState> {
    use libspa::pod::deserialize::PodDeserializer;

    let bytes = pod.as_bytes();
    let (_, deserialised) = PodDeserializer::deserialize_any_from(bytes).ok()?;
    let object = match deserialised {
        libspa::pod::Value::Object(o) => o,
        _ => return None,
    };

    let mut channel_volumes: Vec<f32> = Vec::new();
    let mut single_volume: Option<f32> = None;
    let mut mute: Option<bool> = None;

    for prop in &object.properties {
        match prop.key {
            // SPA_PROP_volume
            0x10003 => {
                if let libspa::pod::Value::Float(f) = &prop.value {
                    single_volume = Some(*f);
                }
            }
            // SPA_PROP_mute
            0x10004 => {
                if let libspa::pod::Value::Bool(b) = &prop.value {
                    mute = Some(*b);
                }
            }
            // SPA_PROP_channelVolumes
            0x10008 => {
                if let libspa::pod::Value::ValueArray(libspa::pod::ValueArray::Float(vs)) =
                    &prop.value
                {
                    channel_volumes = vs.clone();
                }
            }
            _ => {}
        }
    }

    // If this pod didn't carry any volume properties (e.g. it's the
    // hardware-info chunk), tell the caller to skip it.
    if channel_volumes.is_empty() && single_volume.is_none() && mute.is_none() {
        return None;
    }

    let cubic = if !channel_volumes.is_empty() {
        let sum: f32 = channel_volumes.iter().sum();
        sum / channel_volumes.len() as f32
    } else {
        single_volume.unwrap_or(0.0)
    };
    // PipeWire stores `channelVolumes` and `volume` on a *cubic* curve so
    // an internal value of `linear^3` corresponds to a "linear" display
    // value of `linear`. wpctl, pavucontrol, and most other tools show the
    // linear form. Take the cube root here so the OSD bar matches what
    // those tools display (e.g. wpctl 0.50 → bar at 50%, not at 12.5%).
    let linear = (cubic.max(0.0)).powf(1.0 / 3.0);
    Some(AudioState {
        value: linear as f64,
        muted: mute.unwrap_or(false),
    })
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    tracing::info!(
        "native pipewire-rs subscription \
         (per-node listener_id + source hash; one logical listener per Audio node)"
    );

    let (tx, rx) = mpsc::channel::<VolumeEvent>();
    let _io_join = spawn_io_worker(rx, cli.socket, cli.mute_volume_zero);

    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&mainloop, None)?;
    let core = context.connect_rc(None)?;
    let registry = core.get_registry_rc()?;
    let registry_weak = registry.downgrade();

    // Map keyed by registry id for cleanup on `global_remove`.
    type NodeBinding = (pw::node::Node, pw::node::NodeListener);
    let bound_nodes: Rc<RefCell<HashMap<u32, NodeBinding>>> = Rc::new(RefCell::new(HashMap::new()));
    let last_state: Rc<RefCell<ChangeFilter<u32, AudioState>>> =
        Rc::new(RefCell::new(ChangeFilter::new()));

    // Listener handle keeps the PipeWire subscription alive — must outlive
    // the registration.
    type MetadataBinding = (pw::metadata::Metadata, pw::metadata::MetadataListener);
    let bound_metadata: Rc<RefCell<HashMap<u32, MetadataBinding>>> =
        Rc::new(RefCell::new(HashMap::new()));

    // `None` until the metadata listener fires; events from non-defaults
    // are dropped under `--default-only`.
    #[derive(Default)]
    struct Defaults {
        sink: Option<String>,
        source: Option<String>,
    }
    let defaults: Rc<RefCell<Defaults>> = Rc::new(RefCell::new(Defaults::default()));

    let no_speaker = cli.no_speaker;
    let no_mic = cli.no_mic;
    let default_only = cli.default_only;

    let bound_for_global = Rc::clone(&bound_nodes);
    let bound_metadata_for_global = Rc::clone(&bound_metadata);
    let last_state_for_global = Rc::clone(&last_state);
    let defaults_for_global = Rc::clone(&defaults);
    let defaults_for_param = Rc::clone(&defaults);
    let tx_for_global = tx.clone();
    let per_app = cli.per_app;
    let per_app_binaries: Vec<String> = cli.per_app_binaries.clone();
    let _registry_listener = registry
        .add_listener_local()
        .global(move |obj| {
            // Bind unconditionally — the cost is small.
            if obj.type_ == ObjectType::Metadata {
                let props = match obj.props {
                    Some(p) => p,
                    None => return,
                };
                if props.get("metadata.name").unwrap_or("") != "default" {
                    return;
                }
                let registry = match registry_weak.upgrade() {
                    Some(r) => r,
                    None => return,
                };
                let metadata: pw::metadata::Metadata = match registry.bind(obj) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::info!("bind default metadata: {e}");
                        return;
                    }
                };
                let defaults_for_property = Rc::clone(&defaults_for_global);
                let listener = metadata
                    .add_listener_local()
                    .property(move |_subject, key, _ty, value| {
                        // Value is JSON like `{"name":"alsa_output...."}`
                        // when the default is set; `None` when the
                        // property is removed (e.g. last sink unplugged).
                        let parsed = value.and_then(|v| {
                            serde_json::from_str::<serde_json::Value>(v)
                                .ok()
                                .and_then(|json| {
                                    json.get("name").and_then(|n| n.as_str()).map(String::from)
                                })
                        });
                        let mut d = defaults_for_property.borrow_mut();
                        match key {
                            Some("default.audio.sink") => d.sink = parsed,
                            Some("default.audio.source") => d.source = parsed,
                            _ => {}
                        }
                        0
                    })
                    .register();
                bound_metadata_for_global
                    .borrow_mut()
                    .insert(obj.id, (metadata, listener));
                return;
            }
            if obj.type_ != ObjectType::Node {
                return;
            }
            let props = match obj.props {
                Some(p) => p,
                None => return,
            };
            let media_class = props.get("media.class").unwrap_or("");
            // Classify: physical device sink/source, or per-app stream.
            let (channel, kind) = match media_class {
                "Audio/Sink" => (Channel::Speaker, NodeKind::Device),
                "Audio/Source" => (Channel::Mic, NodeKind::Device),
                "Stream/Output/Audio" if per_app => (Channel::Speaker, NodeKind::App),
                "Stream/Input/Audio" if per_app => (Channel::Mic, NodeKind::App),
                _ => return,
            };
            if matches!(channel, Channel::Speaker) && no_speaker {
                return;
            }
            if matches!(channel, Channel::Mic) && no_mic {
                return;
            }

            let identity = match kind {
                NodeKind::Device => identity_for_device(props),
                NodeKind::App => identity_for_app(props, &per_app_binaries),
            };
            let identity = match identity {
                Some(i) => i,
                None => return,
            };

            let registry = match registry_weak.upgrade() {
                Some(r) => r,
                None => return,
            };
            let node: pw::node::Node = match registry.bind(obj) {
                Ok(n) => n,
                Err(e) => {
                    tracing::info!("bind node {}: {e}", obj.id);
                    return;
                }
            };

            let id = obj.id;
            let tx_local = tx_for_global.clone();
            let last_state_for_param = Rc::clone(&last_state_for_global);
            let defaults_for_this_param = Rc::clone(&defaults_for_param);
            let listener = node
                .add_listener_local()
                .param(move |_seq, _id, _index, _next, pod| {
                    let pod = match pod {
                        Some(p) => p,
                        None => return,
                    };
                    if let Some(state) = parse_props_pod(pod) {
                        // --default-only applies to devices only; app
                        // streams bypass.
                        if default_only && matches!(kind, NodeKind::Device) {
                            let d = defaults_for_this_param.borrow();
                            let want = match channel {
                                Channel::Speaker => d.sink.as_deref(),
                                Channel::Mic => d.source.as_deref(),
                            };
                            if want.is_none() || identity.node_name.as_deref() != want {
                                return;
                            }
                        }
                        // ChangeFilter silently seeds — PipeWire emits Props
                        // synchronously on subscribe, otherwise every node
                        // would fire an OSD at daemon start.
                        if last_state_for_param.borrow_mut().changed(id, &state) {
                            let _ = tx_local.send(VolumeEvent {
                                channel,
                                kind,
                                state,
                                source: identity.source.clone(),
                                app: identity.app.clone(),
                                icon_override: identity.icon.clone(),
                            });
                        }
                    }
                })
                .register();
            node.subscribe_params(&[libspa::param::ParamType::Props]);
            bound_for_global.borrow_mut().insert(id, (node, listener));
        })
        .global_remove({
            let bound = Rc::clone(&bound_nodes);
            let last = Rc::clone(&last_state);
            let bound_meta = Rc::clone(&bound_metadata);
            move |id| {
                bound.borrow_mut().remove(&id);
                last.borrow_mut().forget(&id);
                bound_meta.borrow_mut().remove(&id);
            }
        })
        .register();

    mainloop.run();
    Ok(())
}

fn main() -> ExitCode {
    awob_client::init_tracing("info");
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "awob-listener-pipewire starting"
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

    fn icon(ch: Channel, v: f64, m: bool) -> &'static str {
        pick_visuals(ch, v, m).0
    }

    fn style(ch: Channel, v: f64, m: bool) -> &'static str {
        pick_visuals(ch, v, m).1
    }

    #[test]
    fn speaker_icons() {
        assert_eq!(icon(Channel::Speaker, 0.90, false), "audio-volume-high");
        assert_eq!(icon(Channel::Speaker, 0.50, false), "audio-volume-medium");
        assert_eq!(icon(Channel::Speaker, 0.10, false), "audio-volume-low");
        assert_eq!(icon(Channel::Speaker, 0.50, true), "audio-volume-muted");
    }
    #[test]
    fn mic_icons() {
        assert_eq!(
            icon(Channel::Mic, 0.50, false),
            "microphone-sensitivity-high"
        );
        assert_eq!(
            icon(Channel::Mic, 0.10, false),
            "microphone-sensitivity-low"
        );
        assert_eq!(icon(Channel::Mic, 0.50, true), "microphone-disabled");
    }
    #[test]
    fn styles() {
        assert_eq!(style(Channel::Speaker, 1.5, false), "warn");
        assert_eq!(style(Channel::Speaker, 0.5, false), "normal");
        assert_eq!(style(Channel::Speaker, 0.10, false), "low");
        assert_eq!(style(Channel::Speaker, 0.5, true), "muted");
    }
}
