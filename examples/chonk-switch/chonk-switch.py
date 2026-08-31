#!/usr/bin/env python3
"""The appearance switch: light and dark mode as a piece of hardware.

A single-tile dockapp — written against the Python SDK
(`bindings/python/chonkdock`, stdlib only) — that shows which
appearance the desktop is in and flips it on a click. The design brief
was a physical toggle, not a checkbox: a recessed slot sunk into the
themed tile face, a machined knob riding in it, and the exposed track
lit paper-bright or slate-dark so the state reads from across the
room. Clicking presses the knob in; the throw animates over a handful
of frames; the knob carries an engraved sun or moon so the position
never needs a legend.

# The appearance contract

The switch owns no policy; it speaks the shell's appearance files:

- **Current mode** is read from `$XDG_STATE_HOME/chonkstep/appearance`
  (default `~/.local/state/...`): the file contains `light` or `dark`,
  whitespace-trimmed. Absent or unreadable means `light` — the
  desktop's default, and the safe thing to show rather than a third
  "unknown" face nobody can act on.
- **Switching** is one atomic write: `toggle` into
  `appearance-request` in the same directory, via temp-file + rename,
  so the shell can never read half a request. The shell consumes the
  file, deletes it, and updates the mode file.
- The switch then *watches the mode file* rather than trusting its own
  click: a mode changed by anyone — a keybinding, a config reload,
  another instrument — moves this lever too. Its own click gets an
  optimistic throw for responsiveness, with a deadline: if the shell
  has not confirmed within a couple of seconds, the lever settles back
  to what the file actually says. The file is the truth; the animation
  is a prediction.

Polling happens inside `draw`, which is the SDK's natural tick: idle,
that is one small file read a few times a second (built-ins tick at
1 Hz; a mode switch deserves to feel immediate, so this leans a little
faster); mid-throw the tick tightens to ~30 Hz for the few frames the
knob is moving, and `draw` returning False keeps every unchanged tick
off the wire. While the tile is hidden the SDK stops calling `draw`,
so the polling stops with it — a hidden switch samples nothing.

Run with `--render out.png` to rasterize the faces headless (no dock
needed) — that is how the design was iterated, and how the tests look
at pixels without a compositor.
"""

from __future__ import annotations

import math
import os
import struct
import sys
import tempfile
import time
import zlib

# chonkdock is resolved from, in order: an installed copy on
# sys.path, a vendored `chonkdock/` next to this script (what
# build.sh arranges — the SDK README calls vendoring a supported way
# to ship), or the chonkstep checkout this file lives in.
_HERE = os.path.dirname(os.path.abspath(__file__))
for _candidate in (_HERE, os.path.join(_HERE, "..", "..", "bindings", "python")):
    if os.path.isdir(os.path.join(_candidate, "chonkdock")):
        sys.path.insert(0, os.path.abspath(_candidate))
        break
from chonkdock import (  # noqa: E402  (the path dance above is the import)
    Dockapp,
    INPUT_PRESS,
    INPUT_RELEASE,
    LOG_INFO,
    LOG_WARN,
)

LIGHT, DARK = "light", "dark"

#: How long a clicked-but-unconfirmed throw is believed before the
#: lever settles back to what the mode file says. The shell consumes a
#: request within a tick or two; a couple of seconds of optimism covers
#: that with slack, without letting a dead shell hold the lever wrong.
CONFIRM_GRACE = 2.5

#: Ticks. Idle is a cheap mode-file poll; the throw runs at ~30 Hz for
#: the few frames the knob is actually moving (the protocol's frame
#: limiter caps at 30, so asking for more would only be coalesced).
IDLE_TICK = 0.2
THROW_TICK = 1.0 / 30.0

#: Per-tick easing toward the target position; with THROW_TICK this
#: lands the throw in roughly a quarter second — quick enough to feel
#: mechanical, slow enough to be seen travelling.
THROW_EASE = 0.42
THROW_SNAP = 0.02


# ---------------------------------------------------------------------
# The appearance files
# ---------------------------------------------------------------------

def state_dir() -> str:
    """`$XDG_STATE_HOME/chonkstep`, defaulting like the spec does."""
    base = os.environ.get("XDG_STATE_HOME", "").strip()
    if not base:
        base = os.path.join(os.path.expanduser("~"), ".local", "state")
    return os.path.join(base, "chonkstep")


def read_mode(directory: str | None = None) -> str:
    """The current appearance: `light` or `dark`, trimmed; anything
    absent, unreadable, or unrecognized is `light` — a default face
    beats an error face for a file another process owns."""
    path = os.path.join(directory or state_dir(), "appearance")
    try:
        with open(path, "r", encoding="utf-8", errors="replace") as f:
            text = f.read(256).strip()
    except OSError:
        return LIGHT
    return DARK if text == DARK else LIGHT


