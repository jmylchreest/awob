//! SCTK + wlr-layer-shell integration.
//!
//! Owns a single layer-shell `LayerSurface` sized + anchored from the active
//! [`Theme`]'s `surface { … }` block, showing the most recently rendered
//! tiny-skia [`Pixmap`] in a `wl_shm` buffer. The surface is unmapped after
//! the theme's `timeout` until the next render.

#[path = "pacing.rs"]
mod pacing;
use pacing::{ELEMENT_INTERVAL, Pacing};

use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use awob_core::bindings::{Bindings, Value};
use awob_core::render::Renderer;
use awob_core::scene::{Anchor as ThemeAnchor, Edge};
use awob_core::theme::Theme;
use awob_core::{Margin, Surface as ThemeSurface};
use calloop::EventLoop;
use calloop::channel::Event as CalloopEvent;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor as LayerAnchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler,
            LayerSurface, LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
};

pub enum SurfaceCommand {
    /// Push a fresh send. The thread interpolates the bar value from
    /// `last_value` to the value carried by `bindings` over
    /// `transition_duration`, re-rendering each frame.
    Render {
        theme: Theme,
        bindings: Bindings,
        last_value: f64,
        transition_duration: Duration,
        /// `None` for the embedded fallback theme; otherwise the directory
        /// the icon resolver searches before falling back to system themes.
        theme_dir: Option<std::path::PathBuf>,
        source: Option<String>,
        event: String,
        preempt: bool,
    },
    /// Replace the active theme on a visible OSD without restarting the
    /// cycle. No-op when idle.
    Retheme {
        theme: Theme,
        theme_dir: Option<std::path::PathBuf>,
    },
    /// Reserved for graceful shutdown; not yet wired.
    #[allow(dead_code)]
    Stop,
}

pub struct SurfaceHandle {
    tx: Sender<SurfaceCommand>,
}

