# ChonkStep roadmap

Reviewed **26 September 2026**, against `origin/main` at
[`6eb4ab6`](https://github.com/iconidentify/chonkstep/commit/6eb4ab6168812a5a6e11f18786e1f619f6909e12).
This is an ordered delivery plan, not a promise of release dates. GitHub issues
remain the source for detailed acceptance criteria, claims and implementation
discussion. Reproduce each report against current main before changing code.

## Where we are

- **70 open issues:** the [September audit epic](https://github.com/iconidentify/chonkstep/issues/288),
  68 of its 116 findings, and the new clipboard test flake
  [#310](https://github.com/iconidentify/chonkstep/issues/310).
  All seven original P1 audit findings are closed; 48 audit findings are closed
  overall. These are tracker counts, not a fresh verification of every fix.
- **No open pull requests** at this review. Recent contributions are already
  merged: clamshell lid inhibition [#304](https://github.com/iconidentify/chonkstep/pull/304),
  the density-test correction for tabbed chrome [#306](https://github.com/iconidentify/chonkstep/pull/306),
  display-mode fallback [#309](https://github.com/iconidentify/chonkstep/pull/309),
  and seat-device lifetime cleanup [#308](https://github.com/iconidentify/chonkstep/pull/308).
  Start new work from these changes rather than recreating them.
- Main's [CI run](https://github.com/iconidentify/chonkstep/actions/runs/36271037657)
  passed. That does not cover the omitted renderer tests in #183 or disprove the
  intermittent failure in #310.
- The latest published release is the **0.7.0 preview**,
  [`preview-v0.7.0`](https://github.com/iconidentify/chonkstep/releases/tag/preview-v0.7.0),
  published 18 September. Main contains newer work. A new release needs its own
  validation and notes; a green main build is not a released package.

## What we optimize for

An Omarchy user should keep a reliable session, use the shipped shortcuts and
shell, connect ordinary laptop hardware, share one window privately, and get
responsive input with low idle cost. Accessibility belongs in that daily-use
contract. New themes and protocol counts alone do not establish that contract.

Keep desktop services and the optional standalone dock outside the compositor.
Require bounded client-driven work, lock and capture privacy, consistent render
and input order, and evidence for performance claims. Preserve the secondary
X11 backend, while prioritizing Wayland and Omarchy.

## Implemented and locally validated

- [#183](https://github.com/iconidentify/chonkstep/issues/183): renderer and
  binary test gates now run in local preflight and PR CI. The gates require all
  five surfaceless compositor tests and both vendored renderer tests by name;
  CI also runs native modern X11 chrome and Wayland version tests. Local
  preflight and the native X11 check passed.
- [#310](https://github.com/iconidentify/chonkstep/issues/310): the real-browser
  clipboard regression now observes queued input while the browser is paused,
  then requires its real clipboard offer to release that input. An opt-in test
  clock verifies the exact timeout boundary without changing the production
  250 ms deadline. Ten consecutive runs on one CPU passed; disabling key
  deferral made the regression fail. See the
  [implementation and validation record](engineering/2026-09-26-copy-order-test.md).

The open-issue counts above describe the review snapshot. These implementations
have passed local validation; GitHub issue closure is tracked separately.

## Immediate queue

The next implementation is #256. Entries are ordered follow-ups, not
simultaneous claims.

| Order | Work | Why now / completion evidence |
| --- | --- | --- |
| 1 | [#256](https://github.com/iconidentify/chonkstep/issues/256): bound focus grabs and virtual input | Bound memory and uninterrupted work; hostile-client tests must show bystander responsiveness, ordered input, and unchanged lock behavior. |
| 2 | [#281](https://github.com/iconidentify/chonkstep/issues/281): actionable crash evidence | Preserve panic/XWayland diagnostics and durable recovery history. A report after recovery must identify what failed. |
| 3 | [#201](https://github.com/iconidentify/chonkstep/issues/201): hung-loop recovery | Detect a stalled loop from outside that loop, preserve evidence, and enter the existing locked recovery path. Prove suspend, inactive VT and slow modesets do not cause false recovery. |
| 4 | [#181](https://github.com/iconidentify/chonkstep/issues/181): isolate launched applications | Under uwsm, put supported application launches in application scopes. Verify cgroup ownership, activation tokens, argv and non-uwsm fallback. |
| 5 | [#191](https://github.com/iconidentify/chonkstep/issues/191): touch/tablet output mapping | A laptop touchscreen must still hit its own panel when an external display is attached; test rotation, scaling and reconnect. |
| 6 | [#199](https://github.com/iconidentify/chonkstep/issues/199): screen-share chooser | Offer both window and monitor sharing through the actual portal, with cancellation and protected-window coverage. |
| 7 | [#198](https://github.com/iconidentify/chonkstep/issues/198): background work off the event loop | Decode/cache off-thread, discard stale results, and measure input/frame tails during theme changes and monitor resize. |
| 8 | [#248](https://github.com/iconidentify/chonkstep/issues/248): event-driven idle upkeep | Replace recurring file polls with watches and bounded recovery. Measure truly idle wakeups and CPU before/after. |

## Delivery tracks and the remaining backlog

Every open audit finding is placed below. Tracks express priority and related
work, not permission to combine unrelated issues into one large patch. An item
is complete only when its current acceptance criteria and relevant tests pass.

### A. Reliable sessions and trustworthy changes — start now

| Work | Issues |
| --- | --- |
| Renderer coverage, deterministic E2E, bounded protocol work | [#183](https://github.com/iconidentify/chonkstep/issues/183), [#310](https://github.com/iconidentify/chonkstep/issues/310), [#256](https://github.com/iconidentify/chonkstep/issues/256), [#226](https://github.com/iconidentify/chonkstep/issues/226) |
| Crash evidence, hung-loop recovery, application scopes | [#281](https://github.com/iconidentify/chonkstep/issues/281), [#201](https://github.com/iconidentify/chonkstep/issues/201), [#181](https://github.com/iconidentify/chonkstep/issues/181) |
| Reproducible vendor patches, upgrade plan, dependency policy, parser fuzzing | [#267](https://github.com/iconidentify/chonkstep/issues/267), [#233](https://github.com/iconidentify/chonkstep/issues/233), [#229](https://github.com/iconidentify/chonkstep/issues/229), [#238](https://github.com/iconidentify/chonkstep/issues/238) |

Sequence #183 and #267 before a Smithay migration. #233 should identify a
verified upstream revision, required backports and test gates before switching
dependencies. Do not assume that the upstream release information in the
13 September issue is still current. #281 should precede #201 so a watchdog
failure leaves useful evidence.

### B. The everyday Omarchy desktop — next product milestone

| Work | Issues |
| --- | --- |
| Correct touch/tablet mapping, hybrid-GPU outputs, display mirroring | [#191](https://github.com/iconidentify/chonkstep/issues/191), [#185](https://github.com/iconidentify/chonkstep/issues/185), [#186](https://github.com/iconidentify/chonkstep/issues/186) |
| Private window sharing and cross-X11/Wayland drag-and-drop | [#199](https://github.com/iconidentify/chonkstep/issues/199), [#282](https://github.com/iconidentify/chonkstep/issues/282) |
| Sticky/slow/bounce keys, magnification, screen-reader keyboard integration | [#195](https://github.com/iconidentify/chonkstep/issues/195), [#217](https://github.com/iconidentify/chonkstep/issues/217), [#218](https://github.com/iconidentify/chonkstep/issues/218) |
| Stable Mosaic placement and Omarchy window groups | [#224](https://github.com/iconidentify/chonkstep/issues/224), [#222](https://github.com/iconidentify/chonkstep/issues/222) |
| Decide the Lua configuration contract | [#236](https://github.com/iconidentify/chonkstep/issues/236) |

Finish laptop and portal regressions with real clients. Multi-GPU output work
requires a machine whose ports span DRM devices; software CI cannot accept it
alone. Coordinate #185 with mirroring, but keep single-device mirroring useful
independently. Agree on stable layout identity before layering groups onto it.
Evaluate the Lua runtime proposal against real supported Omarchy configuration
and resource limits; choosing a runtime is not itself a parity fix.

### C. Measurably lower latency, idle cost and capture cost

| Work | Issues |
| --- | --- |
| Background decoding, watches and incremental config reload | [#198](https://github.com/iconidentify/chonkstep/issues/198), [#248](https://github.com/iconidentify/chonkstep/issues/248), [#246](https://github.com/iconidentify/chonkstep/issues/246) |
| Per-output damage, unchanged bar workareas and no-op layout revisions | [#210](https://github.com/iconidentify/chonkstep/issues/210), [#255](https://github.com/iconidentify/chonkstep/issues/255), [#244](https://github.com/iconidentify/chonkstep/issues/244) |
| Demand-driven previews, retained hidden-tile buffers and capture damage | [#257](https://github.com/iconidentify/chonkstep/issues/257), [#249](https://github.com/iconidentify/chonkstep/issues/249), [#209](https://github.com/iconidentify/chonkstep/issues/209), [#252](https://github.com/iconidentify/chonkstep/issues/252) |
| VRR scheduling and policy, explicit-sync overhead, suspended clients | [#213](https://github.com/iconidentify/chonkstep/issues/213), [#269](https://github.com/iconidentify/chonkstep/issues/269), [#284](https://github.com/iconidentify/chonkstep/issues/284), [#273](https://github.com/iconidentify/chonkstep/issues/273) |
| Input, surface pacing, commit-state reuse and IPC upkeep | [#231](https://github.com/iconidentify/chonkstep/issues/231), [#232](https://github.com/iconidentify/chonkstep/issues/232), [#234](https://github.com/iconidentify/chonkstep/issues/234), [#251](https://github.com/iconidentify/chonkstep/issues/251) |
| Menu rendering, process reaping and lazy font loading | [#235](https://github.com/iconidentify/chonkstep/issues/235), [#247](https://github.com/iconidentify/chonkstep/issues/247), [#250](https://github.com/iconidentify/chonkstep/issues/250) |
| XWayland lookup, size-hint caching and transient graph reuse | [#259](https://github.com/iconidentify/chonkstep/issues/259), [#263](https://github.com/iconidentify/chonkstep/issues/263), [#268](https://github.com/iconidentify/chonkstep/issues/268) |
| Consistent chrome emission and nested clipping | [#241](https://github.com/iconidentify/chonkstep/issues/241), [#254](https://github.com/iconidentify/chonkstep/issues/254) |

Take measured stalls and unnecessary whole-output work before small hot-path
cleanups. Record release-build baselines for idle, typing/dragging, a video on
one of two outputs, theme switching and screen sharing. Include hardware,
resolution, scale, refresh, CPU, retained memory, frame/input tails and capture
work. Set each change's performance acceptance target from that baseline;
operation counts and software rendering do not establish native GPU latency.

### D. Visual fidelity and additional application protocols

| Work | Issues |
| --- | --- |
| Authored Omarchy borders/shadows, map/unmap motion and panel backdrop effects | [#271](https://github.com/iconidentify/chonkstep/issues/271), [#189](https://github.com/iconidentify/chonkstep/issues/189), [#274](https://github.com/iconidentify/chonkstep/issues/274) |
| Small client protocols and toplevel dragging | [#276](https://github.com/iconidentify/chonkstep/issues/276), [#280](https://github.com/iconidentify/chonkstep/issues/280) |

Keep reduced-motion behavior and client-owned shell animations intact. Blur
must have explicit cost and damage measurements. Advertise a protocol only
when its behavior, lifetime and security rules are implemented and tested.

### E. Hardware expansion — after the daily-use baseline

| Work | Issues |
| --- | --- |
| GPU reset detection/recovery, tearing, HDR/10-bit color, VR leases | [#265](https://github.com/iconidentify/chonkstep/issues/265), [#270](https://github.com/iconidentify/chonkstep/issues/270), [#272](https://github.com/iconidentify/chonkstep/issues/272), [#287](https://github.com/iconidentify/chonkstep/issues/287) |

Each needs explicit hardware coverage and a fallback policy. Prioritize #265
earlier if field evidence identifies GPU resets as a current source of session
loss. Do not advertise HDR, VR or reset recovery based only on protocol wiring.

### F. Focused maintenance alongside the affected feature

| Work | Issues |
| --- | --- |
| X11 focus grabs and EWMH state parity | [#262](https://github.com/iconidentify/chonkstep/issues/262), [#230](https://github.com/iconidentify/chonkstep/issues/230) |
| Geometry helpers, backend API cleanup and shared core test fixtures | [#237](https://github.com/iconidentify/chonkstep/issues/237), [#264](https://github.com/iconidentify/chonkstep/issues/264), [#266](https://github.com/iconidentify/chonkstep/issues/266) |
| Session, core manager and Wayland-state module boundaries | [#240](https://github.com/iconidentify/chonkstep/issues/240), [#258](https://github.com/iconidentify/chonkstep/issues/258), [#261](https://github.com/iconidentify/chonkstep/issues/261) |
| Log levels and packaged artwork size | [#243](https://github.com/iconidentify/chonkstep/issues/243), [#239](https://github.com/iconidentify/chonkstep/issues/239) |
| Reassess dock cleanup and current documentation | [#253](https://github.com/iconidentify/chonkstep/issues/253), [#245](https://github.com/iconidentify/chonkstep/issues/245) |

Avoid large mechanical splits while teams are fixing the same files. Small
refactors should enable an identified change or remove verified duplication.

## Backlog corrections before closing anything

The epic still has unchecked rows for already closed issues **#179, #182,
#190, #205, #219, #220, #228, #242, #275 and #277**. Reconcile those rows with
their merged fixes; they are not new implementation work.

The acceptance criteria for #245 and #253 predate the standalone dock restored
in 0.7.0. Reassess each claimed dead crate, API and documentation reference
against `dock/` and current packaging. Do not remove a live standalone-dock
dependency or erase valid dock documentation to satisfy the old blanket grep
checks. The compositor/standalone boundary remains the intended architecture.

Do not blanket-close #249 or #257 as obsolete: `chonk-shell/src/miniwindows.rs`
still implements optional preview tiles, and `wm-wayland/src/renderer.rs` still
calls `capture::refresh_snapshots`. Their costs and consumers need a current
measurement. Other audit line numbers refer to `31d7e3e`, not today's main.

## Coordination and release gates

Before taking an issue, refresh main, read the complete issue and comments,
check open PRs and claims, and reproduce the current behavior. With GitHub
coordination authorized, publish one claim per contributor and recheck for a
race. Prefer one issue per branch/PR and release a claim when handing it off.
Incoming work is reviewed before anyone starts a duplicate implementation.

For each fix, preserve failing-before/passing-after evidence where feasible,
run the narrow relevant checks and then the applicable repository gates.
Review against the current issue criteria and inspect the full diff. Do not
merge on a green check that omitted the behavior being fixed.

A release candidate needs the exact commit's required CI, package/install
validation, Omarchy login/lock/recovery testing, and applicable real-client
tests. Output, input and GPU work additionally needs the relevant hardware
matrix: single/multiple outputs, mixed scale, lid/dock cycles, suspend/resume,
VT transitions and supported GPU/driver combinations. Record limitations and
skips explicitly. Software-renderer tests remain valuable, with a narrower
claim than hardware validation.

Refresh this snapshot after merges or a release: count actual open issues,
update epic checkboxes, credit incoming contributions, and move completed work
out of the immediate queue. A new crash, privacy leak or lost-input regression
takes priority over planned features.
