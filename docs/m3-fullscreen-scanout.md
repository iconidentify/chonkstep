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

The narrow format change passes the session unit tests, including rejected
alpha/layout/modifier substitutions, strict release Clippy, release build, and
eight fullscreen integration tests under a private headless Weston/pixman host.
Native activation and before/after qualification of this exact candidate are
pending. The 60 FPS goal is not yet met. External-display testing is deferred
until the owner is docked; these measurements describe only the internal panel.

## Qualification

Use the normal direct-scanout opt-in (`CHONKSTEP_M3_DIRECT_SCANOUT=1`), keep
`primary-scanout-any` off, and preserve application-requested cursor visibility.
Verify `primary: true`, `zero_copy: 1`, and `composited: 0` for eligible opaque
fullscreen content. Menu/console entry, windowed mode, visible cursors, and
overlapping content must return to composition whenever the scene requires it.
Repeat the native timedemo with unchanged quality settings and check output
correctness and GPU fault logs before treating this as a qualified improvement.
