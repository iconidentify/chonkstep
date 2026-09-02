#!/usr/bin/env bash
# Verifies that a chonkstep install is one a login manager will
# actually offer: both session entries present, parseable, readable by
# the greeter, and pointing at launchers that exist and are
# executable. Runnable by any user, no root needed, after either
# install route - scripts/install.sh (entries point into the checkout)
# or the Arch package (entries point at /usr/lib/chonkstep). The
# troubleshooting section of docs/quickstart.md is the companion: this
# script proves the files are right, that section covers the ways a
# right file still goes unoffered (Omarchy's pickerless SDDM theme,
# autologin, a missing /dev/dri).
#
# The checks mirror what SDDM 0.21 actually does (src/greeter/
# SessionModel.cpp, src/common/Session.cpp), not what the desktop-entry
# spec suggests a login manager might do:
#   - the greeter (running as the sddm user) lists *.desktop files from
#     /usr/share/xsessions and /usr/share/wayland-sessions and must be
#     able to OPEN each one - an entry the greeter cannot read shows up
#     as a blank, unlaunchable row, so entries must be world-readable;
#   - Hidden=true or NoDisplay=true removes an entry from the list;
#   - an absolute TryExec is stat'ed by the greeter and the entry is
#     hidden when that fails - which is why only the package's entries
#     carry TryExec (paths under /usr), never the checkout's (a default
#     0700 home defeats the stat and hides the session);
#   - Exec is NOT checked at listing time, but at launch SDDM's session
#     wrapper ends in an unquoted `exec $@`, so an Exec path containing
#     whitespace word-splits and the session dies before it starts.
#
# Usage:
#   scripts/verify-install.sh              # verify this machine
#   scripts/verify-install.sh --root DIR   # verify a staged tree, e.g.
#                                          # a makepkg $pkgdir
#
# With --root, Exec/TryExec paths inside the entries are resolved
# under DIR (the layout a package will install), and the live-system
# diagnosis at the end is skipped. Exits 0 when every hard check
# passes, 1 otherwise.
set -u

root=""
while [ $# -gt 0 ]; do
    case "$1" in
        --root)
            [ $# -ge 2 ] || { echo "--root needs a directory" >&2; exit 2; }
            root="$2"; shift 2 ;;
        --root=*)
            root="${1#--root=}"; shift ;;
        -h|--help)
            sed -n '2,40p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *)
            echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
    esac
done
# Strip a trailing slash so "$root$path" concatenation stays clean;
# an empty root is the live system.
root="${root%/}"

failures=0
pass() { printf 'ok:   %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1"; failures=$((failures + 1)); }
warn() { printf 'note: %s\n' "$1"; }

# One key's value from the [Desktop Entry] group of $1. The tiny
# parser matches SDDM's (first '=' splits, no trimming inside values):
# print lines after the [Desktop Entry] header until the next group,
# then take the first "Key=" hit.
entry_value() {
    awk -v key="$2" '
        /^\[/ { in_group = ($0 == "[Desktop Entry]") ; next }
        in_group && index($0, key "=") == 1 { print substr($0, length(key) + 2); exit }
    ' "$1"
}

# Whether every component of an absolute path grants search (x) to
# other users - what lets the sddm greeter user stat a TryExec target
# it does not own. Walks from / down, checking the o+x bit per
# directory.
world_traversable() {
    local dir="$1" mode
    dir=$(dirname "$1")
    while [ "$dir" != "/" ]; do
        mode=$(stat -c '%a' "$dir" 2>/dev/null) || return 1
        case "${mode: -1}" in
            1|3|5|7) ;;
            *) return 1 ;;
        esac
        dir=$(dirname "$dir")
    done
    return 0
}

