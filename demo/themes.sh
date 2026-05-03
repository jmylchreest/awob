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

# Force-palette overlay verification. The 'random' palette in
# themes/_palettes/random.kdl is deliberately gaudy (hot pink, cyan,
# lemon) so any theme with the overlay applied is visually obvious.
# To exercise this for real you'd start the daemon with
# `--force-palette themes/_palettes/random.kdl` (or set
# `force_palette = "..."` in awob.toml). The current daemon may not
# have it set; this section just sends OSDs against whatever
# overlay is in effect so a side-by-side run with/without is easy.
say "force-palette demo (the colours below depend on the daemon's overlay)"
note "with no overlay: theme's own palette."
note "with --force-palette themes/_palettes/random.kdl: hot pink + cyan + lemon."
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium --app "Speakers" volume 0.55 1.0
sleep 3.0

# Hot-reload during a visible OSD. Send a long-timeout OSD, switch
# the theme via IPC while it's still on screen, and immediately fire
# another OSD on the new theme. The first one keeps its theme until
# fade-out (current behaviour); the second one renders on the new
# theme.
say "hot-reload-while-visible — first OSD on theme A, theme switches mid-show, second OSD on theme B"
"$AWOB" theme set default
"$AWOB" send --preempt --source "$SRC" --timeout 4000 \
    --icon audio-volume-medium --app "DEFAULT theme (4s show)" volume 0.55 1.0
sleep 1.5
note "switching to console theme while default OSD is still visible..."
"$AWOB" theme set console
sleep 0.4
"$AWOB" send --preempt --source "$SRC" \
    --icon audio-volume-high --app "CONSOLE theme" volume 0.85 1.0
sleep 4.0

# Restore. Pick "default" as a safe target — the originally-active
# theme isn't recoverable from here.
say "Restoring theme to default"
"$AWOB" theme set default || true

say "DONE"
