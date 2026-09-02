# chonkstep plugins for the Omarchy shell

Omarchy's bar ships a workspace strip that talks to Hyprland. Under chonkstep
that widget has nothing to talk to, so this directory carries replacements
that read chonkstep's own control socket instead (`docs/control-socket.md`
at the repository root is the protocol; the plugins are clients of it and
change nothing about it).

```
omarchy/
  plugins/
    chonkstep.workspaces/   bar widget: one button per workspace, click to switch
    chonkstep.theme/        bar widget: the active theme's name ("NeXTSTEP Classic · dark")
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

## Installing for development

Omarchy loads third-party plugins from `~/.config/omarchy/plugins/<id>/`,
where `<id>` is the manifest's `id`. A symlink to this checkout is enough:

```sh
ln -s "$PWD/omarchy/plugins/chonkstep.workspaces" ~/.config/omarchy/plugins/chonkstep.workspaces
ln -s "$PWD/omarchy/plugins/chonkstep.theme"      ~/.config/omarchy/plugins/chonkstep.theme

# The shell watches that directory, but the watcher does not always see a
# new symlink; a rescan is cheap and idempotent.
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
- Edits to the QML are picked up on the next shell restart
  (`omarchy-restart-shell`, or `omarchy plugin disable` + `enable` to
  recreate just the widget).

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

Settings can also be flipped live:
`omarchy-shell shell setBarWidget chonkstep.workspaces hideEmpty true '{}'`.
The manifests declare the settings under `barWidget.schema`; Omarchy 4.0.1
registers that field but has no UI that renders it yet, so the CLI and the
file are the two ways to change a setting today.

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
or unknown), enforces the line limits the real shell does, and logs every
request it receives to stderr.

```sh
# Static state: three workspaces, windows 3/0/1, the first focused.
omarchy/tools/fake-control-socket.py --socket /tmp/ctl-fake.sock --windows 3,0,1 --active 0

# Cycle through a scripted timeline (focus changes, a fourth workspace
# appearing and disappearing, a theme change) every two seconds.
omarchy/tools/fake-control-socket.py --socket /tmp/ctl-fake.sock --script --interval 2

# Or feed state changes by hand; each line is one nudge.
omarchy/tools/fake-control-socket.py --socket /tmp/ctl-fake.sock
{"active": 2}
{"windows": [1, 1, 0, 4]}
{"theme": {"name": "Ristretto", "appearance": "dark"}}
```

`--script FILE` reads a JSON array of nudges instead of the built-in
timeline; `--protocol 2` announces a version the plugins do not speak, for
checking that they hang up rather than guess. `--help` lists the rest.

Point the plugins at it by exporting `CHONKSTEP_CONTROL_SOCKET=/tmp/ctl-fake.sock`
in the environment the shell starts from. Without that variable the plugins
derive the session socket path from `XDG_RUNTIME_DIR` and `WAYLAND_DISPLAY`
exactly as the spec describes.

To try the widgets without disturbing a running desktop, start a nested
chonkstep (`CHONKSTEP_BACKEND=winit` with scratch `XDG_CONFIG_HOME` and
`XDG_STATE_HOME`), then inside it run the Omarchy shell with a scratch `HOME`
holding only a symlink to `~/.local/share/omarchy`, a copy of
`~/.local/state/omarchy/current`, a `shell.json`, and the plugin symlinks:

```sh
HOME=/path/to/scratch-home WAYLAND_DISPLAY=wayland-N \
CHONKSTEP_CONTROL_SOCKET=/tmp/ctl-fake.sock QS_DISABLE_FILE_WATCHER=1 QS_NO_RELOAD_POPUP=1 \
  dbus-run-session -- quickshell -p ~/.local/share/omarchy/shell
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

qmllint reports `[unqualified]` warnings for `root.*` references inside the
Repeater's delegate and the Loader's `Socket`; Omarchy's own widgets produce
the same class of warning (the shell does not use
`pragma ComponentBehavior: Bound`), and the script fails only on errors.
