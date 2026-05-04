---
sidebar_position: 3
title: Hyprland
---

# Hyprland

Hyprland implements `wlr-layer-shell-v1` natively, so awob works
out of the box. Two integration patterns:

## Pattern A: systemd user unit + auto-discovery

Recommended. The daemon starts when your graphical session starts;
the bundled listeners auto-spawn; volume / brightness keys "just
work" because `awob-listener-pipewire` and
`awob-listener-backlight` see the underlying state changes
directly.

```sh
# 1. Install awob and the listeners you want.
just install

# 2. Drop in the systemd user unit.
mkdir -p ~/.config/systemd/user
cp contrib/systemd/awob.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now awob.service
```

That's it. No keybinds to write. The PipeWire listener drives volume,
the battery listener drives battery state, the backlight listener
drives brightness.

If you're using `uwsm` (Universal Wayland Session Manager), the
unit's `WantedBy=graphical-session.target` plays nice with it
without any extra config.

## Pattern B: keybind-driven via `awob send`

If you'd rather not run listeners — or need finer-grained control
over what shows up on screen — drive `awob send` from your
keybinds. Add to `~/.config/hypr/hyprland.conf`:

```ini
# Volume up / down via PulseAudio (works whether your audio backend
# is PipeWire or PulseAudio, thanks to pactl). Pipe through awob.
bind = , XF86AudioRaiseVolume, exec, pactl set-sink-volume @DEFAULT_SINK@ +5% && \
    awob send --preempt --icon audio-volume-high volume \
    "$(pactl get-sink-volume @DEFAULT_SINK@ | grep -oP '\d+%' | head -1 | tr -d %)" 100

bind = , XF86AudioLowerVolume, exec, pactl set-sink-volume @DEFAULT_SINK@ -5% && \
    awob send --preempt --icon audio-volume-low volume \
    "$(pactl get-sink-volume @DEFAULT_SINK@ | grep -oP '\d+%' | head -1 | tr -d %)" 100

bind = , XF86AudioMute, exec, pactl set-sink-mute @DEFAULT_SINK@ toggle && \
    awob send --preempt --icon audio-volume-muted --style critical mute 1 1

# Mic mute.
bind = , XF86AudioMicMute, exec, pactl set-source-mute @DEFAULT_SOURCE@ toggle && \
    awob send --preempt --icon microphone-disabled --style critical mic 1 1

# Brightness up / down via brightnessctl.
bind = , XF86MonBrightnessUp, exec, brightnessctl set 5%+ && \
    awob send --preempt --icon display-brightness brightness \
    "$(brightnessctl -m | cut -d, -f4 | tr -d %)" 100

bind = , XF86MonBrightnessDown, exec, brightnessctl set 5%- && \
    awob send --preempt --icon display-brightness brightness \
    "$(brightnessctl -m | cut -d, -f4 | tr -d %)" 100
```

These rely on `pactl` and `brightnessctl` being installed (commonly
already are on a Hyprland system).

## Surface position

awob's stock themes anchor to the bottom of the focused output.
Hyprland honours that natively. To anchor differently — top, side,
specific monitor — edit your theme's `surface { anchor … }` block.
See [Themes → Surface](/themes#surface--).

If you have multiple outputs and want the OSD on a specific one,
that requires either an explicit anchor on the surface or a future
multi-output mode in the daemon — not implemented yet, see
[FUTURES.md](https://github.com/jmylchreest/awob/blob/main/FUTURES.md).

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| OSD never appears | Daemon not running. `systemctl --user status awob.service`. |
| OSD shows but Hyprland flickers | Conflict with another layer-shell tool. Most commonly an old wob daemon — `pkill wob`. |
| Listener errors in journal | The relevant upstream isn't running (PipeWire), or your user lacks access to the underlying device (sysfs backlight / `/sys/class/power_supply/*` on some hardware). |
