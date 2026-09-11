# Repeatable product demos

For a live product demo on your own desktop:

```sh
chonkrec start --demo
# Show Spaces, open Command-Shift-5, select an area/window, save, record, review.
chonkrec stop
```

The finished recording opens in Omacut. `--demo` includes capture controls,
selection handles, dimming and the capture cursor. Ordinary screenshots and
the video created inside the demo still exclude their controls. See
[chonkrec](chonkrec.md) for monitor, quality, audio and restart options.

Run the actual compositor, open two real Foot terminals and a live GTK design
board, and demonstrate window screenshots and region recording, including their
automatic opening in the real imv and Omacut applications. A second
scenario shows three Spaces, live desktop miniatures, dragging a window between
Spaces, keyboard navigation, and entering/leaving a dedicated fullscreen Space. The fixtures
are scripted sample content, not benchmark results. No existing desktop,
clipboard, configuration or session bus is used. Fixtures select Adwaita for
a repeatable GTK appearance.

```sh
cargo build --locked --release -p chonkstep-wayland
python3 -B scripts/product-demo.py \
  --binary target/release/chonkstep-wayland \
  --output /tmp/chonkstep-demo-0.5.0
```

For Spaces, use a separate output directory:

```sh
python3 -B scripts/product-demo.py \
  --binary target/release/chonkstep-wayland \
  --scenario spaces --output /tmp/chonkstep-spaces-0.5.0
```

The output directory must be new. Dependencies: Weston with its GL headless
backend and kiosk shell, Foot, grim, wf-recorder, wlr-randr, wayland-info,
FFmpeg/ffprobe, dbus-run-session,
Python 3, Pillow, PyGObject, GTK 4 and the Python Cairo/GI bridge. On Debian/Ubuntu the
Python packages are `python3-pil python3-gi python3-cairo python3-gi-cairo gir1.2-gtk-4.0`.
The capture scenario also requires imv and Omacut with their Wayland support.
A headless Mesa software driver can be selected with
`LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe`; these demos are not performance
measurements. No root access or physical display is needed.

The runner creates a private session bus without service activation directories
and private HOME/XDG paths. A headless Weston hosts the demonstrated ChonkStep
fullscreen at 1920×1080. The runner invokes the actual `chonkrec --demo`
supervisor against that compositor's opt-in capture connection. A second
recording compositor is no longer necessary. This requires a build with demo
capture support; the original 0.5.0 release predates that option. Diagnostic stills
provide an independent full repaint of the same scene for visual comparison.

Outputs:

- `chonkstep-capture-demo.mp4`: silent 1080p H.264/yuv420p walkthrough at 30 fps,
  with MP4 metadata moved to the front for web playback.
- `capture-{area,window,record}-overlay.png` and `desktop.png`: real rendered
  screenshots. Corresponding `-diagnostic.png` files support visual QA.
- `exports/`: the window screenshot and region video produced by using the
  feature itself. Keep these distinct from the walkthrough recording.
- `screenshot-in-imv.png` and `recording-in-omacut.png`: real automatic review
  windows. The runner verifies their exact file arguments and closes them with
  Command-Q; stable process handles also clean them up if the demo fails.
- `chonkstep-spaces-demo.mp4` and `spaces-{overview,design,window-moved,fullscreen,restored}.png`:
  the Spaces walkthrough and its checkpoints. These show actual client textures,
  including a GTK text editor on the third desktop.
- `chonkstep-{capture,spaces}-captioned.mp4` and `timeline.srt`: the same uncut
  footage with readable action labels for sharing. The original walkthrough
  remains available without editorial text. FFmpeg must include libass subtitles.
- `manifest.json`: executable identity and SHA-256, recorder/fixture script hashes, scenario timeline, artifact
  hashes, ffprobe results and Spaces membership checkpoints. Every video must
  also pass a complete FFmpeg decode. Capture additionally checks every frame
  in a stable selection interval for stale dimming, before a diagnostic PNG can
  force a repaint. Both raw and captioned capture videos must also match the
  independent PNG's dark RGB levels; the encoder explicitly labels limited-range
  YUV correctly. Complete capture-overlay PNGs are compared with independent
  full repaints, excluding only the fixture's pulsing dot. An invalid recording
  fails the run. Logs and final window geometry remain alongside it.

The runner stops its process groups and private bus on exit, refuses to replace
an existing artifact directory, bounds waits, and rejects missing outputs or
unexpected compositor/recorder exits. The scripted workflow sends real physical
key/button events through the private test door; GTK fixture placement uses the
public window IPC. It does not emulate screenshot or video exports.

For a new demo, add fixture clients under `scripts/demos/` and a scenario using
the runner's session, placement, pointer and capture helpers. Keep timing labels
honest, require the feature's actual output, and inspect stills plus video frames
before publishing. Recreate final media with the final release executable; do
not relabel a recording made with an earlier build.
