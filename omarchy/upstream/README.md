# Keep these working on Hyprland, and make them work elsewhere too

Five of Omarchy's scripts ask Hyprland a question and treat "no answer"
as an answer. On Hyprland they are correct and nothing here changes
what they do. On any other Wayland compositor the question comes back
empty and the script takes a wrong branch — quietly, in four of the
five cases, which is what makes them worth a patch rather than a
README note.

Each change is the same shape: **Hyprland's existing path stays first,
unchanged, and a fallback is added behind it.** A Hyprland session
never reaches the new code. Nothing is removed, no dependency is added
that Omarchy does not already have (`socat` and `jq` are both already
required by scripts in `bin/`), and each patch is self-contained.

The patches are in `patches/`, numbered; `bin/` holds the same scripts
whole, if reading them that way is easier. They apply with `-p1` from
the repository root and each one reproduces the file in `bin/` exactly.

## What each one fixes

### 0001 — `omarchy-launch-shell`: liveness by socket, not by hyprctl

The supervisor's job is to relaunch Quickshell after a death Qt turned
into a bare `_exit()`. Before relaunching it checks whether the
compositor is still there, so a session that is tearing down does not
burn the attempt budget:

```sh
compositor_alive() { for attempt in 1 2 3; do hyprctl -j monitors >/dev/null 2>&1 && return 0; ...; done; return 1; }
```

Off Hyprland `hyprctl` exits 1 on every attempt, so `compositor_alive`
reports the session as going and the supervisor exits 0 instead of
relaunching. A Quickshell that dies stays dead, and with it the bar,
the OSD, the notification daemon and the lock screen, until the next
login.

The patch adds a second tier inside the same retry loop: a Wayland
compositor is alive exactly when its socket is present and accepts a
connection. That is true on Hyprland too — this is arguably the more
honest test of the two, since it asks about the display server the
shell is actually connected to rather than about a side channel — but
it is placed second so Hyprland's behaviour is bit-for-bit unchanged.

`socat` does the connect; without it the file test alone still stands,
which is strictly better than asking a compositor that is not running.

### 0002 — `omarchy-launch-or-focus`: a portable window activator

This is the script behind every "open it, or raise it if it is already
open" keybinding, and everything routed through
`omarchy-launch-or-focus-tui` and `-webapp`. It finds the window with
`hyprctl clients -j | jq … | head -n1`. Off Hyprland the address is
always empty, so it always takes the launch branch: every such
keybinding opens a second copy of an app that is already on screen.

The portable mechanism is `zwlr_foreign_toplevel_management_v1` — the
protocol that exists so an outside process can enumerate windows and
activate one, implemented by every wlroots-descended compositor and by
Hyprland. What varies is the *client*: there is no single CLI for it
that ships everywhere. So the patch takes one as configuration rather
than hardcoding a package:

```sh
"$OMARCHY_TOPLEVEL_HELPER" activate <window-pattern>
# exit 0 activated · 1 nothing matched · anything else: could not look
```

and falls back to `wlrctl` when no helper is configured, since that is
the most widely packaged implementation. A session with neither behaves
exactly as it does today.

Two notes for review:

- The `hyprctl` branch is now gated on `HYPRLAND_INSTANCE_SIGNATURE`
  and moved into a function, which also means the pipeline is not run
  at all where it cannot work. The jq expression is unchanged.
- **We have exercised the helper tier and not the `wlrctl` tier.** Our
  helper is a small `wlr-foreign-toplevel` client whose pattern
  matching is a deliberate reimplementation of this script's
  `test("\bPATTERN\b"; "i")` over app id and title, so it selects the
  same window; it is in the chonkstep repository as
  `crates/chonk-toplevel` if it is useful as a reference. The `wlrctl`
  invocations are written from its documented interface and should be
  checked by someone who has it installed, or dropped in favour of the
  helper hook alone.

### 0003 — `omarchy-system-logout`: log out of any session

`uwsm stop` looks for an active `wayland-wm@*.service` on the session
bus. A session that uwsm did not start has none, and uwsm's answer is
not an error — it prints "Compositor is not running." and **exits 0**.
Combined with `omarchy-hyprland-window-close-all` closing nothing, the
Logout row shows its OSD for five seconds and then does nothing at all.

The patch keeps `uwsm stop` as the first tier, now asked with `-n`
first so it is used only when there is really a unit to stop, and falls
back to `loginctl terminate-session "$XDG_SESSION_ID"` — the portable
"log this session out", and what ends a session started by a display
manager or from a TTY. Window closing gains the same
`OMARCHY_TOPLEVEL_HELPER` seam 0002 introduces (`close-all`), which is
the protocol's polite close, not a kill.

