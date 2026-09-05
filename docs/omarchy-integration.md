# Running Omarchy's tooling under chonkstep

Chonkstep's native Wayland session can host Omarchy 4's shell, menu,
services, and command-line tools without modifying Omarchy. It reads the
installed configuration beneath `$OMARCHY_PATH`, serves the Hyprland IPC
and Wayland protocols those tools consume, and runs as a normal uwsm
compositor session.

The X11 session remains a standalone chonkstep desktop and does not serve
Hyprland IPC. Use the Wayland session when Omarchy compatibility matters.

## Install and login

The release package is the load-bearing installation:

```sh
omarchy pkg aur add chonkstep
omarchy install desktop-chonkstep
```

The first command installs `chonkstep.desktop`, `chonkstep-uwsm.desktop`, the
compositor, the portal map, the SDDM picker theme, and the explicit
`omarchy-install-desktop-chonkstep` integration command. That package-provided
command follows Omarchy's `# omarchy:` metadata convention, so the second line
is discovered by the normal Omarchy dispatcher.

The integration command first verifies that the `chonkstep` package (or a
package providing it) is installed and ensures `uwsm`, the portal frontend,
`xdg-desktop-portal-wlr`, and the GTK fallback are present. The wlr backend is
the ScreenCast/Screenshot implementation selected by Chonkstep's portal map;
installing the map without those services would leave browser sharing or file
choosers broken. The command then installs
`/etc/sddm.conf.d/zz-chonkstep-theme.conf`. The `zz-` prefix is load-bearing:
fresh Omarchy writes its own selection to `99-omarchy-login.conf`, so an
earlier filename would silently lose.

It also installs
`/etc/systemd/system/sddm.service.d/90-chonkstep-resilience.conf`. Stock
Omarchy allows only five seconds for SDDM's entire session cgroup to stop and
permanently rate-limits the service after two quick starts. The ChonkStep
drop-in allows a 20-second orderly compositor teardown, adds a 3-second DRM/VT
reacquisition delay, and disables the permanent start-limit latch. It does not
restart SDDM or disturb the current session; the installer only reloads
systemd's unit definitions.

On an encrypted fresh install, Omarchy also enables SDDM autologin. Chonkstep
preserves that file and its `User=` value, adding only
`zz-chonkstep-autologin.conf` to select `chonkstep-uwsm.desktop`. On an
unencrypted install there is no autologin override; the visible picker
defaults to the exact **chonkstep (uwsm)** entry and still allows another
session to be chosen.

The `/etc` drop-ins are deliberately not owned by the package: package
upgrades cannot silently change the user's chosen login mode. Remove the
integration and package with:

```sh
omarchy remove desktop-chonkstep
```

That removes only Chonkstep's drop-ins and package. Omarchy's login files and
all user configuration and state are preserved.

Choose **chonkstep (uwsm)** at the next login. The direct **chonkstep
(Wayland)** entry is retained for non-systemd systems and troubleshooting.
From a TTY, the managed equivalent is:

```sh
exec uwsm start -g -1 -e -D chonkstep chonkstep.desktop
```

The checkout installer, `scripts/install.sh`, performs the same SDDM setup
directly and prints its drop-in-only undo command.

None of these paths changes `/usr/share/omarchy`, Omarchy's Hyprland
configuration, or `~/.config/omarchy`. The only `/usr/bin/omarchy-*` file is
the extension command owned by the chonkstep package itself.

## Session lifecycle

Under uwsm, chonkstep publishes `WAYLAND_DISPLAY`, the ready XWayland
`DISPLAY`, and `HYPRLAND_INSTANCE_SIGNATURE` with `uwsm finalize`,
participates in `graphical-session.target`, and lets uwsm clean the activation
environment at logout. Omarchy's lock-before-suspend, fcitx5, Bluetooth agent,
and XDG autostart services therefore share the compositor's lifetime.

The direct session is a recovery/non-systemd path, not the supported Omarchy
lifecycle. It publishes only the curated display/desktop variables (including
`DISPLAY` after XWayland reports ready) and never imports the entire process
environment. Test sessions are explicitly prevented from updating the user's
live activation environment. Use the uwsm entry for
`graphical-session.target`, XDG autostart, and Omarchy logout.

`TERM`, `HUP`, and `INT` are clean logouts. The compositor closes clients in
order and exits zero; the supervisor forwards the signal, exits zero, and
does not create a recovery marker. Panics and other abnormal deaths still
create the marker and enter the bounded crash-recovery loop.

This means the unmodified `omarchy-system-logout` works in the recommended
uwsm session. No logout shim or upstream patch is carried.

For manual recovery, switch to and log into a text VT first (for example
`Ctrl+Alt+F3`), then run `sudo systemctl reset-failed sddm.service` followed by
`sudo systemctl start sddm.service`. Do not use `systemctl restart sddm` from
the only graphical session as a recovery command: its intended first action is
to terminate that session and the terminal running the command.

## Compatibility inventory

