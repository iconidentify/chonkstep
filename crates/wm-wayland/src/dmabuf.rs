//! Hardware buffers: the `zwp_linux_dmabuf_v1` global, which is how a
//! GPU-accelerated client (Chromium, Firefox, mpv, anything drawing
//! through GBM/EGL) hands us a *handle* to video memory instead of a
//! CPU copy of it. Without this global those clients fall back to
//! `wl_shm` — a full-frame readback and re-upload every frame, which
//! is exactly what makes a compositor feel slow under video — and a
//! few refuse outright, because their EGL platform code reads "no
//! linux-dmabuf global" as "this compositor has no GPU".
//!
//! # Integration contract
//!
//! One call and one field, both in `state.rs`. Nothing else in the
//! crate needs to know this module exists — the protocol side is
//! entirely `delegate_dmabuf!` plus the [`DmabufHandler`] impl below,
//! and the *render* side already works: `GlesRenderer` implements
//! `ImportAll`, so a dmabuf-backed `wl_buffer` is imported by the
//! ordinary `WaylandSurfaceRenderElement` path in `renderer.rs`
//! without a line of special-casing.
//!
//! 1. In `run`, make the graphics binding mutable
//!    (`let (mut graphics, output, output_size) = ...`) and add one
//!    line directly after that `if nested { ... } else { ... }` block —
//!    it must land *before* the `ListeningSocketSource` is inserted,
//!    since a global that does not exist when a client binds might as
//!    well not exist at all:
//!
//!    ```ignore
//!    let dmabuf = crate::dmabuf::init_for_graphics(&display_handle, &mut graphics);
//!    ```
//!
//! 2. `Compositor` gains one field, moved in from that local:
//!
//!    ```ignore
//!    /// linux-dmabuf: the format set we advertise and the protocol
//!    /// state behind it. Always present; "this renderer cannot do
//!    /// dmabuf" is represented inside, not by an `Option`.
//!    pub(crate) dmabuf: crate::dmabuf::DmabufSupport,
//!    ```
//!
//! [`init_for_graphics`] never fails: a renderer with no dmabuf
//! support yields a [`DmabufSupport`] that registered no global, so
//! [`DmabufHandler::dmabuf_state`] can hand back a real `&mut` with no
//! `expect` in the path. That is why the field is not an `Option` —
//! an unreachable panic in a login session's protocol dispatch is a
//! black screen with no console to read it on.
//!
//! # Where the format list comes from
//!
//! `renderer.dmabuf_formats()` — which for `GlesRenderer` is the EGL
//! display's `dmabuf_texture_formats`, i.e. the (fourcc, modifier)
//! pairs the driver will actually turn into an `EGLImage`. Advertising
//! anything else would be a lie the client discovers at the worst
//! moment: modern clients allocate against the advertised set and use
//! `create_immed`, and a compositor that then rejects the buffer kills
//! them (see [`ImportNotifier::failed`]'s two behaviours, and the
//! comment on [`DmabufHandler::dmabuf_imported`] below).
//!
//! Both backends reach this the same way, because both own a
//! `GlesRenderer` — the winit backend's behind `renderer()`, the DRM
//! session's in `SessionGraphics::renderer`. So one code path serves
//! both, and a hardware session advertises whatever its actual GPU can
//! import rather than a curated guess.
//!
//! # Feedback (protocol version 4)
//!
//! Advertised whenever the render node can be identified, which on a
//! single-GPU machine it always can: one main device, one main
//! tranche, no preference tranches. Preference tranches exist to steer
//! clients toward buffers a *plane* can scan out directly, and the
//! session composites every frame through the GLES renderer instead
//! (`session.rs`'s `FRAME_FLAGS` is empty) — so there is no scanout
//! tranche to declare, and one advertising formats the main device
//! cannot render would be actively wrong. That ordering is deliberate:
//! this global is the prerequisite for direct scan-out, so scan-out is
//! now a `session.rs` question, and a scanout tranche belongs in the
//! same change that answers it. Multi-GPU is feedback's other reason
//! to exist, and it waits on multi-GPU support generally.
//!
//! When the render node cannot be identified, or the feedback's sealed
//! format-table memfd cannot be created, the global drops to version 3
//! (a flat format list, no feedback). Clients handle that fine; it is
//! what every compositor advertised before 2022.

