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

Done. Daemon + every listener uses `tracing` with a shared
`init_tracing` helper in `awob-client`. Default log directives
(`info,smithay_client_toolkit=warn,...`) keep the journal focused on
awob-meaningful events; users can override at runtime via
`RUST_LOG`. Compact formatter, ANSI colours when stderr is a TTY,
plain text otherwise. This entry retired.


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

## Extract `AdditiveMonitor` into `awob-client::listener`

`ChangeFilter` and `wait_for_resource` now live in
`awob-client::listener` and are shared across every in-tree listener.
The third reusable shape — the **additive monitor** (inotify + udev +
poll → one mpsc) — is still copy-pasted between
`awob-listener-backlight` and `awob-listener-keyboard-backlight`
(~80 LOC each).

Two consumers don't justify the abstraction yet. The natural third
consumer is the proposed `awob-listener-kbd-state` listener
(CapsLock / NumLock / ScrollLock LEDs under `/sys/class/leds`,
identical inotify+udev+poll shape). When that lands — or any other
listener that needs the same primitive — extract:

```rust
// awob-client::listener::AdditiveMonitor
pub struct AdditiveMonitor { rx: mpsc::Receiver<()>, poll: Duration }
impl AdditiveMonitor {
    pub fn for_sysfs(path: &Path, subsystem: &str, sysname: &str,
                     poll: Duration) -> Result<Self> { … }
    pub fn next_wake(&self) -> WakeReason { … }
}
```

Net: ~−160 LOC across the two backlight listeners + ~+100 in the
helper. Defer until a third consumer materialises.


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


## D-Bus integration: opt-in compatibility modes

Researched: there is **no freedesktop.org standard** for OSD events on
D-Bus. The closest approximations:

* `org.freedesktop.Notifications` (FDO standard for desktop notifications)
  carries OSD-relevant hints — `value` (de-facto progress hint, int 0–100),
  `urgency`, `transient`, `x-canonical-private-synchronous` — but you can
  only consume them by **owning the bus name**, which conflicts with
  histui/dunst/mako/fnott/swaync. There is no passive listener path.
* `org.kde.osdService` (KDE Plasma–internal RPC) — `showText`,
  `showProgress`, `volumeChanged`, `brightnessChanged`,
  `kbdLayoutChanged`, … — is a *callee* interface used only by Plasma
  itself; nothing third-party publishes to it. Not useful as an upstream
  source.
* `org.gnome.Shell.ShowOSD` — also a callee, owned by gnome-shell. Same
  story as KDE: nothing on the bus emits OSD signals you can listen to.

Two opt-in modes are worth considering if there's user demand:

1. **`org.freedesktop.Notifications` server compat mode** — awob owns the
   bus name (mutually exclusive with histui/dunst/mako). It treats any incoming
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


## Catalogue of additional listener crates (not yet built)

awob's listener-per-event-source model is meant to grow. The current
set covers the events most laptop users hit constantly (volume,
brightness, battery, keyboard backlight, wob FIFO bridge). The
following are honestly-considered next entries — each would be its
own `awob-listener-<topic>` crate, supervised by the daemon, sending
typed events over the same socket protocol.

Grouped by upstream / mechanism rather than by user-facing event,
because the mechanism dictates the shape of the listener.

### `awob-listener-ups` — UPS state via UPower D-Bus

USB HID UPSes (Eaton, APC, CyberPower etc) **don't appear under
`/sys/class/power_supply/`** because the Linux kernel has no general
HID-Power-Device → power_supply driver. They're visible to UPower via
direct libusb HID parsing, exposed on D-Bus as
`org.freedesktop.UPower.Device { Type = UPS }`.