| Omarchy surface | Status under chonkstep Wayland |
| --- | --- |
| Quickshell shell startup and crash supervision | Works; monitor queries and the event stream are live |
| `omarchy-restart-shell` | Works; Lua `exec_cmd` is applied and lock state is reported as `LOCK` |
| launch-or-focus and close-all helpers | Work against live clients, addresses, pids, and focus |
| `omarchy-system-logout` | Works in the uwsm session; direct-session signals are also clean |
| `omarchy-toggle-nightlight` | Works unmodified through `hyprsunset` and `hyprland-ctm-control-v1` |
| screenshot, region selection, and screen recording | Work through screencopy; selection-layer keys are scoped to the overlay lifetime |
| keybinding menu | Reads the compositor's actual bindings, including locked/repeat/release flags |
| keyboard/input widgets | Read real hardware devices, or bounded logical devices when nested |
| window pop, width, fullscreen, pin, tag, move, resize, and raise helpers | Map to real floating-window operations |
| themes, pickers, OSD, notifications, and the Omarchy menu | Work without compositor-specific translation |
| session lock and IME | Work through standard Wayland lock, text-input, and input-method protocols |

## Quickshell protocol boundary

Chonkstep serves the Quickshell-facing interfaces Omarchy itself uses:
Hyprland request/event sockets, `hyprland_focus_grab_manager_v1`,
`hyprland_toplevel_mapping_manager_v1`,
`hyprland_ctm_control_manager_v1`, wlr foreign-toplevel management,
wlr screencopy, and ext image-copy capture. Both `monitoradded` and
`monitoraddedv2` are emitted so legacy and current monitor listeners
refresh together.

Three optional visual-effect interfaces are intentionally not served:

- `hyprland_toplevel_export_manager_v1`: plugins that require this
  Hyprland-only thumbnail path render no preview; use ext image-copy
  capture for supported window capture.
- `hyprland_surface_manager_v1`: Hyprland-specific per-surface effects
  are unavailable, so plugins must retain their plain-surface fallback.
- `ext_background_effect_manager_v1`: behind-window blur is unavailable;
  translucent panels render without blur rather than receiving a fake
  effect acknowledgement.

`renameworkspace` is unreachable by construction: Chonkstep workspaces
have persistent one-based numeric wire names derived from their internal
indices, with no independent mutable-name field. It is therefore an
intentional model boundary, not an omitted command handler.

The screensaver launcher can open its terminal clients because long-bracket
Lua commands and `openwindow` events are supported. Chonkstep does not have
Hyprland's independent per-monitor workspace/focus model, so requests that
promise monitor-specific placement are refused and logged rather than
pretending all screens landed correctly.

Tiling-only operations—layouts, groups, pseudo-tiling, silent moves, and
special workspaces—remain deliberately unsupported on chonkstep's floating
desktop. The mirrored root menu omits the installed actions that have no
honest meaning here. Every IPC refusal emits a warning and increments a
session counter; see [hyprland-ipc.md](hyprland-ipc.md) for the exact query,
mutation, and event surface.

Chonkstep workspaces are persistent. Visiting workspace 9 leaves workspaces
1–9 available even when some become empty, so Omarchy's workspace row may
remain wider than it would under Hyprland. That is the desktop's real model,
not fabricated IPC state.

## Night light and protocol bridges

Omarchy's unmodified night-light command starts `hyprsunset`, which binds
`hyprland_ctm_control_manager_v1`. Chonkstep applies supported diagonal CTMs
through the hardware gamma path, arbitrates them with wlr gamma clients, and
restores the output when the controller exits. Unsupported matrices fail
with a protocol error. See [night-light.md](night-light.md).

The compositor also serves `hyprland_toplevel_mapping_manager_v1`, mapping
Quickshell's foreign-toplevel handles to the exact IPC client addresses, and
`hyprland_focus_grab_manager_v1` for shell popups. No patched Quickshell or
Omarchy binary is needed.

## Omarchy bar plugins

The package ships the optional `chonkstep.workspaces` and `chonkstep.theme`
plugins beneath `/usr/share/chonkstep/omarchy/plugins/`. They are not enabled
automatically because enabling a plugin changes the user's Omarchy layout.
Copy or link the desired plugin into
`~/.config/omarchy/plugins/<plugin-id>/`, then use `omarchy plugin enable`.
The full development and configuration instructions are in
[`omarchy/README.md`](../omarchy/README.md).

## Verify a session

```sh
/usr/lib/chonkstep/verify-install.sh
systemctl --user is-active graphical-session.target xdg-desktop-autostart.target
hyprctl monitors -j | jq -c '.[] | {name,focused,scale}'
hyprctl devices -j | jq '.keyboards'
hyprctl binds | sed -n '1,20p'
hyprctl hyprsunset temperature
```

The compositor and supervisor write their diagnostics to
`~/.local/state/chonkstep/wayland-session.log`. A refusal line there is a
compatibility bug or an intentional model boundary, never an operation that
was acknowledged as applied.
