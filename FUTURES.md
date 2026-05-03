# Deferred work

Things consciously not done yet but tracked for later iterations. Each entry
describes the ask, what would need to change, and an estimate of effort so
you can prioritise.


## Time-driven element animations (pulse / glow / sparkle)

Today's renderer is "draw once on send, fade-in / fade-out the whole
surface, tween the bar value". Cute extras like a *pulsing critical
glow* (alpha sin-wave on the bar/border when value is in the critical
band), a *breathing icon*, or *sparkle particles* on overflow aren't
expressible because:

* No per-element clock — the renderer has `transitionProgress` (0→1
  during the value tween) and the global fade alpha, but nothing that
  ticks across the `show` window.
* No animation primitives in the scene KDL — only static styles plus
  `transition="..."`.
* No frame loop during the `show` phase — the wayland surface only
  redraws on send, retheme, fade-in, fade-out, and value-tween steps.

What it would take, in increasing scope:

1. **`pulse` flag on the critical style (~80 LOC)**: bool attribute on
   any element that, when true, modulates alpha by `sin(t * 2πf)`
   during the `show` window. Wayland surface schedules a `wl_surface
   .frame()` callback while any pulsing element is visible. Cheap
   quick-win for "critical battery flashes". Specifying `pulse-rate
   ="1Hz"` and `pulse-depth="40%"` gives some control without inventing
   a DSL.
2. **`@animate` micro-DSL (~250 LOC)**: scene-level
   `animate name=alpha from=0.6 to=1.0 duration="800ms" loop="ping-pong"`
   block scoped to an element. Cover alpha + scale + colour-lerp; that
   covers pulse, breathing, glow expansion. Renderer evaluates the
   curve at `t = elapsed_show_ms` per-frame.
3. **Particle effects (~600 LOC + dep)**: sparkles on overflow, motion
   trails on rapid value change. Would want a tiny particle system
   (positions, velocities, life, colour-fade) layered after the bar.
   Real cost is design (which effects feel right, perf budget for
   integrated GPUs) more than code.

Plus a **demo script** (`demo/animations.sh`): cycles through pulse →
glow → sparkle → spinner with explanatory `--app` labels and `--show
3000` so each plays long enough to appreciate.

Defer until at least (1) is wanted; (2) and (3) are speculative until
someone uses (1) and asks for more.


## Structured logging via `tracing`

Today every binary uses `eprintln!` for diagnostics: daemon
startup, send summaries, listener device-discovery, supervisor
spawn/exit notices, errors. Listener stdout/stderr is inherited
into the daemon process via `Stdio::inherit()`, so all output
flows to the daemon's stderr — wherever that's been redirected
(`/tmp/awob.log` for manual launches, `journalctl --user -u awob`
when run via the systemd unit).

Functional, but no log levels and no structured fields. Switching
to `tracing` + `tracing-subscriber` would give:

* `RUST_LOG=info`/`debug`/`warn` filtering at runtime
* Per-listener `target=<name>` so journalctl filters cleanly:
  `journalctl --user -u awob _COMM=awob-daemon SYSLOG_IDENTIFIER=battery`
* Optional JSON output for ingestion by log shippers
* Spans for "this whole Send took N ms" diagnostics

Cost: ~30 LOC across awob-daemon + each listener (one
`tracing-subscriber::fmt::init()` line in `main`, replace
`eprintln!` with `tracing::info!`/`warn!`). Deferred because the
current free-form output is debuggable via `journalctl` and
nothing has actively bitten yet.


## Friendly keyboard names from udev

External USB keyboards often expose vendor + product strings up their
udev parent chain (`udevadm info --attribute-walk
/sys/class/leds/<dev>` → `ID_VENDOR_FROM_DATABASE`,
`ID_MODEL_FROM_DATABASE`). Today the keyboard-backlight listener
defaults to `"Keyboard"` (or `"Keyboard N"` for multi-device); reading
those udev fields would yield labels like `"GMMK Numpad"` for
recognisable hardware.

