# Native GPU comparison across NVIDIA, AMD and Apple Silicon

Measurements collected on 2026-09-10. Each machine compares the preserved
pre-performance-work baseline with the qualified compositor using its real
display controller and internal or attached panel.

The result varies by platform: NVIDIA reduces CPU use substantially; AMD improves
native frame delivery and CPU cost per displayed frame; M1 delivers smoother
frames with a small increase in CPU cost per displayed frame. There is no
universal CPU or power reduction across all three machines.

| Machine | Native CPU time per displayed frame, mean paired change | Scaled CPU time per displayed frame, mean paired change | Native missed refreshes | Scaled missed refreshes |
|---|---:|---:|---:|---:|
| i9beef / RTX 3090 | −34.16% | −32.03% | 396 → 7 | 13 → 2 |
| Intel iMac / AMD | −18.43% | −8.49% | 2362 → 6 | 1 → 0 |
| M1 MacBook Pro | +3.69% | +2.67% | 298 → 17 | 1127 → 15 |

Every missed-refresh total covers five 30-second samples at that machine's
verified refresh rate. [Combined data](comparison.json) retains the input
campaigns and exact binary identities. The hardware and source differences below
matter; these are within-machine improvements, not a ranking of GPU speed.

## M1 paired results

| Workload | Compositor CPU, baseline → final median | CPU µs per displayed frame, baseline → final median | Mean paired CPU/frame change | Actual FPS, baseline → final median | Missed refreshes, baseline → final total |
|---|---:|---:|---:|---:|---:|
| Native 2560×1600 buffer | 5.97% → 6.30% | 1019 → 1053 | +3.69% | 58.531 → 59.931 | 298 → 17 |
| 3414×2134 buffer sampled onto 2560×1600 | 5.43% → 6.37% | 1045 → 1064 | +2.67% | 51.929 → 59.964 | 1127 → 15 |

Each total covers 150 measured seconds, with all five pairs retained. Total
compositor CPU rises by a mean paired 7.10% (native) and 17.40% (scaled), while
actual frame delivery improves. CPU/frame bootstrap 95% intervals are +2.51%
to +4.44% and +0.98% to +4.25%. The CPU increase remains visible after normalizing
for displayed frames; it must not be presented as an M1 CPU optimization.
This campaign does not isolate which code change accounts for that overhead.

Apple SMC total-system power averages are 8.84 → 8.85 W (native) and
8.98 → 9.12 W (scaled). These are system sensor readings on AC power, with
normal CPU scheduling/power policy and retained background apps. No GPU-power
or battery-life improvement is established. [Individual samples and analysis](evidence/m1/ab/analysis.json)
include CPU, presentation and system-power recordings.

## M1 plane and recovery qualification

The [20-cell policy matrix](evidence/m1/planes2/analysis.json) includes native
fullscreen with visible and hidden cursors, scaled fullscreen and decorated
windows. Every cell passed captured pixels and steady-state render/queue error
checks. Two combined-policy cells had substantial performance regressions,
discussed below; pixel correctness is not performance qualification.

| Scene | Default | Overlay | Primary-any | Both | Client scanout disabled |
|---|---|---|---|---|---|
| Native fullscreen, visible cursor | Composed | Composed | Composed | Composed, slower | Composed |
| Native fullscreen, hidden cursor | Composed | Composed | Primary | Primary | Composed |
| Fractional fullscreen, visible cursor | Composed | Composed | Composed | Composed, slower | Composed |
| Decorated window | Composed | Composed | Composed | Composed | Composed |

The primary-any and both hidden-cursor samples reported zero-copy for 600/600
and 599/599 presentations, respectively, with actual primary scanout counted
by the compositor and an active KMS framebuffer. They each used about 3.3% of
one CPU core, versus about 5.1% for the default hidden-cursor case. These short
path checks are separate from the repeated default-policy CPU comparison.

The display controller has no dedicated cursor plane. No client overlay usage
was demonstrated with this XRGB2101010 fixture; this does not qualify other
buffer formats for overlays.

