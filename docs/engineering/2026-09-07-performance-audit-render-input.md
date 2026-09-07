# Render, input and protocol performance audit

Source review in `chonkstep-capture-performance`, September 7, 2026. This pass
reviews current code after the existing September 5 performance work and
September 7 hybrid-layout audit. It is not a claim of exhaustive verification.
No builds or workload measurements ran during the root agent's paired capture
measurements. Coordinated Mosaic before/candidate results and protocol
validation status are recorded below. This final reconciliation identifies the
frozen r6 product and preserves the earlier failures separately from corrected
fixture runs. Whole-process pressure and soak results belong to the root
campaign report and are not inferred from this source review.

## Ranked findings

1. **Synchronous ext capture can monopolize the compositor dispatch.**
   `wm-wayland/src/image_capture.rs::refresh` previously processed every pending request
   in one pass and performed each output/toplevel render, readback and SHM copy
   separately. Unlike `protocols.rs::frame_presented`, this route had no
   presentation cadence, grouping, per-pass copy budget or session admission
   ceiling. One frame per session is enforced, but one client can create many
   sessions. A normal multi-consumer recorder can duplicate expensive readbacks;
   a session flood can turn one dispatch into arbitrarily many full-image
   transfers before unrelated input returns. This is source-established work
   amplification, not a measured latency number. Proposed scheduler and tests
   are specified below. The authorized first patch now bounds sessions and
   pending requests at 256 each, with honest stopped/failed overflow replies;
   accepts pending frames in FIFO order; limits each batch to four readbacks,
   4 Mi pixels, 32 examined entries and 2 ms between readbacks; and schedules
   the next batch after a 4 ms pause. One oversized first frame is allowed so
   large outputs progress. Cooldown persists across newly arriving requests,
   but an empty queue contributes no deadline or idle wake. Frame destruction
   leaves bounded tombstones instead of repeatedly scanning the whole queue.
   The existing per-session constraint scan remains, now bounded at 256.
   A single readback is still synchronous and cannot be preempted. Tests cover
   the first idle frame, orphan-frame lifetime, locked output, session-cap
   recovery, pending-cap overflow and native/input progress during a FIFO
   backlog. The first release run observed a 71 µs unrelated native roundtrip,
   input delivered after five completed captures, and 256 accepted / one
   rejected requests. Two of four E2E cases failed in the test client's
   `reader.read().unwrap()` on a transient `WouldBlock`; no compositor failure
   was reported. Both native client helpers now tolerate `WouldBlock` and
   `Interrupted` while retaining their absolute deadline. The r4 full native
   suite subsequently passed the ext capture cases. The preliminary 71 µs
   observation remains one run, not a latency guarantee.

2. **FIFO housekeeping charged ordinary surfaces on every dispatch.**
   `xdg.rs::service_surface_pacing` ran twice per dispatch, cloned all surfaces
   into a temporary Vec, resolved each root/popup's scene visibility, lazily
   created FIFO cached state for surfaces which never used FIFO, built a fresh
   client HashMap, then polled every client's transaction queue. The renderer's
   `signal_pacing_barriers` also created FIFO cache state. The loop's deadline
   calculation adds another all-surface metadata walk. Window visibility is
   already indexed; an earlier draft's suspected stack-scan multiplier does
   **not** exist in current code.

   The authorized conservative patch checks existing FIFO/timer metadata,
   processes only participating surfaces, resolves visibility only for a live
   unsignalled FIFO barrier, and reuses compositor-owned scratch. Every retained
   client/surface handle is cleared at the original function boundary. The
   renderer checks cache presence too. Timer pre-commit registration remains
   discoverable before a blocked commit reaches the compositor callback, and
   clients whose rendered FIFO barrier was removed still participate because
   their FIFO cache remains present. No speculative surface index, vendor patch
   or capacity ceiling was introduced. The frozen r3 release passed all three
   `surface_pacing` cases: real presentation feedback, FIFO waits, commit
   timestamps, hidden/locked content, disconnects and ordinary clients. The
   subsequent read-helper robustness change also passed in the r4 native suite.

