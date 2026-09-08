# Pinta window controls

The reported session ran packaged ChonkStep 0.4.3, Pinta 3.1.2, and GTK
4.22.4 at output scale 2. Pinta drew its own header bar. Its modal About
window was open, disabling the main window's controls. In an isolated
session without About, the installed compositor successfully handled
Pinta's maximize button and restore request.

There were two compositor defects in edge resizing:

1. `input.rs` limited frameless input to window geometry plus eight device
   pixels. GTK declares a twelve-*logical*-pixel resize band outside the
   window geometry through `wl_surface.set_input_region`. Most of that band
   was unreachable at scale 2. The root-surface fallback also swallowed
   points the client's input region explicitly excluded.
2. Client-initiated resizing placed the dragged edge directly under the
   pointer. Starting in the shadow therefore made the window jump by the
   distance between the pointer and the visible edge.

The native frameless path now uses the surface tree's input regions and
existing coordinate transform, including subsurfaces and fractional scaling.
Excluded pixels fall through the stacking order. Client-requested resizing
records the initial pointer position and applies movement relative to it,
preserving the opposite edge and the existing size-hint/workarea constraints.
The decision applies to native windows generally; it uses no application list.

GTK's input-shape calculation is in
[`update_realized_window_properties`](https://github.com/GNOME/gtk/blob/4.22.4/gtk/gtkwindow.c).
Window geometry and input regions have different purposes in the
[xdg-shell](https://gitlab.freedesktop.org/wayland/wayland-protocols/-/blob/main/stable/xdg-shell/xdg-shell.xml)
and [Wayland core](https://gitlab.freedesktop.org/wayland/wayland/-/blob/main/protocol/wayland.xml)
protocols.

## Regression evidence

`client_input_regions` uses real Wayland clients and pointer events. Its
three scale tests failed against `/usr/bin/chonkstep-wayland` before the fix:
the first resize handle resolved to `root`, instead of `content`, at 1x,
1.5x, and 2x. The corrected tests cover all eight handles, asymmetric shadow
extents, exact client coordinates, input holes, out-of-buffer points, an
authorized resize request, and release ending the resize. A separate
overlapping-window test verifies delivery to the application underneath.

The core offset test exercises all eight edges with grabs inside and outside
the edge, on framed and frameless windows (32 cases). A stationary pointer
must leave the original geometry unchanged; movement must preserve the
opposite edge. A resize without a known pointer position is refused.

Real Pinta was also driven in private headless Weston sessions at both 1x
and 2x, with isolated application settings and a fresh unsaved image:

| Scale | Maximize | Restore | Resize from 10 logical pixels outside the right edge |
| --- | --- | --- | --- |
| 1x | 1280 x 800 | 1100 x 750 | 1170 x 750 after 70 device pixels of movement |
| 2x | 2560 x 1600 | 2200 x 1500 | 2340 x 1500 after 140 device pixels of movement |

The application committed buffers matching those sizes. Screenshots confirmed
one header bar with working window controls. Local diagnostic source, protocol
logs, and screenshots were retained under `/tmp/chonkstep-pinta-review/`.
The automated regression suite uses its own protocol fixture so CI does not
depend on Pinta's packaging or GTK theme.

Run the regression suite with:

```sh
scripts/e2e.sh --headless --test client_input_regions
cargo test --locked -p wm-core --lib
```

Final validation passed:

- `scripts/check.sh`: strict workspace Clippy, documentation, 2,054 Rust
  tests, and 65 harness tests. The ordinary Rust run left 289 ignored tests;
  it does not substitute for the separate integration runs.
- 52 targeted integration tests across `client_input_regions`,
  `interactive_request`, `pointer_coordinates`, `pointer_constraints`,
  `xwayland_input`, GTK frameless resize, and Chromium resize at scale 2.
- The four new integration tests repeated against the optimized release
  compositor using `CHONKSTEP_WAYLAND_BIN`.
- Both `chonkstep` and `chonkstep-wayland` built with `--release --locked`.

The reviewed Wayland binary's build ID is
`bd73d188050cf7f710fd48888f4b7836825d71c4`, with SHA-256
`3654eaa7c0e9b4ea157749d9caa0e901f374f63a7e02b87ed2cd43c245cdea74`.
After user approval, both release binaries were installed atomically in
`/usr/bin` and ChonkStep was restarted. The previous binaries are backed up
in `/var/lib/chonkstep/local-backups/window-controls-20260908T053407Z/`.
The running compositor's `/proc/1343/exe` hash matches the reviewed binary;
its new IPC endpoint reports DP-1 at 3840 x 2160 and scale 2. Installation
and restart verification logs are in
`~/.local/state/chonkstep/window-controls-update/`.

Applying a rebuilt compositor requires a new Wayland session or a restart.
As documented in the README, restarting disconnects existing Wayland clients.
The live Pinta document was left open during investigation; the restart was
performed only after the user approved installing and restarting the build.
