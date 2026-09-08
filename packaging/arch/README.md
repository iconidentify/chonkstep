# Arch and AUR packaging

`PKGBUILD` builds the current branch as `chonkstep-git`.
`PKGBUILD-release` is the source-of-truth recipe for the stable AUR
package `chonkstep`.

`.github/workflows/github-release.yml` is the account-independent preview
release path. Main CI builds and tests the native x86-64 and ARM64 packages
through `package.yml`, verifies each package on its build architecture, and
records GitHub provenance attestations. A `preview-v$pkgver` tag reuses those
exact artifacts from the successful main run for the same commit, creates
`SHA256SUMS`, and publishes both packages and their matching debug packages.
If main CI is still running, the release waits; unavailable artifacts require a
fresh native build. Set the release version before merging so tagging can reuse
the merged commit's packages.

The AArch64 builder uses Arch Linux ARM repositories. A manual workflow dispatch
exercises package assembly and provenance verification without publishing a
release. See the [release pipeline](../../docs/engineering/2026-09-07-release-promotion.md)
for validation freshness and artifact retention.

Pushing a tag whose name matches the workspace version, for example
`v0.4.3`, runs `.github/workflows/aur.yml`. The job copies the release
recipe to `PKGBUILD`, replaces `SKIP` with the tag archive's real
SHA-256 checksum, generates `.SRCINFO` with Arch's `makepkg`, and pushes
both files to `ssh://aur@aur.archlinux.org/chonkstep.git`.

The repository must define one Actions secret:

- `AUR_SSH_PRIVATE_KEY`: an SSH private key attached to the maintainer's
  AUR account. It should be dedicated to this publisher.

The job rejects a tag whose version differs from the workspace, never
cancels a release already publishing, and makes no commit when the AUR
metadata is already identical. This makes the first version tag the
only manual publication step.
