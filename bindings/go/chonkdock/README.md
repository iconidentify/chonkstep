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

A working example lives in `examples/clock`; build it with `go build
./...` and register the binary with a `.dockapp` file (or
`scripts/chonk-get install`). The wire contract, limits and timings
are documented in `docs/dockapp-protocol.md`; the pinned byte-layout
vectors from the Rust reference are asserted in `wire_test.go`.
