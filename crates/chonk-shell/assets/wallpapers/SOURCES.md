# Wallpaper sources

Every artwork here is embedded into the shell binary by
`crates/chonk-shell/src/wallpaper.rs`, so its provenance is the
binary's provenance. Recorded per file.

## Original to this project

- `lavender-grid.png`
- `amber-terminal.png`
- `teal-blueprint.png`
- `graphite-fold.png`
- `lunduke-navy.png` / `lunduke-navy-light.png` — drawn by
  `scripts/gen-lcos-wallpaper.py`, which is committed, so the artwork is
  reproducible from source rather than only from the PNG. Its two
  colours are sampled from LCOS's own boot splash (`boot/grub/splash.png`
  on the LCOS 0.3 ISO): a #081830 ground across 95.8% of the frame,
  inked #F8F8F8. Colour values are facts about a released product, not
  copyrightable expression, and nothing of LCOS's artwork is reproduced
  here — the concentric ring figure is this project's own, drawn
  deliberately so that LCOS's circular mark, which is Lunduke's, stays
  out of the shell binary. Both renditions are generated directly by
  that script rather than derived by `gen-wallpaper-renditions.py`.

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