The [recovery run](evidence/m1/fresh/000/stress/results.json) explicitly required
primary scanout before exercising fullscreen/windowed changes, popup overlap,
Space parking/return, three DPMS cycles and three VT handoffs. Nineteen captured
frame numbers increased strictly. There were 61 asynchronous readbacks, no
synchronous fallback and no staging allocation left active at completion.
Two EACCES queue failures were confined to deliberate VT handoffs, matched the
recorded error logs and recovered. No other render/queue failure was accepted.
The run records 23 phases including an explicit modeset skip: this panel offers
no second physical mode. It does not claim that skipped path passed.

### M1 combined-policy performance regression

The first matrix's native/scaled visible-cursor scenes fell to 40.9/44.6 FPS
with both experimental flags enabled. An additional
[eight-sample check](evidence/m1/flags/analysis.json), two alternating runs of
each scene/policy for 12 seconds, reproduced the problem:

| Scene | Default policy | Both experimental flags |
|---|---|---|
| Native, visible cursor | 60.0 FPS; 6.17–6.25% CPU; 0 missed refreshes | 30.0–33.3 FPS; 17.42–19.42% CPU; 319–358 missed refreshes |
| Fractional, visible cursor | 60.0 FPS; 6.25% CPU; 0 missed refreshes | 30.0 FPS; 25.75–26.92% CPU; 359 missed refreshes per sample |

The client buffers change from Apple's tiled modifier to `Linear` in the
combined-policy cases, but the visible cursor/scene still requires composition.
Most of the extra CPU wall time appears in composition submission. This points
to allocation-feedback/layout selection interacting with fallback composition;
the exact compositor/driver contribution has not been isolated or fixed here.
**The overlay-plus-primary-any combination is not performance-qualified on M1.**
Both options remain off by default, including in the daily-use i9beef install.
The successful hidden-cursor primary-scanout result does not override this
visible-cursor regression.

M1's separate default-policy GPU-timer runs completed 722–723 GPU samples per
12-second measurement. Native composition spans averaged 1039–1045 µs and scaled
spans 1282–1287 µs. These establish functioning asynchronous GPU queries on
Apple Silicon. Unlike the AMD high-performance diagnostic, M1 clocks were not
fixed or exposed through the collector, so these are observed spans rather than
a clock-controlled estimate of resampling cost.

## AMD paired results

| Workload | Compositor CPU, baseline → final median | CPU µs per displayed frame, baseline → final median | Mean paired CPU/frame change | Actual FPS, baseline → final median | Missed refreshes, baseline → final total |
|---|---:|---:|---:|---:|---:|
| Native 3840×2160 buffer | 2.33% → 2.63% | 528 → 439 | −18.43% | 44.156 → 59.963 | 2362 → 6 |
| 5120×2880 buffer sampled onto 4K | 2.87% → 2.60% | 478 → 433 | −8.49% | 59.996 → 59.996 | 1 → 0 |

Missed-refresh totals each cover 150 measured seconds. All five pairs are
retained. Native pacing improves substantially, with a mean paired **10.59%
increase in total compositor CPU** while it delivers more frames. The scaled
workload has an 8.47% mean paired reduction in total CPU. Paired bootstrap 95%
intervals for CPU/frame change are −20.31% to −16.63% (native) and −11.25% to
−5.87% (scaled), from 10,000 resamples of only five pairs. These intervals
describe this campaign, not all applications or hardware.

AMD PPT sensor averages are 21.96 → 25.14 W for native and 26.36 → 26.35 W for
scaled. Native memory-clock averages change from 898 to 1496 MHz as the automatic
clock policy switches between states; both scaled groups average 1695 MHz.
There is no demonstrated native GPU-power saving. Clock policy and different
numbers of delivered frames also prevent treating these as isolated shader-cost
measurements. [Individual samples and analysis](evidence/imac/ab/analysis.json)
retain presentation distributions, stage counters and telemetry.

## AMD plane and recovery qualification

All 15 combinations of native/scaled/windowed scenes and the five scanout
policies passed pixel, physical-mode and steady-state render/queue checks. The
hardware cursor was active in every sample. No client primary or overlay scanout
was observed. This GPU exposes no overlay planes, and its client DMA-BUFs carry
`XR30` with `Modifier::Invalid` (implicit layout). Smithay's framebuffer exporter
explicitly rejects implicit-layout client buffers rather than assume a layout
and risk corrupted scanout. `primary-any` relaxes format matching but does not
bypass that validation. Flags alone did not accelerate these client buffers.

