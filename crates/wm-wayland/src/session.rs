//! The hardware session: DRM/KMS, GBM, libinput, and libseat - what
//! makes `chonkstep-wayland` a desktop you log into from a TTY rather
//! than a window inside somebody else's.
//!
//! This module owns everything about running on real hardware and
//! nothing about how the desktop looks: it opens the seat, finds the
//! graphics device, drives mode setting and page flips, pumps input
//! devices, and hands VT switches back and forth with logind. The
//! pixels come from [`crate::renderer::build_scene`], the same
//! function the nested backend submits, which is why the two sessions
//! cannot drift apart visually.
//!
//! Scope, stated up front so the omissions read as decisions rather
//! than gaps:
//!
//! - **One GPU, one output.** [`init`] picks the primary DRM device and
//!   the first connected connector on it, and that is the session. A
//!   second monitor is dark. Multi-output is not hard here - it is a
//!   second `DrmCompositor` keyed by crtc plus real coordinates in
//!   `wm-core`'s `Backend::monitors` - but `wm-core` and `chonk-shell`
//!   still reason about a single screen rectangle (see
//!   `backend_impl.rs`'s `monitors`), so growing the compositor first
//!   would buy nothing.
//! - **No GPU hot-plug.** The udev source logs device add/remove and
//!   does not act on it. Adopting a GPU that appeared after startup
//!   means re-running every step of [`init`] against it while the old
//!   one is still scanning out; a laptop being docked is a session
//!   restart today.
//! - **No connector hot-plug.** Plugging a monitor in mid-session logs
//!   a udev `Changed` event and nothing else, for the same reason as
//!   multi-output above.
//! - **No hardware cursor plane.** The pointer is composited into the
//!   primary plane like every other element, exactly as the nested
//!   backend does it. A cursor plane would save a full-frame repaint
//!   per pointer motion, but only once the scene stops repainting
//!   fully anyway (see [`crate::renderer`]'s module docs).
//! - **No direct scan-out.** Every frame is composited through the
//!   GLES renderer and page-flipped from the swapchain, the same path
//!   the nested backend takes. See [`FRAME_FLAGS`].
//! - **No DRM leasing and no screencopy.** VR headsets and remote
//!   desktop are not part of this session.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc};
use smithay::backend::drm::compositor::{DrmCompositor, FrameError, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, PhysicalProperties};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::drm::control::{
    connector, crtc, Device as ControlDevice, Mode as DrmMode, ModeTypeFlags, ResourceHandles,
};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{DeviceFd, Transform};

use wm_theme_api::Size;

use crate::state::{Compositor, Graphics};

/// The concrete [`DrmCompositor`] this session drives. Spelled out
/// once because the four type parameters (allocator, framebuffer
/// exporter, per-frame user data, device fd) appear in every signature
/// that touches it. The user-data slot is `()`: it exists to carry
/// presentation feedback back from the page-flip event, and chonkstep
/// advertises no `wp_presentation` global for that feedback to reach.
type SessionDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// Framebuffer formats offered to the primary plane, in preference
/// order; [`DrmCompositor::new`] takes the first the hardware and the
/// renderer agree on. Both are 8-bit with alpha, which every KMS
/// driver worth the name scans out — 10-bit (`Abgr2101010`) buys
/// nothing here because every pixel in the scene originates from an
/// 8-bit-per-channel source (tiny-skia decoration buffers, shm client
/// surfaces), so the extra bits would carry no extra information.
const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Abgr8888, Fourcc::Argb8888];

/// Every frame is composited into the swapchain buffer and page-flipped
/// from there; no element is ever handed straight to a plane.
///
/// Direct scan-out would let a fullscreen client's own buffer become
/// the scanout buffer, skipping the GLES pass entirely — a real win for
/// video and games. It is off because it cannot pay yet: this backend
/// repaints the whole output every frame on purpose (see
/// [`render_frame_session`]), which is precisely the case where
/// composition costs the same either way, and turning it on adds a
/// second, hardware-dependent path through the frame that the nested
/// backend has no counterpart for — so a scan-out-only bug would be
/// invisible until someone logged in from a TTY. Enable it together
/// with per-element damage, and with an import node on the framebuffer
/// exporter (see [`init`]), not before.
const FRAME_FLAGS: FrameFlags = FrameFlags::empty();

