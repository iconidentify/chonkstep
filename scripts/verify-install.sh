#!/usr/bin/env bash
# Verifies that a chonkstep install is one a login manager will
# actually offer: all session entries present, parseable, readable by
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
    local kind="$1" entry="$2" expected_exec="${3:-}" value target executable
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

    # Direct entries contain one absolute launcher path. Managed
    # entries are ordinary desktop commands (currently uwsm plus its
    # arguments), which SDDM intentionally word-splits into argv.
    value=$(entry_value "$entry" Exec)
    if [ -n "$expected_exec" ] && [ "$value" != "$expected_exec" ]; then
        fail "$kind Exec is '$value', expected '$expected_exec'"
    fi
    executable="${value%%[[:space:]]*}"
    if [ "${executable#/}" != "$executable" ]; then
        case "$executable" in
            *[[:space:]]*)
                fail "$kind absolute Exec contains whitespace ('$value') - SDDM's session wrapper word-splits it" ;;
        esac
        target="$root$executable"
    elif [ -n "$root" ]; then
        target="$root/usr/bin/$executable"
    else
        target=$(command -v "$executable" 2>/dev/null || true)
    fi
    if [ -n "$target" ] && [ -f "$target" ] && [ -x "$target" ]; then
        pass "$kind Exec executable exists: $target"
    elif [ "$executable" = "uwsm" ]; then
        warn "$kind is hidden until the optional uwsm package is installed"
    else
        fail "$kind Exec executable missing or not executable: ${target:-$executable} (moved checkout? re-run scripts/install.sh; broken package? reinstall it)"
    fi

    # TryExec, when present, is stat'ed by the greeter as the sddm
    # user - so the target must exist, be executable, AND sit under
    # directories that user can traverse.
    value=$(entry_value "$entry" TryExec)
    if [ -n "$value" ]; then
        if [ "${value#/}" != "$value" ]; then
            target="$root$value"
        elif [ -n "$root" ]; then
            target="$root/usr/bin/$value"
        else
            target=$(command -v "$value" 2>/dev/null || true)
        fi
        if [ -f "$target" ] && [ -x "$target" ]; then
            pass "$kind TryExec target exists and is executable: $target"
        elif [ "$value" = "uwsm" ]; then
            warn "$kind TryExec=uwsm is currently absent; install the optional uwsm package to show this entry"
        else
            fail "$kind TryExec target missing or not executable: $target - SDDM hides the session when this stat fails"
        fi
        if [ -z "$root" ] && [ "${value#/}" != "$value" ]; then
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
check_entry "Wayland/uwsm" "$root/usr/share/wayland-sessions/chonkstep-uwsm.desktop" \
    "uwsm start -g -1 -e -D chonkstep chonkstep.desktop"

echo
echo "== SDDM picker =="
for picker_file in Main.qml metadata.desktop theme.conf; do
    if [ -f "$root/usr/share/sddm/themes/chonkstep/$picker_file" ]; then
        pass "chonkstep SDDM picker has $picker_file"
    else
        fail "chonkstep SDDM picker is missing $root/usr/share/sddm/themes/chonkstep/$picker_file"
    fi
done
theme_metadata="$root/usr/share/sddm/themes/chonkstep/metadata.desktop"
if [ -f "$theme_metadata" ]; then
    if grep -qx 'QtVersion=6' "$theme_metadata"; then
        pass "chonkstep SDDM picker selects the Qt 6 greeter used by Omarchy"
    else
        fail "chonkstep SDDM picker must declare QtVersion=6 (fresh Omarchy installs do not ship the Qt 5 Quick runtime)"
    fi
fi
theme_qml="$root/usr/share/sddm/themes/chonkstep/Main.qml"
if [ -f "$theme_qml" ]; then
    if grep -Eq '(^|[[:space:]])(placeholderText|onAccepted):' "$theme_qml"; then
        fail "chonkstep SDDM picker uses properties absent from SddmComponents 2.0"
    elif command -v qmllint >/dev/null 2>&1; then
        if qml_error=$(qmllint "$theme_qml" 2>&1); then
            pass "chonkstep SDDM picker passes qmllint"
        else
            fail "chonkstep SDDM picker fails qmllint: $qml_error"
        fi
    else
        warn "qmllint not installed; skipping QML type validation"
    fi
fi
if [ -f "$root/etc/sddm.conf.d/zz-chonkstep-theme.conf" ]; then
    pass "chonkstep SDDM picker is enabled"
elif [ -f "$root/usr/share/chonkstep/sddm/zz-chonkstep-theme.conf" ]; then
    pass "chonkstep SDDM picker drop-in is available to the Omarchy installer"
else
    fail "chonkstep SDDM picker drop-in is missing"
fi
if [ -f "$root/usr/share/chonkstep/sddm/zz-chonkstep-autologin.conf" ]; then
    pass "chonkstep SDDM autologin override is available to the Omarchy installer"
elif [ -f "$root/etc/sddm.conf.d/zz-chonkstep-autologin.conf" ]; then
    pass "chonkstep SDDM autologin override is enabled"
elif [ -n "$root" ]; then
    fail "chonkstep SDDM autologin override template is missing"
else
    warn "autologin override template is absent (expected for a checkout install)"
fi

resilience_active="$root/etc/systemd/system/sddm.service.d/90-chonkstep-resilience.conf"
resilience_template="$root/usr/share/chonkstep/systemd/90-chonkstep-sddm-resilience.conf"
resilience_file=""
if [ -f "$resilience_active" ]; then
    resilience_file="$resilience_active"
    pass "SDDM teardown/start-limit resilience is enabled"
