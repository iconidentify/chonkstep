"""Instrument-panel tests for the Python SDK, headless.

The pattern is the chonk-switch harness, extended to the panel half of
the protocol: a fake shell over a *real* ``SOCK_SEQPACKET`` socket,
with the shell-side encoders/decoders written out independently here
rather than borrowed from the SDK, so a codec bug cannot vouch for
itself. The dockapp under test is a real ``Dockapp`` subclass running
its real event loop in a thread, launched the way the dock would
launch it (socket + token in the environment).

What is held:

- byte-exact encodings of OpenPanel, the *banded* PanelFrame
  (generation, y, band_height, width) and ClosePanel, and strict
  decodes of PanelOpened / the padded PanelClosed / PanelInput;
- a full repaint is a top-to-bottom, contiguous band sequence sharing
  one generation, each band within MAX_FRAME_BYTES — reassembled and
  compared byte-exact; an oversized band is rejected at the encoder;
- `open_panel` on a protocol-1 shell fails locally with a clean error
  (the shell advertises its version in Welcome; pre-panel shells zero
  that field);
- the grant flow, including a clamped grant: the SDK draws at the
  granted size, never the requested one;
- no frame crosses the wire before the grant — ``panel.draw`` raises
  and the paint callback stays idle, so a user *cannot* protocol-error;
- refusal (PanelClosed reason=3) surfaces as ``on_closed("refused")``;
- a panel dismissed behind the dockapp's back surfaces as
  ``"dismissed"`` and deadens the handle;
- close is a round trip: ClosePanel out, PanelClosed(0) back,
  ``on_closed("closed")``;
- renegotiation re-sends OpenPanel and blocks frames until the new
  grant;
- PanelInput reaches ``panel.on_input`` in panel coordinates, and a
  True return repaints the panel immediately;
- a shell shutdown (and any dropped connection) synthesizes
  ``on_closed("shutdown")`` — every close reason has a test.

Run: python3 -m unittest discover bindings/python/tests
"""

import os
import queue
import socket
import struct
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))

from chonkdock import (  # noqa: E402
    Dockapp, PanelError, INPUT_PRESS, INPUT_SCROLL,
)
from chonkdock import wire  # noqa: E402

TILE = 56
TOKEN = bytes(range(16))
PANEL_FILL = b"\x10\x20\x30\xff"

# -- the shell's half of the wire, written independently --------------


def enc_welcome(tile_px=TILE, scale=1.0, theme_id="nextstep-classic",
                proto=2):
    # The u16 that was reserved in protocol 1 carries the shell's
    # protocol version; a pre-panel shell sends 0 there.
    ident = theme_id.encode()
    scale_bits = struct.unpack("<I", struct.pack("<f", scale))[0]
    return (struct.pack("<B3x", 0x81)
            + struct.pack("<IIHHI", tile_px, scale_bits, len(ident), proto,
                          0)
            + ident)


def enc_input(kind, button, x, y, delta=0):
    return struct.pack("<B3xBBHiii", 0x83, kind, button, 0, x, y, delta)


def enc_panel_input(kind, button, x, y, delta=0):
    return struct.pack("<B3xBBHiii", 0x89, kind, button, 0, x, y, delta)


def enc_panel_opened(w, h):
    return struct.pack("<B3xII", 0x87, w, h)


def enc_panel_closed(reason):
    # reason u8 + 3 reserved zeros, the Goodbye/Visibility convention.
    return struct.pack("<B3xB3x", 0x88, reason)


def enc_goodbye(reason):
    return struct.pack("<B3xB3x", 0x86, reason)


