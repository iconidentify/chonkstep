# Mac interaction validation — 2026-09-10

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
and optional GPU-producer Clippy passed. The separately invoked ignored two-GPU
hardware test also passed; see [GPU audit evidence](gpu-audit-work.md).

Logs: `/tmp/chonk-final-TARGET.log` (substitute each target; idle's successful
retry is `/tmp/chonk-final-idle-retry.log`), `/tmp/chonk-gpu-final-unit2.log`,
`/tmp/chonk-interop-lint.log`, `/tmp/chonk-gpu-final-docs.log`, and
`/tmp/chonk-texture-fixture-lint.log`. The initial idle attempt collided with a
source edit during compilation; the completed-source retry passed. This was not
an idle runtime failure. Physical Apple input and native KMS remain unqualified.
