# Deterministic copy-order regression (#310)

The previous real-browser test deliberately spent 150 ms in a JavaScript
keydown handler while the compositor's copy guard allowed 250 ms. Browser
scheduling, key dispatch and the clipboard offer all had to fit in the remaining
margin. The guard correctly releases input when that deadline expires; a slow
runner could therefore fail the test's later paste assertion. The recorded
[CI failure](https://github.com/iconidentify/chonkstep/actions/runs/36267464273/attempts/1)
timed out waiting for the terminal payload. Passing on rerun did not validate
the timing assumption.

The replacement keeps the real Chromium → Wayland clipboard → foot PTY path:

1. Enable the private browser's debugger and install a Copy keydown handler
   that removes itself before stopping.
2. Freeze only the compositor's copy deadline through the opt-in test socket.
   Inject Copy and wait for the debugger stop. Input delivery, clipboard offers,
   focus changes and rendering remain real.
3. Inject the complete switch/paste burst and observe eight queued physical
   key transitions, unchanged source focus and an empty terminal.
4. Advance to 249 ms and verify that the queue remains intact. Resume Chromium
   without advancing the clock again. Its real offer must release the queue;
   the timeout cannot make this assertion pass.
5. Verify the exact multiline Unicode payload, released Command and physical
   Control reaching the PTY.
6. Copy with no selection, queue a switch and typing, then verify that 249 ms
   preserves the queue and 250 ms releases it. Restore the real clock and also
   exercise an ordinary no-offer timeout.

`MAX_WAIT` remains 250 ms. Clock controls are reachable only through
`CHONKSTEP_TEST_SOCKET`; switching clock modes requires idle copy state.
A frozen pending deadline supplies no wall-clock wakeup, preventing an expired
test Instant from making the event loop spin. Read-only queue observations do
not drain input or satisfy a frame barrier.

## Validation

- The focused headless test passed with normal local settings and with
  `LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe CI=1`.
- Ten consecutive runs passed with those CI settings and all test processes
  pinned to CPU 0 using `taskset -c 0`. Test bodies took about 3.6 seconds each.
  This checks constrained scheduling, not native GPU performance.
- A separately built mutation removed `defer`'s queue insertion and returned
  `false`, allowing later keys through immediately. The test failed in 1.65 s
  with `(pending=true, queued=0)` instead of `(true, 8)`. The working source was
  restored before rebuilding and running the passing repetitions. No mutation
  is included in the change.

Focused command:

```sh
LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe CI=1 \
  taskset -c 0 scripts/e2e.sh --headless --test mac_mode \
  delayed_real_copy_followed_by_one_burst_switch_and_paste_keeps_clipboard_order
```

The strict Clippy and rustdoc gates in `scripts/check.sh all` passed. Its unit
stage initially encountered an unchanged dock test binary with a compiled-in
fixture path pointing at a removed temporary worktree. Rebuilding the affected
local packages from this checkout resolved that artifact-cache problem; no
fixture or unrelated source change was needed.

The remaining preflight gates were then run explicitly:

| Command | Result |
| --- | --- |
| `cargo test --locked --workspace --all-targets` | 2,573 passed; 475 ignored in the ordinary test run |
| `cargo test --locked --workspace --doc` | 6 passed; 7 ignored documentation examples |
| `scripts/check.sh gles` | All 5 compositor and 2 vendored renderer regressions passed |
| `scripts/check.sh harness` | 125 passed |
| `LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=llvmpipe scripts/e2e.sh --headless` | Passed, with the explicit `uwsm` skip below; all 3 installed Omarchy menu/theme tests also passed |
| `git diff --check` | Passed |

The headless suite reported 441 passes. One of those is an early return for
`an_exec_bind_through_uwsm_app_still_hands_its_launch_a_token`: the isolated
session bus has no systemd user manager, so `uwsm app` cannot launch there.
The script explicitly reported that skip. The remaining 440 tests executed,
including all 13 Mac-mode tests, selection transfers, session lock and restore,
XWayland input, and the normal stability smoke workload. This was software
rendering, not a native GPU or sustained release-build soak run.

The pre-publication fetch still found main at `6eb4ab6`, no open pull requests,
and no claim or competing discussion on #310. Validation was completed locally
before publication.
