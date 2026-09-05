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
//! - **The startup layout is a guess; the running layout is
//!   configurable.** Outputs come up left to right in
//!   connector-enumeration order at their preferred modes — nothing at
//!   *startup* knows the laptop panel sits below the desktop monitor.
//!   Once up, `wlr-output-management` (`output_mgmt.rs`) can reposition
//!   outputs, change their modes ([`apply_mode`] re-programs the crtc)
//!   and set per-output scales — which is how `kanshi` gives a session
//!   a remembered layout. Mirroring and rotation remain future work.
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
//! - **Connector hot-plug on the active GPU.** Udev changes are
//!   debounced, the connector set is re-probed, and outputs, protocol
//!   globals, layout state, gamma slots, and stranded windows are
//!   reconciled together. GPU hot-plug remains separate and unsupported.
//! - The pointer uses the hardware cursor plane when the driver offers
//!   one (see [`FRAME_FLAGS`]); the nested backend composites it
//!   instead, because a window on someone else's desktop has no planes
//!   to ask for.
//! - An eligible fullscreen dmabuf can use the primary plane directly,
//!   bypassing the GLES composition pass. Smithay requires it to be the
//!   final visible element, format-compatible, and accepted by an
//!   atomic KMS test; every other frame falls back to the swapchain.
//!   See [`FRAME_FLAGS`].
//! - **No DRM leasing.** A crtc is never handed to another process,
//!   so a VR headset cannot take one over.

use std::collections::BTreeMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use smithay::backend::allocator::format::FormatSet;
use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
use smithay::backend::allocator::{Format, Fourcc, Modifier};
use smithay::backend::drm::compositor::{DrmCompositor, FrameError, FrameFlags, PrimaryPlaneElement};
use smithay::backend::drm::exporter::gbm::GbmFramebufferExporter;
use smithay::backend::drm::{DrmDevice, DrmDeviceFd, DrmDeviceNotifier, DrmEvent, DrmEventTime, DrmNode, NodeType, PlaneInfo};
use smithay::backend::egl::{EGLContext, EGLDisplay};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::{Event as SessionEvent, Session};
use smithay::backend::udev::{all_gpus, primary_gpu, UdevBackend, UdevEvent};
use smithay::output::{Mode as OutputMode, Output, OutputModeSource, PhysicalProperties};
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::drm::control::{
    connector, crtc, plane, Device as ControlDevice, Mode as DrmMode, ModeFlags, ModeTypeFlags,
    PlaneType, ResourceHandles,
};
use smithay::reexports::input::Libinput;
use smithay::reexports::rustix::fs::OFlags;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::utils::{Clock, DeviceFd, Monotonic, Transform};
use smithay::wayland::presentation::Refresh;
use smithay::desktop::utils::OutputPresentationFeedback;

use wm_theme_api::{Point, Rect, Size};

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
        let Ok(info) = drm.get_plane(handle) else {
            continue;
        };
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
                [Format { code, modifier: Modifier::Invalid }, Format { code, modifier: Modifier::Linear }]
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

/// Whether the kernel explicitly marks a connector as unsuitable for
/// a desktop. HMDs commonly expose this property; consuming a crtc for
/// one both creates an invisible desktop region and can evict a real
/// monitor. Missing/unreadable properties retain the kernel's historic
/// desktop default.
fn connector_is_non_desktop(drm: &DrmDeviceFd, connector: connector::Handle) -> bool {
    let Ok(props) = drm.get_properties(connector) else {
        return false;
    };
    let (ids, values) = props.as_props_and_values();
    ids.iter().zip(values.iter()).any(|(&id, &value)| {
        value != 0
            && drm
                .get_property(id)
                .ok()
                .and_then(|property| property.name().to_str().ok().map(str::to_owned))
                .as_deref()
                == Some("non-desktop")
    })
}

/// The stable, user-facing portion of a connector's EDID. This is the
/// same three-part description Hyprland prints and accepts after a
/// `desc:` monitor selector; the connector name is intentionally not
/// part of it because that identifies the port rather than the panel.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DisplayIdentity {
    make: String,
    model: String,
    serial: String,
}

