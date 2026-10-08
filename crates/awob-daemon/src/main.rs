mod config;
mod ipc;
mod known_listeners;
mod requests;
mod state;
mod supervisor;
mod theme_loader;
mod watcher;
mod wayland;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Instant;

use awob_core::apply_style;
#[cfg(test)]
use awob_protocol::Request;
use awob_protocol::{HistoryEntry, PROTOCOL_VERSION, Response};
use clap::Parser;
use requests::{Requests, ThemeChange, ThemeChangeError};

#[derive(Parser, Debug)]
#[command(version, about = "awob — wayland overlay bar daemon")]
struct Cli {
    /// Theme name to load. Looked up across the themes search path, then the embedded fallback.
    #[arg(long)]
    theme: Option<String>,

    /// Extra themes directory, searched before the defaults
    /// ($XDG_CONFIG_HOME/awob/themes, $XDG_DATA_HOME/awob/themes,
    /// then <dir>/awob/themes for each $XDG_DATA_DIRS entry).
    #[arg(long)]
    themes_dir: Option<PathBuf>,

    /// Override the daemon's IPC socket path. Defaults to $XDG_RUNTIME_DIR/awob.sock.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// Path to an awob.toml config file. Defaults to $XDG_CONFIG_HOME/awob/awob.toml.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Render-only mode: log a one-line "would render" summary instead of
    /// opening a Wayland surface.
    #[arg(long)]
    no_surface: bool,

    /// Late-import a palette overlay applied AFTER the theme's own imports
    /// and inline `palette { … }`. Hot-reloaded.
    #[arg(long)]
    force_palette: Option<PathBuf>,
}

struct Shared {
    history: state::History,
    theme: theme_loader::LoadedTheme,
    themes_roots: Vec<PathBuf>,
    surface: Option<wayland::SurfaceHandle>,
    watcher: Option<watcher::ThemeWatcher>,
    /// Active `awob.toml` path; rewrite target for `SetTheme { persist: true }`.
    config_path: Option<PathBuf>,
    /// Late palette overlay reapplied on every theme (re)load.
    force_palette: Option<PathBuf>,
}

impl Shared {
    fn publish_theme(
        &mut self,
        theme: theme_loader::LoadedTheme,
    ) -> Result<(), wayland::SurfaceSendError> {
        if let Some(surface) = &self.surface {
            surface.retheme(Arc::clone(&theme.theme), theme.source_dir.clone())?;
        }
        self.theme = theme;
        self.rewatch();
        Ok(())
    }

    fn rewatch(&mut self) {
        if let Some(w) = &mut self.watcher {
            w.set_paths(&self.theme.watch_paths());
        }
    }
}

impl Shared {
    fn send(&mut self, payload: awob_protocol::SendPayload) -> Response {
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
        let mut bindings = awob_core::bindings::build(&payload, last_value, last_max, last_seen);
        bindings.palette = self.theme.theme.palette.clone();
        // value > max forces `overflow` regardless of payload.style.
        // Themes without an `overflow` block silently no-op.
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
            let theme = Arc::clone(&self.theme.theme);
            let show_override = payload
                .timeout_ms
                .map(|ms| std::time::Duration::from_millis(u64::from(ms)));
            let last_value_for_anim = last_value.unwrap_or(payload.value);
            let transition = theme.surface.transition;
            let theme_dir = self.theme.source_dir.clone();
            if let Err(error) = handle.render(
                theme,
                bindings,
                last_value_for_anim,
                transition,
                theme_dir,
                payload.source.clone(),
                payload.event.clone(),
                payload.preempt,
                show_override,
            ) {
                return Response::Error {
                    message: error.to_string(),
                };
            }
        }
        if let Some(src) = payload.source.as_deref() {
            let outcome = self.history.record(
                src,
                payload.listener_id.as_deref(),
                &payload.event,
                payload.value,
                payload.max,
            );
            if let Some(dup) = outcome.duplicate_listener {
                tracing::warn!(
                    "duplicate listener `{}` — multiple instances active: [{}]",
                    dup.listener_id,
                    dup.sources.join(", "),
                );
            }
        }
        Response::Ok
    }

    fn query(&self, source: Option<String>) -> Response {
        // One source may have multiple events; filter at iterate time.
        let mut entries = Vec::new();
        for (src, _evt, e) in self.history.entries() {
            if let Some(filter) = source.as_deref()
                && src != filter
            {
                continue;
            }
            entries.push(history_entry(src, e));
        }
        Response::Query { entries }
    }
}

