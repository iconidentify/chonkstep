"""chonk-net's tests, headless.

Three layers, none needing a compositor or the real network:

- **Parsers and the read-only guarantee**: pure functions against
  canned nmcli/iw/sysfs text (including hostile garbage and
  absent-tool cases), and structural assertions that the frozen
  command table is the *only* thing this dockapp can ever execute.
- **Renderers**: byte-stable faces — the same state renders to the
  same bytes, distinct states to distinct bytes.
- **The wire**: a fake shell (the pattern of chonk-switch's tests — a
  real `SOCK_SEQPACKET` listener speaking independently written
  encoders) drives the real `chonk-net.py` process, with
  `CHONKNET_FAKE_DATA` pointing its data layer at canned files so the
  machine's actual network is never touched. Held here: the
  handshake and a byte-exact first tile frame; tile click ->
  OpenPanel; **no PanelFrame before the grant, ever**; a clamped
  grant obeyed exactly; hover input repainting; the rescan row
  running exactly one whitelisted scan, rate-limited; dismissal and
  refusal both resetting the state machine; pings answered.

Run: python3 -m unittest discover examples/chonk-net/tests
"""

import importlib.util
import os
import re
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
APP_DIR = os.path.normpath(os.path.join(HERE, ".."))
SCRIPT = os.path.join(APP_DIR, "chonk-net.py")
sys.path.insert(0, APP_DIR)

import netdata  # noqa: E402

_spec = importlib.util.spec_from_file_location("chonk_net", SCRIPT)
net_mod = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(net_mod)  # also puts the SDK on sys.path

from chonkdock import wire as cwire  # noqa: E402

TILE = 56
TOKEN = bytes(range(16))

# ---------------------------------------------------------------------
# Canned tool output — the real shapes, including this repo machine's
# actual zoo of veths and bridges.
# ---------------------------------------------------------------------

NM_DEVICES_WIRED_ONLY = """\
eno1:ethernet:connected:Wired connection 1
tailscale0:tun:connected (externally):tailscale0
cni0:bridge:connected (externally):cni0
lo:loopback:connected (externally):lo
docker0:bridge:connected (externally):docker0
veth05916708:ethernet:unmanaged:
veth18f69d25:ethernet:unmanaged:
"""

NM_DEVICES_WIFI = """\
wlan0:wifi:connected:Basilisk
eno1:ethernet:unavailable:
lo:loopback:connected (externally):lo
"""

NM_WIFI_LIST = """\
*:Basilisk:88:5180 MHz:405 Mbit/s:WPA2
 :Basilisk:62:2437 MHz:130 Mbit/s:WPA2
 :chonknet-6e:71:5955 MHz:960 Mbit/s:WPA3
 :Cafe\\:Upstairs:58:2412 MHz:65 Mbit/s:WPA1 WPA2
 :PrinterSetup-8F2A:58:2462 MHz:65 Mbit/s:--
 ::40:2412 MHz:65 Mbit/s:WPA2
garbage line with no colons at all
 :moss:not-a-number:2412 MHz:65 Mbit/s:WPA2
"""

IW_LINK_CONNECTED = """\
Connected to aa:bb:cc:dd:ee:ff (on wlan0)
\tSSID: Basilisk
\tfreq: 5180
\tRX: 123456 bytes (789 packets)
\tTX: 654321 bytes (987 packets)
\tsignal: -52 dBm
\trx bitrate: 866.7 MBit/s VHT-MCS 9 80MHz short GI VHT-NSS 2
\ttx bitrate: 405.0 MBit/s VHT-MCS 4 80MHz VHT-NSS 2

\tbss flags:\tshort-slot-time
\tdtim period:\t2
\tbeacon int:\t100
"""

NM_IP4 = "IP4.ADDRESS[1]:10.1.1.71/24\nIP4.ADDRESS[2]:10.1.1.72/24\n"


def fake_runner(outputs):
    """A netdata runner serving canned text; absent key = absent tool."""
    def run(key, ifname=None, timeout=None):
        if key not in netdata.COMMANDS:
            raise KeyError(key)
        return outputs.get(key)
    return run


# ---------------------------------------------------------------------
# Parsers
# ---------------------------------------------------------------------

