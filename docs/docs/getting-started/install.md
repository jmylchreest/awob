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

| Distro | Package | Notes |
|---|---|---|
| Arch (AUR) | `awob-bin` | Prebuilt; tracks tagged releases. |
| Arch (AUR) | `awob-git` | Builds from the latest `main`. |

Other distros: build from source. There's no `apt` / `dnf` package
yet — open an issue if you'd like to maintain one.

## From source

You'll need:

* Rust 1.85 or later (`rustup install stable`)
* `pkg-config`
* fontconfig + freetype dev headers
* PipeWire dev headers (only if you want the PipeWire listener)
* D-Bus dev headers (only if you want the UPower listener)
* libudev (only if you want the backlight listener)

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
