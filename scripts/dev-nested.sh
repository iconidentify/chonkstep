#!/usr/bin/env bash
# Runs chonkstep nested inside a Xephyr window on your real (host)
# desktop, instead of taking over a whole display. The Xephyr window is
# a completely normal window as far as Hyprland/Omarchy is concerned —
# drag it, resize it, snap it, move it to another workspace — and
# everything drawn *inside* it (background, dock, windows) is managed by
# chonkstep itself. This is the standard way to develop a window manager
# without a second machine or a VM.
#
# Usage: scripts/dev-nested.sh [width] [height] [scale]
#
# `scale` multiplies every pixel dimension in the WM's theme and dock/
# icon chrome (titlebar height, buttons, fonts, bevels, ...) — useful on
# a HiDPI display, since a nested X server has no display-scaling of its
# own and the theme's ~1990s pixel sizes read as tiny at native density.
# Defaults to 2, which reads well on a typical 2x-scaled laptop panel;
# override the default below (or pass a 3rd arg / set CHONKSTEP_SCALE
# yourself) if 2 looks wrong on your monitor. 1 = theme's original size.
set -euo pipefail

cd "$(dirname "$0")/.."

WIDTH="${1:-1600}"
HEIGHT="${2:-1000}"
export CHONKSTEP_SCALE="${3:-${CHONKSTEP_SCALE:-2}}"
# See xsession.sh for why this matters: apps spawned from the root menu
# that draw their own cursor via Xcursor (most modern toolkits) read
# this instead of anything chonkstep itself controls.
: "${XCURSOR_SIZE:=$(awk -v s="$CHONKSTEP_SCALE" 'BEGIN { printf "%d", (24*s)+0.5 }')}"
export XCURSOR_SIZE

if ! command -v Xephyr >/dev/null 2>&1; then
    echo "Xephyr not found. On Arch/Omarchy: sudo pacman -S xorg-server-xephyr" >&2
    exit 1
fi

# Pick the first free nested display number (skip whatever's already in use).
nested=1
while [ -e "/tmp/.X11-unix/X${nested}" ]; do
    nested=$((nested + 1))
done
display=":${nested}"

echo "Building chonkstep (debug)..."
cargo build --workspace --quiet

echo "Starting Xephyr on ${display} (${WIDTH}x${HEIGHT})..."
Xephyr "${display}" -screen "${WIDTH}x${HEIGHT}" -resizeable -no-host-grab -title "chonkstep (nested)" &
xephyr_pid=$!

trap 'kill "$xephyr_pid" 2>/dev/null || true' EXIT

for _ in $(seq 1 50); do
    [ -e "/tmp/.X11-unix/X${nested}" ] && break
    sleep 0.1
done
if ! [ -e "/tmp/.X11-unix/X${nested}" ]; then
    echo "Xephyr didn't come up in time" >&2
    exit 1
fi

echo "Starting chonkstep on ${display} (UI scale ${CHONKSTEP_SCALE}x)..."
echo "Right-click the desktop inside the Xephyr window for the root menu (Terminal / About chonkstep / Exit)."
DISPLAY="${display}" RUST_LOG=info ./target/debug/chonkstep
# Trap above kills Xephyr once chonkstep exits (e.g. via the root menu's
# Exit item), so closing the WM cleanly tears down the nested display too.
