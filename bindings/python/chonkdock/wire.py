"""The chonkstep dockapp wire codec, protocol versions 1 and 2.

A line-for-line transcription of ``crates/chonk-dock-proto/src/wire.rs``
into stdlib Python. Everything here is pure: bytes in, messages out, no
I/O. The transport is ``SOCK_SEQPACKET``, so there is no framing to
implement — one ``send()`` is one message, and the byte layouts below
start with the universal 4-byte header (``kind`` + three zero bytes).

Decoding is strict on purpose, mirroring the reference decoder: unknown
kinds, non-zero reserved bytes, trailing bytes, out-of-range enums,
over-cap strings and unusable floats are all rejected with
``DecodeError`` rather than clamped or ignored. The shell is the
trusted end of a dockapp's socket, but "trusted" is a statement about
intent, not about bugs — a client that silently mis-parses a message
draws garbage with no error anywhere to explain it.

See ``docs/dockapp-protocol.md`` for the full contract in prose.
"""

from __future__ import annotations

import struct
from dataclasses import dataclass

# The Hello version this SDK announces. 2 says "I know the
# formerly-reserved proto u16 in the Welcome body" — the shell
# advertises its own version there only to a client that announced
# >= 2, and keeps the byte-exact v1 wire (zeros there) for a client
# that said 1. The shell accepts 1..=its own version.
PROTOCOL_VERSION = 2
TOKEN_BYTES = 16
MAX_MESSAGE_BYTES = 256 * 1024
MAX_FRAME_BYTES = MAX_MESSAGE_BYTES - 64
MAX_TILE_PX = 256
MAX_SCALE = 8.0
MAX_TILE_UNITS = 4
MAX_ID_BYTES = 64
MAX_LOG_BYTES = 256
MAX_THEME_ID_BYTES = 64
MAX_THEME_TOML_BYTES = 128 * 1024
MAX_PANEL_PX = 1024
MAX_PANEL_FRAME_BYTES = 4 * 1024 * 1024

# Message kinds. Client->shell in the low space, shell->client with the
# high bit set, so a reflected message is an UnknownKind and never a
# reinterpretation.
KIND_HELLO = 0x01
KIND_FRAME = 0x02
KIND_PONG = 0x03
KIND_LOG = 0x04
KIND_OPEN_PANEL = 0x05
KIND_PANEL_FRAME = 0x06
KIND_CLOSE_PANEL = 0x07
KIND_WELCOME = 0x81
KIND_THEME_CHANGED = 0x82
KIND_INPUT = 0x83
KIND_VISIBILITY = 0x84
KIND_PING = 0x85
KIND_GOODBYE = 0x86
KIND_PANEL_OPENED = 0x87
KIND_PANEL_CLOSED = 0x88
KIND_PANEL_INPUT = 0x89

# Input mask bits for Hello's `wants` field.
WANT_PRESS = 1 << 0
WANT_RELEASE = 1 << 1
WANT_SCROLL = 1 << 2
WANT_CROSSING = 1 << 3
WANT_ALL = WANT_PRESS | WANT_RELEASE | WANT_SCROLL | WANT_CROSSING

# InputEvent.kind
INPUT_PRESS = 1
INPUT_RELEASE = 2
INPUT_SCROLL = 3
INPUT_ENTER = 4
INPUT_LEAVE = 5
#: Hover tracking, PanelInput only: coords in panel device pixels,
#: button 0. Never sent (and never accepted) as a tile Input.
INPUT_MOTION = 6

# InputEvent.button (0 = none)
BUTTON_LEFT = 1
BUTTON_MIDDLE = 2  # never actually sent by the shell
BUTTON_RIGHT = 3  # never actually sent by the shell

# Log levels
LOG_ERROR = 1
LOG_WARN = 2
LOG_INFO = 3
LOG_DEBUG = 4

# Goodbye reasons
GOODBYE_SHUTDOWN = 1
GOODBYE_PROTOCOL_ERROR = 2
GOODBYE_UNAUTHORIZED = 3
GOODBYE_REPLACED = 4
GOODBYE_TILE_TOO_LARGE = 5
GOODBYE_OVERFLOW = 6
GOODBYE_REMOVED = 7

