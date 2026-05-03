mod config;
mod ipc;
mod known_listeners;
mod state;
mod supervisor;
mod theme_loader;
mod watcher;
mod wayland;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use awob_core::apply_style;
use awob_protocol::{HistoryEntry, PROTOCOL_VERSION, Request, Response};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(version, about = "awob — wayland overlay bar daemon")]
struct Cli {
    /// Theme name to load. Looked up first in --themes-dir, then the embedded fallback.
    #[arg(long)]
    theme: Option<String>,

    /// User themes directory. Defaults to $XDG_CONFIG_HOME/awob/themes (~/.config/awob/themes).
    #[arg(long)]
    themes_dir: Option<PathBuf>,

    /// Override the daemon's IPC socket path. Defaults to $XDG_RUNTIME_DIR/awob.sock.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Path to an awob.toml config file. Defaults to $XDG_CONFIG_HOME/awob/awob.toml.
    /// Listeners declared in `[[listeners]]` are spawned + supervised by the
    /// daemon. CLI flags override values in the config file.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Render-only mode: process incoming sends, log a one-line "would render"
    /// summary, but do NOT open a Wayland surface. Useful for headless smoke
    /// tests and debugging the IPC + scene engine without a compositor.
    #[arg(long)]
    no_surface: bool,

    /// Late-import a palette overlay applied AFTER the theme's own
    /// imports + inline `palette { … }`. Lets you change the colours
    /// of any theme without editing it. The file is added to the
    /// hot-reload watch list, so saving the overlay refreshes
    /// instantly. Accepts the same KDL syntax as
    /// `themes/_palettes/<name>.kdl`.
    #[arg(long)]
    force_palette: Option<PathBuf>,
}

struct Shared {
    history: state::History,
    theme: theme_loader::LoadedTheme,
    themes_root: Option<PathBuf>,
    surface: Option<wayland::SurfaceHandle>,
    watcher: Option<watcher::ThemeWatcher>,
    /// Path to `awob.toml` if one is in effect — explicit `--config`,
    /// otherwise the XDG default (`$XDG_CONFIG_HOME/awob/awob.toml`).
    /// Used as the rewrite target when a client passes `persist=true` to
    /// `SetTheme`. `None` if no config path could be determined.
    config_path: Option<PathBuf>,
    /// Late palette overlay applied after every theme load (initial,
    /// `SetTheme`, `Reload`, hot-reload). See `Cli::force_palette`.
    force_palette: Option<PathBuf>,
}

impl Shared {
    fn rewatch(&mut self) {
        if let Some(w) = &mut self.watcher {
            w.set_paths(&self.theme.watch_paths());
        }
    }
}

