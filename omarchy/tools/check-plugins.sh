#!/bin/bash
# Static checks for the plugins under omarchy/plugins/.
#
#   1. The ControlSocket.qml copies are identical (they must be: each plugin
#      is installed on its own, so the file is duplicated, not shared).
#   2. Every manifest passes Omarchy's own validator when it is installed
#      (`omarchy plugin validate`), or a stdlib re-check of the same rules
#      when it is not.
#   3. qmllint over every .qml file with the Omarchy shell tree providing
#      the qs.Ui / qs.Commons modules, so `import qs.Ui` resolves for real.
#
# Exit status is non-zero on any failure. qmllint warnings are printed but
# do not fail the run: the first-party widgets produce the same classes of
# warning (unqualified access inside inline components), and Omarchy does
# not use `pragma ComponentBehavior: Bound` to silence them.
set -u

here=$(cd "$(dirname "$0")" && pwd)
plugins="$here/../plugins"
shell_root=${OMARCHY_SHELL_ROOT:-$HOME/.local/share/omarchy/shell}
status=0

fail() { echo "check-plugins: $*" >&2; status=1; }

# 1. duplicated ControlSocket.qml
copies=("$plugins"/*/ControlSocket.qml)
for copy in "${copies[@]:1}"; do
  if ! cmp -s "${copies[0]}" "$copy"; then
    fail "ControlSocket.qml differs: ${copies[0]} vs $copy"
  fi
done
echo "ControlSocket.qml: ${#copies[@]} copies, identical"

# 2. manifests
validate=$(command -v omarchy-plugin-validate || true)
for dir in "$plugins"/*/; do
  dir=${dir%/}
  id=$(basename "$dir")
  if [[ -n $validate ]]; then
    "$validate" "$dir" || fail "manifest rejected: $id"
  else
    python3 - "$dir" <<'PY' || fail "manifest rejected: $id"
import json, os, re, sys
d = sys.argv[1]
m = json.load(open(os.path.join(d, "manifest.json")))
assert m.get("schemaVersion") == 1, "schemaVersion must be 1"
for f in ("id", "name", "version", "kinds", "entryPoints"):
    assert f in m, f"missing {f}"
assert re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", m["id"]) and ".." not in m["id"], "bad id"
assert not m["id"].startswith("omarchy."), "reserved id"
assert isinstance(m["kinds"], list) and m["kinds"], "kinds"
assert isinstance(m["entryPoints"], dict), "entryPoints"
for k, v in m["entryPoints"].items():
    assert isinstance(v, str) and v and not v.startswith("/") and ".." not in v, f"unsafe entry point {k}"
    assert os.path.isfile(os.path.join(d, v)), f"entry point missing: {v}"
if "bar-widget" in m["kinds"]:
    assert "barWidget" in m["entryPoints"], "bar-widget needs entryPoints.barWidget"
bw = m.get("barWidget") or {}
if "defaultSection" in bw:
    assert bw["defaultSection"] in ("left", "center", "right"), "defaultSection"
PY
  fi
  # The registry keys the plugin by manifest id, and `omarchy plugin add`
  # names the install directory after it; keeping the directory name equal
  # to the id here means a symlinked checkout behaves like an installed one.
  mid=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$dir/manifest.json")
  [[ $mid == "$id" ]] || fail "directory $id but manifest id $mid"
  echo "manifest ok: $id"
done

# 3. qmllint
qmllint=$(command -v qmllint || command -v /usr/lib/qt6/bin/qmllint || true)
if [[ -z $qmllint ]]; then
  echo "qmllint not found; skipping lint" >&2
elif [[ ! -d $shell_root/Ui ]]; then
  echo "Omarchy shell not found at $shell_root (set OMARCHY_SHELL_ROOT); skipping lint" >&2
else
  # Quickshell exposes the config root as the `qs` module prefix; a scratch
  # directory whose `qs` entry points at the shell tree gives qmllint the
  # same view.
  lintroot=$(mktemp -d)
  ln -s "$shell_root" "$lintroot/qs"
  for qml in "$plugins"/*/*.qml; do
    echo "qmllint $qml"
    out=$("$qmllint" -I "$lintroot" -I /usr/lib/qt6/qml "$qml" 2>&1)
    if [[ ${VERBOSE:-0} == 1 ]]; then
      [[ -n $out ]] && echo "$out"
    else
      echo "  warnings: $(grep -c '^Warning' <<<"$out")  errors: $(grep -c '^Error' <<<"$out")  (VERBOSE=1 for the full text)"
    fi
    grep -q '^Error' <<<"$out" && fail "qmllint error in $qml"
  done
  rm -rf "$lintroot"
fi

exit $status
