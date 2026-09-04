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

# Resolve the compositor binary. This script runs from two homes and
# must work from both: a git checkout (scripts/install.sh points the
# session entry here, and the binary is a sibling target/release), and
# a package install (/usr/lib/chonkstep/wayland-session.sh, where the
# binary is on PATH as /usr/bin/chonkstep-wayland). The checkout wins
# when both exist, because someone running from a checkout is running
# it to test the checkout. CHONKSTEP_SESSION_BIN is the supervisor
# test's seam (see crates/chonk-testkit/tests/supervisor.rs): the
# watchdog loop below is exercised against a crashing stub instead of
# the real compositor. It is honored only with the paired testing
# marker, so a stale or malicious variable imported into systemd's
# persistent user environment cannot replace the login compositor.
BIN=""
if [ "${CHONKSTEP_SESSION_TESTING:-}" = 1 ]; then
    BIN="${CHONKSTEP_SESSION_BIN:-}"
fi
if [ -z "$BIN" ]; then
    checkout_bin="$(cd "$(dirname "$0")/.." && pwd)/target/release/chonkstep-wayland"
    if [ -x "$checkout_bin" ]; then
        BIN="$checkout_bin"
    else
        BIN="$(command -v chonkstep-wayland || echo "$checkout_bin")"
    fi
    unset checkout_bin
fi
# The same state directory the compositor's own state files resolve to
# (chonk_shell::startup::state_dir honors XDG_STATE_HOME first) — it
# must be, because the recovery marker the watchdog drops below is read
# from exactly there by the recovering compositor.
LOG_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/chonkstep"
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

# What xdg-desktop-portal keys its backend choice off: with this set,
# the portal service reads chonkstep-portals.conf (shipped in
# packaging/portal/, installed by scripts/install.sh) and routes
# ScreenCast/Screenshot to xdg-desktop-portal-wlr — which is what makes
# screen sharing in a browser call work — and everything else to the
# GTK backend. Exported unconditionally: this *is* the chonkstep
# session, and a display manager that read DesktopNames from our own
# .desktop entry set the same value anyway. Toolkits also read it, but
# only for desktop-specific quirks; an unknown name is a safe default.
export XDG_CURRENT_DESKTOP=chonkstep
export XDG_SESSION_DESKTOP=chonkstep
export XDG_MENU_PREFIX=chonkstep-
export XDG_BACKEND=wayland

# uwsm already owns the session bus, graphical targets, readiness and
# activation-environment cleanup. The direct/TTY entry keeps the
# fallback path below. INVOCATION_ID is accepted only together with a
# uwsm marker so an unrelated service invocation cannot accidentally
# suppress the fallback session setup.
_CHONKSTEP_UWSM=0
if [ -n "${UWSM_FINALIZE_VARNAMES:-}${UWSM_WAIT_VARNAMES:-}${UWSM_ID:-}" ]; then
    _CHONKSTEP_UWSM=1
elif [ -r /proc/self/cgroup ] \
        && grep -Eq '(^|/)wayland-wm@[^/]+\.service($|/)' /proc/self/cgroup; then
    # uwsm does not promise to export a UWSM_* marker to the compositor,
    # but its systemd unit name is part of our cgroup before the service
    # reaches active. Checking the cgroup also avoids the startup race in
    # `systemctl is-active`: while this script starts, the unit is still
    # `activating`, not `active`.
    _CHONKSTEP_UWSM=1
fi

# Deliberately NO CHONKSTEP_BACKEND override. The compositor decides for
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
# clear all three selectors and let the decision be made from the truth.
# This includes CHONKSTEP_BACKEND because a prior nested development run
# may have imported `winit` into the persistent systemd user environment;
# uwsm intentionally carries arbitrary environment variables forward.
#
# The rest are process-private test/restart/session controls. A shell
# hosted inside a nested compositor can legitimately publish its whole
# environment to the systemd user manager. None of these may then
# alter a real SDDM login. CHONKSTEP_OWNS_XCURSOR_SIZE is paired with a
# value ChonkStep generated itself, so clear that value too; an
# XCURSOR_SIZE with no ownership marker remains a user's preference.
_CHONKSTEP_STALE_ENV=(
    DISPLAY
    WAYLAND_DISPLAY
    HYPRLAND_INSTANCE_SIGNATURE
    CHONKSTEP_BACKEND
    CHONKSTEP_CONTROL_SOCKET
    CHONKSTEP_NO_APPEARANCE_PROPAGATION
    CHONKSTEP_OWNS_XCURSOR_SIZE
    CHONKSTEP_SESSION_BIN
    CHONKSTEP_SESSION_CONTINUES
    CHONKSTEP_SESSION_TESTING
    CHONKSTEP_TEST_CONFIG_HOME
    CHONKSTEP_TEST_GAMMA_SIZE
    CHONKSTEP_TEST_PANEL_TILE
    CHONKSTEP_TEST_RUST_LOG
    CHONKSTEP_TEST_SOCKET
    CHONKSTEP_WAYLAND_BIN
)
_CHONKSTEP_STALE_OWNED_CURSOR=0
if [ -n "${CHONKSTEP_OWNS_XCURSOR_SIZE:-}" ]; then
    _CHONKSTEP_STALE_OWNED_CURSOR=1
    unset XCURSOR_SIZE
