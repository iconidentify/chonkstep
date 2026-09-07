# Interactive native desktop gestures

## Architecture and ownership

`wm-core::SwipeTracker` recognizes and owns a complete stream and produces timed
`SwipeMotion`, without knowing about pixels, windows, or outputs. The Wayland
input adapter owns protocol forwarding and cancellation. `gesture_scene` owns
the transient scene, cached neighbors, and the shared spring. The renderer uses
live surface trees, sparse frame textures, scene transforms and GPU clipping.
No client geometry is changed for animation, and no screenshot is taken.

At horizontal axis lock, cache the logical origin and at most its two neighbors.
The final occupied workspace permits one provisional empty neighbor. This is
only scene metadata: the workspace is created by the existing semantic action
after settlement. An empty final workspace has no next neighbor. Reversals can
cross the origin and expose the opposite side in the same stream. One gesture
commits at most one neighboring workspace.

Normal planes collect windows in compositor stacking order with indexed WM
lookups. Overview neighbors prepare the same packing and captions once, without
switching the WM. Overview caches desktop and packed geometry and a linear-time
stack-order index. During a morph, only transforms, control alpha, and furniture
alpha change; buffers and layout are reused. Existing minimized-window fallbacks
reuse small cached previews only when no live surface is available.

The real workspace and semantic keyboard focus stay at the origin until a
commit. Partial scenes return the root from pointer hit-testing; no exposed
neighbor can activate through hover or focus-follows-mouse. A pointer press
cancels and consumes its matching release. Keyboard commands take over. Spring
commits happen before the regular notification, focus and protocol drains, so
activation is not deferred to an unrelated later event. Adjacent exposed live
clients participate in frame callbacks and presentation feedback.

Reserved streams stay compositor-owned after invalidation. Two/five fingers,
disabled gestures, and unconfigured finger counts retain complete client
streams. Existing pointer grabs (including implicit/client grabs), menus,
Alt-Tab, exclusive layers, focus grabs, capture tools and locks suppress desktop
gestures. Device loss/resume/lock clean up immediately. A libinput cancellation
instead springs to the original state and can never commit. Output/topology
changes and replacement of cached Overview contents cancel stale scenes.

## Mapping and release velocity

The configured `distance` remains the slow-release midpoint, with range
24..1000 logical touchpad units. Full span is `2 * distance`, default 160.
Positive `-dx/span` means next workspace; positive `-dy/span` means opening
Overview. Output resolution, scale, scroll direction and scroll multiplier do
not enter recognition or physics. Each output translates by its physical width
times normalized progress, with a clip tied to the translated source viewport.
This prevents content on another head leaking in through a mixed-scale boundary.

Initial axis lock remains 12 logical units and 1.25x dominance. Once locked it
does not oscillate with cross-axis noise. The inline ring retains eight position
and timestamp samples, spaced at least 12 ms apart. High-rate devices therefore
retain roughly 84 ms rather than only their final eight hardware events.
Intervals older than 100 ms are ignored. Velocity is the weighted sum of interval
displacements divided by weighted time, with exponential recency weighting
`exp(-age / 0.035)`. The current endpoint accounts for motion after the last
retained sample. A pause ages velocity to zero; one zero final sample does not
erase the recent trajectory. Timestamp wrapping and invalid motion are guarded.
Samples sharing a timestamp retain the newest endpoint, not the older stored
position. Less than 12 ms of history is insufficient for a velocity estimate.
This prevents a coalesced reversal from resurrecting the earlier direction.

Projection uses 180 ms of estimated velocity, bounded to eight spans/second.
Midpoint is 0.5. A slow short stroke cancels, a slow long one commits, and a fast
short flick can commit. A reverse flick can cancel even past midpoint. Projection
cannot select an opposite neighbor before the fingers actually cross the origin.
All tuning constants and target selection are in `gestures/physics.rs`.

## Spring and edges

The critically damped spring uses angular frequency 22 radians/second. For
offset `x = position - target`, `c = velocity + omega*x`, and elapsed time `dt`:

```
position = target + (x + c*dt) * exp(-omega*dt)
velocity = (velocity - omega*c*dt) * exp(-omega*dt)
```

This analytic solution preserves release position and velocity and is stable
across frame intervals. It is not an Euler integrator or a restarted easing
curve. Negative/nonfinite times do not advance; an interruption exceeding two
seconds snaps to the already-selected endpoint. Malformed state is sanitized.
Settlement snaps exactly when error is below 0.0001 spans and velocity below
0.003 spans/second. Tests compare 30/60/120/144/240/360 Hz and partitioned time.

Beyond an available boundary, for excess `d`, visual excess is
`sign(d) * L*abs(d)/(L+abs(d))`, with `L = 0.18` spans. Its derivative is
`(L/(L+abs(d)))^2`; multiplying finger velocity by that derivative preserves
the visible velocity at spring handoff. This is bounded and nonlinear, not a
hard stop. A fresh desktop swipe can catch a moving spring: inverse resistance
sets its new displacement origin to the exact currently presented position.

## Scheduling and performance discipline

Following updates change scalar state and mark damage. Native KMS uses the
existing vblank events, per-output `FrameClock`, pending-flip gates and render
deadlines. No extra animation thread or permanent timer exists. Only the nested
non-vsynced backend contributes a temporary spring deadline, derived from its
advertised refresh. An idle held gesture requests no frames on its own.

