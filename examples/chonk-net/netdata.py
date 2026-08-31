"""The chonk-net data layer: what this machine's network is doing.

Pure read-only sampling. Every fact on the tile and in the panel comes
through here, and everything here is either a file read under
`/sys/class/net` or one of the commands in the frozen `COMMANDS` table
below — a table of read-only argv arrays that nothing at runtime can
extend or edit. That table is the read-only guarantee in code rather
than in a comment: `run_command` is the only subprocess call site in
this dockapp, it refuses any key not in the table, and the tests walk
the table asserting no verb in it can change network state.

Backends, detected rather than assumed:

- **NetworkManager** (primary): `nmcli -t` terse output is the stable
  parse surface — machine-readable colon-separated lines with `\\:`
  escaping, documented as such in nmcli(1).
- **iwd**: recognized (so the UI can say so honestly), with the current
  link read via `iw dev <if> link`; iwd's scan list is not spoken in
  this iteration and shows as unavailable rather than pretending.
- **neither**: wired facts still come straight from sysfs
  (`carrier`, `speed`), and everything else reads "unavailable".

Every parser tolerates its tool being absent or its output being
garbage, and degrades to an honest "unavailable" value the renderer
shows as such — never a crash, never a guess.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import time
import types
from dataclasses import dataclass, field, replace

# ---------------------------------------------------------------------
# The frozen command table — the whole vocabulary this dockapp can
# speak to the system. Read-only by construction: `status`, `list`,
# `show`, `link` — no connect, no disconnect, no radio, no set. The
# one entry with a side effect is `nm_wifi_rescan`, which asks the
# supplicant to *look* (a scan), never to join; it runs only from the
# rate-limited rescan row.
#
# `{ifname}` is the single substitution point, and `run_command`
# validates the value against IFNAME_RE before it goes anywhere near
# an argv. The table is a MappingProxyType over tuples: immutable at
# runtime, so "the code can only run these" is checkable by reading
# this one screen.
# ---------------------------------------------------------------------

COMMANDS: types.MappingProxyType = types.MappingProxyType({
    "nm_running": ("nmcli", "-t", "-f", "RUNNING", "general"),
    "nm_devices": ("nmcli", "-t", "-f", "DEVICE,TYPE,STATE,CONNECTION",
                   "device", "status"),
    "nm_wifi_list": ("nmcli", "-t", "-f",
                     "IN-USE,SSID,SIGNAL,FREQ,RATE,SECURITY",
                     "dev", "wifi", "list", "--rescan", "no"),
    "nm_wifi_rescan": ("nmcli", "-t", "-f",
                       "IN-USE,SSID,SIGNAL,FREQ,RATE,SECURITY",
                       "dev", "wifi", "list", "--rescan", "yes"),
    "nm_ip4": ("nmcli", "-t", "-f", "IP4.ADDRESS", "device", "show",
               "{ifname}"),
    "iw_link": ("iw", "dev", "{ifname}", "link"),
})

IFNAME_RE = re.compile(r"^[A-Za-z0-9._-]{1,15}$")

#: Explicit rescans (the panel's rescan row) at most this often.
RESCAN_MIN_INTERVAL = 15.0

#: Environment variable naming a directory of canned command outputs;
#: when set, `run_command` reads `<dir>/<key>.txt` instead of executing
#: anything, and appends the key to `<dir>/calls.log`. This is how the
#: protocol tests drive the real process without touching the real
#: network — and it is *more* restrictive than the table, not less.
FAKE_DATA_ENV = "CHONKNET_FAKE_DATA"


def run_command(key: str, ifname: str | None = None,
                timeout: float = 8.0) -> str | None:
    """The only subprocess call site. Returns the command's stdout,
    or None when the tool is absent, fails, or times out — the callers
    all treat None as "this fact is unavailable"."""
    if key not in COMMANDS:
        raise KeyError(f"not a whitelisted command: {key!r}")
    argv = []
    for part in COMMANDS[key]:
        if part == "{ifname}":
            if ifname is None or not IFNAME_RE.match(ifname):
                raise ValueError(f"not an interface name: {ifname!r}")
            part = ifname
        argv.append(part)

    fake_dir = os.environ.get(FAKE_DATA_ENV)
    if fake_dir:
        try:
            with open(os.path.join(fake_dir, "calls.log"), "a",
                      encoding="utf-8") as f:
                f.write(key + "\n")
            with open(os.path.join(fake_dir, key + ".txt"),
                      encoding="utf-8") as f:
                return f.read()
        except OSError:
            return None

    try:
        proc = subprocess.run(argv, capture_output=True, text=True,
                              timeout=timeout, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout


# ---------------------------------------------------------------------
# The model
# ---------------------------------------------------------------------

@dataclass(frozen=True)
class Link:
    """What the tile shows: the machine's primary link."""

    kind: str = "none"  # "wifi" | "wired" | "none" | "unavailable"
    ifname: str = ""
    ssid: str = ""
    signal: int = 0          # 0-100, wifi only
    band: str = ""           # "2.4" | "5" | "6"
    bitrate_mbps: int = 0    # wifi bitrate or wired speed
    security: str = ""
    ip4: str = ""


