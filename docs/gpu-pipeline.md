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
  --output /tmp/cg5-results --require-gpu-timing
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
paired comparisons; run order alternates.

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
two user screenshots (including worker jobs), and two preview downloads. Queues
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
Diagnostic screenshot-marker export currently uses an explicit synchronous wait;
its frame is a verification artifact and is excluded from benchmark sampling.

Measured 5K results: [paired workload report](benchmarks/5k-2026-09-10/README.md).
