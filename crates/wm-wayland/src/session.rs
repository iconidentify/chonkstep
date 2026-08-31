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
//! - **One GPU, every connected connector on it.** [`init`] picks the
//!   primary DRM device and drives every connector on it that is
//!   plugged in, each with its own crtc, its own `DrmCompositor`, and
//!   its own page-flip bookkeeping (the kernel reports flips per crtc,
//!   so nothing about frame scheduling can be shared between outputs).
//!   A second GPU's outputs are dark.
//! - **No output layout policy.** Outputs are placed left to right in
//!   connector-enumeration order at their mode sizes, with no gaps and
//!   no overlap. That is a guess, not a configuration: nothing here
//!   applies a client-requested layout (`xdg-output` only *reports*
//!   one) and nothing reads a saved one, so nothing in the session can
//!   tell us that the laptop panel is *below* the desktop monitor, or
//!   that the two should mirror. Mirroring,
//!   arbitrary positioning, per-output scale and rotation are all
//!   future work, and all of them are changes to how [`init`] assigns
//!   `position` — not to the render path, which already draws each
//!   output through a viewport offset (see
//!   [`crate::renderer::build_scene`]).
//! - **No per-surface output tracking.** Nothing sends
//!   `wl_surface.enter`/`leave`, so a client is never told which screen
//!   it is on. That was invisible while there was one screen and one
//!   possible answer; with several, a client that scales itself per
//!   output (or wants that output's refresh) gets no signal and falls
//!   back to its default. The same bookkeeping would give frame
//!   callbacks a per-output cadence instead of the primary's (see
//!   [`render_frame_session`]) and is the prerequisite for
//!   `wp_presentation` feedback, so all three arrive together or not
//!   at all.
//! - **No GPU hot-plug.** The udev source logs device add/remove and
//!   does not act on it. Adopting a GPU that appeared after startup
//!   means re-running every step of [`init`] against it while the old
//!   one is still scanning out; a laptop being docked is a session
//!   restart today.
//! - **No connector hot-plug.** Connectors are enumerated once, at
//!   startup. Plugging a monitor in mid-session logs a udev `Changed`
//!   event and nothing else: adopting it means minting an `Output` and
//!   a `wl_output` global after clients have already bound the ones
//!   they know about, re-laying out every existing output, and telling
//!   `wm-core` its screen just changed shape — a session restart picks
//!   the new monitor up today.
//! - The pointer *is* on the hardware cursor plane (see
//!   [`FRAME_FLAGS`]); the nested backend composites it instead,
//!   because a window on someone else's desktop has no planes to ask
//!   for.
//! - **No direct scan-out.** Every frame is composited through the
//!   GLES renderer and page-flipped from the swapchain, the same path
//!   the nested backend takes. See [`FRAME_FLAGS`].
//! - **No DRM leasing.** A crtc is never handed to another process,
//!   so a VR headset cannot take one over.

use std::error::Error;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::backend::drm::compositor::{DrmCompositor, FrameError, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, PlaneInfo};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, OutputModeSource, PhysicalProperties};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::drm::control::{
    connector, crtc, plane, Device as ControlDevice, Mode as DrmMode, ModeTypeFlags, PlaneType,
    ResourceHandles,
};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{DeviceFd, Transform};

use wm_theme_api::{Point, Size};

use crate::state::{Compositor, Graphics, OutputSetup};

/// The concrete [`DrmCompositor`] this session drives. Spelled out
/// once because the four type parameters (allocator, framebuffer
/// exporter, per-frame user data, device fd) appear in every signature
/// that touches it. The user-data slot is `()`: it exists to carry
/// presentation feedback back from the page-flip event, and chonkstep
/// advertises no `wp_presentation` global for that feedback to reach.
type SessionDrmCompositor =
    DrmCompositor<GbmAllocator<DrmDeviceFd>, GbmFramebufferExporter<DrmDeviceFd>, (), DrmDeviceFd>;

/// Framebuffer formats offered to the planes, in preference order;
/// [`DrmCompositor::new`] takes the first the hardware and the renderer
/// agree on. Both are 8-bit with alpha, which every KMS driver worth
/// the name scans out - 10-bit (`Abgr2101010`) buys nothing here
/// because every pixel in the scene originates from an 8-bit-per-
/// channel source (tiny-skia decoration buffers, shm client surfaces),
/// so the extra bits would carry no extra information.
///
/// `Argb8888` leads deliberately. It is the format KMS hardware is
/// most likely to accept, and on hardware that accepts only one it is
/// invariably this one - virtio-gpu's cursor plane, for instance,
/// advertises `AR24` and nothing else, so an `Abgr8888`-first list
/// cost the *hardware cursor* (confirmed live: the cursor plane sat
/// unused with `fb=0` until this order changed, and Smithay logged
/// `Preferred format AB24 not available`). Channel order is invisible
/// to everything above this line - the renderer swizzles either way -
/// so leading with the widely-supported one is free.
const COLOR_FORMATS: &[Fourcc] = &[Fourcc::Argb8888, Fourcc::Abgr8888];