impl Shared {
    fn handle(&mut self, req: Request) -> Response {
        match req {
            Request::Hello { protocol } => Response::Hello {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
            }
            .with_protocol_check(protocol),
            Request::Send(payload) => {
                let prev = payload
                    .source
                    .as_deref()
                    .and_then(|s| self.history.get(s, &payload.event))
                    .cloned();
                let last_value = prev.as_ref().map(|e| e.last_value);
                let last_max = prev.as_ref().map(|e| e.last_max);
                let last_seen = prev
                    .as_ref()
                    .map(|e| Instant::now().duration_since(e.last_seen));
                if let Some(src) = payload.source.as_deref() {
                    let outcome = self.history.record(
                        src,
                        payload.listener_id.as_deref(),
                        &payload.event,
                        payload.value,
                        payload.max,
                    );
                    if let Some(dup) = outcome.duplicate_listener {
                        eprintln!(
                            "warning: duplicate listener `{}` — multiple instances active: [{}]",
                            dup.listener_id,
                            dup.sources.join(", "),
                        );
                    }
                }
                let mut bindings =
                    awob_core::bindings::build(&payload, last_value, last_max, last_seen);
                bindings.palette = self.theme.theme.palette.clone();
                // Auto-detect wob's overflow state: value > max forces
                // the `overflow` style regardless of what the sender
                // asked for. Themes that don't define an `overflow`
                // style block silently no-op (apply_style is lenient
                // on missing names) so this is backwards-compatible.
                let style_to_apply: &str = if payload.value > payload.max {
                    "overflow"
                } else {
                    payload.style.as_deref().unwrap_or("normal")
                };
                let _ = apply_style(&self.theme.theme, &mut bindings, style_to_apply);
                if let Some(accent_override) = &payload.accent {
                    bindings.set("accent", awob_core::Value::String(accent_override.clone()));
                }
                let summary = format!(
                    "send: event={} value={} max={} src={:?} style={:?} app={:?} icon={:?} \
                     last_value={:?} last_max={:?}",
                    payload.event,
                    payload.value,
                    payload.max,
                    payload.source,
                    payload.style,
                    payload.app,
                    payload.icon,
                    last_value,
                    last_max,
                );
                tracing::debug!("{summary}");
                if let Some(handle) = &self.surface {
                    // The wayland thread takes ownership of theme + bindings
                    // for the lifetime of the cycle so it can re-render every
                    // frame with an interpolated `value`.
                    let mut theme = self.theme.theme.clone();
                    if let Some(ms) = payload.timeout_ms {
                        theme.surface.show = std::time::Duration::from_millis(ms as u64);
                    }
                    let last_value_for_anim = last_value.unwrap_or(payload.value);
                    let transition = theme.surface.transition;
                    let theme_dir = self.theme.source_dir.clone();
                    handle.render(
                        theme,
                        bindings,
                        last_value_for_anim,
                        transition,
                        theme_dir,
                        payload.source.clone(),
                        payload.event.clone(),
                        payload.preempt,
                    );
                }
                Response::Ok
            }
            Request::Query { source } => {
                // History is now (source, event)-keyed, so a single source
                // may produce multiple entries — one per distinct event.
                // Filter at iterate time when a source is named, otherwise
                // emit everything.
                let mut entries = Vec::new();
                for (src, _evt, e) in self.history.entries() {
                    if let Some(filter) = source.as_deref() {
                        if src != filter {
                            continue;
                        }
                    }
                    entries.push(history_entry(src, e));
                }
                Response::Query { entries }
            }
            Request::SetTheme { name, persist } => {
                match theme_loader::load(
                    self.themes_root.as_deref(),
                    &name,
                    self.force_palette.as_deref(),
                ) {
                    Ok(t) => {
                        self.theme = t;
                        self.rewatch();
                        // Push the new theme to the wayland thread so
                        // a currently-visible OSD redraws with it
                        // instead of waiting for the next send.
                        if let Some(handle) = &self.surface {
                            handle.retheme(
                                self.theme.theme.clone(),
                                self.theme.source_dir.clone(),
                            );
                        }
                        if persist {
                            if let Some(path) = &self.config_path {
                                if let Err(e) = persist_theme_to_config(path, &name) {
                                    // Non-fatal: the theme is active in
                                    // memory, persistence just didn't take.
                                    return Response::Error {
                                        message: format!(
                                            "theme set in memory but persisting to {}: {e}",
                                            path.display()
                                        ),
                                    };
                                }
                            } else {
                                return Response::Error {
                                    message: "theme set in memory but no awob.toml \
                                              path is configured to persist to"
                                        .into(),
                                };
                            }
                        }
                        Response::Ok
                    }
                    Err(e) => Response::Error {
                        message: format!("set theme: {e}"),
                    },
                }
            }
            Request::Reload => {
                let name = self.theme.name.clone();
                match theme_loader::load(
                    self.themes_root.as_deref(),
                    &name,
                    self.force_palette.as_deref(),
                ) {
                    Ok(t) => {
                        self.theme = t;
                        self.rewatch();
                        if let Some(handle) = &self.surface {
                            handle.retheme(
                                self.theme.theme.clone(),
                                self.theme.source_dir.clone(),
                            );
                        }
                        Response::Ok
                    }
                    Err(e) => Response::Error {
                        message: format!("reload: {e}"),
                    },
                }
            }
            Request::ThemeList => Response::ThemeList {
                themes: enumerate_themes(self.themes_root.as_deref(), &self.theme.name),
            },
            Request::SetForcePalette { path } => {
                // Update the in-memory force_palette and immediately
                // reload the active theme so the overlay applies (or
                // is removed). Then push the result to the wayland
                // thread for instant redraw of any visible OSD.
                self.force_palette = path.map(std::path::PathBuf::from);
                let name = self.theme.name.clone();
                match theme_loader::load(
                    self.themes_root.as_deref(),
                    &name,
                    self.force_palette.as_deref(),
                ) {
                    Ok(t) => {
                        self.theme = t;
                        self.rewatch();
                        if let Some(handle) = &self.surface {
                            handle.retheme(
                                self.theme.theme.clone(),
                                self.theme.source_dir.clone(),
                            );
                        }
                        Response::Ok
                    }
                    Err(e) => Response::Error {
                        message: format!("set force-palette: {e}"),
                    },
                }
            }
            Request::Version => Response::Version {
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                protocol: PROTOCOL_VERSION,
            },
        }
    }
}

