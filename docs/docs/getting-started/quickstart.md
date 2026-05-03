---
sidebar_position: 2
title: Quick start
---

# Quick start

Five minutes from "what is this" to "OSD on screen".

## 1. Start the daemon

```sh
awob-daemon
```

Or, if you've enabled the systemd unit, it's already running. Check
with:

```sh
systemctl --user status awob.service
```

Auto-discovery picks up any of the bundled listeners that are on
your `$PATH` and spawns them. With nothing else installed, the
daemon just sits idle waiting for sends.

## 2. Send your first OSD

```sh
awob send --preempt --icon audio-volume-high volume 0.7 1.0
```

You should see a small ribbon fade in at the bottom of the screen,
showing a green bar at ~70%, an icon, and the label "Volume". It
fades out after about 2 seconds.

`--preempt` marks the send as user-initiated — see
[Preempt semantics](/usage#preempt-semantics).

## 3. Wire up a keybind

For most people, the goal is "volume up" → "OSD shows new volume".
A typical chain looks like:

```sh
# Increase PulseAudio volume by 5%, then send the new value to awob.
NEW=$(pactl get-sink-volume @DEFAULT_SINK@ | grep -oP '\d+%' | head -1 | tr -d %)
awob send --preempt --icon audio-volume-high volume "$NEW" 100
```

But you don't need to script this yourself — the bundled
**`awob-listener-pipewire`** subscribes to PipeWire and emits
volume sends automatically. If it's installed, it auto-spawns when
the daemon starts. Volume keys → OSD with no extra glue.

Same pattern for:

* `awob-listener-battery` — battery state → OSD on level changes
* `awob-listener-backlight` — display brightness via sysfs
* `awob-listener-keyboard-backlight` — keyboard LED brightness
* `awob-listener-wob` — read a wob-format FIFO and forward sends

See your compositor's setup page for keybind examples:

* [Hyprland](/getting-started/hyprland)
* [GNOME](/getting-started/gnome)
* [Sway](/getting-started/sway)

## 4. Pick a different theme

```sh
awob theme set wob               # pixel-faithful wob clone
awob theme set console           # monospace / ANSI-green / cell-block bar
awob theme set minimal           # tiny progress ribbon
awob theme set default --persist # keep this choice across restarts
```

Without `--persist`, theme switches are in-memory only — handy for
trying things on. With `--persist`, the daemon rewrites your
`~/.config/awob/awob.toml` (preserving comments and other settings)
so the choice survives a daemon restart.

## 5. Inspect what's tracked

```sh
awob query
# pipewire   pw-speaker   event=volume     value=0.7  max=1.0  age=12.3s
# upower     dev          event=battery    value=0.55 max=1.0  age=300.1s
```

The history is keyed by `(source, event)` — see
[history keying](/protocol#history-keying).

## Next

* [Usage reference](/usage) — every CLI flag, every config option.
* [Themes](/themes) — write your own.
* [Protocol](/protocol) — write your own listener.
