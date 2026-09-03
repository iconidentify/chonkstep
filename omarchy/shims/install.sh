#!/usr/bin/env bash
# Links (or unlinks) the chonkstep shims for Omarchy's Hyprland-bound
# commands into a directory on your PATH.
#
# Nothing under /usr/share/omarchy or /usr/bin is touched, read, or
# needed: a shim wins by being *earlier on PATH* than the command it
# stands in for, and it is uninstalled by removing one symlink. That is
# the whole mechanism, and it is why this script is a dozen lines of
# ln and rm rather than an installer.
#
#   omarchy/shims/install.sh                 # link into ~/.local/bin
#   omarchy/shims/install.sh --dir /usr/local/bin
#   omarchy/shims/install.sh --list          # what is linked, and who wins on PATH
#   omarchy/shims/install.sh --uninstall
#
# Which directory to pick, and why it matters, is in
# docs/omarchy-integration.md — the short version is that ~/.local/bin
# is ahead of /usr/bin in a *login* shell (which is how chonkstep runs
# every Omarchy menu action) and behind it in the session's own
# environment, so a keybinding that names a bare command needs
# /usr/local/bin or an absolute path instead.

set -euo pipefail

HERE=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
DIR="$HOME/.local/bin"
MODE=install

while (($#)); do
  case $1 in
  --dir)
    shift
    [[ $# -gt 0 ]] || {
      echo "--dir needs a directory" >&2
      exit 2
    }
    DIR=$1
    ;;
  --dir=*) DIR=${1#--dir=} ;;
  --uninstall) MODE=uninstall ;;
  --list) MODE=list ;;
  -h | --help)
    sed -n '2,26p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
    exit 0
    ;;
  *)
    echo "unknown argument: $1" >&2
    exit 2
    ;;
  esac
  shift
done

shims=()
while IFS= read -r path; do
  shims+=("$(basename "$path")")
done < <(find "$HERE/bin" -mindepth 1 -maxdepth 1 -type f | sort)

case $MODE in
list)
  for name in "${shims[@]}"; do
    link="$DIR/$name"
    if [[ -L $link ]]; then
      state="linked -> $(readlink "$link")"
    elif [[ -e $link ]]; then
      state="OCCUPIED by a real file, not this shim"
    else
      state="not linked"
    fi
    # What PATH actually resolves today is the only thing that decides
    # whether the shim is in effect, so say it rather than implying it.
    resolved=$(command -v "$name" 2>/dev/null || echo "not on PATH")
    printf '%-28s %s\n%-28s   PATH resolves to: %s\n' "$name" "$state" "" "$resolved"
  done
  exit 0
  ;;
uninstall)
  for name in "${shims[@]}"; do
    link="$DIR/$name"
    if [[ -L $link && $(readlink -f "$link") == "$HERE/bin/$name" ]]; then
      rm -f "$link"
      echo "removed $link"
    elif [[ -e $link ]]; then
      echo "left $link alone (not a link to this checkout)" >&2
    fi
  done
  exit 0
  ;;
esac

mkdir -p "$DIR"
for name in "${shims[@]}"; do
  target="$HERE/bin/$name"
  link="$DIR/$name"
  if [[ -e $link && ! -L $link ]]; then
    echo "refusing to replace $link: it is a real file, not a symlink" >&2
    exit 1
  fi
  ln -sfn "$target" "$link"
  echo "linked $link -> $target"
done

# The one thing that can be wrong after a successful install, so check
# it rather than leaving the user to discover it.
echo
for name in "${shims[@]}"; do
  resolved=$(command -v "$name" 2>/dev/null || true)
  if [[ $resolved != "$DIR/$name" ]]; then
    printf 'warning: %s still resolves to %s on this PATH\n' "$name" "${resolved:-nothing}" >&2
    printf '         %s must come before it in PATH for the shim to take effect.\n' "$DIR" >&2
  fi
done

cat <<EOF

Two shims are not reached by PATH alone; see docs/omarchy-integration.md:

  * omarchy-launch-shell — chonkstep starts Omarchy's supervisor by its
    absolute path under \$OMARCHY_PATH, so PATH cannot intercept the one
    that runs at login. Set \`omarchy_shell = false\` in
    ~/.config/chonkstep/config.toml and add this shim to \`autostart\`.
  * anything invoked as \`omarchy <subcommand>\` — the omarchy CLI execs
    its scripts by absolute path out of its own directory. Call the
    shimmed command by name (\`omarchy-system-logout\`) rather than
    through \`omarchy logout\`.
EOF