No-op on internal laptop keyboards (which don't carry useful strings).

Cost: ~30 LOC + the `udev` crate, or shelling out to `udevadm info` and
parsing the output (no new dep). Niche enough to leave deferred.

## xdg-output-v1 fallback for backlight friendly names

Today the backlight listener reads `wl_output.description` (event added
in `wl_output` v4). Every mainstream compositor (Hyprland, Sway, KDE,
mutter) emits both `wl_output` v4 *and* the older
`xdg-output-unstable-v1` with the same description, so this is
duplicate-coverage rather than missing-feature.

If awob ever gets used on a niche compositor that only populates
`xdg-output` (not v4 wl_output), we'd want to also bind that protocol.
Pure compatibility insurance, not visible improvement on any current
desktop.

## PipeWire listener: optional default-only mode

The current behaviour is by design — every node fires its own OSD with
the device's friendly name in the `app` field, so users can see *which*
device's volume just changed. If a user later wants the original "single
OSD only for the active default sink/source" behaviour, the listener
could grow a `--default-only` flag that:

1. binds the `default` Metadata object,
2. watches the `default.audio.sink` / `default.audio.source` keys,
3. only forwards events from the matching node.

~60 LOC. Not a regression — purely additive opt-in.


## Native pipewire-rs backend for lower-latency event delivery

Already done — listener is fully event-driven via pipewire-rs subscription.
This entry retired.

## Migrate backlight / keyboard-backlight listeners to udev

Currently the `awob-listener-backlight` and
`awob-listener-keyboard-backlight` binaries watch their respective
`brightness` sysfs files via the `notify` crate (inotify under the
hood). This works because `brightness` is genuinely *written* by
userspace tools (brightnessctl, xbrightness, ACPI hotkeys), so
inotify fires.

`awob-listener-battery` was migrated to udev because `capacity` and
`status` are computed at read time and never fire inotify events.

Possible follow-up: migrate the backlight + keyboard-backlight
listeners to udev too, for architectural uniformity (every sysfs-
aware listener using the same primitive). No responsiveness gain
expected — both fire instantly on the relevant changes today.
Trade-off: drops the `notify` dep across these two crates (~150 KB
saved in compiled binary size); adds nothing the user sees.

Cost: ~30 min per listener. Defer until consistency review or new
contributor onboarding wants the simpler mental model.


## Bluetooth peripheral batteries (`awob-listener-bluetooth`)

The current `awob-listener-battery` reads `/sys/class/power_supply/*` of
`type=Battery`, which on Linux means built-in laptop batteries. Bluetooth
peripherals (keyboards, mice, headphones, controllers) end up in three
different places depending on kernel and connection profile:

* **Linux 5.13+ HID-over-BT/USB**: `/sys/class/power_supply/hid-XX:XX:
  XX:XX:XX:XX-battery/` with `type=Battery`. Already covered by the
  current listener — if the kernel exposes it, we read it.
* **BlueZ 5.48+**: every connected device with the BAS GATT
  characteristic (0x2A19) gets `org.bluez.Battery1` on its D-Bus object,
  with `Percentage` (0–100) plus `PropertiesChanged` signals on
  state change. **Not** mirrored into sysfs for non-HID profiles
  (notably A2DP-only headphones).
* **UPower**: queries BlueZ over D-Bus and surfaces each peripheral as
  its own `org.freedesktop.UPower.Device` (kind: `keyboard`/`mouse`/
  `headphones`). Same data as BlueZ, one extra hop.

What's missing today: A2DP headphones and older kernels won't surface in
sysfs and so won't fire OSDs.

A new `awob-listener-bluetooth` crate would:

1. Connect to the system bus (zbus) and watch
   `org.bluez` ObjectManager for `org.bluez.Battery1` interface
   add / remove (so hot plug/unplug Just Works).
2. For each device, subscribe to `PropertiesChanged` on `Percentage`.
3. Emit one OSD per device with `source=bluetooth-<mac>` and
   `app=<device-friendly-name>` so each peripheral gets its own OSD
   with its own label.
4. Fire only on capacity *change* (not every property update) and only
   when the device is `Connected=true`, mirroring the spirit of the
   sysfs listener's state filter.

Cost: ~150 LOC + zbus dep (which we deliberately removed from
`awob-listener-battery`). One listener per host, not per device — the
crate fans out internally. Defer until a user with BT headphones / a
peripheral-heavy desk asks.


## Clippy-clean workspace

Done. CI + release pipelines now gate on `cargo clippy --workspace
--all-targets --locked -- -D warnings`; pre-commit runs `cargo fmt
--check`. This entry retired.


## FFI: expose theme management + query

The `awob-client-ffi` crate currently exports a *send-only* surface:
`awob_connect` / `awob_connect_to`, the `awob_send_*` builder, the
`awob_send_dispatch` call, plus `awob_hello`, version readers, and
error reporting. Six SDK methods aren't FFI-exposed yet:

* `query(source)` → list history entries
* `set_theme(name)` / `set_theme_with(name, persist)`
* `theme_list()`
* `set_force_palette(path)` / `clear`
* `reload()`
* `version()`

To match the CLI's reach, an FFI client needs all six. Each is a
~15 LOC C wrapper plus a cbindgen declaration. Defer until a real
FFI consumer asks (current consumers are all Rust).


## FFI consumers beyond C

The `awob-client-ffi` crate exposes a C ABI via `cbindgen`. Bindings for
Python (cffi/ctypes), Swift, and Kotlin can be generated via UniFFI later
if anyone wants them. C ABI is the universal substrate so this is
opportunity rather than necessity.


## D-Bus integration: opt-in compatibility modes

Researched: there is **no freedesktop.org standard** for OSD events on
D-Bus. The closest approximations:

* `org.freedesktop.Notifications` (FDO standard for desktop notifications)
  carries OSD-relevant hints — `value` (de-facto progress hint, int 0–100),
  `urgency`, `transient`, `x-canonical-private-synchronous` — but you can
  only consume them by **owning the bus name**, which conflicts with
  dunst/mako/fnott/swaync. There is no passive listener path.
* `org.kde.osdService` (KDE Plasma–internal RPC) — `showText`,
  `showProgress`, `volumeChanged`, `brightnessChanged`,
  `kbdLayoutChanged`, … — is a *callee* interface used only by Plasma
  itself; nothing third-party publishes to it. Not useful as an upstream
  source.
* `org.gnome.Shell.ShowOSD` — also a callee, owned by gnome-shell. Same
  story as KDE: nothing on the bus emits OSD signals you can listen to.

Two opt-in modes are worth considering if there's user demand:

1. **`org.freedesktop.Notifications` server compat mode** — awob owns the
   bus name (mutually exclusive with dunst/mako). It treats any incoming
   `Notify` whose hints contain `value` (int) **or**
   `x-canonical-private-synchronous` **or** `transient=true` with a numeric
   value as an OSD bar event; everything else either gets dropped or
   routed to a configured fallback notifier. Appeals to minimalists who
   don't want a second bubble daemon. Cost: ~150 LOC zbus + a config flag.
2. **`org.kde.osdService` server mode** — also opt-in (doesn't run on
   Plasma since the name's already taken). Lets KDE-aware apps and
   `qdbus` scripts drive awob without going through the native socket
   IPC. Cost: ~80 LOC zbus.

Both are deferred until a user asks — the native socket + listener model
covers all current sources without bus-name conflicts.

References (full research saved separately):
- https://specifications.freedesktop.org/notification/1.2/hints.html
- https://github.com/KDE/plasma-workspace/blob/master/shell/osd.cpp
- https://github.com/GNOME/gnome-shell/blob/main/data/dbus-interfaces/org.gnome.Shell.xml


## Cross-source / cross-event swap policy + preempt hint

Done. `SendPayload.preempt: bool` (default `false`) controls whether a
send may interrupt an OSD that's currently displaying a different
`(source, event)` pair. History is keyed by `(source, event)` so events
on the same source no longer cross-contaminate `$lastValue`.


## Supervisor "auto" mode for known listeners

Done. `[supervisor] auto = true` (default) auto-discovers known listener
binaries on `PATH` (and the daemon's own dir) and spawns each whose
binary is found, unless named in `[supervisor] disable = […]`. Listeners
that need arguments to start (e.g. `awob-listener-wob`'s `--fifo`) are
intentionally outside the auto registry — they require explicit
`[[listeners]]` blocks.
