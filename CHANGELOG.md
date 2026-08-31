# Changelog

All notable changes to chonkstep. Versions are workspace-wide: every
crate and both session binaries carry the same number.

## [0.2.0] - 2026-08-30

The release where the desktop stopped asking you to restart it, and
where "runs my applications" became a verified claim instead of a
hope. Everything below is on both sessions unless it names one.

### Live everything

- A theme pick, a UI scale change, and any edit to
  `~/.config/chonkstep/config.toml` now apply to the running session
  in place - nothing closed, no window lost, every dockapp kept. One
  path (`apply_session_state`) serves startup and reload alike, so a
  setting cannot be reloadable but not startable or the reverse.
  `scripts/reload.sh` triggers it, or bind the new `reload` action to
  a key. This matters most on Wayland, where a restart costs every
  client; the restart gesture (`scripts/restart.sh`, the `restart`
  action) remains for the one thing a running process cannot apply to
  itself: a new build.
- New config settings: `terminal_font_px` (the terminal's type size at
  1x scale, with the launch geometry derived from the monitor rather
  than fixed), `restore_session`, and `lock_command` (both below).
- Three new themes - Jade Lacquer, Ivory Halftone (the light one, which
  keeps the dark focused titlebar so focus stays legible), and Indigo
  Filament - for eight built-in themes total.
- The shell's terminal is now foot (Wayland-native, server-side-
  decoration aware) instead of urxvt.

### The Living Desktop

- Session restore (`restore_session = true`): the desktop records each
  window's application, geometry, workspace and shape as you work -
  debounced, atomically rewritten from the live client set, so a
  window you closed is forgotten the moment you close it - and
  relaunches that layout on the next login. Off by default; a session
  that spawns applications you did not just ask for is opt-in.
- A crash watchdog in `scripts/wayland-session.sh` supervises the
  compositor: an abnormal exit is logged, marked, and restarted with
  the recorded layout; a clean logout ends the session; more than
  three crashes in a minute trips a brake instead of crash-looping.
  With `lock_command` set (any ext-session-lock locker, e.g.
  swaylock), a recovered session comes back locked, not exposed.
- Dockapps already survived a compositor restart (they hold no display
  connection - the shell re-adopts them into the same tile); the
  session around them now survives too.

### The Overview

- A modal Exposé (`super+up` by default, the bindable `overview`
  action): every window on the desk as a card in miniature real
  chrome - the theme's titlebar, a sunken well holding a live
  capture - over a strip of genuine Clip tiles for the workspaces.
  Arrows move, Return or a click focuses, right-click opens the real
  window-commands menu, clicking a workspace tile switches desks with
  the panel open, Escape dismisses.
- Sharp and fast from its first night on real hardware: captures are
  served at card resolution while the panel is open (no more upscaled
  smear), and the selection plate is its own small surface, so moving
  it is a surface move rather than a full-panel repaint.

### The Instrument Platform, opened

- The dockapp system - out-of-process tiles that push finished pixels
  over a private socket and get theme, scale, input and supervision in
  return - is now a documented platform, not a Rust-only internal:
  `docs/dockapp-protocol.md` specifies every byte of the wire format,
  and `docs/instrument-platform.md` traces each guarantee (a tile
  cannot freeze, crash, or wedge the desktop) to the mechanism that
  enforces it.
- Dependency-free Python and Go bindings (`bindings/`), each with a
  working clock; the Python one is verified live in a nested session.
- `chonk-get`: install a dockapp from a git URL or local path, build
  it, and register it - plus `list` and `remove`.
- The Shelf (`examples/chonk-shelf`): clipboard history as a
  three-tile stack, built on the same platform.
- The platform's discipline is enforced, not advised: "a dock widget
  must not do I/O" is a build error (clippy `disallowed_methods` /
  `disallowed_types`, denied in CI), widgets are handed their data
  instead of the means to fetch it, a tile that cannot be drawn at the
  requested scale is refused at the codec, and the protocol's failure
  modes have tests rather than arguments.

### Wayland: applications, honestly

