#!/usr/bin/env bash
# Exercise the public Omarchy integration against disposable fresh-install
# filesystem fixtures. No root access, package manager, or live SDDM is used.
set -euo pipefail

cd "$(dirname "$0")/.."
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
    echo "FAIL: $*" >&2
    exit 1
}

assert_file() {
    [ -f "$1" ] || fail "missing file: $1"
}

assert_absent() {
    [ ! -e "$1" ] || fail "unexpected file: $1"
}

stage_package() {
    local root="$1" executable
    install -d "$root/usr/share/xsessions" \
        "$root/usr/share/wayland-sessions" \
        "$root/usr/share/sddm/themes/chonkstep" \
        "$root/usr/share/chonkstep/sddm" \
        "$root/usr/share/chonkstep/systemd" \
        "$root/usr/share/doc/chonkstep" \
        "$root/usr/share/xdg-desktop-portal" \
        "$root/usr/lib/chonkstep" \
        "$root/usr/lib/systemd/system" \
        "$root/usr/bin"

    install -m644 packaging/sddm/chonkstep/{Main.qml,metadata.desktop,theme.conf} \
        "$root/usr/share/sddm/themes/chonkstep/"
    install -m644 packaging/sddm/zz-chonkstep-{theme,autologin}.conf \
        "$root/usr/share/chonkstep/sddm/"
    install -m644 packaging/systemd/sddm.service.d/90-chonkstep-resilience.conf \
        "$root/usr/share/chonkstep/systemd/90-chonkstep-sddm-resilience.conf"
    install -m644 packaging/portal/chonkstep-portals.conf \
        "$root/usr/share/xdg-desktop-portal/chonkstep-portals.conf"
    install -m755 scripts/verify-install.sh "$root/usr/lib/chonkstep/verify-install.sh"
    install -m644 docs/config.example.toml "$root/usr/share/doc/chonkstep/config.example.toml"

    for executable in xsession.sh chonkstep-session; do
        printf '#!/bin/sh\nexit 0\n' > "$root/usr/lib/chonkstep/$executable"
        chmod 755 "$root/usr/lib/chonkstep/$executable"
    done
    printf '#!/bin/sh\nexit 0\n' > "$root/usr/bin/uwsm"
    chmod 755 "$root/usr/bin/uwsm"
    printf '#!/bin/sh\nexit 0\n' > "$root/usr/bin/sddm"
    chmod 755 "$root/usr/bin/sddm"
    printf '%s\n' \
        '[Unit]' \
        'DefaultDependencies=no' \
        'StartLimitIntervalSec=30' \
        'StartLimitBurst=2' \
        '[Service]' \
        'ExecStart=/usr/bin/sddm' \
        'Restart=always' \
        > "$root/usr/lib/systemd/system/sddm.service"

    printf '%s\n' \
        '[Desktop Entry]' \
        'Name=chonkstep' \
        'Exec=/usr/lib/chonkstep/xsession.sh' \
        'TryExec=/usr/lib/chonkstep/xsession.sh' \
        'Type=Application' \
        > "$root/usr/share/xsessions/chonkstep.desktop"
    printf '%s\n' \
        '[Desktop Entry]' \
        'Name=chonkstep (Wayland)' \
        'Exec=/usr/lib/chonkstep/chonkstep-session' \
        'TryExec=/usr/lib/chonkstep/chonkstep-session' \
        'DesktopNames=chonkstep' \
        'Type=Application' \
        > "$root/usr/share/wayland-sessions/chonkstep.desktop"
    printf '%s\n' \
        '[Desktop Entry]' \
        'Name=chonkstep (uwsm)' \
        'Exec=uwsm start -g -1 -e -D chonkstep chonkstep.desktop' \
        'TryExec=uwsm' \
        'DesktopNames=chonkstep' \
        'Type=Application' \
        > "$root/usr/share/wayland-sessions/chonkstep-uwsm.desktop"
    chmod 644 "$root/usr/share/xsessions/chonkstep.desktop" \
        "$root/usr/share/wayland-sessions/"*.desktop

    grep -qx 'QtVersion=6' \
        "$root/usr/share/sddm/themes/chonkstep/metadata.desktop" \
        || fail "SDDM picker does not select Omarchy's Qt 6 greeter"
    grep -qx 'org.freedesktop.impl.portal.ScreenCast=wlr' \
        "$root/usr/share/xdg-desktop-portal/chonkstep-portals.conf" \
        || fail "portal map does not route ScreenCast to xdg-desktop-portal-wlr"
    grep -qx 'org.freedesktop.impl.portal.Screenshot=wlr' \
        "$root/usr/share/xdg-desktop-portal/chonkstep-portals.conf" \
        || fail "portal map does not route Screenshot to xdg-desktop-portal-wlr"
}

