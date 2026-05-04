#!/usr/bin/env bash
# Preempt + queue policy demo.
#
# Demonstrates the four cases handled by State::handle_send in the
# wayland thread:
#   - same (source, event) → continuity (in-place tween)
#   - different + preempt=true → hot-swap
#   - different + preempt=false → queue (single-slot, newest-wins)
#   - cycle ends → drain pending

set -eu
SCRIPT_NAME="preempt"
. "$(dirname "$0")/lib.sh"
require_daemon

say "A: ambient battery (preempt=false), then volume preempt mid-cycle"
note "Battery shows. Volume key arrives → instantly hot-swaps."
note "After volume settles, battery does NOT come back — it was on"
note "screen, not queued."
"$AWOB" send --source battery --icon battery-good --app "Battery: charging" battery 0.55 1.0
sleep 0.6
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-high --app "Speakers" volume 0.7 1.0
sleep 4.0

say "B: volume preempt active, ambient battery non-preempt → queue + drain"
note "Volume shows. Battery arrives non-preempt during it → QUEUED."
note "Volume cycle ends → battery fades in fresh."
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-high --app "Speakers" volume 0.5 1.0
sleep 0.6
"$AWOB" send --source battery --icon battery-good --app "Battery: charging" battery 0.40 1.0
sleep 6.0

say "C: same (source, event) continuity (rapid same volume sends)"
note "Each send is same (source, event) → continuity update."
note "No fade-in flash; bar tracks smoothly across the chain."
for V in 0.20 0.35 0.50 0.65 0.80; do
    "$AWOB" send --preempt --source pw-speaker --icon audio-volume-high --app "Speakers" volume $V 1.0
    sleep 0.2
done
sleep 3.0

say "D: queue replacement (newest wins)"
note "Volume preempt active. Battery queues. Weather queues (replaces"
note "battery). Volume settles → only weather drains; battery dropped."
"$AWOB" send --preempt --source pw-speaker --icon audio-volume-high --app "Speakers" volume 0.6 1.0
sleep 0.5
"$AWOB" send --source battery --icon battery-good --app "Battery (DROP)" battery 0.30 1.0
sleep 0.3
"$AWOB" send --source weather --icon weather-clear --app "Weather (SHOW)" temperature 0.75 1.0
sleep 6.0

say "DONE"
