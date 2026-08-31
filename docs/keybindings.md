# The keybinding card

The defaults below are read out of [config.example.toml](config.example.toml),
which is the authoritative, fully commented list — if this card and
that file ever disagree, the file wins. Every binding here can be
changed in `~/.config/chonkstep/config.toml`; entries merge over the
defaults, so listing one combo changes only that combo, and setting a
combo to `"none"` unbinds it.

## The defaults

| Binding            | Action                 | What it does                                  |
|--------------------|------------------------|-----------------------------------------------|
| `alt+shift+return` | `spawn-terminal`       | Launch the themed terminal                    |
| `alt+shift+q`      | `close`                | Close the focused window                      |
| `alt+shift+x`      | `toggle-maximize`      | Maximize / restore                            |
| `alt+shift+s`      | `toggle-shade`         | Roll the window up into its titlebar          |
| `alt+shift+m`      | `miniaturize`          | Collapse to an icon tile                      |
| `alt+shift+f`      | `toggle-fullscreen`    | Borderless fullscreen on / off                |
| `alt+ctrl+right`   | `workspace-next`       | Next workspace (grows on demand)              |
| `alt+ctrl+left`    | `workspace-prev`       | Previous workspace (stops at the first)       |
| `alt+shift+right`  | `workspace-carry-next` | Carry the focused window to the next          |
| `alt+shift+left`   | `workspace-carry-prev` | Carry the focused window back                 |
| `super+up`         | `overview`             | The modal Overview: every window as a card    |

Window-targeted actions (`close`, `toggle-maximize`, `toggle-shade`,
`miniaturize`, `toggle-fullscreen`) act on the focused window and do
nothing when no window is focused.

Two more actions exist and are deliberately unbound by default —
give them keys in your config:

| Action    | What it does                                                             |
|-----------|--------------------------------------------------------------------------|
| `reload`  | Re-read the config file and apply all of it, live — nothing closed       |
| `restart` | Re-exec the on-disk binary, for picking up a new build                   |

## Fixed modal machinery (not rebindable)

- **Alt+Tab** — hold Alt, Tab through the switch panel of live window
  thumbnails; Shift+Tab steps backward, Escape cancels, releasing Alt
  commits.
- **Inside the Overview** (`super+up`) — arrows move the selection,
  Return (or clicking a card) focuses and raises, right-click opens
  the window-commands menu, clicking a workspace tile switches desks,
  Escape (or the binding again, or any other key) dismisses.
- **Ctrl+Alt+F1..F12** — on the Wayland login session, switches
  virtual terminals; the session hands back its devices and comes
  alive again on the way in.

## Mouse, for completeness

- Right-click the desktop: the root menu (applications, themes, exit).
- Right-click any titlebar: the window commands menu.
- Drag a miniaturized window's icon tile onto the launcher strip to
  pin its application; click a pin to launch or focus; drag off to
  unpin.

## Rebinding

In `~/.config/chonkstep/config.toml`:

```toml
[keybindings]
"super+return" = "spawn-terminal"   # add or change a combo
"alt+shift+q" = "none"              # unbind one
"super+r" = "reload"                # apply this very file from the keyboard
```

Key spec grammar: case-insensitive, `+`-separated modifiers
(`alt`, `shift`, `ctrl`/`control`, `super`/`mod4`/`win`) followed by
exactly one key: letters, digits, `return`/`enter`, `tab`, `space`,
`escape`, `left`, `right`, `up`, `down`, `home`, `end`, `pageup`,
`pagedown`, `minus`, `equal`, `comma`, `period`, `f1`–`f12`.

A typo'd combo or unknown action is warned about and skipped; every
other line still applies. Apply edits to the running session with
`scripts/reload.sh` (package installs: `/usr/lib/chonkstep/reload.sh`),
or the bound `reload` key.
