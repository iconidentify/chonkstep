# The keybinding card

There are **two** keymaps. `keymap = "chonkstep"` is the default and is
the NeXTSTEP-style `alt+shift` vocabulary the rest of this desktop was
designed around; `keymap = "omarchy"` is Omarchy's own vocabulary
mapped onto chonkstep's actions, for a user arriving with Hyprland
muscle memory (and what `desktop = "omarchy"` selects — see
[omarchy-mode.md](omarchy-mode.md)).

Choosing one **replaces** the other's table; the two are never merged.
Whichever is active, entries in your `[keybindings]` merge over it, so
listing one combo changes only that combo and setting a combo to
`"none"` unbinds it.

Each table below is transcribed from the source that defines it — the
chonkstep defaults from [config.example.toml](config.example.toml), the
Omarchy keymap from `crates/wm-config/src/preset.rs` — and a test fails
if a table here and its source disagree. If this card and its source
ever do disagree, the source wins.

## The chonkstep defaults (`keymap = "chonkstep"`)

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
| `control+escape`   | `window-menu`          | Window commands menu, no titlebar required    |

Window-targeted actions (`close`, `toggle-maximize`, `toggle-shade`,
`miniaturize`, `toggle-fullscreen`, `window-menu`) act on the focused
window and do nothing when no window is focused.

Two more verbs exist and are unbound in this keymap, because the
NeXTSTEP vocabulary reaches workspaces by stepping through them rather
than by number: `workspace <n>` goes to workspace *n*, and
`workspace-carry <n>` takes the focused window there and follows it.
**Workspaces are numbered from 1** in this file, the way the window
menu's `Move To` submenu numbers them and the way every other desktop
does; 1 through 99 are accepted. Naming a workspace that does not exist
yet creates it, along with any gap before it — the row grows on demand
and is never destroyed, exactly as `workspace-next` grows it a step at a
time. Bind them like anything else:

```toml
[keybindings]
"alt+ctrl+1" = "workspace 1"
"alt+shift+1" = "workspace-carry 1"
```

## The Omarchy keymap (`keymap = "omarchy"`)

```toml
keymap = "omarchy"        # ...or desktop = "omarchy", which defaults it
```

114 bindings, derived from Omarchy's own configuration on the machine —
`$OMARCHY_PATH/default/hypr/bindings/*.lua` — rather than from memory of
Hyprland, with the `o.bind` helpers expanded the way `helpers.lua`
expands them. A `run <name>` action names an entry the preset declares
in `[commands]`; the third column is the argv it runs, which is
Omarchy's own command line. Selecting this keymap declares all 77 of
those commands, so nothing here needs a `[commands]` table of your own.

