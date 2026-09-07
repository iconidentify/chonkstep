# Compositor performance measurements

Performance claims should name the source revision, build profile, renderer,
output geometry, configuration, clients, and measurement interval. The harness
in `scripts/bench-compositor.py` writes individual samples and raw process
accounting so a reported median can be checked against the underlying runs.

## Capture and pressure campaign — 2026-09-07

The [capture performance report](engineering/2026-09-07/capture-performance.md)
records the cursor/toolbar repair, ordinary capture CPU comparisons, bounded
readback and encoding work, and the subsequent constrained workload campaign.
It starts from the newer `c74772d` baseline; keep it separate from the September 5
results below.

## Final before/after result — 2026-09-05

The original 0.3.0 preview executable was compared with `combined-7`, the final
candidate including the sampler-resume correction (`eb5f554`). Seven alternating
pairs used 60-second hidden-Dock idle windows after four seconds of settling,
with no overlapping builds or stress workloads from this investigation. The
host's unrelated desktop/services continued running. Both sides used the same
1280×800 scale-1 fixture, llvmpipe renderer, enabled XWayland and compatibility
IPC. Exact hashes and every numeric sample are in `final-{metadata,summary}.json`
and `final-samples.jsonl` under the checked-in data directory.

| Hidden-Dock nested compositor | Before median | After median |
| --- | ---: | ---: |
| CPU, one-core percent | 0.233% | 0.0833% |
| Context switches / second | 45.92 | 12.10 |
| Threads | 58 | 40 |
| RSS | 181,036 KiB | 180,480 KiB |
| PSS | 124,032 KiB | 123,337 KiB |
| Compositor + descendant PSS | 151,668 KiB | 151,105 KiB |
| First-scene rendering barrier | 280.34 ms | 265.30 ms |

That is about **64% less compositor idle CPU** and **74% fewer context
switches**, with 18 fewer threads in this never-shown-Dock fixture. All fourteen
idle intervals recorded zero render calls, and descriptors stayed at 43.
Retained idle memory was essentially unchanged. First-scene medians improved
5.4% in this batch, but ranges overlap (266–292 ms before, 258–288 ms after);
the independent preceding batch improved only 1.9%. Do not describe this as
a universal startup percentage or as a cold-login/scanout measurement.

The separately measured **visible Dock** does not inherit those idle savings.
Seven alternating pairs used 30-second intervals and four seconds of settling:

| Visible built-in instruments | Before median | After median |
| --- | ---: | ---: |
| Compositor CPU, one-core percent | 1.033% | 1.033% |
| Context switches / second | 260.62 | 260.68 |
| Threads | 58 | 58 |
| RSS | 187,376 KiB | 187,900 KiB |
| First-scene rendering barrier | 306.13 ms | 325.91 ms |

Active resource use is essentially unchanged. The visible-Dock first-scene
median regressed **19.78 ms (6.5%)** in this batch, with ranges of 294–344 ms
before and 308–344 ms after. Wayland-sync readiness alone improved
306.00 → 298.35 ms; reporting only that would hide the later rendering cost.
Source inspection identifies deferred initial sampler activation as a candidate
for follow-up profiling, not a measured attribution or a completed fix. Preserve
hidden-start laziness and correct resume generations when investigating it.
Raw records are `dock-{metadata,summary}.json` and `dock-samples.jsonl`.

The larger memory win is in the separate glyph-churn workload: final-phase
stress-process RSS fell 140,892 → 25,128 KiB (82%), with a documented reraster
cost for very large working sets. A 4096-character panel label fell from
1847.660 → 1.745 ms; ordinary sparse decoration stayed fast. These are actual
library workloads with matching pixels, not whole-desktop speedup factors.

The implementation fixes X11 autostart display publication, GLES 2 readback,
nested retained-buffer damage tracking, sampler lifetime/resample races,
redundant pixmap copies and unchanged embedded-wallpaper work. Validation and
the remaining NVIDIA, native-hardware, retention and competitor-comparison
limits are recorded below. No matched Hyprland superiority claim is established.

## Repeatable nested sessions

```sh
cargo build --release -p chonkstep-wayland
python3 scripts/bench-compositor.py \
  --binary candidate=target/release/chonkstep-wayland \
  --output /tmp/chonk-bench-candidate --runs 7 --idle-seconds 10
```