write_fresh_omarchy() {
    local root="$1" asset
    install -d "$root/etc/sddm.conf.d" "$root/var/lib/sddm" \
        "$root/usr/share/sddm/themes/omarchy"
    for asset in logo.png lock.png lock-failed.png entry.png entry-failed.png bullet.png; do
        : > "$root/usr/share/sddm/themes/omarchy/$asset"
    done
    printf '%s\n' \
        '[Theme]' \
        'Current=omarchy' \
        '[Users]' \
        'RememberLastUser=true' \
        'RememberLastSession=true' \
        > "$root/etc/sddm.conf.d/99-omarchy-login.conf"
    printf '%s\n' \
        '[Last]' \
        'Session=omarchy.desktop' \
        > "$root/var/lib/sddm/state.conf"
}

encrypted="$work/encrypted"
stage_package "$encrypted"
write_fresh_omarchy "$encrypted"
printf '%s\n' \
    '[Autologin]' \
    'User=alice' \
    'Session=omarchy.desktop' \
    > "$encrypted/etc/sddm.conf.d/autologin.conf"
cp "$encrypted/etc/sddm.conf.d/99-omarchy-login.conf" "$work/99.expected"
cp "$encrypted/etc/sddm.conf.d/autologin.conf" "$work/autologin.expected"

config_home="$work/config-home"
install -Dm644 /dev/null "$config_home/chonkstep/config.toml"
printf '%s\n' 'scale = 2.0' '[keybindings]' > "$config_home/chonkstep/config.toml"

CHONKSTEP_TEST_CONFIG_HOME="$config_home" scripts/omarchy-install-desktop-chonkstep --root "$encrypted"
assert_file "$encrypted/etc/sddm.conf.d/zz-chonkstep-theme.conf"
assert_file "$encrypted/etc/sddm.conf.d/zz-chonkstep-autologin.conf"
resilience="$encrypted/etc/systemd/system/sddm.service.d/90-chonkstep-resilience.conf"
assert_file "$resilience"
grep -qx 'StartLimitIntervalSec=0' "$resilience" \
    || fail "SDDM resilience drop-in does not disable the permanent start-limit latch"
grep -qx 'TimeoutStopSec=20s' "$resilience" \
    || fail "SDDM resilience drop-in does not allow orderly compositor teardown"
grep -qx 'RestartSec=3s' "$resilience" \
    || fail "SDDM resilience drop-in does not back off before reacquiring DRM/VT"
if command -v systemd-analyze >/dev/null 2>&1; then
    systemd-analyze --root="$encrypted" verify sddm.service \
        || fail "systemd rejected the installed SDDM unit/drop-in combination"
    merged_unit=$(systemd-analyze --root="$encrypted" cat-config systemd/system/sddm.service)
    for directive in StartLimitIntervalSec=0 StartLimitBurst=10 TimeoutStopSec=20s RestartSec=3s; do
        grep -qx "$directive" <<<"$merged_unit" \
            || fail "systemd did not merge $directive from the ChonkStep drop-in"
    done
fi
grep -qx 'Session=chonkstep-uwsm.desktop' \
    "$encrypted/etc/sddm.conf.d/zz-chonkstep-autologin.conf" \
    || fail "encrypted install does not select the managed session"
cmp -s "$work/99.expected" "$encrypted/etc/sddm.conf.d/99-omarchy-login.conf" \
    || fail "integration modified Omarchy's login configuration"
cmp -s "$work/autologin.expected" "$encrypted/etc/sddm.conf.d/autologin.conf" \
    || fail "integration modified Omarchy's autologin configuration"
cmp -s "$work/99.expected" "$encrypted/etc/sddm.conf.d/99-omarchy-login.conf" \
    || fail "integration modified Omarchy configuration on rerun"
head -n 1 "$config_home/chonkstep/config.toml" | grep -qx 'desktop = "omarchy"' \
    || fail "integration did not select the Omarchy desktop posture"
grep -qx 'scale = 2.0' "$config_home/chonkstep/config.toml" \
    || fail "integration damaged an existing ChonkStep setting"