3. **Mosaic's remaining geometric rejection case can repeatedly search.**
   `wm-core/src/spatial.rs::mosaic` skips column searches when total minimum area
   is impossible, which was the previous audit's fix. That does not reject
   incompatible shapes whose *area* fits. Candidate fixture: 1000×1000 output,
   equal populations of 999×1 and 1×999 minimum rectangles, at up to 512 clients.
   Total minimum area is below output area while mixed almost-full-width and
   almost-full-height rectangles severely constrain valid columns. Each
   exclusion can repeat up to N partition attempts and another maximum-demand
   scan. A preserved release baseline confirms the cost on this machine:

   | Clients | Wide first median | Tall first median | Alternating median |
   | --- | ---: | ---: | ---: |
   | 64 | 568 µs | 578 µs | 669 µs |
   | 128 | 3,924 µs | 3,973 µs | 4,572 µs |
   | 256 | 28,758 µs | 29,039 µs | 32,921 µs |
   | 512 | 213,846 µs | 221,016 µs | 259,643 µs |

   These are three samples per case, not a cross-machine latency guarantee.
   `mosaic_cross_strips_profile` records 8/32/64/128/256/512 clients, three
   orderings, three repeats, exact indexed placement digests and geometry
   invariants. The exact baseline executable, source snapshot, source/toolchain
   hashes, build JSON and all 54 raw samples are preserved in
   `/home/chrisk/src/chonkstep-engineering-artifacts/2026-09-07-render-input-audit/mosaic`.
   No solver algorithm change was made before that measurement. The candidate
   now checks necessary summed minimum heights/column widths before calculating
   weights or allocating result geometry. Partition arithmetic, preferred
   ordering and excluded-client selection remain unchanged. Eighteen baseline
   placement digests form an exact-output regression oracle. The coordinated
   full check gate passed that oracle in the ordinary unit suite. The root
   agent then measured the frozen r3 release executable after the seven normal
   capture pairs ended, before taking the next compile slot. Every one of the
   54 candidate samples matched the corresponding exact geometry digest.

   | Clients | Wide first candidate | Tall first candidate | Alternating candidate |
   | --- | ---: | ---: | ---: |
   | 64 | 55 µs | 68 µs | 40 µs |
   | 128 | 139 µs | 317 µs | 165 µs |
   | 256 | 579 µs | 1,290 µs | 594 µs |
   | 512 | 4,509 µs | 7,048 µs | 4,356 µs |

   For this constructed 512-client case, the measured improvement is
   31.4–59.6×. It does not predict normal-workspace or other-machine speedups.
   `candidate-profile.txt`, `candidate-metadata.json` and `comparison.json`
   preserve raw timings, frozen binary/source hashes and all 18 case medians
   beside the baseline artifacts. Candidate executable SHA-256 is
   `9e835ae18568ffc7480c6478820ad4589336cf197382cc953ef3a08fee2f8206`.

4. **Ext capture's idle session scan allocated an avoidable snapshot.**
   Each `image_capture::refresh` cloned all session resources/Arcs into a Vec
   merely to call an immutable constraint updater. The authorized patch
   iterates sessions by reference. This removes that snapshot allocation and
   reference-count traffic while preserving every liveness/constraint check.
   It does not make the entire session reconciliation change-driven.

5. **Retained capture targets previously had only a count bound.**
   `protocols.rs` retained up to eight render targets keyed by
   region/size/transform/cursor mode. Eight
   3840×2160 RGBA8 textures represent about 253 MiB of texture payload before
   driver overhead, with no assertion that all drivers charge it to RSS.
   Repeated large, slightly different capture regions can fill the cache on a
   low-memory machine. Existing target reuse and the eight-entry ceiling are
   real mitigations; neither should be described as missing. The implemented
   64 MiB soft-budget/LRU policy evicts before allocation, allows one larger
   necessary working target, and retires idle entries through existing
   housekeeping while draining deferred GLES deletion. The authoritative
   region-churn/idle-retirement pixel test passed against r5 after the native
   fixture normalized the advertised output transform. The same product
   source is frozen in r6; payload accounting is not a total-memory guarantee.

6. **Settled layout clips remain in repeated dispatch scans.**
   `layout_scene.rs::tick` queries output modes, scans all presentations for
   animation, and retains/walks clipped windows even with no active animation;
   `deadline` and snapshot suppression ask `animating` again. On a large Flow
   workspace this is O(retained layout windows) unrelated work per input
   dispatch. A cached active-motion count plus explicit lifetime invalidation
   is a candidate only after measuring 16/64/256 settled windows. Preserving
   dead/unmapped cleanup, lock cancellation, clipped rendering and caption
   expiration matters more than a speculative early return.

