# 0.6.0 release review

This review covers the 65 non-merge commits from `preview-v0.5.0` to
`origin/main` at `658f816`, merged through PRs #152, #153, #165–#169, #171 and
#289–#293. It also covers the release fixes below, which are not yet on main:
shaded windows in Overview, lost appearance requests, browser click offsets, a
fullscreen resize loop, the test-reliability work, and frames for client-drawn
apps. This is a prerelease through `preview-v0.6.0`.

## Defects reproduced and fixed

| Trigger | Defect | Fix and executable regression |
| --- | --- | --- |
| Shade a window, then open Overview (classic or cards) and pick its card | The card drew the full window, from live content or a stale unmapped snapshot, and the pick unrolled it | Overview carries a per-window `draw_content` flag, cleared for shaded windows; its commit calls `activate_from_overview`, which focuses without unshading while ordinary activation still unrolls. `overview::a_shaded_window_stays_shaded_through_overview` checks both styles, pixels under the strip, Escape and Enter; unit tests `an_overview_pick_keeps_a_shaded_window_rolled_up` and `a_shaded_window_keeps_its_content_hidden_in_overview` |
| A GTK4/libadwaita app that draws its own header bar maps (Files, Pinta) | No ChonkStep frame: only a Close button and no usable resize edges | Themed borders and resize edges around the header bar, tiled state to drop the shadow, minimize and maximize added to an uncustomized GTK button layout; `[decorations] frame_client_drawn = true` for the full titlebar; `client_side` still leaves an app bare. A new capability rather than a regression: `client_edge_frames` drives a probe that binds the KDE decoration manager and creates no decoration object at 1x, 1.5x and 2x (edge frame at map, an exact east-border drag, `frame_client_drawn` and `client_side` by live reload) plus real Nautilus; `decoration.rs` unit tests pin the precedence; `button_layout` tests show a user's layout is never overwritten. A pre-release review of this work found and fixed four more cases: a maximized window changing chrome on reload kept its content size and overflowed its work area (`a_maximized_window_keeps_filling_its_work_area_when_its_chrome_changes` fails without the refit), a `server_side` rule still told a declared client-side window it was tiled, KDE's `None` mode gained edges instead of staying bare, and the button layout publisher wrote without `dconf` to confirm the key was unset |
| `echo light > ~/.local/state/chonkstep/appearance-request` while the shell polls | A poll between the redirect's truncate and its write read an empty file and deleted it; the request was lost | An empty request file younger than one second is left for its writer, and an abandoned one is still consumed afterwards; `docs/appearance.md` now gives the 100 ms poll interval. `appearance::tests::an_empty_request_file_is_a_write_in_progress_not_a_dropped_request` |
| A framed native window settles after fullscreen or a resize with a committed size that differs from the frame interior | Its buffer stayed stretched per axis, so browser clicks and selections landed a few pixels off | Stretch only while `xdg::resize_pending` holds for the surface; a settled buffer is drawn and hit-tested 1:1. `pointer_coordinates::a_settled_buffer_smaller_than_its_fullscreen_interior_is_not_stretched` drives a probe that answers fullscreen with its old 400x300 buffer; with the fix reverted the probe receives (93.75, 93.75) for a motion at (300, 250), the 1280/400 by 800/300 stretch. A buffer that already fills the interior is never stretched: on the pull request's first CI run Chromium's full-size fullscreen buffer was stretched by its stale windowed geometry while a configure was in flight, and the page saw a click at CSS (40, 40) as (24, 25). `pointer_coordinates::a_buffer_filling_its_fullscreen_interior_is_not_stretched_by_stale_geometry` pins a full-size buffer under a stale 400x300 geometry with a configure left unacknowledged |
| A fullscreen client answers its configure with a different buffer (found while writing the regression above) | wm-core adopted the committed size, the fullscreen reflow resized the window straight back, the backend answered that unchanged resize with the same commit, and the compositor repainted about twenty times a second without ever going idle | A fullscreen client's committed sizes and configure requests are ignored, as a maximized axis already is. `manager::tests::a_fullscreen_client_cannot_resize_itself_out_of_the_fullscreen_rectangle` fails without the change (a second backend resize); the lagged-fullscreen e2e's barrier times out without it |
| Repeat Overview, theme and screenshot transients in the heap high-water test | Resident pages rose because glibc keeps freed arena pages mapped and later transients first touch them; the test read that ratchet as a leak | The repeat check compares glibc live allocations (`mallinfo2` in-use plus mmapped bytes) from a new `heap-in-use` test-door query with the warm baseline; extent and arena-size bounds are unchanged. `memory::overview_theme_and_screenshot_transients_do_not_raise_the_heap_high_water` |
| The capture-tool tests take a diagnostic screenshot right after a pointer motion | The screenshot could be taken before the motion was painted | The diagnostic helper waits for a door barrier before writing its screenshot marker |
| First compositor boot on a fresh CI runner | 15 silent seconds paging in the graphics stack consumed a fixed 45 s boot timeout | `scripts/e2e.sh` pre-reads libEGL, the EGL vendor libraries and their dependencies, and the software rasterizer driver; the harness boot wait fails after 10 s with no log output, CPU time, page-ins or storage reads, capped at 120 s |
| Mac-mode LibreOffice Writer copy followed at once by an application switch and paste | The paste could beat Writer or the browser publishing the selection and paste the previous clipboard | Poll the clipboard for the copied text before switching; exact text assertions are unchanged. `mac_mode::libreoffice_and_browser_exchange_document_text` |
| A test uses the XWayland display as soon as "XWayland ready" is logged | A pager message sent before the compositor watches the root window was never seen | `Session::x11_display` waits for the EWMH readiness line |
| One failing test binary during `scripts/e2e.sh` | Cargo stopped at the first failing binary and hid failures in later ones | `cargo test --no-fail-fast` in both headless and hosted runs |
| `x = x or false` then `if x then` in a Hyprland Lua file | Unbounded recursion: an optimized build looped forever, a debug build overflowed its stack, at startup or after a save to `~/.config/hypr` | Fuel for name lookups, depth and per-statement operator bounds, and a per-read statement and directive budget. `hostile_input_never_panics_and_always_yields_something` aborted with a stack overflow at `31d7e3e` |
| A Hyprland IPC request with Unicode whitespace, such as `dispatch<U+00A0>workspace 2` | Slicing inside a multi-byte character panicked on the compositor thread, and abort-on-panic ended the session | Split with `split_once`. Unit tests in `request.rs` and `dispatch.rs`, and a protocol sweep of all 25 whitespace characters, all panicked before |
| A locker requests a lock surface on a `wl_output` that was just unplugged | `unwrap_or(0)` indexed an empty output list and panicked inside Wayland dispatch; with outputs remaining, the surface was filed under the primary | The lock surface's output is optional and the surface renders nowhere; the session stays locked. Two e2e cases with `chonk-lock-probe --stale-output`; with the fix reverted the compositor panics |
| `exec_cmd("notify-send \"build finished\"")` from Omarchy's menu | The string ended at the escaped quote and `notify-send \` ran with an `ok` reply; `cmd=` inside a string was taken for a field | A Lua 5.4 literal reader for dispatch arguments. Three protocol tests failed before |
| Dock, undock or plug in a projector while hyprsunset or a gamma client holds an output | Every hotplug wiped gamma and CTM slots; night light stopped responding silently and a restore on exit was dropped | Slots keyed by output identity and carried across hotplug; departed controls are sent `failed`. Three `gamma` e2e cases fail against the previous compositor |
| Unplug an earlier monitor while a DPMS client holds a later one | Index-keyed controls failed on a connected monitor or switched off a newly plugged one; no `failed` event | Controls and ownership keyed by `WeakOutput`. Ledger unit tests, one of which fails if unplugged claims are kept |
| `focuswindow`, a notification click or launch-or-focus on a window on another workspace (Desktop mode) | Keyboard focus went to a window that was not on screen | Bring the target's workspace into view first; refuse focus off screen; mark urgent while locked. wm-core tests fail against the previous manager; IPC e2e covers classic and Lua spellings |
| An output-management scale change or connector hotplug while `config.toml` does not parse | The session was rebuilt from defaults (bindings, input, theme), and a docked monitor lost its rule | Restyle and place from the running session's state. e2e cases for a broken file, an unreloaded edit and a plugged output |
| Toplevel image capture of a client drawing its own shadow | Capture shifted by the shadow margin and cropped at the right and bottom | Anchor at the scene's presentation offset. Pixel comparison against an output capture at 1x and 1.5x fails on the first pixel with `capture.rs` reverted |
| Omarchy 4's stock `input.lua` computes `kb_layout` at runtime | The expression text became the layout; libxkbcommon fell back to a keymap without Compose | Refuse runtime values; fill layout and variant from `vconsole.conf`. Three reader tests failed at the previous commit |
| `hl.monitor` with output, mode, position and scale | `ok` after applying only the scale; after hotplug the IPC mode mirror named another output | Parse and apply every key or refuse the request; resync the mirror on hotplug. Protocol and e2e cases |
| JBR/Vulkan client commits a delayed GPU buffer during a resize drag on a 150% display | Density inferred from a temporary buffer/viewport mismatch fed wrong sizes back, and the window jumped; a matching frame mid-drag released the retained density too early | Keep the density until the size is committed at it and the drag ends. `older_gpu_buffer_during_resize_cannot_change_window_density`; `a_matching_frame_mid_drag_does_not_release_the_resize_density` fails against the first integrated build |
| Screen or area recording at a fractional display scale | The crop exceeded the captured frame and recording failed immediately | Plan regions with the actual scale, capture rounding and even physical bounds; `capture_tool` covers recording playback across display scales |
| Omarchy's touchpad key sends `hl.device({ name, enabled = false })` | Refused silently; the script showed "Touchpad disabled" while the device stayed live | Serve the verb; drop events and release holds. e2e with a hotplugged pointer; each new test fails with the implementation disabled |
| Omarchy's `idle_inhibit = "fullscreen"` rule on a windowed Steam client | Read as always on; the session never dimmed or locked | Rule modes none, always, focus, fullscreen. e2e case for windowed, fullscreen and back |
| Omarchy's touchpad-only `natural_scroll` and 0.4 scroll factor | Every mouse scrolled backwards at 0.4 of a detent | Separate mouse and touchpad scroll classes; the fraction carried per focus. Reader and scroll-module tests |

