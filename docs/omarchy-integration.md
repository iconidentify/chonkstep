# Running Omarchy's tooling under chonkstep

Omarchy 4 is two things at once: a **desktop** (Hyprland, its config,
its keybindings) and a **distribution of tooling** — 427 `omarchy-*`
scripts, a Quickshell-based shell, a menu, themes, installers. A
chonkstep session replaces the first and hosts the second: it starts
`omarchy-launch-shell` itself, wears Omarchy's palette when the theme
follows, and mirrors Omarchy's menu into its own root menu (see
`docs/appearance.md` and the README's Omarchy section).

Most of that tooling is compositor-agnostic and simply works. The rest
asks Hyprland a question — and chonkstep now answers, in Hyprland's own
IPC (`docs/hyprland-ipc.md`, on by default). This page is the honest
inventory: what works out of the box, what a shim in `omarchy/shims/`
fixes, and what stays Hyprland-only.

Nothing here modifies Omarchy. `/usr/share/omarchy` and `/usr/bin/omarchy-*`
are read and never written; a shim wins by being earlier on `PATH`, and
it is uninstalled by removing a symlink.

This page is about the *scripts*. `omarchy/README.md` covers the bar
widgets that replace Omarchy's Hyprland-bound ones, and
`docs/omarchy-mode.md` covers the config posture that hands the whole
desktop over to Omarchy's shell.

## The inventory

| Omarchy command | Under chonkstep, unshimmed | Fixed by | Notes |
|---|---|---|---|
| the shell itself (`omarchy-launch-shell` at login) | **works** | — | chonkstep launches it the way Omarchy's `autostart.lua` does |
| its **supervision** (relaunch after a crash) | **works** | — | `compositor_alive`'s `hyprctl -j monitors` is answered; see below |
| `omarchy-restart-shell` | **works** | — | kill, `hyprctl dispatch exec_cmd`, ping — all answered; one caveat below |
| `omarchy-launch-or-focus` (and `-tui`, `-webapp`) | **works** | — | `hyprctl clients -j` finds the window, `dispatch` focuses it |
| `omarchy-hyprland-window-close-all` | **works** | — | `hyprctl clients` piped into `hyprctl dispatch` |
| `omarchy-system-logout` | **broken, silently** — shows the OSD, closes the windows, logs nobody out | `omarchy/shims/bin/omarchy-system-logout` | `uwsm stop` is a no-op here; see the wart below |
| `omarchy-toggle-nightlight` | **broken** — two seconds of retries, no tint | *nothing yet* | `hyprsunset` needs `hyprland-ctm-control-v1`; use `wlsunset` |
| `omarchy-launch-screensaver` | **broken** — launches nothing, exits 1 | *nothing* | not worth a shim; see below |
| `omarchy-hyprland-*` (24 scripts) | mostly answered now | — | still excluded from the mirrored menu |
| `omarchy-capture-screenshot` / `-region` / `-screenrecording` | **work** | — | `grim`/`slurp`/`gpu-screen-recorder` over `wlr-screencopy` |
| `omarchy-theme-set`, the pickers, the OSD, notifications, `omarchy-menu` | **work** | — | all of it is shell IPC, no compositor involved |
| `omarchy-launch-webapp`, `-tui`, `-terminal`, `uwsm-app` launches | **work** | — | `uwsm-app` falls back to a plain exec outside a uwsm session |
| `omarchy-system-lock` | **works** | — | `ext-session-lock-v1`; chonkstep implements it |

### What changed, and how it was checked

Three rows in that table used to read "broken, silently" and had a shim
against their name. They are fixed by `crates/chonk-hyprland-ipc`
rather than by a shim, and they were re-checked against **Omarchy's
unmodified scripts** in a nested chonkstep on a private `Xvfb`, with a
scratch `$OMARCHY_PATH` so `quickshell -n` could not collide with the
live instance. Each was run twice: once with the IPC on (the default)
and once with `CHONKSTEP_HYPRLAND_IPC=0`, so the difference is
attributable rather than assumed.

