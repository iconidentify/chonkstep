#!/usr/bin/env bash
# Runs the end-to-end suite: real nested compositors, real clients,
# injected input, screenshot assertions. Each test boots its own
# chonkstep-wayland as a window inside YOUR current Wayland session —
# expect a few compositor windows to blink in and out while it runs.
#
# Why this is a script and not just `cargo test`: the suite needs the
# compositor *binary* built first (the harness launches it as a
# process, so cargo's own dependency tracking never learns about it),
# and it needs a live Wayland session, which is why these tests are
# #[ignore]d during an ordinary `cargo test`. CI's Wayland job calls
# this script with `--headless` to supply that session explicitly.
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
# Prerequisites beyond the toolchain — real clients the tests launch
# inside the nested session, and the one tool the harness screenshots
# through (grep tests/ for `launch("` to see which test needs which):
#   foot       the terminal most tests open a window with
#   alacritty  Omarchy's terminal, for the decoration tests
#   zenity     GTK dialogs, for the drag/resize/miniaturize regressions
#   grim       every screenshot, via the compositor's own screencopy
# Missing ones are reported together up front rather than one at a
# time as each test in turn fails to map a window.
#
# `--headless` starts an isolated Weston host first.  It is useful on a
# CI runner, over SSH, or while the real desktop is locked: a hidden
# nested host window cannot complete presentation, which starves the
# barrier the harness intentionally waits on and produces misleading
# client-map timeouts.
#
# Usage: scripts/e2e.sh [--headless] [extra cargo-test args, e.g. a test name]
set -euo pipefail
cd "$(dirname "$0")/.."

headless=false
if [ "${1:-}" = "--headless" ]; then
    headless=true
    shift
fi

if "$headless"; then
    if ! command -v weston >/dev/null 2>&1; then
        echo "e2e.sh: --headless needs weston on PATH" >&2
        exit 1
    fi
    if ! command -v dbus-run-session >/dev/null 2>&1; then
        echo "e2e.sh: --headless needs dbus-run-session on PATH" >&2
        exit 1
    fi

    host_runtime=$(mktemp -d "${TMPDIR:-/tmp}/chonkstep-e2e-host.XXXXXX")
    chmod 700 "$host_runtime"
    host_log="$host_runtime/weston.log"
    host_socket=wayland-chonkstep-e2e
    XDG_RUNTIME_DIR="$host_runtime" weston \
        --backend=headless-backend.so \
        --socket="$host_socket" \
        --idle-time=0 \
        --width=2560 \
        --height=1600 \
        >"$host_log" 2>&1 &
    host_pid=$!

    cleanup_headless_host() {
        status=$?
        trap - EXIT
        kill "$host_pid" 2>/dev/null || true
        wait "$host_pid" 2>/dev/null || true
        if [ "$status" -ne 0 ]; then
            echo "--- headless Weston log ---" >&2
            tail -200 "$host_log" >&2 || true
        fi
        rm -rf -- "$host_runtime"
        exit "$status"
    }
    trap cleanup_headless_host EXIT

    ready=false
    for _ in $(seq 1 200); do
        if [ -S "$host_runtime/$host_socket" ]; then
            ready=true
            break
        fi
        if ! kill -0 "$host_pid" 2>/dev/null; then
            break
        fi
        sleep 0.05
    done
    if ! "$ready"; then
        echo "e2e.sh: headless Weston did not create $host_socket" >&2
        exit 1
    fi

    export XDG_RUNTIME_DIR="$host_runtime"
    export WAYLAND_DISPLAY="$host_socket"
    export WINIT_UNIX_BACKEND=wayland
    unset DISPLAY
fi

if [ -z "${WAYLAND_DISPLAY:-}" ] && [ -z "${DISPLAY:-}" ]; then
    echo "e2e.sh: no Wayland (or X) session to nest inside — run this from a graphical session" >&2
    exit 1
fi

missing=()
for client in foot alacritty zenity grim; do
    command -v "$client" >/dev/null 2>&1 || missing+=("$client")
done
if [ "${#missing[@]}" -gt 0 ]; then
    echo "e2e.sh: the suite launches these and cannot find them on PATH: ${missing[*]}" >&2
    exit 1
fi

echo "Building the compositor and the harness (debug)..."
cargo build -p chonkstep-wayland -p chonk-testkit --quiet

# Every integration-test target in the harness crate, in one run:
# `--ignored` selects exactly the nesting tests (the crash supervisor's
# tests in tests/supervisor.rs need no session and run un-ignored
# under plain `cargo test`, so they are skipped here, not repeated).
# A new tests/*.rs file is therefore part of this suite the moment it
# exists — nothing to register.
#
# --test-threads=1: each test opens a compositor window on your
# desktop; serial keeps them from fighting for focus and keeps the
# host from tiling five of them into shapes nobody asserted on.
echo "Running the end-to-end suite (one nested compositor at a time)..."
if "$headless"; then
    # GitHub's runner exports a nonfunctional D-Bus address. Chromium
    # retries that address for tens of seconds before mapping, which
    # made the real-browser resize test randomly miss its bounded
    # startup deadline. A private session bus gives every nested test a
    # syntactically valid, responsive endpoint and is reaped
    # automatically with the cargo process.
    dbus-run-session -- cargo test -p chonk-testkit --tests -- --ignored --test-threads=1 "$@"
else
    cargo test -p chonk-testkit --tests -- --ignored --test-threads=1 "$@"
fi

# The unit tests that read the Omarchy installed on this machine
# (`#[ignore]`d for the same no-such-thing-in-CI reason; they skip
# themselves when Omarchy is absent). No "$@": that filter is the
# harness's.
echo "Running the unit tests that read the installed Omarchy..."
cargo test -p chonk-shell -p wm-theme --lib -- --ignored installed

echo
echo "e2e suite passed. Artifacts (logs + screenshots) are under ${TMPDIR:-/tmp}/chonk-testkit/"
