# Campaign measurements

These are working results, not a release claim. See [WORK_LOG.md](WORK_LOG.md)
for executable identities, validation and unresolved work. Preserve individual
samples and unsuccessful reproductions alongside summaries.

## Input correctness: unchanged baseline versus input-only candidate

Real Wayland client, initial pointer position `(80, 70)`, held-button motion to
`(120, 90)` in surface-local coordinates:

| Committed client scale | Baseline delivered | Candidate delivered |
| --- | --- | --- |
| 1x | `(120, 90)` | `(120, 90)` |
| 1.5x | `(140, 100)` | `(120, 90)` |
| 2x | `(160, 110)` | `(120, 90)` |

The complete candidate matrix adds subsurfaces, motion outside the surface,
negotiated native DnD/drop, two independent touch contacts, cancellation and
contact-ID reuse: 12/12 pass. The exact baseline reproduces the scaled-pointer
failure. Touch cancellation was separately reproduced on the adapter before
the vendored dependency correction: all six touch cases failed to receive
cancel, then passed with the correction. Application-level evidence follows.

## Real Edge selection and browser repeat

Microsoft Edge 152.0.4191.53, native Wayland, private profile, same file-only
fixture and physical-input door on both binaries. The DOM caret starts at
character 8 and should end at character 24; the intended release is CSS
`(256, 113)`. DevTools observes results, never injects mouse or keyboard events.

| Output scale | Baseline release X / caret | Focus candidate release X / caret |
| --- | --- | --- |
| 1x | 256 / 24 | 256 / 24 |
| 1.5x | 333 / 32 | 256 / 24 |
| 2x | 410 / 40 | 256 / 24 |

The candidate passes plain and editable text selection in both windowed and
fullscreen modes at all three scales (12 selections), and held Right advances
at least ten characters then delivers key-up at each scale. Chromium passes
the same three-case matrix. The baseline passes the full 1x case and fails at
the first scaled drag in each of the other two cases; subsequent baseline
phases are therefore not evaluated. Baseline browser repeat already works at
1x: this is a repeat compatibility guardrail, not an Edge-repeat fix claim.

Screenshots verify actual compositor rendering as well as the DOM result.
The test dismisses Edge's native selection mini-menu with an outside click
before a subsequent selection. Wire tracing confirmed correct hover delivery
while that browser-owned UI suppressed page events. A test-only forced scale
initially multiplied the compositor scale, and a screenshot sample initially
intersected the browser's fullscreen banner; both fixture errors were corrected.

Evidence: `edge-selection-baseline-final.log`, `edge-selection-candidate.log`,
`chromium-selection-candidate.log`; `baseline/edge-selection-final/` and
`workspace-focus/edge-selection/` hold DOM coordinates, version metadata and
screenshots. Candidate SHA-256:
`77a35addc6c4ce771083a7e26eb6c110c0e438b6f70a51304024b301f3e90260`.
This demonstrates a concrete Edge reproduction/fix, not every website,
multi-monitor layout, GPU or XWayland browser configuration.

## Clipboard stalled-consumer retention

Seven alternating before/after pairs, 8,388,625-byte native source, private
XWayland requestor deliberately withholding the first INCR acknowledgement.
After sampling, the same requestor resumes and verifies every payload byte.
Both binaries already contain the integrity/startup corrections; this isolates
the outgoing backpressure patch, not the complete campaign against main.

| Metric | Before bounded staging | After bounded staging |
| --- | ---: | ---: |
| Added compositor anonymous PSS, median | 8,196 KiB | 0 KiB |
| Added anonymous PSS, observed range | 8,196–8,196 KiB | 0–0 KiB |
| Producer completed while consumer stalled | 7/7 | 0/7 |
| Resumed payload exactly correct | 7/7 | 7/7 |

This removes whole-payload retention from this path. The after value means no
additional resident anonymous pages at the `/proc` sampling granularity, **not**
zero allocation: the implementation has a bounded 64 KiB staging buffer plus
pipe/X11 transport state. Initial exploratory single samples were 8,204 KiB
before and 8 KiB after. The existing two-hour lifecycle soak ran concurrently;
these are not idle-CPU/startup samples or whole-desktop RAM comparisons.

