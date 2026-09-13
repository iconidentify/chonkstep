#!/usr/bin/env bash
# Open an isolated native modern desktop without changing the current session.
set -euo pipefail
repo="$(cd "$(dirname "$0")/.." && pwd)"
theme="${1:-obsidian}"
case "$theme" in obsidian|washi|relay) ;; *) echo 'usage: preview-modern.sh [obsidian|washi|relay]' >&2; exit 2;; esac
host_backend="${CHONKSTEP_PREVIEW_HOST_BACKEND:-wayland}"
case "$host_backend" in wayland|x11) ;; *) echo 'CHONKSTEP_PREVIEW_HOST_BACKEND must be wayland or x11.' >&2; exit 2;; esac
if [ -z "${XDG_RUNTIME_DIR:-}" ] ||
   { [ "$host_backend" = wayland ] && [ -z "${WAYLAND_DISPLAY:-}" ]; } ||
   { [ "$host_backend" = x11 ] && [ -z "${DISPLAY:-}" ]; }; then
    echo "Run this preview from a desktop with an available $host_backend display and XDG_RUNTIME_DIR." >&2
    exit 1
fi
binary="${CHONKSTEP_PREVIEW_BINARY:-$repo/target/release/chonkstep-wayland}"
exporter="${CHONKSTEP_PREVIEW_EXPORTER:-$(dirname "$binary")/omarchy-export-themes}"
if [ ! -x "$binary" ]; then
    echo 'Build first: cargo build --release -p chonkstep-wayland -p chonk-shell --bin chonkstep-wayland --bin omarchy-export-themes' >&2
    exit 1
fi
if [ "${CHONKSTEP_PREVIEW_BUS:-}" != 1 ]; then
    if [ "$host_backend" = wayland ]; then
        case "$WAYLAND_DISPLAY" in /*) host_display="$WAYLAND_DISPLAY";; *) host_display="$XDG_RUNTIME_DIR/$WAYLAND_DISPLAY";; esac
    fi
    preview_dir=$(mktemp -d /tmp/chonk-preview.XXXXXX)
    mkdir -p "$preview_dir"/{runtime,config/chonkstep,state/omarchy,data,cache}
    chmod 700 "$preview_dir/runtime"
    export XDG_RUNTIME_DIR="$preview_dir/runtime" XDG_CONFIG_HOME="$preview_dir/config"
    export XDG_STATE_HOME="$preview_dir/state" XDG_DATA_HOME="$preview_dir/data" XDG_CACHE_HOME="$preview_dir/cache"
    if [ "$host_backend" = wayland ]; then export WAYLAND_DISPLAY="$host_display"; fi
    unset DBUS_SESSION_BUS_ADDRESS DBUS_STARTER_ADDRESS DBUS_STARTER_BUS_TYPE
    # The daemon inherits the private XDG directories. No activation service
    # directories: previewing must not launch the host's portal, keyring, or
    # other session services against an inherited desktop environment.
    cat >"$preview_dir/bus.conf" <<'EOF'
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <policy context="default">
    <allow send_destination="*"/>
    <allow receive_sender="*"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
    exec dbus-run-session --config-file "$preview_dir/bus.conf" -- \
        env CHONKSTEP_PREVIEW_BUS=1 CHONKSTEP_PREVIEW_DIR="$preview_dir" "$0" "$theme"
fi
preview_dir="${CHONKSTEP_PREVIEW_DIR:?private preview directory is required}"
export CHONKSTEP_BACKEND=winit WINIT_UNIX_BACKEND="$host_backend"
export CHONKSTEP_NO_APPEARANCE_PROPAGATION=1 GSETTINGS_BACKEND=memory
parent_x_display="${DISPLAY:-}"
unset DISPLAY CHONKSTEP_CONTROL_SOCKET CHONKSTEP_TEST_SOCKET CHONKSTEP_SCALE CHONKSTEP_SESSION_CONTINUES HYPRLAND_INSTANCE_SIGNATURE
export CHONKSTEP_HYPRLAND_IPC=1
python3 - "$repo" "$theme" "$preview_dir" <<'PY'
import json,os,shutil,sys,pathlib,xml.etree.ElementTree as ET
repo,theme,root=sys.argv[1:];root=pathlib.Path(root)
fonts=ET.Element('fontconfig')
ET.SubElement(fonts,'include',{'ignore_missing':'yes'}).text='/etc/fonts/fonts.conf'
ET.SubElement(fonts,'dir').text=str(pathlib.Path(repo)/'crates/wm-theme/assets/fonts/modern')
ET.SubElement(fonts,'cachedir').text=str(root/'cache/fonts')
ET.ElementTree(fonts).write(root/'fontconfig.xml',encoding='utf-8',xml_declaration=True)
(root/'config/chonkstep/config.toml').write_text(f'theme={json.dumps(theme)}\ndecoration_style="auto"\nscale=1\nomarchy_shell=true\nomarchy_bar=true\nomarchy_menu=true\nhyprland_config=false\nrestore_session=false\ninteraction_mode="desktop"\n')
plugin=root/'config/omarchy/plugins/chonkstep.agents'
shutil.copytree(pathlib.Path(repo)/'omarchy/plugins/chonkstep.agents',plugin,
                ignore=shutil.ignore_patterns('__pycache__','tests'))
service=root/'data/chonkstep/dockapps/chonk-agents'
service.parent.mkdir(parents=True,exist_ok=True)
service.symlink_to(pathlib.Path(repo)/'examples/chonk-agents',target_is_directory=True)
defaults=pathlib.Path(os.environ.get('OMARCHY_PATH','/usr/share/omarchy'))/'config/omarchy/shell.json'
shell=json.loads(defaults.read_text()) if defaults.is_file() else {}
shell.setdefault('bar',{})['transparent']=False
shell.setdefault('idle',{}).update({'screensaver':86400,'lock':86400})
layout=shell['bar'].setdefault('layout',{})
layout.setdefault('right',[]).insert(0,{'id':'chonkstep.agents'})
(root/'config/omarchy/shell.json').write_text(json.dumps(shell))
PY
export FONTCONFIG_FILE="$preview_dir/fontconfig.xml"
pids=()
cleanup() {
    trap - EXIT INT TERM
    for pid in "${pids[@]}"; do kill "$pid" 2>/dev/null || true; done
    wait 2>/dev/null || true
    echo "Preview logs: $preview_dir"
}
trap cleanup EXIT INT TERM
# Omarchy currently resolves its config and state under HOME directly. Give
# the compositor and its shell private mounts as well as private XDG paths.
# The real desktop and existing shell keep their original mount namespace.
if ! command -v bwrap >/dev/null 2>&1; then
    echo 'The isolated Omarchy preview requires bubblewrap (bwrap).' >&2
    exit 1
fi
preview_command=(bwrap --ro-bind / / --dev-bind /dev /dev --proc /proc
    --bind "$preview_dir" "$preview_dir"
    --bind "$preview_dir/config/omarchy" "$HOME/.config/omarchy"
    --bind "$preview_dir/state/omarchy" "$HOME/.local/state/omarchy"
    "$binary")
if [ "$host_backend" = x11 ]; then
    # Only the compositor connects to the host X server. Its own clients use
    # the private Wayland socket below, including the installed Omarchy shell.
    env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET DISPLAY="$parent_x_display" "${preview_command[@]}" >"$preview_dir/compositor.log" 2>&1 &
else
    "${preview_command[@]}" >"$preview_dir/compositor.log" 2>&1 &
fi
wm_pid=$!;pids+=("$wm_pid")
socket=""
for _ in $(seq 1 100); do
    kill -0 "$wm_pid" 2>/dev/null || { tail -25 "$preview_dir/compositor.log"; exit 1; }
    for candidate in "$XDG_RUNTIME_DIR"/wayland-*; do
        if [ -S "$candidate" ]; then socket="$candidate"; break; fi
    done
    [ -z "$socket" ] || break
    sleep 0.05
done
[ -n "$socket" ] || { echo 'Preview display did not start.' >&2; exit 1; }
# Match the name the compositor exports to its own shell. Quickshell keys
# instances by this literal value, even when an absolute path names the same
# socket. The runtime is already private, so the relative name stays isolated.
export WAYLAND_DISPLAY="${socket##*/}"
# Apply exported tokens to this preview's actual Omarchy shell, through its
# own theme API. Quickshell instance selection stays scoped to the private
# runtime AND nested display; never use --any-display here.
if [ -x "$exporter" ] && command -v quickshell >/dev/null 2>&1; then
    "$exporter" "$preview_dir/themes" >"$preview_dir/theme-export.log" 2>&1
    python3 - "$theme" "$preview_dir" <<'PY'
