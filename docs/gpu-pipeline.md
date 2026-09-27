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

- `native_pipeline`: per-output totals, skip reasons and `late_presentations`
  (queued frames whose hardware timestamp missed their intended refresh).
- `native_stage`: CPU durations for scene assembly, capture chrome, preparation,
  plane assignment/validation, composition submission, CPU fence waiting, KMS
  queueing and presentation feedback. `queue_to_vblank` is wall-clock latency.
- `native_frame`: the bounded recent history, including reason counters.
- `native_reason_order` / `native_reason_totals`: the reason names and totals.
- `surface_buffer`: each window's actual SHM/DMA-BUF/EGL buffer type, scale,
  pixel dimensions, import-node hint and format/modifier, queried only when
  requesting diagnostics.
- `multi_gpu_copies`: live cumulative DMA/CPU transfer counts and CPU-copied
  pixels. Device identity is cached at startup; these counters are read when
  diagnostics are requested, so the initial zero does not become permanent.

CPU intervals are not GPU execution measurements. To request asynchronous GPU
queries, start the session with `CHONKSTEP_GPU_TIMINGS=1`. Supported contexts
report `gpu_timer status=EXT_disjoint_timer_query` and `gpu_stage` with
`stage=composition_gpu`, sample counts and nanoseconds. This brackets rendering
after scene imports. Results are read only when available, from a 16-query ring;
a full ring drops a measurement, and GPU clock-disjoint events discard affected
samples. The end marker is flushed immediately so it cannot sit in the driver
until the next refresh after the rendered frame has already been submitted.
Profiling adds no compositor `glFinish` or result waits. Queries can
perturb a workload, so compare equally instrumented sessions and record that
fact. An unsupported context reports `unavailable`, never a fabricated zero.
On the native backend the GPU timestamp span surrounds `DrmCompositor` and can
include CPU submission gaps during plane preparation. It is elapsed GPU-clock
time across that span, not a hardware busy-time or shader-only measurement.

Atomic KMS fence support is determined by the primary plane's `IN_FENCE_FD`
property, retaining the existing NVIDIA version exclusion. A display-only card
need not expose `DRM_CAP_SYNCOBJ` to consume an exported render fence. This
allows the M3 external shadow driver to wait in its commit worker instead of
blocking the compositor's input thread. Unexportable fences still require the
CPU fallback; buffer ownership and completed-swap checks are unchanged.

On Zink/Apple M3/Honeykrisp, large shared-memory window updates use a GLES 3
pixel unpack buffer before texture upload. This moves costly image tiling off
the compositor's input thread; it still copies client pixels into staging
memory. Small or sparse damage keeps the direct path. Storage is re-specified
for each queued upload, preserving previous GPU reads, and existing texture
fences still order writes against sampling. `CHONKSTEP_SHM_UPLOAD_STAGING=0`
restores direct uploads; `1` opts other renderers into the path for testing.
The default on other renderers and GLES 2 is unchanged.

Successful DMA-BUF imports record the renderer's node when the buffer has no
node hint. Without this, Smithay rejects client buffers before attempting plane
export, even when scanout is enabled. This records a demonstrated import path;
it does not assert allocation origin or bypass GBM export and atomic validation.

The compositor's cached cursor sprites use `Argb8888`, matching Smithay's cursor
plane buffers. Their rasterizer produces RGBA bytes, so the import converts them
to BGRA once, preserving colors and premultiplied alpha. The previous `Abgr8888`
sprites rendered correctly through GLES but could not use the cursor copy path.

Native pacing learns from both CPU submission and actual KMS completion. A
queued frame retains its intended refresh until its page flip arrives. When
the hardware timestamp misses that target by more than half a refresh, the
scheduler adds 250 microseconds of headroom, bounded to half a refresh and
the existing maximum margin. It retains the GPU-derived margin for 30 seconds
before gradual decay. GPU timer queries are not required. This covers GPU work
that continues after an on-time CPU submission; idle frames and failed queue
attempts cannot create fictitious misses.

