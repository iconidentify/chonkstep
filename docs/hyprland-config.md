# Reading your Hyprland configuration

> **One line.** `desktop = "omarchy"` in
> `~/.config/chonkstep/config.toml` already turns this on. Your
> `~/.config/hypr/` is read live — bindings, window rules, autostart,
> environment — and re-read within a second whenever it changes.

Chonkstep can be the window manager for an Omarchy desktop. The thing
that makes that swap *invisible* rather than merely possible is this:
you keep configuring your machine the way you already do.

An Omarchy user's keybindings, window rules, startup apps and session
environment live in `~/.config/hypr/`. Crucially, so does everything
Omarchy's own menu writes — change a keybinding through their UI and it
edits a Hyprland config file. If chonkstep did not read those files,
the menu would silently stop working the day you switched window
managers, and there is no worse failure than a settings screen that
accepts your change and does nothing.

So chonkstep reads them. Not a copy taken at development time; the
actual files, at startup, again on every change.

---

## What is on your machine

Which syntax you have depends on your Omarchy version, and both are
read.

| Omarchy | Entry point | Syntax |
|---|---|---|
| 4.x ("quattro") and later | `~/.config/hypr/hyprland.lua` | Lua |
| 3.x, and hand-written upstream configs | `~/.config/hypr/hyprland.conf` | classic `keyword = value` |

If both files exist — which is what a machine mid-upgrade looks like —
**the Lua one wins**, exactly as Hyprland itself decides it. This
matters more than it sounds: the machine this was written on had a live
`hyprland.lua` next to a `hyprland.conf` that a migration had left
behind pointing at a compatibility shim, and a reader that preferred
the older file would have read a configuration Hyprland no longer used.

From the entry point, the whole graph is followed in the order it is
written — `require` and `require_all` for Lua, `source` (globs
included) for conf — so Omarchy's shipped defaults are read first and
your own overrides land on top of them. On a stock Omarchy 4 install
that is around 42 files.

---

## What is read

### Keybindings

`bind`, `bindd`, `bindl`, `binde` and friends in conf; `o.bind`,
`o.bind_toggle`, `hl.bind` and `hl.unbind` in Lua. Every helper form
Omarchy's `helpers.lua` defines is expanded exactly as that file
expands it, so `{ omarchy = "browser" }` becomes
`omarchy-launch-browser` and `{ webapp = …, focus = true }` becomes the
same shell-quoted `omarchy-launch-or-focus-webapp` line Hyprland would
have run.

Each binding gets one of three answers:

1. **A verb chonkstep also has** becomes that verb. `killactive` →
   `close`, `fullscreen 0` → `toggle-fullscreen`, `fullscreen 1` →
   `toggle-maximize`, `workspace 4` → `workspace 4`, `workspace e+1` →
   `workspace-next`, `movefocus l/r/u/d` → geometry-ranked directional
   focus.
2. **A command** becomes a `run` binding naming that command, declared
   automatically in `[commands]` under a generated `hypr:…` name. This
   is the whole "install chonkstep, keep your Omarchy" claim made
   literal: `SUPER + SPACE` opens Omarchy's menu because it runs
   Omarchy's `omarchy-menu`, not an imitation of it.
3. **Everything else stays unbound**, with the reason logged.

The third answer is the important one, and the rule behind it is *an
approximation is worse than a dead key*. `SUPER + J` toggles a tiling
split on Omarchy; on a stacking desk there is nothing to split. A dead
key is discovered in five seconds and looked up; a key that does
something *else* is a bug report.

Because the recogniser keys on the **dispatcher** rather than on the
chord, moving "close window" from `SUPER + W` to `SUPER + Q` through
Omarchy's menu keeps working — what was recognised is `killactive`,
not the key it happened to be on.

Two things a hand-written table could not do, and this can:

- **Generated bindings.** Omarchy writes its workspace and bar-panel
  chords inside `for` loops (`for workspace = 1, 10 do … "code:" ..
  tostring(workspace + 9) …`). Those thirty chords — the ones you reach
  for first — are expanded, up to a 64-iteration bound.
- **Conditional bindings.** Omarchy gates its twenty-odd preinstalled
  app and webapp chords on `o.preinstalled_bindings_enabled()` and its
  dictation chords on `o.cmd_present("voxtype")`. Both are file-system
  questions, and both are answered by asking the file system. A
  condition that would need a *shell* (`o.shell_succeeds`) is not
  answered; that block is skipped and says so.

