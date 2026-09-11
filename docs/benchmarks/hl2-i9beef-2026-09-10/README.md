# Half-Life 2 low-FPS investigation on i9beef

On 2026-09-10, native Half-Life 2 reproduced the reported severe slowdown at
approximately **4 game FPS** on the daily-use chonkstep session. Saving
**`-vulkan`** in Steam → Half-Life 2 → Properties → Launch Options avoided that
slowdown. With the original 4K graphics settings and VSync restored, the
uninstrumented game sustained approximately **144 completed display flips/sec**,
including after focus, Steam overlay and Space transitions. The AI model server
remained loaded during those successful checks.

This is an application configuration workaround, not a new compositor binary or
a demonstrated repair of the underlying OpenGL/driver failure. The earlier
synthetic GPU qualification did not cover this real-game failure mode.

## Configuration and evidence

- Local i9beef: i9-9900K, two RTX 3090 GPUs, NVIDIA 610.57.04,
  Linux 7.1.9-arch1-2. The display is on the first GPU.
- DP-1: 3840×2160 at 144 Hz, desktop scale 1.5.
- Half-Life 2, Steam app 220, build 19307283; native 32-bit `hl2_linux`,
  `-steam -game hl2_complete`, running through Xwayland.
- Original graphics settings: 3840×2160 fullscreen, 4× MSAA, 16× anisotropic
  filtering, `mat_picmip=-1`, HDR level 2, `mat_vsync=1`.
- Same running compositor throughout: PID 137376, executable SHA-256
  `2727bc755de151d0285d2cfff24d59f1b186e1ae40fb71db8ea14ec75b926da2`.
  Qualified source: `52c680d`; repository HEAD before this report: `70b7510`.
- Shipping policy throughout the final checks: experimental primary-any and
  overlay scanout off, ordinary primary/cursor scanout enabled, GPU queries off.

[Measurements](measurements.json) contain every timed sample, including failed
experiments. [Raw evidence](evidence/) retains before/after native counters,
CPU accounting, independent `nvidia-smi` samples, game frame times, launch
identity and recovery assertions. Collection scripts retain the exact PIDs and
socket of this session: they are campaign records, not portable automation.

## Reproduction and isolation

The existing `vllm-dualgpu.service` reserved roughly 21 GiB on **each** GPU for
a tensor-parallel model with a 262144-token context and GPU-memory utilization
set to 0.92. Running/waiting request counts were both zero before the temporary
stop. Its original configuration and capacity were restored, and HTTP health
returned 200 before the successful Vulkan tests.

| Timed condition | Completed display flips/sec | Observation |
|---|---:|---|
| Initial OpenGL launch, model loaded | 4.15 | Severe slowdown reproduced |
| Same slow process after unloading model | 2.95 | Freeing memory afterward did not recover it |
| Same slow process, primary-any enabled | 4.15 | Actual primary scanout; game remained slow |
| Fresh OpenGL launch, model absent, VSync off | 144.00 | Fresh allocation/process recovered |
| Same process, VSync re-enabled | 143.95 | VSync alone did not explain the slowdown |
| Same process, original intro map, VSync on | 144.00 | Original map also ran normally |
| Fresh OpenGL launch with VSync on, model absent | 144.00 | Recovery did not require disabling VSync first |
| Fresh OpenGL launch, model restored | 4.20 | Slowdown reproduced with model-loaded launch state |
| Vulkan, model loaded, intro | 144.00 | Built-in Vulkan renderer recovered throughput |
| Vulkan, model loaded, canal autosave | 143.90 | User's saved game area rendered correctly |
| Vulkan, movement/combat input, 60 seconds | 143.98 | Actual player movement verified |
| Vulkan, uncapped, after recovery transitions | 122.95 | Retained performance limitation of this test configuration |
| Vulkan, primary-any after that slowdown | 143.35 | 3 KMS queue failures; experiment rejected |
| Vulkan, no profiler, original settings and VSync | 144.00 | Final configuration |
| Same final configuration after recovery transitions | 143.97 | Zero render/queue failures |
| Normal Steam launch, New Game intro, no diagnostic arguments | 143.53 | Saved Vulkan option applied; zero render/queue failures |

The model-loaded launch state correlates strongly with the OpenGL failure.
Allocation/residency pressure or a driver/resource interaction is a plausible
mechanism, but this campaign does **not** isolate the exact NVIDIA or game bug.
There was still some free VRAM. No corresponding kernel OOM kill or NVIDIA Xid
was found. Stopping the model after a slow game launch was insufficient;
fresh-process comparisons were necessary.

MangoHud independently confirms that the initial symptom is low **game** FPS,
not merely a low compositor counter. After excluding the first 20 logged seconds:

| Renderer, model loaded | Time-weighted game FPS | Median frame time | p99 frame time |
|---|---:|---:|---:|
| OpenGL | 3.97 | 249.55 ms | 371.77 ms |
| Vulkan, uncapped diagnostic | 217.81 | 4.51 ms | 7.06 ms |

