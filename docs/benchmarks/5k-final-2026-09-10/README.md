# Final 5K rendering and correctness evidence

The final implementation passed **144 end-to-end tests**, **1,917 workspace
library tests** (12 ignored), **68 Python harness tests**, strict Clippy and
Rustdoc. The separately invoked two-GPU hardware test passed eight full/partial
transfers across both RTX 3090 devices. See [test counts and log hashes](validation.json),
[hardware output](multi-gpu-hardware.txt), and [Mac workflow details](../../mac-mode-validation.md#final-gpu-backed-regression).

The performance experiment comprises **27 verified 5K samples**: 18 paired
baseline/final runs with GPU queries disabled in both, followed by nine final
runs with asynchronous GPU queries enabled. Each cell has three runs, with ten
seconds measured after two seconds settling. Paired binary order and case order
alternate. Captures occur after the measurement interval.

## Paired CPU and throughput

| Output / client buffer scale | Client buffer pixels | Baseline CPU % | Final CPU % | Baseline / final rendered FPS |
| --- | --- | ---: | ---: | ---: |
| Native: 2 / 2 | 5120×2880 | 13.58 | 13.29 | 60.131 / 60.133 |
| Fractional fallback: 1.5 / 2 | 6826×3840 | 12.88 | 12.99 | 60.131 / 60.129 |
| Legacy: 2 / 1 | 5120×2880 | 13.48 | 13.78 | 60.118 / 60.132 |

Values are medians. CPU is compositor process CPU time divided by wall time;
100% means one core. Differences are small and mixed: −0.30, +0.10 and +0.30
percentage points respectively. Both builds reach the nested host's roughly
60 Hz cadence. These results establish **no clear throughput improvement**.

The unchanged Mac implementation at `da52c0d` is the baseline. The final binary
includes the GPU audit changes and the subsequent Mac copy-order repair.
Executable hashes, embedded versions and harness/fixture hashes are in
[paired metadata](paired-metadata.json). Individual timings, process counters,
renderer identities and GPU-load snapshots are retained in
[paired samples](paired-samples.json); [summary](paired-summary.json) includes
all three values and their ranges.

## Separate GPU composition measurements

| Case | Median GPU ms | Range of run means, ms |
| --- | ---: | ---: |
| Native | 1.678 | 1.640–1.684 |
| Fractional fallback | 1.673 | 1.665–1.740 |
| Legacy | 1.733 | 1.592–1.808 |

GPU values use available `EXT_disjoint_timer_query` results from the composition
stage. They exclude client drawing, capture and physical display latency. Each
sample contains 605 or 606 completed queries. The native/fractional ranges
overlap: this campaign does **not resolve a reliable fractional-scaling cost**.
It does prove the fractional case uses the larger texture and produces the
correct 5120×2880 result. An integer scale mismatch alone preserves native
physical pixels and does not inherently add resampling.

[GPU metadata](gpu-metadata.json), [individual samples](gpu-samples.json), and
[summary](gpu-summary.json) retain the measurements. The raw field
`render_wall_us_per_frame` includes nested host pacing; its roughly 15 ms values
are neither CPU consumption nor GPU execution time.

## Workload and limits

A private Weston 15.0.1 GL kiosk host forces a real 5120×2880 framebuffer on an
NVIDIA GeForce RTX 3090. The EGL client continuously redraws full-damage,
deterministic pixel-varying content on the GPU. It completes producer writes
before handing over its buffers; the compositor does not finish or wait for its
GPU timer queries. Final diagnostics verify DMA-BUF storage. Every capture
passes dimension checks, exact calibration pixels at three separated positions,
and a non-flat-content check. The benchmark refuses software rendering.

The content is a synthetic opaque texture, not a representative desktop app
mix. This exercises composition/resampling rather than text layout or client CPU
uploads. It does not measure native overlay/primary scanout or input-to-photon
latency. All eight physical display connectors were disconnected. The separate
multi-GPU test used CPU fallback for every transfer, and neither direction
verified an importable target allocation for scanout feedback. Cross-device DMA
acceleration is not established on this driver pair.

Unrelated compute workloads remained running. GPU 0 utilization snapshots ranged
from 4–69% during the paired campaign and 5–42% during the GPU-query campaign;
these snapshots include this experiment's own work. Host one-minute load ranged
from 2.78–4.07 and 2.92–3.56 respectively. Sampling and interleaving reduce some
bias but do not isolate this shared machine. Do not compare these textured
numbers directly with the [earlier solid-workload checkpoint](../5k-2026-09-10/README.md)
to claim a speedup: workload, implementation and competing load changed.

Raw logs, captures and per-run diagnostics remain in `/tmp/cg5-final-pair` and
`/tmp/cg5-final-gpu`. The JSON and hardware evidence above are committed so the
results survive cleanup of those temporary artifacts. The tested binary was
preserved before committing the final source, so its embedded version correctly
contains `5cb2322-dirty`; its exact SHA-256 is recorded in both metadata files.

## Reproduce

Build and preserve each compositor binary separately. Build the optional EGL
fixture with `cargo build --locked --release -p chonk-testkit --bin
chonk-fullscreen-probe --features gpu-probe`, then copy it before ordinary E2E
builds can replace the target path with a fixture lacking EGL support.

```sh
python3 scripts/bench-gpu-scaling.py \
  --binary baseline=/tmp/chonk-gpu-baseline/chonkstep-wayland \
  --binary final=/tmp/chonk-gpu-final2/chonkstep-wayland \
  --probe /tmp/chonk-gpu-texture-fixture/chonk-fullscreen-probe \
  --output /tmp/new-5k-pair --runs 3 --seconds 10 --settle-seconds 2 \
  --pattern texture --gpu-timings off

python3 scripts/bench-gpu-scaling.py \
  --binary final=/tmp/chonk-gpu-final2/chonkstep-wayland \
  --probe /tmp/chonk-gpu-texture-fixture/chonk-fullscreen-probe \
  --output /tmp/new-5k-gpu --runs 3 --seconds 10 --settle-seconds 2 \
  --pattern texture --gpu-timings on --require-gpu-timing
```
