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

# Resolve the WM binary. This script runs from two homes and must work
# from both: a git checkout (scripts/install.sh points the session
# entry here, and the binary is a sibling target/release), and a
# package install (/usr/lib/chonkstep/xsession.sh, where the binary is
# on PATH as /usr/bin/chonkstep). The checkout wins when both exist,
# because someone running from a checkout is running it to test the
# checkout.
BIN="$(cd "$(dirname "$0")/.." && pwd)/target/release/chonkstep"
if [ ! -x "$BIN" ]; then
    BIN="$(command -v chonkstep || echo "$BIN")"
fi
LOG_DIR="$HOME/.local/state/chonkstep"
mkdir -p "$LOG_DIR"

# A session/message D-Bus bus isn't optional plumbing for most modern
# X11 apps (even basic ones may expect $DBUS_SESSION_BUS_ADDRESS to
# exist) — chonkstep itself doesn't touch D-Bus, but xterm and anything
# else launched from its root menu might.
export RUST_LOG="${RUST_LOG:-info}"

# Deliberately no CHONKSTEP_SCALE export here: UI scale belongs to the
# user's config file (~/.config/chonkstep/config.toml, `scale = 2.0`),
# with the environment variable as a manual override — an export in
# this script would silently outrank every user's config, since env
# beats config in the WM's precedence. The WM also derives and exports
# XCURSOR_SIZE itself from the *effective* scale (see main.rs's
# ensure_xcursor_size), so apps drawing their own Xcursor pointers stay
# in proportion without this script guessing the scale.

# A session compositor gives true alpha (the theme engine's translucent
# terminals; shadows/fades later if wanted) — chonkstep itself stays a
# classic non-compositing WM. xrender backend deliberately: on software
# GL (llvmpipe in the VM) picom's glx backend composites stale window
# textures (frames/terminals frozen at their first frame — confirmed
# live), while xrender tracks damage correctly. Its one quirk — losing
# the wallpaper when the WM's root pixmap is replaced — is handled by
# chonkstep itself, which pokes picom with SIGUSR1 after publishing a
# fresh wallpaper (see main.rs).
# --no-use-damage trades incremental repaints for full-screen ones:
# with damage tracking on, picom v13's xrender backend composites stale
# state after window restacks - a window raised from beneath another
# kept showing the old scene, and the loser of a restack rendered
# ghost-faint until an unrelated drag damaged it (confirmed live; the
# WM's stacking and window content were correct throughout). Full
# repaints are measurably fine on the software-rendered VM and this is
# a correctness-over-speed call; revisit if a real GPU host shows the
# damage path behaving.
if command -v picom >/dev/null 2>&1; then
    picom -b --backend xrender --no-use-damage >> "$LOG_DIR/picom.log" 2>&1
fi

# Rotate a runaway log at login rather than letting it grow without
# bound: a session that once got stuck in an error loop (a dead X
# connection being polled, before ShutdownRequested existed) left a
# multi-gigabyte session.log behind. One .old generation is plenty for
# postmortems; the append redirect below starts the fresh file.
if [ -f "$LOG_DIR/session.log" ] && [ "$(wc -c < "$LOG_DIR/session.log")" -gt 52428800 ]; then
    mv -f "$LOG_DIR/session.log" "$LOG_DIR/session.log.old"
fi

exec dbus-run-session -- "$BIN" >> "$LOG_DIR/session.log" 2>&1
