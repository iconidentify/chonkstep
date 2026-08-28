//! Wayland protocol handlers on [`Compositor`]: wl_compositor/wl_shm/
//! wl_seat/wl_output/wl_data_device plumbing plus the xdg-shell and
//! xdg-decoration mapping that turns client requests into the exact
//! `BackendEvent` shapes `wm-core` already speaks (read alongside
//! `wm-x11`'s `translate_event`/`translate_client_message` — every
//! event queued here mirrors a translation there).
//!
//! The one deliberate divergence from X11's lifecycle: xdg toplevels
//! have no MapRequest of their own. A toplevel "maps" by committing its
//! first buffer after the configure handshake, so `commit` below is
//! where `BackendEvent::MapRequest` is synthesized — and a later
//! null-buffer commit is the protocol's unmap, translated to
//! `BackendEvent::Unmapped` the same way `wm-x11` translates
//! UnmapNotify. `wm-core` cannot tell the difference, by construction.

use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::desktop::PopupKind;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    get_parent, with_states, CompositorClientState, CompositorHandler, CompositorState,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
    ServerDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
    XdgShellState, XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::xwayland::XWaylandClientData;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
};

use wm_core::{BackendEvent, NetState, NetStateAction};
use wm_theme_api::{Rect, Size};

use crate::state::{ClientState, Compositor, ManagedSurface, WindowRecord, WlFrameId, WlWindowId};

type WmEvent = BackendEvent<WlWindowId, WlFrameId>;

/// Per-surface "MapRequest already emitted" marker, parked on the
/// surface's user-data map so its lifetime is exactly the surface's —
/// no cleanup path can forget it. Tracks the mapped/unmapped EDGE:
/// `wm-x11` gets that edge from the server (MapRequest/UnmapNotify),
/// here it is derived from buffer-presence transitions across commits.
#[derive(Default)]
struct MappedMarker(AtomicBool);

fn mapped_marker(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(MappedMarker::default);
        states.data_map.get::<MappedMarker>().unwrap().0.load(Ordering::Relaxed)
    })
}

fn set_mapped_marker(surface: &WlSurface, value: bool) {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(MappedMarker::default);
        states.data_map.get::<MappedMarker>().unwrap().0.store(value, Ordering::Relaxed);
    });
}

/// The size the client actually committed: its declared xdg window
/// geometry when set, else the buffer's logical size. `wm-core` reads
/// this through `Backend::window_geometry` at map time (how big does
/// the fresh client want to be), and `commit` compares it against the
/// record to detect client-side resizes.
fn committed_content_size(surface: &WlSurface) -> Option<Size> {
    let geometry = with_states(surface, |states| {
        let mut guard = states.cached_state.get::<SurfaceCachedState>();
        guard.current().geometry
    });
    if let Some(geometry) = geometry {
        if geometry.size.w > 0 && geometry.size.h > 0 {
            return Some(Size::new(geometry.size.w as u32, geometry.size.h as u32));
        }
    }
    with_renderer_surface_state(surface, |state| state.surface_size())
        .flatten()
        .filter(|size| size.w > 0 && size.h > 0)
        .map(|size| Size::new(size.w as u32, size.h as u32))
}

// -- wl_compositor -------------------------------------------------------

impl CompositorHandler for Compositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // XWayland connects as a client of this compositor too, with
        // smithay's own client-data type rather than ours.
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        panic!("unknown client data type");
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Buffer bookkeeping first — everything below (and the
        // renderer, and the hit-test's subsurface walk) reads the
        // RendererSurfaceState this maintains.
        on_commit_buffer_handler::<Self>(surface);
        self.popups.commit(surface);

        // Role logic runs against the tree's root: a subsurface commit
        // must not re-trigger toplevel lifecycle.
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }

        let toplevel = self
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| *toplevel.wl_surface() == root)
            .cloned();
        if let Some(toplevel) = toplevel {
            self.toplevel_committed(&toplevel);
        } else if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(&root) {
            // A popup's first commit must be answered with its initial
            // configure or the client waits forever.
            if !popup.is_initial_configure_sent() {
                if let Err(error) = popup.send_configure() {
                    tracing::warn!(?error, "initial popup configure failed");
                }
            }
        }

        // Every commit can carry fresh pixels (client repaints are the
        // one damage source no backend verb sees), so the scene
        // redraws. Full-frame, same correctness-first call as the X11
        // side made with picom's --no-use-damage.
        self.wm.backend_mut().mark_damaged();
    }
}

impl Compositor {
    /// The xdg toplevel lifecycle, driven from commits (see the module
    /// doc): initial configure, then buffer-presence edges into
    /// MapRequest/Unmapped, then client-side resize drift into
    /// ConfigureRequest.
    fn toplevel_committed(&mut self, toplevel: &ToplevelSurface) {
        let root = toplevel.wl_surface().clone();
        let initial_configure_sent = with_states(&root, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
        })
        .unwrap_or(true);
        if !initial_configure_sent {
            // The protocol's opening move: the client committed its
            // (buffer-less) role setup and now awaits a configure. Sent
            // with no size — the client picks, and its answer becomes
            // the map-time geometry below.
            toplevel.send_configure();
            return;
        }