/// Walk every root in `themes_roots` and return one [`ThemeInfo`] per
/// subdirectory containing a `scene.kdl`, plus the embedded fallback if
/// it isn't already represented by an on-disk theme of the same name.
/// Earlier roots shadow later ones by theme name — matching what
/// `theme_loader::load` would actually pick.
///
/// `description` is read best-effort from a sibling `manifest.toml`'s
/// top-level `description = "..."` key. Anything else in the manifest
/// is ignored — see THEMES.md for the full list of conventional fields.
fn enumerate_themes(themes_roots: &[PathBuf], active_name: &str) -> Vec<awob_protocol::ThemeInfo> {
    use awob_protocol::ThemeInfo;
    let mut out: Vec<ThemeInfo> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();

    for root in themes_roots {
        let Ok(read) = std::fs::read_dir(root) else {
            continue;
        };
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
            if !seen.insert(name.to_string()) {
                continue;
            }
            let description = read_manifest_description(&dir.join("manifest.toml"));
            out.push(ThemeInfo {
                name: name.to_string(),
                active: name == active_name,
                source: "disk".into(),
                description,
            });
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

/// Pull `description` from a theme's `manifest.toml`. `None` on any failure.
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
        tracing::info!(
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
///
/// The read-modify-write is serialised via an exclusive `flock` on a
/// sibling lockfile (`awob.toml.lock`), so two daemon instances or a
/// daemon racing the user's editor can't lose each other's changes.
/// The write itself goes via temp file + rename so a crash mid-write
/// can't truncate the existing config.
fn persist_theme_to_config(path: &Path, theme: &str) -> std::io::Result<()> {
    use rustix::fs::{FlockOperation, flock};

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Lockfile lives next to the target. We hold an exclusive lock for
    // the whole read-modify-write so concurrent persists serialize.
    let lock_path = path.with_extension(
        path.extension()
            .map(|e| {
                let mut s = e.to_os_string();
                s.push(".lock");
                s
            })
            .unwrap_or_else(|| std::ffi::OsString::from("lock")),
    );
    let lock_file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)?;
    flock(&lock_file, FlockOperation::LockExclusive)
        .map_err(|e| std::io::Error::other(format!("flock {}: {e}", lock_path.display())))?;

    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = existing.parse().map_err(|e: toml_edit::TomlError| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
    })?;
    doc["theme"] = toml_edit::value(theme);
    let serialized = doc.to_string();

    // Temp file in the same directory so rename() is atomic on the same
    // filesystem. PID + nanos in the suffix avoids collisions with
    // concurrent writers that somehow slipped past the flock (older
    // tooling that doesn't take it).
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("config path has no filename"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let tmp_name = format!(
        "{}.tmp.{}.{}",
        file_name.to_string_lossy(),
        std::process::id(),
        nanos
    );
    let tmp_path = parent.join(tmp_name);

    std::fs::write(&tmp_path, serialized)?;
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(e);
    }
    Ok(())
}

fn history_entry(source: &str, e: &state::Entry) -> HistoryEntry {
    HistoryEntry {
        source: source.to_string(),
        event: e.event.to_string(),
        last_value: e.last_value,
        last_max: e.last_max,
        age_seconds: Instant::now().duration_since(e.last_seen).as_secs_f64(),
        listener_id: e.listener_id.as_ref().map(ToString::to_string),
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

/// Build the ordered themes search path: explicit `--themes-dir`, then
/// config `themes_dir`, then the XDG defaults. Explicit dirs are
/// *prepended* rather than replacing the defaults — packaged themes
/// under /usr/share/awob/themes stay reachable, and an earlier root
/// shadows a later one by theme name.
fn themes_search_path(cli_dir: Option<PathBuf>, config_dir: Option<PathBuf>) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    roots.extend(cli_dir);
    roots.extend(config_dir);
    roots.extend(awob_core::paths::theme_search_roots());
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    roots.retain(|p| seen.insert(p.clone()));
    roots
}

/// Acquire a mutex, recovering from a poisoned lock instead of
/// panicking. A panic in any IPC handler would otherwise poison the
/// shared-state mutex and brick every subsequent request — for a
/// long-lived daemon, "log it and keep serving" is the right default.
/// The recovered guard exposes whatever state the panicker left
/// behind; callers must tolerate that.
fn lock_or_recover<'a, T>(m: &'a Mutex<T>, label: &str) -> MutexGuard<'a, T> {
    match m.lock() {
        Ok(g) => g,
        Err(poisoned) => {
            tracing::warn!(
                "{label}: mutex poisoned by a previous panic — continuing with recovered state"
            );
            poisoned.into_inner()
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Config precedence: explicit --config > XDG default > none.
    let file_config: config::AwobConfig = match &cli.config {
        Some(p) => config::AwobConfig::load(p)?,
        None => config::AwobConfig::load_default()?.unwrap_or_default(),
    };

    // CLI flags override file values throughout.
    let theme_name = cli
        .theme
        .clone()
        .or(file_config.theme.clone())
        .unwrap_or_else(|| theme_loader::EMBEDDED_DEFAULT_NAME.into());
    let themes_roots = themes_search_path(
        cli.themes_dir.clone(),
        file_config
            .themes_dir
            .as_deref()
            .map(awob_core::paths::expand_config_path),
    );

    // CLI flag wins, then `force_palette` from awob.toml with `$VAR` / `~/`
    // expansion. Loader merges it last and adds it to the hot-reload list.
    let force_palette: Option<PathBuf> = cli.force_palette.clone().or_else(|| {
        file_config
            .force_palette
            .as_deref()
            .map(awob_core::paths::expand_config_path)
    });

    // Cold-start fallback to embedded default — refusing to start would
    // strand the user with no OSD and no way to drive the daemon to recover.
    let initial = match theme_loader::load(&themes_roots, &theme_name, force_palette.as_deref()) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(
                "theme `{theme_name}` failed to load ({e}); \
                 falling back to embedded default"
            );
            theme_loader::load_embedded()?
        }
    };
    tracing::info!(
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

    let shared = Arc::new(Requests::new(Shared {
        history: state::History::new(),
        theme: initial,
        themes_roots,
        surface,
        watcher,
        config_path,
        force_palette,
    }));
    {
        let mut s = lock_or_recover(&shared.shared, "shared(init)");
        s.rewatch();
        tracing::info!(
            "watching: {} paths for hot reload",
            s.theme.watch_paths().len()
        );
    }

    {
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            while reload_rx.recv().is_ok() {
                // Debounce 80ms — editors emit 3-5 modify events per save.
                let deadline = std::time::Instant::now() + std::time::Duration::from_millis(80);
                while let Some(remaining) =
                    deadline.checked_duration_since(std::time::Instant::now())
                {
                    if reload_rx.recv_timeout(remaining).is_err() {
                        break;
                    }
                }
                match shared.change_theme(ThemeChange::Reload) {
                    Ok(watched) => tracing::info!("hot-reloaded theme ({watched} watched files)"),
                    Err(ThemeChangeError::Surface(wayland::SurfaceSendError::Busy)) => {
                        // Retry after debounce, keeping the previous theme until admission succeeds.
                        let _ = reload_tx.send(());
                    }
                    Err(error) => tracing::info!("hot reload failed: {error}"),
                }
            }
        });
    }

    let listener = server.try_clone_listener()?;

    let effective = build_effective_listeners(&file_config);
    let mut sup = supervisor::Supervisor::new();
    if !effective.is_empty() {
        tracing::info!("supervisor: spawning {} listener(s)", effective.len());
        sup.spawn_all(effective, Some(server.path().to_path_buf()).as_ref());
    }
    let sup = Arc::new(Mutex::new(sup));

    {
        let sup = Arc::clone(&sup);
        let socket_for_sup = server.path().to_path_buf();
        thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(250));
                lock_or_recover(&sup, "supervisor(tick)").tick(Some(&socket_for_sup));
            }
        });
    }

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
                lock_or_recover(&sup, "supervisor(shutdown)").shutdown();
                std::process::exit(0);
            }
        });
    }

    let connections = ipc::ConnectionLimiter::new();
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(s) => s,
            Err(e) => {
                tracing::info!("accept: {e}");
                continue;
            }
        };
        let Some(permit) = connections.try_acquire() else {
            ipc::reject_connection(stream);
            continue;
        };
        let shared = Arc::clone(&shared);
        if let Err(error) = thread::Builder::new()
            .name("awob-ipc".into())
            .spawn(move || {
                let _permit = permit;
                if let Err(error) = ipc::serve_connection(stream, move |req| shared.handle(req)) {
                    tracing::debug!("IPC connection closed: {error}");
                }
            })
        {
            // The failed spawn drops its closure, socket, and admission permit.
            tracing::warn!("IPC thread spawn failed: {error}");
        }
    }

    drop(server);
    Ok(())
}

