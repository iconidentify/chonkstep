# Native GPU audit implementation

Requested after the Mac mode work, 2026-09-10. The Mac implementation and its
validation report are committed at `da52c0d`; GPU changes follow on
`codex/native-gpu-performance`.

## Final implementation status

All six audit areas now have an implementation or measurement in this branch:

| Audit priority | Delivered | Qualification limit |
| --- | --- | --- |
| Native instrumentation | Ten CPU/wall stages, optional asynchronous GPU queries, bounded per-output history, pass counters, actual plane assignments and principal composition reasons | Real KMS timing and driver rejection frequencies need an attached display; `unclassified` stays visible |
| Overlay scanout | Independent opt-in and live control, atomic fallback, retention through the replacing flip for every client plane | Experiment remains off by default pending physical plane tests |
| Broader primary scanout | Independent opt-in, renderer-compatible feedback, atomic fallback, global disable takes precedence | Physical fullscreen acceptance is unqualified here |
| Surface/output tracking | Clipped scene membership, enter/leave, per-output DMA-BUF feedback, retained visibility and refresh-aware callback ownership | Nested protocol tests pass; physical mixed-refresh cadence remains to qualify |
| Native 5K scaling | Real GPU-buffer producer, matching/fractional/legacy scales, verified 5120×2880 captures, alternating paired runs and separate GPU-query runs | Nested host cadence and competing compute limit conclusions |
| Async readback and multi-GPU | Fence-polled bounded capture queues, cancellation/retirement, separate diagnostic PNG writer; explicit fixed render/target pair using Smithay MultiRenderer | GLES/fenceless capture and incompatible GPU transfers have counted synchronous fallbacks; multiple KMS controllers and GPU migration are not implemented |

