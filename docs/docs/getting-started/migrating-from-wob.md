---
sidebar_position: 6
title: Migrating from wob
---

# Migrating from wob

awob ships a wob-format FIFO compatibility shim
(`awob-listener-wob`) so you can swap the daemon without changing
any of your existing scripts.

## TL;DR

1. **Install** awob and the wob shim:

   ```sh
   cargo install --path crates/awob-daemon
   cargo install --path crates/awob-cli
   cargo install --path crates/awob-listener-wob
   ```

2. **Pick the wob theme** — a pixel-faithful clone of wob's default
   look (black background, white border, 400×50, 1000ms timeout).

3. **Configure** `~/.config/awob/awob.toml`:

   ```toml
   theme = "wob"

   [[listeners]]
   name = "wob-fifo"
   command = "awob-listener-wob"
   args = ["--fifo", "$XDG_RUNTIME_DIR/wob.sock"]
   ```

4. **Stop wob, start awob**:

   ```sh
   pkill wob
   awob-daemon       # or `systemctl --user start awob.service`
   ```

5. Your existing scripts keep working. They write `<value>` or
   `<value> <style>` to the same FIFO; the shim parses the line and
   forwards it as a structured `Send` to the daemon.

## What carries over

* **FIFO format**: `<value>` and `<value> <style>` lines work as-is.
* **Style names**: wob's `low`, `normal`, `critical`, `muted` are
  all defined in awob's wob theme.
* **Geometry**: 400×50 surface, anchored centre, 1000ms timeout.
  Match wob's `config.c` defaults.

## What changes

* **No more daemon restart on theme change.** awob has IPC, so
  `awob theme set <name>` swaps themes without losing the FIFO
  connection.
* **Hot-reload of the theme file.** Edit
  `~/.config/awob/themes/wob/scene.kdl` and the next OSD picks up
  the new layout / colours immediately.
* **Daemon supervises the shim.** If the FIFO listener crashes, the
  awob supervisor respawns it with capped exponential backoff.
* **Multiple sources without daemon-per-event.** If you also want
  battery / brightness OSDs, just install the relevant listeners
  alongside — auto-discovery handles them. Your wob FIFO stays
  unchanged.

## What's *not* carried over

* **`-W <width>` / `-H <height>` / `--background-color`** etc. CLI
  flags. awob's geometry and colour live in the theme, not on the
  command line. Edit `themes/wob/scene.kdl` for these.
* **Per-instance daemon**. wob's pattern was "one daemon per
  user-source"; awob is one daemon per session, with multiple
  listeners sharing it.

## Graduating beyond pixel-faithful

When you're ready to step beyond the wob look, switch to one of the
richer stock themes:

```sh
awob theme set default --persist
# or
awob theme set tinct --persist        # if you've installed tinct
awob theme set console --persist      # ANSI-green cell-block bar
```

Your wob FIFO scripts keep working — the wob listener just emits
`event="wob"` sends, which the new theme renders with whatever
layout it defines.

## Side-by-side test

If you'd rather not commit until you've seen awob render your
content, run them on different FIFOs for a bit:

```sh
# Keep wob on its FIFO.
wob -i ~/.config/wob/config &

# Run awob on a different FIFO and a separate keybind.
awob-daemon &
awob-listener-wob --fifo /tmp/awob-trial &

# Some volume-up keybind that fans out to both:
echo "$NEW_VOL" > ~/.config/wob/wob.sock
echo "$NEW_VOL" > /tmp/awob-trial
```

Once you're happy, point your scripts at one FIFO and shut the
other daemon down.