        let has_buffer =
            with_renderer_surface_state(&root, |state| state.buffer().is_some()).unwrap_or(false);
        let was_mapped = mapped_marker(&root);
        let committed = committed_content_size(&root);

        let backend = self.wm.backend_mut();
        let Some(id) = backend.window_for_surface(&root) else {
            return;
        };
        if has_buffer && !was_mapped {
            set_mapped_marker(&root, true);
            // Seed the record with the client's own size so
            // `window_geometry` answers the map-time "how big do you
            // want to be" with the truth instead of the fallback.
            if let Some(size) = committed {
                if let Some(record) = backend.windows.get_mut(&id) {
                    record.content.size = size;
                }
            }
            backend.queue(WmEvent::MapRequest(id));
        } else if !has_buffer && was_mapped {
            // Null-buffer commit: the xdg unmap. The toplevel survives
            // and may map again — the marker reset arms the next
            // MapRequest edge, mirroring how an X11 client can unmap
            // and re-map its window under the same WM bookkeeping.
            set_mapped_marker(&root, false);
            // The ledger has to hear it too. `mapped` is what the
            // renderer draws from and what the hit-test routes by, so a
            // record left mapped after its buffer is gone is an
            // invisible rectangle that still swallows clicks - and it
            // outlives the frame, because `wm-core`'s teardown removes
            // the decoration but never touches this flag.
            if let Some(record) = backend.windows.get_mut(&id) {
                record.mapped = false;
            }
            backend.queue(WmEvent::Unmapped(id));
        } else if has_buffer {
            // A managed client committing a size other than the one on
            // record is the Wayland spelling of an X11 self-resize
            // ConfigureRequest (a terminal snapping to its cell grid
            // after our configure, say) — translated to the same event
            // so `wm-core` reflows the decoration around the client's
            // real size. Converges: `wm-core` answers via
            // `resize_client`, which updates the record to match.
            if let (Some(size), Some(record)) = (committed, backend.windows.get(&id)) {
                if record.mapped && size != record.content.size {
                    let requested = Rect { pos: record.content.pos, size };
                    backend.queue(WmEvent::ConfigureRequest { window: id, requested });
                }
            }
        }
    }

    /// Queues a `_NET_WM_STATE`-shaped request — the translation
    /// `wm-x11` does for EWMH client messages, reused for the xdg
    /// requests that mean exactly the same thing.
    fn queue_net_state(
        &mut self,
        surface: &WlSurface,
        action: NetStateAction,
        first: NetState,
        second: Option<NetState>,
    ) {
        let backend = self.wm.backend_mut();
        if let Some(window) = backend.window_for_surface(surface) {
            backend.queue(WmEvent::NetStateRequested { window, action, first, second });
        }
    }
}

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

// -- wl_shm --------------------------------------------------------------

impl ShmHandler for Compositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

// -- wl_output -----------------------------------------------------------

impl OutputHandler for Compositor {
    fn output_bound(&mut self, _output: Output, _wl_output: wl_output::WlOutput) {}
}

// -- wl_seat -------------------------------------------------------------

impl SeatHandler for Compositor {
    // Plain wl_surfaces as focus targets: both xdg toplevels and
    // XWayland's X11 windows resolve to one (see `ManagedSurface`), so
    // no bespoke focus enum is needed — `wm-core` owns focus POLICY,
    // and the backend only ever points the seat at a surface.
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Compositor> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&WlSurface>) {
        // Keyboard focus carries clipboard access with it — without
        // this, paste silently targets whichever client focused first.
        let display_handle = self.display_handle.clone();
        let client = target.and_then(|surface| surface.client());
        set_data_device_focus(&display_handle, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // The renderer composites the pointer itself (there is no
        // hardware cursor plane on the winit backend), so a client
        // changing its cursor is scene damage like any other.
        self.cursor_status = image;
        self.wm.backend_mut().mark_damaged();
    }
}

// -- selections / drag-and-drop ------------------------------------------

impl SelectionHandler for Compositor {
    type SelectionUserData = ();
}