check_entry() {
    local kind="$1" entry="$2" value target
    if [ ! -f "$entry" ]; then
        fail "$kind entry missing: $entry"
        return
    fi
    pass "$kind entry exists: $entry"

    # World-readable, or the greeter (user sddm) lists a blank row.
    local mode
    mode=$(stat -c '%a' "$entry")
    case "${mode: -1}" in
        4|5|6|7) pass "$kind entry is world-readable (mode $mode)" ;;
        *) fail "$kind entry is NOT world-readable (mode $mode) - the sddm greeter user cannot open it, so the session shows as a blank row or not at all. Fix: sudo chmod 644 $entry" ;;
    esac

    # The [Desktop Entry] group and the keys SDDM reads from it.
    if grep -q '^\[Desktop Entry\]' "$entry"; then
        pass "$kind entry has a [Desktop Entry] group"
    else
        fail "$kind entry has no [Desktop Entry] group - nothing will parse it"
        return
    fi
    for key in Name Exec; do
        value=$(entry_value "$entry" "$key")
        if [ -n "$value" ]; then
            pass "$kind entry has $key=$value"
        else
            fail "$kind entry is missing $key="
        fi
    done
    value=$(entry_value "$entry" Type)
    if [ "$value" = "Application" ]; then
        pass "$kind entry has Type=Application"
    else
        fail "$kind entry has Type=${value:-<absent>}, expected Application"
    fi
    for key in Hidden NoDisplay; do
        value=$(entry_value "$entry" "$key" | tr '[:upper:]' '[:lower:]')
        if [ "$value" = "true" ]; then
            fail "$kind entry sets $key=true - login managers hide it"
        fi
    done

    # Exec: a single absolute path here (both installers write one).
    # Whitespace in it is fatal at launch - SDDM's stock session
    # wrappers (/usr/share/sddm/scripts/{Xsession,wayland-session})
    # end in an unquoted `exec $@`.
    value=$(entry_value "$entry" Exec)
    case "$value" in
        *[[:space:]]*)
            fail "$kind Exec contains whitespace ('$value') - SDDM's session wrapper word-splits it and the session cannot start" ;;
    esac
    target="$root$value"
    if [ -f "$target" ] && [ -x "$target" ]; then
        pass "$kind Exec target exists and is executable: $target"
    else
        fail "$kind Exec target missing or not executable: $target (moved checkout? re-run scripts/install.sh; broken package? reinstall it)"
    fi

    # TryExec, when present, is stat'ed by the greeter as the sddm
    # user - so the target must exist, be executable, AND sit under
    # directories that user can traverse.
    value=$(entry_value "$entry" TryExec)
    if [ -n "$value" ]; then
        target="$root$value"
        if [ -f "$target" ] && [ -x "$target" ]; then
            pass "$kind TryExec target exists and is executable: $target"
        else
            fail "$kind TryExec target missing or not executable: $target - SDDM hides the session when this stat fails"
        fi
        if [ -z "$root" ]; then
            if world_traversable "$value"; then
                pass "$kind TryExec path is traversable by other users"
            else
                fail "$kind TryExec path has a directory without o+x ('$value') - the sddm greeter user cannot stat it, so SDDM hides the session. Either make every parent directory world-traversable or drop TryExec"
            fi
        fi
    fi

    # desktop-file-validate, minus its one known session-file
    # complaint: DesktopNames is not in the desktop-entry spec (it is a
    # session-file convention - Hyprland's and GNOME's own session
    # entries fail identically), so that error alone is expected noise.
    if command -v desktop-file-validate >/dev/null 2>&1; then
        local out
        out=$(desktop-file-validate "$entry" 2>&1 | grep -v 'key "DesktopNames"' || true)
        if [ -n "$out" ]; then
            fail "$kind entry fails desktop-file-validate: $out"
        else
            pass "$kind entry passes desktop-file-validate (DesktopNames session-file convention excepted)"
        fi
    else
        warn "desktop-file-validate not installed (pacman -S desktop-file-utils); skipping spec validation"
    fi
}

echo "== session entries =="
check_entry "X11" "$root/usr/share/xsessions/chonkstep.desktop"
check_entry "Wayland" "$root/usr/share/wayland-sessions/chonkstep.desktop"

echo
echo "== portal map =="
if [ -f "$root/usr/share/xdg-desktop-portal/chonkstep-portals.conf" ]; then
    pass "portal backend map installed (screen sharing routes to the wlr backend)"
else
    fail "portal backend map missing: $root/usr/share/xdg-desktop-portal/chonkstep-portals.conf - both installers ship it; screen sharing will silently fail without it"
fi

if [ -z "$root" ]; then
    echo
    echo "== this machine =="
    # Everything below is diagnosis, not verdict: these are the
    # environment reasons a correct entry still is not offered.
    if [ -e /dev/dri ]; then
        pass "/dev/dri exists - SDDM will list Wayland sessions"
    else
        warn "/dev/dri is absent - SDDM hides the whole Wayland session list (VMs without a virtual GPU do this); the X11 session is unaffected"
    fi

    dm_found=""
    for dm in sddm gdm lightdm greetd lemurs ly; do
        if systemctl is-enabled "$dm" >/dev/null 2>&1; then
            dm_found="$dm"
            warn "display manager enabled: $dm"
        fi
    done
    [ -n "$dm_found" ] || warn "no display manager enabled - log in from a TTY (docs/quickstart.md, 'Log in') or enable one (e.g. sudo systemctl enable sddm.service)"

    if [ -d /etc/sddm.conf.d ] || [ -f /etc/sddm.conf ]; then
        # Last assignment wins, matching SDDM's read order closely
        # enough for a diagnostic (conf.d sorted, then sddm.conf).
        sddm_theme=$(cat /etc/sddm.conf.d/*.conf /etc/sddm.conf 2>/dev/null | sed -n 's/^Current=//p' | tail -n 1)
        if [ "$sddm_theme" = "omarchy" ]; then
            warn "SDDM uses Omarchy's greeter theme, which has NO session picker and logs every interactive login into Hyprland (uwsm). chonkstep can be perfectly installed and still never appear. See 'Log in' in docs/quickstart.md for the autologin and theme routes around this"
        elif [ -n "$sddm_theme" ]; then
            warn "SDDM greeter theme: $sddm_theme (if you see no session menu at login, the theme may not offer one)"
        fi
        overrides=$(cat /etc/sddm.conf.d/*.conf /etc/sddm.conf 2>/dev/null | grep -c '^SessionDir=' || true)
        if [ "${overrides:-0}" -gt 0 ]; then
            warn "sddm.conf overrides SessionDir - confirm the override lists /usr/share/xsessions and /usr/share/wayland-sessions or chonkstep's entries are outside SDDM's search path"
        fi
    fi

    if command -v pacman >/dev/null 2>&1; then
        owner=$(pacman -Qoq /usr/share/xsessions/chonkstep.desktop 2>/dev/null || true)
        if [ -n "$owner" ]; then
            warn "session entries owned by package: $owner"
        else
            warn "session entries are unowned (checkout install via scripts/install.sh). Installing the Arch package later will conflict with them; remove both chonkstep.desktop entries first if you switch routes"
        fi
    fi
fi

echo
if [ "$failures" -eq 0 ]; then
    echo "verify-install: all checks passed"
    exit 0
fi
echo "verify-install: $failures check(s) FAILED"
exit 1
