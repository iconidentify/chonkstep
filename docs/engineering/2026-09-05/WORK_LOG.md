# Overnight engineering campaign — 2026-09-05

Started 2026-09-05 around 22:15 UTC. The original request was to continue for
at least 12 hours; around 06:05 UTC the user instead requested a wrap at the
next logical stopping point, with commit, push and an X-friendly report. No
partially implemented sampler change was started. This is an engineering
record, not a claim that every parity or hardware-validation item is complete.

## Scope and safety

- Baseline: `1e6db21f10c039775d642a1f497a0cd9a71cafdb`, whose PR and main CI
  both passed before this campaign.
- Worktree: `/home/chrisk/src/chonkstep-engineering`; branch
  `codex/overnight-engineering-2026-09-05`.
- Evidence and reference checkouts:
  `/home/chrisk/src/chonkstep-engineering-artifacts/2026-09-05/`.
- Preserve the user's main worktree, untracked notes, live compositor,
  installed packages, real clipboard, browser profile and session settings.
  Test applications in private nested sessions and profiles. No native seat
  takeover, desktop restart, release publication, or hardware reconfiguration.
- No unmeasured performance claims. Distinguish renderer/library measurements
  from whole-process or session memory; retain regressions and tradeoffs.
- Work in reviewable changes with reproductions, regression tests, strict
  debug-profile preflight and proportionate release/integration checks.
  Do not bypass required CI checks or convert failures into retries/skips.

## Priorities and progress

- [x] Start explicit long-running goal and isolated worktree.
- [x] Freeze baseline executable and record repeatable startup/idle samples.
- [x] Reproduce/fix native Edge selection at 1x/1.5x/2x with real held drags;
  preserve native Chromium, root/subsurface and touch regression evidence.
- [ ] Extend browser/input coverage to XWayland and mixed-output transitions.
- [x] Real native-toolkit/X11/compositor held-key regressions, no-op/repeat/map
  reload behavior, zero repeat and modal focus ownership.
- [ ] Complete repeat/input ownership coverage across locks, exclusive layers,
  shell focus grabs and active input-method composition.
- [x] Sixteen real clipboard/primary transfer, cancellation, access and lifetime
  cases pass; preserve exact payloads and before/after retention measurements.
- [ ] Exercise clipboard and primary selection across Wayland/X11, UTF-8 and
  binary MIME types, large transfers, owner changes/death, cancellation and lock
  boundaries. Keep clipboard persistence distinct from live transfer support.
- [ ] Exercise Chonkcraft/fullscreen gaming input, relative/locked/confined
  pointers, coordinate conversion, frame pacing and supported scanout paths.
- [ ] Research primary-source compositor behavior and maintain a feature matrix
  distinguishing advertised, implemented, integration-tested and hardware-tested.
- [ ] Organize large Rust modules along tested ownership/coordinate/protocol
  boundaries; preserve behavior and avoid cosmetic whole-tree churn.
- [ ] Measure and improve startup, retained/transient memory, idle CPU and
  rendering work; repeat baselines under matched conditions.
- [ ] Run extended stability/compatibility loops and required local/CI gates;
  produce a final evidence-backed report with unresolved limitations.

## Primary references

