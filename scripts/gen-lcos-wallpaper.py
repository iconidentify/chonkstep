#!/usr/bin/env python3
"""Generates the `lunduke-navy` artwork for the LCOS theme.

Original to this project. The palette is not invented: it is sampled
from LCOS's own boot splash (`boot/grub/splash.png` on the LCOS 0.3
ISO), whose ground is #081830 across 95.8% of the frame with a
near-white #F8F8F8 ink. Those two colours ARE the LCOS identity, and
this artwork is built from them.

The composition is deliberately NOT a copy of the LCOS mark. Lunduke's
circular logo is his; embedding it here would put artwork of unclear
licence into the shell binary, which is exactly the unresolved problem
SOURCES.md already records for the three Omarchy-derived grounds. What
this draws instead is a concentric ring figure in the same navy and the
same chiselled register -- a nod, not a reproduction.

Run from the repository root; requires numpy and Pillow. Writes both
appearance renditions:

    crates/chonk-shell/assets/wallpapers/lunduke-navy.png       (dark)
    crates/chonk-shell/assets/wallpapers/lunduke-navy-light.png (light)
"""
import numpy as np
from PIL import Image

W, H = 1672, 941
OUT = "crates/chonk-shell/assets/wallpapers"

# Sampled from the LCOS grub splash.
GROUND = np.array([0x08, 0x18, 0x30], dtype=float)
INK = np.array([0xF8, 0xF8, 0xF8], dtype=float)

# The dock sits in the right column; that quarter is calmed so the
# sidebar belongs to the composition instead of fighting it. Same
# treatment the other artworks get.
DOCK_FROM = 0.78


def ring_figure(cx, cy):
    """Concentric rings with a brightened upper-left arc, the way a
    chiselled bevel catches a light source from that quarter."""
    yy, xx = np.mgrid[0:H, 0:W].astype(float)
    dx, dy = xx - cx, yy - cy
    r = np.hypot(dx, dy)
    ang = np.arctan2(-dy, dx)  # screen y grows downward

    ink = np.zeros((H, W), dtype=float)
    # radius, stroke half-width, base alpha
    for radius, half, alpha in ((330, 2.0, 0.92), (256, 1.4, 0.46),
                                (186, 1.2, 0.30), (108, 1.0, 0.22)):
        # Smooth the stroke edge over a pixel so the ring does not alias
        # into a dotted line at this radius.
        band = np.clip(1.0 - (np.abs(r - radius) - half), 0.0, 1.0)
        # Light from the upper left: strongest near 135 degrees.
        lit = 0.55 + 0.45 * np.cos(ang - np.deg2rad(135))
        ink = np.maximum(ink, band * alpha * lit)

    # A single horizontal rule through the figure, the flat datum the
    # rings are measured against.
    rule_y = cy + 330
    ink = np.maximum(ink, np.clip(1.0 - np.abs(yy - rule_y), 0.0, 1.0) * 0.16)
    return ink


def compose(dark: bool) -> Image.Image:
    yy, xx = np.mgrid[0:H, 0:W].astype(float)

    if dark:
        top, bottom = GROUND * 1.06, GROUND * 0.62
        ink_color, ink_gain = INK, 1.0
    else:
        # The light rendition inverts the relationship rather than the
        # colours: navy becomes the ink on a cool paper ground, so the
        # theme keeps its identity instead of turning into a grey.
        top = np.array([0xE8, 0xEC, 0xF4], dtype=float)
        bottom = np.array([0xCE, 0xD6, 0xE4], dtype=float)
        ink_color, ink_gain = GROUND, 0.85

    t = (yy / H)[..., None]
    img = top * (1 - t) + bottom * t

    # A soft radial lift behind the figure, matching the glow the LCOS
    # splash carries around its mark.
    cx, cy = W * 0.34, H * 0.50
    glow = np.exp(-(((xx - cx) ** 2 + (yy - cy) ** 2) / (2 * (W * 0.30) ** 2)))
    img += (12.0 if dark else -10.0) * glow[..., None]

    ink = ring_figure(cx, cy)[..., None] * ink_gain
    img = img * (1 - ink) + ink_color * ink

    # A little dither, not texture. This ground is a wide, shallow
    # gradient and would band visibly without it; sigma is kept low
    # because per-pixel noise is nearly incompressible and this file is
    # embedded in the shell binary -- at sigma 2.0 the PNG came out at
    # 1.7MB against the 765KB of the photographic artworks. Seeded so
    # the committed PNG is reproducible.
    rng = np.random.default_rng(0x1C05)
    img += rng.normal(0.0, 0.9, (H, W, 1))

    # Calm the dock column: flatten toward the local ground and drop a
    # little contrast, so instrument tiles read against it.
    ramp = np.clip((xx - W * DOCK_FROM) / (W * (1 - DOCK_FROM)), 0.0, 1.0)[..., None]
    flat = img.mean(axis=(0, 1), keepdims=True) * (0.72 if dark else 1.04)
    img = img * (1 - 0.85 * ramp) + flat * (0.85 * ramp)

    img = np.clip(img, 0, 255).astype(np.uint8)
    edge = img[:, -1, :].mean(axis=0).round().astype(int)
    print(f"  {'dark' if dark else 'light'} dock_color = ({edge[0]}, {edge[1]}, {edge[2]})")
    return Image.fromarray(img, "RGB")


if __name__ == "__main__":
    print("lunduke-navy:")
    compose(dark=True).save(f"{OUT}/lunduke-navy.png")
    compose(dark=False).save(f"{OUT}/lunduke-navy-light.png")
    print(f"  wrote {OUT}/lunduke-navy.png and {OUT}/lunduke-navy-light.png")
