# Mac interaction validation — 2026-09-10

The seven adversarial review fixes passed **170 distinct end-to-end tests across
20 targets**, **1,945 workspace library tests**, and **68 Python harness tests**.
The committed standalone release passed all **16 Spaces workflows**. See
[review fix validation](#spaces-review-fix-validation) and the
[findings and fixes](mac-spaces-review-fixes.md).

The original per-display Spaces follow-up passed **163 distinct end-to-end tests across
20 targets**, **1,933 workspace library tests**, and **68 Python harness tests**.
The committed standalone release also passed all nine new Spaces workflows;
see [per-display validation](#per-display-spaces-validation) below.

The original Mac profile passed 94 distinct end-to-end tests across nine
targets, plus 1,146 unit tests in the affected libraries. The final GPU-backed
regression run passed 144 end-to-end tests across 18 targets and 1,917 workspace
unit tests; details follow below. This is software
qualification of the documented profile, not a claim of complete macOS or
Apple hardware parity. See [supported behavior and remaining work](mac-mode.md).

Source base: `c7dad60`, ChonkStep 0.4.4. Changes were developed on
`codex/mac-interaction-mode`. Tests ran on this working tree before the Mac
implementation commit. No live desktop configuration was changed.

## Environment

Each end-to-end test starts its own nested ChonkStep Wayland compositor under a
private headless Weston host and session bus. Client profiles, files, and capture
destinations are isolated below `/tmp/chonk-testkit`. Rendering uses Mesa
llvmpipe; this run does not qualify DRM/KMS scanout or physical input devices.
The standard fixture is 1280×800; existing capture and browser regressions also
exercise fractional and double scaling.

| Application/tool | Installed package version |
| --- | --- |
| Chromium | 151.0.7922.173-1 |
| foot | 1.27.0-2 |
| Nautilus | 50.2.2-1 |
| LibreOffice | 26.2.5-3 |
| Weston | 15.0.1-3 |
| XWayland | 24.1.13-1 |
| wl-clipboard | 2.3.0-1 |
| wf-recorder | 0.6.0-2 |
| FFmpeg | 9.0.1-1 |

## Results

Run each target with `scripts/e2e.sh --headless --test TARGET`.

| Target | Passed | Evidence |
| --- | ---: | --- |
| `mac_mode` | 10 | Physical seat commands, actual application edits and saved output |
| `selection_transfer` | 20 | Native/X11 clipboard transport, exact rich payload persistence |
| `capture_tool` | 17 | Existing screenshot/recording destinations, pixels, cursor and video regressions |
| `keyboard_focus` | 7 | Modal focus ownership, IME and inhibitor regressions |
| `selection_lifecycle` | 5 | Clipboard lifetime and XWayland lifecycle regressions |
| `keyboard_repeat` | 13 | Repeat, keymap and configuration lifecycle regressions |
| `xwayland_input` | 13 | Real X11 input delivery and grab regressions |
| `session_lock` | 3 | Existing lock input ownership and lifecycle checks |
| `browser_selection` | 6 | Selection/repeat across Wayland, XWayland and output scales |

All nine targets passed with no missing-client skips. The three ordinary helper
tests filtered from the Mac/browser targets are not nested tests and are not
included in these counts. Installed Omarchy menu/theme checks also passed.

The ten Mac workflows verify:

- Unicode copy, cut, paste, undo and redo in Chromium across Wayland/XWayland;
  invalid reload retains the working mode.
- Browser copy → Command-Q → terminal paste, terminal selection copy → browser
  paste, physical Control-C reaches the PTY, and Command-W closes foot.
- Application switching, hide/unhide, Show Desktop, bounded desktop navigation,
  dedicated fullscreen desktop creation/removal and window overview.
- Force Quit selection, Escape, default Cancel, confirmed native disconnect and
  confirmed XWayland disconnect.
- File-only and clipboard-only PNG destinations, preserved prior clipboard,
  capture cancellation, playable recording with exact dimensions and no editor.
- Word/line navigation, balanced releases after resume, focus transfer and
  application passthrough.
- Both Command keys, Caps Lock, overlapping physical/translated Home presses,
  and a translated release after switching the mode off.
- German/French letter positions, undo/redo and shifted screenshot digits.
- Copying an actual file through Nautilus without changing the source.
- Writer → browser and browser → Writer Unicode text, saved to the document.

Chromium DevTools is used to observe private fixtures and locate fields; it
does not inject keyboard input or issue clipboard API calls. Writer assertions
wait for painted document readiness before copying/saving. A compositor barrier
alone does not prove that a client finished an asynchronous paste.

The added persistence workflow compares exact HTML, PNG, URI-list and
1 MiB+17-byte binary representations after the real source receives Command-Q.
Existing selection tests separately exercise transport, replacement and
backpressure; they should not be mistaken for Mac-mode qualification of every
possible clipboard-manager configuration.

`cargo test -p wm-core -p wm-config -p chonk-shell -p wm-wayland --lib`:
1,146 passed, eight environment-dependent tests ignored. `scripts/check.sh lint`
and `scripts/check.sh docs` passed with the repository's warnings-as-errors
rules. `git diff --check` passed.

Local logs: `/tmp/chonk-mac-e2e-final.log`,
`/tmp/chonk-mac-selection-final.log`, `/tmp/chonk-mac-capture-final.log`,
`/tmp/chonk-mac-focus-final.log`, `/tmp/chonk-mac-regression-*.log`,
`/tmp/chonk-mac-terminal-final.log`, `/tmp/chonk-mac-unit-final.log`,
`/tmp/chonk-mac-lint-final.log`, `/tmp/chonk-mac-rustdoc.log`.

Physical Apple Fn/Globe, all international layouts, Bluetooth/hotplug, complete
application-specific widget behavior, and the other gaps in `mac-mode.md`
remain unqualified. No “every application” or “full macOS parity” claim follows
from these results.

## Final GPU-backed regression

The final run used `scripts/e2e.sh --headless --host-renderer gl --release
--test TARGET` with a private Weston GL host on the real RTX 3090. All nine
targets above passed again; `mac_mode` now has **11** workflows. Another nine
targets passed: `omarchy_terminal` (2), `screencopy_pressure` (8), `image_capture`
(5), `capture_cache` (1), `hidden_surface_damage` (2), `hyprland_ipc` (19),
`fullscreen` (5), `overview` (5), and `idle` (2). Total: **144 distinct E2E tests**,
no missing-client skips. Repeated installed-menu/theme helper checks are not
included in that total.

Real GPU scheduling exposed a copy/focus race in Writer: it published its offer
after Command-Tab changed focus, and Wayland correctly refused ownership from
the unfocused source. Mac copy ordering now defers subsequent keyboard commands
until an offer arrives or the bounded deadline expires. The added E2E sends
physical Command-C, Command-Tab and Command-V without intervening compositor
barriers, delays the browser's ordinary copy handler by 150 ms, and verifies
exact Unicode/multiline bytes in foot's PTY. It also verifies balanced Command
release, physical Control-C, and prompt recovery after a copy with no selection.
Writer's existing two-way document exchange passes on the GPU host too.

The Nautilus fixture now waits for its file row to paint before selecting and
copying; directory enumeration is asynchronous and a compositor barrier alone
does not establish that readiness. The test still performs one real copy action.

Final workspace library tests: **1,917 passed, 12 environment-dependent tests
ignored**, across 18 library targets. Strict workspace Clippy, private Rustdoc,
optional GPU-producer Clippy, and all 68 Python harness tests passed. The
separately invoked ignored two-GPU hardware test also passed; see [GPU audit evidence](gpu-audit-work.md).

Logs: `/tmp/chonk-final-TARGET.log` (substitute each target; idle's successful
retry is `/tmp/chonk-final-idle-retry.log`), `/tmp/chonk-gpu-final-unit2.log`,
`/tmp/chonk-interop-lint.log`, `/tmp/chonk-final-rustdoc.log`, and
`/tmp/chonk-texture-fixture-lint.log`. The initial idle attempt collided with a
source edit during compilation; the completed-source retry passed. This was not
an idle runtime failure. Physical Apple input and native KMS remain unqualified.

## Per-display Spaces validation

Implementation: `fe81b40db86f7d7483c72e8931233b29d0b5bb3d`, on
`codex/mac-display-spaces`. See [behavior and configuration](mac-display-spaces.md)
and the [machine-readable results](benchmarks/mac-spaces-2026-09-10/validation.json)
for individual test names, log checksums, and the release record.

The development regression runs used
`scripts/e2e.sh --headless --host-renderer gl --release --test TARGET`, with a
private Weston GL host on the RTX 3090. All 20 targets passed:

| Target | Passed | Target | Passed |
| --- | ---: | --- | ---: |
| `mac_spaces` | 9 | `mac_mode` | 11 |
| `ext_workspace` | 2 | `hyprland_ipc` | 19 |
| `overview` | 5 | `desktop_gestures` | 9 |
| `fullscreen` | 5 | `session_restore` | 5 |
| `spatial_layout` | 7 | `output_management` | 1 |
| `surface_pacing` | 7 | `keyboard_focus` | 7 |
| `client_input_regions` | 4 | `pointer_coordinates` | 12 |
| `xwayland_input` | 13 | `session_lock` | 3 |
| `capture_tool` | 17 | `selection_transfer` | 20 |
| `browser_selection` | 6 | `popup_anchor` | 1 |

The total is **163 distinct E2E tests**, with no missing-client skips. Earlier
Spaces reruns and repeated installed-menu/theme helper checks are excluded.
Other targets ran during implementation; the final nine-test Spaces suite was
repeated against the committed standalone release as described below.

The new fixture creates two real `wl_output` heads in one nested host framebuffer.
It exercises the production hotplug path, per-output rendering, physical seat
input, actual client output-enter/leave events, native workspace protocol, IPC,
and captured pixels. Tests verify independent navigation, application activation,
simultaneous fullscreen and exact restore geometry, display-boundary rendering
and input clipping, cross-display dragging, disconnect/reconnect, display-local
Overview with grab cleanup, persisted empty/fullscreen Spaces, and fullscreen
furniture visibility while the other display has keyboard focus. Live-swipe
assertions prove that pixels move on the selected display while the other
display stays unchanged, before the transition commits.

After committing, `cargo build --locked --release -p chonkstep-wayland` produced a
standalone binary, preserved at `/tmp/chonkstep-fe81b40/chonkstep-wayland`.
Its SHA-256 is:

```text
27f739f3c0ed946ce3545afffacb0a77a1257b48793a64c5d66ef8b0ede53b61
```

With `CHONKSTEP_WAYLAND_BIN` pointing to that binary, the release `mac_spaces`
target passed **9/9** tests in 13.64 seconds. This verifies the standalone artifact
in addition to the harness-built development binaries. The binary and raw logs
remain local temporary artifacts; the linked JSON retains their checksums and
test results.

The committed workspace passed `cargo test --locked --workspace --lib`:
**1,933 passed, zero failed, 12 ignored**, across 18 library targets. Strict
workspace Clippy and private Rustdoc passed, as did all **68 Python harness
tests**. Core coverage includes transient families, topology reorder, duplicate
EDIDs, all outputs disconnected, initial headless startup, capacity limits,
malformed restore data, and linked-display compatibility.

Local logs: `/tmp/chonk-spaces-regression-TARGET.log`,
`/tmp/chonk-spaces-final-TARGET.log`, `/tmp/chonk-spaces-release-mac_spaces.log`,
`/tmp/chonk-spaces-committed-libs.log`, `/tmp/chonk-spaces-final-lint.log`,
`/tmp/chonk-spaces-release-docs.log`, and `/tmp/chonk-spaces-harness.log`.

Both virtual heads share the host swap cadence. These results do not qualify
physical Apple input, lid events, native KMS hotplug, mixed physical refresh
rates, or scanout. Paired Split View, Space reordering, per-app assignment controls,
and automatic Dock relocation remain separate work. No live desktop configuration
was changed.

## Spaces review fix validation

Implementation `a416af59fbc43333167257f9a6ac90e12bc6b56f` fixes all seven findings from
the review of `fe81b40`, plus the related lifecycle edges exposed by the expanded
tests. The [fix matrix](mac-spaces-review-fixes.md) connects each finding to its
permanent regression. [Machine-readable results](benchmarks/mac-spaces-fixes-2026-09-10/validation.json)
retain every E2E test name, result, and log checksum; the
[standalone Spaces log](benchmarks/mac-spaces-fixes-2026-09-10/mac_spaces.log) is
also committed.

| Target | Passed | Target | Passed |
| --- | ---: | --- | ---: |
| `mac_spaces` | 16 | `mac_mode` | 11 |
| `ext_workspace` | 2 | `hyprland_ipc` | 19 |
| `overview` | 5 | `desktop_gestures` | 9 |
| `fullscreen` | 5 | `session_restore` | 5 |
| `spatial_layout` | 7 | `output_management` | 1 |
| `surface_pacing` | 7 | `keyboard_focus` | 7 |
| `client_input_regions` | 4 | `pointer_coordinates` | 12 |
| `xwayland_input` | 13 | `session_lock` | 3 |
| `capture_tool` | 17 | `selection_transfer` | 20 |
| `browser_selection` | 6 | `popup_anchor` | 1 |

The total is **170 distinct E2E tests**, with no missing-client skips. Counts
exclude reruns and the repeated installed-menu/theme helper checks. The nineteen
regression targets ran against the final implementation before committing; the
sixteen-workflow Spaces suite was repeated against the preserved standalone
release after committing.

The expanded fixture verifies complete disconnect/reconnect during a swipe,
late client commits while headless, real keyboard delivery after policy reload,
continued dragging of an actual parented xdg dialog, pinned-window focus, and
active desktops when enabling separate Spaces. Unequal-output checks use a
400x300 survivor and enter fullscreen both before and after disconnect. The
save/restart workflow runs both with the home display connected and while its
Spaces are borrowed, then verifies the original rectangle after leaving fullscreen
and maximize. Assertions include captured pixels, seat input, client logs,
workspace membership, and geometry.

The standalone binary is preserved at `/tmp/chonkstep-a416af5/chonkstep-wayland`.
Its version reports `preview-v0.4.4-11-ga416af5`.
The supported source-ID build override was set explicitly to avoid reusing a
cached source stamp. Its SHA-256 is:

```text
1b19ec6d5b6d7167846bf1c6f393f0d1a031757942c6e5e765490f3e7967a736
```

Build and verification commands, run from the implementation checkout:

```sh
CHONKSTEP_GIT_DESCRIBE=preview-v0.4.4-11-ga416af5 cargo build --locked --release -p chonkstep-wayland
CHONKSTEP_WAYLAND_BIN=/tmp/chonkstep-a416af5/chonkstep-wayland scripts/e2e.sh --headless --host-renderer gl --release --test mac_spaces
cargo test --locked --workspace --lib
scripts/check.sh lint
scripts/check.sh docs
scripts/check.sh harness
```

The final standalone Spaces run passed **16/16**. Workspace library tests passed
**1,945**, with **zero failures and 12 existing ignored tests** across 18 library
targets. Strict workspace Clippy, private Rustdoc, and all **68 Python harness
tests** passed.

Tests ran under an isolated headless Weston GL host using the NVIDIA RTX 3090.
Both virtual heads share a host swap cadence. These results exercise production
hotplug policy and real client protocols; they do not qualify physical Apple
input, lid events, native KMS hotplug, mixed physical refresh rates, or scanout.
Raw regression logs remain under `/tmp/chonk-spaces-fixes-regression-TARGET.log`;
additional check paths and checksums are recorded in the JSON artifact.