The separate [freshness campaign](evidence/imac/fresh/000/stress/results.json)
passed all 24 phases: five fullscreen/windowed cycles, popup overlap/removal,
Space parking/return, three DPMS cycles, three VT handoffs, and verified
3840×2160 → 3200×1800 → 3840×2160 physical modesets. Twenty-one captures contained
strictly increasing frame markers. Hidden, powered-off and inactive clients
stopped callbacks, then resumed with fresh pixels. It completed 72 asynchronous
readbacks, zero synchronous fallbacks, zero render/queue failures and zero live
staging bytes at completion. These recovery checks exercised the composited
client path; they do not prove AMD client scanout.

## AMD GPU timing with controlled performance state

The normal-policy GPU-query runs were bimodal: native composition spans averaged
879–971 µs, while scaled spans averaged about 539 µs. Automatic power-state
changes make that an unreliable estimate of scaling overhead.

A separate four-sample diagnostic set temporarily selected the GPU's existing
`high` performance level. All telemetry samples then reported 1200 MHz graphics
and 1695 MHz memory clocks. Across two runs per scene, native composition spans
were 394.166 and 394.229 µs; scaled spans were 539.537 and 539.582 µs. That is about
145 µs, or 37%, more GPU composition time for this 5K-buffer-to-4K workload under
the controlled setting. These spans exclude the producer's own rendering and
are not end-to-end latency. Two samples do not establish a general resampler
cost, and the physical output is still 4K.

The [diagnostic samples](evidence/imac/timers-high/analysis.json) are separate from
the normal-policy A/B results. Automatic GPU power management was restored and
read back as `auto`; no clocks were overclocked or persistent policy changed.

## Hardware and scope

| Machine | Hardware | Verified physical mode | Fractional workload source buffer |
|---|---|---|---|
| i9beef | RTX 3090, NVIDIA proprietary driver | 3840×2160 at 143.999568 Hz | 5120×2880 |
| imac | iMac19,1, Intel i5-9600K, AMD Ellesmere with 8 GB VRAM | 3840×2160 at 59.996625 Hz | 5120×2880 |
| m1 | 13-inch MacBook Pro (M1, 2020), Asahi/Mesa | 2560×1600 at 59.999856 Hz | 3414×2134 |

The iMac is a 5K panel model, but its current EDID and kernel mode list expose
only up to 3840×2160. These are **not native 5K scanout results**. The fractional
test renders a 5K client buffer and samples it onto the verified 4K output.
No custom mode, kernel patch, reboot or panel-driver change was used.

The M1 render device is `asahi` and its display device is `apple-drm`. These
separate device nodes are one Apple Silicon system, not a two-GPU test. Its
internal output advertises only one mode, so a transition to a second physical
mode cannot be qualified on this setup.

## Method

Five alternating pairs per workload, 30 measured seconds following eight seconds
of warmup. Native means a client buffer matching the physical output at scale 2;
fractional uses output scale 1.5 and client buffer scale 2. The pointer remains
visible. Both versions use shipping scanout policy, with profiling and adaptive
refresh disabled equally. Hardware `wp_presentation` timestamps measure actual
completion; the test requires hardware-clock, hardware-completion and vsync flags.
It independently checks KMS mode, hardware rendering, DMA-BUF delivery and captured
calibration/texture pixels. Captures and recovery tests run outside timed windows.

CPU percentages count compositor threads as a fraction of one CPU core. They
exclude the producer, capture program and telemetry collector. CPU microseconds
per displayed frame normalize for different numbers of completed frames. The
producer uses an opaque, full-damage, frame-paced EGL texture and `glFinish`
before buffer handoff. This is a synthetic workload, not application or
input-to-photon latency qualification. Different resolutions, CPUs and refresh
rates prevent ranking the machines by absolute CPU percentages.

The fixture requests at least eight bits per RGB channel, and EGL selects the
configuration: the NVIDIA producer supplies XRGB8888, while AMD and Apple supply
XRGB2101010. Each machine's baseline and candidate use the same producer and
format. This is another reason to compare improvements within each machine
rather than treat the three systems as identical rendering workloads.

