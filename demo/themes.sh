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

# Restore. Pick "default" as a safe target — the originally-active
# theme isn't recoverable from here.
say "Restoring theme to default"
"$AWOB" theme set default || true

say "DONE"
