# Build merged packages once, then promote them

The first release optimization reused recent main CI but still rebuilt the Arch
packages after tagging. The Ubuntu test binaries cannot serve as release assets:
the packages need native Arch and Arch Linux ARM libraries, release optimization,
installation checks and matching split debug symbols.

The pipeline now has one native package producer, `package.yml`:

1. PRs run the existing validation jobs. They do not build release packages.
2. A push to main runs validation and both native package builds concurrently.
   Both packages and both debug packages are inspected and attested, then retained
   as immutable Actions artifacts for 14 days.
3. A release tag looks for the latest main push run for that exact commit. If it
   is still queued or running, the release waits for it instead of building again.
   A successful run supplies the packages while its artifacts remain available;
   its validation can also be reused if the run started within 24 hours.
4. Release assembly downloads those artifacts by run ID and artifact IDs. It
   requires all four expected files, rejects download digest mismatches, and
   verifies each package's attestation against the repository, shared package
   workflow, source commit and workflow commit. It then creates checksums and
   publishes the same files. There is no Cargo compilation in this path.

The 24-hour validation limit is deliberate: an old green run does not bypass
current dependency auditing indefinitely. Fresh validation can run while reusing
the already built packages; age alone does not require recompilation. Missing,
expired or incomplete artifacts cause native builds; missing, stale or
unsuccessful validation causes fresh CI. The
fresh validation and package builds remain concurrent. A failure to observe an
active main run stops the release rather than launching competing work. A skipped
job is accepted only alongside its explicit reuse proof. The fresh and reused
paths share the same package verification before publication.

Package source identities are `v<version>+git.<full commit SHA>`. Unlike
`git describe`, this value stays unchanged when a tag is added to the commit.
The tag, Cargo version and Arch package version/revision must agree even when no
build is needed. A version bump therefore belongs in the merged commit being
released; changing the version afterward creates a new commit requiring a build.

This moves native packaging to each main push, including commits that are never
released. Main CI consequently waits for the slower of validation and native
packaging, while PR validation keeps its existing cost. It avoids a second build
when that main commit is released. The prior package rehearsal made both
architectures available in 16m24s; promotion itself has not yet been timed on a
merged commit, so this change makes no measured release-duration claim.

`workflow_dispatch` rehearses the complete build/download/provenance path without
publishing a release. Only preview tags enable the final publishing step.

Regression tests cover waiting for active CI, rejecting PR/wrong-commit evidence,
newer failures, expired or incomplete package pairs, API failures, tag-independent
identity, mismatched versions, missing debug packages and failed attestations.

The workflow uses GitHub's documented [cross-run artifact downloads](https://docs.github.com/en/actions/how-tos/writing-workflows/choosing-what-your-workflow-does/storing-and-sharing-data-from-a-workflow)
and [attestation verification policies](https://cli.github.com/manual/gh_attestation_verify).
