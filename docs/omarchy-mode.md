# Omarchy mode

> **One line.** Put `desktop = "omarchy"` at the top of
> `~/.config/chonkstep/config.toml` and log back in.

ChonkStep manages windows, decorations, desktop menus, Alt-Tab and Overview.
Omarchy provides the menu bar, workspace indicators, system panels and desktop
services. The compositor has no dock or persistent desktop workspace switcher.

The preset applies before your explicit configuration keys, so each setting
can be overridden independently.

---

## What the mode changes

| Key | Chonkstep default | Under `desktop = "omarchy"` | Why |
|---|---|---|---|
| `omarchy_bar` | *(unset — the bar is hosted but hidden)* | **`true`** | Show Omarchy's workspace indicators and system controls. |
| `theme` | *(unset — the flagship)* | **`"omarchy"`** | Follow Omarchy's palette: chrome, menus, wallpaper and terminal colours re-dress within a second of `omarchy-theme-set`. |
| `lock_command` | *(unset)* | **`"omarchy-system-lock"`** | Recovery enters a blank, input-isolated lock domain before launching Omarchy's own lock entry point; hosting a newly restarted shell does not lock it by itself. |
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

omarchy_bar = false     # ...or start with the bar hidden
theme = "amber-phosphor"# ...or wear a chonkstep theme anyway
lock_command = "swaylock"# ...or use a different recovery locker
keymap = "chonkstep"    # ...or keep ChonkStep's native chords
omarchy_shell = false   # ...or do not host Omarchy's shell at all
```

The root menu's `Omarchy Bar` row persists visibility above the configured
`omarchy_bar` default. That ordering is deliberate and matches how `theme` already works: a
choice you made *in the running session* is more recent and more
deliberate than a line you wrote in a file once, so hiding the bar from
the menu is not undone the next time you log in.

## What the mode deliberately leaves alone

- **Notifications, the steady-state lock screen, idle and the OSD.**
  Omarchy's shell draws all four, and hosting it (`omarchy_shell = true`,
  already the default) is all they need while the session is healthy.
  Crash recovery is different: relaunching the shell does not lock the
  resurrected session. The preset therefore sets `lock_command` to
  Omarchy's own `omarchy-system-lock` entry point. Recovery first blanks
  the compositor and blocks ordinary input, then waits off the event
  thread for the freshly relaunched shell's lock IPC before invoking that
  command; an explicit `lock_command` still wins.
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

  In particular, Omarchy's shipped Hyprland input table contains
  `follow_mouse = 1`. Chonkstep deliberately does not import it: a stock
  Omarchy session remains click-to-focus, matching chonkstep's documented
  default and traditional window-management model. Opt in explicitly
  with `focus_follows_mouse = true` in `config.toml` (or
  `CHONKSTEP_FOCUS_FOLLOWS_MOUSE=1`).
- **Omarchy's `background` shell plugin**, which chonkstep declines in
  every posture: it would paint over chonkstep's wallpaper and eat every
  click on the desk, right-click included. The desk stays chonkstep's
  and wears Omarchy's background picture through the theme.
- **Hyprland requests chonkstep does not serve.** `hyprctl`, and the
  `omarchy-hyprland-*` scripts that send a request chonkstep's Hyprland
  IPC cannot apply, stay out: the root menu leaves those rows out and the
  keymap leaves those chords unbound, by one shared rule. The scripts
  whose every request is served (pop-out, window width, close-all,
  monitor scaling and the workspace layout toggle) run as Omarchy wrote
  them; [hyprland-config.md](hyprland-config.md#omarchys-hyprland-scripts)
  lists both sides.

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
3. **Workspace styles are native.** Super+T floats/rejoins a window,
   Super+L toggles Mosaic/Flow, and Super+Shift+L returns to Freeform.
   Tree-only messages are quiet no-ops; grouping remains unsupported.

### Three chords we do differently

| Chord | Omarchy | Here | The difference |
|---|---|---|---|
| `super+f` / `super+alt+f` | fullscreen / "full width" (Hyprland's `maximized`) | `toggle-fullscreen` / `toggle-maximize` | The pair keeps its shape: the plain chord takes the whole output with no chrome, the modified one fills the workarea and keeps the titlebar. |
| `super+alt+s` | move the window to the scratchpad workspace | `miniaturize` | Both mean "send this window away, recoverably". Omarchy's goes to a hidden workspace and comes back with the same chord; ChonkStep hides the window until it is selected with Alt-Tab. `super+s` (toggle scratchpad) is unbound. |
| `control+escape` | nothing | `window-menu` | The window menu is a chonkstep verb Omarchy has no vocabulary for. It keeps its own chord, which Omarchy leaves free. |

Binding firing semantics are preserved too. Omarchy's media and brightness
keys marked `locked = true` work over the lock screen, ramps marked
`repeating = true` fire while held, and release bindings fire on release.

### When a mapped command is itself the limitation

The keymap guarantees the chord reaches the command. Whether a
Hyprland-specific operation has a native equivalent is a
separate question, answered script by script in
[omarchy-integration.md](omarchy-integration.md). Common window, capture,
input, layout, and night-light paths are supported. Unsupported grouping
operations are reported; inapplicable layout messages are quiet no-ops.

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

On the machine this was developed on the live read produced **167
bindings over 119 commands**, against the baked table's 151 over 83 —
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
— 151 bindings over 83 declared commands, then the 25 groups of Omarchy
chords that remain unbound and why. Both are transcribed
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

### The gap worth knowing about

One thing an Omarchy user will reach for and not find, with its
workaround. Workspaces by number used to be the first entry on
this list; `super+1..9` and `super+0`, and `super+shift+1..9` and
`super+shift+0` to take the window along, are bound now and go exactly
where an Omarchy user expects. Chonkstep counts workspaces from one in
the config file, as Omarchy does, and grows the row on demand — press
`super+7` on a desk with three workspaces and you have seven.
Directional focus has graduated from this list too:
`super+left/right/up/down` ranks the visible floating frames by their
actual geometry and focuses the closest candidate in that direction.
So has silent workspace movement: `super+shift+alt+1..0` sends the
focused window to that workspace, keeps the current workspace on
screen, and focuses the window it exposed.

---

The root menu's `Omarchy Bar` row controls bar visibility; `omarchy_bar = false`
starts with it hidden.

## What it looks like when it worked

The session log says both halves at boot:

```
hosting Omarchy's shell   launcher=/usr/share/omarchy/bin/omarchy-launch-shell
```

...and the desk comes up with Omarchy's bar across the top, no
extra desktop strips, and Omarchy's own palette on the window
chrome. `super+return` opens a terminal; `super+space` opens Omarchy's
menu; right-clicking the desk still gets chonkstep's root menu, including
its `Omarchy Bar` visibility control.

## Related

- [`config.example.toml`](config.example.toml) — every key, fully commented
- [`keybindings.md`](keybindings.md) — both keymaps, side by side
- [`appearance.md`](appearance.md) — how the light/dark axis interacts
  with following Omarchy's theme