Save the baseline executable before rebuilding. Pass both executables to
alternate their run order under the same host:

```sh
python3 scripts/bench-compositor.py \
  --binary before=/path/to/saved/chonkstep-wayland \
  --binary after=target/release/chonkstep-wayland \
  --output /tmp/chonk-bench-comparison --runs 7 --idle-seconds 10
```

The output directory must not already exist. Use a short path: Wayland and
Hyprland IPC use Unix sockets, whose filesystem paths have a small length
limit. The harness starts its own D-Bus session and headless Weston, isolates
configuration/state/cache/runtime directories, and terminates the compositors
it starts. It does not replace the active desktop or change its preferences.

The default fixture is a 1280×800 nested compositor at scale 1, the
`nextstep-classic` theme, no hosted Omarchy shell, no autostart or session
restore, and a hidden Dock. Hyprland IPC and XWayland remain enabled. Add
`--dock` to measure the visible built-in instruments. Weston uses its headless
pixman renderer; the nested compositor uses Mesa llvmpipe unless `--hardware`
selects the environment's default renderer. Always verify the actual renderer
in the saved compositor log; the switch does not guarantee hardware rendering.
For hardware nesting, add `--host-renderer gl` and explicitly select the EGL
vendor/device through that driver's environment controls. This still opens no
physical output: both the host and the tested session remain nested/headless.

Each run gets an empty shader cache directory. Kernel filesystem caches are
uncontrolled and generally warm; the harness never drops system caches. These
results describe isolated nested sessions, not display-manager login, native
DRM/KMS, suspend/resume, power use, or every supported GPU.

The baseline and candidates use the same workspace release profile:
`codegen-units = 1`, thin LTO and line-table debug information. This work did
not change compiler optimization flags or the dependency lockfile. Saved
executables retain their build-time version metadata; intermediate candidates
were built before their changes were committed, so their version strings say
`preview-v0.3.0-dirty`. Use the recorded SHA-256 values for exact identification.
The package and font inventories are archived as `packages.txt` and `fonts.txt`.
Pixel checksums compare before/after on this font installation; they are not
portable golden values for machines with different fallback fonts.

The measurements are:

- `socket_ms`: process creation to the Wayland socket appearing.
- `ready_ms`: process creation to a real `wl_display.sync` reply. This requires
  a running event loop, not merely a listening socket.
- `frame_ms`: process creation to the compositor test door's rendering barrier.
  It proves the initial scene has rendered; it is not scanout latency.
- `cpu_percent`: user and kernel CPU time of all compositor threads over the
  measured idle interval, with 100% meaning one fully occupied logical CPU.
  Short intervals have scheduler-tick quantization; a zero sample is not proof
  of absolutely zero CPU cost. Child-process CPU is recorded separately in the
  raw snapshots and is not included in this percentage.
- `rss_kib`, `pss_kib`, and `private_kib`: resident, proportional, and private
  resident memory from `/proc/PID/smaps_rollup`. `tree_pss_kib` also includes
  the compositor's descendants observed at the sample. These are CPU mappings,
  not complete GPU memory or peak total-session resource accounting.
- `context_switches_per_second`: voluntary plus involuntary switches across
  compositor threads. This is not a hardware wakeup or energy measurement.