fn main() -> ExitCode {
    // Initialise tracing first so the startup banner is its first
    // line. Default level: info. Quiet noisy framework logs
    // (smithay-client-toolkit, wayland-client, calloop) at warn so
    // info-level output stays focused on awob.
    awob_client::init_tracing("info,smithay_client_toolkit=warn,wayland_client=warn,calloop=warn");
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

#[cfg(test)]
mod persist_tests {
    use super::*;
    use std::sync::Barrier;

    #[test]
    fn persist_creates_file_and_writes_theme() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("awob.toml");
        persist_theme_to_config(&path, "ocean").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("theme = \"ocean\""), "got: {body}");
    }

    #[test]
    fn persist_preserves_unrelated_keys_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("awob.toml");
        std::fs::write(
            &path,
            "# user comment\n\
             theme = \"old\"\n\
             socket = \"/tmp/x.sock\"\n",
        )
        .unwrap();
        persist_theme_to_config(&path, "new").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains("# user comment"), "comment lost: {body}");
        assert!(
            body.contains("socket = \"/tmp/x.sock\""),
            "key lost: {body}"
        );
        assert!(
            body.contains("theme = \"new\""),
            "theme not updated: {body}"
        );
    }

    #[test]
    fn concurrent_persists_serialize_via_flock() {
        // Two threads racing to persist different themes. Without the
        // flock + atomic rename, we can hit a torn write or a lost
        // update. With it, the file ends up containing exactly one of
        // the two values intact.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("awob.toml");
        std::fs::write(&path, "theme = \"start\"\n").unwrap();

        let path1 = path.clone();
        let path2 = path.clone();
        let barrier = Arc::new(Barrier::new(2));
        let b1 = Arc::clone(&barrier);
        let b2 = Arc::clone(&barrier);

        let h1 = thread::spawn(move || {
            b1.wait();
            for _ in 0..50 {
                persist_theme_to_config(&path1, "alpha").unwrap();
            }
        });
        let h2 = thread::spawn(move || {
            b2.wait();
            for _ in 0..50 {
                persist_theme_to_config(&path2, "beta").unwrap();
            }
        });
        h1.join().unwrap();
        h2.join().unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        // Must parse cleanly (no torn writes) and contain one of the
        // two values.
        let doc: toml_edit::DocumentMut = body.parse().expect("config corrupt after race");
        let theme = doc["theme"].as_str().unwrap();
        assert!(
            theme == "alpha" || theme == "beta",
            "unexpected theme {theme}"
        );
    }
}

