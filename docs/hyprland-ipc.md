# Speaking Hyprland's IPC

chonkstep aims to be a drop-in replacement for Hyprland underneath
Omarchy. Omarchy's shell and tooling ask the compositor questions, and
today every one of those questions takes a wrong branch under
chonkstep. Rather than patch Omarchy one script at a time — which does
not scale, and asks upstream to carry our differences forever — this
module answers those questions in Hyprland's own protocol, so that
Omarchy's *unmodified* shell and the real `hyprctl` binary work against
chonkstep.

This document is the inventory that scoped the work, what is served,
what deliberately fails, and how to turn the whole thing off.

## 0. The load-bearing design rule

**A dispatcher we cannot honour must fail, loudly, rather than succeed
plausibly.**

This is the rule the rest of the module is built around, and it is
worth stating before anything else because it is counter-intuitive.
chonkstep is not a tiling compositor. Hyprland's IPC has verbs that
only mean something to a tiling layout — `layoutmsg`, `swapwindow`,
`togglesplit`, `pseudo`. It would be easy to return `ok` to those and
move on.

That is the worst thing we could do. A script that receives a confident
wrong answer takes an unexpected branch and carries the mistake
forward, silently, into behaviour the user sees. A script that receives
a clean error takes its error branch, which its author wrote and
tested, and usually falls back to something that works. An honest
failure is strictly more useful than a plausible success, so every verb
in this module either does the thing or says it did not.

## 1. The inventory — what Omarchy actually calls

Scoped from real usage on this machine against **Omarchy 4.0.0.alpha**
(`/usr/share/omarchy/version`), not from Hyprland's documentation.
Implementing what is used beats implementing what is documented, and in
this case the difference turned out to be very large.

### 1.1 Two clients, not one

Omarchy reaches the compositor by two independent routes, and they have
different requirements. Both had to be inventoried.

**Route A — 53 of 431 shell scripts spawn `hyprctl`.**

```
$ ls /usr/bin/omarchy-* | wc -l
431
$ grep -l hyprctl /usr/bin/omarchy-* | wc -l
53
```

A caution for anyone re-running this: the 427 entries in
`/usr/share/omarchy/bin/` are **symlinks** into `/usr/bin/`, and
`grep -r` does not follow symlinks. Grepping the share directory
recursively reports zero callers and is simply wrong; grep
`/usr/bin/omarchy-*` directly.

Twenty-four of the 53 are `omarchy-hyprland-*`, which are Hyprland-only
by name and by intent and are already excluded from chonkstep's
mirrored menu (see `docs/omarchy-integration.md`). The remaining 29 are
ordinary desktop tooling that happens to ask the compositor a question,
and they are the ones worth unblocking.

Weighted by call site across all 53:

| Subcommand | Call sites | Notable callers |
| --- | --- | --- |
| `dispatch` | 40 | `launch-or-focus`, `launch-signal`, `launch-spotify`, `restart-shell`, `screensaver`, `brightness-display` |
| `monitors -j` / `monitors all -j` | 26 | `bar-text-color`, `capture-region`, `capture-screenrecording`, `launch-shell`, `monitor-state`, `menu-keybindings` |
| `clients -j` | 13 | `launch-or-focus`, `capture-region`, `debug-idle`, `launch-about` |
| `reload` | 12 | `install-preinstalls`, `remove-preinstalls`, `theme-set`, `restart-hyprctl` |
| `eval` | 9 | `capture-screenshot`, `toggle-input-device`, `capture-region` (Lua evaluation — Hyprland 4 only) |
| `activewindow` | 6 | `cmd-terminal-cwd` |
| `keyword` | 4 | `capture-screenshot`, monitor panel |
| `devices -j` | 3 | `hw-touchpad`, `hw-touchscreen` |
| `cursorpos` | 2 | `capture-region` |
| `activeworkspace` | 2 | |
| `binds` | 2 | `menu-keybindings` (plain text, explicitly *not* `-j`) |
| `getoption -j` | 1 | `capture-screenshot` |
| `switchxkblayout` | 1 | `system-lock` |
| `hyprsunset` | 2 | `toggle-nightlight` — **not our socket** |

So `monitors -j` and `clients -j` between them unblock more distinct
scripts than everything else combined, and they are the first thing to
serve.

**Route B — the Quickshell bar talks the sockets directly.**

