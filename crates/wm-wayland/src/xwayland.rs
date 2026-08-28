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

use smithay::delegate_xwayland_shell;
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::xwayland_shell::{XWaylandShellHandler, XWaylandShellState};
use smithay::xwayland::xwm::{
    Reorder, ResizeEdge as X11ResizeEdge, WmWindowProperty, X11Window, XwmId,
};
use smithay::xwayland::{X11Surface, X11Wm, XwmHandler};

use wm_core::{BackendEvent, NetState, NetStateAction};
use wm_theme_api::{Point, Rect, Size};

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
    backend.windows.insert(
        id,
        WindowRecord::new(ManagedSurface::X11(window.clone()), wm_rect(window.geometry())),
    );
    id
}

fn wm_rect(rect: Rectangle<i32, Logical>) -> Rect {
    Rect {
        pos: Point::new(rect.loc.x, rect.loc.y),
        size: Size::new(rect.size.w.max(0) as u32, rect.size.h.max(0) as u32),
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
        if let Some(record) = backend.windows.get_mut(&id) {
            record.content = wm_rect(window.geometry());
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
        if let Some(record) = backend.windows.get_mut(&id) {
            record.content = wm_rect(window.geometry());
        }
        backend.queue(WmEvent::MapRequest(id));
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
        backend.windows.remove(&id);
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
            backend.queue(WmEvent::ConfigureRequest { window: id, requested: wm_rect(requested) });
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
            if let Some(record) = backend.windows.get_mut(&id) {
                record.content = wm_rect(geometry);
            }
            backend.mark_damaged();
        }
    }

    fn property_notify(&mut self, _xwm: XwmId, window: X11Surface, property: WmWindowProperty) {
        // The title watch `wm-x11` keeps via PropertyNotify on
        // WM_NAME/_NET_WM_NAME — smithay already filtered to the
        // interesting properties and parsed the value.
        if property == WmWindowProperty::Title {
            let backend = self.wm.backend_mut();
            if let Some(id) = x11_window_id(backend, &window) {
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
        _window: X11Surface,
        _button: u32,
        _resize_edge: X11ResizeEdge,
    ) {
        // `_NET_WM_MOVERESIZE`-style client-initiated drags: no
        // BackendEvent shape exists and `wm-x11` dropped these client
        // messages too — interactive move/resize is driven from our own
        // chrome.
    }

    fn move_request(&mut self, _xwm: XwmId, _window: X11Surface, _button: u32) {
        // Same rationale as `resize_request`.
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
