# NVIDIA, dmabuf synchronization, and the hover flicker

Why hardware-accelerated clients could flicker on NVIDIA under this
compositor, what was actually measured, what changed, and which levers exist
if it happens again. Written after the 2026-08-31 investigation into
"hovering over page elements in Edge flickers"; every claim below says
whether it is measured or reasoned, because on this topic folklore outnumbers
evidence about ten to one.

## The driver property everything follows from

NVIDIA's driver attaches no implicit fences to a dmabuf. On Mesa drivers the
kernel quietly orders a client's GPU writes against the compositor's GPU
reads through the buffer's reservation object; on NVIDIA nothing does,
in either direction, unless both sides use explicit sync
(`wp_linux_drm_syncobj_v1`). That cuts both ways:

- **Acquire** (client writes, compositor reads): sampling a buffer the
  client is still rendering shows the half-drawn state — the original
  "typing in the URL bar flickers" bug.
- **Release** (compositor reads, client rewrites): telling the client
  "the buffer is free" while the compositor's GPU still has sampling
  commands queued lets the client overwrite pixels mid-read — same visual,
  opposite cause.

## What is measured (live session, 2026-08-31)

The compositor advertises `wp_linux_drm_syncobj_v1` (gated on
`supports_syncobj_eventfd`) and arms a bounded transaction blocker per
commit (`dmabuf.rs`). From the live log of the affected session:

- explicit sync was advertised at every startup;
- the blocker deadline guard (`BLOCKER_DEADLINE`, 1s) warned **zero**
  times — no armed blocker ever saw a fence that failed to signal;
- libinput repeatedly reported 250–300ms event-processing stalls.

A `WAYLAND_DEBUG=1` capture of Microsoft Edge 152 (Chromium 152) launched
against the live compositor settles the "does Chromium bind it" question
**measured, not predicted**:

```
wl_registry#2.bind(14, "zwp_linux_dmabuf_v1", 4, …)
wl_registry#2.bind(15, "wp_linux_drm_syncobj_manager_v1", 1, …)
wp_linux_drm_syncobj_manager_v1#21.get_surface(new id wp_linux_drm_syncobj_surface_v1#36, wl_surface#25)
wp_linux_drm_syncobj_surface_v1#36.set_acquire_point(…timeline#35, 0, 1)
wp_linux_drm_syncobj_surface_v1#36.set_release_point(…timeline#33, 0, 1)
```

Every dmabuf commit of the toplevel carried an acquire point and a release
point (points 1, 2, 3, 4… over the capture). The cursor surface
(`wl_surface#24`, the target of `wl_pointer.set_cursor`) uses plain
`wl_shm` 48×48 buffers, which need no fencing at all. So for current
Edge/Chromium the **acquire half is healthy end to end**: the client
declares readiness, the compositor waits for it, and the waits demonstrably
resolve in time.

