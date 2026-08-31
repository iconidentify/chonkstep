#!/usr/bin/env bash
# Runs the end-to-end suite: real nested compositors, real clients,
# injected input, screenshot assertions. Each test boots its own
# chonkstep-wayland as a window inside YOUR current Wayland session —
# expect a few compositor windows to blink in and out while it runs.
#
# Why this is a script and not just `cargo test`: the suite needs the
# compositor *binary* built first (the harness launches it as a
# process, so cargo's own dependency tracking never learns about it),
# and it needs a live Wayland session, which is also why these tests
# are #[ignore]d and absent from CI (see ci.yml's wayland job for why
# a headless runner cannot boot a compositor at all).
#
# Debug build on purpose — the whole point is to test the binary a
# developer is iterating on, and the harness's waits are all bounded
# polls on observable conditions, so debug-build slowness costs
# latency, never correctness.
#
# Artifacts (compositor logs, screenshots) land under
# $TMPDIR/chonk-testkit/<test-name>/ and are left in place for
# post-mortems.
#
# Usage: scripts/e2e.sh [extra cargo-test args, e.g. a test name]
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    echo "e2e.sh: no Wayland (or X) session to nest inside — run this from a graphical session" >&2
    exit 1
fi

echo "Building the compositor and the harness (debug)..."
cargo build -p chonkstep-wayland -p chonk-testkit --quiet

# --test-threads=1: each test opens a compositor window on your
# desktop; serial keeps them from fighting for focus and keeps the
# host from tiling five of them into shapes nobody asserted on.
echo "Running the end-to-end suite (one nested compositor at a time)..."
cargo test -p chonk-testkit --test e2e -- --ignored --test-threads=1 "$@"

echo
echo "e2e suite passed. Artifacts (logs + screenshots) are under ${TMPDIR:-/tmp}/chonk-testkit/"
