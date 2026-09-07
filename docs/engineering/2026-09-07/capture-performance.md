# Capture and performance campaign, September 7, 2026

This campaign starts at `c74772ddc7e00ce483d107235b8ff025f846d4a0` on
`feat/capture-performance-audit`. The live desktop has not been restarted.
The accepted implementation is r6: the repaired capture workflow, sparse
repaint damage, bounded capture work and audited shell/startup changes.
Ordinary, constrained mixed, allocation and sustained stability checks are
complete. A further nested-input experiment, r7, was measured and rejected.
The host is an Intel Core i9-9900K; its unrelated desktop and services continue
running. Comparisons use private nested sessions and preserve individual samples.

## Capture behavior

The compositor owns the pointer while its capture selector is active, and over
the recording badge. Its arrow now renders above the toolbar even when a client
previously hid the cursor. Explicit cursorless exports still exclude it. Badge
interaction preserves an application's existing button grab and release.

The controls use consistent icons, spacing, rounded boundaries, selection state
and a prominent capture action. Controls and changing status text have separate
small retained buffers, shared session font state, and no idle redraw timer.
The I/O worker starts only when it has work.

Published screenshots open through `xdg-open`, respecting the configured image
viewer; this Omarchy installation associates PNG with imv. Finished recordings
open in `omacut`. Publication precedes launch. Review and notification helpers
are asynchronous and bounded, and filenames remain literal arguments. Missing
review applications leave the saved file intact.

## Repaint finding and ordinary release measurements

Four changing dimming rectangles caused Smithay to damage their complete old
and new bounds on each selection movement. Stable element IDs alone do not
prevent geometry-change damage. The replacement has constant output geometry,
draws translucent solids outside the selection, and reports the symmetric
difference of old and new selection holes. It allocates no fullscreen image.
A bounded history supports output/buffer ages; unavailable history falls back
to full damage. Virtual source coordinates invalidate relocated outputs.

Seven alternating baseline/`capture-r3` pairs used a 1280×800 private nested
session, llvmpipe, a hidden Dock, 120 input events/second and five-second motion
phases. All fourteen fixtures passed. These are medians across processes:

| Measurement | Baseline | r3 |
| --- | ---: | ---: |
| Drawing CPU, % of one core | 79.160 | 11.814 |
| Resizing CPU, % of one core | 77.174 | 10.622 |
| Drawing render calls in five seconds | 186 | 187 |
| Resizing render calls in five seconds | 185 | 187 |
| First selector open, ms | 71.060 | 62.892 |
| Warm selector open, ms | 26.685 | 26.728 |
| Initial scene barrier, ms | 272.564 | 271.834 |
| Closed idle PSS, KiB | 130,767 | 131,428 |

Drawing and resizing used **85.1% and 86.2% less compositor CPU** respectively,
with improvements in every pair. All settled idle phases had zero render calls.
The nested cadence remained about 37 Hz: less rendering work does not establish
higher hardware refresh or a hardware-GPU result.

First opening improved in six of seven pairs, with a median paired reduction
of 10.348 ms; one r3 run took 105.39 ms. Warm opening and startup did not show a
meaningful improvement. The median paired closed-idle PSS change was +901 KiB,
so this fixture does not establish general retained-memory savings. The final
motion barriers were 43.020 → 44.824 ms for drawing and 40.622 → 45.583 ms for
resizing, with overlapping ranges; CPU savings are not an end-to-end latency
claim. The old toolbar did not repaint its missing cursor, so its lower CPU
cannot serve as an equal-output comparison against the repaired cursor.

A separate diagnostic r1/r2 pair, excluded from timing claims, recorded median
damaged rectangle areas of 896,960 → 25,085.5 pixels while drawing and
862,812.5 → 18,233 pixels while resizing. Both used buffer age 1. Toolbar
damage remained 380 pixels. These logs support the repaint attribution.

The r3 binary predates the final panel, clipboard, joined-PNG and wlr fanout
changes. Three subsequent ordinary baseline/r5 pairs confirmed the CPU result:

| Confirmation measurement | Baseline | r5 |
| --- | ---: | ---: |
| Drawing CPU, % of one core | 78.98 | 12.04 |
| Resizing CPU, % of one core | 77.20 | 10.73 |
| Drawing render calls in five seconds | 186 | 186 |
| Resizing render calls in five seconds | 185 | 187 |
| First selector open, ms | 66.48 | 60.50 |
| Warm selector open, ms | 26.62 | 26.65 |
| Initial scene barrier, ms | 265.01 | 267.67 |