Refresh prediction requires a monotonic hardware scanout timestamp. The M3
shadow display reports a software completion after its copy and synchronized
swap; ChonkStep starts the next dirty frame immediately after that completion
instead of predicting another refresh from event receipt. The single pending
flip gate and the driver's synchronized swap still pace presentation and
protect buffer ownership. This avoids an extra compositor delay without
claiming a hardware timestamp for the shadow display.

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
Apple hardware qualification. Both RTX 3090 devices were accessible during that
experiment, but every physical connector was disconnected. Subsequent
[i9beef native testing](benchmarks/i9beef-native-2026-09-10/README.md) uses the
attached 4K/144 Hz panel to verify actual plane assignments, hardware presentation
cadence and display-state recovery. The
[cross-GPU campaign](benchmarks/cross-gpu-2026-09-10/README.md) adds AMD and Apple
hardware results and an M1 regression with both scanout experiments enabled.
Physical multi-monitor qualification remains separate.

[Half-Life 2 testing](benchmarks/hl2-i9beef-2026-09-10/README.md) subsequently
reproduced approximately 4 game FPS in native OpenGL when a model server occupied
most of the RTX 3090's memory. Using the game's built-in Vulkan renderer restored
approximately 144 Hz output cadence with the original graphics settings and
VSync. Direct scanout did not cure the OpenGL slowdown, and a Vulkan primary-any
transition produced KMS queue errors. The final game configuration uses shipping
scanout policy. Synthetic throughput and pixel tests do not qualify real games
under competing GPU resource use; the report retains the unsuccessful cases and
the limits of the crash diagnosis.

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

### Display-only KMS targets

The KMS device may have no render node at all: simpledrm on a boot
framebuffer, as on Apple silicon machines whose display controller has no
driver yet. Its primary node then names the target inside the GPU manager
(`target_kind=display-only` in diagnostics). It is never published as a render
device: linux-dmabuf feedback names only the selected render node. The target
renderer must be a software rasterizer (llvmpipe through kms_swrast) on that
KMS fd. Startup refuses a GPU found behind the display-only fd, such as Mesa's
kmsro wrapping of the render GPU. Proof uses `GL_RENDERER`, because Mesa's EGL
device query names the display's "compatible render-only device" for
kms_swrast screens selected through drirc. There is no cross-device
scanout-feedback probe for this target. Client buffers are never offered to
its planes, because a display-only device could show another GPU's buffer only
by copying it on the CPU in the kernel. Composition still copies each frame
into the target's own swapchain.

On the Apple M3 (J516S, `renderD128` with Honeykrisp/zink, `card0`
simpledrm), both transfer directions of the hardware test pass on a
separately built Mesa: full 3456×2160 frames and a 31×23 partial update, in
XRGB8888 and ARGB8888. All eight transfers are DMA copies: llvmpipe samples
the LINEAR buffer zink rendered. Without the vendored Smithay LINEAR retry
(see `vendor/README.md`) the same test passes through CPU copies. Reproduce it
with Mesa's environment pointing at a build that drives the render node
natively and pins the display device to kms_swrast:

```sh
CHONKSTEP_TEST_DISPLAY_ONLY_PAIR=/dev/dri/renderD128,/dev/dri/card0 \
  cargo test --locked -p wm-wayland --lib \
  multi_gpu::tests::a_render_node_composes_into_a_display_only_kms_target \
  -- --ignored --nocapture
```

### Display controllers paired through kmsro

A real display controller without a render node of its own (Apple's DCP,
Rockchip, Mediatek and similar SoCs) is usually rendered by Mesa's kmsro: the
GBM/EGL screen on the KMS fd renders with the paired GPU's render node,
straight into the display device's scanout buffers. EGL names that render node
as the renderer's device, and the session keeps its ordinary single-GPU stack
(`multi_gpu=disabled`): no intermediate buffer, no copy. The session decides
this in `renderer_render_node`: EGL's render node wins over the KMS device's
(absent) one unless `GL_RENDERER` is a Mesa CPU rasterizer, which marks the
kms_swrast case above. linux-dmabuf feedback and client scanout name the
render node, as on Asahi M1/M2.