/// Walk `themes_root` and return one [`ThemeInfo`] per subdirectory
/// containing a `scene.kdl`, plus the embedded fallback if it isn't
/// already represented by an on-disk theme of the same name.
///
/// `description` is read best-effort from a sibling `manifest.toml`'s
/// top-level `description = "..."` key. Anything else in the manifest
/// is ignored — see THEMES.md for the full list of conventional fields.
fn enumerate_themes(
    themes_root: Option<&Path>,
    active_name: &str,
) -> Vec<awob_protocol::ThemeInfo> {
    use awob_protocol::ThemeInfo;
    let mut out: Vec<ThemeInfo> = Vec::new();

    if let Some(root) = themes_root {
        if let Ok(read) = std::fs::read_dir(root) {
            for entry in read.flatten() {
                let dir = entry.path();
                if !dir.is_dir() {
                    continue;
                }
                let scene = dir.join("scene.kdl");
                if !scene.exists() {
                    continue;
                }
                let Some(name) = dir.file_name().and_then(|s| s.to_str()) else {
                    continue;
                };
                let description = read_manifest_description(&dir.join("manifest.toml"));
                out.push(ThemeInfo {
                    name: name.to_string(),
                    active: name == active_name,
                    source: "disk".into(),
                    description,
                });
            }
        }
    }
    // Always surface the embedded default. If the on-disk version
    // shadows it (same name), keep the disk entry — the daemon
    // would load that one anyway.
    if !out
        .iter()
        .any(|t| t.name == theme_loader::EMBEDDED_DEFAULT_NAME)
    {
        out.push(ThemeInfo {
            name: theme_loader::EMBEDDED_DEFAULT_NAME.into(),
            active: theme_loader::EMBEDDED_DEFAULT_NAME == active_name,
            source: "embedded".into(),
            description: Some(
                "Built-in default theme. Embedded in awob-daemon as the fallback.".into(),
            ),
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Pull `description` out of a theme's `manifest.toml`. Returns
/// `None` for any failure mode (missing file, unreadable, malformed,
/// no `description` key, empty value) — the manifest is
/// best-effort metadata, never a load-blocking concern.
fn read_manifest_description(path: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let parsed: toml::Value = toml::from_str(&raw).ok()?;
    let s = parsed.get("description")?.as_str()?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Combine explicit `[[listeners]]` with auto-discovered known listeners
/// into a single de-duplicated list. Auto entries are skipped if their
/// `name` collides with an explicit one, or if listed in
/// `supervisor.disable`, or if their binary isn't on disk anywhere we
/// can reach.
fn build_effective_listeners(cfg: &config::AwobConfig) -> Vec<config::ListenerConfig> {
    let mut out: Vec<config::ListenerConfig> = cfg.listeners.clone();
    if !cfg.supervisor.auto {
        return out;
    }
    let explicit_names: std::collections::HashSet<&str> =
        cfg.listeners.iter().map(|l| l.name.as_str()).collect();
    let disabled: std::collections::HashSet<&str> =
        cfg.supervisor.disable.iter().map(|s| s.as_str()).collect();
    for known in known_listeners::KNOWN_LISTENERS {
        if explicit_names.contains(known.name) {
            continue;
        }
        if disabled.contains(known.name) {
            continue;
        }
        let Some(path) = known_listeners::resolve_binary(known.binary) else {
            continue;
        };
        eprintln!(
            "supervisor: auto-discovered `{}` -> {}",
            known.name,
            path.display()
        );
        out.push(config::ListenerConfig {
            name: known.name.into(),
            command: path.to_string_lossy().into_owned(),
            args: Vec::new(),
            env: std::collections::HashMap::new(),
            restart: config::RestartPolicy::Always,
        });
    }
    out
}

/// Rewrite `awob.toml` so the active theme survives daemon restart.
/// Uses `toml_edit` to preserve user comments, key order, and any
/// formatting they care about — only the `theme` value is touched.
/// Creates the file (and parent directory) if neither exists.
fn persist_theme_to_config(path: &Path, theme: &str) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = existing.parse().map_err(|e: toml_edit::TomlError| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
    })?;
    doc["theme"] = toml_edit::value(theme);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, doc.to_string())
}

fn history_entry(source: &str, e: &state::Entry) -> HistoryEntry {
    HistoryEntry {
        source: source.to_string(),
        event: e.event.clone(),
        last_value: e.last_value,
        last_max: e.last_max,
        age_seconds: Instant::now().duration_since(e.last_seen).as_secs_f64(),
        listener_id: e.listener_id.clone(),
    }
}

trait WithProtocolCheck {
    fn with_protocol_check(self, client_protocol: u32) -> Response;
}
impl WithProtocolCheck for Response {
    fn with_protocol_check(self, client_protocol: u32) -> Response {
        if client_protocol != PROTOCOL_VERSION {
            return Response::Error {
                message: format!(
                    "protocol mismatch: client={client_protocol} daemon={PROTOCOL_VERSION}"
                ),
            };
        }
        self
    }
}

fn default_themes_dir() -> Option<PathBuf> {
    awob_core::paths::awob_themes_dir()
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Load config file (explicit path > XDG default > none).
    let file_config: config::AwobConfig = match &cli.config {
        Some(p) => config::AwobConfig::load(p)?,
        None => config::AwobConfig::load_default()?.unwrap_or_default(),
    };

    // CLI flags override file values.
    let theme_name = cli
        .theme
        .clone()
        .or(file_config.theme.clone())
        .unwrap_or_else(|| theme_loader::EMBEDDED_DEFAULT_NAME.into());
    let themes_root = cli
        .themes_dir
        .clone()
        .or_else(|| {
            file_config
                .themes_dir
                .as_deref()
                .map(awob_core::paths::expand_config_path)
        })
        .or_else(default_themes_dir);

    // Force-palette overlay. CLI flag wins; otherwise read from
    // `awob.toml`'s `force_palette` key with `$VAR` / `~/` expansion
    // so users can write `force_palette = "~/.config/awob/overlay.kdl"`
    // and have it Just Work. When set, the loader applies it last so
    // it wins per the existing palette merge rule, and adds it to
    // the watch list so saving the file hot-reloads.
    let force_palette: Option<PathBuf> = cli.force_palette.clone().or_else(|| {
        file_config
            .force_palette
            .as_deref()
            .map(awob_core::paths::expand_config_path)
    });

    // Cold-start fallback: if the configured theme can't be loaded (missing
    // directory, parse error, etc.), warn loudly but come up with the
    // embedded default so the user still sees an OSD and can recover via
    // `awob set-theme <name>`. Refusing to start would leave them with no
    // OSD and no way to drive the daemon to fix it.
    let initial = match theme_loader::load(
        themes_root.as_deref(),
        &theme_name,
        force_palette.as_deref(),
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "warning: theme `{theme_name}` failed to load ({e}); \
                 falling back to embedded default"
            );
            theme_loader::load_embedded()?
        }
    };
    eprintln!(
        "theme: {} ({} elements)",
        initial.name,
        initial.theme.scene.elements.len()
    );

    let socket_path = match cli.socket {
        Some(p) => p,
        None => match file_config.socket.as_deref() {
            Some(s) => awob_core::paths::expand_config_path(s),
            None => ipc::default_socket_path()?,
        },
    };
    let server = ipc::Server::bind(socket_path)?;
    tracing::info!("listening on {}", server.path().display());

    let surface = if cli.no_surface {
        tracing::info!("running headless (--no-surface): no Wayland surface will be opened");
        None
    } else {
        match wayland::spawn() {
            Ok((handle, _join)) => {
                tracing::info!("wayland surface thread started");
                Some(handle)
            }
            Err(e) => {
                tracing::warn!("failed to start wayland surface ({e}); running headless");
                None
            }
        }
    };

    // Set up the file watcher for hot-reload. Failure is non-fatal — the
    // daemon still works, just without auto-reload.
    let (reload_tx, reload_rx) = std::sync::mpsc::channel::<()>();
    let watcher = match watcher::ThemeWatcher::new(reload_tx.clone()) {
        Ok(w) => Some(w),
        Err(e) => {
            tracing::warn!("file watcher disabled: {e}");
            None
        }
    };

    // Resolve the awob.toml path the daemon should rewrite when a client
    // sends `SetTheme { persist: true }`. Explicit `--config` wins;
    // otherwise the XDG default. No fallback to a synthetic path — if we
    // genuinely don't know where to write, persist requests get a clear
    // error rather than dumping a file somewhere unexpected.
    let config_path: Option<PathBuf> = cli
        .config
        .clone()
        .or_else(awob_core::paths::awob_config_file);

    let shared = Arc::new(Mutex::new(Shared {
        history: state::History::new(),
        theme: initial,
        themes_root,
        surface,
        watcher,
        config_path,
        force_palette,
    }));
    {
        let mut s = shared.lock().unwrap();
        s.rewatch();
        eprintln!(
            "watching: {} paths for hot reload",
            s.theme.watch_paths().len()
        );
    }

    // Hot-reload worker.
    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            while reload_rx.recv().is_ok() {
                // Debounce: drain any further events that arrive in the
                // next 80ms. Editors and IDEs commonly emit 3-5 modify
                // events per save.
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(80);
                while let Some(remaining) =
                    deadline.checked_duration_since(std::time::Instant::now())
                {
                    if reload_rx.recv_timeout(remaining).is_err() {
                        break;
                    }
                }
                let mut s = shared.lock().unwrap();
                let name = s.theme.name.clone();
                let root = s.themes_root.clone();
                let force_palette = s.force_palette.clone();
                match theme_loader::load(root.as_deref(), &name, force_palette.as_deref()) {
                    Ok(t) => {
                        s.theme = t;
                        s.rewatch();
                        // Push the freshly-parsed theme to the wayland
                        // thread so a visible OSD redraws with the new
                        // colours / layout instantly. Idle surface
                        // ignores the message.
                        if let Some(handle) = &s.surface {
                            handle.retheme(
                                s.theme.theme.clone(),
                                s.theme.source_dir.clone(),
                            );
                        }
                        eprintln!(
                            "hot-reloaded theme `{name}` ({} watched files)",
                            s.theme.watch_paths().len()
                        );
                    }
                    Err(e) => tracing::info!("hot reload failed: {e}"),
                }
            }
        });
    }

    let listener = server.try_clone_listener()?;

    // Build the effective listener list and hand it to the supervisor.
    // Explicit `[[listeners]]` win over auto-discovered ones with the
    // same name; auto-discovery walks `KNOWN_LISTENERS` and pulls in any
    // whose binary is on disk and isn't named in `supervisor.disable`.
    let effective = build_effective_listeners(&file_config);
    let mut sup = supervisor::Supervisor::new();
    if !effective.is_empty() {
        tracing::info!("supervisor: spawning {} listener(s)", effective.len());
        sup.spawn_all(effective, Some(server.path().to_path_buf()).as_ref());
    }
    let sup = Arc::new(Mutex::new(sup));

    // Background tick loop polling supervised children.
    {
        let sup = Arc::clone(&sup);
        let socket_for_sup = server.path().to_path_buf();
        thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                sup.lock().unwrap().tick(Some(&socket_for_sup));
            }
        });
    }

    // Catch SIGINT/SIGTERM and shut children down cleanly.
    {
        let sup = Arc::clone(&sup);
        thread::spawn(move || {
            use nix::sys::signal::{SigSet, Signal};
            let mut signals = SigSet::empty();
            signals.add(Signal::SIGINT);
            signals.add(Signal::SIGTERM);
            let _ = signals.thread_block();
            if let Ok(sig) = signals.wait() {
                tracing::info!("daemon: caught {sig:?}, shutting down");
                sup.lock().unwrap().shutdown();
                std::process::exit(0);
            }
        });
    }

    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(s) => s,
            Err(e) => {
                tracing::info!("accept: {e}");
                continue;
            }
        };
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let _ = ipc::serve_connection(stream, move |req| shared.lock().unwrap().handle(req));
        });
    }

    drop(server);
    Ok(())
}

fn main() -> ExitCode {
    // Initialise tracing first so the startup banner is its first
    // line. Default level: info. Quiet noisy framework logs
    // (smithay-client-toolkit, wayland-client, calloop) at warn so
    // info-level output stays focused on awob.
    awob_client::init_tracing(
        "info,smithay_client_toolkit=warn,wayland_client=warn,calloop=warn",
    );
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        protocol = awob_protocol::PROTOCOL_VERSION,
        "awob-daemon starting"
    );
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "awob-daemon failed to start");
            ExitCode::from(1)
        }
    }
}
