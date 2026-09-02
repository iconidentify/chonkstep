# chonkstep.theme

The name of the active [chonkstep](https://github.com/iconidentify/chonkstep)
theme on the [Omarchy](https://omarchy.org) bar — `NeXTSTEP Classic · dark`,
say — with a tooltip naming the theme id and whether the session is following
Omarchy's palette. A presence indicator, not a control: theme switching stays
with chonkstep's own Themes menu.

It reads chonkstep's **control socket** (`docs/control-socket.md` in the
chonkstep repository, protocol 1) and needs nothing else. With no socket to
connect to the widget has zero width and draws nothing; it also steps aside
on a vertical bar, where a theme name has no room.

## Installing

Once published as its own repository:

```sh
omarchy plugin add https://github.com/<org>/chonkstep.theme --enable
```

From a chonkstep checkout, symlink the plugin directory into Omarchy's
plugin folder under its manifest id and enable it:

```sh
ln -s "$PWD/omarchy/plugins/chonkstep.theme" ~/.config/omarchy/plugins/chonkstep.theme
omarchy-shell shell rescanPlugins
omarchy plugin enable chonkstep.theme --section right
```

Under chonkstep the Omarchy bar starts hidden; the root menu's `Omarchy Bar`
row shows it.

## Settings

| key              | type    | default | meaning                                                  |
|------------------|---------|---------|----------------------------------------------------------|
| `showAppearance` | boolean | `true`  | Append the theme's `dark` or `light` appearance after its name. |

Set a value with `omarchy bar set`, passing `--json` so a boolean is stored
as one (without it the value is stored as a string, and any string — `"true"`
included — reads as off):

```sh
omarchy bar set chonkstep.theme showAppearance false --json
```

## License

GPL-3.0-only; see `LICENSE`.
