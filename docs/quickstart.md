# Quickstart: install to first hour

This walks the whole arc: install, log in, learn the ten keybindings
that matter, turn on the two settings that make the desktop yours, pick
a theme, and put something of your own in the dock. Nothing here is
required — the desktop runs with no config file at all — but this is
the path from "installed" to "mine".

## 1. Install

**Omarchy / GitHub release package (x86-64 or ARM64/AArch64):**

```sh
chonkstep_dir="$(mktemp -d)"
chonkstep_arch="$(uname -m)"
chonkstep_pkg="chonkstep-0.2.0-2-$chonkstep_arch.pkg.tar.zst"
curl -fL -o "$chonkstep_dir/$chonkstep_pkg" \
  "https://github.com/iconidentify/chonkstep/releases/download/preview-v0.2.0-r2/$chonkstep_pkg"
curl -fL -o "$chonkstep_dir/SHA256SUMS" \
  "https://github.com/iconidentify/chonkstep/releases/download/preview-v0.2.0-r2/SHA256SUMS"
(cd "$chonkstep_dir" && sha256sum --ignore-missing --check SHA256SUMS)
sudo pacman -U "$chonkstep_dir/$chonkstep_pkg"
omarchy install desktop-chonkstep
```

The release also carries GitHub provenance attestations. Downloading before
calling pacman is deliberate: Arch applies its repository signature policy to
remote `pacman -U` URLs, while this temporary GitHub path uses release
checksums and provenance attestations rather than pacman-key signatures. The
last command is chonkstep's explicit, idempotent Omarchy integration step. It
configures SDDM for the managed uwsm session and prints the exact removal
command. None of these commands modifies `/usr/share/omarchy`, `~/.config/hypr`,
or `~/.config/omarchy`. Upstream Omarchy does not yet ship an official ARM64
installation, but the AArch64 ChonkStep package is compiled and checked
natively against Arch Linux ARM.

After the stable AUR entry is published, pacman installation can instead be:

```sh
omarchy pkg aur add chonkstep
```

**Branch-head Arch package** (builds from source via `makepkg`):

```sh
git clone https://github.com/iconidentify/chonkstep.git
cd chonkstep/packaging/arch
makepkg -si   # builds the branch head (pkgname chonkstep-git)
```

If you installed the checkout route first (below), remove its three
session entries before installing the package, or pacman will refuse
with "exists in filesystem":

```sh
sudo rm /usr/share/xsessions/chonkstep.desktop \
        /usr/share/wayland-sessions/chonkstep.desktop \
        /usr/share/wayland-sessions/chonkstep-uwsm.desktop
```

**From a checkout** (Omarchy or any Arch; nothing is copied out of the
repo, and `scripts/update.sh` is the upgrade story):

```sh
git clone https://github.com/iconidentify/chonkstep.git
cd chonkstep
scripts/install.sh
```

Either way you get an X11 session, a direct Wayland recovery session,
and the preferred `chonkstep (uwsm)` session, plus the `chonk-get`
dockapp installer and `omarchy-export-themes`. The package puts binaries in `/usr/bin` and
session scripts in `/usr/lib/chonkstep/`; the checkout installer points
the session entries back into the repo and links the two tools into
`~/.local/bin` (it tells you if that is not on your `PATH`; from a
checkout they are also always `scripts/chonk-get` and
`target/release/omarchy-export-themes`).

## 2. Log in

Both installs register three real login sessions:
`/usr/share/xsessions/chonkstep.desktop` and
`/usr/share/wayland-sessions/{chonkstep,chonkstep-uwsm}.desktop`.
The uwsm entry is preferred: it owns `graphical-session.target`, desktop
autostart, application lifetime, clean logout, and activation-environment
cleanup. The direct entry remains for non-systemd and recovery use.

- **A display manager with a session picker:** log out and choose
  `chonkstep (uwsm)`. Choose `chonkstep (Wayland)` only for the direct
  fallback, or `chonkstep` for X11.
