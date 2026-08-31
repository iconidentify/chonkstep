# The Instrument Platform

chonkstep's dock is not a bar that renders plugins. It is a host for
**instruments**: separate processes that each own a tile (or a small
stack of them), push finished pixels over a private socket, and get the
desktop's theme, scale, input and supervision in return. The flagship
NeXT ideas — the Dock, the Shelf — ship on this platform as ordinary
out-of-process programs, because a tile that can crash, hang, leak or
loop must never be able to take the desktop with it.

You can write one in any language that can open a Unix socket. Here is
the whole thing in Python, stdlib only:

```python
from chonkdock import Dockapp

class Hello(Dockapp):
    def draw(self, ctx, buf):
        for i in range(0, len(buf), 4):
            buf[i:i + 4] = b"\x30\x60\x90\xff"   # one opaque blue tile
        return True

Hello("hello-instrument").run()
```

Install it and it appears in the dock at the next shell restart:

```
$ scripts/chonk-get install ./hello-instrument
```

That's the platform. Everything else is what those ten lines are
standing on.

## Try the shipped ones

```
$ scripts/chonk-get install bindings/python       # a Python clock tile
$ scripts/chonk-get install examples/chonk-shelf  # the Shelf: clipboard history
$ scripts/chonk-get install examples/chonk-switch # the light/dark toggle
$ scripts/chonk-get list
$ scripts/chonk-get remove py-dockclock
```

- `examples/chonk-dockclock` — the Rust reference instrument, built on
  `chonk-ui`'s SDK; the conformance dockapp CI builds on every push.
- `examples/chonk-shelf` — the Shelf as a three-tile clipboard-history
  stack: copy anything, see it appear; click an entry to copy it back.
- `examples/chonk-switch` — the appearance switch: light/dark mode as
  a machined toggle, and the Python SDK's worked example — one
  stdlib-only script, tested headless against a fake shell.
- `bindings/python/chonkdock` and `bindings/go/chonkdock` — complete
  protocol implementations with no dependencies beyond each language's
  standard library, each with a working clock example.
- `docs/dockapp-protocol.md` — every byte of the wire format, for a
  binding in the next language.

## The guarantees, and the mechanisms behind them

Each of these is a testable claim with a named enforcement point, not a
promise. File references are into this repository.

**A dockapp cannot freeze the desktop.** The shell never blocks on a
dockapp — not on send, not on accept, not on a handshake. Every socket
the shell touches is non-blocking three independent ways
(`SOCK_NONBLOCK` at creation, `accept4(SOCK_NONBLOCK)`, `MSG_DONTWAIT`
on every send — `chonk-dock-proto/src/transport.rs`); undeliverable
messages go to a bounded 64-deep queue that drops oldest and
disconnects a peer that stays wedged for 2 s (`queue.rs`); inbound
frames pass a 30 Hz token bucket that coalesces a flood down to its
newest frame. The claim is held by tests, including a thousand repaint
passes against a peer that stopped reading, required to fit in one
16 ms frame (`tests/hostile_peer.rs`), and by a real hostile process
with modes for hanging, flooding, crashing and lying
(`examples/chonk-dockapp-torture`).

**A dockapp cannot crash the desktop, and a crash is survived.** The
tile is a separate process; its death is an EOF on a socket, not a
fault in the compositor. The shell supervises it: relaunch per its
declared policy with exponential backoff, and a hard crash-loop cutoff
— five failures in sixty seconds stops the tile permanently with its
name in the log, because a tile restarted forever is an invisible fork
bomb (`chonk-shell/src/dockapp/tile.rs`, `LaunchBudget`). A hung
dockapp is dimmed after three unanswered 2 s pings so a frozen reading
is never shown as a live one — a courtesy to the user; the desktop was
never at risk.