CPU improved in all three pairs: **84.7% less drawing CPU and 86.1% less resizing
CPU**. First opening improved in two pairs, with a median paired reduction of
4.63 ms. All idle phases again had zero renders. Drawing's final barrier was
46.34 → 40.99 ms and resizing's was 37.75 → 28.69 ms, both better in two pairs
with overlapping ranges. Closed-idle PSS had a median paired increase of 764
KiB. These data confirm reduced rendering work, with no universal startup or
retained-memory improvement.

The final r6 correction only changes Link-panel readiness after malformed
Tailscale responses, plus its tests. It does not change capture code. The r5
ordinary and r4 visible-Dock measurements remain explicitly identified; matched
pressure testing uses r6. Results from different checkpoints are not pooled.

## Matched memory pressure

Three alternating baseline/r6 pairs at each limit use two physical CPU cores,
a 200% CPU quota, two llvmpipe threads, disabled swap, and MemoryHigh at 75% of
MemoryMax. The private fixture contains a real foot client, native animation
and input clients, retained anonymous memory and 32 MiB of memory churn.
An observer outside the scope records pressure and process outcomes. The
[mixed workload guide](mixed-workload.md) specifies the frozen workload and
sampling boundaries.

The completed 1 GiB comparison retained 512 MiB of synthetic payload. All six
process samples passed, all 36 recorded process identities exited, and the
owned cgroup was removed. It recorded 101,932 memory.high events, with zero
memory.max, OOM, OOM-kill or CPU quota-throttling events.

| 1 GiB process medians | Baseline | r6 |
| --- | ---: | ---: |
| Drawing CPU, % of one core | 45.04 | 14.29 |
| Resizing CPU, % of one core | 44.39 | 13.39 |
| Drawing render calls | 133 | 182 |
| Resizing render calls | 130 | 177 |
| First selector open, ms | 91.30 | 81.89 |
| Client input after capture, ms | 69.35 | 28.14 |
| Drawing final barrier, ms | 192.16 | 42.45 |
| Resizing final barrier, ms | 103.58 | 39.74 |

Both motion CPU measurements and final barriers improved in every pair. Actual
reclaim severity varied: median active-window memory PSI some was 34.59% versus
28.88%, and full was 27.69% versus 22.35%. The median *paired* change in PSI some
was +0.37 percentage points; subtracting the two process medians would obscure
that ordering. Limits and payloads were identical, while observed pressure and
throughput were outcomes of each run.

Stalls remain visible. The slowest candidate drawing barrier was 163.68 ms;
baseline reached 218.39 ms, and its earlier independent calibration reached
297.76 ms. Passing a 500 ms fixture gate does not establish hitch-free behavior.
Three pairs cannot establish robust latency tail percentiles. These nested
software-rendered sessions do not establish physical display latency, total
machine memory consumption, or performance on older CPUs and native GPUs.

At 2 GiB, the retained payload was 1,280 MiB. All six samples passed, all 36
recorded process identities exited, and that cgroup was also removed. The scope
recorded 46,940 memory.high events and no memory.max or OOM events. One CPU
quota throttle lasted 1.867 ms outside the sampled active windows.

| 2 GiB process medians | Baseline | r6 |
| --- | ---: | ---: |
| Drawing CPU, % of one core | 60.28 | 14.57 |
| Resizing CPU, % of one core | 58.50 | 13.69 |
| Drawing render calls | 173 | 187 |
| Resizing render calls | 173 | 184 |
| Client input after capture, ms | 32.18 | 53.96 |
| Drawing final barrier, ms | 37.77 | 50.33 |
| Resizing final barrier, ms | 54.25 | 43.91 |

CPU improved in every pair, while post-capture input and the drawing barrier
were **slower in every pair**. The candidate's worst post-capture input check
was 160.26 ms, versus 32.28 ms for baseline. Resizing's barrier improved in two
pairs. Median memory PSI some was 6.86% versus 4.60%, but the paired median
change was +0.79 percentage points; full was 5.16% versus 3.71%, with paired
change +1.08 points. These adverse latency results prevent a universal
smoothness claim despite less CPU work and more rendered frames.

