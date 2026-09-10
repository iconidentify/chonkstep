# GPU diagnostics and experimental scanout

Native DRM sessions retain bounded per-output counters for every render pass:
powered off, pending page flip, no damage, inactive device, waiting for the frame
deadline, queued, empty, render failure and queue failure. Each attempt records
actual primary/overlay/cursor assignments, composited and zero-copy element
counts, active flags, full-damage policy, and the principal fallback reason for
each composited element. `unclassified` remains visible rather than inventing a
reason. The last 32 attempts are retained; cumulative counters cover the session.

Request `{"request":"debug","topic":"scene"}` on
`$CHONKSTEP_CONTROL_SOCKET` to read the snapshot. It includes:

- `native_pipeline`: per-output totals and skip reasons.
- `native_stage`: CPU durations for scene assembly, capture chrome, preparation,
  plane assignment/validation, composition submission, CPU fence waiting, KMS
  queueing and presentation feedback. `queue_to_vblank` is wall-clock latency.
- `native_frame`: the bounded recent history, including reason counters.
- `native_reason_order` / `native_reason_totals`: the reason names and totals.
- `surface_buffer`: each window's actual SHM/DMA-BUF/EGL buffer type, scale and
  pixel dimensions, queried only when requesting diagnostics.

CPU intervals are not GPU execution measurements. To request asynchronous GPU
queries, start the session with `CHONKSTEP_GPU_TIMINGS=1`. Supported contexts
report `gpu_timer status=EXT_disjoint_timer_query` and `gpu_stage` with
`stage=composition_gpu`, sample counts and nanoseconds. This brackets rendering
after scene imports. Results are read only when available, from a 16-query ring;
a full ring drops a measurement, and GPU clock-disjoint events discard affected
samples. Profiling adds no compositor `glFinish` or result waits. Queries can
perturb a workload, so compare equally instrumented sessions and record that
fact. An unsupported context reports `unavailable`, never a fabricated zero.

The scanout experiments remain opt-in until physical outputs have been qualified:

| Environment variable | Live Hyprland-compatible IPC control |
|---|---|
| `CHONKSTEP_EXPERIMENTAL_OVERLAY_SCANOUT=1` | `hyprctl debug-set overlay-scanout true` |
| `CHONKSTEP_EXPERIMENTAL_PRIMARY_SCANOUT_ANY=1` | `hyprctl debug-set primary-scanout-any true` |
| `CHONKSTEP_NO_DIRECT_SCANOUT=1` | `hyprctl debug-set no-direct-scanout true` |
| `CHONKSTEP_NO_CURSOR_PLANE=1` | `hyprctl debug-set no-cursor-plane true` |

Use `false` to undo a live change. `no-direct-scanout` disables both primary
variants and overlays, independently of cursor planes. All experiments retain
Smithay's atomic validation and composition fallback. Client buffers on any
plane are retained until the replacing page flip completes, including frames
whose primary plane is composited but which use an overlay.

Surface/output membership comes from clipped scene elements, including
subsurfaces, popups and workspace transitions. Captures do not change membership.
Hidden surfaces leave outputs. The presentation cache retains actual visible
pixels for each output; among outputs displaying at least half the largest
visible portion, the highest refresh rate paces the surface. An output being
removed or powered off retires its presentation state immediately. The remaining
output can become primary using its last successful frame, without requiring
an unrelated repaint. Occluded surfaces receive no callback budget.

The global DMA-BUF feedback advertises rendering capabilities. Per-surface
feedback can prefer the owning output's primary/overlay formats, intersected with
renderer-importable formats so composition fallback stays possible. Capability
tables are cached, refreshed on hotplug or policy changes, and withdrawn from
parked surfaces too. Moving between incompatible monitors no longer restricts
every client to a global intersection of all monitors' scanout formats.

## Reproduce the 5K rendering experiment

Build the shipping binary and the optional GPU producer fixture:

```sh
cargo build --release -p chonkstep-wayland
cargo build --release -p chonk-testkit --bin chonk-fullscreen-probe --features gpu-probe
python3 scripts/bench-gpu-scaling.py \
  --binary candidate=target/release/chonkstep-wayland \
  --output /tmp/cg5-results --pattern texture --require-gpu-timing
```

The private Weston GL kiosk host forces a real 5120×2880 nested framebuffer.
The matrix compares output/client scales 2/2 (native), 1.5/2 (fractional
fallback downsampling), and 2/1 (legacy native pixels). Integer-scale mismatches
do not inherently resample: the compositor preserves physical pixel sizes. The EGL fixture draws into real GPU buffers; its
producer-side finish ensures writes are complete before transfer on drivers
without implicit DMA-BUF synchronization. `--client-renderer shm` measures the
separate CPU-upload path. Saved screenshots verify dimensions and pixels after
timing. Raw process counters, GPU samples, competing GPU load, executable hashes,
renderer identity and individual samples are retained. Repeat `--binary` for
paired comparisons; run order alternates. Use `--gpu-timings off` for an equally
uninstrumented CPU/throughput comparison against older binaries, then a separate
timer-enabled campaign for GPU cost. `--pattern texture` renders deterministic
pixel-varying content on the producer GPU; captures must pass both calibration
pixel checks and a non-flat-content check. The default solid workload remains
available for reproducing the first checkpoint.

