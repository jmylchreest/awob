#!/usr/bin/env bash
# Console theme demo.
#
# Switches to the `console` theme (in-memory only — the daemon's
# original theme is restored at the end) and runs through label
# cases + cell animation.

set -eu
SCRIPT_NAME="console"
. "$(dirname "$0")/lib.sh"
require_daemon
SRC="$(srcid)"

say "Switching to console theme (in-memory, not persisted)"
"$AWOB" theme set console
note "Theme set. Restore at end of demo."
sleep 0.4

say "A: short label (VOLUME)"
"$AWOB" send --preempt --source "$SRC" --icon audio-volume-medium volume 0.45 1.0
sleep 2.6

say "B: medium label (BRIGHTNESS) — event default, no \$app"
note "Falls back to label(\$event) → 'Brightness' → uppercased to 'BRIGHTNESS'."
"$AWOB" send --preempt --source "$SRC" --icon display-brightness brightness 0.65 1.0
sleep 2.6

say "C: long label fits without truncation (~26 chars)"
"$AWOB" send --preempt --source "$SRC" --app "Microphone Sensitivity High" mic 0.30 1.0
sleep 2.6

say "D: very long label triggers truncate(…, 36) → 'EXTERNAL USB AUDIO …'"
"$AWOB" send --preempt --source "$SRC" --app "External USB Audio Device 7.1 Surround Channel L/R" volume 0.78 1.0
sleep 2.6

say "E: rapid scroll (cell-mode smooth fractional animation)"
for V in 0.05 0.20 0.40 0.60 0.80 0.95; do
    "$AWOB" send --preempt --source "$SRC" --app "Speakers" volume $V 1.0
    sleep 0.18
done
sleep 3.0

# Best-effort restore. If the daemon was started with a non-default
# theme we don't know which, so just go back to "default".
say "Restoring theme to default"
"$AWOB" theme set default || true

say "DONE"
