# i9beef native DRM qualification, 2026-09-10

The final build reduces compositor CPU use by about one third against the
preserved pre-GPU-work baseline in this workload. Hardware presentation feedback
also shows substantially fewer missed refreshes. These measurements use the
actual NVIDIA display controller and attached 4K/144 Hz panel.

## Controlled default-settings comparison

Five paired runs per workload, 30 measured seconds after eight seconds of warmup;
order alternates. GPU timer queries and adaptive refresh are disabled equally.
Client scanout policies use shipping defaults. The pointer remains visible.
CPU percentages mean percent of **one CPU core**, counting compositor threads
but excluding the producer and capture/telemetry processes.

| Workload | Baseline CPU, median | Final CPU, median | Mean paired CPU change | Actual presentation FPS, baseline → final medians | Missed refreshes, baseline → final totals |
|---|---:|---:|---:|---:|---:|
| Native 3840×2160 buffer | 8.83% | 6.03% | −32.95% | 140.932 → 143.965 | 396 → 7 |
| 5120×2880 buffer scaled onto 3840×2160 | 8.67% | 5.87% | −31.99% | 143.932 → 143.999 | 13 → 2 |

Each missed-refresh total covers 150 measured seconds. It estimates omitted
refresh opportunities from adjacent hardware presentation timestamps; it is not
an application-submission counter. The final build is not completely stutter-free.
Median CPU time per presented frame falls from 626 to 419 µs for native and
602 to 408 µs for scaled buffers. Median CPU wall time inside the render function
falls from 434 to 221 µs and 409 to 210 µs respectively; that is not GPU busy time.

The mean paired CPU changes have 95% bootstrap intervals of −35.67% to −30.72%
(native) and −32.28% to −31.60% (scaled). These summarize only five pairs on one
machine and do not establish performance across applications, GPUs or drivers.
All individual samples remain in the evidence, including the slower baseline
runs; none were removed as outliers.

After the additional diagnostic fixes, a separate 30-second pair per scene
confirmed the final qualified binary: native CPU 8.90% → 6.07%, scaled CPU
8.60% → 5.93%. Native missed refreshes were 20 → 7; scaled were 2 → 1. This
later confirmation is reported separately, not pooled into the original five
pairs. The remaining misses and variation between campaigns matter; the test
does not promise a fixed stutter reduction for every workload.

Whole display-board power averages are 43.41 → 44.28 W (native) and
81.37 → 81.41 W (scaled). There is **no demonstrated GPU power saving** from the
default changes. These numbers include the EGL producer and other resident
processes. Both GPUs have unrelated applications retaining substantial VRAM;
those applications were preserved, and both boards were sampled throughout.

## Hardware defects found and fixed

1. Imported client DMA-BUFs had no device hint. Smithay rejected them before
   framebuffer export, so enabling scanout flags did not actually enable a
   usable client plane. Successful import now records the importing renderer
   when no hint exists. Existing hints, GBM export and atomic validation remain
   authoritative; allocation origin is not inferred.
2. The scheduler learned CPU submission costs but could miss GPU completion
   deadlines despite finishing CPU submission on time. Each accepted page flip
   now retains its target refresh; actual monotonic KMS completion adds bounded
   250 µs safety steps after misses. A 30-second hold prevents CPU observations
   from immediately erasing the learned GPU allowance. This can start work
   earlier; input-to-photon latency was not measured.
3. Built-in cursor sprites used ABGR storage while Smithay's hardware cursor
   copy requires ARGB. The fallback looked correct but repeatedly failed cursor
   plane preparation. Cached sprites now convert RGBA to BGRA once, preserving
   alpha and colors. Actual primary and cursor planes were observed together.

These three fixes are enabled by default in the final source build. Overlay
scanout and unrestricted primary format selection remain experimental opt-ins;
GPU timer queries and rendering on a second GPU are also opt-in. Default client
composition in the main comparison is expected: this NVIDIA producer supplies
XRGB8888, while the selected swapchain uses ARGB8888, so the conservative primary
policy rejects that format mismatch.