Three of these differ from what Omarchy does with the chord —
`super+f`/`super+alt+f`, `super+alt+s`, and the two chonkstep verbs on
`super+up` and `control+escape` — and
[omarchy-mode.md](omarchy-mode.md#three-chords-we-do-differently) spells
out how. Omarchy's locked media/brightness keys, repeating ramps, and
release bindings retain those firing semantics under chonkstep.

Whether a mapped chord *works* is two questions, and this table answers
only the first. The chord reaching the command is what the keymap
guarantees; whether that Omarchy command does anything under a
compositor that is not Hyprland is
[omarchy-integration.md](omarchy-integration.md)'s subject — it is the
honest inventory, script by script, and a few of the commands below
are intentional floating-desktop refusals. The integration guide records
those boundaries; night light, capture layers, and the common window
helpers are supported directly.

| Binding                  | Action                                 | Omarchy's own command                                                                                               |
|--------------------------|----------------------------------------|---------------------------------------------------------------------------------------------------------------------|
| `super+return`           | `spawn-terminal`                       | --                                                                                                                  |
| `super+shift+return`     | `run omarchy-browser`                  | `omarchy-launch-browser`                                                                                            |
| `super+shift+b`          | `run omarchy-browser`                  | `omarchy-launch-browser`                                                                                            |
| `super+shift+alt+b`      | `run omarchy-browser-private`          | `omarchy-launch-browser --private`                                                                                  |
| `super+shift+f`          | `run omarchy-files`                    | `omarchy-launch-nautilus`                                                                                           |
| `super+alt+shift+f`      | `run omarchy-files-here`               | `omarchy-launch-nautilus-cwd`                                                                                       |
| `super+shift+n`          | `run omarchy-editor`                   | `omarchy-launch-editor`                                                                                             |
| `super+w`                | `close`                                | --                                                                                                                  |
| `super+f`                | `toggle-fullscreen`                    | --                                                                                                                  |
| `super+alt+f`            | `toggle-maximize`                      | --                                                                                                                  |
| `super+tab`              | `workspace-next`                       | --                                                                                                                  |
| `super+shift+tab`        | `workspace-prev`                       | --                                                                                                                  |
| `super+1`                | `workspace 1`                          | --                                                                                                                  |
| `super+2`                | `workspace 2`                          | --                                                                                                                  |
| `super+3`                | `workspace 3`                          | --                                                                                                                  |
| `super+4`                | `workspace 4`                          | --                                                                                                                  |
| `super+5`                | `workspace 5`                          | --                                                                                                                  |
| `super+6`                | `workspace 6`                          | --                                                                                                                  |
| `super+7`                | `workspace 7`                          | --                                                                                                                  |
| `super+8`                | `workspace 8`                          | --                                                                                                                  |
| `super+9`                | `workspace 9`                          | --                                                                                                                  |
| `super+0`                | `workspace 10`                         | --                                                                                                                  |
| `super+shift+1`          | `workspace-carry 1`                    | --                                                                                                                  |
| `super+shift+2`          | `workspace-carry 2`                    | --                                                                                                                  |
| `super+shift+3`          | `workspace-carry 3`                    | --                                                                                                                  |
| `super+shift+4`          | `workspace-carry 4`                    | --                                                                                                                  |
| `super+shift+5`          | `workspace-carry 5`                    | --                                                                                                                  |
| `super+shift+6`          | `workspace-carry 6`                    | --                                                                                                                  |
| `super+shift+7`          | `workspace-carry 7`                    | --                                                                                                                  |
| `super+shift+8`          | `workspace-carry 8`                    | --                                                                                                                  |
| `super+shift+9`          | `workspace-carry 9`                    | --                                                                                                                  |
| `super+shift+0`          | `workspace-carry 10`                   | --                                                                                                                  |
| `super+alt+s`            | `miniaturize`                          | --                                                                                                                  |
| `super+ctrl+v`           | `run omarchy-clipboard`                | `omarchy-shell shell toggle omarchy.clipboard`                                                                      |
| `super+space`            | `run omarchy-menu`                     | `omarchy-menu toggle`                                                                                               |
| `super+shift+f23`        | `run omarchy-menu`                     | `omarchy-menu toggle`                                                                                               |
| `super+alt+space`        | `run omarchy-menu-apps`                | `omarchy-menu toggle apps`                                                                                          |
| `super+escape`           | `run omarchy-menu-system`              | `omarchy-menu toggle system`                                                                                        |
| `poweroff`               | `run omarchy-menu-system`              | `omarchy-menu toggle system`                                                                                        |
| `super+ctrl+c`           | `run omarchy-menu-capture`             | `omarchy-menu toggle capture`                                                                                       |
| `super+ctrl+o`           | `run omarchy-menu-toggles`             | `omarchy-menu toggle toggle`                                                                                        |
| `super+ctrl+h`           | `run omarchy-menu-hardware`            | `omarchy-menu toggle hardware`                                                                                      |
| `super+ctrl+s`           | `run omarchy-menu-share`               | `omarchy-menu toggle share`                                                                                         |
| `super+ctrl+space`       | `run omarchy-menu-background`          | `omarchy-menu toggle background`                                                                                    |
| `super+shift+ctrl+space` | `run omarchy-menu-theme`               | `omarchy-menu toggle theme`                                                                                         |
| `super+ctrl+e`           | `run omarchy-emojis`                   | `omarchy-shell shell toggle omarchy.emojis`                                                                         |
| `super+alt+k`            | `run omarchy-keybindings-tmux`         | `omarchy-menu-tmux-keybindings`                                                                                     |
| `super+ctrl+k`           | `run omarchy-keybindings-herdr`        | `omarchy-menu-herdr-keybindings`                                                                                    |
| `super+ctrl+q`           | `run omarchy-calculator`               | `omacalc`                                                                                                           |
| `calculator`             | `run omarchy-calculator`               | `omacalc`                                                                                                           |
| `super+comma`            | `run omarchy-notification-dismiss`     | `omarchy-shell notifications dismissOne`                                                                            |
| `super+shift+comma`      | `run omarchy-notification-dismiss-all` | `omarchy-shell notifications dismissAll`                                                                            |
| `super+alt+comma`        | `run omarchy-notification-invoke`      | `omarchy-shell notifications invokeLast`                                                                            |
| `super+shift+alt+comma`  | `run omarchy-notification-history`     | `omarchy-shell notifications showHistory`                                                                           |
| `super+ctrl+comma`       | `run omarchy-notification-silence`     | `omarchy-toggle-notification-silencing`                                                                             |
| `super+ctrl+i`           | `run omarchy-toggle-idle`              | `omarchy-toggle-idle`                                                                                               |
| `super+ctrl+n`           | `run omarchy-toggle-nightlight`        | `omarchy-toggle-nightlight`                                                                                         |
| `print`                  | `run omarchy-screenshot`               | `omarchy-capture-screenshot`                                                                                        |
| `alt+print`              | `run omarchy-screenrecord`             | `bash -lc "omarchy-capture-screenrecording --stop-recording \|\| omarchy-menu toggle trigger.capture.screenrecord"` |
| `super+print`            | `run omarchy-colorpicker`              | `bash -lc "pkill hyprpicker \|\| hyprpicker -a"`                                                                    |
| `super+ctrl+print`       | `run omarchy-ocr`                      | `omarchy-capture-text`                                                                                              |
| `super+alt+bracketleft`  | `run omarchy-webcam-smaller`           | `omarchy-capture-webcam-resize smaller`                                                                             |
| `super+alt+bracketright` | `run omarchy-webcam-larger`            | `omarchy-capture-webcam-resize larger`                                                                              |
| `super+ctrl+period`      | `run omarchy-transcode`                | `omarchy-transcode`                                                                                                 |
| `super+ctrl+r`           | `run omarchy-reminder-set`             | `omarchy-menu toggle reminder-set`                                                                                  |
| `super+ctrl+alt+r`       | `run omarchy-reminder-show`            | `omarchy-reminder show`                                                                                             |
| `super+shift+ctrl+r`     | `run omarchy-reminder-clear`           | `omarchy-reminder clear`                                                                                            |
| `super+ctrl+alt+t`       | `run omarchy-show-time`                | `omarchy-notification-time`                                                                                         |
| `super+ctrl+alt+b`       | `run omarchy-show-battery`             | `omarchy-notification-battery`                                                                                      |
| `super+ctrl+alt+w`       | `run omarchy-show-weather`             | `omarchy-notification-weather`                                                                                      |
| `super+shift+ctrl+a`     | `run omarchy-agent`                    | `omarchy-agent --pick`                                                                                              |
| `super+ctrl+a`           | `run omarchy-panel-audio`              | `omarchy-shell shell toggle omarchy.audio`                                                                          |
| `super+ctrl+b`           | `run omarchy-panel-bluetooth`          | `omarchy-shell shell toggle omarchy.bluetooth`                                                                      |
| `super+ctrl+w`           | `run omarchy-panel-network`            | `omarchy-shell shell toggle omarchy.network`                                                                        |
| `super+ctrl+p`           | `run omarchy-panel-power`              | `omarchy-shell shell toggle omarchy.power`                                                                          |
| `super+ctrl+alt+d`       | `run omarchy-panel-clock`              | `omarchy-shell shell toggle omarchy.clock`                                                                          |
| `super+ctrl+t`           | `run omarchy-activity`                 | `omarchy-launch-tui btop`                                                                                           |
| `super+ctrl+1`           | `run omarchy-panel-1`                  | `omarchy-shell -q shell togglePanelAt right 1`                                                                      |
| `super+ctrl+2`           | `run omarchy-panel-2`                  | `omarchy-shell -q shell togglePanelAt right 2`                                                                      |
| `super+ctrl+3`           | `run omarchy-panel-3`                  | `omarchy-shell -q shell togglePanelAt right 3`                                                                      |
| `super+ctrl+4`           | `run omarchy-panel-4`                  | `omarchy-shell -q shell togglePanelAt right 4`                                                                      |
| `super+ctrl+5`           | `run omarchy-panel-5`                  | `omarchy-shell -q shell togglePanelAt right 5`                                                                      |
| `super+ctrl+6`           | `run omarchy-panel-6`                  | `omarchy-shell -q shell togglePanelAt right 6`                                                                      |
| `super+ctrl+7`           | `run omarchy-panel-7`                  | `omarchy-shell -q shell togglePanelAt right 7`                                                                      |
| `super+ctrl+8`           | `run omarchy-panel-8`                  | `omarchy-shell -q shell togglePanelAt right 8`                                                                      |
| `super+ctrl+9`           | `run omarchy-panel-9`                  | `omarchy-shell -q shell togglePanelAt right 9`                                                                      |
| `super+ctrl+l`           | `run omarchy-lock`                     | `omarchy-system-lock`                                                                                               |
| `volumeup`               | `run omarchy-volume-up`                | `omarchy-audio-output-volume raise`                                                                                 |
| `volumedown`             | `run omarchy-volume-down`              | `omarchy-audio-output-volume lower`                                                                                 |
| `volumemute`             | `run omarchy-volume-mute`              | `omarchy-audio-output-volume mute-toggle`                                                                           |
| `micmute`                | `run omarchy-mic-mute`                 | `omarchy-audio-input-mute`                                                                                          |
| `alt+volumeup`           | `run omarchy-volume-up-fine`           | `omarchy-audio-output-volume +1`                                                                                    |
| `alt+volumedown`         | `run omarchy-volume-down-fine`         | `omarchy-audio-output-volume -1`                                                                                    |
| `shift+volumemute`       | `run omarchy-audio-output-switch`      | `omarchy-audio-output-switch`                                                                                       |
| `brightnessup`           | `run omarchy-brightness-up`            | `omarchy-brightness-display +5%`                                                                                    |
| `brightnessdown`         | `run omarchy-brightness-down`          | `omarchy-brightness-display 5%-`                                                                                    |
| `shift+brightnessup`     | `run omarchy-brightness-max`           | `omarchy-brightness-display 100%`                                                                                   |
| `shift+brightnessdown`   | `run omarchy-brightness-min`           | `omarchy-brightness-display 1%`                                                                                     |
| `alt+brightnessup`       | `run omarchy-brightness-up-fine`       | `omarchy-brightness-display +1%`                                                                                    |
| `alt+brightnessdown`     | `run omarchy-brightness-down-fine`     | `omarchy-brightness-display 1%-`                                                                                    |
| `kbdbrightnessup`        | `run omarchy-kbd-brightness-up`        | `omarchy-brightness-keyboard up`                                                                                    |
| `kbdbrightnessdown`      | `run omarchy-kbd-brightness-down`      | `omarchy-brightness-keyboard down`                                                                                  |
| `kbdlightonoff`          | `run omarchy-kbd-brightness-cycle`     | `omarchy-brightness-keyboard cycle`                                                                                 |
| `playpause`              | `run omarchy-media-play-pause`         | `omarchy-shell media playPause`                                                                                     |
| `audiopause`             | `run omarchy-media-play-pause`         | `omarchy-shell media playPause`                                                                                     |
| `audionext`              | `run omarchy-media-next`               | `omarchy-shell media next`                                                                                          |
| `audioprev`              | `run omarchy-media-prev`               | `omarchy-shell media previous`                                                                                      |
| `alt+playpause`          | `run omarchy-media-next`               | `omarchy-shell media next`                                                                                          |
| `alt+shift+playpause`    | `run omarchy-media-prev`               | `omarchy-shell media previous`                                                                                      |
| `shift+playpause`        | `run omarchy-audio-source-switch`      | `omarchy-audio-source-switch`                                                                                       |
| `shift+audiopause`       | `run omarchy-audio-source-switch`      | `omarchy-audio-source-switch`                                                                                       |
| `eject`                  | `run omarchy-eject`                    | `eject`                                                                                                             |
| `super+up`               | `overview`                             | --                                                                                                                  |
| `control+escape`         | `window-menu`                          | --                                                                                                                  |

### Deliberately unbound

34 groups of Omarchy chord this keymap leaves dead, and why. Left dead
rather than approximated: a dead key is looked up in five seconds,
while a `super+j` that does something *else* is a bug report.

| Omarchy chord                                                                                      | What Omarchy does with it                                            | Why not here                                                |
|----------------------------------------------------------------------------------------------------|----------------------------------------------------------------------|-------------------------------------------------------------|
| `super+alt+return, super+ctrl+return, super+shift+{a,c,d,e,g,m,o,p,s,w,x,y,/}, +alt/ctrl variants` | Omarchy's preinstalled application, TUI and webapp chords            | Omarchy binds it conditionally; a table of constants cannot |
| `super+c / super+v / super+x`                                                                      | universal copy / paste / cut, by synthesising Ctrl+C/V/X at the seat | chonkstep has no verb for it, and no command can stand in   |
| `super+j`                                                                                          | toggle window split                                                  | tiling-only: no meaning on a stacking desk                  |
| `super+p`                                                                                          | pseudo-tile the window                                               | tiling-only: no meaning on a stacking desk                  |
| `super+t`                                                                                          | toggle floating / tiling                                             | tiling-only: no meaning on a stacking desk                  |
| `super+ctrl+f`                                                                                     | tiled fullscreen                                                     | tiling-only: no meaning on a stacking desk                  |
| `super+o`                                                                                          | pop the window out, floating and pinned                              | tiling-only: no meaning on a stacking desk                  |
| `super+home / super+alt+home`                                                                      | restore / save window width                                          | commands Hyprland, which is not running                     |
| `super+l`                                                                                          | cycle the workspace layout                                           | commands Hyprland, which is not running                     |
| `super+g / super+alt+g`                                                                            | toggle grouping / move out of group                                  | tiling-only: no meaning on a stacking desk                  |
| `super+alt+left/right/up/down`                                                                     | move the window into the group in that direction                     | tiling-only: no meaning on a stacking desk                  |
| `super+alt+tab / super+alt+shift+tab`                                                              | next / previous window in the group                                  | tiling-only: no meaning on a stacking desk                  |
| `super+ctrl+left / super+ctrl+right`                                                               | move the grouped-window focus                                        | tiling-only: no meaning on a stacking desk                  |
| `super+alt+1..5`                                                                                   | focus the nth window of the group                                    | tiling-only: no meaning on a stacking desk                  |
| `super+left/right/up/down`                                                                         | focus the window in that direction                                   | chonkstep has no verb for it, and no command can stand in   |
| `super+shift+left/right/up/down`                                                                   | swap the window with its neighbour                                   | tiling-only: no meaning on a stacking desk                  |
| `super+minus / super+equal, +shift/alt/ctrl variants`                                              | grow and shrink the window by 25 / 100 / 300 px                      | tiling-only: no meaning on a stacking desk                  |
| `super+shift+alt+1..0`                                                                             | move the window to workspace n without following                     | chonkstep has no verb for it, and no command can stand in   |
| `super+s`                                                                                          | toggle the scratchpad workspace                                      | chonkstep has no verb for it, and no command can stand in   |
| `super+ctrl+tab`                                                                                   | the workspace before this one                                        | chonkstep has no verb for it, and no command can stand in   |
| `super+shift+alt+left/right/up/down`                                                               | move the workspace to the monitor in that direction                  | chonkstep has no verb for it, and no command can stand in   |
| `ctrl+alt+tab / ctrl+alt+shift+tab`                                                                | focus the next / previous monitor                                    | chonkstep has no verb for it, and no command can stand in   |
| `ctrl+alt+delete`                                                                                  | close every window                                                   | commands Hyprland, which is not running                     |
| `super+slash / super+alt+slash`                                                                    | monitor scaling up / down                                            | commands Hyprland, which is not running                     |
| `super+mouse wheel, super+drag`                                                                    | scroll through workspaces; move and resize by mouse                  | not a key chord this config format can express              |
| `super+k`                                                                                          | Omarchy's keybinding cheatsheet                                      | declined on purpose — see the note under the table          |
| `super+shift+space`                                                                                | toggle Omarchy's top bar                                             | chonkstep has no verb for it, and no command can stand in   |
| `super+ctrl+d`                                                                                     | Omarchy's display panel                                              | commands Hyprland, which is not running                     |
| `super+backspace / super+shift+backspace / super+ctrl+backspace`                                   | window transparency; window gaps; single-window square aspect        | commands Hyprland, which is not running                     |
| `super+ctrl+delete / super+ctrl+alt+delete`                                                        | toggle the laptop display; toggle mirroring                          | commands Hyprland, which is not running                     |
| `super+ctrl+z / super+ctrl+alt+z`                                                                  | cursor zoom in / reset                                               | commands Hyprland, which is not running                     |
| `switch:on/off:Lid Switch`                                                                         | run the lid-close and clamshell handlers                             | not a key chord this config format can express              |
| `touchpad toggle / on / off`                                                                       | enable and disable the touchpad                                      | commands Hyprland, which is not running                     |
| `super+ctrl+x, f9`                                                                                 | voxtype dictation: toggle, and push-to-talk                          | Omarchy binds it conditionally; a table of constants cannot |

The two gaps most worth knowing about — directional focus and toggling
Omarchy's bar — have workarounds listed in
[omarchy-mode.md](omarchy-mode.md#the-gaps-worth-knowing-about).

## The mouse gestures

| Gesture                  | What it does                                        |
|--------------------------|-----------------------------------------------------|
| Drag a titlebar          | Move the window, with edge snapping                 |
| Drag an edge or corner   | Resize from there — all eight, with cursor shapes   |
| Double-click a titlebar  | Shade; +Ctrl / +Shift / +both maximize an axis      |
| Right-click a titlebar   | The window commands menu                            |
| `alt` + drag, left       | Move the window, from anywhere on it                |
| `alt` + drag, right      | Resize the window, from anywhere on it              |

The last two are the ones that matter for a window with no titlebar.
Every xdg-decoration negotiation on this desktop ends server-side, as
it does on Hyprland, so a Wayland client that merely *asked* to draw
its own chrome gets a frame anyway. But a client that *declares* its
own chrome — over KDE's server-decoration protocol, which is the one
GTK speaks, or through `_MOTIF_WM_HINTS` on X11 — is believed, and a
few of those declare a titlebar and then draw nothing at all. Those
windows have no titlebar to drag and no resize bar to pull, so the
gesture is grabbed on the window's own content and works on every
window, framed or not; `control+escape` reaches its commands menu for
the same reason. Window Maker binds both the same way, and for the
same case.

The modifier is `drag_modifier` in the config: `"alt"` by default,
`"super"` if an application (CAD, GIMP, Blender) wants Alt+drag for
itself, `"none"` to turn the gesture off. To give such a bare window
its titlebar back permanently instead, name it in `[decorations]
server_side`; to keep an xdg client bare on purpose, name it in
`client_side` — see `docs/config.example.toml`.

Three more actions exist and are deliberately unbound by default —
give them keys in your config:

| Action        | What it does                                                         |
|---------------|----------------------------------------------------------------------|
| `toggle-dock` | Show / hide the Dock, column and reserved strip together             |
| `reload`      | Re-read the config file and apply all of it, live — nothing closed   |
| `restart`     | Re-exec the on-disk binary, for picking up a new build               |

`toggle-dock` is the keyboard's way to the root menu's `Dock` row.
Hidden means hidden *and* out of the way: the column is unmapped and
the one-tile strip it reserves goes straight back to the workarea, so
maximized windows use the full width of the screen. The Clip, the
launcher strip and any miniaturized window's icon tile are their own
surfaces elsewhere on the desk and stay put. The choice is remembered
across sessions; `show_dock = false` in the config is where a session
that never wants a Dock starts — see
[config.example.toml](config.example.toml).

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

- Right-click the desktop: the root menu — `Terminal`, `Applications`,
  `Theme`, `Wallpaper`, `Dock`, then `Omarchy Bar` (when the session
  hosts Omarchy's shell) and the `Omarchy` submenu (when Omarchy is
  installed), and `Exit`. `Dock` and `Omarchy Bar` are bulleted when
  that column is on screen, the way the `Theme` and `Wallpaper` rows
  mark the current choice.
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

The keys with pictures on them rather than letters have names too, and
are usually bound bare: `volumeup`, `volumedown`, `volumemute` (or
`mute`), `micmute`, `playpause` (or `audioplay`), `audiopause`,
`audiostop`, `audionext`, `audioprev`, `brightnessup`,
`brightnessdown`, `kbdbrightnessup`, `kbdbrightnessdown`, `poweroff`,
`search`.

A typo'd combo or unknown action is warned about and skipped; every
other line still applies.

## Running your own commands

Actions are a closed set — the window manager owns what "maximize"
means, so the list above is not extensible. Arbitrary commands go
through one indirection instead: name it in `[commands]`, then bind
`run <name>`.

```toml
[commands]
omarchy-menu = "omarchy-menu toggle"    # what Omarchy's own Super+Space runs
volume-up    = "omarchy-audio-output-volume up"
notify       = ["notify-send", "hello world"]   # array keeps spaces whole

[keybindings]
"super+space" = "run omarchy-menu"
"volumeup"    = "run volume-up"
```

The binding carries the *name*, never the command line. That is what
lets a mistake be caught: a binding naming a command you never declared
is reported at startup — naming both the key and the command — and
dropped, rather than becoming a key that silently does nothing when you
press it.

A string is split on whitespace; an array is taken verbatim, which is
how you pass an argument that contains a space. Commands are launched
detached: the desktop starts them and does not supervise them.

To start something with the session rather than on a key, use
`autostart` — a list, run in order, once, on a genuinely new session
(not on a reload, and not on a hot restart). Apply edits to the running session with
`scripts/reload.sh` (package installs: `/usr/lib/chonkstep/reload.sh`),
or the bound `reload` key.