Evidence: `clipboard-backpressure-pairs.jsonl` and
`clipboard-backpressure-pairs/` retain all 14 runs, their logs and payloads.
Before SHA-256:
`f907cf2f283d4db925ac9204ce717ac4b708949fe4d3226f2ba3e0e775171f9a`.
After SHA-256:
`1060a50ebad9d0be872078b25500dad95140b2b0cd7cdc95a1a0b917c717d8b0`.
The before tests intentionally report the failed backpressure assertion only
after full payload validation; any other failure rejects that sample.

## Two-hour desktop lifecycle soak

Immutable `x11-lifecycle` executable
`e055913e2f040a3a37fa0e872abacfc3618e3d3c0a54233f09d5d01fbac05d2b`,
before the later modal/clipboard changes. Private Weston/nested compositor,
real Foot windows, rotating CJK titles, Overview, Dock, theme reloads,
Hyprland-compatible queries and periodic real screencopy. Other campaign
builds/tests ran concurrently; this is a sustained-lifecycle test, not a quiet
CPU or frame-rate benchmark.

35,122 total cycles; measured interval 7,202.01 seconds after seven warm-up
cycles, including two seconds of final settling. Test passes its existing
bounded-growth and protocol-responsiveness assertions.

| Metric | Settled start | Settled finish |
| --- | ---: | ---: |
| Anonymous PSS | 62,400 KiB | 86,864 KiB |
| Non-pipe file descriptors | 40 | 40 |
| All file descriptors | 48 | 43 |
| Threads | 59 | 59 |
| Window / frame records | 0 / 0 | 0 / 0 |
| Shell records | 4 | 4 |

Anonymous growth is **24,464 KiB**, unresolved at this point. Passing a 64 MiB
growth guardrail does not establish leak freedom; allocation/cache/allocator
attribution remains required. Private-clean/PSS-file classification changed
as other processes mapped shared libraries, so it is not interpreted as heap
growth. Raw samples and full before/after mappings are preserved in
`soak-01-x11-lifecycle/stability-soak/`, with the full command log in
`soak-01-x11-lifecycle.log`.

## Input-only performance guardrail

Seven samples per binary in alternating order, 60-second idle measurement
after settling. Intel i9-9900K, Linux 6.18.46-1-lts, Rust 1.98.1, Weston 15.0.1
headless pixman host, nested llvmpipe compositor, 1280x800, hidden Dock.
No builds or other campaign test workloads ran during this batch. Page cache,
CPU frequency and ordinary user-session activity were uncontrolled.

| Metric (median) | Baseline `1e6db21` | Input-only candidate |
| --- | ---: | ---: |
| First scene | 275.04 ms | 263.43 ms |
| Wayland roundtrip ready | 274.82 ms | 263.16 ms |
| Compositor RSS | 180,324 KiB | 180,316 KiB |
| Compositor PSS | 123,342 KiB | 123,305 KiB |
| Compositor private memory | 109,444 KiB | 109,368 KiB |
| Process-tree PSS | 151,092 KiB | 151,068 KiB |
| Idle CPU (% of one core) | 0.0833% | 0.0667% |
| Context switches/second | 12.18 | 12.10 |
| Threads / file descriptors | 40 / 43 | 40 / 43 |

Interpretation: the correctness fixes show no meaningful retained-memory or
idle-resource regression in this setup. Do **not** turn these small differences
into performance claims. Memory differences are within run-to-run variation;
the idle-CPU median difference is only one 10 ms accounting tick per minute.
First-scene ranges overlap substantially (baseline 255.50-303.81 ms, candidate
260.90-296.02 ms); the earlier baseline-only batch's median was 262.57 ms.
That variation rules out claiming a reliable startup gain from this batch alone.

