#!/usr/bin/env bash
# Cross-source / cross-event demo.
#
# Verifies history keying by (source, event) — distinct events on
# the same source don't cross-contaminate $lastValue, and distinct
# sources stay independent.

set -eu
SCRIPT_NAME="cross-source"
. "$(dirname "$0")/lib.sh"
require_daemon

say "A: same source, different events (volume vs mute on speaker)"
note "Sequence: speaker volume 0.6 → speaker mute 1.0 → speaker volume 0.4"
note "Each event has its own \$lastValue history slot — the volume on"
note "the third send picks up 0.6 from the first, NOT 1.0 from the mute."
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-medium --app "Speakers" volume 0.60 1.0
sleep 2.6
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-muted --style critical --app "Speakers" mute 1 1
sleep 2.6
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-medium --app "Speakers" volume 0.40 1.0
sleep 3.0

say "B: different sources, same event (speaker vs headphones)"
note "Each source keeps its own history. Switching between them shows"
note "the icon/label swap with bar value continuity from the current"
note "interpolated position."
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-medium --app "Speakers" volume 0.50 1.0
sleep 2.6
"$AWOB" send --preempt --source pw-headphones --icon audio-headphones --app "Headphones" volume 0.80 1.0
sleep 2.6
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-low --app "Speakers" volume 0.25 1.0
sleep 3.0

say "C: rapid different-source pings (mid-cycle hot-swaps)"
note "All preempt=true, so each one replaces whatever's on screen."
note "Bar value continues from current interpolated position even as"
note "the icon/label swap."
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-medium --app "Speakers" volume 0.50 1.0
sleep 0.4
"$AWOB" send --preempt --source pw-headphones --icon audio-headphones --app "Headphones" volume 0.80 1.0
sleep 0.4
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-low --app "Speakers" volume 0.30 1.0
sleep 3.0

say "DONE"