/// Flags every DRM node is opened with: read-write mode setting, closed
/// across `exec` so a spawned terminal cannot inherit the GPU, detached
/// from any controlling terminal, and non-blocking because the fd
/// becomes a calloop source and a blocking read on it would park the
/// whole compositor — input, clients and all — until the next vblank.
///
/// Worth knowing before debugging an fd-flags problem here: libseat's
/// `Session::open` ignores this argument entirely and opens the device
/// with seatd's own flags. It is passed anyway because the `Session`
/// trait takes it and other implementations honor it, and because the
/// intent is what a reader needs.
const DEVICE_FLAGS: OFlags = OFlags::RDWR
    .union(OFlags::CLOEXEC)
    .union(OFlags::NOCTTY)
    .union(OFlags::NONBLOCK);

/// How long a page flip may stay in flight before the session says so.
///
/// The kernel promises a completion event for every atomic commit it
/// accepted, so exceeding this is a driver bug rather than a case to
/// recover from — and "recovery" would mean rendering into a buffer the
/// display engine still owns, which corrupts the screen instead of
/// fixing it. So this only produces a log line, but that line is the
/// difference between a frozen desktop that explains itself over SSH
/// and one that says nothing at all. Two seconds is far beyond any real
/// refresh interval, including a 24Hz cinema mode.
const FLIP_STALL_WARNING: Duration = Duration::from_secs(2);

/// Everything the session backend owns while it runs: the seat, the
/// DRM device and its GBM allocator, the EGL/GLES renderer, the one
/// output's DRM compositor, and the libinput context.
///
/// The event-source callbacks registered by [`init`] reach this struct
/// the same way [`render_frame_session`] does — by matching
/// `comp.graphics` against [`Graphics::Session`] — because calloop
/// hands every callback `&mut Compositor` and nothing else. That is
/// also the path [`change_vt`] takes on behalf of `input.rs`.
pub(crate) struct SessionGraphics {
    /// Seat handle for opening devices and for [`change_vt`]. Cloned
    /// from the notifier that calloop owns; if that notifier were ever
    /// dropped this handle's inner `Weak` would go dangling and every
    /// operation on it would start failing, which is why the notifier
    /// is inserted into the loop and never touched again.
    seat_session: LibSeatSession,
    /// The KMS device. Held for `pause`/`activate` across VT switches
    /// and for the `is_active` guard on the render path — the surface
    /// inside the DRM compositor keeps its own reference for
    /// commits.
    drm: DrmDevice,
    /// The GLES renderer every scene element is imported into and
    /// drawn with. `pub(crate)` because it is the one piece of this
    /// struct the rest of the crate legitimately needs: `capture.rs`
    /// renders the same scene offscreen through it, and `dmabuf.rs`
    /// asks it which hardware buffer formats to advertise. Also
    /// reachable as [`SessionGraphics::renderer`], which is the
    /// spelling backend-blind code uses so both arms of
    /// [`Graphics`] read the same.
    pub(crate) renderer: GlesRenderer,
    /// Swapchain, mode setting, and page flips for the one output.
    /// `pub(crate)` for the same capture reason.
    pub(crate) drm_compositor: SessionDrmCompositor,
    /// The libinput context, kept so the session notifier can suspend
    /// and resume it. The `LibinputInputBackend` calloop owns holds its
    /// own clone of the same underlying context.
    libinput: Libinput,
    /// The page flip in flight, if any — set on a successful
    /// `queue_frame`, cleared by the completion event that answers it.
    /// While it is `Some`, [`render_frame_session`] refuses to draw: the
    /// swapchain has at most a couple of slots, and rendering ahead of
    /// the display either exhausts them or throws away work nobody will
    /// ever see.
    frame_pending: Option<PendingFlip>,
}

/// A page flip the kernel has accepted and not yet reported back.
struct PendingFlip {
    queued_at: Instant,
    /// Whether this flip has already been named in the log as stalled,
    /// so [`FLIP_STALL_WARNING`] produces one line per stuck frame
    /// rather than one per event-loop wakeup.
    stall_reported: bool,
}