import base64,os,pathlib,subprocess,sys,time
theme,root=sys.argv[1:];root=pathlib.Path(root)
shell=pathlib.Path(os.environ.get('OMARCHY_PATH',str(pathlib.Path.home()/'.local/share/omarchy')))/'shell'
if not (shell/'shell.qml').is_file():sys.exit(0)
command=['quickshell','ipc','-n','-p',str(shell),'call','--','shell']
deadline=time.monotonic()+10
with (root/'shell-theme.log').open('w') as log:
 while time.monotonic()<deadline:
  try:
   ready=subprocess.run(command+['ping'],capture_output=True,text=True,timeout=2)
   if ready.returncode==0 and ready.stdout.strip()=='ok':
    def payload(name):
     file=root/'themes'/theme/name
     return base64.b64encode(file.read_bytes()).decode() if file.is_file() else ''
    subprocess.run(command+['applyTheme',payload('colors.toml'),payload('shell.toml')],stdout=log,stderr=log,timeout=3,check=True)
    break
  except (OSError,subprocess.SubprocessError) as error:log.write(str(error)+'\n')
  time.sleep(.1)
 else:log.write('Omarchy shell did not become ready; the preview keeps its current bar palette.\n')
PY
fi
python3 "$repo/examples/chonk-agents/chonk-agents.py" serve >"$preview_dir/agents.log" 2>&1 &
pids+=("$!")
case "$theme" in
    obsidian) background=202421; foreground=e0e3d7;;
    washi) background=fffcf0; foreground=34372e;;
    relay) background=1c2235; foreground=d4dcf3;;
esac
cat >"$preview_dir/foot.ini" <<EOF
[main]
font=IBM Plex Mono:size=10
[colors-dark]
background=$background
foreground=$foreground
EOF
foot --config "$preview_dir/foot.ini" --title "Chonkstep · $theme" --window-size-pixels=660x390 \
    sh -c 'printf "\n  CHONKSTEP / MODERN THEMES\n\n  Native windows, shared theme tokens.\n\n  Agent sessions and workspace navigation live in the\n  Omarchy menu bar. No separate dock or bottom switcher.\n\n  This desktop uses an isolated preview profile.\n\n"; exec sleep 86400' \
    >"$preview_dir/foot.log" 2>&1 &
pids+=("$!")
echo "Native $theme preview. Close its outer compositor window to exit. Logs: $preview_dir"
wait "$wm_pid"
