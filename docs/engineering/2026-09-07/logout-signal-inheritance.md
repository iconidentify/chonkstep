# Logout stalled on inherited signal masks

The logout requested from the Omarchy menu at 22:38:37 PDT on September 7
closed windows promptly, but the desktop remained responsive while shutdown
waited on background processes. This was a delayed logout, not a successful
fix being activated: no logout changes had been applied at that point.

## Evidence and cause

The user journal records `uwsm stop` at 22:38:39. At 22:40:10, systemd killed
`app-chonkstep-udiskie-34c46b02.scope` after its 90-second stop timeout. Only
then did the compositor's service begin stopping. Its remaining `inotifywait`,
`faked`, and `uwsm` processes caused another 10-second timeout. The session
was stopped at 22:40:20; a new compositor session became active at 22:40:38.

Live `/proc/*/status` checks in that new session showed the same `SigBlk`
value, `0000000000004003`, in the compositor, udiskie, inotifywait, and faked.
That is HUP, INT, and TERM. ChonkStep registered calloop's signalfd source
before creating workers and launching applications. The blocked mask
therefore reached all of those descendants. Linux preserves a signal mask
across both fork and exec; see the [signal manual](https://man7.org/linux/man-pages/man7/signal.7.html).

## Implementation and review

`wm-wayland/src/termination.rs` replaces the signalfd source with
[signal-hook's self-pipe registration](https://docs.rs/signal-hook/0.4.4/signal_hook/low_level/pipe/fn.register.html).
The signal handler writes to a nonblocking socket; calloop reads it and sets
the existing clean-logout flags. Children inherit normal signal delivery,
and exec resets caught handlers, including for children spawned inside
Smithay rather than through ChonkStep's launch helpers. SIGABRT retains its
crash behavior.

The three termination signals are explicitly unblocked after handler
registration and before workers start. This handles an upgrade via exec from
the old compositor, which left those signals blocked. Unrelated blocked
signals are preserved. Registration owns its handlers and releases them on
partial installation failure or normal teardown; the event loop owns the
reader. Spurious readiness without a byte does not request logout. The
handler contains no compositor state changes, logging, or blocking work.

The recorder's existing explicit signal-default wrapper is retained. No
systemd timeout or Omarchy logout script was changed.

## Validation

- The new application-inheritance regression failed against the previously
  installed release, observing mask `0x4003` instead of zero.
- All three new integration tests passed against both debug and optimized
  release builds: autostart and interactive children receive termination
  signals; Smithay's XWayland child has an unblocked termination mask;
  inherited masks are repaired; HUP, INT, and TERM each produce a successful
  compositor exit with the clean-logout log message.
- An isolated scope launched by a nested compositor exercised the real
  systemd user manager. With a three-second timeout used only for that probe,
  the old release stopped in 3.052 seconds with `Result=timeout`. The fixed
  debug build stopped in 0.006 seconds with `Result=success`. All probe
  scopes were removed. The real desktop session was never stopped by this
  experiment. Script, logs, and JSON results are in
  `/tmp/chonkstep-logout-review/`.
- `scripts/check.sh`: strict workspace Clippy and rustdoc passed; 2,054 Rust
  tests and 65 Python harness tests passed. The display-dependent tests are
  separate from those counts.
- The existing real-compositor termination test passed. The four Pinta/CSD
  input-region regressions also passed against the fixed optimized release.
- A final strict Clippy pass covers the final regression-test cleanup changes.

An exploratory nested hot-restart test exposed an existing limitation:
`restart_in_place` retains the nested compositor's own `WAYLAND_DISPLAY`
instead of restoring the host's display, so its replacement fails winit
initialization. That separate issue is outside this change. The upgrade
regression explicitly supplies the inherited blocked mask at startup; it
does not claim to validate nested hot restart.

## Deployment

The tested optimized binary has SHA-256
`a5ebb2c27597a761e10193e1b3d7eec79a65647e3ae552ca1e19fd461bb10085`.
The binary was installed atomically after checking both the existing binary
and the staged replacement. The previous binary is backed up in
`/var/lib/chonkstep/local-backups/logout-20260908T061531Z/`.
Installation evidence is in
`~/.local/state/chonkstep/logout-update/install.log`.

A fresh graphical login is required for all existing processes to receive
the corrected behavior. Replacing the on-disk binary does not change the
signal masks of the already running compositor or its old children.
