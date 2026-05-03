# awob

**A**nother **W**ayland **O**verlay **B**ar — a Wayland-only on-screen-display
daemon. A spiritual successor and drop-in replacement for the excellent
[wob](https://github.com/francma/wob) by Francesco Mariani, which awob owes
its name and FIFO format to. If wob covers what you need, **use wob** —
it's tiny, fast, and battle-tested. awob exists for users who want a
richer theming model, typed IPC, and a small ecosystem of event-source
listeners (PipeWire, UPower, backlight, keyboard-backlight) on top of
the same simple FIFO interface.

```
                    ┌─────────────────────────────────────┐
                    │  ▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░░░░░░░░░░░░░  │
                    │  🔊  Speakers           ▓▓ wedge ░  │
                    └─────────────────────────────────────┘
```

* Single binary per process (daemon, CLI, each listener) — install only what
  you need.
* `wlr-layer-shell-v1` for the surface; `tiny-skia` + `cosmic-text` + `resvg`
  for rendering. No GTK, no Qt.
* Theme as data: a [KDL](https://kdl.dev) scene file describes elements,
  bindings, expressions, and an animation timeline. Hot-reloaded on save.
* Per-event-source listener processes hand the daemon typed
  `(source, event, value)` tuples over a Unix socket. Listeners are
  auto-discovered on `PATH`; one config line opts each one out.

## Install

From source (Linux + Wayland required):

```sh
cargo install --path crates/awob-cli       # `awob` CLI
cargo install --path crates/awob-daemon    # `awob-daemon` long-running process
cargo install --path crates/awob-listener-pipewire           # optional listeners
cargo install --path crates/awob-listener-upower
cargo install --path crates/awob-listener-backlight
cargo install --path crates/awob-listener-keyboard-backlight
cargo install --path crates/awob-listener-wob                # only if migrating from wob
```

## Quick start

```sh
# 1. Start the daemon. With no config it auto-discovers the listener
#    binaries above and spawns each one whose binary it can find.
awob-daemon

# 2. Drive the OSD from a script or keybind:
awob send --preempt --icon audio-volume-high volume 75 100
awob send --icon battery-low battery 12 100      # ambient (won't preempt)

# 3. Switch theme at runtime:
awob theme set wob              # pixel-faithful wob clone (in-memory only)
awob theme set tinct --persist  # also rewrites awob.toml so it survives restart
```

## Documentation

* [`docs/USAGE.md`](docs/USAGE.md) — CLI reference, `awob.toml` config,
  architecture overview, **migrating from wob**, troubleshooting.
* [`docs/THEMES.md`](docs/THEMES.md) — theme creator guide: scene file
  layout, element reference, expression language, palette imports,
  `manifest.toml` conventions.
* [`docs/PROTOCOL.md`](docs/PROTOCOL.md) — wire protocol + listener
  author guide: every Request/Response, SendPayload fields including
  `preempt`, history keying, building a new listener.
* [`FUTURES.md`](FUTURES.md) — deferred work tracked for later iterations.

## Status

Pre-1.0. Wire format and theme schema may still change without a
deprecation cycle until 0.1.0. Pin a version in scripts.

## Licence

[MIT](LICENSE).