impl DisplayIdentity {
    fn description(&self) -> String {
        [&self.make, &self.model, &self.serial]
            .into_iter()
            .filter(|part| !part.is_empty())
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Reads just the EDID facts needed for output identity. A compact
/// in-crate reader keeps the hardware build free of a new native
/// libdisplay-info dependency while still using the kernel's canonical
/// EDID property and the host's same `hwdata` PNP names as Hyprland.
fn connector_identity(drm: &DrmDeviceFd, connector: connector::Handle) -> Option<DisplayIdentity> {
    let props = drm.get_properties(connector).ok()?;
    let (ids, values) = props.as_props_and_values();
    let blob = ids.iter().zip(values.iter()).find_map(|(&id, &raw)| {
        let property = drm.get_property(id).ok()?;
        (property.name().to_str() == Ok("EDID"))
            .then(|| property.value_type().convert_value(raw).as_blob())
            .flatten()
    })?;
    let data = drm.get_property_blob(blob).ok()?;
    parse_edid_identity(&data)
}

fn parse_edid_identity(data: &[u8]) -> Option<DisplayIdentity> {
    const HEADER: [u8; 8] = [0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00];
    if data.len() < 128 || data[..8] != HEADER {
        return None;
    }
    let encoded_make = u16::from_be_bytes([data[8], data[9]]);
    let mut pnp = String::with_capacity(3);
    for shift in [10, 5, 0] {
        let letter = ((encoded_make >> shift) & 0x1f) as u8;
        if !(1..=26).contains(&letter) {
            return None;
        }
        pnp.push(char::from(b'A' + letter - 1));
    }

    let product = u16::from_le_bytes([data[10], data[11]]);
    let numeric_serial = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
    let model = edid_descriptor_text(data, 0xfc).unwrap_or_else(|| format!("0x{product:04X}"));
    let serial = edid_descriptor_text(data, 0xff).unwrap_or_else(|| {
        if numeric_serial == 0 {
            String::new()
        } else {
            format!("0x{numeric_serial:08X}")
        }
    });
    Some(DisplayIdentity { make: pnp_name(&pnp), model, serial })
}

/// EDID base-block display descriptors occupy four fixed 18-byte
/// slots. Product-name (0xfc) and serial-string (0xff) descriptors use
/// bytes 5..18 as newline-padded ASCII.
fn edid_descriptor_text(data: &[u8], tag: u8) -> Option<String> {
    (54..126).step_by(18).find_map(|offset| {
        let descriptor = data.get(offset..offset + 18)?;
        if descriptor[..3] != [0, 0, 0] || descriptor[3] != tag {
            return None;
        }
        let text: String = descriptor[5..]
            .iter()
            .copied()
            .take_while(|byte| *byte != b'\n' && *byte != 0)
            .filter(|byte| !byte.is_ascii_control())
            .map(char::from)
            .collect();
        let text = text.trim().to_string();
        (!text.is_empty()).then_some(text)
    })
}

/// Aquamarine/Hyprland expands the three-letter PNP code through the
/// system hwdata table. Use that same source when present and retain
/// the true PNP code as the deterministic fallback.
fn pnp_name(code: &str) -> String {
    static NAMES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    NAMES
        .get_or_init(|| {
            ["/usr/share/hwdata/pnp.ids", "/usr/share/misc/pnp.ids"]
                .into_iter()
                .find_map(|path| std::fs::read_to_string(path).ok())
                .map(|contents| {
                    contents
                        .lines()
                        .filter_map(|line| line.split_once('\t'))
                        .map(|(id, name)| (id.to_string(), name.trim().to_string()))
                        .collect()
                })
                .unwrap_or_default()
        })
        .get(code)
        .cloned()
        .unwrap_or_else(|| code.to_string())
}

/// The cursor may use the hardware cursor plane, and a lone eligible
/// fullscreen client may use the primary plane directly. Everything
/// else is composited into the swapchain and page-flipped from there.
/// `CHONKSTEP_NO_CURSOR_PLANE=1` and
/// `CHONKSTEP_NO_DIRECT_SCANOUT=1` independently disable those paths;
/// see [`frame_flags`].
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
/// Primary-plane scanout skips both the GLES draw and the swapchain
/// memory traffic for a fullscreen video or game. It is deliberately
/// the conservative variant: the client's format must match the
/// swapchain, its buffer must come from this GPU, it must be the last
/// visible scene element, and the driver's atomic test must accept it.
/// A Dock, bar, menu, composited cursor, non-opaque surface, mismatched
/// modifier, or failed test leaves the ordinary composition path in
/// place. Overlay scanout and the format-agnostic primary variant stay
/// off; either would multiply the hardware-dependent state space for a
/// smaller win.
///
/// `SKIP_CURSOR_ONLY_UPDATES` is deliberately absent: it suppresses the
/// commit when nothing but the cursor moved, which is the one update
/// this compositor most wants to deliver.
const FRAME_FLAGS: FrameFlags =
    FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT.union(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT);

/// The frame flags actually used, honoring the two independent plane
/// opt-outs.
///
/// The variable exists as a live diagnostic for cursor-related flicker
/// on NVIDIA: smithay re-renders the cursor plane's buffer on every
/// cursor *content* change (a fresh GBM buffer, a fresh framebuffer
/// object, and a cursor-plane FB swap in the next atomic commit), and a
/// hover that flips a client cursor between arrow and hand churns
/// exactly that path at input rate. Whether that churn flickers is a
/// property of the display driver that only the live session can
/// answer — a nested backend has no planes — so the off switch ships as
/// configuration: set `CHONKSTEP_NO_CURSOR_PLANE=1`, restart the
/// session, and the pointer is composited into every frame instead
/// (costing pointer-motion recomposites, the trade [`FRAME_FLAGS`]
/// documents). If the flicker follows the switch, the cursor plane was
/// the culprit and the default deserves revisiting; if it stays, that
/// hypothesis is dead and the switch cost one experiment.
pub(crate) fn frame_flags() -> FrameFlags {
    let mut flags = FRAME_FLAGS;
    if crate::diagnostics::enabled("no-cursor-plane") {
        flags.remove(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT);
    }
    if crate::diagnostics::enabled("no-direct-scanout") {
        flags.remove(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT);
    }
    flags
}

/// The pure half of [`frame_flags`], split out so both environment
/// switches are testable without mutating process-global state. Unset
/// or `0` keeps that plane; anything else removes only its own flag.
#[cfg(test)]
fn configured_frame_flags(
    no_cursor: Option<std::ffi::OsString>,
    no_direct_scanout: Option<std::ffi::OsString>,
) -> FrameFlags {
    let mut flags = FRAME_FLAGS;
    if no_cursor.is_some_and(|value| value != "0") {
        flags.remove(FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT);
    }
    if no_direct_scanout.is_some_and(|value| value != "0") {
        flags.remove(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT);
    }
    flags
}

/// Whether a composited client buffer's release is deferred to the
/// completion of the page flip that sampled it
/// (`SessionOutput::pending_scene`). Direct-scanout buffers are always
/// held for as long as the display engine can still read them.
///
/// The race this closes: smithay signals a buffer's explicit-sync
/// release point (and sends `wl_buffer.release`) from the CPU the
/// moment the last reference to the buffer drops, and with the
/// per-frame element list dying at the end of the render pass, that
/// moment is "the client's next commit merged" — which can precede the
/// GPU actually executing the queued commands that sample the old
/// buffer. A client that honors explicit sync (Chromium ≥ its syncobj
/// default, confirmed live: Edge 152 sets acquire *and release* points
/// on every dmabuf commit) then starts rewriting the buffer while the
/// compositor's GPU is still reading it. On Mesa drivers the kernel's
/// implicit dmabuf fencing quietly serializes that write behind the
/// read, so nobody ever sees the early signal; NVIDIA's driver does no
/// implicit dmabuf fencing — the same property that made the original
/// flicker — so the write lands mid-read and the sampled frame shows
/// it: flicker that scales with how fast the client recommits, i.e.
/// hover-effect repaints.
///
/// Holding the frame's elements until the flip completes makes the
/// release honest: the atomic commit carried the render fence as its
/// in-fence (`EGL_ANDROID_native_fence_sync`, present on this driver),
/// so a completed flip proves the sampling commands retired. Where the
/// fence could not be exported, `render_frame_session` already waited
/// on the CPU before queueing, which proves the same thing earlier.
///
/// The default is on for NVIDIA only (`nvidia` in the DRM driver name)
/// because everywhere else the kernel provides the ordering for free
/// and holding buffers a frame longer would be pure overhead;
/// `CHONKSTEP_STRICT_BUFFER_RELEASE=1`/`0` overrides either way, so
/// the heuristic never needs a compiler to correct.
fn strict_release_configured(env: Option<std::ffi::OsString>, nvidia_driver: bool) -> bool {
    match env {
        Some(value) => value != "0",
        None => nvidia_driver,
    }
}

/// Whether the DRM device is driven by NVIDIA's kernel driver
/// (`nvidia-drm`) — the heuristic input to
/// [`strict_release_configured`]. A device whose driver cannot be read
/// answers `false`: the strict default exists to paper over one known
/// driver's missing implicit sync, and an unknown driver is far more
/// likely to be a VM or a fresh Mesa stack than an unlabeled NVIDIA.
fn driver_is_nvidia(fd: &DrmDeviceFd) -> bool {
    use smithay::reexports::drm::Device as _;
    fd.get_driver()
        .map(|driver| driver.name().to_string_lossy().to_ascii_lowercase().contains("nvidia"))
        .unwrap_or(false)
}

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
const DEVICE_FLAGS: OFlags = OFlags::RDWR.union(OFlags::CLOEXEC).union(OFlags::NOCTTY).union(OFlags::NONBLOCK);

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
/// Generous relative to the 100 ms maximum idle cadence, because the cost
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
pub(crate) fn full_damage_forced() -> bool {
    crate::diagnostics::enabled("full-damage")
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
    /// Primary KMS node selected for this session, retained for live
    /// system information instead of forcing a user to reconstruct it
    /// from startup-only logs.
    device_path: PathBuf,
    /// Kernel DRM driver name for the selected node.
    driver_name: String,
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
    /// The exact render node backing `renderer`, if it could be proved.
    /// This is deliberately not the KMS primary node: split display /
    /// render hardware has no render node on the display device, and
    /// publishing that primary node as a render device crashes clients
    /// such as xdg-desktop-portal-wlr. See [`render_node_for_fd`].
    render_node: Option<DrmNode>,
    /// Retained because a connector appearing after startup needs the
    /// same allocator and renderer-format intersection as the outputs
    /// built during `init`.
    gbm: GbmDevice<DrmDeviceFd>,
    render_formats: Vec<Format>,
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
    /// Whether client buffers are held until the page flip that sampled
    /// them completes — see [`strict_release_configured`] for the
    /// mechanism and the NVIDIA-shaped reason it exists.
    strict_release: bool,
    /// Debounced connector rescan deadline. A physical plug produces a
    /// burst of udev changes and every forced connector probe may block;
    /// one absolute deadline coalesces that burst into one walk.
    hotplug_due: Option<Instant>,
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
    /// Stable KMS identity used to diff a fresh connector enumeration
    /// without relying on vector positions that change on unplug.
    connector: connector::Handle,
    /// The scanout engine driving it. Always equal to
    /// `drm_compositor.crtc()`; kept alongside so the page-flip handler
    /// can find the right output without reaching into the compositor.
    crtc: crtc::Handle,
    /// This output's top-left corner in global compositor space — the
    /// viewport offset the renderer subtracts from every element to put
    /// the shared scene into this output's framebuffer.
    position: Point,
    /// User-visible scanout orientation. The framebuffer remains at
    /// the connector mode's physical pixel size.
    transform: Transform,
    /// Swapchain, mode setting, and page flips for this output.
    drm_compositor: SessionDrmCompositor,
    /// The kernel modes behind the wayland modes this output advertises,
    /// index-aligned with `OutputEntry::modes` for the same output —
    /// what a wlr-output-management `set_mode` resolves against.
    drm_modes: Vec<DrmMode>,
    /// Index of the mode currently programmed from `drm_modes`.
    mode_index: usize,
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
    /// Reusable scene-construction storage. Draining this into
    /// `pending_scene` preserves the allocation for the next frame
    /// while the latter owns any client buffers until vblank.
    scene_scratch: Vec<crate::renderer::SceneElement<GlesRenderer>>,
    /// The render elements of the frame whose page flip is in flight.
    /// Held for composited frames only while
    /// [`SessionGraphics::strict_release`] is on, and unconditionally
    /// when the frame directly scans out a client buffer.
    ///
    /// Each `WaylandSurfaceRenderElement` in here owns a clone of
    /// smithay's `Buffer` handle for the client buffer it sampled, and
    /// dropping the *last* clone is what sends `wl_buffer.release` and
    /// CPU-signals the buffer's explicit-sync release point
    /// (`InnerBuffer::drop` in smithay's `renderer/utils/wayland.rs`).
    /// Without this holder that last clone can die the moment a
    /// client's next commit merges — before the GPU has necessarily
    /// executed the sampling commands of the frame just queued — and a
    /// client that trusts the release (which explicit sync entitles it
    /// to) rewrites a buffer the compositor is still reading. See
    /// [`strict_release_configured`] for why only NVIDIA users see
    /// that race on the composition path. Cleared by the vblank
    /// handler once a composited flip completes; moved into
    /// `scanout_scene` instead when this commit makes a client buffer
    /// the display's continuing source.
    pending_scene: Vec<crate::renderer::SceneElement<GlesRenderer>>,
    /// The elements backing the primary plane currently being scanned
    /// out directly. A vblank makes an atomic commit *current*, not
    /// finished: the display engine keeps reading that buffer until a
    /// later flip replaces it. Keeping this scene separate from
    /// [`SessionOutput::pending_scene`] prevents `wl_buffer.release`
    /// (and explicit-sync release signaling) while the pixels are
    /// still physically on screen.
    scanout_scene: Vec<crate::renderer::SceneElement<GlesRenderer>>,
    /// Cached solely for transition telemetry; unlike inspecting the
    /// scene, this costs no walk in the vblank callback.
    direct_scanout_active: bool,
    /// Presentation requests drained for the page flip in flight.
    /// They are completed with the kernel's vblank timestamp, never
    /// merely when rendering finished.
    presentation: Option<OutputPresentationFeedback>,
    /// Most recent kernel vblank metadata for this crtc. An atomic
    /// commit that produces no flip can still complete presentation
    /// requests against this boundary without inventing a sequence.
    last_vblank: Option<(Duration, u64)>,
    refresh: Refresh,
    /// Predicts the next vblank and learns how much of the interval
    /// composition actually consumes. Rendering is armed late enough
    /// to sample fresh input while retaining a measured safety budget.
    frame_clock: FrameClock,
}

const INITIAL_RENDER_MEAN: Duration = Duration::from_millis(2);
const INITIAL_RENDER_DEVIATION: Duration = Duration::from_millis(1);
const BASE_RENDER_MARGIN: Duration = Duration::from_micros(1_500);
const MAX_RENDER_MARGIN: Duration = Duration::from_millis(8);

/// Per-output deadline estimator. All filtering is elapsed-time based,
/// so it behaves the same at 60 Hz and 144 Hz and naturally forgets a
/// stale sample after an idle gap.
#[derive(Clone, Copy, Debug)]
struct FrameClock {
    last_vblank: Option<Instant>,
    period: Option<Duration>,
    vblank_duration: Duration,
    mean: Duration,
    deviation: Duration,
    margin: Duration,
    last_sample: Option<Instant>,
    deadline: Option<Instant>,
    target_vblank: Option<Instant>,
}

impl FrameClock {
    fn new(mode: DrmMode) -> Self {
        Self {
            last_vblank: None,
            period: mode_period(mode),
            vblank_duration: mode_vblank_duration(mode),
            mean: INITIAL_RENDER_MEAN,
            deviation: INITIAL_RENDER_DEVIATION,
            margin: BASE_RENDER_MARGIN,
            last_sample: None,
            deadline: None,
            target_vblank: None,
        }
    }

    #[cfg(test)]
    fn for_period(period: Duration) -> Self {
        Self {
            last_vblank: None,
            period: Some(period),
            vblank_duration: Duration::ZERO,
            mean: INITIAL_RENDER_MEAN,
            deviation: INITIAL_RENDER_DEVIATION,
            margin: BASE_RENDER_MARGIN,
            last_sample: None,
            deadline: None,
            target_vblank: None,
        }
    }

    fn update_mode(&mut self, mode: DrmMode) {
        self.period = mode_period(mode);
        self.vblank_duration = mode_vblank_duration(mode);
        self.last_vblank = None;
        self.deadline = None;
        self.target_vblank = None;
    }

    fn note_vblank(&mut self, at: Instant) {
        self.last_vblank = Some(at);
        self.deadline = None;
        self.target_vblank = None;
    }

    fn disarm(&mut self) {
        self.last_vblank = None;
        self.deadline = None;
        self.target_vblank = None;
    }

    fn pessimistic_budget(&self) -> Duration {
        self.mean
            .saturating_add(self.deviation.saturating_mul(2))
            .saturating_add(self.margin)
            .saturating_add(self.vblank_duration)
    }

    fn optimistic_budget(&self) -> Duration {
        self.mean.saturating_add(self.margin).saturating_add(self.vblank_duration)
    }

    /// Arms this frame once and returns its absolute deadline. The
    /// first frame and the first frame after an idle gap remain
    /// immediate; deadline scheduling is only useful on a live cadence.
    fn arm(&mut self, now: Instant) -> Instant {
        if let Some(deadline) = self.deadline {
            return deadline;
        }
        let Some(period) = self.period.filter(|period| !period.is_zero()) else {
            self.deadline = Some(now);
            return now;
        };
        let Some(last) = self.last_vblank else {
            self.deadline = Some(now);
            return now;
        };
        if now.saturating_duration_since(last) > period.saturating_mul(3) {
            self.deadline = Some(now);
            return now;
        }

        let mut target = last + period;
        while target <= now {
            target += period;
        }
        let pessimistic = target.checked_sub(self.pessimistic_budget()).unwrap_or(now);
        let optimistic = target.checked_sub(self.optimistic_budget()).unwrap_or(now);
        let deadline = if pessimistic > now {
            pessimistic
        } else if optimistic > now {
            optimistic
        } else {
            target += period;
            target.checked_sub(self.pessimistic_budget()).unwrap_or(now)
        };
        self.target_vblank = Some(target);
        self.deadline = Some(deadline);
        deadline
    }

    fn observe_render(&mut self, sample: Duration, finished: Instant) {
        let elapsed = self
            .last_sample
            .map(|previous| finished.saturating_duration_since(previous))
            .unwrap_or(Duration::from_millis(250));
        self.last_sample = Some(finished);

        let seconds = elapsed.as_secs_f64();
        let mean_alpha = 1.0 - (-seconds / 0.35).exp();
        let decay_alpha = 1.0 - (-seconds / 3.0).exp();
        let mean = self.mean.as_secs_f64();
        let sample_secs = sample.as_secs_f64();
        self.mean = Duration::from_secs_f64((mean + (sample_secs - mean) * mean_alpha).max(0.000_001));
        let overshoot = sample.saturating_sub(self.mean);
        if !overshoot.is_zero() {
            self.deviation = self.deviation.max(overshoot);
        } else {
            self.deviation = Duration::from_secs_f64(
                self.deviation.as_secs_f64() * (1.0 - decay_alpha),
            );
        }

        if self.target_vblank.is_some_and(|target| finished > target) {
            let overrun = self.target_vblank.map(|target| finished.duration_since(target)).unwrap_or_default();
            self.margin = self.margin.saturating_add(overrun.max(Duration::from_micros(250))).min(MAX_RENDER_MARGIN);
        } else {
            let margin = self.margin.as_secs_f64();
            let base = BASE_RENDER_MARGIN.as_secs_f64();
            self.margin = Duration::from_secs_f64((margin + (base - margin) * decay_alpha).max(base));
        }
        self.deadline = None;
        self.target_vblank = None;
    }
}

fn mode_period(mode: DrmMode) -> Option<Duration> {
    let millihertz = OutputMode::from(mode).refresh;
    (millihertz > 0).then(|| Duration::from_nanos(1_000_000_000_000 / millihertz as u64))
}

fn mode_vblank_duration(mode: DrmMode) -> Duration {
    let (_, vdisplay) = mode.size();
    let (_, _, htotal) = mode.hsync();
    let (_, _, vtotal) = mode.vsync();
    let clock_khz = u64::from(mode.clock());
    if clock_khz == 0 || vtotal <= vdisplay || htotal == 0 {
        return Duration::ZERO;
    }
    let mut micros = (u64::from(vtotal - vdisplay) * u64::from(htotal) * 1_000)
        .div_ceil(clock_khz);
    if mode.flags().contains(ModeFlags::DBLSCAN) {
        micros = micros.saturating_mul(2);
    }
    Duration::from_micros(micros)
}

fn drm_monotonic_instant(timestamp: Duration) -> Instant {
    let now = Instant::now();
    let clock_now = Duration::from_micros(Clock::<Monotonic>::new().now().as_micros());
    now.checked_sub(clock_now.saturating_sub(timestamp)).unwrap_or(now)
}

/// A page flip the kernel has accepted and not yet reported back.
struct PendingFlip {
    queued_at: Instant,
    /// Whether this commit put a client buffer, rather than a
    /// compositor swapchain buffer, on the primary plane.
    direct_scanout: bool,
    /// Whether this flip has already been named in the log as stalled,
    /// so [`FLIP_STALL_WARNING`] produces one line per stuck frame
    /// rather than one per event-loop wakeup.
    stall_reported: bool,
}

/// Advances client-buffer ownership at a completed page flip.
///
/// `pending` belongs to the commit that just became current. A direct
/// primary-plane buffer moves into `scanout` because the display keeps
/// reading it after vblank; a composited commit replaces any previous
/// direct buffer with the compositor's swapchain and can release both
/// scene holds. Returns whether direct scanout changed state, for one
/// transition log rather than one line per frame.
fn complete_scene_flip<T>(pending: &mut Vec<T>, scanout: &mut Vec<T>, active: &mut bool, direct: bool) -> bool {
    if direct {
        // Rotate rather than assign: the scene which has just stopped
        // being scanned out can release its element handles while its
        // allocation becomes the next pending-frame buffer. Together
        // with `scene_scratch`, this forms a three-vector ring matching
        // the three possible owners (building, in flight, on screen).
        let completed = std::mem::take(pending);
        let mut retired = std::mem::replace(scanout, completed);
        retired.clear();
        *pending = retired;
    } else {
        pending.clear();
        scanout.clear();
    }
    let changed = *active != direct;
    *active = direct;
    changed
}

/// Releases every scene retained on behalf of a device whose planes
/// were disabled or reset. Once no crtc can read the buffers, keeping
/// either the in-flight or current scene would only delay clients.
fn clear_scene_holds<T>(pending: &mut Vec<T>, scanout: &mut Vec<T>, active: &mut bool) {
    pending.clear();
    scanout.clear();
    *active = false;
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

    /// The actual render node backing this session's EGL renderer.
    /// Resolved once during initialization and absent rather than guessed.
    pub(crate) fn render_node(&self) -> Option<DrmNode> {
        self.render_node
    }

    /// Formats a client can allocate for the conservative primary-plane
    /// direct-scanout path on *every* active output. The default feedback
    /// is global rather than per-surface, so advertising the intersection
    /// is the only claim that stays true when a window moves monitors.
    pub(crate) fn direct_scanout_formats(&self) -> FormatSet {
        if !frame_flags().contains(FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT) {
            return FormatSet::default();
        }
        common_scanout_formats(
            self.outputs
                .iter()
                .map(|output| (output.drm_compositor.format(), output.drm_compositor.modifiers())),
        )
    }
}

/// Stable, user-facing identity of the graphics backend currently
/// driving the compositor. Renderer details that Smithay does not
/// expose remain in its startup log; the KMS node, kernel driver and
/// render node are retained here for live bug reports.
pub(crate) fn graphics_diagnostics(graphics: &Graphics) -> String {
    match graphics {
        Graphics::Winit(_) => "backend=nested-winit renderer=GLES host_output=true".to_string(),
        Graphics::Session(session) => format!(
            "backend=drm-session kms_device={} drm_driver={} render_node={}",
            session.device_path.display(),
            session.driver_name,
            session
                .render_node
                .map_or_else(|| "unknown".to_string(), |node| node.to_string()),
        ),
    }
}

/// Intersects exact format/modifier pairs across outputs for global
/// linux-dmabuf feedback. Kept pure both for tests and to make the
/// deliberately conservative multi-monitor promise unmistakable.
fn common_scanout_formats<'a>(
    mut outputs: impl Iterator<Item = (Fourcc, &'a [Modifier])>,
) -> FormatSet {
    let Some((first_code, first_modifiers)) = outputs.next() else {
        return FormatSet::default();
    };
    let mut common: Vec<Format> = first_modifiers
        .iter()
        .copied()
        .map(|modifier| Format { code: first_code, modifier })
        .collect();
    for (code, modifiers) in outputs {
        common.retain(|format| format.code == code && modifiers.contains(&format.modifier));
    }
    common.into_iter().collect()
}

/// Resolves a render node paired with a KMS fd.
///
/// There is intentionally no primary-node fallback. A DRM primary is
/// not a render node, and on split display/render hardware the GPU that
/// actually rendered the buffers can be another DRM device entirely.
/// `None` disables direct client-buffer scanout and, if EGL cannot name
/// its own device either, linux-dmabuf feedback; that is slower but true.
fn render_node_for_fd(fd: &DrmDeviceFd) -> Option<DrmNode> {
    let primary = DrmNode::from_file(fd).ok()?;
    primary.node_with_type(NodeType::Render).and_then(Result::ok)
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
    // SAFETY: `gbm` owns a live GBM device for the selected DRM node and
    // outlives the display; Smithay performs the EGL handle validation and
    // returns an error if this platform cannot construct the display.
    let egl_display = unsafe { EGLDisplay::new(gbm.clone()) }
        .map_err(|error| format!("EGL display init failed on {}: {error}", device_path.display()))?;
    let egl_context = EGLContext::new(&egl_display)
        .map_err(|error| format!("EGL context creation failed on {}: {error}", device_path.display()))?;
    // SAFETY: the context was just created on this thread, is not
    // current anywhere else, and `GlesRenderer` takes ownership of it —
    // the conditions its `new` documents.
    let renderer = unsafe { GlesRenderer::new(egl_context) }
        .map_err(|error| format!("GLES renderer init failed on {}: {error}", device_path.display()))?;
    // EGL knows the renderer, which may be a different DRM device from
    // the KMS display controller (kmsro on split hardware). Prefer that
    // exact identity. If the optional EGL query extension is absent,
    // pairing the KMS node is safe only when a real render node exists;
    // `render_node_for_fd` deliberately declines a primary-node guess.
    let render_node = crate::dmabuf::render_node_for_renderer(&renderer)
        .or_else(|| render_node_for_fd(drm.device_fd()));
    match render_node {
        Some(node) => tracing::info!(render_node = %node, "session backend: renderer device identified"),
        None => tracing::warn!(
            kms_device = %device_path.display(),
            "session backend: renderer has no identifiable render node; linux-dmabuf and direct client-buffer scanout will be disabled"
        ),
    }
    // Which dmabuf formats the renderer can draw into; intersected
    // against the primary plane's formats by `DrmCompositor::new` to
    // choose the swapchain format. Collected eagerly so the borrow of
    // the renderer ends here.
    let render_formats: Vec<Format> = renderer.egl_context().dmabuf_render_formats().iter().copied().collect();

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
        match attach_output(&mut drm, &gbm, render_node, &render_formats, &target, position) {
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
        return Err(
            format!("no output could be brought up on {} ({})", device_path.display(), failures.join("; ")).into()
        );
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
        .insert_source(drm_notifier, |event, metadata, comp: &mut Compositor| {
            let Compositor {
                graphics,
                wm,
                outputs,
                cursor_status,
                pointer_location,
                start_time,
                ..
            } = comp;
            let Graphics::Session(session) = graphics else {
                return;
            };
            match event {
                DrmEvent::VBlank(crtc) => {
                    // Per crtc: the device hands every output's flip
                    // completion through this one source, and only the
                    // output that owns that crtc is free to draw again.
                    let Some(output_index) = session.outputs.iter().position(|output| output.crtc == crtc)
                    else {
                        tracing::debug!(?crtc, "page flip completed on a crtc we do not drive");
                        return;
                    };
                    let completed_frame = {
                        let output = &mut session.outputs[output_index];
                        let vblank_at = match metadata.map(|meta| meta.time) {
                            Some(DrmEventTime::Monotonic(time)) => drm_monotonic_instant(time),
                            _ => Instant::now(),
                        };
                        output.frame_clock.note_vblank(vblank_at);
                        let pending = output.frame_pending.take();
                        let completed_frame = pending.is_some();
                        let monotonic_metadata = metadata.and_then(|meta| match meta.time {
                            DrmEventTime::Monotonic(time) => Some((time, meta.sequence as u64)),
                            _ => None,
                        });
                        if let Some(last_vblank) = monotonic_metadata {
                            output.last_vblank = Some(last_vblank);
                        }
                        if let Some(mut feedback) = output.presentation.take() {
                            let hw_flags = smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync
                                | smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::HwClock
                                | smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::HwCompletion;
                            match monotonic_metadata {
                                Some((time, sequence)) => {
                                    crate::renderer::present_at(
                                        &mut feedback,
                                        time,
                                        output.refresh,
                                        sequence,
                                        hw_flags,
                                    );
                                }
                                _ => crate::renderer::present_now(
                                    &mut feedback,
                                    output.refresh,
                                    smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync
                                        | smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::HwCompletion,
                                ),
                            }
                        }
                        // A composited frame can release its sampled
                        // buffers now. A direct-scanout frame cannot:
                        // this vblank made its client buffer current.
                        if let Some(pending) = pending {
                            let changed = complete_scene_flip(
                                &mut output.pending_scene,
                                &mut output.scanout_scene,
                                &mut output.direct_scanout_active,
                                pending.direct_scanout,
                            );
                            if changed {
                                tracing::info!(
                                    output = %output.name,
                                    active = pending.direct_scanout,
                                    "DRM primary-plane direct scanout changed"
                                );
                            }
                        }
                        if let Err(error) = output.drm_compositor.frame_submitted() {
                            tracing::warn!(?error, output = %output.name, "page-flip completion was rejected by the DRM compositor");
                        }
                        completed_frame
                    };
                    if completed_frame {
                        crate::renderer::note_frame_success();
                        if let (Some(entry), Some(monitor)) =
                            (outputs.get(output_index), wm.backend().monitors.get(output_index))
                        {
                            crate::renderer::send_frame_callbacks(
                                wm.backend(),
                                &entry.output,
                                monitor.geometry,
                                cursor_status,
                                *pointer_location,
                                start_time.elapsed(),
                            );
                        }
                    }
                }
                DrmEvent::Error(error) => {
                    tracing::error!(?error, "the DRM device reported an error");
                }
            }
        })
        .map_err(|error| format!("failed to register the DRM event source: {error}"))?;

    // udev. Connector changes on the device we own arm one debounced
    // rescan; GPU add/remove remains outside this single-device backend.
    let udev_backend =
        UdevBackend::new(&seat_name).map_err(|error| format!("could not watch udev for seat {seat_name}: {error}"))?;
    loop_handle
        .insert_source(udev_backend, |event, _, comp: &mut Compositor| match event {
            UdevEvent::Added { device_id, path } => {
                tracing::info!(?device_id, path = %path.display(), "a DRM device appeared; chonkstep drives a single GPU and will not adopt it");
            }
            UdevEvent::Changed { device_id } => {
                let Graphics::Session(session) = &mut comp.graphics else { return };
                if session.drm.device_id() == device_id {
                    session.hotplug_due = Some(Instant::now() + Duration::from_millis(120));
                    tracing::debug!(?device_id, "connector change noticed; debounced rescan armed");
                }
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
    let mut libinput = Libinput::new_with_udev::<LibinputSessionInterface<LibSeatSession>>(seat_session.clone().into());
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
            match event {
                SessionEvent::PauseSession => {
                    let Graphics::Session(session) = &mut comp.graphics else {
                        return;
                    };
                    tracing::info!("session paused: releasing the DRM device and input");
                    session.libinput.suspend();
                    session.drm.pause();
                    // A flip queued before the pause will never complete
                    // — the device is gone — so dropping it keeps the
                    // render path from waiting forever for a vblank that
                    // cannot arrive. Every output's, because the device
                    // going away takes all of them. The held scenes go
                    // with them: their vblank is never coming either,
                    // and the foreign session about to own the GPU has
                    // preempted whatever sampling was still queued.
                    for output in session.outputs.iter_mut() {
                        output.frame_pending = None;
                        output.last_vblank = None;
                        output.frame_clock.disarm();
                        clear_scene_holds(
                            &mut output.pending_scene,
                            &mut output.scanout_scene,
                            &mut output.direct_scanout_active,
                        );
                    }
                }
                SessionEvent::ActivateSession => {
                    tracing::info!("session resumed: reclaiming the DRM device and input");
                    // Any button or key held when the seat was taken
                    // away may have released into the other session.
                    // Reconcile routing, xkb modifiers, client-visible
                    // presses, binding repeat and the Alt-Tab modal grab
                    // before accepting fresh libinput events.
                    crate::input::resynchronise_input_after_resume(comp);
                    let Compositor { graphics, wm, gamma, .. } = comp;
                    let Graphics::Session(session) = graphics else {
                        return;
                    };
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
                    // The connector set may have changed while udev and the
                    // DRM fd were suspended. Reconcile once the fd is usable
                    // again even if no hotplug event survived the VT switch.
                    session.hotplug_due = Some(Instant::now() + Duration::from_millis(120));
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
                        output.last_vblank = None;
                        clear_scene_holds(
                            &mut output.pending_scene,
                            &mut output.scanout_scene,
                            &mut output.direct_scanout_active,
                        );
                    }
                    // Marks every output dirty on the next render pass
                    // (see `render_frame_session`), which is what
                    // repaints the screens the other session scribbled
                    // on.
                    wm.backend_mut().mark_damaged();
                    // The other session's `activate(true)` reset every
                    // crtc, and a crtc reset comes back with a linear
                    // LUT — so a night-light daemon's warm screen would
                    // silently go blue-white on the way back from a VT
                    // switch, and stay that way until the daemon next
                    // felt like re-sending. The flag makes
                    // `gamma::refresh` program the ramp that is still
                    // logically in force; the ioctl waits for the pass,
                    // not for this callback.
                    crate::gamma::note_session_resumed(gamma);
                }
            }
        })
        .map_err(|error| format!("failed to register the seat session source: {error}"))?;

    // 6. Hand the assembled stack back to `run`, which registers an
    //    output global per output and builds the damage trackers from
    //    them.
    let strict_release = strict_release_configured(
        std::env::var_os("CHONKSTEP_STRICT_BUFFER_RELEASE"),
        driver_is_nvidia(drm.device_fd()),
    );
    let driver_name = {
        use smithay::reexports::drm::Device as _;
        drm.device_fd()
            .get_driver()
            .map(|driver| driver.name().to_string_lossy().into_owned())
            .unwrap_or_else(|_| "unknown".to_string())
    };
    tracing::info!(
        enabled = strict_release,
        "strict buffer release (hold client buffers until their page flip completes; \
         CHONKSTEP_STRICT_BUFFER_RELEASE overrides)"
    );
    Ok(SessionInit {
        graphics: Graphics::Session(Box::new(SessionGraphics {
            device_path,
            driver_name,
            seat_session,
            drm,
            renderer,
            render_node,
            gbm,
            render_formats,
            outputs: session_outputs,
            libinput,
            last_service: Instant::now(),
            strict_release,
            hotplug_due: None,
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
    render_node: Option<DrmNode>,
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
    let identity = connector_identity(drm.device_fd(), info.handle());
    let make = identity.as_ref().map(|identity| identity.make.clone()).unwrap_or_else(|| "Unknown".into());
    let model = identity.as_ref().map(|identity| identity.model.clone()).unwrap_or_else(|| name.clone());
    let serial = identity.as_ref().map(|identity| identity.serial.clone()).unwrap_or_default();
    let description = identity.as_ref().map(DisplayIdentity::description);
    let output = Output::new(
        name.clone(),
        PhysicalProperties {
            size: (physical_w as i32, physical_h as i32).into(),
            subpixel: info.subpixel().into(),
            make,
            model,
        },
    );
    output.set_preferred(wl_mode);
    output.change_current_state(Some(wl_mode), Some(Transform::Normal), None, Some((position.x, position.y).into()));
    let size = Size::new(wl_mode.size.w.max(0) as u32, wl_mode.size.h.max(0) as u32);
    // The connector's full mode list, current-first and deduplicated by
    // (size, refresh): what wlr-output-management enumerates, and the
    // catalogue `apply_mode` matches a requested mode against. Kernel
    // order otherwise — the EDID's own preference ranking.
    let mut modes: Vec<OutputMode> = vec![wl_mode];
    let mut drm_modes: Vec<DrmMode> = vec![*mode];
    for candidate in info.modes() {
        let candidate_wl = OutputMode::from(*candidate);
        if !modes.iter().any(|held| held.size == candidate_wl.size && held.refresh == candidate_wl.refresh) {
            modes.push(candidate_wl);
            drm_modes.push(*candidate);
        }
    }

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
    // Filter client buffers against the renderer's exact GPU before
    // even trying direct scanout. Passing no import node disables that
    // path in Smithay. On a split display/render system this is the
    // renderer's node, not the display-only KMS primary.
    let framebuffer_exporter = GbmFramebufferExporter::new(gbm.clone(), render_node);
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
        if let Err(error) =
            drm.device_fd().set_client_capability(smithay::reexports::drm::ClientCapability::UniversalPlanes, true)
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
        // used is decided per frame by `frame_flags`: cursor and the
        // conservative, exact-format primary path by default.
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
            connector: info.handle(),
            crtc: *crtc,
            position,
            transform: Transform::Normal,
            drm_compositor,
            drm_modes,
            mode_index: 0,
            frame_pending: None,
            // Nothing has ever been drawn on it.
            dirty: true,
            scene_scratch: Vec::new(),
            pending_scene: Vec::new(),
            scanout_scene: Vec::new(),
            direct_scanout_active: false,
            presentation: None,
            last_vblank: None,
            refresh: if wl_mode.refresh > 0 {
                Refresh::fixed(Duration::from_nanos(1_000_000_000_000 / wl_mode.refresh as u64))
            } else {
                Refresh::Unknown
            },
            frame_clock: FrameClock::new(*mode),
        },
        OutputSetup {
            output,
            identity: description,
            serial,
            position,
            size,
            transform: Transform::Normal,
            requested_mode: None,
            modes,
        },
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
/// outputs per housekeeping wakeup while a flip is outstanding, each a
/// `Duration` comparison per output. A completion on the DRM fd wakes
/// the loop directly; this poll is only the lost-completion backstop.
pub(crate) fn redraw_pending(graphics: &Graphics) -> bool {
    match graphics {
        Graphics::Winit(_) => false,
        Graphics::Session(session) => {
            session.outputs.iter().any(|output| output.dirty || output.frame_pending.is_some())
        }
    }
}

/// Earliest armed render deadline, folded into the event loop's wait.
/// An output first acquires a deadline when `render_frame_session`
/// consumes damage; after that this wakes calloop at the exact absolute
/// instant instead of depending on the shell's housekeeping cadence.
pub(crate) fn next_render_deadline(graphics: &Graphics) -> Option<Instant> {
    let Graphics::Session(session) = graphics else {
        return None;
    };
    if !session.drm.is_active() {
        return None;
    }
    session
        .outputs
        .iter()
        .filter(|output| output.dirty && output.frame_pending.is_none())
        .filter_map(|output| output.frame_clock.deadline)
        .min()
}

/// Deadline of a pending connector rescan, for the event-loop wait.
pub(crate) fn next_hotplug_deadline(graphics: &Graphics) -> Option<Instant> {
    let Graphics::Session(session) = graphics else {
        return None;
    };
    session.hotplug_due
}

/// Performs one due connector rescan and hands the structural delta to
/// the compositor ledger. The forced KMS probes stay off the udev
/// callback itself, so a burst is coalesced before any potentially slow
/// connector query runs.
pub(crate) fn service_connector_hotplug(comp: &mut Compositor) {
    let now = Instant::now();
    let Graphics::Session(session) = &mut comp.graphics else {
        return;
    };
    if !session.hotplug_due.is_some_and(|deadline| deadline <= now) {
        return;
    }
    session.hotplug_due = None;
    if !session.drm.is_active() {
        // Resume re-arms a scan below; probing a revoked fd only creates
        // noise and can block in the session implementation.
        session.hotplug_due = Some(now + Duration::from_millis(120));
        return;
    }

    match rescan_session_outputs(session) {
        Ok((removed, added)) if removed.is_empty() && added.is_empty() => {
            tracing::debug!("connector rescan found no output changes");
        }
        Ok((removed, added)) => crate::state::apply_connector_hotplug(comp, &removed, added),
        Err(error) => tracing::warn!(%error, "connector rescan failed; keeping the current output set"),
    }
}

fn rescan_session_outputs(session: &mut SessionGraphics) -> Result<(Vec<usize>, Vec<OutputSetup>), String> {
    let resources = session
        .drm
        .resource_handles()
        .map_err(|error| format!("could not enumerate KMS resources: {error}"))?;
    let mut connected = Vec::new();
    for handle in resources.connectors() {
        match session.drm.get_connector(*handle, true) {
            Ok(info) if info.state() == connector::State::Connected && !info.modes().is_empty() => {
                connected.push(info);
            }
            Ok(_) => {}
            Err(error) => tracing::debug!(?handle, ?error, "connector probe failed during hotplug rescan"),
        }
    }

    let connected_handles: Vec<connector::Handle> = connected.iter().map(connector::Info::handle).collect();
    let mut removed: Vec<usize> = session
        .outputs
        .iter()
        .enumerate()
        .filter_map(|(index, output)| (!connected_handles.contains(&output.connector)).then_some(index))
        .collect();
    removed.sort_unstable_by(|a, b| b.cmp(a));
    for index in &removed {
        let mut output = session.outputs.remove(*index);
        if let Some(mut feedback) = output.presentation.take() {
            feedback.discarded();
        }
        tracing::info!(output = %output.name, "connector unplugged; output removed from scanout");
    }

    let mut next_x = session
        .outputs
        .iter()
        .map(|output| {
            let width = output.drm_modes.first().map(|mode| mode.size().0 as i32).unwrap_or(0);
            output.position.x.saturating_add(width)
        })
        .max()
        .unwrap_or(0);
    let mut added = Vec::new();
    for info in connected {
        if session.outputs.iter().any(|output| output.connector == info.handle()) {
            continue;
        }
        let Some(mode) = preferred_mode(&info) else { continue };
        let taken: Vec<crtc::Handle> = session.outputs.iter().map(|output| output.crtc).collect();
        let Some(crtc) = crtc_for(&session.drm, &resources, &info, &taken) else {
            tracing::warn!(output = %connector_name(&info), "hot-plugged connector has no free crtc");
            continue;
        };
        let target = ConnectorTarget { info, crtc, mode };
        let position = Point::new(next_x, 0);
        match attach_output(
            &mut session.drm,
            &session.gbm,
            session.render_node,
            &session.render_formats,
            &target,
            position,
        ) {
            Ok((output, setup)) => {
                next_x = next_x.saturating_add(setup.size.w as i32);
                tracing::info!(output = %output.name, x = position.x, "hot-plugged connector adopted");
                session.outputs.push(output);
                added.push(setup);
            }
            Err(error) => tracing::warn!(output = %connector_name(&target.info), %error, "could not adopt hot-plugged connector"),
        }
    }
    Ok((removed, added))
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
        output.last_vblank = None;
        // The abandoned pending scene and the buffer that was current
        // before the reset both go: reset_state disabled the planes,
        // so the display engine can no longer read either.
        clear_scene_holds(
            &mut output.pending_scene,
            &mut output.scanout_scene,
            &mut output.direct_scanout_active,
        );
        if let Some(mut feedback) = output.presentation.take() {
            feedback.discarded();
        }
        output.dirty = true;
    }
    true
}

/// Whether a mode change to `mode_index` on output `index` could be
/// honored on this graphics stack. The nested backend's output is the
/// host window — it has exactly one mode and keeps it; the session
/// backend can set any mode the connector advertised.
pub(crate) fn mode_is_applicable(graphics: &Graphics, index: usize, mode_index: usize) -> bool {
    match graphics {
        Graphics::Winit(_) => mode_index == 0,
        Graphics::Session(session) => {
            session.outputs.get(index).is_some_and(|output| mode_index < output.drm_modes.len())
        }
    }
}

/// Re-programs one output's crtc to the mode at `mode_index` in its
/// advertised list — the real half of a wlr-output-management mode
/// change. The `DrmCompositor` performs the modeset on its next frame;
/// the static mode source it renders against is retuned here so the
/// swapchain and damage arithmetic follow the new size (see
/// `attach_output` for why the source is static in the first place).
pub(crate) fn apply_mode(graphics: &mut Graphics, index: usize, mode_index: usize) -> Result<(), String> {
    match graphics {
        Graphics::Winit(_) => {
            if mode_index == 0 {
                Ok(())
            } else {
                Err("the nested output is the host window and keeps its size".into())
            }
        }
        Graphics::Session(session) => {
            let output = session.outputs.get_mut(index).ok_or_else(|| format!("no session output at index {index}"))?;
            let mode =
                *output.drm_modes.get(mode_index).ok_or_else(|| format!("no mode {mode_index} on {}", output.name))?;
            output.drm_compositor.use_mode(mode).map_err(|error| format!("modeset refused: {error}"))?;
            let size = smithay::utils::Size::from((mode.size().0 as i32, mode.size().1 as i32));
            output.drm_compositor.set_output_mode_source(OutputModeSource::Static {
                size,
                scale: smithay::utils::Scale::from(1.0),
                transform: output.transform,
            });
            output.mode_index = mode_index;
            let millihertz = OutputMode::from(mode).refresh;
            output.refresh = if millihertz > 0 {
                Refresh::fixed(Duration::from_nanos(1_000_000_000_000 / millihertz as u64))
            } else {
                Refresh::Unknown
            };
            output.frame_clock.update_mode(mode);
            output.dirty = true;
            Ok(())
        }
    }
}

/// Changes one hardware output's user-visible rotation while keeping
/// the KMS framebuffer at the connector mode's physical size. The
/// renderer maps the transformed logical viewport into that buffer.
pub(crate) fn apply_transform(graphics: &mut Graphics, index: usize, transform: Transform) -> Result<(), String> {
    match graphics {
        Graphics::Winit(_) => {
            if transform == Transform::Normal {
                Ok(())
            } else {
                Err("the nested output reserves its transform for the host EGL surface".into())
            }
        }
        Graphics::Session(session) => {
            let output = session.outputs.get_mut(index).ok_or_else(|| format!("no session output at index {index}"))?;
            let mode = *output
                .drm_modes
                .get(output.mode_index)
                .ok_or_else(|| format!("current mode is missing on {}", output.name))?;
            let size = smithay::utils::Size::from((mode.size().0 as i32, mode.size().1 as i32));
            output.drm_compositor.set_output_mode_source(OutputModeSource::Static {
                size,
                scale: smithay::utils::Scale::from(1.0),
                transform,
            });
            output.transform = transform;
            output.dirty = true;
            Ok(())
        }
    }
}

/// Applies mode/rotation decisions made from startup monitor rules
/// before the corresponding globals become visible to clients.
pub(crate) fn apply_output_setups(graphics: &mut Graphics, setups: &mut [OutputSetup]) {
    for (index, setup) in setups.iter_mut().enumerate() {
        let requested_transform = setup.transform;
        let result = setup
            .requested_mode
            .map_or(Ok(()), |mode_index| apply_mode(graphics, index, mode_index))
            .and_then(|()| apply_transform(graphics, index, requested_transform));
        if let Err(error) = result {
            tracing::warn!(
                %error,
                output = %setup.output.name(),
                "startup output configuration was refused; keeping the active mode and orientation"
            );
            setup.requested_mode = None;
            setup.transform = Transform::Normal;
            if let Some(mode) = setup.output.current_mode() {
                setup.size = Size::new(mode.size.w.max(0) as u32, mode.size.h.max(0) as u32);
            }
            continue;
        }
        if matches!(graphics, Graphics::Session(_)) {
            let mode = setup
                .requested_mode
                .and_then(|mode_index| setup.modes.get(mode_index).copied());
            setup.output.change_current_state(
                mode,
                Some(requested_transform),
                None,
                Some((setup.position.x, setup.position.y).into()),
            );
        }
    }
}

/// The gamma ramp length each output's hardware wants, index-aligned
/// with `Compositor::outputs` — the number `zwlr_gamma_control_v1`
/// advertises as `gamma_size`, and the number of entries a client's
/// table must contain per channel.
///
/// Zero means "this output cannot have its gamma set", and that answer
/// is given twice over: the nested backend has no crtc to program at
/// all, and a real crtc whose `gamma_length` the kernel reports as zero
/// is hardware with no LUT. `gamma.rs` turns a zero into the protocol's
/// `failed` rather than into a claim it cannot honor — and an all-zero
/// list into no global at all.
pub(crate) fn gamma_ramp_sizes(graphics: &Graphics) -> Vec<u32> {
    let Graphics::Session(session) = graphics else {
        // The nested output is a window on somebody else's desktop.
        // There is no scanout engine behind it and no LUT to program;
        // see `gamma.rs` for why that is answered with an absent global
        // rather than a silent no-op.
        return vec![0];
    };
    session
        .outputs
        .iter()
        .map(|output| match session.drm.device_fd().get_crtc(output.crtc) {
            Ok(info) => {
                if info.gamma_length() == 0 {
                    tracing::info!(
                        output = %output.name,
                        "this crtc reports no gamma ramp; night-light tools will be told so"
                    );
                }
                info.gamma_length()
            }
            Err(error) => {
                tracing::warn!(
                    output = %output.name,
                    ?error,
                    "could not read this crtc's gamma length; treating the output as uncontrollable"
                );
                0
            }
        })
        .collect()
}

/// Reads back the ramp currently programmed on one output's crtc, so
/// `gamma.rs` has something exact to restore a crashed night-light
/// daemon's screen to.
///
/// `None` when the driver will not answer — some do not implement
/// `GETGAMMA` even when they accept `SETGAMMA` — and the caller falls
/// back to a linear ramp, which is what an untouched LUT holds.
pub(crate) fn read_gamma(graphics: &Graphics, index: usize, size: usize) -> Option<(Vec<u16>, Vec<u16>, Vec<u16>)> {
    let Graphics::Session(session) = graphics else {
        return None;
    };
    let output = session.outputs.get(index)?;
    let mut red = vec![0u16; size];
    let mut green = vec![0u16; size];
    let mut blue = vec![0u16; size];
    match session.drm.device_fd().get_gamma(output.crtc, &mut red, &mut green, &mut blue) {
        Ok(()) => Some((red, green, blue)),
        Err(error) => {
            tracing::info!(output = %output.name, ?error, "could not read the crtc's gamma ramp back");
            None
        }
    }
}

/// Programs one output's crtc gamma LUT — the real half of every
/// `set_gamma` and every restore.
///
/// The legacy `DRM_IOCTL_MODE_SETGAMMA` ioctl, for the reasons
/// `gamma.rs`'s header sets out at length: the `drm` crate exposes it
/// as one call on the control device, whereas the atomic `GAMMA_LUT`
/// property would mean committing behind the `DrmCompositor` that owns
/// every other atomic commit on this crtc. On an atomic driver the
/// kernel routes this ioctl into the same atomic state the property
/// would have set, and smithay's own commits never mention `GAMMA_LUT`,
/// so the value survives every frame queued afterwards.
///
/// Called only from `gamma::refresh`, once per dispatch pass, because
/// the kernel's legacy-gamma helper performs a *blocking* commit and
/// can wait about a vblank for a page flip already in flight here.
pub(crate) fn write_gamma(
    graphics: &Graphics,
    index: usize,
    red: &[u16],
    green: &[u16],
    blue: &[u16],
) -> Result<(), String> {
    let Graphics::Session(session) = graphics else {
        return Err("the nested backend has no crtc to program a gamma ramp on".into());
    };
    let output = session.outputs.get(index).ok_or_else(|| format!("no session output at index {index}"))?;
    session
        .drm
        .device_fd()
        .set_gamma(output.crtc, red, green, blue)
        .map_err(|error| format!("the crtc refused the gamma ramp: {error}"))
}

/// Mirrors the (position, size) layout wlr-output-management just
/// applied onto the session backend's per-crtc viewports — the offset
/// `render_frame_session` hands `build_scene`. A no-op on the nested
/// backend, whose single output never moves.
pub(crate) fn sync_positions(graphics: &mut Graphics, layout: &[(Point, Size)]) {
    let Graphics::Session(session) = graphics else {
        return;
    };
    for (output, (position, _)) in session.outputs.iter_mut().zip(layout) {
        if output.position != *position {
            output.position = *position;
            output.dirty = true;
        }
    }
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
#[cfg_attr(feature = "profile", profiling::function)]
pub(crate) fn render_frame_session(comp: &mut Compositor) -> bool {
    // Disjoint field borrows: the graphics stack mutates while the
    // ledger is read. Both live on `Compositor`, so destructure rather
    // than going through `&mut self` methods.
    let Compositor {
        wm,
        graphics,
        outputs: output_entries,
        pointer_location,
        cursor_status,
        cursors,
        ..
    } = comp;
    let Graphics::Session(session) = graphics else {
        return false;
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

    let SessionGraphics { renderer, outputs: session_outputs, strict_release, .. } = &mut **session;
    let strict_release = *strict_release;
    let mut drew_any = false;
    for (output_index, output) in session_outputs.iter_mut().enumerate() {
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
        let now = Instant::now();
        if output.frame_clock.arm(now) > now {
            continue;
        }

        let render_started = Instant::now();

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

        // One scene build per output: shared objects are translated
        // into this framebuffer's coordinates, while objects wholly
        // outside its global viewport are omitted before import and
        // element construction. `render_frame` sees a compact local
        // scene rather than discovering the other displays by clipping.
        let viewport = output_entries
            .get(output_index)
            .map(|entry| Rect::new(entry.position, entry.size))
            .unwrap_or_else(|| {
                wm.backend()
                    .monitors
                    .get(output_index)
                    .map(|monitor| monitor.geometry)
                    .unwrap_or_else(|| Rect::new(output.position, wm.backend().output_size))
            });
        let clear_color = crate::renderer::build_scene_into(
            &mut output.scene_scratch,
            wm.backend(),
            renderer,
            *pointer_location,
            cursor_status,
            cursors,
            viewport,
        );

        let (rendered, direct_scanout, render_states) =
            match output.drm_compositor.render_frame(renderer, &output.scene_scratch, clear_color, frame_flags()) {
            Ok(result) => {
                let direct_scanout = matches!(&result.primary_element, PrimaryPlaneElement::Element(_));
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
                (!result.is_empty, direct_scanout, result.states)
            }
            Err(error) => {
                // Includes the atomic test failures a returning VT
                // switch can produce; the seat-activation handler above
                // resets the device and marks damage, so the recovery
                // path is there rather than duplicated here.
                if crate::renderer::note_frame_failure() {
                    tracing::warn!(?error, output = %output.name, "DRM render failed; keeping this output dirty for a retry");
                }
                output.frame_clock.observe_render(render_started.elapsed(), Instant::now());
                output.scene_scratch.clear();
                continue;
            }
        };

        let mut frame_queued = false;
        if rendered {
            match output.drm_compositor.queue_frame(()) {
                Ok(()) => {
                    output.frame_pending = Some(PendingFlip {
                        queued_at: Instant::now(),
                        direct_scanout,
                        stall_reported: false,
                    });
                    frame_queued = true;
                    if let (Some(entry), Some(monitor)) =
                        (output_entries.get(output_index), wm.backend().monitors.get(output_index))
                    {
                        output.presentation = Some(crate::renderer::take_presentation_feedback(
                            wm.backend(),
                            &entry.output,
                            monitor.geometry,
                            Some(&render_states),
                        ));
                    }
                }
                // Nothing on the crtc actually changed. Not an error,
                // and not worth a retry: the scene is already on screen.
                Err(FrameError::EmptyFrame) => {
                    tracing::trace!(output = %output.name, "frame produced no crtc changes; no page flip queued");
                    if let (Some(entry), Some(monitor)) =
                        (output_entries.get(output_index), wm.backend().monitors.get(output_index))
                    {
                        let mut feedback = crate::renderer::take_presentation_feedback(
                            wm.backend(),
                            &entry.output,
                            monitor.geometry,
                            Some(&render_states),
                        );
                        let flags = smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync;
                        if let Some((at, sequence)) = output.last_vblank {
                            crate::renderer::present_at(
                                &mut feedback,
                                at,
                                output.refresh,
                                sequence,
                                flags | smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::HwClock,
                            );
                        } else {
                            crate::renderer::present_now(&mut feedback, output.refresh, flags);
                        }
                    }
                }
                Err(error) => {
                    // The prepared frame was consumed and its
                    // swapchain slot marked submitted before the
                    // ioctl failed (smithay's `queue_frame` order), so
                    // the damage tracker now believes that frame
                    // reached the screen. Left alone, the retry pass
                    // could render "nothing changed", report an empty
                    // frame, and clear `dirty` with the *previous*
                    // frame still on the crtc — dirty-but-never-
                    // flipped, resolved only by the next unrelated
                    // damage. Resetting the buffer ages makes the
                    // retry a full-frame render and a real flip.
                    output.drm_compositor.reset_buffer_ages();
                    tracing::warn!(?error, output = %output.name, "queueing the page flip failed; keeping this output dirty for a retry");
                    output.frame_clock.observe_render(render_started.elapsed(), Instant::now());
                    output.scene_scratch.clear();
                    continue;
                }
            }
            // Keep this frame's elements — and through them its client
            // buffers — alive until the ownership point appropriate to
            // the path. Strict compositing waits through this flip so
            // no release precedes the GPU's reads. Direct scanout then
            // moves the scene into `scanout_scene` at vblank and holds
            // it until a later flip replaces the primary plane. The
            // strict `EmptyFrame` case keeps its scene too: with no
            // vblank coming, deferring those releases to the next real
            // flip errs on the side the mechanism exists for.
            if strict_release || (direct_scanout && frame_queued) {
                // `append` transfers element ownership without taking
                // the source vector's allocation. Usually pending is
                // empty; after a strict EmptyFrame it deliberately
                // retains both scenes until the next real vblank.
                output.pending_scene.append(&mut output.scene_scratch);
            }
        }
        // When no buffer-lifetime hold was needed, release the
        // element handles at the old temporary vector's boundary. If
        // they were appended above this is an empty, allocation-
        // preserving no-op.
        output.scene_scratch.clear();
        output.frame_clock.observe_render(render_started.elapsed(), Instant::now());
        output.dirty = false;
        drew_any = true;
    }

    // Empty and successfully prepared frames both prove the rendering
    // path is healthy. Client callbacks wait for the vblank handler,
    // which is the only place that knows a queued frame really landed.
    if drew_any {
        crate::renderer::note_frame_success();
    }
    drew_any
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
    let mut selected: Option<(PathBuf, Device)> = None;
    for path in candidates {
        if selected.is_none() {
            match probe_device(seat_session, &path) {
                Ok(device) => {
                    selected = Some((path, device));
                }
                Err(reason) => {
                    tracing::info!(device = %path.display(), %reason, "skipping DRM device");
                    failures.push(format!("{}: {reason}", path.display()));
                }
            }
            continue;
        }

        match connected_desktop_connectors(seat_session, &path) {
            Ok(connectors) if !connectors.is_empty() => {
                let selected_path = selected.as_ref().map(|(path, _)| path.as_path()).unwrap();
                tracing::warn!(
                    device = %path.display(),
                    connectors = %connectors.join(", "),
                    selected = %selected_path.display(),
                    override_variable = "CHONKSTEP_DRM_DEVICE",
                    "connected outputs are omitted because this session drives one DRM device"
                );
            }
            Ok(_) => {}
            Err(reason) => tracing::debug!(device = %path.display(), %reason, "could not inspect unselected DRM device"),
        }
    }
    if let Some(selected) = selected {
        Ok(selected)
    } else {
        Err(format!("no usable DRM device on seat {seat_name} ({})", failures.join("; ")).into())
    }
}

/// Connector-only probe for devices after the selected GPU. It avoids
/// GBM/EGL creation and, crucially, does not construct a `DrmDevice`
/// with connector-reset enabled; it observes other GPUs without
/// disturbing whichever compositor currently owns their display state.
fn connected_desktop_connectors(
    seat_session: &mut LibSeatSession,
    path: &Path,
) -> Result<Vec<String>, String> {
    let fd = seat_session.open(path, DEVICE_FLAGS).map_err(|error| format!("the seat would not open it: {error:?}"))?;
    let fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let resources = fd.resource_handles().map_err(|error| format!("could not enumerate KMS resources: {error}"))?;
    let mut names = Vec::new();
    for handle in resources.connectors() {
        let Ok(info) = fd.get_connector(*handle, true) else {
            continue;
        };
        if info.state() == connector::State::Connected && !connector_is_non_desktop(&fd, *handle) {
            names.push(connector_name(&info));
        }
    }
    Ok(names)
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
                    path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("card"))
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
    let fd = seat_session.open(path, DEVICE_FLAGS).map_err(|error| format!("the seat would not open it: {error:?}"))?;
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
        if let Err(error) = fd.set_client_capability(smithay::reexports::drm::ClientCapability::UniversalPlanes, true) {
            tracing::warn!(?error, "universal planes refused at open; the hardware cursor may be unavailable");
        }
    }

    let (drm, notifier) =
        DrmDevice::new(fd.clone(), true).map_err(|error| format!("not a usable DRM device: {error}"))?;
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
    let resources =
        drm.resource_handles().map_err(|error| format!("no KMS resources (a render-only node?): {error}"))?;

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
        if connector_is_non_desktop(drm.device_fd(), *handle) {
            skipped.push(format!("{name} is marked non-desktop (reserved for a lease/VR runtime)"));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_edid() -> [u8; 128] {
        let mut edid = [0u8; 128];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        // DEL: D=4, E=5, L=12 in EDID's packed five-bit alphabet.
        edid[8..10].copy_from_slice(&((4u16 << 10) | (5u16 << 5) | 12).to_be_bytes());
        edid[10..12].copy_from_slice(&0x1234u16.to_le_bytes());
        edid[12..16].copy_from_slice(&0x01020304u32.to_le_bytes());
        edid[54..59].copy_from_slice(&[0, 0, 0, 0xfc, 0]);
        edid[59..72].copy_from_slice(b"DELL U2720Q\n ");
        edid[72..77].copy_from_slice(&[0, 0, 0, 0xff, 0]);
        edid[77..90].copy_from_slice(b"ABC123\n      ");
        edid
    }

    #[test]
    fn minimal_edid_reader_builds_hyprland_style_identity() {
        let identity = parse_edid_identity(&test_edid()).unwrap();
        assert!(identity.make == "Dell Inc." || identity.make == "DEL");
        assert_eq!(identity.model, "DELL U2720Q");
        assert_eq!(identity.serial, "ABC123");
        assert_eq!(identity.description(), format!("{} DELL U2720Q ABC123", identity.make));
    }

    #[test]
    fn edid_reader_uses_numeric_fallbacks_without_text_descriptors() {
        let mut edid = test_edid();
        edid[54..90].fill(0);
        let identity = parse_edid_identity(&edid).unwrap();
        assert_eq!(identity.model, "0x1234");
        assert_eq!(identity.serial, "0x01020304");
        assert!(parse_edid_identity(&edid[1..]).is_none());
    }

    /// The strict-release default follows the driver: on for NVIDIA
    /// (no implicit dmabuf fencing to mask an early release), off
    /// everywhere else (the kernel already orders client writes behind
    /// compositor reads, so holding buffers longer buys nothing). The
    /// environment overrides both directions, because a heuristic
    /// about driver behaviour must never need a compiler to correct.
    #[test]
    fn strict_release_defaults_to_the_driver_and_bows_to_the_environment() {
        assert!(strict_release_configured(None, true));
        assert!(!strict_release_configured(None, false));
        // Forced on where the default says off…
        assert!(strict_release_configured(Some("1".into()), false));
        // …and off where the default says on.
        assert!(!strict_release_configured(Some("0".into()), true));
        // Any non-"0" value is a request to enable, matching the
        // crate's other env switches (`CHONKSTEP_FULL_DAMAGE` et al).
        assert!(strict_release_configured(Some("yes".into()), false));
    }

    /// Each plane switch removes only its own capability: debugging a
    /// cursor-plane issue must not silently turn off fullscreen direct
    /// scanout, and vice versa.
    #[test]
    fn plane_switches_are_on_by_default_and_independent() {
        let cursor = FrameFlags::ALLOW_CURSOR_PLANE_SCANOUT;
        let primary = FrameFlags::ALLOW_PRIMARY_PLANE_SCANOUT;
        assert_eq!(configured_frame_flags(None, None), FRAME_FLAGS);
        assert_eq!(configured_frame_flags(Some("0".into()), Some("0".into())), FRAME_FLAGS);
        assert_eq!(configured_frame_flags(Some("1".into()), None), primary);
        assert_eq!(configured_frame_flags(None, Some("yes".into())), cursor);
        assert_eq!(configured_frame_flags(Some("true".into()), Some("1".into())), FrameFlags::empty());
    }

    /// A direct buffer remains owned after the vblank that activates it
    /// and throughout the next flip. Only completion of that replacing
    /// flip may release it; returning to a compositor buffer releases
    /// both direct scenes.
    #[test]
    fn direct_scanout_scene_lives_until_its_replacement_is_current() {
        let mut pending = Vec::with_capacity(8);
        pending.push(1);
        let first_capacity = pending.capacity();
        let mut scanout = Vec::new();
        let mut active = false;

        assert!(complete_scene_flip(&mut pending, &mut scanout, &mut active, true));
        assert!(pending.is_empty());
        assert_eq!(scanout, vec![1]);
        assert!(active);

        // Buffer 2 is queued, but buffer 1 is still physically being
        // scanned while that commit is in flight.
        pending.push(2);
        assert_eq!(scanout, vec![1]);
        assert!(!complete_scene_flip(&mut pending, &mut scanout, &mut active, true));
        assert_eq!(scanout, vec![2]);
        assert_eq!(
            pending.capacity(), first_capacity,
            "the retired scanout allocation becomes storage for the next in-flight scene"
        );

        pending.push(3);
        assert!(complete_scene_flip(&mut pending, &mut scanout, &mut active, false));
        assert!(pending.is_empty());
        assert!(scanout.is_empty());
        assert!(!active);
    }

    #[test]
    fn disabling_the_crtc_releases_all_scene_holds() {
        let mut pending = vec![1];
        let mut scanout = vec![2];
        let mut active = true;
        clear_scene_holds(&mut pending, &mut scanout, &mut active);
        assert!(pending.is_empty());
        assert!(scanout.is_empty());
        assert!(!active);
    }

    #[test]
    fn scanout_feedback_promises_only_pairs_shared_by_every_output() {
        let first = [Modifier::Linear, Modifier::Invalid];
        let second = [Modifier::Invalid];
        let formats = common_scanout_formats(
            [(Fourcc::Argb8888, first.as_slice()), (Fourcc::Argb8888, second.as_slice())].into_iter(),
        );
        assert_eq!(
            formats.indexset().iter().copied().collect::<Vec<_>>(),
            vec![Format { code: Fourcc::Argb8888, modifier: Modifier::Invalid }]
        );

        let mismatched = common_scanout_formats(
            [(Fourcc::Argb8888, first.as_slice()), (Fourcc::Abgr8888, first.as_slice())].into_iter(),
        );
        assert!(mismatched.indexset().is_empty());
        assert!(common_scanout_formats(std::iter::empty()).indexset().is_empty());
    }

    #[test]
    fn first_and_post_idle_frames_are_never_delayed() {
        let period = Duration::from_millis(16);
        let now = Instant::now();
        let mut clock = FrameClock::for_period(period);
        assert_eq!(clock.arm(now), now);

        clock.deadline = None;
        clock.last_vblank = Some(now - period.saturating_mul(4));
        assert_eq!(clock.arm(now), now);
    }

    #[test]
    fn active_cadence_arms_before_the_predicted_vblank() {
        let period = Duration::from_millis(16);
        let vblank = Instant::now();
        let now = vblank + Duration::from_millis(1);
        let mut clock = FrameClock::for_period(period);
        clock.note_vblank(vblank);
        let deadline = clock.arm(now);
        assert!(deadline > now);
        assert!(deadline < vblank + period);
        assert_eq!(clock.target_vblank, Some(vblank + period));
    }

    #[test]
    fn pausing_disarms_the_deadline_and_forgets_the_old_cadence() {
        let period = Duration::from_millis(16);
        let vblank = Instant::now();
        let mut clock = FrameClock::for_period(period);
        clock.note_vblank(vblank);
        assert!(clock.arm(vblank + Duration::from_millis(1)) > vblank);

        clock.disarm();
        let resumed = vblank + Duration::from_secs(1);
        assert_eq!(clock.arm(resumed), resumed);
        assert!(clock.target_vblank.is_none());
    }

    #[test]
    fn render_estimate_rises_fast_and_decays_slowly() {
        let mut clock = FrameClock::for_period(Duration::from_millis(16));
        let start = Instant::now();
        clock.observe_render(Duration::from_millis(9), start);
        let raised = clock.pessimistic_budget();
        assert!(raised > Duration::from_millis(6));

        clock.observe_render(Duration::from_millis(1), start + Duration::from_millis(16));
        assert!(clock.pessimistic_budget() > Duration::from_millis(5));
        assert!(clock.pessimistic_budget() <= raised);
    }
}
