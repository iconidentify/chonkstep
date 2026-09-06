# Capture

Chonkstep's Wayland session has a native, Mac-inspired screenshot and recording
tool. Selection stays inside the compositor: no external region picker, editor
launch, or X11 round trip. Saving, PNG encoding, clipboard serving and video
finalization run on a bounded background worker.

## Shortcuts

| Capture | Omarchy keymap | Chonkstep keymap |
| --- | --- | --- |
| Entire desktop → PNG + clipboard | Super+Ctrl+Shift+3 | Super+Shift+3 |
| Select an area | Super+Ctrl+Shift+4 or Print¹ | Super+Shift+4 |
| Screenshot / recording toolbar | Super+Ctrl+Shift+5 | Super+Shift+5 |
| Stop recording | Super+Ctrl+Escape | Super+Ctrl+Escape |

¹ The standard Omarchy `omarchy-capture-screenshot` binding is routed to the
native selector on Wayland. A customized command is left alone.

Omarchy already assigns Super+Shift+digits to desktop carrying and
Super+Alt+digits to group selection. The additional Ctrl preserves those
choices. Capture defaults fill otherwise-unclaimed chords in a live Hyprland
import; an explicit bind or unbind wins. To choose the literal Mac-style chords
instead, knowingly replacing those three workspace bindings:

```toml
[keybindings]
"super+shift+3" = "capture-screen"
"super+shift+4" = "capture-area"
"super+shift+5" = "capture"
```

`capture-window` is also bindable for direct window selection. Native capture
actions require the Wayland session; X11's existing command-based tools remain
unchanged.

## Selecting

Drag an area and release for an immediate screenshot. Space switches between
area and window selection; click a highlighted window to capture it, including
its Chonkstep titlebar, without overlapping applications. Escape or right-click
cancels without saving or changing the clipboard.

The toolbar offers screen, window, area, record-screen and record-area modes.
Keys 1–5 select the modes; Enter captures or starts recording. In toolbar area
mode, releasing the pointer keeps the selection so it can be adjusted: drag
inside to move, drag a corner to resize, or drag outside to replace it. Arrow
keys move by one device pixel, Shift+arrow by ten. Space during a drag toggles
moving the selection. Dimensions are shown in actual output pixels.

Screenshots preserve the compositor's device-pixel resolution at integer and
fractional UI scales. The immediate whole-desktop shortcut covers all outputs;
the toolbar's screen mode selects the display under the pointer. Area captures
may cross displays. The pointer and capture controls are excluded from PNGs.
Selection is keyboard/pointer driven; touch, tablet and desktop gestures are
suppressed while the selector owns input.

## Files and clipboard

PNG screenshots go to `Pictures/Screenshots` and recordings to
`Videos/Recordings`, using the user's localized XDG Pictures/Videos directories.
`OMARCHY_SCREENSHOT_DIR` and `OMARCHY_SCREENRECORD_DIR`, when set, override those
destinations directly. Filenames contain the date, time and a unique suffix.
Overrides must be absolute paths.
Existing files are never replaced; published captures have owner-only access.

Every successful screenshot also offers the exact saved PNG as `image/png` on
the Wayland clipboard. Saving does not depend on clipboard availability; the
completion notification distinguishes those outcomes. Failed writes are
reported and do not leave the input grab active. No review application opens
automatically. Open/review and annotation UI are intentionally deferred.

## Recording

Select a display or an area within one display, then press Record or Enter.
The elapsed-time indicator can be clicked to stop. The toolbar shortcut also
stops an active recording; Super+Ctrl+Escape is the direct stop shortcut.
The indicator never appears in the video or exported screencopy frames.
Locking the session stops recording. Another recording cannot start until the
current file has finished finalizing.

Recordings are silent, 60 fps H.264 at CRF 18, using a portable software encoder
that does not assume NVIDIA or VAAPI support. Odd selections retain their last
row/column and are padded to even encoder dimensions. High-resolution 60 fps
software encoding can be CPU-intensive; physical M1/Asahi performance has not
been measured. Hardware acceleration, audio controls and window-following
recording are not part of this first version.

An in-progress hidden `.partial.mkv` lives beside the eventual file. Stopping
flushes the recorder, then remuxes to a fast-start MP4 without re-encoding.
On an encoder/finalization failure the MKV and diagnostic log remain available;
the notification identifies their paths. An abrupt process or machine failure
can lose the latest buffered frames; an unfinished recording is not promised
to be perfectly recoverable. A recording region must fit one output and its
capture grid; an unrepresentable edge on an odd-sized output is refused rather
than silently cropped.

Dependencies are included in the Arch package and source installer:
`wl-clipboard`, `wf-recorder`, `ffmpeg`, `libnotify`, and `xdg-user-dirs`.

## Verification

Run `scripts/e2e.sh --headless --test capture_tool` for real keyboard/pointer,
GPU readback, PNG clipboard, scale, occlusion, recording and failure tests.
Use the screenshot marker when inspecting the visible selector: ordinary
screencopy intentionally omits capture chrome. See
[the engineering report](engineering/2026-09-06-capture.md) for validation and
known limits. This is a tested native workflow, not a claim that every macOS
capture feature or every GPU has been reproduced.
