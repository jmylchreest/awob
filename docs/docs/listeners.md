---
sidebar_position: 4
title: Listeners
---

# Listeners

awob ships five official listener binaries. Each subscribes to one
upstream (PipeWire, sysfs+udev, or a FIFO), translates events into
typed sends, and forwards them to the daemon. Pick the slice of
listeners that matches your hardware — there's no penalty for
running all five, and no obligation to run any.

This page is a per-listener reference: what each listener does, the
flags it takes, the hardware it watches, and the behaviour the daemon
expects from it.

## Cross-cutting behaviour

Every listener follows the same lifecycle:

* **Silent startup.** Listeners never fire an OSD on first
  observation. They seed an internal `last` baseline from whatever
  the upstream's current state is and fall through to their main
  loop. The first OSD only surfaces when a real change crosses that
  baseline. Daemon restarts and supervisor respawns are invisible to
  the user.
* **Wait + rescan on no-device.** When a listener can't find anything
  to watch (no battery on a desktop, no `/sys/class/leds/*kbd*`
  entry), it does *not* exit. It logs the no-device state once at
  INFO and enters a 60 s rescan loop, picking up hot-plugged hardware
  when it appears. The supervisor never enters a tight respawn loop.
* **Stable `source` IDs.** Each listener uses a stable `source`
  identifier (sysfs device name, PipeWire object id, etc) so the
  daemon's history map keys cleanly across listener restarts.
* **`preempt` defaults differ by listener.** Volume / brightness /
  keyboard-backlight changes are direct user input → `preempt=true`.
  Battery state is ambient → `preempt=false`. The wob FIFO bridge
  forwards `preempt=true` (assuming wob clients are keybind-driven).

---

## `awob-listener-pipewire`

Subscribes to the PipeWire graph via the native pipewire-rs crate;
fires an OSD on every audio-node volume or mute change. One logical
listener per Audio node, so output and input devices fire
independently.

### What it watches

* All `Audio/Sink` nodes (output devices: speakers, headphones, HDMI
  audio).
