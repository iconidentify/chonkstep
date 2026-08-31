#!/usr/bin/env bash
# "Build" for a stdlib Python dockapp: there is nothing to compile —
# this only vendors the chonkdock SDK next to the script, so the
# installed copy is self-contained (the SDK README calls vendoring a
# supported way to ship).
#
# chonk-get copies this directory to its install location and runs
# build.sh *there*, so the chonkstep checkout is found rather than
# assumed: an already-vendored copy wins, then the repo the install
# was started from ($OLDPWD survives chonk-get's cd), then an
# importable system copy.
set -euo pipefail
caller_dir="${OLDPWD:-$PWD}"  # before our own cd clobbers it
cd "$(dirname "$0")"

if [ -d chonkdock ]; then
    echo "chonkdock already vendored"
    exit 0
fi

# $CHONKSTEP_SDK pins the SDK directory explicitly; otherwise walk up
# from wherever the install was started, which finds the checkout from
# its root or any directory inside it.
try_sdk() {
    if [ -n "$1" ] && [ -d "$1" ]; then
        cp -R -- "$1" ./chonkdock
        echo "vendored chonkdock from $1"
        exit 0
    fi
}
try_sdk "${CHONKSTEP_SDK:-}"
root="$caller_dir"
for _ in 1 2 3 4 5 6; do
    try_sdk "$root/bindings/python/chonkdock"
    [ "$root" = "/" ] && break
    root=$(dirname "$root")
done
try_sdk "$PWD/../../bindings/python/chonkdock"

if python3 -c 'import chonkdock' 2>/dev/null; then
    echo "using the system-installed chonkdock"
    exit 0
fi

echo "chonk-switch: could not find the chonkdock Python SDK." >&2
echo "Run the install from a chonkstep checkout, or copy" >&2
echo "bindings/python/chonkdock next to chonk-switch.py." >&2
exit 1