`/usr/share/omarchy/shell/` never spawns `hyprctl` for its live state.
It imports `Quickshell.Hyprland`, whose IPC client connects to both
sockets itself. This is the route that makes the bar *live*, and it is
the demonstration target. Its exact behaviour is knowable rather than
guessable, because Quickshell's source is installed at
`/usr/src/debug/quickshell-git/quickshell/src/wayland/hyprland/ipc/`.
That source is this module's specification.

Omarchy 4 also moved compositor *config* into Lua
(`configProvider = "lua"`), so dispatch is written `hl.dsp.focus({
workspace = "3" })` rather than `dispatch workspace 3`. Several scripts
send the Lua form first and fall back to the classic form — for
example `omarchy-launch-or-focus`:

```sh
hyprctl dispatch "hl.dsp.focus({ window = \"address:$WINDOW_ADDRESS\" })" \
  || hyprctl dispatch focuswindow "address:$WINDOW_ADDRESS"
```

That fallback is a gift. A server that **rejects** the Lua form cleanly
gets the classic form on the next line for free, which is the whole
argument of section 0 playing out in Omarchy's own source.

### 1.1a The jq filters, which are the real field spec

Omarchy's `jq` expressions name exactly which keys are load-bearing.
Counted across all 53 scripts:

`monitors -j` — `.focused` (13), `.name` (12), `.height` (7),
`.width` (6), `.scale` (6), `.disabled` (4), `.activeWorkspace.id` (2),
`.id`, `.make`, `.model`, `.dpmsStatus`, `.x`, `.y`.

`clients -j` — `.class` (8), `.title` (7), `.address` (6), `.size` (5),
`.initialClass` (3), `.tags` (2), `.pid` (2), `.inhibitingIdle` (2),
`.at` (2), `.workspace.id`, `.hidden`, `.focusHistoryID`.

`.at` and `.size` are two-element **arrays** (`.at[0]`, `.size[1]`), not
objects — `omarchy-capture-region` formats window rectangles as
`"\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"`. Getting that wrong
produces `null,null nullxnull` rather than an error, which is precisely
the silent-wrong-answer failure mode section 0 exists to prevent.

`cursorpos` is **plain text**, not JSON, and the caller splits on
`", "` — a comma *and a space*.

### 1.2 What Quickshell's `Hyprland` singleton requests

From `ipc/connection.cpp`. Five requests, and that is the entire set:

| Request | When | Consumed by |
| --- | --- | --- |
| `j/status` | once, at startup, before anything else | reads `configProvider`; decides whether Hyprland is in Lua mode |
| `j/monitors` | startup, `configreloaded`, `monitoraddedv2`, `monitorremoved`, and when a workspace loses its monitor | `Hyprland.monitors`, `Hyprland.focusedMonitor` |
| `j/workspaces` | startup, `configreloaded`, `createworkspacev2`, `fullscreen` | `Hyprland.workspaces`, `Hyprland.focusedWorkspace` |
| `j/clients` | startup, `configreloaded` | populates each workspace's `toplevels` list |
| `dispatch <args>` | on `Hyprland.dispatch(...)` | — |

Note `j/status` is requested **first and blocking** — the event socket
is not even connected until its callback runs. A compositor that does
not answer `j/status` never gets Quickshell as far as connecting to the
event stream. This is the single highest-priority request in the whole
inventory.

### 1.3 The JSON fields Quickshell actually reads

This matters more than matching Hyprland's full output: these are the
only keys that are load-bearing. Everything else in a real Hyprland
response is decoration (though it is preserved verbatim in QML as
`lastIpcObject`, so extra fields are harmless and missing ones are not).

**`j/status`** → object
- `configProvider` — string; `"lua"` puts Quickshell in Lua-dispatch mode.

**`j/monitors`** → array of objects (`ipc/monitor.cpp`)
- `id` (int), `name` (string), `description` (string)
- `x`, `y`, `width`, `height` (int), `scale` (real)
- `focused` (bool) — **the only way the focused monitor is identified**
- `activeWorkspace` — nested object with `id` (int) and `name` (string)

**`j/workspaces`** → array of objects (`ipc/workspace.cpp`)
- `id` (int), `name` (string)
- `monitorID` (int), `monitor` (string, monitor *name*)
- `hasfullscreen` (bool)

