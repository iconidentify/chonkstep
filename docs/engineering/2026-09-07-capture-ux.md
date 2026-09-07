# Capture UX and release build follow-up

The toolbar's Window mode previously selected a window on hover but committed
only through Capture or Enter. Moving to Capture could replace the intended
target while crossing another window. A click now captures the highlighted
window in both quick and toolbar modes. A cached camera sprite replaces the
crosshair in Window mode, with the lens as its hotspot. Controls still use the
normal pointer. Both ends of the click must belong to the same window; dragging
out of toolbar padding or changing modes during a press cannot capture it.

Tests drive the actual Window toolbar button, inspect camera pixels at 1x, 1.5x
and 2x, verify saved window dimensions and review launch, and verify that capture
consumes its click and releases keyboard input. Existing tests cover quick
capture, retained areas, clipboard bytes, occlusion, cancellation and recording
badge input ownership.

## Playback investigation

A report described thumbnails appearing in Omacut without video playback for
both screen and area recordings. The available machine runs an older installed
ChonkStep session, so reproduction used the 0.4.0 source revision `3c12310` in an
isolated nested compositor, not the installed 0.3.2 binary.

The 0.4.0 baseline generated a 302x202 H.264 MP4 with a zero start time and
3.033-second duration. The installed Omacut 0.4.0 displayed it, advanced its
playhead on Space and responded to seeking. A second baseline probe exercised
the actual background-worker launch of Omacut after recording a terminal whose
background alternated red and blue. The MP4 contained both colors across 239
frames, and Omacut displayed the changing preview with an advancing playhead.

This does **not** reproduce or explain the reported playback failure on the
affected machine. No decoder workaround or global Qt setting is justified by
these observations. A failing clip, decoder diagnostics or the affected machine
is still needed before declaring that symptom fixed.

The old recording test checked metadata and one decoded frame. The new
regression decodes the full screen and odd-sized area recordings, requiring
multiple red/blue transitions inside the client's content. Pointer motion alone
cannot satisfy it. Failed MP4 conversion also retains the muxer's stderr beside
the recoverable MKV and reports that diagnostic path.

The changing-content regression exposed another real recording defect in CI:
the nested backend's advertised output includes an EGL flip, but recording had
used its unflipped layout transform. wf-recorder 0.4.1 left the video upside
down; 0.6.0's automatic transform hid that defect locally and applied the flip
after the crop instead of before it. Recording now uses
`entry.output.current_transform()` so its explicit normalization matches the
actual screencopy buffer. Both 0.4.1 (in an isolated Ubuntu container) and the
host's 0.6.0 pass the area and screen motion tests with this correction. This
orientation defect does not establish the cause of the reported Omacut stall.

Local artifacts, including the baseline executable, probe source, videos,
Omacut screenshots and command logs, are retained outside the repository at
`/home/chrisk/src/chonkstep-capture-ux-artifacts`.

## Release build work

The 0.4.0 GitHub release run `34137573202` took **32m43s**. Validation ran for
about 12m30s before package jobs could begin. On x86_64, the release compilation
then took **13m08s**, followed by **3m45s** of debug test compilation. The release
build unnecessarily compiled and linked every E2E probe with thin LTO, although
the package installs only five binaries.

The package build now selects exactly those five executable targets. A
regression compares the target list with both packaging recipes and verifies
that Cargo failures propagate. The full workspace debug tests remain in each
native architecture's package check. Validation and package building run
concurrently, and publication depends on both. Already compressed package
artifacts are uploaded without another compression pass. Release optimization,
debug symbols, package verification, checksums and attestations are preserved.

Tags may reuse the latest successful main-branch push CI for the exact commit
when it started within the preceding 24 hours. Wrong revisions, PRs, failed or
cancelled runs, old runs, and API errors require fresh CI. Tests exercise these
fallbacks, including a newer failure following an older success. Publication
explicitly checks native package success and either successful fresh validation
or verified reuse; a skipped job alone never permits publication.

A local build of the five shipping binaries succeeded in 2m09s using the
existing dependency cache. The native package rehearsal `34142960605` then
measured these release compilation times on the same runner classes:

| Architecture | 0.4.0 release | Five shipping binaries | Reduction |
| --- | --- | --- | --- |
| x86_64 | 13m08s | 9m52s | 25% |
| aarch64 | 6m42s | 5m49s | 13% |

Both native package jobs passed tests, package verification and attestations;
the last package finished 16m24s after workflow start. The original release took
32m43s including publication. The rehearsal itself is recorded as failed
because its full Wayland gate exposed the orientation defect described above;
it is not a successful-release measurement. The corrected source is qualified
separately, and no 0.4.1 release has been published from this rehearsal.

Local qualification passed 2,049 Rust unit/doc tests, strict workspace Clippy
and documentation checks, the 15-test capture suite plus the MP4 failure test,
49 Python harness tests, four validation-proof tests, Actionlint, ShellCheck and
the Omarchy installer integration tests. Hardware decoder behavior on the
reporter's machine remains unverified.

The corrected implementation `21f2348` passed the complete GitHub CI run
`34143790174`, including the full Wayland suite (12m31s), unit tests, lint,
SDK/harness checks, installer integration and dependency audit. The follow-up
commit to this report changes documentation only.
