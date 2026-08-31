#!/usr/bin/env python3
"""chonk-net: the network instrument, and the panel concept's showcase.

One dock tile shows the machine's live link — Wi-Fi as signal arcs
carved into a sunken well and lit by strength, wired as a machined
jack glyph, a clearly-dead face for no link at all. Clicking the tile
opens a *detail panel*: a chiseled list of the networks in range with
per-row signal bars, band tag, a lock glyph for secured networks, the
current connection's SSID, band, bitrate and address in a header, and
a rescan row at the bottom. **Read-only in this iteration**: it never
joins, leaves, or reconfigures anything — see `netdata.py`'s frozen
command table for the proof.

Both halves ride the Python SDK (`bindings/python/chonkdock`): the
tile through the classic `Dockapp` callbacks, the panel through the
SDK's panel API — `open_panel()` returning a `Panel` whose `paint` /
`on_input` / `on_closed` callbacks this file fills in. The v2 wire
underneath: `0x05 OpenPanel {w, h}` answered by `0x87 PanelOpened` (a
grant, possibly clamped) or `0x88 PanelClosed {reason, pad}` with
reason 3 (refused); `0x06 PanelFrame` is **banded** — `{generation,
y, band_height, width, pixels}`, each band fitting the 256 KiB
transport, a full repaint being a top-to-bottom run sharing one
generation (the SDK slices this); `0x89 PanelInput` is the Input
layout in panel-local device pixels, and may additionally carry
kind 6 = Motion (panels only) — the hover signal. The shell advertises protocol 2
in its Welcome; against a v1 shell chonk-net stays tile-only with one
log line rather than dying by ProtocolError. Hover repaints ship as
partial updates (`Panel.draw_rows` of just the rows that changed);
and the invariant that matters most — **never a frame before the
grant** — is enforced by the SDK itself (`Panel.draw*` raise until
`PanelOpened` arrives) and re-checked on the wire by the tests.

Run with `--render out.png` for headless faces (no dock needed); that
is the design loop and the test harness's ground truth.
"""

from __future__ import annotations

import os
import struct
import sys
import threading
import time
import zlib

_HERE = os.path.dirname(os.path.abspath(__file__))
for _candidate in (_HERE, os.path.join(_HERE, "..", "..", "bindings", "python")):
    if os.path.isdir(os.path.join(_candidate, "chonkdock")):
        sys.path.insert(0, os.path.abspath(_candidate))
        break
sys.path.insert(0, _HERE)  # netdata lives next to this script

import netdata  # noqa: E402
from chonkdock import (  # noqa: E402
    Dockapp,
    INPUT_MOTION,
    INPUT_PRESS,
    INPUT_RELEASE,
    INPUT_LEAVE,
    LOG_INFO,
    LOG_WARN,
    PanelError,
)
from chonkdock import wire  # noqa: E402

# ---------------------------------------------------------------------
# Chiseled drawing — the desktop's relief recipes (+80/-40 relative
# bevels over gradients, hard black outer lines), same numbers as
# crates/wm-theme and the chonk-switch example.
# ---------------------------------------------------------------------

def _cl(v):
    return 0 if v < 0 else (255 if v > 255 else v)


def fill_rect(buf, W, x, y, w, h, rgb):
    H = len(buf) // (W * 4)
    x0, y0 = max(x, 0), max(y, 0)
    x1, y1 = min(x + w, W), min(y + h, H)
    if x1 <= x0 or y1 <= y0:
        return
    row = bytes((rgb[0], rgb[1], rgb[2], 255)) * (x1 - x0)
    for yy in range(y0, y1):
        base = (yy * W + x0) * 4
        buf[base:base + len(row)] = row


def op_rect(buf, W, x, y, w, h, delta):
    H = len(buf) // (W * 4)
    x0, y0 = max(x, 0), max(y, 0)
    x1, y1 = min(x + w, W), min(y + h, H)
    for yy in range(y0, y1):
        base = (yy * W + x0) * 4
        for xx in range(x1 - x0):
            i = base + xx * 4
            buf[i] = _cl(buf[i] + delta)
            buf[i + 1] = _cl(buf[i + 1] + delta)
            buf[i + 2] = _cl(buf[i + 2] + delta)


def diag_gradient(buf, W, x, y, w, h, c0, c1):
    span = max(w + h - 2, 1)
    for yy in range(h):
        for xx in range(w):
            f = (xx + yy) / span
            i = ((y + yy) * W + x + xx) * 4
            buf[i] = int(c0[0] + (c1[0] - c0[0]) * f)
            buf[i + 1] = int(c0[1] + (c1[1] - c0[1]) * f)
            buf[i + 2] = int(c0[2] + (c1[2] - c0[2]) * f)
            buf[i + 3] = 255


def vert_gradient(buf, W, x, y, w, h, c0, c1):
    for yy in range(h):
        f = yy / max(h - 1, 1)
        rgb = tuple(int(a + (b - a) * f) for a, b in zip(c0, c1))
        fill_rect(buf, W, x, y + yy, w, 1, rgb)


def raised2_bevel(buf, W, x, y, w, h, t):
    op_rect(buf, W, x, y, w, t, 80)
    op_rect(buf, W, x, y + t, t, h - t, 80)
    op_rect(buf, W, x, y + h - 2 * t, w - 2 * t, t, -40)
    fill_rect(buf, W, x, y + h - t, w, t, (0, 0, 0))
    op_rect(buf, W, x + w - 2 * t, y, t, h - 2 * t, -40)
    fill_rect(buf, W, x + w - t, y, t, h - t, (0, 0, 0))