/// The cursor and overlay planes Smithay's own discovery dropped.
///
/// Smithay gates both lists behind whether it could set the
/// universal-planes client capability on the device fd, and empties
/// them when that call reports failure (`backend/drm/mod.rs`'s
/// `planes`). On this project's virtio-gpu test machine that gate
/// misfires: the capability sets cleanly on the very same fd, the
/// kernel then lists both the primary and the cursor plane to this
/// process, and `modetest` confirms plane 36 is `type=Cursor` - yet
/// Smithay records the attempt as failed and the cursor plane
/// disappears, taking the hardware cursor with it (the symptom is a
/// pointer that lags the hand, and a trace line reading "no free plane
/// found").
///
/// `DrmCompositor::new` takes an explicit `Planes` precisely so a
/// compositor can make this call itself, so this fills in what is
/// missing rather than replacing what works: Smithay's discovery is
/// kept whole (including the richer format sets it reads from
/// `IN_FORMATS` on hardware that has them) and only an *empty* cursor
/// list is repopulated. On a machine where the gate behaves, this
/// function never changes anything.
fn rediscover_cursor_planes(drm: &DrmDeviceFd, crtc: crtc::Handle) -> Vec<PlaneInfo> {
    let Ok(resources) = drm.resource_handles() else {
        return Vec::new();
    };
    let Ok(handles) = drm.plane_handles() else {
        return Vec::new();
    };
    let mut cursor = Vec::new();
    for handle in handles {
        let Ok(info) = drm.get_plane(handle) else { continue };
        if !resources.filter_crtcs(info.possible_crtcs()).contains(&crtc) {
            continue;
        }
        if plane_kind(drm, handle) != Some(PlaneType::Cursor) {
            continue;
        }
        // Modifier-less drivers describe planes with the implicit
        // modifier; a cursor plane additionally wants the linear layout
        // spelled out, which is what Smithay does for this same case.
        let formats: FormatSet = info
            .formats()
            .iter()
            .filter_map(|code| Fourcc::try_from(*code).ok())
            .flat_map(|code| {
                [
                    Format { code, modifier: Modifier::Invalid },
                    Format { code, modifier: Modifier::Linear },
                ]
            })
            .collect();
        cursor.push(PlaneInfo { handle, type_: PlaneType::Cursor, zpos: None, formats, size_hints: None });
    }
    cursor
}

/// A plane's `type` property, read the way Smithay reads it.
fn plane_kind(drm: &DrmDeviceFd, plane: plane::Handle) -> Option<PlaneType> {
    let props = drm.get_properties(plane).ok()?;
    let (ids, vals) = props.as_props_and_values();
    for (&id, &val) in ids.iter().zip(vals.iter()) {
        let info = drm.get_property(id).ok()?;
        if info.name().to_str().ok() == Some("type") {
            return Some(match val {
                x if x == PlaneType::Primary as u64 => PlaneType::Primary,
                x if x == PlaneType::Cursor as u64 => PlaneType::Cursor,
                _ => PlaneType::Overlay,
            });
        }
    }
    None
}

/// The cursor goes on the hardware cursor plane; everything else is
/// composited into the swapchain buffer and page-flipped from there.
///
/// The cursor plane is not an optimization here, it is the expected
/// behavior of a Wayland compositor: the display controller scans the
/// pointer out itself, so it tracks the hand at input rate and its
/// movement costs no recomposite of the scene beneath it. A pointer
/// drawn into the frame like any other element instead moves at the
/// frame rate and drags a full redraw behind it, which is exactly the
/// lag people notice and describe as a compositor feeling wrong. The
/// renderer already tags the pointer `Kind::Cursor`, and
/// [`attach_output`] hands the `DrmCompositor` a GBM device to
/// allocate cursor buffers from, so this flag is the whole switch.
///
/// The *other* scan-out flags stay off. Direct scan-out would let a
/// fullscreen client's own buffer become the scanout buffer, skipping
/// the GLES pass entirely - a real win for video and games - but it
/// cannot pay yet: this backend repaints the whole output every frame
/// on purpose (see [`render_frame_session`]), which is precisely the
/// case where composition costs the same either way, and it adds a
/// second, hardware-dependent path through the frame that the nested
/// backend has no counterpart for, so a scan-out-only bug would be
/// invisible until someone logged in from a TTY. Enable those together
/// with per-element damage, and with an import node on the framebuffer
/// exporter (see [`init`]), not before.
///
/// `SKIP_CURSOR_ONLY_UPDATES` is deliberately absent: it suppresses the
/// commit when nothing but the cursor moved, which is the one update
/// this compositor most wants to deliver.
const FRAME_FLAGS: FrameFlags = FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT;

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
/// accepted, so exceeding this is a driver bug rather than ordinary
/// backpressure, and the log line is the difference between a frozen
/// desktop that explains itself over SSH and one that says nothing at
/// all. Two seconds is far beyond any real refresh interval, including
/// a 24Hz cinema mode. This threshold only talks; the one that acts is
/// [`FLIP_STALL_RECOVERY`].
const FLIP_STALL_WARNING: Duration = Duration::from_secs(2);

/// How long a page flip may stay in flight before the session stops
/// waiting for it and resets the device out from under it.
///
/// This used not to exist, on the reasoning that a lost flip cannot be
/// recovered from because the display engine still owns the buffer and
/// drawing into it would corrupt the screen. That is right about the
/// buffer and wrong about the conclusion: the flip is unrecoverable
/// only while the crtc keeps its current programming.
/// [`service_pending_flips`] never renders into the stuck buffer — it
/// tears the crtc's state down and drops every swapchain buffer, which
/// makes the next frame go out as a full modeset commit rather than a
/// page flip, on a display engine that by then owns nothing. The
/// alternative is what the session log from 2026-08-29 05:23 records:
/// an output frozen for the remaining thirty seconds of the session,
/// with the error line advising a VT switch as the only way out.
///
/// Five seconds rather than two, for two reasons. A driver that is
/// merely very late deserves to finish, because the reset costs a
/// visible modeset flicker. And the threshold doubles as the retry
/// interval: if a reset does not take, the next attempt is five seconds
/// away instead of one frame away.
const FLIP_STALL_RECOVERY: Duration = Duration::from_secs(5);

