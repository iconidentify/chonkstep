# Hybrid layout adversarial review

This review starts at `8acbc30` plus the uncommitted hybrid layout implementation
on `feat/hybrid-spatial-layout`. It is an engineering readiness assessment, not
a claim of exhaustive security certification or commercial release approval.
The user's existing desktop was not replaced or restarted.

## Confirmed defects and repairs

| Finding | Consequence | Repair and regression evidence |
| --- | --- | --- |
| Managed rendering omitted xdg popups | Application menus received input while invisible | Live popup trees use the parent's scene transform and each popup's own scale. Native tests check colored pixels and delivered clicks in all three styles at 1x, 1.5x and 2x. The preserved baseline fails in Mosaic with parent pixels at the popup's input location. |
| Floating a maximized/fullscreen window wrote into its visible geometry | Maximized flags and geometry disagreed; restore could return to a tile | Membership updates the existing restore chain while preserving the active presentation. Tests exercise both states and fullscreen over maximize. |
| Changing maximize underneath fullscreen retained the wrong restore rectangle | Eventually returning to Freeform could lose the original arrangement | Derive maximization from the underlying state; unmaximizing updates fullscreen's restore target. Direct tests and generated lifecycle sequences cover repeated changes. |
| Pinning an offscreen Flow cell retained its virtual position | A pinned window became unreachable on every workspace | Restore its freeform view while pinned, retain managed order, and reflow on unpin. |
| Unpinning on another workspace retained keyboard ownership | Focus could belong to a hidden window | Select the existing visible successor before hiding it. Generated lifecycle coverage checks focus eligibility after every operation. |
| Output moves changed affinity without refitting special presentations | A fullscreen/maximized window remained on its old output | Refit the active presentation through the existing core paths. |
| Lifecycle changes could outlive an interactive grab | Moving/minimizing/maximizing during a drag retained stale targets or rollback | Cancel the affected interaction before mutation; committed cross-output drops use the internal placement path without cancelling themselves. |
| Fast repeated drags counted as titlebar double-clicks | An intended reorder shaded and floated the window | Clear click history after movement; require spatial proximity and wrap-safe timestamps for real double-clicks. Test all three styles. |
| Managed windows never advertised xdg tiled states | Clients could retain inappropriate freeform chrome/edge affordances | Stage the four tiled edge states through the existing deduplicated configure queue and clear them on exclusion. Native tests inspect the client's actual configure. |
| Committed size-hint changes did not reflow Mosaic | Late minimum sizes could invalidate the layout until another operation | Emit change-driven constraint events from native xdg, XWayland and X11 metadata paths. Reflow/cancel through the authoritative core. |
| Unescaped session text could create records/directives | Tabs/newlines in client identity corrupted recovery and could forge workspace modes | Versioned, JSON-escaped text fields; legacy records still load. No client payloads in corruption warnings. |
| Unbounded session reads and predictable temporary writes | Corrupt files consumed unbounded memory; stale temporary symlinks could redirect writes | Bounded regular-file reads, bounded records, owner-only unique atomic replacement, and retry tests. Process-crash atomicity is distinct from power-loss durability. |
| Extreme saved coordinates overflowed restoration arithmetic | Returning to Freeform could panic in debug or choose the wrong monitor in release | Saturating frame/content offsets and a widened exact monitor-distance score. Regression tests restore both signed limits through floating and direct mode exit, retaining size and focus. |
| Flow constructed invisible chrome/surface elements | Unnecessary per-frame imports and allocations | Reuse the renderer's exact surface-tree visibility check before construction; preserve overflow shadows/subsurfaces/popups. |
| Impossible minimum-area cases repeatedly searched every partition | Many demanding clients could stall the event loop | Reject impossible total minimum area before searching, retaining the exact deterministic exclusion policy. |
| Presentation setup rounded each window to microseconds | Fast operations misleadingly reported zero setup cost | Accumulate nanoseconds and convert only when reporting. Separate layout scene-build cost from nested presentation waits. |

