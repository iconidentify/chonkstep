# ChonkStep progress update

A focused performance and compatibility push landed after the 0.3.0 preview, with before-and-after measurements captured throughout:

- Hidden Dock instruments now start their sampling threads only when first shown, pause while hidden, and resume with fresh readings.
- In seven alternating before/after pairs of our nested software-rendered, hidden-Dock fixture, compositor idle CPU fell from 0.233% to 0.0833% of one core: about 64% less CPU. Context switches fell from 45.92 to 12.10 per second, about 74% fewer, with 18 fewer threads at startup. XWayland and Hyprland-compatible IPC stayed enabled on both sides.
- Glyph caching now uses budgeted eviction instead of retaining an ever-growing collection of rasterized text. In the separate glyph-churn benchmark process, final-phase resident memory fell from 137.6 MiB to 24.5 MiB, an 82% reduction. Large working sets can pay extra rerasterization cost; this is not the compositor's total memory footprint.
- Long panel labels no longer trigger repeated quadratic text fitting. The 4,096-character label benchmark fell from 1,847.7 ms to 1.745 ms, with identical output pixels in the measured workloads.
- Removed 28 redundant owned-pixel copies across compositor chrome. The full-decoration library benchmark used by X11 fell from 2.291 ms to 0.378 ms and roughly halved allocation traffic; ordinary sparse Wayland decorations also improved.
- Empty Overview rendering at 1920×1080 fell from 1.291 ms to 0.703 ms in the library benchmark, with roughly half the allocation traffic and peak live pixel-buffer memory.
- Bundled wallpaper decoding now reuses the existing image decoder. Stock Lavender Grid decoding fell from 20.280 ms to 14.445 ms, with exact pixel matches across all 14 bundled artworks. Unchanged embedded backgrounds also skip redundant decoding, scaling, and upload work during restyles; mutable Omarchy wallpaper files still reload.
- X11 autostart now receives ChonkStep's own reserved XWayland display before applications launch, preventing startup against a missing or inherited host display.
- Nested rendering now accepts the same GLES 2 minimum as the native backend, with a compatible screenshot and screen-capture readback path for contexts without GLES 3 pixel-buffer support.
- Nested buffer-age queries now restore the correct EGL surface after offscreen capture, fixing the reproduced EGL surface errors and preserving partial-damage rendering.
- Sampler lifetime and resample races are covered by regression tests. Visibility generations reject stale readings after reopening the Dock, preventing false network-rate spikes.
- A repeatable benchmark harness now alternates preserved executables in private sessions, verifies real Wayland and Hyprland-compatible IPC readiness, and records first-scene timing, CPU, memory, threads, context switches, and raw samples without replacing the running desktop.
- Validation includes 1,867 passing workspace tests, 114 passing checks in the final nested software-rendered integration and installed Omarchy fixture suite, plus 80 minutes of baseline and candidate desktop-churn soaks covering 20,639 cycles. Constrained GLES 2 and Intel-rendered integration runs also passed during the investigation.

The limits matter too: retained idle compositor memory was essentially unchanged. Hidden-Dock first-scene medians improved from 280 to 265 ms with overlapping ranges, while the visible-Dock control regressed from 306 to 326 ms and showed no idle CPU saving. Those are nested rendering measurements, not cold-login times. Two nested NVIDIA/Alacritty failures and some memory growth during churn remain unresolved. Native hardware coverage and a matched Hyprland comparison are still needed; these results do not establish competitor superiority.

Full methodology, before/after data, tradeoffs, and follow-up work: https://github.com/iconidentify/chonkstep/blob/main/docs/performance.md

Huge thanks to everyone contributing patches, reviews, bug reports, and testing. More stabilization and performance work is ahead.

https://github.com/iconidentify/chonkstep