GOODBYE_NAMES = {
    GOODBYE_SHUTDOWN: "Shutdown",
    GOODBYE_PROTOCOL_ERROR: "ProtocolError",
    GOODBYE_UNAUTHORIZED: "Unauthorized",
    GOODBYE_REPLACED: "Replaced",
    GOODBYE_TILE_TOO_LARGE: "TileTooLarge",
    GOODBYE_OVERFLOW: "Overflow",
    GOODBYE_REMOVED: "Removed",
}

# PanelClosed reasons
PANEL_CLOSED_CLIENT = 0
PANEL_CLOSED_DISMISSED = 1
PANEL_CLOSED_SHUTDOWN = 2
PANEL_CLOSED_REFUSED = 3

PANEL_CLOSED_NAMES = {
    PANEL_CLOSED_CLIENT: "closed",
    PANEL_CLOSED_DISMISSED: "dismissed",
    PANEL_CLOSED_SHUTDOWN: "shutdown",
    PANEL_CLOSED_REFUSED: "refused",
}

_ID_CHARS = frozenset(
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_.:"
)


class DecodeError(ValueError):
    """The peer's bytes could not be read as a protocol message."""


class EncodeError(ValueError):
    """Local code tried to build a message that cannot legally exist."""


def is_valid_id(dockapp_id: str) -> bool:
    """The wire's id rule: 1..=64 bytes of ``[A-Za-z0-9._:-]``."""
    return (
        0 < len(dockapp_id) <= MAX_ID_BYTES
        and all(c in _ID_CHARS for c in dockapp_id)
    )


def frame_fits(tile_px: int, tile_units: int) -> bool:
    """Whether a tile of this geometry can cross the v1 inline transport."""
    if not (0 < tile_px <= MAX_TILE_PX and 0 < tile_units <= MAX_TILE_UNITS):
        return False
    return tile_px * tile_px * tile_units * 4 <= MAX_FRAME_BYTES


def panel_fits(width: int, height: int) -> bool:
    """Whether a panel of this geometry is within the panel caps: at
    most ``MAX_PANEL_PX`` per edge and ``width * height * 4 <=
    MAX_PANEL_FRAME_BYTES`` (the shell's total-buffer allocation cap).
    Transport is not the constraint it once was: a ``PanelFrame`` is a
    *band*, so any grantable panel can be streamed band by band."""
    if not (0 < width <= MAX_PANEL_PX and 0 < height <= MAX_PANEL_PX):
        return False
    return width * height * 4 <= MAX_PANEL_FRAME_BYTES