class Parsers(unittest.TestCase):
    def test_split_terse_honors_nmcli_escaping(self):
        self.assertEqual(netdata.split_terse(r"a\:b:c\\d:e"),
                         ["a:b", "c\\d", "e"])

    def test_devices_wired_machine(self):
        devs = netdata.parse_nm_devices(NM_DEVICES_WIRED_ONLY)
        self.assertEqual([d.device for d in devs], ["eno1"],
                         "bridges, tuns, loopback and veths are noise")
        self.assertEqual(devs[0].state, "connected")

    def test_devices_wifi_machine(self):
        devs = netdata.parse_nm_devices(NM_DEVICES_WIFI)
        self.assertEqual([(d.device, d.dev_type) for d in devs],
                         [("wlan0", "wifi"), ("eno1", "ethernet")])

    def test_wifi_list_dedupes_sorts_and_survives_garbage(self):
        nets = netdata.parse_nm_wifi_list(NM_WIFI_LIST)
        self.assertEqual([n.ssid for n in nets],
                         ["Basilisk", "chonknet-6e", "Cafe:Upstairs",
                          "PrinterSetup-8F2A"])
        basilisk = nets[0]
        self.assertTrue(basilisk.in_use)
        self.assertEqual(basilisk.signal, 88, "the in-use BSS wins its SSID")
        self.assertEqual(basilisk.band, "5")
        self.assertEqual(nets[1].band, "6")
        self.assertEqual(nets[3].security, "", "-- is an open network")
        self.assertEqual(nets[2].ssid, "Cafe:Upstairs",
                         "escaped colons belong to the SSID")

    def test_wifi_list_empty_and_hostile(self):
        self.assertEqual(netdata.parse_nm_wifi_list(""), [])
        self.assertEqual(netdata.parse_nm_wifi_list("\x00\xff nonsense"), [])

    def test_iw_link(self):
        link = netdata.parse_iw_link(IW_LINK_CONNECTED)
        self.assertTrue(link.connected)
        self.assertEqual(link.ssid, "Basilisk")
        self.assertEqual(link.signal_dbm, -52)
        self.assertEqual(link.bitrate_mbps, 405, "tx bitrate is the one shown")
        self.assertEqual(link.freq_mhz, 5180)
        self.assertFalse(netdata.parse_iw_link("Not connected.\n").connected)
        self.assertFalse(netdata.parse_iw_link("").connected)

    def test_band_of(self):
        for mhz, band in [(2412, "2.4"), (2484, "2.4"), (5180, "5"),
                          (5905, "5"), (5955, "6"), (7100, "6"),
                          (900, ""), (0, "")]:
            self.assertEqual(netdata.band_of(mhz), band, mhz)

    def test_ip4(self):
        self.assertEqual(netdata.parse_nm_ip4(NM_IP4), "10.1.1.71/24")
        self.assertEqual(netdata.parse_nm_ip4(""), "")
        self.assertEqual(netdata.parse_nm_ip4("GENERAL.DEVICE:eno1\n"), "")

    def test_sysfs_wired(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "eno1"))
            with open(os.path.join(d, "eno1", "carrier"), "w") as f:
                f.write("1\n")
            with open(os.path.join(d, "eno1", "speed"), "w") as f:
                f.write("1000\n")
            self.assertEqual(netdata.read_sysfs_wired("eno1", d), (True, 1000))
            with open(os.path.join(d, "eno1", "carrier"), "w") as f:
                f.write("0\n")
            self.assertEqual(netdata.read_sysfs_wired("eno1", d), (False, 0))
            self.assertEqual(netdata.read_sysfs_wired("nope", d), (False, 0))

    def test_dbm_to_percent(self):
        self.assertEqual(netdata.dbm_to_percent(-50), 100)
        self.assertEqual(netdata.dbm_to_percent(-100), 0)
        self.assertEqual(netdata.dbm_to_percent(-75), 50)
        self.assertEqual(netdata.dbm_to_percent(-30), 100)
        self.assertEqual(netdata.dbm_to_percent(-120), 0)