def request(value: str, directory: str | None = None) -> None:
    """Writes one appearance request (`light`, `dark`, or `toggle`)
    atomically: temp file in the same directory, then rename, so the
    shell's reader can only ever see a whole request. A second request
    before the first is consumed replaces it — the rename is the whole
    transaction."""
    if value not in (LIGHT, DARK, "toggle"):
        raise ValueError(f"not an appearance request: {value!r}")
    directory = directory or state_dir()
    os.makedirs(directory, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix=".appearance-request.", dir=directory)
    try:
        with os.fdopen(fd, "wb") as f:
            f.write(value.encode("ascii"))
        os.replace(tmp, os.path.join(directory, "appearance-request"))
    except OSError:
        # Best-effort cleanup; a request that cannot be written is a
        # log line upstream, not a crash — the switch still shows the
        # truth it can read.
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


# ---------------------------------------------------------------------
# Pixels. Premultiplied RGBA8 — everything here is opaque, so
# premultiplied and straight are the same bytes. The recipes are the
# desktop's own (crates/wm-theme/src/paint.rs): relative +80/-40 relief
# deltas over a diagonal gradient face, hard black outer lines. Numbers
# are the theme's, not this file's.
# ---------------------------------------------------------------------

def fill_rect(buf, W, x, y, w, h, rgb):
    r, g, b = rgb
    row = bytes((r, g, b, 255)) * w
    H = len(buf) // (W * 4)
    for yy in range(max(y, 0), min(y + h, H)):
        base = (yy * W + x) * 4
        buf[base:base + w * 4] = row


def op_rect(buf, W, x, y, w, h, delta):
    """Clamped add/subtract over a region — the relative relief
    primitive, so bevels take their tone from whatever face they sit
    on instead of assuming one."""
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


def _cl(v):
    return 0 if v < 0 else (255 if v > 255 else v)


def diag_gradient(buf, W, x, y, w, h, c0, c1):
    """Top-left to bottom-right — the signature tile face."""
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
    """The double raised relief: +80 light along top/left, a -40 shade
    line inside a hard black outer line along bottom/right."""
    op_rect(buf, W, x, y, w, t, 80)
    op_rect(buf, W, x, y + t, t, h - t, 80)
    op_rect(buf, W, x, y + h - 2 * t, w - 2 * t, t, -40)
    fill_rect(buf, W, x, y + h - t, w, t, (0, 0, 0))
    op_rect(buf, W, x + w - 2 * t, y, t, h - 2 * t, -40)
    fill_rect(buf, W, x + w - t, y, t, h - t, (0, 0, 0))


def sunken_bevel(buf, W, x, y, w, h, t):
    """The recessed counterpart: shade on top/left, light on the
    bottom/right lip — a well the light falls into."""
    op_rect(buf, W, x, y, w, t, -40)
    op_rect(buf, W, x, y + t, t, h - t, -40)
    op_rect(buf, W, x, y + h - t, w, t, 80)
    op_rect(buf, W, x + w - t, y, t, h - 2 * t, 80)


# ---------------------------------------------------------------------
# Palette. theme_toml is the correctness path: the serialized theme's
# [tile.fill] is parsed (stdlib tomllib) so the switch sits on the
# session's actual tile face; failing that, the flagship
# nextstep-classic face — wrong colors beat a blank tile.
# ---------------------------------------------------------------------

CLASSIC_TILE = ((0xA6, 0xA6, 0xB6), (0x51, 0x55, 0x61))

#: The exposed track: the loudest state signal on the tile. Light mode
#: shows warm paper; dark mode a deep blue-black slate. Both are
#: deliberately outside the tile gradient's family so the slot reads
#: as *lit*, not merely shaded.
TRACK_LIGHT = ((0xEE, 0xEA, 0xDF), (0xC9, 0xC5, 0xB9))
TRACK_DARK = ((0x0E, 0x10, 0x18), (0x26, 0x2A, 0x36))


def tile_colors(theme_toml: str):
    """`(from, to)` of the theme's tile face; a Solid fill is a
    degenerate gradient of itself."""
    try:
        import tomllib
        fill = tomllib.loads(theme_toml)["tile"]["fill"]
        if "Gradient" in fill:
            g = fill["Gradient"]
            return (_rgb(g["from"]), _rgb(g["to"]))
        if "Solid" in fill:
            c = _rgb(fill["Solid"])
            return (c, c)
    except Exception:  # noqa: BLE001 — any malformed palette falls back
        pass
    return CLASSIC_TILE


def _rgb(c):
    return (int(c["r"]) & 0xFF, int(c["g"]) & 0xFF, int(c["b"]) & 0xFF)


