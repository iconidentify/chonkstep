//! XWayland: the X11 half of the compositor. `run()` spawns a rootless
//! Xwayland server and attaches this process as its window manager
//! (`X11Wm::start_wm`) once it reports ready; the handlers here are
//! that window manager. Every X11 window enters the SAME `WlWindowId`
//! space as native toplevels (`ManagedSurface::X11`) — so urxvt gets
//! the exact chrome, focus, and stacking treatment a Wayland-native
//! terminal gets, and `wm-core` never learns which protocol a window
//! spoke.
//!
//! The translations here mirror `wm-x11`'s `translate_event` almost
//! line-for-line — unsurprisingly, since XWM events ARE X11 events:
//! map request, unmap/destroy notify, configure request, property
//! notify, and the `_NET_WM_STATE` request family. The differences are
//! where smithay already digested the protocol (titles and classes come
//! from `X11Surface` accessors, not GetProperty round-trips).
//!
//! One thing here has no counterpart in `wm-x11` at all: the selection
//! callbacks at the bottom. On X11 the clipboard is the X server's
//! business and a window manager never touches it. Here the compositor
//! sits between two clipboards that know nothing about each other, so
//! CLIPBOARD and PRIMARY are bridged in both directions — see those
//! callbacks and `xdg.rs`'s `SelectionHandler` for the two halves.

use std::os::fd::OwnedFd;

use smithay::delegate_xwayland_shell;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::selection::data_device::{
    clear_data_device_selection, current_data_device_selection_userdata,
    request_data_device_client_selection, set_data_device_selection,
};
use smithay::wayland::selection::primary_selection::{
    clear_primary_selection, current_primary_selection_userdata, request_primary_client_selection,
    set_primary_selection,
};
use smithay::wayland::selection::SelectionTarget;
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{
    Reorder, ResizeEdge as X11ResizeEdge, WmWindowProperty, X11Window, XwmId,
};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use wm_core::{BackendEvent, NetState, NetStateAction};
use wm_theme_api::{clamp_client_size, Point, Rect, ResizeEdge, Size};

use crate::state::{
    Compositor, ManagedSurface, WaylandBackend, WindowRecord, WlFrameId, WlWindowId,
};

type WmEvent = BackendEvent<WlWindowId, WlFrameId>;

/// The association protocol Xwayland uses to tie its X11 windows to
/// wl_surfaces — `X11Wm::start_wm` requires it, and without it no X11
/// window ever gets content on screen.
impl XWaylandShellHandler for Compositor {
    fn xwayland_shell_state(&mut self) -> &mut XWaylandShellState {
        &mut self.xwayland_shell_state
    }
}

delegate_xwayland_shell!(Compositor);

/// Reverse lookup by X11 surface handle (cheap Arc'd equality). The
/// wl_surface-based lookup in `state.rs` cannot serve here: an X11
/// window exists — and needs configure/property routing — before its
/// wl_surface association ever arrives.
fn x11_window_id(backend: &WaylandBackend, window: &X11Surface) -> Option<WlWindowId> {
    backend.windows.iter().find_map(|(id, record)| match &record.surface {
        ManagedSurface::X11(existing) if existing == window => Some(*id),
        _ => None,
    })
}

/// Finds the record for this X11 window, creating one on first sight —
/// map request and mapped-override-redirect both funnel through here so
/// a window that unmaps and remaps keeps its id, like an X11 window
/// keeps its XID.
fn ensure_x11_record(backend: &mut WaylandBackend, window: &X11Surface) -> WlWindowId {
    if let Some(id) = x11_window_id(backend, window) {
        return id;
    }
    let id = WlWindowId(backend.alloc_id());
    // X11 clients pick their own geometry before mapping (the XWM
    // tracked their pre-map configures); starting the record from that
    // truth is what makes `Backend::window_geometry` answer correctly
    // at map time.
    let geometry = wm_rect(window.geometry(), backend.output_size);
    backend.remember_window(id, WindowRecord::new(ManagedSurface::X11(window.clone()), geometry));
    id
}

