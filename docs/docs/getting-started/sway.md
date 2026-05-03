---
sidebar_position: 5
title: Sway
---

# Sway

Sway is a wlroots compositor and implements `wlr-layer-shell-v1`,
so awob works as-is. Setup is essentially the same as Hyprland.

## systemd user unit + auto-discovery

```sh
just install
mkdir -p ~/.config/systemd/user
cp contrib/systemd/awob.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now awob.service
```

If your `~/.config/sway/config` includes
`exec systemctl --user start sway-session.target` (the standard
"sway-session.target wires graphical-session.target" pattern), the
unit fires automatically when sway starts.

## Keybind-driven (alternative)

If you don't run listeners, drive `awob send` from sway bindsyms:

```
bindsym XF86AudioRaiseVolume exec pactl set-sink-volume @DEFAULT_SINK@ +5% && \
    awob send --preempt --icon audio-volume-high volume \
    $(pactl get-sink-volume @DEFAULT_SINK@ | grep -oP '\d+%' | head -1 | tr -d %) 100

bindsym XF86AudioLowerVolume exec pactl set-sink-volume @DEFAULT_SINK@ -5% && \
    awob send --preempt --icon audio-volume-low volume \
    $(pactl get-sink-volume @DEFAULT_SINK@ | grep -oP '\d+%' | head -1 | tr -d %) 100

bindsym XF86AudioMute exec pactl set-sink-mute @DEFAULT_SINK@ toggle && \
    awob send --preempt --icon audio-volume-muted --style critical mute 1 1

bindsym XF86MonBrightnessUp exec brightnessctl set 5%+ && \
    awob send --preempt --icon display-brightness brightness \
    $(brightnessctl -m | cut -d, -f4 | tr -d %) 100

bindsym XF86MonBrightnessDown exec brightnessctl set 5%- && \
    awob send --preempt --icon display-brightness brightness \
    $(brightnessctl -m | cut -d, -f4 | tr -d %) 100
```

## Migrating from wob on Sway

Sway is the most common platform for wob. To swap:

```toml
# ~/.config/awob/awob.toml
theme = "wob"

[[listeners]]
name = "wob-fifo"
command = "awob-listener-wob"
args = ["--fifo", "$XDG_RUNTIME_DIR/wob.sock"]
```

`pkill wob` and your existing scripts that write to the FIFO keep
working. See [Migrating from wob](/getting-started/migrating-from-wob)
for the long form.