/// How long the event loop may go without reaching
/// [`service_pending_flips`] before the gap is treated as the loop
/// having been blocked, rather than as time a flip spent outstanding.
///
/// The stall thresholds above measure "how long has the kernel had this
/// flip", and they are only meaningful if this process was actually
/// around to notice a completion. It is not always: anything that
/// blocks the single main thread — most infamously a synchronous child
/// process in a dock widget's sampler — parks the loop with the flip's
/// completion event sitting unread in the DRM fd. Charging that time to
/// the driver produced the 2026-08-29 diagnosis this comment exists to
/// prevent a repeat of: a widget shelling out to `nmcli` blocked the
/// loop for ~3.6s once every ~34s, and every one of those was logged as
/// "no page-flip completion from the DRM device". The device was idle
/// and correct the whole time; the compositor had simply stopped
/// asking.
///
/// The danger is not the misleading log line, it is
/// [`FLIP_STALL_RECOVERY`]: a blocked loop that crosses five seconds
/// would trigger `reset_state()`, and on Apple's DCP a modeset commit
/// blocks the caller for as long as 8.5 seconds inside `iomfb_modeset`.
/// The "recovery" would then be several times worse than the stall, for
/// a flip that was never stuck. So time the loop demonstrably spent
/// away is credited back to every flip in flight instead of counted
/// against it.
///
/// Generous relative to the 16ms housekeeping cadence, because the cost
/// of being wrong is asymmetric: crediting a little real driver latency
/// only delays a warning, while charging a little blocked time can fire
/// a modeset.
const LOOP_BLOCK_GRACE: Duration = Duration::from_millis(250);

/// Whether to force full-output damage on every frame instead of
/// trusting the damage tracker's per-element result.
///
/// The escape hatch for the change described at the `reset_buffer_ages`
/// call site. Partial damage is the default because full-frame damage
/// costs a 2560x1600 recomposite per pointer sample on a panel with no
/// cursor plane; but the artifact it prevented (stale rectangles where
/// an element moved and the tracker did not notice) is a *visual* bug,
/// so getting back to the safe behaviour must not require a compiler.
/// `CHONKSTEP_FULL_DAMAGE=1` restores it for the next session.
///
/// Read once and cached: this sits in the per-output frame path, and a
/// per-frame environment lookup is exactly the kind of small syscall
/// that this file has spent its recent history removing.
fn full_damage_forced() -> bool {
    static FORCED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FORCED.get_or_init(|| {
        let forced = std::env::var_os("CHONKSTEP_FULL_DAMAGE").is_some_and(|value| value != "0");
        if forced {
            tracing::info!(
                "CHONKSTEP_FULL_DAMAGE is set; forcing full-output damage on every frame"
            );
        }
        forced
    })
}

/// Everything the session backend owns while it runs: the seat, the
/// DRM device and its GBM allocator, the EGL/GLES renderer, one
/// [`SessionOutput`] per connected connector, and the libinput context.
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
    /// and for the `is_active` guard on the render path — the surfaces
    /// inside the DRM compositors keep their own references for
    /// commits.
    drm: DrmDevice,
    /// The GLES renderer every scene element is imported into and
    /// drawn with — one renderer for every output, because all of them
    /// hang off the same EGL display on the same device. `pub(crate)`
    /// because it is the one piece of this struct the rest of the crate
    /// legitimately needs: `capture.rs` renders the same scene
    /// offscreen through it, and `dmabuf.rs` asks it which hardware
    /// buffer formats to advertise. Also reachable as
    /// [`SessionGraphics::renderer`], which is the spelling
    /// backend-blind code uses so both arms of [`Graphics`] read the
    /// same.
    pub(crate) renderer: GlesRenderer,
    /// One per connected connector, in the order [`init`] enumerated
    /// them. That order is load-bearing: it is the order
    /// `Compositor::outputs` holds the matching `Output`s in, which is
    /// the order `Backend::monitors` reports to `wm-core`, so index 0
    /// is the primary monitor on all three.
    outputs: Vec<SessionOutput>,
    /// The libinput context, kept so the session notifier can suspend
    /// and resume it. The `LibinputInputBackend` calloop owns holds its
    /// own clone of the same underlying context.
    libinput: Libinput,
    /// When [`service_pending_flips`] last ran. The gap between
    /// consecutive visits is how long the main thread was away, which
    /// is time no flip should be charged for — see [`LOOP_BLOCK_GRACE`].
    last_service: Instant,
}

/// One output being scanned out: its crtc, its place in the global
/// coordinate space every rect in this compositor lives in, and the
/// frame bookkeeping that cannot be shared with any other output.
///
/// Both of the per-frame flags are here rather than on
/// [`SessionGraphics`] because the kernel completes page flips *per
/// crtc*: two monitors on different refresh rates (or the same rate,
/// unsynchronized) are ready to accept a new frame at different
/// moments, so "is a flip in flight" and "does this need redrawing"
/// are questions with one answer each per output.
struct SessionOutput {
    /// Connector name (`eDP-1`, `HDMI-A-2`), for log lines that have to
    /// name which screen is misbehaving.
    name: String,
    /// The scanout engine driving it. Always equal to
    /// `drm_compositor.crtc()`; kept alongside so the page-flip handler
    /// can find the right output without reaching into the compositor.
    crtc: crtc::Handle,
    /// This output's top-left corner in global compositor space — the
    /// viewport offset the renderer subtracts from every element to put
    /// the shared scene into this output's framebuffer.
    position: Point,
    /// Swapchain, mode setting, and page flips for this output.
    drm_compositor: SessionDrmCompositor,
    /// The page flip in flight, if any — set on a successful
    /// `queue_frame`, cleared by the completion event that answers it.
    /// While it is `Some`, [`render_frame_session`] refuses to draw this
    /// output: the swapchain has at most a couple of slots, and
    /// rendering ahead of the display either exhausts them or throws
    /// away work nobody will ever see.
    frame_pending: Option<PendingFlip>,
    /// Whether what is on this screen is stale. The ledger's single
    /// `damage` flag says the *scene* changed; this says the change has
    /// not reached this particular screen yet, which is a different
    /// question once an output can be blocked on a flip while its
    /// neighbour is not. [`render_frame_session`] distributes the one
    /// into the many, and [`redraw_pending`] is what keeps the dispatch
    /// loop coming back until every output has caught up.
    dirty: bool,
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

    /// The KMS device's fd, cloned for whoever needs to import into
    /// it — today that is the explicit-sync global
    /// (`dmabuf::init_syncobj`), which imports client syncobj
    /// timelines into this device to wait on them.
    pub(crate) fn drm_device_fd(&self) -> DrmDeviceFd {
        self.drm.device_fd().clone()
    }
}