/// Smithay's XWM edge -> the theme vocabulary `wm-core` resizes in.
/// Total, unlike the xdg mapping in `xdg.rs`: `_NET_WM_MOVERESIZE`'s
/// move and keyboard directions arrive as separate XWM callbacks, so
/// every value this enum can carry names a real edge.
fn wm_resize_edge(edge: X11ResizeEdge) -> ResizeEdge {
    match edge {
        X11ResizeEdge::Top => ResizeEdge::North,
        X11ResizeEdge::Bottom => ResizeEdge::South,
        X11ResizeEdge::Left => ResizeEdge::West,
        X11ResizeEdge::Right => ResizeEdge::East,
        X11ResizeEdge::TopLeft => ResizeEdge::NorthWest,
        X11ResizeEdge::TopRight => ResizeEdge::NorthEast,
        X11ResizeEdge::BottomLeft => ResizeEdge::SouthWest,
        X11ResizeEdge::BottomRight => ResizeEdge::SouthEast,
    }
}

fn wm_rect(rect: Rectangle<i32, Logical>, screen: Size) -> Rect {
    let requested = Size::new(rect.size.w.max(0) as u32, rect.size.h.max(0) as u32);
    Rect {
        pos: Point::new(rect.loc.x, rect.loc.y),
        size: clamp_client_size(requested, screen),
    }
}

impl XwmHandler for Compositor {
    fn xwm_state(&mut self, _xwm: XwmId) -> &mut X11Wm {
        // Smithay only dispatches XWM events after `start_wm` succeeded,
        // which is the only place `xwm` is set (see `run()`).
        self.xwm.as_mut().expect("XWM event before start_wm")
    }

    fn new_window(&mut self, _xwm: XwmId, _window: X11Surface) {
        // Creation is not management — X11 clients create windows long
        // before mapping (some never map). The record is made at map
        // time; pre-map configures are honored below without one.
    }

    fn new_override_redirect_window(&mut self, _xwm: XwmId, _window: X11Surface) {}

    fn map_window_request(&mut self, _xwm: XwmId, window: X11Surface) {
        // The X11 map veto is ours now: allow the map (which also lets
        // Xwayland associate a wl_surface), then hand `wm-core` the
        // same MapRequest an X server would have routed to `wm-x11` —
        // decoration policy, placement, and workspace assignment all
        // run unchanged from there.
        if let Err(error) = window.set_mapped(true) {
            tracing::warn!(?error, "X11 map_window_request set_mapped failed");
        }
        let backend = self.wm.backend_mut();
        let id = ensure_x11_record(backend, &window);
        // Refresh the pre-map geometry — the client may have configured
        // itself since the record was first created.
        let geometry = wm_rect(window.geometry(), backend.output_size);
        if let Some(record) = backend.windows.get_mut(&id) {
            record.content = geometry;
        }
        backend.queue(WmEvent::MapRequest(id));
    }

    fn mapped_override_redirect_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Menus, tooltips, dropdowns: already mapped by the client (no
        // request to veto). The MapRequest still flows to `wm-core`,
        // whose `window_type` read classifies them Unmanaged (the
        // backend reports override-redirect as such) and maps them
        // as-is with no frame — the exact path `wm-x11` takes for
        // these window types.
        let backend = self.wm.backend_mut();
        let id = ensure_x11_record(backend, &window);
        let geometry = wm_rect(window.geometry(), backend.output_size);
        if let Some(record) = backend.windows.get_mut(&id) {
            record.content = geometry;
        }
        backend.queue(WmEvent::MapRequest(id));
    }

    fn map_window_notify(&mut self, _xwm: XwmId, window: X11Surface) {
        // Smithay has just APPENDed this window to the root's
        // `_NET_CLIENT_LIST` on its own connection — after our EWMH
        // flush already REPLACEd the property with `wm-core`'s list
        // (the manage runs off the map *request*, one step earlier),
        // so the window is now listed twice. Re-dirty the list so the
        // next `xewmh::flush` REPLACEs it again, now ordered after
        // smithay's append on the server; see
        // `EwmhLedger::mark_client_list_dirty` for the observed
        // failure.
        let _ = window;
        self.wm.backend_mut().ewmh.mark_client_list_dirty();
    }

    fn unmapped_window(&mut self, _xwm: XwmId, window: X11Surface) {
        let backend = self.wm.backend_mut();
        let Some(id) = x11_window_id(backend, &window) else {
            return;
        };
        if let Some(record) = backend.windows.get_mut(&id) {
            // Stop drawing immediately — matters for override-redirect
            // windows, which have no frame lifecycle to hide them.
            record.mapped = false;
        }
        backend.scene_index.mark_hidden(id);
        backend.mark_damaged();
        backend.queue(WmEvent::Unmapped(id));
        // Return the window to withdrawn so a future map restarts the
        // cycle cleanly; smithay refuses this for override-redirect
        // windows (they were never ours to unmap).
        if !window.is_override_redirect() {
            if let Err(error) = window.set_mapped(false) {
                tracing::warn!(?error, "X11 unmapped_window set_mapped(false) failed");
            }
        }
    }