- Client-side decorations are honored end to end. A native client that
  negotiated nothing self-decorates, as the protocol says; one that
  requests client-side keeps its own chrome (Chromium, Edge); one that
  requests server-side, or leaves the choice to the compositor, gets
  the chiseled frame. X11 clients that set Motif hints are likewise
  left unframed. One titlebar per window, whoever draws it.
- Client-decorated windows are movable and resizable: the pointer
  ledger answers where the pointer actually is, `resize_request`
  reaches the core from a client's own grips, drags end when the
  button comes up, and size arbitration no longer lets the compositor
  and the client fight (the Edge resize flicker, the maximize that
  didn't).
- Outputs advertise the real scale, so native clients finally render
  at the desktop's size - while the compositor's own composition stays
  physical and byte-exact. Client minimize buttons work. The dock
  column is reserved so maximize respects it.
- Ecosystem protocols: wlr-layer-shell (launchers, bars, notification
  daemons), ext-session-lock (with an absolute contract: while locked,
  the scene is lock surfaces over black and nothing else, on screen
  and in every capture path), and idle-notify. A desktop that cannot
  lock is not usable professionally; this one locks.
- XWayland parity: the compositor publishes EWMH to the XWayland root
  (client list, active window, desktops, workarea with the dock
  reservation, frame extents - read-only for now, and the docs say
  so), forwards maximize/fullscreen state to clients, and holds focus
  for a window whose surface has not arrived yet.
  `docs/xwayland-compatibility.md` records exactly what works, what
  doesn't, and how to verify both.

### Verification

- The compositor can be driven from the outside: a test-only control
  socket (`CHONKSTEP_TEST_SOCKET`) injects synthetic pointer and
  keyboard events through the production input path, with a barrier
  that acks only after dispatch and a rendered frame. `chonk-testkit`
  boots an isolated nested session, launches real clients, injects
  input, and asserts on captured pixels - the recipe that found three
  hand-found regressions is now infrastructure (`scripts/e2e.sh`).
- CI gates: workspace tests, an X11 smoke test against a live Xvfb
  display (EWMH handshake, client management, framing policy), a
  Wayland build+test job against the real system libraries, and the
  clippy bans above.

## [0.1.0] - 2026-08-28

The foundation: a NeXTSTEP-style desktop as an X11 window manager,
then the same desktop as a Wayland compositor.

- Chiseled decorations specified to the pixel: titlebar metrics, the
  raised relief recipe, the button glyph bitmaps, the resizebar with
  corner grips; focused black, unfocused gray.
- Window management: all-edge resize with directional cursors, smart /
  cascade / center placement, edge-resistance snapping, shade,
  miniaturize to icon tiles, fullscreen, a modal Alt-Tab switcher with
  live thumbnails, and real workspaces driven from the dock Clip.
- The shell: right-click root menu with cascading submenus generated
  from the system's `.desktop` entries, the launcher dock with
  drag-to-pin, eight wallpaper artworks, five LED instruments (network,
  CPU/memory, sound, link, power) plus the analog clock, and a theme
  engine - five themes at this point - restyling chrome, menus,
  wallpaper, dock and terminal palette at once.
- EWMH compliance on X11: `_NET_SUPPORTED`, client list, active
  window, workspaces, workarea; activation, close, fullscreen and
  maximize requests honored; `scripts/verify-ewmh.sh` checks a live
  session from the outside.
- The dual architecture: one backend-generic shell (`chonk-shell` over
  `wm-core` / `wm-theme`) behind two backends, `wm-x11` and
  `wm-wayland` (Smithay). The compositor runs nested in a window or as
  a real login session on DRM/KMS with libinput and libseat - VT
  switching, multi-monitor on the primary GPU, the hardware cursor
  where the driver provides a plane - with XWayland from boot.
- Installer (`scripts/install.sh`) putting both sessions in a login
  manager's picker, `scripts/update.sh` (pull, rebuild, hot-restart),
  and in-place hot restart that keeps windows open on X11 via the
  SaveSet.
