# Hyprland compatibility for Omarchy

The Wayland session serves Hyprland's two IPC sockets so Omarchy's
unmodified Quickshell shell, `hyprctl`, and the desktop's scripts can
run on chonkstep. The implementation is split between
`crates/chonk-hyprland-ipc` (wire format) and
`crates/wm-wayland/src/hyprland_ipc.rs` (live state and mutations).

The X11 session does not serve these sockets. Omarchy parity targets
the native Wayland session; the X11 window manager remains a standalone
chonkstep session and is documented as such.

## The rule—and an important `hyprctl` trap

A request either changes the desktop as asked or returns an error. It
never returns `ok` for a guessed or discarded operation. End-to-end
tests assert the effect after the response, not merely that parsing
produced an action.

Do not mistake a textual refusal for a working shell fallback.
`hyprctl` 0.56.2 was measured against both chonkstep and a synthetic
socket: it exited zero for `ok`, `Invalid dispatcher: ...`, `unknown
request: ...`, `error`, and an empty response. Therefore this common
Omarchy shape does not enter its fallback branch:

```sh
hyprctl dispatch 'hl.dsp.focus({ window = "address:0x..." })' || \
  hyprctl dispatch focuswindow 'address:0x...'
```

The policy is consequently:

- implement every reachable, meaningful Omarchy operation;
- omit unsupported Hyprland script actions from chonkstep's mirrored menu;
- make inapplicable layout messages quiet no-ops;
- return `Invalid dispatcher: <reason>` for unavailable operations;
- log every refusal at warning level with a session-long counter.

That makes interactive failures readable and silent script failures
discoverable in `~/.local/state/chonkstep/wayland-session.log`. It does
not issue a notification for arbitrary IPC: callers often probe, and a
compositor-generated notification would turn probes into UI spam.

## Discovery and transport

The sockets are:

```text
$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket.sock
$XDG_RUNTIME_DIR/hypr/$HYPRLAND_INSTANCE_SIGNATURE/.socket2.sock
```

The instance directory is mode `0700` and both sockets are `0600`. The
event socket accepts peers with the compositor user's uid; the request
socket accepts those and root. Root is past the directory mode
regardless and can read or kill the process outright, so refusing it
protected nothing — it only answered a root-run `hyprctl` with silence,
and Omarchy's pacman hooks pause and resume the configuration watch
through exactly that (`sudo hyprctl reload` is another). Root gets the
same verbs this user gets and nothing more; every other uid is refused.
There is no `/tmp` fallback. Requests are capped at 64 KiB, all request readers together
share a 128 KiB budget per server pass, and all descriptors are
non-blocking. The server retains at most **64 one-shot request clients**
and **64 event subscribers**; an accepted connection beyond either cap
is closed immediately and the continuously-full population is logged
once. A request client which sends no byte for 256 service passes is
reaped. Event subscribers are deliberately exempt because their valid
protocol behavior is to connect, never write, and wait for events. A
client that stops reading is disconnected rather than allowed to stall
the compositor.

`scripts/wayland-session.sh` publishes the live signature and
`WAYLAND_DISPLAY` through a curated activation environment. Under uwsm
it calls `uwsm finalize HYPRLAND_INSTANCE_SIGNATURE`; the direct
session removes every value it published at logout.

The request grammar, measured from the real client, is:

```text
[flags]/command [arguments]       hyprctl
command [arguments]               Quickshell dispatch
```

There is no length prefix or newline. `j` requests JSON. Batch requests
start with `[[BATCH]]` and use `;` separators.

## Queries

