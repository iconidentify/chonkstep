#!/usr/bin/env bash
# The native producer already checked ELF, runtime dependencies and symbols.
# Promotion checks every immutable package's signer and exact source commit.
set -euo pipefail
if [[ $# != 1 ]]; then
  echo "usage: $0 ASSET_DIRECTORY" >&2; exit 2
fi
assets=$(realpath "$1")
cd "$(dirname "$0")/.."
metadata=$(bash scripts/release-metadata.sh)
version=$(sed -n 's/^version=//p' <<< "$metadata")
package_release=$(sed -n 's/^package_release=//p' <<< "$metadata")
shopt -s nullglob
files=("$assets"/*)
[[ ${#files[@]} == 4 ]] || {
  echo 'release must contain exactly two native packages and their two debug packages' >&2; exit 1;
}
for arch in x86_64 aarch64; do
  for name in chonkstep chonkstep-debug; do
    package="$assets/$name-$version-$package_release-$arch.pkg.tar.zst"
    [[ -f $package && ! -L $package ]] || {
      echo "missing release package: $package" >&2; exit 1;
    }
    gh attestation verify "$package" --repo "$GITHUB_REPOSITORY" \
      --signer-workflow "$GITHUB_REPOSITORY/.github/workflows/package.yml" \
      --source-digest "$GITHUB_SHA" --signer-digest "$GITHUB_SHA" \
      --deny-self-hosted-runners
  done
done
