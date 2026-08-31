#!/usr/bin/env python3
"""A digital clock tile in pure stdlib Python — the chonkdock hello-world.

Register it with a .dockapp file whose exec points here (or install it
with `scripts/chonk-get install bindings/python/chonkdock/examples`),
and the dock launches it, themes it, and restarts it if it dies.
Clicking the tile logs a line into the session journal, to show input
and Ctx.log working.
"""

import sys
import time

sys.path.insert(0, __file__.rsplit("/", 3)[0])  # find chonkdock in-tree
from chonkdock import Dockapp, INPUT_PRESS, LOG_INFO

# 3x5 bitmaps for '0'-'9' and ':' — three bits per row, top first.
GLYPHS = {
    "0": (7, 5, 5, 5, 7), "1": (2, 6, 2, 2, 7), "2": (7, 1, 7, 4, 7),
    "3": (7, 1, 7, 1, 7), "4": (5, 5, 7, 1, 1), "5": (7, 4, 7, 1, 7),
    "6": (7, 4, 7, 5, 7), "7": (7, 1, 2, 2, 2), "8": (7, 5, 7, 5, 7),
    "9": (7, 5, 7, 1, 7), ":": (0, 2, 0, 2, 0),
}
BG, FG = (24, 26, 22, 255), (140, 235, 120, 255)  # LED-screen-ish


class Clock(Dockapp):
    shown = None

    def draw(self, ctx, buf):
        hhmm = time.strftime("%H:%M")
        if hhmm == self.shown:
            return False  # nothing changed; send nothing
        px = self._cell(ctx)  # pixels per font cell, from the tile size
        text_w = len(hhmm) * 4 * px - px
        x0 = (ctx.tile_px - text_w) // 2
        y0 = (ctx.height - 5 * px) // 2
        self._fill(buf, BG)
        for i, ch in enumerate(hhmm):
            self._glyph(ctx, buf, GLYPHS[ch], x0 + i * 4 * px, y0, px)
        self.shown = hhmm
        return True

    def on_theme(self, ctx):
        self.shown = None  # force a redraw at the new geometry

    def on_input(self, ctx, event):
        if event.kind == INPUT_PRESS:
            ctx.log(LOG_INFO, f"clock clicked at {event.x},{event.y}")
        return False

    @staticmethod
    def _cell(ctx):
        return max(1, ctx.tile_px // 24)

    @staticmethod
    def _fill(buf, rgba):
        buf[:] = bytes(rgba) * (len(buf) // 4)

    @staticmethod
    def _glyph(ctx, buf, rows, x0, y0, px):
        for row, bits in enumerate(rows):
            for col in range(3):
                if not bits & (4 >> col):
                    continue
                for dy in range(px):
                    y = y0 + row * px + dy
                    base = (y * ctx.tile_px + x0 + col * px) * 4
                    buf[base:base + 4 * px] = bytes(FG) * px


if __name__ == "__main__":
    try:
        Clock("py-dockclock", redraw_interval=0.25).run()
    except Exception as e:  # a dockapp's stderr is /dev/null when docked
        sys.stderr.write(f"py-dockclock: {e}\n")
        sys.exit(1)
