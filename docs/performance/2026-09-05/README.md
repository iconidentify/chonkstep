# Raw measurement series — 2026-09-05

Interpretation, methodology, executable hashes and limitations are in
[`../../performance.md`](../../performance.md).

- `final-*`: the final `combined-7` hidden-Dock comparison, seven alternating
  pairs and 60-second idle windows. `final-samples.jsonl` retains full numeric
  before/after process snapshots, descendants, frame counters and compatibility
  replies; `run` is added from the original per-run directory name.
- `dock-*`: the final visible-Dock comparison, seven alternating pairs and
  30-second intervals. Active resource use is unchanged; the first-scene median
  is slower, as reported explicitly in the main report.
- `combined1-*` and `samplers-*`: alternating compositor-session comparisons.
  Summaries preserve every sample as well as median/minimum/maximum; metadata
  identifies the machine, executable and fixture. `combined1` uses 30-second
  idle intervals; the earlier sampler experiment uses ten seconds.
- `combined6-*`: seven alternating pairs with 60-second idle windows after
  other stress tests/builds stopped. This preserved binary predates the later
  sampler-resume correction; hashes distinguish it from the final candidate.
- `theme-text-*`: five repeats of CPU raster and text-fitting workloads.
  `allocated_bytes_per_iteration` is cumulative Rust allocation traffic, not
  retained memory. `peak_live_growth_bytes` measures additional live Rust
  allocations above the warmed starting point.
- `glyph-*`: five sixteen-phase title/font-size churn runs. RSS is the memory
  of the isolated benchmark process, not a desktop-session footprint.
- `cache-normal-*`: five warm, ordinary-workload checks around cache bounding.
- `wallpaper-decode.jsonl`: alternating old/new decoders, 25 decodes per sample.
- `soak-before.jsonl` and `soak-after-combined3.jsonl`: twenty-minute continuing
  sessions. These are not controlled throughput runs. The earlier harness has
  fewer fields; its anonymous-memory accounting comes from the archived raw
  `/proc` snapshots. Temporary sampler pipes can affect descriptor counts.
- `soak-gles2-final.jsonl`: thirty minutes and 6,932 measured workload cycles
  on `combined-6`, with two CPU cores and Mesa's GLES 2 override. These
  development constraints are not a substitute for real older hardware.
- `soak-final7.jsonl`: a further ten-minute, 2,560-cycle confirmation of the
  final sampler-resume correction under those constrained settings. Other
  integration/diagnostic work overlapped; no throughput comparison is claimed.
- `intel-*` and `nvidia-*`: seven alternating hardware-rendered nested pairs
  per driver. Five-second idle intervals are too short for precise low-CPU
  comparisons. These ran alongside the constrained software soak on different
  CPU cores; see the report for shader-cache and shared-memory caveats.

All times, allocations, checksums and process-memory fields are emitted by the
real workload executables. No samples have been removed as outliers. These
files intentionally include intermediate implementations; names and binary
hashes distinguish them from the final result.

Large artifacts (saved binaries, compositor logs, screenshots and process
mapping snapshots) are kept outside Git at
`/home/chrisk/src/chonkstep-performance-artifacts/2026-09-05/` on the measurement
machine. Source tools are `scripts/bench-compositor.py`,
`crates/wm-theme/examples/performance.rs`,
`crates/chonk-shell/examples/wallpaper_decode.rs`, and
`crates/chonk-testkit/tests/stability_soak.rs`.
