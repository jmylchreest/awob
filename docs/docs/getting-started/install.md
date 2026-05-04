---
sidebar_position: 1
title: Install
---

# Install

awob is Wayland-only and Linux-only. You'll need a compositor that
supports `wlr-layer-shell-v1` — Hyprland, Sway, KDE Plasma 6,
Wayfire, river, and most other wlroots-based compositors do.
GNOME's mutter has had layer-shell support landing in stages — see
the [GNOME setup page](/getting-started/gnome) for the current state.

## Distro packages

### Arch (AUR)

The full set of awob packages on AUR:

| Package | Contents |
|---|---|
| `awob-bin` | Daemon + CLI + stock themes + systemd user unit. The minimum viable install. |
| `awob-listener-pipewire-bin` | PipeWire volume / mute listener. |
| `awob-listener-battery-bin` | Battery + AC state listener. |
| `awob-listener-backlight-bin` | Display backlight listener. |
| `awob-listener-keyboard-backlight-bin` | Keyboard backlight listener. |
| `awob-listener-wob-bin` | wob-protocol FIFO bridge. |
| `awob-listeners-all` | Meta-package — pulls every listener above. No payload. |
| `awob-git` | From-source build of `main`: daemon + CLI + every listener + themes + systemd unit. |

Quick paths:

```sh
# Kitchen-sink: daemon + every listener.
paru -S awob-bin awob-listeners-all

# Daemon + a hand-picked listener subset.
paru -S awob-bin awob-listener-pipewire-bin awob-listener-battery-bin

# Track main from source.
paru -S awob-git
```

`yay` works identically. Then enable the systemd user unit:

```sh
systemctl --user enable --now awob.service
```

Once enabled, the auto-discovery in the daemon spawns each
`awob-listener-*` binary it finds on `PATH` — no extra config.

### Other distros

No `apt` / `dnf` package yet. Build from source (below) — open an
issue if you'd like to maintain a package.

## From source

You'll need:

* Rust 1.85 or later (`rustup install stable`)
* `pkg-config`
* fontconfig + freetype dev headers
* PipeWire dev headers (only if you want the PipeWire listener)
* libudev (only if you want the battery or backlight listeners — both
  use udev for hot-plug / state-change events)

On Debian / Ubuntu:

```sh
sudo apt install build-essential pkg-config \
  libfontconfig1-dev libfreetype6-dev \
  libpipewire-0.3-dev libdbus-1-dev \
  libudev-dev libxkbcommon-dev
```

On Arch:

```sh
sudo pacman -S base-devel pkgconf fontconfig freetype2 \
  pipewire dbus libxkbcommon
```

`just deps-hint` prints the equivalent for whichever distro you're
on.

Then:

```sh
git clone https://github.com/jmylchreest/awob
cd awob

# Install everything to ~/.cargo/bin (including all listeners).
just install

# Or just the daemon + CLI:
just install-min
```

`cargo install --path …` works directly too if you'd rather skip
`just`. Each crate under `crates/` is independently installable.

## systemd user service (optional)

```sh
mkdir -p ~/.config/systemd/user
cp contrib/systemd/awob.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now awob.service
```

The unit:

* Starts automatically on graphical-session login.
* Refuses to start if `WAYLAND_DISPLAY` isn't set.
* Restarts on failure but not on clean exit (Restart=on-failure).
* Sandbox: `ProtectSystem=strict`, `ProtectHome=read-only`,
  `MemoryDenyWriteExecute=yes`, etc.
* Logs to `journalctl --user -u awob`.

If you're on a compositor that uses `uwsm` or any other tool that
manages systemd graphical-session targets, the unit fits right in
because it's wired to `WantedBy=graphical-session.target`.

## Verify

```sh
awob version
# client: 0.0.1
# daemon: 0.0.1
# protocol: 1
```

If `awob version` errors with "connection refused", the daemon
isn't running. Either start it manually (`awob-daemon`) or enable
the systemd unit above.

## Next

* [Quick start](/getting-started/quickstart) — drive an OSD from a
  script.
* [Hyprland setup](/getting-started/hyprland) — example keybinds for
  the volume + brightness keys.
* [GNOME setup](/getting-started/gnome) — current state on mutter.
