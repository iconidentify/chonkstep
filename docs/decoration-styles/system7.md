# System 7.5 decoration reference

This is the acceptance specification for #163, measured before implementing its
renderer. Sources are genuine System Software **7.5.3 Revision 2** frames, painted
by the OS Window Manager. See [capture provenance and reproduction](system7/reference/README.md).
Coordinates below are **inclusive, absolute framebuffer pixels**, at 1x. Crop
manifests use the conventional exclusive right/bottom coordinates instead.

## Exact target and focus

The flagship target is the **1-bit** capture, using the classic role set. The
8-bit captures document a separate historical rendition; they are not claimed to
be reproduced by the monochrome recipe. This resolves the focus contract in
#159/#163: monochrome focus changes only the title band. The real 8-bit OS also
changes the outer outline on focus, which would violate that contract.

There is no gray/dithered inactive title in these 1-bit captures. Both its text
and its outline stay black; stripes and boxes disappear. Tests compare every
pixel in the inactive title area and the unchanged other bands. A remembered
"gray inactive Chicago" rendition must not replace this evidence.

## Measured geometry

Unless qualified, each capture path is relative to `system7/reference/1bit/`.
The standard specimen's client rectangle is `(120,140)` with size `400×240`.

| Name | Value and capture coordinates |
| --- | --- |
| Visible frame origin | `(119,121)` (`zoom-short-active.png`, top-left outline pixel) |
| Visible frame size | `403×261` (`zoom-short-active.png`, x119..521, y121..381) |
| Client offset | `(1,19)` (`zoom-short-active.png`, client starts x120,y140 relative to x119,y121) |
| Title band | `19` rows including outer top outline and bottom separator (`zoom-short-active.png`, y121..139) |
| API titlebar height | `18` rows excluding outer top outline (`zoom-short-active.png`, y122..139) |
| Title interior height | `17` (`zoom-short-active.png`, y122..138) |
| Outline width | `1` (`zoom-short-active.png`, x119 and x520 beside client, y121 above title, y380 below client) |
| Shadow offset/extent | `(1,1)`, one extra right column and bottom row (`zoom-short-active.png`, x521,y122..381 and y381,x120..521) |
| Unpainted corners | Top-right `(521,121)`, bottom-left `(119,381)` (`zoom-short-active.png`); retain desktop background, not paper |
| Stripe count | `6` (`zoom-empty-active.png`, y125,127,129,131,133,135) |
| Stripe thickness / pitch | `1 / 2` (`zoom-empty-active.png`, x160,y124..136); offsets4,6,8,10,12,14 from frame top |
| Stripe left/right inset | `2` from outer outline (`zoom-empty-active.png`, x121..518 at y125; interrupted by box surrounds) |
| Close box | `11×11` (`zoom-short-active.png`, x128..138,y125..135) |
| Close box inset | `9` from left outline, `4` from top (`zoom-short-active.png`, x119→128,y121→125) |
| Zoom box | `11×11` (`zoom-short-active.png`, x501..511,y125..135) |
| Zoom box inset | `9` from right outline to right box edge, `4` from top (`zoom-short-active.png`, x511→520,y121→125) |
| Box white surround | One column each side (`zoom-short-active.png`, x127,139 and x500,512, y125..135) |
| Zoom inner lines | Vertical x507,y126..131 and horizontal y131,x501..507 (`zoom-short-active.png`); top-left miniature outline7×7 including shared top/left edges |
| Title advance | `56` for `Terminal` (`glyph-ascii.png`, independent glyph rulers; T6+e8+r6+m12+i4+n8+a8+l4) |
| Title origin / baseline | `(292,135)` (`zoom-short-active.png`, capital ink starts y126, last normal row y134); baseline14 below frame top |
| Title cell | `15` rows, baseline12 (`glyph-ascii.png`, j: x372..377,y264..278 around baseline276; includes descenders; chart ruler is baseline+5 and excluded) |
| Title paper padding | `6` each side (`zoom-short-active.png`, paper x286..353 surrounding text advance x292..347) |
| Title paper vertical extent | Entire17-row interior (`zoom-short-active.png`, x286,y122..138) |
| Title centering | `client_x + floor((client_width − clipped_advance)/2)` (`zoom-short-active.png`, x120+(400−56)/2=292; `zoom-long-active.png`, x120+(400−336)/2=152) |
| Title maximum advance | `client_width − 64`, clamped at0 (`zoom-long-inactive.png`, text clip x152..487, width336) |
| Long title paper | Document x146..493; zoom x146..492 (`doc-long-active.png` / `zoom-long-active.png`, y125); the zoom surround reserves x493..519, shortening only the right paper pad by1 |
| Empty title | No title paper interruption (`zoom-empty-active.png`, stripe y125 continues through x286..353) |
| Collapsed visible height | `20` (`zoom-short-shaded-active.png`, y121..140); 19-row top band plus shadow at y140,x120..521 |
| Screen-edge shadow | Last right column x1023; bottom row y766,x622..1023 (`zoom-screen-edge.png`); `(621,766)` stays desktop background |