def _lerp(c0, c1, f):
    return tuple(int(a + (b - a) * f) for a, b in zip(c0, c1))


# ---------------------------------------------------------------------
# The face
# ---------------------------------------------------------------------

def render(buf, size, pos, pressed, tile_from, tile_to, scale=1.0):
    """Paints the whole switch at `size`x`size`.

    `pos` is the lever's travel, 0.0 (light, knob left) to 1.0 (dark,
    knob right); `pressed` sinks the knob while the button is held.
    Everything is proportional to `size`, so scale changes are just a
    bigger tile, and the relief thickness follows the session scale the
    way all chrome does.
    """
    t = max(1, round(scale))

    # The themed tile face under everything, same recipe as every
    # square surface on this desktop.
    diag_gradient(buf, size, 0, 0, size, size, tile_from, tile_to)
    raised2_bevel(buf, size, 0, 0, size, size, t)

    # The slot: a horizontal well sunk into the face. Shade the region
    # down, then the sunken lip — set in, not stickered on.
    wx = round(size * 0.125)
    wh = round(size * 0.46)
    wy = (size - wh) // 2
    ww = size - 2 * wx
    op_rect(buf, size, wx, wy, ww, wh, -24)
    sunken_bevel(buf, size, wx, wy, ww, wh, t)

    # Track interior: the lit part of the machine. Its ramp crossfades
    # with the throw, so the slot dims as the knob travels toward dark.
    ix, iy = wx + t, wy + t
    iw, ih = ww - 2 * t, wh - 2 * t
    top = _lerp(TRACK_LIGHT[0], TRACK_DARK[0], pos)
    bot = _lerp(TRACK_LIGHT[1], TRACK_DARK[1], pos)
    vert_gradient(buf, size, ix, iy, iw, ih, top, bot)
    # A one-line inner shadow under the top lip: recessed things carry
    # their own shade.
    op_rect(buf, size, ix, iy, iw, t, -22)

    # Engraved travel pips at each end of the slot, so the knob has
    # visible somewhere-to-go even mid-throw.
    pip = max(t, size // 56)
    pipy = iy + ih // 2 - pip // 2
    for px_ in (ix + 2 * pip, ix + iw - 3 * pip):
        op_rect(buf, size, px_, pipy, pip, pip, -60)
        op_rect(buf, size, px_ + pip, pipy + pip, pip, pip, 60)

    # The knob: a machined block riding the slot, faced with a lifted
    # cut of the tile's own gradient. Raised at rest; pressed, it
    # sinks — the relief inverts rather than vanishing, the desktop's
    # own pressed-button rule.
    kw = round(iw * 0.46)
    kh = ih
    kx = ix + round(pos * (iw - kw))
    face0 = _lerp(_lighten(tile_from, 28), _lighten(tile_from, -6), pos)
    face1 = _lerp(_lighten(tile_to, 46), _lighten(tile_to, 10), pos)
    vert_gradient(buf, size, kx, iy, kw, kh, face0, face1)
    if pressed:
        op_rect(buf, size, kx, iy, kw, kh, -20)
        sunken_bevel(buf, size, kx, iy, kw, kh, t)
    else:
        raised2_bevel(buf, size, kx, iy, kw, kh, t)

    # The engraved glyph: sun on the light throw, moon on the dark
    # one, switching at the midpoint so the knob always names the side
    # it is heading for. Engraving is a light copy a pixel down-right
    # under a dark cut — carved, not printed.
    cx, cy = kx + kw // 2, iy + kh // 2
    r = max(4, round(kh * 0.32))
    glyph = moon_points if pos >= 0.5 else sun_points
    pts = glyph(cx, cy, r)
    for gx, gy in pts:
        op_rect(buf, size, gx + 1, gy + 1, 1, 1, 70)
    for gx, gy in pts:
        op_rect(buf, size, gx, gy, 1, 1, -110)


def _lighten(c, d):
    return tuple(_cl(v + d) for v in c)


def sun_points(cx, cy, r):
    """A disc with eight rays — pixel positions, not colors, so the
    engraver above can cut it twice. Rays are distance-to-segment
    tested with a real half-width, so they stay solid strokes at any
    tile size instead of decaying into speckle."""
    pts = []
    rr = max(2, round(r * 0.50))
    half = max(0.6, r * 0.11)  # ray stroke half-width
    inner, outer = r * 0.68, float(r)
    rays = []
    for k in range(8):
        a = k * math.pi / 4
        rays.append((math.cos(a), math.sin(a)))
    for yy in range(-r - 1, r + 2):
        for xx in range(-r - 1, r + 2):
            if xx * xx + yy * yy <= rr * rr:
                pts.append((cx + xx, cy + yy))
                continue
            for dx, dy in rays:
                along = xx * dx + yy * dy
                if inner <= along <= outer and abs(xx * dy - yy * dx) <= half:
                    pts.append((cx + xx, cy + yy))
                    break
    return pts


def moon_points(cx, cy, r):
    """A crescent: the disc minus a second disc pulled up-right."""
    pts = []
    ox, oy = round(r * 0.45), -round(r * 0.30)
    bite = round(r * 0.85)
    for yy in range(-r, r + 1):
        for xx in range(-r, r + 1):
            if xx * xx + yy * yy > r * r:
                continue
            bx, by = xx - ox, yy - oy
            if bx * bx + by * by <= bite * bite:
                continue
            pts.append((cx + xx, cy + yy))
    return pts


# ---------------------------------------------------------------------
# The dockapp
# ---------------------------------------------------------------------

class Switch(Dockapp):
    """One tile, one lever, one job."""

    def __init__(self, dockapp_id="chonk-switch", directory=None):
        super().__init__(dockapp_id, tile_units=1, redraw_interval=IDLE_TICK)
        self.directory = directory  # None = the real state dir
        self.mode = read_mode(self.directory)
        self.pos = 1.0 if self.mode == DARK else 0.0
        self.pending = None  # (target_mode, deadline) after our own click
        self.pressed = False
        self.tile = CLASSIC_TILE
        self._painted = None

    # -- SDK callbacks -------------------------------------------------

    def on_theme(self, ctx):
        self.tile = tile_colors(ctx.theme_toml)
        self._painted = None

    def draw(self, ctx, buf):
        self._sample()
        target = 1.0 if self._target_mode() == DARK else 0.0
        if abs(target - self.pos) <= THROW_SNAP:
            self.pos = target
        else:
            self.pos += (target - self.pos) * THROW_EASE
        # Tighten the tick only while the knob is moving; idle is a
        # file poll and (usually) no frame at all.
        self.redraw_interval = IDLE_TICK if self.pos == target else THROW_TICK

        signature = (round(self.pos, 3), self.pressed, ctx.tile_px, self.tile)
        if signature == self._painted:
            return False
        render(buf, ctx.tile_px, self.pos, self.pressed,
               self.tile[0], self.tile[1], ctx.scale)
        self._painted = signature
        return True

    def on_input(self, ctx, event):
        if event.kind == INPUT_PRESS:
            self.pressed = True
            self._flip(ctx)
            return True
        if event.kind == INPUT_RELEASE:
            self.pressed = False
            return True
        return False

    # -- the mechanism -------------------------------------------------

    def _sample(self):
        """One poll of the mode file — the truth the lever follows."""
        mode = read_mode(self.directory)
        if mode != self.mode:
            self.mode = mode
            self.pending = None  # confirmed (or overridden) by reality
        elif self.pending and time.monotonic() > self.pending[1]:
            self.pending = None  # nobody answered; settle back

    def _target_mode(self):
        return self.pending[0] if self.pending else self.mode

    def _flip(self, ctx):
        """One click, one atomic `toggle` — and an optimistic throw
        that the mode file gets to veto."""
        wanted = LIGHT if self._target_mode() == DARK else DARK
        try:
            request("toggle", self.directory)
        except OSError as e:
            ctx.log(LOG_WARN, f"switch: could not write request: {e}")
            return
        self.pending = (wanted, time.monotonic() + CONFIRM_GRACE)
        ctx.log(LOG_INFO, f"switch: requested toggle -> {wanted}")


# ---------------------------------------------------------------------
# Headless rendering — the design loop and the tests use this; the
# dock never does.
# ---------------------------------------------------------------------

def write_png(path, buf, w, h):
    """A minimal stdlib PNG writer (RGBA8): enough to look at a tile."""
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


def render_to_png(path, size, pos, pressed=False,
                  tile=CLASSIC_TILE, scale=None):
    buf = bytearray(size * size * 4)
    render(buf, size, pos, pressed, tile[0], tile[1],
           scale if scale is not None else size / 56)
    write_png(path, buf, size, size)


def _main(argv):
    if len(argv) >= 2 and argv[1] == "--render":
        out = argv[2] if len(argv) > 2 else "chonk-switch.png"
        size = int(argv[argv.index("--size") + 1]) if "--size" in argv else 112
        pos = float(argv[argv.index("--pos") + 1]) if "--pos" in argv else 0.0
        render_to_png(out, size, pos, pressed="--pressed" in argv)
        print(f"wrote {out} ({size}x{size}, pos={pos})")
        return 0
    try:
        Switch().run()
    except Exception as e:  # noqa: BLE001 — stderr is /dev/null when docked
        sys.stderr.write(f"chonk-switch: {e}\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(_main(sys.argv))
