#!/usr/bin/env bash
# Shared helpers for awob demo scripts. Source from each demo:
#
#     . "$(dirname "$0")/lib.sh"
#
# Provides: $AWOB, say, note, require_daemon, srcid.

set -u

# Locate the awob CLI binary. Prefer $PATH so installed users get
# what they installed; fall back to a workspace release build for
# dev workflows that haven't `cargo install`-ed.
if command -v awob >/dev/null 2>&1; then
    AWOB="awob"
elif [ -x "$(dirname "${BASH_SOURCE[0]}")/../target/release/awob" ]; then
    AWOB="$(realpath "$(dirname "${BASH_SOURCE[0]}")/../target/release/awob")"
else
    echo "demo: cannot locate the awob CLI binary" >&2
    echo "  cargo install --path crates/awob-cli  ← installs to ~/.cargo/bin" >&2
    echo "  cargo build --release --workspace     ← finds target/release/awob" >&2
    exit 2
fi

# Section header on stdout. Easy to grep for in transcripts.
say() { printf '\n=== %s ===\n' "$*"; }

# Indented note line, two spaces.
note() { printf '  %s\n' "$*"; }

# Confirm a daemon is listening before firing sends. The CLI gives
# friendly errors on its own, but doing this up front means a typo
# in the daemon path stops the demo before any sends happen.
require_daemon() {
    local sock="${AWOB_SOCKET:-${XDG_RUNTIME_DIR:-/tmp}/awob.sock}"
    if [ ! -S "$sock" ]; then
        echo "demo: no awob daemon at $sock" >&2
        echo "  start one with:" >&2
        echo "    ./target/release/awob-daemon --themes-dir ./themes --theme default" >&2
        exit 2
    fi
    if ! "$AWOB" version >/dev/null 2>&1; then
        echo "demo: daemon socket exists at $sock but isn't responding" >&2
        exit 2
    fi
}

# Stable, per-script source ID. PID suffix keeps repeat runs from
# colliding (`source` is the history key, so reusing one across
# unrelated runs would inherit lastValue from the last invocation).
srcid() {
    local prefix="${1:-${SCRIPT_NAME:-demo}}"
    printf '%s-%d' "$prefix" "$$"
}
