# Pinned dependency patches

`smithay-0.7.0/` is the published crates.io 0.7.0 source, including its original
MIT license (`LICENSE.txt`). It is excluded from this workspace's member list
and selected through `[patch.crates-io]`; no Git branch or unpinned dependency
is fetched at build time. This is not a general Smithay fork.

Provenance:

- Archive: <https://static.crates.io/crates/smithay/smithay-0.7.0.crate>
- SHA-256: `740cea6927892bc182d5bf70c8f79806c8bc9f68f2fb96e55a30be171b63af98`
  (matches the original `Cargo.lock` registry checksum).
- Upstream revision from `.cargo_vcs_info.json`:
  `a166cf4c94b5aedc332a65aa1dd753e8148829c3`.
- Imported mechanically from that verified archive. Intentional
  source changes are listed below. Preserve upstream attribution and compare
  against the archive when updating; do not reformat the dependency.

## Local patch: cancel every active touch, including framed touches

`src/input/touch/mod.rs`, `TouchInternal::cancel`:

- Do not apply frame de-duplication to cancellation. Once `frame()` flushed a
  held touch, 0.7 skipped its cancellation and retained the focused surface.
- Drain all slot state, deliver cancellation to active targets and any target
  awaiting its final frame, and discard pending frame state. The existing
  shared sequence number keeps Wayland cancellation de-duplicated per resource.
- Preserve all public APIs. There is no extra worker, timer, synthetic motion,
  lock or allocation on the compositor's input path.

Evidence: the real Wayland tests in
`crates/chonk-testkit/tests/pointer_coordinates.rs` exercise cancellation after
flushed frames, slot reuse, root/subsurface scaling and multiple held fingers.
Before this patch, all six touch cases reached correct coordinates but timed
out waiting for cancellation; see the campaign artifact `input-touch-coordinates.log`.

Upstream's later [touch API rework](https://github.com/Smithay/smithay/commit/b43494b0803dd53dfc1f9e64ea3804f313260c16)
changes frame-marker handling and trait signatures across multiple protocols.
Evaluate that upgrade separately with the complete compatibility suite. Remove
this vendored patch once an adopted upstream release passes these regressions.

Do not edit the global Cargo registry cache: that would make local tests pass
while fresh CI and user builds still receive the broken code.

## Local patch: generation-safe XWayland surface associations

`src/wayland/xwayland_shell.rs` and `src/xwayland/xwm/mod.rs`:

- Key pending Wayland surface associations by `(XwmId, serial)`, and use that
  generation-aware lookup in the X11 client-message handler. A restarted X
  server legitimately reuses serials that belonged to the previous server.
- Remove each entry on surface destruction in constant expected time, and
  reject dead resources on lookup. The compatibility serial-only accessor
  returns a surface only if there is one unambiguous live match.
- Process a committed association once rather than reinserting it on every
  surface frame. Reject zero serials and attempts to replace an already
  committed association, as specified by xwayland-shell-v1.

Evidence: `xwayland_loss_cancels_pending_pastes_and_restores_the_native_owner`
now checks actual key delivery before and after restarting private XWayland,
as well as clipboard and primary pastes without a second native copy. The
preserved `selection-completion` binary times out with no delivered key events;
`xwayland-serial` delivers both key edges and both exact payloads. A stricter
focus-liveness check had also exposed refusal of the first paste. Do not relax
that check to conceal a stale backing surface.

## Backport: selection devices are retired when their client disappears