class Sampling(unittest.TestCase):
    def test_nm_wifi_snapshot(self):
        runner = fake_runner({
            "nm_devices": NM_DEVICES_WIFI,
            "nm_wifi_list": NM_WIFI_LIST,
            "iw_link": IW_LINK_CONNECTED,
            "nm_ip4": NM_IP4,
        })
        snap = netdata.sample(runner=runner, backend="networkmanager")
        self.assertEqual(snap.link.kind, "wifi")
        self.assertEqual(snap.link.ssid, "Basilisk")
        self.assertEqual(snap.link.signal, 88)
        self.assertEqual(snap.link.band, "5")
        self.assertEqual(snap.link.bitrate_mbps, 405)
        self.assertEqual(snap.link.ip4, "10.1.1.71/24")
        self.assertEqual(len(snap.networks), 4)

    def test_nm_wired_snapshot(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "eno1"))
            for name, val in (("carrier", "1"), ("speed", "1000")):
                with open(os.path.join(d, "eno1", name), "w") as f:
                    f.write(val)
            runner = fake_runner({"nm_devices": NM_DEVICES_WIRED_ONLY,
                                  "nm_ip4": NM_IP4})
            snap = netdata.sample(runner=runner, sys_net=d,
                                  backend="networkmanager")
        self.assertEqual(snap.link.kind, "wired")
        self.assertEqual(snap.link.bitrate_mbps, 1000)
        self.assertEqual(snap.scan_note, "no Wi-Fi hardware")
        self.assertEqual(snap.networks, ())

    def test_absent_tools_are_an_honest_unavailable(self):
        snap = netdata.sample(runner=fake_runner({}),
                              backend="networkmanager")
        self.assertEqual(snap.link.kind, "unavailable")
        self.assertTrue(snap.scan_note)

    def test_no_manager_falls_back_to_sysfs(self):
        with tempfile.TemporaryDirectory() as d:
            os.makedirs(os.path.join(d, "eth9"))
            with open(os.path.join(d, "eth9", "carrier"), "w") as f:
                f.write("1")
            with open(os.path.join(d, "eth9", "speed"), "w") as f:
                f.write("2500")
            with open(os.path.join(d, "eth9", "device"), "w") as f:
                f.write("")  # a real NIC has a device link
            snap = netdata.sample(runner=fake_runner({}), sys_net=d,
                                  backend="none")
        self.assertEqual(snap.backend, "none")
        self.assertEqual(snap.link.kind, "wired")
        self.assertEqual(snap.link.bitrate_mbps, 2500)

    def test_detect_backend_without_nm_or_iwd(self):
        with mock.patch.object(netdata.shutil, "which", return_value=None):
            self.assertEqual(netdata.detect_backend(fake_runner({})), "none")
        with mock.patch.object(netdata.shutil, "which",
                               return_value="/usr/bin/iwctl"):
            self.assertEqual(netdata.detect_backend(fake_runner({})), "iwd")
        nm = fake_runner({"nm_running": "running\n"})
        self.assertEqual(netdata.detect_backend(nm), "networkmanager")

    def test_rescan_gate(self):
        clock = [0.0]
        gate = netdata.RescanGate(min_interval=15.0, clock=lambda: clock[0])
        self.assertTrue(gate.allow())
        self.assertFalse(gate.allow(), "inside the interval: refused")
        self.assertAlmostEqual(gate.remaining(), 15.0)
        clock[0] = 20.0
        self.assertEqual(gate.remaining(), 0.0)
        self.assertTrue(gate.allow())


# ---------------------------------------------------------------------
# The read-only guarantee, structurally.
# ---------------------------------------------------------------------

FORBIDDEN_WORDS = {
    "connect", "disconnect", "up", "down", "modify", "add", "delete",
    "clone", "edit", "reload", "set", "radio", "hotspot", "rescan-now",
    "con", "connection", "reapply", "monitor", "disc", "auth", "scan",
}
# note: "scan" the iw subcommand (which *triggers* scans arbitrarily) is
# forbidden; nmcli's `--rescan yes` list flag is the one sanctioned look.


class ReadOnlyGuarantee(unittest.TestCase):
    def test_command_table_is_frozen(self):
        with self.assertRaises(TypeError):
            netdata.COMMANDS["evil"] = ("nmcli", "dev", "disconnect")
        for key, argv in netdata.COMMANDS.items():
            self.assertIsInstance(argv, tuple, key)

    def test_every_command_is_a_whitelisted_reader(self):
        for key, argv in netdata.COMMANDS.items():
            self.assertIn(argv[0], ("nmcli", "iw"), key)
            for word in argv[1:]:
                self.assertNotIn(word, FORBIDDEN_WORDS,
                                 f"{key} carries a state-changing verb")

    def test_run_command_refuses_off_table_keys_and_bad_ifnames(self):
        with self.assertRaises(KeyError):
            netdata.run_command("evil")
        for bad in ("", "a" * 16, "eth0; rm -rf", "../etc", "wlan 0", None):
            with self.assertRaises(ValueError):
                netdata.run_command("iw_link", ifname=bad)

    def test_single_subprocess_call_site(self):
        """`subprocess` is spoken exactly once, inside run_command, and
        nowhere at all in the renderer/app file — so the frozen table
        provably bounds what this dockapp can execute."""
        with open(os.path.join(APP_DIR, "netdata.py")) as f:
            code = [ln for ln in f
                    if not ln.lstrip().startswith("#") and "subprocess" in ln]
        calls = [ln for ln in code if "subprocess.run" in ln
                 or "subprocess.Popen" in ln or "subprocess.call" in ln
                 or "subprocess.check" in ln]
        self.assertEqual(len(calls), 1, calls)
        imports = [ln for ln in code if re.match(r"\s*import subprocess", ln)]
        self.assertEqual(len(imports), 1)
        with open(SCRIPT) as f:
            app_src = f.read()
        for word in ("subprocess", "os.system", "os.exec", "os.spawn",
                     "os.popen"):
            self.assertNotIn(word, app_src,
                             f"the app file must not speak {word}")
        with open(os.path.join(APP_DIR, "netdata.py")) as f:
            nd_src = f.read()
        for word in ("os.system", "os.exec", "os.spawn", "os.popen"):
            self.assertNotIn(word, nd_src)


