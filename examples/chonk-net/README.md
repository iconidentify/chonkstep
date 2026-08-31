# chonk-net

The network instrument, and the showcase for the dock's *panel*
concept. One tile shows the machine's live link — Wi-Fi as a fan of
signal arcs carved into a sunken display well and lit by strength, a
wired link as a machined RJ45 glyph, a red-slashed dead face for no
link, and an honest engraved `?` when the tools to ask are missing.
A green or red status lamp on the well floor gives the one-glance
answer from across the room.

Click the tile and a **detail panel** (~700x500 device pixels,
clamped by the shell as it sees fit) opens beside it: the current
connection's SSID, band, bitrate and address in a chiseled header;
the networks in range sorted by signal, each row carrying signal
bars, the band tag (2.4 / 5 / 6), a padlock for secured networks and
the raw signal number; hover highlights the row under the pointer;
and a RESCAN row at the bottom. Click the tile again — or anywhere
the shell treats as "away" — and the panel is gone.

**Read-only in this iteration.** chonk-net never joins, leaves, or
reconfigures a network, and it holds no secrets — it cannot even
express such a command: every fact it shows comes through a frozen
whitelist of read commands (`netdata.COMMANDS`: `nmcli` terse
queries, `iw dev <if> link`, sysfs reads), the one subprocess call
site refuses anything off that table, and the tests assert all of
this structurally. The single side effect it can cause is asking the
supplicant to *look* — the RESCAN row runs `nmcli dev wifi list
--rescan yes`, rate-limited to once per 15 seconds; the periodic
refresh only ever reads the cached scan list (`--rescan no`).

## What it reads

Backends are detected, not assumed:

- **NetworkManager** (primary): device state and scan results from
  `nmcli -t` — terse mode is nmcli's documented machine-readable
  surface — plus `iw dev <if> link` for the live bitrate and
  `IP4.ADDRESS` for the header.
- **iwd**: recognized honestly; the current link comes from `iw`,
  and the scan list shows as unavailable (this iteration speaks
  nmcli for scans) rather than pretending.
- **Neither**: wired carrier and speed straight from
  `/sys/class/net/<if>/{carrier,speed}`; everything else reads
  "unavailable".

Every parser tolerates its tool being absent or hostile output, and
degrades to a state the renderer shows as such — never a crash,
never a guess.

## The wire it speaks

Both halves ride the Python SDK (`bindings/python/chonkdock`, stdlib
only): the tile through the classic `Dockapp` callbacks, the panel
through the SDK's panel API — `open_panel()` returning a `Panel`
whose `paint` / `on_input` / `on_closed` callbacks this dockapp
fills in. Underneath, that is the v2 panel wire: `OpenPanel {w, h}`
answered by `PanelOpened` (a grant, possibly clamped) or `PanelClosed
{refused}`; frames go up as **bands** — `PanelFrame {generation, y,
band_height, width, pixels}`, each fitting the 256 KiB transport, a
full repaint being a top-to-bottom run of bands sharing one
generation (the SDK slices it) and a hover change shipping only the
rows that differ (`Panel.draw_rows`). A shell that still speaks v1
(no version advert in its Welcome) gets a tile-only chonk-net: one
log line, no OpenPanel, never a protocol error. And no band is ever
sent before the grant — the SDK's panel object cannot express one.

## Install

From a chonkstep checkout:

```
$ scripts/chonk-get install examples/chonk-net
```

`build.sh` vendors the `chonkdock` SDK next to the script so the
installed copy is self-contained; the tile appears at the next shell
restart. Or register it in place by copying `chonk-net.dockapp` into
`~/.config/chonkstep/dockapps/` with the exec path absolutized.

## Look at it without a dock

The renderers run headless — this is how the design was iterated:

```
$ ./chonk-net.py --render tile.png  --what tile  --state wifi:88 --size 112
$ ./chonk-net.py --render tile.png  --what tile  --state wired
$ ./chonk-net.py --render panel.png --what panel --state wifi:88 --hover 2
$ ./chonk-net.py --render panel.png --what panel --state wired --light
```

States: `wifi:<signal>`, `wired`, `down`, `unavailable`; `--light`
for the light-appearance panel; `--rescan scanning|wait:9` and
`--pressed-row N` for the button states.

## Tests

Headless and hermetic: parser units against canned nmcli/iw/sysfs
output (hostile and absent-tool cases included); byte-stable tile and
panel renders; and a fake shell over a real `SOCK_SEQPACKET` socket
driving the real process with `CHONKNET_FAKE_DATA` canning its data
layer — handshake, byte-exact first paints, the never-a-band-before-
the-grant rule, clamped grants, hover repaints, dismissal, refusal,
v1 fallback, and the read-only guarantee down to a source scan for
stray subprocess call sites.

```
$ python3 -m unittest discover examples/chonk-net/tests
```
