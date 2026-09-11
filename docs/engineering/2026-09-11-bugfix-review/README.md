# Capture, Overview and scale review — 2026-09-11 UTC

This batch addresses GitHub issues #154–157 alongside the pending
`chonkrec --demo` implementation. All four reported mechanisms were reproduced
with failing regressions before their production fixes.

## Fixes and adversarial checks

| Area | Failure / invariant reviewed | Fix and evidence |
| --- | --- | --- |
| Overview / #155 | A content refresh replaced the panel lifetime token and reset gesture progress. | Retain lifetime, progress, render IDs and label backing while refreshing live membership/geometry. Resize, map/focus and 30ms terminal title changes survive opening and closing strokes across three Spaces, both interaction profiles. Existing cancellation, ownership, lock and topology tests remain enforced. |
| Recording boundary / #154 | A live recording badge carried no visible area boundary. | Retained bright inward strips and outward shadow, clipped to the owning output. Input passes inside, outside and through the edge; clean PNG/video exports omit it; demo output includes it; finishing removes it. Tiny and output-edge rectangles, three scales, 10,000 allocation-free iterations. |
| Seek interval / #156 | x264's default maximum GOP was 250 frames, independent of the selected frame rate. | Derive GOP from constant FPS in built-in capture and chonkrec, including recovery re-encoding. Probe actual encoded keyframes, decode complete files, exercise both recorder versions and the real editor. Storage cost is measured, not assumed. |
| Geometry / #157 | `recent_asks` could suppress a legitimate committed density change because its new physical size matched an older resize. | Expire physical-size history when committed density changes or the toplevel unmaps. Pending configure/debt and layout-transition protections remain. Tests cover buffer-scale, larger-buffer viewport density, destination-only viewport changes, reverse changes and live output-scale changes at 1×/1.5×/2×. Pixel bounds and actual titlebar reach are checked. |
| Decoration cache / #157 | Three writers stored rendered extents while the cache compared requested layout extents. | All writers store the requested extent. A theme returning a different extent proves identical inputs do not rasterize repeatedly; changed inputs still repaint. |
| Recorder startup | An empty muxer file was accepted as recording success. | Require a readable video packet, with bounded startup/probe time and worker cleanup. Flush completed fragments promptly so small/low-bitrate captures do not stay buffered. Failure regression plus real one-fps fullscreen capture. |
| Recorder recovery | Stream-copy failure re-encoded with the old GOP default. | Forced join failure drives a real re-encode; completed video retains controls, fully decodes and satisfies one-second keyframe spacing. |

The code review covered the entire branch diff against `14e594b`, plus
interacting gesture validation, xdg configure/ack/commit ordering, output scale
synchronization, scene rendering, decoration cache writers, capture admission
and cache keys, lock transitions, and recorder process/FIFO/finalization paths.
This is a scoped adversarial review and broad automated validation, not a claim
that every possible defect in the repository has been disproved.

The demo capture policy is immutable per connection and present in request
grouping/cache keys for both output-capture protocols. Normal concurrent exports
remain clean. Confined clients do not inherit capture privileges. A lock
transition invalidates pending desktop readback and prevents overlay rendering.
No blanket clipping of client surface trees was introduced: legitimate CSD
shadows, subsurfaces and popups retain their existing behavior.

## Real editor and native measurement

Omacut 0.4.0 opened a fresh 1920×1080/30 recording from the modified recorder.
The actual filmstrip contained twelve distinct images. Four clicks through the
private compositor seat displayed four different pictures in the player.
[Pixel observations](verified.json), [filmstrip](filmstrip.png),
[scrubbed player](scrub-1.png). This was an isolated fixture desktop; the existing
i9beef desktop and the M1 session were not disturbed by editor testing.

The installed version's [thumbnail code](https://github.com/omacom/omacut/blob/v0.4.0/src/ffmpeg.cpp)
already uses accurate FFmpeg seeking. Interframes were never missing. The GOP
change bounds decoder work; it does not establish the cause of every reported
editor stall or guarantee arbitrary players' seek policies.

The native 4K/60 ABBA comparison showed 4.167s → 1.000s maximum keyframe gaps,
+0.50% encoder CPU and **3.29× file size**, at unchanged CRF 18. The requested
“within a few percent” file-size estimate was not met. See
[method, limitations and raw data](../../benchmarks/capture-gop-2026-09-11/README.md).

## Validation

- Failing-before evidence: [Overview](overview-before.txt),
  [boundary pixels](boundary-before.txt), [scale](scale-before.txt),
  [cache](cache-before.txt), [keyframes](keyframes-before.txt), [false recording success](startup-before.txt).
- `scripts/check.sh all`: strict workspace Clippy, private-item documentation,
  2,111 Rust checks (including doctests) and 104 Python harness tests passed.
- `cargo test --workspace --all-targets --locked`: passed.
- `bash -n scripts/chonkrec`, `shellcheck scripts/chonkrec`, `git diff --check`: passed.
- Real recording tests passed on wf-recorder 0.6.0 and 0.4.1, including restart,
  forced recovery re-encode, low-bitrate startup and keyframe spacing.
- Full Wayland suite: 326 checks passed, including installed Omarchy integration.
  Two additional real recorder recovery/startup tests passed separately (328
  distinct checks total); the final viewport-destination matrix also passed.

Raw local logs, native videos and fixtures are retained under
`~/.local/state/chonkstep/issues-20260911`; compact evidence is checked in here.
The native test restored tty2 and retained the original i9beef session.