**`j/clients`** → array of objects (`ipc/hyprland_toplevel.cpp`)
- `address` (string, **hex, parsed with base 16**; an entry whose
  address does not parse as hex is skipped entirely)
- `title` (string)
- `workspace` — nested object, matched by its `name`

### 1.4 The events Quickshell handles

From `HyprlandIpc::onEvent`. Line format is `EVENT>>DATA`, with
comma-separated fields in `DATA`. Anything not in this list is ignored
by Quickshell (but still delivered to QML `onRawEvent` handlers, which
Omarchy's idle service and keyboard-layout widget both use).

| Event | Payload | Effect |
| --- | --- | --- |
| `configreloaded` | — | re-queries monitors, workspaces, clients |
| `monitoraddedv2` | `id,name,description` | adds monitor, re-queries monitors |
| `monitorremoved` | `name` | removes monitor |
| `createworkspacev2` | `id,name` | adds workspace |
| `destroyworkspacev2` | `id,name` | removes workspace |
| `workspacev2` | `id,name` | sets focused monitor's active workspace |
| `moveworkspacev2` | `id,name,monitorName` | reassigns workspace to monitor |
| `renameworkspace` | `id,name` | renames |
| `focusedmon` | `name,workspaceName` | sets focused monitor (`?` means "no workspace") |
| `fullscreen` | `0`/`1` | sets `hasFullscreen`, re-queries workspaces |
| `openwindow` | `address,workspaceName,class,title` | creates toplevel on a workspace |
| `closewindow` | `address` | destroys toplevel |
| `movewindowv2` | `address,workspaceId,workspaceName` | moves toplevel between workspaces |
| `windowtitlev2` | `address,title` | retitles |
| `activewindowv2` | `address` | sets active toplevel |
| `urgent` | `address` | sets urgent flag |

Addresses in event payloads are bare hex **without** a `0x` prefix
(`toULongLong(&ok, 16)`), while addresses in `j/clients` JSON are the
same hex string. Both must agree, because Quickshell matches event
payloads against JSON entries by numeric address.

Omarchy additionally watches raw events by name:
- `plugins/bar/widgets/KeyboardLayout.qml` refreshes on any event whose
  name contains `activelayout`, and on `configreloaded`.
- `plugins/services/idle/Service.qml` binds `Hyprland.onRawEvent` and
  cancels its idle cycle on window events.

### 1.5 What Omarchy's shell reads, widget by widget

The bar is the demonstration target, so this is what actually has to
light up:

| File | Needs |
| --- | --- |
| `plugins/bar/widgets/Workspaces.qml` | `Hyprland.workspaces` (`.id`, `.toplevels.values.length`), `Hyprland.focusedWorkspace.id`; dispatches `hl.dsp.focus({ workspace = "N" })` |
| `plugins/bar/Bar.qml` | `Hyprland.focusedMonitor.name` — routes keyboard-summoned panels to the focused output |
| `Ui/PopupCard.qml` | `HyprlandFocusGrab` — the `hyprland_focus_grab_v1` **Wayland protocol**, not IPC |
| `plugins/services/idle/Service.qml` | raw event stream |
| `plugins/bar/widgets/KeyboardLayout.qml` | `hyprctl -j devices` → `.keyboards[].active_keymap`/`.name`/`.main`; `hyprctl switchxkblayout <kbd> next` |
| `Commons/Style.qml` | `hyprctl -j getoption decoration:rounding` → `.int`; `hyprctl -j getoption general:gaps_out` → `.css` (falls back to `.int`) |
| `plugins/panels/monitor/Panel.qml` | `hyprctl keyword monitor <name>,disable\|preferred,auto,auto` |
| `plugins/services/nightlight/Service.qml` | `hyprctl hyprsunset temperature` — **not our socket**; `hyprctl` routes `hyprsunset` to hyprsunset's own socket, so this needs no work here |

Crucially, `plugins/bar/widgets/ActiveWindow.qml` — the window-title
widget — uses `ToplevelManager.activeToplevel`, which is Quickshell's
**wlr-foreign-toplevel** client, *not* Hyprland IPC. chonkstep already
serves that protocol (`crates/chonk-toplevel`), so the window title in
the bar is expected to work without any of this module. The same is
true of `services/AppLibrary.qml`.

### 1.6 The wire format, measured rather than guessed

Determined by standing up a Unix socket at the path `hyprctl` looks for
and recording the exact bytes the real binary writes. For each listed
`hyprctl` invocation, the bytes on the wire were:

```
hyprctl clients                        ->  /clients
hyprctl clients -j                     ->  j/clients
hyprctl -j clients                     ->  j/clients
hyprctl monitors                       ->  /monitors
hyprctl monitors all                   ->  /monitors all
hyprctl activewindow                   ->  /activewindow
hyprctl workspaces                     ->  /workspaces
hyprctl activeworkspace                ->  /activeworkspace
hyprctl version                        ->  /version
hyprctl devices -j                     ->  j/devices
hyprctl -j getoption decoration:rounding -> j/getoption decoration:rounding
hyprctl dispatch workspace 3           ->  /dispatch workspace 3
hyprctl keyword monitor eDP-1,disable  ->  /keyword monitor eDP-1,disable
hyprctl switchxkblayout kbd next       ->  /switchxkblayout kbd next
hyprctl reload                         ->  /reload
hyprctl configerrors                   ->  /configerrors
```

So the grammar is:

```
request := [flags] "/" command [" " args]     (hyprctl)
         | command [" " args]                  (Quickshell)
```

- The flag block precedes a literal `/`. `j` means "answer in JSON".
  `-j` may be given before or after the subcommand; `hyprctl`
  normalises both to the same `j/` prefix.
- **Quickshell omits the `/` entirely** for dispatch — it writes
  `dispatch focuswindow address:0x...` with no leading slash — while
  using `j/clients` for queries. A conforming server must accept a
  request with no `/` at all and treat it as flagless.
- There is **no trailing newline** and no length header. The request is
  the entire payload of a short-lived connection; the client then reads
  until EOF. One connection, one request, one response, close.
- `hyprctl --batch` prefixes `[[BATCH]]` and separates commands with
  `;` (the literal string is present in the binary).
- `hyprctl hyprsunset ...` writes **nothing** to this socket — it is
  routed to hyprsunset's own socket.

### 1.7 Socket location and discovery

Both clients agree (`connection.cpp`, and `hyprctl`'s probe above):

```
$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock   # requests
$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock  # events
```

Quickshell falls back to `/tmp/hypr/$HYPRLAND_INSTANCE_SIGNATURE` if the
`XDG_RUNTIME_DIR` path is not a directory, and gives up if neither is.
Real Hyprland's signature has the shape
`<hash>_<unixtime>_<random>`; nothing in either client parses it, so
its content is free — but it must be **exported into the session**, the
way `WAYLAND_DISPLAY` already is, or no client can find us.

## 2. What is served

The normative implementation is `crates/chonk-hyprland-ipc`; where this
document and that crate disagree, the crate is right and this document
has a bug worth reporting. The compositor-side half — reading live state
and applying actions — is `crates/wm-wayland/src/hyprland_ipc.rs`.

### 2.1 Read queries

| Request | Plain | `-j` | Notes |
| --- | --- | --- | --- |
| `status` | — | yes | `configProvider` is `"chonkstep"`, deliberately **not** `"lua"` |
| `monitors` | yes | yes | one entry; chonkstep has a single global workspace row |
| `workspaces` | yes | yes | 1-based on the wire |
| `clients` | yes | yes | `at`/`size` are two-element arrays |
| `activewindow` | yes | yes | `{}` when nothing is focused, never `null` |
| `activeworkspace` | yes | yes | |
| `version` | yes | yes | reports chonkstep's version, not a Hyprland one |
| `cursorpos` | yes | n/a | plain text `X, Y` — comma *and space* |
| `devices` | — | yes | empty arrays, shape preserved |
| `binds`, `configerrors` | yes | yes | empty |

### 2.2 Events

`configreloaded`, `monitoraddedv2`, `monitorremoved`, `createworkspacev2`,
`destroyworkspacev2`, `workspacev2`, `workspace`, `moveworkspacev2`,
`focusedmon`, `fullscreen`, `openwindow`, `closewindow`, `movewindowv2`,
`windowtitlev2`, `windowtitle`, `activewindowv2`, `activewindow`,
`urgent`.

They are **derived by diffing successive snapshots**, not emitted from
call sites. That is the choice that keeps them honest: a future change
to `wm-core` that introduces a new way for a window to move or be
retitled produces the right event without anyone remembering to add an
emit, because the event is a function of the state rather than a
side-effect someone has to wire up.

### 2.3 Dispatch

Honoured: `workspace`, `movetoworkspace`, `focuswindow`, `closewindow`,
`killactive`, `exec`, `fullscreen`, `cyclenext` — in both the classic
dialect and the Lua one (`hl.dsp.focus({ workspace = "3" })`) that
Omarchy 4 sends first.

Window selectors: `address:`, `pid:`, `class:`, `title:`,
`initialclass:`, `initialtitle:`, and bare `activewindow`. Hyprland's
anchored regexes (`class:^(foot)$`) are matched as a case-insensitive
substring after stripping the anchors — a deliberate *narrowing*, so a
selector we cannot interpret matches nothing and says so, rather than
matching the wrong window.

## 3. What fails, and why that is the feature

Refusals answer `Invalid dispatcher: <reason>`, which is Hyprland's own
error prefix and therefore the one callers already branch on.

**Tiling-only** — chonkstep is a floating window manager, so these have
no meaning here rather than merely no implementation: `layoutmsg`,
`togglesplit`, `swapsplit`, `swapwindow`, `swapnext`, `pseudo`,
`togglegroup` and the rest of the group verbs, `togglespecialworkspace`,
`workspaceopt`, `movetoworkspacesilent`.

**Not modelled** — `pin`, `togglefloating`, `setfloating`, `settiled`.
Every chonkstep window already floats, so there is nothing to toggle;
`floating: true` in `j/clients` is the truth, not a stub.

**Deliberately declined** — `keyword` and `reload` would claim to have
changed a Hyprland config chonkstep does not read; `getoption` answers
the documented "unset" shape rather than inventing a corner radius,
which leaves Omarchy's `Style.qml` on its previous value (its `catch`
branch) instead of restyling the bar to match a compositor the user is
not running.

**Out of range is an error, never a clamp.** `dispatch workspace 500`
fails; it does not switch to workspace 99. A clamp is a wrong answer
wearing a success.

The rule is enforced by a test rather than by good intentions:
`an_ok_answer_always_comes_with_an_action` asserts that a response of
`ok` and a produced action imply each other. That test exists because
the first end-to-end run against the real `hyprctl` found this crate
committing exactly the sin it was written to prevent — `dispatch
workspace 3` answered `ok` and did nothing, because feasibility was
checked when the action was applied and the answer had been written
earlier. The response and the action must be decided together, from one
snapshot.

### 3.1 The one place the off-by-one lives

Hyprland's workspaces are 1-based; `wm-core`'s are 0-based. The
conversion happens in exactly two functions —
`state::Workspace::hypr_id` on the way out and
`state::workspace_index_from_hypr_id` on the way in — and nowhere else,
so there is one place to look when someone reports an off-by-one.
Range is checked once, at the dispatch boundary, against
`MAX_WORKSPACE` (99, mirroring `wm_config::MAX_WORKSPACE`).

Naming a workspace past the end **creates** it, and the gap before it,
because `WindowManager::switch_workspace` grows the row on demand —
which is also what Hyprland does.

## 4. Security posture

**This socket accepts commands and is not authenticated**, which is the
same choice `docs/control-socket.md` §1.2 makes and rests on the same
argument: everything it offers — switch workspace, focus a window, close
one, run a command — *the user's own keyboard already offers*. A token
would withhold no capability from anything that can reach the socket,
because reaching it already means running as this user in this session;
what a token would reliably do is stop the real `hyprctl` from working,
which is the whole point of the exercise.

Access control is positional and layered three deep:

1. `$XDG_RUNTIME_DIR/hypr/<signature>/` is created **0700**, explicitly
   rather than by umask. Real Hyprland leaves it 0755; we decline to.
   There is **no `/tmp` fallback**, though Quickshell will look there —
   a command-accepting socket does not belong in a world-writable
   directory where any local process can win a create race for the name.
2. Each socket is `chmod` 0600 after `bind`, because `bind` applies the
   umask and the umask is not ours to trust.
3. `SO_PEERCRED` on accept, which restates rather than enforces: a
   socket that answers only to its own user should check, not assume.

The compositor never blocks on a client. Every fd is non-blocking from
`socket(2)` and `accept4(2)` rather than a later `fcntl`; reads are
budgeted per pass; a client whose unsent backlog passes 256 KiB has
stopped reading and is disconnected rather than waited for. This is the
discipline the workspace `clippy.toml` exists to protect — that file
records a wifi tile freezing the desktop for 3.6 seconds at a time and
the failure being reported as a display-driver stall.

## 5. Turning it off

**It is on by default.** Set `CHONKSTEP_HYPRLAND_IPC=0` (or `false`,
`no`, or empty) in the session to decline it; anything else, including
an unset variable, answers.

It began the other way, opted into, on the argument that impersonating
another compositor is a larger claim than chonkstep's own control socket
makes — it changes how unrelated software on the machine behaves, and a
user is entitled to decline it. That argument was right about the claim
and wrong about who makes it. Answering this IPC is not a side feature
of this desktop; it is most of what makes it usable as a drop-in under
Omarchy, where fifty-three scripts and the bar itself depend on it. A
default that has to be discovered in a document is a default that is
wrong on every machine nobody read the document on.

So the claim is still declinable, and the reasons to decline it are
still these: it changes how unrelated software behaves, and a machine
running both chonkstep and Hyprland at different times may prefer that
only one of them ever answers. One variable does it.

A value nobody anticipated means yes, deliberately: the failure mode of
a typo should be the feature working, not a silently inert server whose
absence looks like a bug in Omarchy's tooling. A bind failure is a
warning and an inert server, never a failed login.

When on, the compositor exports `HYPRLAND_INSTANCE_SIGNATURE` into its
own children before `Shell::new` autostarts anything, and
`scripts/wayland-session.sh` republishes it into the systemd and D-Bus
activation environment beside `WAYLAND_DISPLAY` — D-Bus-activated
services inherit nothing from the compositor, and both clients find the
sockets through that variable and nothing else.

## 6. Evidence

Run against a nested chonkstep on a private `Xvfb`, with the real
`hyprctl` 0.56.2 and Omarchy 4.0.0.alpha's **byte-identical** Quickshell
shell — run straight from its installed path,
`quickshell -p /usr/share/omarchy/shell`, with nothing patched, shimmed
or copied:

- `hyprctl monitors -j`, `workspaces -j`, `clients -j`, `activewindow -j`
  all return correct JSON, and Omarchy's own `jq` filters run against it
  — `launch-or-focus`'s address lookup, `capture-region`'s
  `.activeWorkspace.id` and `"\(.at[0]),\(.at[1]) \(.size[0])x\(.size[1])"`
  rectangle, `bar-text-color`'s focused-monitor name.
- `hyprctl dispatch workspace 3` switches, and the workspace row grows
  to `[1,2,3]`. `workspace 500` errors without clamping. `togglesplit`
  and five other tiling verbs fail with `Invalid dispatcher`.
- Quickshell made all four of its requests (`j/status`, `j/monitors`,
  `j/workspaces`, `j/clients`), parsed each, and built a toplevel from a
  real window's address.
- With windows opened and workspaces switched underneath it, Quickshell
  logged receiving and parsing our events and *acting* on them. The
  decisive lines, from its own `quickshell.hyprland.ipc` category:

  ```
  Received event: "openwindow>>100000004,2,foot,EVENT-PROOF"
  New toplevel created with address 4294967300 , title "EVENT-PROOF" , workspace "2"
  Received event: "createworkspacev2>>7,7"
  Workspace created with id 7 name "7"
  Received event: "workspacev2>>7,7"
  Workspace 7 activated on "chonkstep"
  ```

  That last line is the bar's workspace indicator following a real
  chonkstep workspace change, which is what this whole document is for.
  Note also `Received event: "activewindowv2>>"` with an empty payload,
  handled without complaint: a bare workspace switch in chonkstep leaves
  nothing focused, and the stream says so rather than inventing a window.

### 6.1 What the evidence does not cover

- **`hyprland-toplevel-mapping-v1`.** Quickshell warns that it cannot
  derive Hyprland addresses from `wl_toplevel`s. That is a Wayland
  protocol, not IPC, and it is out of this crate's scope; nothing in
  Omarchy's bar needed it in the runs above.
- **A long-lived bar under load.** The nested sessions above are
  headless and get no input, so Omarchy's idle service reaches its lock
  branch within about thirteen seconds and takes the shell down with it.
  Every run here therefore covers the first few seconds of a bar's life.
  Nothing observed suggests a problem beyond that, but nothing here
  proves one is absent either.