### Window rules

`windowrule`, `windowrulev2` and `o.window` / `hl.window_rule`, in all
three syntaxes Hyprland has shipped:

```text
windowrule   = float, ^(steam)$                          # v1
windowrulev2 = float, class:^(steam)$, title:^(Steam)$   # v2
windowrule   = float on, match:class steam               # 0.53+
```

The supported properties are `float`, `size`, `center`, `idle_inhibit`,
`pin`, `no_focus`, `no_initial_focus`, `focus_on_activate`,
`fullscreen`, and `maximize`. They match `class` and `title` as regular
expressions, unanchored (a *search*, which is how Hyprland matches and
why `o.window("localsend", …)` catches `localsend_app`). Last matching
rule wins independently for each property.

`idle_inhibit` follows the mapped/visible interpretation: a matching
window inhibits idle while it is visible on the current workspace (or
pinned), without requiring keyboard focus. `pin` makes the client
sticky across workspaces. Focus exclusions affect initial focus and
later activation requests separately. Fullscreen and maximize are
applied after initial placement, with maximize underneath fullscreen so
unfullscreen restores the expected state.

Every unsupported property produces its own `Skipped` line naming both
the property and matcher. A rule with an unsupported matcher is refused
whole, so a partially understood condition can never broaden the rule.

**Tags are resolved**, one level deep. Omarchy never writes `float`
next to a class; it writes two rules:

```lua
o.window("(org.omarchy.btop|…|imv|mpv)", { tag = "+floating-window" })
o.window({ tag = "floating-window" }, { float = true, size = { 875, 600 } })
```

A reader that skipped tags would conclude Omarchy floats nothing.

This replaces a hardcoded rule that used to live in `wm-core`: any
window whose app-id started `org.omarchy.` mapped at 875×600. That
number was a transcription of one of Omarchy's lines, and it got every
*other* float rule wrong — Steam wants 1100×700, picture-in-picture
600×338, the About box 920×480. Reading the real rules gets all
thirty-eight of them right. The hardcoded rule stays behind this one as
the answer for a machine with nothing to read.

### `exec-once` → autostart

`exec-once` lines, and the body of Lua's
`hl.on("hyprland.start", function() … end)`, become chonkstep's
`autostart` list, in file order, before anything that needs them.

Two are deliberately dropped:

- Anything commanding Hyprland (see below).
- `omarchy-launch-shell`. Chonkstep starts Omarchy's shell itself, at
  the point in startup where Hyprland's autostart would have; taking it
  from this list too would start a second bar.

### `env` → session environment

`env` lines become the session's environment, applied in `main` before
the compositor starts anything — which is the only place they can work,
since their whole purpose is to be *inherited*
(`GDK_BACKEND=wayland,x11,*`, `MOZ_ENABLE_WAYLAND=1`,
`ELECTRON_OZONE_PLATFORM_HINT=wayland`).

A variable already set in the session's environment is left alone: the
launcher, your shell profile and systemd are all more specific than a
config file being read on somebody else's behalf.

Session-identity and toolkit-wide scale variables are refused by name
and logged:

| Refused | Why |
|---|---|
| `XDG_CURRENT_DESKTOP`, `XDG_SESSION_DESKTOP` | Omarchy sets both to `Hyprland`, which under chonkstep is false. Carrying them routes xdg-desktop-portal at `xdg-desktop-portal-hyprland`, which would then try to talk to a compositor that is not there — and break screen sharing rather than one key. |
| `WAYLAND_DISPLAY`, `DISPLAY`, `XDG_SESSION_TYPE`, `XDG_RUNTIME_DIR`, `HYPRLAND_INSTANCE_SIGNATURE` | They name *this* session, which the compositor sets for itself. A stale value out of a file points every child at a display that does not exist. |
| `GDK_SCALE`, `GDK_DPI_SCALE`, `QT_SCALE_FACTOR`, `ELM_SCALE` | Global toolkit scaling can disagree with per-output Wayland scale. Monitor rules and fractional scale are the single scale path. |

Blanket activation-environment commands are never admitted as
autostart. In particular, `systemctl --user import-environment $(env
...)` and `dbus-update-activation-environment --all` are skipped with a
named reason. The session launcher publishes only its curated Wayland,
desktop, menu-prefix, backend, and IPC variables; test sessions publish
nothing to the real bus.