History, projection, resistance and spring stepping allocate nothing. Preparing
neighbors and Overview captions may allocate once at axis lock. Per-frame scene
vectors and surface-tree traversal scratch reuse capacity. Clipping wraps scene
elements in place, without another large vector, framebuffer or readback.
Snapshots for icon previews are suspended during transitions and Overview.

Measured on an Intel Core i9-9900K, release profile, nested Weston with Mesa
llvmpipe (LLVM 22.1.8), not Apple hardware:

- Inline tracker size: 240 bytes.
- 100,000 timed tracker/projection/resistance/spring updates: zero allocation
  calls and zero requested bytes; 9.54 ms total in the final optimized run
  (approximately 95 ns/update; the initial run was 10.58 ms). The allocation suite also drives 100,000 snap
  queries and remains allocation-free.
- 120 warmed following updates: 240 scene-build attempts, 3,326 microseconds
  total CPU scene-building time (13.86 microseconds/build), maximum 38
  microseconds. The additional attempts include test-barrier damage no-ops.
- Those updates submitted 120 frames. Whole render/submission time averaged
  16.44 ms, including the nested display's presentation wait. That number is
  **not** GPU execution time or scene-building CPU cost. The new
  `gesture_build_calls/us/max_us` counters separate those quantities.
- After settlement, the static-client test observes zero render calls across a
  160 ms observation interval; no gesture animation deadline survives cleanup.

These software-path results are not measured physical touch-to-photon latency.
There is no allocation profiler enabled in the timing build. Allocation counts
come from the separate allocation test, not inferred from low CPU time.

## Overview window dragging

[Apple Mission Control](https://support.apple.com/guide/mac-help/work-in-multiple-spaces-mh14112/mac)
and [GNOME Overview](https://help.gnome.org/gnome-help/shell-workspaces-movewindow.html)
both move windows by dragging them to workspace thumbnails. The implementation
uses a scale-aware three-point pickup threshold, preserves the grabbed location,
shrinks without enlarging small windows, and renders the live image at 82%
opacity. The whole thumbnail, including the normally destructive close corner,
is one drop target. A bright outline, tint, and “Move to Desktop N” caption
provide continuous feedback. Drops stay on the source desktop with Overview open.

Press records a stable client identity and acquires a pointer grab immediately.
Release revalidates identity, lifecycle and workspace row. Escape invalidates the
held press while retaining ownership of release. Invalid drops do not activate;
layout changes invalidate a drag rather than moving whatever occupies its old
index. Native pointer updates only move the cached image and selection metadata.
X11 keeps its existing panel fallback with a lightweight moving selection plate.

## Coverage and hardware boundary

Unit/allocation tests cover recognition, finger settings, timing history,
reversal, malformed input/time/state, midpoint/flick projection, resistance and
its derivative/inverse, spring continuity/exact settlement, normalized output
mapping, cached morph endpoints, and drag threshold/anchor arithmetic.

Native Wayland E2E checks intermediate live pixels at 1x/1.5x/2x, untouched WM
geometry, no configure events during morphing, pointer isolation, explicit
Overview semantics, symmetric opening/closing, flicks, reversal, cancellation,
device removal, resume, spring catching, client swipe protocol pass-through,
empty-final-desktop policy, and existing layer-bar boundaries. Overview E2E
checks scale-aware drops, small-motion cancellation, invalid drops, desktop
close corners, keyboard/pointer activation, deletion and live rendering.

The nested harness exposes one output, so physical mixed-output cadence and
actual libinput devices still need hardware validation. Required manual matrix:
Apple/M1 trackpad on Omarchy, another libinput trackpad, 60/120/144+ Hz KMS,
mixed 1x/1.5x/2x heads, output hotplug, lock/unlock and VT suspend/resume. Confirm
finger attachment, reversal on the next composed frame, release continuity,
elasticity and focus with Chrome and native terminals. No headless test certifies
subjective macOS parity, physical latency, or Apple hardware compatibility.

Existing scope boundaries remain explicit: Overview uses ChonkStep's existing
single-output arrangement, while horizontal workspace planes render on every
output. The adjacent plane previews live windows; workspace-local minimized-icon
shell surfaces are reconciled at semantic commit. Current-workspace icons slide
with its plane, bars/dock and pinned windows remain fixed. The X11 login session
does not receive native libinput gestures. Desktop/workspace geometry never
changes just to animate any of these views.

## Preflight record

- `scripts/check.sh all`: strict workspace/all-target Clippy, strict rustdoc,
  1,953 passing Rust tests (including Wayland and allocation tests), and 26
  passing Python harness tests.
- `scripts/e2e.sh --headless`: 223 passing Wayland E2E tests and three passing
  installed-Omarchy checks. Includes real Chrome selection/repeat, capture,
  focus, lock, workspace, Overview, gestures, and protocol regression suites.
- Optimized `desktop_gestures`: nine passing E2E tests; Overview: five passing
  E2E tests, with drag coverage at 1x/1.5x/2x.
- Targeted final reruns cover the explicit pause cleanup, layer ordering and
  interpolated selection outline. The first heavily concurrent preflight
  exposed an existing 16 ms dock-IPC timing test at 18.37 ms; the clean serial
  preflight passed it without changing its implementation or tolerance.