The tiling behavior tests now live in `wm-core/src/manager/tests/spatial.rs`.
This keeps policy tests beside their owner without adding another public API or
expanding the already large manager file. The seeded tests exercise 2,000 solver
cases and 3,840 lifecycle operations. They check constraints, nonoverlap,
determinism, unique workspace ownership, focus eligibility and preserved intent.
Neither suite asserts a fixed number of animation frames.

## Product measurements

The preserved before/candidate binaries and raw logs are in the local campaign
directory `../chonkstep-engineering-artifacts/2026-09-07-hybrid-audit`.
The host is nested Weston with llvmpipe, LLVM 22.1.8; these results do not measure
physical input-to-photon latency or KMS high-refresh behavior.

The constrained solver profile (1,000 x 1,000 workarea, 600 x 600 client minima)
changed from 10/692/35,272 µs for 8/64/256 clients to 4/10/56 µs. This is a
synthetic regression case, not an ordinary desktop benchmark. The optimization
does not change which client remains managed.

The native benchmark covers 4/8-window Mosaic and Flow, 112 focus moves,
16 mode changes, and 16 floating/rejoining operations. It measures actual
configure flushes and asserts that settling leaves no idle render loop.
`build_us` measures layout surface/chrome element construction and clipping;
it excludes GPU submission and presentation waits. `render_us` includes those
waits and must not be presented as CPU time. Allocation counts require a
separate `memory-profile` binary and include client commits and test IPC.

The final normal release binary measured 587/525/610/834 µs for 4-window
Mosaic/Flow and 8-window Mosaic/Flow, with 4/4/8/8 final configures. Its
144-operation sequence took 13.58 ms in core and emitted 368 configures.

The final instrumented product run measured 594/511/611/817 µs for 4-window
Mosaic/Flow and 8-window Mosaic/Flow respectively, with exactly 4/4/8/8 final
configures. The 144-operation focus/mode/membership sequence took 13.98 ms in
core, staged 1,152 presentation changes in 652 µs total, and emitted 368
configures. Compare these instrumented timings only with instrumented builds.

An additional live-client benchmark alternates eight-window Mosaic and Flow,
waits for transitions and captions to finish, and measures actual client-paced
frames for 500 ms. Both before and after builds emitted **zero layout calculations
and zero configures** during these settled intervals. Flow allocation operations
per rendered frame changed from 46.7/45.9 to 41.7/41.8 in the two observations.
The new visibility check skipped three quarters of window builds in each Flow sample.
This is an allocation-count reduction, not an allocation-free renderer claim:
requested byte counts varied with client and sampler activity, and Mosaic did
not consistently improve (46.8/44.5 versus 46.6/45.0 operations per frame).

The complete operation sequence changed from 103,312 to 100,725 allocation
operations, with about 103 MB cumulatively requested in either build. Final
Rust live bytes were below their pre-sequence values in both runs, as were the
two settled Flow samples in the final build. These short runs do not establish
a long-term leak bound. The independent cached-motion
test still performs zero allocations across 144,000 interpolated frames.

A separate two-minute workload completed 566 additional client lifecycle cycles
after warmup, including live Overview, theme changes, dock toggling, screencopy
and native Hyprland IPC. Descriptors stayed at 74 (66 non-pipe), threads at 60,
and final windows/frames/clipboard devices at zero. Anonymous-plus-swap memory
rose 5,076 KiB; RSS rose 12,052 KiB. Rust live memory grew from 6,318,245 to
9,903,468 bytes while glyph-image payload grew from 24,975 to 2,133,772 bytes.
The existing raster cache has 16,384-entry/8 MiB soft limits and eviction tests;
this run ended below those thresholds and does not demonstrate a plateau.
Average process CPU use was 0.48 cores in this software-rendered workload,
including its real client and desktop activity. Raw `/proc` samples and allocator
counters are retained under the campaign's `soak` directory.

## Wider project review