**Supervision** (`omarchy-launch-shell`). Its `compositor_alive()` is
three attempts at `hyprctl -j monitors`; off Hyprland every attempt
failed, the supervisor read that as "the session is tearing down" and
exited 0, so a Quickshell that died stayed dead — and with it the bar,
the OSD, the notification daemon and the lock screen. Start the
unmodified supervisor, `kill -9` the Quickshell it started by recorded
PID:

```
IPC on   RESULT: RELAUNCHED — quickshell #2 pid=1549985 ; supervisor still alive
IPC off  RESULT: NOT relaunched ; supervisor exited
```

**`omarchy-launch-or-focus`.** It finds the window with `hyprctl
clients -j | jq … | head -n1`; an empty address means "nothing is
open", so every launch-or-focus keybinding opened a second copy. Run
the unmodified script twice for the same app:

```
IPC on   clients after run 1: 1 ; clients after run 2: 1   (and it is the active window)
IPC off  jq: parse error: Invalid numeric literal ... (twice) ; a second copy each time
```

and it activates the right one rather than merely finding it — with
Alacritty focused, `omarchy-launch-or-focus foot "foot"` leaves `foot`
active and the client count unchanged at 2.

**`omarchy-restart-shell`**, the destructive one: it kills Quickshell
and respawns it with `hyprctl dispatch 'hl.dsp.exec_cmd("omarchy-launch-shell")'`,
so off Hyprland the one manual recovery for a dead shell was also what
left you without one. Traced through the unmodified script:

```
IPC on   + timeout 5 quickshell kill -p /tmp/ck-om/shell --any-display
         + hyprctl dispatch 'hl.dsp.exec_cmd("omarchy-launch-shell")'
         + omarchy-shell shell ping        → answers on the 2nd attempt
         + exit 0                          → a new quickshell pid
IPC off  Omarchy shell did not become ready after restart.   exit 1, no shell
```

### One caveat on `omarchy-restart-shell`: its lock guard is blind here

The script refuses to restart while the session is locked, because
restarting the locker would strand the session behind the compositor's
failsafe. It asks `omarchy-hyprland-session-locked`, which reads the
lock out of the `solitaryBlockedBy` field of `hyprctl -j monitors`.

chonkstep reports `solitaryBlockedBy: null` unconditionally
(`chonk-hyprland-ipc/src/state.rs`) — nothing here blocks a solitary
client from direct scanout for a reason it could name — so that helper
always answers **unlocked** (exit 1, verified), and the guard never
fires. Restarting the shell while the screen is locked would therefore
kill the locker without re-locking afterwards.

This is a chonkstep gap, not an Omarchy one, and the fix is one field:
report `["LOCK"]` there while an `ext-session-lock` is held. Until it
lands, do not run `omarchy-restart-shell` from a locked session; ask
`omarchy-shell lock status` first if in doubt.

## The one remaining shim

`omarchy/shims/bin/omarchy-system-logout`, and its problem is not a
compositor problem. `uwsm stop` looks for an active
`wayland-wm@*.service` on the session bus; chonkstep is started by the
display manager (or from a TTY) through its own session script, not by
uwsm, so there is no such unit. uwsm's answer is not an error:

```
$ uwsm stop -n
Stopping compositor...
Compositor is not running.
$ echo $?
0
```

So the Logout row shows its OSD, closes the windows — that part works
now, `omarchy-hyprland-window-close-all` is `hyprctl clients` piped
into `hyprctl dispatch` — and leaves you logged in. The shim keeps
`uwsm stop` as a fast path (asked with `-n` first, so it is used only
when there is really a unit) and falls back to logind's
`loginctl terminate-session`.

The shim needs nothing from this repository: no `chonk-toplevel`, no
control socket, no build. It is Omarchy's script with one statement
replaced.

### Installing it

```sh
omarchy/shims/install.sh              # symlinks into ~/.local/bin
omarchy/shims/install.sh --list       # what is linked, and what PATH resolves today
omarchy/shims/install.sh --uninstall  # removes only links into this checkout
```

### Which directory, and the part that is genuinely awkward

A shim only takes effect if it is found before `/usr/bin`, and **this
session has two different PATHs**:

- A **login shell** (`bash -lc`) puts `~/.local/bin` *ahead* of
  `/usr/bin`. This is the important one: chonkstep runs every mirrored
  Omarchy menu action as `bash -lc '<action>'`
  (`chonk_shell::omarchy_menu::action_argv`), exactly as Omarchy's own
  shell does, and the menu rows name their scripts bare
  (`"action":"omarchy-system-logout"`). So a `~/.local/bin` install
  covers the menu, and covers anything you type in a terminal.
- The **session's own environment** — what chonkstep hands to a process
  it spawns directly — has `/usr/bin` *ahead* of `~/.local/bin` on a
  stock Arch install. chonkstep's `[commands]` table and `autostart`
  are argv lists, not shell lines, so a bare name there resolves
  against that PATH and finds Omarchy's copy.

Two consequences:

1. **For a chonkstep keybinding, name the shim by absolute path** in
   `[commands]`, or install into `/usr/local/bin`, which is ahead of
   `/usr/bin` in both PATHs.
2. **`omarchy <subcommand>` never reaches a shim.** The `omarchy` CLI
   sets `OMARCHY_BIN_DIR` to its own directory and `exec`s
   `$OMARCHY_BIN_DIR/omarchy-…` by absolute path, so `omarchy logout`
   runs `/usr/bin/omarchy-system-logout` whatever PATH says. Call the
   command by name — `omarchy-system-logout` — or use the menu row.

Note that the supervisor no longer needs any of this. chonkstep starts
the shell by the resolved path `$OMARCHY_PATH/bin/omarchy-launch-shell`,
which PATH cannot intercept — and that is now fine, because Omarchy's
own launcher supervises correctly here.

### The logout wart, said plainly

chonkstep has no script-reachable "exit". Its root menu's Exit ends
`wm_wayland::run` with status 0, which is the one clean end the session
script (`scripts/wayland-session.sh`) recognises; there is no socket
verb and no signal that reproduces it. So the logout shim uses logind's
`terminate-session`, and logind's SIGTERM is — by the session script's
definition — an abnormal exit. It drops a `recovery` marker in
`$XDG_STATE_HOME/chonkstep`, and the *next* session announces itself as
recovered and comes back locked if `lock_command` is set.

The shim cannot clean that up: anything it leaves running to remove the
marker is inside the scope logind is tearing down. The fix is
compositor-side and small — a `quit` marker beside the existing
`restart` and `reload` ones, or a SIGTERM handler that exits 0 — and
until it exists, **the root menu's own Exit is the better way out**.
The shim is for the case it is for: Omarchy's Logout row, which today
does nothing at all.

## What stays Hyprland-only

### Nightlight — the one thing the IPC cannot reach

`omarchy-toggle-nightlight` drives `hyprsunset` through
`hyprctl hyprsunset temperature`, and neither half of that is served
here:

1. `hyprctl hyprsunset …` writes **nothing** to the compositor socket —
   it is routed to hyprsunset's own `.hyprsunset.sock`. Answering
   Hyprland's IPC does not help at all:

   ```
   $ hyprctl hyprsunset temperature
   Couldn't connect to …/hypr/<signature>/.hyprsunset.sock. (3)
   ```

2. `hyprsunset` tints through **`hyprland-ctm-control-v1`**, which
   chonkstep does not implement. Run on this session it says so:

   ```
   ┣ Setting the temperature to 4000K
   ┣ Found new output with ID 16, binding
   ✖ Compositor doesn't support hyprland-ctm-control-v1, are you running on Hyprland?
   ```

So the unmodified row spends about two seconds in ten retries and
changes nothing — measured on the live session, `--status` reading
`{"enabled":false,"temperature":null}` before and after.

**The screen itself is tintable.** chonkstep implements
`wlr-gamma-control-unstable-v1` (`docs/night-light.md`), so `wlsunset`,
`gammastep` and `redshift` all work on the DRM backend. Until Omarchy's
row learns about them, warm the screen directly:

```sh
wlsunset -T 6500 -t 3000 -S 07:00 -s 20:00
```