- **Omarchy 4 / SDDM:** both installation routes add late-sorting
  `zz-chonkstep-*` snippets without patching Omarchy's theme or login
  configuration. If Omarchy configured autologin, its user is preserved and
  the session becomes `chonkstep (uwsm)`. Otherwise the chonkstep picker is
  shown and defaults to that exact managed entry. Undo the AUR route with:

  ```sh
  omarchy remove desktop-chonkstep
  ```

  The reason a separate picker is necessary is precise: Omarchy's
  `Main.qml` exposes no session control and computes `sessionIndex` by
  returning the first session whose display name contains the literal
  substring `"uwsm"`, otherwise `sessionModel.lastIndex`. Thus
  `RememberLastSession=true` cannot select a nonmatching session.
  Chonkstep never edits that file.
- **No display manager at all** (Omarchy 3 and earlier, minimal
  Arch): switch to a TTY (Ctrl+Alt+F3), log in, and run
  `exec uwsm start -g -1 -e -D chonkstep chonkstep.desktop`. Shell-profile integration may
  instead use `uwsm check may-start && uwsm select`, followed by
  `exec uwsm start default`. On a non-systemd machine use
  `exec /usr/lib/chonkstep/chonkstep-session` (package) or
  `exec scripts/chonkstep-session` (checkout). No `startx` for the
  Wayland session—the compositor is the display server. The X11
  session is `startx /usr/lib/chonkstep/xsession.sh` (or
  `scripts/start-session.sh` in a checkout, which wraps exactly
  that). To get a graphical picker instead, install one and enable
  it: `sudo pacman -S sddm && sudo systemctl enable sddm.service`
  (disable any other display manager first — only one can own the
  boot).
- **Just looking?** Run `chonkstep-wayland` from a terminal inside
  your current desktop: it notices there is already a desktop here and
  opens a window that is its screen — same chrome, dock, menus,
  themes, with X11 apps through XWayland.

Seat access needs no setup on any systemd machine — logind hands the
session its devices. Without logind, enable `seatd` and join the
`seat` group.

### chonkstep is not in the session list

Run the verifier first — it mechanically checks everything below and
diagnoses the machine:

```sh
scripts/verify-install.sh              # checkout
/usr/lib/chonkstep/verify-install.sh   # package
```

What it checks, and the fixes, in the order they bite:

- **Are the entries actually there?** `ls
  /usr/share/xsessions/chonkstep.desktop
  /usr/share/wayland-sessions/chonkstep.desktop`. Missing means the
  install never ran to completion — re-run it.
- **Is the greeter allowed to read them?** The greeter runs as its own
  user (`sddm`), so the entries must be world-readable (`0644`). An
  install piped through `tee` under a hardened `umask` used to leave
  them `0600`; `sudo chmod 644` both files. (Current `install.sh`
  writes them with an explicit mode.)
- **Do the Exec targets exist and execute?** A moved or deleted
  checkout leaves entries pointing at nothing: SDDM still lists them
  (it does not check `Exec` before launch), the session dies
  instantly, and you bounce back to the greeter. Re-run
  `scripts/install.sh` from the checkout's new home.
- **Is the Wayland list hidden entirely?** SDDM only lists
  `wayland-sessions` when `/dev/dri` exists. A VM without a virtual
  GPU has no `/dev/dri` — the X11 session still shows.
- **Is SDDM even looking in the right place?** `SessionDir=` in
  `/etc/sddm.conf` or `/etc/sddm.conf.d/` overrides the default
  `/usr/share/xsessions` + `/usr/share/wayland-sessions` search path.
- **Is another display manager winning the boot?** `systemctl status
  display-manager` names the one systemd actually starts.
- **Which login path is active?** The verifier reports exactly one of
  chonkstep picker, SDDM autologin (and its selected session), or no
  display manager. A `desktop-file-validate` complaint about `DesktopNames` is
  *not* your problem: that key is a session-file convention the
  validator doesn't know, and Hyprland's own entry fails the same
  way.

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

Right-click the desktop → **Theme**, and pick one of the eight:
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

