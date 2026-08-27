#!/usr/bin/env bash
# Mac-side dev loop for the omarchy-arm64 Lume VM. The repo on the Mac is
# the source of truth; this pushes the working tree into the VM over ssh,
# rebuilds there (a native aarch64-linux build, the real product), asks the
# running chonkstep to hot-restart in place (scripts/restart.sh's marker
# mechanism — existing windows survive via the X11 SaveSet), and captures
# the VM display to a PNG for review.
#
# The VM setup (ssh key, how it was built) is documented in
# ~/VMs/omarchy-arm64. Requires lume and rsync on the Mac. Screenshots are
# taken inside the guest over X11 and copied out; nothing here connects to
# the VM's VNC server, whose framework-side implementation crashes the VM
# when clients attach (VZVNCServer assertion, see lume-*.ips crash logs).
#
# Usage: scripts/dev-vm.sh [sync|build|restart|shot|loop]
#   sync     rsync the repo into the VM at ~/chonkstep
#   build    sync, then cargo build --release in the VM
#   restart  build, then touch the hot-restart marker
#   shot     capture the VM screen to ~/VMs/omarchy-arm64/shots/latest.png
#   loop     restart + shot: the full edit-compile-see cycle (default)
set -euo pipefail

cd "$(dirname "$0")/.."

KEY="$HOME/VMs/omarchy-arm64/id_ed25519"
SHOT_DIR="$HOME/VMs/omarchy-arm64/shots"

ip=$(lume ls | awk '$1 == "omarchy-arm64" { print $(NF-2) }')
if [ -z "$ip" ] || [ "$ip" = "-" ]; then
    echo "omarchy-arm64 VM is not running (no IP from 'lume ls')." >&2
    echo "Start it: lume run omarchy-arm64 --display native" >&2
    exit 1
fi

vm() { ssh -i "$KEY" -o ConnectTimeout=5 "omarchy@$ip" "$@"; }

do_sync() {
    rsync -az --delete --exclude target --exclude .git \
        -e "ssh -i $KEY" ./ "omarchy@$ip:chonkstep/"
    echo "synced -> omarchy@$ip:chonkstep/"
}

do_build() {
    do_sync
    vm 'cd ~/chonkstep && cargo build --release --workspace'
    echo "built release in VM"
}

do_restart() {
    do_build
    vm 'mkdir -p ~/.local/state/chonkstep && touch ~/.local/state/chonkstep/restart'
    echo "hot-restart requested"
}

do_shot() {
    mkdir -p "$SHOT_DIR"
    # Give the WM a beat to re-exec and redraw before grabbing the frame.
    sleep 2
    # SDDM hands the session a random per-boot xauth file; lift it from the
    # running WM's environment rather than guessing.
    vm 'XAUTHORITY=$(tr "\0" "\n" < /proc/$(pgrep -x chonkstep)/environ | sed -n "s/^XAUTHORITY=//p") DISPLAY=:0 import -window root /tmp/chonkshot.png'
    scp -q -i "$KEY" "omarchy@$ip:/tmp/chonkshot.png" "$SHOT_DIR/latest.png"
    echo "$SHOT_DIR/latest.png"
}

case "${1:-loop}" in
    sync)    do_sync ;;
    build)   do_build ;;
    restart) do_restart ;;
    shot)    do_shot ;;
    loop)    do_restart; do_shot ;;
    *) echo "usage: $0 [sync|build|restart|shot|loop]" >&2; exit 1 ;;
esac