impl SessionGraphics {
    /// The renderer, named the way `WinitGraphicsBackend::renderer`
    /// is, so code that has to work on either backend can match the
    /// two arms of [`Graphics`] into one expression instead of
    /// branching on a method here and a field there.
    pub(crate) fn renderer(&mut self) -> &mut GlesRenderer {
        &mut self.renderer
    }
}

/// What [`init`] hands back to `run`: the graphics stack plus the
/// output it discovered, already configured with its mode. Any event
/// sources the session needs (DRM page flips, libinput devices, udev
/// hot-plug, seat activation) are registered on the loop by [`init`]
/// itself.
pub(crate) struct SessionInit {
    pub graphics: Graphics,
    pub output: Output,
    pub output_size: Size,
}

/// Opens the seat and the graphics device and prepares the session.
/// Errors here are fatal but explicable - no seat, no DRM device, no
/// usable connector - and the binary reports them instead of dying
/// silently on a black screen.
///
/// Nothing in here panics on hardware state. A TTY session has no
/// terminal left to print a backtrace to and no window manager to
/// survive it, so every fallible probe either produces a message that
/// names the device and the reason or is skipped in favor of the next
/// candidate.
pub(crate) fn init(
    loop_handle: &LoopHandle<'static, Compositor>,
    _display_handle: &DisplayHandle,
) -> Result<SessionInit, Box<dyn Error>> {
    // 1. The seat. libseat (or logind behind it) is what lets an
    //    unprivileged process open DRM and input devices at all, and
    //    what tells us when another session takes the VT away.
    let (mut seat_session, notifier) = LibSeatSession::new()
        .map_err(|error| format!("could not take a seat (is seatd or logind running?): {error:?}"))?;
    let seat_name = seat_session.seat();
    tracing::info!(seat = %seat_name, "session backend: seat acquired");

    // 2. The graphics device. Every candidate is opened *through the
    //    seat* so libseat owns the fd and can revoke it on a VT switch;
    //    opening `/dev/dri/cardN` directly would work as root and then
    //    strand the device on the first VT switch.
    let (device_path, device) = open_first_usable_device(&mut seat_session, &seat_name)?;
    let Device { mut drm, notifier: drm_notifier, gbm, connector, crtc, mode } = device;
    let connector_name = connector_name(&connector);
    tracing::info!(
        device = %device_path.display(),
        output = %connector_name,
        mode = %format!("{}x{}@{}", mode.size().0, mode.size().1, mode.vrefresh()),
        "session backend: driving one output"
    );

    // 3. The render stack. EGL is created on the GBM device rather than
    //    on the DRM node directly: GBM is what turns "a scanout buffer"
    //    into "something EGL can render into", and the same device then
    //    backs the allocator that fills the swapchain.
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }
        .map_err(|error| format!("EGL display init failed on {}: {error}", device_path.display()))?;
    let egl_context = EGLContext::new(&egl_display)
        .map_err(|error| format!("EGL context creation failed on {}: {error}", device_path.display()))?;
    // SAFETY: the context was just created on this thread, is not
    // current anywhere else, and `GlesRenderer` takes ownership of it —
    // the conditions its `new` documents.
    let renderer = unsafe { GlesRenderer::new(egl_context) }
        .map_err(|error| format!("GLES renderer init failed on {}: {error}", device_path.display()))?;
    // Which dmabuf formats the renderer can draw into; intersected
    // against the primary plane's formats by `DrmCompositor::new` to
    // choose the swapchain format. Collected eagerly so the borrow of
    // the renderer ends here.
    let render_formats: Vec<Format> =
        renderer.egl_context().dmabuf_render_formats().iter().copied().collect();

    // 4. The output, in wayland terms. No `Transform::Flipped180`: that
    //    correction belongs to the winit backend, whose EGL surface
    //    disagrees with the output about where the origin is (see
    //    `state.rs`'s `run`). A KMS scanout buffer's origin is the
    //    screen's, so any transform here would visibly flip the
    //    desktop.
    let wl_mode = OutputMode::from(mode);
    let (physical_w, physical_h) = connector.size().unwrap_or((0, 0));
    let output = Output::new(
        connector_name.clone(),
        PhysicalProperties {
            size: (physical_w as i32, physical_h as i32).into(),
            subpixel: connector.subpixel().into(),
            // The manufacturer and model live in the connector's EDID
            // blob, and parsing EDID means either a parser in this
            // crate or the `smithay-drm-extras` dependency — neither
            // worth it for two strings clients only ever show in an
            // about box. The connector name is at least true and
            // stable.
            make: "Unknown".into(),
            model: connector_name.clone(),
        },
    );
    output.set_preferred(wl_mode);
    output.change_current_state(Some(wl_mode), Some(Transform::Normal), None, Some((0, 0).into()));
    let output_size = Size::new(wl_mode.size.w.max(0) as u32, wl_mode.size.h.max(0) as u32);

    // The DRM surface binds crtc + connector + mode; the compositor
    // wraps it with the swapchain and the damage tracking that turn a
    // list of render elements into a page flip.
    let surface = drm
        .create_surface(crtc, mode, &[connector.handle()])
        .map_err(|error| format!("could not drive {connector_name} from {crtc:?}: {error}"))?;
    let allocator = GbmAllocator::new(
        gbm.clone(),
        // RENDERING: the buffer is an EGL render target. SCANOUT: it is
        // also handed to the display engine. Both are required — a
        // buffer allocated for only one of the two is rejected by the
        // other.
        GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT,
    );
    // `None` for the import node: that argument only enables direct
    // scan-out of *client* buffers, which cannot happen here (see
    // [`FRAME_FLAGS`]).
    let framebuffer_exporter = GbmFramebufferExporter::new(gbm, None);
    let drm_compositor = SessionDrmCompositor::new(
        &output,
        surface,
        // `None` planes: take whatever the surface reports. With
        // scan-out off, overlay and cursor planes are never assigned
        // anything anyway.
        None,
        allocator,
        framebuffer_exporter,
        COLOR_FORMATS.iter().copied(),
        render_formats,
        drm.cursor_size(),
        // No GBM device for the cursor plane: no hardware cursor (see
        // the module docs).
        None,
    )
    .map_err(|error| format!("could not set up scanout on {connector_name}: {error}"))?;

    // 5. Event sources. All four are registered before `run` builds the
    //    `Compositor`, which is safe because calloop only fires them
    //    from inside `event_loop.dispatch`, long after that struct
    //    exists.

    // Page-flip completions. The handler does not render: it clears the
    // in-flight flag and returns, and the loop's `dispatch_pending`
    // (which runs right after `dispatch` returns) draws the next frame
    // if the ledger is still dirty. Keeping the decision in one place
    // means the session and nested backends schedule redraws by exactly
    // the same rule — "damage, then draw".
    loop_handle
        .insert_source(drm_notifier, |event, _metadata, comp: &mut Compositor| {
            // `_metadata` carries the vblank timestamp and sequence,
            // which only matter for `wp_presentation` feedback we do
            // not advertise.
            let Graphics::Session(session) = &mut comp.graphics else {
                return;
            };
            match event {
                DrmEvent::VBlank(crtc) => {
                    if session.drm_compositor.crtc() != crtc {
                        tracing::debug!(?crtc, "page flip completed on a crtc we do not drive");
                        return;
                    }
                    session.frame_pending = None;
                    if let Err(error) = session.drm_compositor.frame_submitted() {
                        // The swapchain slot could not be recycled.
                        // Rendering continues; if this repeats the next
                        // frame will fail with `NoFreeSlotsError`, which
                        // is the loud version of the same problem.
                        tracing::warn!(?error, "page-flip completion was rejected by the DRM compositor");
                    }
                }
                DrmEvent::Error(error) => {
                    tracing::error!(?error, "the DRM device reported an error");
                }
            }
        })
        .map_err(|error| format!("failed to register the DRM event source: {error}"))?;

    // udev. Watched but not acted on: see the module docs on GPU and
    // connector hot-plug. Logging it is still worth the source, because
    // "the screen went black" and "the kernel took your GPU away" look
    // identical over SSH otherwise.
    let udev_backend = UdevBackend::new(&seat_name)
        .map_err(|error| format!("could not watch udev for seat {seat_name}: {error}"))?;
    loop_handle
        .insert_source(udev_backend, |event, _, _comp: &mut Compositor| match event {
            UdevEvent::Added { device_id, path } => {
                tracing::info!(?device_id, path = %path.display(), "a DRM device appeared; chonkstep drives a single GPU and will not adopt it");
            }
            UdevEvent::Changed { device_id } => {
                tracing::info!(?device_id, "a DRM device changed (connector hot-plug?); the session keeps its startup output");
            }
            UdevEvent::Removed { device_id } => {
                tracing::warn!(?device_id, "a DRM device went away; if it is ours the session will stop painting");
            }
        })
        .map_err(|error| format!("failed to register the udev event source: {error}"))?;

    // Input. The context opens devices through the same seat as the
    // GPU, so a VT switch revokes both together, and the events land in
    // `crate::input::process_input_event` — the identical routing the
    // nested backend feeds with winit events, because that function is
    // generic over the input backend.
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(
        seat_session.clone().into(),
    );
    libinput
        .udev_assign_seat(&seat_name)
        .map_err(|()| format!("libinput could not take seat {seat_name}; no keyboard or mouse would work"))?;
    loop_handle
        .insert_source(LibinputInputBackend::new(libinput.clone()), |event, _, comp: &mut Compositor| {
            crate::input::process_input_event(comp, event);
        })
        .map_err(|error| format!("failed to register the libinput event source: {error}"))?;

    // Seat activation. This is the half of VT switching we do not
    // initiate: another session (or a `chvt` from anywhere) takes the
    // seat, and we have to stop touching the hardware before the kernel
    // revokes it, then rebuild our idea of the crtc when it comes back.
    loop_handle
        .insert_source(notifier, |event, &mut (), comp: &mut Compositor| {
            let Compositor { graphics, wm, .. } = comp;
            let Graphics::Session(session) = graphics else {
                return;
            };
            match event {
                SessionEvent::PauseSession => {
                    tracing::info!("session paused: releasing the DRM device and input");
                    session.libinput.suspend();
                    session.drm.pause();
                    // A flip queued before the pause will never complete
                    // — the device is gone — so dropping it keeps the
                    // render path from waiting forever for a vblank that
                    // cannot arrive.
                    session.frame_pending = None;
                }
                SessionEvent::ActivateSession => {
                    tracing::info!("session resumed: reclaiming the DRM device and input");
                    // libinput reports failure as a bare `()`; there is
                    // nothing to log but the fact.
                    if session.libinput.resume().is_err() {
                        tracing::error!("could not resume libinput; keyboard and mouse are dead until restart");
                    }
                    // `true` = reset the device state (all connectors
                    // and planes disabled) instead of assuming the
                    // foreign session left our crtc/connector binding
                    // intact. It costs one modeset's worth of flicker
                    // and rules out the atomic-commit test failures that
                    // an optimistic resume produces when the other
                    // session rearranged things.
                    if let Err(error) = session.drm.activate(true) {
                        tracing::error!(?error, "could not reactivate the DRM device; the screen will stay dark");
                    }
                    if let Err(error) = session.drm_compositor.reset_state() {
                        tracing::error!(?error, "could not reset the crtc state after resuming");
                    }
                    // Buffer contents and buffer ages both survived the
                    // switch but mean nothing now: the foreign session
                    // painted over the screen.
                    session.drm_compositor.reset_buffers();
                    session.frame_pending = None;
                    wm.backend_mut().mark_damaged();
                }
            }
        })
        .map_err(|error| format!("failed to register the seat session source: {error}"))?;

    // 6. Hand the assembled stack back to `run`, which registers the
    //    output global and builds the damage tracker from it.
    Ok(SessionInit {
        graphics: Graphics::Session(Box::new(SessionGraphics {
            seat_session,
            drm,
            renderer,
            drm_compositor,
            libinput,
            frame_pending: None,
        })),
        output,
        output_size,
    })
}

