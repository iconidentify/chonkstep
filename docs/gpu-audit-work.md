# Native GPU audit implementation

Requested after the Mac mode work, 2026-09-10. The Mac implementation and its
validation report are committed at `da52c0d`; GPU changes follow on
`codex/native-gpu-performance`.

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

## Source observations

The pinned Smithay 0.7.0 `FrameFlags::DEFAULT` includes overlays. The repository
currently enables only cursor and conservative primary scanout. The online
Smithay documentation sometimes describes newer APIs than this vendored tree
(for example `Kind::ScanoutCandidate` is absent locally); implementation must
follow the pinned source.

`RenderFrameResult` exposes actual primary, overlay and cursor assignments and
per-element presentation states. Existing fallback reasons distinguish only
unsupported format and failed scanout; unclassified branches need diagnostics.
The compositor already selects a primary presentation output from rendered
visibility and refresh rate, but `new_surface` still enters all outputs and
DMA-BUF feedback uses a global capability intersection.

References: [Smithay DRM compositor](https://smithay.github.io/smithay/smithay/backend/drm/compositor/index.html),
[GPU manager](https://smithay.github.io/smithay/smithay/backend/renderer/multigpu/struct.GpuManager.html),
[memory export](https://smithay.github.io/smithay/smithay/backend/renderer/trait.ExportMem.html).

## First implementation checkpoint

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
Repeated paired results are being collected in `/tmp/cg5-pair2`.

Asynchronous capture completion and render/target GPU support remain the next
implementation stages. Physical KMS qualification remains unavailable with all
connectors disconnected.
