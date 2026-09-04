#!/usr/bin/env bash
# Validate a release package from the outside, on its native architecture.
set -euo pipefail

if [ "$#" -ne 3 ]; then
    echo "usage: $0 PACKAGE EXPECTED_ARCH EXPECTED_VERSION-PKGREL" >&2
    exit 2
fi

package="$1"
expected_arch="$2"
expected_version="$3"

# Arch keeps Qt's development tools outside the default shell PATH. Make the
# distribution's canonical Qt 6 location visible to this verifier and to the
# packaged verify-install.sh it invokes below.
if ! command -v qmllint >/dev/null 2>&1 && [ -x /usr/lib/qt6/bin/qmllint ]; then
    PATH="$PATH:/usr/lib/qt6/bin"
    export PATH
fi

case "$expected_arch" in
    x86_64|aarch64) ;;
    *) echo "unsupported expected architecture: $expected_arch" >&2; exit 2 ;;
esac

for command in bsdtar desktop-file-validate file ldd qmllint; do
    command -v "$command" >/dev/null 2>&1 || {
        echo "release verifier needs $command" >&2
        exit 2
    }
done

[ -f "$package" ] || { echo "package not found: $package" >&2; exit 1; }
[ "$(uname -m)" = "$expected_arch" ] || {
    echo "package must be verified natively on $expected_arch, not $(uname -m)" >&2
    exit 1
}

pkginfo="$(bsdtar -xOf "$package" .PKGINFO)"
metadata_value() {
    sed -n "s/^$1 = //p" <<<"$pkginfo" | head -n 1
}

[ "$(metadata_value pkgname)" = chonkstep ] || {
    echo "release archive does not contain pkgname=chonkstep" >&2
    exit 1
}
[ "$(metadata_value pkgver)" = "$expected_version" ] || {
    echo "release archive version is $(metadata_value pkgver), expected $expected_version" >&2
    exit 1
}
[ "$(metadata_value arch)" = "$expected_arch" ] || {
    echo "release archive architecture is $(metadata_value arch), expected $expected_arch" >&2
    exit 1
}

if bsdtar -tf "$package" | grep -Eq '(^/|(^|/)\.\.(/|$))'; then
    echo "release archive contains a path outside its install root" >&2
    exit 1
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
bsdtar -xf "$package" -C "$stage"

"$stage/usr/lib/chonkstep/verify-install.sh" --root "$stage"

for binary in \
    chonkstep \
    chonkstep-wayland \
    chonk-netjoin \
    omarchy-export-themes
do
    path="$stage/usr/bin/$binary"
    [ -x "$path" ] || { echo "missing executable: $path" >&2; exit 1; }
    case "$expected_arch" in
        x86_64) file "$path" | grep -q 'x86-64' ;;
        aarch64) file "$path" | grep -q 'ARM aarch64' ;;
    esac || { echo "$binary is not an $expected_arch ELF" >&2; exit 1; }
    if ldd "$path" 2>&1 | grep -q 'not found'; then
        echo "$binary has an unresolved runtime library:" >&2
        ldd "$path" >&2
        exit 1
    fi
done

echo "verify-release-package: $expected_arch $expected_version passed"