# A rerun must be a no-op in effect and must not create backup snippets that
# SDDM would parse as additional configuration.
CHONKSTEP_TEST_CONFIG_HOME="$config_home" scripts/omarchy-install-desktop-chonkstep --root "$encrypted"
[ "$(find "$encrypted/etc/sddm.conf.d" -maxdepth 1 -type f | wc -l)" -eq 4 ] \
    || fail "idempotent rerun changed the SDDM snippet set"
[ "$(find "$encrypted/etc/systemd/system/sddm.service.d" -maxdepth 1 -type f | wc -l)" -eq 1 ] \
    || fail "idempotent rerun changed the SDDM service drop-in set"
[ "$(grep -c '^desktop[[:space:]]*=' "$config_home/chonkstep/config.toml")" -eq 1 ] \
    || fail "idempotent rerun duplicated the desktop posture"

explicit_home="$work/explicit-config-home"
install -Dm644 /dev/null "$explicit_home/chonkstep/config.toml"
printf '%s\n' 'desktop = "chonkstep"' > "$explicit_home/chonkstep/config.toml"
CHONKSTEP_TEST_CONFIG_HOME="$explicit_home" scripts/omarchy-install-desktop-chonkstep --root "$encrypted"
grep -qx 'desktop = "chonkstep"' "$explicit_home/chonkstep/config.toml" \
    || fail "integration overwrote an explicit desktop posture"

fresh_home="$work/fresh-config-home"
CHONKSTEP_TEST_CONFIG_HOME="$fresh_home" scripts/omarchy-install-desktop-chonkstep --root "$encrypted"
grep -qx 'desktop = "omarchy"' "$fresh_home/chonkstep/config.toml" \
    || fail "fresh integration did not enable the documented Omarchy preset"
grep -q '^# Focus policy' "$fresh_home/chonkstep/config.toml" \
    || fail "fresh integration did not seed the documented config template"

scripts/omarchy-remove-desktop-chonkstep --root "$encrypted"
assert_absent "$encrypted/etc/sddm.conf.d/zz-chonkstep-theme.conf"
assert_absent "$encrypted/etc/sddm.conf.d/zz-chonkstep-autologin.conf"
assert_absent "$resilience"
cmp -s "$work/99.expected" "$encrypted/etc/sddm.conf.d/99-omarchy-login.conf" \
    || fail "removal modified Omarchy's login configuration"
cmp -s "$work/autologin.expected" "$encrypted/etc/sddm.conf.d/autologin.conf" \
    || fail "removal modified Omarchy's autologin configuration"
assert_file "$encrypted/usr/bin/uwsm"

unencrypted="$work/unencrypted"
stage_package "$unencrypted"
write_fresh_omarchy "$unencrypted"
printf '%s\n' '[Theme]' 'Current=chonkstep' \
    > "$unencrypted/etc/sddm.conf.d/20-chonkstep-theme.conf"
scripts/omarchy-install-desktop-chonkstep --root "$unencrypted"
assert_file "$unencrypted/etc/sddm.conf.d/zz-chonkstep-theme.conf"
assert_file "$unencrypted/etc/systemd/system/sddm.service.d/90-chonkstep-resilience.conf"
assert_absent "$unencrypted/etc/sddm.conf.d/zz-chonkstep-autologin.conf"
assert_absent "$unencrypted/etc/sddm.conf.d/20-chonkstep-theme.conf"
grep -q 'name === "chonkstep (uwsm)"' \
    "$unencrypted/usr/share/sddm/themes/chonkstep/Main.qml" \
    || fail "picker does not default to the exact managed chonkstep session"

# /etc/sddm.conf has higher precedence than conf.d. Refuse to claim success
# when an administrator override makes the requested login setup ineffective.
overridden="$work/overridden"
stage_package "$overridden"
write_fresh_omarchy "$overridden"
printf '%s\n' '[Theme]' 'Current=site-theme' > "$overridden/etc/sddm.conf"
if scripts/omarchy-install-desktop-chonkstep --root "$overridden" \
    > "$work/override.log" 2>&1; then
    fail "installer accepted an ineffective /etc/sddm.conf theme override"
fi
grep -q "resolves Theme/Current to 'site-theme'" "$work/override.log" \
    || fail "installer did not diagnose the effective SDDM override"

echo "test-omarchy-install: encrypted, picker, idempotence, removal, and precedence checks passed"
