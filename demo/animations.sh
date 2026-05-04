#!/usr/bin/env bash
# Animation engine demo.
#
# Switches to the `pulse-demo` theme (which has `pulse="true"` on the
# bar element) and fires a sequence of OSDs so the pulse animation is
# visible while the bar's settled. Switches back to the previous theme
# at the end so the demo doesn't change persistent state.

set -eu
SCRIPT_NAME="animations"
. "$(dirname "$0")/lib.sh"
require_daemon

PREV_THEME=$("$AWOB" theme get 2>/dev/null || echo "default")

cleanup() {
    "$AWOB" theme set "$PREV_THEME" 2>/dev/null || true
}
trap cleanup EXIT

say "switch to pulse-demo theme — bar will pulse alpha at 1Hz, 50% depth"
"$AWOB" theme set pulse-demo
sleep 1

say "ambient brightness OSD; pulse runs through the show window"
"$AWOB" send --icon display-brightness --app "Display" brightness 60 100
sleep 3.5

say "volume OSD with --show 5000 — 5 full pulse cycles visible"
"$AWOB" send --preempt --icon audio-volume-high --app "Speakers" \
    --show 5000 volume 75 100
sleep 6

say "critical-style battery — pulse is a natural fit for warnings"
"$AWOB" send --preempt --icon battery-empty --app "Battery: 4%" \
    --style critical --show 4000 battery 4 100
sleep 5

say "DONE — restoring previous theme: $PREV_THEME"