Controls, queue bounds, feedback policy, and reproduction commands are in
[GPU pipeline documentation](gpu-pipeline.md). The final tests also fixed a
[Mac copy/switch race](mac-mode-validation.md#final-gpu-backed-regression) and
overview preview refresh across multiple asynchronous completion batches.

The [final 27-sample 5K report](benchmarks/5k-final-2026-09-10/README.md) separates
equally uninstrumented baseline comparisons from GPU-query measurements. Both
builds sustain about 60.13 FPS, with small mixed CPU differences. Final median
composition times are 1.678 ms native, 1.673 ms fractional and 1.733 ms legacy;
overlapping ranges do not establish a reliable fractional cost or a speedup.

### Final correctness evidence

**144 distinct E2E tests across 18 targets passed** on a private Weston GL host
with a real RTX 3090; no missing-client skips. This includes Mac workflows with
Chromium, Writer, Nautilus, foot and XWayland; clipboard transport/persistence;
lock/input/repeat; screenshots/recording and both capture protocols; membership,
hidden damage, live diagnostic controls, fullscreen, overview and idle.
Durable counts and log hashes are saved in
[`validation.json`](benchmarks/5k-final-2026-09-10/validation.json).
Counts and commands are in [the final validation record](mac-mode-validation.md#final-gpu-backed-regression).

Workspace libraries: **1,917 passed, 12 ignored**. Strict workspace Clippy,
private Rustdoc, the optional GPU producer's strict Clippy, and all 68 Python
harness tests passed. The hardware-only multi-GPU test was invoked separately and passed full and partial
opaque rendering at 96×64 and 5120×2880 in **both** RTX 3090 directions. All eight
transfers used CPU fallback; zero target formats passed cross-device import
preflight. The implementation therefore withholds target scanout preferences
on this pair instead of encouraging allocations the source GPU cannot import.

Local evidence: `/tmp/chonk-final-TARGET.log` (idle's successful run uses
`/tmp/chonk-final-idle-retry.log`), `/tmp/chonk-gpu-final-unit2.log`,
`/tmp/chonk-interop-lint.log`, `/tmp/chonk-final-rustdoc.log`,
`/tmp/chonk-texture-fixture-lint.log`, and
`/tmp/chonk-multigpu-hardware-final.log`. The 17 non-idle nested targets preceded
the native-only import preflight change; idle, strict lint and the real two-GPU
test ran after it. The initial idle build collided with an in-progress edit;
its completed-source retry passed.

The preserved final benchmark binary is
`/tmp/chonk-gpu-final2/chonkstep-wayland`, SHA-256
`fd8c279dd16cc645b1f80c8d5acf5b62b276bfbece16cc53b2aef6e23c8c4613`.
It was built from this branch before the final source commit, so its embedded
version correctly reports `preview-v0.4.4-3-g5cb2322-dirty`.

### Hardware qualification still required

All eight physical connectors are disconnected. No result here demonstrates
overlay acceptance, broadened primary acceptance, physical page-flip latency,
mixed-refresh monitor pacing, Apple keyboard firmware behavior, or cross-device
scanout. On attached hardware, collect the diagnostics with each scanout switch
independently and together, then exercise fullscreen transitions, translucent
overlap, capture, lock, unplug/replug and DPMS. Verify output membership and
callback ownership while moving a client between differently scaled/clocked
outputs. These experiments remain opt-in until that evidence exists.

## Baseline and environment

Preserved optimized baseline:
`/tmp/chonk-gpu-baseline/chonkstep-wayland`, source
`preview-v0.4.4-1-gda52c0d`, build ID
`74f7d20488b6a604efa2ef2f90dc71ebac6a19c4`, SHA-256
`a59b0b54995e47f9d8924f7723f5355e85a573869fdfcaa11d94fc793b4b751b`.

The isolated Weston GL host probe succeeded with the real NVIDIA GeForce RTX
3090 renderer (logs and metadata in `/tmp/chonk-gpu-host-probe`). The machine
has two NVIDIA DRM devices and all eight display connectors report disconnected.
GPU rendering/readback can be measured here; native KMS scanout on a physical
panel cannot be qualified without a connected output. Existing unrelated GPU
compute workloads must be recorded as an experiment limitation.

## Work order and completion evidence

1. Native pipeline instrumentation: separate scene build, plane selection,
   composition submission, CPU fence wait, KMS queue, page flip and feedback
   timing. Count every attempted, skipped, empty, failed, composited and scanned
   frame. Record actual element fallback reasons and active policy. Expose live
   diagnostics without per-frame string allocation or unbounded traces. Identify
   CPU intervals explicitly; GPU completion time needs asynchronous GPU queries.
2. Experimental overlay scanout: independent opt-in plus the existing global
   scanout escape switch. Retain buffers for every plane until replacement,
   including overlay-only frames; the existing scene hold only recognizes
   primary direct scanout. Validate policy, hold lifetimes and fallback paths.
3. Experimental primary-any-format: independent opt-in, renderer-importable
   formats, atomic test/fallback retained. Ensure no-direct-scanout disables both
   primary variants and overlays. Keep cursor control independent.
4. Surface output membership and DMA-BUF feedback: replace entering every
   surface on every output with real membership, including subsurfaces, popups,
   movement, visibility and hotplug. Preserve the existing render-state-based
   presentation/callback selection. Cache feedback per output and refresh it
   on capability/policy changes; no allocation promise from another monitor.
5. 5120×2880 scaling measurements: matching and mismatched client scales,
   deterministic animation, paired baseline/candidate order, saved raw timings,
   pixels and renderer identity. Report GPU and nested/KMS limits accurately.
6. Asynchronous capture readback and multi-GPU: bounded readback queue with fence
   polling and deadline/cancellation, then explicit render/target device support
   using the pinned Smithay APIs. Preserve correct DMA-BUF import, feedback,
   capture and buffer-release behavior across device changes. Qualify what can
   actually be exercised and document hardware-only checks still required.

Use the repository's strict lint/doc checks and affected unit/protocol suites.
Run the Mac conformance suite against the final candidate so GPU changes cannot
silently regress the earlier deliverable. No comparative “best compositor” claim
without measured, reproducible evidence.

## Baseline source observations

The pinned Smithay 0.7.0 `FrameFlags::DEFAULT` includes overlays. The baseline repository
enabled only cursor and conservative primary scanout. The online
Smithay documentation sometimes describes newer APIs than this vendored tree
(for example `Kind::ScanoutCandidate` is absent locally); implementation must
follow the pinned source.

`RenderFrameResult` exposes actual primary, overlay and cursor assignments and
per-element presentation states. Existing fallback reasons distinguish only
unsupported format and failed scanout; unclassified branches need diagnostics.
The baseline already selected a primary presentation output from rendered
visibility and refresh rate, but `new_surface` entered all outputs and
DMA-BUF feedback used a global capability intersection.

References: [Smithay DRM compositor](https://smithay.github.io/smithay/smithay/backend/drm/compositor/index.html),
[GPU manager](https://smithay.github.io/smithay/smithay/backend/renderer/multigpu/struct.GpuManager.html),
[memory export](https://smithay.github.io/smithay/smithay/backend/renderer/trait.ExportMem.html).

## Historical first implementation checkpoint

Implemented native per-output CPU telemetry, bounded frame/scanout reason
history, optional asynchronous GPU timer queries, the two independent scanout
experiments, client-plane buffer lifetime retention, scene-based surface/output
membership, per-output DMA-BUF feedback, and retained per-output presentation
visibility for callback ownership after movement/removal/power changes.

Validation so far: 284 Wayland unit tests passed (2 ignored); strict workspace
Clippy and optional EGL fixture Clippy; GPU-backed hidden-surface E2E (2 tests)
and live diagnostic/GPU-query E2E (1 test). Logs:
`/tmp/chonk-gpu-unit.log`, `/tmp/chonk-gpu-membership-final.log`,
`/tmp/chonk-gpu-timing-e2e.log`, `/tmp/chonk-gpu-strict-lint.log`,
`/tmp/chonk-gpu-probe-lint.log`, `/tmp/chonk-gpu-docs.log`.

Preserved first GPU candidate: `/tmp/chonk-gpu-stage1/chonkstep-wayland`, SHA-256
`9544a49c7f04c2674711a4f06c13ee1e628894816cff4df2a8d1d5c10884c501`.
The optional real EGL producer is preserved at
`/tmp/chonk-gpu-fixture/chonk-fullscreen-probe` so subsequent default E2E builds
cannot replace it with a fixture lacking the EGL feature.

5K SHM and EGL smoke captures verified physical dimensions and expected pixels.
Those single short runs are feasibility evidence, not performance conclusions.
The first paired harness rejected an incorrect expected source size: integer
output/client scale mismatches remain native physical pixels; the common
resampling case is output 1.5 with a client buffer scale of 2. The corrected
matrix measures 2/2, 1.5/2, and 2/1 and refuses mismatched source dimensions.
The first paired results were collected in `/tmp/cg5-pair2`.

At this checkpoint, asynchronous capture completion and render/target GPU
support were the next implementation stages; both have since landed. Physical KMS qualification remains unavailable with all
connectors disconnected.

## Historical capture and 5K checkpoint

PBO readback now defers mapping behind a post-copy EGL fence for WLR screencopy,
ext-image-copy output/toplevel capture, user screenshots and window previews.
Queues and retirement are bounded; canceled GPU work retains client-buffer
references until completion. A five-second deadline fails stalled consumers
without blocking input. Older GLES/fence paths report synchronous fallback.

GPU-backed validation: screencopy pressure 8/8, image-copy 5/5, capture cache 1/1,
user screenshot/recording 17/17. New fault-injection tests prove input progress,
no writes to destroyed image-copy buffers, bounded staging after cancellation,
and safe retirement after a timeout. Wayland unit suite: 284 passed, 2 ignored;
strict Wayland/testkit Clippy passed. Logs: `/tmp/chonk-async-screencopy_pressure.log`,
`/tmp/chonk-async-image-final.log`, `/tmp/chonk-async-capture_cache.log`,
`/tmp/chonk-async-capture_tool.log`, `/tmp/chonk-async-unit.log`,
`/tmp/chonk-async-lint.log`.

The repeated 5K comparison is complete: [report and machine-readable results](benchmarks/5k-2026-09-10/README.md).
All cases sustained roughly 60 FPS; candidate GPU composition medians were
1.786 ms native, 1.999 ms fractional, and 1.835 ms legacy. CPU differences were
small and mixed. These nested, shared-GPU measurements establish no compositor
ranking and no KMS scanout result.
