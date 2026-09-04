# Changelog

All notable changes to chonkstep. Versions are workspace-wide: every
crate and both session binaries carry the same number.

## [Unreleased]

### Release hardening

- **A slow nested boot now explains itself.** If the compositor exits, fails
  to publish its Wayland socket, or never answers the test door, the harness
  includes the final 80 compositor log lines in the failure instead of a bare
  timeout while preserving the existing performance deadline.
- **Public Rust documentation is a CI gate.** Broken API links and rustdoc
  warnings now fail the build; the existing stale/private links have been
  corrected, while all executable examples continue to pass as doctests.
- **Clippy is strict across the entire workspace.** The last theme and Wayland
  warning backlog is resolved, including a 6.4 KiB graphics enum imbalance and
  avoidable per-pixel slice chunking; every warning now fails CI for every crate.
- **Headless browser tests get a real private D-Bus session.** Chromium no
  longer burns its startup deadline retrying the hosted runner's invalid bus
  address, eliminating the intermittent scale-and-resize test failure.
- **Release automation now trusts immutable action revisions.** CI, native
  package attestation, artifact assembly, and the future AUR publisher pin
  every third-party GitHub Action to a full commit instead of a mutable tag.
  CI also runs RustSec's dependency audit so a newly disclosed vulnerability
  cannot pass silently, and every unsafe block must state the invariant that
  makes its libc, EGL, or raw-descriptor operation sound.
- **The text stack no longer depends on unmaintained `rustybuzz`.** All
  consumers now use `cosmic-text` 0.19 and its `harfrust` shaper. This also
  replaces two obsolete `ttf-parser` releases with one current dependency;
  workspace tests and the full nested-desktop suite cover the resulting font
  measurement and rendering changes.
- **Disconnected Hyprland IPC clients no longer pin a CPU core.** Empty
  request probes and quiet event subscribers are removed as soon as their
  peer closes. Previously their permanently readable hangup fds survived
  forever; a live desktop with 23 accumulated probes was measured at 1,560
  event-loop passes per second and 99% of one core. Unit tests cover both
  sockets, and a nested-compositor regression verifies that repeated probes
  leave neither descriptors nor an unhealthy server behind.
- **Full desktop behavior now runs in CI.** The Wayland job boots an isolated
  Weston headless host and runs ChonkStep's complete real-client integration
  suite: Chromium and native terminals, window input/geometry, Omarchy's
  themes/menu/bar, lock, gamma and `hyprsunset`, Hyprland IPC, protocols,
  workspaces, session restore, and crash supervision. Python and Go SDK tests
  are now first-class CI jobs as well.
- **Test controls cannot poison a later login.** Application launches now
  remove compositor-private backend, debug, restart, and test variables while
  preserving the documented child API. The Wayland session also scrubs stale
  selectors from both its process and systemd's user activation environment
  before startup, including a compositor-owned cursor size as one atomic pair.
  Its supervisor test begins with a deliberately poisoned environment and
  proves none of it reaches the compositor. The old cleanup attempt using the
  nonexistent `dbus-update-activation-environment --unset` option is gone;
  Omarchy's systemd user manager now performs the deletion through its real
  API.
- **The browser scale regression has a faithful test.** The Chromium scale-2
  scenario no longer clicks through the always-on-top dock while intending to
  grab Chromium's client-side resize edge. It runs dockless, exercises the
  actual browser surface, and passes against current Chromium under the same
  headless compositor path CI uses. On hosted runners that prohibit Chromium's
  user-namespace sandbox, only this isolated `about:blank` test process opts
  out; local browser tests retain normal sandboxing.

### Omarchy-compatible session and install path

- **A real managed login.** ChonkStep now ships a dedicated UWSM session,
  publishes only the desktop environment variables a session owns, reaches
  `graphical-session.target` and `xdg-desktop-autostart.target`, and treats a
  session-manager stop as logout rather than a compositor crash. Omarchy's
  suspend lock, input method, portals, shell, and application scopes therefore
  follow the same lifecycle they do under Hyprland.
