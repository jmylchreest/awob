#!/usr/bin/env bash
# Run every demo in sequence. Useful as a smoke test or for showing
# off a build to someone over a screen share.
#
# Each script is invoked with `bash` so they don't need exec bits.
# `set -e` propagates: if any inner demo fails, the whole suite stops.

set -eu
HERE="$(dirname "$0")"

bash "$HERE/wedge.sh"
bash "$HERE/preempt.sh"
bash "$HERE/cross-source.sh"
bash "$HERE/icons.sh"
bash "$HERE/styles.sh"
bash "$HERE/console.sh"
bash "$HERE/themes.sh"

printf '\n=== ALL DEMOS COMPLETE ===\n'
