//! Selection ownership and asynchronous Wayland/X11 clipboard forwarding.
//!
//! X11 callbacks live with the XWM in `xwayland.rs`; native client requests
//! enter here. Clipboard, primary selection and drag-and-drop are distinct
//! protocols, even when they share the seat's data-device machinery.

pub(crate) mod persistence;
use persistence::SelectionData;

use std::os::fd::OwnedFd;

use smithay::input::Seat;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::{delegate_data_device, delegate_primary_selection};

use crate::state::Compositor;

/// Reconcile existing native ownership once the asynchronous XWM is ready.
/// Read the seat's authoritative source; do not retain duplicate MIME lists or
/// payloads during ordinary input, and never re-export a dead X11-owned offer.
pub(crate) fn xwayland_ready(compositor: &mut Compositor) {
    use smithay::wayland::selection::current_client_selection;

    let Some(xwm) = compositor.xwayland.wm.as_mut() else {
        return;
    };
    for target in [SelectionTarget::Clipboard, SelectionTarget::Primary] {
        let mimes = current_client_selection(&compositor.seat, target).map(|source| source.mime_types())
            .or_else(|| {
                if target != SelectionTarget::Clipboard { return None; }
                let data = smithay::wayland::selection::data_device::current_data_device_selection_userdata(&compositor.seat)?;
                match &*data { SelectionData::Memory(data) => data.upgrade().map(|data| data.keys().cloned().collect()), _ => None }
            });
        let Some(mimes) = mimes else { continue; };
        if let Err(error) = xwm.new_selection(target, Some(mimes)) {
            tracing::warn!(
                ?error,
                ?target,
                "could not restore native selection to XWayland"
            );
        }
    }
}

/// Withdraw only bridge-owned offers when their X11 server disappears.
/// A native owner's clipboard/primary selection survives an XWayland restart.
pub(crate) fn xwayland_lost(compositor: &mut Compositor) {
    compositor.clipboard_persistence.x11_lost();
    use smithay::wayland::selection::data_device::{
        clear_data_device_selection, current_data_device_selection_userdata,
    };
    use smithay::wayland::selection::primary_selection::{
        clear_primary_selection, current_primary_selection_userdata,
    };

    if current_data_device_selection_userdata(&compositor.seat).is_some_and(|data| matches!(*data, SelectionData::Bridge)) {
        clear_data_device_selection(&compositor.display_handle, &compositor.seat);
    }
    if current_primary_selection_userdata(&compositor.seat).is_some() {
        clear_primary_selection(&compositor.display_handle, &compositor.seat);
    }
}

/// The Wayland-to-X11 half of the clipboard bridge.
///
/// Both callbacks fire only for *client* requests — `set_selection` on
/// a `wl_data_device` or a `zwp_primary_selection_device_v1` — never
/// for the compositor-side selections `xwayland.rs` installs when an X
/// client takes ownership. That asymmetry is what keeps the two halves
/// from ping-ponging a selection back and forth forever, and it is a
/// property of smithay's dispatch rather than of a guard here, so it is
/// worth knowing before adding one.
impl SelectionHandler for Compositor {
    type SelectionUserData = SelectionData;

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        tracing::debug!(?ty, clear = source.is_none(), "client selection changed");
        if ty == SelectionTarget::Clipboard {
            self.clipboard_persistence.clear();
            if self.wm.mac_mode() && self.wm.interaction_config().clipboard_persistence {
                if let Some(source) = source.as_ref() {
                    let requests = self.clipboard_persistence.begin(Some(source.clone()), source.mime_types());
                    for (mime, fd) in requests { source.send(mime, fd); }
                }
            }
        }
        // A Wayland client copied something. Xwayland owns the X-side
        // selection window, so it has to be told to claim CLIPBOARD (or
        // PRIMARY) on the X server and advertise these mime types;
        // `None` means the client dropped the selection, which releases
        // the X ownership again. Before this existed, copying in a
        // Wayland app and pasting in xterm produced nothing at all — X
        // clients asked the X server who owned the selection and the
        // answer was nobody.
        let Some(xwm) = self.xwayland.wm.as_mut() else {
            // XWayland is not running (or failed to start); there is no
            // X server to mirror the selection onto and native clients
            // already have it.
            return;
        };
        if let Err(error) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
            tracing::warn!(?error, ?ty, "could not hand the selection to XWayland");
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        user_data: &SelectionData,
    ) {
        if matches!(user_data, SelectionData::Memory(_)) {
            self.clipboard_persistence.send(user_data, &mime_type, fd);
            return;
        }
        // A Wayland client is pasting a selection this compositor owns
        // on behalf of an X client (the one `xwayland.rs`'s
        // `new_selection` installed). Fetching the bytes is an X
        // round-trip — INCR transfers included — so `X11Wm` runs it as
        // a calloop source and writes into `fd` when it completes,
        // which is why the loop handle goes with it. Nothing blocks
        // here; the pasting client simply reads its pipe when data
        // arrives.
        let loop_handle = self.loop_handle.clone();
        let Some(xwm) = self.xwayland.wm.as_mut() else {
            return;
        };
        if let Err(error) = xwm.send_selection(ty, mime_type, fd, loop_handle) {
            tracing::warn!(
                ?error,
                ?ty,
                "could not read the X11 selection for a Wayland client"
            );
        }
    }
}

impl DataDeviceHandler for Compositor {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for Compositor {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

// Defaults throughout: client-to-client DnD works through the seat's
// own grab machinery; a rendered drag icon is a follow-up for the
// renderer (the icon surface arrives in `started`, unused for now).
impl ClientDndGrabHandler for Compositor {}
impl ServerDndGrabHandler for Compositor {}

delegate_data_device!(Compositor);
delegate_primary_selection!(Compositor);
