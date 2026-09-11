# Genuine System 7 reference captures

Captured2026-09-11 using [Infinite Mac, System7.5.3](https://infinitemac.org/1996/System%207.5.3).
`about-system.png` verifies **System Software7.5.3 Revision2**,128MiB RAM.
`provenance.json` records the source, dimensions, settings and fixture/toolchain
hashes. `emulator-settings.png` records the host emulator settings.

The framebuffer and canvas CSS size are both1024×768. A private headless Chromium
session used viewport1280×1000, device pixel ratio1 and browser/visual viewport
zoom1. Browser canvas screenshot pixels were compared with the canvas's native
PNG export and were byte-identical in decoded RGB. A vertical frame outline is
exactly one pixel wide. No screenshot was resized, recolored or retouched. The
rounded black screen corners are pixels of the emulated OS desktop itself.

`1bit/` is Monitors “Black & White”; `8bit/` is256colors. Each has captures of the
Monitors, Color and WindowShade panels. Color uses “Black & White” highlighting
and “Standard” windows. WindowShade was enabled for two title clicks, no modifiers,
no sound. It is the genuine System7 extension, not a custom collapse renderer.

Each depth has42 state captures and a `requests.json`: two window variants × four
titles × active/inactive × normal/shaded; six held-box states; screen-edge shadow;
and three character charts. `documentProc` has close only; `zoomDocProc` adds zoom.
The real OS draws every frame. Our [fixture source](../../../../scripts/system7-reference/reference.c)
only creates standard windows, handles events and paints a Chicago12 character
chart inside the client. Held close/zoom tracking is canceled outside the window
so the same fixture can be reused. Its `h` key hides the cursor before capture.

The reference chart places glyphs in24-pixel cells with28-pixel row spacing. Its
rulers are actual QuickDraw `CharWidth` lengths, drawn five pixels below baseline.
These are content for measurement, not part of the decoration goldens.

## Reproduce

Build the small classic-Mac fixture with the pinned open-source
[Retro68 toolchain](https://github.com/autc04/Retro68). Its freely implemented
Multiversal Interfaces provide the Toolbox declarations. From the repository:

```sh
mkdir -p /tmp/chonkstep-system7-reference
docker run --rm --network none --user "$(id -u):$(id -g)" --entrypoint /bin/bash \
  -v "$PWD/scripts/system7-reference:/src:ro" \
  -v /tmp/chonkstep-system7-reference:/out -w /out \
  ghcr.io/autc04/retro68@sha256:459dd3ea9856262162615527021be7b64f198631dc59cca8cedbf197b7656019 \
  -c 'cmake -S /src -B /out/build -DCMAKE_TOOLCHAIN_FILE=/Retro68-build/toolchain/m68k-apple-macos/cmake/retro68.toolchain.cmake && cmake --build /out/build -j2'
```

This is an optional reference-authoring toolchain, not a ChonkStep build/runtime
requirement. Disk-image timestamps can change the built disk hash. No fixture
binary, OS disk, ROM or font file is committed.

Open the linked OS in an unscaled1024×768 canvas. Drag the built
`ChonkFrameReference.dsk` onto it, open the disk, and launch the app. Set and
capture the three control panels as above, close their windows and return focus
to the fixture. `1`..`4` choose titles; `d`/`z` choose document/zoom windows; `r`
resets; `e` moves to the edge; `g` cycles character charts; `q` exits.

[`capture_matrix.py`](../../../../scripts/system7-reference/capture_matrix.py)
exports `capture_matrix(page, output, depth)` for a Playwright page already running
that fixture. It checks dimensions/DPR/zoom and uses real keyboard/mouse events
with time for the emulated event queue. Capture into a fresh staging location,
visually review all states, then copy the accepted captures here with updated
provenance. The driver must not be used to claim performance numbers.

Derive goldens only from accepted OS captures:

```sh
python3 scripts/system7-reference/crop_goldens.py --write /tmp/new-system7-goldens
python3 scripts/system7-reference/crop_goldens.py --check crates/wm-theme/tests/fixtures/system7
scripts/check.sh harness
```

The writer refuses an existing output directory. See the
[metric sheet](../../system7.md) for coordinates, scope and the classic1-bit versus
8-bit focus distinction.

## Rights and scope

System7 screenshots are retained solely as documentation and test evidence.
They are the only Apple-derived content in this package. No Apple-owned artwork
asset, font file or ROM is committed. The fixture, capture/crop tools and title
atlas drawings are ChonkStep source under the repository license. The title atlas
was written as explicit1-bit grid drawings and original accent compositions,
then verified against screenshots; it was not exported from a font or taken from
a third-party clone. Its Latin-1 extensions are not claimed to reproduce missing
Chicago glyphs. Infinite Mac supplies its own emulated system through the linked
service; this repository does not redistribute that system.