Raw evidence: `input-paired-hidden/` (metadata, summary, every sample and `/proc`
snapshot), `input-paired-benchmark.log`, and the preserved `baseline/bin/` and
`input-candidate/bin/` executables in the campaign artifact directory. These
numbers exclude the subsequent keyboard changes. No matched Hyprland comparison
or native DRM/KMS result has been obtained.

## Clipboard-completion checkpoint performance guardrail

Seven alternating pairs, the same hidden-Dock/1280x800 nested software-renderer
setup above, three-second settling and 60-second idle samples. Batch starts at
02:10 UTC on September 6. No other campaign builds, tests or soak workloads run
during sampling; ordinary user activity and kernel page cache remain uncontrolled.
The fixed executable includes the input, focus, repeat, X11 lifetime and clipboard
work through completed-transfer retirement, **not** the later memory diagnostics.

| Metric (median) | Baseline `1e6db21` | Clipboard-completion checkpoint |
| --- | ---: | ---: |
| First scene | 276.57 ms | 259.19 ms |
| Wayland roundtrip ready | 276.33 ms | 258.98 ms |
| Compositor RSS | 180,364 KiB | 180,836 KiB |
| Compositor PSS | 123,282 KiB | 123,766 KiB |
| Compositor anonymous PSS | 57,052 KiB | 57,232 KiB |
| Compositor private memory | 109,352 KiB | 109,852 KiB |
| Process-tree PSS | 151,051 KiB | 151,479 KiB |
| Idle CPU (% of one core) | 0.0833% | 0.0833% |
| Context switches/second | 12.10 | 12.11 |
| Threads / file descriptors | 40 / 43 | 40 / 43 |

No ordinary idle-memory improvement is established: median PSS increases 484 KiB
(0.39%), with overlapping ranges, and anonymous PSS increases 180 KiB. The ELF
text/data/BSS totals increase 53,312 bytes; that alone does not explain resident
memory, and file-backed page sharing also varies. All individual anonymous-PSS
samples are unchanged across their 60-second idle windows.

First-scene median is 17.38 ms lower in this batch, but ranges overlap (baseline
256.98–282.36 ms; checkpoint 253.74–262.91 ms), and an earlier baseline-only
median was 262.57 ms. This remains a regression guardrail, not evidence for a
general 6% startup claim or superiority over Hyprland. The material measured
memory win is the separately tested stalled-clipboard path, not idle desktop RAM.

Raw evidence: `clipboard-checkpoint-paired-hidden/` preserves all 14 samples,
metadata and `/proc` snapshots; command log is
`clipboard-checkpoint-paired-benchmark.log`. Baseline SHA-256:
`9277565dd563769110559caf00fcbd06701f6382e7fd4317e2b870bb97c9cff7`.
Checkpoint SHA-256:
`3885badf82399e55a2e8fe35124dfc2147774b3bc95907b09d281c2337e1e6a9`.

## Real Chonkcraft menu/geometry coverage

Unmodified Chonkcraft `fe7c787c936bd7c44647baf8e3b3ae063a7889ec`, private local
Maven build, app-jar SHA-256
`e50c91d8925b19d24a10874c090b51ba434c5339145073367cfeca7749d32137`.
JBR 25.0.2 b329.117, explicitly XToolkit/XWayland, private licensed pack,
network/device/profile-isolated Bubblewrap session, nested llvmpipe.

Both unchanged baseline and `selection-completion` pass all three scale cases:
1×, 1.5×, 2×, each checking four real menu actions (two windowed, two fullscreen),
exact AWT press/release coordinates within one logical pixel, and restoration
of the original window geometry. This adds coverage; **it does not reproduce or
newly fix the historical Chonkcraft click report**. The fractional-scale JBR
uses a 2× graphics transform, and the fixture measures that separately from the
compositor's scale. Fullscreen uses the compositor's ordinary binding here,
not the game's in-battle Alt-F implementation.

Logs: `chonkcraft-menu-{baseline,candidate}.log`; full raw sessions including
observations and screenshots are archived in the matching directories. The
first two fixture attempts are retained: a read-only sandbox mount target
needed to live below its private /tmp; then a fullscreen assertion incorrectly
expected frame records to unmap, although fullscreen retains a transparent frame.
Neither is represented as a compositor regression. See CHONKCRAFT_TESTING.md
for the isolation and observer contract. No game frame-rate result is claimed.

