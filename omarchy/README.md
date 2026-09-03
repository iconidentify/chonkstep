# chonkstep plugins for the Omarchy shell

Omarchy's bar ships a workspace strip that talks to Hyprland. Under chonkstep
that widget has nothing to talk to, so this directory carries replacements
that read chonkstep's own control socket instead (`docs/control-socket.md`
in `docs/` at the repository root is the protocol; the plugins are clients of
it and change nothing about it).

This directory also carries the other half of the same relationship:
where one of Omarchy's own *scripts* still asks a question this session
cannot answer, `shims/` holds a drop-in that answers it portably, and
`upstream/` holds the same fix prepared as a patch to Omarchy itself.
Most of what used to live in both is gone: chonkstep now answers
Hyprland's IPC (`docs/hyprland-ipc.md`), so the scripts that only ever
needed `hyprctl` work unmodified. The rule is the same throughout —
nothing under `/usr/share/omarchy` or `/usr/bin` is ever written, and
every piece here is opt-in.

```
omarchy/
  plugins/
    chonkstep.workspaces/   bar widget: one button per workspace, click to switch
    chonkstep.theme/        bar widget: the active theme's name ("NeXTSTEP Classic · dark")
                            (each carries its own README, LICENSE and ControlSocket.qml,
                            so it can be split out and published on its own)
  shims/                    one script: logout, whose `uwsm stop` is a no-op in a
                            session uwsm did not start. Symlinked onto PATH by
                            shims/install.sh; see docs/omarchy-integration.md.
  upstream/                 two fixes as a reviewable patch set for Omarchy — logout,
                            and the nightlight fallback chain that no compositor IPC
                            can reach. Omarchy's path kept first in both. Prepared,
                            not sent.
  tools/
    fake-control-socket.py  a stand-in server for developing without a compositor
    check-plugins.sh        manifest validation, qmllint, and a diff of the shared file
```

Both plugins target Omarchy 4.0.x, whose shell is Quickshell 0.3.1. Neither
imports `Quickshell.Hyprland`, so they load unchanged on a bar running under
any compositor; with no chonkstep socket to connect to they simply render
nothing (zero width), which is also what they do while chonkstep restarts.

## The widgets

**chonkstep.workspaces** draws the `workspaces` facet the same way
`omarchy.workspaces` draws Hyprland's: a numbered button per workspace
(1-based labels on screen; the wire is 0-based), the focused one replaced
with the same filled-dot glyph, empty ones dimmed. A click sends
`{"request":"focus-workspace","index":N}`. Unlike the first-party widget it
never shows a fixed five: it shows exactly the workspaces the compositor
reports.

Setting: `hideEmpty` (boolean, default `false`) hides workspaces with no
windows other than the focused one.

**chonkstep.theme** shows the `theme` facet's name and appearance as plain
text, right section by default. It is a presence indicator, not a control;
theme switching stays with chonkstep's own menu. Setting: `showAppearance`
(boolean, default `true`).

Each plugin carries its own copy of `ControlSocket.qml`, the component that
owns the connection. Omarchy installs every plugin as its own directory and a
plugin cannot import files outside it, so there is nowhere for a shared copy
to live. The copies must stay byte-identical; `tools/check-plugins.sh`
enforces that.

## Showing the bar

A chonkstep session that finds Omarchy installed hosts the Omarchy shell
itself, but keeps its bar **hidden** to start with: the desk already has a
Dock and a Clip in the corners the bar would want. The root menu's
`Omarchy Bar` row shows it (and hides it again); the choice is remembered in
chonkstep's own state, not Omarchy's, so it does not follow you into a
Hyprland session. Until the bar is shown, an enabled widget is running but
has nowhere to be seen.

## Installing for development

Omarchy loads third-party plugins from `~/.config/omarchy/plugins/<id>/`,
where `<id>` is the manifest's `id`. A symlink to this checkout is enough:

