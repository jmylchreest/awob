# awob: protocol + listener author guide

For end-user CLI / config see [`USAGE.md`](USAGE.md); for theme
authoring see [`THEMES.md`](THEMES.md). This document is for people
writing new event-source listeners or tools that talk to the daemon
directly.

## Transport

* **Unix stream socket** at `$XDG_RUNTIME_DIR/awob.sock` (override
  via `--socket <path>` on the daemon, `awob.toml`'s `socket` key,
  or `AWOB_SOCKET` env var on listeners).
* **JSON-lines.** One `Request` JSON object per line (`\n`-terminated)
  in; one `Response` JSON object per line out. UTF-8.
* **Synchronous, single-shot per line.** Send a request, read a
  response, repeat. No multiplexing, no streaming responses.
* `serde`-tagged enum: every JSON object has a `"type"` discriminator.

Connection lifetime is up to the client. Listeners typically hold a
long-lived connection; CLI invocations connect, send, disconnect.

## Protocol version

`PROTOCOL_VERSION` is currently `1`. Clients are expected to send a
`Hello` first (the daemon checks the version and returns
`Response::Hello { protocol, daemon_version }` or
`Response::Error { … }` on mismatch). The `awob-client` crate does
this automatically on `Client::connect()`.

## Requests

| Variant | Purpose |
|---|---|
| `Hello { protocol: u32 }` | Handshake. |
| `Send(SendPayload)` | Trigger an OSD. **Hot path.** |
| `Query { source: Option<String> }` | List history entries (optionally filtered by source). |
| `SetTheme { name: String, persist: bool }` | Switch active theme. With `persist`, also rewrites `awob.toml`. |
| `Reload` | Reread current theme files. |
| `Version` | Get daemon version + protocol number. |

## Responses

| Variant | Purpose |
|---|---|
| `Ok` | Generic success. |
| `Error { message: String }` | Error string. |
| `Hello { protocol: u32, daemon_version: String }` | Reply to `Hello`. |
| `Query { entries: Vec<HistoryEntry> }` | Reply to `Query`. |
| `Version { daemon_version: String, protocol: u32 }` | Reply to `Version`. |

`HistoryEntry`: `source`, `event`, `last_value`, `last_max`,
`age_seconds`, `listener_id`. One entry per `(source, event)` pair.

## `SendPayload` reference