On the Apple M3 (J516S) with its native display card (`m3-dcp`, devicetree
`apple,t6030-display-subsystem`), a Mesa whose drirc selects zink for that
card renders with zink on `renderD128` into the card's dumb buffers. The
ignored hardware test below runs the session's device steps without a seat or
DRM master: EGL on the KMS fd's GBM, the render-node decision, the single
stack, an ARGB8888 LINEAR swapchain buffer from `attach_output`'s allocator
and framebuffer exporter, and chonkstep's own scene elements drawn into it.
It then reads the pixels from the display device's dma-buf of that buffer,
which is the memory the display scans out: all 3456×2234 pixels match.

```sh
CHONKSTEP_TEST_KMSRO_DISPLAY=/dev/dri/renderD128,/dev/dri/card2 \
  cargo test --locked -p wm-wayland --lib \
  session::tests::a_kmsro_display_is_composed_by_its_gpu_in_scanout_memory \
  -- --ignored --nocapture
```

### Restricting linux-dmabuf to one Mesa build

`CHONKSTEP_DMABUF_REQUIRE_MESA=<prefix>` is an opt-in guard for sessions whose
render GPU can be driven safely by only one separately installed Mesa. On the
Apple M3, the distribution's Mesa accepts the GPU but submits work for an older
generation, and one fault ends the GPU until reboot. linux-dmabuf is shown only
to a client that meets one of two conditions. Either its process maps a library
from the prefix, or its environment will load the prefix when graphics start:
`LD_LIBRARY_PATH` names `<prefix>/lib`, every Vulkan driver manifest is inside
the prefix, and the process is not secure-exec. A process that maps a Mesa
driver library from elsewhere never sees the global, and neither does a process
whose `/proc` entries are unreadable. Such clients use shared memory and render
in software. Denials are logged with the client's PID and executable. The guard
cannot stop a process from opening the render node itself. The value `none`
hides the global from every client, so only the compositor renders on the GPU.
Unset, every client sees linux-dmabuf as before.

### Apple M3 session

`scripts/wayland-session-m3gpu.sh` is the opt-in login session built on these
pieces. It sets the private Mesa environment, `CHONKSTEP_DMABUF_REQUIRE_MESA`
and `XWAYLAND_NO_GLAMOR=1`, then runs the ordinary `scripts/wayland-session.sh`.
It has two display modes, `CHONKSTEP_M3_DISPLAY` or a leading
`--display=auto|dcp|simpledrm` argument (`auto`, the default, picks `dcp` when
the native display card exists):

- `simpledrm`: `CHONKSTEP_RENDER_DEVICE` composes on the M3 and copies each
  frame into the display-only boot framebuffer (the display-only target above).
- `dcp`: the native display card, found by its devicetree node
  (`apple,t6030-display-subsystem`), never by number. No
  `CHONKSTEP_RENDER_DEVICE`: kmsro renders on the M3 straight into the card's
  scanout buffers (the kmsro section above). The pre-flight requires the
  prefix drirc to select zink for `asahi`, `m3-dcp` and `apple`, a connected
  connector, the M3 as the only render node, and a kernel log without GPU
  faults, rejected compute jobs or failed DCP flips. Direct scanout of client
  buffers stays off (`CHONKSTEP_NO_DIRECT_SCANOUT=1`) unless
  `CHONKSTEP_M3_DIRECT_SCANOUT=1`.

`scripts/install-m3gpu-session.sh` adds the uwsm entry "chonkstep (M3 GPU,
experimental)", and `--remove` takes it away again. Neither script changes the
ordinary session entries. Under uwsm, pass the argument after `--`
(`uwsm start ... -- scripts/chonkstep-session-m3gpu --display=dcp`).

This supports a fixed render/target pair and the connected outputs of **one KMS
controller**. Adopting another KMS controller, render-device hotplug/migration,
and changing GPU selection during a session require further work. Physical
page flips, overlay planes, and cross-device scanout remain unverified here
because every display connector is disconnected.