def panel_band_rows(width: int) -> int:
    """The tallest band a PanelFrame datagram can carry at this width:
    ``MAX_FRAME_BYTES // (width * 4)`` rows, capped at the panel edge
    bound. The SDK slices full repaints with this."""
    return max(1, min(MAX_FRAME_BYTES // (width * 4), MAX_PANEL_PX))


@dataclass(frozen=True)
class ThemeState:
    """The geometry and palette a dockapp draws with.

    ``theme_id`` is the fast path (look the palette up if you ship
    them); ``theme_toml`` is the correctness path (a serialized theme
    table, possibly empty). A client that can use neither falls back to
    its own default colors — wrong colors, never a blank tile.
    """

    tile_px: int
    scale: float
    theme_id: str
    theme_toml: str
    #: The shell's protocol version, advertised in Welcome (and
    #: ThemeChanged). Shells that predate the field zero it, and zero
    #: decodes as 1: a protocol-1 shell, which has tiles but no
    #: instrument panels. Panels need ``proto >= 2``.
    proto: int = 1

    def height_for(self, tile_units: int) -> int:
        """The frame height for a dockapp of `tile_units` stacked tiles."""
        return self.tile_px * tile_units


@dataclass(frozen=True)
class InputEvent:
    """One pointer event, in coordinates local to this dockapp's tile."""

    kind: int  # INPUT_*
    button: int  # BUTTON_* or 0
    x: int
    y: int
    delta: int  # scroll notches; 0 for everything but INPUT_SCROLL


def _header(kind: int) -> bytes:
    return struct.pack("<B3x", kind)


def _check_header(buf: bytes) -> int:
    if len(buf) == 0:
        raise DecodeError("empty message")
    if len(buf) > MAX_MESSAGE_BYTES:
        raise DecodeError(f"message of {len(buf)} bytes exceeds the cap")
    if len(buf) < 4:
        raise DecodeError("message ended inside the header")
    if buf[1:4] != b"\x00\x00\x00":
        raise DecodeError("reserved header bytes were not zero")
    return buf[0]


def _exact(buf: bytes, want: int, what: str) -> None:
    if len(buf) < want:
        raise DecodeError(f"message ended inside {what}")
    if len(buf) > want:
        raise DecodeError(f"{len(buf) - want} trailing bytes after {what}")


# ---------------------------------------------------------------------
# Client -> shell encoders
# ---------------------------------------------------------------------

def encode_hello(dockapp_id: str, tile_units: int, token: bytes,
                 wants: int = WANT_ALL) -> bytes:
    if not is_valid_id(dockapp_id):
        raise EncodeError(f"invalid dockapp id {dockapp_id!r}")
    if len(token) != TOKEN_BYTES:
        raise EncodeError(f"token must be {TOKEN_BYTES} bytes")
    if wants & ~WANT_ALL:
        raise EncodeError(f"undefined wants bits {wants:#x}")
    ident = dockapp_id.encode("ascii")
    return (
        _header(KIND_HELLO)
        + struct.pack("<IBBBx", PROTOCOL_VERSION, tile_units, wants, len(ident))
        + token
        + ident
    )


def encode_frame(generation: int, width: int, height: int,
                 pixels: bytes) -> bytes:
    """``pixels`` is premultiplied RGBA8, top row first, no row padding."""
    if not (0 < width <= MAX_TILE_PX
            and 0 < height <= MAX_TILE_PX * MAX_TILE_UNITS):
        raise EncodeError(f"frame geometry {width}x{height} is out of range")
    expected = width * height * 4
    if len(pixels) != expected:
        raise EncodeError(
            f"frame needs {expected} pixel bytes but was given {len(pixels)}")
    if expected > MAX_FRAME_BYTES:
        raise EncodeError(f"frame of {expected} bytes exceeds the cap")
    return (
        _header(KIND_FRAME)
        + struct.pack("<III", generation & 0xFFFFFFFF, width, height)
        + pixels
    )


def encode_pong(seq: int) -> bytes:
    return _header(KIND_PONG) + struct.pack("<I", seq & 0xFFFFFFFF)


def encode_log(level: int, text: str) -> bytes:
    """Sanitized and truncated exactly as the reference encoder does:
    control characters (plus the Unicode line/paragraph separators, bidi
    controls and zero-width characters) are dropped, and the result is
    clipped to 256 bytes on a character boundary."""
    if level not in (LOG_ERROR, LOG_WARN, LOG_INFO, LOG_DEBUG):
        raise EncodeError(f"undefined log level {level}")
    out = []
    size = 0
    for c in text:
        cp = ord(c)
        dangerous = (
            cp < 0x20 or cp == 0x7F or 0x80 <= cp <= 0x9F
            or cp in (0x2028, 0x2029, 0x200B, 0x200C, 0x200D, 0xFEFF)
            or 0x202A <= cp <= 0x202E or 0x2066 <= cp <= 0x2069
        )
        if dangerous:
            continue
        b = c.encode("utf-8")
        if size + len(b) > MAX_LOG_BYTES:
            break
        out.append(c)
        size += len(b)
    encoded = "".join(out).encode("utf-8")
    return (
        _header(KIND_LOG)
        + struct.pack("<BxH", level, len(encoded))
        + encoded
    )


def encode_open_panel(width: int, height: int) -> bytes:
    """A request for an instrument panel of `width` x `height` device
    pixels. Answered by PanelOpened (a grant, possibly clamped) or by
    PanelClosed reason=3 (refused). Re-sending while a panel is open
    renegotiates its size."""
    if not panel_fits(width, height):
        raise EncodeError(f"panel geometry {width}x{height} is out of range")
    return _header(KIND_OPEN_PANEL) + struct.pack("<II", width, height)


def encode_panel_frame(generation: int, y: int, band_height: int,
                       width: int, pixels: bytes) -> bytes:
    """One panel *band*: rows ``y .. y + band_height`` of the panel,
    premultiplied RGBA8, top row first, no row padding.

    ``width`` must equal the granted width and ``y + band_height`` must
    stay within the granted height, with the tile Frame's strictness —
    the client layer enforces the grant half; this encoder enforces the
    protocol bounds: each edge within ``MAX_PANEL_PX``, and the band's
    ``width * band_height * 4 <= MAX_FRAME_BYTES``, which is what makes
    any grantable panel streamable one datagram at a time. A full
    repaint is a top-to-bottom band sequence sharing one
    ``generation``; generation carries the tile Frame's
    drop-attribution semantics."""
    if not (0 < width <= MAX_PANEL_PX):
        raise EncodeError(f"panel band width {width} is out of range")
    if not (0 < band_height <= MAX_PANEL_PX):
        raise EncodeError(f"panel band height {band_height} is out of range")
    if y < 0 or y + band_height > MAX_PANEL_PX:
        raise EncodeError(
            f"panel band rows {y}..{y + band_height} are out of range")
    expected = width * band_height * 4
    if expected > MAX_FRAME_BYTES:
        raise EncodeError(
            f"panel band of {expected} bytes exceeds MAX_FRAME_BYTES")
    if len(pixels) != expected:
        raise EncodeError(
            f"panel band needs {expected} pixel bytes but was given "
            f"{len(pixels)}")
    return (
        _header(KIND_PANEL_FRAME)
        + struct.pack("<IIII", generation & 0xFFFFFFFF, y, band_height,
                      width)
        + pixels
    )


def encode_close_panel() -> bytes:
    """Takes the panel down; the shell confirms with PanelClosed
    reason=0."""
    return _header(KIND_CLOSE_PANEL)


# ---------------------------------------------------------------------
# Shell -> client decoder
# ---------------------------------------------------------------------

def _decode_theme_state(body: bytes, kind_name: str) -> ThemeState:
    if len(body) < 16:
        raise DecodeError(f"{kind_name} ended inside its fixed fields")
    tile_px, scale_bits, id_len, proto, toml_len = struct.unpack_from(
        "<IIHHI", body, 0)
    # The u16 that was reserved (and zero) in protocol 1 now carries
    # the shell's protocol version: this is how a shell advertises
    # panel support in Welcome. Zero is what protocol-1 shells always
    # sent there, so zero decodes as 1.
    if proto == 0:
        proto = 1
    scale = struct.unpack("<f", struct.pack("<I", scale_bits))[0]
    # The BadFloat rule: a scale a tile cannot be drawn at is rejected
    # here, in the codec, so the "resend when changed" loops above can
    # rely on a decoded state being equal to itself.
    if not (scale == scale and abs(scale) != float("inf")
            and 0.0 < scale <= MAX_SCALE):
        raise DecodeError(f"unusable scale (bits {scale_bits:#010x})")
    if id_len > MAX_THEME_ID_BYTES:
        raise DecodeError(f"theme_id of {id_len} bytes is over its cap")
    if toml_len > MAX_THEME_TOML_BYTES:
        raise DecodeError(f"theme_toml of {toml_len} bytes is over its cap")
    _exact(body, 16 + id_len + toml_len, kind_name)
    try:
        theme_id = body[16:16 + id_len].decode("utf-8")
        theme_toml = body[16 + id_len:16 + id_len + toml_len].decode("utf-8")
    except UnicodeDecodeError as e:
        raise DecodeError(f"{kind_name} carries invalid UTF-8") from e
    return ThemeState(tile_px=tile_px, scale=scale, theme_id=theme_id,
                      theme_toml=theme_toml, proto=proto)


def _decode_input(body: bytes, kind_name: str,
                  allow_motion: bool = False) -> InputEvent:
    _exact(body, 16, kind_name)
    ev_kind, button, reserved, x, y, delta = struct.unpack("<BBHiii", body)
    top = INPUT_MOTION if allow_motion else INPUT_LEAVE
    if not INPUT_PRESS <= ev_kind <= top:
        raise DecodeError(f"undefined input kind {ev_kind} in {kind_name}")
    if button > BUTTON_RIGHT:
        raise DecodeError(f"undefined button {button}")
    if reserved != 0:
        raise DecodeError(f"{kind_name} reserved field was not zero")
    return InputEvent(ev_kind, button, x, y, delta)


def decode_server(buf: bytes):
    """Decodes one shell->client datagram.

    Returns a ``(name, payload)`` tuple, one of::

        ("welcome",       ThemeState)
        ("theme_changed", ThemeState)
        ("input",         InputEvent)
        ("visibility",    bool)
        ("ping",          seq: int)
        ("goodbye",       reason: int)   # GOODBYE_* codes
        ("panel_opened",  (width, height))
        ("panel_closed",  reason: int)   # PANEL_CLOSED_* codes
        ("panel_input",   InputEvent)    # panel-local coordinates

    Raises :class:`DecodeError` for anything else. A client's correct
    response to a DecodeError is to drop the connection — the two ends
    disagree about the protocol, and continuing would be guessing.
    """
    kind = _check_header(buf)
    body = buf[4:]
    if kind == KIND_WELCOME:
        return ("welcome", _decode_theme_state(body, "Welcome"))
    if kind == KIND_THEME_CHANGED:
        return ("theme_changed", _decode_theme_state(body, "ThemeChanged"))
    if kind == KIND_INPUT:
        return ("input", _decode_input(body, "Input"))
    if kind == KIND_PANEL_INPUT:
        # The payload is identical to Input; only the coordinate space
        # differs (panel-local device pixels) — and PanelInput alone
        # may carry Motion (kind 6, hover tracking).
        return ("panel_input", _decode_input(body, "PanelInput",
                                             allow_motion=True))
    if kind == KIND_PANEL_OPENED:
        _exact(body, 8, "PanelOpened")
        width, height = struct.unpack("<II", body)
        if not panel_fits(width, height):
            raise DecodeError(
                f"PanelOpened grant {width}x{height} is out of range")
        return ("panel_opened", (width, height))
    if kind == KIND_PANEL_CLOSED:
        # reason u8 + 3 reserved zero bytes, the Goodbye/Visibility
        # padding convention.
        _exact(body, 4, "PanelClosed")
        if body[1:] != b"\x00\x00\x00":
            raise DecodeError("PanelClosed reserved bytes were not zero")
        if body[0] not in PANEL_CLOSED_NAMES:
            raise DecodeError(f"undefined panel-closed reason {body[0]}")
        return ("panel_closed", body[0])
    if kind == KIND_VISIBILITY:
        _exact(body, 4, "Visibility")
        if body[1:] != b"\x00\x00\x00":
            raise DecodeError("Visibility reserved bytes were not zero")
        if body[0] > 1:
            raise DecodeError(f"undefined visibility value {body[0]}")
        return ("visibility", body[0] == 1)
    if kind == KIND_PING:
        _exact(body, 4, "Ping")
        return ("ping", struct.unpack("<I", body)[0])
    if kind == KIND_GOODBYE:
        _exact(body, 4, "Goodbye")
        if body[1:] != b"\x00\x00\x00":
            raise DecodeError("Goodbye reserved bytes were not zero")
        if body[0] not in GOODBYE_NAMES:
            raise DecodeError(f"undefined goodbye reason {body[0]}")
        return ("goodbye", body[0])
    raise DecodeError(f"unknown message kind {kind:#04x}")
