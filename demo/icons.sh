#!/usr/bin/env bash
# Icon resolution demo.
#
# Walks through the resolver's chain:
#   1. Real freedesktop name → system icon theme
#   2. Bogus name → image-missing-symbolic (theme override if present)
#   3. Bogus name when theme has no override → embedded SVG fallback
#
# To force the embedded fallback, run with --no-system-icons (env
# AWOB_NO_SYSTEM_ICONS=1 is not currently respected; this script
# notes the limitation and only demonstrates 1+2).

set -eu
SCRIPT_NAME="icons"
. "$(dirname "$0")/lib.sh"
require_daemon
SRC="$(srcid)"

say "A: real freedesktop icon (audio-volume-high)"
note "Resolves via system Adwaita / hicolor."
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-high volume 0.7 1.0
sleep 3.0

say "B: bogus icon name #1"
note "Falls through to image-missing-symbolic. If the active theme"
note "ships icons/image-missing-symbolic.svg, you'll see the theme's"
note "glyph; otherwise the system Adwaita picture-frame; otherwise"
note "the embedded ?-square."
"$AWOB" send --preempt --source "$SRC" --icon does-not-exist-12345 volume 0.5 1.0
sleep 3.0

say "C: bogus icon name #2 (consistency check)"
"$AWOB" send --preempt --source "$SRC" --icon zzzzz-not-a-real-icon volume 0.4 1.0
sleep 3.0

say "D: explicit image-missing-symbolic"
note "Skips the recursion step and goes straight to whatever the"
note "resolver finds for that exact name."
"$AWOB" send --preempt --source "$SRC" --icon image-missing-symbolic volume 0.6 1.0
sleep 3.0

say "DONE"