## Test reliability

A census of CI failures found 41 distinct tests failing across 61 runs. Nearly
all were already fixed on main by named commits. Most were deterministic
regressions hidden because cargo stops at the first failing test binary, not
timing noise. Examples of such fixes on main since 0.5.0:

- `21a7771` and `02d5746`, comparison fixtures aligned at fractional scales
- `11d84cf` and `afd6d03`, IME readiness on Ubuntu's keyboard data
- `99f1b40`, Quickshell reported unavailable on Ubuntu's Qt
- `50e6f6c`, `a92bd05`, `1341662`, `b72a9b1` and `87d3f5b`, expectations updated
  for Help, default preview tiles, fixed-size clients and logical coordinates
- `94dc0fc`, capture test IPC sent as one buffer

The remaining live causes are in the table above: the appearance race, the
browser stretch, the heap high-water ratchet, the unpainted pointer motion, the
cold graphics-stack boot, the Writer clipboard wait and XWayland EWMH readiness.
Each was fixed at its root. `scripts/e2e.sh` now runs every test binary, so one
failure no longer hides others.

An intermittent failure is treated as a defect in the product or the test. No
retries, `#[ignore]`s or rerun-until-green steps were added, and no exact pixel,
text or clipboard assertion was relaxed. The heap test's resident-memory bound
is replaced by a live-allocation bound, which still fails on a real leak.

