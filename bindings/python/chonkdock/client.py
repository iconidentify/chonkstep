"""The dockapp client loop: connect, draw, retheme, reconnect.

The Python mirror of ``crates/chonk-ui/src/dockapp.rs``, with the same
shape and the same timings, because the timings are a *contract* with
the shell rather than tuning knobs:

- ``Welcome`` must arrive within 2 seconds of ``Hello`` or the shell is
  wedged and the right move is to exit while the user is looking.
- An EOF is not an error: the shell restarts without saying goodbye,
  holds this slot open for 10 seconds, and readopts the survivor that
  reconnects to the same (stable) socket path. So the client retries
  for exactly 10 seconds — 100 ms doubling to a 1 s cap — and exits if
  the shell has not come back, at which point the shell's registry will
  relaunch it anyway.
- ``Goodbye { Shutdown }`` is a clean exit; every other Goodbye is an
  error worth reporting.

Usage::

    from chonkdock import Dockapp

    class MyTile(Dockapp):
        def draw(self, ctx, buf):
            # paint `buf` (premultiplied RGBA8, ctx.width x ctx.height,
            # top row first); return whether anything changed
            return True

    MyTile("my-tile-id").run()
"""

from __future__ import annotations

import os
import socket
import time

from . import wire

ENV_SOCKET = "CHONKSTEP_DOCK_SOCKET"
ENV_TOKEN = "CHONKSTEP_DOCK_TOKEN"

HANDSHAKE_TIMEOUT = 2.0
RECONNECT_WINDOW = 10.0
RECONNECT_FIRST_DELAY = 0.1
RECONNECT_MAX_DELAY = 1.0
DEFAULT_REDRAW_INTERVAL = 1.0


class DockappError(Exception):
    """The dock connection failed in a way that is not recoverable."""


class Refused(DockappError):
    """The shell declined the connection, or ended it deliberately."""

    def __init__(self, reason: int):
        self.reason = reason
        name = wire.GOODBYE_NAMES.get(reason, str(reason))
        super().__init__(f"the shell closed this dockapp's connection: {name}")


class Ctx:
    """Everything a dockapp is told about its surroundings.

    Rebuilt whenever the shell sends a ``ThemeChanged`` — which is how a
    theme switch or a scale change reaches a dockapp *without
    restarting it*.
    """

    def __init__(self, state: wire.ThemeState, tile_units: int, sock,
                 visible: bool):
        #: Device pixels along one tile edge; the frame's width.
        self.tile_px = state.tile_px
        self.tile_units = tile_units
        #: The frame's height: ``tile_px * tile_units``.
        self.height = state.tile_px * tile_units
        #: The session's scale factor, for sizing hand-drawn geometry.
        self.scale = state.scale
        #: The active theme id (e.g. ``"nextstep-classic"``).
        self.theme_id = state.theme_id
        #: The serialized theme table (TOML text; may be empty). Parse
        #: it if you want the real palette; ignore it and pick your own
        #: colors otherwise — wrong colors beat a blank tile.
        self.theme_toml = state.theme_toml
        #: False while the dock is hidden or this tile is scrolled out
        #: of view. Stop sampling as well as drawing.
        self.visible = visible
        self._sock = sock

    def log(self, level: int, text: str) -> None:
        """Says something in the shell's journal (a dockapp's stdout is
        /dev/null). Best-effort: a diagnostic that could block a redraw
        would be worse than no diagnostic."""
        try:
            self._sock.send(wire.encode_log(level, text))
        except OSError:
            pass


