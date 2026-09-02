# chonkstep.workspaces

A workspace strip for the [Omarchy](https://omarchy.org) bar, drawn from the
[chonkstep](https://github.com/iconidentify/chonkstep) compositor instead of
Hyprland: one numbered button per workspace, the focused one marked with the
same filled dot `omarchy.workspaces` uses, empty ones dimmed. Click a button
to switch. There is no fixed five: chonkstep grows workspaces on demand, and
the strip shows exactly the workspaces that exist.

It reads chonkstep's **control socket** (`docs/control-socket.md` in the
chonkstep repository, protocol 1) and needs nothing else. With no socket to
connect to — a bar running under another compositor, or chonkstep in the
middle of a restart — the widget has zero width and draws nothing.

## Installing

Once published as its own repository:

```sh
omarchy plugin add https://github.com/<org>/chonkstep.workspaces --enable
```

From a chonkstep checkout, symlink the plugin directory into Omarchy's
plugin folder under its manifest id, then enable it in place of the Hyprland
strip:

```sh
ln -s "$PWD/omarchy/plugins/chonkstep.workspaces" ~/.config/omarchy/plugins/chonkstep.workspaces
omarchy-shell shell rescanPlugins
omarchy plugin disable omarchy.workspaces
omarchy plugin enable  chonkstep.workspaces --section left --after omarchy.menu
```

Under chonkstep the Omarchy bar starts hidden; the root menu's `Omarchy Bar`
row shows it.

## Settings

| key         | type    | default | meaning                                                                   |
|-------------|---------|---------|---------------------------------------------------------------------------|
| `hideEmpty` | boolean | `false` | Leave workspaces with no windows out of the strip. The current one always shows. |

Set a value with `omarchy bar set`, passing `--json` so a boolean is stored
as one (without it the value is stored as the string `"true"`, which the
widget reads as off):

```sh
omarchy bar set chonkstep.workspaces hideEmpty true --json
```

## License

GPL-3.0-only; see `LICENSE`.