7. **Grouped wlr readback still allowed unbounded destination copies.**
   Root's further review found that one shared download could synchronously
   fan out to every waiting client buffer. The root-owned patch admits at most
   256 pending/staged consumers, retains one owned GLES mapping (or GLES2 CPU
   buffer), and copies at most four buffers / 4 Mi pixels / 2 ms between copies
   before a 4 ms cooldown. One oversized first copy preserves progress. Read-only
   cross-review confirmed that Smithay's mapping owns its PBO and cached map
   pointer across dispatch turns; explicit cleanup after batch drop drains
   deferred deletion even if the scene goes idle. Lock state is checked before
   each deferred batch. The review caught removal of the prior one-geometry
   per-output/presentation guard; root restored it before validation. Existing
   admitted fanout can finish without forcing another presentation, while new
   damage-aware requests still wait. New native fanout/input/newcomer/lock
   tests passed in the r4 native suite. The final review then found the
   cancellation/progress edge documented below. Its deterministic test fails
   against r4 and passes against r5; all six pressure cases also pass in r6.

## Further ext capture grouping proposal

The first patch above bounds serial service without sharing readbacks. Further
grouping remains a proposal and requires separate semantic/throughput evidence.
Use a compositor-owned scheduler, not sleeping threads or blocking handlers:

- A `SourceKey` contains output identity or toplevel identity, resolved region,
  buffer transform, cursor policy and a scene/lock generation. Never key only
  by pixel dimensions.
- Each source owns a FIFO of pending frame resources and a refresh deadline.
  Maintain a round-robin queue of runnable sources so a continuously active
  source cannot starve others. Admission tracks live sessions/queued frames
  per client and globally; limits must be selected from measured workloads.
- A servicing pass has separate readback and destination-copy budgets. Shared
  readback alone is insufficient: N identical-source requests still cost N
  full-frame SHM writes. If work is continued on a later dispatch, retain only
  the bounded readback cohort necessary for those copies and revalidate its
  scene/lock generation before writing.
- Initially preserve full-damage responses and current cursor/output/toplevel
  semantics. First successful capture must get a scheduled slot even when
  the source is idle; later captures may wait for source damage. The vendored
  ext-image-copy-capture XML expressly permits indefinite damage wait only
  after a first successful capture.
- Existing frame objects survive destruction of their parent session. Remove
  pending work on frame destruction/disconnect, source death, or stopped shared
  state as appropriate, not merely because the session resource is absent. The
  bounded serial implementation consumes dead resources within its entry budget.
- At service time, recheck current constraints and lock policy. A toplevel
  capture must never bypass the lock. Output capture must render the locked
  scene. Never publish an unlocked cached readback after lock acquisition.
- Register the nearest service deadline with calloop. Do not turn queued
  capture requests into an unbounded zero-timeout loop or depend on unrelated
  input to service an initial idle frame.

The initial `image_capture` acceptance target establishes first-idle-frame
completion, output pixel equality, a frame captured *after* session destruction,
and an existing session changing from desktop pixels to lock pixels. Before
further grouping changes, add mixed-source fairness, continuous input/IPC
flood measurements, per-client disconnect,
queued-frame cancellation, output resize/rotation and toplevel lock tests.
Record readbacks, bytes copied, dispatch maximum/histograms, protocol results,
memory and exact output comparisons. Compare normal 1/2-source recording as
well as 8/32/128-session pressure. No throughput claim is valid without both
normal and adversarial observations.

## Reviewed module inventory

“Inspected” here means entry points, hot-path control flow and resource
ownership were read; it does not mean every line received a proof.

