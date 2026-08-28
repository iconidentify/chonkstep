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
//! Interim skeleton: the real implementation is being built against
//! this exact API, and until it lands `init` fails cleanly so a TTY
//! launch says what is missing instead of crashing.

use smithay::output::Output;
use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::DisplayHandle;
use wm_theme_api::Size;

use crate::state::{Compositor, Graphics};

/// Everything the session backend owns while it runs: the seat, the
/// DRM device and its GBM allocator, the EGL/GLES renderer, the
/// per-output DRM compositors, and the input backend. Opaque to the
/// rest of the crate on purpose - only [`init`] and
/// [`render_frame_session`] reach inside.
pub(crate) struct SessionGraphics {
    _private: (),
}

/// What [`init`] hands back to `run`: the graphics stack plus the
/// output it discovered, already configured with its mode. Any event
/// sources the session needs (DRM page flips, libinput devices, udev
/// hot-plug, seat activation) are registered on the loop by `init`
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
pub(crate) fn init(
    _loop_handle: &LoopHandle<'static, Compositor>,
    _display_handle: &DisplayHandle,
) -> Result<SessionInit, Box<dyn std::error::Error>> {
    Err("the DRM/KMS session backend is not built yet; run nested inside an existing desktop, \
         or set CHONKSTEP_BACKEND=winit"
        .into())
}

/// Draws one frame onto the hardware: [`crate::renderer::build_scene`]
/// through the session's renderer, submitted as a page flip.
pub(crate) fn render_frame_session(_comp: &mut Compositor) {}