- The Rust dependency audit examined 378 resolved packages against RustSec
  database commit `5a0ebedfe8bdd2e295b171f4162f8c977bcad9a5`. It reported zero
  vulnerability-class entries, **but is not warning-free**: `cgmath 0.18.0`
  has unmaintained and unsound warnings; `ttf-parser 0.25.1` is unmaintained.
  The `cgmath` warning concerns `Matrix::swap_columns` with identical indices.
  No invocation exists in ChonkStep or its vendored Smithay source. Smithay
  supplies that transitive dependency; fontdb/cosmic-text supplies ttf-parser.
  An upstream dependency migration remains maintenance work, not a suppressed
  advisory or an undocumented new fork.
- The dependency inventory records package versions, declared licenses, sources,
  repositories, toolchain, host, HEAD and lockfile digest. Every resolved package
  declares a license. This does not audit artwork/font rights or establish legal
  compliance for a binary distribution and its system libraries.
- The vendored Smithay patches already have archive checksums, upstream links,
  attribution and focused lifecycle tests in `vendor/README.md`; preserve that
  discipline when upgrading. This review adds no vendor patch or unsafe block.
- Control/Hyprland sockets already use bounded nonblocking reads/backlogs,
  private paths and peer checks. They intentionally grant session control to
  same-user processes; they are not a sandbox boundary between those processes.
  The native test door remains opt-in. Capture and native client-buffer parsing
  continue to require adversarial protocol and hardware coverage.
- Existing release packaging tests the workspace, inspects package contents,
  and attests binary/debug artifacts. Packaging is now gated on the reusable
  full CI workflow for the **same release revision**, including SDK, installer,
  lint, X11 and nested Wayland tests. A passing earlier branch build is not used
  as evidence for a newly tagged revision.

## Remaining release evidence

`scripts/check.sh all` passed: strict Clippy, private rustdoc, 1,997 Rust tests
(including gesture/Overview/session/allocation coverage), and 26 Python harness
tests. The 254 ignored Rust cases are separately classified display-dependent
or development tests, not counted as passes. The Python SDK, Go SDK race tests,
isolated Omarchy installer fixtures, installed menu/theme fixtures and actionlint
also passed. The complete final native run passed 227 exercised native cases
and three installed Omarchy fixtures. Three optional Chonkcraft cases explicitly
skipped because the external game fixtures were absent; they are not claimed as
coverage. The earlier full run caught two test readers that still assumed legacy
session rows; those readers now accept escaped rows, and the legacy startup
fixture remains. All seven spatial tests passed, including the expanded popup
and live-frame benchmarks, on both final normal and instrumented binaries.
Native screenshots were inspected for the popup fix,
Mosaic/Flow insertion previews and fractional-scale live Overview. The campaign
retains raw logs, source hashes and separate normal/instrumented release binaries.

Reproduce the native gates with `scripts/e2e.sh --headless --release`; select
`--test spatial_layout --nocapture` for product measurements. Point
`CHONKSTEP_WAYLAND_BIN` at an explicitly preserved binary built with
`--features memory-profile` for allocation counters. The sustained run used
`CHONKSTEP_SOAK_SECONDS=120`, `CHONKSTEP_SOAK_MEMORY_STATS=1` and
`CHONKSTEP_SOAK_SELECTION_STATS=1` with `--test stability_soak --nocapture`.

Formatting checks pass for all six new Rust modules and `git diff --check` is
clean. `cargo fmt --all -- --check` still reports drift in 232 files, including
untouched baseline code. A repository-wide formatting migration remains separate
maintenance work; this review does not represent that global gate as passing.

Physical KMS on Intel/AMD/NVIDIA, mixed-output fractional scale, rotation,
hotplug during animation, suspend/resume and 120/144+ Hz input/render cadence
still need a hardware campaign. Headless scale tests and analytic spring tests
do not substitute for those observations. Long-running production workloads,
assistive-input/accessibility review, application-specific UI polish, and a
human evaluation of the complete acceptance sequence remain release gates.

Session replacement survives a process crash atomically. It does not currently
promise durable persistence across sudden power loss; adding fsync on the
compositor thread would require latency measurements and a deliberate I/O design.
The new reader accepts legacy files; older binaries do not understand versioned
escaped rows, so downgrades should preserve a copy of the session state.
