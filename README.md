# chonkstep

A window manager for X11, written in Rust, that takes WindowMaker's
stock look seriously: the default chrome is reproduced value-for-value
from the WindowMaker source (titlebar metrics, the RAISED2 relief
recipe, the 10x10 button glyph bitmaps, the 8px resizebar with 28px
corner grips), then extended with the conveniences a modern desktop
expects - resize from every edge and corner with macOS-style cursors, a
theme engine, translucent terminals, HiDPI scaling, and in-place hot
restarts that keep your windows open.

![Theme menu on the Teal Blueprint theme](docs/screenshots/theme-menu.png)

## Features

- **WindowMaker-parity decorations.** Focused black/unfocused gray
  titlebars, flush full-height buttons with the stock glyphs, etched
  resizebar grips - checked against WindowMaker's own `framewin.c`,
  `wrlib`, and `def_pixmaps.h` rather than eyeballed from screenshots.
- **Resize everywhere.** All eight edges and corners resize, with
  macOS-style cursor affordances: diagonal arrows on corners, vertical
  and horizontal arrows on the flat sides. North and west drags anchor
  the opposite edge, and client size hints (a terminal's cell grid) are
  respected mid-drag.
- **WindowMaker window behaviors.** New windows are placed by
  WindowMaker's smart-placement scan - least overlap, top-left bias -
  with the `placement` setting switching to cascade or center. A
  right-click on any titlebar opens the window commands menu:
  maximize, miniaturize, shade, fullscreen, move to another workspace,
  close, and kill for the window that ignores close. And move-drags
  snap flush against screen and window edges, with WindowMaker's edge
  resistance feel (`edge_resistance` tunes the distance; 0 turns it
  off).
- **A modal Alt-Tab switcher.** Hold Alt and Tab through a centered
  switch panel of live window thumbnails - selection commits when Alt
  is released, Escape cancels, Shift+Tab steps backward. The panel is
  drawn in the active theme's language, WindowMaker switchpanel style.
- **Theme engine with five built-in themes.** Window Maker, Amber
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
  installed app is launchable on day one. And WindowMaker's defining
  feature, the launcher dock: drag a miniaturized window's icon tile
  onto the strip below the Clip to pin its application (resolved
  through the `.desktop` index), click to launch - or to focus the
  running window, marked by an accent lamp on the tile - drag along
  the strip to reorder, drag off to unpin. Pins persist across
  sessions.
- **A WindowMaker-style desktop shell.** Right-click root menu with
  cascading submenus, a dock of instrument apps - an analog clock plus
  five LED instruments on theme-reactive glass (network traffic with a
  mirrored up/down history matrix, CPU and memory load, sound volume
  with click-zone control, wifi/ethernet link state, battery/power) -
  miniaturized-window icon tiles with drag-to-place, and
  five built-in wallpaper artworks. Real workspaces, too: a dock
  Clip - WindowMaker's workspace tile, ported from its dock.c
  recipes - sits at the top-left corner: clipped-corner arrows advance
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
fonts, and a Rust toolchain if needed), builds the release binaries,
installs a `chonkstep.desktop` session entry that points back into the
checkout, and seeds `~/.config/chonkstep/config.toml` from the
fully-commented example if you don't have one.

How you start it depends on the machine:

- **Stock Omarchy** boots straight into Hyprland via autologin - there
  is no login-manager session picker. Switch to a TTY (Ctrl+Alt+F3),
  log in, and run `startx scripts/xsession.sh` from the checkout. (Or
  install and enable a display manager such as sddm; the session entry
  is already in place for it.)
- **A machine with a display manager** (sddm, gdm, lightdm, ...): log
  out and pick "chonkstep" in the session list.

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

The Wayland side is the younger half, and its current shape is stated
honestly: it runs today as a *nested* session for development. The
compositor opens a regular window on your existing desktop (Wayland or
X11, via Smithay's winit backend), and that window is its screen -
chrome, dock, root menu, themes, and all. A true DRM/KMS session that
owns the hardware from a TTY, the way Xorg does for the X11 side, is
planned behind the crate's `session` feature but is not built yet.

To run it nested, from a terminal on any desktop:

```sh
cargo build --release -p chonkstep-wayland
./target/release/chonkstep-wayland
```

The compositor allocates its own Wayland socket and sets
`WAYLAND_DISPLAY` (and `DISPLAY`, through XWayland) for everything it
spawns, so applications launched from the root menu or the themed
terminal land inside the nested session automatically. To aim a client
at it from an outside terminal instead, point these variables at the
socket names from the startup log - the Wayland socket is typically
the first free slot (`wayland-1` when your host desktop holds
`wayland-0`), and XWayland's display number is printed alongside it:

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
  regression tests for the WindowMaker relief recipes.

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
diagonal WindowMaker `IconBack`-gradient face under the stock RAISED2
relief, luminance-picked ink, and sunken instrument-panel wells. Dock
items, miniaturized-window icon tiles, and third-party `chonk-ui` apps
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

![Focused and unfocused WindowMaker chrome](docs/screenshots/windowmaker-chrome.png)

## License

GPL-3.0-only. See [LICENSE](LICENSE).
