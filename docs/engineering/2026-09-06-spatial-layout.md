# Three views of the same desktop

Baseline: `8acbc30` (`feat/overview-window-drag`), including finger-following
workspace and Overview gestures. No existing spatial layout engine was found.
The untracked maintainer notes are left alone. This change builds on that HEAD.

The subsequent [adversarial review](2026-09-07-adversarial-review.md) records
additional lifecycle/protocol fixes, hardened escaped session records, expanded
native tests, and revised measurements. Results below describe the initial
implementation; the follow-up supersedes its setup-counter resolution limit.

## Ownership

`wm-core::WindowManager` owns workspace mode, ordered membership, remembered
freeform content geometry, output affinity, Mosaic proportions and Flow widths.
The existing global workspace row stays global. Each mode arranges its windows
independently within each output's reserved workarea. Floating and minimized
windows retain their positions in the ordered membership list. Neither renderer,
shell nor input adapter owns a second copy of layout policy.

Core and backend currently exchange root-relative device-coordinate rectangles,
including pre-scaled decoration extents. The solver uses output-local logical
coordinates; its boundary adapter converts positions and dimensions once using
that output's scale. It must not reinterpret the existing core geometry contract.
Output affinity is independent of Flow's offscreen geometry. Hardware identity,
then connector, then the current primary output resolves affinity after hotplug.

Entering a managed mode snapshots freeform geometry once. Mode changes, reflow,
temporary floating edits, minimize, maximize and fullscreen never overwrite that
snapshot. Returning to Freeform restores it through the existing reachability
rules. A subsequent visit begins a new snapshot. Explicitly floating windows,
dialogs/transients, fixed-size and rule-selected utility windows stay floating.
Maximize/fullscreen temporarily suspend participation and restore it afterwards.
Shade explicitly floats a managed window before rolling it up; rejoining unshades.

## Layout and interaction

Mosaic uses deterministic balanced columns (rows on portrait outputs), with
one large first region for three windows. No tree or tree commands. Weighted
partitions retain user proportions; minimum sizes constrain boundaries. Windows
whose constraints cannot fit remain reachable as floating exceptions rather
than breaking the other cells. Flow is one horizontal sequence per output, with
remembered useful widths and a focus-following viewport. Up/down within Flow is
an intentional no-op. Spatial movement reorders the same membership list.

Managed drags use whole-cell targets and a retained native preview. Release
commits reordering; cancellation commits nothing. Floating is always explicit.
Focus stays on the same client during mode changes, reflow, resizing and toggles;
only existing focus operations change it. Destruction removes the generational
client identity from all membership state before selecting a successor.

## Presentation and protocol boundary

Reuse `gesture_physics::Spring`, native surface trees, retained render elements,
GPU clipping and the existing frame/deadline loop. A layout transaction records
source/destination rectangles and stages final geometry once through the existing
configure queue. Scene transforms animate live committed buffers to the final
rectangles; client ack/commit remains asynchronous and cannot hold the animation
open indefinitely. No screenshots, per-frame configures or animation workers.
Interrupted reflow starts at its currently displayed geometry. One cleanup path
settles transforms when a modal/gesture/lock/output change takes ownership.

The small mode caption is native, input-transparent and transient. Layout IPC
is projected from core state. Omarchy's dwindle/scrolling names map to
Mosaic/Flow; its layout-toggle launcher is translated directly into a native
action. Super+T floats/rejoins, Super+L toggles Mosaic/Flow, and Super+Shift+L
returns to Freeform. Freeform's floating toggle changes nothing. Inapplicable
layout messages deliberately succeed quietly. IPC exposes `tiledLayout`, actual
floating membership and the standard `changefloatingmode` event.

## Integration and validation