| Request | Result |
| --- | --- |
| `status` | `configProvider="chonkstep"`; Quickshell uses classic dispatch while scripts may use the supported Lua forms |
| `monitors` | Live output name, geometry, scale, focus, active workspace, shown special workspace (`specialWorkspace`), transform, DPMS state, and measured VRR capability/runtime state. The optional `all` argument is accepted and ignored because connected outputs remain in the layout while powered down. |
| `workspaces` | One-based ids; real per-workspace monitor assignment, fullscreen state and `tiledLayout` (`freeform`, `dwindle`, `scrolling`); then each special workspace, with a negative id (`-99` downwards in creation order, stable for the session) and the name `special:NAME` |
| `clients` / `activewindow` | Live pid, class/title, position, size, workspace, monitor, XWayland, floating, pinned, fullscreen, tags, focus history and idle inhibition. `fullscreen` is the compositor's mode (2 fullscreen, 1 maximized, 0 neither) and `fullscreenClient` what the window is told (2 for real or tiled fullscreen, 1 maximized, 0 neither), so Omarchy's tiled-fullscreen toggle can read its own state back. `xdgTag` and `xdgDescription` are the window's `xdg_toplevel_tag_v1` tag and description as the client set them (each kept to 256 bytes, cut at a character boundary), empty for a window that set none — every XWayland window. A tag changed after the map is republished on the next pass |
| `activeworkspace` | Exactly the active workspace, in JSON or one plain block |
| `cursorpos` | The live pointer as plain `X, Y`, or `{"x": X, "y": Y}` with `-j` |
| `devices` | Seat keyboards and pointers; keyboards include `name`, `layout` (the installed layout list, such as `us,de`), `active_keymap` (the group in force now), and `active_layout_index`. Plain `devices` uses Hyprland's block format, with one `active keymap:` line per keyboard |
| `binds` | The live chonkstep keymap in Hyprland's plain bind-block format (or JSON). Every row replays through `dispatch`: an action with a Hyprland verb reports that verb, `exec` rows are shell-quoted so the command rebuilds exactly, and an action with no Hyprland verb reports `chonkstep <name>` |
| `getoption` | An explicitly unset `{ "option": ..., "set": false }` object for every option but two. Value fields are absent so JavaScript keeps its own default instead of coercing `null` or zero. `misc:disable_autoreload` (also `misc.disable_autoreload`, as Omarchy's reload guard spells it) answers `{ "int", "bool", "set": true }` with the live state of the configuration watch, and `debug:suppress_errors` is always set and `true`: chonkstep has no on-screen configuration-error surface, so there is never anything shown to suppress |
| `version`, `splash` | Supported |
| `systeminfo` | Version and source, the config path, `autoreload:` — `on`, or `paused` while `misc:disable_autoreload` holds the configuration watch — the current workspace and output count, then `shortcut_inhibitor:` — `active holder=APP` when a client holds every chord through `zwp_keyboard_shortcuts_inhibit_v1`, `suspended holder=APP` after the escape chord, `disabled` under `allow_shortcut_inhibit = false`, else `none` — and the graphics and output snapshot |
| `configerrors` | Retained live-Hyprland refusals, one per line or as JSON `{"error": "…"}` objects |

The nested backend has no libinput device records, so it reports one
logical keyboard and pointer. A hardware session reports its libinput
devices. This prevents Omarchy's keyboard widget from polling forever
without claiming nonexistent nested hardware.

`dpmsStatus` follows the connector's live power state. `dispatch dpms
on|off|toggle [OUTPUT]` uses the same hardware path as
`zwlr_output_power_manager_v1`; real input wakes powered-down outputs.
The nested backend refuses power control because its output is a host
window, not a connector.

Plain `clients` and `activewindow` use Hyprland's tab-indented field
blocks. In particular, the real pid lets `omarchy-cmd-terminal-cwd`
read `/proc/<pid>/cwd`, and `at`/`size` round-trip through Omarchy's
window-width and capture scripts.

Cursor and window positions and window sizes use logical layout coordinates,
matching xdg-output and Quickshell. Monitor origins are the advertised logical
origins; monitor width/height remain physical mode dimensions, before rotation.
Convert each output's local offsets with its own scale. For example, a pointer
at physical `(1511, 523)` on a 2× output at `(0, 0)` reports `(755, 261)`.
Classic and Lua move/resize dispatches accept those same logical units, including
relative deltas, so saved dimensions can be passed back unchanged. ChonkStep's
native control socket and internal rendering geometry remain in physical pixels.

## Mutations

Classic dispatch and Omarchy's Lua dispatch vocabulary reach the same
actions. Supported families include:

- workspace focus and moving a window to a workspace;
- special workspaces, Omarchy's scratchpad: `togglespecialworkspace [NAME]`
  (Lua `hl.dsp.workspace.toggle_special("NAME")`) shows the named special
  as an overlay on the active output, above pinned windows, or hides it
  when it is the one shown there; `workspace special:NAME` shows it;
  `movetoworkspacesilent special[:NAME][,window]` moves a window there
  hidden, and `movetoworkspace special[:NAME][,window]` moves it and
  shows the special (Lua `hl.dsp.window.move({ workspace =
  "special:NAME", follow = … })`). A bare `special` is the default
  special workspace. Names are at most 64 bytes and a session holds at
  most 16 special workspaces; a request that would create another is
  refused before anything changes, and `name:…` workspaces are refused
  by name;
- focus by selector or spatial direction, close, kill-active, cycle,
  fullscreen/maximize, and `fullscreenstate <internal> <client>` /
  `hl.dsp.window.fullscreen_state({ internal = …, client = … })`, each
  axis 0, 1 or 2 and refused by name otherwise. `0 2` tells the window
  it is fullscreen without moving it — Omarchy's tiled fullscreen — and
  asking for the state a window already has clears both axes, as in
  Hyprland. `hasfullscreen` and the `fullscreen` event follow only the
  compositor's own fullscreen;
- move, resize, center, raise, pin, tags, and floating membership;
- `layout freeform|mosaic|flow`, `togglelayout`, `togglefloating`, `setfloating`,
  `settiled`, and directional `movewindow`/`swapwindow`;
- `workspace +1|-1` and `movetoworkspace +1|-1` as relative steps by index,
  growing the row past its end like the keyboard's `workspace-next`;
  `workspace e+1|e-1` as Omarchy's "next existing workspace": only
  workspaces with windows on them plus the current one, wrapping, never
  creating a workspace; and `workspace previous`, the workspace before
  this one, refused until there has been a switch;
- `focusmonitor +N|-N|current|l|r|u|d|ID|NAME` and `hl.dsp.focus({ monitor
  = … })`: the pointer warps to the centre of that output's workarea
  through the same path as `movecursor`, the output is selected under
  separate Spaces, and the keyboard goes to its most recently focused
  window (or stays put when it has none). A window opened next lands on
  that output, which is how `omarchy-launch-screensaver` covers every
  display. A name no output carries is refused by name, and the whole
  verb is refused while the session is locked;
- `movecurrentworkspacetomonitor DIR|NAME` and `hl.dsp.workspace.move({
  monitor = … })`: under separate Spaces the active Space is re-homed to
  that display with its windows, keeping their position relative to the
  display. On the shared desktop the workspace already spans every
  display, so the request is refused with the setting that would change
  that; a fullscreen Space is refused because it is bound to its window's
  display. Never `ok` and left undone;
- `chonkstep <name>`, which runs a ChonkStep binding `binds` reported with that
  label, exactly as its key would. Only reported labels are accepted, and while
  the session is locked only a binding marked locked;
- `eval hl.workspace_rule({ workspace = "1", layout = "scrolling" })` and
  `keyword workspace 1, layout:scrolling` (also `dwindle` and `freeform`);
- `exec -- <argv...>` as direct argv and Lua `exec_cmd` as shell
  source, including `[[...]]` and `[=[...]=]` strings;
- `eval hl.dispatch(hl.dsp....)`;
- `eval hl.monitor({ output=..., mode=..., position=..., scale=... })` for a live
  output. `mode` is `preferred`, `highrr`, `highres` or an advertised
  `WxH@RATE`, and `position` is `auto` or `XxY`, each meaning what it means in a
  monitor rule. Any other key, `mirror` included, refuses the whole request,
  and a mode the output does not advertise is refused before anything changes.
  The mode is set first; if the connector refuses it, the reply is a refusal
  and the position and scale are left as they were;
- `eval hl.monitor({ output=..., disabled=BOOL })`, the line Omarchy's
  clamshell and laptop-display toggles write. `disabled = true` takes the
  output out of the desktop layout (see below); `disabled = false` puts it
  back. It is the whole request: a mode, position or scale beside it is
  refused rather than half applied, and disabling the last output in the
  layout is refused with a log line;
- `dispatch dpms on|off|toggle [OUTPUT]` for temporary connector power;
- `switchxkblayout DEVICE next|prev|INDEX`, which changes the live XKB
  group and emits `activelayout` with its human-readable name;
- `eval hl.config({ cursor = { invisible = BOOL } })`, the live
  cursor-visibility property used by Omarchy's screensaver;
- `eval hl.config({ misc = { disable_autoreload = BOOL } })` and
  `keyword misc:disable_autoreload BOOL`, which pause and resume the
  one-second watch over the desktop's Hyprland configuration — the switch
  Omarchy's pacman hooks throw around every `omarchy-settings` upgrade so
  a tree that is half replaced is never read (see
  [hyprland-config.md](hyprland-config.md#following-your-edits)). Paused,
  an edit waits; `reload` applies it regardless and re-baselines the
  watch, so a resume after that reload re-reads nothing more. `debug = {
  suppress_errors = BOOL }` may ride along in the same call or come
  alone: `true` is accepted as what is already the case, `false` is
  refused by name. A call with any other table or key, or a value that is
  not a boolean — Omarchy's resume writes back the word `null` after a
  failed read — is refused whole, so the guard never gets `ok` for a
  pause or a restore it did not get. The pause is session-local and not
  persisted; the transition is logged and `systeminfo` reports it;
- `eval hl.device({ name = "NAME", enabled = BOOL })`, the request
  Omarchy's touchpad and touchscreen toggles send. `NAME` is a quoted Lua
  string, escapes included, and must be exactly the name of a pointer,
  touch or tablet device that `devices` lists. A name that any keyboard
  carries is never disabled, and the nested backend's logical
  `chonkstep-keyboard` and `chonkstep-pointer` are refused by name. A
  disabled device stops sending events before the reply is written: it no
  longer moves the pointer or counts as activity, a button or touch it
  held is released, and it stays off across hotplug and resume until it
  is enabled again or the configuration changes its own rule for that
  device;
- `eval hl.dispatch(hl.dsp.cursor.move({ x = X, y = Y }))` and
  `dispatch movecursor X Y`, which warp the pointer to a logical layout point
  as `cursorpos` reports it (Omarchy's screenshot picker moves its highlight
  this way). A warp is refused while the session is locked or a client holds
  a pointer constraint;
- `reload`, which re-reads chonkstep/Hyprland configuration and emits
  `configreloaded` only after it has applied.

`hl.config`, `hl.device`, and `hl.workspace_rule` are recognized and
refused by name when their requested property is not modeled (for
`hl.device`, anything besides `enabled`, which belongs in the configuration). They are
not reported as unknown syntax. Monitor scaling validates the output
and range before changing anything, so an Omarchy script cannot record
a scale that the compositor said it applied but did not.

`keyword` supports workspace layouts, the named
`keyword cursor:invisible BOOL` screensaver fallback, which reaches the
same live cursor flag as `hl.config`, `keyword misc:disable_autoreload
BOOL`, which reaches the same watch switch, and the two `keyword monitor`
forms Omarchy's Display panel sends from its row toggle:
`keyword monitor NAME,disable` and `keyword monitor
NAME,MODE,POSITION,SCALE`. If the focused client that hid the cursor
disconnects without restoring it, chonkstep restores the cursor
automatically. Every other refusal names what does work instead:
chonkstep re-reads `~/.config/hypr` within a second of an edit, and
`hyprctl eval hl.monitor({ ... })` changes a live output.

`NAME,disable` takes a connected output out of the desktop layout. The
output is *parked*: its `wl_output` global is withdrawn, its workspaces
and windows move to the remaining outputs, the pointer can no longer
reach it, and on the DRM session its crtc is cleared the way DPMS-off
clears it while the connector is kept. `NAME,preferred,auto,auto` (any
mode, position and scale) puts a parked output back at the end of the
layout, where the configuration's own rule for it applies; on an output
already in the layout the same form means what the same monitor line
means in the configuration, and anything past the scale is refused as
belonging there. Disabling the last output in the layout is refused,
with a log line: the desktop is never without an output. Powering a
connector down remains `dispatch dpms off OUTPUT`, which keeps the head
in the layout.

The IPC is honest about which outputs are in the layout. Plain
`monitors` lists the layout alone, so a bar never draws a workspace row
for a panel inside a closed lid; `monitors all` lists the parked outputs
after it with `disabled: true` and `dpmsStatus: false` (and `id: -1`,
since ids are layout positions), which is where Omarchy's monitor
scripts look for the panel they disabled. `monitorremoved` and
`monitoradded` fire on disable and enable, and name the output that
moved: the diff goes by name, because disabling the first output hands
its id to the second.

The monitor object reports measured values, not conventional ones.
`refreshRate` is the driven mode's rate, `availableModes` is the
connector's mode list in `WIDTHxHEIGHT@RATEHz` with the current mode
first, and `make`/`model`/`serial` are read from the connector EDID. The
same `make model serial` description backs `monitor = desc:…`,
`wl_output`, IPC, and `zwlr_output_management`, so those interfaces
cannot describe one panel two ways. `serial` stays empty only when the
EDID itself supplies none. `vrr` is true only while a capable output is
actually using adaptive sync; wlr-output-management controls policy,
and runtime activation is restricted to direct scanout. Set
`CHONKSTEP_NO_VRR=1` to force it off. A backend driving no real mode reports 60 Hz rather than 0,
because a bar divides this into a frame budget.

Omarchy's `dwindle` and `scrolling` select Mosaic and Flow. `Super+L` toggles
between them; `Super+Shift+L` restores Freeform. `layoutmsg`, `togglesplit`,
`swapsplit`, `pseudo` and `splitratio` are deliberate quiet no-ops. Groups
and unsupported workspace options remain explicit refusals.
The mirrored menu retains its filter for direct Hyprland script rows; native
shortcuts show a transient compositor caption without launching a script.

## Event stream and workspace lifetime

The event socket emits state diffs, plus the explicit post-reload
event:

`configreloaded`, `monitoradded`, `monitoraddedv2`, `monitorremoved`,
`createworkspacev2`, `destroyworkspacev2`, `workspacev2`, `workspace`,
`moveworkspacev2`, `focusedmon`, `fullscreen`, `openwindow`,
`closewindow`, `movewindowv2`, `windowtitlev2`, `windowtitle`,
`activewindowv2`, `activewindow`, `activespecial`, `activespecialv2`,
`urgent`, `changefloatingmode`, and `activelayout`.

Addresses are `0x...` in JSON and bare hexadecimal in events; both are
the same `ClientId`. Workspace ids are one-based on this wire and
converted exactly once at its boundary. Special workspaces carry
negative ids, `-99` downwards in creation order, resolved by their own
function rather than the numbered one, and a member window's
`workspace` is its special. `activespecial>>NAME,MONITOR` and
`activespecialv2>>ID,NAME,MONITOR` announce a special shown on an
output, with an empty name (and id) when it is hidden again.

Chonkstep workspaces are persistent by design. Visiting workspace 9
creates the intervening row and empty workspaces do not disappear, so
Omarchy's bar may keep pills 1–9. This is intentional state reporting,
not a fabricated Hyprland lifecycle; destroying the user's workspace
objects just to shorten another shell's bar would change chonkstep's
model.

## Hyprland-namespaced Wayland protocols

Three protocol globals remove the remaining shell/tool warnings:

- `hyprland_focus_grab_manager_v1` v1: Quickshell popup focus grabs;
- `hyprland_toplevel_mapping_manager_v1` v1: maps a live
  `zwlr_foreign_toplevel_handle_v1` to the exact IPC address; stale
  handles fail and are cleaned on unmap;
- `hyprland_ctm_control_manager_v1` v2: the real `hyprsunset` path.
  See [night-light.md](night-light.md).

No patched Quickshell or Omarchy command is installed.

## Standard Wayland globals

Ordinary applications also receive the Smithay-backed protocol set:

| Capability | Global/version |
| --- | --- |
| application activation | `xdg_activation_v1` v1 |
| cursor shapes / solid-color buffers | `wp_cursor_shape_manager_v1` v2, `wp_single_pixel_buffer_manager_v1` v1 |
| presentation timing | `wp_presentation` v2 |
| explicit client pacing | `wp_fifo_manager_v1` v1, `wp_commit_timing_manager_v1` v1 |
| sandbox context tagging | `wp_security_context_manager_v1` v1 |
| physical output power | `zwlr_output_power_manager_v1` v1 (hardware sessions only) |
| relative/locked/confined pointer | `zwp_relative_pointer_manager_v1` v1, `zwp_pointer_constraints_v1` v1 |
| gestures / tablets | `zwp_pointer_gestures_v1` v3, `zwp_tablet_manager_v2` v1 |
| foreign surface parenting | `zxdg_exporter_v2` and `zxdg_importer_v2` v1 |
| shortcut inhibition | `zwp_keyboard_shortcuts_inhibit_manager_v1` v1 |
| IME | `zwp_text_input_manager_v3` v1, `zwp_input_method_manager_v2` v1 |
| modern xdg helpers | `xdg_wm_dialog_v1`, `xdg_system_bell_v1`, `xdg_toplevel_tag_manager_v1` v1 |

The IME popup participates in rendering and hit testing, activation
focuses the target, pointer constraints follow focus and lifetime, and
presentation feedback comes from winit presentation or DRM vblank.
FIFO barriers release at that same presentation boundary; commit
timestamps arm the event loop's monotonic-clock deadline, and invisible
surfaces cannot remain wedged behind an unpresentable barrier. Security
contexts tag admitted clients. A shared policy hides privileged globals from
those clients, including capture, input injection, clipboard monitoring,
layer surfaces and output/session management. Ordinary desktop helpers retain
access; confined applications keep normal window, input and focused clipboard
protocols. This boundary requires the sandbox launcher to use a security-context
listener and prevent access to the unrestricted display socket.
Tablet proximity, tip, buttons, pressure, distance, tilt, rotation,
slider and wheel are forwarded from libinput.

Measured deltas from Hyprland's current registry are documented rather
than hidden: chonkstep advertises `xdg_wm_base` v6 (Hyprland v7),
`zxdg_decoration_manager_v1` v1 (Hyprland v2), and
`zwlr_layer_shell_v1` v4 (Hyprland v5). No caller in the compatibility
suite requires the newer requests. The registry test binds every
advertised global with the real `wayland-info` client so a dispatch
omission is a test failure, not just a name in a table.

## Testing and disabling

The pure protocol suite checks every schema, selector, parser, event,
and refusal. Ignored end-to-end tests boot the real compositor under a
private Xvfb and exercise sockets with real windows, `hyprsunset`,
generated Wayland clients, and `wayland-info`.

Hyprland IPC is enabled by default. Set
`CHONKSTEP_HYPRLAND_IPC=0` (`false`, `no`, or empty also work) before
starting the compositor to disable it. Socket creation failure is
logged but never prevents the desktop from starting.