/// Switches the seat to virtual terminal `vt`, reporting whether this
/// process is a hardware session at all.
///
/// `input.rs` calls this from inside the seat keyboard's filter, where
/// the only thing in reach is `&mut Compositor` — hence the lookup
/// through [`Graphics`] rather than a handle passed down. A `false`
/// return means the nested backend is running and the key combo should
/// be forwarded to clients like any other; `true` means it was consumed
/// (whether or not the switch itself succeeded — a session that swallows
/// Ctrl+Alt+F2 and then fails is confusing, but forwarding a VT combo to
/// a text editor is worse).
pub(crate) fn change_vt(comp: &mut Compositor, vt: i32) -> bool {
    let Graphics::Session(session) = &mut comp.graphics else {
        return false;
    };
    match session.seat_session.change_vt(vt) {
        Ok(()) => tracing::info!(vt, "switching virtual terminal"),
        Err(error) => tracing::warn!(?error, vt, "the seat refused the VT switch"),
    }
    true
}

/// Draws one frame onto the hardware: [`crate::renderer::build_scene`]
/// through the session's renderer, submitted as a page flip.
///
/// Deliberately shaped like `render_frame_winit`: bind, build the one
/// shared scene, submit, then send frame callbacks and clear the damage
/// flag *only on success*. Every early return leaves `damage` set, so a
/// transient failure costs one frame and retries on the next wakeup
/// instead of wedging the desktop.
pub(crate) fn render_frame_session(comp: &mut Compositor) {
    // Disjoint field borrows: the graphics stack mutates while the
    // ledger is read. Both live on `Compositor`, so destructure rather
    // than going through `&mut self` methods.
    let Compositor {
        wm,
        graphics,
        output,
        pointer_location,
        cursor_status,
        default_cursor,
        start_time,
        ..
    } = comp;
    let Graphics::Session(session) = graphics else {
        return;
    };

    if let Some(flip) = session.frame_pending.as_mut() {
        // A page flip is in flight. Rendering now would burn a
        // swapchain slot on a frame the display cannot show before the
        // one already queued, so leave `damage` set and let the vblank
        // handler's clearing of this flag be what schedules the redraw.
        let waited = flip.queued_at.elapsed();
        if !flip.stall_reported && waited > FLIP_STALL_WARNING {
            flip.stall_reported = true;
            tracing::error!(
                ?waited,
                "no page-flip completion from the DRM device; the desktop is frozen until one \
                 arrives (driver bug — switch VTs to get a console back)"
            );
        }
        return;
    }
    if !session.drm.is_active() {
        // Someone else owns the VT. Every commit would fail with
        // `DeviceInactive`; the resume handler marks damage again.
        return;
    }

    let SessionGraphics { renderer, drm_compositor, frame_pending, .. } = &mut **session;

    // Force full-frame damage, the session-side equivalent of the winit
    // path's `age = 0`: with every buffer age reset the internal damage
    // tracker treats the whole output as stale. Same trade the X11
    // session made by running picom with `--no-use-damage` — a little
    // GPU fill in exchange for never chasing a partial-damage artifact.
    // Revisit only with evidence, and revisit both backends together.
    drm_compositor.reset_buffer_ages();

    let (elements, clear_color) = crate::renderer::build_scene(
        wm.backend(),
        renderer,
        *pointer_location,
        cursor_status,
        default_cursor,
    );

    let rendered = match drm_compositor.render_frame(renderer, &elements, clear_color, FRAME_FLAGS) {
        Ok(result) => {
            // The GPU may still be drawing into the buffer we are about
            // to hand the scanout engine. Where the driver supports
            // fencing the DRM compositor passes the fence along with the
            // commit and this is skipped; where it does not, waiting on
            // the CPU is the only thing standing between us and a
            // half-drawn frame on screen.
            if result.needs_sync() {
                if let PrimaryPlaneElement::Swapchain(element) = &result.primary_element {
                    if let Err(error) = element.sync.wait() {
                        tracing::warn!(?error, "interrupted waiting on the render fence; the frame may tear");
                    }
                }
            }
            !result.is_empty
        }
        Err(error) => {
            // Includes the atomic test failures a returning VT switch
            // can produce; the seat-activation handler above resets the
            // device and marks damage, so the recovery path is there
            // rather than duplicated here.
            tracing::warn!(?error, "DRM render failed; keeping damage for a retry");
            return;
        }
    };

    if rendered {
        match drm_compositor.queue_frame(()) {
            Ok(()) => {
                *frame_pending = Some(PendingFlip { queued_at: Instant::now(), stall_reported: false })
            }
            // Nothing on the crtc actually changed. Not an error, and
            // not worth a retry: the scene is already on screen.
            Err(FrameError::EmptyFrame) => {
                tracing::trace!("frame produced no crtc changes; no page flip queued");
            }
            Err(error) => {
                tracing::warn!(?error, "queueing the page flip failed; keeping damage for a retry");
                return;
            }
        }
    }

    crate::renderer::send_frame_callbacks(wm.backend(), output, cursor_status, start_time.elapsed());
    wm.backend_mut().damage = false;
}