The tiles that ship with the desktop have panels too. Right-click
**LNK** for the link panel: wifi networks in range, your saved
connection profiles and WireGuard tunnels, and the Tailscale row, each
a switch you can throw from the dock. Joining a new secured network
opens a small passphrase dialog, because a panel takes no keyboard by
design. [link-panel.md](link-panel.md) is the whole story — including
the one-time `sudo tailscale set --operator=$USER` grant that
`scripts/install.sh` offers you, without which Tailscale can be shown
but not toggled.

Try the shipped ones (paths are relative to a checkout; `chonk-get` is
`~/.local/bin/chonk-get` after `scripts/install.sh`, `/usr/bin/chonk-get`
from the package, or simply `scripts/chonk-get` in the checkout):

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

## 7. On Omarchy

On an Omarchy 4 machine the Wayland session stands where Hyprland
stood, and nothing below needs configuring — it is all on by default
and inert anywhere Omarchy is absent. The README's
[Installing on Omarchy](../README.md#installing-on-omarchy-or-any-arch)
section is the long form.

- **The `Omarchy` submenu.** Right-click the desktop: the `Omarchy`
  row *is* Omarchy's menu (Learn, Trigger, Style, Setup, Install,
  Remove, Update, About, System), read from Omarchy's own definition
  and run the way Omarchy runs it, through `bash -lc`, so your login
  shell's `OMARCHY_PATH` is what makes it work. Only the rows that
  would command Hyprland are left out. `omarchy_menu = false` hides
  it.
- **Omarchy's theme and background.** `theme = "omarchy"`, or the
  `Omarchy (...)` row in the Theme submenu, dresses the desk in the
  palette Omarchy is currently wearing and follows `omarchy-theme-set`
  live, with Omarchy's current background as the wallpaper; the
  Wallpaper submenu offers `Omarchy's Background` on its own too. The
  other way round, `omarchy-export-themes` writes the eight built-ins
  as Omarchy themes. Details in [appearance.md](appearance.md).
- **Omarchy's shell, hosted.** The session starts Omarchy's Quickshell
  shell as Hyprland would, so the panels, pickers, OSD, notifications
  and lock screen the menu ends in are all here. `omarchy_shell =
  false` leaves it to you; a shell that dies is not relaunched under
  chonkstep (`omarchy-restart-shell` brings it back).
- **The bar, on request.** The shell's bar starts hidden — the Dock
  holds that corner — and the root menu's `Omarchy Bar` row switches
  it on, remembered in chonkstep's own state. When it is on, the Dock
  hangs under it.
- **Two widgets for that bar.** `chonkstep.workspaces` and
  `chonkstep.theme` under `omarchy/plugins/` replace the workspace
  strip and name the theme, reading chonkstep's control socket instead
  of Hyprland; [omarchy/README.md](../omarchy/README.md) has the
  symlink-and-enable steps.
- **The control socket.** Anything can watch the desktop's state —
  workspaces, outputs, the focused window, the theme — as one JSON line
  per fact at `$XDG_RUNTIME_DIR/chonkstep/control-<display>.sock`;
  `socat - UNIX-CONNECT:$CHONKSTEP_CONTROL_SOCKET` shows it, and
  [control-socket.md](control-socket.md) is the contract.

## Where things live

| Thing                  | Path                                             |
|------------------------|--------------------------------------------------|
| Config                 | `~/.config/chonkstep/config.toml`                |
| Config template        | `/usr/share/doc/chonkstep/config.example.toml` (package) or `docs/config.example.toml` |
| Session logs           | `~/.local/state/chonkstep/*.log`                 |
| Persisted choices      | `$XDG_STATE_HOME/chonkstep/{theme,wallpaper,omarchy-bar,appearance}` (`~/.local/state/chonkstep/` when unset) — the theme and wallpaper picked from the menu, whether Omarchy's bar is shown, the light/dark mode |
| Control socket         | `$XDG_RUNTIME_DIR/chonkstep/control-<display>.sock` (also in `CHONKSTEP_CONTROL_SOCKET` for everything the shell launches) |
| Dockapp registrations  | `~/.config/chonkstep/dockapps/*.dockapp`         |
| Dockapp sources        | `~/.local/share/chonkstep/dockapps/`             |
