#!/usr/bin/env bash
# Real X11 client + WM checks in a disposable, network-disabled X server.
set -euo pipefail
cd "$(dirname "$0")/.."
command -v Xvfb >/dev/null || { echo 'check-x11.sh needs Xvfb on PATH' >&2; exit 1; }
cargo build --locked -p chonkstep -p chonk-dock -p chonk-dockclock
scratch=$(mktemp -d "${TMPDIR:-/tmp}/chonk-dock-x11.XXXXXX")
server_pid=''
cleanup() {
    if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
    echo "X11 test artifacts: $scratch"
}
trap cleanup EXIT
export XDG_RUNTIME_DIR="$scratch/runtime" XDG_CONFIG_HOME="$scratch/config"
export XDG_DATA_HOME="$scratch/data" XDG_STATE_HOME="$scratch/state"
export XDG_DATA_DIRS="$scratch/data" XDG_SESSION_TYPE=x11
export CHONK_DOCK_X11_TEST="$scratch"
target="${CARGO_TARGET_DIR:-target}"
case "$target" in /*) ;; *) target="$PWD/$target" ;; esac
export CHONKSTEP_X11_BIN="$target/debug/chonkstep"
export CHONK_DOCKCLOCK_BIN="$target/debug/chonk-dockclock"
unset WAYLAND_DISPLAY WAYLAND_SOCKET CHONKSTEP_CONTROL_SOCKET
mkdir -m700 -p "$XDG_RUNTIME_DIR" "$XDG_CONFIG_HOME/chonkstep" "$XDG_DATA_HOME" "$XDG_STATE_HOME"
cat > "$XDG_CONFIG_HOME/chonkstep/config.toml" <<'CONFIG'
desktop='omarchy'
hyprland_config=false
theme='nextstep-classic'
scale=1.0
minimized_previews=false
[keybindings]
'super+m'='miniaturize'
CONFIG
Xvfb -displayfd 3 -screen 0 1280x800x24 -nolisten tcp -ac 3>"$scratch/display" >"$scratch/xvfb.log" 2>&1 &
server_pid=$!
for _ in {1..100}; do
    if [ -s "$scratch/display" ]; then break; fi
    kill -0 "$server_pid" || { cat "$scratch/xvfb.log" >&2; exit 1; }
    sleep 0.05
done
display_number=$(cat "$scratch/display")
if [[ ! "$display_number" =~ ^[0-9]+$ ]]; then
    echo 'Xvfb did not publish a display number' >&2
    exit 1
fi
export DISPLAY=":$display_number"
cargo test --locked -p chonk-dock --test x11_client -- --ignored --nocapture --test-threads=1
