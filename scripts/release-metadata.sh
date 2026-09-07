#!/usr/bin/env bash
# Tag-independent identity lets a release promote the bytes built on main.
set -euo pipefail
cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n1)
package_version=$(sed -n 's/^pkgver=//p' packaging/arch/PKGBUILD-release | head -n1)
package_release=$(sed -n 's/^pkgrel=//p' packaging/arch/PKGBUILD-release | head -n1)
commit=$(git rev-parse HEAD)
[[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][a-zA-Z0-9.-]+)?$ ]] || {
  echo "invalid workspace version: $version" >&2; exit 1;
}
[[ $package_version == "$version" && $package_release =~ ^[1-9][0-9]*$ ]] || {
  echo 'workspace and Arch package versions must agree, with a positive pkgrel' >&2; exit 1;
}
[[ $commit == "$GITHUB_SHA" ]] || {
  echo 'checked-out commit does not match the workflow revision' >&2; exit 1;
}
if [[ $GITHUB_REF == refs/tags/preview-v* ]]; then
  tag_version=${GITHUB_REF#refs/tags/preview-v}
  tag_release=1
  if [[ $tag_version =~ ^(.+)-r([1-9][0-9]*)$ ]]; then
    tag_version=${BASH_REMATCH[1]}
    tag_release=${BASH_REMATCH[2]}
  fi
  [[ $tag_version == "$version" && $tag_release == "$package_release" ]] || {
    echo "tag $GITHUB_REF does not match package $version-$package_release" >&2; exit 1;
  }
fi
printf 'version=%s\npackage_release=%s\nsource_id=v%s+git.%s\n' \
  "$version" "$package_release" "$version" "$commit"