A separate UPS listener that subscribes to UPower's `DeviceAdded` /
`DeviceRemoved` + `PropertiesChanged` (Percentage, State,
TimeToEmpty) avoids both the laptop-battery sysfs path (already fast)
and the original "drop UPower for AC-plug latency" decision (UPSes
are slow-changing — 30 s poll lag is acceptable for "running on
battery, 48 min remaining"). Reuses the band logic from the main
battery listener: `empty / caution / low / good / full` thresholds
fire OSDs, state transitions always do.

Cost: ~150 LOC + zbus dep, scoped only to this crate. Doesn't disturb
the laptop-battery sysfs+udev path.

### `awob-listener-kbd-state` — Caps Lock / Num Lock / Scroll Lock

Toggle indicators that don't currently fire any OSD on awob. Two
viable upstreams:

* **`/sys/class/leds/input*::capslock` / `::numlock` / `::scrolllock`**
  — same sysfs-LED model the keyboard-backlight listener already uses.
  inotify covers most paths; udev fills the gap for firmware-driven
  state changes.
* **libinput / evdev** — wake on the keypress itself, then read the
  current modifier mask. Lower latency; needs `input` group access.

Cost: ~120 LOC. Probably the most-asked-for of the catalogue —
SwayOSD and KDE both surface this prominently.

### `awob-listener-media` — playback state + track changes via MPRIS

Subscribe to all `org.mpris.MediaPlayer2.*` bus names; emit OSDs on
play/pause toggles, track changes (artist + title), and seek
operations. MPRIS is the freedesktop standard every Linux media
player implements (Spotify, Mpris, mpv, VLC, browsers via
extensions).

Cost: ~200 LOC + zbus. Distinct OSD style needed — track-change OSDs
want `app=<player>`, `value=<progress>`, `event=track-change` with
album-art icon resolution; not a fit for the bar-with-percentage
default theme without theme tweaks.

### `awob-listener-network` — Wi-Fi / Bluetooth / airplane / VPN

NetworkManager exposes everything via D-Bus
(`org.freedesktop.NetworkManager`): radio toggles, connection state,
VPN connection. rfkill provides the kernel-level airplane-mode signal
(`/dev/rfkill` + udev `rfkill` subsystem) for pre-NetworkManager
firmware-handled toggles.

Cost: ~250 LOC + zbus. Broad surface area — could ship in stages
(radio toggles first, VPN second).

### `awob-listener-power-profile` — performance / balanced / power-save

`power-profiles-daemon` (the GNOME / KDE-blessed replacement for
`tlp` interactive bits) exposes the active profile via D-Bus
(`net.hadess.PowerProfiles` / `org.freedesktop.UPower.PowerProfiles`).
A user pressing a Fn-key bound to switch profile gets an OSD with the
new profile name + an icon. Three states + a transition icon, very
small surface area.

Cost: ~80 LOC + zbus. Trivial once we've got zbus pulled in for one
of the above.

### `awob-listener-display` — output connect / disconnect / rotation

When an external monitor is plugged in or rotated (for laptop tablet
modes), surface an OSD with the connector name and resolution.
`wlr-output-management-unstable-v1` gives this on wlroots
compositors; on KDE / mutter the equivalent is the desktop's own
event stream. The keyboard-backlight listener already pulls in Wayland
client code via `wayland_outputs.rs` for `wl_output.description` —
some of that logic could be shared.

Cost: ~150 LOC. Niche but visually satisfying.

### `awob-listener-touchpad` — disable / re-enable on Fn-key

Many laptops have a Fn key that toggles the touchpad. libinput
exposes per-device send-events state via udev. Watch for
`add` / `remove` of `input` devices matching the touchpad pattern,
plus property changes on the existing one.

Cost: ~100 LOC. Low priority — touchpad toggles are rare enough that
most users wouldn't notice the OSD.

## Considered and rejected (or scoped out)

These came up in the canvass but don't fit awob's model well:

* **Screenshot / screen-recording confirmation OSDs** — better handled
  by the screenshot tool itself (grim, slurp, hyprshot) producing its
  own toast or sound. awob would need to be invoked explicitly from
  every screenshot script — no obvious universal hook.
* **Do-Not-Disturb toggle** — desktop-specific (mako has its own
  mode flag, swaync has another, KDE has another). No standard signal
  to subscribe to. The notification daemon is the right place to
  surface this.
* **Clipboard content preview** — the OSD model (transient bar +
  icon) is wrong for showing arbitrary clipboard contents (could be a
  long string, a file path, an image). Better fit for a notification
  daemon's transient mode.
* **FPS / GPU-temp / RAM heads-up overlay** — fundamentally different:
  a *persistent* screen-corner overlay, not a transient bar. awob's
  layer-shell surface is built to fade in, draw briefly, fade out;
  remodelling it for a HUD would be a different program. MangoHud /
  RivaTuner-on-Linux are the right tools.
* **Sticky Keys / accessibility-key indicators** — desktop-specific,
  no standard signal. Live in each compositor's own a11y stack.