Editing an `env` line takes effect at your next login, not on the live
re-read. A process's environment is fixed when it starts.

---

## What is deliberately not read

Everything below is *logged* when it is met — one line naming the
specific directive, not a count. Turn on `RUST_LOG=debug` to see them.

| Not read | Why |
|---|---|
| Anything commanding Hyprland — `hyprctl`, `omarchy-hyprland-*` | It talks to a compositor that is not running, so the binding could only fail. The same filter chonkstep's Omarchy menu already applies to menu rows. `hyprpicker`, `hyprlock` and `hypridle` are *not* caught by it: they are ordinary Wayland clients and work here. |
| Gaps, borders, rounding, blur, shadows, animations, layouts (`hl.config`, `general { … }`, `decoration { … }`) | Hyprland's look. This desktop has its own — a theme, a titlebar, a decoration policy. Following them would mean drawing a NeXTSTEP frame in Hyprland's border colour. |
| Layer rules (`layerrule`, `hl.layer_rule`) | They configure Hyprland's layer-shell implementation. This compositor has its own. |
| Whole-desktop and device input policy (`follow_mouse`, touchpad policy, sensitivity, gestures) | The live read carries hardware-facing keyboard xkb/repeat values only. Chonkstep owns interaction policy: use its `focus_follows_mouse` key explicitly. Every declined value is logged. |
| Unsupported window-rule properties | `opacity`, `no_blur`, `suppress_event`, `workspace`, `move`, `keep_aspect_ratio`, … are each logged with their matcher. Tags used to select another supported rule are resolved. |
| Window rules carrying a matcher not implemented here (`match:xwayland 1`, `match:workspace 5`, `match:fullscreen 0`) | Refused **whole**. Applying a rule on the matchers that *were* understood turns "float this one XWayland window" into "float every window of this class". |
| A `size` given as a Hyprland layout expression (`(monitor_h*4/25)`) | It needs a monitor to evaluate against, and a config reader has a file, not an output. |
| Mouse, wheel and switch bindings (`bindm`, `mouse:272`, `mouse_up`, `switch:on:Lid Switch`) | Not key chords; this config format cannot express one. |
| `exec` (as opposed to `exec-once`) | It re-runs on every config reload, which here would mean on every poll. Taking it as autostart would start a fresh copy each time you edited anything. |
| `submap`, workspace rules, `plugin`, `bezier`, `animation` | Hyprland's own machinery. Every binding inside conf `submap = name … reset` or Lua `hl.define_submap` is skipped with its chord and submap; it is never promoted to a global grab. |
| `hl.on("layer.opened")` selection bindings | Read as a namespace-scoped keymap. It is installed only while a matching layer-shell surface is mapped and removed after the last such surface closes. A handler with unknown side effects is refused whole. |
| Unsupported `monitor =` lines | A line containing disable, mirror, transform/extra fields, or an explicit mode is refused whole. The supported subset is applied as described below. |

### Bindings this desktop has no verb for

Beyond the tiling vocabulary, one specific chord family stays dead and
is worth knowing about:

- **The universal clipboard chords** (`SUPER + C/V/X`), which Omarchy
  builds by synthesising `Ctrl+C` at the seat. That is the
  compositor's own input path; no command could stand in.

Directional focus (`movefocus l/r/u/d`) is native: Chonkstep ranks the
visible floating frames by their actual root-coordinate geometry and
focuses the closest candidate in that direction. Directional movement
and swapping remain tiling-only because a free-form window has no
neighbouring slot to move into.

Silent workspace sends (`movetoworkspacesilent 1..99`) are native too:
the active window moves without changing the current workspace, and an
exposed window receives focus. The scratchpad form remains mapped to
`miniaturize`, because Chonkstep models recoverable desktop icons rather
than a special scratchpad workspace.

A last group is refused for a different reason — *declined on purpose*,
meaning chonkstep could bind them and does not, because what it would
do is not what you are asking for:

- **`ALT + TAB`** (`cyclenext`, `bringactivetotop`). This desk's window
  switcher already owns this chord, and it is modal machinery rather
  than a binding — while it is up the shell owns the whole keyboard, so
  arrows move and Return commits. Binding it from a config file would
  break it.