class Dockapp:
    """Base class for a dockapp. Subclass and override :meth:`draw`
    (and optionally :meth:`on_input` / :meth:`on_theme`), then call
    :meth:`run`.

    ``tile_units`` is how many stacked square tiles to ask for (1-4).
    ``redraw_interval`` is a ceiling on effort, not a frame rate: a
    ``draw`` that returns False sends nothing.
    """

    def __init__(self, dockapp_id: str, tile_units: int = 1,
                 redraw_interval: float = DEFAULT_REDRAW_INTERVAL,
                 wants: int = wire.WANT_ALL):
        if not wire.is_valid_id(dockapp_id):
            raise ValueError(f"invalid dockapp id {dockapp_id!r}")
        self.id = dockapp_id
        self.tile_units = tile_units
        self.redraw_interval = redraw_interval
        self.wants = wants

    # -- the three callbacks ------------------------------------------

    def draw(self, ctx: Ctx, buf: bytearray) -> bool:
        """Paint the tile into ``buf`` — premultiplied RGBA8, top row
        first, ``ctx.tile_px * ctx.height * 4`` bytes — and return
        whether anything changed. Returning False skips the send, which
        is what keeps a 1 Hz clock at one message per second."""
        raise NotImplementedError

    def on_input(self, ctx: Ctx, event: wire.InputEvent) -> bool:
        """One pointer event in tile-local coordinates; return True to
        request an immediate repaint. The dock reserves middle and
        right click for itself, so only Left and Scroll ever arrive."""
        return False

    def on_theme(self, ctx: Ctx) -> None:
        """The geometry or palette changed (also called once after each
        successful handshake). ``draw`` is called right after with a
        buffer of the new size; override only if you cache theme-derived
        state."""

    # -- plumbing ------------------------------------------------------

    def run(self) -> None:
        """Connects to the dock and serves until the shell says
        Shutdown (returns) or refuses us (raises)."""
        path, token = self._connection_details()
        sock = self._connect(path)
        state = self._handshake(sock, token)
        visible = True
        while True:
            outcome, payload = self._serve(sock, state, visible)
            if outcome == "shutdown":
                return
            if outcome == "refused":
                sock.close()
                raise Refused(payload)
            if outcome == "retheme":
                state, visible = payload
                continue
            # outcome == "disconnected": the stable-path retry loop.
            sock.close()
            sock = self._reconnect(path)
            if sock is None:
                return  # the registry relaunches us when the shell is back
            state = self._handshake(sock, token)
            visible = True  # a fresh shell welcomes a tile it intends to show

    @staticmethod
    def _connection_details():
        path = os.environ.get(ENV_SOCKET)
        if not path:
            raise DockappError(
                f"{ENV_SOCKET} is not set: a dockapp is launched by the "
                "dock, not run from a shell")
        token_hex = os.environ.get(ENV_TOKEN, "")
        try:
            token = bytes.fromhex(token_hex.strip())
        except ValueError:
            token = b""
        if len(token) != wire.TOKEN_BYTES:
            raise DockappError(f"{ENV_TOKEN} is not 32 hex digits")
        return path, token

    @staticmethod
    def _connect(path: str):
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        # Widen the buffers so a whole tile fits in one datagram with
        # room to queue a few; best-effort, exactly like the reference.
        for opt in (socket.SO_SNDBUF, socket.SO_RCVBUF):
            try:
                sock.setsockopt(socket.SOL_SOCKET, opt,
                                2 * wire.MAX_MESSAGE_BYTES)
            except OSError:
                pass
        sock.connect(path)
        return sock

    def _handshake(self, sock, token: bytes) -> wire.ThemeState:
        sock.send(wire.encode_hello(self.id, self.tile_units, token,
                                    self.wants))
        sock.settimeout(HANDSHAKE_TIMEOUT)
        try:
            buf = sock.recv(wire.MAX_MESSAGE_BYTES)
        except socket.timeout:
            raise DockappError("no Welcome from the shell")
        finally:
            sock.settimeout(None)
        if not buf:
            raise DockappError(
                "the shell closed the connection during the handshake")
        name, payload = wire.decode_server(buf)
        if name == "welcome":
            self._check_drawable(payload)
            return payload
        if name == "goodbye":
            raise Refused(payload)
        raise DockappError(f"expected Welcome, got {name}")

    def _check_drawable(self, state: wire.ThemeState) -> None:
        if not wire.frame_fits(state.tile_px, self.tile_units):
            raise DockappError(
                f"the shell sent a {state.tile_px}px tile x "
                f"{self.tile_units} units, which cannot cross the socket")

    def _serve(self, sock, state: wire.ThemeState, visible: bool):
        """One connection's worth of event loop. Returns on a theme
        change so the Ctx and the buffer are rebuilt in one place."""
        self._check_drawable(state)
        ctx = Ctx(state, self.tile_units, sock, visible)
        buf = bytearray(ctx.tile_px * ctx.height * 4)
        self.on_theme(ctx)

        generation = 0
        must_present = True  # the shell has nothing to show until frame one
        next_draw = time.monotonic()
        while True:
            now = time.monotonic()
            if ctx.visible and (must_present or now >= next_draw):
                changed = self.draw(ctx, buf)
                if changed or must_present:
                    generation = (generation + 1) & 0xFFFFFFFF
                    self._present(sock, generation, ctx, buf)
                must_present = False
                next_draw = now + self.redraw_interval

            deadline = next_draw if ctx.visible else (
                time.monotonic() + self.redraw_interval)
            data = self._recv_until(sock, deadline)
            if data is None:
                continue  # timeout: back to the draw check
            if data == b"":
                return ("disconnected", None)
            try:
                name, payload = wire.decode_server(data)
            except wire.DecodeError:
                # The two ends genuinely disagree about the protocol;
                # continuing would be guessing.
                return ("disconnected", None)
            if name in ("welcome", "theme_changed"):
                return ("retheme", (payload, ctx.visible))
            if name == "input":
                if self.on_input(ctx, payload):
                    must_present = True
            elif name == "visibility":
                became_visible = payload and not ctx.visible
                ctx.visible = payload
                must_present = must_present or became_visible
            elif name == "ping":
                self._send(sock, wire.encode_pong(payload))
            elif name == "goodbye":
                if payload == wire.GOODBYE_SHUTDOWN:
                    return ("shutdown", None)
                return ("refused", payload)

    @staticmethod
    def _recv_until(sock, deadline: float):
        """One message, or None if `deadline` passes first. b'' on EOF."""
        remaining = deadline - time.monotonic()
        sock.settimeout(max(remaining, 0.001))
        try:
            return sock.recv(wire.MAX_MESSAGE_BYTES)
        except socket.timeout:
            return None
        except ConnectionResetError:
            return b""
        finally:
            sock.settimeout(None)

    def _present(self, sock, generation: int, ctx: Ctx, buf) -> None:
        self._send(sock, wire.encode_frame(generation, ctx.tile_px,
                                           ctx.height, bytes(buf)))

    @staticmethod
    def _send(sock, message: bytes) -> None:
        """A send that drops rather than blocks: an EAGAIN means the
        shell is momentarily behind, and the next frame supersedes this
        one anyway."""
        try:
            sock.setblocking(False)
            sock.send(message)
        except (BlockingIOError, InterruptedError):
            pass
        finally:
            sock.setblocking(True)

    @staticmethod
    def _reconnect(path: str):
        """Retries connect() against the stable socket path for the
        shell-restart window; None means the window elapsed."""
        deadline = time.monotonic() + RECONNECT_WINDOW
        delay = RECONNECT_FIRST_DELAY
        while time.monotonic() < deadline:
            time.sleep(delay)
            try:
                return Dockapp._connect(path)
            except OSError:
                delay = min(delay * 2, RECONNECT_MAX_DELAY)
        return None