impl SurfaceHandle {
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &self,
        theme: Theme,
        bindings: Bindings,
        last_value: f64,
        transition_duration: Duration,
        theme_dir: Option<std::path::PathBuf>,
        source: Option<String>,
        event: String,
        preempt: bool,
    ) {
        let _ = self.tx.send(SurfaceCommand::Render {
            theme,
            bindings,
            last_value,
            transition_duration,
            theme_dir,
            source,
            event,
            preempt,
        });
    }
    pub fn retheme(&self, theme: Theme, theme_dir: Option<std::path::PathBuf>) {
        let _ = self.tx.send(SurfaceCommand::Retheme { theme, theme_dir });
    }
    #[allow(dead_code)]
    pub fn stop(&self) {
        let _ = self.tx.send(SurfaceCommand::Stop);
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WaylandError {
    #[error("connect: {0}")]
    Connect(#[from] wayland_client::ConnectError),
    #[error("registry init: {0}")]
    Registry(#[from] wayland_client::globals::GlobalError),
    #[error("dispatch: {0}")]
    Dispatch(#[from] wayland_client::DispatchError),
    #[error("calloop: {0}")]
    Calloop(String),
    #[error("shm: {0}")]
    Shm(String),
    #[error("layer-shell global missing")]
    NoLayerShell,
}

impl From<calloop::Error> for WaylandError {
    fn from(e: calloop::Error) -> Self {
        WaylandError::Calloop(e.to_string())
    }
}

/// Spawn a Wayland event-loop thread. Returns a handle for IPC threads to push
/// pixmaps into the surface, and a JoinHandle the caller can keep around.
pub fn spawn() -> Result<
    (
        SurfaceHandle,
        std::thread::JoinHandle<Result<(), WaylandError>>,
    ),
    WaylandError,
> {
    let (tx, rx) = channel::<SurfaceCommand>();
    let join = std::thread::Builder::new()
        .name("awob-wayland".into())
        .spawn(move || run(rx))
        .map_err(|e| WaylandError::Calloop(format!("thread spawn: {e}")))?;
    Ok((SurfaceHandle { tx }, join))
}

fn run(cmd_rx: Receiver<SurfaceCommand>) -> Result<(), WaylandError> {
    let conn = Connection::connect_to_env()?;
    let (globals, event_queue) = registry_queue_init::<State>(&conn)?;
    let qh = event_queue.handle();

    let registry_state = RegistryState::new(&globals);
    let output_state = OutputState::new(&globals, &qh);
    let compositor_state = CompositorState::bind(&globals, &qh)
        .map_err(|e| WaylandError::Calloop(format!("compositor: {e}")))?;
    let shm = Shm::bind(&globals, &qh).map_err(|e| WaylandError::Calloop(format!("shm: {e}")))?;
    let layer_shell = LayerShell::bind(&globals, &qh).map_err(|_| WaylandError::NoLayerShell)?;

    // Forward channel commands into the calloop event loop.
    let (loop_tx, loop_rx) = calloop::channel::channel::<SurfaceCommand>();
    let bridge = std::thread::Builder::new()
        .name("awob-wayland-bridge".into())
        .spawn(move || {
            while let Ok(c) = cmd_rx.recv() {
                if loop_tx.send(c).is_err() {
                    break;
                }
            }
        })
        .map_err(|e| WaylandError::Calloop(format!("bridge spawn: {e}")))?;

    let mut event_loop: EventLoop<'_, State> =
        EventLoop::try_new().map_err(|e| WaylandError::Calloop(e.to_string()))?;

    let pool = SlotPool::new(360 * 64 * 4, &shm).map_err(|e| WaylandError::Shm(e.to_string()))?;

    let mut state = State {
        registry_state,
        output_state,
        compositor_state,
        shm,
        layer_shell,
        pool,
        buffers: Vec::new(),
        pacing: Pacing::default(),
        layer: None,
        configured: false,
        theme: None,
        bindings: None,
        last_value: 0.0,
        target_value: 0.0,
        sent_at: Instant::now(),
        transition_duration: Duration::from_millis(180),
        renderer: Renderer::new(),
        cycle_start: None,
        surface_def: ThemeSurface::default(),
        qh: qh.clone(),
        running: true,
        current_source: None,
        current_event: None,
        pending: None,
    };

    WaylandSource::new(conn.clone(), event_queue)
        .insert(event_loop.handle())
        .map_err(|e| WaylandError::Calloop(format!("wayland source: {e}")))?;

    let _channel_token = event_loop
        .handle()
        .insert_source(loop_rx, |ev, _meta, state| {
            if let CalloopEvent::Msg(cmd) = ev {
                match cmd {
                    SurfaceCommand::Render {
                        theme,
                        bindings,
                        last_value,
                        transition_duration,
                        theme_dir,
                        source,
                        event,
                        preempt,
                    } => {
                        state.handle_send(
                            theme,
                            bindings,
                            last_value,
                            transition_duration,
                            theme_dir,
                            source,
                            event,
                            preempt,
                        );
                    }
                    SurfaceCommand::Retheme { theme, theme_dir } => {
                        state.retheme(theme, theme_dir);
                    }
                    SurfaceCommand::Stop => state.running = false,
                }
            }
        })
        .map_err(|e| WaylandError::Calloop(format!("channel insert: {e}")))?;

    while state.running {
        let timeout = state.next_tick_timeout();
        event_loop
            .dispatch(timeout, &mut state)
            .map_err(|e| WaylandError::Calloop(e.to_string()))?;
        state.tick();
    }

    drop(bridge);
    Ok(())
}

struct State {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    shm: Shm,
    layer_shell: LayerShell,
    pool: SlotPool,
    // Keep at most three buffers, including old sizes still held by the compositor.
    buffers: Vec<Buffer>,
    pacing: Pacing,
    layer: Option<LayerSurface>,
    configured: bool,
    /// Re-rendered every animation frame from `theme` + interpolated
    /// `bindings`. `None` while idle.
    theme: Option<Theme>,
    bindings: Option<Bindings>,
    last_value: f64,
    target_value: f64,
    sent_at: Instant,
    transition_duration: Duration,
    renderer: Renderer,
    cycle_start: Option<Instant>,
    surface_def: ThemeSurface,
    qh: QueueHandle<State>,
    running: bool,
    /// `(source, event)` of the active OSD, used by `handle_send` to pick
    /// continuity vs preempt vs queue.
    current_source: Option<String>,
    current_event: Option<String>,
    /// Single-slot newest-wins queue for non-preempt sends arriving while a
    /// different `(source, event)` is on screen. Drained at `Phase::Done`.
    pending: Option<PendingRender>,
}

struct PendingRender {
    theme: Theme,
    bindings: Bindings,
    last_value: f64,
    transition_duration: Duration,
    theme_dir: Option<std::path::PathBuf>,
    source: Option<String>,
    event: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    FadeIn,
    Show,
    FadeOut,
    Done,
}

impl State {
    #[allow(clippy::too_many_arguments)]
    fn handle_send(
        &mut self,
        theme: Theme,
        bindings: Bindings,
        last_value: f64,
        transition_duration: Duration,
        theme_dir: Option<std::path::PathBuf>,
        source: Option<String>,
        event: String,
        preempt: bool,
    ) {
        let phase = self.current_phase();
        let active = !matches!(phase, Phase::Done);
        // "Same pair" means continuity: the active OSD is already
        // showing this metric and a new value just rolled in. Both source
        // and event must match — and matching `None` source is treated as
        // a fresh send (history-less sends never get continuity).
        let same_pair = active
            && source.is_some()
            && source.as_deref() == self.current_source.as_deref()
            && self.current_event.as_deref() == Some(event.as_str());

        if !active || same_pair || preempt {
            self.queue_render(
                theme,
                bindings,
                last_value,
                transition_duration,
                theme_dir,
                source,
                event,
                same_pair,
            );
        } else {
            // Different `(source, event)` and the sender asked to wait.
            // Single-slot, newest-wins: any earlier pending send is dropped.
            self.pending = Some(PendingRender {
                theme,
                bindings,
                last_value,
                transition_duration,
                theme_dir,
                source,
                event,
            });
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn queue_render(
        &mut self,
        theme: Theme,
        bindings: Bindings,
        last_value: f64,
        transition_duration: Duration,
        theme_dir: Option<std::path::PathBuf>,
        source: Option<String>,
        event: String,
        is_continuity: bool,
    ) {
        let surface = theme.surface.clone();
        if self.layer.is_none() {
            self.create_layer(&surface);
        } else {
            self.update_layer(&surface);
        }
        let target_value = bindings.get("value").as_number().unwrap_or(0.0);
        let now = Instant::now();
        // Snapshot before mutating so the continuity branch sees the old
        // animation parameters.
        let prev_phase = self.current_phase();
        let was_active = matches!(prev_phase, Phase::FadeIn | Phase::Show | Phase::FadeOut);
        let current_interp = self.current_value_interpolated();

        self.surface_def = surface;
        self.theme = Some(theme);
        self.bindings = Some(bindings);
        self.target_value = target_value;
        self.transition_duration = transition_duration;
        self.renderer.set_theme_dir(theme_dir);

        if was_active && is_continuity {
            // Same `(source, event)` mid-animation: start from the current
            // interpolated position (no jump-back) and backdate `sent_at`
            // past `fade_in` so the new transition starts immediately
            // instead of waiting through another hold.
            self.last_value = current_interp;
            self.sent_at = now.checked_sub(self.surface_def.fade_in).unwrap_or(now);
        } else {
            // Fresh send or preempting metric switch — use the
            // (source, event)-keyed `last_value`. The on-screen interp
            // belongs to the previous metric and would corrupt the delta.
            self.last_value = last_value;
            self.sent_at = now;
        }

        // Continuity sends past fade-in jump back to start-of-show so rapid
        // re-sends don't strobe. A metric switch gets a fresh fade-in so the
        // new OSD reads as a distinct event.
        self.cycle_start = match (prev_phase, self.cycle_start, is_continuity) {
            (Phase::Show, Some(_), true) | (Phase::FadeOut, Some(_), true) => {
                Some(now - self.surface_def.fade_in)
            }
            _ => Some(now),
        };

        self.current_source = source;
        self.current_event = Some(event);

        self.pacing.request(now);
    }

    /// Hot-swap theme + palette on a visible OSD without restarting the
    /// cycle. When idle, invalidate caches; the next send picks up the theme.
    /// Palette-keyed colours (`fill="$bg"`) refresh on the next frame;
    /// style-resolved colours (e.g. `$accent` from `apply_style`) only
    /// refresh on the next send.
    fn retheme(&mut self, theme: Theme, theme_dir: Option<std::path::PathBuf>) {
        self.renderer.invalidate_caches();
        self.renderer.set_theme_dir(theme_dir);
        if self.theme.is_none() || self.bindings.is_none() {
            return;
        }
        // Layout from the new theme, timing from the in-flight cycle —
        // a runtime swap mustn't shorten an OSD whose `show` was extended
        // by `--timeout`.
        let new_surface = theme.surface.clone();
        let merged = ThemeSurface {
            width: new_surface.width,
            height: new_surface.height,
            anchor: new_surface.anchor,
            margin: new_surface.margin,
            fade_in: self.surface_def.fade_in,
            show: self.surface_def.show,
            fade_out: self.surface_def.fade_out,
            transition: self.surface_def.transition,
        };
        if self.layer.is_some() {
            self.update_layer(&merged);
        }
        self.surface_def = merged;
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.palette = theme.palette.clone();
        }
        self.theme = Some(theme);
        self.pacing.request(Instant::now());
    }

    /// Compute the bar's current interpolated value using the same formula
    /// as `draw()`. Used by `queue_render` to capture the on-screen
    /// position before a new send mutates the animation parameters.
    fn current_value_interpolated(&self) -> f64 {
        if self.transition_duration.as_millis() == 0 {
            return self.target_value;
        }
        let elapsed = Instant::now().saturating_duration_since(self.sent_at);
        let post_fade = elapsed.saturating_sub(self.surface_def.fade_in);
        let progress =
            (post_fade.as_secs_f64() / self.transition_duration.as_secs_f64()).clamp(0.0, 1.0);
        let eased = 1.0 - (1.0 - progress).powi(3);
        self.last_value + (self.target_value - self.last_value) * eased
    }

    fn current_phase(&self) -> Phase {
        let Some(start) = self.cycle_start else {
            return Phase::Done;
        };
        let elapsed = Instant::now().saturating_duration_since(start);
        let s = &self.surface_def;
        if elapsed < s.fade_in {
            Phase::FadeIn
        } else if elapsed < s.fade_in + s.show {
            Phase::Show
        } else if elapsed < s.fade_in + s.show + s.fade_out {
            Phase::FadeOut
        } else {
            Phase::Done
        }
    }

    /// Alpha multiplier in [0.0, 1.0] for the current point in the cycle.
    fn current_alpha(&self) -> f32 {
        let Some(start) = self.cycle_start else {
            return 0.0;
        };
        let elapsed = Instant::now().saturating_duration_since(start);
        let s = &self.surface_def;
        let fade_in_ms = s.fade_in.as_millis().max(1) as f32;
        let fade_out_ms = s.fade_out.as_millis().max(1) as f32;
        let show_end = s.fade_in + s.show;
        let total = show_end + s.fade_out;
        if elapsed >= total {
            0.0
        } else if elapsed >= show_end {
            let into = (elapsed - show_end).as_millis() as f32;
            (1.0 - into / fade_out_ms).clamp(0.0, 1.0)
        } else if elapsed < s.fade_in {
            (elapsed.as_millis() as f32 / fade_in_ms).clamp(0.0, 1.0)
        } else {
            1.0
        }
    }

    fn create_layer(&mut self, surface: &ThemeSurface) {
        let wl_surface = self.compositor_state.create_surface(&self.qh);
        let layer = self.layer_shell.create_layer_surface(
            &self.qh,
            wl_surface,
            Layer::Overlay,
            Some("awob"),
            None,
        );
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        layer.set_size(surface.width, surface.height);
        let (anchor, margin) = layer_anchor_and_margin(surface);
        layer.set_anchor(anchor);
        layer.set_margin(
            margin.top as i32,
            margin.right as i32,
            margin.bottom as i32,
            margin.left as i32,
        );
        layer.commit();
        self.layer = Some(layer);
        self.configured = false;
        self.pacing = Pacing::default();
    }

    fn update_layer(&mut self, surface: &ThemeSurface) {
        if let Some(layer) = &self.layer {
            layer.set_size(surface.width, surface.height);
            let (anchor, margin) = layer_anchor_and_margin(surface);
            layer.set_anchor(anchor);
            layer.set_margin(
                margin.top as i32,
                margin.right as i32,
                margin.bottom as i32,
                margin.left as i32,
            );
            layer.commit();
        }
    }

    fn draw(&mut self) -> bool {
        if self.theme.is_none() || self.bindings.is_none() || self.layer.is_none() {
            return false;
        }
        let alpha = self.current_alpha();

        // Value transition is sequenced *after* fade-in: the bar holds at
        // `last_value` while fading in, then animates once fully visible.
        let elapsed = Instant::now().saturating_duration_since(self.sent_at);
        let fade_in = self.surface_def.fade_in;
        let transition_progress = if self.transition_duration.as_millis() == 0 {
            1.0
        } else {
            let post_fade = elapsed.saturating_sub(fade_in);
            (post_fade.as_secs_f64() / self.transition_duration.as_secs_f64()).clamp(0.0, 1.0)
        };
        // Ease-out cubic so the bar decelerates into its final value.
        let eased = 1.0 - (1.0 - transition_progress).powi(3);
        let interp_value = self.last_value + (self.target_value - self.last_value) * eased;

        let mut frame_bindings = self.bindings.as_ref().unwrap().clone();
        frame_bindings.set("value", Value::Number(interp_value));
        frame_bindings.set("transitionProgress", Value::Number(transition_progress));

        // `Some` only during Show so element animations (pulse etc.) stay
        // paused while the OSD is fading in or out.
        let phase = self.current_phase();
        let show_elapsed = if matches!(phase, Phase::Show) {
            Some(elapsed.saturating_sub(fade_in))
        } else {
            None
        };

        let width = self.surface_def.width.max(1) as i32;
        let height = self.surface_def.height.max(1) as i32;
        let stride = width * 4;
        // A release makes storage reusable; it is never a request to render.
        self.buffers.retain(|buffer| {
            (buffer.height() == height && buffer.stride() == stride)
                || buffer.canvas(&mut self.pool).is_none()
        });
        let index = self.buffers.iter().position(|buffer| {
            buffer.height() == height
                && buffer.stride() == stride
                && buffer.canvas(&mut self.pool).is_some()
        });
        let index = if let Some(index) = index {
            index
        } else {
            if self.buffers.len() >= 3 {
                return false;
            }
            match self
                .pool
                .create_buffer(width, height, stride, wl_shm::Format::Argb8888)
            {
                Ok((buffer, _)) => self.buffers.push(buffer),
                Err(e) => {
                    tracing::warn!("shm buffer alloc failed: {e}");
                    return false;
                }
            }
            self.buffers.len() - 1
        };
        let pm = match self.renderer.render_cached(
            self.theme.as_ref().unwrap(),
            &frame_bindings,
            show_elapsed,
        ) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("render: {e}");
                return false;
            }
        };
        let buffer = &self.buffers[index];
        let Some(canvas) = buffer.canvas(&mut self.pool) else {
            return false;
        };
        argb_premul_with_alpha(pm.data(), canvas, alpha);
        let layer = self.layer.as_ref().unwrap();
        let wl_surface = layer.wl_surface();
        if let Err(e) = buffer.attach_to(wl_surface) {
            tracing::warn!("attach buffer failed: {e}");
            return false;
        }
        wl_surface.damage_buffer(0, 0, width, height);
        wl_surface.frame(&self.qh, FrameCallbackData(wl_surface.clone()));
        wl_surface.commit();
        true
    }

    /// The cycle deadline remains live even while the compositor withholds frames.
    fn next_tick_timeout(&self) -> Option<Duration> {
        let now = Instant::now();
        let end = self.cycle_start?
            + self.surface_def.fade_in
            + self.surface_def.show
            + self.surface_def.fade_out;
        Some(self.pacing.timeout(now, end, self.configured))
    }

    fn next_frame(&self, now: Instant) -> Instant {
        let start = self.cycle_start.unwrap_or(now);
        let fade_in_end = start + self.surface_def.fade_in;
        let show_end = fade_in_end + self.surface_def.show;
        let end = show_end + self.surface_def.fade_out;
        let transition_end = self.sent_at + self.surface_def.fade_in + self.transition_duration;
        next_frame_deadline(
            now,
            fade_in_end,
            show_end,
            end,
            transition_end,
            self.has_active_element_animations(),
        )
    }

    fn has_active_element_animations(&self) -> bool {
        let Some(theme) = &self.theme else {
            return false;
        };
        theme
            .scene
            .elements
            .iter()
            .any(|el| !el.common().animations.is_empty())
    }

    fn tick(&mut self) {
        match self.current_phase() {
            Phase::Done => {
                if self.cycle_start.is_some() {
                    self.layer = None;
                    self.configured = false;
                    self.theme = None;
                    self.bindings = None;
                    self.cycle_start = None;
                    self.current_source = None;
                    self.current_event = None;
                    self.pacing = Pacing::default();
                }
                // Drain a queued non-preempt send as a fresh OSD.
                if let Some(p) = self.pending.take() {
                    self.queue_render(
                        p.theme,
                        p.bindings,
                        p.last_value,
                        p.transition_duration,
                        p.theme_dir,
                        p.source,
                        p.event,
                        false,
                    );
                }
            }
            Phase::FadeIn | Phase::FadeOut | Phase::Show => {
                let now = Instant::now();
                if self.configured && self.pacing.ready(now) {
                    if self.draw() {
                        self.pacing.submitted(Instant::now(), self.next_frame(now));
                    } else {
                        self.pacing.retry(now);
                    }
                }
            }
        }
    }
}

/// Preserve phase boundaries and the final transition value even when an animation
/// ends between frames. Late wakeups use wall-clock time; they never catch up by
/// submitting a burst of obsolete frames.
fn next_frame_deadline(
    now: Instant,
    fade_in_end: Instant,
    show_end: Instant,
    end: Instant,
    transition_end: Instant,
    elements_animated: bool,
) -> Instant {
    const FRAME: Duration = Duration::from_nanos(1_000_000_000 / 60);
    if now < fade_in_end {
        (now + FRAME).min(fade_in_end)
    } else if now < show_end {
        if now < transition_end {
            (now + FRAME).min(transition_end).min(show_end)
        } else if elements_animated {
            (now + ELEMENT_INTERVAL).min(show_end)
        } else {
            show_end
        }
    } else {
        (now + FRAME).min(end)
    }
}

fn argb_premul_with_alpha(src: &[u8], dst: &mut [u8], alpha: f32) {
    // tiny-skia: premultiplied RGBA in R,G,B,A byte order.
    // wl_shm Argb8888 on little-endian: B,G,R,A byte order.
    // Uniform scaling preserves premultiplication.
    debug_assert_eq!(src.len(), dst.len());
    let a = alpha.clamp(0.0, 1.0);
    if a >= 0.999 {
        for (s, d) in src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<4>().0)
        {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
    } else if a <= 0.001 {
        dst.fill(0);
    } else {
        for (s, d) in src
            .as_chunks::<4>()
            .0
            .iter()
            .zip(dst.as_chunks_mut::<4>().0)
        {
            d[0] = ((s[2] as f32) * a) as u8;
            d[1] = ((s[1] as f32) * a) as u8;
            d[2] = ((s[0] as f32) * a) as u8;
            d[3] = ((s[3] as f32) * a) as u8;
        }
    }
}

fn layer_anchor_and_margin(s: &ThemeSurface) -> (LayerAnchor, Margin) {
    use ThemeAnchor::*;
    let mut a = LayerAnchor::empty();
    let (he, ve) = s.anchor.edges();
    match he {
        Edge::Start => a |= LayerAnchor::LEFT,
        Edge::End => a |= LayerAnchor::RIGHT,
        Edge::Center => {} // no horizontal anchor; compositor centers
    }
    match ve {
        Edge::Start => a |= LayerAnchor::TOP,
        Edge::End => a |= LayerAnchor::BOTTOM,
        Edge::Center => {} // no vertical anchor; compositor centers
    }
    // Diagonal special-cases set both edges
    match s.anchor {
        TopLeft => {
            a = LayerAnchor::TOP | LayerAnchor::LEFT;
        }
        TopRight => {
            a = LayerAnchor::TOP | LayerAnchor::RIGHT;
        }
        BottomLeft => {
            a = LayerAnchor::BOTTOM | LayerAnchor::LEFT;
        }
        BottomRight => {
            a = LayerAnchor::BOTTOM | LayerAnchor::RIGHT;
        }
        Top | Bottom | Left | Right | Center => {}
    }
    (a, s.margin)
}

// ---- handler delegations ----

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if self
            .layer
            .as_ref()
            .is_some_and(|layer| layer.wl_surface() == surface)
        {
            self.pacing.frame_done();
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, layer: &LayerSurface) {
        if self
            .layer
            .as_ref()
            .is_some_and(|current| current.wl_surface() == layer.wl_surface())
        {
            self.layer = None;
            self.configured = false;
        }
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        if self
            .layer
            .as_ref()
            .is_some_and(|current| current.wl_surface() == layer.wl_surface())
        {
            self.configured = true;
            self.pacing.request(Instant::now());
        }
    }
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers!(OutputState);
}

delegate_registry!(State);
smithay_client_toolkit::delegate_dispatch2!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_show_sleeps_until_fade_out() {
        let start = Instant::now();
        let fade = start + Duration::from_millis(100);
        let show = fade + Duration::from_secs(3);
        let end = show + Duration::from_millis(100);
        assert_eq!(
            next_frame_deadline(
                fade + Duration::from_secs(1),
                fade,
                show,
                end,
                fade + Duration::from_millis(180),
                false
            ),
            show
        );
    }

    #[test]
    fn transition_and_phase_endpoints_are_not_skipped() {
        let start = Instant::now();
        let fade = start + Duration::from_millis(100);
        let transition = fade + Duration::from_millis(180);
        let show = fade + Duration::from_secs(3);
        let end = show + Duration::from_millis(100);
        for boundary in [fade, transition, show, end] {
            let now = boundary - Duration::from_millis(1);
            assert_eq!(
                next_frame_deadline(now, fade, show, end, transition, false),
                boundary
            );
        }
    }

    #[test]
    fn zero_durations_finish_without_division_or_extra_frames() {
        let start = Instant::now();
        assert_eq!(
            next_frame_deadline(start, start, start, start, start, false),
            start
        );
        let show = start + Duration::from_secs(3);
        assert_eq!(
            next_frame_deadline(start, start, show, show, start, false),
            show
        );
    }

    #[test]
    fn elements_use_thirty_hz_after_value_transition() {
        let now = Instant::now();
        let end = now + Duration::from_secs(3);
        assert_eq!(
            next_frame_deadline(now, now, end, end, now, true),
            now + ELEMENT_INTERVAL
        );
    }
}