The long title clips at a pixel boundary, including part of the final glyph; it
does not append an ellipsis. `documentProc` and `zoomDocProc` use identical title
ink positioning and clipping (`doc-long-inactive.png` versus
`zoom-long-inactive.png`, x152..487,y122..138). The 64-pixel reservation applies
with and without zoom. At widths absent from the captures, keep that reservation
and the centering equation; avoid negative extents. Very narrow frames must
suppress overlapping controls rather than create an impossible hit target.

The invisible resize ring required by #163 is a ChonkStep input extension, not
an OS pixel measurement. It must be documented with that implementation and
excluded from the visual bounds above, snapping, placement, Overview, and opaque
regions. Neither the reference PNGs nor these cropped oracles include that ring.

## Pressed and inactive boxes

Close and zoom have the same held rendition (`zoom-close-pressed.png`,
x128..138,y125..135; `zoom-zoom-pressed.png`, x501..511,y125..135). `#` is ink:

```text
###########
#....#....#
#.#..#..#.#
#..#.#.#..#
#.........#
####...####
#.........#
#..#.#.#..#
#.#..#..#.#
#....#....#
###########
```

Inactive boxes and stripes vanish into paper (`zoom-short-inactive.png`,
x120..519,y122..138). Close remains the only left control; zoom exists only in
`zoomDocProc` (`doc-empty-active.png` versus `zoom-empty-active.png`,
x500..512,y125..135). There is no Miniaturize control or hover paint. WindowShade
uses the same controls, pressed pattern and title (`*-shaded-*.png`).

## Color roles and appearance

| Role | Classic exact value | Evidence |
| --- | --- | --- |
| paper | `#ffffff` | `1bit/zoom-short-active.png`, (120,122) |
| ink | `#000000` | `1bit/zoom-short-active.png`, (292,126) |
| stripe | `#000000` | `1bit/zoom-short-active.png`, (160,125) |
| frame | `#000000` | `1bit/zoom-short-active.png` and `zoom-short-inactive.png`, (119,180) |
| shadow | `#000000` | `1bit/zoom-short-active.png`, (521,180) |
| inactive ink | `#000000` | `1bit/zoom-short-inactive.png`, (292,126) |

`nextstep-classic` selects this classic role set. Other palettes and Omarchy
follow use the same recipe with tinted roles: resolve a built-in theme's **light**
variant; take the lighter of `terminal.bg`/`terminal.fg` as paper seed and the
darker as ink seed (ties favor bg as paper). Mix paper seed 7/8 toward white and
ink seed 1/2 toward black, rounding channels half-up with `Color::mix`. Stripe,
frame, shadow and inactive ink use that ink. Force alpha255. This is a deliberate
ChonkStep palette mapping, not a claim about historical 8-bit colors. Sorting
seeds also produces light chrome for a dark-only custom/Omarchy palette.

The style ignores the appearance axis. Resolve light palette inputs consistently
on startup and reload; log that choice once when dark appearance is selected.
Do not invert System 7 chrome or gray its outer frame on focus.