# ---------------------------------------------------------------------
# Renderers: byte-stable.
# ---------------------------------------------------------------------

def tile_bytes(state, size=TILE):
    pal = net_mod.Palette("")
    snap = net_mod.demo_snapshot(state)
    buf = bytearray(size * size * 4)
    net_mod.render_tile(buf, size, snap.link, pal, 1.0)
    return bytes(buf)


def panel_bytes(state, w=700, h=500, hover=None, rescan="ready"):
    pal = net_mod.Palette("")
    snap = net_mod.demo_snapshot(state)
    buf = bytearray(w * h * 4)
    net_mod.render_panel(buf, w, h, snap, pal, 1.0, hover, rescan)
    return bytes(buf)


class Renderers(unittest.TestCase):
    def test_tile_states_are_byte_stable_and_distinct(self):
        faces = {}
        for state in ("wifi:88", "wifi:30", "wired", "down", "unavailable"):
            a, b = tile_bytes(state), tile_bytes(state)
            self.assertEqual(a, b, f"{state} must render deterministically")
            faces[state] = a
        vals = list(faces.values())
        self.assertEqual(len(vals), len(set(vals)),
                         "every state face must be distinguishable")

    def test_signal_strength_changes_the_face(self):
        self.assertNotEqual(tile_bytes("wifi:88"), tile_bytes("wifi:55"))
        self.assertEqual(net_mod.signal_arcs(88), 3)
        self.assertEqual(net_mod.signal_arcs(55), 2)
        self.assertEqual(net_mod.signal_arcs(30), 1)
        self.assertEqual(net_mod.signal_arcs(10), 0)

    def test_panel_is_byte_stable_and_hover_repaints(self):
        a, b = panel_bytes("wifi:88"), panel_bytes("wifi:88")
        self.assertEqual(a, b)
        self.assertNotEqual(a, panel_bytes("wifi:88", hover=1))
        self.assertNotEqual(a, panel_bytes("wifi:88", rescan="scanning"))

    def test_panel_hitboxes_cover_networks_and_rescan(self):
        pal = net_mod.Palette("")
        snap = net_mod.demo_snapshot("wifi:88")
        buf = bytearray(700 * 500 * 4)
        boxes = net_mod.render_panel(buf, 700, 500, snap, pal, 1.0)
        kinds = [b[2] for b in boxes]
        self.assertEqual(kinds[-1], ("rescan",))
        self.assertEqual(len([k for k in kinds if k[0] == "net"]),
                         len(snap.networks))
        for y0, y1, _t in boxes:
            self.assertLess(y0, y1)


# ---------------------------------------------------------------------
# The v2 panel wire, as the SDK speaks it for this dockapp.
# ---------------------------------------------------------------------