fi
unset "${_CHONKSTEP_STALE_ENV[@]}"

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
# Answer Hyprland's IPC, so Omarchy's tooling works here. This is the
# compositor's own default now (docs/hyprland-ipc.md §5) and the export
# is redundant on a default build -- it is kept, spelled out, because
# this is the file a user reads to find out what their session does,
# and a session that impersonates another compositor should say so
# somewhere a person will actually look. Set it to 0 to decline; the
# 53 Omarchy scripts that shell out to hyprctl and the Quickshell bar
# that talks the sockets directly stop working, and nothing else
# changes.
export CHONKSTEP_HYPRLAND_IPC=${CHONKSTEP_HYPRLAND_IPC:-1}

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

# Not built, not installed, or the checkout moved out from under the
# session entry installed by scripts/install.sh.
[ -x "$BIN" ] || fail "$BIN is missing or not executable; build it with \
'cargo build --release --workspace' in the checkout (or re-run scripts/install.sh \
if the checkout moved), or install the chonkstep package"

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
#
# This used to `exec dbus-run-session -- $BIN` directly; now that a
# watchdog loop supervises the compositor, the *script* has to stay
# alive around every compositor run, so it re-execs ITSELF under the
# bus once and then supervises from inside it. That also keeps one bus
# spanning every recovery — a compositor re-execed after a crash finds
# the same $DBUS_SESSION_BUS_ADDRESS its predecessor's clients were
# on, so a restored app can still talk to whatever outlived the crash.
# The recursion terminates because dbus-run-session sets the very
# variable this condition checks; the marker export is a belt for the
# suspenders — if dbus-run-session somehow ran us without a bus we log
# and carry on rather than exec forever.
if [ "$_CHONKSTEP_UWSM" -eq 0 ] && [ -z "${DBUS_SESSION_BUS_ADDRESS:-}" ] && [ -z "${_CHONKSTEP_BUS_WRAPPED:-}" ] \
        && command -v dbus-run-session >/dev/null 2>&1; then
    export _CHONKSTEP_BUS_WRAPPED=1
    exec dbus-run-session -- "$0" "$@"
fi
unset _CHONKSTEP_BUS_WRAPPED

# Removing the variables from this process protects the compositor and
# its direct children. Remove the same stale values from systemd's user
# activation environment as well: its services do not descend from
# this process, and without this repair a poisoned value would survive
# this login and be offered to the next one. Omarchy's D-Bus broker
# activates through that manager. A direct/TTY session instead starts
# a fresh private bus *after* the local scrub above. There is
# deliberately no `dbus-update-activation-environment --unset` here:
# that option does not exist in dbus-update-activation-environment.
# The current session's real Wayland socket and Hyprland-compatible
# signature are republished by the watcher below after the compositor
# creates them.
scrub_stale_activation_env() {
    local names=("${_CHONKSTEP_STALE_ENV[@]}")
    if [ "$_CHONKSTEP_STALE_OWNED_CURSOR" -eq 1 ]; then
        names+=(XCURSOR_SIZE)
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user unset-environment "${names[@]}" >>"$LOG" 2>&1 || true
    fi
}
scrub_stale_activation_env