@dataclass(frozen=True)
class Network:
    """One row of the panel's scan list."""

    ssid: str
    signal: int              # 0-100
    band: str                # "2.4" | "5" | "6" | ""
    security: str            # "" for open
    in_use: bool = False


@dataclass(frozen=True)
class Snapshot:
    """Everything the renderers draw from, in one immutable value."""

    backend: str = "none"    # "networkmanager" | "iwd" | "none"
    link: Link = field(default_factory=Link)
    networks: tuple = ()     # tuple[Network, ...], strongest first
    scan_note: str = ""      # why the list is empty, when it is


# ---------------------------------------------------------------------
# Parsers — pure text in, values out, unit-tested against canned
# output. Terse nmcli escapes `:` and `\` in values with a backslash;
# `split_terse` is the one splitter everything shares.
# ---------------------------------------------------------------------

def split_terse(line: str) -> list[str]:
    fields, cur, esc = [], [], False
    for ch in line:
        if esc:
            cur.append(ch)
            esc = False
        elif ch == "\\":
            esc = True
        elif ch == ":":
            fields.append("".join(cur))
            cur = []
        else:
            cur.append(ch)
    fields.append("".join(cur))
    return fields


def band_of(freq_mhz: int) -> str:
    if 2300 <= freq_mhz < 3000:
        return "2.4"
    if 4900 <= freq_mhz < 5925:
        return "5"
    if 5925 <= freq_mhz < 7200:
        return "6"
    return ""


@dataclass(frozen=True)
class NmDevice:
    device: str
    dev_type: str
    state: str
    connection: str


def parse_nm_devices(text: str) -> list[NmDevice]:
    """`nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device status`.
    Keeps only real NICs: wifi and ethernet, managed. Bridges, tuns,
    veth pairs and the rest of a busy machine's zoo are noise here."""
    out = []
    for line in text.splitlines():
        fields = split_terse(line)
        if len(fields) < 4:
            continue
        dev, dev_type, state = fields[0], fields[1], fields[2]
        conn = ":".join(fields[3:]) if len(fields) > 4 else fields[3]
        if dev_type not in ("wifi", "ethernet"):
            continue
        if state == "unmanaged":
            continue
        out.append(NmDevice(dev, dev_type, state, conn))
    return out


def parse_nm_wifi_list(text: str) -> list[Network]:
    """`nmcli -t -f IN-USE,SSID,SIGNAL,FREQ,RATE,SECURITY dev wifi
    list --rescan no`. Deduplicates by SSID keeping the strongest BSS
    (an in-use BSS always wins its SSID), sorts strongest first with
    the in-use network on top; hidden (empty-SSID) entries dropped."""
    best: dict[str, Network] = {}
    for line in text.splitlines():
        fields = split_terse(line)
        if len(fields) < 6:
            continue
        in_use = fields[0].strip() == "*"
        ssid = fields[1]
        if not ssid:
            continue
        try:
            signal = max(0, min(100, int(fields[2])))
        except ValueError:
            continue
        m = re.match(r"\s*(\d+)", fields[3])
        band = band_of(int(m.group(1))) if m else ""
        security = fields[5].strip()
        if security in ("--", "none"):
            security = ""
        net = Network(ssid=ssid, signal=signal, band=band,
                      security=security, in_use=in_use)
        prev = best.get(ssid)
        if prev is None or (net.in_use and not prev.in_use) or (
                net.in_use == prev.in_use and net.signal > prev.signal):
            best[ssid] = net
    return sorted(best.values(),
                  key=lambda n: (not n.in_use, -n.signal, n.ssid))