- **Explicit, reversible installation.** The release package provides
  `omarchy install desktop-chonkstep` and its matching remove command. The
  installer preserves Omarchy's autologin user, selects the managed ChonkStep
  session, enables an Omarchy-native SDDM theme with a session picker, and
  selects the `desktop = "omarchy"` posture in a new config or one without an
  explicit desktop choice while preserving an explicit user choice; it never patches
  Omarchy-owned files. A tag-driven workflow publishes the
  release PKGBUILD and `.SRCINFO` to the AUR, making the intended entry path
  `omarchy pkg aur add chonkstep` followed by the install command.
- **Native GitHub packages while AUR registration is unavailable.** A preview
  tag builds, tests, and publishes pacman-installable `x86_64` and `aarch64`
  packages on matching GitHub runners, with checksums and GitHub provenance
  attestations. The ARM package links against Arch Linux ARM rather than an
  Ubuntu cross-toolchain.
- **Omarchy's configuration has real effects.** Monitor position and scale,
  input repeat settings, bind flags, layer-scoped bindings, window rules, Lua
  long strings, and window move/resize/centre/fullscreen dispatchers are
  applied or refused with a named reason. Bindings inside unsupported submaps
  can no longer leak into the global keymap.
- **Hyprland-compatible IPC reports live state.** Window, monitor, workspace,
  keyboard, binding, reload, and refusal data now come from the running desk;
  `exec --` preserves argv; mapping events and addresses agree with the
  foreign-toplevel protocol; and unsupported tiling commands no longer claim
  success. This is enough for Omarchy's current shell, menus, scripts, and bar
  to operate without a ChonkStep-specific fork.
- **The protocols ordinary clients expect.** The Wayland compositor now serves
  activation, cursor-shape, primary-selection, text-input/input-method,
  relative-pointer and pointer-constraints, presentation-time,
  `hyprland-ctm-control-v1`, and `hyprland-toplevel-mapping-v1`. Night light
  works through the stock `hyprsunset`; linux-dmabuf publishes default
  feedback from the session's authoritative DRM node even when EGL device
  discovery is unavailable. The public ScreenCast portal path was exercised
  through PipeWire on a clean Omarchy VM and returned real desktop frames.
- **Small interaction parity.** Escape closes the root menu and all of its
  cascades, while preserving the existing Escape behavior for instrument
  panels and modal overlays.

### A desktop that hosts Omarchy

chonkstep now stands where Hyprland stands on an Omarchy 4 machine:
it runs Omarchy's own shell, mirrors Omarchy's own menu, wears
Omarchy's own theme and background, and feeds Omarchy's bar - all
through Omarchy's public extension points (its JSONC menu, its
`current/theme` state, its Quickshell plugin system), never by
emulating Hyprland.

- **Omarchy's shell, hosted.** A Wayland session starts `omarchy-shell`
  (the Quickshell process behind Omarchy's bar, panels, pickers,
  notifications, OSD and lock screen) through Omarchy's own
  `omarchy-launch-shell`, exactly as Omarchy's Hyprland autostart does.
  Every Omarchy menu row that ends in a panel - the speed tests, Style
  > Theme, the volume keys - now works here. `omarchy_shell = false`
  opts out; the key is inert without Omarchy or on X11. The shell's
  Background plugin is the one surface the compositor declines (it
  would paint over the desk and take the root menu's right-click); the
  surface stays a healthy client and is simply never shown.
- **Omarchy Bar, on request.** The shell's bar starts hidden - the Dock
  and the Clip already hold the corners - and the root menu's new
  `Omarchy Bar` row switches it on and off, remembering the choice in
  chonkstep's own state (never Omarchy's, so it does not follow you
  into a Hyprland session). Hidden means not drawn, not clickable, no
  reserved edge; the bar keeps running and is whole the instant it is
  shown. Underneath: a `Backend::set_layer_surface_hidden` verb and
  one `layer_presented` predicate every layer-shell consumer asks.