- [Wayland core specification](https://wayland.freedesktop.org/docs/html/apa.html)
- [Smithay 0.7.0](https://github.com/Smithay/smithay/tree/v0.7.0), the dependency
  actually pinned in ChonkStep, plus its locally installed source.
- [Niri](https://github.com/niri-wm/niri)
- [Hyprland](https://github.com/hyprwm/Hyprland)
- [Sway](https://github.com/swaywm/sway)
- [Chonkcraft](https://github.com/iconidentify/chonkcraft), found through the
  project's GitHub organization; confirm the user's desired build when possible.

Record exact upstream revisions before deriving implementation comparisons;
do not use marketing lists as proof of exercised behavior.

## Observations and experiments

### Baseline capture and scaled-drag reproduction

- Preserved release binary: `baseline/bin/chonkstep-wayland` in the artifact
  directory; SHA-256
  `9277565dd563769110559caf00fcbd06701f6382e7fd4317e2b870bb97c9cff7`.
- A new real Wayland input probe and integration tests run against that exact
  executable. Unscaled drag passes. At 1.5x, expected `(120, 90)` arrives as
  `(140, 100)`; at 2x it arrives as `(160, 110)`. Hover coordinates pass at
  every scale. The starting position was `(80, 70)`, confirming the error
  multiplies drag deltas by the scale. Raw result: `baseline/pointer-coordinates.log`.
  This confirms a compositor bug consistent with the Edge report, not yet an
  Edge-specific reproduction.
- Seven 60-second hidden-Dock baseline samples run from the preserved binary;
  short socket paths require `/tmp/chonk-eng-bench.evcwgb/hidden` while running.
  Archive the completed directory under baseline evidence. No build/test stress
  during sampling; ordinary live-desktop activity remains uncontrolled.
- Default `cargo fmt --all -- --check` reports extensive pre-existing formatting
  differences. It is not currently a shared CI gate. No whole-tree formatting
  changes made; new files use explicit 120-column formatting matching nearby code.

### Pinned reference checkouts

| Reference | Revision | Declared license |
| --- | --- | --- |
| Niri | `dd75865f547f0eac0e9b6c4d86d2cd00c0744252` | GPL-3.0 |
| Hyprland | `ab136393c2eb9e106846a704da1a1b3d6af415b4` | BSD-3-Clause |
| Sway | `5bc72dee4771a2d2d2648b8f69d30e0747f263f6` | MIT |
| Chonkcraft | `fe7c787c936bd7c44647baf8e3b3ae063a7889ec` | GPL-2.0-only |

These are research references, not imported implementations. Chonkcraft uses
Java2D and SDL input via its shared runtime, not the assumed GLFW/LWJGL path.
Its full game requires the player's authenticated asset pack; no downloaded
proprietary game assets are part of this investigation.

### First coordinate correction and dependency lifecycle finding

- `input/surface.rs` now owns the full affine surface transform and typed
  pointer/touch focus. Smithay grabs retain enough information to deliver
  subsequent motion in client units. Data-device DnD retains its separately
  tested current-origin contract; relative/scroll/gesture streams are unchanged.
- Root/subsurface pointer tests at 1x/1.5x/2x pass, including motion outside the
  surface, native DnD action negotiation/drop and restored pointer focus.
- The same matrix with two touch slots found an additional dependency bug:
  Smithay 0.7's cancellation applies frame de-duplication, skips already framed
  touches and retains stale focus. All six touch cases initially timed out on
  cancellation, despite correct preceding coordinates (`input-touch-coordinates.log`).
- Vendored the checksum-verified published Smithay 0.7 crate with a narrow
  cancellation fix that drains slot state and handles pending final frames.
  Provenance, original license and patch details are in `vendor/README.md`.
  Upstream's later touch-API rework is a separate upgrade, not silently pulled in.
  No registry-cache edits. Other dependency versions/checksums are unchanged.
- All 12 root/subsurface mouse and touch cases now pass (6.51 seconds), including
  independent fingers, cancellation, ignoring stale contact motion and ID reuse.
  Raw result: `input-touch-fixed.log`. Four coordinate math tests also pass.
- Full shared preflight passed (strict Clippy and docs, debug workspace/Wayland
  tests, 21 Python harness tests). A final rerun follows mechanical vendor warning
  cleanup; the complete software-rendered nested suite is running as well.
- Final warning-free preflight completed successfully. The full release nested
  suite passed: 123 nested cases plus three installed-Omarchy unit cases
  (`input-full-e2e.log`). Preserve this input-only candidate separately from
  subsequent keyboard work: SHA-256
  `611dad5eea733af31f4bebba1ee3c86867199d5eadd7aaaef9f9168bb7e4ccd1`.
- Seven alternating baseline/input-candidate pairs with 60-second idle windows
  are running under `/tmp/chonk-eng-input.V0FO2Z/hidden`; do not build or run
  additional compositor sessions during their measurement windows.
- Initial baseline medians: first scene 262.57 ms (261.40–284.62), RSS 180,104 KiB,
  PSS 123,155 KiB, private 109,248 KiB, tree PSS 150,836 KiB, 40 threads, 43 FDs,
  idle 0.0833% of one CPU and 12.13 context switches/second. Seven 60-second
  samples on the same private llvmpipe setup; this is not an improvement claim.
- Chonkcraft's configured authenticated asset pack was located through
  `CHONKCRAFT_ASSET_PACK`, and its pinned Linux JBR 25 SDK is already installed.
  Use read-only pack access and an isolated `CHONKCRAFT_HOME`; do not touch the
  player's library, saves, profile or running game. A real game run is pending.
- Chonkcraft's reference build succeeded using its pinned installed JBR 25,
  private Maven repository and private profile path (`chonkcraft-build.log`).
  This was compilation with tests skipped, not a game-play or test-suite result.
- Further isolation audit: `CHONKCRAFT_HOME` redirects the launcher library,
  but desktop `Settings.defaultFile()` still uses Java `user.home`. A game run
  must additionally set `-Duser.home` to private scratch. Bubblewrap's read-only
  root, private `/dev`, and network/PID namespaces work on this host; use them
  to prevent game audio/controller access or writes to real settings/saves.

### Next keyboard investigations

Source observations, not yet fixed or attributed to the reported symptom:

- The Hyprland parser rejects repeat rate/delay zero, though Wayland permits
  nonnegative values and zero rate disables repeat.
- Compositor binding repeat clamps rate to at least one; modal key grabs do not
  repeat ordinary navigation keys unless they are separately marked `binde`.
- `apply_pending_keyboard` recompiles the keymap for every pending
  configuration, including startup's duplicate application. Investigate
  avoiding unchanged work without breaking virtual-keyboard keymap restoration,
  held keys, layout state or failed-configuration rollback.
- Eight real-session keyboard tests are prepared: native wire settings,
  unchanged/timing-only/invalid-keymap reloads, modifier release, disabling an
  existing binding timer, real Foot repeat and real XWayland repeat. They have
  not yet run; compilation remains paused for the paired input measurement.
- A keyboard configuration module now separates successful installed settings,
  keymap changes and timing changes. New deterministic unit tests inject an
  environment reader; they no longer assume the test runner has no XKB defaults.
  Production changes are pending baseline reproduction and validation.

### Keyboard baseline and newly confirmed compatibility gaps

- The finalized initial keyboard matrix uses a real version-7 native client,
  a separate version-5 legacy case, Foot, XWayland and wtype. Against the exact
  unchanged baseline: four pass and seven fail (`keyboard-baseline.log`).
  Passes: real Foot repeat, rejected-layout recovery, physical keymap restoration
  after virtual typing, and unchanged-reload wire-map count. Smithay already
  suppresses an identical map on the wire even though ChonkStep recompiles it;
  do not claim the no-op reload test reproduced a client-visible reset.
- Confirmed failures: X11's actual input focus never reaches the active test
  window; repeat rate/delay zero; disabling an existing binding timer; released
  shortcut modifier; input-only Hyprland configuration discarded; legacy
  keyboard keymap missing its required NUL. The timing-only case fails on zero
  delay in the baseline, not proof of a redundant wire keymap.
- Keyboard focus now retains the X11 window identity and delegates ICCCM focus
  to Smithay's X11 target. A bare backing wl_surface bypassed SetInputFocus and
  WM_TAKE_FOCUS entirely. All focus consumers still extract the native surface
  for clipboard, IME, shortcut inhibition, lock and cursor-owner bookkeeping.
- The Hyprland read's `is_empty` now covers every semantic category, not only
  press bindings/env/autostart/float rules. Input, monitor, release-only and
  layer configurations cannot vanish for lacking a normal press binding.
- A narrow second dependency correction terminates unsealed keyboard keymaps
  with NUL, matching the sealed path and core protocol. See `vendor/README.md`.
- New probe development exposed two test errors (reading an unspecified fd
  offset and assuming every keymap was terminated). Read with positional I/O;
  report termination independently so a legacy failure does not abort unrelated
  coordinate/repeat tests. Intermediate logs are retained and are not counted
  as the finalized baseline matrix.
- Empty XKB_DEFAULT_* values in the test environment also prevented libxkbcommon
  from initializing. The harness now removes inherited XKB defaults, rather
  than replacing them with empty strings; tests can explicitly override them.
  Actual compositor resilience to empty/invalid environment remains follow-up.
- Debug tests pass: 154 wm-config and 233 wm-wayland, with their two existing
  ignored tests unchanged (`keyboard-unit-full.log`). The release candidate
  keyboard matrix is building/running; no after-result is claimed yet.

### Keyboard validation, focus ordering and an X11 lifetime leak

- The first keyboard candidate passes the initial 11 tests and three installed
  Omarchy checks (`keyboard-candidate.log`). Preserve its executable separately:
  `dc5f89bc2dcb5d19ffa6f81d4da8906bb02d933949d027406636f435f7e05cb4`.
- A stronger reload case proves that the baseline resets Caps Lock and the
  selected XKB layout despite suppressing the unchanged wire keymap. The test
  observes modifier/group state after a subsequent physical key, not merely a
  map count. Baseline fails `(0, 0)` versus retained `(2, 1)`; candidate passes
  both unchanged and timing-only reload (`keyboard-locked-state-baseline.log`).
- The expanded 12-case run exposed an intermittent X11 focus failure even with
  the typed wrapper: association can precede first-commit reverse-indexing.
  Focus now takes its X11 identity directly from the known managed record.
  Equality includes that identity rather than silently treating a native-only
  target as the same X11 target. No per-key scan or allocation was introduced.
- This focus-order candidate passes all 12 cases plus installed Omarchy checks
  and the complete strict preflight (`keyboard-focus-order-fixed.log`,
  `keyboard-focus-order-preflight.log`). SHA-256:
  `7f2c7e61f85d815878d058d8954c410c8b5e6821c7b69fcde75dd12b59cedd7e`.
  This is not yet full nested-suite validation of the keyboard changes.
- A new required 30-window map/focus/first-key/destroy sequence then exposed
  a real X11 teardown leak in both keyboard candidates. Smithay marks an
  X11Surface dead before its destroy callback; its PartialEq rejects dead
  handles, so ChonkStep's lookup cannot find and collect the record. Match
  `(XwmId, XID)` instead, preserving distinct XWayland server generations.
  Failure evidence: `keyboard-map-order-before.log` and
  `keyboard-focus-order-expanded.log`. Fixed validation is pending.
- Add independent real-X11 coverage for ten withdraw/remap cycles preserving
  compositor identity and for all twelve windows retiring on client connection
  exit. Unmapping must retain a reusable record; destroying must collect it.
- The unchanged baseline's client-exit test retains all twelve destroyed X11
  records with `mapped=false` and no frames: confirmed accumulation, not a
  live-window assertion error (`x11-lifecycle-exit-baseline.log`, raw case in
  `/tmp/chonk-x11-before.iy4k0y/chonk-testkit/x11-client-exit`).
- The lifecycle candidate passes all thirteen keyboard cases, the full nested
  integration suite including both new X11 lifecycle cases, installed Omarchy
  checks, and strict preflight (`x11-lifecycle-keyboard.log`,
  `x11-lifecycle-full-e2e.log`, `x11-lifecycle-preflight.log`). Its SHA-256:
  `e055913e2f040a3a37fa0e872abacfc3618e3d3c0a54233f09d5d01fbac05d2b`.
- First two-hour sustained desktop-churn run started around 2026-09-06 00:02 UTC
  against that immutable binary. Log: `soak-01-x11-lifecycle.log`; independent
  temporary root `/tmp/chonk-soak1.38qWks`. Ongoing development/tests may share
  the host: this is resource-lifetime evidence, not an idle CPU/startup sample.
- Three new modal keyboard tests are being run separately against this
  candidate; they were added after the full suite compiled and are **not**
  included in its green result. They cover Overview/Alt-Tab withdrawing client
  focus and same-window restoration, plus held modal navigation repeat.

### Modal keyboard ownership and empty-workspace restoration

- All three initial modal cases fail on the lifecycle candidate
  (`modal-keyboard-before.log`): no client leave on Overview/Alt-Tab, and no
  held Overview navigation repeat. Modal focus transitions now reach the seat
  independently of window-focus intents, so Escape returning to the same
  window still restores input. Exclusive layers and shell focus grabs keep
  their higher-priority role in both routing and restoration.
- Focus-only candidate `b566066e9226baaec4584b26c6930d15522a5447681c4dc45cc505e22a16845f`
  passes both focus-handoff cases (`modal-focus-fixed.log`).
- Repeat scheduling moved out of `input.rs` into `input/keyboard/repeat.rs`.
  Each hold has explicit binding/modal ownership, a bounded catch-up burst,
  and the existing event-loop deadline rather than another timer thread.
  The opening Alt+Tab hold is armed when its modal becomes active; repeat is
  not added to a user's ordinary launcher binding. New presses, modifier loss,
  modal closure, zero rate and lost privileges retire the old hold.
- Smithay 0.7 omits the documented focus-change callback when focus becomes
  empty. A deterministic unit reproduces `[true, true]` instead of
  `[true, false, true]`; a one-line vendored correction passes. This affects
  dependent selection-offer focus and shortcut inhibition, **not** Smithay's
  separate text-input enter/leave machinery. See `vendor/README.md`.
- The initial text-input probe had no input-method service, so absence of
  initial enter was a fixture error (`text-input-missing-test-ime.log`). With
  a real private input-method-v2 peer, both keyboard and text-input leave
  correctly. Returning from the empty workspace then restores neither:
  the shared window-manager successor branch only runs when an old focus
  exists. Reproduced on both focus-only and modal-repeat candidates and in a
  deterministic wm-core test (`empty-workspace-focus-before.log`).
- The shared workspace fix selects the destination's most recent eligible
  window when the old workspace had none. `focus_successor` also stops building
  an entire cycle vector and de-duplication set merely to select one result;
  it scans the existing history and creation-order fallback without allocation.
- Modal-repeat candidate `15a313828ff24dd78129052262cd862f646d7bc6bc3b0b934989b2a04c2de503`
  passes Overview handoff/repeat, Alt-Tab cancel and first-hold repeat. The
  expanded run still fails the newly exposed empty-workspace restoration.
  A zero-rate fixture first used unsupported native TOML keyboard settings,
  then omitted an explicit Overview binding after enabling Hyprland import;
  those fixture errors were corrected, not treated as product failures.
  Final zero-rate modal test passes (`modal-zero-repeat-fixed.log`).
- Debug wm-wayland tests pass (236 plus one existing ignored case), and the
  modal-repeat strict preflight passes. Empty-workspace debug tests pass;
  its release build and expanded real-client matrix are pending.
- Added a positive real-protocol test for the missing callback: an inhibited
  client minimizes itself and must receive inhibitor-inactive with keyboard
  leave. Before/after execution is pending. Do not claim clipboard payload or
  full IME editing support from these focus-only cases.

### Expanded focus candidate and browser evidence (2026-09-06 UTC)

- The empty-workspace/focus candidate is now validated: seven modal, text-input
  focus and shortcut-inhibition cases pass, the full nested suite passes 145
  cases plus three installed-Omarchy checks, and strict preflight passes.
  Immutable binary SHA-256:
  `77a35addc6c4ce771083a7e26eb6c110c0e438b6f70a51304024b301f3e90260`.
  Logs: `workspace-focus-keyboard.log`, `workspace-focus-full-e2e.log` and
  `workspace-focus-preflight.log` in the campaign artifact directory.
- The inhibitor case fails positively on the focus-only candidate: keyboard
  leave arrives but inhibitor-inactive does not (`shortcut-inhibitor-before.log`).
  The vendor callback correction passes the same real-protocol case.
- Added browser-level probes for selection and held-arrow repeat, driven through
  the compositor's test input door. DevTools only observes the private fixture's
  DOM and sets up its own editor/fullscreen state; CDP input injection is forbidden.
  Runs use private profiles, XDG config/cache/data/state and a file-only CSP.
- The first Edge 152.0.4191.53 attempt failed before attaching to the fixture:
  its DevTools discovery connection remained open and the harness waited for
  EOF. Corrected the bounded reader to use Content-Length and added unit coverage
  for persistent connections, ambiguous/oversized responses and truncated bodies.
  This is a harness defect, not an Edge selection reproduction. Its log is
  preserved as `edge-discovery-fixture-before.log`; browser before/after results
  remain pending.
- At 46 minutes the first two-hour churn run has completed roughly 15,000
  native-window lifecycle cycles. Non-pipe descriptors remain 40 and threads
  remain 59; anonymous PSS has grown from about 62,400 to 78,500 KiB. This is
  provisional drift evidence, not a completed soak or an identified leak.

### Browser completion and protocol module boundaries (around 01:00 UTC)

- Final Edge baseline run passes 1x and fails 1.5x/2x exactly at the scaled
  drag endpoint. The unchanged fixture passes all three scales on the frozen
  workspace-focus candidate: twelve selections across plain/editable text and
  windowed/fullscreen, plus three held-arrow checks. Chromium passes the same
  matrix. See `MEASUREMENTS.md` for coordinates, hashes and evidence locations.
- The browser test also needed real dismissal of Edge's native mini-menu
  between selections; the wire trace showed correct compositor hover delivery
  while the menu owned browser input. This was not another compositor fix.
  Pixel verification now samples outside the temporary fullscreen-exit banner.
- Moved the seat's focus-dependent grants from `xdg.rs` into `input/seat.rs`,
  and selection forwarding into `selection.rs`, keeping protocol delegation
  with each owner. No behavioral change was intended. Strict Clippy for
  wm-wayland and all testkit targets passes; a new release/full-suite run is
  still required before treating this source organization as validated.

### Clipboard transfer matrix (in progress, around 01:15 UTC)

- Seat/selection extraction builds with immutable executable SHA-256
  `b05b41849c7349dbb3be778111d8e1a13de0afb94646fc4cf0ae88867f712d4c`.
  Native focused-client clipboard/primary tests pass UTF-8, binary/NUL and
  1 MiB + 17 byte payloads, both directions through owner replacement,
  cancellation, and clearing one selection independently of the other.
- New X11 fixture implements real TARGETS and INCR property-delete handshakes,
  not an in-memory approximation. Initial sender wrote a nonblocking protocol
  FD with write_all and stopped at EAGAIN: fixture defect, corrected with
  deadline-bounded polling and partial-write accounting. Native receivers now
  await the replacement offer after cancellation, not an earlier focus offer.
- Steady-state six-case matrix currently passes all three native cases and
  bidirectional X11 UTF-8; binary and large X11-to-Wayland transfers return
  zero bytes with `UnableToDetermineAtom`. Smithay reads a nonexistent TARGETS
  property after its actual _WL_SELECTION target list has been consumed.
  A narrow MIME-atom lookup correction is building; no successful correction
  claim yet. Logs: `selection-bridge-initial.log`, `selection-bridge-steady.log`.
- Separate startup evidence suggests a native copy before XWM readiness is
  never mirrored later. Added a private PATH shim that gates the real XWayland
  executable on a fixture marker, permitting a deterministic test without a
  delay hook in production code. Baseline execution is pending.

- The gated startup regression now fails positively on the refactor candidate
  (`selection-startup-before.log`): a native owner predates XWM readiness and
  the subsequent X11 conversion is refused. A read-only live-source accessor
  lets the ready callback reconcile authoritative seat state without keeping
  another selection cache. The live/cleared/exited startup cases all pass on
  candidate `869beb5535fa0280c21fb1183b06f4228d9e4360bdd329bd16786f384a8d9bed`.
- MIME-only candidate
  `a33dd48bdf95b8fc5fa3111ec9593a1b78f4befa812e906f5d6f53392794f17d`
  passes small UTF-8 and binary bridging. Large X11-to-Wayland payloads reveal
  another defect: 65,540 received bytes instead of 1,048,603, including four
  bytes from the INCR size header. The initial hint was treated as content;
  chunk properties were deleted on read and then acknowledged again after
  writing, letting an owner overwrite unread chunks.
- Removing that header and double acknowledgement leaves the transfer waiting
  for an unflushed X request from the writable-FD callback. This is reproducible
  without unrelated X event traffic (`selection-incr-startup-candidate.log`:
  eight cases pass, the large case stalls). Explicit acknowledgement flushing
  is building; the full large-transfer correction is not yet validated.

### Clipboard integrity and bounded staging (around 01:45 UTC)

- Explicitly flushing the INCR acknowledgement resolves the quiet-connection
  stall: all nine clipboard cases and three Omarchy command cases pass on
  immutable `selection-incr-flush`, SHA-256
  `f907cf2f283d4db925ac9204ce717ac4b708949fe4d3226f2ba3e0e775171f9a`.
  The full exact payload now crosses the real X11 INCR protocol in both
  directions for clipboard and primary selection.
- An additional stalled-consumer test exposes unbounded Wayland-to-X11
  buffering. Before the fix, the producer completes an 8 MiB + 17 byte send
  while its X11 consumer has acknowledged no chunks. Resume still validates
  the full payload before reporting that failed backpressure policy.
- Bounded staging suspends the readable source after 64 KiB and resumes on
  property acknowledgement, preserving cancellation tokens and reusing the
  buffer. Immutable `selection-backpressure`, SHA-256
  `1060a50ebad9d0be872078b25500dad95140b2b0cd7cdc95a1a0b917c717d8b0`,
  passes all 11 clipboard and three Omarchy cases, including cancellation
  followed by a successful fresh paste. Full-suite/preflight revalidation of
  this exact source remains pending.
- The first retention sample is 8,204 KiB added anonymous PSS before versus
  8 KiB after. Seven matched pairs are running before publication. These are
  transfer-path samples, not whole-desktop memory claims; the independent
  lifecycle soak remains active. The first pair launcher used an overlong
  Unix socket path (115 bytes, kernel limit 107); shortening its private
  temporary root fixes the fixture, with the original log preserved as
  `clipboard-pairs-path-fixture-before.log`.

### Clipboard lifetime follow-through (around 02:00 UTC)

- Seven alternating stalled-paste pairs complete: every before sample adds
  8,196 KiB anonymous PSS, every after sample adds 0 KiB at page-accounting
  granularity; every resumed payload is exact. See MEASUREMENTS.md for scope
  and binary identities. This is not zero-allocation or desktop-RAM evidence.
- New tests positively reproduce pending-pipe leaks when one child requestor
  holds both selection kinds, and when the private XWayland server is killed.
  The requestor need not be a managed root child: subscribe to its destruction
  explicitly while preserving existing event masks, then clean both kinds.
  Before handing disconnect to the compositor, retire all transfer sources.
- The first disconnect candidate (`2a0b45dd10eff7ba9726b57eee00f07ccd8a83bde7f0b68439a3abb56bf42e89`)
  releases both blocked senders but the replacement's first paste is refused.
  Source/trace inspection identifies another first-commit ordering boundary:
  the access check re-derives XWM identity from a reverse index populated after
  keyboard focus is already delivered. Use the typed focus target's live XWM
  generation instead. A negative unfocused-access case accompanies the change.
- A dead X11 owner's offers also survive restart on the before binary.
  Clear only compositor/bridge-owned selections at loss; native ownership is
  authoritative and preserved. The interim lifetime candidate
  `cb9167b531ee1a7fdd3422ce274e884ce7961ed7026437c0dfe943fae4fb12fe`
  passes 13/14 cases: cancellation and dead-offer withdrawal pass, while the
  replacement first-paste access failure remains.
- Error-path Debug formatting previously dumped clipboard bytes to logs.
  It now reports lengths only. The synthetic regression logs retain the before
  evidence; no live user clipboard has been read or logged by these tests.
- A new completed-transfer stress case leaves 128 requestor child windows
  alive after exact 60 KiB pastes: the interim binary retains 7,884 KiB.
  Removing completed non-INCR transfer records immediately is building/testing
  with the focus-access fix; no after result is claimed yet.
- Harness regression: reaping a child reduced `clients.len()`, causing later
  launches to overwrite `client-0-...log`. The test reads `third launch` where
  `first launch` must remain. A monotonic identity and create-new logs preserve
  all attempts independently of live-child count. Validation is in progress.

- Combined candidate `selection-completion`, SHA-256
  `3885badf82399e55a2e8fe35124dfc2147774b3bc95907b09d281c2337e1e6a9`,
  passes all 16 clipboard cases plus three Omarchy command cases in
  `selection-completion-candidate.log`. Restart now releases both pending
  producers, accepts the first fresh paste without copying again, withdraws
  dead X11-owned offers, and still denies unfocused X11 clipboard/primary reads.
  The 128 completed-paste case adds 340 KiB versus the before 7,884 KiB in the
  first samples (not a repeated desktop-RAM benchmark). Strict full
  `scripts/check.sh all` passes on this source; complete nested suite started
  at around 02:01 UTC. The first focus helper build needed its XwmId import
  corrected to Smithay's `xwayland::xwm` module; no stale binary was used to
  claim the corrected access check.

- The first complete suite catches an old `layer_bar.rs` fixture relying on
  log truncation: it reads the first bar's old `mapped` message when waiting
  for the replacement bar, then checks the compositor before that replacement
  has committed. Fix that dependency to read `Session::client_log` for the
  newest launch, and retain `selection-completion-full-e2e.log` plus
  `selection-completion/layer-log-fixture-before/`. The other hard-coded log
  references were inspected: they name intentionally distinct first/second
  launches without intervening reaping. The full suite is rerunning from the
  beginning with the corrected fixture, not retrying away a failing assertion.
- The two-hour lifecycle workload completes at around 02:03 UTC: 35,122
  cycles, 24,464 KiB added anonymous PSS, unchanged non-pipe FD/thread/window/
  frame/shell counts. The broad growth gate passes, but memory attribution is
  unresolved; the next diagnostic run must distinguish live Rust allocations,
  font caches, C/renderer allocations and allocator-retained free space.
- The corrected full suite passes at around 02:10 UTC: 165 nested cases plus
  all three installed-Omarchy cases, including the new launch-log regression
  and the real Chromium scale matrix. Its log is
  `selection-completion-full-e2e-final.log`. The amended layer fixture also
  passes all strict Clippy flags (`layer-log-fixture-clippy.log`). Main/origin
  remains `1e6db21` as checked via ls-remote; the user's main worktree still has
  only its original untracked notes.
- All campaign builds and soak workloads stop for a new seven-pair baseline
  versus `selection-completion` startup/idle/RAM batch, hidden Dock, 60-second
  samples after three-second settling. Raw working root `/tmp/cf.cIeUk8/run`,
  log `clipboard-checkpoint-paired-benchmark.log`. The script has no executable
  bit; the first direct launch returned permission denied before any sample.
  Running the existing harness with python3 starts the actual experiment.

- The seven-pair clipboard checkpoint benchmark finishes successfully. Median
  first-scene time is 276.57 → 259.19 ms, idle CPU is unchanged at 0.0833% of one
  core, and median PSS is 123,282 → 123,766 KiB. Anonymous PSS is 57,052 →
  57,232 KiB and is unchanged within every 60-second sample. Overlapping ranges
  and the earlier baseline median prevent a general startup-gain claim. Record
  the small memory increase rather than advertising idle-memory savings. Raw
  evidence is archived in `clipboard-checkpoint-paired-hidden/`; full values
  and limitations are in MEASUREMENTS.md.
- While that quiet batch ran, prepare (without compiling) an opt-in
  `memory-profile` feature: allocation-free Rust allocation counters, reported
  glibc allocator totals, and non-evicting font-cache payload accounting, exposed
  only through the private test door. No allocation contents or pointers are
  recorded; default binaries install no profiling allocator. Start feature
  validation only after all benchmark samples finish. Feature Clippy passes;
  unit and runtime validation remain underway at 02:32 UTC.

- The diagnostic executable builds successfully, SHA-256
  `18c7bbd9efb00ab0fd5d54082892bb73585ec4012548ae3269a4f9b7d634e4c8`,
  preserved under `memory-profile/bin/`. Five new Rust accounting/cache/parser
  unit tests and strict testkit Clippy pass; Python harness guards pass 23 cases,
  including rejecting instrumented timing binaries. The diagnostic binary
  reports its marker in --version; default builds do not install its allocator.
- The first diagnostic smoke run has valid nonzero allocation counters, but
  was launched without the earlier soak's explicit software-renderer variables.
  It ultimately renders with llvmpipe after probing NVIDIA and retains 26 extra
  driver descriptors from startup. Stop this private compositor deliberately
  after roughly 660 cycles; its resulting test failure is operator cancellation,
  not a spontaneous product failure. Preserve `memory-profile-soak.log` and
  `/tmp/cmp.dsJ6H2/chonk-testkit/stability-soak/`. Start a fresh two-hour run with
  LIBGL_ALWAYS_SOFTWARE=1 and GALLIUM_DRIVER=llvmpipe, log
  `memory-profile-soak-software.log`. Never compare the mismatched startup totals
  as a profiling-induced leak or memory regression.

- Add explicit isolated X11 launching to the test harness, using the newest
  announced private XWayland display and removing inherited Wayland handles.
  A regression verifies the selected transport and private XDG directories;
  per-launch logs remain monotonic. Compiler subprocesses for the game observer
  are owned by Session and bounded by its polling/cleanup contract.
- Real Chonkcraft menu/geometry coverage passes all three scale cases against
  both unchanged baseline and `selection-completion`: 12 menu actions per
  binary, windowed/fullscreen, exact AWT press/release coordinates and window
  restoration. This report is coverage, not a new game-click fix. Initial
  sandbox-target and frame-ledger fixture mistakes are retained in their logs.
  No game source or licensed assets are added to the repository. The actual
  fullscreen screenshot is visually inspected; all evidence is archived.
- Organize XWayland process startup/restart into `xwayland/lifecycle.rs`, group
  generation-owned XWM/display/XSETTINGS/EWMH connections under one State, and
  move toolkit-settings publication to `xwayland/settings.rs`. State.rs drops
  roughly 330 lines from this move without new threads, wakeups or policy.
  Initial private-method visibility compile error is fixed at the new module
  boundary. Strict Clippy and the complete shared preflight pass. Preserved
  default executable SHA-256
  `e0cdf2bf5c76b93f591f7a0e96b1bc486b7a4546fa550d3cfda6602fe2b6106b`,
  full nested suite (including explicit real game inputs) in progress at 03:00.
- Memory attribution: default-GPU-probing smoke is archived separately; the
  corrected software diagnostic run remains active, stable 40 non-pipe FDs,
  with repeated glyph eviction visible in the counters. Rust live allocation
  floors still need attribution beyond glyph payloads. Retrieve heaptrack
  1.5.0-11 from the configured distribution mirror into the artifact directory,
  verify its packager signature against the shipped Arch keyring, and extract
  it locally (no package installation or system changes). Package SHA-256
  `9b49c1e7b4accf9a02b558e9cecc0dd4a177342bf4da59583d791e505e153c8a`;
  signature is Christian Heusel's F00B96D15228013FFC9C9D0393B11DAA4C197E3D.
  A separate 300-second private heap trace uses the preserved diagnostic binary;
  its overhead and profiler allocations make it unsuitable for RAM/timing
  comparison. Log `heaptrack-soak-300.log`. No attachment to a live user process.

### XWayland restart association and memory attribution (around 03:25 UTC)

- The `xwayland-organization` full suite stopped at clipboard: 15/16 transfer
  cases passed, but the first native-to-X11 paste after restart was refused.
  Strict preflight had passed. Trace showed a focused new-generation X11 window
  carrying a dead old-generation Wayland surface. The new focus-liveness guard
  correctly denied it; the earlier clipboard-only test masked this identity bug.
- Smithay 0.7's xwayland-shell lookup used only the X server's reusable serial
  and never removed destroyed surfaces. Keyed it by `(XwmId, serial)`, added
  exact-key destruction cleanup/liveness checks, and stopped processing the
  association on every frame. The compatibility serial-only API is retained.
- Strengthened the real restart regression to require the first actual X11 key
  press/release as well as both exact clipboard payloads. Preserved
  `selection-completion` SHA `3885badf...` fails with `observed []` after restart;
  candidate SHA `0a0efd72a7ef52b0819edaa5b15e38bbb91f9f6196bf72ababc999252c967983`
  passes. Logs `xwayland-first-key-{baseline,candidate}.log`; strict workspace
  Clippy passes in `xwayland-serial-clippy.log`. Full suite then passes in
  `xwayland-serial-full-e2e.log`, including the explicit three real-game cases,
  all sixteen clipboard cases and the three installed-Omarchy checks. Raw
  suite directory `/tmp/cxsuite.hTQcmo/chonk-testkit/`.
- Heaptrack's five-minute recording completed and was interpreted with local
  symbols. `heaptrack-soak-300-report.log` labels 74.83 MB as “leaked”, but the
  harness terminates the compositor: that number includes ordinary live state,
  fonts and software-renderer allocations and is NOT a demonstrated leak size.
  A concrete lead is 1,386 retained allocations each at data-device and primary
  device creation, matching exited client count. Source inspection confirms
  explicit destruction cleans the seat list, but connection teardown does not.
  Upstream fix `712d565b4b8bf34a136f42b39fcbf729ab635d3f` addresses all four
  selection-device protocols. Preparing a focused backport and object-lifetime
  regression before attributing the long soak's entire memory drift to it.

- Preserve the read-only ledger-observer executable before backporting cleanup:
  SHA `b7150e74f2e5287cbba1d84ad3698bc2fe0eb7f0e137428cf1a6b3cfac661ec5`.
  The five-case lifecycle suite fails exactly the three disconnect cases:
  normal exit, SIGKILL and legacy core v1 each retain 128 dead devices per kind
  (512 total) after one client. Explicit release and seat-first release pass.
  `selection-lifecycle-baseline.log` preserves the failures; inspection does
  not prune the ledger, so this is an ownership count, not a memory heuristic.
- Backport all four upstream destruction callbacks without changing selection
  access policy. Candidate SHA
  `92d78da89267ee6dcb1aea42f07acdd358a808884b7006cc4b02f0fe4f50ae2e`
  passes all five cases, four batches each: all 10,240 created devices retire,
  with zero dead resources and the exact original counts after every batch.
  `selection-lifecycle-candidate.log`; complete strict shared preflight also
  passes in `selection-device-cleanup-preflight.log`. Preparing a preserved
  allocation-counting version for another two-hour soak; no whole-soak memory
  saving is asserted until that measurement completes.

- The cleanup checkpoint's full suite passes in
  `selection-device-cleanup-full-e2e.log`, including the five new lifetime
  cases, game cases and installed-Omarchy checks. Its allocation-counting
  executable SHA `21d7785a0e91b544b7291e370b685548feed16619736a95809df50c2f1d097c4`
  starts a fresh two-hour soak around 03:36 UTC, logging
  `selection-device-cleanup-soak.log`. The earlier diagnostic baseline continues
  separately. Both use explicit llvmpipe; concurrent correctness/build work
  makes these allocation/lifecycle studies, not CPU or frame-time benchmarks.

### Pointer constraints and invisible render work (around 03:50 UTC)

- Add client-side region/lock requests to an opt-in submodule of the existing
  input probe. Real keys cause requests; display-sync fences confirm server
  processing. Six baseline cases (lock/confine at 1x/1.5x/2x) demonstrate
  immediate activation outside the specified region. Window focus alone is
  insufficient: the protocol requires the intersection with surface input.
- A null-region confinement additionally hangs on the first motion:
  `apply_pointer_constraint` holds Smithay surface state, then its fallback
  hit-test reenters that same mutex. Preserve the bounded failure in
  `pointer-confinement-deadlock-baseline.log`.
- Initial relative-only test sees zero submitted frames, but source inspection
  finds an unconditional dirty mark. Add a separate `render_attempts` counter
  (no additional clock or allocation) and preserve the observation-only binary
  SHA `d4f132ce43003ad551763e5f86e49006d4416af8cc94f65aea1db262e44067a8`.
  It performs exactly 128 attempts for 128 separately observed locked reports,
  despite submitting zero frames: damage tracking only discards the work late.
  `pointer-constraints-observed-baseline.log` now correctly fails this test.
- Separate activation/enforcement into `input/constraints.rs`. Check region and
  visible input geometry before acquiring the constraint mutex; activate after
  delivering entry motion, not while the cursor is still outside. Preserve raw
  relative units. A locked-only routing fast path forwards through Smithay's
  existing grab and skips absolute motion, hover queues, serial creation and
  dirty marking. Candidate SHA
  `a782b6cb9a81f0c6addec2d9642667b81c77df0cfc2e0dacb439f81c55e4e653`
  passes all nine focused cases and strict shared preflight. Both 128 separate
  reports and an 8,192-report burst produce zero render attempts/submissions;
  this is avoided work, not an FPS improvement or measured
  8 kHz hardware latency. The first module-boundary unused import is corrected
  in current source and strict Clippy passes after correction.
- Full integration run `pointer-constraints-full-e2e.log` passes the nine new
  cases, then fails the existing Chromium popup-anchor test. Its real wire trace
  contains `popup_done`, but the requested parent resize is configured back to
  the old 868x558 size. Preserve `/tmp/cpcsuite.FeUnss/chonk-testkit/`; investigate
  staged-versus-sent configure ordering instead of retrying the failure away.

### Pointer transitions and a confirmed resize race (around 04:16 UTC)

- Three new tests fail on the pointer checkpoint: committed lock/confine
  regions excluding a stationary anchor are not reconciled until mouse motion,
  and Overview withdraws keyboard focus but leaves a game's pointer lock active.
  `pointer-transition-baseline.log`: nine pass, three fail. Reconcile focused
  surface constraints after applied commits and pointer ownership on modal
  transitions. Unchanged active locks do not receive synthetic absolute motion
  from scene reconciliation; if release warps a hint, re-hit-test its new point.
- Candidate `pointer-transitions` SHA
  `86e05416aabcbfd01f75ec4ec296b98806a733b6429f9ed1902a9ce862628aba`
  passes all twelve cases plus installed-Omarchy checks; strict workspace
  Clippy passes. `pointer-transition-candidate.log`, raw
  `/tmp/cpctranfix.1VTgR6/chonk-testkit/`. More hint/focus-boundary coverage remains.
- Preserve metadata-tracing-only `resize-observer` SHA
  `6281d6120c8f31043f6c215ece9c27f6a10ffb42c41bf2b5ccf2991d8d31bd73`.
  The existing real-browser test passes 46 launches, then reproduces the race
  on launch 47. Its trace proves 868x558 -> staged 988x638 -> old committed
  868x558 with `client_behind=false, echoes_ask=false, staged=true` -> 868x558.
  `resize-observer-round-47.log`, raw `/tmp/cresizebase.R7KwjD/chonk-testkit/`.
  Repeated passing runs do not negate the preserved failure.
- Add a small real client that commits previously rendered content immediately
  after a private IPC resize reply, before reading its new xdg configure. This
  reproduces the same race in the first round in 0.23 seconds: the first
  configure is 400x300 instead of 520x380. `resize-order-baseline.log`.
  No compositor sleep, scheduling bypass or larger timeout is involved.
- Include staged configure debt in the existing caught-up guard. Candidate
  SHA `843a1c6824f3124ef9f280f0fa37a282c3c718d534fa74df2cd73f74965c3046`
  passes all sixteen controlled rounds, each receiving exactly 520x380.
  `resize-order-candidate.log`; strict shared preflight passes in
  `resize-order-preflight.log`. The full suite finds a separate client-receipt
  test race below, so it is not yet a green complete-suite checkpoint.
- Around 04:19 UTC the baseline diagnostic soak's resident anonymous PSS falls
  from about 80 MiB to 40 MiB, but `/proc/2842253/smaps_rollup` shows about 40 MiB
  in `SwapPss`. That is paging, not freed memory. The running harness preserves
  warmup/final raw smaps already; a read-only 30-second external sampler now
  saves both soak PIDs' rollups to `soak-swap-observations/`. Future soak JSON
  includes SwapPss and combined anonymous+swap memory; its growth guard uses
  the combined value. Rust requested-live accounting is independent of paging.
- Add two display-free tests: paging an unchanged 80,000 KiB footprint half to
  swap still passes, but paging a >64 MiB increase out cannot evade the guard.
  `soak-swap-unit-tests.log`: both pass.

### Fence the client's answer, not only compositor geometry (around 04:28 UTC)

- `resize-order-full-e2e.log` reaches maximize-control and fails an immediate
  client-log assertion after geometry restoration. The log has sent-unset but
  no answer yet; a compositor barrier does not fence a separate client process.
  Preserve `/tmp/cresizeordersuite.ifPhk8/chonk-testkit/` and its artifact copy.
- Add an opt-in 200 ms client-side configure handling delay to the existing
  fullscreen probe. With unchanged compositor SHA `843a1c68...`, this reliably
  fails both original fullscreen and maximize answer assertions while the
  compositor sends the correct state. `configure-client-fence-baseline.log`:
  two pass, two fail. The server is not delayed or modified for this test.
- Poll the actual request's granted answer with the existing ten-second bound,
  rejecting any refusal immediately. Keep the delayed client in these tests so
  a future premature log read fails reliably. All four cases and installed
  Omarchy checks pass in `configure-client-fence-candidate.log`.

### Clipboard owner lifetime and headless runtime (around 04:49 UTC)

- Add slow/gated real native receivers. On the preserved resize-order binary,
  18/19 clipboard cases pass: both selections survive slow reads and consumer
  cancellation, but an X11 producer disappearing during INCR leaves the native
  reader waiting forever. Clearing its offer does not close an existing pipe.
  Preserve `slow-native-selection-baseline.log` and `/tmp/cslowbase.YGG9Ev/`.
- Each incoming transfer now retains its exact source window and subscribes to
  its destruction without replacing existing XWM event subscriptions. Retire
  transfers on producer or proxy destruction, independently of later owners.
  Immutable `incoming-owner-lifetime` SHA
  `345bc187dd5fb06bd8ca2311123e17fe26fad27cb0fbd9d1097fea4d2a77c8d1`
  passes all 19 cases plus installed Omarchy checks. EOF after source death is
  tested as the exact delivered prefix, not a promise to recover lost bytes.
  `incoming-owner-lifetime-candidate.log`; strict workspace Clippy passes.
- The next full suite gets past fullscreen but the real packaged hyprsunset
  0.4.0 aborts with `*** buffer overflow detected ***`. Its private Unix socket
  path is 109 bytes, beyond Linux's 108-byte field including terminator. Pinned
  upstream v0.4.0 source (`25f704346ec22e7623b0873ef8c4573b57ca1512`)
  uses unchecked `strcpy` in `src/IPCSocket.cpp`. No core was available; journal
  checks found no OOM event. This is source/log mechanism evidence, not a
  symbolized backtrace. No installed configuration or user data was touched.
- Keep headless socket runtime in a short private `/tmp/cse.XXXXXX`, independent
  of artifact TMPDIR. A real AF_UNIX-binding stub test fails before with a long
  artifact directory and passes afterward; mode 0700, child status propagation
  and cleanup are verified. All 24 Python tests pass. This bounds display/IPC
  runtime paths, not every possible arbitrarily long test-artifact filename.
- A first new readiness check mistakenly required a log disabled by upstream
  default verbosity: `headless-runtime-real-hyprsunset.log` fails. Replace that
  with an actual connection to the private listener, then launch hyprctl as an
  owned child with bounded exit polling. All seven gamma cases and installed
  Omarchy checks pass in `headless-runtime-real-hyprsunset-fixed.log` with a
  long artifact prefix; strict workspace Clippy passes. No test rerun policy,
  larger timeout, or disabled assertion was used to hide either failure.
- The baseline diagnostic two-hour soak itself passes 29,559 cycles, but its
  enclosing shell exits 127 afterward: I edited `scripts/e2e.sh` while that shell
  was paused inside Cargo, shifting its subsequent read offset. The observed
  `automatically: command not found` is a campaign runner-edit error, not a
  compositor failure or a complete green wrapper result. Preserve both results.
  The already-running candidate soak may hit the same continuation hazard;
  do not interrupt its test, and do not edit the runner while it is executing.
- Baseline memory after settling: Rust requested-live 6,129,619 -> 22,678,479
  bytes; anonymous PSS 62,836 -> 43,668 KiB, with final SwapPss 41,176 KiB.
  Combined anonymous+swap grows 22,008 KiB, **not** a resident-memory saving.
  Glyph payload grows 13,969 -> 1,532,776 bytes; non-pipe FDs remain 40, threads
  59, no live windows/frames. Candidate two-hour run and five-minute heaptrack
  comparison are still running; no whole-heap leak-free claim is justified.

### Incoming buffer allocation and panel freshness (around 05:04 UTC)

- `incoming-owner-lifetime-full-e2e.log` passes the complete suite and installed
  Omarchy checks, including the real game/browser fixtures supplied in its
  environment. Preserve `/tmp/cownerfull.5ivcXi/chonk-testkit/`. This is the first
  completely green checkpoint after the pointer, configure-order and runner
  corrections; earlier full-suite failures remain archived.
- Extract the incoming buffer algorithm into a private display-independent
  dependency helper. The initial equivalent algorithm passes three new tests
  but fails transient-write, zero-write and allocation regressions. For 128
  already-owned 64 KiB replies through controlled 1 KiB writes, allocation
  accounting observes 8,192 additional calls requesting 272,629,760 bytes.
  This is cumulative allocation traffic, **not** 260 MiB of resident memory.
  `incoming-buffer-baseline.log` and preserved baseline helper source.
- Move the owned reply into the buffer and advance an offset on partial writes;
  release drained payloads. Interrupted/would-block writes yield without data
  loss, zero writes fail, fatal errors remain fatal. All six helper tests pass
  with zero additional buffer allocations (`incoming-buffer-candidate.log`).
  Normal workspace CI compiles the exact dependency helper through a path
  module, since an excluded dependency cannot run its dev tests with `-p`.
  No additional shipping dependency, timer or worker is introduced.
- Immutable `incoming-buffer` SHA
  `df49c6d480cc4c235140b9ada9f90ba239d06514eab2486f655b7ce0c9d2505d`
  passes all 19 real selection cases and installed Omarchy checks; strict
  shared preflight also passes after the panel fix below. The first real
  slow-reader heap profile does not actually pressure its large kernel socket
  enough to produce partial writes. Preserve it, but do not use the isolated
  allocation result as a measured whole-compositor clipboard saving.
- Bound the slow/gated fixture's own socket send buffer to 4 KiB. This is a
  legitimate receiver-owned destination FD, not a compositor test hook. The
  next real baseline profile records 288 `split_off` allocation calls while
  delivering both exact 1 MiB+17-byte payloads. Candidate profile interpretation
  is in progress; source/ICCCM request allocations are separate from this count.
- While auditing periodic network allocation work, reproduce panel-only source
  freshness loss: `WifiWidget::update` returns before `LinkPanel::update` if
  no tile source is fresh. A command completion's one-pass freshness is lost.
  `link-panel-freshness-baseline.log` fails with zero rows instead of one.
  Service the panel before the tile freshness guard; panel dirty scheduling
  remains separate from tile repainting. `link-panel-freshness-candidate.log`:
  243 pass, one optional test ignored. No live network settings are changed.

### Omarchy menu identity and startup work (in progress around 05:04 UTC)

- Confirm a stale-action bug in replacement handles: a popup's old generation
  1 resolves the new menu's different command at the same index, because each
  fresh handle starts its counter over. `omarchy-menu-generation-baseline.log`
  fails with `Some("false")` instead of `None`; no command is executed by the
  test. Assign process-unique checked revisions on every load/reload. This is
  one relaxed atomic update per definition load, not per render or action.
  Shell unit suite: 402 pass, two installed-Omarchy tests ignored here.
- Use `tempfile` (already locked in the dependency graph) only as a test
  dependency for private menu fixtures. The existing fixture no longer deletes
  a predictable PID-named directory before use.
- A real nested fixture proves initial menu parsing occurs twice: constructor,
  then the shared config applier. `omarchy-menu-startup-baseline.log` fails the
  required single-load assertion. Add explicit initial window-manager policy
  installation through the same private applier, preserving the constructor's
  already loaded menu. Both Wayland and X11 startup use it; explicit live
  reload keeps its reread behavior. Candidate build and verification pending.
  No startup-time percentage is claimed from these loaded-host runs.

### Sustained memory result and xdg interactive authorization (around 05:55 UTC)

- The cleanup candidate completes the full two-hour churn workload: 29,530
  cycles in 7,202.14 seconds, zero live selection-device records in all four
  lists, 40 -> 40 non-pipe descriptors and 59 -> 59 threads. Archive all 154
  MiB of raw evidence under `selection-device-cleanup-soak-raw/` before further
  source changes.
- Do not mistake swapping for a memory win. Candidate anonymous PSS is
  62,716 -> 53,632 KiB while SwapPss is 0 -> 18,684 KiB, so combined
  anonymous+swap grows 9,600 KiB. The matched baseline grows 22,008 KiB.
  Requested-live Rust growth falls from 16,548,860 to 6,415,455 bytes and the
  observed peak falls 9,999,644 bytes. Record these as fixture observations,
  not universal resident-memory or per-object claims.
- The compositor test passes. Its wrapper then hits the anticipated exit-127
  continuation fault because the campaign edited `scripts/e2e.sh` while that
  exact Bash process was waiting in Cargo. The independently invoked installed
  Omarchy menu/theme checks pass; no retry or relaxed assertion is involved.
- Add an opt-in xdg move/resize client probe and real nested tests. The preserved
  baseline begins a WM drag for zero, stale, wrong, cross-client and DnD serials;
  its no-button safety net masks two at the geometry layer only after needless
  grab and leave/re-enter churn. Cross-client and DnD grabs remain active and
  demonstrate the stronger fault directly.
- Validate the owning `wl_seat`, exact current Smithay pointer-grab serial and
  same client before queueing any request; refuse both client and server DnD
  grab types. This is request-path-only, allocation-free work. Eight real-client
  cases pass on immutable candidate `498f89d…`, including legitimate move and
  resize geometry; strict all-feature Clippy and installed Omarchy checks pass.
  Preserve the pre-fix failures and candidate pass logs with the
  `interactive-request-` prefix.

### Clock-render allocation budget and campaign freeze (around 06:05 UTC)

- Heaptrack identifies the dock clock's one-line `PathBuilder`s as a recurring
  allocation site. A dedicated thread-local allocator test measures exactly one
  warmed 56px render: 52 allocator requests / 24,332 requested bytes before,
  versus 32 / 24,002 after batching the four cardinal and eight intermediate
  tick marks by color. That is 20 fewer requests (-38.5%); it is an operation
  budget, not a compositor RSS or frame-time claim.
- Batching overlapping anti-aliased strokes changes alpha blending at tiny
  radii. The normal path therefore activates only at radius 3.5 or above, while
  tiny clocks retain the original stroke order. Three fixed FNV-1a image
  fingerprints and a byte-for-byte comparison with the pre-optimization
  renderer for every size from 8 through 128 pass. `wm-theme` all-feature,
  all-target strict Clippy passes. Evidence is under `clock-path-allocation/`.
- The user requested a logical wrap before work began on the next candidate
  (`Source::Tree` path construction). Freeze source here and run the complete
  release gate rather than mixing an unmeasured sampler rewrite into the
  campaign commit.
- Frozen-source `scripts/check.sh all` passes strict workspace Clippy, strict
  private-item rustdoc, display-free workspace tests, 236 Wayland unit tests
  (one benchmark ignored) and all 24 Python harness tests. The immutable release
  compositor SHA-256 is
  `d52b9fc28b609c22e9211f1f1e5b382b6b1e91a6610d84f1548a308b9eb014d3`.
- The final private headless release run uses that exact executable and passes
  199 nested cases plus all three installed-Omarchy cases, with an empty skip
  log. It includes real Edge at three scales and the isolated real Chonkcraft
  matrix. The log and 151 MiB raw fixture evidence are under `final-release/`.

### CI client-presentation fence (around 06:35 UTC)

- The first pushed campaign commit passes lint, dependency audit, workspace,
  release, X11, SDK and Omarchy-install jobs. Its Wayland job catches one real
  ordering gap in the fractional-scale Chromium fixture: DOM selection and
  input coordinates are exact, but the screenshot still contains Chromium's
  old 780x510 presented buffer inside the new 1280x800 fullscreen geometry.
  The remaining right and bottom bands are black. Preserve the job artifact
  and screenshot under `ci-failure-34016434023/`; do not rerun it as a flake.
- Extend the private test-door ledger with the root surface's currently
  presented physical width and height, distinct from the compositor's requested
  window geometry. The browser regression now waits for that client commit and
  then uses the existing compositor frame barrier before taking its unchanged
  pixel assertion. This is an observable protocol-state fence, with no sleep,
  retry or relaxed expectation.
- The exact GitHub-style debug setup (Chromium, SwiftShader, no sandbox,
  llvmpipe and headless Weston) passes the fractional case, then the complete
  1x/1.5x/2x browser matrix. `scripts/check.sh all` also passes after the fix:
  strict all-target Clippy, private-item rustdoc, workspace tests, 236 Wayland
  unit tests (one benchmark ignored), and all 24 Python harness tests.

### Initial audit (historical starting state)

- No open GitHub issues were returned; the reported Edge/repeat/game symptoms
  are user reports, not yet individually reproduced in this campaign.
- `input.rs` is 4,051 lines, `state.rs` 5,645, `desktop.rs` 5,763 and
  `wm-core/manager.rs` 8,673 (including comments/tests). Size is a navigation
  signal, not proof of poor architecture or a reason to rewrite working code.
- Baseline release build started in the isolated worktree; output is in
  `baseline-build.log` in the artifact directory. Preserve its executable before
  any production edits and do not reuse a stale binary for a claimed fix.
- Candidate root cause for scaled text-selection drags: `input::seat_origin`
  manufactures a position-dependent origin so Smithay's subtraction yields
  scaled local coordinates. Smithay 0.7's default `ClickGrab::motion` ignores
  the newly hit-tested focus and retains the press-time origin. Thus hover can
  be correct while drag deltas use global pixels instead of client coordinates.
  This is source evidence, not yet an application-level reproduction. Add a
  real scaled-client regression before changing the coordinate boundary.

## Known measurement limits carried forward

The earlier investigation is recorded in `docs/performance.md`: hidden-Dock
idle CPU/thread improvements were measured, ordinary idle RAM was essentially
unchanged, and visible-Dock startup showed a regression. Nested NVIDIA/Alacritty
and anonymous-memory drift require follow-up. No matched Hyprland superiority,
native DRM/KMS gaming result, universal GPU support or real old-hardware parity
has been established. Keep these limits visible throughout this campaign.