The follow-up inspection found lower sender lateness, shorter render durations
and more animation callbacks in r6's drawing phases. Those observations do not
explain its slower final barrier. Two slower post-capture input checks occurred
in bracketing windows with no memory PSI or high events, so reclaim cannot
explain every adverse result. Nested frame alignment is a hypothesis; the
fixture lacks the event-to-presentation timestamps needed to establish it.
Continuous timestamped input/presentation tracing on native hardware is the
next measurement priority. The [adverse analysis](data/capture-r6-2g-1280-adverse-analysis.json)
retains this evidence and the attribution limits.

## Component costs and allocation bounds

Application collation at 64, 512 and 4,096 distinct IDs fell from 65.4 → 47.5 µs,
2.003 → 0.456 ms and 122.79 → 3.244 ms respectively in the r3 component benchmark.
The fixture checks the exact old override and sorting behavior. These numbers
measure catalogue processing, not complete application startup.

The layout solver's 54 baseline/candidate scenarios have matching inclusion and
placement digests. On the adversarial 512-window cases, baseline calculations
took 214–260 ms and the candidate took 4.36–7.05 ms, improvements of 31.4–59.6×.
These deliberately difficult thin-wide/thin-tall inputs expose impossible row
or column minima despite their total minimum area fitting the screen. Ordinary
small desktop layouts do not inherit that factor.

PNG encoding now joins each Sub filter byte with its demultiplied row before
feeding the same fast compressor, then emits bounded IDAT chunks. This preserves
the compressor's efficient grouping, which a generic streaming writer had lost.
All alpha values and decoded pixels match the previous encoder. Latched writer
failures, including partial chunks, CRC, final checksum, IEND and flush, prevent
publication. The r4 component benchmark measured:

| PNG fixture | Old encoder, ms | Bounded encoder, ms |
| --- | ---: | ---: |
| 1920×1080 structured | 11.464 | 12.201 |
| 1920×1080 noise | 17.496 | 17.566 |
| 3840×2160 structured | 51.737 | 47.604 |
| 3840×2160 noise | 117.105 | 69.881 |

Each median contains three alternating measured rounds after warmup. Fixture
construction, allocation-counting instrumentation, filesystem I/O and fsync are
excluded from those times. Separate allocation counting recorded three requests,
138,753 bytes at 1080p and 146,433 bytes at 4K, versus 23–370 MB requested by the
old encoder. Requested bytes are cumulative allocator traffic, not peak RSS;
the already-owned input image is additional. Noise output is approximately 8.9%
larger because the bounded encoder does not perform the old whole-image stored
fallback. There is no universal encoding-speed or compression-ratio win.

## Whole-process allocation diagnostics

Separate baseline/r6 `--features memory-profile` binaries ran three alternating
ordinary fixture pairs. All six samples passed, and all 12 recorded process
identities exited. These instrumented builds supply allocation counters only;
none of their timing or CPU values is used for a speed claim.

| Phase | Allocation operations, baseline → r6 | Requested MiB, baseline → r6 |
| --- | ---: | ---: |
| First selector opening | 19,006 → 2,654 | 3.908 → 1.114 |
| Ten warm open/close cycles | 4,220 → 1,511 | 5.829 → 0.056 |
| Drawing, 600 motion events | 31,344 → 12,954 | 57.224 → 18.031 |
| Resizing, 600 motion events | 31,284 → 12,477 | 57.209 → 17.227 |

Every requested-byte and operation reduction in the table held in all three
pairs. Drawing requested 68.5% fewer bytes and resizing 69.9% fewer. Median
render counts were 186 → 187 and 186 → 186 respectively; the savings do not
come from dropping most frames. Ten warm open/close cycles requested 99.0%
fewer bytes. Settled idle phases also had less allocator traffic.

The repaired toolbar adds visible cursor and hover work: its motion phase
requested 0.660 → 26.553 MiB and rendered 1 → 188 times. That comparison has
different visible output and is retained explicitly. Closed-idle live Rust
bytes increased by about 51 KiB. Requested bytes count cumulative successful
Rust allocation/reallocation requests, including full replacement sizes and
test-door/background work; they are not resident bytes, C/driver allocations
or an exclusive capture-function attribution. Full counters and paired ranges
are in the allocation records under [data/](data/).

## Visible Dock and closed panels