class PanelWire(unittest.TestCase):
    def test_request_scales_and_clamps_to_the_edge_cap(self):
        self.assertEqual(net_mod.panel_request_px(1.0), (700, 500))
        self.assertEqual(net_mod.panel_request_px(2.0), (1024, 1000),
                         "the 1024/edge cap clamps the wide axis")
        for w, h in (net_mod.panel_request_px(s / 4) for s in range(2, 33)):
            self.assertTrue(0 < w <= 1024 and 0 < h <= 1024)
            self.assertLessEqual(w * h * 4, 4 * 1024 * 1024,
                                 "every request fits the 4 MiB panel cap")

    def test_band_layout_and_bounds(self):
        px = b"\x00" * (8 * 3 * 4)
        msg = cwire.encode_panel_frame(7, 40, 3, 8, px)
        self.assertEqual(msg[:20],
                         struct.pack("<B3xIIII", 0x06, 7, 40, 3, 8))
        self.assertEqual(msg[20:], px)
        with self.assertRaises(Exception):
            cwire.encode_panel_frame(7, 40, 3, 8, px + b"\x00")
        w = 1024
        too_tall = cwire.panel_band_rows(w) + 1
        with self.assertRaises(Exception):
            cwire.encode_panel_frame(0, 0, too_tall, w,
                                     b"\x00" * (w * too_tall * 4))

    def test_band_rows_fit_the_transport(self):
        for w in (56, 640, 700, 1024):
            rows = cwire.panel_band_rows(w)
            self.assertGreater(rows, 0)
            self.assertLessEqual(rows * w * 4, cwire.MAX_FRAME_BYTES,
                                 "every SDK band fits one datagram")

    def test_decode_panel_opened_closed_input(self):
        self.assertEqual(
            cwire.decode_server(struct.pack("<B3xII", 0x87, 640, 480)),
            ("panel_opened", (640, 480)))
        # v2 PanelClosed: reason u8 padded to the 4-byte grain.
        self.assertEqual(
            cwire.decode_server(struct.pack("<B3xB3x", 0x88, 1)),
            ("panel_closed", 1))
        with self.assertRaises(Exception):
            cwire.decode_server(struct.pack("<B3xB3x", 0x88, 9))
        with self.assertRaises(Exception):  # nonzero pad
            cwire.decode_server(struct.pack("<B3xBBBB", 0x88, 1, 0, 0, 7))
        name, ev = cwire.decode_server(
            struct.pack("<B3xBBHiii", 0x89, 4, 0, 0, 10, 20, 0))
        self.assertEqual(name, "panel_input")
        self.assertEqual((ev.kind, ev.x, ev.y), (4, 10, 20))

    def test_motion_is_a_panel_only_input_kind(self):
        # v2.1: kind 6 = Motion decodes on PanelInput (0x89)...
        name, ev = cwire.decode_server(
            struct.pack("<B3xBBHiii", 0x89, 6, 0, 0, 33, 44, 0))
        self.assertEqual(name, "panel_input")
        self.assertEqual((ev.kind, ev.button, ev.x, ev.y), (6, 0, 33, 44))
        # ...and stays undefined on the tile's Input (0x83).
        with self.assertRaises(Exception):
            cwire.decode_server(
                struct.pack("<B3xBBHiii", 0x83, 6, 0, 0, 33, 44, 0))

    def test_welcome_carries_the_version_advert(self):
        for advertised, expect in ((2, 2), (0, 1), (1, 1), (7, 7)):
            name, state = cwire.decode_server(enc_welcome(version=advertised))
            self.assertEqual(name, "welcome")
            self.assertEqual(state.proto, expect)
            self.assertEqual(state.tile_px, TILE)

    def test_a_frame_before_the_grant_is_unrepresentable(self):
        from chonkdock import Panel, PanelError
        panel = Panel(None, None, 700, 500)  # requested, never granted
        self.assertFalse(panel.opened)
        with self.assertRaises(PanelError):
            panel.draw(b"\x00" * (700 * 500 * 4))
        with self.assertRaises(PanelError):
            panel.draw_rows(0, b"\x00" * (700 * 4))


# ---------------------------------------------------------------------
# The fake shell.
# ---------------------------------------------------------------------

def enc_welcome(tile_px=TILE, scale=1.0, theme_id="nextstep-classic",
                toml="", version=2):
    """v2 Welcome: the u16 that was reserved now advertises the
    shell's protocol version (0 = an old v1 shell's zeros)."""
    ident, body = theme_id.encode(), toml.encode()
    scale_bits = struct.unpack("<I", struct.pack("<f", scale))[0]
    return (struct.pack("<B3x", 0x81)
            + struct.pack("<IIHHI", tile_px, scale_bits, len(ident),
                          version, len(body))
            + ident + body)


def enc_input(kind, button, x, y, delta=0):
    return struct.pack("<B3xBBHiii", 0x83, kind, button, 0, x, y, delta)


def enc_panel_input(kind, button, x, y, delta=0):
    return struct.pack("<B3xBBHiii", 0x89, kind, button, 0, x, y, delta)


def enc_panel_opened(w, h):
    return struct.pack("<B3xII", 0x87, w, h)


def enc_panel_closed(reason):
    # v2: reason u8 padded to the 4-byte grain with reserved zeros.
    return struct.pack("<B3xB3x", 0x88, reason)


def enc_ping(seq):
    return struct.pack("<B3xI", 0x85, seq)