Living Desktop extends its atomic, debounced store with optional layout metadata;
legacy workspace state loads as Freeform. Invalid workspace modes fall back to
Freeform; invalid window metadata is ignored without losing the base record.
Saved ordering is applied independently of application startup order. Floating
edits have a separate monitor-relative rectangle; the original freeform rectangle
stays intact. The saved focused client reconstructs Flow's useful viewport.
The shell's existing live Overview and drag/drop paths continue to use core
membership. Managed arrangements inform Overview placement without changing
gesture ownership or semantic workspace focus during a partial gesture.

Behavior tests cover solver bounds and constraints, geometry preservation,
membership/lifecycle, focus, output affinity, persistence, spring interruption,
native rendering/configure counts, scaling and existing gestures/Overview.
Fixed counters record reflow duration, affected windows, transition setup and
actual configure flushes without production logging. They are exposed through
the existing opt-in test socket. `calculation_us` measures the whole core reflow,
including chrome/geometry staging; `setup_us` measures presentation setup.
Existing frame counters include nested presentation waits, so their `render_us`
must not be reported as CPU rendering cost. The existing memory-profile build
measures allocations separately from shipping-build timings.
Headless nested E2E can verify live pixels and protocol behavior; physical
multi-output cadence, input latency and subjective feel require hardware checks
and will be reported separately from automated evidence.

The native acceptance test drives Freeform arrangement, animated Mosaic, floating
edits/rejoin, Flow navigation/resize/reorder, minimize/restore, Overview from every
mode, workspace gestures and the return to original freeform geometry at 1x,
1.5x and 2x. A separate real SIGKILL/relaunch test checks membership, proportions,
width, floating edits and empty-workspace modes. Pointer tests cover previews,
commit, Escape rollback, modal takeover and disappearing clients. Mixed-output
affinity, scale conversion, unplug/reconnect and workspace deletion are exercised
through the core backend contract; the nested host exposes one output at 60 Hz.
Spring/interpolation tests exercise 60/120/144/240 Hz and delayed frames. The
cached motion path allocates zero bytes across 144,000 sampled frames; this is
not a claim that the entire Wayland renderer or application commits allocate
nothing.

Reproduce with `scripts/check.sh all`, `cargo test --workspace`, and
`scripts/e2e.sh --headless`. For shipping-build product measurements, build
`chonkstep-wayland --release` and run the native `spatial_layout` target with
`CHONKSTEP_WAYLAND_BIN` pointing to that preserved binary. Build the same binary
with `--features memory-profile` for allocation measurements. The native
benchmark reports 4/8-window modes, rapid focus, repeated mode changes and
floating/rejoining, and asserts no idle render loop after settlement.

Shipping-build samples on the isolated 1280x800 Weston/llvmpipe host:

| Operation | Core reflow | Final configures |
| --- | ---: | ---: |
| 4-window Mosaic | 724 µs | 4 |
| 4-window Flow | 545 µs | 4 |
| 8-window Mosaic | 592 µs | 8 |
| 8-window Flow | 826 µs | 8 |

The 144-reflow focus/mode/membership sequence took 13.2 ms total in core and
emitted 368 configures. Presentation setup is below the counter's one-microsecond
per-window resolution in most release samples; a reported zero is rounding,
not zero work. In the separate allocation build, a 120 ms transition interval
allocated 61,357 bytes in 394 operations including renderer, client commits and
test IPC. The complete stress sequence retained 9,162,781 bytes before and
9,121,045 after. These are local samples, not hardware latency guarantees.
The stress test also exposed and fixed a probe-client leak: replaced `wl_buffer`
and `wl_shm_pool` objects must be destroyed so repeated configures cannot exhaust
the test runtime's storage quota.

Validation completed: strict Clippy, private rustdoc, workspace tests, the 26
Python harness tests, formatting checks for the new Rust modules, allocation
tests and the full native Wayland suite. The final lifecycle changes also passed
all five native layout tests and ten existing desktop regression tests in the
release build. The full suite explicitly skipped three optional Chonkcraft tests
because their game fixtures were absent. Native screenshots were inspected for
the mode caption, Mosaic/Flow drag previews and Flow's live Overview. Physical
multi-output and high-refresh input/rendering remain hardware validation work.
