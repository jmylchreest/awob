#!/usr/bin/env bash
# Value-transition wedge demo.
#
# Shows the bar's wedge overlay during growth, the wedge-free
# behaviour during shrink, and the wedge anchor shifting on rapid
# successive sends.

set -eu
SCRIPT_NAME="wedge"
. "$(dirname "$0")/lib.sh"
require_daemon
SRC="$(srcid)"

say "A: large GROW (0.30 → 0.85)"
note "Dark wedge between old and new value, fading to bar colour over"
note "the surface.transition window (default 300ms)."
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-low volume 0.30 1.0
sleep 2.4
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-high volume 0.85 1.0
sleep 3.0

say "B: large SHRINK (0.85 → 0.15)"
note "Bar contracts smoothly. No wedge — wedge only renders on growth"
note "(no ghost above the new level)."
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-low volume 0.15 1.0
sleep 3.0

say "C: tiny grow (0.15 → 0.20)"
note "Wedge is short but visible during the early transition window."
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-low volume 0.20 1.0
sleep 3.0

say "D: rapid grows (0.20 → 0.45 → 0.65 → 0.80)"
note "Each new send shifts the wedge anchor (\$lastValue) forward;"
note "bar tracks the rising target without restarting the fade-in."
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium volume 0.45 1.0
sleep 0.18
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium volume 0.65 1.0
sleep 0.18
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-high volume 0.80 1.0
sleep 3.0

say "DONE"
