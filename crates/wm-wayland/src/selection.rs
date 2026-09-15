//! Selection ownership and asynchronous Wayland/X11 clipboard forwarding.
//!
//! X11 callbacks live with the XWM in `xwayland.rs`; native client requests
//! enter here. Clipboard, primary selection and drag-and-drop are distinct
//! protocols, even when they share the seat's data-device machinery.

pub(crate) mod persistence;
use persistence::SelectionData;

use std::os::fd::OwnedFd;

use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_data_source::WlDataSource;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::{with_states, SurfaceAttributes};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::{delegate_data_device, delegate_primary_selection};

use crate::state::{Compositor, DndIcon};

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
            if source.is_some() { self.mac_copy_order.offered(); }
            self.clipboard_persistence.clear();
            if self.wm.spaces_mode() && self.wm.interaction_config().clipboard_persistence {
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

/// Client-to-client DnD works through the seat's own grab machinery;
/// what the compositor adds is the icon. The surface a client passes
/// to `start_drag` arrives here with smithay's `dnd_icon` role and
/// nothing else: drawing it under the pointer, damaging on its
/// commits and answering its frame callbacks are all the renderer's
/// job, keyed off the ledger entry these two hooks maintain.
impl ClientDndGrabHandler for Compositor {
    fn started(&mut self, _source: Option<WlDataSource>, icon: Option<WlSurface>, seat: Seat<Self>) {
        let Some(surface) = icon else {
            return;
        };
        // Smithay hands the drag to whichever implicit grab the serial
        // names, the pointer's first (`data_device/device.rs`). The
        // serial is not passed on, so the same preference is read
        // back from the seat: a grabbed pointer means a button-held
        // drag, and only otherwise does the touch's own grab — whose
        // start data names the finger — make this a touch drag.
        let (touch, origin) = if let Some(start) = seat.get_pointer().and_then(|pointer| pointer.grab_start_data()) {
            (None, start.focus.map(|(focus, _)| focus.surface().clone()))
        } else {
            let start = seat.get_touch().and_then(|touch| touch.grab_start_data());
            (start.as_ref().map(|start| (start.slot, start.location)),
                start.and_then(|start| start.focus.map(|(focus, _)| focus.surface().clone())))
        };
        let capture_redacted = origin.is_some_and(|mut root| {
            while let Some(parent) = smithay::wayland::compositor::get_parent(&root) {
                root = parent;
            }
            if let Some(popup_root) = self.popups.find_popup(&root)
                .and_then(|popup| smithay::desktop::find_popup_root_surface(&popup).ok())
            {
                root = popup_root;
            }
            let backend = self.wm.backend();
            backend.window_for_surface(&root).and_then(|id| backend.windows.get(&id))
                .is_some_and(|record| record.capture_redacted)
        });
        // A client may commit the icon, offset included, before the
        // `start_drag` that gives it its role; that delta is still in
        // the surface's current state until its next commit, which is
        // when `xdg.rs`'s commit handler takes over the accumulation.
        let offset = with_states(&surface, |states| {
            states.cached_state.get::<SurfaceAttributes>().current().buffer_delta.take()
        })
        .unwrap_or_default();
        let backend = self.wm.backend_mut();
        backend.dnd_icon = Some(DndIcon { surface, capture_redacted, offset, touch });
        backend.mark_damaged();
    }

    /// Every end of a client drag lands here — the drop itself, the
    /// cancel, and the `unset_grab` a session lock or a replaced grab
    /// performs (`DnDGrab::unset` calls its `drop`) — so this is the
    /// one place the icon leaves the scene.
    fn dropped(&mut self, _target: Option<WlSurface>, _validated: bool, _seat: Seat<Self>) {
        let backend = self.wm.backend_mut();
        if backend.dnd_icon.take().is_some() {
            backend.mark_damaged();
        }
    }
}
impl ServerDndGrabHandler for Compositor {}

delegate_data_device!(Compositor);
delegate_primary_selection!(Compositor);