This is a rendering experiment, not a physical KMS scanout, panel latency, or
Apple hardware qualification. On the development machine both RTX 3090 devices
are accessible but every physical connector is disconnected. An attached output
is required to measure real overlay/primary scanout acceptance, page-flip
latency, hotplug and multi-monitor cadence end to end.

References: [Khronos timer-query specification](https://registry.khronos.org/OpenGL/extensions/EXT/EXT_disjoint_timer_query.txt),
[Smithay DRM compositor](https://smithay.github.io/smithay/smithay/backend/drm/compositor/index.html).

## Capture completion

GLES 3 output screencopy, ext-image-copy output/toplevel captures, user screenshots,
and window previews submit PBO downloads and poll an EGL fence placed **after**
the copy command. They map only after a zero-time fence check reports completion.
GPU submission is flushed, not finished. GLES 2 or missing fence support retains
an explicitly counted synchronous fallback. PNG encoding, publication, clipboard
I/O and recorder work remain on the existing capture worker.

Staging is bounded: one shared WLR fanout download, one ext-image-copy download,
two user screenshots (including worker jobs), two preview downloads, and one
diagnostic screenshot (including its PNG writer). Queues
retain their existing request limits. Services poll active work every 4 ms and
retired work every 100 ms, without adding an idle deadline after queues drain.
A five-second timeout fails consumers; their original staging slot and scene
buffer references remain retained until the GPU fence signals. Client destruction,
lock changes and stale image-copy constraints cancel delivery before mapping or
writing. No unbounded retirement queue is created by repeated cancellation.

The `readback` diagnostic line reports submissions, pending polls, synchronous
fallbacks, live/peak staging bytes, maps and CPU map time. Staging byte counts do
not include cached render targets, client SHM or PNG-worker images. The private
test door allows `CHONKSTEP_TEST_READBACK_DELAY_MS` (bounded at 10 seconds) to defer
readiness without blocking the compositor, proving input progress, timeout and
safe retirement deterministically. It has no effect without `CHONKSTEP_TEST_SOCKET`.
Diagnostic screenshot-marker export follows the same fence polling and uses one
bounded PNG writer; it no longer waits or compresses PNGs on the event loop.
Diagnostic frames remain outside benchmark sampling intervals.

Measured 5K results: [final textured workload and validation report](benchmarks/5k-final-2026-09-10/README.md)
and [earlier solid-workload checkpoint](benchmarks/5k-2026-09-10/README.md).

## Experimental render/target GPU selection

A native session can compose on another GPU with
`CHONKSTEP_RENDER_DEVICE=/dev/dri/renderD129`. The value must be a real DRM render
node. Selecting the existing renderer keeps the ordinary single-GPU path;
invalid or unidentifiable devices fail startup explicitly. Without the variable,
the existing device-selection behavior is unchanged.

The source GLES context owns all client imports, chrome textures and capture
readbacks for the session. Smithay `GpuManager<GbmGlesBackend>` supplies a
`MultiRenderer` that transfers composition into the target GPU's swapchain.
Swapchain allocation uses **target-renderable** modifiers, while default DMA-BUF
feedback identifies the **source** render device. Per-output scanout preferences
identify the target device and still intersect source-importable formats so
rejected scanout buffers can be composited. Equal modifier lists alone are not
enough: startup allocates real 64×64 target buffers for up to 256 candidate
formats and retains only formats the source GPU actually imports. Diagnostics
report `verified_scanout_formats`; no verified format means render-only feedback,
without an unsafe target allocation preference. This is a capability preflight,
not proof of every size or modifier working on a physical plane.
Custom GLES element drawing forwards
its damage rectangles to the multi-GPU transfer; opaque and partial updates must
not disappear merely because they skipped the frame's clear operation.

Native diagnostics include `multi_gpu=experimental`, both devices, DMA-copy and
CPU-copy counters, and CPU-copy pixel totals. Smithay prefers a shared DMA-BUF
texture; incompatible devices fall back to synchronous CPU mapping/upload.
That fallback is functional, not a performance recommendation. Strict client
buffer retention defaults on for a multi-GPU session, including when the target
driver is not NVIDIA. The existing explicit strict-release override still applies.
GPU elapsed queries measure the **source** GPU's composition timeline, not the
complete target GPU execution interval or display latency.

Hardware verification on this host passed both RTX 3090 transfer directions:
96×64 and 5120×2880 opaque frames, then a 31×23 partial update, with unchanged
surrounding pixels verified on the target. The driver selected CPU fallback for
all eight transfers; DMA acceleration across these two GPUs is not qualified.
Neither direction accepted a target allocation on the source GPU during the
scanout-feedback preflight (zero verified formats), so no target scanout tranche
is advertised on this pair.
Using linear-only target allocation failed on this driver; selecting the target's
advertised renderable modifiers resolved the binding failure. Reproduce:

```sh
CHONKSTEP_TEST_GPU_PAIR=/dev/dri/renderD128,/dev/dri/renderD129 \
  cargo test --locked -p wm-wayland --lib multi_gpu::tests::two_real_gpus \
  -- --ignored --nocapture
```

This supports a fixed render/target pair and the connected outputs of **one KMS
controller**. Adopting another KMS controller, render-device hotplug/migration,
and changing GPU selection during a session require further work. Physical
page flips, overlay planes, and cross-device scanout remain unverified here
because every display connector is disconnected.