impl DataDeviceHandler for Compositor {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

// Defaults throughout: client-to-client DnD works through the seat's
// own grab machinery; a rendered drag icon is a follow-up for the
// renderer (the icon surface arrives in `started`, unused for now).
impl ClientDndGrabHandler for Compositor {}
impl ServerDndGrabHandler for Compositor {}

// -- xdg-shell -----------------------------------------------------------

impl XdgShellHandler for Compositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Chrome is ours: advertise server-side decorations from the
        // very first configure so toolkits (GTK, Qt) never draw their
        // own titlebars. Clients that skip xdg-decoration entirely get
        // framed regardless — same as every X11 client under a
        // reparenting WM.
        surface.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        // Record now, MapRequest later: the id must exist before the
        // first commit (which may map immediately), but the initial
        // configure is only legal in response to that commit — see
        // `toplevel_committed`.
        let backend = self.wm.backend_mut();
        let id = WlWindowId(backend.alloc_id());
        backend.windows.insert(
            id,
            WindowRecord::new(ManagedSurface::Xdg(surface), Rect::default()),
        );
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        // Position straight from the positioner, parent-relative — the
        // renderer and the hit-test both resolve it against the
        // parent's content rect via `PopupManager`. No unconstraining
        // pass yet: NeXTSTEP-style menus hug their parent, and a
        // popup off the output edge is the client's own placement to
        // fix. (Initial configure happens at first commit.)
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
        });
        if let Err(error) = self.popups.track_popup(PopupKind::from(surface)) {
            tracing::warn!(?error, "failed to track xdg popup");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // Not granted: smithay's popup-grab machinery needs a focus
        // type convertible from popups (`KeyboardFocus: From<PopupKind>`),
        // which the deliberately-plain `WlSurface` focus above isn't.
        // Menus still open, render, and take pointer input through the
        // ordinary hit-test; the cost is no popup_done auto-dismissal
        // on outside clicks (toolkits dismiss on their own focus/click
        // handling). Strictly the protocol prefers dismissing an
        // ungranted grab — deliberately not done, since insta-closing
        // every menu is uselessness dressed up as compliance. Revisit
        // with a dedicated focus-target enum if it bites.
    }

    fn move_request(&mut self, _surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        // A client-initiated interactive move (CSD titlebar drag).
        // Under server-side decorations moves start from OUR titlebar,
        // which `wm-core` runs off frame button events — there is no
        // BackendEvent shape for a client asking, exactly as there was
        // none for X11's `_NET_WM_MOVERESIZE` (which `wm-x11` also
        // drops).
    }

    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        // Same rationale as `move_request`.
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // One request toggling both axes — byte-for-byte the
        // `_NET_WM_STATE` maximize message shape `wm-x11` translates.
        self.queue_net_state(
            surface.wl_surface(),
            NetStateAction::Add,
            NetState::MaximizedHorz,
            Some(NetState::MaximizedVert),
        );
        // The protocol demands a configure in reply whether or not the
        // request is honored; `wm-core`'s own reconfigure follows once
        // it acts on the queued event.
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.queue_net_state(
            surface.wl_surface(),
            NetStateAction::Remove,
            NetState::MaximizedHorz,
            Some(NetState::MaximizedVert),
        );
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<wl_output::WlOutput>,
    ) {
        // Single output today — the output hint has nothing to select.
        self.queue_net_state(surface.wl_surface(), NetStateAction::Add, NetState::Fullscreen, None);
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.queue_net_state(
            surface.wl_surface(),
            NetStateAction::Remove,
            NetState::Fullscreen,
            None,
        );
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn minimize_request(&mut self, _surface: ToplevelSurface) {
        // Miniaturization is a WM gesture (titlebar button, keybinding)
        // with no request-shaped path into `wm-core` — the X11 backend
        // had no `_NET_WM_STATE_HIDDEN` request translation either.
        // Ignoring is protocol-legal: minimize has no required reply.
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        // Same reason this event exists on X11: most apps set their
        // real title well after mapping (see
        // `BackendEvent::TitleChanged`'s doc), so the titlebar must
        // repaint on the property change, not the map.
        let title = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().title.clone())
        });
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            if let Some(record) = backend.windows.get_mut(&id) {
                // Keep the record's cache current per its contract
                // (`WindowRecord::title`'s doc in state.rs).
                record.title = title;
            }
            backend.queue(WmEvent::TitleChanged(id));
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        let app_id = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().app_id.clone())
        });
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            if let Some(record) = backend.windows.get_mut(&id) {
                record.app_id = app_id;
            }
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // The record goes immediately (`wm-x11` drops `known_clients`
        // on DestroyNotify the same way); `wm-core`'s teardown then
        // calls backend verbs that all tolerate the missing window.
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            backend.windows.remove(&id);
            backend.queue(WmEvent::Destroyed(id));
            backend.mark_damaged();
        }
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        // PopupManager prunes dead popups itself (the loop calls its
        // `cleanup`); the scene just needs a repaint without it.
        self.wm.backend_mut().mark_damaged();
    }
}

// -- xdg-decoration ------------------------------------------------------
// The policy is one line long: this desktop draws the chrome, always —
// the chiseled frames are the whole point, and a client drawing its own
// titlebar under one would wear two hats. Clients keep asking for
// ClientSide (GTK does, universally); the answer is configured
// ServerSide every time, which the protocol explicitly allows the
// compositor to impose.

impl XdgDecorationHandler for Compositor {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        // No configure here: if this races the initial commit, the
        // initial configure carries the mode; otherwise request_mode/
        // unset_mode follow immediately and send one.
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            let _ = toplevel.send_pending_configure();
        }
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        if toplevel.is_initial_configure_sent() {
            let _ = toplevel.send_pending_configure();
        }
    }
}

delegate_compositor!(Compositor);
delegate_shm!(Compositor);
delegate_output!(Compositor);
delegate_seat!(Compositor);
delegate_data_device!(Compositor);
delegate_xdg_shell!(Compositor);
delegate_xdg_decoration!(Compositor);