/// One opened, mode-set-capable DRM device together with the output it
/// will drive. Produced by [`probe_device`] and consumed by [`init`].
struct Device {
    drm: DrmDevice,
    notifier: DrmDeviceNotifier,
    gbm: GbmDevice<DrmDeviceFd>,
    connector: connector::Info,
    crtc: crtc::Handle,
    mode: DrmMode,
}

/// Walks the candidate DRM nodes in preference order and returns the
/// first that opens, does mode setting, and has something plugged into
/// it. Failures accumulate into one error message rather than the last
/// one winning: on a machine with an integrated and a discrete GPU,
/// "card0 has no connected output, card1 is not permitted" is the
/// diagnosis, and either half alone is misleading.
fn open_first_usable_device(
    seat_session: &mut LibSeatSession,
    seat_name: &str,
) -> Result<(PathBuf, Device), Box<dyn Error>> {
    let candidates = candidate_devices(seat_name);
    if candidates.is_empty() {
        return Err(format!(
            "no DRM device found for seat {seat_name}: is a graphics driver loaded, and does this \
             seat own a GPU?"
        )
        .into());
    }

    let mut failures = Vec::new();
    for path in candidates {
        match probe_device(seat_session, &path) {
            Ok(device) => return Ok((path, device)),
            Err(reason) => {
                tracing::info!(device = %path.display(), %reason, "skipping DRM device");
                failures.push(format!("{}: {reason}", path.display()));
            }
        }
    }
    Err(format!("no usable DRM device on seat {seat_name} ({})", failures.join("; ")).into())
}