## Keyboard and X11 lifetime correctness

The original 11-case keyboard matrix passes four and fails seven against the
unchanged baseline. The lifecycle candidate passes all 13 expanded cases,
including 30 consecutive X11 map/focus/first-key/destroy cycles, as well as the
complete 138-case nested suite and three installed-Omarchy checks. These are
correctness results, not key-to-display latency measurements.

| Observable | Unchanged baseline | Lifecycle candidate |
| --- | --- | --- |
| X11 client input focus and held-key repeat | Focus never reaches the test window | Focus and repeated keys delivered |
| Legacy version-5 keymap's final byte | Newline, not required NUL | NUL |
| Repeat rate/delay zero | Rejected / old timing retained | Delivered; zero rate disables timers |
| Caps Lock / selected layout after unchanged reload and next physical modifier | Reset to `(0, 0)` | Retains `(2, 1)` |
| Records retained after a 12-window X11 client exits | 12 dead, unmapped records | 0 |
| X11 window identity through ten withdraw/remap cycles | Not separately quantified | Same identity retained; destroyed record collected |

The X11 collection result is a count of stale records, not a claim about bytes
saved per window. Baseline client-exit evidence is in
`x11-lifecycle-exit-baseline.log`; candidate keyboard/full-suite/preflight logs
have the `x11-lifecycle-` prefix. Candidate executable SHA-256:
`e055913e2f040a3a37fa0e872abacfc3618e3d3c0a54233f09d5d01fbac05d2b`.
Subsequent modal-focus/repeat changes and ongoing sustained churn are excluded
from these completed results until separately validated.

## Selection-device lifetime, including abrupt client exit

An observation-only ledger counts resources without cleaning them up. Before
the upstream Smithay destruction-callback backport, one disconnected client
leaves 128 dead objects in each of four seat lists: core data-device, primary
selection, wlr data-control and ext data-control. Normal disconnect, SIGKILL
and legacy core version 1 all reproduce this; explicit release succeeds.

After the backport, five cases each run four batches of 512 objects: all 10,240
objects retire with exactly the initial list lengths and zero dead objects
after every batch. This is a confirmed ownership leak fix, not an estimate of
bytes per object or attribution of the earlier two-hour soak's entire drift.

Before SHA: `b7150e74f2e5287cbba1d84ad3698bc2fe0eb7f0e137428cf1a6b3cfac661ec5`.
After SHA: `92d78da89267ee6dcb1aea42f07acdd358a808884b7006cc4b02f0fe4f50ae2e`.
Logs: `selection-lifecycle-{baseline,candidate}.log` and
`selection-device-cleanup-{preflight,full-e2e}.log`.

A second, matched two-hour churn run used the cleanup candidate
`21d7785a0e91b544b7291e370b685548feed16619736a95809df50c2f1d097c4`.
Both runs use the same software-rendered fixture and finish within 0.06 seconds
of 7,202 seconds; baseline completes 29,559 cycles and candidate 29,530. Because
the host swapped during both runs, resident anonymous PSS alone is misleading:
the comparison below uses `Pss_Anon + SwapPss` and the compositor's allocator
counter. Raw smaps, samples and client logs are preserved in
`memory-profile-soak-raw/stability-soak/` and
`selection-device-cleanup-soak-raw/`.

| Observable | Baseline | Cleanup candidate | Observed change |
| --- | ---: | ---: | ---: |
| Final live selection-device records | Not instrumented | 0 across all four lists | 0 retained in candidate |
| Non-pipe FDs, before -> after | 40 -> 40 | 40 -> 40 | flat in both |
| Threads, before -> after | 59 -> 59 | 59 -> 59 | flat in both |
| Rust requested-live growth | +16,548,860 B | +6,415,455 B | -10,133,405 B (-61.2%) |
| Rust requested-live peak | 40,100,177 B | 30,100,533 B | -9,999,644 B |
| Anonymous + swap PSS growth | +22,008 KiB | +9,600 KiB | -12,408 KiB (-56.4%) |