The comparison-only 8-bit rendition has paper `#f3f3f3`, stripes `#969696`, top
bevel `#dadaff`, bottom bevel `#b3b3da` (`8bit/zoom-empty-active.png`: paper(160,123), stripe(160,125),
top bevel(160,122), bottom bevel(160,138)). Its inactive outline is `#777777` and title ink `#a5a5a5`
(`8bit/zoom-short-inactive.png`, (119,180) and title x292..347,y126..134).
Settings for both depths are captured, including the Color panel's “Standard”
window style and “Black & White” highlight.

## Original title atlas

[`title.atlas`](../../crates/wm-theme/src/styles/system7/title.atlas) contains
191 original integer-grid drawings: printable ASCII U+0020..007E and Latin-1
U+00A0..00FF. These are explicit row masks, including our own accent compositions
and additional symbols. No Apple font resource, outline, extracted font table or
third-party clone is included. The atlas is embedded with `include_bytes!` by
#163; this reference story intentionally adds no renderer.

Each line is `unicode_hex advance_decimal top_row_decimal hex_row...`. The high
bit of an advance-wide row is its leftmost pixel. The cell has15 rows, baseline12,
and omitted rows are blank. Advance includes side bearings. Example: `0041 8 3`
starts A at row3 in an8-pixel cell. The source is human-editable; the verifier
never writes or derives its row masks.

All95 ASCII glyphs, their bearings, 15-row cells and advances are checked
individually against the actual OS chart. For code point `c`, let `i=c−32`:
origin x=`132+24*(i%16)`, baseline y=`164+28*floor(i/16)` in
`1bit/glyph-ascii.png`. The independent horizontal ruler at baseline+5 has the
OS `CharWidth` length. MacRoman charts use the same coordinates with i=byte−128
or byte−224. The non-ASCII golden title additionally verifies é, ï, Å and ö.

Not every Latin-1 glyph exists in the captured Chicago font; some MacRoman slots
show missing-character boxes. Our added Latin-1 glyphs are original compatible
designs, not claimed as exact reconstructions of absent OS glyphs. U+00AD uses a
visible hyphen in a single-line title; U+00A0 is a nonbreaking blank. Other
Unicode scalars fall back to cosmic-text, with hinting disabled and coverage
thresholded to a binary mask (no antialiasing). Fallback is explicitly **not
pixel-exact**. It must support Cyrillic/CJK when an installed fallback face
covers them, initialize font data outside the render path, and never introduce
blocking font I/O during layout or raster. Missing system coverage must degrade
to a visible missing-glyph mark, not silently omit a title character.

## Scale policy

At integer scales2 and3, every1x decoration pixel is replicated into an exact
2×2 or3×3 block. Title masks, box patterns, the two transparent corners and shadow
scale with the same rule. Content dimensions may be physical pixels, but the
style's metrics are scaled exactly once.

For fractional scales, use `r(v)=floor(v*scale+0.5)`. Round each named geometric
metric, use outline/shadow/stripe thickness `max(1,r(1))`, and stripe pitch
`max(thickness+1,r(2))`. Keep six stripes, starting at `r(4)` from the frame top;
each has the same thickness. Scale each bitmap with nearest-neighbor sampling
`source=floor(destination/scale)`, clamp to its source extent, and use rounded
bitmap extents. Center with integer floor after scaling. The renderer must lock
1.25x and1.5x examples as additional implementation goldens; they are faithful
new renditions, **not** exact historical captures. No interpolation/gray edge
pixels are introduced by geometry or the original atlas.

## Capture-derived acceptance oracles

[`manifest.json`](../../crates/wm-theme/tests/fixtures/system7/manifest.json)
records84 cases, requests, source hashes, exact absolute crops and sparse part
offsets. Normal frames have top/bottom/left/right bands; shaded frames have only
top/bottom. Only the two proven outside-shape corner samples become transparent;
every opaque pixel comes unchanged from its source screenshot. The 8-bit cases
remain comparison evidence; renderer acceptance selects the1-bit cases.

Run `python3 scripts/system7-reference/crop_goldens.py --check
crates/wm-theme/tests/fixtures/system7` and `scripts/check.sh harness` to verify the
capture package. Generation requires a **new directory** and cannot overwrite
accepted fixtures. It never calls a ChonkStep renderer. Add newly measured
references deliberately; do not refresh a failed acceptance oracle from renderer
output.