def sunken_bevel(buf, W, x, y, w, h, t):
    op_rect(buf, W, x, y, w, t, -40)
    op_rect(buf, W, x, y + t, t, h - t, -40)
    op_rect(buf, W, x, y + h - t, w, t, 80)
    op_rect(buf, W, x + w - t, y, t, h - 2 * t, 80)


def _lighten(c, d):
    return tuple(_cl(v + d) for v in c)


def _lerp(c0, c1, f):
    return tuple(int(a + (b - a) * f) for a, b in zip(c0, c1))


# ---------------------------------------------------------------------
# A 5x7 pixel font — enough letterforms for SSIDs, sized by an integer
# factor so it stays crisp at every scale. Rows are 5-bit ints, bit 4
# leftmost.
# ---------------------------------------------------------------------

FONT = {
    " ": (0, 0, 0, 0, 0, 0, 0),
    "0": (0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E),
    "1": (0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E),
    "2": (0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F),
    "3": (0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E),
    "4": (0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02),
    "5": (0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E),
    "6": (0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E),
    "7": (0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08),
    "8": (0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E),
    "9": (0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C),
    "A": (0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11),
    "B": (0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E),
    "C": (0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E),
    "D": (0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C),
    "E": (0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F),
    "F": (0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10),
    "G": (0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F),
    "H": (0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11),
    "I": (0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E),
    "J": (0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C),
    "K": (0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11),
    "L": (0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F),
    "M": (0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11),
    "N": (0x11, 0x19, 0x15, 0x13, 0x11, 0x11, 0x11),
    "O": (0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E),
    "P": (0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10),
    "Q": (0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D),
    "R": (0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11),
    "S": (0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E),
    "T": (0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04),
    "U": (0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E),
    "V": (0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04),
    "W": (0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A),
    "X": (0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11),
    "Y": (0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04),
    "Z": (0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F),
    "a": (0x00, 0x00, 0x0E, 0x01, 0x0F, 0x11, 0x0F),
    "b": (0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x1E),
    "c": (0x00, 0x00, 0x0E, 0x10, 0x10, 0x11, 0x0E),
    "d": (0x01, 0x01, 0x0F, 0x11, 0x11, 0x11, 0x0F),
    "e": (0x00, 0x00, 0x0E, 0x11, 0x1F, 0x10, 0x0E),
    "f": (0x06, 0x08, 0x1C, 0x08, 0x08, 0x08, 0x08),
    "g": (0x00, 0x0F, 0x11, 0x11, 0x0F, 0x01, 0x0E),
    "h": (0x10, 0x10, 0x1E, 0x11, 0x11, 0x11, 0x11),
    "i": (0x04, 0x00, 0x0C, 0x04, 0x04, 0x04, 0x0E),
    "j": (0x02, 0x00, 0x06, 0x02, 0x02, 0x12, 0x0C),
    "k": (0x10, 0x10, 0x12, 0x14, 0x18, 0x14, 0x12),
    "l": (0x0C, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E),
    "m": (0x00, 0x00, 0x1A, 0x15, 0x15, 0x15, 0x15),
    "n": (0x00, 0x00, 0x1E, 0x11, 0x11, 0x11, 0x11),
    "o": (0x00, 0x00, 0x0E, 0x11, 0x11, 0x11, 0x0E),
    "p": (0x00, 0x00, 0x1E, 0x11, 0x1E, 0x10, 0x10),
    "q": (0x00, 0x00, 0x0F, 0x11, 0x0F, 0x01, 0x01),
    "r": (0x00, 0x00, 0x16, 0x19, 0x10, 0x10, 0x10),
    "s": (0x00, 0x00, 0x0F, 0x10, 0x0E, 0x01, 0x1E),
    "t": (0x08, 0x08, 0x1C, 0x08, 0x08, 0x09, 0x06),
    "u": (0x00, 0x00, 0x11, 0x11, 0x11, 0x13, 0x0D),
    "v": (0x00, 0x00, 0x11, 0x11, 0x11, 0x0A, 0x04),
    "w": (0x00, 0x00, 0x11, 0x11, 0x15, 0x15, 0x0A),
    "x": (0x00, 0x00, 0x11, 0x0A, 0x04, 0x0A, 0x11),
    "y": (0x00, 0x00, 0x11, 0x11, 0x0F, 0x01, 0x0E),
    "z": (0x00, 0x00, 0x1F, 0x02, 0x04, 0x08, 0x1F),
    ".": (0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C),
    ",": (0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08),
    "-": (0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00),
    "_": (0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x1F),
    "/": (0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10),
    "\\": (0x10, 0x10, 0x08, 0x04, 0x02, 0x01, 0x01),
    ":": (0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00),
    "(": (0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02),
    ")": (0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08),
    "'": (0x04, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00),
    '"': (0x0A, 0x0A, 0x00, 0x00, 0x00, 0x00, 0x00),
    "!": (0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04),
    "?": (0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04),
    "%": (0x19, 0x1A, 0x02, 0x04, 0x08, 0x0B, 0x13),
    "+": (0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00),
    "*": (0x00, 0x04, 0x15, 0x0E, 0x15, 0x04, 0x00),
    "&": (0x08, 0x14, 0x14, 0x08, 0x15, 0x12, 0x0D),
    "#": (0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A),
    "@": (0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0E),
    "[": (0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E),
    "]": (0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E),
    "=": (0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00),
    "<": (0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02),
    ">": (0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08),
}
FONT_FALLBACK = (0x1F, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1F)


