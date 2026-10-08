---
title: Themes
---

# Theme author guide

Themes are small directories named `<themes-root>/<name>/`. The daemon
searches an ordered list of roots — first root containing the theme
wins:

1. `--themes-dir` on the daemon command line, then `themes_dir` from
   `awob.toml` (both optional; prepended to the defaults below, not
   replacing them)
2. `~/.config/awob/themes` (`$XDG_CONFIG_HOME/awob/themes`)
3. `~/.local/share/awob/themes` (`$XDG_DATA_HOME/awob/themes`)
4. `<dir>/awob/themes` for each `$XDG_DATA_DIRS` entry — by default
   `/usr/local/share/awob/themes` and `/usr/share/awob/themes`, which
   is where distro packages install the stock themes

Switch active theme at runtime with `awob theme set <name>`.

To customise a stock theme, copy its directory into
`~/.config/awob/themes/<name>` — it shadows the packaged copy by name.
Imports resolve relative to the copy you're editing, so if the theme
imports from `../_palettes/` (most stock themes do), copy `_palettes/`
alongside it or inline the palette.

## Directory layout

```
~/.config/awob/themes/
├── _palettes/                 ← shared palettes (optional, opt-in)
│   └── tinct.kdl
├── default/
│   ├── scene.kdl              ← required: the theme's scene definition
│   ├── manifest.toml          ← optional: metadata for tooling
│   └── icons/                 ← optional: override system icons
│       ├── image-missing-symbolic.svg
│       └── audio-volume-high.svg
├── minimal/
│   └── scene.kdl
└── wob/
    └── scene.kdl
```

The theme loader treats every subdirectory of a themes root that
contains a `scene.kdl` as a candidate theme. `_palettes/` doesn't
have one and is naturally skipped — the leading underscore is a
visual hint, not a parser rule. `awob theme list` merges all roots,
with earlier roots shadowing later ones by name.

## `scene.kdl`

