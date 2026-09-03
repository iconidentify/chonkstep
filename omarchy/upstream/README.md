# Keep these working on Hyprland, and make them work elsewhere too

Two of Omarchy's scripts ask a question and treat "no answer" as an
answer. On Hyprland they are correct and nothing here changes what
they do. Off Hyprland — or, for the first of the two, in *any* session
uwsm did not start, Hyprland included — the question comes back empty
and the script takes a wrong branch, silently.

Each change is the same shape: **Omarchy's existing path stays first,
unchanged, and a fallback is added behind it.** A Hyprland session
under uwsm never reaches the new code. Nothing is removed, no
dependency is added that Omarchy does not already have, and each patch
is self-contained.

The patches are in `patches/`, numbered; `bin/` holds the same scripts
whole, if reading them that way is easier. They apply with `-p1` from
the repository root and each one reproduces the file in `bin/`
exactly.

## This was five patches, and most of it was our bug

An earlier draft of this set had five patches in it. Three of them —
for `omarchy-launch-shell`, `omarchy-launch-or-focus` and
`omarchy-restart-shell` — argued that those scripts break wherever
`hyprctl` is not answering, and offered fallbacks that do not need it.

They are gone, because that diagnosis was ours to fix and we fixed it.
We maintain a Wayland compositor that hosts Omarchy's shell and
tooling, and the reason `hyprctl` was not answering was that we were
not answering it. We now serve Hyprland's IPC — the same socket, the
same wire format, the same JSON — so the real `hyprctl` binary works
against our session and **all three of those scripts are correct
unmodified**. Asking you to carry a fallback for a question we had
simply declined to answer was the wrong request.

What is left is the part that is not about the compositor at all, and
the part that no compositor IPC can reach.

## What each one fixes

### 0001 — `omarchy-system-logout`: log out of a session uwsm did not start

This one is not a Hyprland issue; it is a uwsm issue, and it applies to
a Hyprland session too.

`uwsm stop` looks for an active `wayland-wm@*.service` on the session
bus. A session uwsm did not start has none — a display manager that
`Exec`s a session script, or a bare `exec` from a TTY — and uwsm's
answer is not an error. From `uwsm/main.py`, `stop_wm()`:

```python
if not units:
    print_ok("Compositor is not running.")
    return False
```

`print_ok`, and the process exits 0. So the Logout row shows its OSD
for five seconds, closes the windows, and then leaves the user logged
in.