- **`SUPER + K`**, Omarchy's keybinding cheatsheet, which lists
  Omarchy's *Hyprland* bindings. About a third of them are wrong here,
  and a cheatsheet that lies is worse than none.

### Input and binding behavior

`kb_rules`, `kb_model`, `kb_layout`, `kb_variant`, and `kb_options`
build the seat's xkb keymap. `repeat_rate` and `repeat_delay` configure
both client key repeat and `binde` actions. These hardware-facing values
transfer; whole-desktop interaction policy does not. In particular,
Hyprland's `follow_mouse` is logged and ignored—even when it is `1` in
Omarchy's shipped defaults—so a stock Omarchy install retains
chonkstep's click-to-focus default. Set `focus_follows_mouse = true` in
chonkstep's own `config.toml` to opt in. Focus-follows-mouse pairs with
`autoraise = false`, which stops a window from being brought to the
front merely because the pointer crossed it; a click still raises. Environment `XKB_DEFAULT_*`
values remain more specific and win. If libxkbcommon rejects a
configured map, the error is logged and the session falls back to the
default usable keymap instead of aborting the login.

Binding flags retain their behavior: `bindl`/`locked` actions may run
on the lock screen, `binde`/`repeating` actions repeat after the
configured delay, and `bindr`/`release` actions fire on release without
overwriting a press action on the same chord. Hardware key names include
the touchpad toggle/on/off symbols and F23 used by Omarchy.

Omarchy's `hl.on("layer.opened")` screenshot handler is compiled only
when its body is a namespace guard plus `hl.bind` lifecycle
bookkeeping. Its Return, Tab and arrow bindings are installed while a
`selection` layer-shell surface is mapped and removed after the final
surface unmaps; a user binding on the same chord resumes afterwards.
Any additional side effect refuses the entire handler.

### `monitor =`

Monitor rules are resolved only after the compositor has the connected
outputs and their EDID facts. An exact output rule beats the last
catch-all rule. The supported transaction is:

- `preferred` mode (or omitted), resolved from that head's mode list;
- `auto` position, laid out left-to-right, or an explicit `XxY`;
- numeric scale from 0.5 through 4, or `auto` from physical DPI
  (1.0/1.5/2.0 thresholds).

Negative positions are normalized together so the logical desktop
starts at zero without changing relative placement. Any unsupported
field, `disable`, `mirror`, malformed position/scale, or explicit mode
refuses the whole line with the output and field in the log. The same
output state backs IPC and `zwlr_output_management`, so advertised
scale, renderer scale, shell geometry, and application fractional scale
cannot diverge.

---

## Precedence

Four layers, each beating the one above it:

1. Chonkstep's built-in defaults.
2. The preset your `desktop` / `keymap` line selects.
3. **Your Hyprland configuration**, read live — Omarchy's shipped
   defaults first, then your own `~/.config/hypr/` files in the order
   your entry file includes them.
4. **Your `~/.config/chonkstep/config.toml`.**

Layer 4 is not a new rule. It is the rule this format already has:
presets are applied to the defaults *before* the file's own keys are
walked, so writing any key out overrides them. The live read sits in
exactly that position. So:

- `[keybindings]` in `config.toml` has the last word on any chord, and
  `"none"` still unbinds one.
- A `[commands]` entry of your own replaces one the read declared.
- `autostart` and `terminal` in `config.toml` replace what was read.

Inside layer 3, ordering is Hyprland's own: last one wins, and an
`unbind` followed by a `bind` does what it says — which is only true
because included files are spliced in at the point their `require` or
`source` line sits, rather than read in some fixed order.

### The baked preset becomes the fallback

`wm_config::preset::OMARCHY_BINDINGS` — the hand-transcribed table
`keymap = "omarchy"` used to install — is now **the fallback, not a
second source of truth**. When a configuration is found, the live read
*replaces* that table outright; when nothing is found, or nothing
usable comes out, the table stands exactly as before. That is what it
is for: a machine where Omarchy is not installed, or is installed in a
shape this reader cannot follow. There is never a moment where both are
in effect.

The preset's *judgements* are carried over rather than re-argued: the
same `Unbound` reasons, the same "tiling-only stays dead", the same
scratchpad-to-`miniaturize` call. `docs/keybindings.md` still documents
that table, and it remains accurate for a machine with no Hyprland
configuration on it.

---

## Following your edits

