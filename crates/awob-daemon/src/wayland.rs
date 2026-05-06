//! SCTK + wlr-layer-shell integration.
//!
//! Owns a single layer-shell `LayerSurface` sized + anchored from the active
//! [`Theme`]'s `surface { … }` block, showing the most recently rendered
//! tiny-skia [`Pixmap`] in a `wl_shm` buffer. The surface is unmapped after
//! the theme's `timeout` until the next render.

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
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_layer, delegate_output, delegate_registry, delegate_shm,
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
    shm::{Shm, ShmHandler, slot::SlotPool},
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

        if self.configured {
            self.draw();
        }
    }

    /// Hot-swap theme + palette on a visible OSD without restarting the
    /// cycle. No-op when idle — the next send picks up the new theme.
    /// Palette-keyed colours (`fill="$bg"`) refresh on the next frame;
    /// style-resolved colours (e.g. `$accent` from `apply_style`) only
    /// refresh on the next send.
    fn retheme(&mut self, theme: Theme, theme_dir: Option<std::path::PathBuf>) {
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
        self.renderer.set_theme_dir(theme_dir);
        if let Some(bindings) = self.bindings.as_mut() {
            bindings.palette = theme.palette.clone();
        }
        self.theme = Some(theme);
        if self.configured {
            self.draw();
        }
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

    fn draw(&mut self) {
        if self.theme.is_none() || self.bindings.is_none() || self.layer.is_none() {
            return;
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

        let pm =
            match self
                .renderer
                .render(self.theme.as_ref().unwrap(), &frame_bindings, show_elapsed)
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("render: {e}");
                    return;
                }
            };
        let width = pm.width() as i32;
        let height = pm.height() as i32;
        let stride = width * 4;
        let buffer_result =
            self.pool
                .create_buffer(width, height, stride, wl_shm::Format::Argb8888);
        let (buffer, canvas) = match buffer_result {
            Ok((b, c)) => (b, c),
            Err(e) => {
                tracing::warn!("shm buffer alloc failed: {e}");
                return;
            }
        };
        argb_premul_with_alpha(pm.data(), canvas, alpha);
        let layer = self.layer.as_ref().unwrap();
        let wl_surface = layer.wl_surface();
        if let Err(e) = buffer.attach_to(wl_surface) {
            tracing::warn!("attach buffer failed: {e}");
            return;
        }
        wl_surface.damage_buffer(0, 0, width, height);
        wl_surface.commit();
    }

    /// Returns how long until the next animation tick should fire. `None` = idle.
    fn next_tick_timeout(&self) -> Option<Duration> {
        match self.current_phase() {
            Phase::FadeIn | Phase::FadeOut => Some(Duration::from_millis(16)),
            Phase::Show => {
                // 60Hz during the value transition window (fade_in + transition);
                // 30Hz while element animations are still playing; otherwise sleep
                // straight through to fade-out.
                let elapsed = Instant::now().saturating_duration_since(self.sent_at);
                let animation_window = self.surface_def.fade_in + self.transition_duration;
                if elapsed < animation_window {
                    Some(Duration::from_millis(16))
                } else if self.has_active_element_animations() {
                    // 30fps is plenty for transient OSDs and saves battery
                    // on integrated GPUs.
                    Some(Duration::from_millis(33))
                } else {
                    let start = self.cycle_start?;
                    let elapsed_cycle = Instant::now().saturating_duration_since(start);
                    let until_fade_out = (self.surface_def.fade_in + self.surface_def.show)
                        .saturating_sub(elapsed_cycle);
                    Some(until_fade_out)
                }
            }
            Phase::Done => None,
        }
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
                if self.layer.is_some() {
                    self.layer = None;
                    self.configured = false;
                    self.theme = None;
                    self.bindings = None;
                    self.cycle_start = None;
                    self.current_source = None;
                    self.current_event = None;
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
                if self.configured {
                    self.draw();
                }
            }
        }
    }
}

fn argb_premul_with_alpha(src: &[u8], dst: &mut [u8], alpha: f32) {
    // tiny-skia: premultiplied RGBA in R,G,B,A byte order.
    // wl_shm Argb8888 on little-endian: B,G,R,A byte order.
    // Uniform scaling preserves premultiplication.
    debug_assert_eq!(src.len(), dst.len());
    let a = alpha.clamp(0.0, 1.0);
    if a >= 0.999 {
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = s[3];
        }
    } else if a <= 0.001 {
        dst.fill(0);
    } else {
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(4)) {
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
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
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
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.layer = None;
        self.configured = false;
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &LayerSurface,
        _configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.configured = true;
        if self.theme.is_some() && self.bindings.is_some() {
            self.draw();
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

delegate_compositor!(State);
delegate_output!(State);
delegate_registry!(State);
delegate_shm!(State);
delegate_layer!(State);