/// What [`init`] hands back to `run`: the graphics stack plus every
/// output it discovered, each already configured with its mode and its
/// position in the global space. Any event sources the session needs
/// (DRM page flips, libinput devices, udev hot-plug, seat activation)
/// are registered on the loop by [`init`] itself.
pub(crate) struct SessionInit {
    pub graphics: Graphics,
    /// Connector order, primary first — see [`SessionGraphics::outputs`]
    /// for why that order is the same everywhere.
    pub outputs: Vec<OutputSetup>,
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
    let Device { mut drm, notifier: drm_notifier, gbm, connectors } = device;
    tracing::info!(
        device = %device_path.display(),
        outputs = connectors.len(),
        "session backend: driving every connected output on one device"
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

    // 4. One output per connected connector, laid out left to right in
    //    connector order at their mode sizes (see the module docs on
    //    why that layout is a guess and not a configuration). A
    //    connector that cannot be brought up costs its own screen and
    //    nothing else — the session runs on whichever ones worked, and
    //    only an empty result is fatal, which is exactly the
    //    single-output behavior when that one output fails.
    let mut session_outputs: Vec<SessionOutput> = Vec::with_capacity(connectors.len());
    let mut setups: Vec<OutputSetup> = Vec::with_capacity(connectors.len());
    let mut failures: Vec<String> = Vec::new();
    let mut next_x: i32 = 0;
    for target in connectors {
        let name = connector_name(&target.info);
        let position = Point::new(next_x, 0);
        match attach_output(&mut drm, &gbm, &render_formats, &target, position) {
            Ok((session_output, setup)) => {
                tracing::info!(
                    output = %name,
                    mode = %format!(
                        "{}x{}@{}",
                        target.mode.size().0,
                        target.mode.size().1,
                        target.mode.vrefresh()
                    ),
                    x = position.x,
                    y = position.y,
                    "session backend: output up"
                );
                next_x += setup.size.w as i32;
                session_outputs.push(session_output);
                setups.push(setup);
            }
            Err(reason) => {
                tracing::error!(output = %name, %reason, "could not bring up an output; it stays dark");
                failures.push(format!("{name}: {reason}"));
            }
        }
    }
    if session_outputs.is_empty() {
        return Err(format!(
            "no output could be brought up on {} ({})",
            device_path.display(),
            failures.join("; ")
        )
        .into());
    }

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
                    // Per crtc: the device hands every output's flip
                    // completion through this one source, and only the
                    // output that owns that crtc is free to draw again.
                    let Some(output) = session.outputs.iter_mut().find(|output| output.crtc == crtc)
                    else {
                        tracing::debug!(?crtc, "page flip completed on a crtc we do not drive");
                        return;
                    };
                    output.frame_pending = None;
                    if let Err(error) = output.drm_compositor.frame_submitted() {
                        // The swapchain slot could not be recycled.
                        // Rendering continues; if this repeats the next
                        // frame will fail with `NoFreeSlotsError`, which
                        // is the loud version of the same problem.
                        tracing::warn!(?error, output = %output.name, "page-flip completion was rejected by the DRM compositor");
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
                tracing::info!(?device_id, "a DRM device changed (connector hot-plug?); the session keeps the outputs it started with");
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
            let Compositor { graphics, wm, seat, .. } = comp;
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
                    // cannot arrive. Every output's, because the device
                    // going away takes all of them.
                    for output in session.outputs.iter_mut() {
                        output.frame_pending = None;
                    }
                }
                SessionEvent::ActivateSession => {
                    tracing::info!("session resumed: reclaiming the DRM device and input");
                    // Any button held when the seat was taken away
                    // released into whichever session owned it, so the
                    // grab those presses built can never be completed
                    // here.
                    crate::input::clear_implicit_grab(seat);
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
                    for output in session.outputs.iter_mut() {
                        if let Err(error) = output.drm_compositor.reset_state() {
                            tracing::error!(
                                ?error,
                                output = %output.name,
                                "could not reset the crtc state after resuming"
                            );
                        }
                        // Buffer contents and buffer ages both survived
                        // the switch but mean nothing now: the foreign
                        // session painted over the screen.
                        output.drm_compositor.reset_buffers();
                        output.frame_pending = None;
                    }
                    // Marks every output dirty on the next render pass
                    // (see `render_frame_session`), which is what
                    // repaints the screens the other session scribbled
                    // on.
                    wm.backend_mut().mark_damaged();
                }
            }
        })
        .map_err(|error| format!("failed to register the seat session source: {error}"))?;

    // 6. Hand the assembled stack back to `run`, which registers an
    //    output global per output and builds the damage trackers from
    //    them.
    Ok(SessionInit {
        graphics: Graphics::Session(Box::new(SessionGraphics {
            seat_session,
            drm,
            renderer,
            outputs: session_outputs,
            libinput,
            last_service: Instant::now(),
        })),
        outputs: setups,
    })
}

