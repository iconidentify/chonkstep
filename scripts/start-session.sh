#!/usr/bin/env bash
# Bootstraps a real Xorg server for chonkstep when launched from a spot
# that doesn't already have one running (SDDM's "wayland-session" launch
# path hands us a bare VT with no X server underneath — unlike a real
# xsessions entry launched via SDDM's own "Xsession" path, which starts
# Xorg for us first). `startx` does that: spins up Xorg on the VT we're
# already attached to, sets up Xauthority, then runs xsession.sh as its
# client once the server is ready.
set -u

LOG_DIR="$HOME/.local/state/chonkstep"
mkdir -p "$LOG_DIR"

exec startx /home/chrisk/chonkstep/scripts/xsession.sh -- vt1 >> "$LOG_DIR/startx.log" 2>&1