use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::Buffer;
use smithay::backend::egl::EGLDevice;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::ImportDma;
use smithay::delegate_dmabuf;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::wayland::dmabuf::{
    DmabufFeedback, DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};

use crate::state::{Compositor, Graphics};

/// The linux-dmabuf protocol state plus the format set it was built
/// from. The `DmabufGlobal` handle the constructor gets back is
/// deliberately dropped: it is a `Copy` id (so it provably has no
/// `Drop` behaviour), and its only uses are withdrawing the global or
/// swapping the default feedback — neither of which a session does
/// while it is alive. What must survive is `state`, since every
/// `zwp_linux_dmabuf_v1` request dispatches through it.
pub(crate) struct DmabufSupport {
    state: DmabufState,
    /// Exactly what was advertised to clients, kept for the import
    /// check in [`DmabufHandler::dmabuf_imported`]. Empty means no
    /// global was registered and no client can ever reach that check.
    formats: FormatSet,
}

impl DmabufSupport {
    /// The no-hardware-buffers state: protocol machinery allocated (so
    /// the handler always has something to return) but no global
    /// registered, so no client ever sees the protocol.
    fn disabled() -> Self {
        DmabufSupport { state: DmabufState::new(), formats: FormatSet::default() }
    }
}

/// The one call `run` makes — see the module docs. Picks the renderer
/// out of whichever graphics stack this session ended up with and
/// defers to [`init`].
pub(crate) fn init_for_graphics(
    display_handle: &DisplayHandle,
    graphics: &mut Graphics,
) -> DmabufSupport {
    init(display_handle, graphics_renderer(graphics))
}

/// Registers the linux-dmabuf global for `renderer`'s formats.
///
/// Split out from [`init_for_graphics`] so the renderer, not the
/// graphics enum, is the dependency: `session::init` builds its
/// renderer before any `Graphics` exists, and can call this directly
/// if it ever needs the global up earlier than `run` puts it up.
///
/// Never fails: a renderer that cannot import dmabufs at all gets a
/// state with no global rather than an error, because "this GPU stack
/// is software-only" is a degraded session, not a broken one — the
/// same call every other optional piece of this compositor makes
/// (XWayland missing, a theme that will not load).
pub(crate) fn init(display_handle: &DisplayHandle, renderer: &mut GlesRenderer) -> DmabufSupport {
    let formats = renderer.dmabuf_formats();
    if formats.indexset().is_empty() {
        // llvmpipe without the dmabuf import extensions, or an EGL
        // display that came up without `EGL_EXT_image_dma_buf_import`.
        // Advertising an empty global is worse than advertising none:
        // clients that see it negotiate, find nothing, and have to
        // discover the dead end themselves.
        tracing::warn!("the renderer reports no dmabuf formats; not advertising linux-dmabuf");
        return DmabufSupport::disabled();
    }

    let mut state = DmabufState::new();
    match default_feedback(renderer, &formats) {
        Some(feedback) => {
            let _global =
                state.create_global_with_default_feedback::<Compositor>(display_handle, &feedback);
            tracing::info!(formats = formats.indexset().len(), "linux-dmabuf v4 advertised");
        }
        None => {
            let _global = state.create_global::<Compositor>(display_handle, formats.clone());
            tracing::info!(formats = formats.indexset().len(), "linux-dmabuf v3 advertised");
        }
    }
    DmabufSupport { state, formats }
}

