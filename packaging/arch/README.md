# Arch and AUR packaging

`PKGBUILD` builds the current branch as `chonkstep-git`.
`PKGBUILD-release` is the source-of-truth recipe for the stable AUR
package `chonkstep`.

Pushing a tag whose name matches the workspace version, for example
`v0.2.0`, runs `.github/workflows/aur.yml`. The job copies the release
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
