# M3 fullscreen scanout

The conservative primary-plane policy accepts the swapchain format and its
opaque equivalent with the same modifier. For example, an ARGB8888 swapchain
can scan out XRGB8888. The reverse substitution, unrelated channel layouts,
and differing modifiers remain rejected. DMA-BUF feedback advertises only
those formats actually supported by the output's primary plane.

Plane capabilities, scene eligibility, framebuffer import, atomic validation,
buffer retention, and composition fallback still apply. This does not enable
the `primary-scanout-any` experiment or change the M3 session's opt-in policy.

## Motivation and measurements

On an M3 Pro J516S, vkQuake 1.36.0 `timedemo demo1` at native 3456x2234,
120 Hz, scale 2, with the owner's existing quality settings measured
48.0, 47.8, and 47.8 FPS with composition. The machine was undocked, on battery,
running `7.1.12-m3-reliability1-20260927`, ChonkStep `95ba5a3`, and private
Honeykrisp/Zink 26.1.4 (`d796ddd471`).

The live trace identified two blockers: demo playback left a software cursor
visible, and the client's linear XRGB8888 buffer failed an exact-format check
against the compositor's ARGB8888 swapchain. Diagnostic overrides allowing
primary formats and hiding the cursor produced 52.9 FPS with primary scanout
and no composited elements. A separate vkQuake fix reactivating game input
at demo startup produced 52.6 FPS under the same format override, without
globally hiding the cursor. These single profiled diagnostic runs establish
the path's potential; they are not measurements of this new compositor.

The first native candidate (`03ff5e1`) did not engage scanout. A live
argument trace showed AR24/LINEAR versus XR24/Invalid: the client's explicit
LINEAR layout was lost in the single-fd GBM import path. The corrected exporter
preserves the original DMA-BUF modifier for ADDFB2 and format comparison;
it does not infer LINEAR for implicit buffers or relax the modifier check.
A hardware regression test fails on the first candidate and passes with this
fix, including all 7,720,704 scanout-memory reference pixels.

A newly mapped window also received no pointer enter until the first physical
motion. Reconcile pointer focus after window focus is applied so games can
hide their cursor and activate pointer locks under a stationary pointer.
The new nested enter/lock test fails on the first candidate and passes at
scales 1 and 2 with the correction. All 14 pointer-constraint and 8 fullscreen
integration tests pass, along with the 41 session tests, strict release Clippy
and release build.

On performance1 with AC charging, ordinary composition and a pointer already
inside the game, compute16 measured48.8/49.1/49.3FPS; the return-to-compute1
check measured47.9/47.7. These runs do not measure this corrected compositor.

The corrected compositor (`fb28717`) passed its approved native boot on the
same performance1 kernel, with render/compute limits 16, the internal panel
at 120 Hz, and battery power. Ordinary fullscreen demo1 now uses
`primary: true`, `zero_copy: 1`, `composited: 0`; the arbitrary-format override
is off and the application controls cursor visibility. No pointer movement
or global cursor hiding was needed.

Three unprofiled runs at unchanged native resolution and quality measured
54.2, 54.4, and 54.2 FPS (median 54.2). Disabling direct scanout temporarily
on this same boot measured 48.8 and 49.0 FPS (median 48.9), a 10.8%
throughput improvement with direct scanout.
Direct scanout was restored afterward. The GPU checks pass 33 dependent
compute dispatches, 96 render references, and 300 Wayland frames with no new
faults. The owner confirms a clean desktop and normal keyboard/trackpad input.
Native menu entry, console entry via the keyboard, and windowed mode correctly
return to composition; the console startup command alone closed the initial
console and was not a valid console-entry test.

The 60 FPS goal remains open. External-display qualification is deferred until
the owner is docked.

## Qualification

Use the normal direct-scanout opt-in (`CHONKSTEP_M3_DIRECT_SCANOUT=1`), keep
`primary-scanout-any` off, and preserve application-requested cursor visibility.
Verify `primary: true`, `zero_copy: 1`, and `composited: 0` for eligible opaque
fullscreen content. Menu/console entry, windowed mode, visible cursors, and
overlapping content must return to composition whenever the scene requires it.
Repeat the native timedemo with unchanged quality settings and check output
correctness and GPU fault logs before treating this as a qualified improvement.
