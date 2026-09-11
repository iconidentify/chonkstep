# WindowMaker compatibility oracle

Generated on 2026-09-11 from the unmodified `wm-theme/src/raster.rs` at main
`22f5590675cdc771ccdfc685547fb441de22adf9`, before the style refactor. The renderer
source was byte-compared with `git show` before generating these fixtures.

- Renderer source SHA-256: `63abfecdd332fe3ffdb1fe67a59b3d3444b4bc5a0dc96404509c8e17014d912c`
- Compressed oracle SHA-256: `4b664469d4ac314a8e52d169f4373f504db029cd9346c9b3e2db01a6f8322698`
- Test font SHA-256: `b5d64817b6331723b5e59eaaa6db90057cbed58e9733f65687f110638192359f`

720 cases: three theme button sets (stock miniaturize/close, close-only, all three),
scales 1/1.5/2, focused/inactive, resizable/fixed, shaded/expanded, empty/long titles,
and five interactions (idle, hovered, each of the three buttons pressed). Content
is 240×120 before shading. Exact request generation is shared between the explicit
generator and test; the oracle bytes themselves are never generated during tests.
`cases.txt` enumerates every case. The binary records the complete layout, every
part offset/dimension/pixel, frame size and retained bytes in little-endian order.

The unmodified DejaVu Sans Bold font is included only for deterministic tests and
the explicit generator. It is not linked into the compositor. The fixture replaces
the font database with this single face and fixes the locale to en-US, so installed
host fonts cannot silently change golden pixels. The font came from Arch Linux's
`ttf-dejavu` package (`/usr/share/fonts/TTF/DejaVuSans-Bold.ttf`); its full distributed
notices are in `FONT-LICENSE`. Upstream: https://dejavu-fonts.github.io/.

Deliberate regeneration into a **new** directory:

```sh
cargo run --release -p wm-theme --example decoration_goldens -- --write /tmp/new-chrome-oracle
```

The generator refuses to overwrite an existing directory. Copy fixtures only after
explaining the intended pixel/layout change and recording the source revision. This
oracle preserves WindowMaker; it is not a System 7 reference or an Apple font.
