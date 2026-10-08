# awob

**Another Wayland Overlay Bar** — a Wayland on-screen display for volume, brightness, battery status, and custom events.

Inspired by [wob](https://github.com/francma/wob) by Francesco Mariani, which also gave awob its name and FIFO format. wob remains a good choice if you want a simple bar. awob adds theming, animations, icons, and listeners for PipeWire, battery, and backlight events.

https://github.com/user-attachments/assets/c8455de5-f147-44d3-a8f9-01da59708e82

- Themes use [KDL](https://kdl.dev) and are hot-reloaded.
- Optional listeners handle volume, mute, battery, screen brightness, and keyboard backlight changes.
- The `awob` CLI sends custom events and controls the daemon.
- A wob compatibility listener accepts the existing FIFO format.
- Rendering uses `tiny-skia`, `cosmic-text`, and `resvg`. No GTK or Qt.

Requires Linux and a Wayland compositor with `wlr-layer-shell-v1` support.

## Install

### Arch Linux

Install the daemon, CLI, themes, and all official listeners from the AUR:

```sh
paru -S awob-bin awob-listeners-all
```

Or install `awob-bin` with individual listeners:

| Package | Listener |
| --- | --- |
| `awob-listener-pipewire-bin` | Volume and mute |
| `awob-listener-battery-bin` | Battery and power status |
| `awob-listener-backlight-bin` | Screen brightness |
| `awob-listener-keyboard-backlight-bin` | Keyboard backlight |
| `awob-listener-wob-bin` | wob FIFO compatibility |

`awob-git` builds everything from `main`.

### From source

See the [installation guide](docs/docs/getting-started/install.md) for build dependencies.

```sh
git clone https://github.com/jmylchreest/awob
cd awob

cargo install --path crates/awob-daemon
cargo install --path crates/awob-cli
```

Install whichever listeners you need:

```sh
cargo install --path crates/awob-listener-pipewire
cargo install --path crates/awob-listener-battery
cargo install --path crates/awob-listener-backlight
cargo install --path crates/awob-listener-keyboard-backlight
cargo install --path crates/awob-listener-wob
```

## Running

Start the daemon:

```sh
awob-daemon
```

By default, the daemon discovers and starts installed listeners on `PATH`. With the PipeWire listener installed, volume and mute changes appear automatically.

For autostart, enable the systemd user service if your desktop manages `graphical-session.target`:

```sh
systemctl --user enable --now awob.service
```

The AUR package includes the service. For source installations, see [systemd setup](docs/docs/getting-started/install.md#systemd-user-service-optional).

Alternatively, add the daemon to your compositor’s autostart configuration. On Hyprland:

```ini
# ~/.config/hypr/hyprland.conf
exec-once = awob-daemon
```

## Usage

Send an update immediately, preempting the current display:

```sh
awob send --preempt --icon audio-volume-high volume 75 100
```

Omit `--preempt` for a queued update:

```sh
awob send --icon battery-low battery 12 100
```

Switch themes:

```sh
awob theme set wob
awob theme set tinct --persist
```

`--persist` saves the theme choice to `awob.toml`. Without it, the change lasts until the daemon restarts.

## Themes

Themes define the palette, layout, icons, text, and animations in a KDL scene file and are hot-reloaded.

See the [default scene](themes/default/scene.kdl) for an example, or the [theme reference](docs/docs/themes.md) for elements, bindings, expressions, and animation settings.

## Documentation

[Full documentation](https://jmylchreest.github.io/awob/)

- [Installation](docs/docs/getting-started/install.md)
- [Quick start](docs/docs/getting-started/quickstart.md)
- [Hyprland](docs/docs/getting-started/hyprland.md) / [Sway](docs/docs/getting-started/sway.md)
- [Migrating from wob](docs/docs/getting-started/migrating-from-wob.md)
- [CLI and configuration](docs/docs/usage.md)
- [Themes](docs/docs/themes.md)
- [Protocol and writing listeners](docs/docs/protocol.md)

## Status

Under development. The protocol and theme format are not yet stable.

## Licence

[MIT](LICENSE).
