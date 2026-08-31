# Wallpaper sources

Every artwork here is embedded into the shell binary by
`crates/chonk-shell/src/wallpaper.rs`, so its provenance is the
binary's provenance. Recorded per file.

## Original to this project

- `lavender-grid.png`
- `amber-terminal.png`
- `teal-blueprint.png`
- `graphite-fold.png`

## Composited over Omarchy's bundled background art

Ground taken from the artwork installed under
`/usr/share/omarchy/themes/<theme>/backgrounds/`, recomposed to
1672x941 with the ChonkStep mark rendered in each theme's own material
and the right quarter calmed for the dock column:

| File | Omarchy theme | Source file |
| --- | --- | --- |
| `jade-terrace.png` | `osaka-jade` | `3-mountain-moon.webp` |
| `ivory-orb.png` | `flexoki-light` | `1-orb.webp` |
| `indigo-waves.png` | `catppuccin` | `2-waves.webp` |

## Appearance renditions (derived in-repo)

Every artwork carries a counterpart rendition for the other side of
the light/dark appearance axis: `<name>-light.png` for the six
natively dark artworks, `ivory-orb-dark.png` for the natively light
one. (`classic-lavender` is a solid color on both sides and has no
file.) Each counterpart is derived from the committed original by
`scripts/gen-wallpaper-renditions.py` — a hue-preserving luminance
remap with per-artwork curves, tuned by eye — so its provenance is its
original's provenance, including the unresolved question below for the
three Omarchy-derived grounds.

**Unresolved:** Omarchy is MIT-licensed, but it ships no license or
attribution for the background images themselves, and they are not all
Omarchy's own work. ChonkStep is GPL-3.0, and these three files are
embedded in the binary rather than merely referenced. Clear the
underlying art's terms - or swap these three grounds for original
artwork - before shipping a release that contains them.