| Area | Modules and current mitigation / remaining examination |
| --- | --- |
| Scene build and rendering | `renderer.rs`, `decoration.rs`, `backend_impl.rs`: retained per-output scene Vecs, iterative retained surface walk, exact overflow fallback and sparse decorations already exist. Cursor input resolves ownership; damage tracker governs actual repaint. Root owns capture dimming/chrome examination. |
| Native presentation | `session.rs`, `output_power.rs`: native output frame clocks, dirty flags, flip watchdog, scanout/pending buffer lifetime and necessary driver-fence waits inspected. Any coarse scene damage currently visits all outputs; optimizing that needs physical multi-output lifetime/refresh tests. No unsupported claim that fence waits can simply be removed. |
| Dispatch/startup | `state.rs`, `lib.rs`, `diagnostics.rs`, `memory_profile.rs`: deadlines and source readiness already avoid refresh-rate idle polling; capture/reload markers remain bounded housekeeping. Pacing work was the principal new common-dispatch issue. Native DRM/font/driver startup is not measured by source inspection. |
| Pointer and touch | `input.rs`, `input/{constraints,gestures,seat,surface}.rs`, `virtual_pointer.rs`: route ownership, client grabs, hidden-cursor policy and relative locked-pointer bypass reviewed. Root owns capture-specific motion/selection changes. General hit testing still walks visual bands in order; caching would require precise mutation invalidation. |
| Keyboard/focus | `input/keyboard.rs`, `input/keyboard/{focus,repeat}.rs`, `virtual_keyboard.rs`, `focus_grab.rs`, `global_shortcuts.rs`: repeat deadlines, focus changes and lifecycle gates inspected. Shortcut admission is bounded. Avoid coalescing key/button transitions as if they were pointer motion. |
| Surface commit/lifetimes | `xdg.rs`, `core_protocols.rs`: configure debt deduplication, indexed window visibility, popup-root fast path, depth bounds and activation/IME admission already present. New pacing patch preserves timer/FIFO discovery and release semantics. |
| DMA transport | `dmabuf.rs`, `readback.rs`: acquire readiness is event-driven with bounded fallback; GLES3 borrows mapped readback and GLES2 has validated owned storage. Removing readiness or release waits without equivalent fencing would change correctness. |
| Capture/protocol publication | `protocols.rs`, `image_capture.rs`, `capture.rs`, `toplevel_mapping.rs`: wlr grouping/pacing, change-driven toplevel publication and preview suppression already exist. Ext dispatch amplification and target cache bytes remain findings above. |
| Selection | `selection.rs`, `data_control.rs`, XWM callbacks: native/X11 ownership avoids feedback loops and transfers are asynchronous; no new compositor-side blocking pipe transfer found in these entry points. |
| XWayland | `xwayland.rs`, `xwayland/{lifecycle,settings}.rs`, `xewmh.rs`: generation-owned restart/readiness, dirty stacking publication and buffered EWMH updates already present. Repeated X server round trips are not on unchanged ordinary pointer dispatch. |
| Desktop lifecycle protocols | `layers.rs`, `lock.rs`, `idle.rs`, `inhibit_bus.rs`, `output_mgmt.rs`, `output_power.rs`, `gamma.rs`, `ctm.rs`, `workspace.rs`: change/dirty gates, coalesced gamma writes, bounded inhibitor ledger and async D-Bus service inspected. Preserve lock ordering and gamma hardware fallback during optimization. |
| Compatibility IPC | `hyprland_ipc.rs`, `chonk-hyprland-ipc/src/server.rs`: snapshots are request/change driven, event snapshots omit binding strings, read/backlog budgets and round-robin request admission already exist. Avoid reporting previously repaired always-per-frame snapshots as new findings. |
| Overview and transitions | `overview.rs`, `gesture_scene.rs`, `layout_scene.rs`: cached scene intent, live surfaces and allocation-free spring updates already exist. Retained settled layout scan remains a profiling candidate. |
| Core manager/algorithms | `manager.rs`, `manager/spatial_layout.rs`, `spatial.rs`, `placement.rs`, `snap.rs`, `resize.rs`, `hittest.rs`, `focus.rs`, `gestures.rs`, `gestures/physics.rs`: direct client/frame indices, borrowed snap targets and deferred decoration paint already exist. Mosaic cross-strip rejection is the new adversarial solver candidate. Layout/focus/membership and geometry test oracles must remain exact. |
| Core model/backends | `backend.rs`, `client.rs`, `types.rs`, `motif.rs`, `lib.rs`, `fake_backend.rs`: borrowed monitor API and cached hit-test inputs support the existing hot paths. Model definitions and test backend are not independent measured bottlenecks. |
| Test/diagnostic transport | `test_door.rs` and `chonk-testkit`: barrier explicitly marks damage; native diagnostic screenshots independently recompose a scene. New scheduling assertions use read-only frame counters before either operation to prevent false passes. |

## Reproduction entry points

- `scripts/e2e.sh --headless --release --test surface_pacing --nocapture`.
  The profile case supports `CHONKSTEP_PACING_PASSES` (default 256) and
  `CHONKSTEP_PACING_MEMORY=1` for a separately instrumented executable.
- `scripts/e2e.sh --headless --release --test image_capture --nocapture`.
- `cargo test --release -p wm-core mosaic_cross_strips_profile -- --ignored --nocapture`.