/// Builds the version-4 default feedback, or `None` to fall back to a
/// version-3 format list.
///
/// Feedback is keyed by the render node's `dev_t`, so it needs the DRM
/// node behind the EGL display. Two things can deny us that and
/// neither is fatal: an EGL implementation without the
/// `EGL_EXT_device_drm*` extensions (software rendering, some nested
/// setups), and a `build()` that cannot create the sealed memfd the
/// format table is sent over.
fn default_feedback(renderer: &GlesRenderer, formats: &FormatSet) -> Option<DmabufFeedback> {
    let node = match EGLDevice::device_for_display(renderer.egl_context().display())
        .and_then(|device| device.try_get_render_node())
    {
        Ok(Some(node)) => node,
        Ok(None) => {
            tracing::warn!("EGL reports no DRM render node; linux-dmabuf drops to v3");
            return None;
        }
        Err(error) => {
            tracing::warn!(?error, "could not query the EGL device; linux-dmabuf drops to v3");
            return None;
        }
    };

    // One tranche, the main one — the single-GPU, always-composite
    // case argued in the module docs.
    match DmabufFeedbackBuilder::new(node.dev_id(), formats.clone()).build() {
        Ok(feedback) => Some(feedback),
        Err(error) => {
            tracing::warn!(?error, "could not build dmabuf feedback; linux-dmabuf drops to v3");
            None
        }
    }
}

/// The session's `GlesRenderer`, whichever graphics stack is running —
/// there is always exactly one, which is the whole reason a single
/// dmabuf implementation covers both backends.
///
/// `capture.rs` carries the same three lines for the same reason;
/// neither module owns the other's file, and a shared helper belongs
/// on `Graphics` itself (in `state.rs`) if anyone ever consolidates.
fn graphics_renderer(graphics: &mut Graphics) -> &mut GlesRenderer {
    match graphics {
        Graphics::Winit(backend) => backend.renderer(),
        Graphics::Session(session) => &mut session.renderer,
    }
}

impl DmabufHandler for Compositor {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf.state
    }

    /// Smithay's contract: decide whether this buffer can become a
    /// texture, and say so through `notifier` exactly once.
    ///
    /// The asymmetry that shapes this is [`ImportNotifier::failed`]:
    /// for a buffer created with `create` it sends a `failed` event
    /// the client recovers from, but for `create_immed` — what
    /// Mesa/GBM clients use in the common path — it posts a protocol
    /// error and *kills the client*. So a false rejection is far more
    /// expensive than a false acceptance, whose worst case is one
    /// surface that renders empty because `ImportAll` declines it
    /// later in `renderer.rs`.
    ///
    /// Hence: reject up front only what we never advertised (a client
    /// that invented a format/modifier pair is already outside the
    /// negotiation, and gets a log line naming it rather than an
    /// opaque EGL error), then let the renderer's own import be the
    /// judge. That import is not just a test — `GlesRenderer` caches
    /// the resulting texture against the dmabuf, so the first frame
    /// that uses this buffer costs nothing extra.
    ///
    /// The pre-check is safe against legacy clients because smithay's
    /// EGL format query inserts a `Modifier::Invalid` entry for every
    /// fourcc it finds, so implicit-modifier (version 3) buffers are
    /// always inside the advertised set.
    fn dmabuf_imported(&mut self, _global: &DmabufGlobal, dmabuf: Dmabuf, notifier: ImportNotifier) {
        if !self.dmabuf.formats.contains(&dmabuf.format()) {
            tracing::debug!(format = ?dmabuf.format(), "rejecting a dmabuf in an unadvertised format");
            notifier.failed();
            return;
        }

        match graphics_renderer(&mut self.graphics).import_dmabuf(&dmabuf, None) {
            Ok(_texture) => {
                let _ = notifier.successful::<Compositor>();
            }
            Err(error) => {
                tracing::debug!(?error, format = ?dmabuf.format(), "renderer refused a dmabuf import");
                notifier.failed();
            }
        }
    }

    // `new_surface_feedback` is deliberately left at its default
    // (`None` — use the global's default feedback). Per-surface
    // feedback earns its keep by telling a client "allocate this one
    // differently and a plane can scan it out", and there are no
    // planes in play here; a single-GPU compositing session has
    // exactly one right answer for every surface.
}

delegate_dmabuf!(Compositor);
