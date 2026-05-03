# awob: usage guide

End-user reference. For theme authoring see
[`THEMES.md`](THEMES.md); for the wire protocol or building a new
listener see [`PROTOCOL.md`](PROTOCOL.md).

## Architecture

```
┌─────────────────────┐     unix socket      ┌──────────────────────┐
│  awob-listener-*    │ ──── JSON-lines ────▶│  awob-daemon         │
│  (pipewire, upower, │                       │  - history map        │
│   backlight, …)     │                       │  - theme + bindings   │
└─────────────────────┘                       │  - wlr-layer-shell    │
                                              │    surface (one OSD)  │
┌─────────────────────┐                       │  - listener supervisor│
│  awob (CLI)         │ ─────────────────────▶│                       │
│  awob send …        │                       └───────────┬───────────┘
└─────────────────────┘                                   │
                                                          ▼
                                                   ┌──────────────┐
                                                   │  Wayland     │
                                                   │  compositor  │
                                                   └──────────────┘
```

* **`awob-daemon`** owns the surface and the IPC socket. One per
  Wayland session.
* **`awob` (CLI)** is a one-shot client for sending events from
  scripts or keybinds.
* **`awob-listener-*`** processes are long-running event sources. They
  subscribe to their upstream (PipeWire, UPower, sysfs, FIFO, …) and
  forward typed sends to the daemon. Spawned and restarted by the
  daemon's supervisor.

The IPC socket lives at `$XDG_RUNTIME_DIR/awob.sock` by default.

## CLI

### `awob send <event> <value> [max]`

Send a value to the daemon.

