//! XWayland process generations and the resources that retire with them.
//!
//! Native Wayland remains available when X11 cannot start. A previously ready
//! X server may restart once; the budget is consumed before spawn, never reset
//! by readiness, so repeated failures cannot spin an unbounded supervisor.

use std::ffi::OsString;
use std::process::Stdio;

use smithay::reexports::calloop::LoopHandle;
use smithay::reexports::wayland_server::DisplayHandle;
use smithay::xwayland::{X11Wm, XWayland, XWaylandEvent};
use wm_core::BackendEvent;

use crate::state::{Compositor, ManagedSurface, WlWindowId};

/// Connections and restart policy belonging to ChonkStep's X11 subsystem.
/// All generation-owned connections are cleared together in the loss handler.
pub(crate) struct State {
    /// XWM exists only after the rootless server is ready.
    pub(crate) wm: Option<X11Wm>,
    /// Ready display number; DISPLAY is exported earlier for autostart.
    pub(crate) display: Option<u32>,
    /// Independent XSETTINGS queue; never shares the XWM event reader.
    pub(crate) settings: Option<chonk_xsettings::XSettingsManager>,
    /// Independent EWMH publisher on this generation's root window.
    pub(crate) ewmh: Option<crate::xewmh::XEwmh>,
    /// Consumed before the single restart; readiness never replenishes it.
    restart_available: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            wm: None,
            display: None,
            settings: None,
            ewmh: None,
            restart_available: true,
        }
    }
}

/// Smithay clears the child environment. Keep the graphics stack and explicit
/// software/glamor policy selected for this session; do not pass unrelated
/// application credentials or compositor control variables to the X server.
fn graphics_environment() -> Vec<(OsString, OsString)> {
    const NAMES: &[&str] = &[
        "LD_LIBRARY_PATH", "LIBGL_DRIVERS_PATH", "GBM_BACKENDS_PATH",
        "__EGL_VENDOR_LIBRARY_FILENAMES", "__EGL_VENDOR_LIBRARY_DIRS",
        "__GLX_VENDOR_LIBRARY_NAME", "VK_DRIVER_FILES", "VK_ICD_FILENAMES",
        "VK_ADD_DRIVER_FILES", "VK_LOADER_LAYERS_DISABLE", "DRIRC_CONFIGDIR",
        "ASAHI_M3_EXPERIMENTAL", "AGX_MESA_DEBUG", "HK_DEBUG_LOG",
        "MESA_LOADER_DRIVER_OVERRIDE", "GALLIUM_DRIVER", "LIBGL_ALWAYS_SOFTWARE",
        "GBM_ALWAYS_SOFTWARE", "LIBGL_DRI3_DISABLE", "XWAYLAND_NO_GLAMOR",
    ];
    NAMES.iter().filter_map(|name| std::env::var_os(name)
        .map(|value| (OsString::from(name), value))).collect()
}

pub(crate) fn register_source(
    display_handle: &DisplayHandle,
    loop_handle: &LoopHandle<'static, Compositor>,
) -> Result<(), String> {
    let (xwayland, xwayland_client) = match XWayland::spawn(
        display_handle,
        None,
        graphics_environment(),
        true,
        Stdio::null(),
        Stdio::null(),
        |_| (),
    ) {
        Ok(pair) => pair,
        Err(error) => {
            tracing::warn!(?error, "could not spawn XWayland; X11 apps unavailable");
            return Ok(());
        }
    };
    let display_number = xwayland.display_number();
    loop_handle
        .insert_source(xwayland, move |event, _, comp| match event {
            XWaylandEvent::Ready {
                x11_socket,
                display_number,
            } => {
                match X11Wm::start_wm(
                    comp.loop_handle.clone(),
                    x11_socket,
                    xwayland_client.clone(),
                ) {
                    Ok(xwm) => {
                        comp.xwayland.wm = Some(xwm);
                        comp.xwayland.display = Some(display_number);
                        crate::selection::xwayland_ready(comp);
                        tracing::info!(display = display_number, "XWayland ready");
                        comp.start_xsettings(display_number);
                        crate::xewmh::start(comp, display_number);
                    }
                    Err(error) => {
                        std::env::remove_var("DISPLAY");
                        tracing::error!(
                            ?error,
                            "failed to attach the X11 window manager to XWayland"
                        );
                    }
                }
            }
            XWaylandEvent::Error => comp.handle_xwayland_loss("startup failure"),
        })
        .map_err(|error| format!("failed to register the XWayland event source: {error}"))?;
    // Smithay has already reserved the display and bound its listening
    // sockets. X11 clients can connect now and wait for startup to finish;
    // publishing only at Ready made autostart inherit the host's DISPLAY
    // (or no DISPLAY at all) before our first event-loop dispatch.
    std::env::set_var("DISPLAY", format!(":{display_number}"));
    tracing::info!(
        display = display_number,
        "XWayland listening; display exported for autostart"
    );
    Ok(())
}

impl Compositor {
    /// Retires every piece of state owned by one XWayland generation.
    /// Called by the startup source and, critically, by
    /// `XwmHandler::disconnected` after a running X server dies.
    pub(crate) fn handle_xwayland_loss(&mut self, reason: &'static str) {
        let was_ready = self.xwayland.display.is_some() || self.xwayland.wm.is_some();
        tracing::warn!(reason, "XWayland exited; X11 apps temporarily unavailable");
        crate::selection::xwayland_lost(self);
        self.xwayland.wm = None;
        self.xwayland.display = None;
        self.xwayland.settings = None;
        self.xwayland.ewmh = None;
        std::env::remove_var("DISPLAY");

        let backend = self.wm.backend_mut();
        let orphaned: Vec<WlWindowId> = backend
            .windows
            .iter()
            .filter(|(_, record)| matches!(record.surface, ManagedSurface::X11(_)))
            .map(|(id, _)| *id)
            .collect();
        for id in orphaned {
            backend.forget_window(id);
            backend.queue(BackendEvent::Destroyed(id));
        }
        backend.mark_damaged();

        if was_ready && std::mem::take(&mut self.xwayland.restart_available) {
            tracing::info!("restarting XWayland once after disconnect");
            let display_handle = self.display_handle.clone();
            let loop_handle = self.loop_handle.clone();
            if let Err(error) = register_source(&display_handle, &loop_handle) {
                tracing::warn!(%error, "could not register the XWayland restart");
            }
        }
    }
}