## Limits

Output disable and mirroring over Hyprland IPC remain refused (#174, #186). An
early fullscreen request is applied one configure after map. The `hl.monitor`
change is covered in a nested session with split outputs, not a real
connector's mode set, and its two-output hardware check is not recorded.
Several issues' own nested acceptance runs (#173, #172, #221) were not part of
their commits; unit and protocol tests cover the same paths. No finite test run
guarantees compatibility with every application, input device or monitor
topology.

## Validation

On the release candidate `82b4982` and the test fix after it, on an aarch64
laptop, in headless Weston sessions run one at a time:

- `scripts/check.sh lint`, `docs`, `unit` and `wayland-unit` pass.
- The complete headless end-to-end suite ran 386 tests. 385 passed.
  `restore_after_miniaturize_is_a_real_focus_cycle` failed on every run because
  it judged its zenity dialog hidden by the window's own mapped flag, and the
  dialog now wears edge chrome and hides with its frame. With the check reading
  the frame, the `e2e` target passed 10 of 10 and that test three times.
- Repeated runs of the fixed tests all passed: the appearance switch 10 times,
  the capture tool's fractional window click 10 times, the heap high-water test
  3 times, browser text selection twice (12 tests), shade through Overview 3
  times and the lagged-fullscreen coordinates test 4 times.
- Each regression was run with its fix reverted. The lagged-fullscreen test
  never settles without the fullscreen size fix, and receives (93.75, 93.75)
  for a motion at (300, 250) without the stretch fix. The fullscreen size and
  maximized chrome refit unit tests fail without their fixes. A backend
  replay guard written during the investigation made no difference to the
  regression and was left out.
- Real clients exercised: Chromium (Wayland and XWayland), LibreOffice Writer,
  Nautilus with edge chrome, zenity and Fcitx.
- The pull request's first `wayland` run failed
  `browser_text_selection_and_repeat_at_fractional_scale`: a full-size
  fullscreen buffer was still stretched by Chromium's stale windowed geometry
  while a configure was pending. The fill rule and its regression above fix
  it; the job was not rerun to get past it.

The required `test`, `wayland` and `lint` jobs on the pull request, and main's
push run for the merge commit, gate the `preview-v0.6.0` tag.
