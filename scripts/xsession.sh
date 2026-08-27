#!/usr/bin/env bash
# Entry point for the "chonkstep" session listed in SDDM's login-screen
# session picker (see /usr/share/xsessions/chonkstep.desktop, which
# points Exec at this script). Runs as a real X11 session on real
# hardware, not the nested-Xephyr dev sandbox scripts/dev-nested.sh
# sets up — there's no Xephyr wrapper here because SDDM already started
# the real Xorg server this script's process becomes the window
# manager for.
#
# `exec`d directly by SDDM, not from a login shell — PATH/env here is
# whatever SDDM's own X11 session path provides, not this user's usual
# shell environment. Keep this self-contained; don't assume dotfiles ran.
set -u

# Resolve the repo root from this script's own location so the session
# works wherever the checkout lives (real hardware, VM, any username).
BIN="$(cd "$(dirname "$0")/.." && pwd)/target/release/chonkstep"
LOG_DIR="$HOME/.local/state/chonkstep"
mkdir -p "$LOG_DIR"

# A session/message D-Bus bus isn't optional plumbing for most modern
# X11 apps (even basic ones may expect $DBUS_SESSION_BUS_ADDRESS to
# exist) — chonkstep itself doesn't touch D-Bus, but xterm and anything
# else launched from its root menu might.
export CHONKSTEP_SCALE="${CHONKSTEP_SCALE:-3}"
export RUST_LOG="${RUST_LOG:-info}"

# chonkstep scales its own chrome (titlebars, buttons, its own cursor —
# see wm-x11's create_scaled_cursor) by CHONKSTEP_SCALE, but it has no
# say over apps that draw their *own* cursor via the standard Xcursor
# mechanism (most modern toolkits, including alacritty's winit) — those
# read XCURSOR_SIZE instead. Without this, such an app's cursor stays
# whatever Xcursor's own DPI-unaware default is, visibly out of
# proportion next to chonkstep's own (correctly scaled) pointer the
# instant it crosses from chrome onto that app's content. 24px is
# Xcursor's own conventional 1x base size.
if [ -z "${XCURSOR_SIZE:-}" ]; then
    export XCURSOR_SIZE="$(awk -v s="$CHONKSTEP_SCALE" 'BEGIN { printf "%d", (24*s)+0.5 }')"
fi

# A session compositor gives true alpha (the theme engine's translucent
# terminals; shadows/fades later if wanted) — chonkstep itself stays a
# classic non-compositing WM. xrender backend deliberately: on software
# GL (llvmpipe in the VM) picom's glx backend composites stale window
# textures (frames/terminals frozen at their first frame — confirmed
# live), while xrender tracks damage correctly. Its one quirk — losing
# the wallpaper when the WM's root pixmap is replaced — is handled by
# chonkstep itself, which pokes picom with SIGUSR1 after publishing a
# fresh wallpaper (see main.rs).
if command -v picom >/dev/null 2>&1; then
    picom -b --backend xrender >> "$LOG_DIR/picom.log" 2>&1
fi

exec dbus-run-session -- "$BIN" >> "$LOG_DIR/session.log" 2>&1
