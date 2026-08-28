#!/usr/bin/env bash
# Verifies the EWMH surface of a *live* chonkstep session from the
# outside, the way a pager, taskbar, or `wmctrl` would see it: every
# check reads root-window properties with plain `xprop`, no chonkstep
# internals. Run it from a terminal inside the session (DISPLAY must
# point at the chonkstep display — automatic for a terminal it
# spawned), e.g. under scripts/dev-nested.sh or in the dev VM.
#
# Prints one "ok:"/"FAIL:" line per assertion ("skip:" where a check's
# preconditions aren't met) and exits nonzero if anything failed. No
# sudo, no side effects beyond activating one window and a round-trip
# hop to workspace 1 and back at the end.
#
# Usage: scripts/verify-ewmh.sh
set -u

fails=0
ok()   { echo "ok: $*"; }
fail() { echo "FAIL: $*"; fails=$((fails + 1)); }
skip() { echo "skip: $*"; }

if [ -z "${DISPLAY:-}" ]; then
    echo "FAIL: DISPLAY is not set — run this from inside the chonkstep session" >&2
    exit 1
fi
if ! command -v xprop >/dev/null 2>&1; then
    echo "FAIL: xprop not found (on Arch/Omarchy: sudo pacman -S xorg-xprop)" >&2
    exit 1
fi

root_prop() { xprop -root "$1" 2>/dev/null; }

# xprop reports a missing property as "not found" (or "no such atom")
# on stdout with exit status 0, so presence has to be judged from the
# output text, not the exit code.
prop_present() {
    local out
    out=$(root_prop "$1")
    [ -n "$out" ] && ! printf '%s' "$out" | grep -qE 'not found|no such atom'
}

# --- _NET_SUPPORTED: the protocols we advertise to clients ------------
supported=$(root_prop _NET_SUPPORTED)
for atom in _NET_ACTIVE_WINDOW _NET_WM_STATE_FULLSCREEN; do
    if printf '%s' "$supported" | grep -q "$atom"; then
        ok "_NET_SUPPORTED lists $atom"
    else
        fail "_NET_SUPPORTED does not list $atom"
    fi
done

# --- _NET_SUPPORTING_WM_CHECK: the "a compliant WM is running" handshake
# (root property points at a WM-owned window whose _NET_WM_NAME names
# the WM — this is how tools decide whether EWMH requests will work).
wcheck=$(root_prop _NET_SUPPORTING_WM_CHECK | grep -oE '0x[0-9a-fA-F]+' | head -n 1)
if [ -z "$wcheck" ]; then
    fail "_NET_SUPPORTING_WM_CHECK yields no window id"
else
    name=$(xprop -id "$wcheck" _NET_WM_NAME 2>/dev/null)
    if printf '%s' "$name" | grep -qi chonkstep; then
        ok "_NET_SUPPORTING_WM_CHECK window $wcheck names chonkstep"
    else
        fail "_NET_SUPPORTING_WM_CHECK window $wcheck does not name chonkstep (got: ${name:-nothing})"
    fi
fi

# --- root properties a taskbar/pager reads unconditionally ------------
for prop in _NET_CLIENT_LIST _NET_NUMBER_OF_DESKTOPS _NET_CURRENT_DESKTOP _NET_WORKAREA; do
    if prop_present "$prop"; then
        ok "$prop present on the root window"
    else
        fail "$prop missing from the root window"
    fi
done

# --- activation round-trip: does a _NET_ACTIVE_WINDOW ClientMessage
# actually move focus and get republished? Needs a window to activate;
# the terminal chonkstep ships is urxvt, so look for one and skip (not
# fail) when none is running — the property checks above still stand.
active_id() { root_prop _NET_ACTIVE_WINDOW | grep -oE '0x[0-9a-fA-F]+' | head -n 1; }

if ! command -v xdotool >/dev/null 2>&1; then
    skip "_NET_ACTIVE_WINDOW activation check (xdotool not installed)"
else
    before=$(active_id)
    target=""
    for w in $(xdotool search --class URxvt 2>/dev/null); do
        target="$w"
        # Prefer a window that isn't already active, so the property
        # has to visibly change rather than merely stay put.
        [ "$(printf '0x%x' "$w")" != "${before:-}" ] && break
    done
    if [ -z "$target" ]; then
        skip "_NET_ACTIVE_WINDOW activation check (no URxvt window running)"
    else
        target_hex=$(printf '0x%x' "$target")
        xdotool windowactivate "$target" 2>/dev/null || true
        # The WM handles the ClientMessage on its next event-loop tick;
        # give it a moment before re-reading the property.
        sleep 0.3
        after=$(active_id)
        if [ "${after:-}" = "$target_hex" ]; then
            if [ "$target_hex" = "${before:-}" ]; then
                ok "_NET_ACTIVE_WINDOW tracks windowactivate ($after — only URxvt was already active)"
            else
                ok "_NET_ACTIVE_WINDOW changed after windowactivate (${before:-none} -> $after)"
            fi
        else
            fail "_NET_ACTIVE_WINDOW did not follow windowactivate (wanted $target_hex, before ${before:-none}, after ${after:-none})"
        fi
    fi
fi

# --- workspace round-trip: does a pager-style desktop switch
# (_NET_CURRENT_DESKTOP ClientMessage, which is what `xdotool
# set_desktop` sends) actually change the published current desktop?
# The WM grows workspaces on demand, so asking for desktop 1 is always
# legal even in a fresh session that only has desktop 0 yet.
if ! command -v xdotool >/dev/null 2>&1; then
    skip "workspace checks (xdotool not installed)"
else
    desk=$(xdotool get_desktop 2>/dev/null)
    if [ -n "${desk:-}" ] && [ "$desk" -ge 0 ] 2>/dev/null; then
        ok "xdotool get_desktop reports desktop $desk"
    else
        fail "xdotool get_desktop reported nothing usable (got: ${desk:-nothing})"
    fi

    xdotool set_desktop 1 2>/dev/null || true
    sleep 0.3
    after_switch=$(xdotool get_desktop 2>/dev/null)
    if [ "${after_switch:-}" = "1" ]; then
        ok "_NET_CURRENT_DESKTOP followed set_desktop 1"
    else
        fail "_NET_CURRENT_DESKTOP did not follow set_desktop 1 (got: ${after_switch:-nothing})"
    fi
    # Hop back so the check leaves the session on the workspace (and
    # with the windows) the user was actually looking at.
    xdotool set_desktop 0 2>/dev/null || true
    sleep 0.3

    # Per-window desktop assignment: every managed client should carry
    # _NET_WM_DESKTOP (pagers use it to place windows on the right
    # miniature). Same URxvt-or-skip convention as the activation check.
    client=$(xdotool search --class URxvt 2>/dev/null | head -n 1)
    if [ -z "${client:-}" ]; then
        skip "_NET_WM_DESKTOP check (no URxvt window running)"
    else
        client_hex=$(printf '0x%x' "$client")
        if xprop -id "$client" _NET_WM_DESKTOP 2>/dev/null | grep -qE '= *[0-9]+'; then
            ok "_NET_WM_DESKTOP present on URxvt window $client_hex"
        else
            fail "_NET_WM_DESKTOP missing from URxvt window $client_hex"
        fi
    fi
fi

if [ "$fails" -gt 0 ]; then
    echo "$fails EWMH check(s) failed"
    exit 1
fi
echo "all EWMH checks passed"
