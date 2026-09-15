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
   `toggle-maximize`, `fullscreenstate 0 2` → `toggle-tiled-fullscreen`
   (any other `fullscreenstate <internal> <client>` pair with each axis
   0, 1 or 2 binds too), `workspace 4` → `workspace 4`, `workspace e+1` →
   `workspace-next-occupied` (the next workspace that has windows,
   wrapping; the bare `+1` is `workspace-next`, which steps by index),
   `workspace previous` → `workspace-previous`, `focusmonitor +1|l|NAME`
   → `focus-monitor …`, `movecurrentworkspacetomonitor l|NAME` →
   `move-workspace-to-monitor …` (a Space move under separate Spaces;
   refused with a reason on the shared desktop), `movefocus l/r/u/d` →
   geometry-ranked directional focus.
2. **A command** becomes a `run` binding naming that command, declared
   automatically in `[commands]` under a generated `hypr:…` name. This
   is the whole "install chonkstep, keep your Omarchy" claim made
   literal: `SUPER + SPACE` opens Omarchy's menu because it runs
   Omarchy's `omarchy-menu`, not an imitation of it.
3. **Everything else stays unbound**, with the reason logged.

Workspace styles are native: `dwindle` selects Mosaic and `scrolling` selects
Flow. Omarchy's layout toggle and floating toggle work without a config change.
`Super+Shift+L` returns to Freeform. Tree-only messages such as `togglesplit`
are quiet no-ops in every style, including Flow; unavailable window groups
remain explicitly unsupported.

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

  Your own conditions combine the way Lua combines them. `and`, `or`,
  `not`, `==`, `~=` and the orderings follow Lua's precedence, and an
  unset global is `nil`, so `omarchy_default_bindings = nil` turns the
  defaults back on. A `local` stays in its own file and never reaches
  `_G`. A part of a condition only running code could answer decides
  nothing unless the rest decides it: `o.shell_succeeds(…) and false` is
  false, while `o.shell_succeeds(…) and true` skips the block. A name
  that a skipped construct could have set (a `while` body, a function,
  an `if` that could not be answered) is not taken to be `nil` either.
  An `if` whose condition is not followed by `then` is skipped whole.

### Switch bindings

`switch:on:NAME`, `switch:off:NAME` and `switch:NAME` bind a hardware
switch instead of a key, in both syntaxes: `bindl = , switch:on:Lid
Switch, exec, …` in conf, `o.bind("switch:on:Lid Switch", nil, …, {
locked = true })` in Lua. `on` is a lid closing or tablet mode starting,
`off` the reverse, and a bare name answers both. `NAME` is the device
name libinput reports, the one `hyprctl devices` lists under `switches`,
and it must match exactly: Apple Silicon calls its lid `Apple SMC
power/lid events`, which is why Omarchy binds both names. A switch
binding runs its command the way a key binding does, and `unbind` takes
the same spelling. While the session is locked only bindings marked
locked (`bindl`, `locked = true`) run, so Omarchy's lid handlers still
answer on the lock screen. Opening the lid wakes sleeping screens and
counts as activity for idle timers; closing it does neither. One
configuration holds at most 32 switch bindings.