A separate healthy-command fixture ran one validation pair, then three
alternating baseline/r4 pairs with two 20-second closed-panel phases. It uses
real sampler workers and deterministic command replies in a private PATH; it
never contacts live audio/network services or executes control actions. All four
command-backed tile sources must remain active in every phase.

| Median measurement | Baseline | r4 |
| --- | ---: | ---: |
| Panel command starts/sec, before first opening | 3.45 | 0 |
| Panel command starts/sec, after panels close | 3.75 | 0 |
| Reaped helper CPU, % of one core, before opening | 0.700 | 0.400 |
| Reaped helper CPU, % of one core, after closing | 0.749 | 0.400 |
| Total threads, before opening | 35 | 28 |
| Total threads, after opening both panels | 35 | 34 |

Six panel workers do not start until needed, and the capture worker remains
lazy. After both panels have opened, those six workers are parked rather than
removed; the command savings remain. Compositor CPU was essentially unchanged
and noisy, about 0.75–0.80% of one core. Initial closed PSS was 129,691 → 129,823
KiB and later closed PSS was 131,917 → 132,109 KiB: no retained-RAM saving is
established here. The cheap synthetic commands do not measure the actual cost
of pactl, nmcli, BlueZ or Tailscale services.

Fresh panel commands began 1.0–1.7 ms after opening in r4. The baseline waited
for existing periodic schedules, with source medians of 82–348 ms for Sound
and 984–2,903 ms for Link. Surface mapping was 27.20 → 27.41 ms for Sound and
26.99 → 39.38 ms for Link. The forced-frame/queued-event settling barrier was
27.35 → 52.92 ms and 27.11 → 80.09 ms respectively; these regressions remain
visible. The cold Link panel initially maps at 230×163 and grows to 230×286 as
fresh rows arrive, matching the baseline's populated dimensions. Neither the
mapping nor command-start measurements establish data-ready/input-ready latency.

## Broader audit and implemented changes

- Application collation uses indexed deduplication while preserving override
  priority, first-source ties and final sorting. A shuffled catalogue is checked
  against the previous algorithm.
- Dock hit testing iterates without a slot allocation. Rendering skips fully
  clipped widget faces while updates and lifecycle handling continue.
- Closed audio/network panels stop seven periodic command sources after
  discovery. Tile readings continue. Opening requests fresh panel data;
  accepted effects and bounded confirmations finish even after closing.
  The final Link readiness correction requires a successfully parsed Tailscale
  status before enabling its action; missing or malformed fresh replies revoke
  stale readiness. Two regressions cover invalidation and recovery.
- Samplers drain bounded nonblocking pipes while waiting for exit. A complete
  successful response and EOF must arrive within one deadline. Large responses
  and inherited pipe writers have real subprocess regressions.
- Surface pacing avoids materializing FIFO state and resolving visibility for
  ordinary surfaces. Reused scratch releases all resource handles per pass.
- Ext capture uses bounded FIFO batches and admission limits, explicit pending
  deadlines, and no capture-generated idle timer. First-frame, orphan-frame,
  lock, overflow and unrelated-input tests cover protocol behavior. A single
  GPU readback remains non-preemptible.
- Wlr screencopy also caps accepted pending consumers at 256. Matching requests
  share one owned GPU download while copies yield after four buffers, four
  Mi pixels or two milliseconds between copies. One oversized image can
  make progress. A four-millisecond cooldown preserves input opportunities;
  an empty queue has no deadline. One geometry per output is admitted by each
  presentation, and newcomers do not inherit an old cohort's eligibility.
  The retained download is additional to the target cache and is bounded to
  one image. Lock changes discard undelivered pixels from the previous state.
  A late adversarial review found that canceling an admitted geometry could
  strand a different plain request, and destroyed buffers could retain SHM
  mappings while idle. The r5 fix prunes dead queued resources at dispatch and
  preserves the remaining plain requests' presentation. The previous r4
  executable reproduces the cancellation timeout; r5 passes the same regression
  and five other native cancellation/fanout cases.
- Capture targets have a 64 MiB retained pixel budget and an eight-entry bound.
  One larger working target can stand alone to preserve continuous large-frame
  capture. Eviction and GL deletion precede replacement allocation. Targets
  retire after five idle seconds through existing housekeeping. Pixel bytes
  exclude driver overhead and client buffers.
- PNG output streams through bounded scratch with exact alpha conversion and
  explicit Fast/Sub settings. Both finish layers and underlying write failures
  must succeed before publication. Icon previews borrow their source pixels.
