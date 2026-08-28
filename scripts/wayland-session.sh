#!/usr/bin/env bash
# Entry point for the "chonkstep (Wayland)" session listed in a login
# manager's session picker (see
# /usr/share/wayland-sessions/chonkstep.desktop, which points Exec at
# this script), and the thing to `exec` from a bare TTY for a login
# session with no display manager at all:
#
#     exec /path/to/chonkstep/scripts/wayland-session.sh
#
# The Wayland twin of scripts/xsession.sh, and the differences are the
# interesting part. There is no Xorg here for the session to become the
# window manager of: chonkstep-wayland *is* the display server, and it
# takes the DRM device, the input devices, and the VT for itself
# through libseat. That also means no nesting wrapper — the
# development equivalent of scripts/dev-nested.sh's Xephyr is just
# running the binary directly inside an existing desktop, where it
# picks the winit backend on its own.
#
# `exec`d directly by the display manager or by the user's login
# shell — PATH/env here is whatever that launcher provides, not this
# user's usual interactive environment. Keep this self-contained;
# don't assume dotfiles ran.
set -u

# Resolve the repo root from this script's own location so the session
# works wherever the checkout lives (real hardware, VM, any username).
BIN="$(cd "$(dirname "$0")/.." && pwd)/target/release/chonkstep-wayland"
LOG_DIR="$HOME/.local/state/chonkstep"
mkdir -p "$LOG_DIR"

# Its own log, not the X11 session's: both sessions write into the
# same state directory, and a Wayland login that clobbered
# session.log would destroy the evidence from the X11 session the user
# is about to fall back to when something goes wrong.
LOG="$LOG_DIR/wayland-session.log"

export RUST_LOG="${RUST_LOG:-info}"

# What every well-behaved toolkit checks to decide whether it is
# allowed to prefer its Wayland path. Nothing in chonkstep reads it —
# this is for the clients.
export XDG_SESSION_TYPE=wayland

# Deliberately NO CHONKSTEP_BACKEND export. The compositor decides for
# itself which half of its dual backend to run: an existing
# WAYLAND_DISPLAY or DISPLAY means there is already a desktop here to
# nest a window inside, and their absence means it owns the hardware.
# Pinning it here would be a lie the moment someone copied this script
# somewhere else — and it would also disable the one useful diagnostic,
# which is that the compositor logs which backend it chose and why.
#
# The corollary is that a stale DISPLAY or WAYLAND_DISPLAY leaking in
# from a launcher (or from a dotfile that exports DISPLAY=:0
# unconditionally) would make the compositor try to open a window on a
# desktop that is not there and fail on a black screen. This is a
# login session; by definition neither variable is meaningful yet, so
# clear both and let the decision be made from the truth. The
# compositor exports its own values — the socket it allocates, and
# DISPLAY from the XWayland server it starts — to everything it
# spawns.
unset DISPLAY WAYLAND_DISPLAY

# Keyboard layout. libxkbcommon's XKB_DEFAULT_* convention is what the
# compositor reads to build the seat's keymap (see state.rs), because a
# session started from a TTY has no settings daemon to ask and a
# hardcoded US layout is unusable for most of the world. These are
# re-exported rather than assigned: whatever the environment already
# carries wins, and a user who has none sets them either in their own
# environment or by editing the block below —
#
#     XKB_DEFAULT_LAYOUT=de
#     XKB_DEFAULT_VARIANT=nodeadkeys
#     XKB_DEFAULT_OPTIONS=ctrl:nocaps
#
# — before the loop. The unset ones are left alone rather than exported
# empty: the compositor already discards empty values (state.rs), but
# XWayland and the toolkits read these variables too, and handing them
# an empty string where the user simply has no preference is a worse-
# formed environment than an absent variable.
for _xkb in XKB_DEFAULT_RULES XKB_DEFAULT_MODEL XKB_DEFAULT_LAYOUT \
            XKB_DEFAULT_VARIANT XKB_DEFAULT_OPTIONS; do
    if [ -n "${!_xkb:-}" ]; then
        export "${_xkb?}"
    fi
done
unset _xkb

# Deliberately NO picom, and this is not an oversight to be corrected
# by copying the block over from scripts/xsession.sh. picom is an X11
# compositing manager; under Wayland the compositor *is* the
# compositor, and the translucent terminals the themes ask for are
# composited by chonkstep-wayland itself in the same GLES scene that
# draws the chrome. Starting picom here would at best do nothing (no
# X11 root window to own) and at worst fight XWayland for redirection.

# Rotate a runaway log at login rather than letting it grow without
# bound: a session that once got stuck in an error loop (a dead X
# connection being polled, before ShutdownRequested existed) left a
# multi-gigabyte session.log behind. One .old generation is plenty for
# postmortems; the append redirect below starts the fresh file.
if [ -f "$LOG" ] && [ "$(wc -c < "$LOG")" -gt 52428800 ]; then
    mv -f "$LOG" "$LOG.old"
fi

# Pre-flight, because the failure modes here are silent ones. A display
# manager that starts this and gets an immediate exit drops the user
# back at the greeter with no explanation, and the session log would
# hold nothing at all: the redirect that creates it is on the exec
# below. So the two things that can be wrong before the compositor
# even runs are said out loud, into both the log and the launcher's
# stderr.
fail() {
    printf 'chonkstep-wayland session: %s\n' "$1" | tee -a "$LOG" >&2
    exit 1
}

# Not built, or the checkout moved out from under the session entry
# installed by scripts/install.sh.
[ -x "$BIN" ] || fail "$BIN is missing or not executable; build it with \
'cargo build --release --workspace' in the checkout, or re-run scripts/install.sh if the checkout moved"

# The compositor cannot create its Wayland socket without this; on a
# systemd machine pam_systemd sets it at login, so its absence means
# either a non-systemd login path or a launcher that stripped the
# environment.
[ -n "${XDG_RUNTIME_DIR:-}" ] || fail "XDG_RUNTIME_DIR is not set - a Wayland compositor has nowhere to put its socket"

# A session bus is not optional plumbing for a modern desktop —
# chonkstep itself never touches D-Bus, but the applications its menus
# launch expect $DBUS_SESSION_BUS_ADDRESS to exist. Unlike the X11
# script this wraps conditionally: a display manager that starts
# sessions through the systemd user instance has already given us a
# bus, and starting a second one inside it would split the session in
# half — an app launched from the dock would not be able to talk to
# the same app started from anywhere else. A TTY login usually has no
# bus, and that is the case dbus-run-session exists for.
if [ -n "${DBUS_SESSION_BUS_ADDRESS:-}" ] || ! command -v dbus-run-session >/dev/null 2>&1; then
    exec "$BIN" >> "$LOG" 2>&1
fi

exec dbus-run-session -- "$BIN" >> "$LOG" 2>&1