Hardware review found two additional diagnostic defects after the long A/B
campaign. GPU query end markers now receive a nonblocking flush, preventing a
marker from waiting in the driver until the next frame. This executes only when
profiling is enabled. Multi-GPU copy counters now come from live atomics when
diagnostics are requested, instead of being embedded in the graphics identity
string cached at startup. Separate confirmation samples identify the final
qualified binary; the five-pair table above identifies the earlier performance
candidate. These diagnostic corrections do not run in the ordinary render path.

The initial audited build's 15-cell hardware matrix had **zero client scanout**
under every policy. It is retained as failed acceleration qualification, not
presented as evidence that flags alone worked. Intermediate builds isolated the
import, pacing and cursor defects. Timing runs made during compilation are not
used in the controlled comparison.

## Actual plane qualification

Fifteen final-build samples cover three scenes and all five policies. These are
10-second path/correctness checks after six seconds of warmup, not the five-pair
performance campaign above. All fifteen passed capture verification, physical
mode checks and steady-state render/queue error checks. The hardware cursor was
active in every scene, including fractional output scaling.

| Scene | Default | Overlay enabled | Primary-any enabled | Both enabled | Client scanout disabled |
|---|---|---|---|---|---|
| Native fullscreen, visible cursor | Composed | Overlay | Primary | Primary | Composed |
| 5K buffer scaled to 4K, visible cursor | Composed | Composed | Composed | Composed | Composed |
| Decorated 800×600 client | Composed | Overlay | Composed | Overlay | Composed |

The native overlay and primary-any samples each delivered 1440/1440 presented
client frames with zero-copy feedback. Both windowed overlay samples also
delivered 1440/1440. KMS inspection independently confirmed an active overlay
framebuffer alongside the primary and cursor planes. Enabling both flags selected
primary for native fullscreen and overlay for the decorated client. Compositor
counters distinguish client-plane use from the always-present primary plane
carrying a composited framebuffer.

Scaled-buffer plane attempts were rejected and composition preserved the exact
pixels. Diagnostics retain format-mismatch and failed-scanout reasons instead of
reporting the flags as successful acceleration. These results qualify this
fixture on this driver; they do not justify enabling experimental policies for
every GPU by default.

## Recovery, capture and profiling

The first complete performance-candidate transition campaign passed 24 recorded phases:
five fullscreen/windowed cycles, popup overlap and removal, parking/returning a
Space, three DPMS cycles, three VT handoffs and real 60→144 Hz modesets. Hidden,
powered-off and inactive-session clients stopped receiving frame callbacks;
callbacks and verified pixels recovered afterward. All 124 readbacks used the
asynchronous path, with no synchronous fallback and no staging memory left
active at completion. Peak staging was 33,325,056 bytes.

Two EACCES page-flip retries occurred during intentional VT handoff. DRM master
can be revoked between a seat-state check and the atomic ioctl. The test accepts
only that exact error inside explicitly recorded VT-switch intervals, requires
counter/log agreement and verifies recovery. It rejects other queue failures.
Earlier incomplete attempts, including a rounded 60 Hz mode name that the EDID
did not advertise and an overstrict VT-error assertion, remain in the raw
archive. The actual nominal-60 mode is 59.997002 Hz.

The final qualified binary repeated all 24 phases with an opt-in 24-bit frame
number encoded in the producer's actual GPU pixels. Every checked capture had a
strictly newer number bounded by the submitted client frames; a stale texture
could no longer pass solely because its calibration colors were correct. This
run completed 123 asynchronous readbacks, zero synchronous fallbacks, zero
render/queue failures and no live staging allocation at completion. Its changed
producer binary is used only for freshness qualification, not the A/B table.

A separate capture-pressure campaign completed 27 verified 4K PNG exports in
four samples. Baseline presentation cadence was 138.30 and 135.23 FPS; the
candidate achieved 142.79 and 142.83 FPS. The requested two-second capture cadence
was limited by roughly three-second PNG exports of the noisy texture, so actual
sample durations were 20.20–23.11 seconds and completed capture counts differed.
This is a correctness/stress result, not an equal-work capture-speed benchmark.
There is no demonstrated PNG-export speedup. Candidate readbacks used the
asynchronous path without synchronous fallbacks.

