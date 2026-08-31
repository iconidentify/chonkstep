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
#: How long one panel band send may wait for socket space. Unlike tile
#: frames, bands do not supersede each other — a dropped band is a
#: stale stripe, not a skipped frame — so they are sent with a bounded
#: wait instead of drop-on-EAGAIN. A healthy shell drains its socket
#: far faster than this.
PANEL_BAND_SEND_TIMEOUT = 1.0


class DockappError(Exception):
    """The dock connection failed in a way that is not recoverable."""


class Refused(DockappError):
    """The shell declined the connection, or ended it deliberately."""

    def __init__(self, reason: int):
        self.reason = reason
        name = wire.GOODBYE_NAMES.get(reason, str(reason))
        super().__init__(f"the shell closed this dockapp's connection: {name}")


class PanelError(DockappError):
    """The instrument panel was used in a state it cannot be used in
    (drawing before the grant, drawing after a close, opening outside
    the run loop)."""


class Panel:
    """One instrument panel: a larger popup surface the shell places
    near this dockapp's tile. Obtained from :meth:`Dockapp.open_panel`;
    one per dockapp.

    The lifecycle is asynchronous on purpose — the shell answers an
    open request with a *grant* (possibly clamped) or a refusal, and
    can dismiss the panel at any time. The SDK enforces the ordering
    the wire demands: no frame leaves before the grant arrives, and
    every frame is exactly the granted size.

    Attributes you read:

    - ``opened`` — True once the shell's grant has arrived (and the
      panel is not mid-renegotiation or closed).
    - ``width`` / ``height`` — the *granted* size in device pixels
      (None until opened). Draw at this size, never at what you asked.
    - ``requested`` — the ``(width, height)`` last asked for.
    - ``closed`` — True once the panel is gone, for any reason.
    - ``close_reason`` — the PANEL_CLOSED_* code once closed.

    Callbacks you assign (all optional):

    - ``paint(panel, buf) -> bool`` — the sibling of ``Dockapp.draw``:
      called on the redraw cadence once the panel is open, with ``buf``
      a premultiplied-RGBA8 bytearray of ``width * height * 4`` bytes;
      return whether anything changed.
    - ``on_opened(panel)`` — the grant arrived (useful for push-style
      drawing via :meth:`draw`).
    - ``on_input(panel, event)`` — a pointer event in panel-local
      device pixels; return True to request an immediate panel repaint.
    - ``on_closed(panel, reason)`` — the panel is gone; ``reason`` is
      one of the names ``"closed"`` (you asked), ``"dismissed"`` (the
      user clicked away), ``"shutdown"`` (the shell is going away, or
      the connection dropped), ``"refused"`` (the open request was
      declined and the panel never existed).
    """

    def __init__(self, app: "Dockapp", sock, width: int, height: int):
        self._app = app
        self._sock = sock
        self.requested = (width, height)
        self.opened = False
        self.closed = False
        self.width: int | None = None
        self.height: int | None = None
        self.close_reason: int | None = None
        self._closing = False
        self._must_present = False
        self._buf: bytearray | None = None
        self._generation = 0
        self.paint = None
        self.on_opened = None
        self.on_input = None
        self.on_closed = None

    def draw(self, pixels) -> None:
        """Pushes one full repaint, for panels not driven by ``paint``.
        ``pixels`` is premultiplied RGBA8 of exactly ``width * height *
        4`` bytes; the SDK slices it into maximal legal bands and
        streams them under one generation — you never think in bands.
        Raises :class:`PanelError` before the grant has arrived (a
        frame before ``PanelOpened`` is a protocol error the SDK will
        not let you commit) or after the panel closed."""
        self._check_streamable()
        expected = self.width * self.height * 4
        if len(pixels) != expected:
            raise PanelError(
                f"panel frame needs {expected} bytes for the granted "
                f"{self.width}x{self.height}, got {len(pixels)}")
        if not self._stream_bands(0, pixels):
            raise PanelError(
                "the shell stopped taking panel bands (send timed out)")

    def draw_rows(self, y: int, pixels) -> None:
        """Pushes a partial update: rows ``y ..`` of the panel, for
        hover-highlight economy. ``pixels`` is premultiplied RGBA8, a
        whole number of ``width``-wide rows, and ``y`` plus that row
        count must stay within the granted height. Same grant and
        lifecycle rules as :meth:`draw`."""
        self._check_streamable()
        stride = self.width * 4
        if len(pixels) == 0 or len(pixels) % stride:
            raise PanelError(
                f"partial update must be whole {self.width}px rows "
                f"({stride} bytes each), got {len(pixels)} bytes")
        rows = len(pixels) // stride
        if y < 0 or y + rows > self.height:
            raise PanelError(
                f"rows {y}..{y + rows} fall outside the granted height "
                f"{self.height}")
        if not self._stream_bands(y, pixels):
            raise PanelError(
                "the shell stopped taking panel bands (send timed out)")

    def _check_streamable(self) -> None:
        if self.closed:
            raise PanelError("this panel is closed")
        if not self.opened:
            raise PanelError(
                "the shell has not granted this panel yet; frames before "
                "PanelOpened are a protocol error")

    def _stream_bands(self, y: int, pixels) -> bool:
        """One update, streamed top-to-bottom as bands that each fit a
        datagram, sharing one generation. Bands are sent with a bounded
        wait rather than the tile's drop-on-EAGAIN — a dropped band
        would be a stale stripe, not a superseded frame. Returns False
        if the shell stopped taking them; the caller decides whether to
        retry the repaint or raise."""
        stride = self.width * 4
        total_rows = len(pixels) // stride
        per_band = wire.panel_band_rows(self.width)
        self._generation = (self._generation + 1) & 0xFFFFFFFF
        row = 0
        while row < total_rows:
            rows = min(per_band, total_rows - row)
            band = pixels[row * stride:(row + rows) * stride]
            msg = wire.encode_panel_frame(
                self._generation, y + row, rows, self.width, bytes(band))
            try:
                self._sock.settimeout(PANEL_BAND_SEND_TIMEOUT)
                self._sock.send(msg)
            except socket.timeout:
                return False
            finally:
                self._sock.settimeout(None)
            row += rows
        return True

    def close(self) -> None:
        """Asks the shell to take the panel down. ``on_closed`` fires
        with ``"closed"`` when the shell confirms. Safe to call twice."""
        if self.closed or self._closing:
            return
        self._closing = True
        self.opened = False
        try:
            self._sock.send(wire.encode_close_panel())
        except OSError:
            pass


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
        #: The shell's protocol version (from Welcome/ThemeChanged).
        #: 1 has tiles only; instrument panels need 2. Feature-gate any
        #: panel affordance on this rather than finding out the hard
        #: way.
        self.shell_proto = state.proto
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
        self._panel: Panel | None = None
        self._retired: list[Panel] = []  # closed by us, PanelClosed pending
        self._active_sock = None
        self._shell_proto = 1  # what the last Welcome/ThemeChanged said

    @property
    def panel(self) -> Panel | None:
        """The current instrument panel (requested or open), or None."""
        return self._panel

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

    # -- the instrument panel -----------------------------------------

    def open_panel(self, width: int, height: int) -> Panel:
        """Requests an instrument panel of `width` x `height` device
        pixels (at most 1024 per edge). Returns the :class:`Panel`
        handle immediately; the shell's answer arrives later — either a
        grant (``panel.opened`` becomes True at the possibly-clamped
        ``panel.width`` x ``panel.height``) or a refusal
        (``on_closed(panel, "refused")``).

        Called again while the panel is open, this renegotiates its
        size on the same handle; frames pause until the new grant.

        Only callable while the dockapp is running (typically from
        ``on_input``). Note that a shell predating panels treats the
        request as a protocol error and closes the whole connection.
        """
        sock = self._active_sock
        if sock is None:
            raise PanelError(
                "open_panel is only available while the dockapp is running")
        if self._shell_proto < 2:
            # A protocol-1 shell rejects unknown message kinds as a
            # protocol error and closes the whole connection — the tile
            # would die with the panel. Refuse locally instead.
            raise PanelError(
                "this shell predates instrument panels (protocol "
                f"{self._shell_proto}); open_panel needs a protocol-2 "
                "shell")
        if not wire.panel_fits(width, height):
            raise PanelError(
                f"panel geometry {width}x{height} is out of range "
                f"(at most {wire.MAX_PANEL_PX} per edge)")
        panel = self._panel
        if panel is not None and panel.closed:
            panel = None
        if panel is not None and panel._closing:
            # The old panel's PanelClosed is still in flight; SEQPACKET
            # ordering guarantees it arrives before the new grant, so
            # park it and attribute the next PanelClosed to it.
            self._retired.append(panel)
            panel = None
        if panel is None:
            panel = Panel(self, sock, width, height)
            self._panel = panel
        else:
            # Renegotiation: same handle, frames blocked until the
            # fresh grant — a frame at the old size could otherwise
            # race the shell's re-grant and be rejected as mismatched.
            panel.requested = (width, height)
            panel.opened = False
        sock.send(wire.encode_open_panel(width, height))
        return panel

    def _grant_panel(self, size) -> None:
        panel = self._panel
        if panel is None or panel.closed or panel._closing:
            return  # a grant that crossed our ClosePanel; already gone
        panel.width, panel.height = size
        panel.opened = True
        panel._buf = bytearray(size[0] * size[1] * 4)
        panel._must_present = True  # the shell has nothing to show yet
        if panel.on_opened is not None:
            panel.on_opened(panel)

    def _finish_panel(self, reason: int) -> None:
        if self._retired:
            panel = self._retired.pop(0)
        else:
            panel = self._panel
            self._panel = None
        if panel is None or panel.closed:
            return
        panel.closed = True
        panel.opened = False
        panel.close_reason = reason
        if panel.on_closed is not None:
            panel.on_closed(panel, wire.PANEL_CLOSED_NAMES[reason])

    def _drop_panel(self) -> None:
        """The connection is gone; whatever panel it carried is too.
        The shell could not tell us, so synthesize the shutdown close
        locally — a dockapp should not have to special-case an EOF to
        learn its panel died."""
        while self._retired:
            # These were closed by us; only the confirmation was lost.
            self._finish_panel(wire.PANEL_CLOSED_CLIENT)
        if self._panel is not None:
            reason = (wire.PANEL_CLOSED_CLIENT if self._panel._closing
                      else wire.PANEL_CLOSED_SHUTDOWN)
            self._finish_panel(reason)

    def _paint_panel(self, panel: Panel) -> None:
        if panel.paint is not None:
            changed = panel.paint(panel, panel._buf)
            if changed or panel._must_present:
                if not panel._stream_bands(0, panel._buf):
                    panel._must_present = True  # retry the whole repaint
                    return
        panel._must_present = False

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
            if outcome == "retheme":
                # Same connection, new palette: the panel (if any)
                # stays open across a retheme.
                state, visible = payload
                continue
            self._drop_panel()  # every other outcome loses the panel
            self._active_sock = None
            if outcome == "shutdown":
                sock.close()
                return
            if outcome == "refused":
                sock.close()
                raise Refused(payload)
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
        self._active_sock = sock
        self._shell_proto = state.proto
        ctx = Ctx(state, self.tile_units, sock, visible)
        buf = bytearray(ctx.tile_px * ctx.height * 4)
        self.on_theme(ctx)

        generation = 0
        must_present = True  # the shell has nothing to show until frame one
        next_draw = time.monotonic()
        while True:
            now = time.monotonic()
            due = now >= next_draw
            if ctx.visible and (must_present or due):
                changed = self.draw(ctx, buf)
                if changed or must_present:
                    generation = (generation + 1) & 0xFFFFFFFF
                    self._present(sock, generation, ctx, buf)
                must_present = False
                next_draw = now + self.redraw_interval
            # The panel paints on the same cadence as the tile — a
            # sibling, not a second clock. It is not gated on tile
            # visibility: an open panel is on screen by definition.
            panel = self._panel
            if panel is not None and panel.opened and (panel._must_present
                                                      or due):
                self._paint_panel(panel)
                if due and not ctx.visible:
                    next_draw = now + self.redraw_interval

            wake = ctx.visible or (panel is not None and panel.opened)
            deadline = next_draw if wake else (
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
            elif name == "panel_opened":
                self._grant_panel(payload)
            elif name == "panel_closed":
                self._finish_panel(payload)
            elif name == "panel_input":
                panel = self._panel
                if (panel is not None and panel.opened
                        and panel.on_input is not None):
                    if panel.on_input(panel, payload):
                        panel._must_present = True
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
