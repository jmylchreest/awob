# Theme author guide

Themes are small directories under `~/.config/awob/themes/<name>/`
(or the path set by `themes_dir` in `awob.toml`). Switch active theme
at runtime with `awob theme set <name>`.

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

The theme loader treats every subdirectory of `themes_dir` that
contains a `scene.kdl` as a candidate theme. `_palettes/` doesn't
have one and is naturally skipped — the leading underscore is a
visual hint, not a parser rule.

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
}
```

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
| `clamp(v, lo, hi)` | Clamp a number to `[lo, hi]`. |
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
2. **System freedesktop icon themes** (Adwaita, hicolor, …) via the
   `freedesktop-icons` crate.
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
| `import "../_palettes/X.kdl"` only | Theme that wants the shared palette as-is. Generator-managed (e.g. [tinct](../../tinct/)) or reused by multiple themes. |
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
  [tinct](../../tinct/) regenerate `_palettes/<name>.kdl`
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
