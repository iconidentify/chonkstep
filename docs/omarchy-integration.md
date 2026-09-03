# Running Omarchy's tooling under chonkstep

Omarchy 4 is two things at once: a **desktop** (Hyprland, its config,
its keybindings) and a **distribution of tooling** — 427 `omarchy-*`
scripts, a Quickshell-based shell, a menu, themes, installers. A
chonkstep session replaces the first and hosts the second: it starts
`omarchy-launch-shell` itself, wears Omarchy's palette when the theme
follows, and mirrors Omarchy's menu into its own root menu (see
`docs/appearance.md` and the README's Omarchy section).

Most of that tooling is compositor-agnostic and simply works. A
handful of scripts ask Hyprland a question, and off Hyprland those
answers do not come. This page is the honest inventory: what works out
of the box, what a shim in `omarchy/shims/` fixes, and what stays
Hyprland-only. Every row was checked by reading the script on a live
Arch install of Omarchy 4.0.x; rows that could be exercised without
ending the session were also run, and where a claim is reasoning rather
than observation this page says so.

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
| the shell itself (`omarchy-launch-shell` at login) | **works** — the shell starts and runs | — | chonkstep launches it the way Omarchy's `autostart.lua` does |
| its **supervision** (relaunch after a crash) | **broken, silently** — a dead Quickshell never comes back | `omarchy/shims/bin/omarchy-launch-shell` | liveness by Wayland socket instead of `hyprctl -j monitors` |
| `omarchy-restart-shell` | **broken, destructively** — kills the shell, cannot respawn it, exits 1 | `omarchy/shims/bin/omarchy-restart-shell` | respawn by `setsid` instead of `hyprctl dispatch` |
| `omarchy-launch-or-focus` (and `-tui`, `-webapp`) | **broken, silently** — always opens a second copy | `omarchy/shims/bin/omarchy-launch-or-focus` | window list and activation over `wlr-foreign-toplevel` |
| `omarchy-system-logout` | **broken, silently** — shows the OSD, logs nobody out | `omarchy/shims/bin/omarchy-system-logout` | ends the session through logind; see the wart below |
| `omarchy-toggle-nightlight` | **broken** — two seconds of retries, no tint | *nothing yet* | needs a gamma protocol chonkstep does not implement |
| `omarchy-launch-screensaver` | **broken** — errors on stderr, launches nothing, exits 1 | *nothing* | not worth a shim; see below |
| `omarchy-hyprland-*` (24 scripts) | **inert by design** | — | excluded from the mirrored menu |
| `omarchy-capture-screenshot` / `-region` / `-screenrecording` | **work** | — | `grim`/`slurp`/`gpu-screen-recorder` over `wlr-screencopy`; two caveats below |
| `omarchy-theme-set`, the pickers, the OSD, notifications, `omarchy-menu` | **work** | — | all of it is shell IPC, no compositor involved |
| `omarchy-launch-webapp`, `-tui`, `-terminal`, `uwsm-app` launches | **work** | — | `uwsm-app` falls back to a plain exec outside a uwsm session |
| `omarchy-system-lock` | **works** | — | `ext-session-lock-v1`; chonkstep implements it |

### What "broken, silently" means, and why it is the interesting column

Every one of the four shimmed commands fails by *doing nothing* rather
than by reporting an error. `hyprctl` exits 1 with
`HYPRLAND_INSTANCE_SIGNATURE not set! (is hyprland running?)`, and each
of these scripts reads that as an answer:

- an empty window list means "no window is open", so launch one;
- a failed liveness probe means "the session is going", so stop
  supervising;
- `uwsm stop` finds no `wayland-wm@*.service`, prints "Compositor is
  not running." and **exits 0**.

None of that produces a visible failure. That is what makes these worth
shimming rather than documenting as limitations.

## The shims

Four scripts in `omarchy/shims/bin/`, each a copy of Omarchy's own with
the Hyprland-specific part replaced and every comment kept. Each file's
header states the original mechanism, the failure, and the
substitution, so a rebase onto a newer Omarchy is a diff rather than an
archaeology exercise.

