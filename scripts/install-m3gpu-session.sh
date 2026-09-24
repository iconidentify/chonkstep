#!/usr/bin/env bash
# Registers the opt-in "chonkstep (M3 GPU, experimental)" login session,
# or with --remove unregisters it. This is exactly one new file:
#
#     /usr/share/wayland-sessions/chonkstep-m3gpu-uwsm.desktop
#
# It points at this checkout's scripts/chonkstep-session-m3gpu through
# uwsm, like the "chonkstep (uwsm)" entry scripts/install.sh writes. The
# ordinary chonkstep entries, SDDM's configuration (including its
# autologin session) and every other file are left alone. Nothing here is
# needed for, or used by, the ordinary session. The session itself is
# described in scripts/wayland-session-m3gpu.sh.
#
# Usage: scripts/install-m3gpu-session.sh [--remove]
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$(pwd)"
entry=/usr/share/wayland-sessions/chonkstep-m3gpu-uwsm.desktop

case "${1:-}" in
    --remove)
        sudo rm -f "$entry"
        echo "Removed $entry"
        exit 0
        ;;
    "") ;;
    *)
        echo "Usage: scripts/install-m3gpu-session.sh [--remove]" >&2
        exit 2
        ;;
esac

# Same refusal as scripts/install.sh: SDDM's session wrapper word-splits
# Exec (`exec $@`), so a checkout path with whitespace, quotes or
# backslashes can never start.
case "$repo" in
    *[[:space:]\"\\]*)
        echo "The checkout path '${repo}' contains whitespace, a quote or a backslash;" >&2
        echo "SDDM cannot start a session entry pointing there." >&2
        exit 1
        ;;
esac
[ -x "$repo/scripts/chonkstep-session-m3gpu" ] || {
    echo "$repo/scripts/chonkstep-session-m3gpu is missing or not executable" >&2
    exit 1
}
[ -x "$repo/target/release/chonkstep-wayland" ] || {
    echo "Build the compositor first: cargo build --release -p chonkstep-wayland" >&2
    exit 1
}

# No TryExec on the script, for the reason scripts/install.sh gives: the
# greeter runs as the sddm user and cannot stat a path under a 0700 home.
# uwsm receives a path, which implies its hardcoded-command mode; its unit
# is named after the wrapper (wayland-wm@chonkstep-session-m3gpu.service).
# -N has no spaces because SDDM word-splits Exec.
# DesktopNames stays "chonkstep" so the portal configuration and every
# desktop-specific rule treat this as the ordinary chonkstep desktop.
entry_tmp="$(mktemp)"
trap 'rm -f "$entry_tmp"' EXIT
cat > "$entry_tmp" <<DESKTOP
[Desktop Entry]
Name=chonkstep (M3 GPU, experimental)
Comment=chonkstep composed on the Apple M3 GPU with a private Mesa. Experimental: a GPU fault freezes the desktop until reboot
Exec=uwsm start -g -1 -e -D chonkstep -N chonkstep-M3-GPU ${repo}/scripts/chonkstep-session-m3gpu
TryExec=uwsm
DesktopNames=chonkstep
Type=Application
DESKTOP
sudo install -Dm644 "$entry_tmp" "$entry"
echo "Installed $entry -> ${repo}/scripts/chonkstep-session-m3gpu"