def text_width(text: str, size: int = 1) -> int:
    return (6 * len(text) - 1) * size if text else 0


def draw_text(buf, W, x, y, text, rgb, size: int = 1):
    """Left-aligned; each font pixel is a size x size block."""
    cx = x
    for ch in text:
        rows = FONT.get(ch, FONT_FALLBACK)
        for ry, row in enumerate(rows):
            for rx in range(5):
                if row & (0x10 >> rx):
                    fill_rect(buf, W, cx + rx * size, y + ry * size,
                              size, size, rgb)
        cx += 6 * size
    return cx


def clip_text(text: str, size: int, max_px: int) -> str:
    """Ellipsize to fit; SSIDs come from the air and can be long."""
    if text_width(text, size) <= max_px:
        return text
    while text and text_width(text + "...", size) > max_px:
        text = text[:-1]
    return text + "..."


# ---------------------------------------------------------------------
# Palette. theme_toml's [tile.fill] is the tile face; the appearance
# key picks the panel's paper-or-slate reading surfaces.
# ---------------------------------------------------------------------

CLASSIC_TILE = ((0xA6, 0xA6, 0xB6), (0x51, 0x55, 0x61))

#: The tile's sunken display well and the panel's list well: lit paper
#: in light mode, deep slate in dark — the same voltage as the
#: switch's track, so the instruments read as one desktop.
WELL_LIGHT = ((0xEE, 0xEA, 0xDF), (0xC9, 0xC5, 0xB9))
WELL_DARK = ((0x12, 0x14, 0x1C), (0x22, 0x26, 0x32))
INK_LIGHT = (0x22, 0x22, 0x28)   # ink on paper
INK_DARK = (0xD6, 0xD8, 0xE0)    # ink on slate
FAINT_LIGHT = (0x9A, 0x96, 0x8C)
FAINT_DARK = (0x55, 0x5B, 0x6A)
LIT_ON_DARK = (0xE8, 0xE4, 0xD6)  # lit strokes inside a dark tile well
ACCENT_OK = (0x4F, 0xB3, 0x62)    # link-up LED
ACCENT_DEAD = (0x8A, 0x33, 0x2E)  # link-down LED


class Palette:
    def __init__(self, theme_toml: str = "", appearance: str | None = None):
        self.tile = CLASSIC_TILE
        self.appearance = "dark"  # protocol: absent means dark
        try:
            import tomllib
            table = tomllib.loads(theme_toml)
            if table.get("appearance") in ("light", "dark"):
                self.appearance = table["appearance"]
            fill = table["tile"]["fill"]
            if "Gradient" in fill:
                g = fill["Gradient"]
                self.tile = (_rgb(g["from"]), _rgb(g["to"]))
            elif "Solid" in fill:
                c = _rgb(fill["Solid"])
                self.tile = (c, c)
        except Exception:  # noqa: BLE001 — any malformed palette falls back
            pass
        if appearance in ("light", "dark"):
            self.appearance = appearance
        light = self.appearance == "light"
        self.well = WELL_LIGHT if light else WELL_DARK
        self.ink = INK_LIGHT if light else INK_DARK
        self.faint = FAINT_LIGHT if light else FAINT_DARK


def _rgb(c):
    return (int(c["r"]) & 0xFF, int(c["g"]) & 0xFF, int(c["b"]) & 0xFF)


# ---------------------------------------------------------------------
# The tile face. A sunken display well on the themed face; inside it,
# the link state drawn to read across a room: lit arcs, a lit jack, or
# a dead well with an unlit glyph and a red status lamp.
# ---------------------------------------------------------------------

def signal_arcs(signal: int) -> int:
    """How many arcs (above the dot) light for a 0-100 signal."""
    return (signal >= 25) + (signal >= 50) + (signal >= 75)


