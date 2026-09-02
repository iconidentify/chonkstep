# layer-role-repro

A ~250-line Wayland client that answers one question: **does this
compositor kill a client for destroying a layer surface?**

It exists because the answer used to be yes, and finding that out cost
an evening. The symptom arrives with no error text on either side — the
client's socket simply closes — so the only way to see it is to reduce
it to a program small enough to read.

## What it does

1. Binds `wl_compositor`, `wl_shm` and `zwlr_layer_shell_v1`.
2. Creates a layer surface with `set_size(200, 100)` and
   `set_anchor(TOP)` — a centered menu, deliberately *not* anchored
   left and right.
3. Waits for the configure, attaches a real shm buffer, commits, and
   maps.
4. Tears the layer surface down, in one of three orderings.

## Running it

The compositor under test must already be running; point the client at
it by name.

```
cargo build
WAYLAND_DISPLAY=wayland-2 ./target/debug/layer-repro [ordering]
```

| ordering | what it does | who does this for real |
|---|---|---|
| `destroy-first` (default) | `destroy()`, then `attach(nil)` + `commit()` | Qt, Quickshell, gtk4-layer-shell |
| `unmap-first` | `attach(nil)` + `commit()`, then `destroy()` | the ordering upstream suggests instead |
| `bare-commit` | `destroy()`, then `commit()` with no attach | the minimal form |

Exit codes are the whole interface, so this works as a test:

| code | meaning |
|---|---|
| 0 | survived — the compositor handled it correctly |
| 2 | killed by the layer-role lifetime bug |
| 3 | killed by some *other* protocol error |
| 1 | setup failed — the compositor is missing a global, or is not running |

All three orderings matter. There is no client-side workaround: any
commit on the `wl_surface` after the role object dies is enough, so a
compositor that passes only `unmap-first` has not fixed anything.

## What it caught

Every ordering killed the client on chonkstep, because smithay
registers a layer-shell pre-commit hook on the `wl_surface`, never
removes it, and then resets the cached layer state to 0×0 with no
anchors when the role is destroyed — which is exactly the shape that
surviving hook treats as a fatal protocol error. The client is killed
over state it never wrote, on an object it already destroyed.

The spec is on the client's side. `zwlr_layer_surface_v1.destroy` says
only "This request destroys the layer surface" and imposes no lifetime
rule on the `wl_surface`, and core `wayland.xml` is explicit that
destroying the role object first is the *required* order and that
"destroying the role object does not remove the role from the
wl_surface".

This is smithay's bug, not the protocol's: wlroots has always had the
missing guard, returning early from its layer-surface commit path once
the role resource is gone. Upstream tracks it as
[Smithay#1979](https://github.com/Smithay/smithay/issues/1979), open
since March 2026. chonkstep works around it in
`crates/wm-wayland/src/layers.rs` — see `install_orphaned_role_guard`,
which also documents what to delete once upstream lands a fix.

Compositors that use smithay's `use_system_lib` feature appear immune,
but only because libwayland declines to post an error to a destroyed
object where the pure-Rust backend kills the client instead. The stale
hook is still there.

## Why it lives outside the workspace

It links `wayland-client`, and it is the only client-side program in
this repository. The workspace should not take on that dependency for
one diagnostic, so `layer-role-repro` is listed in the root
`Cargo.toml`'s `exclude` — build it on its own when you need it.
