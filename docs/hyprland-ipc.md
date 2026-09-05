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
- omit known tiling-only actions from chonkstep's mirrored menu;
- return `Invalid dispatcher: <reason>` for operations with no honest
  floating-desktop meaning;
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

The instance directory is mode `0700`; both sockets are `0600`, and
accepted peers must have the compositor user's uid. There is no `/tmp`
fallback. Requests are capped at 64 KiB, all request readers together
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
| `monitors` | Live output name, geometry, scale, focus, active workspace, transform and powered-on `dpmsStatus` |
| `workspaces` | One-based ids; real per-workspace monitor assignment and fullscreen state |
| `clients` / `activewindow` | Live pid, class/title, position, size, workspace, monitor, XWayland, floating, pinned, fullscreen, tags, focus history and idle inhibition |
| `activeworkspace` | Exactly the active workspace, in JSON or one plain block |
| `cursorpos` | The live pointer as plain `X, Y` |
| `devices` | Seat keyboards and pointers; keyboards include `name`, `active_keymap`, `layout`, and `active_layout_index` |
| `binds` | The live chonkstep keymap in Hyprland's plain bind-block format (or JSON) |
| `getoption` | A complete, explicitly unset option shape; no fabricated Hyprland style value |
| `version`, `splash` | Supported |
| `configerrors` | Retained live-Hyprland refusals, one per line or as JSON `{"error": "…"}` objects |

The nested backend has no libinput device records, so it reports one
logical keyboard and pointer. A hardware session reports its libinput
devices. This prevents Omarchy's keyboard widget from polling forever
without claiming nonexistent nested hardware.

`dpmsStatus` is `true` because chonkstep currently keeps advertised
outputs powered. `dispatch dpms` is refused; status and mutation cannot
contradict one another.

Plain `clients` and `activewindow` use Hyprland's tab-indented field
blocks. In particular, the real pid lets `omarchy-cmd-terminal-cwd`
read `/proc/<pid>/cwd`, and `at`/`size` round-trip through Omarchy's
window-width and capture scripts.

## Mutations

Classic dispatch and Omarchy's Lua dispatch vocabulary reach the same
actions. Supported families include:

- workspace focus and moving a window to a workspace;
- focus by selector or spatial direction, close, kill-active, cycle,
  fullscreen/maximize;
- move, resize, center, raise, pin, tags, and confirm-floating;
- `exec -- <argv...>` as direct argv and Lua `exec_cmd` as shell
  source, including `[[...]]` and `[=[...]=]` strings;
- `eval hl.dispatch(hl.dsp....)`;
- `eval hl.monitor({ output=..., scale=... })` for a live output;
- `eval hl.config({ cursor = { invisible = BOOL } })`, the live
  cursor-visibility property used by Omarchy's screensaver;
- `reload`, which re-reads chonkstep/Hyprland configuration and emits
  `configreloaded` only after it has applied.

`hl.config`, `hl.device`, and `hl.workspace_rule` are recognized and
refused by name when their requested property is not modeled. They are
not reported as unknown syntax. Monitor scaling validates the output
and range before changing anything, so an Omarchy script cannot record
a scale that the compositor said it applied but did not.

`keyword` is refused except for the named
`keyword cursor:invisible BOOL` screensaver fallback, which reaches the
same live cursor flag as `hl.config`. If the focused client that hid
the cursor disconnects without restoring it, chonkstep restores the
cursor automatically. Every other refusal names what does work instead:
chonkstep re-reads `~/.config/hypr` within a second of an edit, and
`hyprctl eval hl.monitor({ output=..., scale=... })` changes a live
scale. The one shipped Omarchy caller is the monitor panel's row toggle
(`hyprctl keyword monitor NAME,disable` / `NAME,preferred,auto,auto`),
and that specific form is refused by name for a reason the user can act
on: chonkstep drives every connected output and has no disable path, so
its `disabled` field is honestly `false` for every head and the panel's
checkmark will not move. Growing an output-disable path—routing that
form into the same validated apply `zwlr_output_management` already
uses—is a real but separate piece of work; until it exists, saying so
in the refusal is the honest answer, because `hyprctl` exits zero for a
refusal and the string is the only diagnostic a user gets.

The monitor object reports measured values, not conventional ones.
`refreshRate` is the driven mode's rate, `availableModes` is the
connector's mode list in `WIDTHxHEIGHT@RATEHz` with the current mode
first, and `make`/`model`/`serial` are read from the connector EDID. The
same `make model serial` description backs `monitor = desc:…`,
`wl_output`, IPC, and `zwlr_output_management`, so those interfaces
cannot describe one panel two ways. `serial` stays empty only when the
EDID itself supplies none; `vrr` remains covered by the adaptive-sync
work. A backend driving no real mode reports 60 Hz rather than 0,
because a bar divides this into a frame budget.

Tiling vocabulary—`layoutmsg`, `togglesplit`, `swapwindow`, `pseudo`,
groups, special workspaces, tiled workspace options—has no faithful
meaning on this floating desktop and is refused. The mirrored Omarchy
menu continues to hide the five installed `omarchy-hyprland-*` actions;
each remains genuinely tiling/Hyprland specific. In particular,
workspace layout toggle (`SUPER+L`) is unbound and its menu row is
absent, so it cannot display a false “layout changed” notification.

## Event stream and workspace lifetime

The event socket emits state diffs, plus the explicit post-reload
event:

`configreloaded`, `monitoraddedv2`, `monitorremoved`,
`createworkspacev2`, `destroyworkspacev2`, `workspacev2`, `workspace`,
`moveworkspacev2`, `focusedmon`, `fullscreen`, `openwindow`,
`closewindow`, `movewindowv2`, `windowtitlev2`, `windowtitle`,
`activewindowv2`, `activewindow`, `urgent`, and `activelayout`.

Addresses are `0x...` in JSON and bare hexadecimal in events; both are
the same `ClientId`. Workspace ids are one-based on this wire and
converted exactly once at its boundary.

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
| relative/locked/confined pointer | `zwp_relative_pointer_manager_v1` v1, `zwp_pointer_constraints_v1` v1 |
| gestures / tablets | `zwp_pointer_gestures_v1` v3, `zwp_tablet_manager_v2` v1 |
| foreign surface parenting | `zxdg_exporter_v2` and `zxdg_importer_v2` v1 |
| shortcut inhibition | `zwp_keyboard_shortcuts_inhibit_manager_v1` v1 |
| IME | `zwp_text_input_manager_v3` v1, `zwp_input_method_manager_v2` v1 |
| modern xdg helpers | `xdg_wm_dialog_v1`, `xdg_system_bell_v1`, `xdg_toplevel_tag_manager_v1` v1 |

The IME popup participates in rendering and hit testing, activation
focuses the target, pointer constraints follow focus and lifetime, and
presentation feedback comes from winit presentation or DRM vblank.
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