This is the hot-path message. Field-by-field:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `event` | `String` | required | Free-form event name (`"volume"`, `"brightness"`, `"battery"`, `"mic"`, `"caps"`, …). The daemon doesn't check it against a list — it's whatever your listener wants. Influences default icon/label via the expression language's `icon($event)` / `label($event)` builtins. |
| `value` | `f64` | required | Current measurement. Units match `max`. |
| `max` | `f64` | `100.0` | Upper bound. The bar renders `(value - min) / (max - min)`. |
| `listener_id` | `Option<String>` | `None` | Stable identity for the listener *type* (e.g. `"awob-listener-pipewire"`). The daemon emits a duplicate-listener warning if it sees two distinct sources sharing one `listener_id`. CLI invocations leave this unset. |
| `source` | `Option<String>` | `None` | Per-instance identity (e.g. `"pipewire-7a3f"`). Together with `event`, forms the history key. Sends without `source` get no history → `$lastValue` is `Null`. |
| `style` | `Option<String>` | `None` | Named style block to apply (`"low"`, `"normal"`, `"warn"`, `"critical"`, `"muted"`, or any custom name). Defaults to `"normal"` at render time. |
| `accent` | `Option<String>` | `None` | One-off CSS-syntax colour override applied after style merge. |
| `app` | `Option<String>` | `None` | Free-form label. Available as `$app` in the theme. |
| `icon` | `Option<String>` | `None` | Icon name / path / `data:` URI. Available as `$icon`. |
| `timeout_ms` | `Option<u32>` | `None` | One-shot override for `surface.show` duration. |
| `preempt` | `bool` | `false` | See [Preempt semantics](#preempt-semantics). |

JSON shape (with all fields populated):

```json
{
    "type": "send",
    "event": "volume",
    "value": 0.7,
    "max": 1.0,
    "listener_id": "awob-listener-pipewire",
    "source": "pipewire-7a3f",
    "style": "normal",
    "accent": null,
    "app": "Speakers",
    "icon": "audio-volume-high",
    "timeout_ms": null,
    "preempt": true
}
```

## Preempt semantics

`preempt` controls how a send interacts with an OSD that's already
on screen:

* **Same `(source, event)` as the visible OSD** → continuity update,
  regardless of `preempt`. Bar value tweens to the new target; alpha
  fade doesn't restart; show timer resets.
* **Different `(source, event)`, `preempt: true`** → hot-swap. The
  visible OSD is replaced immediately. Bar value continues from its
  current interpolated position to the new target so the swap is
  smooth.
* **Different `(source, event)`, `preempt: false`** → queued in a
  single-slot, last-write-wins buffer. When the active OSD reaches
  `Phase::Done` (after fade-out), the queued send drains as a fresh
  cycle.

When to set `preempt: true`:

* Volume keys (the user just pressed a button; show it now).
* Brightness keys.
* Mic mute.
* Anything user-initiated where queueing behind a battery bar would
  feel broken.

When to leave `preempt: false`:

* Battery state changes.
* Network connection events.
* Any ambient/background notification that shouldn't preempt
  whatever the user is currently doing.

## History keying

The daemon's history map is keyed by `(source, event)`. Distinct
events on the same source — e.g. `volume` then `mute` on a single
PipeWire node — don't cross-contaminate `$lastValue`. A listener
publishing both metrics for one source uses the same `source` value
with different `event` values:

```json
{ "type": "send", "event": "volume", "value": 0.6, "source": "speaker", "preempt": true }
{ "type": "send", "event": "mute",   "value": 1.0, "source": "speaker", "preempt": true, "icon": "audio-volume-muted" }
```

Each gets its own history slot.

## Building a listener

Long-running event source. Pattern:

1. Connect to the daemon socket (or accept `AWOB_SOCKET` env var; the
   supervisor sets it).
2. Subscribe to your upstream (D-Bus signal, sysfs notify, FIFO read,
   PipeWire registry, …).
3. On each upstream event, build a `SendPayload` and write a JSON
   line to the socket.
4. Reconnect on socket loss; the daemon's supervisor will respawn
   you on crash.

The simplest path is to depend on `awob-client`:

```toml
[dependencies]
awob-client = "0.0"
```

```rust
use awob_client::{Client, Send};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::connect()?;        // honours AWOB_SOCKET
    loop {
        let value = read_upstream()?;            // your event source
        let payload = Send::new("volume", value)
            .max(1.0)
            .source("my-source-id")
            .listener_id("awob-listener-mything")
            .icon("audio-volume-high")
            .app("My Source")
            .preempt(true)
            .auto_listener_id()                  // basename(argv[0]) if not set
            .build();
        client.send(payload)?;
    }
}
```

The `Send` builder validates field combinations and sets sensible
defaults. `auto_listener_id()` is a convenience that fills
`listener_id` with the basename of the running binary if you didn't
set it explicitly — used by all the bundled listeners so the daemon
can warn on duplicate processes.

### Choosing identifiers

* **`listener_id`** is per-binary or per-binary-variant. PipeWire's
  listener publishes `awob-listener-pipewire-speaker` and
  `awob-listener-pipewire-mic` from one process — they're "different
  listener types" semantically.
* **`source`** is per-physical-source. One sink in PipeWire = one
  source. Use a stable identifier (PipeWire object id, sysfs device
  name, UPower path); avoid PIDs — they change on respawn and break
  history continuity.

The daemon flags as duplicate when *two distinct sources share one
`listener_id`*. That signals two processes of the same listener
running concurrently — almost always a misconfiguration.

### Talking directly without `awob-client`

The wire format is small enough to drive from any language:

```sh
printf '%s\n' '{"type":"hello","protocol":1}' \
       '{"type":"send","event":"volume","value":50,"max":100,"preempt":true}' \
  | nc -U "$XDG_RUNTIME_DIR/awob.sock"
```

Replace `nc -U` with whatever Unix-socket plumbing your runtime
provides. Each output line is a `Response` JSON object.

## Auto-discovery + supervisor

If your listener's binary lands on `$PATH` or in the daemon's own
directory, and you give it a stable name, you can register it for
automatic spawning:

1. Add an entry to `KNOWN_LISTENERS` in
   `crates/awob-daemon/src/known_listeners.rs`:

   ```rust
   pub const KNOWN_LISTENERS: &[KnownListener] = &[
       …,
       KnownListener { name: "mything", binary: "awob-listener-mything" },
   ];
   ```

2. Make sure the binary works with no arguments. Listeners that
   need flags don't belong in the registry — they require explicit
   `[[listeners]]` blocks where the user provides args.

The daemon will then auto-spawn and supervise it on startup whenever
it's installed.

## Theme persistence

`SetTheme { name, persist: true }` rewrites the `theme` key in
`awob.toml` using `toml_edit`. Comments, key order, and unrelated
tables (`[supervisor]`, `[[listeners]]`, …) are preserved. The
config path used for the rewrite is whichever was loaded at startup
(`--config <path>` or the XDG default at
`$XDG_CONFIG_HOME/awob/awob.toml`).

If no config path is in effect, `SetTheme { persist: true }` returns
an `Error` saying so — the in-memory theme still changes.

## Cold-start fallback

If the configured theme can't be loaded at daemon start (missing
directory, parse error, etc.), the daemon warns to stderr and falls
back to the embedded default. This means awob always comes up with
*something* renderable, so the user can `awob theme set <real>` to
recover. There's no scenario where awob refuses to start because of
a bad theme.

## Stability

`PROTOCOL_VERSION = 1` is pre-1.0. The Request/Response shape is
likely stable (well-trafficked across all listeners + the CLI), but
adding fields to `SendPayload` is expected. Old clients omit
unknown fields → `serde(default)` makes them roundtrip safely. New
clients targeting old daemons should rely on `Hello` rejection
rather than feature-detection.
