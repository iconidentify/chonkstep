# M3 Xwayland OpenGL

The M3 session can run X11 OpenGL clients on zink/Honeykrisp using a private
Mesa prefix that includes `libGLX_mesa.so.0`. The kernel alone does not enable
this path. The original session prefix supplied EGL and Vulkan but lacked GLX.

Smithay starts Xwayland with a cleared environment. ChonkStep now explicitly
passes the session's graphics variables, including its software-rendering
policy. The Mesa dma-buf filter uses the spawned Xwayland child's PID rather
than the compositor-created socket pair's credentials. Xwayland must pass the
same driver-mapping, environment and secure-execution checks as native clients;
it is not exempt from the filter. The small Smithay API patch is recorded in
`vendor/README.md`.

## Session selection

After building the compositor, select a qualified prefix in
`~/.config/chonkstep/m3gpu-session.env`:

```sh
CHONKSTEP_M3_CLIENTS=gpu
CHONKSTEP_M3_MESA_PREFIX=/absolute/path/to/qualified-mesa-prefix
CHONKSTEP_M3_XWAYLAND_GLAMOR=1
```

The prefix must include the display-specific drirc selections already required
by the M3 launcher, as well as GLX. The launcher checks these before starting
and clears an inherited `XWAYLAND_NO_GLAMOR` when glamor is selected. Applications
must inherit the prefix environment: admitting Xwayland checks the X server,
not the Mesa libraries of each separate GLX application. Bundled or sandboxed
graphics stacks require separate qualification.

Log out and start the M3 session again to replace both the compositor and its
X server. Restarting just the game cannot change an existing Xwayland server's
DRI3 support. Set `CHONKSTEP_M3_XWAYLAND_GLAMOR=0` to restore software Xwayland
on the following login. Keep the previous binary and prefix available.

## Local qualification, 2026-09-26

Machine: J516S, Apple M3 Pro; running kernel
`7.1.12-m3-main-08c90aa8b`. Private Mesa:
`~/src/m3-gpu-work/mesa-eryk-prefix-20260926`, with the existing M3 drirc copied
from the previous prefix. No Mesa source or system Mesa changes were made.

- Requalified against main `ff93e71`: release compositor build and strict
  workspace Clippy passed. The `wm-wayland` and `chonk-shell` library suites
  passed 715 tests (including all 11 Mesa guard tests); 13 special-fixture
  tests were ignored in that invocation. The native test below ran separately.
- The ignored kmsro scanout-memory test passed on
  `/dev/dri/renderD128,/dev/dri/card2`: 7,720,704 pixels, zero mismatches.
  This test does not take DRM master, modeset or page-flip the live desktop.
- A private Weston headless host ran the patched ChonkStep compositor with
  its Mesa guard enabled and its own rootless Xwayland server. GLX reported
  `zink Vulkan 1.4(Apple M3 Pro (G15S B1) (MESA_HONEYKRISP))`.
  Context creation, 256-pixel render readback and presented X11 pixel readback
  passed without Kopper or global Gallium-driver overrides.
- JBR 25.0.2 selected `sun.java2d.opengl.GLXGraphicsConfig`; its volatile image
  was accelerated and returned the expected red pixel.
- The installed, unmodified Chonkcraft `2026.0916.152` jar ran its OpenGL menu
  with the repository's read-only `ChonkGameObserver` and a temporary game home.
  At scale 1, content followed requested sizes 940×700, 620×460 and 800×600.
  Compositor screenshots confirmed the picture scaled after redraw completed.
- At scale 2, synthetic pointer events through the nested compositor's test
  socket dragged the actual frame corner. AWT menu content grew from 398×227
  to 475×264 logical pixels. Screenshots showed the larger menu, and a real
  pointer click opened Campaign Game afterward. The menu keeps its 4:3 aspect
  ratio; widening alone can increase the surrounding black borders.
- No M3 GPU fault or native DCP flip failure was recorded during these checks.

These are isolated compositor tests plus a native display-buffer test, not a
claim that the existing desktop process has already switched to the new build.
The menu tests do not qualify map gameplay or performance. Initial screenshots
taken immediately after a resize caught unfinished redraws; settled screenshots
were used to verify the rendered result. No game assets belong in this repo.