- Screenshot review launches immediately after publication. Clipboard providers
  are checked asynchronously in order, with admission held until completion,
  replacing the fixed worker sleep while preserving the prior clipboard on
  helper failure.
- The environment publisher reads appended log bytes rather than replaying
  session history. Tests cover rotation, truncation/regrowth, late XWayland,
  split/oversized records and bounded handling of NUL input.
- The layout solver rejects impossible row/column minima early. The preserved
  baseline spent 214–260 ms on 512 mixed thin-wide/thin-tall windows despite
  total minimum area fitting. Exact placement digests guard the changed path;
  the candidate takes 4.36–7.05 ms in those cases.

The [render/input audit](../2026-09-07-performance-audit-render-input.md) and
[startup/shell audit](audit-startup-memory.md) record reviewed areas and remaining
findings. Their inventories describe source inspection, not an exhaustive proof.

## Reproduction and evidence

Use ordinary release executables for timing. Preserve `--features memory-profile`
builds separately for allocation diagnosis. Do not run compilation, tests or
other benchmark workloads concurrently with controlled comparisons.

```sh
python3 scripts/bench-capture.py \
  --binary before=/absolute/path/to/preserved-before \
  --binary after=/absolute/path/to/preserved-after \
  --output /tmp/chonk-capture-comparison --runs 7
```

`scripts/bench-pressure.py` confines only a newly launched fixture and observes
its cgroup externally. A memory ceiling does not by itself demonstrate pressure:
report the actual PSI/events, process outcomes and working set. Cached shared
pages may be charged outside the fixture, and modern CPU/cache/storage behavior
does not become that of an older machine through affinity or quotas.

## Validation and limits

The r5 checkpoint passed `scripts/check.sh all`: strict workspace/all-target
Clippy, documentation, workspace tests, Wayland tests and Python harness tests.
The log contains 2,047 passing Rust test cases across unit, integration and
documentation test groups; tests requiring a display are run separately.

The broad native r4 run covered 58 test targets, with 246 functional cases
passing after correcting a cache test's handling of the advertised output
transform. Three targets passed their assertions but encountered an artifact
copy error on Chromium's dangling singleton symlinks. The runner now preserves
symlinks and records test status before copying artifacts. Original failures
and corrected runs remain available rather than being overwritten.

The r5 protocol correction passed 31 native cases across eight affected targets,
including the exact cancellation regression that times out on r4. Final r6
passed strict all-target Clippy, 677 instrument/shell unit cases, and native
panel, capture, screencopy-pressure and idle checks. The initial r6 capture run
had two fixture failures: a diagnostic filename marker was visible while still
empty, invoking the supported default screenshot path. Publishing the marker
by atomic rename fixed the test race; the same frozen r6 product passed all
11 capture cases. Both ordinary and mixed benchmark fixtures use that corrected
publication method, outside timed phases. All 48 final Python harness tests
passed.

The ordinary r6 sustained run completed 602.2 measured seconds and 1,813 desktop
cycles after warmup. It repeatedly opened real foot clients, exercised Overview
and Dock visibility, switched themes, queried compatibility IPC and captured
frames. All growth guards passed; all 184 snapshots had zero dead selection
objects. Threads stayed at 25, non-pipe descriptors at 40, and total descriptors
changed from 44 to 43. Retired windows, frames and shell surfaces did not
accumulate. Anonymous-plus-swap PSS changed from 62,416 to 63,704 KiB (+1,288),
with a sampled maximum of 69,092 KiB. Total PSS increased by 21,671 KiB; the
anonymous growth result is not a claim that total resident memory was constant.

This soak used the same private nested renderer, two physical cores, a 2 GiB
ceiling and disabled swap. It added no synthetic memory payload. Memory high,
max and OOM events and CPU quota throttles were all zero, so it is a sustained
cleanup check rather than an additional reclaim experiment. The owned scope
exited and was removed. The final build restored to the shared target directory
was byte-identical to the frozen ordinary r6 executable; diagnostic builds
remain separately preserved.

The full validation history, including the additional experimental gate below,
is preserved in [the checkpoint ledger](data/validation-checkpoints.json).