[Frame-time analysis](game-frame-times.json) records sample counts and intervals.
These traces contain different intro/loading progression, not an identical
deterministic timedemo, so they do not establish a general speedup multiplier.
Only FPS, frame time and elapsed time are retained from MangoHud: its 32-bit GPU
power readings were invalid. GPU telemetry instead comes from `nvidia-smi`.
MangoHud also materially perturbed game CPU use; instrumented/uninstrumented game
CPU percentages must not be compared as an optimization result.

## Presentation and recovery

The first Vulkan diagnostic launches used `VK_PRESENT_MODE_IMMEDIATE_KHR`.
After focus/overlay/Space transitions, one 20-second sample fell to 122.95
display flips/sec despite the HUD showing over 250 game FPS. This is retained
as a limitation, not discarded as noise or presented as a successful 144 Hz run.

Restoring the original video configuration **before a fresh launch** selected
`VK_PRESENT_MODE_FIFO_KHR` at 3840×2160 with four swapchain images. The final
Steam launch option contains only `-vulkan`; there is no permanent profiling
wrapper, frame-cap override or experimental scanout flag.
Vulkan automatically rewrote the detected vendor/device IDs and enabled
`r_lightmap_bicubic`; the requested resolution, MSAA, anisotropic filtering,
texture quality and VSync values remained unchanged.

The final unprofiled recovery run verified three focus-away/return cycles and
Space 1 → 2 → 1 parking/return, asserting the same process, correct workspace,
focus and fullscreen state. A captured Steam overlay verified that Shift+Tab
actually opened it; the subsequent gameplay capture verified its removal.
Both 30-second final-configuration samples had zero render failures, queue
failures and late-target presentations. The earlier 60-second input workload
issued movement and attack inputs and verified changed in-game coordinates;
it was not just an idle menu benchmark.

A further launch through `steam://rungameid/220` used exactly
`hl2_linux -steam -game hl2_complete -vulkan`, with no diagnostic arguments or
MangoHud library loaded. Starting New Game from its normal menu produced 143.53
completed display flips/sec over 30 seconds, also with zero render/queue failures
and zero late-target presentations. This last sample covers the intro, while the
preceding recovery samples cover the canal save.

The separate primary-any Vulkan experiment recorded 2868 actual primary queues
but three `EINVAL` page-flip failures during the transition. It was immediately
disabled and is **not qualified as the gaming fix**. The default-policy Vulkan
samples used composition because the client format did not match the primary
swapchain format. Direct scanout was therefore unnecessary for recovering from
the approximately 4 FPS failure.

Completed KMS flips measure output cadence, not uniquely displayed game frames,
input-to-photon latency or a whole-game FPS guarantee. A zero late-target count
also does not prove that every physical refresh had a new game frame.

## Crash findings and limits

Two earlier **game** cores were found; neither was a compositor crash:

- 12:43:23 PDT, PID 1022313: `MatQueue0` faulted in a memcpy with a null
  destination. Callers included `shaderapidx9.so`, `studiorender.so` and
  `materialsystem.so`. Proprietary game frames were largely unsymbolized.
- 13:09:42 PDT, PID 1039853: the main thread faulted in `_XGetAtomName` in
  32-bit libX11, called through `XGetAtomName`, with an invalid atom-cache entry
  pointer. The caller above libX11 could not be reliably unwound.

The cores do not prove that both crashes share the FPS failure's cause, or that
Vulkan permanently fixes them. Process virtual sizes around 1.6 GB did not
indicate 32-bit address-space exhaustion. The compositor log also contained two
earlier `GL_INVALID_VALUE` messages; no causal connection to the game crashes
was established. Full debugger records remain private locally. Extracted cores
were deleted, and no core memory or user save data is included in this repository.

No new game crash occurred in the Vulkan checks. This is finite reproduction and
recovery coverage, not hours-long gameplay stability or qualification of other
games, drivers, GPUs, resolutions or active AI inference. Synthetic benchmark
results remain useful for their stated workloads; real games under competing
GPU resource use must be qualified separately.

## Final state

The test game was closed and focus returned to the original terminal. The
saved Steam option is `-vulkan`; the AI service is healthy with its original
configuration, and both experimental scanout flags are off. No compositor
restart, clock override or driver change was required.

Original save files were restored and verified byte for byte, and the private
test save was removed locally and from Steam Cloud. The cloud conflict caused
by restoring the original canal state was resolved in favor of that verified
local copy. A final normal launch/exit completed cloud synchronization. Ordinary
engine-generated configuration changes are listed in
[final-state.json](evidence/final-state.json); diagnostic console/FPS settings
and the MangoHud launch wrapper are absent. Original backups remain private in
`~/.local/state/chonkstep/benchmarks/hl2-i9beef-2026-09-10/`.

Review checks retain all 16 timed samples, recompute counter deltas and frame-time
statistics, separate game submissions from output flips, include all three KMS
queue errors, and exclude user saves, Steam account configuration and core
memory from the repository. No Rust code changed, so this report does not claim
a new build or a new unit-test run.