On a stock Omarchy install, closing the lid runs
`omarchy-system-lid-close`, which locks the session straight away when
no external monitor is connected. The clamshell handler Omarchy binds
beside it, `omarchy-hyprland-monitor-clamshell`, writes
`hl.monitor({ output = "<eDP>", disabled = true })` into
`~/.local/state/omarchy/toggles/hypr/` and runs `hyprctl reload`, which
this desktop reads and applies (see [Monitors](#monitors) below); the
script itself stays outside the served list until its every request is
proved served there. The baked Omarchy keymap holds key chords only, so
switch bindings come from the live configuration.

### Window rules

`windowrule`, `windowrulev2` and `o.window` / `hl.window_rule`, in all
three syntaxes Hyprland has shipped:

```text
windowrule   = float, ^(steam)$                          # v1
windowrulev2 = float, class:^(steam)$, title:^(Steam)$   # v2
windowrule   = float on, match:class steam               # 0.53+
```

The supported properties are `float`, `size`, `move`, `center`,
`idle_inhibit`, `pin`, `no_focus`, `no_initial_focus`,
`focus_on_activate`, `fullscreen`, `maximize`, `suppress_event`,
`scroll_touchpad`, `workspace`, `opacity`, `no_dim` and
`no_screen_share`. They match `class`, `title` and `xdg_tag` as regular
expressions, matched against the entire class, title or tag, as in
Hyprland's `RE2::FullMatch`. Use `.*` when a substring is intended.
Last matching rule wins independently for each property.

`xdg_tag` (`match:xdg_tag`, `xdgTag:` in the v2 form, `xdg_tag =` in a
Lua `match` table) reads the window's `xdg_toplevel_tag_v1` tag: the
application's own stable, untranslated name for one of its windows
(`main`, `preferences`, `quake`), which unlike the title does not change
with the document and unlike the class tells one of an application's
windows from another. Rules are evaluated once, when the window maps,
against the tag it carried then — a client tags a window before its
first commit, so that is the tag the protocol means. A tag set or
changed after the map reaches `hyprctl clients` at once but does not
re-run the rules, exactly as a later title change does not. A window
whose client never tagged it has an empty tag, which only `^$` matches.

`size` and `move` take two values, each a number or one of Hyprland's
layout expressions — arithmetic over `monitor_w`, `monitor_h`,
`window_w` and `window_h` with `+ - * /`, unary minus and parentheses,
in logical pixels:

```lua
o.window({ tag = "pip" }, {
  float = true, size = { 600, 338 },
  move = { "(monitor_w-window_w-40)", "(monitor_h*0.04)" },
})
o.window("^WebcamOverlay-small$", {
  size = { "(monitor_h*4/25)", "(monitor_h*9/50)" },
  move = { "(monitor_w-monitor_h*4/25-40)", "(monitor_h-monitor_h*9/50-40)" },
})
```

The expressions are compiled when the file is read and evaluated when
the window maps, against the monitor it maps on — the whole monitor,
not its workarea, which is what Omarchy's rules are written against.
`size` is evaluated first; `move` then sees the resolved size as
`window_w`/`window_h`, so `(monitor_w-window_w-40)` means "40 in from
the right edge of the window this rule just sized". `window_w` and
`window_h` are the frame's visual size, chrome included. The position is
relative to the monitor's own origin and is converted with that
output's scale, so a mixed-DPI desk places the window where the rule
says on whichever head it opens on. The frame is then pulled inside the
workarea: a rule can never put a window under a reserved bar or off the
edge of the screen. An expression longer than 128 bytes or nested more
than 16 levels deep is refused when the file is read; one that does not
come out finite (a division by zero) drops that property when the
window maps, and the rule's `float` still applies. `move` is replaced
by an explicit `center`; Hyprland's other `move` spellings (`cursor`,
`onscreen`, percentages) and `keep_aspect_ratio` are reported and not
read.

`idle_inhibit` reads four modes. `always` (also `on`, `true`, `1`, `yes`)
inhibits idle while a matching window is visible on the current workspace
or pinned, without requiring keyboard focus. `focus` additionally requires
the window to hold keyboard focus, and `fullscreen` additionally requires
it to be fullscreen, which is how Omarchy keeps a game or stream awake
without letting a launcher's windowed library do the same. `none` (also
`off`, `false`, `0`, `no`) explicitly clears an earlier matching rule. Any
other value is reported and ignored. A minimized, parked or locked-away
window never inhibits, whatever its mode. `pin` makes the client
sticky across workspaces. Focus exclusions affect initial focus and
later activation requests separately. Fullscreen and maximize are
applied after initial placement, with maximize underneath fullscreen so
unfullscreen restores the expected state.

`suppress_event` takes a list of client requests to ignore. `maximize`
and `fullscreen` ignore an application's own request to enter that state,
which is how Omarchy's first rule keeps every window in its tile when an
application asks to be maximized. The refusal is answered with the
window's unchanged state. ChonkStep's own verbs, the window menu and IPC
still apply, and an application may still leave a state it did not ask
for. `activate` and `activatefocus` stop activation requests from
focusing the window, like `focus_on_activate = false`. Any other event is
reported by name.

`workspace` maps the window somewhere other than the current
workspace: a number (`workspace = "3"`), `special` for the default
special workspace, or `special:NAME`. Adding `silent` (`"special
silent"`, `"3 silent"`) sends it there without following — no switch,
no shown overlay and no initial focus — which is how Omarchy's
`apps/browser.lua` keeps Chromium's "is sharing your screen" bar off
the desk, and out of the tiling, for the length of a call. Without
`silent` a numbered target is switched to and a special one shown.
`name:…` and the relative forms are refused by name: chonkstep
workspaces are numbered. `hl.workspace_rule` for a special workspace
(`gaps_out`, `on_created_empty` and `dim_special`, which Omarchy's
agent console sets for `special:scratchpad`) is not read yet.

`opacity` is Omarchy's focus cue. It takes one to three numbers, each
clamped to `0..1`: the body alpha while the window is focused, while it
is not, and — a third value — while it is fullscreen. Omarchy's default
is `0.985 0.96` for every window, `1.0 0.985` for browsers, and `1 1`
for video players, games, virtual machines and colour-critical work,
which it writes by removing the `default-opacity` tag (below). With no
third value a fullscreen window is opaque whatever the other two say,
so direct scanout is untouched by a translucent rule. The alpha
applies to the whole window — content, popups, chrome, border and
shadow — and never to input: a click lands where it always did.
`SUPER + BACKSPACE` (Omarchy's `omarchy-hyprland-window-transparency-toggle`,
the native `toggle-opaque`, or `hyprctl dispatch setprop … opaque toggle`)
forces the focused window opaque for the rest of the session and back.
Translucency is not free: everything beneath a translucent window is
composited too. `window_opacity = false` in `config.toml` draws every
window opaque and gives that occlusion back; `docs/performance.md`
records the element counts either way.