#[cfg(test)]
mod surface_admission_tests {
    use super::*;
    use awob_protocol::SendPayload;

    fn requests(surface: Option<wayland::SurfaceHandle>) -> Requests {
        Requests::new(Shared {
            history: state::History::new(),
            theme: theme_loader::load_embedded().unwrap(),
            themes_roots: Vec::new(),
            surface,
            watcher: None,
            config_path: None,
            force_palette: None,
        })
    }

    fn fill(requests: &Requests) {
        let state = requests.shared.lock().unwrap();
        for _ in 0..64 {
            state
                .surface
                .as_ref()
                .unwrap()
                .retheme(state.theme.theme.clone(), None)
                .unwrap();
        }
    }

    #[test]
    fn rejected_send_does_not_change_history() {
        let (surface, receiver) = wayland::SurfaceHandle::channel();
        let requests = requests(Some(surface));
        fill(&requests);
        let mut payload = SendPayload::new("volume", 75.0);
        payload.source = Some("probe".into());
        assert!(matches!(
            requests.handle(Request::Send(payload.clone())),
            Response::Error { .. }
        ));
        assert!(
            requests
                .shared
                .lock()
                .unwrap()
                .history
                .get("probe", "volume")
                .is_none()
        );
        receiver.try_recv().unwrap();
        assert!(matches!(
            requests.handle(Request::Send(payload)),
            Response::Ok
        ));
        assert_eq!(
            requests
                .shared
                .lock()
                .unwrap()
                .history
                .get("probe", "volume")
                .unwrap()
                .last_value,
            75.0
        );
    }

