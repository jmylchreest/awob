//! Listener-process supervisor.
//!
//! Spawns each `[[listeners]]` entry from `awob.toml` as a child process,
//! monitors them, restarts on exit per the configured `restart` policy with
//! capped exponential backoff. On daemon shutdown, sends SIGTERM to all
//! children, waits a grace period, then SIGKILL anything that didn't exit.
//!
//! Linux-specific: uses `prctl(PR_SET_PDEATHSIG, SIGTERM)` so children
//! reliably die if the daemon process is killed without going through the
//! normal shutdown path.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::{ListenerConfig, RestartPolicy};

/// Maximum failure count before we cap backoff at the last entry.
const BACKOFF_MS: [u64; 5] = [200, 500, 1000, 2000, 5000];
const SHUTDOWN_GRACE: Duration = Duration::from_millis(2000);

pub struct Supervisor {
    children: HashMap<String, ChildState>,
}

struct ChildState {
    config: ListenerConfig,
    process: Option<Child>,
    /// Index into [`BACKOFF_MS`] tracking how many failures in a row.
    backoff_idx: usize,
    /// When the next respawn attempt is allowed.
    next_attempt_at: Instant,
    /// Whether to give up restarting (after a `Never` policy hit a clean exit).
    stopped: bool,
}

impl Supervisor {
    pub fn new() -> Self {
        Self {
            children: HashMap::new(),
        }
    }

    /// Spawn every configured listener immediately. Failures are logged
    /// but don't stop the daemon — the supervisor loop will retry.
    pub fn spawn_all(&mut self, configs: Vec<ListenerConfig>, socket_path: Option<&PathBuf>) {
        for cfg in configs {
            let name = cfg.name.clone();
            let mut state = ChildState {
                config: cfg,
                process: None,
                backoff_idx: 0,
                next_attempt_at: Instant::now(),
                stopped: false,
            };
            spawn_child(&mut state, socket_path);
            self.children.insert(name, state);
        }
    }

    /// Poll all children: collect any that have exited, decide whether to
    /// respawn per their policy, and respawn now if backoff has elapsed.
    pub fn tick(&mut self, socket_path: Option<&PathBuf>) {
        let now = Instant::now();
        for (name, state) in self.children.iter_mut() {
            if state.stopped {
                continue;
            }

            // Reap exited children.
            let exited = if let Some(p) = state.process.as_mut() {
                match p.try_wait() {
                    Ok(Some(status)) => Some(status),
                    Ok(None) => None,
                    Err(e) => {
                        tracing::info!("supervisor[{name}]: try_wait: {e}");
                        None
                    }
                }
            } else {
                None
            };

            if let Some(status) = exited {
                state.process = None;
                let success = status.success();
                let policy = state.config.restart;
                let should_restart = match policy {
                    RestartPolicy::Always => true,
                    RestartPolicy::OnFailure => !success,
                    RestartPolicy::Never => false,
                };
                eprintln!(
                    "supervisor[{name}]: exited {} restart={policy:?} -> {}",
                    if success {
                        "cleanly".into()
                    } else {
                        format!("with {status:?}")
                    },
                    if should_restart {
                        "will respawn"
                    } else {
                        "stopping"
                    },
                );
                if !should_restart {
                    state.stopped = true;
                    continue;
                }
                let backoff = BACKOFF_MS[state.backoff_idx.min(BACKOFF_MS.len() - 1)];
                state.next_attempt_at = now + Duration::from_millis(backoff);
                if state.backoff_idx + 1 < BACKOFF_MS.len() {
                    state.backoff_idx += 1;
                }
            }

            // Spawn (or re-spawn) when its backoff window has passed.
            if state.process.is_none() && !state.stopped && now >= state.next_attempt_at {
                spawn_child(state, socket_path);
            }
        }
    }