Use preserved before/candidate binaries with matching build profiles; never
compare instrumented allocation runs to an ordinary release for throughput.
This artifact records source findings and proposed/implemented changes; final
test results and raw paired samples belong in the root campaign report.

## Coordinated validation status

The root agent reported 269 `wm-wayland` unit tests passing, followed by a
successful `scripts/check.sh all` run (workspace lint, docs, non-Wayland units,
Wayland units and Python harness checks). The native flood fixture was then
refined to use distinct `wl_buffer` objects in one shared SHM pool, avoiding
reuse of an outstanding protocol buffer while keeping test memory small.
Frozen r3 subsequently passed 11/11 capture-tool E2E cases and 3/3 pacing
cases; ext image capture passed 2/4 before the transient client-read failure
described above. The helper repair was pending at that checkpoint. Mosaic candidate
timings and all 54 exact geometry comparisons passed as recorded above.

The wave-2 `scripts/check.sh all` gate subsequently passed strict Clippy, docs,
workspace tests and 276 Wayland unit tests, including the joined PNG adapter's
all-alpha, exact compressed Sub stream, transient-write, IDAT-boundary, IEND
and flush tests, and isolated clipboard lifecycle tests. The frozen r4 native
suite reported 245 passing cases and one cache pixel-orientation failure across
58 targets. Independent review identified the native fixture's missing
`wl_output` transform normalization: winit's Flipped180 stores the crop's last
row first, while grim returns an upright PNG. The fixture now records and
normalizes all eight advertised transforms, with an asymmetric padded-buffer
oracle. The corrected cache test passed, including the later r5 rerun. Three
browser artifact-copy wrappers also encountered
dangling Chromium Singleton symlinks after their assertions passed; that
harness-copy failure is distinct from a failed product assertion. The corrected
runner preserves symlinks and records the test result before copying artifacts.

The r5 full `scripts/check.sh all` gate passed, including 276 Wayland units.
Its eight affected native targets passed exactly 31 cases: screencopy pressure
6, capture cache 1, ext image capture 4, surface pacing 3, capture tool 11,
nested buffer age 1, hidden surface damage 2, and session lock 3. The raw group
results are in `capture-r5/e2e-results.json` and
`capture-r5/e2e/<target>/capture-e2e.log` beneath the artifact root below.

Comparing r5/r6 `provenance.json` source hashes shows one changed product file:
`crates/chonk-instruments/src/link_panel.rs`. Frozen r6 passed strict all-target
Clippy and 253 instrument plus 424 shell unit cases, including both malformed
status regressions. Its native panel, pressure and idle cases passed. The
first r6 capture run passed 9/11: two diagnostics timed out because the test's
filename marker could be observed between truncation and write, requesting the
supported default screenshot path. Publishing the marker by atomic rename
fixed only the fixture. All 11 capture cases then passed against the **same**
frozen r6 compositor; the failed run remains intact.

Artifact root:
`/home/chrisk/src/chonkstep-capture-performance-artifacts/2026-09-07`.
The final compositor SHA-256 is
`315598fbe0b8a49eec2e0c3654331472dfd67c2f5946aaef54caa9077ab6da8f`.
`capture-r6-marker-fixture-fix/e2e/result.json` records that exact executable
and the separately frozen corrected harness. Relevant logs are
`capture-r5/check-all.log`, `capture-r6/{lint,panel-units}.log`,
`capture-r6/e2e-results.json`, and
`capture-r6-marker-fixture-fix/e2e/capture-e2e.log`.

Clipboard candidates remain serialized and capped at two with screenshot
admission charged through completion; review launch now follows synced
publication immediately, without a fixed worker sleep. Failure retains the
previous live provider. The PNG adapter keeps exact tiny-skia demultiplication
and bounded row/64 KiB IDAT scratch, uses png's chunk writer for CRC, and latches
downstream failures before fdeflate's header/checksum writes can panic.

The r4 pure encoding benchmark passed exact decoded-pixel checks and reported:

| Image | Prior median | Joined stream median | New requested allocator bytes |
| --- | ---: | ---: | ---: |
| 1920×1080 structured | 11.464 ms | 12.201 ms | 138,753 |
| 1920×1080 noise | 17.496 ms | 17.566 ms | 138,753 |
| 3840×2160 structured | 51.737 ms | 47.604 ms | 146,433 |
| 3840×2160 noise | 117.105 ms | 69.881 ms | 146,433 |