Omarchy's menu writes these files. A rebind through their UI reaches a
**running** session within a second — no logout, nothing closed.

The session polls at 1 Hz, comparing a signature over every file the
last read actually opened (modification time, size *and* inode) plus
the modification times of the directories they live in. Polling rather
than inotify, for the same reason chonkstep follows Omarchy's theme by
polling: these files are **replaced**, not modified. `omarchy-menu`
writes a temporary file and renames it over the original, and upgrades
move whole trees; an inotify watch on a path that is unlinked and
recreated has to be re-armed by exactly the kind of code that goes
wrong at 3 a.m., where a signature comparison simply sees a different
inode. Watching the directories too is what notices a *new* file.

When it fires, the whole session re-resolves through the same one path
a `reload` binding takes, so a session that has followed a dozen edits
is indistinguishable from one that started where it now stands. Grabs
are taken and released by the same delta a reload uses; window rules
reach the next window that maps. What a live re-read cannot change is
`env` (see above) and `autostart` (it has already run).

---

## Turning it on and off

| In `~/.config/chonkstep/config.toml` | Effect |
|---|---|
| `desktop = "omarchy"` | On. The posture already means "chonkstep is the window manager for my Omarchy desktop", and it already replaced your keymap with a transcription of these files. |
| `keymap = "omarchy"` | On. Wanting Hyprland chords means wanting *your* Hyprland chords. |
| *(neither)* | Off. A plain chonkstep desk reads nobody else's files. |
| `hyprland_config = false` | **Off, from any posture.** The escape hatch. |
| `hyprland_config = true` | On, from any posture — including a plain chonkstep desk. |

It is not "whenever the files exist" on purpose. A `~/.config/hypr` is
left behind by trying Hyprland for an afternoon, and Omarchy's defaults
sit in `/usr/share` on any machine with the package installed. Reading
them automatically would mean that installing a package silently
replaced a chonkstep user's entire keymap from a file they have no
reason to think anything is still reading.

And it is not a *second* opt-in either: someone who wrote
`desktop = "omarchy"` has already accepted a frozen copy of these
bindings. Giving them the live original is not a surprise; it is the
thing the frozen copy was standing in for.

---

## When your configuration is broken

Reading someone else's file must never be able to break the session.
The rule is absolute: **a malformed file, an unknown directive or a
wild value is a logged warning and a skipped line — never a crash, and
never a refusal to start.**

- Recursion is depth-bounded, loops are iteration-bounded, the include
  graph is cycle-checked (through symlinks too) and budget-limited to
  256 files and 8 MiB, and regex patterns are compiled with a size cap
  by an engine that cannot backtrack.
- A file that is not valid UTF-8 is read lossily rather than dropped.
- A binding that will not parse costs you that binding. A rule that
  will not compile costs you that rule. A file that will not open costs
  you that file.
- **Nothing is ever executed.** The Lua reader parses; it does not
  interpret. The two conditions Omarchy branches on are answered by
  asking the file system. A config file must not be a code-execution
  path into the window manager.

The tests for this feed the parser unterminated strings, five thousand
nested braces, loops asking for a hundred million iterations, patterns
that would hang a backtracking engine, four hundred random byte
strings, and every truncation of the real files — and, end to end, boot
a whole session against a configuration tree made of garbage and check
that the desk still comes up usable.

---

## Seeing what happened

One `info` line per read, and one `debug` line per thing skipped:

```
INFO  hyprland-config: read the desktop's live Hyprland configuration
      files=42 bindings=153 commands=113 env=8 autostart=4
      float_rules=45 monitors=1 skipped=175
DEBUG hyprland-config: not carried over kind=bind what="SUPER + J (Toggle window split)"
      why="tiling-only: no meaning on a stacking desk"
```

`skipped` being large is normal and not a problem — a stock Omarchy
machine has around 150 directives this desktop has its own answer for.
Each one names itself, because "47 rules ignored" tells you nothing you
can act on and "float rule carries `match:xwayland 1`, which this
reader does not implement" tells you exactly which line to rewrite.

## See also

- [`omarchy-mode.md`](omarchy-mode.md) — the `desktop = "omarchy"`
  posture this rides on.
- [`keybindings.md`](keybindings.md) — the baked keymap, which is what
  you get when there is no configuration to read.
- [`omarchy-integration.md`](omarchy-integration.md) — the menu, the
  shell and the theme.