/// Candidate DRM nodes, best first and deduplicated: an explicit
/// override, then udev's idea of the primary GPU for this seat, then
/// every other GPU on the seat, then a raw directory scan.
///
/// The directory scan is the fallback the udev queries cannot replace:
/// a device with no `ID_SEAT` property and no `boot_vga` PCI parent —
/// most virtual machines, some ARM SoCs where the display controller
/// is not on a PCI bus at all — is invisible to `primary_gpu` and
/// `all_gpus` but is still the only screen the machine has.
fn candidate_devices(seat_name: &str) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let push = |path: PathBuf, candidates: &mut Vec<PathBuf>| {
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    };

    // An explicit override, for bringing the session up on a machine
    // where the automatic choice picks the wrong card — the one thing a
    // developer debugging over SSH cannot work around otherwise.
    if let Some(path) = std::env::var_os("CHONKSTEP_DRM_DEVICE") {
        push(PathBuf::from(path), &mut candidates);
    }
    match primary_gpu(seat_name) {
        Ok(Some(path)) => push(path, &mut candidates),
        Ok(None) => tracing::debug!(seat = seat_name, "udev names no primary GPU for this seat"),
        Err(error) => tracing::warn!(?error, "could not ask udev for the primary GPU"),
    }
    match all_gpus(seat_name) {
        Ok(paths) => {
            for path in paths {
                push(path, &mut candidates);
            }
        }
        Err(error) => tracing::warn!(?error, "could not enumerate GPUs through udev"),
    }
    match std::fs::read_dir("/dev/dri") {
        Ok(entries) => {
            let mut cards: Vec<PathBuf> = entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .filter(|path| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("card"))
                })
                .collect();
            // `read_dir` order is whatever the filesystem hands back, so
            // sort for a stable choice across boots. Lexicographic, so
            // card10 lands before card2 — which does not matter, because
            // by the time this fallback runs any KMS-capable node will
            // do and only the determinism is worth having.
            cards.sort();
            for path in cards {
                push(path, &mut candidates);
            }
        }
        Err(error) => tracing::warn!(?error, "could not scan /dev/dri"),
    }
    candidates
}