def dec_client(buf):
    kind = buf[0]
    assert buf[1:4] == b"\x00\x00\x00", "reserved header bytes must be zero"
    body = buf[4:]
    if kind == 0x01:
        proto, units, wants, id_len = struct.unpack_from("<IBBBx", body, 0)
        token = body[8:24]
        ident = body[24:24 + id_len].decode()
        assert len(body) == 24 + id_len
        return ("hello", (proto, units, wants, token, ident))
    if kind == 0x02:
        gen, w, h = struct.unpack_from("<III", body, 0)
        pixels = body[12:]
        assert len(pixels) == w * h * 4
        return ("frame", (gen, w, h, pixels))
    if kind == 0x03:
        return ("pong", struct.unpack("<I", body)[0])
    if kind == 0x04:
        level, text_len = struct.unpack_from("<BxH", body, 0)
        return ("log", (level, body[4:4 + text_len].decode()))
    if kind == 0x05:
        assert len(body) == 8, "OpenPanel is two u32s"
        return ("open_panel", struct.unpack("<II", body))
    if kind == 0x06:
        gen, y, band_h, w = struct.unpack_from("<IIII", body, 0)
        pixels = body[16:]
        assert len(pixels) == w * band_h * 4, "band length must match"
        assert len(pixels) <= 262080, "a band never exceeds MAX_FRAME_BYTES"
        assert w <= 1024 and y + band_h <= 1024
        return ("panel_band", (gen, y, band_h, w, pixels))
    if kind == 0x07:
        assert len(body) == 0, "ClosePanel is bare"
        return ("close_panel", None)
    raise AssertionError(f"unexpected client message kind {kind:#x}")


FAKE_FILES = {
    "nm_running": "running\n",
    "nm_devices": NM_DEVICES_WIFI,
    "nm_wifi_list": NM_WIFI_LIST,
    "nm_wifi_rescan": NM_WIFI_LIST + " :fresh-after-rescan:52:5745 MHz:433 Mbit/s:WPA2\n",
    "iw_link": IW_LINK_CONNECTED,
    "nm_ip4": NM_IP4,
}

RECV_MAX = 4 * 1024 * 1024 + 64