The patch in `omarchy/upstream/` adds exactly that fallback chain to
Omarchy's own row, and it is now **tested** rather than hoped for: the
patched script starts `wlsunset -T 4001 -t 4000`, reports
`{"enabled":true,"temperature":4000}`, and the compositor logs the ramp
reaching the hardware and being restored on the way back off. No shim
ships for it, because the fix belongs upstream and the workaround is
one command.

Note that the gamma control is a **DRM-backend** feature: a nested
chonkstep on someone else's desktop advertises no gamma global at all
(deliberately — see `docs/night-light.md`), so nightlight cannot be
tested in a nested session.

### Screensaver

`omarchy-launch-screensaver` needs Hyprland's `.socket2.sock` event
stream to wait for each window to map, and `hyprctl dispatch` to place
a terminal *onto a named monitor*. chonkstep serves the event socket,
but "spawn this command and have its window land on that monitor" has
no compositor-agnostic form and no chonkstep equivalent — rewriting the
script around per-monitor placement is a redesign, not a shim.

This row is **not** filtered out of the mirrored menu:
`chonk_shell::omarchy_menu::is_hyprland_only` matches a word that is
exactly `hyprctl` or begins with `omarchy-hyprland-`, and
`omarchy-launch-screensaver force` is neither. Widening that rule to
name individual scripts would be a list to keep in sync with Omarchy.

### Two capture caveats

Screenshots and screen recording work — `grim`, `slurp` and
`gpu-screen-recorder` all go through `wlr-screencopy`, which chonkstep
advertises at version 3. Two rough edges, both in the Hyprland-specific
trimmings rather than the capture:

- `omarchy-capture-screenshot` reads and restores
  `cursor:no_hardware_cursors` through `hyprctl getoption`/`keyword`.
  chonkstep declines both deliberately (`docs/hyprland-ipc.md` §3), and
  both calls are `&>/dev/null`-guarded, so the screenshot is correct —
  the setting was never chonkstep's to change.
- `omarchy-capture-screenrecording --fullscreen` asks
  `omarchy-hyprland-monitor-focused` for a monitor name and
  `hyprctl monitors -j` for its resolution. Both are answered now, so
  this is expected to work; it has not been re-run since the IPC
  landed, and this line says so rather than claiming it.

## Preparing the same fixes for upstream

`omarchy/upstream/` holds the compositor-agnostic version of the two
scripts that still need one, as complete replacements (`bin/`) and as
unified diffs against Omarchy's `quattro` branch (`patches/`). Both
keep Omarchy's existing path first and add a fallback, so nothing
changes for a Hyprland user on uwsm.

It used to hold five. Three were withdrawn when the IPC layer made the
unmodified scripts correct — the honest outcome, and a better one than
a patch. `omarchy/upstream/README.md` is written as the pull request
description would be, and says so.

Nothing in that directory has been sent anywhere. It is a prepared set.

## Checking it yourself

```sh
# Is the IPC actually answering? The compositor logs the signature it
# bound, and both sockets live under it.
grep 'hyprland ipc listening' ~/.local/state/chonkstep/wayland-session.log
hyprctl monitors -j | jq -c '.[]|{name,focused}'

# launch-or-focus no longer opens a second copy. Omarchy's own script,
# twice:
omarchy-launch-or-focus probe "foot --app-id=probe -- sleep 900"
sleep 3
omarchy-launch-or-focus probe "foot --app-id=probe -- sleep 900"
hyprctl clients -j | jq '[.[]|select(.class=="probe")]|length'   # 1

# The supervisor. Do this against a scratch OMARCHY_PATH (a copy of
# $OMARCHY_PATH/shell, so quickshell's -n does not collide with your
# live instance), under dbus-run-session, and kill the Quickshell it
# started *by recorded PID*. A bare `pkill quickshell` or
# `pgrep -f omarchy-launch-shell` will find your live session's shell
# and take your desktop down with it.

# The logout no-op, safely:
uwsm stop -n; echo "exit=$?"     # "Compositor is not running.", exit 0

# Nightlight, safely (Ctrl-C to restore; the compositor puts the ramp
# back when the client goes away):
wlsunset -T 4001 -t 4000
```