**A dockapp cannot spy on the session.** It is not a Wayland or X
client — the shell launches it with `WAYLAND_DISPLAY` and `DISPLAY`
*removed* — so screen capture, window enumeration and clipboard
protocols are unreachable rather than denied
(`chonk-shell/src/dockapp/tile.rs`, `launch`). It sees its own tile
size, the theme, and pointer events inside its own tile, in tile-local
coordinates; it is never told where its tile is or what its neighbors
are. It cannot swallow the dock's own gestures either: middle-click
(reorder) and right-click (per-tile menu) are never forwarded. And a
stray process cannot claim a tile: the socket is 0600 in a 0700
directory, the peer's uid is checked with `SO_PEERCRED`, and admission
requires echoing a per-slot 128-bit `getrandom` token
(`handshake.rs`). One honest caveat, stated in the SDK's own docs: a
dockapp is *not sandboxed* — it is your process, with your files.
Install one with the care you'd install any program.

**A hostile dockapp cannot corrupt the shell.** The transport is
`SOCK_SEQPACKET`, so there is no length-prefix parser to exploit — the
kernel keeps message boundaries. Every decoder assumes the bytes came
from a hostile process: unknown kinds, non-zero reserved bytes,
trailing bytes, out-of-range enums, lying frame headers and unusable
floats are rejected, never clamped (`wire.rs`). Log text is stripped of
control characters, bidi overrides and zero-width characters before it
can reach a journal or a text shaper. The codec is held by a
deterministic fuzz harness including every single-byte mutation of
every valid message (`tests/codec_fuzz.rs`).

**Theme and scale changes are pushed live — no restart.** Picking a
theme or changing scale sends one `ThemeChanged` message carrying the
new tile size, scale, theme id and the full serialized palette; the
dockapp rebuilds its buffer on the same connection and keeps its
in-process state (`wire.rs` `ServerMessage::ThemeChanged`; the SDK's
`Outcome::Retheme` in `chonk-ui/src/dockapp.rs`). A frame drawn against
the old geometry is rejected rather than rescaled — the last good frame
stays up until the correctly-sized one arrives, so a resize can never
paint a blurred or cropped tile.

**A dockapp survives shell restarts — better than a Wayland client
can.** The socket path is stable per display, a restarting shell sends
no Goodbye, and the outgoing shell hands each slot's token to its
replacement, which holds the slot open for 10 seconds and *readopts*
the surviving process when it reconnects
(`chonk-shell/src/dockapp/handoff.rs`; the SDK retries for exactly the
same 10 seconds). Updates and restarts of the desktop are invisible to
a tile's internal state. An ordinary Wayland client dies with its
compositor; an instrument does not.

**A rejected dockapp learns why.** Admission failures come back as a
`Goodbye` with a reason — wrong protocol version, bad token, a tile
too large for the transport — at connect time, once, rather than as a
per-frame mystery (`handshake.rs`, `validate_hello`).

## Writing one

1. Read `docs/dockapp-protocol.md`, or skip it and use a binding:
   - Rust: `chonk_ui::dockapp` (`examples/chonk-dockclock` is the
     model; `examples/chonk-shelf` shows multi-tile and input).
   - Python: `bindings/python/chonkdock` (`examples/clock.py`).
   - Go: `bindings/go/chonkdock` (`examples/clock`).
2. Draw premultiplied RGBA into the buffer the SDK hands you; return
   whether anything changed. Update at 1 Hz unless you have a reason —
   the built-ins do.
3. Ship a `.dockapp` registration (id, argv, tile_units, restart
   policy — the format is in the protocol doc, section 9) at the root
   of your repository.
4. Anyone installs it with
   `scripts/chonk-get install <your-git-url>` — clone, build
   (`build.sh`/`Cargo.toml`/`Makefile` are recognized), register.

The conformance bar is `chonk-ui/tests/dockapp_conformance.rs`: it
plays a whole session against the SDK — handshake, first frame,
sanitized logs, ping/pong, a live retheme with a size change, hide and
reveal, a shell restart with reconnect, and a clean shutdown. If your
client can survive that script, it is a citizen of the dock.