These are whole-process observations under this fixture, not a claim that every
byte of the difference is one selection object or that the allocator returned
the same pages to the OS in two independently scheduled runs. The attribution
is strengthened by a separate 306-second heaptrack pair: the baseline retains
two protocol allocation stacks (core data-device and primary-selection), each
110.88 KiB over 1,386 calls; neither stack appears in the candidate live set.
The candidate completes 1,379 cycles, has 4,380 fewer allocations live at
forced profiler termination overall, and reports a 0.67 MiB lower peak heap.
Heaptrack's `total memory leaked` includes all allocations live when the harness
forcibly terminates a healthy compositor, so that headline is not treated as a
leak count. Both two-hour compositor tests pass. Their enclosing shells exit
127 only afterward because the campaign edited the already-running script and
shifted Bash's continuation offset; installed-Omarchy checks were rerun
separately and pass.

## xdg move/resize authorization

The preserved pre-fix compositor accepts serial zero, a released serial, a
different serial while a grab is live, another client's live serial, and a DnD
serial as xdg move/resize authority. Its later leaked-grab safety net prevents
some invalid requests from moving pixels, but only after taking a WM grab and
sending the client a pointer leave/re-enter; the cross-client and DnD cases can
move a window while a button remains held.

The candidate validates the supplied seat, exact active grab serial, initiating
client identity, and excludes client/server DnD grabs before queuing wm-core
work. All eight real-client cases pass: five invalid authority paths remain
inert, while a legitimate held-pointer move and resize both change geometry.
Strict all-feature Clippy passes. Before compositor:
`f862029dbfaab28eba667e6b390fa192387d32f134a74e0f440fd272c018d725`;
after compositor:
`498f89d8530d414a66fc29480bc317305e0c3532dca99af62cbcf5811cceb0ea`.
Logs: `interactive-request-{baseline,candidate}.log`, plus focused
`interactive-request-{cross-client,dnd}-baseline.log`.

## Pointer constraints: count work before damage tracking discards it

The old `render_calls` metric counts successful submissions only. An independent
`render_attempts` counter now counts calls into rendering, with no additional
clock or allocation. Preserve an observation-only executable before changing
input behavior so the before/after counter has the same meaning.

| Workload / observable | Observation-only before | Pointer-constraint checkpoint |
| --- | ---: | ---: |
| 128 separately observed locked relative reports: render attempts | 128 | 0 |
| Same workload: submitted frames | 0 | 0 |
| 8,192-report burst at scale 2: exact raw relative reports | Not run | 8,192 |
| Same burst: render attempts / submissions | Not run | 0 / 0 |
| Lock/confine region activation cases, 1x/1.5x/2x | 0 / 6 pass | 6 / 6 pass |
| Full-surface confinement's first motion | Hangs; bounded test fails | Motion delivered |

Before SHA: `d4f132ce43003ad551763e5f86e49006d4416af8cc94f65aea1db262e44067a8`.
After SHA: `a782b6cb9a81f0c6addec2d9642667b81c77df0cfc2e0dacb439f81c55e4e653`.
Logs: `pointer-constraints-observed-baseline.log`,
`pointer-confinement-deadlock-baseline.log`, `pointer-constraints-candidate.log`.

These are software/nested correctness and avoided-work measurements. The burst
is unpaced, not a measured 8 kHz device; neither test establishes mouse latency,
CPU savings or game FPS. Strict preflight passes; the complete integration run
passes these nine cases, then exposes an existing Chromium parent-resize race
under investigation. Later region-commit/modal transition tests are separate
from this completed nine-case checkpoint.

## Incoming clipboard allocation traffic

The private incoming buffer helper is compiled unchanged into both Smithay's
XWM and the display-free regression. A thread-local System allocator wrapper
counts only the measured operation; owned X11 reply fixtures are allocated
before the scope.