@dataclass(frozen=True)
class IwLink:
    connected: bool = False
    ssid: str = ""
    signal_dbm: int | None = None
    bitrate_mbps: int = 0
    freq_mhz: int = 0


def parse_iw_link(text: str) -> IwLink:
    """`iw dev <if> link` — bitrate and frequency for the header;
    'Not connected.' is a valid answer, not an error."""
    if "Connected to" not in text:
        return IwLink()
    ssid = ""
    signal = None
    bitrate = 0
    freq = 0
    for line in text.splitlines():
        line = line.strip()
        if line.startswith("SSID:"):
            ssid = line[5:].strip()
        elif line.startswith("signal:"):
            m = re.search(r"(-?\d+)\s*dBm", line)
            if m:
                signal = int(m.group(1))
        elif line.startswith(("tx bitrate:", "rx bitrate:")):
            m = re.search(r"([\d.]+)\s*MBit/s", line)
            if m and line.startswith("tx"):
                bitrate = int(float(m.group(1)))
            elif m and not bitrate:
                bitrate = int(float(m.group(1)))
        elif line.startswith("freq:"):
            m = re.search(r"(\d+)", line)
            if m:
                freq = int(m.group(1))
    return IwLink(True, ssid, signal, bitrate, freq)


def dbm_to_percent(dbm: int) -> int:
    """The conventional mapping (-100 dBm .. -50 dBm -> 0..100)."""
    return max(0, min(100, 2 * (dbm + 100)))


def parse_nm_ip4(text: str) -> str:
    """`nmcli -t -f IP4.ADDRESS device show <if>` — first address."""
    for line in text.splitlines():
        fields = split_terse(line)
        if len(fields) >= 2 and fields[0].startswith("IP4.ADDRESS"):
            addr = fields[1].strip()
            if addr:
                return addr
    return ""


def read_sysfs_wired(ifname: str, sys_net: str = "/sys/class/net"):
    """(carrier: bool, speed_mbps: int) from sysfs; honest zeros when
    unreadable (speed reads -1 or errors on a down interface)."""
    def read_int(name):
        try:
            with open(os.path.join(sys_net, ifname, name),
                      encoding="ascii") as f:
                return int(f.read().strip())
        except (OSError, ValueError):
            return -1
    carrier = read_int("carrier") == 1
    speed = read_int("speed")
    return carrier, max(0, speed if carrier else 0)


def is_wireless(ifname: str, sys_net: str = "/sys/class/net") -> bool:
    return os.path.isdir(os.path.join(sys_net, ifname, "wireless"))


# ---------------------------------------------------------------------
# Sampling — orchestration over the parsers. `runner` is injectable so
# the unit tests feed canned text with no subprocess anywhere.
# ---------------------------------------------------------------------

def detect_backend(runner=run_command) -> str:
    out = runner("nm_running")
    if out is not None and out.strip() == "running":
        return "networkmanager"
    if shutil.which("iwctl"):
        return "iwd"
    return "none"


def sample(runner=run_command, sys_net: str = "/sys/class/net",
           backend: str | None = None) -> Snapshot:
    """One full read of the world. Cheap: cached scan results only
    (`--rescan no`); an explicit rescan is `rescan()` below."""
    if backend is None:
        backend = detect_backend(runner)
    if backend == "networkmanager":
        return _sample_nm(runner, sys_net)
    if backend == "iwd":
        return _sample_iwd(runner, sys_net)
    return _sample_bare(sys_net)


