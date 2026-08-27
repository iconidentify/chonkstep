#!/usr/bin/env bash
# Asks a running chonkstep to hot-restart itself in place — no logout,
# no reboot. chonkstep polls for this marker file once per event-loop
# tick and re-execs the on-disk binary it was launched from the
# instant it sees it (see restart_in_place() in main.rs). Existing
# windows survive via X11's SaveSet mechanism and get redecorated
# automatically by the fresh instance.
#
# Usage: rebuild first (`cargo build --release`), then run this.
set -eu
STATE_DIR="$HOME/.local/state/chonkstep"
mkdir -p "$STATE_DIR"
touch "$STATE_DIR/restart"
echo "restart requested — chonkstep should re-exec within ~100ms"
