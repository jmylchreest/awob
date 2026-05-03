---
sidebar_position: 4
title: GNOME
---

# GNOME

GNOME's mutter compositor has historically not implemented
`wlr-layer-shell-v1`, the protocol awob uses to draw its surface.
That changed in mutter 47 with experimental layer-shell support, and
GNOME 49 brought it closer to mainline. **awob runs on
sufficiently recent GNOME but the experience is hit-or-miss
depending on your version.**

If your GNOME doesn't have layer-shell, you have two options:

* Use GNOME's built-in OSD (gnome-shell handles volume / brightness
  natively).
* Switch to a compositor with first-class layer-shell support
  (Hyprland, Sway, KDE Plasma, river, Wayfire, …).

## Checking layer-shell support

```sh
echo $WAYLAND_DISPLAY    # confirm Wayland (not X11)
awob-daemon              # foreground; watch the log
```

If awob can't bind a layer-shell surface, you'll see:

```
warning: layer-shell global missing
```

In that case, GNOME's built-in OSD is your friend.

## Pattern: keybind-driven, complementing GNOME's own OSD

Even on a compositor without layer-shell, `awob send` works as long
as the daemon can bind *some* surface — but the OSD itself won't
render. The use case where awob makes sense on GNOME is:

* You're on GNOME 47+ with mutter's experimental layer-shell support
  enabled, **and**
* You want the richer theming or the listener ecosystem.

In that case, the same keybind patterns from
[Hyprland setup](/getting-started/hyprland#pattern-b-keybind-driven-via-awob-send)
apply. Bind your volume / brightness keys via
GNOME Settings → Keyboard → Custom Shortcuts.

## Pattern: GNOME Shell + listener-driven sends

If you'd rather GNOME Shell continue to render its native OSDs and
have awob coexist (e.g. for battery / network listeners that
GNOME doesn't surface as transient bars), run only the listeners
you care about and disable auto-discovery for the rest:

```toml
# ~/.config/awob/awob.toml
theme = "minimal"

[supervisor]
disable = ["pipewire", "backlight", "keyboard-backlight"]
```

This leaves only `awob-listener-upower` running, so awob shows
battery state and lets GNOME handle volume / brightness.

## Status

GNOME / mutter layer-shell support is moving. If you're on a recent
mutter and awob fails to draw, file an issue with
`mutter --version` and your `awob-daemon` log. There's a tracking
section in [`FUTURES.md`](https://github.com/jmylchreest/awob/blob/main/FUTURES.md).