Every new case made three scratch allocations; prior requested allocation
totals ranged from 23,040,945 to 369,783,766 bytes. These are sums of allocator
requests, not live memory or RSS. Timings exclude fixture cloning, filesystem
writes and fsync, and allocation instrumentation was outside the timed section.
The row-alignment regression is substantially removed, without a universal
speedup claim: structured 1080p remained about 6.4% slower in this run. Noise
PNGs are about 8.9% larger because bounded streaming cannot use the previous
whole-image stored-block fallback. Raw output is preserved in
`/home/chrisk/src/chonkstep-capture-performance-artifacts/2026-09-07/capture-r4/microbenchmarks/png.log`.

## Capture acceptance and final adversarial review

The final fixture rerun supports the requested cursor, chrome and save/open
behavior. It exercises real native input, renderer pixels, clipboard bytes and
recording processes; isolated launchers prevent opening real review apps on
the user's desktop during tests.

| Requirement | Acceptance and practical limit |
| --- | --- |
| Visible cursor over capture controls | At 1×/1.5×/2×, local pixel oracles require a bright tip/halo and dark arrow body above three interior controls. Moving within the same control must increment read-only presentation counters before a diagnostic can force rendering. Capture temporarily overrides the desktop's hidden cursor, and dismissal restores that ownership. |
| Chrome and selection behavior | Scaled toolbar actions, retained area drag/resize, keyboard nudge presentation and pixels, unoccluded window capture, and recording badge behavior pass. Pressing in an app and releasing over the badge still reaches the app and does not stop recording. Root also inspected the saved visual QA images; automated contrast/geometry tests do not measure subjective design quality. |
| Clean image and video output | Pixel comparisons exclude the toolbar, dimming and cursor from cursorless screencopy. Saved crop dimensions/pixels are exact, and odd-sized recording finalization yields playable MP4 with controls excluded. |
| Open saved PNG in the default viewer | A NUL-separated fixture log proves `xdg-open` receives exactly one literal published path. The independent opener audit confirmed this Omarchy installation's PNG handler is imv; user MIME preferences are respected. This verifies routing, not a real viewer's UI, startup or memory cost. |
| Open final recording in Omacut | The same native route oracle records `omacut` only after successful MP4 publication. Failure to save does not open an app; isolated worker tests cover missing/failing launchers, hostile/non-UTF8 path boundaries, helper caps and cleanup. |
| Clipboard and failure behavior | Native tests retrieve the exact published PNG through `wl-paste`; units cover serialized candidates, preserving a prior provider on failure, queue bounds and shutdown. The retained 100 ms process-liveness probe is not a protocol-level ownership acknowledgement. |

The fixture source is `crates/chonk-testkit/tests/capture_tool.rs`. The
independent installed-opener/packaging review and its limits are recorded in
`docs/engineering/2026-09-07/audit-startup-memory.md`, under “Final opener,
packaging and lifetime review.” Final capture acceptance is 11/11 in the
`capture-r6-marker-fixture-fix` log identified above.

The final read-only pass found one additional delivery blocker in r4:
`protocols.rs::frame_presented` could let a canceled first geometry consume the
only forced presentation for a different plain request, then remove the canceled
request and return with neither an eligible deadline nor scene damage. Exact
trigger: queue damage-aware A, queue plain B in another region, destroy A's
buffer, flush all three requests together. A later frame/buffer cancellation
during cooldown could similarly erase the last admitted work. Waiting
damage-aware requests also retained dead buffer handles until a new admission
or presentation; generated Wayland resources retain their data Arc and Smithay
SHM buffer data retains its pool mapping.

The authorized follow-up source patch prunes the bounded queued list once per
dispatch when nonempty, before admission, and requests a presentation for any
remaining plain request on no-deadline/no-eligible early returns. Retained
batches still validate their handles before copying. It introduces no new timer:
the continuation predicate excludes eligible requests and an empty queue, so
completed fanout cannot perpetually repaint. The exact same-burst native
regression against r4 observed one presentation but no ready plain frame and
timed out. The same fixture passed against r5. Staged buffer/frame cancellation
and the idle dead-buffer case also passed: all 256 destroyed buffers receive
failure before any replacement admission is issued. Evidence is preserved in
`capture-r5/regression-before/e2e/capture-e2e.log` and
`capture-r5/e2e/screencopy_pressure/capture-e2e.log`. All six pressure cases pass
again on r6, whose protocol implementation has the same source hash as r5.