The patch keeps `uwsm stop` as the first tier, now asked with `-n`
first — uwsm's own dry run, which reports `Will stop compositor
<unit>.` when there is a unit and `Compositor is not running.` when
there is not, so it answers exactly this question without ending
anything. When there is no unit it falls back to
`loginctl terminate-session "$XDG_SESSION_ID"`, which is the portable
"log this session out" and what ends a session started by a display
manager or from a TTY.

The `nohup bash -c "sleep 2 && uwsm stop"` line becomes a backgrounded
function, so the fallback chain lives in one place instead of being
re-quoted into a string. `trap '' HUP` and the redirection are what
`nohup` was doing.

Window closing is **not** touched: `omarchy-hyprland-window-close-all`
stays exactly as it is. An earlier draft added a fallback for it; it
turned out not to be needed, because that script is `hyprctl clients`
piped into `hyprctl dispatch` and any compositor answering Hyprland's
IPC answers it.

### 0002 — `omarchy-toggle-nightlight`: a fallback chain

`hyprsunset` is unreachable off Hyprland twice over, and the second
reason is the one that matters: it is driven through Hyprland's IPC
socket, *and* it tints through `hyprland-ctm-control-v1`. Serving
Hyprland's IPC does not help here at all — `hyprctl hyprsunset ...`
never touches that socket, it is routed to hyprsunset's own — and the
protocol is a separate thing again. Run on a compositor that does not
implement it, hyprsunset says so itself:

```
┣ Setting the temperature to 4000K
┣ Found new output with ID 16, binding
✖ Compositor doesn't support hyprland-ctm-control-v1, are you running on Hyprland?
```

Today the row spends two seconds in ten retries and then asks the
shell to refresh an indicator that never changed.

The patch adds `wlsunset` and `gammastep`, which do the same job over
`wlr-gamma-control-unstable-v1`. Two details a reviewer should look
at:

- Neither has a query interface, so the chosen temperature is recorded
  in `$XDG_RUNTIME_DIR/omarchy-nightlight-temperature` and that file is
  what `--status` reports. The hyprsunset path still reads the real
  temperature back and is untouched.
- Neither can be retuned once running, so a change restarts the daemon,
  and turning nightlight *off* stops it rather than running a daemon
  that does nothing.

The one line the patch changes for a reason unrelated to any of this
is quoting `$OFF_TEMP` on the right-hand side of a `[[ … == … ]]`,
which shellcheck flags today (SC2053) and which is inside a hunk the
patch already touches. Say the word and it comes back out.

## Testing

We could not test this patch when it was first written: our compositor
could not tint a screen by any means, so `wlsunset` had nothing to bind
either and the patch was offered on shape alone. It now implements
`wlr-gamma-control-unstable-v1`, so this is tested rather than
reasoned about.

**0002 — `omarchy-toggle-nightlight`**, on a live session on real
hardware:

```
$ omarchy-toggle-nightlight --status            # unpatched
{"enabled":false,"temperature":null}
$ time omarchy-toggle-nightlight                # unpatched
┏ hyprsunset v0.4.0 ━━╸
┣ Loaded 1 profiles
real    0m2.133s
$ omarchy-toggle-nightlight --status            # unpatched, after
{"enabled":false,"temperature":null}
$ pgrep -a hyprsunset
                                                # nothing; it exited
```

Two seconds, no tint, status unchanged — and running `hyprsunset`
directly gives the `hyprland-ctm-control-v1` error quoted above.

```
$ bin/omarchy-toggle-nightlight                 # patched
$ pgrep -a wlsunset
1609230 wlsunset -T 4001 -t 4000
$ bin/omarchy-toggle-nightlight --status
{"enabled":true,"temperature":4000}
```

and the compositor's own log confirms the ramp reached the hardware,
and that toggling off put it back:

```
gamma ramp programmed  size=256 white_r=65535 white_g=53969 white_b=39177
gamma control released; restoring the original ramp
gamma ramp programmed  size=256 white_r=65535 white_g=65535 white_b=65535
```

`gammastep` is not installed here, so the `gammastep` arm is written
from its documented interface and has not been run — a second pair of
eyes on `-O` would be welcome. The `wlsunset` arm is the one above.

**0001 — `omarchy-system-logout`.** The no-op is verified directly:

```
$ uwsm stop -n
Stopping compositor...
Compositor is not running.
$ echo $?
0
```

on a live graphical session that logind knows about
(`loginctl list-sessions` → session `1`, `seat0`, `tty1`). The
`loginctl` tier is verified as far as branch selection — `end_session`
lifted verbatim with the two session-ending calls replaced by an echo
picks `loginctl terminate-session 1`, the right session — but the call
itself is reasoned about, not run: we did not want to end the session
we were working in.

`shellcheck` is clean on both patched scripts, and both apply cleanly
to `quattro` at `f99d33a` and reproduce `bin/` byte for byte.

## Where these came from

We maintain a non-tiling Wayland compositor that hosts Omarchy's shell
and tooling, so these are the seams we hit. The logout one is a seam
for anybody running Omarchy outside a uwsm-started session, Hyprland
included; the nightlight one is a seam for sway, river, Wayfire and
labwc. Neither is offered as anything about our own project — the
three patches that *were* about our project have been withdrawn, which
is the honest half of this set's history and the reason it is now two
patches instead of five.