| Flag | Purpose |
|---|---|
| `--source <id>` | Stable source identity. Required for `$lastValue` history (otherwise the renderer sees `Null`). Per-(source, event) keyed. |
| `--listener-id <id>` | Listener identity for duplicate-process detection. CLI invocations leave this unset by design — every `awob send` is a one-shot. |
| `--style <name>` | Apply a named style block (e.g. `low`, `normal`, `warn`, `critical`, `muted`). |
| `--accent <css>` | One-off colour override (`"#ff00aa"`, `"rgba(…)"`). |
| `--app <label>` | Free-form label string available as `$app` in the theme. |
| `--icon <name>` | Icon name (freedesktop name like `audio-volume-high`, absolute path, or `data:` URI). |
| `--timeout <ms>` | One-shot override for the surface `show` duration. |
| `--preempt` | Mark the send as user-interactive. See [Preempt semantics](#preempt-semantics) below. |

Examples:

```sh
awob send volume 75              # max defaults to 100 → 75%
awob send volume 50 200          # 50/200 = 25%
awob send --preempt --icon audio-volume-high --source pw-speaker volume 0.7 1.0
```

### `awob query [--source <id>]`

List the daemon's history. With `--source`, returns every recorded
event for that source.

### `awob theme set <name> [--persist]`

Switch the active theme at runtime. Without `--persist` the change is
in-memory only. With `--persist` the daemon rewrites `awob.toml`'s
`theme` key in place (preserving comments and other tables) so the
choice survives daemon restart.

If `<name>` can't be loaded, the daemon keeps the current theme and
returns an error. At cold start, an unloadable theme falls back to
the embedded default (so awob always comes up *something*).

### `awob theme reload`

Reread the active theme's files. Manual trigger; the daemon also
auto-watches `scene.kdl` and any `import`-ed files.

### `awob version`

Print client + daemon version + protocol number.

## `awob.toml`

Optional config file at `$XDG_CONFIG_HOME/awob/awob.toml` (or
`--config <path>`). All keys optional; CLI flags win.

```toml
theme = "default"                             # active theme name
themes_dir = "~/.config/awob/themes"          # theme directory root
socket = "$XDG_RUNTIME_DIR/awob.sock"         # IPC socket path

[supervisor]
auto = true                                   # default; auto-discover known listeners
disable = ["upower"]                          # opt-out names from KNOWN_LISTENERS

[[listeners]]                                 # explicit listeners; merged with auto
name = "wob-fifo"                             # explicit entries with the same name as
command = "awob-listener-wob"                 # an auto-discovered one win
args = ["--fifo", "$XDG_RUNTIME_DIR/wob.sock"]
restart = "always"                            # always | on-failure | never
```

`$VAR` and `~/` in `themes_dir`, `socket`, `[[listeners]].args`, and
`[[listeners]].env` values are expanded.

### Auto-discovery

With `[supervisor] auto = true` (the default), the daemon walks an
internal `KNOWN_LISTENERS` registry on startup and spawns any whose
binary is present. The registry today:

| Name | Binary |
|---|---|
| `pipewire` | `awob-listener-pipewire` |
| `upower` | `awob-listener-upower` |
| `backlight` | `awob-listener-backlight` |
| `keyboard-backlight` | `awob-listener-keyboard-backlight` |

Listeners that need arguments (e.g. `awob-listener-wob`'s `--fifo`)
are deliberately *not* in the registry — add them via `[[listeners]]`.

Lookup order: the directory containing `awob-daemon`, then `$PATH`.
Dev workflows that run from `target/release` therefore find sibling
listener binaries automatically.

To disable a single auto entry: `disable = ["pipewire"]`. To skip
auto-discovery entirely: `auto = false`.

## Preempt semantics

`SendPayload.preempt: bool` (default `false`) controls how a send
interacts with an already-visible OSD.

* **Same `(source, event)` as currently displayed** → continuity update
  regardless of `preempt`. Bar tweens to the new value, fade alpha
  doesn't restart, the show timer resets.
* **Different `(source, event)`, `preempt: true`** → hot-swap. The
  visible OSD is replaced immediately; bar value continues from its
  current interpolated position to the new target.
* **Different `(source, event)`, `preempt: false`** → queued in a
  single-slot, last-write-wins buffer. When the active OSD reaches
  `Phase::Done` (after fade-out), the queued send drains as a fresh
  cycle.

Listener defaults wired in: PipeWire, backlight, keyboard-backlight,
and the wob FIFO shim send `preempt=true`. UPower keeps the default
`false`. The CLI's default is `false`; pass `--preempt` to mark a
script-driven send interactive.

## Migrating from wob

You can run alongside the wob binary or replace it outright. Both
paths look the same from a script's point of view: write `<value>`
or `<value> <style>` lines to a FIFO.

1. **Install awob and the wob compatibility listener:**

   ```sh
   cargo install --path crates/awob-daemon
   cargo install --path crates/awob-cli
   cargo install --path crates/awob-listener-wob
   ```

2. **Switch your scripts' FIFO target unchanged.** wob's default FIFO
   is whatever you configured; the awob shim defaults to
   `$XDG_RUNTIME_DIR/wob.sock`. Override with `--fifo <path>` if you
   want a different location.

3. **Pick the wob theme.** The repo ships `themes/wob/` — a
   pixel-faithful clone of upstream wob's default look (black
   background, white border, 400×50, 1000ms timeout, no fade
   animations). Geometry matches `config.c` defaults.

4. **Configure** (`~/.config/awob/awob.toml`):

   ```toml
   theme = "wob"

   [[listeners]]
   name = "wob-fifo"
   command = "awob-listener-wob"
   args = ["--fifo", "$XDG_RUNTIME_DIR/wob.sock"]
   ```

5. **Start the daemon:**

   ```sh
   awob-daemon
   ```

   Auto-discovery is on, so the PipeWire / UPower / backlight
   listeners spawn too if their binaries are installed. Disable any
   you don't want with `[supervisor] disable = […]`.

6. **Stop the wob daemon** (`pkill wob`) once you're satisfied awob
   is reading the same FIFO.

Wob's `<value> <style>` line format passes through unchanged — the
shim parses it and forwards to the daemon as a structured `Send`
with `event="wob"` (override with `--event`) and the style name.

When you want to graduate beyond pixel-faithful, switch to the
`default` or `tinct` theme and pull in the typed listeners — your
existing scripts can keep writing the FIFO while volume keys, etc.
are handled by `awob-listener-pipewire`.

## Hot reload

The daemon watches:

* The active theme's `scene.kdl`.
* Every file inlined via `import` (transitive — including
  `_palettes/*.kdl`).

Edit any of those, save, and the next OSD render uses the new theme.
External palette generators (e.g.
[tinct](../../tinct/)) take advantage of this — they refresh
`_palettes/<name>.kdl` and every consuming theme picks it up.

## Troubleshooting

| Symptom | Likely cause |
|---|---|
| No OSD appears | Daemon not running, or compositor doesn't support `wlr-layer-shell-v1`. Check `awob version`. |
| `connection refused` from the CLI | Daemon's socket missing. Default `$XDG_RUNTIME_DIR/awob.sock`; check daemon stderr for the path it's actually listening on. |
| Theme didn't change after edit | The watcher is set up after the first successful theme load. If the initial load failed, no files are watched — fix the parse error and `awob theme reload`. |
| Listener not spawning | `which awob-listener-<name>` — auto-discovery only finds binaries on `$PATH` or in the daemon's own dir. Check `[supervisor] disable`. |
| `image-missing-symbolic` glyph showing | Icon name didn't resolve. Theme can override by shipping `icons/image-missing-symbolic.svg` in its directory; see [`THEMES.md`](THEMES.md#icons). |
| Old `wob` daemon also running | Two processes on the same FIFO will race. `pkill wob`. |
