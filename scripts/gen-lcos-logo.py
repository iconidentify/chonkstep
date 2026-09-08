#!/usr/bin/env python3
"""Draws the LCOS dock mark: `crates/chonk-shell/assets/branding/lcos-logo-icon.png`.

Original to this project, and drawn rather than borrowed for two
reasons.

The first is that LCOS's own logo does not survive the size. It is a
wordmark -- a glowing ring around a plaque reading "THE / LUNDUKE /
COMPUTER OPERATING SYSTEM" on three lines. In a dock tile that is 56px
at scale 1, the third line is sub-pixel and the second is mush. Shrinking
somebody's wordmark until it is illegible serves nobody.

The second is provenance. Lunduke's mark is his; embedding a copy in this
binary would redistribute it. Everything LCOS-flavoured this project
ships is its own work for that reason -- see `gen-lcos-wallpaper.py`.

So this keeps what makes LCOS recognisable at a glance and is legible
small: the ring, the navy, the white. Four letters instead of three
lines. Colours are LCOS's own, sampled from its boot splash (#081830
ground, #F8F8F8 ink), which are facts about a released product rather
than copyrightable expression.

Requires Pillow and a DejaVu Bold face. Run from the repository root.
"""
from PIL import Image, ImageDraw, ImageFont

SIZE = 256           # matches chonkstep-logo-icon.png
SS = 4               # supersample; the ring and letterforms need it
OUT = "crates/chonk-shell/assets/branding/lcos-logo-icon.png"
FONT = "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"

NAVY = (0x08, 0x18, 0x30, 255)
INK = (0xF8, 0xF8, 0xF8, 255)
GLOW = (0xF8, 0xF8, 0xF8, 70)

W = SIZE * SS
img = Image.new("RGBA", (W, W), (0, 0, 0, 0))
d = ImageDraw.Draw(img)

# A filled disc, not a filled square. The tile's own material shows
# around it, exactly as the ChonkStep mark's disc does, so the mark sits
# in the dock instead of looking like a picture wedged into it -- and it
# reads on a light tile and a dark one alike.
pad = 2 * SS
d.ellipse([pad, pad, W - pad, W - pad], fill=NAVY)

# The ring. Two strokes: a soft wide one for the glow LCOS's own mark
# has, and a hard thin one to hold an edge when this is scaled down to
# 56 pixels.
ring_pad = 14 * SS
d.ellipse([ring_pad, ring_pad, W - ring_pad, W - ring_pad], outline=GLOW, width=9 * SS)
d.ellipse([ring_pad, ring_pad, W - ring_pad, W - ring_pad], outline=INK, width=4 * SS)

# "LCOS" sized to the ring's inner width rather than to a fixed point
# size, so the letters stay as large as the circle allows.
target = int(W - 2 * ring_pad - 26 * SS)
size = 10
while True:
    probe = ImageFont.truetype(FONT, size + 2)
    left, top, right, bottom = d.textbbox((0, 0), "LCOS", font=probe)
    if right - left > target or size > W:
        break
    size += 2
font = ImageFont.truetype(FONT, size)
left, top, right, bottom = d.textbbox((0, 0), "LCOS", font=font)
d.text(((W - (right - left)) / 2 - left, (W - (bottom - top)) / 2 - top), "LCOS", font=font, fill=INK)

img.resize((SIZE, SIZE), Image.LANCZOS).save(OUT)
print(f"wrote {OUT} ({SIZE}x{SIZE}, text set at {size}px before downsampling)")
