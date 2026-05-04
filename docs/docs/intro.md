---
sidebar_position: 1
slug: /intro
title: What is awob?
---

# What is awob?

**awob** is **A**nother **W**ayland **O**verlay **B**ar — a small,
fast on-screen-display daemon for Wayland. When you press a volume
key, change brightness, mute the mic, or your battery dips into the
red, awob is the slim ribbon that appears on screen, shows the
current value, and quietly fades out.

It is a spiritual successor and drop-in replacement for the excellent
[wob](https://github.com/francma/wob) by Francesco Mariani. If wob
covers what you need, **use wob** — it's tiny, fast, battle-tested,
and shaped exactly right for what it does. awob owes its name and
FIFO format to wob.

## Why awob exists

Three things wob doesn't do that some users want:

* **Richer theming.** awob themes are KDL files describing a small
  scene tree (rect / text / image / bar) with bindings, an
  expression language, palette imports, and a transition timeline.
  See [Themes](/themes) for the full reference.
* **Typed IPC.** A Unix socket speaking JSON-lines instead of a
  one-way FIFO. Listeners send structured events with `event`,
  `value`, `source`, `style`, `icon`, etc. — easier to integrate
  with than parsing wob's positional `<value> [<style>]` lines.
  See [Protocol](/protocol).
* **An event-source listener ecosystem.** PipeWire, sysfs battery,
  sysfs backlight, keyboard backlight — auto-discovered and supervised.
  Plug in your own listener via the same socket protocol.

## What awob doesn't try to be

* **Cross-platform.** awob is Wayland-only. No X11, no macOS, no
  Windows. The renderer assumes `wlr-layer-shell-v1`.
* **A notification daemon.** OSDs and notification bubbles are
  different things. awob is the OSD; pair it with mako / dunst /
  swaync for notifications.
* **Tiny.** wob is ~25 KB stripped; awob's daemon is ~4.5 MB.
  That's the cost of cosmic-text + tiny-skia + resvg + the listener
  supervisor. If a 5 MB daemon offends you, use wob.

## Status

Pre-1.0. Wire format and theme schema may still change without a
deprecation cycle until 0.1.0. Pin a version in scripts.

## Where next

* [Install](/getting-started/install) — distro packages, building
  from source, the systemd user unit.
* [Quick start](/getting-started/quickstart) — get the OSD on screen
  and drive it from a script in five minutes.
* [Migrating from wob](/getting-started/migrating-from-wob) — keep
  your existing FIFO scripts working while gaining the rest of the
  ecosystem.
* [Themes](/themes) — write your own theme.
* [Protocol](/protocol) — write your own listener or driver.
