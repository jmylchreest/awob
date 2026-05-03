# Deferred work

Things consciously not done yet but tracked for later iterations. Each entry
describes the ask, what would need to change, and an estimate of effort so
you can prioritise.


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


## Clippy-clean workspace

The CI clippy gate is informational pre-1.0 — `cargo clippy --workspace
--all-targets` runs but doesn't fail the build. The workspace currently
trips on a small set of legitimate-but-noisy lints:

* `clippy::too_many_arguments` on the wayland render entry-point
  (8 args carrying theme + bindings + value + transition + dir +
  source + event + preempt). Could be folded into a struct; pre-1.0
  the explicit signature is more readable.
* `clippy::missing_safety_doc` on the FFI crate's raw-pointer
  functions (which *are* `unsafe` — clippy wants doc comments).
* Minor stylistic ones — `clamp`-able patterns, indexing loops,
  field-after-default-init — in the listener binaries.

Path to clippy-clean: address each lint with the smallest viable fix
(usually `#[allow(...)]` with a `// reason: …` line, occasionally
a small refactor), then flip CI back to `-D warnings` and add a
clippy-clean pre-release-hook in `release.toml`. ~30 min of focused
work; deferred so it doesn't churn the codebase mid-feature.


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