    /// Send SIGTERM to all children, wait up to SHUTDOWN_GRACE, then SIGKILL
    /// anything still alive.
    pub fn shutdown(&mut self) {
        for (name, state) in self.children.iter_mut() {
            if let Some(p) = state.process.as_mut() {
                let pid = p.id() as i32;
                tracing::info!("supervisor[{name}]: SIGTERM pid={pid}");
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGTERM,
                );
            }
        }
        let deadline = Instant::now() + SHUTDOWN_GRACE;
        let poll = Duration::from_millis(50);
        while Instant::now() < deadline {
            let any_alive = self.children.values_mut().any(|s| {
                if let Some(p) = s.process.as_mut() {
                    matches!(p.try_wait(), Ok(None))
                } else {
                    false
                }
            });
            if !any_alive {
                return;
            }
            std::thread::sleep(poll);
        }
        for (name, state) in self.children.iter_mut() {
            if let Some(mut p) = state.process.take() {
                if matches!(p.try_wait(), Ok(None)) {
                    tracing::info!("supervisor[{name}]: SIGKILL pid={}", p.id());
                    let _ = p.kill();
                    let _ = p.wait();
                }
            }
        }
    }
}

fn spawn_child(state: &mut ChildState, socket_path: Option<&PathBuf>) {
    let cfg = &state.config;
    let mut cmd = Command::new(&cfg.command);

    // Expand `$VAR` and `~/` in args so config files can reference
    // standard paths without being absolute.
    let expanded: Vec<String> = cfg
        .args
        .iter()
        .map(|a| {
            awob_core::paths::expand_config_path(a)
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    cmd.args(&expanded);

    // Pass our socket path as AWOB_SOCKET so listeners pick it up if they
    // honour the env var (most don't yet, but it's a stable convention).
    if let Some(p) = socket_path {
        cmd.env("AWOB_SOCKET", p);
    }
    for (k, v) in &cfg.env {
        cmd.env(k, awob_core::paths::expand_env(v));
    }
    cmd.stdin(Stdio::null());
    // Inherit stdout/stderr so listener log lines appear inline with the
    // daemon's. If you want per-listener prefixed routing, switch to
    // `Stdio::piped()` and read on a worker thread.
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    // Linux: ensure children die if the daemon does. SIGTERM gives them a
    // chance to clean up, matching our normal shutdown path.
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                use nix::sys::prctl;
                let _ = prctl::set_pdeathsig(Some(nix::sys::signal::Signal::SIGTERM));
                Ok(())
            });
        }
    }

    match cmd.spawn() {
        Ok(child) => {
            tracing::info!("supervisor[{}]: spawned pid={}", cfg.name, child.id());
            state.process = Some(child);
            state.backoff_idx = 0;
        }
        Err(e) => {
            tracing::info!("supervisor[{}]: spawn failed: {e}", cfg.name);
            // Schedule a retry with current backoff index instead of
            // resetting — repeat spawn failures shouldn't tight-loop.
            let backoff = BACKOFF_MS[state.backoff_idx.min(BACKOFF_MS.len() - 1)];
            state.next_attempt_at = Instant::now() + Duration::from_millis(backoff);
            if state.backoff_idx + 1 < BACKOFF_MS.len() {
                state.backoff_idx += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supervisor_handles_no_children_gracefully() {
        let mut s = Supervisor::new();
        s.tick(None);
        s.shutdown();
    }

    #[test]
    fn spawn_failure_advances_backoff() {
        let cfg = ListenerConfig {
            name: "missing".into(),
            command: "/no/such/binary".into(),
            args: vec![],
            env: HashMap::new(),
            restart: RestartPolicy::Always,
        };
        let mut state = ChildState {
            config: cfg,
            process: None,
            backoff_idx: 0,
            next_attempt_at: Instant::now(),
            stopped: false,
        };
        spawn_child(&mut state, None);
        assert!(state.process.is_none());
        assert!(state.backoff_idx > 0);
    }
}