# ---------------------------------------------------------------------
# Publish the session's environment to D-Bus activation and the systemd
# user instance, for the services that are NOT children of the
# compositor. The portals (xdg-desktop-portal and its backends — the
# path a browser's "share your screen" takes) are D-Bus-activated into
# the systemd user session, so they inherit nothing from this script or
# from the compositor; unless someone tells that environment which
# WAYLAND_DISPLAY to open and which desktop this is, the wlr backend
# connects to nothing and screen sharing silently fails. The standard
# cure is `dbus-update-activation-environment --systemd ...` — but it
# can only run once the socket exists, and only the compositor knows
# which name it allocated (it exports WAYLAND_DISPLAY to its own
# children, not to us: $BIN runs in the foreground below).
#
# So: a background watcher tails the compositor's own startup line —
# state.rs logs `wayland socket listening socket="wayland-N"` — and
# publishes the name whenever it changes. "Whenever", not "once",
# because a crash recovery re-execs the compositor, which may allocate
# a different socket; the portals must be repointed or they hold a dead
# one. Silently a no-op when the tooling is absent (non-systemd, or the
# supervisor test's scratch environment, where the stub compositor
# never logs a socket line).
publish_portal_env() {
    # The integration harness launches the compositor binary directly
    # inside a scratch runtime and never enters this login-session
    # wrapper. A CHONKSTEP_TEST_SOCKET found here is therefore stale
    # contamination, not a reason to leave the real session's portal
    # environment unpublished; it was scrubbed above.
    command -v dbus-update-activation-environment >/dev/null 2>&1 || return 0
    local published="" sock
    while :; do
        # The value is the line's one quoted string; matched by its
        # "wayland-" shape rather than by the `socket=` key, because
        # tracing colors the key with ANSI escapes even into a file.
        sock=$(sed -n '/wayland socket listening/s/.*"\(wayland-[^"]*\)".*/\1/p' \
            "$LOG" 2>/dev/null | tail -n 1)
        # The Hyprland instance signature, when the session serves
        # Hyprland's IPC. Same reasoning as WAYLAND_DISPLAY exactly: the
        # compositor chooses it, a D-Bus-activated shell inherits
        # nothing from here, and `hyprctl` and Quickshell's IPC client
        # both find the sockets through this variable and nothing else.
        # Republished on the same change-detection as the socket, so a
        # crash recovery — which re-execs the compositor and mints a new
        # signature — repoints instead of leaving a dead one.
        # Tracing decorates the field name and `=` with ANSI sequences,
        # even in the redirected log. Match from the plain `signature`
        # word to its next quoted value instead of requiring a literal
        # `signature="` adjacency.
        sig=$(sed -n '/hyprland ipc listening/s/.*signature[^"]*"\([^"]*\)".*/\1/p' \
            "$LOG" 2>/dev/null | tail -n 1)
        if [ -n "$sock" ] && [ "$sock$sig" != "$published" ] \
                && [ -S "$XDG_RUNTIME_DIR/$sock" ]; then
            dbus-update-activation-environment --systemd \
                "WAYLAND_DISPLAY=$sock" \
                "XDG_CURRENT_DESKTOP=$XDG_CURRENT_DESKTOP" \
                "XDG_SESSION_DESKTOP=$XDG_SESSION_DESKTOP" \
                "XDG_SESSION_TYPE=$XDG_SESSION_TYPE" \
                "XDG_MENU_PREFIX=$XDG_MENU_PREFIX" \
                "XDG_BACKEND=$XDG_BACKEND" \
                ${sig:+"HYPRLAND_INSTANCE_SIGNATURE=$sig"} \
                >>"$LOG" 2>&1 || true
            if [ "$_CHONKSTEP_UWSM" -eq 1 ] && command -v uwsm >/dev/null 2>&1; then
                WAYLAND_DISPLAY="$sock" \
                    HYPRLAND_INSTANCE_SIGNATURE="$sig" \
                    uwsm finalize HYPRLAND_INSTANCE_SIGNATURE >>"$LOG" 2>&1 || true
            elif command -v systemctl >/dev/null 2>&1; then
                # The direct session owns these targets and therefore
                # owns stopping them at logout too. This brings the
                # lock-before-suspend, IME and desktop-autostart units
                # up on non-uwsm/systemd logins.
                systemctl --user start graphical-session.target xdg-desktop-autostart.target \
                    >>"$LOG" 2>&1 || true
            fi
            published="$sock$sig"
        fi
        sleep 1
    done
}
publish_portal_env &
_env_watcher=$!
# The watcher must not outlive the session: it holds the log open and
# would republish a stale socket into the next login's environment.
_session_stopping=0
_compositor_pid=""
cleanup_session() {
    kill "$_env_watcher" 2>/dev/null || true
    # UWSM owns its activation environment and graphical targets. More
    # importantly, its logout may have been initiated by SDDM stopping
    # the entire session cgroup: do not put a synchronous user-manager
    # round trip on that time-critical path. The next login's startup
    # scrub above removes any private variable a child imported late.
    if [ "$_CHONKSTEP_UWSM" -eq 1 ]; then
        return
    fi
    # A direct/TTY session has no outer lifecycle owner, so it performs
    # the corresponding best-effort cleanup itself.
    scrub_stale_activation_env
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user stop xdg-desktop-autostart.target graphical-session.target \
            >>"$LOG" 2>&1 || true
        systemctl --user unset-environment WAYLAND_DISPLAY HYPRLAND_INSTANCE_SIGNATURE \
            XDG_CURRENT_DESKTOP XDG_SESSION_DESKTOP XDG_SESSION_TYPE XDG_MENU_PREFIX XDG_BACKEND \
            >>"$LOG" 2>&1 || true
    fi
}
stop_session() {
    _session_stopping=1
    # Forward one TERM even when only the supervisor was signalled.
    # When the whole process group was signalled the child may already
    # be gone; kill's failure is expected and harmless.
    if [ -n "$_compositor_pid" ]; then
        kill -TERM "$_compositor_pid" 2>/dev/null || true
    fi
}
trap cleanup_session EXIT
trap stop_session TERM HUP INT