/// Opens one DRM node through the seat and decides whether it can carry
/// this session: it must do mode setting and it must have a connected
/// connector with a mode and a crtc to drive it.
///
/// On failure the fd is dropped rather than handed back to libseat with
/// `close`. libseat forgets it when the session ends; the alternative is
/// threading a close through every early return of a function that runs
/// at most a handful of times before the process either has a screen or
/// exits.
fn probe_device(seat_session: &mut LibSeatSession, path: &Path) -> Result<Device, String> {
    let fd = seat_session
        .open(path, DEVICE_FLAGS)
        .map_err(|error| format!("the seat would not open it: {error:?}"))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));

    // `true`: start with every connector disabled. smithay enables the
    // ones it drives when a surface is attached, so anything we do not
    // use stays dark instead of showing whatever the previous session
    // left in its scanout buffer.
    let (drm, notifier) = DrmDevice::new(fd.clone(), true)
        .map_err(|error| format!("not a usable DRM device: {error}"))?;
    let (connector, crtc, mode) = pick_output(&drm)?;
    let gbm = GbmDevice::new(fd).map_err(|error| format!("GBM init failed: {error}"))?;

    Ok(Device { drm, notifier, gbm, connector, crtc, mode })
}

/// Picks the connector this session paints on: the first connected one
/// that has both a mode and a crtc available. "First" is the kernel's
/// enumeration order, which is stable across boots on a given machine —
/// good enough while the session is single-output, and the thing to
/// replace with a real policy (primary connector, saved layout) when it
/// is not.
fn pick_output(drm: &DrmDevice) -> Result<(connector::Info, crtc::Handle, DrmMode), String> {
    let resources = drm
        .resource_handles()
        .map_err(|error| format!("no KMS resources (a render-only node?): {error}"))?;

    // Why each connector was passed over, so a black screen is
    // diagnosable from one log line instead of a bisect.
    let mut skipped: Vec<String> = Vec::new();
    for handle in resources.connectors() {
        let info = match drm.get_connector(*handle, true) {
            Ok(info) => info,
            Err(error) => {
                skipped.push(format!("{handle:?} could not be read: {error}"));
                continue;
            }
        };
        let name = connector_name(&info);
        if info.state() != connector::State::Connected {
            skipped.push(format!("{name} is {:?}", info.state()));
            continue;
        }
        let Some(mode) = preferred_mode(&info) else {
            skipped.push(format!("{name} is connected but reports no modes"));
            continue;
        };
        let Some(crtc) = crtc_for(drm, &resources, &info) else {
            skipped.push(format!("{name} has no crtc able to drive it"));
            continue;
        };
        return Ok((info, crtc, mode));
    }

    Err(if skipped.is_empty() {
        "the device exposes no connectors at all".to_string()
    } else {
        format!("no connector is usable ({})", skipped.join(", "))
    })
}

