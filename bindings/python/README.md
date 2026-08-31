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