[KDL](https://kdl.dev) document. Structure:

```kdl
import "../_palettes/tinct.kdl"   // optional; pulls in palette + styles

palette { … }                      // optional if importing a palette
styles  { … }                      // optional; named accent overrides

surface { … }                      // surface geometry + animation timeline

scene {
    rect  …
    text  …
    image …
    bar   …
}
```

### `surface { … }`

| Key | Default | Notes |
|---|---|---|
| `width <px>` | `360` | |
| `height <px>` | `64` | |
| `anchor "<edge>"` | `"bottom"` | One of `top` `top-left` `top-right` `left` `center`/`centre` `right` `bottom-left` `bottom` `bottom-right` |
| `offset <x> <y>` | `0 -56` | Pixel offset from the anchor edge. Sign convention follows margin direction. |
| `margin <top> <right> <bottom> <left>` | `0 0 0 0` | Alternative to `offset`. |
| `fade-in "<ms>"` | `"150ms"` | Alpha fade-in duration. |
| `show "<ms>"` | `"2000ms"` | Settled display duration. |
| `fade-out "<ms>"` | `"150ms"` | Alpha fade-out duration. |
| `transition "<ms>"` | `"300ms"` | Bar value tween duration, sequenced *after* `fade-in`. |

A send's `--timeout <ms>` overrides `show` for that one cycle. The
total visible window is `fade-in + show + fade-out`.

Fades and value transitions render at up to 60 fps; element animations render at
up to 30 fps once the value transition finishes. Frames follow compositor
callbacks, so an obscured or inactive surface may update less often. Durations
still use elapsed time: throttling does not extend the display timeout. Static
content is not redrawn until it changes or fade-out starts.

### `palette { … }`

Named colours. Any CSS-syntax colour string parses (`#hex`, `#rgba`,
`rgba(…)`, named).

```kdl
palette {
    bg     "rgba(28,28,35,0.85)"
    fg     "#f3e8d7"
    accent "#baea96"
}
```

### `styles { … }`

Named style blocks that override individual bindings. Apply via
`awob send --style <name>` or via the `payload.style` field. The
default style is `"normal"`.

```kdl
styles {
    style "low"      accent="$low"
    style "normal"   accent="$normal"
    style "warn"     accent="$warn"
    style "critical" accent="$crit"
    style "muted"    accent="$crit" alpha="0.6"
    style "overflow" bg="$overflow_bg" accent="$overflow_accent"
}
```

#### Overflow auto-style

The daemon auto-applies `style="overflow"` whenever an incoming
`SendPayload` has `value > max` — useful for "volume above 100 %"
indicators, etc. Sender-supplied styles are ignored in that case;
the bar always renders in the overflow look on overflow.

The convention is to ship `overflow_*` palette entries alongside
the regular palette and an `overflow` style that maps them
through:

```kdl
palette {
    bg              "rgba(28,28,35,0.85)"
    fg              "#f3e8d7"
    track           "rgba(255,255,255,0.08)"
    crit            "#dc8855"

    // Overflow defaults: same surface bg, critical-coloured accent
    // so the visual reads as "past the limit". Override either
    // independently for a more dramatic overflow look.
    overflow_bg     "rgba(28,28,35,0.85)"
    overflow_accent "#dc8855"
}

styles {
    style "overflow" bg="$overflow_bg" accent="$overflow_accent"
}
```

The wob theme additionally defines `overflow_border` + `overflow_bar`
to mirror upstream wob's three-knob overflow colours
(`overflow_background_color` / `overflow_border_color` /
`overflow_bar_color`); see `themes/wob/scene.kdl` for the full
example.

Themes that don't define an `overflow` style block silently fall
back to the base style on overflow — no breakage, just no visual
indication. Add the block when you want overflow handling.

### Elements (in `scene { … }`)

Every element accepts these common attributes:

| Attribute | Notes |
|---|---|
| `z=<int>` | Stacking order. Higher renders on top. Default `0`. |
| `x=<expr>` `y=<expr>` | Position in pixels or `%` of the surface. `"center"` valid for `y`. |
| `anchor="<edge>"` | Per-element anchor; same values as `surface.anchor`. |

#### `rect`

| | |
|---|---|
| `width=<expr>` `height=<expr>` | Required. Accepts `%` (of surface), arithmetic (`100%-60`), bindings. |
| `fill="<colour-expr>"` | Solid fill. Defaults to surface accent. |
| `stroke="<colour-expr>"` `stroke-width=<expr>` | Optional outline. |
| `radius=<expr>` | Corner radius in pixels. `999` for fully rounded. |
| `shadow="<x> <y> <blur> <colour>"` | Drop shadow (e.g. `"0 8 24 rgba(0,0,0,0.4)"`). Cached per (w,h,blur). |

#### `text`

| | |
|---|---|
| `value="<template>"` | Required. Interpolation + expressions allowed (`"{$app ?? label($event)}"`). |
| `font="<family> <size> <weight>"` | e.g. `"Inter 14 500"`. |
| `colour="<colour-expr>"` | Defaults to `$fg`. American spelling `color="…"` accepted as alias. |
| `max-width=<expr>` | Truncates with ellipsis. |

#### `image`

| | |
|---|---|
| `src="<icon-expr>"` | Required. Freedesktop icon name, absolute path, or `data:` URI. |
| `width=<expr>` `height=<expr>` | Required. Image is fit-scaled. |
| `colour="<expr>"` | Tint behaviour. See [Icons](#icons) below. `color="…"` aliased. |

#### `bar`

| | |
|---|---|
| `width=<expr>` `height=<expr>` | Required. |
| `value=<expr>` | Required. Per-frame interpolated value (the daemon writes this). |
| `min=<expr>` `max=<expr>` | Defaults to `0` and `$max`. |
| `from=<expr>` | Wedge anchor. Default `"{$lastValue ?? $value}"`. When `from < value`, the segment between renders in the transition tint. |
| `fill="<colour-expr>"` | Bar colour. Defaults to `$accent`. |
| `radius=<expr>` | Corner radius. |
| `transition="<percent>"` | Transition wedge tint. Default `-80%`. Negative = darker; positive = brighter. Lerps to `0%` over `surface.transition` so the wedge fades into the bar by the time it settles. Accepts `"-80%"`, `"40%"`, `"-0.8"`, `"0.4"`. |
| `cells=<int>` `gap=<px>` | Render the bar as N discrete cell blocks separated by `gap` pixels (default 2) instead of one continuous fill. The cell at the progress boundary renders at fractional width so animation stays smooth. Wedge is disabled in cell mode. See `themes/console/` for an example. |

## Bindings

Each render frame, the daemon writes these into the bindings table.
Reference them as `$name` in attribute expressions.

| Binding | Source | Type |
|---|---|---|
| `$event` | `payload.event` | string |
| `$value` | per-frame interpolated current value | number |
| `$max` | `payload.max` (default `100`) | number |
| `$progress` | `(value-min)/(max-min)` | number |
| `$lastValue` | history entry, or `Null` if none | number / null |
| `$lastMax` | history entry, or `Null` | number / null |
| `$delta` | `value - lastValue`, or `0` | number |
| `$direction` | `"up"` / `"down"` / `"flat"` | string |
| `$valueAge` | seconds since last update for this `(source, event)` | number |
| `$app` | `payload.app`, or `Null` | string / null |
| `$icon` | `payload.icon`, or `Null` | string / null |
| `$style` | `payload.style`, or `Null` | string / null |
| `$accent` | resolved from style block + `payload.accent` | colour / string |
| `$transitionProgress` | `0.0`–`1.0`, position within `surface.transition` | number |

## Expression language

Attribute values are templates with `{interpolation}` segments.
Each segment evaluates an expression:

```
ternary  = coalesce ('?' expr ':' expr)?
coalesce = compare ('??' compare)*
compare  = add (('=='|'!='|'<'|'<='|'>'|'>=') add)?
add      = mul (('+'|'-') mul)*
mul      = unary (('*'|'/'|'%') unary)*
unary    = ('-' | '!')? primary
primary  = NUMBER | STRING | '$' IDENT | IDENT '(' args? ')' | '(' expr ')'
```

### Builtins

| Call | Returns |
|---|---|
| `icon(<event>)` | Default freedesktop icon name for an event (`"volume"` → `"audio-volume-high"`, `"battery"` → `"battery"`, …). |
| `label(<event>)` | Default human label for an event (`"volume"` → `"Volume"`). |
| `clamp(v, lo, hi)` | Clamp a number to `[lo, hi]`. Reversed or NaN bounds return an expression error; equal and infinite bounds are allowed. |
| `lerp(a, b, t)` | Linear interpolation `a + (b - a) * t`. |
| `min(a, b, …)` `max(a, b, …)` | Min/max of any number of arguments. |
| `int(v)` | Truncate toward zero (drop fractional part). Use for percent readouts: `"{int($progress * 100)}%"`. |
| `round(v)` | Round to nearest integer. |
| `upper(s)` `lower(s)` | ASCII / Unicode case fold. |
| `capitalize(s)` | Uppercase the first character, leave the rest unchanged. |
| `truncate(s, n)` `truncate(s, n, suffix)` | Truncate to `n` Unicode code points, appending `suffix` (default `"…"`) if anything was cut. Useful for monospace labels: `"{upper(truncate($app ?? label($event), 8))}"`. |

### Operators

* **`??`** — null-coalesce. Returns the first non-null operand.
  Idiomatic for `value="{$app ?? label($event)}"` and
  `from="{$lastValue ?? $value}"`.
* **`?:`** — ternary. `condition ? a : b`.

### Examples

```kdl
text  z=1 value="{$app ?? label($event)}" font="Inter 14 500" colour="$fg"
image z=1 src="{$icon ?? icon($event)}" x=14 y="center" width=22 height=22
rect  z=1 x=46 y=42 width="100%-60" height=8 radius=999 fill="$track"
bar   z=2 x=46 y=42 width="100%-60" height=8 radius=999 \
    fill="$accent" min=0 max="$max" value="$value" \
    from="{$lastValue ?? $value}"
```

## Icons

Icon resolution order, for an `image src="<name>"`:

1. **`<theme-dir>/icons/<name>.svg`** (or `.png`). Theme-supplied
   override. Per-theme — coexisting themes in different directories
   never collide.
2. **System freedesktop icon themes** — the preferred theme
   (`$AWOB_ICON_THEME` / `$GTK_THEME` / gsettings), then Adwaita and
   hicolor, across `$XDG_DATA_HOME/icons`, `$XDG_DATA_DIRS/icons`, and
   `/usr/share/icons`. Both size-first (`24x24/status/`) and
   category-first (`status/24/`, the breeze family) layouts are probed,
   then legacy flat pixmaps dirs (`/usr/share/pixmaps`) as a last
   resort.
3. **Recurse with `image-missing-symbolic`** if `<name>` couldn't be
   resolved and isn't already that name. This gives themes a chance
   to ship their own missing-icon glyph (`icons/image-missing-symbolic.svg`).
4. **Embedded fallback SVG** compiled into the daemon binary. Last
   resort.

Symbolic icons (path contains `symbolic/` or filename ends
`-symbolic`) are auto-tinted to `$fg`. Multicolour app icons stay as
authored. Override per-element:

| `colour="…"` value | Behaviour |
|---|---|
| unset | Auto-tint if symbolic, else preserve original. |
| `"$fg"`, `"#ff00aa"`, etc. | Flat-tint to that colour (overrides auto). |
| `"auto"` / `"none"` | Never tint, even if symbolic. |

## Palettes: inline, imported, or both

A theme can declare its colours three ways. The choice is the theme
author's — there is no precedence rule based on *location*; the
parser just walks the file top-to-bottom and **whichever palette
entry is processed last wins, key by key**.

| Pattern | When to use |
|---|---|
| Inline `palette { … }` only | Standalone, single-file theme. No external dependency. |
| `import "../_palettes/X.kdl"` only | Theme that wants the shared palette as-is. Generator-managed (e.g. [tinct](https://github.com/jmylchreest/tinct)) or reused by multiple themes. |
| Import **plus** inline `palette { … }` | Pull in the shared base, then override a few keys. Idiomatic order: `import` first, then a local block with the tweaks. |

Concrete merge behaviour:

```kdl
import "../_palettes/tinct.kdl"     # tinct's accent = #5fff5f
palette { accent "#ff0000" }         # local block runs AFTER, wins for `accent`
# → accent = #ff0000, every other tinct key untouched
```

Reverse the order and the import wins:

```kdl
palette { accent "#ff0000" }
import "../_palettes/tinct.kdl"     # this runs after, overwrites accent
# → accent = #5fff5f
```

Same rule applies to `styles { … }` blocks: later declarations win.

### Why the `_palettes/` directory at all?

It's a convention, not a parser rule. Three reasons it earns its
keep when you're doing more than a one-off theme:

* **Cross-theme reuse.** `default` and `minimal` both want the
  tinct palette; one file, two consumers.
* **Generator-friendly.** Tools like
  [tinct](https://github.com/jmylchreest/tinct) regenerate `_palettes/<name>.kdl`
  in place; the daemon's hot-reload watcher follows imports
  transitively, so every consuming theme picks up the change with
  no daemon restart.
* **Separation of concerns.** Layout lives in `<theme>/scene.kdl`,
  colour lives in `_palettes/<name>.kdl`. Swap one without
  touching the other.

The leading underscore is purely a visual hint that the directory
isn't a theme — the loader skips any subdirectory of `themes_dir`
that lacks a `scene.kdl`, regardless of name.

## `manifest.toml`

Currently a **convention only** — the awob daemon doesn't parse it.
Useful for theme repositories, package managers, future browsers.
Suggested fields, matching `themes/default/manifest.toml`:

```toml
name = "default"
description = "Built-in default theme. Embedded in awob-daemon as the fallback."
author = "awob"
version = "0.0.1"

[layout]
template = "scene.kdl"

[icons]
volume        = "audio-volume-high"
volume-low    = "audio-volume-low"
volume-medium = "audio-volume-medium"
volume-muted  = "audio-volume-muted"
brightness    = "display-brightness"
mic           = "microphone-sensitivity-high"
battery       = "battery"
```

If the daemon ever grows a theme browser or `awob theme list` with
metadata, this is what it'll consume.

## Worked example: minimal theme

```kdl
// themes/minimal/scene.kdl

import "../_palettes/tinct.kdl"

surface {
    width 240
    height 6
    anchor "bottom"
    offset 0 -32
    fade-in  "120ms"
    show     "900ms"
    fade-out "240ms"
}

scene {
    rect z=0 x=0 y=0 width="100%" height="100%" radius=3 fill="$track"
    bar  z=1 x=0 y=0 width="100%" height="100%" radius=3 \
        fill="$accent" \
        min=0 max="$max" value="$value" from="{$lastValue ?? $value}"
}
```

A 240×6 ribbon at the bottom of the screen. No icon, no label, just
the bar value. Useful if you want a wob-shaped slice of an OSD.


## Resource limits

Scene files, imports, and forced palette files must be regular UTF-8 files of at
most 1 MiB each. Symlinks to regular files work. A complete load, including a
forced palette and its imports, allows 4 MiB of source, 64 imports, 16 import
levels, and 10,000 KDL nodes (including ignored nodes). KDL child blocks, nested
block comments, and consecutive slashdash markers are limited to 32 levels.
These checks run before parsing. A failed reload keeps the current theme active.

Each expression inside `{…}` allows at most 16 KiB of source and 128 unquoted
operator or delimiter bytes (`()?:+-*/%!<>=,`). Every byte counts separately,
including both characters of `??` or `<=`; punctuation in quoted string literals
does not count. Expressions exceeding either limit fail theme loading. These
limits cover nested expressions and long arithmetic chains, whose parsed trees
also recurse during evaluation and cleanup. They do not shorten literal labels
or inline icons outside interpolation; the existing text and asset limits below
still apply. Manually constructed Rust `Expr` trees are outside this parser guard.

Raster surfaces, PNG sources and icon targets accept axes from 1 to 8,192 pixels
and at most 8,388,608 pixels. PNG decoded output is additionally limited to
32 MiB, with an 8 MiB decoder scratch budget. Shadow masks use the same axis and
pixel limits after blur padding; a mask uses at most 8 MiB plus a same-sized blur
scratch buffer. These limits apply before allocation and are separate from cache
budgets. They do not impose a total process memory limit.

Surface dimensions must be positive integers. Invalid dimensions reject the theme;
a failed reload leaves the previous theme active. Rejected icons use the usual
placeholder. Dynamic shadow geometry over the budget fails that render rather
than changing its shape.

Each rendered text label is limited to 16 KiB of UTF-8 and a finite font size greater
than zero and no more than 512 pixels. Exceeding either limit fails the render;
labels are not silently truncated. Applications using the core text API must handle
errors from `TextRenderer::measure` and `TextRenderer::draw`.


### SVG image resources

Icons support plain SVG and PNG. JPEG, GIF, WebP and gzip-compressed SVG are not
accepted, including images embedded in SVG. Inline MIME types must be exactly
`image/svg+xml` or `image/png` (optionally followed by `;base64`).

Files are limited to 1 MiB and must resolve to regular files; symlinks to regular
files are allowed. Inline decoded data is limited to 256 KiB. SVG image references
share a 2 MiB source-data budget, at most 64 references, four nested SVG levels,
and a 32 MiB cumulative decoded PNG budget per root icon. Repeated references
count again. These limits supplement the raster dimensions above.

Top-level SVGs may reference external PNG/SVG files. Relative paths keep their
existing resolution against the daemon's working directory. Nested SVGs can embed
inline images but cannot load external files, following the SVG specification.
A rejected resource makes the whole icon use the normal placeholder rather than
cache a partial image. FIFOs, devices and directories cannot be icon inputs.
