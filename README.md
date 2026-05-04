# awob

**A**nother **W**ayland **O**verlay **B**ar — a Wayland-only on-screen-display
daemon. A spiritual successor and drop-in replacement for the excellent
[wob](https://github.com/francma/wob) by Francesco Mariani, which awob owes
its name and FIFO format to. If wob covers what you need, **use wob** —
it's tiny, fast, and battle-tested. awob exists for users who want a
richer theming model, typed IPC, and a small ecosystem of event-source
listeners (PipeWire, UPower, backlight, keyboard-backlight) on top of
the same simple FIFO interface.

<!--
Demo video. To embed: open this README on github.com, click the
pencil icon, drag the MP4 into the editor. GitHub uploads it to
user-attachments.githubusercontent.com and pastes a URL of the form
`https://github.com/user-attachments/assets/<uuid>.mp4`. Replace the
placeholder URL below with that one and commit. The video will then
render inline as a player on the GitHub repo page.
-->

https://github.com/user-attachments/assets/c8455de5-f147-44d3-a8f9-01da59708e82

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
cargo install --path crates/awob-listener-battery
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

## Theme example

The default theme is a self-contained KDL scene file: palette + named
styles + element tree + animation phases all in one place. Below is
the full source — every other shipped theme builds on the same
vocabulary.

<details>
<summary><code>themes/default/scene.kdl</code> (93 lines)</summary>

```kdl
// awob default theme.
//
// Reference scene illustrating every concept the engine supports.
// Self-contained: palette + styles + scene live in this one file. To
// vary the colour scheme without editing the theme, use
// `--force-palette <path>` on awob-daemon (or `force_palette` in
// awob.toml). Themes that want to import a shared palette instead can
// — see `themes/light/scene.kdl` for an example.
//
// Demonstrates:
//   * fade-in / show / fade-out animation phases.
//   * Style-driven accent override (low / normal / warn / critical / muted).
//   * Per-element bindings ($value, $max, $lastValue, $event, $app, $icon).

palette {
    bg     "rgba(28,28,35,0.85)"
    fg     "#f3e8d7"
    track  "rgba(255,255,255,0.08)"
    low    "#8fdc55"
    normal "#baea96"
    warn   "#e89a49"
    crit   "#dc8855"
    muted  "#6e6e75"

    // Overflow state (value > max). Auto-applied by the daemon as
    // `style="overflow"` whenever the incoming value exceeds the
    // max. Defaults map to the critical accent so the visual reads
    // as "this is past the limit" without a separate colour knob;
    // override here for a more dramatic overflow look.
    overflow_bg     "rgba(28,28,35,0.85)"
    overflow_accent "#dc8855"
}

styles {
    style "low"      accent="$low"
    style "normal"   accent="$normal"
    style "warn"     accent="$warn"
    style "critical" accent="$crit"
    style "muted"    accent="$crit" alpha="0.6"
    // Auto-applied when value > max. Falls back to base bg so only
    // the bar (accent) flips; bg can be punched up in custom themes.
    style "overflow" bg="$overflow_bg" accent="$overflow_accent"
}

surface {
    width 360
    height 64
    anchor "bottom"
    offset 0 -56

    // Animation phases. Cycle = fade-in → value-transition (post fade-in)
    // → settled show → fade-out. Total visible window = fade-in + show
    // + fade-out (the value transition runs *during* show).
    fade-in    "150ms"
    show       "2000ms"
    fade-out   "150ms"
    // Bar value tween. Sequenced *after* fade-in completes so the bar
    // appears at the previous value, then visibly transitions toward the
    // new one. Override per-theme as `transition "<ms>"`.
    transition "300ms"
}

scene {
    // Container with rounded corners + soft drop shadow.
    rect z=0 \
        x=0 y=0 width="100%" height="100%" \
        radius=12 fill="$bg" \
        shadow="0 8 24 rgba(0,0,0,0.4)"

    // Icon: send-time $icon wins; otherwise fall back to event default.
    image z=1 src="{$icon ?? icon($event)}" \
        x=14 y="center" width=22 height=22

    // Label: send-time $app wins; otherwise event-derived label.
    // Truncated to 24 chars so a long $app can't collide with the
    // percentage readout on the right.
    text z=1 value="{truncate($app ?? label($event), 24)}" \
        x=46 y=14 font="Inter 14 500" colour="$fg"

    // Percentage readout, right-aligned. Rounded integer; the
    // anchor=top-right + x=14 puts it 14px from the right edge so it
    // sits above the bar's right end.
    text z=1 anchor="top-right" value="{int($progress * 100)}%" \
        x=14 y=14 font="Inter 14 500" colour="$fg"

    // Track behind the bar.
    rect z=1 x=46 y=42 width="100%-60" height=8 radius=999 fill="$track"

    // The bar. `from` taps lastValue so the engine tweens between renders.
    bar z=2 x=46 y=42 width="100%-60" height=8 radius=999 \
        fill="$accent" \
        min=0 max="$max" value="$value" from="{$lastValue ?? $value}"
}
```

</details>

## Documentation

The full docs site lives at <https://jmylchreest.github.io/awob/>.
Source markdown is under [`docs/docs/`](docs/docs/) — Docusaurus 3
project rooted at [`docs/`](docs/).

Key entry points:

* [Getting Started → Install](docs/docs/getting-started/install.md)
  — distro packages, building from source, the systemd unit.
* [Getting Started → Quick start](docs/docs/getting-started/quickstart.md)
* [Getting Started → Hyprland](docs/docs/getting-started/hyprland.md)
  / [GNOME](docs/docs/getting-started/gnome.md)
  / [Sway](docs/docs/getting-started/sway.md)
* [Getting Started → Migrating from wob](docs/docs/getting-started/migrating-from-wob.md)
* [Usage reference](docs/docs/usage.md) — CLI, `awob.toml`,
  troubleshooting.
* [Themes](docs/docs/themes.md) — scene file structure, element
  reference, expression language, palettes.
* [Protocol](docs/docs/protocol.md) — wire format + listener
  author guide.
* [`FUTURES.md`](FUTURES.md) — deferred work tracked for later iterations.

## Status

Pre-1.0. Wire format and theme schema may still change without a
deprecation cycle until 0.1.0. Pin a version in scripts.

## Licence

[MIT](LICENSE).
