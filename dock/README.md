# Chonk Dock

The NeXTSTEP dock returns as **`chonk-dock`**, a separate X11 and Wayland application
in the ChonkStep repository. The dock owns its tiles, rendering, samplers,
panels, socket and dockapp children. The compositor links none of the dock
crates. Installing the binary does not start it.

**Omarchy mode starts no dock process.** There are no dock workers, timers,
sockets, buffers or hidden surfaces until you explicitly launch it. Quit,
`--stop` and the off half of `--toggle` exit the process and release its column.
An explicitly running dock naturally uses resources, including on Omarchy.
Hosted dockapps shut down with it and start fresh on the next launch.

## Build and run

From the repository root:

```sh
cargo build --release -p chonk-dock -p chonk-netjoin -p chonk-btpair
./target/release/chonk-dock
./target/release/chonk-dock --stop
./target/release/chonk-dock --toggle
./target/release/chonk-dock --status
```

The Arch package and source installer include the application and its launcher
entry. There is no autostart entry or enabled user service. To install only this
subproject, run `./dock/install.sh`; it installs into `~/.local/bin` by default.

The client automatically selects X11 in an X11 session, or native Wayland when
`WAYLAND_DISPLAY` is available. `--x11` and `--wayland` select explicitly; use the
same flag with `--stop`, `--toggle` or `--status` when overriding automatic
selection. Each display has its own process and control socket.

Wayland requires layer-shell and xdg-shell. X11 uses ordinary dock windows and
EWMH struts, with no embedded window manager and no compositing requirement.
`--output NAME` selects a Wayland output or RandR output. The default is the first
Wayland output, or X11's primary output. Removing the selected output ends the
dock. Wayland output scale and X11's `Xft.dpi` are followed live;
`--scale 1.25` additionally enlarges the dock UI. X11 reserves its column at the
outer right edge of the X screen; an internal monitor edge cannot be represented
by an EWMH strut without reserving space on the neighboring monitor.
`--theme nextstep-classic` fixes the palette. Without it, ChonkStep's control
socket supplies live theme and appearance changes. On another compositor the
default is NeXTSTEP Classic. The workspace Clip uses ChonkStep's control socket,
with standard EWMH workspace selection as a fallback on other X11 window managers.

## What is restored

- Original tile artwork and renderers, identity mark, clock, traffic graph,
  CPU/memory, audio, network, Bluetooth and power instruments.
- Audio, network and Bluetooth detail panels, declarative background sampling,
  supervised actions and the existing network/password and pairing helpers.
- Middle-drag tile reordering, persisted across dock restarts.
- Application launcher pins, running indicators, launch-or-focus, drag to
  reorder and drag off the strip to unpin.
- Workspace Clip and minimized-window tiles. Click a minimized tile to restore;
  drag it onto the left launcher strip to pin its application. These tiles show
  application labels rather than capturing window contents.
- Authenticated dockapp transport, bounded frame queues, liveness checks,
  crash budgets, restart/remove menus, and out-of-process instrument panels.
- Rust, Python and Go SDKs, examples, protocol tests and hostile-peer tests.

Click the identity tile for applications, **Pin application**, and **Quit Dock**.
Right-click an instrument to open its panel; Escape or clicking outside closes
it. Right-click a remote dockapp for its status, restart and remove actions.

```sh
chonk-dock --list-apps
chonk-dock --pin org.mozilla.firefox
```

Pins and instrument order retain the original locations:
`$XDG_STATE_HOME/chonkstep/dock` and `dock-items`, with the usual
`~/.local/state` fallback. Dockapp registrations retain their `.dockapp` format
and discovery paths. No compositor `show_dock` setting is needed or revived.

## SDKs and examples

The host and libraries live in `dock/crates/`. `chonk-dock-theme` adds the
recovered instrument artwork to the shared `wm-theme` types; the shared theme
crate does not depend on it. `chonk-dock-widget` provides the declarative widget
vocabulary. `chonk-instruments` remains restricted to pure sampled-data folds.
`chonk-dock-proto` is the wire contract; `chonk-dock-client` is the Rust client SDK.
Existing source can alias the latter dependency as `chonk-ui` when migrating.

The [Python SDK](bindings/python/README.md), [Go SDK](bindings/go/chonkdock/README.md),
[protocol](docs/dockapp-protocol.md) and [platform guide](docs/instrument-platform.md)
retain their original design and wire format. In the restored design documents,
the host/shell is now the dock process, not the compositor. Renderers, SDKs,
instruments and host state machines were recovered from the parent of commit
`5d6564959cd8a8ac827ac95c5fe0b4aa3a869931`; client surface transport and lifecycle
are new. No historical compositor implementation is vendored into this project.

```sh
cargo build -p chonk-dockclock -p chonk-shelf -p chonk-dockapp-torture
dock/chonk-get install dock/examples/chonk-dockclock
dock/chonk-get install dock/bindings/python
dock/chonk-get install dock/examples/chonk-switch
dock/chonk-get list
```

`chonk-get` requires Python 3.11+ and accepts a source directory or git URL.
It builds and copies the application into `$XDG_DATA_HOME/chonkstep/dockapps`
and registers it under `$XDG_CONFIG_HOME/chonkstep/dockapps`, using the usual
home-directory fallbacks. Restart the dock to load or unload registrations;
`chonk-get remove ID` removes an installed example. Install trusted sources:
build scripts and dockapps execute as your user. You can also register an
example manually using its `.dockapp` file with absolute executable paths.
Examples are not registered or launched by the Chonk Dock installer.

## Verification

```sh
./dock/check.sh
./dock/check-x11.sh    # requires Xvfb; uses an isolated X server
scripts/e2e.sh --headless --test standalone_dock
```

The native tests launch isolated X11 and Wayland sessions and check the
absent-dock default, launch/focus/restore, repeated panel dismissal, live scaling,
process restart, child shutdown and reservation release. The
compositor's normal build remains independent: `cargo tree -p chonkstep-wayland`
contains no dock, instrument, or dock SDK crate.