/// The connector's own preferred mode — the panel's native resolution
/// on a laptop, the monitor's EDID preference on a desktop — falling
/// back to the first mode listed. Never guesses a resolution: a mode
/// the connector did not advertise is a mode the display will refuse.
fn preferred_mode(connector: &connector::Info) -> Option<DrmMode> {
    connector
        .modes()
        .iter()
        .find(|mode| mode.mode_type().contains(ModeTypeFlags::PREFERRED))
        .or_else(|| connector.modes().first())
        .copied()
}

/// Finds a crtc — the scanout engine — able to drive this connector.
/// The encoder already attached to it is tried first because reusing
/// the kernel's existing routing avoids disturbing a working
/// configuration; failing that, any crtc that any of the connector's
/// possible encoders can reach will do.
fn crtc_for(
    drm: &DrmDevice,
    resources: &ResourceHandles,
    connector: &connector::Info,
) -> Option<crtc::Handle> {
    connector
        .current_encoder()
        .and_then(|handle| drm.get_encoder(handle).ok())
        .and_then(|encoder| encoder.crtc())
        .or_else(|| {
            connector
                .encoders()
                .iter()
                .filter_map(|handle| drm.get_encoder(*handle).ok())
                .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
                .next()
        })
}

/// The name every other tool shows for this output — `eDP-1`, `HDMI-A-2`
/// — assembled the way the kernel and every other compositor assemble
/// it, so logs here line up with `drm_info` and `wayland-info` output.
fn connector_name(connector: &connector::Info) -> String {
    format!("{}-{}", connector.interface().as_str(), connector.interface_id())
}