The engineering is in one of them. `omarchy-launch-or-focus` needs to
enumerate windows and activate one, and the compositor-agnostic way to
do that is `zwlr_foreign_toplevel_management_v1` — the protocol that
exists for taskbars, which chonkstep advertises at version 3 (see
`crates/wm-wayland/src/protocols.rs`). The client is
**`chonk-toplevel`** (`crates/chonk-toplevel`), a ~500-line Wayland
client with four verbs:

```
chonk-toplevel list                # id, app id, title, states — tab separated
chonk-toplevel activate <pattern>  # raise and focus the first match
chonk-toplevel close <pattern>     # politely close the first match
chonk-toplevel close-all           # politely close every window
```

Its exit codes are the interface: `0` did it, `1` nothing matched, `2`
could not look, `3` usage. `1` versus `2` is the distinction the
`hyprctl | jq` pipeline loses, and the reason the shim can tell "no
such window, so launch" apart from "the focus half of this command is
off".

The pattern rule is a literal reimplementation of Omarchy's
`test("\bPATTERN\b"; "i")` against app id and title, so the shim picks
the same window Omarchy's script would have picked on Hyprland. The one
deliberate difference is documented in `matches_pattern`: the pattern
is matched **literally**, not as a regex, because every pattern Omarchy
passes is a literal and the dots in `org.omarchy.btop` should not match
any character.

### Installing them

```sh
omarchy/shims/install.sh              # symlinks into ~/.local/bin
omarchy/shims/install.sh --list       # what is linked, and what PATH resolves today
omarchy/shims/install.sh --uninstall  # removes only links into this checkout
```

`chonk-toplevel` has to exist. From a checkout that is
`cargo build --release -p chonk-toplevel`; the shims find it on `PATH`,
or under `target/release`, or under `target/debug`, or wherever
`$CHONK_TOPLEVEL` points.

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

Two consequences, both worth knowing before you decide the shims are
not working:

1. **For a chonkstep keybinding, name the shim by absolute path** in
   `[commands]`, or install into `/usr/local/bin`, which is ahead of
   `/usr/bin` in both PATHs.
2. **`omarchy <subcommand>` never reaches a shim.** The `omarchy` CLI
   sets `OMARCHY_BIN_DIR` to its own directory and `exec`s
   `$OMARCHY_BIN_DIR/omarchy-…` by absolute path, so `omarchy logout`
   runs `/usr/bin/omarchy-system-logout` whatever PATH says. Call the
   command by name — `omarchy-system-logout` — or use the menu row.

### The supervisor is a special case

chonkstep starts the shell by the **resolved path**
`$OMARCHY_PATH/bin/omarchy-launch-shell`, deliberately (it has just
checked that file exists). PATH cannot intercept the supervisor that
runs at login. To use the shim's supervisor for the whole session,
decline chonkstep's own launch and start the shim from `autostart`:

```toml
# ~/.config/chonkstep/config.toml
omarchy_shell = false
autostart = ["/path/to/chonkstep/omarchy/shims/bin/omarchy-launch-shell"]
```

The shim on `PATH` is still worth having: `omarchy-restart-shell`
(shimmed or not) respawns through `omarchy-launch-shell` by name, so a
manual restart picks up the portable supervisor either way.

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

### Nightlight — and why no shim ships

`omarchy-toggle-nightlight` drives `hyprsunset` through
`hyprctl hyprsunset temperature`. Two separate things are missing here,
and the second is the one that matters:

1. The **control channel** is Hyprland's IPC socket, so the set and the
   read-back both fail. On its own this would be shimmable.
2. `hyprsunset` tints the screen through
   **`hyprland-ctm-control-v1`**, and the portable alternatives
   (`wlsunset`, `gammastep`) use
   **`wlr-gamma-control-unstable-v1`**. chonkstep implements *neither*.
   Run on this session, hyprsunset says so itself:

   ```
   ┣ Setting the temperature to 4000K
   ┣ Found new output with ID 16, binding
   ✖ Compositor doesn't support hyprland-ctm-control-v1, are you running on Hyprland?
   ```

So there is no program on this session that can tint the screen, and a
shim would be a shim over nothing. Nightlight becomes available the day
chonkstep implements `wlr-gamma-control-unstable-v1` — at which point
`wlsunset` works and the upstream patch in `omarchy/upstream/` (which
adds exactly that fallback chain) makes Omarchy's own row work here
unchanged. Until then the honest answer is "not supported", and the
menu row wastes two seconds in ten retries before telling the shell to
refresh an indicator that never changed.