`no_dim` exempts a window from `decoration:dim_inactive`, the one part
of Hyprland's `decoration` table this desktop reads: `dim_inactive = true`
with `dim_strength` (Hyprland's default `0.5`) darkens every unfocused
window by drawing one black quad in front of it. The window itself stays
opaque, which is what makes dimming the cheaper focus cue of the two.

`no_screen_share` is what Omarchy writes for 1Password and Bitwarden:
the window is drawn on your screen as usual, and every capture the
compositor renders shows an opaque grey rectangle where the window,
its titlebar and its popups are. That covers the portal screen share
in a call (`xdg-desktop-portal-wlr`, over `zwlr_screencopy`), a
recording by `wf-recorder` or by this desktop's own recorder, a
`grim` screenshot, this desktop's own region and window screenshots,
and "share this window" through `ext-image-copy-capture`, which is
answered with a solid image of the window's size rather than refused.
Only a rule sets it; nothing a client asks for clears it. Two things
are outside its reach. A recorder that reads the scanned-out
framebuffer from KMS directly - gpu-screen-recorder's default backend,
which Omarchy's screen recording uses unless
`OMARCHY_SCREENRECORD_USE_PORTAL=true` - never asks the compositor
for a picture, so it records the window exactly as the screen shows
it; no compositor can redact that path. And a capture that asks for
the pointer still gets it drawn over the rectangle.

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
**Tag removal is followed too**, in file order: `tag -default-opacity`
in an app file takes that window out of the opacity rule written for
the tag afterwards, and a removal made on the strength of another tag
(`match:tag chromium-based-browser` → `tag -default-opacity`) is
followed when that tag is carried by class or title. Membership settles
the way Hyprland's repeated rule passes settle it — the last matching
add or remove decides — so Omarchy's `floating-window` rules, written
above the lines that add the tag, still resolve. A tag whose carriers
are themselves tag-matched is refused with a log line.

This replaces a hardcoded rule that used to live in `wm-core`: any
window whose app-id started `org.omarchy.` mapped at 875×600. That
number was a transcription of one of Omarchy's lines, and it got every
*other* float rule wrong — Steam wants 1100×700, picture-in-picture
600×338, the About box 920×480. Reading the real rules gets all
thirty-eight of them right. The hardcoded rule stays behind this one as
the answer for a machine with nothing to read.

### Workspace layout

The one layout setting that is not a look. `general.layout` decides
whether windows tile at all, and chonkstep already answers to
Hyprland's names for the two styles it has — `dwindle` is **Mosaic**,
`scrolling` is **Flow** — so the name is read and nothing about
Hyprland's drawing comes with it.

```lua
hl.config({ general = { layout = "dwindle" } })          -- Omarchy's looknfeel.lua
hl.workspace_rule({ workspace = "2", layout = "scrolling" })
```

```ini
general {
    layout = dwindle
}
workspace = 2, layout:scrolling
```

- **`general.layout`** is the style every workspace *starts* in —
  including one first reached with `SUPER+7`. Omarchy ships `dwindle`,
  which is why an Omarchy desktop tiles from the first login rather
  than after a `SUPER+L` on every workspace.
- **A workspace rule's `layout`** is that one workspace's starting
  style, ranked above the default. Omarchy's own
  `omarchy-hyprland-workspace-layout-toggle` saves one of these per
  workspace under `~/.local/state/omarchy/workspace-layouts/`, which
  its `toggles.lua` reads back at login, and so does this. Workspace
  `N` is chonkstep's index `N−1`, exactly as the IPC path resolves
  `hyprctl eval 'hl.workspace_rule(…)'`. Only numbered workspaces from
  1 to 99 are read: a `special:` workspace, a `name:`, a range and
  every other key in the rule (`gapsin`, `monitor`, …) each earn their
  own logged line.
- **An unknown layout name** (`master`, `hy3`) is logged with its
  name and changes nothing; the workspace keeps the style it would
  have had.

Precedence, highest first: a restored session's own workspace modes
(`restore_session = true`, so an existing opt-in sees no change), then
the workspace rule, then `general.layout`, then Freeform.

**A live re-read never undoes a choice you made.** The watch fires on
any file in the tree, so it applies only what changed: a rule whose
value differs from the last read, and a changed default only on
workspaces that never had a style chosen for them — by `SUPER+L`, by
IPC, by a restored session or by a rule. A workspace you switched to
Flow stays in Flow through an unrelated edit, while Omarchy's toggle
script saving a *different* layout for a workspace still lands.

A native `SUPER+L` changes the live workspace and is not written back
to Omarchy's `workspace-layouts/` files; chonkstep's own session
store records the modes, and reads them back when `restore_session`
is on.

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

Wayland startup also removes inherited `GDK_SCALE` and `GDK_DPI_SCALE`, including
stale values in the activation environment. GTK and Steam receive scale through
Wayland or XSETTINGS/X resources. Per-application launch commands can still set
an explicit override when needed.

Blanket activation-environment commands are never admitted as
autostart. In particular, `systemctl --user import-environment $(env
...)` and `dbus-update-activation-environment --all` are skipped with a
named reason. The session launcher publishes only its curated Wayland,
desktop, menu-prefix, backend, and IPC variables; test sessions publish
nothing to the real bus.

Editing an `env` line takes effect at your next login, not on the live
re-read. A process's environment is fixed when it starts.

### Animations → `[motion]`

Turning motion off is a comfort and accessibility preference, not a
look, so the switches are read even though the styling around them is
not. Omarchy's own override template offers it as a commented-out
block; uncommented, it works here:

```lua
hl.config({
  animations = {
    -- Disable all animations.
    enabled = false,
  },
})
```

or, in conf, `animations { enabled = no }`. Either turns off every
transition this desktop starts on its own: spatial-layout reflows,
Overview opening and closing from the keyboard, and the settle after a
released swipe. Fingers on a touchpad still move the desktop 1:1 while
they are down — that is input, never animation.

Per-leaf switches are read for the leaves this desktop has a
transition for:

| Hyprland leaf | What it turns off here |
|---|---|
| `global` (`hl.animation({ leaf = "global", enabled = false })`, `animation = global, 0, …`) | Everything, exactly like `animations.enabled = false`. |
| `windows`, `windowsMove` | Window geometry motion: the reflow when a spatial layout changes. |

Later wins, so a switch in your own file lands over Omarchy's
defaults. Every other leaf — `border`, `fade*`, `layers*`,
`workspaces`, `specialWorkspace` — names something this desktop draws
differently or not at all and is logged by name. The speed, curve and
style on any line, and every `bezier`/`hl.curve` definition, are
declined: this desktop's motion is one critically damped spring, and
its one knob is the native table below.

The native table in `config.toml` wins over all of it:

```toml
[motion]
enabled = true          # false: every compositor-started transition completes in one frame
layout = true           # spatial-layout reflow motion
overview = true         # keyboard and pointer Overview open/close
gesture_settle = true   # the spring after a released swipe
speed = 1.0             # multiplies the spring's stiffness; 0.25..=4
```

A reload applies it to transitions already in flight, which land on
their targets.

---

## What is deliberately not read

Everything below is *logged* when it is met — one line naming the
specific directive, not a count. Turn on `RUST_LOG=debug` to see them.

| Not read | Why |
|---|---|
| Hyprland requests chonkstep does not serve — `hyprctl`, and `omarchy-hyprland-*` scripts outside [the list below](#omarchys-hyprland-scripts) | Chonkstep answers Hyprland's IPC, but only with the requests it can apply, and `hyprctl` exits zero on a refusal, so a binding whose request is refused would be a key that silently does nothing. A script therefore runs only when every request it sends is proven served. The same rule filters chonkstep's Omarchy menu rows and `exec-once` lines. `hyprpicker`, `hyprlock` and `hypridle` are *not* caught by it: they are ordinary Wayland clients and work here. |
| Gaps, borders, rounding, blur, shadows, layouts (`hl.config`, `general { … }`, `decoration { … }`) | Hyprland's look. This desktop has its own — a theme, a titlebar, a decoration policy. Following them would mean drawing a NeXTSTEP frame in Hyprland's border colour. Three exceptions: `general.layout`, [read above](#workspace-layout) (the per-layout tables `dwindle { … }`, `master { … }`, `scrolling { … }` are not); the animation *switches*, because turning motion off is a preference, not a look — see [Animations](#animations); and `decoration:dim_inactive` with `dim_strength`, a focus cue rather than a look, read as described under [window rules](#window-rules). |
| Layer rules (`layerrule`, `hl.layer_rule`) | They configure Hyprland's layer-shell implementation. This compositor has its own. |
| Whole-desktop interaction policy (`follow_mouse`, gestures) | Chonkstep owns focus and gesture policy: use `focus_follows_mouse` and native [`[input.gestures]`](gestures.md). Arbitrary Hyprland gesture bindings remain declined. Device properties listed below are applied; remaining declined values are logged. |
| Unsupported window-rule properties | `no_blur`, `keep_aspect_ratio`, `rounding`, … are each logged with their matcher. Tags used to select another supported rule are resolved, removals included. |
| Window rules carrying a matcher not implemented here (`match:xwayland 1`, `match:workspace 5`, `match:fullscreen 0`) | Refused **whole**. Applying a rule on the matchers that *were* understood turns "float this one XWayland window" into "float every window of this class". |
| A `size` or `move` written in a form other than a number or a layout expression (`move cursor 0 0`, `size 50% 50%`, `move onscreen`) | Only the arithmetic Omarchy's rules use is read — see [window rules](#window-rules). The property is skipped with its text; the rule's other properties still apply. |
| Mouse and wheel bindings (`bindm`, `mouse:272`, `mouse_up`) | Not key chords; this config format cannot express one. [Switch bindings](#switch-bindings) are read. |
| `exec` (as opposed to `exec-once`) | It re-runs on every config reload, which here would mean on every poll. Taking it as autostart would start a fresh copy each time you edited anything. |
| `submap`, `plugin`, `bezier` (Lua `hl.curve`), and the speed, curve and style of every `animation` line (Lua `hl.animation`) | Hyprland's own machinery. Every binding inside conf `submap = name … reset` or Lua `hl.define_submap` is skipped with its chord and submap; it is never promoted to a global grab. This desktop's motion is one spring; only the on/off switch of an animation line is read, and only for the leaves named under [Animations](#animations). |
| Workspace rules other than `layout`, and rules for `special:`, `name:` and range selectors | Only [the layout of a numbered workspace](#workspace-layout) is read. Every other rule and every other selector is logged by name. |
| Lua calls that act while Hyprland runs (`hl.timer`, `hl.dispatch`, `hl.get_*`), and any other call with no configuration meaning here (such as `table.insert`) | None of them configures anything as the file is read. Each is logged by name, so a call this reader cannot place is never dropped silently. |
| `hl.on("layer.opened")` selection bindings | Read as a namespace-scoped keymap. It is installed only while a matching layer-shell surface is mapped and removed after the last such surface closes. A handler with unknown side effects is refused whole. |
| Unsupported `monitor =` lines | A line containing mirror, or an extra field other than a 0/90/180/270-degree transform or `disabled`, is refused whole. Explicit modes, those transforms and `disable` are supported as described below. |

### Omarchy's Hyprland scripts

Omarchy implements several window and display chords as
`omarchy-hyprland-*` scripts that drive the compositor through `hyprctl`.
These six send only requests chonkstep's Hyprland IPC applies, so their
bindings, menu rows and autostart lines run as written. The list lives in
`crates/wm-config/src/hyprland/dispatch.rs`, and
`crates/chonk-hyprland-ipc/tests/protocol.rs` feeds every request each
script sends through the IPC, failing if one is refused or a field the
script reads is missing. A script cannot join the list without that proof.

| Script | Omarchy's use of it |
|---|---|
| `omarchy-hyprland-window-pop` | `SUPER + O`: pop the window out, floating and pinned, or put it back |
| `omarchy-hyprland-window-width` | `SUPER + ALT + Home` / `SUPER + Home`: save / restore the window's width |
| `omarchy-hyprland-window-close-all` | `CTRL + ALT + DELETE`: close every window, then show workspace 1 |
| `omarchy-hyprland-monitor-scaling` | `SUPER + SLASH` / `SUPER + ALT + SLASH`: step the focused monitor's scale |
| `omarchy-hyprland-workspace-layout-toggle` | The menu's Workspace Layout row. Its `SUPER + L` binding takes chonkstep's own `toggle-layout`. |
| `omarchy-hyprland-window-tiled-fullscreen-toggle` | `SUPER + CTRL + F`: tell the window it is fullscreen in its tile, or stop. It reads `fullscreenClient` back to decide which. |
| `omarchy-hyprland-window-transparency-toggle` | `SUPER + BACKSPACE`: force the focused window opaque, or let its opacity rule apply again. The binding takes chonkstep's own `toggle-opaque`; the script's `setprop … opaque` requests are served for a menu row or a shell. |

On a Freeform workspace the pop-out's float toggle does nothing, because
Freeform has no layout to float a window out of; the window is still
resized, centred, pinned and raised.

`hyprctl` itself, and every other `omarchy-hyprland-*` script, is refused
as "commands Hyprland beyond the requests ChonkStep serves". That is also
the answer for a script a future Omarchy adds. The ones Omarchy binds or
starts are refused with the piece they need:

| Script | Why not here |
|---|---|
| `omarchy-hyprland-window-gaps-toggle` | toggles Hyprland's gaps, which ChonkStep does not read |
| `omarchy-hyprland-window-single-square-aspect-toggle` | toggles a Hyprland layout option, which ChonkStep does not read |
| `omarchy-hyprland-monitor-internal` | disables an output, which ChonkStep does not do |
| `omarchy-hyprland-monitor-internal-mirror` | mirrors an output, which ChonkStep does not do |
| `omarchy-hyprland-monitor-clamshell` | disables an output, which ChonkStep does not do |
| `omarchy-hyprland-monitor-watch` | disables an output, which ChonkStep does not do |

### Bindings this desktop has no verb for

One additional chord family remains unbound and
is worth knowing about:

- **The universal clipboard chords** (`SUPER + C/V/X`), which Omarchy
  builds by synthesising `Ctrl+C` at the seat. That is the
  compositor's own input path; no command could stand in.

Directional focus (`movefocus l/r/u/d`) follows actual geometry in Freeform
and Mosaic. Flow follows its horizontal sequence; up/down does nothing.
`movewindow` and `swapwindow` directions reorder managed windows while keeping
focus. `resizeactive` changes Mosaic boundaries or the focused Flow width,
in both syntaxes, including Omarchy 4's Lua
`hl.dsp.window.resize({ x = …, y = …, relative = true })` chords. Its
deltas are logical pixels, converted by the scale of the focused
window's output exactly as `hyprctl dispatch resizeactive` is. The
exact-size forms (`resizeactive exact w h`, and the Lua call without
`relative = true`) have no verb here and are refused rather than read
as a delta.
`fullscreen 0` toggles real fullscreen; `fullscreen 1` toggles maximize within
the workarea. Floating windows retain traditional movement and resizing.

Silent workspace sends (`movetoworkspacesilent 1..99`) are native too:
the active window moves without changing the current workspace, and an
exposed window receives focus. So is the scratchpad.
`togglespecialworkspace [NAME]` (Lua
`hl.dsp.workspace.toggle_special("NAME")`) shows the named special
workspace as an overlay on the active output, above pinned windows, or
hides it again when it is the one shown there; `movetoworkspacesilent
special:NAME` (Lua `hl.dsp.window.move({ workspace = "special:NAME",
follow = false })`) sends the focused window there without following;
and the following form, `movetoworkspace special:NAME`, shows the
special and keeps the keyboard on the window. A bare `special` is the
default special workspace. Omarchy's `SUPER + S` and `SUPER + ALT + S`
are the first two, and a window sent away comes back with the chord
that hid it. `miniaturize` keeps its own chord and its place in the
window menu. Named workspaces (`name:…`) remain unbound.

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

`binds.hide_special_on_workspace_change`, which Omarchy turns on, makes
a workspace switch hide the special workspace shown on the output the
switch lands on. Off — Hyprland's own default — the scratchpad stays
shown across the switch. The rest of the `binds` table is Hyprland's
own binding behaviour and is reported rather than carried.

`kb_rules`, `kb_model`, `kb_layout`, `kb_variant`, and `kb_options`
build the seat's xkb keymap. A value Hyprland would compute as it runs,
such as Omarchy 4's `kb_layout = vconsole.XKBLAYOUT or "us"`, is logged
and left unset rather than passed on as the text of the expression, and
a window rule or `hl.monitor` line with such a value is refused whole.
Whatever of `kb_layout`, `kb_variant`, `kb_model` and `kb_options` is
still unset then comes from `/etc/vconsole.conf`, the file `localectl`
writes and Omarchy's `input.lua` reads. So for each setting, a
non-empty `XKB_DEFAULT_*` variable wins, then a value the configuration
spells out (an empty one included), then `/etc/vconsole.conf`, then
libxkbcommon's default. Omarchy's `us,` prefix for a layout with no
Latin letters is not applied. `repeat_rate` and `repeat_delay` configure
both client key repeat and `binde` actions. `numlock_by_default`, which
Omarchy turns on, locks Num Lock when the session installs its keymap, when
a later edit replaces the keymap, and when a reload newly turns the setting
on. Any other reload leaves Num Lock where you put it, so pressing the key
to turn it off is not undone by the next edit to your configuration. These
hardware-facing values transfer; whole-desktop interaction policy does not. In particular,
Hyprland's `follow_mouse` is logged and ignored—even when it is `1` in
Omarchy's shipped defaults—so a stock Omarchy install retains
chonkstep's click-to-focus default. Set `focus_follows_mouse = true` in
chonkstep's own `config.toml` to opt in. Focus-follows-mouse pairs with
`autoraise = false`, which stops a window from being brought to the
front merely because the pointer crossed it; a click still raises. Environment `XKB_DEFAULT_*`
values remain more specific and win. If libxkbcommon rejects a
configured map, the error is logged and the session falls back to the
default usable keymap instead of aborting the login.

Pointer configuration is also carried from both classic `input {}` /
`touchpad {}` blocks and Omarchy's Lua tables. `sensitivity` and
`accel_profile` configure libinput acceleration; `tap_to_click`,
`disable_while_typing`, `clickfinger_behavior`, and `left_handed` are applied
where the device advertises them.

The touchpad's tapping settings reach touchpads only: `touchpad:tap-and-drag`
(`tap_and_drag` in a Lua table), `touchpad:drag_lock` (0 or 1; libinput's
sticky mode 2 is newer than the libinput binding chonkstep is built with and
is refused by name), `touchpad:tap_button_map` (`lrm` or `lmr`), and
`touchpad:drag_3fg` (0 off, 1 three fingers, 2 four fingers). Three-finger
drag needs libinput 1.27 or later. On an older libinput the session still
starts, and each touchpad that was asked for it logs
`drag_3fg: requires a newer libinput`. `touchpad:middle_button_emulation`
applies to touchpads, while `input:scroll_method` (`2fg`, `edge`,
`on_button_down` or `no_scroll`) and `input:scroll_button` (an evdev button
code, 0 through 300, 0 meaning the device's own) apply to every other
device, which is where a trackpoint or trackball wants them. `[input]` and
`[input.touchpad]` in `config.toml` take the same scroll and middle-button
keys for each class. Removing any of these keys restores each device's
libinput default. A value out of range is logged with its key and skipped.

A device rule applies to one device, named exactly as `hyprctl devices`
lists it: a `device { name = …; … }` block, or `hl.device({ name = "…", … })`
in Lua. A rule carries `enabled`, `sensitivity`, `accel_profile`,
`natural_scroll`, `left_handed` and `tap_to_click`, each laid over the
settings above for that device alone. Any other key is logged and skipped,
and at most 64 rules are read. `enabled = false` stops the device sending
events, through hotplug and resume, and is never applied to a device that
has keys. Omarchy's touchpad and touchscreen toggles keep a disable as one
line of data in `~/.local/state/omarchy/toggles/hypr/<kind>-disabled-name`.
When `toggles.lua` calls `disabled_input_device`, that line is read as a
device name, never as Lua, and becomes the same rule. The toggles reach the
running session through `hyprctl eval hl.device(…)`, described in
[hyprland-ipc.md](hyprland-ipc.md).

Scrolling is configured per device class. `input:natural_scroll` and
`input:scroll_factor` (or `[input]` in `config.toml`) apply to mice,
trackpoints and every other device that is not a touchpad;
`input:touchpad:natural_scroll` and `input:touchpad:scroll_factor` (or
`[input.touchpad]`) apply only to touchpads, so Omarchy's touchpad settings
never invert or slow a mouse wheel. Removing a natural-scroll key restores
each device's libinput default. `scroll_factor` multiplies axis motion after
libinput, chosen by the event's source (finger scrolling is touchpad-class),
so the configured speed also works on the nested backend, and
high-resolution wheel units keep their fraction between events instead of
rounding it away. A `scroll_touchpad` window rule replaces the touchpad
factor while the pointer is over a matching window. Unsupported capabilities
are named per device without rejecting the rest of the configuration.

The `cursor {}` block and Omarchy's `cursor` table carry the settings that
decide when the pointer hides. `hide_on_key_press`, which Omarchy turns on,
hides it while you type into a window; a bare modifier or a key a binding
consumes does not. `hide_on_touch` hides it on a touch, and
`inactive_timeout` hides it after that many seconds without pointer input
(zero never does; the compositor honours one second through an hour). Any
pointer motion, click, scroll or tablet input shows it again. The warp keys
(`warp_on_change_workspace`, `no_warps` and the rest) are declined by name:
chonkstep moves the pointer only when you do or a script asks with
`cursor.move`. The zoom keys are not implemented. A `[cursor]` table in
`config.toml` takes the same three keys and wins over both.

Active pointer locks and confinement temporarily suspend `disable_while_typing`
so games can receive keyboard and touchpad motion together. This includes
XWayland pointer grabs. Releasing capture, changing focus, or opening the
overview restores the configured preference (or each device’s libinput default).
Hotplug and live reload honor the current capture state.

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
catch-all rule. The selector may be a connector name such as `DP-2` or
Hyprland's stable `desc:make model serial` form (copy the `description`
from `hyprctl monitors`); the latter follows a physical display when it
moves to another dock port. A selector that matches nothing is named in
the log together with the connected connectors and EDID descriptions.
The supported transaction is:

- `preferred` (or omitted), `highrr`, `highres`, `WIDTHxHEIGHT`,
  `WIDTHxHEIGHT@RATE`, or `preferred@RATE`, resolved from that head's
  advertised mode list (with measured-refresh tolerance);
- `auto` position, laid out left-to-right, or an explicit `XxY`;
- numeric scale from 0.5 through 4, or `auto` from physical DPI
  (1.0/1.5/2.0 thresholds). With no matching rule and no global ChonkStep
  scale override, `auto` is also the default, matching current Hyprland and
  niri behavior. High-resolution internal panels use a conservative
  resolution fallback when the driver omits physical dimensions;
- `transform, 0` through `transform, 3` for 0/90/180/270-degree
  output rotation, also accepted as `transform = N` in Lua `hl.monitor`.
  Values map directly to Smithay's `Normal`, `_90`, `_180`, and `_270`.
  The intended clockwise direction still needs confirmation on a physical
  panel: the nested `winit` test backend refuses non-normal transforms
  (see [#144][transform-direction]).

[transform-direction]: https://github.com/iconidentify/chonkstep/issues/144

Negative positions are normalized together so the logical desktop
starts at zero without changing relative placement. An unadvertised
mode, unsupported field, `mirror`, or malformed
position/scale refuses the whole line with the output and field in the log. The same
output state backs IPC and `zwlr_output_management`, so advertised
scale, renderer scale, shell geometry, and application fractional scale
cannot diverge.

`monitor = eDP-1, disable` and Lua `hl.monitor({ output = "eDP-1",
disabled = true })` take a connected output out of the desktop layout.
The output is *parked*: no `wl_output` global, no place in the layout,
no workspaces, and on the DRM session a cleared crtc with the connector
kept, so putting it back is the DPMS-on path rather than a fresh
modeset. Its windows move to the remaining outputs and its lock surface
is released the way an unplug releases one, while the other outputs stay
covered; when it returns, the locked scene is presented on it before any
client content. A rule that disables every connected output keeps the
first, and the log says so: the desktop is never without an output. A
line with `disabled` carries no geometry, and one given beside it is
not applied. Mirroring is still refused whole. The same disable is
available live, from `hyprctl keyword monitor NAME,disable`,
`hyprctl eval hl.monitor({ output = NAME, disabled = true })` and
wlr-output-management, and `monitors all` lists a parked output with
`disabled: true`; see [hyprland-ipc.md](hyprland-ipc.md).

Omarchy's toggle directory, `~/.local/state/omarchy/toggles/hypr/`, is
read: its `toggles.lua` loads every `*.lua` there through
`require_all.files(toggles_dir, nil, { exclude = … })`, and that one
fan-out — `paths.state_home` plus a literal, with no module prefix — is
followed, honouring the `exclude` table so the legacy
`touchpad-disabled` and `touchscreen-disabled` names are never read as
code. This is where `omarchy-hyprland-monitor-clamshell` and
`omarchy-hyprland-monitor-internal` write their `disabled = true` line
before running `hyprctl reload`. Any other `require_all.files` call
without a module prefix stays ignored, and named as such.

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
same `Unbound` reasons and the same deliberate handling of unsupported
operations. `docs/keybindings.md` still documents
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
reach the next window that maps. The keyboard half of `input` —
`kb_rules`, `kb_model`, `kb_layout`, `kb_variant`, `kb_options`,
`repeat_rate`, `repeat_delay` — is re-resolved and installed on the
seat, by exactly the rules startup used, so `XKB_DEFAULT_*` keeps
winning over the file across a reload rather than only at login. A
keymap libxkbcommon rejects costs the edit and not the session: the
running keymap is kept and the refusal is logged, and `hyprctl devices`
reports the layout actually in force rather than the one that was asked
for.

What a live re-read cannot change is `env` (see above), `autostart` (it
has already run), and — from the file watch — `monitor` lines. Monitor
rules are applied at startup, when a connector is hot-plugged, and on an
**explicit reload** (`hyprctl reload`, the reload marker, a bound
`reload` key), never from the one-second file watch or from Omarchy
theme following: re-applying a mode or a position to a live output is a
modeset, and doing it on every save of an unrelated key would reflow
the desk. A reload compares each connector's resolved rule with the one
last applied and touches only the connectors whose rule changed: a
changed scale, position, mode or transform is applied to that live
output; a rule that now disables the output parks it; a rule that no
longer does puts it back. A reload that changes no monitor rule performs
no modeset and moves no output, and an output disabled or enabled at
runtime keeps that state across a reload whose rule for it did not
change — a changed rule wins. Changing an output live without a reload
has its own verb, `hyprctl eval hl.monitor({ output = …, … })`.

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
- Work is bounded per file, not only per construct, because nested
  loops multiply. One Lua file walks at most 65,536 statements (every
  pass of a loop counts) and contributes at most 8,192 directives. One
  statement chains at most 256 operators, a value bound to a name is
  capped in size, and following a name bound to a name is metered, so
  `o = o or {}` followed by `if o then` is a condition that cannot be
  answered rather than a hang. Past any bound, what is left is skipped
  with a line naming the bound.
- A file that is not valid UTF-8 is read lossily rather than dropped.
- A binding that will not parse costs you that binding. A rule that
  will not compile costs you that rule. A file that will not open costs
  you that file.
- **Nothing is ever executed.** The Lua reader parses; it does not
  interpret. The two conditions Omarchy branches on are answered by
  asking the file system. A config file must not be a code-execution
  path into the window manager.

The tests for this feed the parser unterminated strings, five thousand
nested braces, loops asking for a hundred million iterations, five
nested loops asking for a billion, names bound to themselves, two
hundred thousand chained `not`s, values that double each time they are
rebound, patterns
that would hang a backtracking engine, four hundred random byte
strings, and every truncation of the real files — and, end to end, boot
a whole session against a configuration tree made of garbage and check
that the desk still comes up usable.

---

## Seeing what happened

One `info` line per read, and one `debug` line per thing skipped:

```
INFO  hyprland-config: read the desktop's live Hyprland configuration
      files=42 bindings=189 commands=121 env=8 autostart=4
      float_rules=43 monitors=1 skipped=136
DEBUG hyprland-config: not carried over kind=bind what="SUPER + G (Toggle window group)"
      why="requires window groups or a feature ChonkStep does not provide"
```

`skipped` being large is normal and not a problem — a stock Omarchy
machine has around 190 directives this desktop has its own answer for.
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