    fn destroyed_window(&mut self, _xwm: XwmId, window: X11Surface) {
        // Record dropped immediately, event queued — the DestroyNotify
        // translation, matching `wm-x11` removing `known_clients` on
        // the spot (every later backend verb tolerates the missing id).
        let backend = self.wm.backend_mut();
        let Some(id) = x11_window_id(backend, &window) else {
            return;
        };
        backend.forget_window(id);
        backend.mark_damaged();
        backend.queue(WmEvent::Destroyed(id));
    }

    fn configure_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        x: Option<i32>,
        y: Option<i32>,
        w: Option<u32>,
        h: Option<u32>,
        _reorder: Option<Reorder>,
    ) {
        // Merge the request over the current geometry — X11 configure
        // requests name only the fields the client cares about.
        let mut requested = window.geometry();
        if let Some(x) = x {
            requested.loc.x = x;
        }
        if let Some(y) = y {
            requested.loc.y = y;
        }
        if let Some(w) = w {
            requested.size.w = w as i32;
        }
        if let Some(h) = h {
            requested.size.h = h as i32;
        }
        let backend = self.wm.backend_mut();
        if let Some(id) = x11_window_id(backend, &window) {
            // Known window: `wm-core` decides, exactly as it does for
            // the X11 backend's ConfigureRequest events (it honors
            // sizes, ignores positions on managed clients, and applies
            // everything verbatim pre-manage via `configure_unmanaged`).
            let requested = wm_rect(requested, backend.output_size);
            backend.queue(WmEvent::ConfigureRequest { window: id, requested });
        } else {
            // Never mapped, no record yet: honor directly — ICCCM
            // requires acknowledging pre-map configures or clients
            // deadlock waiting for their geometry.
            if let Err(error) = window.configure(requested) {
                tracing::warn!(?error, "X11 pre-manage configure failed");
            }
        }
    }

    fn configure_notify(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        geometry: Rectangle<i32, Logical>,
        _above: Option<X11Window>,
    ) {
        // Only self-positioning windows matter here: an override-
        // redirect menu moving itself must carry its scene rect (and
        // hit-test target) along. Managed windows' geometry is owned by
        // `wm-core` — their notifies just echo our own configures.
        if !window.is_override_redirect() {
            return;
        }
        let backend = self.wm.backend_mut();
        if let Some(id) = x11_window_id(backend, &window) {
            let geometry = wm_rect(geometry, backend.output_size);
            if let Some(record) = backend.windows.get_mut(&id) {
                record.content = geometry;
            }
            backend.mark_damaged();
        }
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        // The property watch `wm-x11` keeps via PropertyNotify —
        // smithay has already filtered to the interesting atoms and
        // re-read the value into the surface before calling us, so both
        // arms below are reads of parsed state rather than GetProperty
        // round-trips.
        let backend = self.wm.backend_mut();
        let Some(id) = x11_window_id(backend, &window) else {
            return;
        };
        match property {
            WmWindowProperty::Title => {
                if let Some(record) = backend.windows.get_mut(&id) {
                    // Keep the record's cache current per its contract
                    // (`WindowRecord::title`'s doc in state.rs); empty
                    // means never really set, same rule as
                    // `window_title`.
                    let title = window.title();
                    record.title = if title.is_empty() { None } else { Some(title) };
                }
                backend.queue(WmEvent::TitleChanged(id));
            }
            // `_MOTIF_WM_HINTS`, which is where an X11 client says
            // whether it wants to be decorated. Smithay has no
            // "decorations changed" variant — `MotifHints` is the whole
            // property, and the decoration bit is the only part of it
            // this WM reads (`Backend::client_draws_own_chrome`), so
            // this is as narrow a trigger as the enum allows. A
            // spurious `ChromeChanged` costs `wm-core` one re-read that
            // returns the same answer; a missed one leaves a window
            // wearing two titlebars until it is remapped, which is the
            // bug.
            //
            // Worth having even though most clients set the hint before
            // mapping: Motif hints on a mapped window are how an app
            // toggles its own titlebar off for a presentation or
            // borderless mode, and several do.
            WmWindowProperty::MotifHints => {
                backend.queue(WmEvent::ChromeChanged(id));
            }
            _ => {}
        }
    }

    fn maximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.queue_x11_net_state(
            &window,
            NetStateAction::Add,
            NetState::MaximizedHorz,
            Some(NetState::MaximizedVert),
        );
    }

    fn unmaximize_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.queue_x11_net_state(
            &window,
            NetStateAction::Remove,
            NetState::MaximizedHorz,
            Some(NetState::MaximizedVert),
        );
    }

    fn fullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.queue_x11_net_state(&window, NetStateAction::Add, NetState::Fullscreen, None);
    }

    fn unfullscreen_request(&mut self, _xwm: XwmId, window: X11Surface) {
        self.queue_x11_net_state(&window, NetStateAction::Remove, NetState::Fullscreen, None);
    }

    fn resize_request(
        &mut self,
        _xwm: XwmId,
        window: X11Surface,
        _button: u32,
        resize_edge: X11ResizeEdge,
    ) {
        // The other half of `_NET_WM_MOVERESIZE`, dropped until
        // `BackendEvent::ResizeRequest` existed — on the argument that
        // it was not symmetrical with `move_request`: taking our chrome
        // away from a client-decorated window removed the only way to
        // *move* it, whereas such a client draws its own resize grips
        // and, it was assumed, handles the drag itself. The assumption
        // was backwards. `_NET_WM_MOVERESIZE` is the grip *delegating*
        // the drag — a client that ran it itself would never have sent
        // the message — so dropping it left every grip on a
        // client-decorated X11 window armed and inert. `wm-core` now
        // runs the same interactive resize a resizebar drag runs; the
        // edge is the client's report of which grip was grabbed, the
        // start geometry is our own record.
        let backend = self.wm.backend_mut();
        if let Some(id) = x11_window_id(backend, &window) {
            backend.queue(WmEvent::ResizeRequest {
                window: id,
                edge: wm_resize_edge(resize_edge),
            });
        }
    }

    fn move_request(&mut self, _xwm: XwmId, window: X11Surface, _button: u32) {
        // `_NET_WM_MOVERESIZE` with a move direction — an application
        // saying "the user has grabbed something I consider a titlebar;
        // you take it from here". Dropped until this window manager
        // could leave a managed window unframed, at which point dropping
        // it stopped being a preference and became a regression: a
        // client-decorated window has no chrome of ours to drag, so this
        // request is the only handle on it that exists. `wm-core` starts
        // the drag from the pointer's current offset and moves the
        // client directly.
        let backend = self.wm.backend_mut();
        if let Some(id) = x11_window_id(backend, &window) {
            backend.queue(WmEvent::MoveRequest(id));
        }
    }

    // -- selections -------------------------------------------------------
    // The X11 side of the clipboard bridge; `xdg.rs`'s `SelectionHandler`
    // is the Wayland side. Between them, CLIPBOARD and PRIMARY are one
    // selection each across both protocols, which is the only way a
    // session that runs urxvt and a native editor side by side can feel
    // like one desktop.
    //
    // Only selections. Drag-and-drop across the boundary is a separate
    // negotiation (an XDND handshake driven from pointer grabs) and is
    // deliberately not attempted here.

    fn allow_selection_access(&mut self, xwm: XwmId, _selection: SelectionTarget) -> bool {
        // The X clipboard has no focus rule of its own: any X client can
        // ask the selection owner for the data at any time, and if we
        // always said yes, a background X process could read whatever a
        // Wayland client had copied without the user ever interacting
        // with it. Gating on "an X11 window from this XWM currently
        // holds the keyboard" is the same rule the Wayland protocols
        // enforce on their own clients (`set_selection` requires focus),
        // applied to the one client — Xwayland — that speaks for many.
        let Some(keyboard) = self.seat.get_keyboard() else {
            return false;
        };
        let Some(focused) = keyboard.current_focus() else {
            return false;
        };
        let backend = self.wm.backend();
        let Some(window) = backend.window_for_surface(&focused) else {
            return false;
        };
        match backend.windows.get(&window).map(|record| &record.surface) {
            // The XWM identity is checked, not just "is X11": one
            // process could in principle manage a second Xwayland
            // instance, and a selection request from that one must not
            // be answered because a window of this one has focus.
            Some(ManagedSurface::X11(surface)) => surface.xwm_id() == Some(xwm),
            _ => false,
        }
    }

    fn send_selection(
        &mut self,
        _xwm: XwmId,
        selection: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
    ) {
        // An X client is pasting something a Wayland client owns. The
        // owning client writes into `fd` itself (that is what
        // `wl_data_source.send` does), so this hands the descriptor
        // straight over and returns; Xwayland is already waiting to read
        // the other end.
        //
        // Logged per arm rather than through a shared `Result`: the two
        // helpers return same-named but distinct `SelectionRequestError`
        // types, one per selection module. Either way a failure here is
        // routine — X clients probe TEXT, STRING and UTF8_STRING in turn
        // and the owner rarely offers all three — so it is a debug line,
        // not a warning.
        match selection {
            SelectionTarget::Clipboard => {
                if let Err(error) = request_data_device_client_selection(&self.seat, mime_type, fd)
                {
                    tracing::debug!(?error, "no Wayland clipboard data for an X11 paste");
                }
            }
            SelectionTarget::Primary => {
                if let Err(error) = request_primary_client_selection(&self.seat, mime_type, fd) {
                    tracing::debug!(?error, "no Wayland primary selection for an X11 paste");
                }
            }
        }
    }

    fn new_selection(&mut self, _xwm: XwmId, selection: SelectionTarget, mime_types: Vec<String>) {
        // An X client copied something. Installing it as a
        // *compositor-provided* selection is what makes it visible to
        // Wayland clients: they see the offer and its mime types now,
        // and the bytes are fetched lazily through
        // `SelectionHandler::send_selection` if anyone actually pastes.
        // Copying an X selection eagerly would mean an X round-trip per
        // copy for data nobody may ever ask for — and X clients copy on
        // every text selection.
        let display_handle = self.display_handle.clone();
        match selection {
            SelectionTarget::Clipboard => {
                set_data_device_selection(&display_handle, &self.seat, mime_types, ())
            }
            SelectionTarget::Primary => {
                set_primary_selection(&display_handle, &self.seat, mime_types, ())
            }
        }
    }

    fn cleared_selection(&mut self, _xwm: XwmId, selection: SelectionTarget) {
        // The X client that owned the selection dropped it (or exited).
        // Only OUR selection is cleared — the user-data check asks "is
        // the current selection one the compositor installed", i.e. one
        // that came from X. Skipping it would let a disappearing xterm
        // wipe a selection a Wayland client had set in the meantime,
        // since Xwayland reports the X-side loss either way.
        let display_handle = self.display_handle.clone();
        match selection {
            SelectionTarget::Clipboard => {
                if current_data_device_selection_userdata(&self.seat).is_some() {
                    clear_data_device_selection(&display_handle, &self.seat);
                }
            }
            SelectionTarget::Primary => {
                if current_primary_selection_userdata(&self.seat).is_some() {
                    clear_primary_selection(&display_handle, &self.seat);
                }
            }
        }
    }
}