The `nohup bash -c "sleep 2 && uwsm stop"` line becomes a
backgrounded function, so the fallback chain lives in one place instead
of being re-quoted into a string.

### 0004 — `omarchy-toggle-nightlight`: a fallback chain

`hyprsunset` is unreachable off Hyprland twice over: it is driven
through Hyprland's IPC socket, *and* it tints through
`hyprland-ctm-control-v1`. It says so itself:

```
✖ Compositor doesn't support hyprland-ctm-control-v1, are you running on Hyprland?
```

Today the row spends two seconds in ten retries and then asks the shell
to refresh an indicator that never changed.

The patch adds `wlsunset` and `gammastep`, which do the same job over
`wlr-gamma-control-unstable-v1`. Two details a reviewer should look at:

- Neither has a query interface, so the chosen temperature is recorded
  in `$XDG_RUNTIME_DIR/omarchy-nightlight-temperature` and that file is
  what `--status` reports. The hyprsunset path still reads the real
  temperature back and is untouched.
- Neither can be retuned once running, so a change restarts the daemon,
  and turning nightlight *off* stops it rather than running a daemon
  that does nothing.

We cannot test this one: the compositor we work on implements neither
gamma protocol, so `wlsunset` has nothing to bind either. The patch is
offered because it is the right shape and because it makes the row
correct on sway, river, Wayfire and labwc — please treat the
wlsunset/gammastep arguments as needing a second pair of eyes.

### 0005 — `omarchy-restart-shell`: respawn without dispatch

The companion to 0001, and the more urgent of the two, because it is
destructive rather than merely inert. The script kills Quickshell with
`quickshell kill` (portable, works), then respawns it with

```sh
hyprctl dispatch 'hl.dsp.exec_cmd("omarchy-launch-shell")'
```

Off Hyprland that fails, nothing is spawned, the readiness loop times
out, and the script reports "Omarchy shell did not become ready after
restart" and exits 1 — having already killed the shell. The one manual
recovery for a dead shell is also what leaves you without one.

The patch keeps the dispatch first and falls back to `setsid` from an
environment reconstructed out of the systemd user manager. That is the
portable stand-in for the comment's own reasoning ("spawn from Hyprland
so the shell inherits the canonical session environment"): a Wayland
session publishes `WAYLAND_DISPLAY` and `XDG_CURRENT_DESKTOP` there so
that D-Bus-activated services — the desktop portals in particular — can
find the display, which makes it the one place outside the compositor
that knows the canonical values.

It also gives the lock check a fallback. `omarchy-hyprland-session-locked`
exits 2 ("undetermined") where there is no `hyprctl`; on that answer the
patch asks the lock service itself (`omarchy-shell lock status`), which
is the question actually being asked. What that cannot see is the
recovery case your own comment describes — a session still held by the
compositor's failsafe behind a locker that has already died — because
that state is only visible in `solitaryBlockedBy`. Rather than guess,
the patch exposes it as `--relock`, and leaves the Hyprland path
detecting it automatically exactly as it does now.

## Testing

On the compositor we work on, and with the caveats named per patch:

- **0001** — verified A/B against a scratch `$OMARCHY_PATH` (a copy of
  `shell/`, so `quickshell -n` does not collide with the live
  instance), each supervisor under `dbus-run-session`, killing the
  Quickshell it started by recorded PID. Unpatched: the supervisor
  exits. Patched: `Omarchy shell exited with status 137; relaunching.`
  in the journal under the `omarchy-shell` tag, and a new Quickshell
  PID.
- **0002** — verified with a helper: Omarchy's script run twice leaves
  two windows; the patched logic run twice leaves one, activated.
- **0003** — the `uwsm stop` no-op is verified (`uwsm stop -n` reports
  "Compositor is not running." and exits 0). The `loginctl` tier is
  reasoned about, not tested; we did not want to end the session we
  were working in.
- **0004** — not tested; see above.
- **0005** — the failure is verified by reading the script and by
  confirming `hyprctl dispatch` fails here. The respawn is the same
  `setsid` mechanism 0001's supervisor is launched by in the A/B above.

`shellcheck` is clean on all five.

## Where these came from

We maintain a non-tiling Wayland compositor that hosts Omarchy's shell
and tooling, so these are the seams we hit. They are equally seams for
sway, river, Wayfire, labwc and anyone running Omarchy's tooling
outside its own session — which is why they are offered here as
compositor-agnostic patches rather than as anything about our own
project. Each keeps Hyprland's path first because that is the path
almost every Omarchy user is on.