The last cross-review of closed-panel sampling found a separate readiness
defect: a successful command returning empty/malformed Tailscale status could
authorize a cached action after reopening. The r6 Link correction requires a
successfully parsed current status before enabling that action and clears
readiness on subsequent invalid fresh status. Cached pixels may remain while
controls wait. Two unit regressions cover reopening, malformed/empty/wrong
schema responses, revocation after a valid opening sample, and recovery with a
valid new status. Both pass in `capture-r6/panel-units.log`. Source review also
confirmed visibility epochs reject older in-flight samples, completion nudges
survive active sampling, source IDs remain stable, and closed/hidden policy
does not create unbounded sampling workers.

No unresolved correctness blocker was found in the final source/evidence
review of PNG publication, clipboard lifecycle, cursor/chrome, dimming, pacing,
retained readback cleanup, cancellation or Link readiness. This is a bounded
review conclusion, not proof against every driver, client or resource flood.

Remaining limits are explicit, rather than hard-real-time guarantees:

- One image render/readback and one client buffer copy are not preemptible.
  The 2 ms budget is checked between operations; a large image, driver stall
  or page fault can exceed it. The first oversized operation deliberately
  remains admissible so large outputs make progress.
- The 64 MiB target budget covers retained texture payload, allows one larger
  necessary working target, and excludes driver overhead. One wlr download,
  up to two admitted screenshot pixel buffers, live output surfaces and
  client-owned SHM still consume memory. Queue/session caps constrain capture
  bookkeeping and work; they are not global per-client memory quotas or
  protection against every Wayland resource flood.
- Sparse dimming retains at most 64 hole states and falls back to full damage
  when history is unavailable. Capture-owned cursor rendering stays above the
  toolbar; offscreen capture omits these scanout overlays. Pacing scratch
  clears client/surface handles at its original lifetime boundary.
- Empty capture queues and a settled selector add no periodic deadline. A
  live clipboard/review helper still receives bounded worker housekeeping;
  recording labels tick once per second. Idle capture target retirement uses
  existing housekeeping and flushes deferred GLES deletion.
- Clipboard/file I/O runs on the bounded worker. Fsync, filesystem calls,
  process spawning and recording finalization can still delay later worker
  commands and orderly logout. The compositor does not wait for these during
  ordinary input/render dispatch, but this is not a promise of bounded disk or
  kernel latency.
- Sampling workers are bounded by declared sources, but general shell action
  batches still spawn individual threads with per-command timeouts. Ordering
  holds within a Sound migration batch, while rapid separate batches can
  interleave. A shared bounded executor/coalescing policy remains deferred;
  this campaign does not claim to bound arbitrary action floods or detached
  descendants of review launchers.
- Further ext readback sharing, per-client fairness/admission quotas, settled
  layout scan avoidance and physical low-memory/driver stress remain separate
  measured opportunities. The accepted FIFO queue progresses; a client holding
  the global session cap can still deny admission to another client. No
  speculative redesign was folded into the verified improvements.

## Rejected r7 nested input experiment

**Disposition: retain r6.** The r7 pre-render flush was reverted because the
bounded comparison did not demonstrate enough benefit to retain the extra
active-frame work. This rejects an unproven optimization; it does not establish
that r7 caused a latency regression. All 28 frozen r6 production/dependency
source hashes match after removing the exact ten-line experimental block.

The r6 constrained measurements remain unchanged. A subsequent read-only
investigation found a concrete delivery dependency but did not establish the
cause of the adverse 2 GiB endpoint values or the roughly 27 ms nested cadence.
In the three r6 2 GiB drawing phases, `render_us / render_calls` was
26.869/26.885/27.193 ms. Rendering accounted for about 5,025–5,031 ms of
5,035–5,042 ms recorded dispatch time. The corresponding baseline means were
27.325–28.677 ms. These counters place most elapsed time inside the rendering
boundary, which includes binding, drawing and submission; they cannot separate
those costs. Sources are the preserved `sample.json` files under
`comparison-capture-r6-2g-1280-mixed-capture`.