Remaining audit opportunities include a shared bounded executor for shell
actions, repeated Hyprland configuration parsing, appearance-worker ordering,
and serial X11 round trips. Individual capture helpers are bounded, but general
shell actions still create a thread per batch and use per-command deadlines;
separate action batches can interleave. The `date` and `xdg-user-dir` helpers,
filesystem synchronization and recorder finalization can block the capture I/O
worker. They do not execute on the compositor's input thread. None of these
deferred areas is included in the measured gains above.

Native DRM/GPU, physical low-memory hardware, M1/Asahi and sustained 4K60 video
encoding remain unmeasured. Startup and retained idle PSS did not improve
meaningfully in the recorded whole-process comparisons.

## Rejected nested-input experiment

R7 added one nonblocking client flush before active Winit rendering, preserving
the final completion flush and avoiding extra idle or DRM work. Source review
confirmed that already queued input could otherwise wait through synchronous
nested drawing/submission. The candidate passed the complete
`scripts/check.sh all` gate (2,049 Rust and 48 Python cases) and all 58 native
targets (248 cases), with successful artifact collection and no surviving
fixture process found. Correctness did not establish a speed improvement.

Three alternating r6/r7 pairs repeated the exact 2 GiB/1,280 MiB mixed workload.
All six samples passed and all 36 recorded process identities exited; the
owned scope was removed. Input-after values were 53.296/28.895/27.731 ms for
r6 and 27.663/27.265/184.622 ms for r7. The process median improved only from
28.895 to 27.663 ms, with improvement in two of three pairs. Drawing/resizing
barriers were mixed, and CPU/frame counts substantially overlapped. Aggregate
drawing flush time increased from 3.470 to 4.490 ms over approximately five
seconds; this was a modest cost, not a demonstrated cause of the large tail.

Actual memory PSI and high events were higher for r7 in every pair despite
identical limits and payloads. Its outlier's bracketing window included 92.138
ms of memory PSI some, but the available timestamps cannot attribute that to
the precise key interval. Earlier independent r6 data also contained a 160.256
ms input tail. These observations do not prove r7 caused a regression, and do
not justify discarding the outlier or claiming it fixes pressure latency.

The ten-line experiment was removed because the measured benefit was
insufficient. Accepted production sources match frozen r6; the experimental
source, binary and every result remain available. Its executable SHA-256 is
`f58b224a46a354be407d60780f120de90d9e6a488f99766058ac266b230ca31a`.
Timestamped client/input/render/presentation tracing is a better next step
than adding another flush based solely on a plausible mechanism.

## Artifact locations

Local raw evidence is retained under:

```
/home/chrisk/src/chonkstep-capture-performance-artifacts/2026-09-07/
  baseline/                  preserved c74772d binary and baseline report
  capture-r1/ ... capture-r6/ preserved builds, source hashes and gate logs
  capture-r4-fixture-fix/    output-transform test correction
  capture-r6-marker-fixture-fix/ atomic marker and eleven capture cases
  comparison-capture-r3-seven/ seven ordinary release pairs
  comparison-capture-r5-three/ three final capture confirmation pairs
  comparison-capture-r6-1g-512-mixed-capture/ matched 1 GiB comparison
  comparison-capture-r6-2g-1280-mixed-capture/ matched 2 GiB comparison
  comparison-capture-r6-memory-three/ separate allocation diagnosis
  baseline/memory-profile/ and capture-r6/memory-profile/ diagnostic binaries
  capture-r6-soak/ and capture-r6-soak-scope/ sustained churn and scope evidence
  capture-r6-final-validation/ resource summary and cleanup
  comparison-r6-r7-2g-1280-mixed-capture/ rejected experiment and all outcomes
  accepted-r6/             source verification, decision and restored build
  campaign-r6-prepared/     commands, execution and cleanup records
  panel-sampling-three/    three visible-Dock/panel pairs
  damage-r1-r2-one/          separate damage diagnostics
  validation/              full gate logs, including failed attempts
```

The solver's raw baseline, source hashes and preserved test executable are under
`/home/chrisk/src/chonkstep-engineering-artifacts/2026-09-07-render-input-audit/mosaic/`.

Compact paired results and fixture metadata are checked in under [data/](data/).
The baseline ordinary executable SHA-256 is
`b9dbcfb9ff13039980c6eb750bea34533fffdb7b74074f33bf53f433189771f1`;
final r6 is
`315598fbe0b8a49eec2e0c3654331472dfd67c2f5946aaef54caa9077ab6da8f`.
Source freezes bind each measurement to its implementation and test executables.
