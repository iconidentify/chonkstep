#!/usr/bin/env bash
# Validate a release package from the outside, on its native architecture.
set -euo pipefail

if [ "$#" -ne 4 ]; then
    echo "usage: $0 PACKAGE EXPECTED_ARCH EXPECTED_VERSION-PKGREL EXPECTED_SOURCE_ID" >&2
    exit 2
fi

package="$1"
expected_arch="$2"
expected_version="$3"
expected_source_id="$4"
package_version="${expected_version%-*}"

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

for command in bsdtar desktop-file-validate file ldd qmllint readelf; do
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
    local prefix="$1 = "
    awk -v prefix="$prefix" '
        index($0, prefix) == 1 {
            print substr($0, length(prefix) + 1)
            exit
        }
    ' <<<"$pkginfo"
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

package_entries="$(bsdtar -tf "$package")"
if grep -Eq '(^/|(^|/)\.\.(/|$))' <<<"$package_entries"; then
    echo "release archive contains a path outside its install root" >&2
    exit 1
fi

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
bsdtar -xf "$package" -C "$stage"

"$stage/usr/lib/chonkstep/verify-install.sh" --root "$stage"

# Keep every packaged ELF in one list so presence, architecture,
# dependencies, unwindability, and split debug symbols cannot drift apart.
release_binaries=(
    chonkstep
    chonkstep-wayland
    chonk-about
    chonk-netjoin
    omarchy-export-themes
)
for binary in "${release_binaries[@]}"; do
    path="$stage/usr/bin/$binary"
    [ -x "$path" ] || { echo "missing executable: $path" >&2; exit 1; }
    file_description="$(file "$path")"
    case "$expected_arch" in
        x86_64) architecture_marker='x86-64' ;;
        aarch64) architecture_marker='ARM aarch64' ;;
    esac
    if [[ $file_description != *"$architecture_marker"* ]]; then
        echo "$binary is not an $expected_arch ELF: $file_description" >&2
        exit 1
    fi
    if ! ldd_output="$(ldd "$path" 2>&1)"; then
        echo "ldd could not inspect $binary:" >&2
        printf '%s\n' "$ldd_output" >&2
        exit 1
    fi
    if [[ $ldd_output == *'not found'* ]]; then
        echo "$binary has an unresolved runtime library:" >&2
        printf '%s\n' "$ldd_output" >&2
        exit 1
    fi
    # A shipped binary must still be unwindable from a coredump. The
    # panic hook turns every panic into a SIGABRT so the session
    # supervisor can recover, which makes the core the one durable
    # artifact a lost session leaves — and a core is only a stack if
    # `.eh_frame` survived the packaging strip. It does survive
    # `--strip-all`; this check protects that independent unwind
    # contract while the debug-package checks below protect symbols.
    # Do not use `readelf | grep -q` here. With `pipefail`, grep's successful
    # early exit can SIGPIPE readelf and turn a present section into an
    # architecture/scheduling-dependent false failure. Read the complete
    # output and require the exact section name (`.eh_frame_hdr` is not it).
    section_headers="$(readelf -SW "$path")"
    if ! awk '{ for (field = 1; field <= NF; field++) if ($field == ".eh_frame") found = 1 }
        END { exit(found ? 0 : 1) }' \
        <<<"$section_headers"; then
        echo "$binary has no .eh_frame: a coredump from it cannot be unwound" >&2
        exit 1
    fi
done

# Both session binaries name the exact source used before it became a
# source archive and read their actual linker's GNU build-ID note. Check
# both spellings and compare the report to `readelf`, so this is also a
# test of the stripped binaries users actually install.
for binary in chonkstep chonkstep-wayland; do
    path="$stage/usr/bin/$binary"
    long_version="$("$path" --version)"
    short_version="$("$path" -V)"
    [ "$long_version" = "$short_version" ] || {
        echo "$binary -V and --version disagree" >&2
        exit 1
    }
    grep -Fxq -- "$binary $package_version" <<<"$long_version" || {
        echo "$binary reports the wrong package version: $long_version" >&2
        exit 1
    }
    grep -Fxq -- "source: $expected_source_id" <<<"$long_version" || {
        echo "$binary reports the wrong source identity: $long_version" >&2
        exit 1
    }
    reported_build_id="$(awk -F ': ' '$1 == "build id" { print $2; exit }' \
        <<<"$long_version")"
    elf_notes="$(readelf -n "$path")"
    elf_build_id="$(awk '/Build ID:/ { print $3; exit }' <<<"$elf_notes")"
    if [ -z "$elf_build_id" ] || [ "$reported_build_id" != "$elf_build_id" ]; then
        echo "$binary reports build ID $reported_build_id, readelf reports $elf_build_id" >&2
        exit 1
    fi
done

# And the split has to have actually happened: `options=(strip debug)`
# is what puts the symbols in a -debug package instead of the bin, and
# a packaging change that dropped it would leave crashes unreadable
# again with nothing failing to say so.
# `makepkg` names it "$pkgname-debug-$pkgver-$pkgrel-$arch.pkg.tar*"
# and writes it beside the main package. `$expected_version` is
# already "version-pkgrel" (see the usage line), so the only unknown
# left is the compression suffix. An explicit `if` rather than
# `[ -f ] && ...`: this script runs under `set -e`, where a failing
# test as the last statement of a loop body would end the run instead
# of trying the next candidate.
debug_package=""
for candidate in "$(dirname "$package")"/chonkstep-debug-"$expected_version"-"$expected_arch".pkg.tar*; do
    if [ -f "$candidate" ]; then
        debug_package="$candidate"
        break
    fi
done
if [ -z "$debug_package" ]; then
    echo "no debug package beside $(basename "$package"): the symbols were not split out" >&2
    echo "  expected chonkstep-debug-$expected_version-$expected_arch.pkg.tar*" >&2
    echo "  check that the PKGBUILD still sets options=(strip debug)" >&2
    exit 1
fi
debug_entries="$(bsdtar -tf "$debug_package")"
for binary in "${release_binaries[@]}"; do
    grep -Fq -- "usr/lib/debug/usr/bin/$binary" <<<"$debug_entries" || {
        echo "the debug package carries no symbols for $binary" >&2
        exit 1
    }
done
echo "verify-release-package: debug symbols present in $(basename "$debug_package")"

echo "verify-release-package: $expected_arch $expected_version passed"