The Linux kernel documents [the process memory accounting fields](https://www.kernel.org/doc/html/latest/filesystems/proc.html).
The current harness additionally checks a real Hyprland-compatible `j/version`
reply, validates the longer IPC socket path before launching, and records its
own source hash, CPU affinity and driver environment. This prevents a missing
compatibility socket from masquerading as a cheaper complete fixture.
The `frame-stats` sample also records compositor dispatch work and actual
render calls, so a lack of visible activity can be checked against rendering.

Run integration tests against the optimized binary with:

```sh
scripts/e2e.sh --headless --release
```

Use a separate, short `TMPDIR` when another checkout is running the suite:
the harness stores per-test directories beneath `$TMPDIR/chonk-testkit`.

## September 2026 improvement log

Work began at 2026-09-05 17:20 UTC in the `codex/performance-2026-09-05`
worktree, based on the 0.3.0 preview commit
`9d4d009cd46105e3cf3287e8ff71a8d3899a5023`. Small raw measurement series are
checked in under [`performance/2026-09-05`](performance/2026-09-05/).
The release checkout and the user's running compositor were not modified or
restarted. Review the performance branch as a whole: the later sampler-resume
correction (`eb5f554`) is part of the implementation, not an optional benchmark
change to omit when taking the earlier hidden-Dock optimization.
The complete local artifacts and preserved executables are archived to
`/home/chrisk/src/chonkstep-performance-artifacts/2026-09-05/` from the working
directory `/tmp/chonk-perf-20260905/`. The binary hashes identify each measured
intermediate implementation; do not assume every table measures the final one.

The first seven-run baseline used an i9-9900K, Linux 6.18.46 LTS, Rust 1.98.1,
Weston 15.0.1 and Mesa 26.2.1 llvmpipe. The baseline median was 307.21 ms to
the rendering barrier, 179,404 KiB RSS, 119,744 KiB PSS, 145,464 KiB descendant
PSS, 58 threads, and 0.30% compositor CPU over ten-second idle intervals.
These initial numbers are a diagnostic baseline; paired comparisons use the
same current harness for both executable versions.

The first identified waste was built-in instrument sampling while the Dock
was hidden, including the default Omarchy posture. The implementation now
prepares source ids and effect handles at construction, starts sampler threads
on first show, pauses them while hidden, and wakes them for a fresh reading
on show. An already-running read may finish after hiding; it cannot block the
repaint thread. Dropping a sampler signals thread termination. The same audit
found and fixed a resample request being lost when an effect finished during
an in-progress sample.

A final audit caught a resume edge case in the new pause behavior: an unread
counter produced before or during hiding could be consumed just after showing,
followed immediately by a current counter. A network-rate widget would divide
that large second delta by the short time between folds, creating a false spike.
Readings now carry a visibility generation captured when the read starts, so
only the current period is folded after showing. An unavailable source remains
reportable across visibility changes. The deterministic regression fails on the
pre-correction implementation and covers completion before hide, while hidden,
and after reopening. Its before/after logs are `sampler-resume-{before,after}.log`.
This adds no sampler thread or per-reading allocation. Hidden histories are
paused; the first fresh counter rate covers the interval since the last visible
reading, instead of pretending that interval was continuously sampled.

The sampler change passed 14 sampler tests and the complete release Wayland
integration suite and installed Omarchy fixture checks (111 checks altogether).
Its seven-pair comparison gave these medians:

| Dockless nested session | Before | After |
| --- | ---: | ---: |
| Threads | 58 | 40 |
| Context switches / second | 48.20 | 15.17 |
| Compositor CPU, one-core percent | 0.399% | 0.200% |
| Render barrier | 337.36 ms | 334.97 ms |
| PSS | 121,134 KiB | 120,420 KiB |

This is an idle-efficiency result: startup and retained memory changed only
slightly. The CPU intervals are ten seconds, so longer runs are appropriate
before treating the percentage reduction as a precise power-saving claim.

After the sampler, pixel-copy, label-fitting and X11-startup changes, a second
seven-pair comparison used **30-second** idle intervals after four seconds of
settling. Its raw directory is `combined1-ab`, and the after binary hash is
`2f6ed15287f330ca303bda639b9d987b2c46ff46698fde10dcfd67ef98c49ec8`.

| Dockless nested session, longer interval | Before median | After median |
| --- | ---: | ---: |
| Compositor CPU, one-core percent | 0.300% | 0.100% |
| Context switches / second | 46.18 | 13.06 |
| Threads | 58 | 40 |
| Render barrier | 307.68 ms | 296.46 ms |
| PSS | 119,467 KiB | 120,278 KiB |
| Descendant PSS | 145,117 KiB | 145,943 KiB |
| File descriptors | 43 | 43 |

The measured CPU reduction is about 67%, context switches about 72%. Both
versions rendered zero frames during the recorded idle intervals. Startup
ranges overlapped (282–327 ms before, 282–303 ms after), and retained idle
memory was essentially unchanged. These are not GPU-power measurements.

The later `combined-6` comparison used seven alternating pairs with **60-second**
idle windows and four seconds of settling, after all stress tests and builds
had stopped. Its median compositor CPU was 0.250% → 0.083%, context switches
46.19 → 12.15 per second, threads 58 → 40, and RSS 181,120 → 180,420 KiB.
All fourteen intervals recorded zero render calls. The first-scene barrier
was 272.04 → 266.88 ms, with overlapping ranges of 268–296 and 262–280 ms.
This independently repeats the idle-efficiency improvement, not a substantial
idle-RSS reduction. Raw summaries/metadata are `combined6-*`; full snapshots
are archived under `combined6-ab`. This measured binary predates the final
sampler-resume regression correction described below.

X11 autostart now receives the nested compositor's own `DISPLAY` before the
first dispatch. Previously shell construction launched autostart programs
before the XWayland ready callback published that variable. The regression
uses an invalid inherited host display and a standalone, one-shot X11 client:
the preserved baseline fails to connect, while the corrected version connects
and maps inside ChonkStep. All nine XWayland integration tests pass, including
crash recovery, keyboard grabs, geometry, urgency, X resources and EWMH actions.

## CPU raster and label workloads

`cargo run --release -p wm-theme --example performance` exercises the actual
theme library with a counting Rust allocator. It warms caches, checksums a
complete output outside the timed interval, then measures repeated work. The
allocation count covers Rust allocation requests, including temporary buffers;
it is **not RSS** and does not include driver allocations. Save the executable
to run identical workloads against both versions.

Twenty-eight rendering return paths copied finished pixmaps before dropping
the originals. Moving the already-owned pixel vectors into the result removes
those copies. Separately, panel label fitting used to reshape every prefix,
one character shorter at a time. A bounded binary search now measures candidate
prefixes with their ellipsis. Tiny Wi-Fi, network, and Bluetooth labels use the
same search without an ellipsis. Every accepted candidate is measured using the
real font; unusual kerning may choose a slightly shorter fitting prefix.
Title elision also no longer allocates a full character vector for the entire
client title when only its two visible ends are needed.

Five repeated comparisons with the original theme executable produced these
medians, with identical output checksums in every row:

| Library workload | Before time | After time | Before allocated bytes / call | After allocated bytes / call |
| --- | ---: | ---: | ---: | ---: |
| Whole decoration, 1600×1000 content | 2.291 ms | 0.378 ms | 13,259,451 | 6,639,707 |
| Sparse decoration, 1600×1000 content | 18.03 µs | 16.90 µs | 347,854 | 283,774 |
| Clock tile, 224×224 | 0.397 ms | 0.370 ms | 413,442 | 212,738 |
| Empty Overview, 1920×1080 | 1.291 ms | 0.703 ms | 16,793,473 | 8,448,897 |
| Panel label, 64 ASCII characters | 0.601 ms | 0.079 ms | 882,440 | 105,933 |
| Panel label, 1024 ASCII characters | 122.687 ms | 0.497 ms | 232,467,112 | 1,018,265 |
| Panel label, 4096 ASCII characters | 1847.660 ms | 1.745 ms | 3,693,458,056 | 3,914,367 |

The Overview workload's peak additional live Rust allocation fell from
16,588,800 to 8,312,104 bytes. The whole-decoration measurement exercises the
full raster API used by X11; Wayland normally uses the separately measured
sparse decoration path. None of these rows is a claim that the whole compositor
runs six times faster. The theme changes passed 206 unit tests, including
Unicode fitting, narrow boxes, bounded width-query counts, and existing
rendering/opacity assertions.

Raw samples are `theme-text-{before,after}-[1-5].jsonl` in the artifact directory.
The before executable's SHA-256 is
`fefb45249f930320e300c208fe6f4b0c4109c83cd3d09a75ae0b2881762c1322`;
the measured after executable's SHA-256 is
`284ca64c9bdc6268e7c6b536e9e50552470aaa554b2b1966e3f92d6dd333fdcf`.

No matched Hyprland comparison or native-hardware superiority claim has been
established by these measurements.

## Bounded session glyph cache

`cargo run --release -p wm-theme --example performance -- --glyph-churn`
renders 600 distinct twelve-character CJK labels at each of eight font sizes
(16–72 pixels), then repeats the same sequence. The image is overwritten each
time; only the shared session font machinery survives. This deliberately large
working set exercises theme/scale and title churn, not an ordinary idle desk.

Five repetitions gave the following final-phase medians, with identical pixel
checksums at every phase:

| Glyph-cache stress process | Before | After |
| --- | ---: | ---: |
| RSS | 140,892 KiB | 25,128 KiB |
| Live Rust allocations | 131,982,753 bytes | 1,600,756 bytes |
| Cached glyph pixels | 120,242,033 bytes | 466,484 bytes |

The shared cache now evicts reproducible glyph data at soft 8 MiB pixel/outline
and 16K-entry watermarks, checked between render calls. A single render and the
amortized checking interval can temporarily exceed the watermark; the font
database and scaler context are not included in that budget and stay loaded.
Cached negative lookups and hash-table storage are bounded too. Warm draws
check entry counts without scanning the cache every frame.

This is a memory/CPU tradeoff for large working sets. On the repeated 72-pixel
phase, 600 renders took 139 ms with the unbounded warm cache and 356 ms with
eviction. Small warm sparse decorations measured 16.014 µs and 16.021 µs in a
separate five-pair check, with identical allocation counts and pixels. Three
new regression tests cover retained capacity, negative entries, and pixel/font
identity after eviction; all 209 theme unit tests pass.

Raw samples: `glyph-{before,after}-[1-5].jsonl` and
`cache-normal-{before,after}-[1-5].jsonl`. The glyph-workload executable hashes
are `745c547487f6dfb944cb70a914fe9d41d31e0d723ba9a4c7c8f917e56caa1db5`
before and `de4094ffb15705d545dbc3d4722801ddf4b21f9201e00b22a44b8110e9ea8c19`
after. The RSS improvement here belongs to this stress process, not the idle
compositor table above.

## Embedded wallpaper decoding

The shell now uses the already-linked `image` PNG decoder for its embedded
artwork too, sharing the existing premultiplied-RGBA conversion used by Omarchy
backgrounds. No new codec dependency was added. The dedicated experiment is
`cargo run --release -p chonk-shell --example wallpaper_decode`: five alternating
rounds of 25 decodes each measured 20.280 ms for tiny-skia's older PNG path and
14.445 ms for the shared decoder on the stock Lavender Grid image, a 29%
reduction in that operation. This saves about 5.8 ms of decoding, not 29% of
total compositor startup. A regression compares every pixel of all fourteen
bundled artwork renditions against the original path; all eleven wallpaper
tests pass. Raw results are `wallpaper-decode.jsonl`.

The shell also retains an identity for the background already owned by the
backend: artwork, output extent, and appearance. Restyling chrome or changing
UI scale no longer decodes, covers and uploads an unchanged embedded image.
This adds no second image cache. Appearance, artwork and extent changes still
repaint; Omarchy's mutable background files always bypass the reuse guard.
A regression counts root paints across repeated restyles and each invalidation.
The removal is verified structurally, not reported as an invented reload-time
percentage.

## GLES 2 capture and nested buffer age

The constrained-renderer experiment initially failed at startup: Smithay 0.7's
winit convenience initializer requires GLES 3, although its renderer and the
native EGL context support a GLES 2 baseline. The nested initializer now asks
for that same minimum. Unrestricted Mesa llvmpipe, Intel UHD 630 and NVIDIA
RTX 3090 still returned GLES 3.2 contexts in the saved logs.

Booting was not sufficient. With GLES 2 selected, thumbnail and screenshot
readback failed because Smithay 0.7's `ExportMem` implementation unconditionally
uses GLES 3 pixel-pack-buffer operations. A shared capture helper now uses
direct RGBA `ReadPixels` on the older path, with checked allocation sizes and
restored pack alignment. The newer path retains its existing borrowed mapping;
it does not acquire an additional full-image CPU copy. The GLES 2 screenshot
stress smoke test then passed.

The capture investigation also exposed an existing damage-tracking error.
`WinitGraphicsBackend::bind` prepares a framebuffer and handles resize, but
does not make its surface current until rendering starts. Querying buffer age
before that point, after a capture made a surfaceless context current, produces
`BAD_SURFACE` and forces age zero/full damage. The fix explicitly makes the
backend's surface current after resize and before the age query. The same
four-capture regression produces seven `BAD_SURFACE` errors on the preserved
baseline and zero on the fix, with retained-buffer ages and partial damage
visible in the after log. This is not a claim of seven fewer errors per every
possible workload; it is one repeatable regression scenario.

These changes are in preserved compositor `combined-6`, SHA-256
`e82fad6bea2bd8a2247c9554e0c96650eb7a71ed2f36b4ebe9a8cfac7b06d0cb`.
Raw logs include `buffer-age-{before,after}.log` and
`soak-gles2-{after-probe,fixed-probe,long}.log`.

## Hardware-rendered nested startup samples

Seven alternating before/`combined-6` pairs per driver used a GL-rendered
headless Weston, explicitly selected Intel/NVIDIA EGL vendors, CPU affinity
2–7, two seconds of settling and five-second idle samples. The constrained
software soak continued on CPUs 0–1. These short intervals are useful for
startup and memory inspection, not precise low-idle-CPU percentages. Both
drivers returned GLES 3.2 contexts. The Intel host used the earlier harness's
uncontrolled host shader cache; tested compositor shader caches were private
and empty for every run in both series.

| Hardware-nested fixture | Before median | After median |
| --- | ---: | ---: |
| Intel UHD 630: Wayland sync readiness | 159.85 ms | 128.74 ms |
| Intel UHD 630: first-scene barrier | 168.68 ms | 142.04 ms |
| Intel UHD 630: RSS | 80,700 KiB | 80,192 KiB |
| Intel UHD 630: threads | 29 | 12 |
| NVIDIA RTX 3090: Wayland sync readiness | 303.95 ms | 294.49 ms |
| NVIDIA RTX 3090: first-scene barrier | 316.45 ms | 317.41 ms |
| NVIDIA RTX 3090: RSS | 144,680 KiB | 144,052 KiB |
| NVIDIA RTX 3090: threads | 25 | 7 |

Intel's first-scene median improved about 15.8%; NVIDIA's did not improve.
Idle RSS was essentially unchanged on both. PSS fell by roughly 8–9 MiB, but
the raw breakdown shows most of that difference in shared-memory accounting,
not anonymous heap retention. Do not turn it into a claim of equivalent total
machine/GPU memory saved. Full per-run values and ranges are in
`intel-summary.json` and `nvidia-summary.json` in the checked-in data directory;
the complete raw process snapshots are archived as `intel-ab` and `nvidia-ab`.

## Sustained validation

The normal e2e suite includes a short `stability_soak` smoke test. To repeatedly
exercise real terminal creation/destruction, Overview, Dock hide/show, theme
reload, Hyprland IPC and screencopy in one continuing session:

```sh
CHONKSTEP_SOAK_SECONDS=1200 scripts/e2e.sh --headless --release --test stability_soak
```

It writes `samples.jsonl` and before/after `/proc` snapshots under the isolated
test directory. Resource guards run throughout, so a failing baseline stops
without waiting for the full duration to accumulate an excessive leak. This
measures the compositor process; the CPU/PSS of terminal clients and the
headless Weston host are not part of those samples.

Two twenty-minute software-rendered runs completed the real workload:

| Continuing session | Original baseline | Improved `combined-3` |
| --- | ---: | ---: |
| Measured workload cycles | 5,153 | 5,994 |
| RSS, settled start → end | 186,272 → 208,476 KiB | 186,452 → 205,564 KiB |
| Anonymous PSS, start → end | 60,760 → 72,660 KiB | 60,860 → 69,668 KiB |
| Threads, start → end | 59 → 59 | 59 → 59 |
| Client-window records, start → end | 0 → 0 | 0 → 0 |
| Descriptors, start → end | 43 → 43 | 47 → 43 |

The after run's non-pipe descriptors stayed at 40. Total counts can fluctuate
with short-lived instrument-command pipes. Overview's retained shell surfaces
can fall from four to two on a theme reload and return to four on next opening;
the guard checks against the warmed maximum, not against an incidental minimum.
The post-run anonymous-memory increases were about 11.6 MiB and 8.6 MiB. Both
runs levelled off rather than retaining one window or thread per cycle.

These are liveness/resource-retention runs, **not a controlled throughput
comparison**: other builds/tests overlapped, and the later harness adds an
initial two-second settling period and finer descriptor/memory diagnostics.
Do not turn the cycle-count difference into a speedup claim. Shared-library
page ownership changed during these runs, so total PSS and `Private_Clean`
deltas would also be misleading measures of newly retained allocations.
The original Rust soak test passed, but its shell wrapper subsequently exited
127 because the running script was edited; its installed-fixture phase did not
run. The improved soak's complete wrapper passed. Both facts remain in the logs.

A further thirty-minute run passed against the GLES/buffer-age candidate
`combined-6` using
two CPU cores, `LP_NUM_THREADS=2` and
`MESA_GLES_VERSION_OVERRIDE=2.0`, with Mesa explicitly selected. These are
[Mesa's development controls](https://docs.mesa3d.org/envvars.html), not a
substitute for testing a real GLES-2-only GPU or an older CPU's instruction set.
Its 6,932 measured workload cycles completed with the full shell wrapper and
installed-fixture checks passing. Settled start/end values were:

| Thirty-minute constrained GLES 2 session | Start | End |
| --- | ---: | ---: |
| RSS | 152,220 KiB | 171,700 KiB |
| Anonymous PSS | 51,428 KiB | 60,604 KiB |
| Threads | 31 | 31 |
| Non-pipe descriptors | 40 | 40 |
| Total descriptors | 45 | 44 |
| Client windows / frame records | 0 / 0 | 0 / 0 |

Anonymous PSS rose by about 9.0 MiB across the workload; this is not a claim
of zero allocation growth. Most growth was early (51,428 → 58,288 KiB in the
first five minutes), but the later samples still drift: the last five-minute
window ranges from 60,528 to 63,224 KiB. Allocation-site profiling and a longer
run are needed to distinguish remaining retention from allocator/driver caches.
No `BAD_SURFACE`, invalid-GL-operation, or readback
failure was found in its compositor log. This run is additional liveness and
bounded-resource evidence, not a before/after speed comparison or a guarantee
against every long-session leak. Raw samples are `soak-gles2-final.jsonl`.

The churn logs also retain calloop warnings about unregistering an already
invalid descriptor. Such warnings occur in the original baseline as well as
the candidates; their teardown ordering has not been attributed or corrected
by this work. Stable descriptor counts do not prove every event-source teardown
is correct, and the report does not describe these logs as warning-free.
A short, separately launched IPC test under `strace` passed but did not
reproduce those warnings. Attaching to the continuing test process was denied
by the system; no ptrace restrictions were changed. The trace wrapper's PID
belongs to the tracer, so none of its timings or process accounting is used
as compositor performance evidence.

The final `combined-7` candidate then passed a fresh ten-minute constrained
GLES 2 run, including its complete installed-fixture wrapper: 2,560 measured
cycles, RSS 154,292 → 173,816 KiB, anonymous PSS 53,668 → 62,888 KiB,
threads 31 → 31 and non-pipe descriptors 40 → 40. Final client-window and
frame records were both zero. The full software suite and the separate tracing
diagnostic overlapped this run on other CPU cores; its cycle count is not a
controlled performance comparison. It confirms the new visibility-generation
behavior under continuing real Dock/theme/client churn. Samples are
`soak-final7.jsonl`; the roughly 9 MiB anonymous-memory increase remains part
of the open allocation-profiling question above.

## Validation and limits

The final sampler-resume correction is preserved in `combined-7`, SHA-256
`52c6354aad10c6270779b34b1ab71d3bc68b033031fb040579a6f6b39285755e`,
and committed as `eb5f554`. Its source passes 1,867 workspace tests with zero
failures (134 ignored), all 16 sampler tests, and release Clippy across all
workspace targets with warnings denied. This is the production candidate used
for the final handoff measurements; earlier tables name their intermediate
binaries explicitly.
The complete 114-check nested software-rendered suite and installed Omarchy
fixtures also pass against that exact preserved executable.

The `combined-6` source passed 1,865 workspace tests (134 integration/fixture
tests deliberately ignored in an ordinary workspace run), release Clippy with
warnings denied, and 114 checks in the complete nested Wayland/installed
Omarchy suite. The same complete suite passed on an explicitly selected Intel
UHD Graphics 630 under a hardware-rendered headless Weston. Python benchmark
guards have 15 tests, including stream framing, real compatibility-socket
readiness, input validation, environment isolation and child-process cleanup
after a failed measurement. Shell syntax/ShellCheck and diff whitespace checks
also pass.

The complete 114-check suite also passes with the Mesa GLES version restricted
to 2.0 and two llvmpipe renderer threads. This exercises pixel assertions through
the new fallback, not merely the presence of a screenshot file. These development
overrides do not remove every modern extension or emulate an old CPU.

An explicit NVIDIA RTX 3090 nested run reached the Alacritty tests and failed
both because the client could not initialize its EGL display. The corrected
test fixture quotes the TOML string `window.decorations="None"` and supplies
`--config-file /dev/null`; previously Alacritty silently ignored its unquoted
option. The corrected fixture passes on Intel. NVIDIA's failure reproduces on
both the original baseline and `combined-6`: this nested EGL display lacks
`EGL_EXT_device_query`, so no render node can be identified for DMA-BUF feedback
and that global is withheld. The report does not call this a new regression or
a successful full NVIDIA suite. The native DRM path has another source of
render-node identity, but native KMS login was not tested in this session.
The remaining NVIDIA run passed 112 checks with those two exact test names
explicitly excluded; the exclusion is recorded in the command and logs.

Hyprland 0.56.2 comparison feasibility was investigated in device-isolated
namespaces without exposing DRM card nodes or the live seat. Direct nesting
under the installed Weston and Sway encountered protocol-version failures.
An intermediate ChonkStep host allowed the installed Hyprland to render, map a
terminal and start XWayland; logs and screenshots are retained. However, that
fixture negotiated different output geometry and displayed startup warnings.
It is **not yet a matched competitor benchmark**, and no Hyprland speed or RAM
superiority claim follows from it.

Still required before broad hardware/drop-in claims: native DRM/KMS and
multi-monitor testing, AMD coverage, actual older hardware, suspend/resume,
hotplug, GPU reset, frame pacing under games/video, and a feature/geometry-matched
competitor matrix. The code audit also found the built-in clock deriving UTC
time directly from epoch seconds; local-zone/DST correction remains separate
work and should keep timezone I/O off the repaint thread.

## Next measurement gates

The next pass should prioritize these concrete questions:

1. Profile and recover the visible-Dock first-scene regression without starting
   hidden samplers or reintroducing stale counter folds. Gate the next change
   on both visibility fixtures, including first-scene readiness rather than
   just socket creation or an event-loop reply.
2. Attribute the remaining long-soak anonymous-memory drift. Collect allocation
   stacks and repeat many complete title cycles, distinguishing
   live Rust retention, allocator slack and driver storage. Neither `heaptrack`
   nor `valgrind` was installed on this machine; no packages were installed for
   this investigation. Do not diagnose a leak from RSS alone.
3. Replace the remaining session-marker polling deadlines with event-driven
   file notification, preserving requests created before watch registration,
   atomic renames, directory replacement and fallback behavior. The existing
   screenshot/restart/theme request contract must remain compatible. Validate
   idle dispatch counts and request latency, not just a smaller timer count.
4. Resolve nested NVIDIA render-node identification without guessing a device
   on a multi-GPU machine. Require the two currently excluded Alacritty tests
   and DMA-BUF client/capture checks to pass before calling that matrix green.
   Separately trace the churn-time calloop invalid-descriptor warnings through
   source removal and descriptor lifetime before deciding whether they are
   harmless duplicate cleanup or an ordering bug.
5. Use a dedicated native test session for DRM/KMS, multi-monitor, hotplug,
   suspend/resume and frame pacing. Compare CPU mappings **and** appropriate
   GPU-memory accounting; include Intel, AMD, NVIDIA and real older hardware.
   This cannot be established by changing the live desktop during a nested run.
6. Establish a matched Hyprland fixture before competitor charts: same output
   geometry/scale, renderer, wallpaper, shell/widgets, XWayland, clients,
   startup-readiness definition, cache policy and measurement duration. Keep
   every raw sample and report ranges as well as medians.

The remaining clock timezone issue is a compatibility follow-up, not a measured
performance improvement from this branch. Keyboard-map reuse on unchanged
reloads also needs to preserve edits to user-supplied keymap files before it is
safe to optimize.
