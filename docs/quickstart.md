# Quickstart: install to first hour

This walks the whole arc: install, log in, learn the ten keybindings
that matter, turn on the two settings that make the desktop yours, pick
a theme, and put something of your own in the dock. Nothing here is
required — the desktop runs with no config file at all — but this is
the path from "installed" to "mine".

## 1. Install

**Arch package** (builds from source via `makepkg`):

```sh
git clone https://github.com/iconidentify/chonkstep.git
cd chonkstep/packaging/arch
makepkg -si -p PKGBUILD-git   # the branch head
# the plain PKGBUILD is the release shape: it pins the v0.2.0 tag
# once that is published
```

**From a checkout** (Omarchy or any Arch; nothing is copied out of the
repo, and `scripts/update.sh` is the upgrade story):

```sh
git clone https://github.com/iconidentify/chonkstep.git
cd chonkstep
scripts/install.sh
```

Either way you get two real login sessions — `chonkstep` (X11) and
`chonkstep (Wayland)` — plus the `chonk-get` dockapp installer. The
package puts binaries in `/usr/bin` and session scripts in
`/usr/lib/chonkstep/`; the checkout installer points the session
entries back into the repo.

## 2. Log in

- **With a display manager** (sddm, gdm, lightdm, ...): log out and
  pick `chonkstep` or `chonkstep (Wayland)` from the session list.
- **Without one** (stock Omarchy boots straight into Hyprland): switch
  to a TTY (Ctrl+Alt+F3), log in, and run
  `exec /usr/lib/chonkstep/wayland-session.sh` (package) or
  `exec scripts/wayland-session.sh` (checkout). No `startx` for the
  Wayland one — the compositor *is* the display server. The X11
  session is `startx /usr/lib/chonkstep/xsession.sh`.
- **Just looking?** Run `chonkstep-wayland` from a terminal inside
  your current desktop: it notices there is already a desktop here and
  opens a window that is its screen — same chrome, dock, menus,
  themes, with X11 apps through XWayland.

Seat access needs no setup on any systemd machine — logind hands the
session its devices. Without logind, enable `seatd` and join the
`seat` group.

## 3. The keybinding card

The full card is [keybindings.md](keybindings.md); these are the ones
to learn first:

| Keys               | Does                                   |
|--------------------|----------------------------------------|
| `alt+shift+return` | terminal                               |
| `super+up`         | the Overview: every window as a card   |
| `alt+tab` (hold)   | the modal window switcher              |
| `alt+shift+q`      | close                                  |
| `alt+shift+m`      | miniaturize to an icon tile            |
| `alt+ctrl+left/right` | previous / next workspace           |
| `alt+shift+left/right` | carry the window along              |

Right-click the desktop for the root menu (every installed application
is in it, generated from the system's `.desktop` entries); right-click
any titlebar for the window commands menu.

## 4. Make the session yours: `restore_session` and `lock_command`

Your config lives at `~/.config/chonkstep/config.toml`. The checkout
installer seeds it from the fully commented example; on a package
install, copy the template once:

```sh
mkdir -p ~/.config/chonkstep
cp /usr/share/doc/chonkstep/config.example.toml ~/.config/chonkstep/config.toml
```

Then turn on the two settings that make sessions durable:

```toml
# Record every window's app, geometry, workspace and shape as you
# work, and bring that layout back at the next login -- and after a
# crash the watchdog recovered from. Never resurrects a window you
# deliberately closed. Off by default because a session that spawns
# apps you did not just ask for is something to opt into.
restore_session = true

# Wayland session only: when the compositor crashes, the session
# script restarts it -- and with this set (any ext-session-lock
# locker), the recovered session comes back LOCKED instead of exposing
# your desktop to whoever walks past. Never runs on a normal login.
lock_command = "swaylock"
```

Every edit applies to the running session without restarting anything:
run `scripts/reload.sh` (`/usr/lib/chonkstep/reload.sh` from the
package), or bind the `reload` action to a key and the config applies
itself from the keyboard. On a HiDPI display, `scale = 2.0` scales the
chrome, dock, cursors and terminal font as one system — also live.

## 5. Theming

Right-click the desktop → **Themes**, and pick one of the eight:
NeXTSTEP Classic, Amber Phosphor, Teal Blueprint, Graphite, NeXT
Lavender, Jade Lacquer, Ivory Halftone (the light one), Indigo
Filament. It applies on the spot — chrome, menus, wallpaper, dock,
dockapps, and the palette of every terminal launched from then on —
with nothing closed and no restart. The pick is persisted and wins
over the config file's `theme =` line on later startups.

Every theme also has a **light and a dark rendition** — a second,
session-wide axis, independent of which theme you picked. Switch it
live from anywhere:

```sh
echo toggle > ~/.local/state/chonkstep/appearance-request
```

The desktop re-dresses in place — chrome, menus, wallpaper mood, the
dock — the terminals it spawned retint on the spot (scrollback
included), and GTK/portal applications follow through the standard
color-scheme setting. `light` and `dark` work in place of `toggle`,
and `appearance = "light"` in the config seeds a first session. With
nothing said, each theme wears its native mood — dark for seven of
the eight, light for Ivory Halftone. The whole contract, including
which applications follow live and which wait for their next launch,
is [appearance.md](appearance.md). And if you'd rather click than
echo: `chonk-get install examples/chonk-switch` (from a checkout)
puts a machined light/dark toggle in the dock.

## 6. Put something in the dock: `chonk-get`

The dock's tiles are **instruments**: separate processes that push
finished pixels over a private socket and get the desktop's theme,
scale, input and supervision in return. A crashed, hung or looping
tile shows a dead face in its tile; it cannot take the desktop down.
An instrument can also open a framed detail panel beside the dock
when you click its tile — streamed by the same process, dismissed by
the shell (click the tile again, or Escape).
You can write one in any language that can open a Unix socket —
[instrument-platform.md](instrument-platform.md) has a complete
Python one in ten lines.

Try the shipped ones (paths are relative to a checkout):

```sh
chonk-get install bindings/python        # a Python clock tile
chonk-get install examples/chonk-shelf   # the Shelf: clipboard history
chonk-get install examples/chonk-switch  # the light/dark toggle
chonk-get list
chonk-get remove py-dockclock
```

`chonk-get install <git-url>` works too: it clones, builds (build.sh,
Cargo or make), and registers. The tile appears at the next shell
restart. Dockapps are ordinary processes running as you — not
sandboxed — so install one with the same care as any other program.

## Where things live

| Thing                  | Path                                             |
|------------------------|--------------------------------------------------|
| Config                 | `~/.config/chonkstep/config.toml`                |
| Config template        | `/usr/share/doc/chonkstep/config.example.toml` (package) or `docs/config.example.toml` |
| Session logs           | `~/.local/state/chonkstep/*.log`                 |
| Dockapp registrations  | `~/.config/chonkstep/dockapps/*.dockapp`         |
| Dockapp sources        | `~/.local/share/chonkstep/dockapps/`             |
