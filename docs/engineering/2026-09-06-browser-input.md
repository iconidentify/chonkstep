# Browser selection and key repeat — 2026-09-06

The reported symptoms were unreliable held-key repeat and intermittent pointer
text selection in a Chromium-family browser on an M1 MacBook running Omarchy
and ChonkStep. The reporter's exact browser version, ChonkStep build and Ozone
backend were unavailable, so the original hardware report is not conclusively
attributed to one defect.

## Confirmed older defect and existing corrections

The preserved 0.3.0 executable (`preview-v0.2.0-r3-110-g9d4d009`) fails
`an_xwayland_client_repeats_a_held_physical_key`: the window maps, but the
X server never focuses it. The test times out at its actual X11 input-focus
query before attempting the held key. This independently reproduces the
missing X11 focus contract; it is not a repeat-timer measurement.

Commit `8f5705a`, already included in 0.3.1, retains the managed X11 window in
the seat's keyboard focus target and delegates its ICCCM focus operations to
Smithay. Previously, treating that target only as a Wayland surface bypassed
the X server's focus contract. The current optimized executable at `97bde1d`
passes the same test and all 13 keyboard-repeat integration cases.

That earlier commit also replaced the pointer's synthetic-origin-only
adaptation with a complete surface coordinate transform retained through
implicit grabs. This is the relevant correction for scaled selection drags:
hover and held-button motion must arrive in the same surface coordinate space.
The new browser matrix verifies the resulting behavior; this investigation
did not independently reproduce the selection symptom on M1 hardware.

No additional compositor input change was justified by the current results.
This change strengthens the regression coverage around the existing fixes.

## Coverage added

Every browser case now loads isolated Hyprland/Omarchy input configuration
with the `apple` XKB model, US layout, Omarchy's Compose/Caps Lock options,
40 Hz repeat and a 250 ms delay. Input crosses ChonkStep's seat path; DevTools
observes the private page and prepares its fixture, without injecting input.

The six cases cover native Wayland and XWayland at 1x, 1.5x and 2x. Each checks:

- Pointer selection in plain text and contenteditable text, both windowed and
  fullscreen: exact selected string, anchor, endpoint and pointer coordinates,
  with screenshots verifying the browser has rendered its page.
- Held Right advancing the contenteditable caret in both window states.
- Held `a` inserting text and held Backspace deleting text in both single-line
  inputs and textareas, both windowed and fullscreen. The initial key event
  must not be marked repeated, and subsequent repeats must reach the DOM.
- Continued text insertion while the same key remains held across a requested
  Omarchy configuration reload.
- A physical release followed by 250 ms without further events or edits.
- Overview withdrawing browser focus and stopping typing behind it, followed
  by cancellation restoring focus and working letter/backspace repeat.

The XWayland browser uses its private X display, never the ambient desktop's
display. An explicit browser device-scale factor makes each X11 scale case
deterministic; this does not measure automatic X11 DPI detection.

The existing CI Wayland job runs every ignored browser integration case via
`scripts/e2e.sh --headless`, including the new XWayland cases. Ordinary
`cargo test` intentionally skips these graphical cases.

## Test corrections needed for accurate results

The original fixture's range reset also clears Chromium's form-control
insertion point. New typing tests clear event history separately, preserving
the caret established by the physical click.

X11 carries integer device-pixel pointer coordinates. At 1.5x, truncating
`73 * 1.5` to 109 selects CSS pixel 72. The harness rounds into the intended
pixel instead. Integer CSS viewport dimensions can also round beyond a
fullscreen surface by one device pixel; the derived content origin is bounded
to the client rectangle. Finally, fullscreen qualification waits for the DOM
viewport, compositor extent and presented buffer, since their updates are
asynchronous. Exact selection and coordinate assertions remain in place.

PR #137's CI run exposed a test-environment difference: the hosted runner uses
`--no-sandbox`, whose Chromium warning infobar stays visible even in DOM
fullscreen. Reproducing with `CI=1` locally showed the same full-size client
with a shorter page viewport. The isolated CI browser now uses Chromium's
`--test-type` startup-infobar suppression (see the
[Chromium implementation](https://chromium.googlesource.com/chromium/src/+/refs/heads/main/chrome/browser/ui/startup/infobar_utils.cc)).
This leaves all extent, rendering, selection and repeat assertions intact and
does not change browser launch options in the user's desktop session.
Fullscreen timeouts now retain a screenshot and both DOM/compositor geometry.
The CI-mode rerun also caught SwiftShader presenting an initial black buffer
at the correct extent before painting the page. The screenshot color assertion
now polls for that asynchronous paint for at most ten seconds; a permanently
blank page still fails, and all exact input assertions are unchanged.
Selection points and viewport size are read atomically after renderer frames,
and requalified against fullscreen geometry immediately before input. Separate
CDP reads had occasionally combined an old DOM viewport with the new client
extent during an XWayland configure sequence.

## Results

The tested compositor is the installed optimized 0.3.2 executable from
`preview-v0.3.2-1-g97bde1d`, ELF build ID
`e65d87916a207fe9aed9507aa7c56da0252f09c9`.

| Suite | Result |
| --- | --- |
| Chromium 151.0.7922.173, both backends and all three scales | 6 passed |
| Microsoft Edge 152.0.4191.53, both backends and all three scales | 6 passed |
| Keyboard repeat, including native/X11 clients and live keymap changes | 13 passed |
| Keyboard focus and modal ownership | 7 passed |
| Installed Omarchy menu/theme checks | 3 passed |
| Browser observation-helper unit tests | 3 passed |
| Strict Clippy for the modified integration target | Passed |
| Preserved 0.3.0 XWayland focus/repeat negative control | Failed as expected |

No browser cases or required clients were skipped in the graphical matrix.
The 2x native Chromium and 1.5x XWayland Edge selection screenshots were also
visually inspected.

Reproduce with an already-built compositor:

```sh
CHONKSTEP_WAYLAND_BIN=/path/to/chonkstep-wayland \
  scripts/e2e.sh --headless --test browser_selection
CHONKSTEP_TEST_BROWSER=microsoft-edge-stable \
  CHONKSTEP_WAYLAND_BIN=/path/to/chonkstep-wayland \
  scripts/e2e.sh --headless --test browser_selection
CHONKSTEP_WAYLAND_BIN=/path/to/chonkstep-wayland \
  scripts/e2e.sh --headless --test keyboard_repeat
CHONKSTEP_WAYLAND_BIN=/path/to/chonkstep-wayland \
  scripts/e2e.sh --headless --test keyboard_focus
```

Local logs and per-case DOM/screenshot evidence are retained under
`/home/chrisk/.cache/cbi.vwoyaOay/`. Final matrix logs are
`chromium-complete.log` and `edge-verified.log`; earlier exploratory failures
are retained separately and are not counted as passing qualification runs.

These are x86_64 nested-compositor results. The Apple XKB model exercises
configuration, not a physical M1 keyboard, touchpad, Asahi driver, or native
ARM GPU. They cannot guarantee that every browser version or hardware path
works. A physical M1 smoke test with the reporter's actual browser remains
necessary to close that specific hardware report.

## Upgrading requires activating the new compositor

Installing a package does not replace a running Wayland compositor. On this
host, the installed executable was 0.3.2 while the still-open desktop process
was 0.3.0. Log out and back in after saving work to activate the fixes.
An on-disk `chonkstep-wayland --version` report alone does not prove that the
running desktop has been updated.
