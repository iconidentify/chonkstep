# Chonkrec

Record a product walkthrough, including ChonkStep's own screenshot controls:

```sh
chonkrec start --demo
chonkrec stop
```

`--demo` (or `--include-overlays`) records the displayed scene on the selected
monitor: windows, menus, Spaces, screenshot controls, selection borders/handles,
dimming, the camera/crosshair cursor, and the recording status control. This lets
you demonstrate saving a screenshot into imv and recording a clip into Omacut
inside one continuous walkthrough. `stop` finalizes the walkthrough and opens it
in Omacut; use `--no-open` on start or stop to skip that review.

Normal `chonkrec start` keeps capture controls out of the video. The mode belongs
only to this recorder's connection. Running a demo does not change the PNGs or
videos produced by the built-in capture tool, other recorders, or future clean
captures. A locked session exposes its locked scene, never the desktop behind it.

## Options

```sh
chonkrec start --demo -O DP-1 -r 60 -o "$HOME/Videos/Spaces walkthrough.mp4"
chonkrec status
chonkrec stop
```

- `-O NAME` selects a monitor; it is required when multiple monitors are enabled.
  `wf-recorder --list-output` lists the names.
- `-g "x,y WxH"` limits recording to a numeric region in logical coordinates.
  Use even-sized regions: wf-recorder can trim an odd pixel edge before encoding.
- `-r FPS` selects constant frame rate, from 1 to 240; the default is 30.
- `-d SECONDS` delays startup.
- `-a [DEVICE]` records audio separately; `chonkrec list-audio` lists sources.
- `-A SECONDS` offsets audio at finalization, for example `-A -0.3`.
- `CHONKREC_DIR` selects the destination directory (default `~/Videos`).
- `CHONKREC_PRESET` and `CHONKREC_CRF` set the software H.264 encoder's speed and
  quality; defaults are `superfast` and `20`. The scripted product demos use
  `fast` and `18`. RGB is explicitly converted to correctly labeled limited-range
  YUV for consistent colors across wf-recorder/FFmpeg versions.

The recorder refuses to replace an existing output file. Startup failures retain
diagnostics and stop their workers. Completed files contain the joined recording;
failed joins leave the source segments available for recovery.

## Restarts and requirements

Chonkrec supervises wf-recorder and starts another video segment after a compositor
restart. Demo mode is reapplied on each connection. It stays pinned to the selected
Wayland display, so a temporary restart cannot redirect a take into a different
desktop. If the display socket's name changes, stop and start a new take with the
new session environment. Time while the compositor is absent is missing from the
video. Separately recorded audio continues during that gap, so a multi-restart
take may need an audio offset or an edit in Omacut.

The Wayland compositor, wf-recorder, wlr-randr, wayland-info (wayland-utils),
FFmpeg, Bash, coreutils and util-linux are
required. Omacut is optional for automatic review. Demo capture needs a ChonkStep
build with this feature; the original 0.5.0 release does not expose it. An unsupported
desktop fails with an explicit error instead of producing an overlay-free demo.
Monitor rotation is normalized explicitly, including with older wf-recorder
versions that otherwise record rotated or nested outputs upside down.

For isolated, repeatable 1080p footage with fixture windows, captions and pixel
verification, use the [product-demo runner](product-demos.md).
