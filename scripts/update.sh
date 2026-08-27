#!/usr/bin/env bash
# Updates a running chonkstep install in place: pull, rebuild, and ask
# the live session to hot-restart itself (scripts/restart.sh's marker
# mechanism - the WM re-execs the fresh binary without logging out, and
# open windows survive via the X11 SaveSet).
#
# Usage: scripts/update.sh
set -euo pipefail

cd "$(dirname "$0")/.."

echo "Pulling latest..."
git pull --ff-only

echo "Building (release)..."
cargo build --release --workspace

if pgrep -x chonkstep >/dev/null 2>&1; then
    scripts/restart.sh
else
    echo "No running chonkstep session; the new build will be used at next login."
fi