- **Omarchy's menu, in the root menu.** Right-click the desk and the
  `Omarchy` submenu *is* Omarchy's menu: read from
  `omarchy-menu.jsonc` (and your extension file) on every reload, kind
  inferred as Omarchy's `MenuModel.js` infers it, every `when` /
  `checked` / `disabled` condition evaluated the way Omarchy evaluates
  it (batched, in the background, never on the desktop's thread), and
  every action run exactly as Omarchy runs it (`bash -lc <action>`,
  detached). Only the rows that would command Hyprland are left out.
  `omarchy_menu = false` turns the submenu off.
- **Omarchy's theme, followed.** `theme = "omarchy"` (or the `Omarchy
  (...)` row in the Theme submenu) reads the palette
  `omarchy-theme-set` leaves under `~/.local/state/omarchy/current/
  theme/` and dresses chrome, dock, menus and terminals in it - light
  or dark as the palette says - on chonkstep's own geometry, and keeps
  watching, so an Omarchy theme switch restyles this desk within a
  second. The desk wears Omarchy's *current background* as its
  wallpaper too (whatever format Omarchy ships it in), repainting when
  `omarchy-theme-bg-next` cycles it, and the Wallpaper submenu offers
  `Omarchy's Background` so any built-in theme can wear it. The other
  direction: `omarchy-export-themes` writes the eight built-in themes
  as Omarchy themes into `~/.config/omarchy/themes/`, so
  `omarchy-theme-set amber-phosphor` dresses the rest of the machine
  to match.
- **A socket for the bar.** Always-on, newline-framed JSON at
  `$XDG_RUNTIME_DIR/chonkstep/control-<display>.sock` (also in
  `CHONKSTEP_CONTROL_SOCKET`): workspaces, outputs, the focused window,
  the theme, each re-sent when it changes, and two verbs (`snapshot`,
  `focus-workspace`). `docs/control-socket.md` is the contract;
  its first clients are two Omarchy bar widgets under `omarchy/plugins/`
  - `chonkstep.workspaces` (the strip Omarchy's own widget cannot draw
  here, because it asks Hyprland) and `chonkstep.theme`.
- **Making room.** The Dock steps out from under a layer-shell bar's
  exclusive zone (top and right edges) and back the moment the bar
  goes; maximized windows refit to the workarea live. Those are the
  only two edges the Dock yields to: a left or bottom bar still pushes
  the workarea but overlaps the Clip and the icon row today. Omarchy's
  terminals (`org.omarchy.terminal`, alacritty, kitty), which Omarchy
  configures to draw no chrome of their own, get this desktop's frame:
  the compositor now answers every xdg-decoration negotiation
  server-side, as Hyprland does, and `[decorations] client_side` is
  the opt-out for a client that really should stay bare.
- **Plumbing the integration needed.** `[commands]` (named shell
  commands a `run <name>` keybinding refers to - the action vocabulary
  stays closed, the names may now stand for `omarchy-menu`), `terminal`
  and `autostart` in the config, and the XF86 media keysyms. Three
  protocols Omarchy's shell and tools speak: `ext-data-control-v1` and
  `wlr-data-control` (clipboard managers), `virtual-keyboard-v1`
  (`wtype`), and `hyprland-focus-grab-v1` (the shell's popups dismiss
  on a click elsewhere). And three bugs living under the shell exposed:
  a hot-restart marker every child process inherited, a
  `_NET_WORKAREA` PropertyNotify storm (14,000 events a second with a
  bar reserving an edge), and a Smithay layer-shell pre-commit hook
  that disconnected any client destroying a layer surface the way Qt
  and GTK do (Smithay #1979, guarded in-tree).

### Chrome, decided by what the client says

- The per-name allowlist (`self_decorating_apps`) is gone, replaced by
  reading what each client actually says on the two decoration
  protocols: xdg-decoration negotiations are concluded server-side,
  KDE's `org_kde_kwin_server_decoration` - the only one GTK speaks - is
  now advertised and its client-side declarations believed, and
  `_MOTIF_WM_HINTS` is read by one parser on every leg. `[decorations]`
  in the config corrects both directions (`client_side` to keep an xdg
  client bare, `server_side` to frame a KDE or X11 client that declares
  chrome and draws none), reaches XWayland windows too, and a reload
  re-decides the chrome of every window already open. The old key
  still parses, as `decorations.client_side`. And the floor under it:
  `drag_modifier` (Alt by default) moves a window with a left drag and
  resizes it with a right drag from anywhere on its content, and the
  new `window-menu` action (`control+escape`) opens the window commands
  menu, so a window with no titlebar from either side is still yours to
  move, resize and close.

### Light and dark, everywhere

- Every theme now has two deliberate renditions of itself - a light
  one and a dark one - along a new session-wide `appearance` axis.
  Same identity, same chrome geometry, two designed dresses: fills,
  chisel ramps, menu palettes, the full 16-color terminal scheme and
  the wallpaper artwork's own mood each exist per side. The focused
  titlebar stays ink on both sides (focus must stay legible on a pale
  desk), light-mode selection highlights use each theme's accent
  rather than inverting to white, and every wallpaper artwork gained a
  counterpart rendition so the composition survives the mood change.
  With nothing configured, each theme wears its native mood - dark for
  seven of the eight, light for Ivory Halftone - so nothing changes
  until you ask.
- Switching is live and scriptable: write `light`, `dark` or `toggle`
  to `$XDG_STATE_HOME/chonkstep/appearance-request` and the session
  applies it within a tick through the same in-place path a theme pick
  takes; the current mode is published (atomically) at
  `$XDG_STATE_HOME/chonkstep/appearance` for anything - a dockapp, a
  script - to read. `appearance = "light"|"dark"` in the config seeds
  a first session. See the new `docs/appearance.md` for the whole
  contract.
- Applications follow. Terminals the desktop spawns get both foot
  color sections plus the current `initial-color-theme`, and running
  ones are retinted on the spot via foot's SIGUSR1/SIGUSR2 color-theme
  switch. GTK4/libadwaita/Electron apps follow live through GSettings
  `color-scheme` and the desktop portal; GTK3 follows when the
  `gtk-theme` in play is a member of an installed light/dark pair
  (Adwaita/Adwaita-dark and friends - a hand-picked theme is never
  overwritten); the X11 session republishes the pair member over
  XSETTINGS. Dockapps follow through the existing `ThemeChanged`
  broadcast, whose `theme_toml` now carries an `appearance` tag, and
  SDK apps get `CHONKSTEP_APPEARANCE` beside `CHONKSTEP_THEME`.
  `docs/appearance.md` has the honest table of what follows live and
  what waits for its next launch (Qt, notably, is documented rather
  than forced).
- `chonk-switch` (`examples/chonk-switch`): the appearance switch as a
  dock tile - a machined slide toggle in the desktop's chiseled idiom,
  sun and moon trading places at the midpoint of a quarter-second
  throw. Built purely on the platform's public surfaces (the Python
  SDK for the tile, the appearance files for the mode), which makes it
  the citizenship test a third-party instrument would take: it knows
  nothing the docs don't say. It follows the mode file rather than its
  own optimism - a click throws immediately but settles back if the
  desktop refuses - and a hidden tile samples nothing.

### Instrument panels

- The Instrument Platform grows its second surface: click an
  instrument's tile and a framed detail panel unfolds beside the dock,
  streamed by the same process over the same socket, with the shell
  drawing the chiseled chrome and owning placement and every dismissal
  gesture (click-away, Escape, a tile re-click). One panel on screen
  at a time; a panel is a popover, not a window, and never takes
  keyboard focus. Every platform guarantee extends to it unchanged: the
  desktop never blocks on a panel, and a hung instrument's panel is
  torn down by the same ping machinery as its tile.
- The wire grew honestly: panel frames cross the transport as bounded
  top-to-bottom bands under one generation (a full-size panel cannot
  fit one datagram), flow control drops only whole generations so a
  repaint can never tear mid-stripe, panels get the one input kind
  tiles never needed (Motion, for hover), and the shell now says its
  protocol version in Welcome so a client can ask before speaking
  panel messages. Protocol 2, specified byte-for-byte in
  `docs/dockapp-protocol.md` section 11.
- Both SDKs speak it: `open_panel` in Python and Go, with banding kept
  out of authors' hands - draw whole panels and the SDK slices them
  into maximal legal bands; `draw_rows` exists for the economical case
  (a hover highlight is one narrow band, not a repaint). Illegal states
  are unrepresentable: no frame before the grant, and a panel request
  on a version-1 shell fails with a clean local error instead of
  letting the shell disconnect the dockapp. The spec was negotiated
  adversarially - two consumers built from the written contract alone,
  every discrepancy ruled on and folded back until all three
  implementations agreed byte for byte - and the conformance probe
  (`chonk-panel-probe` in `chonk-testkit`) plays the whole conversation
  over a real socket.
- One limitation documented rather than fudged: a click inside an
  application window does not reach the shell on either backend, so it
  does not dismiss the panel. Escape, the tile re-click, and
  panel-replacement cover the gap.

### Real hardware

- wlr-output-management: `wlr-randr` and `kanshi` can list and
  configure outputs. Scale applies live - fractionally - and a
  primary-scale change restyles the whole chrome through the same path
  a config reload takes. What the backend cannot honor (disabling an
  output, transforms) is refused with `failed()` and a named log line
  rather than accepted and botched.
- Fractional scale, end to end: fractional-scale-v1 and viewporter are
  in, each output carries its own scale, and the integral `wl_output`
  fallback advertises the ceiling - a client without the protocol
  renders sharp and is downscaled, never blurred up. Watched on the
  wire: foot at preferred scale 1.5 committing pixel-crisp into its
  viewport, and a live `wlr-randr --scale 1.25` mid-session with the
  client re-committing to match.
- Real damage tracking with EGL buffer age: the nested renderer used to
  admit full-frame damage every frame. Now the idle desktop repaints
  about 1% of what it did, truly idle periods render nothing, and
  `CHONKSTEP_FULL_DAMAGE=1` remains the escape hatch.

### XWayland and application fixes

- The last EWMH gap: the XWayland root now listens as well as speaks.
  A pager's `_NET_ACTIVE_WINDOW` and `_NET_CURRENT_DESKTOP` client
  messages translate into the same activation and desktop-switch
  events every other path queues - proven by a pager round-tripping a
  workspace switch in the end-to-end suite. `wmctrl -a` commands the
  Wayland session now, not just reads it.
- Input no longer dies after a window minimizes itself (Edge's own
  menu does this): "no window focused" now clears seat focus and the
  Activated state for real, so hiding and restoring the same window is
  a real leave, enter, and configure. Miniaturized windows are told
  Suspended - the protocol's word for them.
- Explicit GPU sync on DRM sessions that support syncobj
  (`wp_linux_drm_syncobj_v1`), with a readiness blocker on every
  dmabuf commit - the canonical Chromium-on-NVIDIA flicker (sampling a
  buffer the client's GPU was still writing) fixed at the compositor.
- An XWayland client asking to be decorated is decorated: smithay's
  `is_decorated` answers "is client-side decorated", not "wants
  decorations", and the X11 arm of the chrome policy read it with the
  sign flipped - every XWayland client's decoration decision was
  inverted. Found by Spotify arriving frameless with `MWM_DECOR_ALL`
  set; one sign flip, verified live.
- GTK3 X11 apps are no longer double-scaled: the desktop's XSETTINGS
  manager already publishes the scaling story
  (`Gdk/WindowScalingFactor`, `Xft/DPI`, `Gdk/UnscaledDPI`), so the
  launcher stopped forcing `GDK_SCALE` on top of it - LibreOffice
  rendered at 4x on a scale-2 session while both were in play. Qt has
  no XSETTINGS client, so `QT_SCALE_FACTOR` stays.

### Showing up at the login screen

An audit of "chonkstep is not showing up in SDDM" from both install
routes, against what SDDM 0.21 actually does (its greeter lists a
session only after opening the entry as the `sddm` user, stats an
absolute `TryExec` when one is present, and hides the whole Wayland
list without `/dev/dri`; its stock session wrappers launch `Exec`
through an unquoted `exec $@`).

- `scripts/verify-install.sh` (also installed to
  `/usr/lib/chonkstep/`): proves an install is one a login manager
  will offer - entries present, world-readable, parseable, `Exec` /
  `TryExec` targets executable, `desktop-file-validate` clean - and
  diagnoses the machine: pickerless greeter themes, missing
  `/dev/dri`, `SessionDir=` overrides, which route owns the entries.
  `--root` points it at a staged tree, e.g. a makepkg `$pkgdir`.
- The Arch packaging's default recipe now builds: plain `makepkg -si`
  in `packaging/arch/` used to pick the release PKGBUILD, whose
  source is the `v0.2.0` tag archive - and upstream has no tags yet,
  so the download 404ed before anything was built. The branch-head
  recipe is now the default `PKGBUILD`; the pinned-tag twin waits as
  `PKGBUILD-release`.
- The `-git` package ships the portal backend map: `package()` had
  dropped `chonkstep-portals.conf`, so a `-git` install exported
  `XDG_CURRENT_DESKTOP=chonkstep` with no portal config behind it and
  screen sharing silently failed. Both recipes also gained `bash` in
  `depends` and `TryExec=` in their session entries - safe for a
  package (pacman guarantees the target under `/usr`), and left out
  of the checkout installer's entries on purpose: the greeter's
  `TryExec` stat runs as the `sddm` user, and a default 0700 home
  (login.defs `HOME_MODE`) would fail it and hide the session.
- `scripts/install.sh` writes the session entries with an explicit
  mode instead of piping through `sudo tee`: tee honors the caller's
  umask, and a hardened umask (077) left entries 0600 - unreadable by
  the greeter user, which renders as a blank row or no row at all.
  It also refuses a checkout path containing whitespace, quotes or
  backslashes, with the reason spelled out (no `.desktop` escaping
  survives SDDM's `exec $@` wrapper), and its closing instructions
  now tell the truth on Omarchy 4: SDDM is enabled there, but the
  stock greeter theme draws no session picker and hardwires
  interactive logins to Hyprland (uwsm), so the honest routes -
  `[Autologin] Session=chonkstep.desktop`, or a theme with a session
  menu - are printed instead of "pick chonkstep in the session list".
- The marker scripts reach the session they signal:
  `scripts/restart.sh` and `scripts/reload.sh` hardcoded
  `~/.local/state`, while the binaries resolve `XDG_STATE_HOME`
  first - with that variable set, a restart or reload request was
  dropped in a directory the running session never polls. Both now
  resolve the same way the binaries do, as do the log paths in
  `scripts/xsession.sh` and `scripts/start-session.sh`.
- `docs/quickstart.md`'s login section walks the three machines a
  user actually has (a DM with a picker, Omarchy 4's pickerless SDDM,
  no DM at all) and ends in a troubleshooting list ordered by how the
  failures bite, with the verifier as its first line.

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