The original native GPU queries misleadingly measured about 6.89 ms in both
scenes because their end markers were not promptly submitted. After the flush
correction, two 12-second samples per scene measured spans of 1266–1268 µs
(native) and 401–407 µs (scaled). The display GPU's automatic memory clock was
810 MHz in the native case and 5001 MHz in the scaled case, with approximately
43 W versus 80 W of whole-board power. **These results do not mean scaling is
cheaper.** Clock policy and the producer's larger framebuffer confound that
comparison. They establish working asynchronous native queries; a clock-controlled
experiment is needed to isolate resampler cost. No GPU clock settings were
changed for this test.

Adaptive sync separately activated with primary scanout at the 144 Hz mode and
passed pixel/plane checks. It was disabled for the controlled comparisons. A
short idle control used 0.1% of one core for baseline and 0.0% at the measurement's
resolution for the candidate; this single short sample is not a power claim.

Rendering on renderD129 and displaying on renderD128 preserved the image but
fell back to CPU transfers: only 45.35 FPS at 51.20% of one core in the first
15-second sample. Real target allocations produced zero verified interop formats.
The log records the failed DMA transfer and CPU fallback; the frozen zero copy
counters exposed the diagnostic defect described above. This path is not
qualified as acceleration on this machine and remains off by default.

The final binary's repeat confirmed 682 CPU transfer frames, 5,656,780,800 copied
pixels and zero DMA transfer frames across the diagnostic interval. It achieved
45.26 FPS, with GPU profiling also enabled. The independent two-real-GPU test
passed full and partial opaque-pixel checks in both transfer directions.

## Measurement method and scope

- Sony SDMU27M90*30, serial 0000001, DP-1 on `/dev/dri/card1`, one physical output.
  KMS pixel clock and timing totals establish 3840×2160 at 143.999568 Hz.
- Display GPU: RTX 3090, PCI 0000:01:00.0, `/dev/dri/renderD128`. Second RTX 3090:
  PCI 0000:02:00.0, `/dev/dri/renderD129`, no attached output. Driver 610.57.04.
- Release compositor and optional EGL producer are built separately to avoid
  Cargo feature unification changing the shipping binary. Executable SHA-256,
  build IDs, kernel, arguments and exact harness snapshots identify each run.
- Private PAM/logind sessions take tty3 and return to the existing tty2 desktop.
  Private configuration disables shell/bar/dock and starts Mac mode. Existing
  apps remain alive; benchmark results describe the specified synthetic scene.
- The opaque EGL fixture renders a deterministic spatial texture, uses full
  damage on each frame-paced commit, and calls `glFinish` on the **producer**
  before handoff to accommodate NVIDIA buffer synchronization. This is not an
  unsynchronized real-world client stress test or a video/game benchmark.
- `wp_presentation` feedback must assert vsync, hardware clock and hardware
  completion on CLOCK_MONOTONIC. Sequence numbers are unavailable on this driver;
  timestamps, not sequence values or client wakeups, establish cadence. Zero-copy
  feedback is checked separately from compositor plane counters and active KMS
  framebuffer assignments.
- Captures verify physical dimensions, three exact calibration colors and
  non-flat texture content; windowed captures validate the client rectangle.
  Capture validation is after timing except in explicitly labeled capture runs.
- NVIDIA board samples use one-second polling. Analysis retains only timestamps
  inside the CPU/presentation measurement window, trimming one second at both
  ends. The initial comparison predates per-snapshot wall timestamps; its saved
  wall/monotonic calibration maps the clocks. Local telemetry time is UTC−07:00.
- Native stage percentiles are logarithmic histogram upper bounds. GPU queries
  are a separate profiling campaign; their span can include CPU submission gaps
  during plane preparation, and is not a shader-only or GPU busy-time metric.

There is no physical 5K panel or second connected monitor in this run. The
5120×2880 client-buffer case tests real scaling to a 4K output. Earlier
[5K nested rendering](../5k-final-2026-09-10/README.md) and
[Spaces validation](../mac-spaces-fixes-2026-09-10/validation.json) cover different
properties. This report does not claim physical multi-monitor, Apple GPU,
hot-unplug, photon latency or fastest-compositor-on-Linux qualification.

## Reproduce

Run only on an authorized test machine with an available tty3 and a recoverable
session on tty2. Select the actual DRM card and connector; DRM node numbers can
change across boots. `scripts/native-drm-info.c` is a read-only KMS inspector.

