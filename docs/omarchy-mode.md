# Omarchy mode

> **One line.** Put `desktop = "omarchy"` at the top of
> `~/.config/chonkstep/config.toml` and log back in.

Chonkstep is a stacking window manager with a whole desktop wrapped
around it — a Dock of instruments, a root menu, its own themes, its own
wallpaper. Omarchy is a whole desktop too, and a good one. Running both
at once gets you two clocks, two volume readouts and two ideas about
what `super+return` means.

`desktop = "omarchy"` resolves that in the direction an Omarchy user
wants: **chonkstep becomes the window manager for Omarchy's desktop.**
Its windowing, its chrome, its Alt+Tab and its Overview; Omarchy's bar,
menu, pickers, panels, notifications, lock screen and theme.

Everything below is a **default**. A preset in chonkstep is a set of
starting values, never a lock: the posture is applied to the built-in
defaults *before* your file's own keys are read, so writing any one of
these keys out overrides it by the ordinary TOML rule, with no
precedence table to learn. `desktop = "omarchy"` followed by
`show_dock = true` gets you the whole posture with the Dock back.

---

## What the mode changes

| Key | Chonkstep default | Under `desktop = "omarchy"` | Why |
|---|---|---|---|
| `show_dock` | `true` | **`false`** | Omarchy's bar already carries the clock, volume, network, Bluetooth and power readouts the Dock's instruments carry. Two strips of furniture on one screen is the thing this mode exists to stop. |
| `omarchy_bar` | *(unset — the bar is hosted but hidden)* | **`true`** | With the Dock gone, the bar is the desk's furniture rather than a guest's, so it starts on screen instead of waiting to be asked for. |
| `theme` | *(unset — the flagship)* | **`"omarchy"`** | Follow Omarchy's palette: chrome, menus, wallpaper and terminal colours re-dress within a second of `omarchy-theme-set`. |
| `keymap` | `"chonkstep"` | **`"omarchy"`** | The adoption cliff, and the reason this is one line and not two. See [the keymap](#the-keymap) below. |
| `omarchy_menu` | `true` | `true` | Already on; restated by the posture so a change of default cannot silently take the posture with it. |
| `omarchy_shell` | `true` | `true` | Same. |

Nothing else. The mode sets six values and no more; there is no hidden
behaviour keyed off the posture anywhere in the codebase, which is why
`desktop` is carried on the resolved config only so a session can
*report* what it read.

### Individually overridable — every one of them

```toml
desktop = "omarchy"     # the whole posture...

show_dock = true        # ...but keep the Dock
omarchy_bar = false     # ...or start with the bar hidden
theme = "amber-phosphor"# ...or wear a chonkstep theme anyway
keymap = "chonkstep"    # ...or keep the NeXTSTEP chords
omarchy_shell = false   # ...or do not host Omarchy's shell at all
```

Two of these have a *third* layer above the file, and it wins over both
your key and the preset:

- **The Dock** — the root menu's `Dock` row and the `toggle-dock`
  binding write your choice to chonkstep's state, and a stored choice
  beats `show_dock`.
- **Omarchy's bar** — the root menu's `Omarchy Bar` row does the same
  for `omarchy_bar`.

That ordering is deliberate and matches how `theme` already works: a
choice you made *in the running session* is more recent and more
deliberate than a line you wrote in a file once, so hiding the bar from
the menu is not undone the next time you log in.

## What the mode deliberately leaves alone

- **Notifications, the lock screen, idle and the OSD.** Omarchy's shell
  draws all four, and hosting it (`omarchy_shell = true`, already the
  default) is all they need. Chonkstep's own `lock_command` stays a
  crash-recovery fallback for a Wayland session that comes back up
  without a shell; the mode does not set it.
- **The terminal.** `spawn-terminal` still launches chonkstep's built-in
  terminal, because it is the only one the desktop can theme end to
  end — the palette, the font size and the launch geometry go on its
  command line — and with `theme = "omarchy"` that palette *is*
  Omarchy's. If you would rather have the terminal Omarchy configured,
  that is one line: `terminal = "omarchy-launch-terminal"`.
- **`autostart`.** The *preset* sets it in neither posture. On a
  machine with an Omarchy configuration to read, the live read fills it
  from their `exec-once` lines — see
  [hyprland-config.md](hyprland-config.md) — and your own `autostart`
  in `config.toml` still replaces that, like every other key. Omarchy's
  shell is never in it either way: it is started through Omarchy's own
  launcher by `omarchy_shell`, and taking it from the list too would
  start it twice.
- **Placement, focus policy, edge resistance, scale, decorations, the
  drag modifier.** These are how chonkstep manages windows, which is
  the half of the desktop the mode is *keeping*. An Omarchy user
  adopting chonkstep is adopting these.
- **Omarchy's `background` shell plugin**, which chonkstep declines in
  every posture: it would paint over chonkstep's wallpaper and eat every
  click on the desk, right-click included. The desk stays chonkstep's
  and wears Omarchy's background picture through the theme.
- **Anything that commands Hyprland.** `hyprctl` and the
  `omarchy-hyprland-*` scripts talk to a compositor that is not running.
  The root menu already leaves those rows out; the keymap leaves those
  chords unbound for the same reason.

---

## The keymap

An Omarchy user arrives holding Hyprland's vocabulary: `super+return`
for a terminal, `super+w` to close, `super+space` for the menu,
`super+1..n` for workspaces. Chonkstep answers NeXTSTEP `alt+shift`
chords. Someone evaluating for five minutes never finds ours and
bounces, so `keymap = "omarchy"` maps Omarchy's own bindings onto
chonkstep's actions.

`keymap` is a key of its own, not part of the posture, because the two
questions are different: whose furniture is on screen, and which chords
your hands know. `desktop = "omarchy"` *defaults* it to `"omarchy"`, and
`keymap = "omarchy"` works on its own — a chonkstep desk with Hyprland
chords is a perfectly reasonable thing to want.

**The keymap replaces; it never merges.** Choosing one discards the
other's table outright. A desk answering to both vocabularies at once
would close a window on `alt+shift+q` *and* `super+w` with neither one
being the documented answer, and — worse — would leave a chord one
keymap deliberately kills alive because the other bound it. Every
conflict is therefore resolved in favour of the active keymap by
construction. To bring one binding back from the other vocabulary, name
it:

```toml
keymap = "omarchy"

[keybindings]
"alt+shift+return" = "spawn-terminal"   # the chonkstep chord, too
"super+shift+right" = "workspace-carry-next"
"super+space" = "none"                  # or drop one of the preset's
```

### Where the bindings come from

Not from memory. From Omarchy's own configuration on this machine —
`$OMARCHY_PATH/default/hypr/bindings/{applications,clipboard,media,tiling,utilities,voxtype}.lua`
— read binding by binding, with the `o.bind` helpers expanded the way
`helpers.lua` expands them (`{ omarchy = "browser" }` is
`omarchy-launch-browser`; `o.bind_toggle(.., "idle")` is
`omarchy-toggle-idle`; `{ tui = "btop" }` is `omarchy-launch-tui btop`).

Three kinds of Omarchy binding get three different answers:

1. **A window or workspace verb** chonkstep also has becomes that verb.
2. **An Omarchy command** becomes `run <name>` pointing at that same
   command, declared in `[commands]` by the preset. `super+space` opens
   Omarchy's menu because it runs Omarchy's `omarchy-menu` — not an
   imitation of it.
3. **Everything else stays unbound, and says why.** A tiling desktop's
   vocabulary is full of verbs with no meaning on a stacking desk, and
   an approximation is worse than a dead key: a dead key is looked up in
   five seconds, while `super+j` that does something *else* is a bug
   report.

### Three chords we do differently

| Chord | Omarchy | Here | The difference |
|---|---|---|---|
| `super+f` / `super+alt+f` | fullscreen / "full width" (Hyprland's `maximized`) | `toggle-fullscreen` / `toggle-maximize` | The pair keeps its shape: the plain chord takes the whole output with no chrome, the modified one fills the workarea and keeps the titlebar. |
| `super+alt+s` | move the window to the scratchpad workspace | `miniaturize` | Both mean "send this window away, recoverably". Omarchy's goes to a hidden workspace and comes back with the same chord; chonkstep's collapses to an **icon tile on the desk** and comes back by double-clicking that tile. There is no chord for the way back — `super+s` (toggle scratchpad) is unbound. |
| `super+up` / `control+escape` | directional focus / nothing | `overview` / `window-menu` | Two chonkstep verbs Omarchy has no vocabulary for at all. They keep chonkstep's own chords, which are free here because the Omarchy chords they would have collided with are unbound anyway. Carrying them is not inventing an Omarchy binding; it is refusing to make a chonkstep feature unreachable. |

Two more differences are about *how* a binding fires rather than what it
does. Omarchy marks its media and brightness keys `locked = true` (they
work over the lock screen) and its ramps `repeating = true` (they fire
while held). Chonkstep's bindings do neither: a held volume key steps
once, and the keys are inert while Omarchy's lock screen is up.

### When a mapped command is itself the limitation

The keymap guarantees the chord reaches the command. Whether the
command then does anything under a compositor that is not Hyprland is a
separate question, answered script by script in
[omarchy-integration.md](omarchy-integration.md). A few of the mapped
commands are on its broken list; they stay bound, because they are
Omarchy's own commands on Omarchy's own chords and the binding is
already right for the day the gap closes.

### On a real Omarchy machine, the table is read live

Everything below describes the **baked** keymap: Omarchy's bindings
transcribed by hand into `preset.rs`. On a machine that actually has
Omarchy on it, that table is the *fallback*, not what you get.

`desktop = "omarchy"` also reads your **live** `~/.config/hypr/**` —
Lua on Omarchy 4, classic `hyprland.conf` on 3 — and the bindings it
finds there replace the baked table outright. That is the difference
between "chonkstep knows what Omarchy's chords were in August" and
"Omarchy's menu still configures your machine": rebind a key through
their UI and the running session follows it within a second.

On the machine this was developed on the live read produced **135
bindings over 101 commands**, against the baked table's 113 over 77 —
the extra ones are mostly the preinstalled webapp and TUI chords, which
a table of constants had to write off because Omarchy gates them on a
file test that only a live read can make.

It also generalises the one hardcoded window rule: `org.omarchy.*` at
875x600 becomes Omarchy's real 38 float rules, so Steam gets 1100x700
and picture-in-picture 600x338 instead of the size Omarchy's terminals
want.

**[hyprland-config.md](hyprland-config.md)** is the whole story: what is
read, what is deliberately ignored and why, the precedence, and
`hyprland_config = false` to turn it off. The tables below stay
accurate for a machine with no Omarchy configuration to read, and are
what the live read falls back to.

### The full map, and what is unbound

Both tables live in the keybinding card, beside chonkstep's own:
**[keybindings.md](keybindings.md), under "The Omarchy keymap"**
— 113 bindings over 77 declared commands, then the 35 groups of Omarchy
chords that are deliberately dead here and why. Both are transcribed
from `crates/wm-config/src/preset.rs`, which is the authoritative list;
`crates/wm-config/tests/preset_doc.rs` fails if the card and the table
disagree.

The two `declined on purpose` rows:

- **`super+k`** would open Omarchy's keybinding cheatsheet, which lists
  Omarchy's *Hyprland* bindings. About a third of them are wrong here.
  A cheatsheet that lies is worse than none; use this page.
- **`super+mouse`** move and resize by mouse — chonkstep already has
  that gesture, on `drag_modifier` (Alt by default, `"super"` if you
  want Omarchy's modifier: `drag_modifier = "super"`). The
  scroll-through-workspaces half has no equivalent.

### The gaps worth knowing about

Two things an Omarchy user will reach for and not find, with the
workaround for each. Workspaces by number used to be the first entry on
this list; `super+1..9` and `super+0`, and `super+shift+1..9` and
`super+shift+0` to take the window along, are bound now and go exactly
where an Omarchy user expects. Chonkstep counts workspaces from one in
the config file, as Omarchy does, and grows the row on demand — press
`super+7` on a desk with three workspaces and you have seven.

- **Directional focus** (`super+left/right/up/down`) does not exist on a
  stacking desk. Alt+Tab does, and it is fixed modal machinery here —
  hold Alt, Tab through live thumbnails — exactly as Omarchy binds it.
- **Toggling Omarchy's bar** (`super+shift+space`) has no chonkstep
  verb: `toggle-dock` toggles *this desk's* Dock, which is a different
  piece of furniture, and mapping one to the other would hide the wrong
  thing. The root menu's `Omarchy Bar` row is the way, and
  `omarchy_bar = false` is the way to start without it.

---

## What it looks like when it worked

The session log says both halves at boot:

```
hosting Omarchy's shell   launcher=/usr/share/omarchy/bin/omarchy-launch-shell
```

...and the desk comes up with Omarchy's bar across the top, no
chonkstep Dock in the corner, and Omarchy's own palette on the window
chrome. `super+return` opens a terminal; `super+space` opens Omarchy's
menu; right-clicking the desk still gets chonkstep's root menu, with
`Dock` and `Omarchy Bar` rows in it to change your mind.

## Related

- [`config.example.toml`](config.example.toml) — every key, fully commented
- [`keybindings.md`](keybindings.md) — both keymaps, side by side
- [`appearance.md`](appearance.md) — how the light/dark axis interacts
  with following Omarchy's theme