    #[test]
    fn rejected_reload_does_not_publish_a_new_theme() {
        let (surface, _receiver) = wayland::SurfaceHandle::channel();
        let requests = requests(Some(surface));
        fill(&requests);
        let original = requests.shared.lock().unwrap().theme.theme.clone();
        assert!(matches!(
            requests.handle(Request::Reload),
            Response::Error { .. }
        ));
        assert!(Arc::ptr_eq(
            &original,
            &requests.shared.lock().unwrap().theme.theme
        ));
        assert!(matches!(
            requests.handle(Request::SetTheme {
                name: "default".into(),
                persist: false
            }),
            Response::Error { .. }
        ));
        assert!(Arc::ptr_eq(
            &original,
            &requests.shared.lock().unwrap().theme.theme
        ));
    }

    #[test]
    fn disconnected_surface_rejects_and_headless_accepts() {
        let (surface, receiver) = wayland::SurfaceHandle::channel();
        drop(receiver);
        let requests = requests(Some(surface));
        let mut payload = SendPayload::new("volume", 50.0);
        payload.source = Some("probe".into());
        assert!(matches!(
            requests.handle(Request::Send(payload.clone())),
            Response::Error { .. }
        ));
        assert!(
            requests
                .shared
                .lock()
                .unwrap()
                .history
                .get("probe", "volume")
                .is_none()
        );
        requests.shared.lock().unwrap().surface = None;
        assert!(matches!(
            requests.handle(Request::Send(payload)),
            Response::Ok
        ));
        assert_eq!(
            requests
                .shared
                .lock()
                .unwrap()
                .history
                .get("probe", "volume")
                .unwrap()
                .last_value,
            50.0
        );
    }

    #[test]
    fn rejected_force_palette_keeps_previous_configuration() {
        let (surface, receiver) = wayland::SurfaceHandle::channel();
        let requests = requests(Some(surface));
        let dir = tempfile::tempdir().unwrap();
        let palette = dir.path().join("palette.kdl");
        std::fs::write(&palette, "palette { accent \"#abcdef\"; }").unwrap();
        fill(&requests);
        let original = requests.shared.lock().unwrap().theme.theme.clone();
        let request = || Request::SetForcePalette {
            path: Some(palette.display().to_string()),
        };
        assert!(matches!(requests.handle(request()), Response::Error { .. }));
        assert!(requests.shared.lock().unwrap().force_palette.is_none());
        assert!(Arc::ptr_eq(
            &original,
            &requests.shared.lock().unwrap().theme.theme
        ));
        receiver.try_recv().unwrap();
        assert!(matches!(requests.handle(request()), Response::Ok));
        assert_eq!(requests.shared.lock().unwrap().force_palette, Some(palette));
        assert!(!Arc::ptr_eq(
            &original,
            &requests.shared.lock().unwrap().theme.theme
        ));
    }
}

