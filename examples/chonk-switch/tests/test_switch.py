"""Protocol-level tests for the appearance switch, headless.

The harness here plays the *shell's* half of the dockapp protocol —
a real `SOCK_SEQPACKET` listener, a real `Welcome`, real `Input`
messages — against the real `chonk-switch.py` process, launched
exactly as the dock would launch it (socket + token in the
environment, `XDG_STATE_HOME` pointed at a scratch directory). No
compositor anywhere. The shell-side encoders/decoders are written out
independently here rather than borrowed from the SDK, so a codec bug
cannot vouch for itself.

What is held:

- the handshake, and a correct first frame in each mode — compared
  *byte-exact* against the renderer, because the switch is
  deterministic once the lever settles;
- one click produces exactly one atomic `toggle` request (no temp
  litter, no second file), and the visual completes once the mode
  file confirms;
- a mode changed by someone else entirely moves this lever too;
- pings are answered, and a settled switch sends nothing at all.

Run: python3 -m unittest discover examples/chonk-switch/tests
"""

import importlib.util
import os
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
SCRIPT = os.path.join(HERE, "..", "chonk-switch.py")

_spec = importlib.util.spec_from_file_location("chonk_switch", SCRIPT)
switch_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(switch_mod)

TILE = 56
TOKEN = bytes(range(16))

# -- the shell's half of the wire, written independently --------------


def enc_welcome(tile_px=TILE, scale=1.0, theme_id="nextstep-classic",
                toml=""):
    ident, body = theme_id.encode(), toml.encode()
    scale_bits = struct.unpack("<I", struct.pack("<f", scale))[0]
    return (struct.pack("<B3x", 0x81)
            + struct.pack("<IIHHI", tile_px, scale_bits, len(ident), 0,
                          len(body))
            + ident + body)


def enc_input(kind, button, x, y, delta=0):
    return struct.pack("<B3xBBHiii", 0x83, kind, button, 0, x, y, delta)


def enc_ping(seq):
    return struct.pack("<B3xI", 0x85, seq)


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
    raise AssertionError(f"unexpected client message kind {kind:#x}")


def expected_frame(pos, pressed=False, size=TILE):
    """What the settled switch must have sent, to the byte."""
    buf = bytearray(size * size * 4)
    switch_mod.render(buf, size, pos, pressed,
                      *switch_mod.CLASSIC_TILE, scale=1.0)
    return bytes(buf)