/// Brings one connector up: the wayland [`Output`] clients see, the DRM
/// surface binding crtc + connector + mode, and the [`DrmCompositor`]
/// that turns render elements into page flips on it.
///
/// `position` is where this output's top-left corner sits in the global
/// coordinate space `wm-core` and the renderer share. It is written
/// into the output's wayland state too, so `wl_output`'s geometry and
/// `xdg_output`'s logical position tell clients the same story the
/// compositor's own hit-testing does.
fn attach_output(
    drm: &mut DrmDevice,
    gbm: &GbmDevice<DrmDeviceFd>,
    render_formats: &[Format],
    target: &ConnectorTarget,
    position: Point,
) -> Result<(SessionOutput, OutputSetup), String> {
    let ConnectorTarget { info, crtc, mode } = target;
    let name = connector_name(info);

    // The output, in wayland terms. No `Transform::Flipped180`: that
    // correction belongs to the winit backend, whose EGL surface
    // disagrees with the output about where the origin is (see
    // `state.rs`'s `run`). A KMS scanout buffer's origin is the
    // screen's, so any transform here would visibly flip the desktop.
    let wl_mode = OutputMode::from(*mode);
    let (physical_w, physical_h) = info.size().unwrap_or((0, 0));
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (physical_w as i32, physical_h as i32).into(),
            subpixel: info.subpixel().into(),
            // The manufacturer and model live in the connector's EDID
            // blob, and parsing EDID means either a parser in this
            // crate or the `smithay-drm-extras` dependency — neither
            // worth it for two strings clients only ever show in an
            // about box. The connector name is at least true and
            // stable.
            make: "Unknown".into(),
            model: name.clone(),
        },
    );
    output.set_preferred(wl_mode);
    output.change_current_state(
        Some(wl_mode),
        Some(Transform::Normal),
        None,
        Some((position.x, position.y).into()),
    );
    let size = Size::new(wl_mode.size.w.max(0) as u32, wl_mode.size.h.max(0) as u32);

    // The DRM surface binds crtc + connector + mode; the compositor
    // wraps it with the swapchain and the damage tracking that turn a
    // list of render elements into a page flip.
    let surface = drm
        .create_surface(*crtc, *mode, &[info.handle()])
        .map_err(|error| format!("could not drive it from {crtc:?}: {error}"))?;
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
    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), None);
    // A *static* mode source, deliberately not the `Output`: the
    // output advertises the session's UI scale to clients
    // (`state.rs`'s `advertise_scale`), and an auto-tracking source
    // would feed that scale into the `DrmCompositor`'s damage tracker,
    // which multiplies every element's logical size by it while
    // leaving its physical position alone — the doubled-chrome failure
    // `state.rs::physical_damage_tracker` recounts, on the session
    // backend this time. This compositor composes in physical pixels,
    // so its render pipeline is pinned to the mode's pixel size at
    // scale 1, matching the winit backend's pinned trackers. Static is
    // safe here because this backend never changes a mode after setup
    // (connector hotplug is scoped out — see the module docs); the day
    // it does, `DrmCompositor::set_output_mode_source` is the lever.
    // The output's *position* never enters the frame either way: every
    // element reaching `render_frame` must already be in this output's
    // own space (which is what the renderer's viewport offset does).
    // Which planes this crtc actually offers. Logged because a
    // missing cursor plane is invisible otherwise: the frame just
    // quietly composites the pointer, and the only clue is a trace
    // line deep inside the DRM compositor.
    // Universal planes, asserted here rather than only at open time.
    //
    // Client capabilities live on the open file description, and this
    // session does not keep one: libseat hands the device back as a
    // *fresh* fd every time the seat activates (a VT switch in, or the
    // very first activation racing startup), and a fresh fd starts with
    // the capability off. That is enough to hide the cursor and overlay
    // planes for the rest of the session - measured directly here: two
    // planes visible before the device was opened, one at surface time.
    // Asking again at the point of use costs an ioctl and makes plane
    // discovery independent of when activation happened to land.
    {
        use smithay::reexports::drm::Device as _;
        if let Err(error) = drm
            .device_fd()
            .set_client_capability(smithay::reexports::drm::ClientCapability::UniversalPlanes, true)
        {
            tracing::warn!(?error, "universal planes refused at surface time; no hardware cursor");
        }
    }
    let mut planes = surface.planes().clone();
    if planes.cursor.is_empty() {
        let recovered = rediscover_cursor_planes(drm.device_fd(), *crtc);
        if !recovered.is_empty() {
            tracing::info!(
                output = %name,
                count = recovered.len(),
                "recovered cursor plane(s) Smithay's discovery dropped; the pointer stays on hardware"
            );
            planes.cursor = recovered;
        }
    }
    tracing::info!(
        output = %name,
        primary = planes.primary.len(),
        cursor = planes.cursor.len(),
        overlay = planes.overlay.len(),
        "DRM planes available to this crtc"
    );

    let drm_compositor = SessionDrmCompositor::new(
        OutputModeSource::Static {
            size: wl_mode.size,
            scale: smithay::utils::Scale::from(1.0),
            transform: Transform::Normal,
        },
        surface,
        // Explicit planes: Smithay's own discovery, with any cursor
        // plane it wrongly dropped put back (see
        // `rediscover_cursor_planes`). Which of them may actually be
        // used is decided per frame by `FRAME_FLAGS` - today that is
        // the cursor plane only.
        Some(planes),
        allocator,
        framebuffer_exporter,
        COLOR_FORMATS.iter().copied(),
        render_formats.iter().copied(),
        drm.cursor_size(),
        // The GBM device the cursor plane's buffers are allocated
        // from. Passing `None` here is what disables the hardware
        // cursor outright, so it is passed: see `FRAME_FLAGS`.
        Some(gbm.clone()),
    )
    .map_err(|error| format!("could not set up scanout: {error}"))?;

    Ok((
        SessionOutput {
            name,
            crtc: *crtc,
            position,
            drm_compositor,
            frame_pending: None,
            // Nothing has ever been drawn on it.
            dirty: true,
        },
        OutputSetup { output, position, size },
    ))
}

/// Whether any output is still waiting for a frame it has not been able
/// to draw yet — a flip was in flight, the device was inactive, or the
/// last attempt failed.
///
/// The dispatch loop renders when the ledger reports damage; this is the
/// second half of that condition, and it exists because
/// [`render_frame_session`] consumes the ledger's one damage flag into
/// per-output [`SessionOutput::dirty`] flags. Without it a frame that
/// reached one screen but not its neighbour would sit unfinished until
/// something unrelated damaged the scene again. The nested backend has
/// exactly one output and clears damage only on success, so it never
/// needs this and always answers `false`.
///
/// A flip still in flight counts too, and not because anything can be
/// drawn while it is — [`render_frame_session`] will skip that output —
/// but because [`service_pending_flips`] runs from inside that function
/// and has to be reached on a desktop where nothing at all is
/// happening. That is precisely the state a lost flip leaves behind: no
/// damage, no dirty output, nothing to bring the render path back, and
/// so no one left to notice. The cost is one extra pass over the
/// outputs per housekeeping wakeup while a flip is outstanding, which
/// at 60Hz is most of them, and each pass is a `Duration` comparison
/// per output.
pub(crate) fn redraw_pending(graphics: &Graphics) -> bool {
    match graphics {
        Graphics::Winit(_) => false,
        Graphics::Session(session) => session
            .outputs
            .iter()
            .any(|output| output.dirty || output.frame_pending.is_some()),
    }
}