* All `Audio/Source` nodes (input devices: microphones).
* Volume changes (PipeWire's `channelVolumes` mean) and mute toggles.

### CLI flags

| Flag | Default | Notes |
|---|---|---|
| `--socket <PATH>` | (auto) | Override the daemon socket path. |
| `--mute-volume-zero` | `false` | If set, treat `mute=true` as `value=0` in the OSD. Default is to keep the volume value and add a `muted` style. |

### Hardware requirements

* PipeWire running. Doesn't need PulseAudio's compatibility shim;
  doesn't speak ALSA directly.

### Source / event shape

Per node:

* `source = pipewire-<node-hash>` (stable across PipeWire restarts).
* `event = volume` or `event = mute`.
* `app = node.description` from PipeWire (e.g. "Built-in Audio
  Analog Stereo").
* `icon` resolves to `audio-volume-{high,medium,low,muted}` (sinks)
  or `microphone-sensitivity-{high,medium,low,muted}` (sources).
* `style` defaults to `low` / `normal` / `critical` based on level;
  `muted` overrides everything when mute is engaged.

### Caveats

* Default behaviour is "every audio node fires its own OSD". If a
  user wants only the active default sink/source, that's a deferred
  `--default-only` flag (see `FUTURES.md`).

---

## `awob-listener-battery`

Watches `/sys/class/power_supply/*` of `type=Battery` via udev
`power_supply` uevents (instant) plus a 60 s rescan as backstop and
a 5 s burst-poll window after every uevent (handles hardware where
the battery driver lags the AC adapter's uevent by a few seconds —
Dell, ThinkPad, Framework).

### What it watches

* All `/sys/class/power_supply/*` directories with `type=Battery`.
* Per-battery: `capacity`, `status` (Charging / Discharging /
  Full / Not charging / Empty), `energy_full` / `charge_full` for
  multi-battery weighting.

### CLI flags

| Flag | Default | Notes |
|---|---|---|
| `--socket <PATH>` | (auto) | |
| `--source <SUFFIX>` | `battery` | Final source = `battery-<suffix>`. |
| `--states <LIST>` | `charging,discharging,empty,fully-charged` | Comma-separated state filter. Recognised: `charging`, `discharging`, `empty`, `fully-charged`, `pending-charge`, `pending-discharge`, `unknown`, `all`. |
| `--alert-bands <LIST>` | `empty,caution` | Comma-separated capacity bands that fire OSDs on entry. |

### Capacity bands

A single `BANDS` table drives alert filtering, OSD style, and icon
selection — they stay in lockstep:

| Band | Capacity | Style (discharging) | Icon (discharging) |
|---|---|---|---|
| `empty` | 0–5 % | `critical` | `battery-empty` |
| `caution` | 6–20 % | `warn` | `battery-caution` |
| `low` | 21–50 % | `normal` | `battery-low` |
| `good` | 51–80 % | `normal` | `battery-good` |
| `full` | 81–100 % | `normal` | `battery-full` |

State transitions (Charging↔Discharging↔FullyCharged) **always** fire
regardless of `--alert-bands`. Capacity-only changes fire on **entry
into a band listed in `--alert-bands`**. Charging always uses style
`normal` (charging isn't an emergency); the icon swaps to the
`-charging` / `-charged` variants.

So the default `--alert-bands empty,caution` produces:

* AC unplug at 87 % → OSD (state transition)
* Drains 87 → 21 % → silent
* Hits 20 % → OSD (entered caution)
* Drains 20 → 6 % → silent
* Hits 5 % → OSD (entered empty)
* AC plug → OSD (state transition)

To get a notification at every band entry: `--alert-bands all`. To
suppress every band entry and only see state transitions:
`--alert-bands none`.

### Hardware requirements

* `/sys/class/power_supply/` populated by the kernel.

### Caveats

* USB HID UPSes (Eaton, APC, CyberPower) **don't appear under
  `/sys/class/power_supply/`** — the Linux kernel has no general
  HID-Power-Device → power_supply driver. UPower sees them via
  direct libusb HID parsing. A future `awob-listener-ups` is tracked
  in `FUTURES.md` for that case.
* Bluetooth peripheral batteries (keyboards, mice, headphones)
  similarly don't all surface in sysfs; tracked separately.

---

## `awob-listener-backlight`

Watches `/sys/class/backlight/<dev>/brightness` via inotify (wakes on
userspace writes from brightnessctl etc) plus an adaptive-interval
sysfs poll. Resolves a friendly device name from `wl_output.make` /
`model` for the matching connector.

### What it watches

* All `/sys/class/backlight/*` devices, picking the first one found
  unless `--device` is specified.
* `brightness` (current) and `max_brightness` (range) under each
  device.

### CLI flags

| Flag | Default | Notes |
|---|---|---|
| `--device <NAME>` | (auto) | e.g. `intel_backlight`, `amdgpu_bl1`. |
| `--socket <PATH>` | (auto) | |
| `--source <ID>` | `backlight-<sanitised-device>` | |
| `--label <STRING>` | (auto) | Override the auto-detected `wl_output` make+model. |
| `--poll-interval <MS>` | `250` | Polling cadence in ms. Range `100..=2000`. Worst-case OSD latency on hardware where neither inotify nor udev fires. |

### Hardware requirements

* `/sys/class/backlight/` populated. Most laptops surface their
  internal panel here as `intel_backlight`, `amdgpu_bl1`, etc.
* External display brightness is rarely exposed (DDC/CI is what
  `ddcutil` does). Out of scope for this listener.

### Caveats

* On hardware where firmware writes the cached brightness without
  firing `kernfs_notify()` (rare for displays), the polling backstop
  is the only signal. Default 250 ms gives 4 reads/sec; adjust via
  `--poll-interval` if your hardware has a different sweet spot.

---

## `awob-listener-keyboard-backlight`

Watches `/sys/class/leds/<kbd>/brightness` for any LED whose name
contains `kbd` or `keyboard`. Uses an additive monitor — inotify +
udev + adaptive sysfs polling, all feeding one wake channel.

### What it watches

* `/sys/class/leds/*kbd*` / `*keyboard*` LED entries with a
  `brightness` attribute.
* Auto-discovers the first match; `--device` overrides.

### CLI flags

| Flag | Default | Notes |
|---|---|---|
| `--device <NAME>` | (auto) | e.g. `tpacpi::kbd_backlight`, `chromeos::kbd_backlight`. |
| `--socket <PATH>` | (auto) | |
| `--source <ID>` | `<sanitised-device>` | |
| `--label <STRING>` | (auto) | Default `"Keyboard"` for one device, `"Keyboard 1"` / `"2"` for multiple. |
| `--poll-interval <MS>` | `250` | Polling cadence. Range `100..=2000`. Critical for Framework laptops with the chromeos EC, where neither inotify nor udev fires on backlight key presses. |

### Why three wake sources?

No single mechanism is reliable across the laptop ecosystem:

* **inotify** fires on userspace `write()` to sysfs (brightnessctl
  hotkey scripts). Microsecond latency, doesn't fire on
  firmware-driven changes.
* **udev** fires on `kobject_uevent()` calls in the LED driver.
  Some EC drivers do, some don't.
* **Polling** (250 ms by default) is the backstop — Framework's
  chromeos EC updates the cached brightness without firing either
  notification primitive, so polling is the only signal.

All three feed a single mpsc channel; whichever wakes first wins.
The polling cadence is the worst-case latency floor.

### Hardware requirements

* A built-in keyboard backlight that the kernel exposes as a sysfs
  LED. **USB and Bluetooth keyboards with vendor-specific RGB
  protocols** (Wooting, Razer, Logitech G-series, Apple Magic
  Keyboard) generally don't expose backlight as a sysfs LED — they
  speak proprietary HID protocols handled by tools like Wootility,
  OpenRazer, etc. Those keyboards need their own listener (not yet
  built).

### Caveats

* The auto-discovery picks the *first* matching LED. On systems with
  multiple keyboard backlights (built-in plus a USB keyboard with a
  kernel-exposed LED), specify `--device` to pin to one.

---

## `awob-listener-wob`

Reads wob's positional FIFO format from a named pipe and forwards
each line as a typed send. Drop-in replacement for the wob daemon
itself when you want to keep existing keybinds working unchanged.

### What it watches

* A named FIFO. wob's standard path is `$XDG_RUNTIME_DIR/wob.sock`
  (set by wob's own `wob.socket` systemd unit), which is exactly the
  awob listener's default.

### CLI flags

| Flag | Default | Notes |
|---|---|---|
| `--fifo <PATH>` | `$XDG_RUNTIME_DIR/wob.sock` | Named pipe to read. |
| `--socket <PATH>` | (auto) | |
| `--event <NAME>` | `wob` | Event name attached to every send. |
| `--source <ID>` | `wob-fifo-<pid>` | |

### Wire format read

```
<value>
<value> <bg-colour>
<value> <bg-colour> <border-colour>
<value> <bg-colour> <border-colour> <bar-colour>
```

Same format wob has read since v0.16. Bare numbers map to
`event=wob value=<n> max=100`. Trailing colour words (hex with
optional alpha) override the bg / border / bar fields on the
outgoing send so wob's positional style spec keeps working.

### Hardware requirements

None — it's a FIFO reader. Nothing needs to be present at startup;
the listener creates the FIFO if it doesn't exist and waits.

### Why isn't this auto-discovered?

awob's auto-discovery only spawns listeners that can run with no
required arguments. The wob FIFO listener defaults to a sensible
path but ships *not* in the registry on purpose: a configuration
that auto-creates a FIFO at `$XDG_RUNTIME_DIR/wob.sock` would
conflict with users who actually run wob alongside awob, or who
want their FIFO at a different location. Add it explicitly via
`[[listeners]]` (see below).

### Drop-in for wob

If you've replaced wob outright:

```toml
# ~/.config/awob/awob.toml
[[listeners]]
name = "wob-fifo"
command = "awob-listener-wob"
args = ["--fifo", "$XDG_RUNTIME_DIR/wob.sock"]
restart = "always"
```

Then any tool that previously spoke to wob (`echo 50 > $XDG_RUNTIME_DIR/wob.sock`)
now drives an awob OSD with the active theme.

---

## Configuring listeners via `awob.toml`

awob.toml has two listener-related sections: `[supervisor]` (governs
auto-discovery) and `[[listeners]]` (one block per explicit listener).

### Auto-discovery

```toml
[supervisor]
auto = true                       # default; opt out with auto = false
disable = []                      # names from KNOWN_LISTENERS to skip
```

With `auto = true`, the daemon walks the internal `KNOWN_LISTENERS`
registry on startup and spawns each one whose binary is found:

| Name | Binary |
|---|---|
| `pipewire` | `awob-listener-pipewire` |
| `battery` | `awob-listener-battery` |
| `backlight` | `awob-listener-backlight` |
| `keyboard-backlight` | `awob-listener-keyboard-backlight` |

Lookup order: the directory containing `awob-daemon`, then `$PATH`.
Dev workflows running from `target/release/` therefore pick up
sibling listener binaries automatically.

The wob FIFO bridge is not in this registry — it requires explicit
`[[listeners]]` configuration so the FIFO path is intentional.

### Disabling specific auto-discovered listeners

```toml
[supervisor]
auto = true
disable = ["battery"]             # don't auto-spawn the battery listener
```

`disable` is a list of names from the `KNOWN_LISTENERS` table above.
Anything not in the registry is irrelevant. To turn off auto-discovery
entirely: `auto = false`.

### Adding non-auto listeners

```toml
[[listeners]]
name = "wob-fifo"                 # unique listener name
command = "awob-listener-wob"     # binary path or PATH-resolvable name
args = ["--fifo", "$XDG_RUNTIME_DIR/wob.sock"]
restart = "always"                # always | on-failure | never

[listeners.env]                   # optional env vars to inject
RUST_LOG = "debug"
```

`$VAR` and `~/` in `command`, `args`, and env values expand at parse
time.

### Customising auto-discovered listener arguments

The merge rule is: **explicit `[[listeners]]` entries with the same
`name` as an auto-discovered one win**. Auto-discovery sees the name
already claimed and skips it.

So to pass `--alert-bands all` to the battery listener (which would
otherwise be auto-discovered), declare it explicitly:

```toml
[[listeners]]
name = "battery"                  # same name as the auto entry → overrides it
command = "awob-listener-battery"
args = ["--alert-bands", "all"]
restart = "always"
```

The auto-discovery path only fires if you don't list a
same-named explicit entry. There's no "add args without
overriding" mode — full replacement is the canonical mechanism. If
you want a non-default flag on an auto entry, copy the entry into
`[[listeners]]` with the flags you want.

### Restart policy

`restart` controls what the supervisor does when a listener exits:

| Value | Behaviour |
|---|---|
| `always` | Respawn on any exit (default for auto-discovered entries). |
| `on-failure` | Respawn only on non-zero exit / crash. |
| `never` | Never respawn — one-shot listeners. |

Listeners following the conventions on this page (silent startup,
wait + rescan on no-device) virtually never exit on their own, so
the supervisor's respawn machinery is mostly inert in practice.
Real exits indicate genuine failures the supervisor *should*
recover from automatically.

### Disabling everything

```toml
[supervisor]
auto = false                      # no auto-discovery

# no [[listeners]] entries
```

Daemon runs the surface + IPC server, but never spawns a child
process. Useful for keybind-only setups where every send comes from
the CLI directly.

## Writing a custom listener

The daemon doesn't care which listener fired; it just renders the
OSD according to the theme. To add a new event source, write a
binary that:

1. Connects to `$XDG_RUNTIME_DIR/awob.sock`.
2. Sends `Hello { protocol: PROTOCOL_VERSION }`.
3. Sends `Send { listener_id, source, event, value, max?, style?, ... }`
   on every event from your upstream.
4. Reuses a stable `source` ID across reconnects so the daemon's
   history map keys cleanly.
5. Stays alive across no-device states (sleep + rescan rather than
   exit).
6. Doesn't fire an OSD on first observation — seed `last` silently.

Use the `awob-client` crate from this workspace (or any
JSON-line–speaking IPC client; see [Protocol](/protocol)) and
register the binary in your `awob.toml` under `[[listeners]]`. The
daemon treats third-party listeners and bundled ones identically.
