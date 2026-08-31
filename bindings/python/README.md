# chonkdock for Python

The chonkstep dockapp protocol (v1) as a stdlib-only Python package:
wire codec, handshake, and a `Dockapp` base class with the SDK's
reconnect-on-EOF behavior. No pip install, no dependencies — vendoring
the `chonkdock/` directory next to your script is a supported way to
ship.

```python
from chonkdock import Dockapp

class Hello(Dockapp):
    def draw(self, ctx, buf):
        for i in range(0, len(buf), 4):
            buf[i:i + 4] = b"\x30\x60\x90\xff"
        return True

Hello("hello-instrument").run()
```

`ctx` carries `tile_px`, `height`, `scale`, `theme_id`, `theme_toml`
and `visible`; override `on_input` for clicks and scrolls (tile-local
coordinates) and `on_theme` if you cache theme-derived state. `buf` is
premultiplied RGBA8, top row first.

## Instrument panels

A tile is 56 logical pixels of glanceable state. When a click deserves
a real surface — a calendar under a clock, a mixer under a volume tile
— a dockapp may open one *instrument panel*: a larger popup the shell
places near the tile. One panel per dockapp; the shell may clamp your
requested size, dismiss the panel whenever it likes, or refuse it
outright, so the panel API is asynchronous the same way the tile API
already is.

A minimal, runnable panel — a solid themed rectangle that prints
clicks (to the shell's journal, since a dockapp's stdout is
`/dev/null`):

```python
from chonkdock import Dockapp, INPUT_PRESS, LOG_INFO

class Tile(Dockapp):
    def draw(self, ctx, buf):
        for i in range(0, len(buf), 4):
            buf[i:i + 4] = b"\x30\x60\x90\xff"
        return True

    def on_input(self, ctx, event):
        if event.kind != INPUT_PRESS or self.panel is not None:
            return False
        self._ctx = ctx
        panel = self.open_panel(320, 240)   # a request, not a grant
        panel.paint = self.paint_panel      # driven like draw()
        panel.on_input = self.panel_click
        panel.on_closed = lambda p, reason: self._ctx.log(
            LOG_INFO, f"panel went away: {reason}")
        return False

    def paint_panel(self, panel, buf):
        # buf is premultiplied RGBA8, panel.width x panel.height —
        # the *granted* size, which may be smaller than you asked for.
        dark = "dark" in self._ctx.theme_toml  # or parse the real palette
        color = b"\x20\x24\x28\xff" if dark else b"\xc8\xc0\xb0\xff"
        for i in range(0, len(buf), 4):
            buf[i:i + 4] = color
        return True

    def panel_click(self, panel, event):
        if event.kind == INPUT_PRESS:
            self._ctx.log(LOG_INFO, f"panel click at {event.x},{event.y}")
        return False   # True requests an immediate panel repaint

Tile("panel-demo").run()
```

The contract, enforced by the SDK so you cannot protocol-error:

- `open_panel(w, h)` sends the request and returns a `Panel` handle
  immediately. `panel.opened` is False and `panel.width`/`panel.height`
  are None until the shell's grant arrives; `panel.draw(pixels)` before
  that raises `PanelError`, and the `paint` callback is simply not
  called yet. Set `panel.on_opened` if you push frames by hand.
- The grant may be clamped. Always draw at `panel.width x
  panel.height`, never at what you asked for.
- Calling `open_panel` again while the panel is open renegotiates the
  size on the same handle; frames pause until the new grant lands.
- `panel.close()` asks the shell to take it down; `on_closed` fires
  with `"closed"` when the shell confirms. The shell can also close it
  behind your back: `on_closed` gets `"dismissed"` (the user clicked
  away), `"shutdown"` (the shell is going away — also synthesized
  locally if the connection drops), or `"refused"` (the request was
  declined; the panel never opened). After any of these the handle is
  dead — open a fresh one.
- Limits: panels go up to 1024 device pixels per edge, `w * h * 4 <=
  4 MiB` (the shell's total-buffer cap). On the wire a panel frame is
  a *band* — a run of whole rows small enough for one datagram — and
  a full repaint is a top-to-bottom band sequence sharing one
  generation. You never think in bands: `paint` and `panel.draw`
  take the whole panel buffer and the SDK slices and streams it. For
  hover-highlight economy there is also `panel.draw_rows(y, pixels)`,
  which updates just the rows you pass.
- Panels also receive `INPUT_MOTION` (kind 6) events — hover tracking
  in panel device pixels. Motion arrives only inside panels, never on
  the tile.

**On a pre-panel shell** the panel simply is not there, and the SDK
tells you so cleanly. The shell advertises its protocol version in
`Welcome` (protocol 2 is the first with panels; older shells leave
the field zeroed, which reads as 1), and it is exposed as
`ctx.shell_proto`. On a protocol-1 shell, `open_panel` raises
`PanelError("this shell predates instrument panels ...")` *without*
putting anything on the wire — an old shell would treat the unknown
message as a protocol error and take your tile down with it. The tile
keeps working either way; feature-gate your panel affordance on
`ctx.shell_proto >= 2` if you want to hide it entirely.

A working example lives in `chonkdock/examples/clock.py`, registered
by `py-dockclock.dockapp`. Install this directory into a running
desktop with:

```
scripts/chonk-get install bindings/python
```

The wire contract, limits and timings are documented in
`docs/dockapp-protocol.md`; this package mirrors the Rust SDK
(`chonk_ui::dockapp`) decision for decision, including the ones that
are contracts: the 2 s Welcome timeout, the 10 s reconnect window
after an EOF, dropping (never blocking on) a send the shell is too
busy to take, and treating `Goodbye { Shutdown }` as a clean exit.