# ---------------------------------------------------------------------
# The crash watchdog. A compositor panic used to be a black screen and
# a lost session; now an *abnormal* exit (nonzero status, or a signal —
# the panic hook in chonkstep-wayland's main aborts, so every panic is
# one) drops a recovery marker and re-execs the compositor, which sees
# the marker, restores the recorded session layout, and — when
# lock_command is configured — comes back locked. A clean exit (the
# root menu's Exit, i.e. the user logging out) ends the loop and the
# session. Note the compositor's own hot-restart (scripts/restart.sh,
# the `restart` keybinding) never surfaces here at all: that is an
# in-place exec inside the same process, not an exit.
#
# The brake: more than $MAX_CRASHES abnormal exits inside
# $CRASH_WINDOW_SECS seconds means the compositor is crash-looping —
# a broken build, a driver that dies on startup — and re-execing it
# forever would sit the user in front of a flickering black screen
# while filling the disk with logs. The loop stops, says so in the log
# and on stderr (the display manager's journal), and exits nonzero so
# the greeter comes back.
MAX_CRASHES=3
CRASH_WINDOW_SECS=60
crash_times=""

while :; do
    "$BIN" >> "$LOG" 2>&1 &
    _compositor_pid=$!
    wait "$_compositor_pid"
    status=$?
    _compositor_pid=""

    # A TERM/HUP/INT delivered to the supervisor is a session-manager
    # logout. The child either handled the forwarded TERM cleanly or
    # died with 128+signal because it predates that handler; neither is
    # a crash to recover while the session itself is being torn down.
    if [ "$_session_stopping" -eq 1 ]; then
        exit 0
    fi

    # A clean exit is the user logging out: the loop's one normal end.
    if [ "$status" -eq 0 ]; then
        break
    fi

    now=$(date +%s)
    # Keep only the crashes still inside the window, then count this
    # one against them.
    kept=""
    for t in $crash_times; do
        if [ $((now - t)) -lt "$CRASH_WINDOW_SECS" ]; then
            kept="$kept $t"
        fi
    done
    crash_times="$kept $now"
    # `wc -w` over the space-separated list — no arrays, so the loop
    # stays portable to whatever /bin/sh-ish bash a rescue boots.
    count=$(echo "$crash_times" | wc -w)

    if [ "$count" -gt "$MAX_CRASHES" ]; then
        # The marker belongs to a restart inside this supervisor. Once the
        # brake returns to the greeter there is no session left to recover;
        # carrying it into a later login would report a false recovery and
        # could unnecessarily lock an otherwise clean new session.
        rm -f "$LOG_DIR/recovery"
        printf 'chonkstep-wayland session: crash loop (%s abnormal exits in %ss) - giving up. See %s\n' \
            "$count" "$CRASH_WINDOW_SECS" "$LOG" | tee -a "$LOG" >&2
        exit 1
    fi

    printf 'chonkstep-wayland session: compositor exited abnormally (status %s), restarting (crash %s of %s in the last %ss)\n' \
        "$status" "$count" "$MAX_CRASHES" "$CRASH_WINDOW_SECS" | tee -a "$LOG" >&2

    # The marker the recovering compositor consumes at startup — the
    # entire channel between this loop and the recovery behavior
    # (prominent log line, session lock, layout restore). Dropped only
    # on the abnormal path, so a logout can never look like a crash.
    touch "$LOG_DIR/recovery"
done