#[cfg(test)]
mod theme_concurrency_tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    fn requests(surface: Option<wayland::SurfaceHandle>) -> Requests {
        Requests::new(Shared {
            history: state::History::new(),
            theme: theme_loader::load_embedded().unwrap(),
            themes_roots: Vec::new(),
            surface,
            watcher: None,
            config_path: Some(PathBuf::from("unused-test-config")),
            force_palette: None,
        })
    }

    #[test]
    fn sends_use_previous_theme_while_replacement_is_prepared() {
        let (surface, receiver) = wayland::SurfaceHandle::channel();
        let requests = Arc::new(requests(Some(surface)));
        let original = requests.shared.lock().unwrap().theme.theme.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let worker = {
            let requests = Arc::clone(&requests);
            thread::spawn(move || {
                requests.change_theme_with(
                    ThemeChange::Set {
                        name: "next".into(),
                        persist: false,
                    },
                    move |_, name, _| {
                        started_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        let mut loaded = theme_loader::load_embedded()?;
                        loaded.name = name.into();
                        Ok(loaded)
                    },
                    |_, _| panic!("unexpected persistence"),
                )
            })
        };
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            requests.shared.try_lock().is_ok(),
            "theme parsing held shared state"
        );
        assert!(matches!(
            requests.handle(Request::Send(awob_protocol::SendPayload::new(
                "volume", 50.0
            ))),
            Response::Ok
        ));
        resume_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
        match receiver.try_recv().unwrap() {
            wayland::SurfaceCommand::Render { theme, .. } => {
                assert!(Arc::ptr_eq(&theme, &original))
            }
            _ => panic!("send should precede theme publication"),
        }
        assert!(matches!(
            receiver.try_recv().unwrap(),
            wayland::SurfaceCommand::Retheme { .. }
        ));
        assert_eq!(requests.shared.lock().unwrap().theme.name, "next");
    }

    #[test]
    fn persistence_keeps_theme_order_without_blocking_sends() {
        let requests = Arc::new(requests(None));
        let (started_tx, started_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let worker = {
            let requests = Arc::clone(&requests);
            thread::spawn(move || {
                requests.change_theme_with(
                    ThemeChange::Set {
                        name: "default".into(),
                        persist: true,
                    },
                    theme_loader::load,
                    move |_, _| {
                        started_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        Ok(())
                    },
                )
            })
        };
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            requests.shared.try_lock().is_ok(),
            "persistence held shared state"
        );
        assert!(
            requests.theme_operations.try_lock().is_err(),
            "theme ordering lock was released before persistence"
        );
        assert!(matches!(
            requests.handle(Request::Send(awob_protocol::SendPayload::new(
                "volume", 50.0
            ))),
            Response::Ok
        ));
        assert!(matches!(
            requests.handle(Request::Query { source: None }),
            Response::Query { .. }
        ));
        resume_tx.send(()).unwrap();
        worker.join().unwrap().unwrap();
    }
    #[test]
    fn failed_preparation_preserves_theme_and_palette() {
        let requests = requests(None);
        let original = requests.shared.lock().unwrap().theme.theme.clone();
        let result = requests.change_theme_with(
            ThemeChange::Palette(Some(PathBuf::from("missing-palette"))),
            |_, _, _| Err(theme_loader::LoadError::NotFound("test failure".into())),
            |_, _| panic!("failed preparation must not persist"),
        );
        assert!(result.is_err());
        let shared = requests.shared.lock().unwrap();
        assert!(Arc::ptr_eq(&original, &shared.theme.theme));
        assert!(shared.force_palette.is_none());
    }

    #[test]
    fn persistence_error_reports_that_the_theme_is_already_live() {
        let requests = requests(None);
        let original = requests.shared.lock().unwrap().theme.theme.clone();
        let error = requests
            .change_theme_with(
                ThemeChange::Set {
                    name: "default".into(),
                    persist: true,
                },
                theme_loader::load,
                |_, _| Err(std::io::Error::other("test write failure")),
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("theme set in memory but persisting")
        );
        assert!(!Arc::ptr_eq(
            &original,
            &requests.shared.lock().unwrap().theme.theme
        ));
    }

    #[test]
    fn reload_snapshots_after_prior_theme_persistence() {
        let requests = Arc::new(requests(None));
        let (persist_tx, persist_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let setter = {
            let requests = Arc::clone(&requests);
            thread::spawn(move || {
                requests.change_theme_with(
                    ThemeChange::Set {
                        name: "updated".into(),
                        persist: true,
                    },
                    |_, name, _| {
                        let mut theme = theme_loader::load_embedded()?;
                        theme.name = name.into();
                        Ok(theme)
                    },
                    move |_, _| {
                        persist_tx.send(()).unwrap();
                        resume_rx.recv().unwrap();
                        Ok(())
                    },
                )
            })
        };
        persist_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let reloader = {
            let requests = Arc::clone(&requests);
            thread::spawn(move || {
                requests.change_theme_with(
                    ThemeChange::Reload,
                    |_, name, _| {
                        assert_eq!(name, "updated");
                        let mut theme = theme_loader::load_embedded()?;
                        theme.name = name.into();
                        Ok(theme)
                    },
                    |_, _| panic!("reload should not persist"),
                )
            })
        };
        assert!(requests.theme_operations.try_lock().is_err());
        resume_tx.send(()).unwrap();
        setter.join().unwrap().unwrap();
        reloader.join().unwrap().unwrap();
        assert_eq!(requests.shared.lock().unwrap().theme.name, "updated");
    }
}
