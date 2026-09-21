# OS/2 Warp 4 decoration and shell specification

The target is the **1996 OS/2 Warp 4 Workplace Shell**, with original captures
and resources documented in [reference/README.md](os2warp/reference/README.md).
This is a dedicated renderer (`os2warp`), not a WindowMaker palette variant.

## Measured geometry and colors

Coordinates below refer to the unmodified 498×320 `reference/editor.png`.
Rectangles use exclusive right and bottom bounds.

| Feature | Original 1× measurement |
| --- | --- |
| Frame / client | 4-pixel sides and bottom; client offset (4,24) |
| Outer top / left | `#828282` |
| Inner top / left highlight | `#ffffff` |
| Frame face | `#cfcfcf` |
| Outer bottom / right | `#000000`, inside shadow `#828282` |
| Active caption field | (22,5)–(440,23), recessed one-pixel relief |
| Active caption face | `#2800aa` |
| Caption text origin | (32,6), WarpSans Bold 9 at 96 dpi |
| Close cell | (442,7)–(456,21), diagonal slash through a square |
| Minimize cell | (460,7)–(474,21), small recessed square |
| Maximize cell | (478,7)–(492,21), large recessed square |
| Title icon cell | (4,5)–(22,23), opens the window commands menu |
| Inactive title | `#7d7d7d` / `#828282` checker, `#cfcfcf` text |
| Menu selection | `#0000aa`, white text |

The inactive title does **not** retain the active caption's recessed border.
The upper frame texture and caption texture restart their checker phase at
their respective origins. Voice Manager at (12,344) in `desktop.png` supplies
the independent inactive-text and checker oracle.

Controls retain their original unpressed bitmaps and ignore hover. Pressed
controls reverse their relief; that state is an adaptation because the source
captures do not show held buttons. Maximize toggles ChonkStep maximize/restore,
and minimize enters its existing minimized-window system. The left icon is a
neutral document substitute because decoration requests do not supply app icons;
it opens the actual window menu on release and supports cancellation by dragging
away. Very narrow windows suppress controls before they overlap. Right-click
and keyboard window-menu access remain available.

## Typography, pixels, and bounds

Caption text uses the original WarpSans Bold bitmap. Menu/body text uses the
original Helv 8 bitmap verified against WarpCenter's **Command Prompts** row.
Both atlases contain ASCII and Latin-1 cells with original advances. No host font
discovery, hinting, antialiasing, or file I/O occurs for these glyphs at render
time. Uncovered Unicode runs use the existing resident shaping fallback, retain
combining marks and joining, and are not historical pixel matches.

Integer scales replicate source pixels exactly. Fractional geometry uses
`floor(n*scale+0.5)` and nearest sampling; sprites fill their rounded pixel cells.
1.25× and 1.5× are modern adaptations, not historical screenshot oracles. Titles,
menus, previews, and panels have explicit input/dimension limits.

Four nonoverlapping perimeter bands leave client pixels untouched. The bounded
title cache normalizes height and hover, and preserves controls when shaded.
The existing allocation and retained-pixel budgets pass unchanged. Edge-only
client decorations use the same four-pixel relief without a title field.

## Desktop and modern shell translation

The wallpaper contains IBM's original textured blue ground and wave wordmark.
Native 640×480, 800×600, and 1024×768 outputs select the corresponding original
256-color bitmap. Other sizes use proportional cover and nearest sampling,
including centered cropping on widescreen displays. This keeps original pixel
colors and avoids blur. The capture's WarpCenter reduced the desktop work area;
ChonkStep paints the full output and keeps its own bar/work-area behavior.

Posted desktop/window menus add a compact PM caption and working close button.
Cascades and transient menus use headerless gray popups. Their 20-pixel rows,
Helv labels, blue selection, solid cascade markers, and original 16-pixel
isometric folders translate WarpCenter's menu vocabulary to ChonkStep actions.
They do not advertise OS/2 commands that the compositor cannot perform.

Switcher, minimized-window previews, Overview, workspace labels, selection
plates and workspace close buttons all use the dedicated renderer. Live previews
and existing workspace navigation are retained. These are deliberate modern
adaptations using the same gray relief, purple titles and blue selection.

`auto` chooses this style for `os2-warp-4`; the explicit `os2warp` override works
with any theme. Light/dark application appearance leaves the historical chrome
and wallpaper fixed. External applications retain ownership of their widgets
and fonts. Omarchy export includes matching shell surface/selection colors and
the original background; Omarchy still owns its bar layout.

## Verification

```sh
cargo test --locked -p wm-theme --test os2warp --test decoration_contract
cargo test --locked -p wm-core os2warp
python3 scripts/os2warp-reference/generate_assets.py --check
scripts/e2e.sh --headless --test os2warp
xvfb-run -a cargo test --locked -p wm-x11 native_os2warp -- --ignored --test-threads=1
cargo run --locked -p wm-theme --example os2warp_preview -- /tmp/os2warp-review
```

The capture tests compare every active frame pixel outside the substituted app
icon, including the complete title string, at 1×/2×/3×. Independent inactive
text, menu text, and folder crops are also verified. Behavioral tests cover the
menu icon's press/release contract, narrow/fractional/Unicode layouts, edges,
shading, hover, pressed states, menu limits and fixed chrome across appearances.
The wallpaper tests compare each native rendition's entire decoded pixel buffer.

The isolated compositor test exercises Wayland and XWayland clients at 1× and
2×, checks the uploaded frame pixels, opens the icon's window menu, cycles the
switcher, reloads through System 7 and back, maximizes/restores, minimizes/restores and closes, then
opens the desktop menu. A private X11 server verifies frame uploads and cleanup
after switching from a BeOS tab shape. See [reviewed output](os2warp/preview/README.md).
