# BeOS R5 decoration and shell specification

The target is the original BeOS R5 appearance. Primary evidence and its
attribution are in [reference/README.md](beos/reference/README.md). The historical
oracle is independent of the renderer and must never be updated from its output.

## Measured pixels

Coordinates in this table refer to `reference/r5-desktop.png`, at native 1×.
Rectangles use exclusive right/bottom bounds. `GLTeapot` is the focused window;
`apps` is an inactive window. The PNG's channel maximum is 252, not 255.

| Feature | Measurement / reference |
| --- | --- |
| Desktop | `#306498`, e.g. (20, 900) |
| Active tab | `#fcc800`, (130, 82); 19 rows above the body |
| Tab top outline | `#989898`, y76 |
| Tab upper highlight | `#fcfc64`, y77 |
| Active body starts | (95, 95); client begins (100, 100) |
| Frame width | 5 pixels on every side |
| Active left frame | x95..99 at y200: `#989898`, `#fcfcfc`, `#d8d8d8`, `#888888`, `#989898` |
| Active bottom frame | y416..420 at x160: `#989898`, `#fcfcfc`, `#d8d8d8`, `#888888`, `#606060` |
| Inactive left frame | x50..54 at y600: `#989898`, `#fcfcfc`, `#e8e8e8`, `#989898`, `#989898` |
| Close control | active `(99,80,113,94)`; inactive `(54,432,68,446)` |
| Zoom control | active `(205,80,219,94)`; inactive `(132,432,146,446)` |
| Control origin | close (4,4) relative to tab; zoom 18 pixels from its right edge |
| Title origin | 36 pixels from tab left; 12px bold text |
| Inactive title ink | `#505050` |
| Menu face | `#d8d8d8`, Be's `desktopContext.gif` |
| Menu selection | `#989898` with black ink, Be's `menu1.gif`, Add-Ons row |

The tab width is compatible text advance +72 pixels (+54 without zoom), capped
by frame width. Controls are suppressed before they can overlap in a narrow
window. No minimize glyph, hover fill, full-width yellow bar, gradient wallpaper,
blur, soft frame shadow or modern rounded corners are introduced.

Only the active tab is yellow. Inactivity also lightens the frame's middle
band and changes its inner shade, as the source pixels show. Close and zoom's
idle patterns are exact indexed drawings of the captured controls. Depressed
controls reverse the bevel's light/dark colors without rotating the glyph;
this pressed state is an adaptation, not a captured acceptance case.

## Text and scaling

`regular.atlas` and `bold.atlas` each contain 191 Latin-1 glyph cells: one advance
byte followed by 16×15 coverage bytes. They were rasterized from **Liberation
Sans 2.1.5** at 12 pixels, baseline 12, using Pillow/FreeType. Source font hashes,
the full SIL OFL license, and the reproducible generator accompany them:

```sh
python3 scripts/beos-reference/generate_atlas.py --check
```

This is a freely licensed Swiss-compatible substitute, not BeOS's proprietary
Swis721 font. Text bearings, advances, antialias coverage and hence tab widths
can differ from an original installation. Pixel-exact claims cover the captured
controls and straight frame bands, not all text. Uncovered Unicode runs use the
existing resident fallback set, preserving joining and combining marks; they
also are not historical pixel matches. No render-time font discovery or file
I/O is needed. Input titles are bounded before measurement and shaping.

Integer scales replicate source pixels exactly, including text coverage and
transparent space. Fractional geometry uses `floor(n*scale+0.5)`; glyphs and
controls use nearest sampling. 1.25× and 1.5× are new renditions, not OS captures.

## Modern shell translation

Desktop/window/cascade menus use Tracker's gray panels, two-line header
separator, black text, gray selection, hollow cascade arrows and original tiny
folder drawings. The header labels the actual ChonkStep menu and exposes the
posted menu's close control. Actions and their painted hit rectangles are returned
together; oversized models are rejected whole. Menus do not invent a Be menu
with commands that ChonkStep cannot perform.

Switcher, Overview and minimized tiles retain live previews and existing
navigation. Their raised gray panels, Swiss-compatible labels and yellow
selected title strips translate the same vocabulary to these new surfaces.
Blue focus outlines distinguish modern navigation selection. Those surfaces are
deliberate adaptations, not claims that BeOS shipped ChonkStep's Overview.
Omarchy receives the matching application palette and shell color roles, but
continues to own its bar and app widgets. Dark application appearance does not
recolor the fixed historical chrome or blue ground.

## Input, storage and verification

`DecorationLayout.input_exclusion` describes the transparent rectangle beside
the tab. Wayland rejects it during scene hit testing; native X11 subtracts the
same rectangle from the bounding Shape and retains the client interior.
Title changes refresh the cutout and control hitboxes without configuring the
client. Restyles/fullscreen remove stale exclusions. Resize handles occupy the
five-pixel body frame below the tab. Shading retains the controls and restore
geometry. Client-decorated applications use the same five-pixel edge frame.

The renderer retains the short tab separately from the four frame bands, so
empty space beside the tab consumes no pixel storage. A bounded title cache
avoids reshaping Unicode on warm repaints. The existing allocation and retained
pixel budgets apply unchanged. Historical tests compare actual control crops and straight bands
at 1×/2×/3×. Additional tests cover scale replication, fractional/tiny layouts,
Unicode, menu limits, title changes, hover/press/shade, and descriptor round trips.

```sh
cargo test --locked -p wm-theme --test beos
cargo test --locked -p wm-core beos
scripts/e2e.sh --headless --test beos
xvfb-run -a cargo test --locked -p wm-x11 native_beos -- --ignored --test-threads=1
cargo run --locked -p wm-theme --example beos_preview -- /tmp/beos-new-review
```

The nested compositor test exercises Wayland and XWayland clients at 1× and 2×,
compares uploaded chrome pixels, clicks through the tab cutout onto another
window, uses the switcher, switches through System 7 and back, zooms/restores
and closes the window, then opens the desktop menu. The separate native X11 test verifies actual server Shape regions as
titles grow/shrink and when returning to WindowMaker.

[Reviewed output](beos/preview/README.md) includes renderer specimens at four
scales and unmodified compositor screenshots of frames and the desktop menu.