def render_tile(buf, size, link: netdata.Link, pal: Palette,
                scale: float = 1.0):
    t = max(1, round(scale))
    diag_gradient(buf, size, 0, 0, size, size, *pal.tile)
    raised2_bevel(buf, size, 0, 0, size, size, t)

    # The display well — always the dark slate, whatever the desktop
    # appearance: this is a lit instrument window, not a page.
    wx = round(size * 0.125)
    wy = round(size * 0.125)
    ww = size - 2 * wx
    wh = size - 2 * wy
    op_rect(buf, size, wx, wy, ww, wh, -24)
    sunken_bevel(buf, size, wx, wy, ww, wh, t)
    ix, iy = wx + t, wy + t
    iw, ih = ww - 2 * t, wh - 2 * t
    vert_gradient(buf, size, ix, iy, iw, ih, WELL_DARK[0], WELL_DARK[1])
    op_rect(buf, size, ix, iy, iw, t, -18)

    cx = ix + iw // 2
    lamp = ACCENT_DEAD
    if link.kind == "wifi":
        _tile_wifi(buf, size, ix, iy, iw, ih, link.signal)
        lamp = ACCENT_OK
    elif link.kind == "wired":
        _tile_wired(buf, size, ix, iy, iw, ih)
        lamp = ACCENT_OK
    elif link.kind == "unavailable":
        _tile_unknown(buf, size, ix, iy, iw, ih)
        lamp = (0x6A, 0x5B, 0x28)  # amber: can't tell
    else:
        _tile_dead(buf, size, ix, iy, iw, ih)

    # The status lamp: a small LED on the well's floor, green for a
    # live link, red for none — the one-glance summary.
    lr = max(2, size // 18)
    ly = iy + ih - lr - max(2, size // 18)
    fill_rect(buf, size, cx - lr // 2, ly, lr, lr, lamp)
    op_rect(buf, size, cx - lr // 2, ly, lr, max(1, lr // 3), 50)
    sunken_bevel(buf, size, cx - lr // 2 - 1, ly - 1, lr + 2, lr + 2, 1)


def _arc_points(cx, cy, r, half, clip):
    """A 90-degree arc opening upward: the classic Wi-Fi fan.
    `clip` = (x0, y0, x1, y1) keeps every point inside the well."""
    pts = []
    x0, y0, x1, y1 = clip
    r2o, r2i = (r + half) ** 2, max(0, (r - half)) ** 2
    for yy in range(-r - half - 1, 1):
        for xx in range(-r - half - 1, r + half + 2):
            d2 = xx * xx + yy * yy
            if r2i <= d2 <= r2o and -yy >= abs(xx):
                px, py = cx + xx, cy + yy
                if x0 <= px < x1 and y0 <= py < y1:
                    pts.append((px, py))
    return pts


def _wifi_fan(buf, size, ix, iy, iw, ih, lit_arcs, color_lit, color_off):
    """The fan glyph, sized to stay inside its well: dot plus three
    arcs, the outermost reaching just short of the well walls."""
    m = min(iw, ih)
    cx = ix + iw // 2
    cy = iy + round(ih * 0.82)
    clip = (ix + 1, iy + 1, ix + iw - 1, iy + ih - 1)
    dot = max(2, round(m * 0.075))
    half = max(1, round(m * 0.05))
    for yy in range(-dot, dot + 1):
        for xx in range(-dot, dot + 1):
            if xx * xx + yy * yy <= dot * dot:
                px, py = cx + xx, cy + yy
                if clip[0] <= px < clip[2] and clip[1] <= py < clip[3]:
                    fill_rect(buf, size, px, py, 1, 1,
                              color_lit if lit_arcs >= 0 else color_off)
    for k in range(3):
        r = round(m * (0.22 + 0.15 * k))
        color = color_lit if k < lit_arcs else color_off
        for px, py in _arc_points(cx, cy, r, half, clip):
            fill_rect(buf, size, px, py, 1, 1, color)


def _tile_wifi(buf, size, ix, iy, iw, ih, signal):
    _wifi_fan(buf, size, ix, iy, iw, ih, signal_arcs(signal),
              LIT_ON_DARK, FAINT_DARK)


def _tile_wired(buf, size, ix, iy, iw, ih):
    """A machined RJ45 jack, face on: body, latch, lit pins."""
    bw = round(iw * 0.52)
    bh = round(ih * 0.44)
    bx = ix + (iw - bw) // 2
    by = iy + round(ih * 0.14)
    t = max(1, min(iw, ih) // 36)
    # Body outline, lit.
    for yy in range(bh):
        for xx in range(bw):
            edge = xx < t or xx >= bw - t or yy < t or yy >= bh - t
            if edge:
                fill_rect(buf, size, bx + xx, by + yy, 1, 1, LIT_ON_DARK)
    # Latch tab below the body.
    lw = round(bw * 0.5)
    lx = bx + (bw - lw) // 2
    fill_rect(buf, size, lx, by + bh, lw, t, LIT_ON_DARK)
    fill_rect(buf, size, lx + lw // 4, by + bh + t, lw // 2, t, LIT_ON_DARK)
    # Pins: short lit teeth inside the top of the body.
    pins = 4
    pw = max(1, t)
    gap = (bw - 2 * t - pins * pw) // (pins + 1)
    px = bx + t + gap
    for _ in range(pins):
        fill_rect(buf, size, px, by + t, pw, round(bh * 0.30), LIT_ON_DARK)
        px += pw + gap
    # The cable stem rising from the body.
    fill_rect(buf, size, bx + bw // 2 - t // 2 - 1, iy + max(1, t),
              max(2, t), by - iy - max(1, t), LIT_ON_DARK)


def _tile_dead(buf, size, ix, iy, iw, ih):
    """No link: the unlit fan, cut by a slash."""
    _wifi_fan(buf, size, ix, iy, iw, ih, -1, FAINT_DARK, FAINT_DARK)
    # The slash, in the dead lamp's red so it cannot read as signal.
    x0, y0 = ix + round(iw * 0.20), iy + round(ih * 0.14)
    x1, y1 = ix + round(iw * 0.78), iy + round(ih * 0.80)
    steps = max(x1 - x0, y1 - y0)
    w = max(2, min(iw, ih) // 16)
    for s in range(steps + 1):
        px = x0 + (x1 - x0) * s // steps
        py = y0 + (y1 - y0) * s // steps
        fill_rect(buf, size, px, py, w, w, ACCENT_DEAD)


def _tile_unknown(buf, size, ix, iy, iw, ih):
    """Tools missing: an honest engraved question mark, unlit."""
    sz = max(2, round(ih / 10))
    tx = ix + (iw - text_width("?", sz)) // 2
    ty = iy + (ih - 7 * sz) // 2 - sz
    draw_text(buf, size, tx + 1, ty + 1, "?", (0, 0, 0))
    draw_text(buf, size, tx, ty, "?", FAINT_DARK, sz)


# ---------------------------------------------------------------------
# The panel: current-connection header, the scan list, the rescan row.
# Returns the row hitboxes so input handling and rendering can never
# disagree about where a row is.
# ---------------------------------------------------------------------

PANEL_LOGICAL_W = 700
PANEL_LOGICAL_H = 500


def panel_request_px(scale: float) -> tuple[int, int]:
    w = min(wire.MAX_PANEL_PX, round(PANEL_LOGICAL_W * scale))
    h = min(wire.MAX_PANEL_PX, round(PANEL_LOGICAL_H * scale))
    return w, h


def _bars(buf, W, x, y, h, signal, on, off):
    """Four ascending signal bars, level = signal quartile."""
    lit = 1 + (signal >= 30) + (signal >= 55) + (signal >= 80)
    bw = max(2, h // 4)
    for k in range(4):
        bh = round(h * (0.4 + 0.2 * k))
        bx = x + k * (bw + max(1, bw // 2))
        color = on if k < lit else off
        fill_rect(buf, W, bx, y + h - bh, bw, bh, color)
    return x + 4 * (bw + max(1, bw // 2))


def _lock(buf, W, x, y, s, rgb):
    """A padlock in a 5x7-ish cell scaled by s: shackle + body."""
    fill_rect(buf, W, x + s, y, 3 * s, s, rgb)          # shackle top
    fill_rect(buf, W, x, y + s, s, 2 * s, rgb)          # shackle left
    fill_rect(buf, W, x + 4 * s, y + s, s, 2 * s, rgb)  # shackle right
    fill_rect(buf, W, x, y + 3 * s, 5 * s, 4 * s, rgb)  # body


def render_panel(buf, W, H, snap: netdata.Snapshot, pal: Palette,
                 scale: float = 1.0, hover: int | None = None,
                 rescan_state: str = "ready", pressed_row: int | None = None):
    """Paints the whole panel; returns a list of hitboxes:
    [(y0, y1, ("net", i) | ("rescan",))]. `hover` indexes that list.
    `rescan_state` is "ready" | "scanning" | "wait:<seconds>"."""
    s = max(1, round(scale))
    t = max(1, s)
    boxes = []

    diag_gradient(buf, W, 0, 0, W, H, *pal.tile)
    raised2_bevel(buf, W, 0, 0, W, H, t)
    margin = 10 * s

    # -- header: the current connection, on a raised plate ------------
    head_h = 78 * s
    hx, hy = margin, margin
    hw = W - 2 * margin
    face0 = _lighten(pal.tile[0], 14)
    face1 = _lighten(pal.tile[1], 14)
    vert_gradient(buf, W, hx, hy, hw, head_h, face0, face1)
    raised2_bevel(buf, W, hx, hy, hw, head_h, t)

    link = snap.link
    glyph_w = 64 * s
    gx, gy = hx + 8 * s, hy + 8 * s
    gh = head_h - 16 * s
    vert_gradient(buf, W, gx, gy, glyph_w, gh, *WELL_DARK)
    sunken_bevel(buf, W, gx, gy, glyph_w, gh, t)
    inner = (gx + t, gy + t, glyph_w - 2 * t, gh - 2 * t)
    if link.kind == "wifi":
        _tile_wifi(buf, W, *inner, link.signal)
    elif link.kind == "wired":
        _tile_wired(buf, W, *inner)
    elif link.kind == "unavailable":
        _tile_unknown(buf, W, *inner)
    else:
        _tile_dead(buf, W, *inner)

    ink_on_tile = INK_LIGHT if _luma(pal.tile[0]) > 120 else INK_DARK
    tx = gx + glyph_w + 12 * s
    tw = hx + hw - tx - 8 * s
    if link.kind == "wifi":
        title = link.ssid or "(associated)"
        parts = []
        if link.band:
            parts.append(f"{link.band} GHz")
        if link.bitrate_mbps:
            parts.append(f"{link.bitrate_mbps} Mb/s")
        if link.security:
            parts.append(link.security)
        if link.ip4:
            parts.append(link.ip4)
        sub = "  ".join(parts) or f"signal {link.signal}%"
    elif link.kind == "wired":
        title = "Wired"
        parts = [link.ifname]
        if link.bitrate_mbps:
            parts.append(f"{link.bitrate_mbps} Mb/s")
        if link.ip4:
            parts.append(link.ip4)
        sub = "  ".join(p for p in parts if p)
    elif link.kind == "unavailable":
        title = "Unknown"
        sub = snap.scan_note or "network tools unavailable"
    else:
        title = "Not connected"
        sub = snap.scan_note or ""
    draw_text(buf, W, tx + s, hy + 15 * s + s, clip_text(title, 3 * s, tw),
              (0, 0, 0), 3 * s)
    draw_text(buf, W, tx, hy + 15 * s, clip_text(title, 3 * s, tw),
              _lighten(ink_on_tile, 20 if pal.appearance == "dark" else 0),
              3 * s)
    draw_text(buf, W, tx, hy + 15 * s + 26 * s,
              clip_text(sub, 2 * s, tw), _op_ink(ink_on_tile, -30), 2 * s)

    # -- the list well ------------------------------------------------
    rescan_h = 34 * s
    ly = hy + head_h + 8 * s
    lh = H - ly - margin - rescan_h - 8 * s
    lx, lw = margin, W - 2 * margin
    op_rect(buf, W, lx, ly, lw, lh, -20)
    sunken_bevel(buf, W, lx, ly, lw, lh, t)
    wx, wy = lx + t, ly + t
    ww, wh = lw - 2 * t, lh - 2 * t
    vert_gradient(buf, W, wx, wy, ww, wh, *pal.well)
    op_rect(buf, W, wx, wy, ww, t, -16)

    row_h = 30 * s
    if snap.networks:
        max_rows = wh // row_h
        for i, net in enumerate(snap.networks[:max_rows]):
            ry = wy + i * row_h
            idx = len(boxes)
            boxes.append((ry, ry + row_h, ("net", i)))
            hovered = hover == idx
            if net.in_use:
                op_rect(buf, W, wx, ry, ww, row_h,
                        14 if pal.appearance == "dark" else -10)
            if hovered:
                op_rect(buf, W, wx, ry, ww, row_h,
                        18 if pal.appearance == "dark" else -14)
                sunken_bevel(buf, W, wx, ry, ww, row_h, s)
            # separator
            if i:
                op_rect(buf, W, wx + 6 * s, ry, ww - 12 * s, s,
                        18 if pal.appearance == "light" else -14)

            cy = ry + row_h // 2
            if net.in_use:
                # a lit lamp in the row's left gutter: "you are here"
                lr = 4 * s
                fill_rect(buf, W, wx + 3 * s, cy - lr // 2, lr, lr,
                          ACCENT_OK)
                op_rect(buf, W, wx + 3 * s, cy - lr // 2, lr,
                        max(1, lr // 3), 40)
            bx = wx + 12 * s
            after = _bars(buf, W, bx, cy - 8 * s, 16 * s, net.signal,
                          pal.ink, pal.faint)
            # right side, in fixed columns so the digits line up:
            # [lock] [band] [signal]
            pct_w = text_width("100", 2 * s)
            band_w = text_width("2.4", 2 * s)
            pct_x = wx + ww - 10 * s - pct_w
            band_x = pct_x - 14 * s - band_w
            lock_x = band_x - 16 * s - 5 * (2 * s)
            pct = f"{net.signal}"
            draw_text(buf, W, pct_x + pct_w - text_width(pct, 2 * s),
                      cy - 7 * s, pct, pal.faint, 2 * s)
            band = net.band or "?"
            draw_text(buf, W, band_x + band_w - text_width(band, 2 * s),
                      cy - 7 * s, band, pal.ink, 2 * s)
            if net.security:
                _lock(buf, W, lock_x, cy - 7 * s, 2 * s, pal.ink)
            name_w = lock_x - 10 * s - (after + 10 * s)
            name = clip_text(net.ssid, 2 * s, name_w)
            draw_text(buf, W, after + 10 * s, cy - 7 * s, name,
                      pal.ink, 2 * s)
    else:
        msg = snap.scan_note or "nothing in range"
        mw = text_width(msg, 2 * s)
        draw_text(buf, W, wx + (ww - mw) // 2, wy + wh // 2 - 7 * s,
                  msg, pal.faint, 2 * s)

    # -- the rescan row -----------------------------------------------
    ry = H - margin - rescan_h
    idx = len(boxes)
    boxes.append((ry, ry + rescan_h, ("rescan",)))
    pressed = pressed_row == idx
    rface0 = _lighten(pal.tile[0], 6 if pressed else 20)
    rface1 = _lighten(pal.tile[1], 6 if pressed else 20)
    vert_gradient(buf, W, lx, ry, lw, rescan_h, rface0, rface1)
    if pressed:
        op_rect(buf, W, lx, ry, lw, rescan_h, -16)
        sunken_bevel(buf, W, lx, ry, lw, rescan_h, t)
    else:
        raised2_bevel(buf, W, lx, ry, lw, rescan_h, t)
        if hover == idx:
            op_rect(buf, W, lx + t, ry + t, lw - 2 * t, rescan_h - 2 * t, 10)
    if rescan_state == "scanning":
        label = "SCANNING..."
    elif rescan_state.startswith("wait:"):
        label = f"RESCAN ({rescan_state[5:]}s)"
    else:
        label = "RESCAN"
    lw_px = text_width(label, 2 * s)
    lx_px = lx + (lw - lw_px) // 2
    ly_px = ry + (rescan_h - 14 * s) // 2
    draw_text(buf, W, lx_px + s, ly_px + s, label, (0, 0, 0), 2 * s)
    draw_text(buf, W, lx_px, ly_px, label, ink_on_tile, 2 * s)
    return boxes


def _luma(c):
    return (c[0] * 3 + c[1] * 6 + c[2]) // 10


def _op_ink(c, d):
    return _lighten(c, d if _luma(c) > 120 else -d)


# ---------------------------------------------------------------------
# The dockapp, on the SDK's panel API. `open_panel()` sends the
# request (and raises PanelError against a v1 shell — caught below,
# tile-only from then on); the returned Panel's callbacks are wired
# here. The SDK's state machine makes a frame-before-grant
# unrepresentable, and streams every update as v2 bands.
# ---------------------------------------------------------------------

IDLE_TICK = 2.5          # tile sampling cadence, panel shut
PANEL_TICK = 1.0         # loop wake while the panel is open
PANEL_REFRESH = 3.0      # data refresh while the panel is open


class NetApp(Dockapp):
    def __init__(self, dockapp_id="chonk-net"):
        super().__init__(dockapp_id, tile_units=1,
                         redraw_interval=IDLE_TICK)
        self.pal = Palette("")
        self.snap = netdata.Snapshot()
        self.backend = None  # cached after first detect
        self.hover = None
        self.pressed_row = None
        self.boxes = []
        self.gate = netdata.RescanGate()
        self._scan_thread = None
        self._scan_result = None
        self._scan_lock = threading.Lock()
        self._painted = None
        self._panel_painted = None
        self._panel_pix = None  # last streamed pixels, for partial rows
        self._next_refresh = 0.0
        self._v1_noted = False

    # -- tile side (SDK callbacks) ------------------------------------

    def on_theme(self, ctx):
        self.pal = Palette(ctx.theme_toml)
        self._painted = None
        self._panel_painted = None

    def draw(self, ctx, buf):
        self._maybe_sample()
        # Tighten the cadence only while the panel needs live paint
        # (the rescan countdown and the refresh ride this tick).
        panel_open = self.panel is not None and self.panel.opened
        self.redraw_interval = PANEL_TICK if panel_open else IDLE_TICK
        key = (self.snap.link, ctx.tile_px, self.pal.appearance, self.pal.tile)
        if key == self._painted:
            return False
        render_tile(buf, ctx.tile_px, self.snap.link, self.pal, ctx.scale)
        self._painted = key
        return True

    def on_input(self, ctx, event):
        if event.kind != INPUT_PRESS:
            return False
        panel = self.panel
        if panel is not None and not panel.closed:
            panel.close()  # second click: toggle it away
            return False
        w, h = panel_request_px(ctx.scale)
        try:
            panel = self.open_panel(w, h)
        except PanelError:
            # A v1 shell: no panels there. Say so once, stay tile-only.
            if not self._v1_noted:
                ctx.log(LOG_WARN, "net: shell speaks protocol 1; the "
                        "detail panel needs 2, staying tile-only")
                self._v1_noted = True
            return False
        panel.paint = self._panel_paint
        panel.on_input = self._panel_input
        panel.on_closed = self._panel_closed
        panel.on_opened = self._panel_opened
        ctx.log(LOG_INFO, f"net: panel requested {w}x{h}")
        return False

    # -- sampling -----------------------------------------------------

    def _maybe_sample(self, force=False):
        now = time.monotonic()
        if not force and now < self._next_refresh:
            return
        if self.backend is None:
            self.backend = netdata.detect_backend()
        snap = netdata.sample(backend=self.backend)
        with self._scan_lock:
            fresh = self._scan_result
            self._scan_result = None
        if fresh is not None and snap.networks == () and fresh:
            snap = netdata.Snapshot(backend=snap.backend, link=snap.link,
                                    networks=fresh, scan_note="")
        self.snap = snap
        panel_open = self.panel is not None and self.panel.opened
        self._next_refresh = now + (
            PANEL_REFRESH if panel_open else IDLE_TICK)

    def _rescan_state(self):
        if self._scan_thread is not None and self._scan_thread.is_alive():
            return "scanning"
        wait = self.gate.remaining()
        if wait > 0.5:
            return f"wait:{int(wait) + 1}"
        return "ready"

    def _start_rescan(self):
        if self._scan_thread is not None and self._scan_thread.is_alive():
            return
        if not self.gate.allow():
            return

        def work():
            result = netdata.rescan()
            with self._scan_lock:
                self._scan_result = result if result is not None else ()
            self._next_refresh = 0.0

        self._scan_thread = threading.Thread(target=work, daemon=True)
        self._scan_thread.start()

    # -- panel side (SDK Panel callbacks) -----------------------------

    def _panel_opened(self, panel):
        self._panel_pix = None
        self._panel_painted = None
        self._maybe_sample(force=True)

    def _render_panel_into(self, panel, buf):
        w, h = panel.width, panel.height
        scale = max(min(w / PANEL_LOGICAL_W, h / PANEL_LOGICAL_H), 0.5)
        self.boxes = render_panel(buf, w, h, self.snap, self.pal, scale,
                                  self.hover, self._rescan_state(),
                                  self.pressed_row)

    def _panel_key(self, panel):
        return (self.snap, self.hover, self.pressed_row,
                self._rescan_state(), panel.width, panel.height,
                self.pal.appearance)

    def _panel_paint(self, panel, buf) -> bool:
        """The SDK's scheduled repaint: render, and let it stream the
        full band run if anything changed since the last paint."""
        self._maybe_sample()
        key = self._panel_key(panel)
        if key == self._panel_painted:
            return False
        self._render_panel_into(panel, buf)
        self._panel_painted = key
        self._panel_pix = bytes(buf)
        return True

    def _panel_partial(self, panel):
        """The hover path: repaint locally, ship only the rows that
        changed (`draw_rows` bands them), and keep the caches straight
        so the next scheduled paint sends nothing redundant."""
        key = self._panel_key(panel)
        if key == self._panel_painted:
            return
        if self._panel_pix is None:
            panel._must_present = True  # no baseline: full paint instead
            return
        w, h = panel.width, panel.height
        buf = bytearray(w * h * 4)
        self._render_panel_into(panel, buf)
        pixels = bytes(buf)
        stride = w * 4
        old = self._panel_pix
        y0 = next((y for y in range(h)
                   if old[y * stride:(y + 1) * stride]
                   != pixels[y * stride:(y + 1) * stride]), None)
        if y0 is not None:
            y1 = next(y for y in range(h - 1, y0 - 2, -1)
                      if old[y * stride:(y + 1) * stride]
                      != pixels[y * stride:(y + 1) * stride]) + 1
            panel.draw_rows(y0, pixels[y0 * stride:y1 * stride])
        self._panel_pix = pixels
        self._panel_painted = key

    def _box_at(self, y):
        for i, (y0, y1, _target) in enumerate(self.boxes):
            if y0 <= y < y1:
                return i
        return None

    def _panel_input(self, panel, ev) -> bool:
        if ev.kind == INPUT_LEAVE:
            self.hover = None
            self.pressed_row = None
        elif ev.kind == INPUT_RELEASE:
            idx = self._box_at(ev.y)
            if (idx is not None and idx == self.pressed_row
                    and self.boxes[idx][2] == ("rescan",)):
                self._start_rescan()
            self.pressed_row = None
            self.hover = idx
        elif ev.kind == INPUT_PRESS:
            self.pressed_row = self._box_at(ev.y)
            self.hover = self.pressed_row
        elif ev.kind == INPUT_MOTION:
            # v2.1: panel-only Motion is the hover signal proper.
            self.hover = self._box_at(ev.y)
        else:
            # Enter (and scroll) keep hover working against a shell
            # that throttles motion hard or predates it.
            self.hover = self._box_at(ev.y)
        # Ship just the changed rows now; nothing left for the
        # scheduled paint to resend.
        self._panel_partial(panel)
        return False

    def _panel_closed(self, panel, reason):
        self.hover = None
        self.pressed_row = None
        self._panel_painted = None
        self._panel_pix = None


# ---------------------------------------------------------------------
# Headless rendering — the design loop and the tests.
# ---------------------------------------------------------------------

def write_png(path, buf, w, h):
    raw = b"".join(b"\x00" + bytes(buf[y * w * 4:(y + 1) * w * 4])
                   for y in range(h))

    def chunk(tag, data):
        body = tag + data
        return struct.pack(">I", len(data)) + body + struct.pack(
            ">I", zlib.crc32(body))
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)))
        f.write(chunk(b"IDAT", zlib.compress(raw)))
        f.write(chunk(b"IEND", b""))


def demo_snapshot(state: str) -> netdata.Snapshot:
    """Plausible worlds for --render and the tests."""
    nets = (
        netdata.Network("Basilisk", 88, "5", "WPA2", in_use=True),
        netdata.Network("Basilisk-guest", 84, "5", "WPA2"),
        netdata.Network("chonknet-6e", 71, "6", "WPA3"),
        netdata.Network("PrinterSetup-8F2A", 58, "2.4", ""),
        netdata.Network("Overlook Hotel", 46, "2.4", "WPA2"),
        netdata.Network("kd7-mesh-node long-name-that-will-not-fit-at-all",
                        33, "5", "WPA2"),
        netdata.Network("moss", 19, "2.4", "WEP"),
    )
    if state.startswith("wifi"):
        signal = int(state.split(":")[1]) if ":" in state else 88
        return netdata.Snapshot(
            backend="networkmanager",
            link=netdata.Link(kind="wifi", ifname="wlan0", ssid="Basilisk",
                              signal=signal, band="5", bitrate_mbps=866,
                              security="WPA2", ip4="10.1.1.71/24"),
            networks=nets)
    if state == "wired":
        return netdata.Snapshot(
            backend="networkmanager",
            link=netdata.Link(kind="wired", ifname="eno1",
                              ssid="Wired connection 1", bitrate_mbps=1000,
                              ip4="10.1.1.55/24"),
            networks=(), scan_note="no Wi-Fi hardware")
    if state == "down":
        return netdata.Snapshot(backend="networkmanager",
                                link=netdata.Link(kind="none"),
                                networks=nets[1:],
                                scan_note="")
    return netdata.Snapshot(backend="none",
                            link=netdata.Link(kind="unavailable"),
                            scan_note="no network manager found")


def render_tile_png(path, size, state, dark=True):
    pal = Palette("", appearance="dark" if dark else "light")
    snap = demo_snapshot(state)
    buf = bytearray(size * size * 4)
    render_tile(buf, size, snap.link, pal, size / 56)
    write_png(path, buf, size, size)


def render_panel_png(path, w, h, state, dark=True, hover=None,
                     rescan="ready", pressed=None):
    pal = Palette("", appearance="dark" if dark else "light")
    snap = demo_snapshot(state)
    buf = bytearray(w * h * 4)
    render_panel(buf, w, h, snap, pal,
                 min(w / PANEL_LOGICAL_W, h / PANEL_LOGICAL_H),
                 hover, rescan, pressed)
    write_png(path, buf, w, h)


def _arg(argv, flag, default=None):
    return argv[argv.index(flag) + 1] if flag in argv else default


def _main(argv):
    if "--render" in argv:
        out = _arg(argv, "--render", "chonk-net.png")
        what = _arg(argv, "--what", "tile")
        state = _arg(argv, "--state", "wifi:88")
        dark = "--light" not in argv
        if what == "tile":
            size = int(_arg(argv, "--size", "112"))
            render_tile_png(out, size, state, dark)
            print(f"wrote {out} (tile {size}x{size}, {state})")
        else:
            wh = _arg(argv, "--panel-size", "700x500")
            w, h = (int(v) for v in wh.split("x"))
            hover = _arg(argv, "--hover")
            pressed = _arg(argv, "--pressed-row")
            render_panel_png(out, w, h, state, dark,
                             int(hover) if hover is not None else None,
                             _arg(argv, "--rescan", "ready"),
                             int(pressed) if pressed is not None else None)
            print(f"wrote {out} (panel {w}x{h}, {state})")
        return 0
    try:
        NetApp().run()
    except Exception as e:  # noqa: BLE001 — stderr is /dev/null when docked
        sys.stderr.write(f"chonk-net: {e}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv))
