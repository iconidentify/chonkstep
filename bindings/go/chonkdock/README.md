# chonkdock for Go

The chonkstep dockapp protocol (v1) as a stdlib-only Go module: wire
codec, handshake, and a `Dockapp` runner with the SDK's
reconnect-on-EOF behavior. The socket layer speaks raw syscalls because
`net` has no `SOCK_SEQPACKET` — and SEQPACKET is what deletes message
framing from the protocol.

```go
app := &chonkdock.Dockapp{
    ID: "hello-instrument",
    Draw: func(ctx *chonkdock.Ctx, buf []byte) bool {
        for i := 0; i < len(buf); i += 4 {
            copy(buf[i:], []byte{0x30, 0x60, 0x90, 0xff})
        }
        return true
    },
}
if err := app.Run(); err != nil {
    log.Fatal(err)
}
```

`Ctx` carries `TilePx`, `Height`, `Scale`, `ThemeID`, `ThemeTOML` and
`Visible`; set `OnInput` for clicks and scrolls (tile-local
coordinates) and `OnTheme` if you cache theme-derived state. Frames are
premultiplied RGBA8, top row first.

## Instrument panels

A tile is glanceable state; when a click deserves a real surface, a
dockapp may open one *instrument panel* — a larger popup the shell
places near the tile. One panel per dockapp. The shell may clamp the
requested size, dismiss the panel at any time, or refuse it, so the
panel API is asynchronous the same way the tile API already is: the
`DrawPanel`/`OnPanelInput`/`OnPanelClosed` fields on `Dockapp` are the
siblings of `Draw`/`OnInput`/`OnTheme`, and `ctx.OpenPanel` is a
request whose grant arrives later.

A minimal panel — a solid themed rectangle that prints clicks (into
the shell's journal; a dockapp's stdout is /dev/null):

```go
app := &chonkdock.Dockapp{
    ID: "panel-demo",
    Draw: func(ctx *chonkdock.Ctx, buf []byte) bool {
        for i := 0; i < len(buf); i += 4 {
            copy(buf[i:], []byte{0x30, 0x60, 0x90, 0xff})
        }
        return true
    },
    OnInput: func(ctx *chonkdock.Ctx, ev chonkdock.InputEvent) bool {
        if ev.Kind == chonkdock.InputPress && ctx.Panel() == nil {
            _, _ = ctx.OpenPanel(320, 240) // a request, not a grant
        }
        return false
    },
    // Called like Draw, but only once the grant has arrived; buf is
    // premultiplied RGBA8 at the *granted* p.Width x p.Height, which
    // may be smaller than was asked for.
    DrawPanel: func(ctx *chonkdock.Ctx, p *chonkdock.Panel, buf []byte) bool {
        color := []byte{0xc8, 0xc0, 0xb0, 0xff}
        if strings.Contains(ctx.ThemeTOML, `"dark"`) { // or parse the palette
            color = []byte{0x20, 0x24, 0x28, 0xff}
        }
        for i := 0; i < len(buf); i += 4 {
            copy(buf[i:], color)
        }
        return true
    },
    OnPanelInput: func(ctx *chonkdock.Ctx, p *chonkdock.Panel, ev chonkdock.InputEvent) bool {
        if ev.Kind == chonkdock.InputPress {
            ctx.Log(chonkdock.LogInfo, fmt.Sprintf("panel click at %d,%d", ev.X, ev.Y))
        }
        return false // true requests an immediate panel repaint
    },
    OnPanelClosed: func(ctx *chonkdock.Ctx, p *chonkdock.Panel, reason chonkdock.PanelCloseReason) {
        ctx.Log(chonkdock.LogInfo, "panel went away: "+reason.String())
    },
}
```

What the SDK enforces so you cannot protocol-error:

- `ctx.OpenPanel(w, h)` sends the request and returns a `*Panel`
  immediately; `p.Opened()` is false and no frame crosses the wire
  until the shell's `PanelOpened` grant arrives. `p.Draw(pixels)`
  (push-style, for apps that do not use `DrawPanel`) returns an error
  before the grant.
- The grant may be clamped: draw at `p.Width x p.Height`, never at the
  requested size.
- `ctx.OpenPanel` again while the panel is open renegotiates the size
  on the same handle; frames pause until the new grant.
- `p.Close()` asks the shell to take the panel down; `OnPanelClosed`
  fires with `PanelClosedByClient` when confirmed. The shell can also
  close it behind your back — `PanelDismissed` (the user clicked
  away), `PanelShutdown` (the shell is going away; also synthesized
  locally when the connection drops), `PanelRefused` (the open request
  was declined; the panel never existed). The handle is dead after any
  of these; open a fresh one.
- Limits: panels go up to `MaxPanelPx` (1024) per edge, `w * h * 4 <=
  4 MiB` (`MaxPanelFrameBytes`, the shell's total-buffer cap). On the
  wire a panel frame is a *band* — a run of whole rows small enough
  for one datagram — and a full repaint is a top-to-bottom band
  sequence sharing one generation. You never think in bands:
  `DrawPanel` and `p.Draw` take the whole panel buffer and the SDK
  slices and streams it. For hover-highlight economy there is also
  `p.DrawRows(y, pixels)`, which updates just the rows you pass.
- Panels also receive `InputMotion` (kind 6) events — hover tracking
  in panel device pixels. Motion arrives only inside panels, never on
  the tile.

**On a pre-panel shell** the panel simply is not there, and the SDK
tells you so cleanly. The shell advertises its protocol version in
`Welcome` (protocol 2 is the first with panels; older shells leave
the field zeroed, which reads as 1), exposed as `ctx.ShellProto`. On
a protocol-1 shell `ctx.OpenPanel` returns `ErrPanelsUnsupported`
*without* putting anything on the wire — an old shell would treat the
unknown message as a protocol error and take your tile down with it.
The tile keeps working either way; feature-gate your panel affordance
on `ctx.ShellProto >= 2` if you want to hide it entirely.

A working example lives in `examples/clock`; build it with `go build
./...` and register the binary with a `.dockapp` file (or
`scripts/chonk-get install`). The wire contract, limits and timings
are documented in `docs/dockapp-protocol.md`; the pinned byte-layout
vectors from the Rust reference are asserted in `wire_test.go`.