The nested backend has no compositor frame-clock deadline:
`session::next_render_deadline` returns `None` for Winit. Its advertised
60,000 mHz mode informs client callback throttling and presentation metadata.
Smithay pumps Winit with a zero timeout, and the reviewed changes add no fixed
frame delay. Its `vsync: false` setting selects an EGL configuration whose
allowed interval range includes zero; the vendored code does not call
`eglSwapInterval`. Mesa 26.2.1's Wayland software swap path can wait for the
previous host callback, with a default interval of one unless configuration
overrides it. This confirms a possible synchronous host dependency, without
proving which wait dominated these samples. See the exact
[Mesa swap path](https://gitlab.freedesktop.org/mesa/mesa/-/blob/mesa-26.2.1/src/egl/drivers/dri2/platform_wayland.c)
and [interval defaults](https://gitlab.freedesktop.org/mesa/mesa/-/blob/mesa-26.2.1/src/egl/drivers/dri2/egl_dri2.c).

The recorded host was Weston 15.0.1 with a 7 ms repaint window. Its headless
backend uses `weston_output_arm_frame_timer`, targeting the previous frame
timestamp plus the configured refresh period with millisecond rounding. The
historical fixed 16 ms headless timer is not the implementation in this host.
No exact 27 ms delay was identified. See the versioned
[headless backend](https://gitlab.freedesktop.org/wayland/weston/-/blob/15.0.1/libweston/backend-headless/headless.c)
and [frame timer](https://gitlab.freedesktop.org/wayland/weston/-/blob/15.0.1/libweston/compositor.c).

In the accepted r6 source, `state.rs::dispatch_pending` flushes outgoing client
events only after rendering, wlr copy service and pacing. A key already routed
to a client could therefore remain buffered through that frame's synchronous
drawing/host wait. The test-door barrier separately waits until scene damage
and queued backend events are clear, after the final client flush. The mixed
input probe times key injection to flushed real-client log receipt using
1 ms polling; its focus click and barrier occur before that timer. Both probes
include scheduling dependencies and neither measures physical input-to-photon
latency. Existing snapshots do not contain separate key-send/receive and
bind/draw/submit timestamps sufficient to attribute the 2 GiB anomaly.

After the r6 soak completed, the r7 experiment added one nonblocking
client flush immediately before a **Winit render attempt**, after focus/modal/
lock reconciliation. Its elapsed time was included in `frame_stats.flush`; the
existing post-render flush remained for frame callbacks and later replies.
There was no new state, timer, idle-only flush or DRM change. The cost was one
extra all-client scan on each active nested render attempt, including attempts
whose damage tracker ultimately submitted no pixels. It could not deliver input
which has not yet been dispatched, or remove earlier capture/readback waits.

The earlier alternative of flushing immediately before swap was rejected on
source grounds: `GlesFrame::finish_internal` may return an unsignalled EGL fence,
and the scene handles have already been cleared at that point. Pre-render
placement avoided advancing this frame's buffer-release events without an
additional GPU lifetime proof.

R7 passed its full correctness gate: 2,049 Rust cases and 48 Python harness
cases, followed by 248 native cases across 58 targets, including capture,
keyboard/focus, lock, pacing, clipboard/selection and screencopy pressure.
These results remain attributed to the experimental checkpoint. Logs and
source hashes are preserved in `capture-r7/{check-all.log,provenance.json,
e2e-results.json,e2e-assertion-summary.json}` under the artifact root above.

Three alternating r6/r7 pairs at 2 GiB retained all six passing process
samples. Typical changes were small or mixed: post-capture input process
medians were 28.895 → 27.663 ms, but the third r7 sample reached 184.622 ms
versus 27.731 ms for its paired r6 sample. Drawing/resizing barriers improved
in two pairs each, while their independent process medians worsened. Active
window reclaim was higher for r7 in every pair. The pressure observer's
250 ms cadence and missing exact input timestamps prevent assigning that tail
to reclaim or to the code change. The earlier independent r6 run's 160.256 ms
tail also remains recorded. Three pairs establish neither a reliable latency
tail distribution nor a pressured-input fix.

The experiment's scope had 44,656 memory.high events and no memory.max, OOM,
OOM-kill or CPU quota-throttling events. All 36 recorded process identities
exited and the owned cgroup was removed. Full results, stage counters, tail
analysis and the rejection recommendation remain in
`comparison-r6-r7-2g-1280-mixed-capture/{REPORT.md,RECOMMENDATION.md,
compact-pressure-report.json,frame-stage-analysis.json,input-tail-analysis.json}`.
The final product restores r6 exactly; no r7 latency improvement is included
in the delivered performance claims.