Apply upstream Smithay commit
[`712d565b4b8bf34a136f42b39fcbf729ab635d3f`](https://github.com/Smithay/smithay/commit/712d565b4b8bf34a136f42b39fcbf729ab635d3f)
(Midge 't Hoen, “Clean up selection devices in `destroyed()` callback”) to
the core, primary, wlr-data-control and ext-data-control device handlers.
This preserves explicit release behavior and also cleans on connection loss.
The seat is obtained from resource user data, which remains available even
if its wl_seat object was released first. No timer, worker or per-frame sweep.

Diagnostic-only observation is exposed by `selection_device_statistics`:
it counts the actual retained devices, including dead ones, without pruning
them. ChonkStep exposes it only through its explicitly enabled private test
door. `selection_lifecycle.rs` covers normal exit, killed clients, legacy
core devices without a release request, explicit release while the client
stays connected, and releasing wl_seat before its dependent devices.

Keep the upstream attribution and exact before/after lifecycle evidence when
upgrading. Remove this backport once the adopted upstream release includes it.

## Local patch: compiler hygiene

Vendoring exposes dependency warnings previously hidden by Cargo's registry
lint cap. Remove six unused `tracing::error` imports (DRM GBM surface, libseat,
winit, keyboard input, Wayland keyboard and XWM). Make the existing SHM SIGBUS
handler's function-to-integer cast explicit through `*const ()`, as required
by current compiler guidance. These are mechanical, behavior-preserving
changes; they do not disable lints or change the signal handler.

## Local patch: legacy keyboard keymap termination

`src/input/keyboard/keymap_file.rs`, `KeymapFile::with_fd`:

The unsealed temporary-file path (keyboard versions below 7, or a failed sealed
file allocation) wrote only the Rust string bytes, unlike the sealed CString
path. Append the required NUL and include it in the advertised length. No API
change, extra retained allocation or steady-state event-loop work is involved.

Evidence: the real version-5 client in `keyboard_repeat.rs` reads the announced
file at offset zero and checks the final byte. The original ends in newline,
not NUL, violating the core `wl_keyboard.keymap_format.xkb_v1` contract. The
ordinary version-7 probe also checks the sealed path separately. Remove this
patch once an adopted upstream version passes both paths.

## Local patch: notify when keyboard focus becomes empty

`src/input/keyboard/mod.rs`, `KeyboardInnerHandle::set_focus`:

Notify `SeatHandler::focus_changed(..., None)` after a real focus removal,
matching the documented callback contract and the two existing nonempty-focus
paths. Repeated `None` and unchanged targets stay silent. Without this callback,
the compositor's selection-offer focus and shortcut inhibitor can
retain their old owner after the keyboard has left that client.

Evidence: `removing_keyboard_focus_notifies_dependent_focus_owners_once`
in `wm-wayland` observes `[true, true]` before versus the required
`[true, false, true]`. Text-input has separate enter/leave handling in Smithay's
Wayland keyboard target; do not attribute that lifecycle to this callback.
Remove this patch when an adopted upstream version passes both notification
and client-visible dependent focus-lifetime coverage.

## Local patches: X11 selection transfer and startup reconciliation

`src/xwayland/xwm/mod.rs`:

- Resolve a non-text MIME target with `InternAtom(only_if_exists=true)`.
  The old code read a property named TARGETS, but the actual target list arrived
  through `_WL_SELECTION` and was already consumed when advertising the offer.
  Real binary X11-to-Wayland pastes returned EOF with `UnableToDetermineAtom`.
- Do not prepend the INCR size-hint property to payload bytes.
- Read subsequent INCR chunks without deleting their properties immediately.
  Acknowledge each chunk only after its bytes reach the Wayland destination;
  the earlier double deletion allowed overwrites and premature completion.
- Flush property acknowledgements from the writable-FD callback. Unlike the
  X11 event dispatcher, this calloop callback has no outer X connection flush;
  an unflushed acknowledgement leaves an otherwise quiet transfer stalled.
- Adopt incoming x11rb property buffers instead of copying them, and advance
  a read offset on partial pipe writes instead of allocating/copying the unread
  tail on each dispatch. Release a drained payload immediately. Interrupted and
  would-block writes yield for readiness without dropping bytes; zero writes
  terminate instead of busy-looping. `xwm/incoming_buffer.rs` is the exact
  display-independent helper tested by the workspace's `selection_buffer.rs`;
  no test-only alternate implementation or new public Smithay API is involved.
- Bound Wayland-to-X11 staging to one 64 KiB INCR chunk. Disable the readable
  source while waiting for the consumer's property deletion and re-enable it
  after acknowledgement; keep its token so cancellation can still retire it.
  Reuse the staging allocation and treat interrupted/would-block reads as
  transient, not transfer failures. A stalled consumer must not cause the
  compositor to drain and retain an entire arbitrary-size clipboard payload.
- Retire non-INCR completed buffers immediately, without waiting for the
  requestor window to die. Keep an INCR transfer after source EOF only while
  its final property acknowledgement is still outstanding.
- Subscribe to property/destruction events on the actual requestor, including
  unmapped child windows, preserving the XWM's existing event mask. Destruction
  cleans both clipboard and primary transfers, not only the first matching kind.
- Retain and observe each incoming INCR producer's exact window too. Producer
  death closes the native pipe even while its writable source is disabled
  waiting for another property; selection replacement does not change that
  transfer's source identity. Never promise a complete payload after its owner
  exits before sending it.
- Remove every pending transfer's event-loop source before invoking the XWM
  disconnect callback. Otherwise disabled sources and their pipes survive the
  old server and can refer to absent or replacement XWM state.
- Debug output describes buffered byte counts, never clipboard contents.
  Previously an abandoned-transfer warning formatted the entire payload.

`src/wayland/selection/mod.rs` adds the read-only `current_client_selection`
accessor. It returns only a live client source from the seat's authoritative
state, excluding compositor-provided and destroyed sources. ChonkStep uses it
once when an asynchronous XWM starts, including a restart: native selections
copied before XWayland readiness must not disappear at the protocol boundary.
There is no parallel clipboard cache or eager payload copying.

Evidence lives in `crates/chonk-testkit/tests/selection_transfer.rs`. Native
tests cover UTF-8/binary/large clipboard and primary payloads, owner replacement
and independent clearing. The X11 fixture exercises both directions and real
INCR acknowledgements; a gated real XWayland start covers early live, cleared
and exited owners. Preserve campaign before/after logs when replacing these
patches. Remove them only when an adopted upstream release passes the same
payload and lifetime matrix; clipboard support alone does not prove XDND or
clipboard-history/persistence support.

## Local patch: translated modifiers reach input-method keyboard grabs

`src/input/keyboard/mod.rs` and
`src/wayland/input_method/input_method_keyboard_grab.rs`:

- Add `input_forward_with_modifiers` to forward a translated shortcut through
  the existing grab and pressed-key bookkeeping with an explicit modifier mask,
  while preserving the physical XKB state. The original forwarding API retains
  its behavior. Projecting only at the final keyboard focus misses IME grabs.
- Deliver projected modifiers before the associated key to the input method,
  and restore the physical mask before the first untranslated key. Ordinary
  physical input keeps its existing key/modifier ordering. Fcitx reinjects keys
  asynchronously, so a temporary compositor focus override cannot fix them.

Evidence: the unit regression in `wm-wayland/src/input.rs` checks grab-visible
modifiers, balanced key bookkeeping and unchanged physical state. The real
Fcitx/GTK test in `chonk-testkit/tests/mac_mode.rs` requires an actual Wayland
input-method grab and checks Command copy/paste between applications, clipboard
retention after the owner quits, untranslated chords and release ordering.
The previously installed i9beef binary fails at Command-A; the patched binary
selects and transfers the text. Remove this patch when an adopted upstream
version provides equivalent projection through grabs and passes these tests.

## Release 0.5.0: input-method lifetime and mode-toggle clipboard adoption

- Give each input-method keyboard grab its installation serial. Destroying a
  superseded protocol object cannot clear the replacement or unset another
  compositor grab. A normal input after a projected chord restores physical
  modifiers even when XKB reports no modifier transition.
- `retire_forwarded_key` balances an already-processed translated release after
  its original focus leaves, without giving an IME a stale release to reinject
  into the newly focused client. Physical XKB processing remains unchanged.
- `current_selection_mime_types` provides a read-only snapshot of the current
  selection's advertised formats, including compositor bridge offers. It lets
  persistence adopt an existing X11 clipboard on enable without rereading every
  frame or replacing an already-published memory snapshot.

Real protocol regressions in `keyboard_focus.rs` reproduce the first two faults.
`selection_transfer.rs` proves adoption for native and X11 owners and cycles
Mac mode three times. Both ownership regressions fail on the installed pre-fix
build. The release review records before/after evidence.

## Local patch: retained rounded frame effects

Three read-only/storage accessors support rounded live client textures without
an offscreen copy: inspect the element inside `RescaleRenderElement`, recover a
texture shader override's reusable uniform vector, and map framebuffer fragment
coordinates back to physical scene pixels. Existing rendering methods and their
defaults are unchanged. Only bounded corner squares use the custom mask shader;
the client interior keeps the existing texture path.

`cargo test -p wm-wayland native_gles_rounded_texture_crop_relocation_and_shadow_pixels -- --ignored`
is the explicit surfaceless GLES gate. It checks every pixel across all eight
output transforms, nonuniform scaling, cropping and relocation, as well as the
zero-blur shadow. Remove these accessors when an adopted Smithay version provides
equivalent allocation-free retained shader overrides and passes that gate.

`GlesFrame::with_texture_read` groups the ordinary body and masked corner draws
under one texture read lock. It waits for upload before entering the callback
and publishes one final read fence before unlocking, including after errors or
unwinding. Same-texture nesting reuses the lock; another texture is rejected
before acquiring a second lock, preventing lock inversion. Ordinary draws
outside the scope retain their existing synchronization. The shared-context
fallback without fence support still completes the submitted draws before
unlocking. No per-frame allocation or client-buffer ownership change is added.

The ignored native regression in `gles/read_batch_tests.rs` covers held writer
locks, nested scopes, conflicting textures, error and unwind cleanup, and exact
pixels with and without fence support. Because this dependency is excluded from
the workspace, run its library test through an isolated copy of its manifest
with `--no-default-features --features renderer_gl`; the Chonkstep native gate
above also exercises actual mutable client textures and warm allocation bounds.

`GlesRenderer::compile_custom_pixel_shader_with_vertex` adds a vertex stage to
the existing pixel-shader constructor. It retains the same damage-instance and
projection interface and checks required active input types before publishing
normal and debug programs. The original constructor uses its unchanged default
vertex stage. Immutable shadow atlases use this to compute affine nine-slice UVs
per vertex, batching tile-clipped damage without moving that work into every
fragment. Native interface tests cover malformed inputs and default/custom
pixel parity; Chonkstep's strict reference gate additionally checks all output
transforms, fractional scaling, cropping and collapsed middle slices.

## Local patch: bounded inspection of retained surface opacity

`src/backend/renderer/element/surface.rs` exposes `opaque_region_count()`
without transforming or copying the retained rectangles. ChonkStep checks for
exactly one region before using existing scaled opacity APIs to prove a modern
client fully covers its resize fill. Complex declarations keep the fill and do
not allocate new opacity copies. This avoids applying rounded coverage twice
behind opaque clients while preserving transparent clients and resize gaps.

## Local patch: a toplevel description is stored as the description

`src/wayland/xdg_toplevel_tag.rs`, the `SetToplevelDescription` arm:

Write the description into `XdgToplevelTagSurfaceData::description`.
Upstream 0.7.0 writes it into `tag`, so a client that sets both loses
its tag in that copy and `description()` never answers. The
`XdgToplevelTagHandler` callbacks were already passed the right
strings; only the surface-side copy was crossed. No API change.

ChonkStep keeps its own bounded copies from the handler arguments and
reads neither field, so the patch protects only a future reader of the
surface data. Evidence: `crates/chonk-testkit/tests/hyprland_ipc.rs`
tags a real toplevel and then describes it, and checks that
`hyprctl clients` reports both. Remove this patch once an adopted
upstream release stores the description in its own field.

## Local patch: link-status retrain and forced modeset on request

`src/backend/drm/surface/atomic.rs`, `src/backend/drm/surface/mod.rs` and
`src/backend/drm/compositor/mod.rs`:

- `AtomicDrmSurface::request_link_retrain` sets a flag that makes
  `commit_pending` report true until the device accepts a commit. The next
  submission therefore takes the `commit` path (`ALLOW_MODESET`) even when the
  pending and current crtc states are equal, and that request — and the
  `ALLOW_MODESET` test that precedes it — carries `link-status = GOOD` (raw 0)
  for every pending connector that exposes the property. Connectors without it
  are committed unchanged, so on such drivers the request degrades to a plain
  forced modeset. Non-modeset tests (`test_state` with `allow_modeset = false`,
  which is what `render_frame` runs) never carry the property, because the
  kernel treats a `link-status` change as a connector change and would reject
  the test without `ALLOW_MODESET`. The flag clears on the first accepted commit.
- `DrmSurface::request_link_retrain` forwards to the atomic surface and returns
  `false` on a legacy surface, which has no property commits.
- `DrmCompositor::request_link_retrain` forwards to the surface and, when
  honoured, sets the existing `reset_pending` so the next `render_frame` is a
  full, non-empty frame and an idle scene still produces the submission.
- No existing public API changes shape or behaviour: without the request the
  `commit_pending` comparison, the request builders and the commit flags are
  exactly upstream's.

Why: the kernel's KMS documentation for `link-status` says a DisplayPort link
that fails to train leaves the sink with no pixels until userspace performs a
modeset that writes the property back to `GOOD`; the kernel does that itself
only for legacy `SETCRTC` callers. Smithay 0.7 writes only `CRTC_ID` on
connectors and takes the commit path only when its own pending state differs
from the current one, so an atomic compositor on it had no way to retrain a
link, and no way to force a modeset for a crtc whose commits the driver keeps
refusing with the state it already holds. `wm-wayland`'s session backend uses
this seam from its debounced connector rescan (`link-status` read as `BAD`)
and as the second rung of its per-output commit-failure escalation.

Evidence: `wm-wayland`'s unit tests cover the pure `link-status` match and
the escalation schedule that reaches this seam; the seam itself needs a real
KMS device and is exercised by the hardware check recorded on the issue that
introduced it (docked suspend/resume on DisplayPort). Remove this patch once an
adopted upstream release exposes an equivalent connector-property seam or a
forced-modeset request on `DrmCompositor`.
