# chonkstep

A NeXTSTEP-style desktop written in Rust, running as an X11 window
manager or as a Wayland compositor over one shared shell. The chrome is
chiseled and specified to the pixel - titlebar metrics, the raised
relief recipe, the 10x10 button glyph bitmaps, the 8px resizebar with
28px corner grips - under a dock of live instruments and real
workspaces, then extended with the conveniences a modern desktop
expects: resize from every edge and corner with macOS-style cursors, a
theme engine, translucent terminals, HiDPI scaling, and in-place hot
restarts that keep your windows open.

![Theme menu on the Teal Blueprint theme](docs/screenshots/theme-menu.png)

## Features

- **Chiseled decorations.** Focused black/unfocused gray titlebars,
  flush full-height buttons with the stock glyphs, etched resizebar
  grips - every metric, bevel step, and glyph bitmap written down to
  the pixel rather than eyeballed from screenshots.
- **Resize everywhere.** All eight edges and corners resize, with
  macOS-style cursor affordances: diagonal arrows on corners, vertical
  and horizontal arrows on the flat sides. North and west drags anchor
  the opposite edge, and client size hints (a terminal's cell grid) are
  respected mid-drag.
- **Classic window behaviors.** New windows are placed by a
  smart-placement scan - least overlap, top-left bias - with the
  `placement` setting switching to cascade or center. A right-click on
  any titlebar opens the window commands menu:
  maximize, miniaturize, shade, fullscreen, move to another workspace,
  close, and kill for the window that ignores close. And move-drags
  snap flush against screen and window edges, with the classic edge
  resistance feel (`edge_resistance` tunes the distance; 0 turns it
  off).
- **A modal Alt-Tab switcher.** Hold Alt and Tab through a centered
  switch panel of live window thumbnails - selection commits when Alt
  is released, Escape cancels, Shift+Tab steps backward. The panel is
  drawn in the active theme's language - the same chiseled chrome as
  everything else on screen, not a generic overlay.
- **Theme engine with five built-in themes.** NeXTSTEP Classic, Amber
  Phosphor, Teal Blueprint, Graphite, and NeXT Lavender. A theme
  restyles everything at once - window chrome, menus, wallpaper, dock,
  and the terminal's 16-color palette - and switching from the root
  menu applies instantly via hot restart, windows intact.
- **Translucent terminals.** Each theme sets a glass opacity for the
  terminals it spawns, composited as true alpha through a session
  compositor. The window manager creates 32-bit ARGB frames so client
  alpha survives reparenting - any translucent app works, not just the
  terminal.
- **HiDPI scaling.** `CHONKSTEP_SCALE` scales every piece of chrome -
  titlebars, buttons, bevels, cursors, glyphs - as one system.
- **Hot restart.** `scripts/restart.sh` asks a live session to re-exec
  the freshly built binary in place; open windows survive via the X11
  SaveSet. This is also how theme switching and `scripts/update.sh`
  apply changes without logging out.
- **EWMH compliance.** Publishes `_NET_SUPPORTED`, the client list,
  active window, workspaces, and workarea, and honors activation,
  close, and fullscreen/maximize requests - so `wmctrl`, `xdotool`,
  taskbars, and pagers see real state, video players go properly
  fullscreen, and `_NET_WM_WINDOW_TYPE` picks the decoration policy
  (dialogs get chrome, docks and notifications are left alone).
  `scripts/verify-ewmh.sh` checks a live session from the outside.
- **A real application story.** The root menu's Applications submenu
  is generated from the system's freedesktop `.desktop` entries -
  categories become cascades, `Terminal=true` apps launch inside the
  themed terminal, NoDisplay/Hidden/TryExec respected - so every
  installed app is launchable on day one. And the classic NeXTSTEP
  desktop's defining feature, the launcher dock: drag a miniaturized
  window's icon tile onto the strip below the Clip to pin its
  application (resolved through the `.desktop` index), click to
  launch - or to focus the running window, marked by an accent lamp on
  the tile - drag along the strip to reorder, drag off to unpin. Pins
  persist across sessions.
- **A NeXTSTEP-style desktop shell.** Right-click root menu with
  cascading submenus, a dock of instrument apps - an analog clock plus
  five LED instruments on theme-reactive glass (network traffic with a
  mirrored up/down history matrix, CPU and memory load, sound volume
  with click-zone control, wifi/ethernet link state, battery/power) -
  miniaturized-window icon tiles with drag-to-place, and
  five built-in wallpaper artworks. Real workspaces, too: a dock
  Clip - the workspace tile, drawn on the same recipes as the rest of
  the dock - sits at the top-left corner: clipped-corner arrows advance
  (growing workspaces on demand) and rewind, Alt+Ctrl+Left/Right
  switches, Alt+Shift+Left/Right carries the focused window
  along, and pagers can drive it all via `_NET_CURRENT_DESKTOP` and
  `_NET_WM_DESKTOP`.

![Translucent terminal on the Amber Phosphor theme](docs/screenshots/translucent-terminal.png)

## Installing on Omarchy (or any Arch)

```sh
git clone https://github.com/iconidentify/chonkstep.git
cd chonkstep
scripts/install.sh
```

The installer pulls the runtime dependencies with pacman (Xorg,
rxvt-unicode, picom, wireplumber for the sound instrument, the theme
fonts, the stack the Wayland compositor builds and runs against -
libxkbcommon, EGL/mesa, Xwayland, and libdrm/libinput/libudev/libseat
for the hardware session - and a Rust toolchain if needed), builds both
release binaries, installs **two** session entries that point back into
the checkout - `/usr/share/xsessions/chonkstep.desktop` and
`/usr/share/wayland-sessions/chonkstep.desktop` - and seeds
`~/.config/chonkstep/config.toml` from the fully-commented example if
you don't have one.

Both halves are real login sessions. How you start one depends on the
machine, and on which you want:

- **With a display manager** (sddm, gdm, lightdm, ...), log out and
  pick either "chonkstep" (X11) or "chonkstep (Wayland)" from the
  session list.
- **On stock Omarchy** there is no session picker at all (it boots
  straight into Hyprland via autologin), so switch to a TTY
  (Ctrl+Alt+F3), log in, and run either `startx scripts/xsession.sh`
  or `exec scripts/wayland-session.sh` from the checkout. The Wayland
  one needs no `startx`: the compositor *is* the display server, and it
  takes the graphics device, the input devices, and the VT for itself.
- **Nested**, for development or a look before you log in:
  `./target/release/chonkstep-wayland` from a terminal inside your
  current desktop opens a window that is its screen. See
  [Wayland](#wayland) below.

Seat access needs no setup on any systemd machine, Omarchy included -
logind already hands the active session its devices. On a machine
without logind, enable `seatd` and join the `seat` group; the installer
prints this only when it finds logind missing.

On a HiDPI display, set `scale = 2.0` in
`~/.config/chonkstep/config.toml` - it scales chrome, dock, cursors,
and the terminal font as one system.

Nothing is copied out of the repository, so updating is:

```sh
scripts/update.sh
```

which pulls, rebuilds, and hot-restarts the running session in place.

## Wayland

chonkstep is one desktop with two faces. Everything the desktop *is*
lives in backend-generic crates - `wm-core` decides (placement, focus,
stacking, workspaces, the modal Alt-Tab machinery), `wm-theme` renders
the decorations, and `chonk-shell` is the whole desktop above them:
dock, instruments, Clip, launcher strip, menus, wallpaper, themes. Two
backends implement the one backend contract those crates are written
against - `wm-x11`, and `wm-wayland`, a compositor built on
[Smithay](https://github.com/Smithay/smithay) - and two thin binaries,
`chonkstep` and `chonkstep-wayland`, wire the same shell to each. That
is what keeps the two sessions identical: a feature or a fix lands
once, in the shared crates, and both stacks get it by construction
rather than by porting discipline.

The Wayland side is the younger half, and it runs in two shapes from
one binary. Which one it takes is decided at startup, not at build
time: if there is already a desktop here to nest inside (an existing
`WAYLAND_DISPLAY` or `DISPLAY`) it opens a window; if there is not - a
bare TTY, a display manager's Wayland session - it takes the hardware.
`CHONKSTEP_BACKEND=winit` or `=session` forces the choice.

**As a login session**, the compositor owns the machine the way Xorg
does for the X11 side: it opens the seat through libseat (logind, or
seatd where there is no logind), sets a mode on the DRM device, drives
page flips through GBM and EGL, and reads real keyboards, mice, and
touchpads through libinput and udev. Ctrl+Alt+F1..F12 switches VTs and
the session hands its devices back and comes alive again on the way
in, so the console is never more than a keystroke away. Pick
"chonkstep (Wayland)" in your display manager's session list, or from a
TTY:

```sh
exec scripts/wayland-session.sh
```

That script is the Wayland twin of `scripts/xsession.sh` - it is what
the `wayland-sessions` entry points at, and it is also where you set
your keyboard layout for a TTY login (`XKB_DEFAULT_LAYOUT` and
friends; there is no settings daemon on a bare VT to ask). It starts no
compositing manager, unlike the X11 script: the compositor composites
itself, so the themes' translucent terminals are true alpha in the same
GLES scene that draws the chrome.

What the session backend does not do yet, stated plainly:

- **One GPU, every connector on it.** The session drives every display
  plugged into the primary DRM device, each with its own mode, page
  flips, and place in the desktop layout; a second GPU's outputs stay
  dark. Nothing hot-plugs: a monitor or GPU that appears mid-session is
  logged and ignored, so docking a laptop means restarting the session.
  Output layout is left to right in connector order - there is no
  configuration for arrangement, mirroring, or per-output scale yet.
- **The hardware cursor depends on your driver.** The pointer is asked
  for the display controller's cursor plane, which is what makes it
  track the hand instead of the frame rate. Whether it gets one is the
  driver's answer: on the virtio-gpu VM this was developed against, the
  cursor plane exists and `modetest` sees it, but it never reaches the
  compositor - the universal-planes capability that exposes it is
  enabled on the device fd and the plane still does not appear by the
  time the surface is built, which looks like an interaction between
  Smithay's device bookkeeping and the fresh fd libseat hands over on
  session activation. The session logs the planes it found
  ("DRM planes available to this crtc"), so the answer for a given
  machine is one line in the log; where the plane is missing the
  pointer is composited into the frame and everything else is
  unaffected.
- **Restart costs you your clients.** The compositor does re-exec in
  place - that is how the theme menu and `scripts/restart.sh` apply
  changes on both backends - but Wayland clients die with the socket
  they were connected to, and there is no SaveSet equivalent to adopt
  them afterwards. The X11 session keeps your windows across a
  restart; this one keeps only itself - and its dockapps. A dockapp is
  not a display-server client (it holds no `wl_display` at all, which is
  the whole point of that boundary), so the compositor leaves it running
  across the re-exec and hands its connection token to the replacement,
  which readopts it into the same tile instead of launching a second
  copy. The result is the odd one out on this list: an out-of-process
  dock tile gets a guarantee across a restart that no ordinary Wayland
  client on this desktop can have.
- **No EWMH-analog protocols.** `wlr-foreign-toplevel-management`,
  `wlr-output-management`, layer-shell, screencopy, idle-inhibit, and
  DRM leasing are all absent, so external taskbars, pagers, bars,
  screen recorders, and remote-desktop tools have nothing to talk to.
  The X11 side's EWMH surface - which `wmctrl`, `xdotool`, and pagers
  drive - has no counterpart here yet. The desktop's own dock, Clip,
  and menus need none of it: they are drawn by the compositor, not by
  clients.

**Nested** remains the way to develop the compositor, and the way to
look at it without logging out: it opens a regular window on your
existing desktop (Wayland or X11, via Smithay's winit backend) and
that window is its screen - chrome, dock, root menu, themes, and all.
The two modes render through the same scene-building code and the same
GLES renderer, which is why what you see nested is what you get on the
hardware. From a terminal on any desktop:

```sh
cargo build --release -p chonkstep-wayland
./target/release/chonkstep-wayland
```

Either way, the compositor allocates its own Wayland socket and sets
`WAYLAND_DISPLAY` (and `DISPLAY`, through XWayland) for everything it
spawns, so applications launched from the root menu or the themed
terminal land inside the session automatically. To aim a client at a
nested one from an outside terminal instead, point these variables at
the socket names from the startup log - the Wayland socket is
typically the first free slot (`wayland-1` when your host desktop
holds `wayland-0`), and XWayland's display number is printed alongside
it:

```sh
WAYLAND_DISPLAY=wayland-1 some-wayland-app
DISPLAY=:1 urxvt
```

X11 applications are first-class citizens, not a compatibility
afterthought: the compositor starts XWayland at boot and manages its
windows through exactly the same decoration and policy machinery as
native Wayland clients, so urxvt - and everything else that predates
Wayland - runs unchanged.

## Configuration

chonkstep reads `~/.config/chonkstep/config.toml` (or
`$XDG_CONFIG_HOME/chonkstep/config.toml`) at startup. The file is
optional - with no file, the defaults below apply - and a broken file
never prevents the session from starting: invalid lines are warned
about and skipped, and a completely unreadable file just means the
defaults. See [docs/config.example.toml](docs/config.example.toml) for
a fully commented example of every option.

Five settings and a keybinding table are available:
`focus_follows_mouse` (click-to-focus by default), `scale` (HiDPI UI
scaling; the `CHONKSTEP_SCALE` environment variable overrides it),
`theme` (a theme picked live from the root menu is persisted and wins
over it), `placement` (where new windows land: `smart` by default, or
`cascade` / `center`), and `edge_resistance` (how close, in pixels, a
dragged window gets to a screen or window edge before snapping flush;
`0` disables snapping). Keybindings merge over the defaults - list a
combo to change it, set it to `"none"` to unbind it, and every
unlisted default stays.

Edits apply on the next restart: `scripts/restart.sh` hot-restarts the
live session in place (windows survive), or bind the `restart` action
to a key and never leave the keyboard.

The default bindings:

| Binding          | Action                                       |
|------------------|----------------------------------------------|
| alt+shift+return | Spawn a terminal                             |
| alt+shift+q      | Close the focused window                     |
| alt+shift+x      | Toggle maximize                              |
| alt+shift+s      | Toggle shade (roll up into the titlebar)     |
| alt+shift+m      | Miniaturize to an icon tile                  |
| alt+shift+f      | Toggle fullscreen                            |
| alt+ctrl+right   | Next workspace (grows on demand)             |
| alt+ctrl+left    | Previous workspace                           |
| alt+shift+right  | Carry the focused window to the next         |
| alt+shift+left   | Carry the focused window to the previous     |

Alt+Tab window cycling is part of the modal switcher machinery and is
always available; it is not rebindable from the config file.

## Development

- `scripts/dev-nested.sh [width] [height] [scale]` runs chonkstep
  nested inside a Xephyr window on your existing desktop (Wayland or
  X11) - the standard way to develop a window manager without a second
  machine. Requires `xorg-server-xephyr`.
- `scripts/dev-vm.sh [sync|build|restart|shot|loop]` is the loop used
  to develop chonkstep from a Mac against an Omarchy ARM64 VM (managed
  by [Lume](https://github.com/trycua/cua)): rsync the tree in, build
  natively in the VM, hot-restart the live session, and pull a
  screenshot back out.
- `cargo test --workspace` runs the test suite; the decoration renderer
  is pure Rust (tiny-skia + cosmic-text, no X server needed), so most
  of the visual pipeline is unit-testable, including pixel-level
  regression tests for the relief recipes.

The workspace splits along seams that keep the core testable: `wm-core`
(window management logic, no display server), `wm-theme` (decoration
rendering), `wm-theme-api` (the boundary between them), `chonk-shell`
(the entire desktop - dock, instruments, Clip, launcher, menus,
wallpaper - generic over the backend), `wm-x11` and `wm-wayland` (the
two backends implementing that contract), `chonkstep` and
`chonkstep-wayland` (the thin binaries wiring the shell to each), and
`chonk-ui`/`chonk-about` (a small SDK surface proving third-party apps
can draw with the same visual language). Within `wm-theme`, the `tile` module is the common UI
platform for everything square: the workspace Clip's look formalized - a
diagonal gradient face under the stock raised relief, luminance-picked
ink, and sunken instrument-panel wells. Dock items,
miniaturized-window icon tiles, and third-party `chonk-ui` apps
all build on that one tile, which is why the whole desktop reads as a
single family. One level up, the `panel` module is the instrument SDK:
a theme-reactive LED screen (glass recessed behind a gasket, with an
accent-derived palette, seven-segment digits, meters, and history
matrices) that all five dock instruments draw on - and every instrument
ships a preview example (`cargo run -p wm-theme --example
preview_nettraffic -- <dir>`, likewise `_sysload`, `_soundctl`,
`_wifi`, `_power`) that renders every theme, scale, and state to PNG,
which doubles as the visual-regression harness new instruments should
copy.

![Focused and unfocused chiseled chrome](docs/screenshots/chiseled-chrome.png)

## License

GPL-3.0-only. See [LICENSE](LICENSE).