def dec_client(buf):
    kind = buf[0]
    assert buf[1:4] == b"\x00\x00\x00", "reserved header bytes must be zero"
    body = buf[4:]
    if kind == 0x01:  # Hello
        proto, units, wants, id_len = struct.unpack_from("<IBBBx", body, 0)
        token = body[8:24]
        ident = body[24:24 + id_len].decode()
        assert len(body) == 24 + id_len, "trailing bytes after Hello"
        return ("hello", (proto, units, wants, token, ident))
    if kind == 0x02:  # Frame
        gen, w, h = struct.unpack_from("<III", body, 0)
        pixels = body[12:]
        assert len(pixels) == w * h * 4, "frame length must match geometry"
        return ("frame", (gen, w, h, pixels))
    if kind == 0x03:  # Pong
        return ("pong", struct.unpack("<I", body)[0])
    if kind == 0x04:  # Log
        level, text_len = struct.unpack_from("<BxH", body, 0)
        return ("log", (level, body[4:4 + text_len].decode()))
    if kind == 0x05:  # OpenPanel
        assert len(body) == 8, "OpenPanel body must be exactly 8 bytes"
        return ("open_panel", struct.unpack("<II", body))
    if kind == 0x06:  # PanelFrame: one BAND — gen, y, band_height, width
        gen, y, bh, w = struct.unpack_from("<IIII", body, 0)
        pixels = body[16:]
        assert len(pixels) == w * bh * 4, \
            "panel band length must match its geometry"
        assert w * bh * 4 <= 262080, "a band must fit MAX_FRAME_BYTES"
        return ("panel_frame", (gen, y, bh, w, pixels))
    if kind == 0x07:  # ClosePanel
        assert body == b"", "ClosePanel carries no body"
        return ("close_panel", None)
    raise AssertionError(f"unexpected client message kind {kind:#x}")


# -- the dockapp under test -------------------------------------------


class Probe(Dockapp):
    """A real dockapp whose behavior each test scripts via `on_press`.

    The tile never changes after its mandatory first frame, so the
    tile half of the wire goes quiet and the panel traffic is easy to
    see. Everything observable lands on `events`."""

    def __init__(self):
        super().__init__("panel-probe", redraw_interval=0.05)
        self.events = queue.Queue()
        self.on_press = None  # set by each test; runs in the loop thread

    def draw(self, ctx, buf):
        return False  # the first frame goes out via must_present anyway

    def on_input(self, ctx, event):
        if event.kind == INPUT_PRESS and self.on_press is not None:
            try:
                self.on_press(self)
            except Exception as e:  # surfaced to the test thread
                self.events.put(("error", e))
        return False

    # panel callback plumbing, attached to every panel we open

    def open_probe_panel(self, w, h):
        panel = self.open_panel(w, h)
        panel.paint = self._paint
        panel.on_opened = lambda p: self.events.put(
            ("opened", (p.width, p.height)))
        panel.on_input = self._panel_input
        panel.on_closed = lambda p, reason: self.events.put(
            ("closed", reason))
        return panel

    def _paint(self, panel, buf):
        for i in range(0, len(buf), 4):
            buf[i:i + 4] = PANEL_FILL
        return False  # unchanged; must_present drives the sends

    def _panel_input(self, panel, event):
        self.events.put(("panel_input", event))
        return event.kind == INPUT_PRESS  # a press requests a repaint


