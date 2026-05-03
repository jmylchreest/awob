#!/usr/bin/env bash
# Theme cycle demo.
#
# Iterates every directory under ./themes/ that contains a scene.kdl,
# switches the daemon to it (in-memory), and fires a representative
# pair of sends so you can compare looks back-to-back.

set -eu
SCRIPT_NAME="themes"
. "$(dirname "$0")/lib.sh"
require_daemon

THEMES_DIR="$(dirname "$0")/../themes"
SRC="$(srcid)"

# Find theme dirs by `scene.kdl` presence so we skip _palettes/.
mapfile -t THEMES < <(
    find "$THEMES_DIR" -mindepth 2 -maxdepth 2 -name scene.kdl \
        | sed 's|/scene.kdl$||' \
        | xargs -n1 basename \
        | sort
)

if [ "${#THEMES[@]}" -eq 0 ]; then
    echo "demo: no themes found under $THEMES_DIR" >&2
    exit 2
fi

note "Found themes: ${THEMES[*]}"

for THEME in "${THEMES[@]}"; do
    say "theme: $THEME"
    "$AWOB" theme set "$THEME"
    sleep 0.3

    "$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium --app "Speakers" volume 0.40 1.0
    sleep 2.5
    "$AWOB" send --preempt --source "$SRC" --icon audio-volume-high --app "Speakers" volume 0.85 1.0
    sleep 3.0
done

# Switch to default before the force-palette tests. The wob theme's
# bar uses fill="$bar" (its own palette key) rather than fill="$accent",
# so a force-palette overlay that only redefines accent colours
# wouldn't reach the bar — wob is intentionally pixel-faithful and
# stays white. The default theme uses $accent throughout, so the
# overlay's hot-pink/cyan/lemon accents land on the bar visibly.
say "switching to default theme for the force-palette tests"
"$AWOB" theme set default
sleep 0.4

# Force-palette overlay live demo. The 'random' palette in
# themes/_palettes/random.kdl is deliberately gaudy (hot pink, cyan,
# lemon) so the overlay is visually obvious. We toggle it via
# `awob force-palette set/clear`, fire several events across
# different styles + values, then turn it off.
say "force-palette: ON"
RANDOM_PALETTE="$(realpath "$(dirname "$0")/../themes/_palettes/random.kdl")"
"$AWOB" force-palette set "$RANDOM_PALETTE"
sleep 0.4
note "every send below uses the gaudy overlay — pink/cyan/lemon."
for V in 0.20 0.55 0.85; do
    "$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium \
        --app "FORCE-PALETTE: random" volume "$V" 1.0
    sleep 1.6
done
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-muted \
    --app "FORCE-PALETTE + critical style" --style critical mute 1 1
sleep 1.6
"$AWOB" send --preempt --source "$SRC" --icon battery-low \
    --app "FORCE-PALETTE + warn style" --style warn battery 0.20 1.0
sleep 1.6

say "hot-redraw-while-visible — overlay change repaints the live OSD"
note "long-timeout OSD with random overlay, clear overlay mid-show,"
note "watch the visible OSD repaint with the theme's native palette."
"$AWOB" send --preempt --source "$SRC" --timeout 4000 \
    --icon audio-volume-medium --app "RANDOM overlay (4s show)" volume 0.45 1.0
sleep 1.5
note "clearing force-palette while OSD is still visible..."
"$AWOB" force-palette clear
sleep 4.0

say "force-palette: OFF (back to theme's native palette)"
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-high \
    --app "Native palette restored" volume 0.85 1.0
sleep 3.0

# Hot-redraw via theme switch — same idea, switching the whole
# theme rather than just the palette. The visible OSD repaints
# instantly with the new theme.
say "hot-redraw-while-visible — theme switch repaints the live OSD"
"$AWOB" theme set default
"$AWOB" send --preempt --source "$SRC" --timeout 4000 \
    --icon audio-volume-medium --app "DEFAULT theme (4s show)" volume 0.55 1.0
sleep 1.5
note "switching to console theme while default OSD is still visible..."
"$AWOB" theme set console
sleep 4.0

# Restore. Pick "default" as a safe target — the originally-active
# theme isn't recoverable from here.
say "Restoring theme to default"
"$AWOB" theme set default || true

say "DONE"
