# Demo scripts

Self-contained scenario scripts for showing off awob's behaviour or
spot-checking a build by eye. Each one fires a scripted sequence of
sends through the `awob` CLI against an already-running daemon —
none of these scripts start, stop, or kill the daemon.

## Prerequisites

* An `awob-daemon` running and bound to the default socket
  (`$XDG_RUNTIME_DIR/awob.sock`), e.g. via:

  ```sh
  ./target/release/awob-daemon --themes-dir ./themes --theme default
  ```

* The `awob` CLI on `$PATH`, or a release build at
  `./target/release/awob`. `lib.sh` finds either.

## Running

Individually:

```sh
bash demo/wedge.sh           # value-transition wedge
bash demo/preempt.sh         # preempt + queue policy
bash demo/cross-source.sh    # different (source, event) pairs
bash demo/icons.sh           # icon resolution + theme override
bash demo/styles.sh          # named styles + --accent override
bash demo/console.sh         # console theme: label cases + truncation
bash demo/themes.sh          # cycle through every installed theme
```

All in sequence:

```sh
bash demo/all.sh
```

Scripts use `set -eu` so a typo or a network/socket error stops the
script with a non-zero exit — handy for CI smoke tests.

## What each script does

| Script | Demonstrates |
|---|---|
| `wedge.sh` | Value transition: large grow / large shrink (no wedge) / tiny grow / rapid grows. |
| `preempt.sh` | Hot-swap on `preempt=true`, queue + drain on `preempt=false`, continuity for matching `(source, event)`, queue-replacement (newest wins). |
| `cross-source.sh` | Same source different events (volume → mute), different sources same event (speaker → headphones), different sources different events. |
| `icons.sh` | Real freedesktop icon, bogus name → theme's `image-missing-symbolic` override → embedded fallback. |
| `styles.sh` | `--style low/normal/warn/critical/muted` on the default theme; `--accent` override winning over style. |
| `console.sh` | Switches to the `console` theme and renders short / medium / long / very-long labels to show `upper()` + `truncate(…)` behaviour and cell-mode bar animation. Restores `default` at the end. |
| `themes.sh` | Iterates every theme directory under `themes/` and renders a sample on each. Restores `default` at the end. |
| `all.sh` | Runs everything in order. |

## Coverage map

If you want to verify a specific behaviour is exercised by the
demos, this is the rough mapping. (Unit tests in
`crates/awob-core/` cover the rest — `cargo test --workspace`.)

| Behaviour | Demo |
|---|---|
| Bar value tween with delta wedge | `wedge.sh` |
| Bar continuity vs hot-swap vs queue | `preempt.sh` |
| `(source, event)` history keying (no cross-event bleed) | `cross-source.sh` |
| Icon resolution via theme `icons/` override | `icons.sh` |
| Embedded fallback icon | `icons.sh` (when no theme override + system icon both miss) |
| Named style blocks | `styles.sh` |
| One-shot `--accent` colour override | `styles.sh` |
| Critical / muted styling | `styles.sh` |
| Theme switching at runtime | `console.sh`, `themes.sh` |
| Cell-mode bar rendering | `console.sh` |
| Monospace generic-family resolution | `console.sh` (label visible only if cosmic-text picked the system mono face) |
| `upper()` / `truncate()` builtins | `console.sh` |
| `lower()` / `capitalize()` builtins | unit tests in `crates/awob-core/src/expr.rs` |

## Adding a new demo

1. Copy `wedge.sh` as a starting template.
2. Set `SCRIPT_NAME="<your-name>"` near the top.
3. Use `say` for scenario headers, `note` for explanatory text,
   `"$AWOB" send …` for actual events. `srcid` produces a stable
   PID-suffixed source for the script.
4. `chmod +x demo/<your>.sh` (optional — scripts run fine via
   `bash demo/<your>.sh`).
5. List it in `demo/all.sh` if it should run in the suite.