### Screensaver

`omarchy-launch-screensaver` needs three Hyprland things — `hyprctl
monitors` for the monitor list, `hyprctl dispatch` to place a terminal
*onto a named monitor*, and Hyprland's `.socket2.sock` event stream to
wait for each window to map. Run here it prints

```
jq: parse error: Invalid numeric literal at line 1, column 28
socat[…] E connect(, AF=1 "/run/user/1000/hypr//.socket2.sock", 36): No such file or directory
```

launches nothing, and exits 1. The first two have portable equivalents
in principle (`wl_output` for the list, foreign-toplevel for the wait);
the middle one does not — "spawn this command and have its window land
on that monitor" has no compositor-agnostic form, and rewriting the
script around per-monitor placement is a redesign, not a shim.

Note that this row is **not** filtered out of the mirrored menu:
`chonk_shell::omarchy_menu::is_hyprland_only` matches a word that is
exactly `hyprctl` or begins with `omarchy-hyprland-`, and
`omarchy-launch-screensaver force` is neither. Widening that rule to
name individual scripts would be a list to keep in sync with Omarchy;
the row is left where it is, doing nothing, which is what it did before
this page existed.

### Two capture caveats

Screenshots and screen recording work — `grim`, `slurp` and
`gpu-screen-recorder` all go through `wlr-screencopy`, which chonkstep
advertises at version 3. Two rough edges, both in the Hyprland-specific
trimmings rather than the capture:

- `omarchy-capture-screenshot` reads and restores
  `cursor:no_hardware_cursors` through `hyprctl getoption`/`keyword`.
  Both calls are `&>/dev/null`-guarded and their failure is harmless
  here (chonkstep composites its own cursor and does not bake it into
  a screencopy frame), so the screenshot is correct — the setting was
  never chonkstep's to change.
- `omarchy-capture-screenrecording --fullscreen` asks
  `omarchy-hyprland-monitor-focused` for a monitor name and
  `hyprctl monitors -j` for its resolution. Both come back empty, so
  gpu-screen-recorder is handed `-w ""`. **Fullscreen recording does
  not work** — read-verified from the script and from `hyprctl monitors
  -j` returning nothing here, not run. The default region flow (slurp)
  does work, because slurp is a layer-shell client and needs nothing
  from Hyprland.

## Preparing the same fixes for upstream

`omarchy/upstream/` holds the compositor-agnostic version of each of
these scripts, as complete replacements (`bin/`) and as unified diffs
against Omarchy 4.0.x (`patches/`). Every patch keeps Hyprland's
existing path first and adds a fallback, so nothing changes for a
Hyprland user. `omarchy/upstream/README.md` is written as the pull
request description would be.

Nothing in that directory has been sent anywhere. It is a prepared set.

## Checking it yourself

The two failures that are worth reproducing, because both are silent:

```sh
# 1. launch-or-focus opens a second copy. Run Omarchy's own script twice:
omarchy-launch-or-focus probe "foot --app-id=probe -- sleep 900"
sleep 3
omarchy-launch-or-focus probe "foot --app-id=probe -- sleep 900"
chonk-toplevel list | grep probe        # two windows

# Now the shim, twice, from a clean start:
chonk-toplevel close probe; chonk-toplevel close probe
omarchy/shims/bin/omarchy-launch-or-focus probe "foot --app-id=probe -- sleep 900"
sleep 3
omarchy/shims/bin/omarchy-launch-or-focus probe "foot --app-id=probe -- sleep 900"
chonk-toplevel list | grep probe        # one window, activated

# 2. The supervisor. `pkill -9 quickshell` on your live session and
#    watch nothing come back; `omarchy-restart-shell` will not fix it
#    either. To test it without losing your own shell, point a scratch
#    OMARCHY_PATH at a copy of $OMARCHY_PATH/shell (so quickshell's
#    -n does not collide with the live instance), run each supervisor
#    under `dbus-run-session`, kill the Quickshell it started by PID,
#    and watch for a new one. Omarchy's launcher exits; the shim logs
#    "Omarchy shell exited with status 137; relaunching." to the
#    journal under the omarchy-shell tag and brings it back.
```