```sh
cargo build --locked --release -p chonkstep-wayland
cp target/release/chonkstep-wayland /tmp/chonk-final
cargo build --locked --release -p chonk-testkit --bin chonk-fullscreen-probe --features gpu-probe
cc -O2 -Wall -Wextra -Werror scripts/native-drm-info.c $(pkg-config --cflags --libs libdrm) -o /tmp/native-drm-info
python scripts/bench-native-gpu.py \
  --binary baseline=/path/to/preserved-baseline --binary final=/tmp/chonk-final \
  --probe target/release/chonk-fullscreen-probe --drm-info /tmp/native-drm-info \
  --device /dev/dri/card1 --connector DP-1 --test-vt 3 --return-vt 2 \
  --cases native fractional --policies default --runs 5 --seconds 30 \
  --settle-seconds 8 --output /tmp/cn-ab
python scripts/analyze-native-gpu.py /tmp/cn-ab \
  --utc-offset-seconds -25200 --output /tmp/cn-ab/analysis.json
```

Use the local UTC offset that was in effect when telemetry was recorded. The
harness refuses occupied test VTs, software rendering, wrong physical modes,
missing hardware feedback, failed native frames and incorrect capture pixels.
Use `--require-scanout primary|overlay|cursor` to require actual plane use.
The `direct` case hides the pointer to test eligibility; its name does not imply
that scanout occurred. `--stress --cases direct` tests fullscreen/windowed
transitions, popups, Spaces, capture, DPMS, VT handoff and 60→144 Hz modesets.
Add `--frame-marker` with the updated producer to require advancing GPU pixel
content after each transition.

## Evidence and final checks

[Validation record](validation.json), [five-pair analysis](evidence/ab/analysis.json),
[final-binary confirmation](evidence/confirmation/analysis.json),
[fresh-pixel recovery](evidence/freshness/000/stress/results.json), and
[file checksums](evidence/manifest.json) retain 58 completed native samples.
Four original profiling samples are explicitly superseded by the corrected
queries. Each campaign records the exact binary and producer hashes; the label
alone must not be treated as a binary identity across different campaigns.

The qualified release binary has SHA-256
`2727bc755de151d0285d2cfff24d59f1b186e1ae40fb71db8ea14ec75b926da2`
and build ID `ab789770f61f743a4f7d5b0d8c3ed11f91460b08`.
The main performance candidate is
`5e646fb2209e7191c84c2242002f5dedb9da44df059fd33bd044075a9772e64e`;
the query-correction measurement uses
`01907ab0d6fb83703d607fc29f6209ed2708f357eebdb1420e701ddaa2932f14`.

Final checks passed: 287 compositor library tests, the separately enabled
two-real-GPU test, 81 Python tests, strict release Clippy for compositor and EGL
producer, and whitespace validation. The library suite's other two ignored
tests are manual benchmarks. Review specifically checked retained DMA-BUF hints,
fallback validation, queued-target lifetime, monotonic timestamp use, bounded
pacing correction, cursor byte order, deferred query submission, cached versus
live counters, stale pixel detection, telemetry windows and duplicate pair keys.

[Desktop restoration](desktop-restored.json) confirms the original desktop
process and its two clients remained alive on tty2, with DP-1 physically at
3840×2160/143.999568 Hz, scale 1.5 and adaptive sync preserved. A backed-up user
`hypr/monitors.lua` now requests 144 Hz explicitly. Reload introduced no new
configuration diagnostics; the old running desktop required the live mode to
be applied through wlr-output-management. Existing unrelated compatibility
diagnostics were not removed. No test-VT sessions remain. The installed/running
desktop executable was preserved; the qualified build is a separate artifact.

The qualified executable, complete raw archive and source patch are saved in
`~/.local/state/chonkstep/benchmarks/i9beef-native-2026-09-10/`.
[Artifact checksums](artifacts.json) identify them. The raw archive retains the
full images, client presentation logs and incomplete attempts; the repository
contains a smaller reviewable evidence set. Redundant terminal blank lines in
two copied test logs were normalized for repository whitespace checks; their
original checksums are recorded and the raw archive preserves the originals.