impl Compositor {
    /// The `_NET_WM_STATE` translation `wm-x11` does for X11 client
    /// messages — the XWM already decoded action and property, so this
    /// just re-queues the same shape against the shared id space.
    fn queue_x11_net_state(
        &mut self,
        window: &X11Surface,
        action: NetStateAction,
        first: NetState,
        second: Option<NetState>,
    ) {
        let backend = self.wm.backend_mut();
        if let Some(id) = x11_window_id(backend, window) {
            backend.queue(WmEvent::NetStateRequested { window: id, action, first, second });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The XWM edge table, pinned for the same reason as the xdg one in
    /// `xdg.rs`: a swapped pair resizes the wrong side of the window and
    /// nothing nearer the drag would say why. Total — `_NET_WM_MOVERESIZE`'s
    /// move and keyboard values never reach this enum.
    #[test]
    fn every_x11_resize_edge_maps_to_its_compass_point() {
        assert_eq!(wm_resize_edge(X11ResizeEdge::Top), ResizeEdge::North);
        assert_eq!(wm_resize_edge(X11ResizeEdge::Bottom), ResizeEdge::South);
        assert_eq!(wm_resize_edge(X11ResizeEdge::Left), ResizeEdge::West);
        assert_eq!(wm_resize_edge(X11ResizeEdge::Right), ResizeEdge::East);
        assert_eq!(wm_resize_edge(X11ResizeEdge::TopLeft), ResizeEdge::NorthWest);
        assert_eq!(wm_resize_edge(X11ResizeEdge::TopRight), ResizeEdge::NorthEast);
        assert_eq!(wm_resize_edge(X11ResizeEdge::BottomLeft), ResizeEdge::SouthWest);
        assert_eq!(wm_resize_edge(X11ResizeEdge::BottomRight), ResizeEdge::SouthEast);
    }
}