/// Services every page flip in flight: names the ones that have overrun
/// [`FLIP_STALL_WARNING`], and resets the device out from under any
/// that have overrun [`FLIP_STALL_RECOVERY`].
///
/// Called once per render pass, before any output is drawn, and that
/// placement is half the fix. The check this replaces sat *after* the
/// `dirty` test inside the draw loop, so a stuck flip on an idle
/// desktop went unnoticed until something happened to damage the scene:
/// the session log that prompted this work shows a flip queued at
/// 05:22:59.73 and first reported at 05:23:03.29 — `waited=3.558s`
/// against a two-second threshold — because nothing asked the question
/// until the pointer moved.
///
/// Returns whether the device was reset. No caller needs it today (the
/// reset leaves every output dirty, and the draw loop that follows
/// repaints them), but a bare `bool` at the call site reads better than
/// a unit-returning function whose name suggests it might not do
/// anything.
fn service_pending_flips(session: &mut SessionGraphics) -> bool {
    // Credit back whatever time the main thread spent not running. A
    // flip is only "late" relative to a loop that was there to see it
    // complete; see [`LOOP_BLOCK_GRACE`] for why charging a blocked
    // loop's time to the driver is actively dangerous on this hardware.
    // Pushing `queued_at` forward (rather than subtracting at the
    // comparison) keeps the credit sticky: a flip blocked across
    // several passes accumulates all of them, and the `waited` value
    // that reaches the log is then the driver's share alone.
    let now = Instant::now();
    let away = now.saturating_duration_since(session.last_service);
    session.last_service = now;
    if let Some(blocked) = away.checked_sub(LOOP_BLOCK_GRACE) {
        for output in session.outputs.iter_mut() {
            if let Some(flip) = output.frame_pending.as_mut() {
                flip.queued_at += blocked;
                // A flip already named as stalled that turns out to
                // have been waiting on us, not on the kernel, should be
                // allowed to say so again once it has genuinely
                // overrun on its own merits.
                if flip.queued_at.elapsed() < FLIP_STALL_WARNING {
                    flip.stall_reported = false;
                }
            }
        }
        tracing::debug!(
            ?blocked,
            "main loop was away; crediting the time back to flips in flight rather than the driver"
        );
    }

    let mut needs_reset = false;
    for output in session.outputs.iter_mut() {
        let Some(flip) = output.frame_pending.as_mut() else {
            continue;
        };
        let waited = flip.queued_at.elapsed();
        if !flip.stall_reported && waited > FLIP_STALL_WARNING {
            flip.stall_reported = true;
            tracing::error!(
                ?waited,
                output = %output.name,
                "no page-flip completion from the DRM device; this output is frozen until one \
                 arrives or the stall watchdog resets the device"
            );
        }
        needs_reset |= waited > FLIP_STALL_RECOVERY;
    }
    if !needs_reset {
        return false;
    }

    // Device-wide, and deliberately not `DrmDevice::activate(true)`
    // even though the VT-resume path recovers with exactly that call.
    // `activate`'s `disable_connectors` argument only takes effect when
    // the device had actually been paused — it is reached through
    // `!set_active(true)`, and `set_active` returns the *previous*
    // flag — so on a device that never left us it does nothing at all.
    // The reset has to be asked for directly.
    tracing::warn!("resetting the DRM device to recover from a stalled page flip");
    if let Err(error) = session.drm.reset_state() {
        tracing::error!(
            ?error,
            "could not reset the DRM device after a stalled page flip; the screen stays frozen \
             until the next attempt"
        );
        return false;
    }

    for output in session.outputs.iter_mut() {
        // Forces the next `render_frame` to report a non-empty result
        // and the next `queue_frame` to go out as a full modeset commit
        // rather than a page flip. That is the part that actually
        // unwedges a crtc the kernel still believes has a flip
        // outstanding, and the reason a plain retry would not: further
        // page flips against a pending one come back `EBUSY`.
        if let Err(error) = output.drm_compositor.reset_state() {
            tracing::error!(
                ?error,
                output = %output.name,
                "could not reset the crtc state after a stalled page flip"
            );
        }
        // The swapchain slot the lost flip is holding will never come
        // back through `frame_submitted`. Dropping every buffer is what
        // keeps the frames after the reset from failing with
        // `NoFreeSlotsError`.
        output.drm_compositor.reset_buffers();
        // A late completion for the flip just abandoned finds this
        // `None` (or, if the reset's own frame is already out, a newer
        // flip's `Some`) and calls `frame_submitted` with nothing
        // pending, which smithay answers `Ok(None)`. Harmless, and the
        // same shape the pause/resume path has always had.
        output.frame_pending = None;
        output.dirty = true;
    }
    true
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

/// Draws one frame onto the hardware — [`crate::renderer::build_scene`]
/// through the session's renderer, submitted as a page flip — once per
/// output that needs one.
///
/// Deliberately shaped like `render_frame_winit`: build the one shared
/// scene, submit, then send frame callbacks; an output that could not
/// be drawn stays dirty, so a transient failure costs it one frame and
/// retries on the next wakeup instead of wedging the desktop.
///
/// The ledger carries a single `damage` flag — the scene either changed
/// or it did not — and that is right, because the scene is one scene.
/// What is per output is whether the change has *landed* there: page
/// flips complete per crtc, so at any moment one screen can be free to
/// draw while its neighbour is still showing the frame before. So the
/// one flag is consumed into per-output [`SessionOutput::dirty`] flags
/// here, at the top of the pass, and [`redraw_pending`] is what brings
/// the dispatch loop back for the outputs that had to wait. With a
/// single output the two collapse into exactly the old behavior: damage
/// sets dirty, dirty clears on a successful submit, and a blocked or
/// failed frame is retried on the next wakeup.
pub(crate) fn render_frame_session(comp: &mut Compositor) {
    // Disjoint field borrows: the graphics stack mutates while the
    // ledger is read. Both live on `Compositor`, so destructure rather
    // than going through `&mut self` methods.
    let Compositor {
        wm,
        graphics,
        outputs,
        pointer_location,
        cursor_status,
        cursors,
        start_time,
        ..
    } = comp;
    let Graphics::Session(session) = graphics else {
        return;
    };

    if wm.backend().damage {
        for output in session.outputs.iter_mut() {
            output.dirty = true;
        }
        wm.backend_mut().damage = false;
    }
    // Someone else owns the VT: every commit would fail with
    // `DeviceInactive`, and the outputs keep their dirty flags so the
    // resume handler's repaint has something to repaint.
    let device_active = session.drm.is_active();

    // Before anything is drawn, and unconditionally rather than per
    // dirty output: a flip that is never going to complete is exactly
    // the case where nothing else in this function would run. Skipped
    // on an inactive device, where every flip is legitimately abandoned
    // and the resume handler is what clears them.
    if device_active {
        service_pending_flips(session);
    }

    let SessionGraphics { renderer, outputs: session_outputs, .. } = &mut **session;
    let mut drew_any = false;
    for output in session_outputs.iter_mut() {
        if output.frame_pending.is_some() {
            // A page flip is in flight on this crtc. Rendering now would
            // burn a swapchain slot on a frame the display cannot show
            // before the one already queued, so leave the output dirty
            // and let the vblank handler's clearing of this flag be what
            // schedules the redraw. Whether that flip is merely in
            // flight or stuck is `service_pending_flips`' question, and
            // it has already been asked this pass.
            continue;
        }
        if !output.dirty {
            continue;
        }
        if !device_active {
            continue;
        }

        // Resetting every buffer age makes the internal damage tracker
        // treat the whole output as stale, forcing a full-frame submit —
        // the session-side equivalent of the winit path's `age = 0`, and
        // the same trade the X11 session made by running picom with
        // `--no-use-damage`: a little GPU fill in exchange for never
        // chasing a partial-damage artifact.
        //
        // That comment used to end "revisit only with evidence". The
        // evidence arrived. This display has no hardware cursor plane
        // (apple-drm registers none by design — the DCP cannot blend a
        // third surface, and it faults on a framebuffer that clips
        // off-screen), so the pointer is composited into the scene and
        // every pointer motion marks the output dirty. With full-frame
        // damage that is a 2560x1600 recomposite — 4.1 megapixels,
        // roughly 983 MB/s of writes — per trackpad sample, on a machine
        // whose panel is driven over a firmware mailbox. It is the
        // single largest avoidable cost in the frame path, and it is
        // paid hardest during exactly the interaction where latency is
        // most visible.
        //
        // So the default is now the tracker's own per-element damage.
        // The old behaviour stays one environment variable away because
        // the artifact class it guarded against is real and shows up as
        // stale rectangles rather than a crash — the kind of bug a user
        // hits before a developer does, and one that would otherwise
        // require a rebuild to escape.
        if full_damage_forced() {
            output.drm_compositor.reset_buffer_ages();
        }

        // One scene build per output: the elements are the same objects
        // in different places, since `render_frame` intersects them
        // against a rectangle anchored at this output's own origin and
        // knows nothing of where the output sits globally.
        let (elements, clear_color) = crate::renderer::build_scene(
            wm.backend(),
            renderer,
            *pointer_location,
            cursor_status,
            cursors,
            output.position,
        );

        let rendered = match output
            .drm_compositor
            .render_frame(renderer, &elements, clear_color, FRAME_FLAGS)
        {
            Ok(result) => {
                // The GPU may still be drawing into the buffer we are
                // about to hand the scanout engine. Where the driver
                // supports fencing the DRM compositor passes the fence
                // along with the commit and this is skipped; where it
                // does not, waiting on the CPU is the only thing
                // standing between us and a half-drawn frame on screen.
                if result.needs_sync() {
                    if let PrimaryPlaneElement::Swapchain(element) = &result.primary_element {
                        if let Err(error) = element.sync.wait() {
                            tracing::warn!(?error, output = %output.name, "interrupted waiting on the render fence; the frame may tear");
                        }
                    }
                }
                !result.is_empty
            }
            Err(error) => {
                // Includes the atomic test failures a returning VT
                // switch can produce; the seat-activation handler above
                // resets the device and marks damage, so the recovery
                // path is there rather than duplicated here.
                if crate::renderer::note_frame_failure() {
                    tracing::warn!(?error, output = %output.name, "DRM render failed; keeping this output dirty for a retry");
                }
                continue;
            }
        };

        if rendered {
            match output.drm_compositor.queue_frame(()) {
                Ok(()) => {
                    output.frame_pending =
                        Some(PendingFlip { queued_at: Instant::now(), stall_reported: false });
                }
                // Nothing on the crtc actually changed. Not an error,
                // and not worth a retry: the scene is already on screen.
                Err(FrameError::EmptyFrame) => {
                    tracing::trace!(output = %output.name, "frame produced no crtc changes; no page flip queued");
                }
                Err(error) => {
                    tracing::warn!(?error, output = %output.name, "queueing the page flip failed; keeping this output dirty for a retry");
                    continue;
                }
            }
        }
        output.dirty = false;
        drew_any = true;
    }

    if drew_any {
        // Frame callbacks are attributed to the primary output. They
        // pace clients, and a client that hears from one output per
        // frame draws at that output's rate — so on a mixed-refresh
        // desktop everything is paced by the primary. Doing better means
        // tracking which output each surface is actually on
        // (`Output::enter`/`leave` and a per-surface primary scan-out
        // choice), which is the same bookkeeping presentation feedback
        // would need and neither exists yet.
        // A frame reached the hardware, so the failure streak that
        // throttles the warnings above is over.
        crate::renderer::note_frame_success();
        if let Some(primary) = outputs.first() {
            crate::renderer::send_frame_callbacks(
                wm.backend(),
                &primary.output,
                cursor_status,
                start_time.elapsed(),
            );
        }
    }
}

/// One opened, mode-set-capable DRM device together with every output
/// it will drive. Produced by [`probe_device`] and consumed by [`init`].
struct Device {
    drm: DrmDevice,
    notifier: DrmDeviceNotifier,
    gbm: GbmDevice<DrmDeviceFd>,
    /// Every usable connected connector, in kernel enumeration order.
    /// Never empty — a device with nothing plugged in is not a device
    /// this session can run on (see [`probe_device`]).
    connectors: Vec<ConnectorTarget>,
}

/// A connector this session will drive, with the crtc and mode chosen
/// for it. Chosen up front, before any EGL or GBM state exists, so a
/// device that cannot actually drive anything is rejected while the
/// next candidate is still worth trying.
struct ConnectorTarget {
    info: connector::Info,
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
    // Enable universal planes on the device ourselves, before Smithay
    // opens it.
    //
    // Without universal planes the kernel shows a client only overlay
    // planes - the primary and cursor planes stay hidden, which is the
    // pre-2014 API a modern compositor cannot work from. Smithay asks
    // for the capability itself, but on this project's virtio-gpu test
    // machine that attempt is recorded as a failure, and the fallout is
    // silent and total: `plane_handles` then lists one plane instead of
    // two, `DrmSurface::planes()` reports no cursor plane, and the
    // pointer is composited into every frame with only a trace line
    // ("no free plane found") to say why. Asking first, on the same fd,
    // is what makes the kernel list every plane (confirmed by probing
    // both ways on the same hardware), and `rediscover_cursor_planes`
    // then puts back what Smithay's own bookkeeping dropped.
    {
        use smithay::reexports::drm::Device as _;
        if let Err(error) =
            fd.set_client_capability(smithay::reexports::drm::ClientCapability::UniversalPlanes, true)
        {
            tracing::warn!(?error, "universal planes refused at open; the hardware cursor may be unavailable");
        }
    }

    let (drm, notifier) = DrmDevice::new(fd.clone(), true)
        .map_err(|error| format!("not a usable DRM device: {error}"))?;
    let connectors = pick_outputs(&drm)?;
    let gbm = GbmDevice::new(fd).map_err(|error| format!("GBM init failed: {error}"))?;

    Ok(Device { drm, notifier, gbm, connectors })
}

/// Picks the connectors this session paints on: every connected one
/// that has both a mode and a free crtc, in the kernel's enumeration
/// order — which is stable across boots on a given machine, and is
/// therefore what decides which monitor is primary and how the outputs
/// are laid out left to right (see the module docs: there is no
/// configuration to consult instead, yet).
///
/// Failing outright when *nothing* is usable is the point of doing this
/// during probing: a device with no connected screen is how a
/// render-only node or the wrong GPU announces itself, and [`init`]'s
/// candidate walk should move on to the next one rather than come up
/// with a session nobody can see.
fn pick_outputs(drm: &DrmDevice) -> Result<Vec<ConnectorTarget>, String> {
    let resources = drm
        .resource_handles()
        .map_err(|error| format!("no KMS resources (a render-only node?): {error}"))?;

    // Why each connector was passed over, so a black screen is
    // diagnosable from one log line instead of a bisect.
    let mut skipped: Vec<String> = Vec::new();
    let mut targets: Vec<ConnectorTarget> = Vec::new();
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
        // A crtc drives exactly one output, so one already spoken for by
        // an earlier connector is not a candidate for this one — which
        // is also the ceiling on how many monitors this session lights
        // up: hardware with two crtcs and three connected monitors
        // leaves the third dark, and says so here.
        let taken: Vec<crtc::Handle> = targets.iter().map(|target| target.crtc).collect();
        let Some(crtc) = crtc_for(drm, &resources, &info, &taken) else {
            skipped.push(format!("{name} has no free crtc able to drive it"));
            continue;
        };
        targets.push(ConnectorTarget { info, crtc, mode });
    }

    if !targets.is_empty() {
        if !skipped.is_empty() {
            tracing::debug!(skipped = %skipped.join(", "), "connectors this session will not drive");
        }
        return Ok(targets);
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

/// Finds a crtc — the scanout engine — able to drive this connector and
/// not already assigned to another one. The encoder already attached to
/// it is tried first because reusing the kernel's existing routing
/// avoids disturbing a working configuration; failing that, any crtc
/// that any of the connector's possible encoders can reach will do.
///
/// `taken` is what makes this safe to call per connector: two outputs
/// sharing a crtc is not a mirror, it is a `create_surface` that fails
/// (or worse, one that succeeds and fights the other for the plane), so
/// the crtc the kernel currently has routed is skipped as readily as any
/// other once someone else has claimed it.
fn crtc_for(
    drm: &DrmDevice,
    resources: &ResourceHandles,
    connector: &connector::Info,
    taken: &[crtc::Handle],
) -> Option<crtc::Handle> {
    connector
        .current_encoder()
        .and_then(|handle| drm.get_encoder(handle).ok())
        .and_then(|encoder| encoder.crtc())
        .filter(|crtc| !taken.contains(crtc))
        .or_else(|| {
            connector
                .encoders()
                .iter()
                .filter_map(|handle| drm.get_encoder(*handle).ok())
                .flat_map(|encoder| resources.filter_crtcs(encoder.possible_crtcs()))
                .find(|crtc| !taken.contains(crtc))
        })
}

/// The name every other tool shows for this output — `eDP-1`, `HDMI-A-2`
/// — assembled the way the kernel and every other compositor assemble
/// it, so logs here line up with `drm_info` and `wayland-info` output.
fn connector_name(connector: &connector::Info) -> String {
    format!("{}-{}", connector.interface().as_str(), connector.interface_id())
}
