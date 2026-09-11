# Native 5K pixel workload, GPU-backed nested backend

Three paired runs per case, 10 seconds measured after 2 seconds settling. Binary
order alternates. Each sample verifies the 5120×2880 output capture, opaque probe
pixels, source-buffer dimensions, and (on the instrumented candidate) DMA-BUF
storage. The EGL producer completes its own writes before handing buffers to the
compositor. A private Weston 15.0.1 GL kiosk host forces the physical output size.
The real renderer is NVIDIA GeForce RTX 3090. Raw artifacts, renderer and process
logs, GPU-load snapshots, and per-run JSON remain in `/tmp/cg5-pair2`.

| Case: output scale / client buffer scale | Source pixels | Baseline CPU % | Candidate CPU % | Baseline / candidate rendered FPS | Candidate GPU ms |
| --- | --- | ---: | ---: | ---: | ---: |
| Native: 2 / 2 | 5120×2880 | 14.58 | 14.08 | 60.132 / 60.126 | 1.786 |
| Fractional: 1.5 / 2 | 6826×3840 | 15.08 | 15.48 | 60.131 / 60.129 | 1.999 |
| Legacy: 2 / 1 | 5120×2880 | 13.88 | 14.38 | 60.131 / 60.132 | 1.835 |

Values are medians of three runs. CPU is compositor process CPU time divided by
wall time, where 100% represents one core. GPU is elapsed-query composition time
per completed query. `render_cpu_us_per_frame` in raw JSON is a legacy field name:
on this nested backend it measures the render-call wall interval, including host
pacing, **not CPU consumption or GPU execution**. Do not use its roughly 15 ms
values as an estimate of composition cost.

Fractional output scale 1.5 with integral buffer scale 2 samples a larger client
texture down to the output. It costs about 0.213 ms more GPU time in this simple
opaque workload than the matching native case. This is a workload observation,
not a universal cost of fractional scaling. Integer scale mismatch alone does
not cause resampling: ChonkStep's physical-pixel geometry preserves the native
source size in the legacy case.

There is no demonstrated throughput gain here: all cases reach the nested host's
roughly 60 Hz cadence. Candidate CPU differences are small and mixed. The
candidate has opt-in GPU queries enabled while the baseline predates those
queries. Another process was doing substantial GPU compute during these runs;
its load was recorded and left alone. These data do not establish a compositor
ranking, idle power result, input-to-photon latency, or native KMS scanout result.
All physical connectors were disconnected. The candidate is stage 1, before the
asynchronous capture and multi-GPU changes; hashes are in `metadata.json`.

Reproduce (preserved binary paths are local artifacts):

```sh
python3 scripts/bench-gpu-scaling.py \
  --binary baseline=/tmp/chonk-gpu-baseline/chonkstep-wayland \
  --binary stage1=/tmp/chonk-gpu-stage1/chonkstep-wayland \
  --probe /tmp/chonk-gpu-fixture/chonk-fullscreen-probe \
  --output /tmp/cg5-new --runs 3 --seconds 10 --settle-seconds 2
```