class Harness(unittest.TestCase):
    """One chonk-net process, one fake shell, hermetic fake data."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="chonk-net-test.")
        self.fake = os.path.join(self.tmp.name, "fake")
        os.makedirs(self.fake)
        for key, text in FAKE_FILES.items():
            with open(os.path.join(self.fake, key + ".txt"), "w") as f:
                f.write(text)
        sock_path = os.path.join(self.tmp.name, "dock.sock")
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        self.listener.bind(sock_path)
        self.listener.listen(1)
        for opt in (socket.SO_SNDBUF, socket.SO_RCVBUF):
            try:
                self.listener.setsockopt(socket.SOL_SOCKET, opt, RECV_MAX)
            except OSError:
                pass
        env = dict(os.environ,
                   CHONKSTEP_DOCK_SOCKET=sock_path,
                   CHONKSTEP_DOCK_TOKEN=TOKEN.hex(),
                   CHONKSTEP_SCALE="1.0000",
                   CHONKSTEP_THEME="nextstep-classic",
                   CHONKNET_FAKE_DATA=self.fake)
        env.pop("WAYLAND_DISPLAY", None)
        env.pop("DISPLAY", None)
        self.proc = subprocess.Popen([sys.executable, SCRIPT], env=env,
                                     stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL)
        self.listener.settimeout(5.0)
        self.conn, _ = self.listener.accept()
        self.conn.settimeout(5.0)
        for opt in (socket.SO_SNDBUF, socket.SO_RCVBUF):
            try:
                self.conn.setsockopt(socket.SOL_SOCKET, opt, RECV_MAX)
            except OSError:
                pass

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
            self.conn.recv(RECV_MAX))
        self.assertEqual(name, "hello")
        self.assertEqual(proto, 1)
        self.assertEqual(units, 1)
        self.assertEqual(token, TOKEN)
        self.assertEqual(ident, "chonk-net")
        self.conn.send(enc_welcome())

    def next_message(self, timeout=5.0):
        self.conn.settimeout(timeout)
        return dec_client(self.conn.recv(RECV_MAX))

    def next_of(self, wanted, timeout=5.0):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            name, payload = self.next_message(deadline - time.monotonic())
            if name == wanted:
                return payload
        self.fail(f"no {wanted} arrived in time")

    def assert_none_of(self, banned, window):
        """Drains messages for `window` seconds; none may be `banned`."""
        deadline = time.monotonic() + window
        while time.monotonic() < deadline:
            try:
                name, _ = self.next_message(deadline - time.monotonic())
            except socket.timeout:
                return
            self.assertNotEqual(name, banned)

    def scan_calls(self):
        try:
            with open(os.path.join(self.fake, "calls.log")) as f:
                return [ln for ln in f.read().splitlines()
                        if ln == "nm_wifi_rescan"]
        except OSError:
            return []

    def collect_repaint(self, grant_w, grant_h, image=None, timeout=5.0):
        """Collects v2 bands into a full `grant_w` x `grant_h` image.
        With no `image` (a first paint) it insists on full coverage by
        one generation of top-to-bottom bands; with one, bands patch it
        (a partial repaint) until the wire goes quiet."""
        stride = grant_w * 4
        first_paint = image is None
        if first_paint:
            image = bytearray(grant_w * grant_h * 4)
        covered = 0
        gens = set()
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                name, payload = self.next_message(
                    min(deadline - time.monotonic(),
                        0.6 if (not first_paint or covered >= grant_h)
                        else 5.0))
            except socket.timeout:
                break
            if name != "panel_band":
                continue
            gen, y, band_h, w, pixels = payload
            self.assertEqual(w, grant_w,
                             "every band matches the granted width")
            self.assertLessEqual(y + band_h, grant_h,
                                 "no band reaches past the granted height")
            gens.add(gen)
            image[y * stride:(y + band_h) * stride] = pixels
            covered += band_h
            if first_paint and covered >= grant_h and len(gens) == 1:
                # full coverage in one generation: the paint is whole
                return image
        if first_paint:
            self.assertGreaterEqual(covered, grant_h,
                                    "a first paint must cover the panel")
            self.assertEqual(len(gens), 1,
                             "a full repaint shares one generation")
        return image

    def open_panel(self, grant_w=700, grant_h=500):
        """Clicks the tile, checks the request, grants, returns the
        first full panel image."""
        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))
        w, h = self.next_of("open_panel")
        self.assertEqual((w, h), (700, 500),
                         "the request at scale 1 is the logical size")
        self.conn.send(enc_panel_opened(grant_w, grant_h))
        return self.collect_repaint(grant_w, grant_h)

    # -- the tests -----------------------------------------------------

    def test_handshake_then_a_byte_exact_tile_frame(self):
        self.handshake()
        _, w, h, pixels = self.next_of("frame")
        self.assertEqual((w, h), (TILE, TILE))
        # The same fake data through the same code paths, locally.
        os.environ[netdata.FAKE_DATA_ENV] = self.fake
        try:
            snap = netdata.sample(backend="networkmanager")
        finally:
            del os.environ[netdata.FAKE_DATA_ENV]
        expected = bytearray(TILE * TILE * 4)
        net_mod.render_tile(expected, TILE, snap.link,
                            net_mod.Palette(""), 1.0)
        self.assertEqual(pixels, bytes(expected),
                         "the wifi tile face, to the byte")

    def test_a_v1_shell_gets_a_tile_only_chonk_net(self):
        # An old shell: Welcome with the reserved field still zero.
        name, _ = dec_client(self.conn.recv(RECV_MAX))
        self.assertEqual(name, "hello")
        self.conn.send(enc_welcome(version=0))
        self.next_of("frame")
        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))
        # No OpenPanel may ever be attempted against a v1 shell.
        self.assert_none_of("open_panel", 1.5)

    def test_no_panel_band_before_the_grant_ever(self):
        self.handshake()
        self.next_of("frame")
        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))
        self.next_of("open_panel")
        # The shell says nothing. Nothing panel-shaped may arrive.
        self.assert_none_of("panel_band", 1.5)

    def test_first_paint_is_byte_exact_and_banded(self):
        self.handshake()
        self.next_of("frame")
        image = self.open_panel()  # collect_repaint asserts the banding
        os.environ[netdata.FAKE_DATA_ENV] = self.fake
        try:
            snap = netdata.sample(backend="networkmanager")
        finally:
            del os.environ[netdata.FAKE_DATA_ENV]
        expected = bytearray(700 * 500 * 4)
        net_mod.render_panel(expected, 700, 500, snap, net_mod.Palette(""),
                             1.0, None, "ready", None)
        self.assertEqual(bytes(image), bytes(expected),
                         "the assembled bands are the panel, to the byte")

    def test_grant_clamped_and_bands_match_it_exactly(self):
        self.handshake()
        self.next_of("frame")
        image = self.open_panel(grant_w=640, grant_h=416)
        self.assertEqual(len(image), 640 * 416 * 4,
                         "bands cover exactly the clamped grant")

    def test_hover_rides_motion_and_repaints_only_what_changed(self):
        self.handshake()
        self.next_of("frame")
        first = bytes(self.open_panel())
        # v2.1 Motion (kind 6, button 0) over the second list row
        # (rows start ~100 device px): the app must neither error nor
        # disconnect, and must ship a partial repaint.
        self.conn.send(enc_panel_input(6, 0, 300, 145))
        patched = self.collect_repaint(700, 500, bytearray(first))
        self.assertNotEqual(bytes(patched), first, "hover must show")
        # Motion to another row moves the highlight again — the
        # connection is demonstrably still healthy after kind 6.
        self.conn.send(enc_panel_input(6, 0, 300, 115))
        moved = self.collect_repaint(700, 500, bytearray(patched))
        self.assertNotEqual(bytes(moved), bytes(patched))
        self.conn.send(enc_ping(0x600D))
        self.assertEqual(self.next_of("pong"), 0x600D,
                         "still connected after Motion input")

    def test_hover_falls_back_to_enter_when_motion_is_scarce(self):
        # A shell that throttles motion hard still highlights: Enter
        # alone must move the hover.
        self.handshake()
        self.next_of("frame")
        first = bytes(self.open_panel())
        self.conn.send(enc_panel_input(4, 0, 300, 145))  # Enter
        patched = self.collect_repaint(700, 500, bytearray(first))
        self.assertNotEqual(bytes(patched), first, "hover must show")

    def test_rescan_row_runs_one_whitelisted_scan_rate_limited(self):
        self.handshake()
        self.next_of("frame")
        self.open_panel()
        self.assertEqual(self.scan_calls(), [], "no scan before the click")
        # The rescan row lives in the bottom band of the panel.
        self.conn.send(enc_panel_input(1, 1, 350, 480))  # press
        self.conn.send(enc_panel_input(2, 0, 350, 480))  # release
        deadline = time.monotonic() + 5.0
        while not self.scan_calls():
            self.assertLess(time.monotonic(), deadline, "no rescan ran")
            time.sleep(0.05)
        self.assertEqual(len(self.scan_calls()), 1)
        # A second click inside the rate window must not scan again.
        self.conn.send(enc_panel_input(1, 1, 350, 480))
        self.conn.send(enc_panel_input(2, 0, 350, 480))
        time.sleep(1.0)
        self.assertEqual(len(self.scan_calls()), 1,
                         "the gate holds: one scan per interval")

    def test_click_on_a_network_row_is_inert(self):
        self.handshake()
        self.next_of("frame")
        self.open_panel()
        before = self.scan_calls()
        self.conn.send(enc_panel_input(1, 1, 300, 110))
        self.conn.send(enc_panel_input(2, 0, 300, 110))
        time.sleep(0.5)
        self.assertEqual(self.scan_calls(), before,
                         "read-only: clicking a network does nothing")

    def test_dismissal_resets_and_reopen_works(self):
        self.handshake()
        self.next_of("frame")
        self.open_panel()
        self.conn.send(enc_panel_closed(1))  # user dismissed
        time.sleep(0.3)
        # Clicking the tile again must be a fresh OpenPanel, and no
        # stray PanelFrame may arrive before the new grant.
        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))
        w, h = self.next_of("open_panel")
        self.assertEqual((w, h), (700, 500))
        self.assert_none_of("panel_band", 1.0)
        self.conn.send(enc_panel_opened(700, 500))
        self.collect_repaint(700, 500)

    def test_refusal_is_accepted_quietly(self):
        self.handshake()
        self.next_of("frame")
        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))
        self.next_of("open_panel")
        self.conn.send(enc_panel_closed(3))  # refused
        self.assert_none_of("panel_band", 1.0)
        # And the state machine recovered: a new click asks again.
        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))
        self.next_of("open_panel")

    def test_second_tile_click_closes_the_panel(self):
        self.handshake()
        self.next_of("frame")
        self.open_panel()
        self.conn.send(enc_input(1, 1, TILE // 2, TILE // 2))
        self.next_of("close_panel")
        self.assert_none_of("panel_band", 1.0)

    def test_pings_are_answered(self):
        self.handshake()
        self.next_of("frame")
        self.conn.send(enc_ping(0xBEEF))
        self.assertEqual(self.next_of("pong"), 0xBEEF)


if __name__ == "__main__":
    unittest.main()