elif [ -f "$resilience_template" ]; then
    resilience_file="$resilience_template"
    pass "SDDM resilience drop-in is available to the Omarchy installer"
else
    fail "SDDM resilience drop-in is missing"
fi
if [ -n "$resilience_file" ]; then
    for directive in StartLimitIntervalSec=0 StartLimitBurst=10 TimeoutStopSec=20s RestartSec=3s; do
        if ! grep -qx "$directive" "$resilience_file"; then
            fail "SDDM resilience drop-in is missing $directive: $resilience_file"
        fi
    done
fi
if [ -z "$root" ] && [ -f "$resilience_active" ] && command -v systemctl >/dev/null 2>&1; then
    effective_sddm=$(systemctl show sddm.service \
        -p StartLimitIntervalUSec -p StartLimitBurst -p TimeoutStopUSec -p RestartUSec \
        2>/dev/null || true)
    for directive in StartLimitIntervalUSec=0 StartLimitBurst=10 TimeoutStopUSec=20s RestartUSec=3s; do
        if ! grep -qx "$directive" <<<"$effective_sddm"; then
            fail "SDDM has not loaded the expected $directive value; run sudo systemctl daemon-reload"
        fi
    done
fi

echo
echo "== portal map =="
portal_map="$root/usr/share/xdg-desktop-portal/chonkstep-portals.conf"
if [ -f "$portal_map" ] \
    && grep -qx 'org.freedesktop.impl.portal.ScreenCast=wlr' "$portal_map" \
    && grep -qx 'org.freedesktop.impl.portal.Screenshot=wlr' "$portal_map"; then
    pass "portal backend map installed (ScreenCast and Screenshot route to wlr)"
elif [ -f "$portal_map" ]; then
    fail "portal backend map does not route ScreenCast and Screenshot to wlr: $portal_map"
else
    fail "portal backend map missing: $portal_map - both installers ship it; screen sharing will silently fail without it"
fi
if [ -x "$root/usr/lib/xdg-desktop-portal-wlr" ]; then
    pass "wlr portal backend installed for screen sharing and portal screenshots"
elif [ -z "$root" ] && [ -f /etc/sddm.conf.d/zz-chonkstep-theme.conf ]; then
    fail "chonkstep login integration is enabled but xdg-desktop-portal-wlr is absent; rerun 'omarchy install desktop-chonkstep'"
else
    warn "xdg-desktop-portal-wlr is external to this staged package; the Omarchy integration command installs it"
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
    if [ -z "$dm_found" ]; then
        warn "login mode: no display manager; start the uwsm session from a TTY (docs/quickstart.md, 'Log in')"
    fi

    if [ "$dm_found" = "sddm" ] && { [ -d /etc/sddm.conf.d ] || [ -f /etc/sddm.conf ]; }; then
        # SDDM reads vendor drop-ins, administrator drop-ins, then the
        # monolithic administrator file; the last assignment in a
        # section wins. Track the section so an unrelated Session= key
        # cannot be mistaken for autologin.
        sddm_config=$(
            for directory in /usr/lib/sddm/sddm.conf.d /etc/sddm.conf.d; do
                [ -d "$directory" ] || continue
                while IFS= read -r file; do
                    cat "$file"
                done < <(find "$directory" -maxdepth 1 -type f -name '*.conf' -print | LC_ALL=C sort)
            done
            [ ! -f /etc/sddm.conf ] || cat /etc/sddm.conf
        )
        sddm_theme=$(printf '%s\n' "$sddm_config" | awk '
            /^\[/ { section=$0; next }
            section == "[Theme]" && /^Current=/ { print substr($0, 9) }
        ' | tail -n 1)
        autologin_user=$(printf '%s\n' "$sddm_config" | awk '
            /^\[/ { section=$0; next }
            section == "[Autologin]" && /^User=/ { print substr($0, 6) }
        ' | tail -n 1)
        autologin_session=$(printf '%s\n' "$sddm_config" | awk '
            /^\[/ { section=$0; next }
            section == "[Autologin]" && /^Session=/ { print substr($0, 9) }
        ' | tail -n 1)
        if [ -f /etc/sddm.conf.d/zz-chonkstep-theme.conf ] && [ "$sddm_theme" != "chonkstep" ]; then
            fail "chonkstep theme override is installed but SDDM resolves Theme/Current to '${sddm_theme:-<unset>}' - check /etc/sddm.conf for a later override"
        fi
        if [ -f /etc/sddm.conf.d/zz-chonkstep-autologin.conf ]; then
            if [ -z "$autologin_user" ]; then
                fail "chonkstep autologin override is installed but SDDM has no Autologin/User"
            elif [ "$autologin_session" != "chonkstep-uwsm.desktop" ]; then
                fail "chonkstep autologin override is installed but SDDM resolves Autologin/Session to '${autologin_session:-<unset>}'"
            else
                pass "login mode: SDDM autologin for $autologin_user selects chonkstep-uwsm.desktop"
            fi
        elif [ -n "$autologin_user" ]; then
            warn "login mode: SDDM autologin for $autologin_user selects ${autologin_session:-<unset>}; the picker is bypassed"
        elif [ "$sddm_theme" = "chonkstep" ]; then
            pass "login mode: chonkstep SDDM picker theme is active"
        elif [ "$sddm_theme" = "omarchy" ]; then
            warn "login mode: stock Omarchy greeter (no picker); run 'omarchy install desktop-chonkstep'"
        elif [ -n "$sddm_theme" ]; then
            warn "login mode: SDDM theme '$sddm_theme'; confirm that it presents a session picker"
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