class ShellHarness(unittest.TestCase):
    """The plumbing: one Probe in a thread, one fake shell, per test.
    Subclasses hold the tests; SHELL_PROTO is what the fake shell
    advertises in Welcome."""

    SHELL_PROTO = 2

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="chonkdock-panel.")
        sock_path = os.path.join(self.tmp.name, "dock.sock")
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        self.listener.bind(sock_path)
        self.listener.listen(1)
        self._env = {k: os.environ.get(k) for k in
                     ("CHONKSTEP_DOCK_SOCKET", "CHONKSTEP_DOCK_TOKEN")}
        os.environ["CHONKSTEP_DOCK_SOCKET"] = sock_path
        os.environ["CHONKSTEP_DOCK_TOKEN"] = TOKEN.hex()
        self.app = Probe()
        self.thread = threading.Thread(target=self._run_app, daemon=True)
        self.app_result = queue.Queue()
        self.thread.start()
        self.listener.settimeout(5.0)
        self.conn, _ = self.listener.accept()
        self.conn.settimeout(5.0)
        self.handshake()

    def _run_app(self):
        try:
            self.app.run()
            self.app_result.put(("returned", None))
        except Exception as e:
            self.app_result.put(("raised", e))

    def tearDown(self):
        try:
            self.conn.send(enc_goodbye(1))  # Shutdown: a clean exit
        except OSError:
            pass
        self.thread.join(timeout=5.0)
        self.conn.close()
        self.listener.close()
        self.tmp.cleanup()
        for k, v in self._env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        self.assertFalse(self.thread.is_alive(), "the dockapp never exited")

    # -- plumbing ------------------------------------------------------

    def handshake(self):
        name, (proto, units, wants, token, ident) = dec_client(
            self.conn.recv(262144))
        self.assertEqual(name, "hello")
        self.assertEqual((proto, units, token, ident),
                         (1, 1, TOKEN, "panel-probe"))
        self.conn.send(enc_welcome(proto=self.SHELL_PROTO))
        # The mandatory first tile frame; after this the tile is quiet.
        name, (_, w, h, _) = self.next_message()
        self.assertEqual((name, w, h), ("frame", TILE, TILE))

    def next_message(self, timeout=5.0):
        self.conn.settimeout(timeout)
        return dec_client(self.conn.recv(wire.MAX_MESSAGE_BYTES))

    def expect(self, want, timeout=5.0):
        """The next message of kind `want`, skipping tile frames/logs."""
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            name, payload = self.next_message(deadline - time.monotonic())
            if name == want:
                return payload
            self.assertIn(name, ("frame", "log", "pong"),
                          f"unexpected {name} while waiting for {want}")
        self.fail(f"no {want} arrived in time")

    def expect_silence(self, forbidden, window=0.4):
        """Nothing of kind `forbidden` may cross the wire for `window`."""
        deadline = time.monotonic() + window
        while True:
            try:
                self.conn.settimeout(max(deadline - time.monotonic(), 0.001))
                name, _ = dec_client(self.conn.recv(wire.MAX_MESSAGE_BYTES))
            except socket.timeout:
                return
            self.assertNotEqual(name, forbidden,
                                f"{forbidden} crossed the wire too early")

    def collect_repaint(self, want_w, want_h, timeout=5.0):
        """One full repaint: a top-to-bottom band sequence covering the
        granted height, contiguous, one shared generation. Returns the
        reassembled pixels."""
        deadline = time.monotonic() + timeout
        gen = None
        next_y = 0
        out = bytearray()
        while next_y < want_h:
            g, y, bh, w, pixels = self.expect(
                "panel_frame", deadline - time.monotonic())
            self.assertEqual(w, want_w,
                             "every band is exactly the granted width")
            self.assertEqual(y, next_y, "bands arrive top-to-bottom, "
                             "contiguous, starting at row 0")
            if gen is None:
                gen = g
            else:
                self.assertEqual(g, gen,
                                 "one repaint shares one generation")
            self.assertLessEqual(w * bh * 4, wire.MAX_FRAME_BYTES)
            out += pixels
            next_y += bh
        self.assertEqual(next_y, want_h, "bands tile the height exactly")
        return bytes(out)

    def press_tile(self):
        self.conn.send(enc_input(INPUT_PRESS, 1, TILE // 2, TILE // 2))

    def app_event(self, timeout=5.0):
        try:
            return self.app.events.get(timeout=timeout)
        except queue.Empty:
            self.fail("the dockapp reported nothing in time")


class Harness(ShellHarness):
    """The panel lifecycle against a protocol-2 shell."""

    def test_grant_flow_with_a_clamped_grant(self):
        self.app.on_press = lambda app: app.open_probe_panel(300, 200)
        self.press_tile()
        self.assertEqual(self.expect("open_panel"), (300, 200))
        panel = self.app.panel
        self.assertFalse(panel.opened, "no grant has arrived yet")
        self.assertIsNone(panel.width)
        # No PanelFrame may cross before the grant.
        self.expect_silence("panel_frame")
        # Grant it — clamped smaller than asked.
        self.conn.send(enc_panel_opened(280, 180))
        self.assertEqual(self.app_event(), ("opened", (280, 180)))
        pixels = self.collect_repaint(280, 180)
        self.assertEqual(pixels, PANEL_FILL * (280 * 180),
                         "the first repaint, reassembled, to the byte")
        self.assertTrue(panel.opened)
        self.assertEqual((panel.width, panel.height), (280, 180))
        self.assertEqual(panel.requested, (300, 200))

    def test_draw_before_the_grant_is_blocked_by_the_sdk(self):
        def open_and_jump_the_gun(app):
            panel = app.open_probe_panel(100, 100)
            try:
                panel.draw(b"\x00" * (100 * 100 * 4))
                app.events.put(("draw", "was allowed"))
            except PanelError:
                app.events.put(("draw", "raised"))
        self.app.on_press = open_and_jump_the_gun
        self.press_tile()
        self.assertEqual(self.expect("open_panel"), (100, 100))
        self.assertEqual(self.app_event(), ("draw", "raised"),
                         "a frame before PanelOpened must be refused locally")
        self.expect_silence("panel_frame")

    def test_a_refused_panel_reports_refused_and_never_opens(self):
        self.app.on_press = lambda app: app.open_probe_panel(64, 64)
        self.press_tile()
        self.expect("open_panel")
        self.conn.send(enc_panel_closed(3))  # refused
        self.assertEqual(self.app_event(), ("closed", "refused"))
        self.assertIsNone(self.app.panel, "a refused panel leaves no handle")
        self.expect_silence("panel_frame")

    def test_dismissed_behind_your_back_deadens_the_handle(self):
        self.app.on_press = lambda app: app.open_probe_panel(64, 64)
        self.press_tile()
        self.expect("open_panel")
        self.conn.send(enc_panel_opened(64, 64))
        self.expect("panel_frame")
        panel = self.app.panel
        self.conn.send(enc_panel_closed(1))  # the user clicked away
        self.assertEqual(self.app_event(), ("opened", (64, 64)))
        self.assertEqual(self.app_event(), ("closed", "dismissed"))
        self.assertTrue(panel.closed)
        with self.assertRaises(PanelError):
            panel.draw(b"\x00" * (64 * 64 * 4))
        self.expect_silence("panel_frame")

    def test_close_is_a_round_trip_ending_in_closed(self):
        self.app.on_press = lambda app: app.open_probe_panel(64, 64)
        self.press_tile()
        self.expect("open_panel")
        self.conn.send(enc_panel_opened(64, 64))
        self.expect("panel_frame")
        self.app.on_press = lambda app: app.panel.close()
        self.press_tile()
        self.assertIsNone(self.expect("close_panel"))
        self.conn.send(enc_panel_closed(0))  # the shell confirms
        self.assertEqual(self.app_event(), ("opened", (64, 64)))
        self.assertEqual(self.app_event(), ("closed", "closed"))
        self.assertIsNone(self.app.panel)

    def test_renegotiation_blocks_frames_until_the_new_grant(self):
        self.app.on_press = lambda app: app.open_probe_panel(64, 64)
        self.press_tile()
        self.expect("open_panel")
        self.conn.send(enc_panel_opened(64, 64))
        self.expect("panel_frame")
        first = self.app.panel
        # Ask again, bigger: same handle, a fresh OpenPanel on the wire.
        self.app.on_press = lambda app: app.open_probe_panel(128, 96)
        self.press_tile()
        self.assertEqual(self.expect("open_panel"), (128, 96))
        self.assertIs(self.app.panel, first, "renegotiation keeps the handle")
        self.assertFalse(first.opened, "frames pause until the new grant")
        self.expect_silence("panel_frame")
        self.conn.send(enc_panel_opened(128, 96))
        pixels = self.collect_repaint(128, 96)
        self.assertEqual(pixels, PANEL_FILL * (128 * 96))

    def test_panel_input_is_dispatched_in_panel_coordinates(self):
        self.app.on_press = lambda app: app.open_probe_panel(64, 64)
        self.press_tile()
        self.expect("open_panel")
        self.conn.send(enc_panel_opened(64, 64))
        self.expect("panel_frame")
        self.assertEqual(self.app_event(), ("opened", (64, 64)))
        self.conn.send(enc_panel_input(INPUT_SCROLL, 0, 7, 9, delta=-2))
        kind, event = self.app_event()
        self.assertEqual(kind, "panel_input")
        self.assertEqual((event.kind, event.x, event.y, event.delta),
                         (INPUT_SCROLL, 7, 9, -2))
        # Motion (kind 6) is hover tracking: panel-only, button 0,
        # dispatched to on_input like any other panel event.
        self.conn.send(enc_panel_input(6, 0, 15, 16))
        kind, event = self.app_event()
        self.assertEqual(kind, "panel_input")
        self.assertEqual((event.kind, event.button, event.x, event.y),
                         (wire.INPUT_MOTION, 0, 15, 16))
        # A press asks for a repaint (the probe returns True for it).
        self.conn.send(enc_panel_input(INPUT_PRESS, 1, 3, 4))
        kind, event = self.app_event()
        self.assertEqual((kind, event.x, event.y), ("panel_input", 3, 4))
        self.expect("panel_frame")

    def test_a_full_repaint_streams_as_contiguous_bands(self):
        # 640 wide: a band carries at most 262080 // 2560 = 102 rows,
        # so a 300-row repaint is exactly 102 + 102 + 96.
        self.app.on_press = lambda app: app.open_probe_panel(640, 300)
        self.press_tile()
        self.assertEqual(self.expect("open_panel"), (640, 300))
        self.conn.send(enc_panel_opened(640, 300))
        self.assertEqual(self.app_event(), ("opened", (640, 300)))
        bands = []
        pixels = bytearray()
        deadline = time.monotonic() + 5.0
        while sum(b[2] for b in bands) < 300:
            g, y, bh, w, px = self.expect("panel_frame",
                                          deadline - time.monotonic())
            bands.append((g, y, bh))
            self.assertEqual(w, 640)
            pixels += px
        self.assertEqual([(y, bh) for _, y, bh in bands],
                         [(0, 102), (102, 102), (204, 96)],
                         "maximal legal bands, top to bottom")
        self.assertEqual(len({g for g, _, _ in bands}), 1,
                         "one repaint shares one generation")
        self.assertEqual(bytes(pixels), PANEL_FILL * (640 * 300),
                         "the reassembled repaint, to the byte")

    def test_draw_rows_sends_one_narrow_band(self):
        row = b"\xEE\xDD\xCC\xff" * 64
        def open_it(app):
            app.open_probe_panel(64, 64).paint = None  # push-style
        self.app.on_press = open_it
        self.press_tile()
        self.expect("open_panel")
        self.conn.send(enc_panel_opened(64, 64))
        self.assertEqual(self.app_event(), ("opened", (64, 64)))
        self.app.on_press = lambda app: app.panel.draw_rows(10, row * 2)
        self.press_tile()
        g, y, bh, w, px = self.expect("panel_frame")
        self.assertEqual((y, bh, w), (10, 2, 64),
                         "a partial update is one band at its own rows")
        self.assertEqual(px, row * 2)
        # Out-of-grant partial updates are refused locally.
        def bad(app):
            try:
                app.panel.draw_rows(63, row * 2)
            except PanelError:
                app.events.put(("rows", "raised"))
        self.app.on_press = bad
        self.press_tile()
        self.assertEqual(self.app_event(), ("rows", "raised"))

    def test_shell_shutdown_reaches_the_panel_as_shutdown(self):
        self.app.on_press = lambda app: app.open_probe_panel(64, 64)
        self.press_tile()
        self.expect("open_panel")
        self.conn.send(enc_panel_opened(64, 64))
        self.expect("panel_frame")
        self.conn.send(enc_goodbye(1))  # Shutdown
        self.assertEqual(self.app_event(), ("opened", (64, 64)))
        self.assertEqual(self.app_event(), ("closed", "shutdown"),
                         "a dying connection closes the panel locally")
        self.assertEqual(self.app_result.get(timeout=5.0),
                         ("returned", None))


class V1Shell(ShellHarness):
    """The same probe against a shell that predates panels (it zeroes
    the version field in Welcome). The tile must work; open_panel must
    fail locally, cleanly, without ever putting 0x05 on the wire —
    a pre-panel shell would answer it with Goodbye{ProtocolError} and
    take the tile down too."""

    SHELL_PROTO = 0

    def test_open_panel_is_gated_off_cleanly(self):
        def try_open(app):
            try:
                app.open_probe_panel(64, 64)
            except PanelError as e:
                app.events.put(("gate", str(e)))
        self.app.on_press = try_open
        self.press_tile()
        kind, message = self.app_event()
        self.assertEqual(kind, "gate")
        self.assertIn("predates instrument panels", message)
        self.assertIsNone(self.app.panel)
        # Nothing panel-shaped may reach a shell that cannot read it.
        self.expect_silence("open_panel")
        # And the tile half is entirely unharmed: pings still answer.
        self.conn.send(struct.pack("<B3xI", 0x85, 0xBEEF))
        deadline = time.monotonic() + 5.0
        while True:
            name, payload = self.next_message(deadline - time.monotonic())
            if name == "pong":
                self.assertEqual(payload, 0xBEEF)
                break


class PanelWire(unittest.TestCase):
    """The codec alone: byte-exact encodings, strict decodes."""

    def test_open_panel_has_the_documented_byte_layout(self):
        msg = wire.encode_open_panel(320, 240)
        self.assertEqual(msg, b"\x05\x00\x00\x00"
                         + struct.pack("<II", 320, 240))

    def test_panel_frame_is_a_band_with_the_documented_byte_layout(self):
        # {generation u32, y u32, band_height u32, width u32, pixels}
        pixels = b"\xAA" * (2 * 3 * 4)
        msg = wire.encode_panel_frame(7, 5, 3, 2, pixels)
        self.assertEqual(msg, b"\x06\x00\x00\x00"
                         + struct.pack("<IIII", 7, 5, 3, 2) + pixels)

    def test_close_panel_is_header_only(self):
        self.assertEqual(wire.encode_close_panel(), b"\x07\x00\x00\x00")

    def test_panel_fits_agrees_with_the_protocol_caps(self):
        self.assertTrue(wire.panel_fits(1, 1))
        self.assertTrue(wire.panel_fits(1024, 1024))  # exactly 4 MiB
        self.assertFalse(wire.panel_fits(1025, 1))
        self.assertFalse(wire.panel_fits(1, 1025))
        self.assertFalse(wire.panel_fits(0, 64))
        self.assertFalse(wire.panel_fits(64, 0))
        self.assertEqual(1024 * 1024 * 4, wire.MAX_PANEL_FRAME_BYTES)

    def test_panel_band_rows_fills_but_never_overflows_a_datagram(self):
        self.assertEqual(wire.panel_band_rows(1024), 63)   # 262080//4096
        self.assertEqual(wire.panel_band_rows(320), 204)   # 262080//1280
        self.assertEqual(wire.panel_band_rows(1), 1024)    # edge-capped
        for width in (1, 17, 56, 320, 640, 1024):
            rows = wire.panel_band_rows(width)
            self.assertLessEqual(width * rows * 4, wire.MAX_FRAME_BYTES)
            self.assertLessEqual(rows, wire.MAX_PANEL_PX)
            self.assertGreater(rows, 0)

    def test_encoders_reject_out_of_range_geometry(self):
        with self.assertRaises(wire.EncodeError):
            wire.encode_open_panel(0, 64)
        with self.assertRaises(wire.EncodeError):
            wire.encode_open_panel(64, 1025)
        # An OpenPanel request may ask up to the protocol bound.
        wire.encode_open_panel(1024, 1024)
        with self.assertRaises(wire.EncodeError):
            wire.encode_panel_frame(1, 0, 2, 2, b"\x00" * 15)  # short byte
        with self.assertRaises(wire.EncodeError):
            wire.encode_panel_frame(1, 0, 1, 0, b"")  # zero width
        with self.assertRaises(wire.EncodeError):
            wire.encode_panel_frame(1, 0, 0, 2, b"")  # zero band height
        with self.assertRaises(wire.EncodeError):
            wire.encode_panel_frame(1, 0, 1, 1025, b"\x00" * (1025 * 4))
        with self.assertRaises(wire.EncodeError):
            # Rows run past the protocol's panel edge.
            wire.encode_panel_frame(1, 1000, 25, 8, b"\x00" * (8 * 25 * 4))
        with self.assertRaises(wire.EncodeError):
            # An oversized band: 1024 * 64 * 4 = 262144 > 262080.
            wire.encode_panel_frame(1, 0, 64, 1024,
                                    b"\x00" * (1024 * 64 * 4))
        # One row shy fits.
        wire.encode_panel_frame(1, 0, 63, 1024, b"\x00" * (1024 * 63 * 4))

    def test_panel_opened_decodes_and_bounds_the_grant(self):
        name, size = wire.decode_server(enc_panel_opened(280, 180))
        self.assertEqual((name, size), ("panel_opened", (280, 180)))
        # Banding made big grants streamable: the whole protocol range
        # is grantable now.
        self.assertEqual(wire.decode_server(enc_panel_opened(1024, 1024)),
                         ("panel_opened", (1024, 1024)))
        with self.assertRaises(wire.DecodeError):
            wire.decode_server(enc_panel_opened(1025, 64))  # over the cap
        with self.assertRaises(wire.DecodeError):
            wire.decode_server(enc_panel_opened(0, 64))  # zero edge
        with self.assertRaises(wire.DecodeError):
            wire.decode_server(enc_panel_opened(64, 64) + b"\x00")  # trailing

    def test_panel_closed_is_padded_and_decodes_every_reason(self):
        for reason, name in [(0, "closed"), (1, "dismissed"),
                             (2, "shutdown"), (3, "refused")]:
            got = wire.decode_server(enc_panel_closed(reason))
            self.assertEqual(got, ("panel_closed", reason))
            self.assertEqual(wire.PANEL_CLOSED_NAMES[reason], name)
        with self.assertRaises(wire.DecodeError):
            wire.decode_server(enc_panel_closed(4))
        with self.assertRaises(wire.DecodeError):
            # The unpadded 1-byte body is NOT the layout any more.
            wire.decode_server(struct.pack("<B3xB", 0x88, 0))
        with self.assertRaises(wire.DecodeError):
            # The reserved padding must be zero.
            wire.decode_server(struct.pack("<B3xBBBB", 0x88, 0, 1, 0, 0))

    def test_welcome_advertises_the_shell_protocol_version(self):
        _, state = wire.decode_server(enc_welcome(proto=2))
        self.assertEqual(state.proto, 2)
        # A pre-panel shell zeroes the field; zero decodes as 1.
        _, state = wire.decode_server(enc_welcome(proto=0))
        self.assertEqual(state.proto, 1)

    def test_panel_input_is_input_shaped_and_just_as_strict(self):
        name, ev = wire.decode_server(enc_panel_input(1, 1, 10, 20))
        self.assertEqual(name, "panel_input")
        self.assertEqual((ev.kind, ev.button, ev.x, ev.y, ev.delta),
                         (1, 1, 10, 20, 0))
        bad = bytearray(enc_panel_input(1, 1, 10, 20))
        bad[6] = 1  # reserved field
        with self.assertRaises(wire.DecodeError):
            wire.decode_server(bytes(bad))
        with self.assertRaises(wire.DecodeError):
            wire.decode_server(struct.pack("<B3xBBHiii", 0x89, 7, 0, 0,
                                           0, 0, 0))  # undefined kind

    def test_motion_exists_only_inside_panel_input(self):
        # Kind 6 Motion, byte-exact: button 0, panel device pixels.
        motion = struct.pack("<B3xBBHiii", 0x89, 6, 0, 0, 12, 34, 0)
        name, ev = wire.decode_server(motion)
        self.assertEqual(name, "panel_input")
        self.assertEqual((ev.kind, ev.button, ev.x, ev.y, ev.delta),
                         (wire.INPUT_MOTION, 0, 12, 34, 0))
        # The same 16 bytes as a tile Input (0x83) are undefined.
        with self.assertRaises(wire.DecodeError):
            wire.decode_server(struct.pack("<B3xBBHiii", 0x83, 6, 0, 0,
                                           12, 34, 0))


if __name__ == "__main__":
    unittest.main()