def _sample_nm(runner, sys_net) -> Snapshot:
    devices_out = runner("nm_devices")
    if devices_out is None:
        return Snapshot(backend="networkmanager",
                        link=Link(kind="unavailable"),
                        scan_note="nmcli is not answering")
    devices = parse_nm_devices(devices_out)
    wifi = next((d for d in devices if d.dev_type == "wifi"), None)
    wired = next((d for d in devices
                  if d.dev_type == "ethernet" and d.state == "connected"),
                 None)

    networks: tuple = ()
    scan_note = ""
    if wifi is not None:
        listing = runner("nm_wifi_list")
        if listing is None:
            scan_note = "scan list unavailable"
        else:
            networks = tuple(parse_nm_wifi_list(listing))
            if not networks:
                scan_note = "no networks in range"
    else:
        scan_note = "no Wi-Fi hardware"

    if wifi is not None and wifi.state.startswith("connected"):
        iw = parse_iw_link(runner("iw_link", wifi.device) or "")
        current = next((n for n in networks if n.in_use), None)
        signal = (current.signal if current else
                  dbm_to_percent(iw.signal_dbm)
                  if iw.signal_dbm is not None else 0)
        link = Link(kind="wifi", ifname=wifi.device,
                    ssid=iw.ssid or (current.ssid if current else
                                     wifi.connection),
                    signal=signal,
                    band=band_of(iw.freq_mhz) or
                    (current.band if current else ""),
                    bitrate_mbps=iw.bitrate_mbps,
                    security=current.security if current else "",
                    ip4=parse_nm_ip4(runner("nm_ip4", wifi.device) or ""))
    elif wired is not None:
        carrier, speed = read_sysfs_wired(wired.device, sys_net)
        link = Link(kind="wired" if carrier else "none",
                    ifname=wired.device, ssid=wired.connection,
                    bitrate_mbps=speed,
                    ip4=parse_nm_ip4(runner("nm_ip4", wired.device) or ""))
    else:
        link = Link(kind="none",
                    ifname=wifi.device if wifi else "")
    return Snapshot(backend="networkmanager", link=link,
                    networks=networks, scan_note=scan_note)


def _sample_iwd(runner, sys_net) -> Snapshot:
    """iwd: the current link honestly, the scan list honestly absent
    (this iteration speaks nmcli for scans)."""
    wifi_if = next((n for n in sorted(_ifnames(sys_net))
                    if is_wireless(n, sys_net)), None)
    if wifi_if is None:
        snap = _sample_bare(sys_net)
        return replace(snap, backend="iwd", scan_note="no Wi-Fi hardware")
    iw = parse_iw_link(runner("iw_link", wifi_if) or "")
    if iw.connected:
        link = Link(kind="wifi", ifname=wifi_if, ssid=iw.ssid,
                    signal=dbm_to_percent(iw.signal_dbm)
                    if iw.signal_dbm is not None else 0,
                    band=band_of(iw.freq_mhz),
                    bitrate_mbps=iw.bitrate_mbps)
    else:
        link = Link(kind="none", ifname=wifi_if)
    return Snapshot(backend="iwd", link=link,
                    scan_note="scan list unavailable under iwd")


def _sample_bare(sys_net) -> Snapshot:
    """No manager at all: sysfs still tells the wired truth."""
    for name in sorted(_ifnames(sys_net)):
        if name == "lo" or is_wireless(name, sys_net):
            continue
        if not os.path.exists(os.path.join(sys_net, name, "device")):
            continue  # virtual: bridges, veths, tunnels
        carrier, speed = read_sysfs_wired(name, sys_net)
        if carrier:
            return Snapshot(backend="none",
                            link=Link(kind="wired", ifname=name,
                                      bitrate_mbps=speed),
                            scan_note="no network manager found")
    return Snapshot(backend="none", link=Link(kind="none"),
                    scan_note="no network manager found")


def _ifnames(sys_net):
    try:
        return os.listdir(sys_net)
    except OSError:
        return []


# ---------------------------------------------------------------------
# The explicit rescan — the panel's one button, rate-limited here so
# no caller can forget to.
# ---------------------------------------------------------------------

class RescanGate:
    """At most one supplicant scan per RESCAN_MIN_INTERVAL. `allow()`
    consumes the budget; `remaining()` is for the button label."""

    def __init__(self, min_interval: float = RESCAN_MIN_INTERVAL,
                 clock=time.monotonic):
        self.min_interval = min_interval
        self._clock = clock
        self._last: float | None = None

    def allow(self) -> bool:
        now = self._clock()
        if self._last is not None and now - self._last < self.min_interval:
            return False
        self._last = now
        return True

    def remaining(self) -> float:
        if self._last is None:
            return 0.0
        return max(0.0, self.min_interval - (self._clock() - self._last))


def rescan(runner=run_command) -> tuple | None:
    """Ask the supplicant to look again; returns the fresh list, or
    None when the tool is unavailable. Callers go through a
    RescanGate first."""
    listing = runner("nm_wifi_rescan")
    if listing is None:
        return None
    return tuple(parse_nm_wifi_list(listing))
