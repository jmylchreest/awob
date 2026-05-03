#!/usr/bin/env bash
# Style override demo.
#
# Each `--style <name>` send applies a named style block from the
# active theme. The default theme's tinct palette ships:
#   low / normal / warn / critical / muted
# Each remaps `accent` (and `muted` adds an alpha mod) so the bar
# colour visibly changes between scenarios.

set -eu
SCRIPT_NAME="styles"
. "$(dirname "$0")/lib.sh"
require_daemon
SRC="$(srcid)"

say "A: normal (default accent — green-on-tinct)"
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium --app "Speakers" \
    --style normal volume 0.50 1.0
sleep 2.6

say "B: low (lighter green accent)"
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-low --app "Speakers" \
    --style low volume 0.20 1.0
sleep 2.6

say "C: warn (orange/amber accent)"
"$AWOB" send --preempt --source "$SRC" --icon battery-caution --app "Battery" \
    --style warn battery 0.20 1.0
sleep 2.6

say "D: critical (red accent)"
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-muted --app "Speakers" \
    --style critical mute 1 1
sleep 2.6

say "E: muted (red accent, lower alpha)"
"$AWOB" send --preempt --source "$SRC" --icon microphone-disabled --app "Microphone" \
    --style muted mute 1 1
sleep 2.6

say "F: --accent override wins over style"
note "Style says critical (red), but --accent forces magenta."
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium --app "Speakers" \
    --style critical --accent "#ff00aa" volume 0.65 1.0
sleep 3.0

say "DONE"