```sh
ln -s "$PWD/omarchy/plugins/chonkstep.workspaces" ~/.config/omarchy/plugins/chonkstep.workspaces
ln -s "$PWD/omarchy/plugins/chonkstep.theme"      ~/.config/omarchy/plugins/chonkstep.theme

# The shell watches that directory and sees the new symlink appear. What
# it never sees is an edit made through the symlink, so a rescan is the
# habit to form; it is cheap and idempotent.
omarchy-shell shell rescanPlugins
omarchy plugin list | grep chonkstep

omarchy plugin disable omarchy.workspaces
omarchy plugin enable  chonkstep.workspaces --section left --after omarchy.menu
omarchy plugin enable  chonkstep.theme      --section right
```

`omarchy plugin enable` edits `~/.config/omarchy/shell.json` and the bar
re-lays itself out immediately; no restart. Enabled means present in the
layout, so `omarchy plugin disable chonkstep.workspaces` is the undo.

Two things to know about the symlink route:

- `omarchy plugin validate` refuses a symlinked folder ("symlinks are not
  allowed inside a plugin folder"). Validate the checkout path instead:
  `omarchy plugin validate omarchy/plugins/chonkstep.workspaces`.
- Edits to the QML are not noticed through the symlink (the shell watches
  the plugins directory, not the files behind a link). Pick them up with
  `omarchy-shell shell rescanPlugins`, which unloads the plugin widgets,
  clears Quickshell's component cache and rescans, so the edited file is
  compiled afresh; `omarchy plugin disable` + `enable` only re-creates the
  widget from the cached component and shows the old code.

### Editing shell.json by hand

The layout entry is the widget id plus any settings; this replaces
`omarchy.workspaces` in the default left section:

```json
"bar": {
  "layout": {
    "left": [
      { "id": "omarchy.menu" },
      { "id": "chonkstep.workspaces", "hideEmpty": false }
    ]
  }
}
```

Settings can also be set live from the CLI:

```sh
omarchy bar set chonkstep.workspaces hideEmpty true --json
omarchy bar set chonkstep.theme showAppearance false --json
```

`--json` matters for a boolean: without it the value is stored as the string
`"true"`, and the widgets read a setting as on only when it is the JSON
`true` (the same `=== true` test Omarchy's own widgets use), so a stringly
`"true"` is off. The manifests declare the settings under `barWidget.schema`;
Omarchy 4.0.x registers that field but has no UI that renders it yet, so the
CLI and the file are the two ways to change a setting today. The QML
fallbacks (`setting("hideEmpty", false)`) are the effective defaults — the
shell reads a manifest's `schema` as metadata and never merges declared
defaults into a widget's settings.

## Installing from git

`omarchy plugin add <git-url>` clones the **repository root** into
`~/.config/omarchy/plugins/<manifest id>/`, so the `manifest.json` must sit
at the top of the repository being added. These plugins live in a
subdirectory of the chonkstep monorepo, which `plugin add` cannot consume
directly. Publish each one as its own repository, for example with

```sh
git subtree split --prefix=omarchy/plugins/chonkstep.workspaces -b publish/chonkstep.workspaces
git push <plugin-remote> publish/chonkstep.workspaces:main
```

after which users install with

```sh
omarchy plugin add https://github.com/<org>/chonkstep.workspaces --enable
```

Each plugin directory carries its own `README.md` and `LICENSE` so the split
repository is complete on its own.

The repository name does not matter to Omarchy: the install directory, the
`omarchy plugin` commands, and the `shell.json` entries all use the manifest
`id` (`chonkstep.workspaces`), and `omarchy plugin update` pulls whatever
remote that directory was cloned from. Ids are validated against
`^[A-Za-z0-9][A-Za-z0-9._-]*$`, may not contain `..`, and `omarchy.*` is
reserved.

## Developing without a compositor

`tools/fake-control-socket.py` speaks protocol 1 from the Python standard
library alone. On connect it sends `hello` and the full snapshot; it answers
`snapshot` and `focus-workspace` (with an `error` for anything out of range
or unknown, and — as the spec requires — an acknowledgement to the asker
alone when the named workspace is already active), enforces the line limits
the real shell does, and logs every request it receives to stderr.

It listens on `$XDG_RUNTIME_DIR/chonkstep/control-fake.sock` by default,
next to the session's `control-<display>.sock` and never on it: your own
desktop is a chonkstep session, and `CHONKSTEP_CONTROL_SOCKET` in your
terminal names its live socket, so the fake deliberately ignores that
variable. A `--socket` path that answers a connect is refused rather than
replaced; only a stale socket file is cleaned up, and on exit the fake
removes only the file it bound.

```sh
# Static state: three workspaces, windows 3/0/1, the first focused.
omarchy/tools/fake-control-socket.py --windows 3,0,1 --active 0

# Cycle through a scripted timeline (focus changes, a fourth workspace
# appearing and disappearing, a theme change) every two seconds.
omarchy/tools/fake-control-socket.py --script --interval 2

# Or feed state changes by hand; each line is one nudge.
omarchy/tools/fake-control-socket.py
{"active": 2}
{"windows": [1, 1, 0, 4]}
{"theme": {"name": "Ristretto", "appearance": "dark"}}
```

`--script FILE` reads a JSON array of nudges instead of the built-in
timeline; `--protocol 2` announces a version the plugins do not speak, for
checking that they hang up rather than guess. `--help` lists the rest.

Point the plugins at it by exporting
`CHONKSTEP_CONTROL_SOCKET=$XDG_RUNTIME_DIR/chonkstep/control-fake.sock` in
the environment the shell starts from. Without that variable the plugins
derive the session socket path from `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY`
exactly as the spec describes.

To try the widgets without disturbing a running desktop, start a nested
chonkstep (`CHONKSTEP_BACKEND=winit` with scratch `XDG_CONFIG_HOME` and
`XDG_STATE_HOME`). Two things about the scratch environment:

- A chonkstep that finds Omarchy installed launches `omarchy-launch-shell`
  itself (`omarchy_shell = true` is the default). A nested session whose
  shell you mean to start by hand must decline that, or you get two: put
  `omarchy_shell = false` in the scratch `$XDG_CONFIG_HOME/chonkstep/config.toml`
  before starting it.
- The environment must carry `OMARCHY_PATH` (the login shell normally sets
  it; `/usr/share/omarchy` for the package): the shell and every
  `omarchy-*` script find the tree through it.

Then inside the nested session run the Omarchy shell with a scratch `HOME`
holding only a symlink to `~/.local/share/omarchy` (the pre-package fallback
location, so that resolves too), a copy of `~/.local/state/omarchy/current`,
a `shell.json`, and the plugin symlinks:

```sh
HOME=/path/to/scratch-home WAYLAND_DISPLAY=wayland-N OMARCHY_PATH="$OMARCHY_PATH" \
CHONKSTEP_CONTROL_SOCKET=$XDG_RUNTIME_DIR/chonkstep/control-fake.sock \
QS_DISABLE_FILE_WATCHER=1 QS_NO_RELOAD_POPUP=1 \
  dbus-run-session -- quickshell -p "$OMARCHY_PATH/shell"
```

`dbus-run-session` keeps the second shell's notification and polkit
services off the live session bus; Quickshell scopes `qs ipc` (and so the
`omarchy plugin` commands) by `WAYLAND_DISPLAY`, so with the nested display
exported they reach only the nested shell.

## Checks

```sh
omarchy/tools/check-plugins.sh            # manifests, qmllint, identical ControlSocket copies
VERBOSE=1 omarchy/tools/check-plugins.sh  # full qmllint output
```

qmllint reports warnings of three kinds, none of them ours to fix:
`[unqualified]` for `root.*` references inside the Repeater's delegate and
the Loader's `Socket` (Omarchy's own widgets produce the same, since the
shell does not use `pragma ComponentBehavior: Bound`); `[missing-property]`
for `link.item.connected` and friends, because a Loader's `item` is typed
as a bare `QObject` and the linter cannot see the `Socket` behind it; and
`[signal-handler-parameters]` on `onError`, whose `QLocalSocket::LocalSocketError`
parameter type is not exposed to QML. The script counts warnings and fails
only on errors.