AMD telemetry reads sysfs/hwmon once per second without changing clocks or power
policy. NVIDIA uses the retained `nvidia-smi` recordings. Both include other
resident processes and the producer. Apple SMC readings are explicitly labeled
**system/rail power**, not GPU power; this machine exposes no GPU utilization,
clock or GPU-power sensor through the tested interface. Telemetry analysis trims
the first and last second of each measurement; sysfs uses monotonic timestamps.

## Source and reproducibility

AMD uses the exact x86 binaries retained from the NVIDIA qualification. M1 uses
fresh, separate ARM release build directories from source archives of `da52c0d`
and `52c680d`, with the committed lockfile. The compositor is copied before
building the EGL producer separately, avoiding Cargo feature unification changing
the compositor under test. No diagnostic allocation-counting build is timed.

The ARM producer contains the optional changing-pixel marker; it is disabled in
the paired comparisons and enabled only for freshness qualification. The normal
shader and producer workload are unchanged. Source archives, build logs, binary
hashes, campaign arguments and exact harness copies identify the tested artifacts.

The earlier [NVIDIA report](../i9beef-native-2026-09-10/README.md) distinguishes its
five-pair performance candidate from the subsequent exact-final-binary
confirmation. The final diagnostic corrections concern opt-in GPU queries and
live multi-GPU counters; they do not run in the ordinary single-GPU timed path.

## i9beef daily-use deployment

At the user's request, the exact qualified x86 binary was installed at
`/usr/bin/chonkstep-wayland` and the active desktop restarted after explicit
authorization to close its existing apps. Its running SHA-256 matches the tested
artifact. Physical 4K/143.999568 Hz and 1.5× scaling were verified afterward.
Adaptive sync remains requested; this build activates it for eligible direct
scanout. Experimental overlay/primary-any flags remain opt-in.

The prior binary remains at
`/usr/local/lib/chonkstep/rollback-2026-09-10/chonkstep-wayland`; the qualified
copy is also retained under `experimental-2026-09-10`. This is a local development
binary install: pacman still records package version 0.4.3-1, and a future package
upgrade can replace the executable. The normal session wrapper resolves the new
`/usr/bin/chonkstep-wayland`; no test socket is enabled in the daily-use session.

## Completion and retained evidence

The two Mac campaigns completed **103 native samples: 47 on the iMac and 56 on
M1**, including paired performance measurements, policy matrices, recovery,
profiling and idle checks. [Validation](validation.json) records the expected
sample counts, zero steady-state render/queue failures, performance regressions
and untested scope. Both versions on both Macs recorded zero render calls during
their 12-second idle samples. The corresponding CPU readings contain only a few
accounting ticks and do not establish an idle CPU improvement.

All [86 Python checks passed](python-tests.log) for the benchmark tooling in
`df6bfc1`. This work added AMD/Apple telemetry, distinct-mode selection, explicit
modeset skip reporting, a Pillow preflight and a fixture startup fix. It did not
change the compositor's Rust implementation. The M1 fixture previously mistook
an activation configure for a fullscreen response; waiting for its initial frame
callbacks before requesting fullscreen resolved that setup race, and the full
matrix was rerun. The incomplete matrix, initial missing-Pillow attempt and
discarded ARM build using a shared Cargo cache remain in the raw archives; they
are excluded from the completed campaign counts and performance comparisons.

Both Macs returned to their original desktop processes and physical modes, with
no benchmark services left running. The [iMac restoration](imac-restored.json)
preserved all six original clients and restored automatic GPU power management;
its normal idle lock remains engaged. The [M1 restoration](m1-restored.json)
preserved its installed binary and existing source checkout. The local
[i9beef install record](i9-install.json) identifies the running qualified binary,
display settings and rollback copy.

The repository retains measurement JSON, telemetry, KMS snapshots, diagnostics,
build logs and exact harness copies. Full raw images, logs, tested binaries and
source archives are retained locally under
`/home/chrisk/.local/state/chonkstep/benchmarks/cross-gpu-2026-09-10/`.
All four archive SHA-256 values were recomputed and verified against the
[artifact manifest](artifacts.json). These local archives are not embedded in
Git; keep them alongside the report when transferring the complete evidence.