class Harness(unittest.TestCase):
    """One switch process, one fake shell, per test."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="chonk-switch-test.")
        self.state = os.path.join(self.tmp.name, "state", "chonkstep")
        os.makedirs(self.state)
        sock_path = os.path.join(self.tmp.name, "dock.sock")
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        self.listener.bind(sock_path)
        self.listener.listen(1)
        env = dict(os.environ,
                   CHONKSTEP_DOCK_SOCKET=sock_path,
                   CHONKSTEP_DOCK_TOKEN=TOKEN.hex(),
                   CHONKSTEP_SCALE="1.0000",
                   CHONKSTEP_THEME="nextstep-classic",
                   XDG_STATE_HOME=os.path.join(self.tmp.name, "state"))
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)
        self.proc = subprocess.Popen([sys.executable, SCRIPT], env=env,
                                     stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
        self.listener.settimeout(5.0)
        self.conn, _ = self.listener.accept()
        self.conn.settimeout(5.0)

    def tearDown(self):
        self.conn.close()
        self.listener.close()
        self.proc.terminate()  # our own recorded child, nothing else
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
            self.proc.wait()
        self.tmp.cleanup()

    # -- plumbing ------------------------------------------------------

    def handshake(self):
        name, (proto, units, wants, token, ident) = dec_client(
            self.conn.recv(262144))
        self.assertEqual(name, "hello", "the first message must be Hello")
        # The SDK announces protocol 2 ("I know the formerly-reserved
        # proto u16 in the Welcome body"); the shell accepts 1..=2.
        self.assertEqual(proto, 2)
        self.assertEqual(units, 1, "the switch is a single tile")
        self.assertEqual(token, TOKEN)
        self.assertEqual(ident, "chonk-switch")
        self.conn.send(enc_welcome())

    def next_message(self, timeout=5.0):
        self.conn.settimeout(timeout)
        return dec_client(self.conn.recv(262144))

    def next_frame(self, timeout=5.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            name, payload = self.next_message(deadline - time.monotonic())
            if name == "frame":
                return payload
        self.fail("no frame arrived in time")

    def settle_on(self, pixels, what, timeout=5.0):
        """Drains frames until one matches `pixels` byte-exactly."""
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                _, w, h, got = self.next_frame(deadline - time.monotonic())
            except (socket.timeout, AssertionError):
                break
            self.assertEqual((w, h), (TILE, TILE),
                             "every frame must match the allocated geometry")
            last = got
            if got == pixels:
                return
        self.fail(f"the switch never settled on {what} "
                  f"(last frame differed: {last != pixels})")

    def write_mode(self, mode):
        with open(os.path.join(self.state, "appearance"), "w") as f:
            f.write(mode + "\n")

    # -- the tests -----------------------------------------------------

    def test_handshake_then_a_correct_light_frame_unprompted(self):
        # No mode file at all: the spec's default is light.
        self.handshake()
        _, w, h, pixels = self.next_frame()
        self.assertEqual((w, h), (TILE, TILE))
        self.assertEqual(pixels, expected_frame(pos=0.0),
                         "the settled light face, to the byte")

    def test_a_click_is_one_atomic_toggle_and_the_confirmed_throw_lands(self):
        self.write_mode("light")
        self.handshake()
        self.next_frame()

        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))  # Press, Left
        self.conn.send(enc_input(2, 0, TILE // 2, TILE // 2))  # Release

        request = os.path.join(self.state, "appearance-request")
        deadline = time.monotonic() + 5.0
        while not os.path.exists(request):
            self.assertLess(time.monotonic(), deadline, "no toggle request")
            time.sleep(0.02)
        with open(request) as f:
            self.assertEqual(f.read(), "toggle",
                             "the request is the bare word, atomically whole")
        litter = [n for n in os.listdir(self.state)
                  if n.startswith(".appearance-request.")]
        self.assertEqual(litter, [], "no temp files may survive the rename")
        self.assertEqual(
            sorted(os.listdir(self.state)),
            ["appearance", "appearance-request"],
            "exactly one request per click")

        # Now be the shell: consume the request, flip the mode.
        os.unlink(request)
        self.write_mode("dark")
        self.settle_on(expected_frame(pos=1.0), "the dark face")

    def test_an_external_mode_change_moves_the_lever_too(self):
        self.write_mode("light")
        self.handshake()
        self.next_frame()
        # Nobody clicked this tile; someone else changed the mode.
        self.write_mode("dark")
        self.settle_on(expected_frame(pos=1.0), "the dark face")
        self.write_mode("light")
        self.settle_on(expected_frame(pos=0.0), "the light face again")

    def test_pings_are_answered_and_a_settled_switch_sends_nothing(self):
        self.handshake()
        self.next_frame()
        self.conn.send(enc_ping(0xC0FFEE))
        deadline = time.monotonic() + 5.0
        while True:
            name, payload = self.next_message(deadline - time.monotonic())
            if name == "pong":
                self.assertEqual(payload, 0xC0FFEE)
                break
        # Settled and unpoked: the wire must go quiet (logs included).
        self.conn.settimeout(0.8)
        try:
            extra = dec_client(self.conn.recv(262144))
            self.fail(f"a settled switch sent {extra[0]}")
        except socket.timeout:
            pass


class PureLogic(unittest.TestCase):
    """The pieces that need no process and no socket."""

    def test_read_mode_defaults_trims_and_rejects_garbage(self):
        with tempfile.TemporaryDirectory() as d:
            self.assertEqual(switch_mod.read_mode(d), "light",
                             "absent means light")
            path = os.path.join(d, "appearance")
            for text, want in [("dark\n", "dark"), ("  light  ", "light"),
                               ("dark", "dark"), ("mauve", "light"),
                               ("", "light")]:
                with open(path, "w") as f:
                    f.write(text)
                self.assertEqual(switch_mod.read_mode(d), want, repr(text))

    def test_request_is_atomic_and_validates_its_vocabulary(self):
        with tempfile.TemporaryDirectory() as d:
            switch_mod.request("toggle", d)
            with open(os.path.join(d, "appearance-request")) as f:
                self.assertEqual(f.read(), "toggle")
            self.assertEqual(sorted(os.listdir(d)), ["appearance-request"],
                             "temp file renamed away, nothing else created")
            switch_mod.request("dark", d)  # replaces, still exactly one file
            self.assertEqual(sorted(os.listdir(d)), ["appearance-request"])
            with self.assertRaises(ValueError):
                switch_mod.request("darker", d)

    def test_the_two_faces_differ_where_legibility_lives(self):
        light, dark = expected_frame(0.0), expected_frame(1.0)
        self.assertNotEqual(light, dark)
        # The exposed track is the across-the-room signal: sample the
        # slot's right half in light mode against its left half in
        # dark mode — both are exposed track, and their brightness
        # must be far apart.
        def region_luma(pixels, x0, x1):
            total = count = 0
            y0, y1 = round(TILE * 0.42), round(TILE * 0.58)
            for y in range(y0, y1):
                for x in range(x0, x1):
                    i = (y * TILE + x) * 4
                    total += pixels[i] + pixels[i + 1] + pixels[i + 2]
                    count += 3
            return total / count
        lit = region_luma(light, round(TILE * 0.60), round(TILE * 0.82))
        unlit = region_luma(dark, round(TILE * 0.18), round(TILE * 0.40))
        self.assertGreater(lit - unlit, 120,
                           f"track contrast too low: {lit:.0f} vs {unlit:.0f}")

    def test_an_unconfirmed_click_settles_back_to_the_file(self):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "appearance"), "w") as f:
                f.write("light")
            sw = switch_mod.Switch(directory=d)
            self.assertEqual(sw.mode, "light")
            # A click happened, but the deadline has already passed and
            # the mode file still says light: the prediction expires.
            sw.pending = ("dark", time.monotonic() - 1.0)
            sw._sample()
            self.assertIsNone(sw.pending)
            self.assertEqual(sw._target_mode(), "light",
                             "the file is the truth; the click was a guess")

    def test_theme_toml_palette_is_used_and_garbage_falls_back(self):
        toml = ('[tile.fill.Gradient]\ndirection = "Diagonal"\n'
                '[tile.fill.Gradient.from]\nr = 1\ng = 2\nb = 3\na = 255\n'
                '[tile.fill.Gradient.to]\nr = 9\ng = 8\nb = 7\na = 255\n')
        self.assertEqual(switch_mod.tile_colors(toml),
                         ((1, 2, 3), (9, 8, 7)))
        self.assertEqual(switch_mod.tile_colors("not toml ["),
                         switch_mod.CLASSIC_TILE)
        self.assertEqual(switch_mod.tile_colors(""),
                         switch_mod.CLASSIC_TILE)


if __name__ == "__main__":
    unittest.main()