That kills the simplest theory ("Edge never binds syncobj, so it gets the
original unsynchronized sampling"). What it leaves standing is the release
half.

## The release race (reasoned from code, closed by default on NVIDIA)

Smithay 0.7 signals a buffer's syncobj release point — and sends
`wl_buffer.release` — from the CPU when the last clone of its internal
`Buffer` handle drops (`InnerBuffer::drop`, `renderer/utils/wayland.rs`).
The compositor's per-frame element list holds one such clone, but it died at
the end of the render pass; the surface state holds the other and drops it
the moment the client's *next* commit merges. Neither event is ordered
against the GPU executing the sampling commands of the frame just queued.
On Mesa the kernel covers the gap; on NVIDIA a client that trusts the
release — which explicit sync entitles it to — can rewrite a buffer the
compositor's GPU is still reading. The faster the client recommits, the
tighter its buffer pool cycles and the likelier the race: rapid small
repaints, i.e. exactly hover effects.

The fix: each session output keeps the render elements of a composited frame
while its page flip is in flight (`SessionOutput::pending_scene`,
`session.rs`) and drops them when the flip completes. The atomic commit
carries the render fence as its in-fence (`EGL_ANDROID_native_fence_sync` is
present on this driver — verified in the live log's EGL extension list), so a
completed flip proves the sampling retired; where the fence cannot be
exported the render path already waited on the CPU before queueing. Release
timing therefore becomes: no earlier than the flip of the last composited
frame that sampled the buffer.

Direct scanout has a stronger lifetime. The vblank completing an atomic
commit makes the client's buffer the primary plane's *current* source; the
display engine keeps reading it after that event. Such a scene moves from
`pending_scene` to `scanout_scene` at vblank and is released only after a
later flip replaces it, or after the crtc is disabled/reset. This is not
NVIDIA-specific and is always enforced when direct scanout is selected.

- **Gate**: `CHONKSTEP_STRICT_BUFFER_RELEASE=1` forces it on, `=0` off.
- **Default**: on when the DRM driver name contains `nvidia`, off
  otherwise — other drivers get this ordering from the kernel for free, so
  holding buffers a frame longer would buy nothing there.
- **Cost**: one frame's element list per output held until vblank; a
  client's previous buffer is released roughly one flip later than before,
  which is the timing every major compositor already exhibits.

The live proof is the user's next session: if hover flicker in Edge is the
release race, it stops with this default; disabling it
(`CHONKSTEP_STRICT_BUFFER_RELEASE=0`) should bring it back.

## The cursor-plane hypothesis (open; a lever ships)

The other credible mechanism is the DRM cursor plane. Hovering flips the
client cursor between arrow and hand; every cursor *content* change makes
smithay allocate a fresh GBM buffer, add a fresh framebuffer object, CPU-copy
the sprite, and swap the cursor plane's FB in the next atomic commit
(`try_assign_cursor_plane`, smithay `drm/compositor/mod.rs`). Whether that
churn glitches NVIDIA's display engine cannot be proven from a nested
session — no planes there — so the experiment ships as configuration:

- `CHONKSTEP_NO_CURSOR_PLANE=1` composites the pointer into every frame
  instead of using the cursor plane (`session::frame_flags`). If the flicker
  follows this switch, the cursor plane was the culprit; report that, and
  the default gets revisited. The cost while set is a recomposite per
  pointer motion — the exact trade `FRAME_FLAGS` documents.

Damage handling was audited for the related theory (a cursor change forcing
oversized damage): a cursor-image change marks the scene damaged, but the
damage *tracker* still computes per-element rects, and element identity is
stable across cursor shape changes (same surface, same id), so no full-frame
repaint results. Reasoned from `renderer.rs`/`xdg.rs::cursor_image`, not
measured.

## Clients that never bind syncobj (honest state)

For a dmabuf client that sets no acquire point, the compositor falls back to
polling the dmabuf fd for readability. On NVIDIA that poll returns instantly
— there is no implicit fence to wait for — so the fallback is a no-op and
those clients are still sampled on trust. **No sound compositor-side wait
exists**: the driver exports no fence for the client's work, and neither
`EGL_KHR_fence_sync` nor `GL_NV_timeline_semaphore` can conjure one from a
buffer handle after the fact; a GL fence inserted at import time would only
order against the compositor's own queue, not the client's. The honest
remedies are client-side:

- Chromium/Edge from the version that enables `WaylandLinuxDrmSyncobj` by
  default (current Edge 152 measured doing so) are fine as shipped; older
  builds can force it with
  `--enable-features=WaylandLinuxDrmSyncobj`.
- Any client still without explicit sync on NVIDIA may show write-tearing
  under load. That is a driver property, not something this compositor can
  paper over; the deadline blocker at least guarantees it never *hangs*
  such a client.

## The input stalls (audit findings and mitigation)

The 250–300ms libinput "event processing lagging" bursts were audited
against the render loop. Candidates, in order of plausibility:

1. **Synchronous GPU readbacks in the frame path** (`capture.rs`): window
   snapshots do `copy_framebuffer` + `map_texture` — a glReadPixels that waits
   for *all* prior GL work in the compositor's context. The readback itself is
   ≤256px, but the wait inherits whatever depth the GPU queue has, and a
   browser repainting under hover is precisely when that queue is deep. The
   compositor now marks a preview dirty from its toplevel or subsurface commit
   and recaptures only dirty windows, at most once per second. In a release A/B
   with one animated and eight static clients this cut readbacks from 9/s to
   1/s and compositor CPU by 44.8%. Overview entry still captures every mapped
   window at card resolution in a single frame, uncapped by design; that is a
   deliberate quality/latency tradeoff and the remaining readback hotspot.
2. **Shell widget samplers** that shell out synchronously — the documented
   2026-08-29 incident (`LOOP_BLOCK_GRACE`, `session.rs`) blocked the loop
   3.6s on `nmcli`. Same signature, out of this module's scope; check the
   dock's samplers if stalls persist with an idle GPU.
3. **Per-commit blocker bookkeeping** (`dmabuf.rs`): each dmabuf commit
   registers an eventfd source and a one-second timer. At a browser's
   ~60 commits/s that is ~120 insertions/s and ~60 live timers — real but
   microsecond-scale work, nowhere near 300ms.
4. **`element.sync.wait()`** in `render_frame_session` runs only when the
   render fence cannot travel with the commit; with
   `EGL_ANDROID_native_fence_sync` present it should not fire on this
   driver. If it did, it would block for the frame's full GPU time.

The recurring part of (1) is now removed. If stalls persist, Overview entry
and the synchronous portions of (2) remain the first places to instrument;
the capture path exposes trace-level per-readback telemetry (including target
dimensions) so this can be measured rather than guessed at.
