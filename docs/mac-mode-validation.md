# Mac interaction validation — 2026-09-10

The implemented Mac profile passed 94 distinct end-to-end tests across nine
targets, plus 1,146 unit tests in the affected libraries. This is software
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