| Workload | Previous algorithm | Offset/ownership buffer |
| --- | ---: | ---: |
| 8 MiB: 128 owned 64 KiB chunks, controlled 1 KiB writes; extra allocations | 8,192 | 0 |
| Same helper workload: requested allocation bytes | 272,629,760 | 0 |
| Real X11 → native clipboard and primary, each 1 MiB+17; `split_off` allocation calls with a 4 KiB consumer socket setting | 288 | 0 |

The 260 MiB figure is **cumulative allocation traffic in the helper workload**,
not resident memory or a measured whole-compositor saving. The real profile
still allocates X11 requests/replies; only unread-tail copies are zero. Both
real transfers compare every payload byte. An earlier real profile with the
default large socket buffer did not meaningfully pressure partial writes and
is not used to claim this result.

Before real SHA: `345bc187dd5fb06bd8ca2311123e17fe26fad27cb0fbd9d1097fea4d2a77c8d1`.
After SHA: `df49c6d480cc4c235140b9ada9f90ba239d06514eab2486f655b7ce0c9d2505d`.
Logs: `incoming-buffer-{baseline,candidate}.log`,
`incoming-buffer-candidate-e2e.log`, and
`incoming-buffer-bounded-heaptrack-{baseline,candidate}-report.log`.
The last pair was recorded with heaptrack, so no CPU/latency claim follows.

## Periodic network parser allocation budgets

Synthetic nmcli output contains 256 ignored virtual-device or bridge-profile
rows. No network command runs, and fixture construction is outside the scope.

| Ignored rows | Previous allocation calls / requested bytes | Borrowed parser |
| --- | ---: | ---: |
| Devices | 1,792 / 43,008 | 0 / 0 |
| Connection profiles | 1,948 / 43,602 | 0 / 0 |

All 19,531 short strings over an alphabet containing escapes, delimiters and
Unicode match the original splitter. Dedicated public-parser cases retain
escaped type/state lengths and mixed raw/escaped colons. The instrument suite
passes 247 unit cases (one optional test ignored) plus both allocation budgets.
Logs: `network-allocation-{baseline,candidate}.log` and
`network-parser-complete-tests.log`. These are per-parse allocation savings,
not a claim about idle CPU, desktop RSS or every possible malformed row.

## Dock clock path allocation budget

One warmed `render_clock_tile` call at the normal 56px dock size, measured by
the dev-only thread-local System allocator wrapper:

| Observable | One path per line | Batched tick paths | Change |
| --- | ---: | ---: | ---: |
| Allocator requests | 52 | 32 | -20 (-38.5%) |
| Requested bytes | 24,332 B | 24,002 B | -330 B (-1.4%) |

The candidate groups same-color, same-width tick marks into two paths; the
three differently styled hands remain separate. At radii below 3.5, overlapping
round caps require the old separate stroke order to preserve alpha compositing.
All 121 integer sizes from 8 through 128 compare byte-for-byte with a retained
pre-optimization renderer, and three representative pixel hashes are fixed as
long-term guardrails. Strict all-target, all-feature `wm-theme` Clippy passes.
Logs: `clock-path-allocation/{baseline,candidate,clippy}.log`.

This is an exact per-render allocation budget, not evidence of desktop RSS,
frame-time, startup-time or power savings. The 330-byte total reduction is much
smaller than the request-count reduction because the returned pixel buffer is
still the dominant requested allocation.

## Startup menu work and stale actions

The initial shell/WM handoff parsed the same Omarchy menu twice. A nested
fixture observes two loads before versus one after, and verifies one further
load on an explicit reload even with unchanged mtimes. No larger timeout or
retry was introduced. A separate unit case proves that replacing the complete
menu handle cannot let an old action generation resolve a different command.

Candidate SHA: `7c39b4c40c5087180b1fba499ee517031f6a38390788fcda825e867030f3a187`.
Logs: `omarchy-menu-startup-{baseline,candidate}.log`,
`omarchy-menu-generation-{baseline,candidate}.log`. One avoided parse is
measured work; its isolated startup-time effect has not yet been timed.
