//! EWMH root-property publishing for the XWayland display — the reason
//! `wmctrl -l`, `xdotool`, pagers and taskbars see the same desktop
//! under the compositor that they see under the native X11 session.
//!
//! # Why a second X connection
//!
//! This process already speaks X to Xwayland — that is what `X11Wm`
//! is — but smithay owns that connection outright: its event queue is
//! drained by smithay's own calloop source, and there is no public
//! handle for arbitrary property writes on it. So this module opens
//! the compositor's *own* ordinary x11rb client connection once
//! XWayland reports ready, exactly as the XSETTINGS manager does (see
//! `Compositor::start_xsettings` for the longer version of the same
//! argument). Root properties don't require being the window manager;
//! any client may set them.
//!
//! # Inbound control, the two messages that matter
//!
//! Publishing alone left a pager able to *see* everything and change
//! nothing: its `_NET_ACTIVE_WINDOW` and `_NET_CURRENT_DESKTOP`
//! ClientMessages went nowhere. Those messages are sent to the *root*
//! with the `SubstructureRedirect | SubstructureNotify` event mask
//! (the spec's spelling), and while only smithay's XWM may hold
//! SubstructureRedirect, any number of clients may select
//! SubstructureNotify — so this connection selects it at connect time
//! and [`flush`] drains the resulting events (non-blocking, once per
//! pass) before writing. The two messages translate into the exact
//! `BackendEvent`s the Wayland-native paths already queue
//! (`ActivateRequested` from wlr-foreign-toplevel and xdg-activation,
//! `DesktopSwitchRequested` from the shell's pager), so an X pager
//! and a Wayland taskbar can never disagree about what "activate"
//! means. Everything else SubstructureNotify carries (Map/Configure
//! notifies of root children) is ignored in the same drain.
//!
//! # Two writers, one root
//!
//! Smithay's XWM already writes a few of these properties itself:
//! `_NET_CLIENT_LIST` (appended per map, rewritten per destroy),
//! `_NET_CLIENT_LIST_STACKING`, `_NET_ACTIVE_WINDOW` (from raw
//! FocusIn/FocusOut), and a startup `_NET_SUPPORTED` /
//! `_NET_SUPPORTING_WM_CHECK` naming "Smithay X WM". This module's
//! REPLACE writes land after smithay's in every steady state — the
//! check window and `_NET_SUPPORTED` are rewritten once at connect
//! time (after `start_wm`, so ours win and stay), and the per-change
//! properties are republished from `wm-core`'s authoritative ledger on
//! every change, so any interleaving with smithay's own writes
//! converges on ours by the next flush. The one known wart: smithay
//! rewrites `_NET_ACTIVE_WINDOW` on raw focus events, which can
//! briefly resurrect a stale value after our write; it is corrected on
//! the next focus change and no tool observed cares about the window
//! in between.
//!
//! # Record now, act later
//!
//! The `Backend::publish_*` verbs run inside the `WindowManager`'s
//! `&mut self` and must stay non-blocking, and they can fire before
//! XWayland (let alone this connection) exists. So they only write
//! into [`EwmhLedger`] on the backend — the same record-now/act-later
//! pattern as `pending_focus` — and `Compositor::dispatch_pending`
//! flushes the dirty state to the wire via [`flush`] once per pass,
//! after the event drains, so external tools always read a settled
//! desktop.

use std::collections::HashMap;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt, CreateWindowAux, PropMode, WindowClass, Window as XWindow,
};
use x11rb::rust_connection::RustConnection;
// `change_property32`/`change_property8` live on the wrapper trait,
// distinct from the request-level `xproto::ConnectionExt` above —
// both are needed, hence the alias.
use x11rb::wrapper::ConnectionExt as WrapperConnectionExt;

use wm_theme_api::Rect;

use crate::state::{Compositor, ManagedSurface, WaylandBackend, WindowRecord, WlWindowId};

/// What this compositor's `_NET_SUPPORTED` advertises, by atom name.
///
/// Kept textually identical to `wm-x11`'s `EwmhAtoms::supported()`
/// wherever both stacks implement the hint — the two crates cannot
/// share the list without a new shared dependency neither wants, so
/// the pairing is by convention: change one, check the other
/// (`crates/wm-x11/src/backend.rs`). The deliberate differences:
///
/// * `_NET_CLOSE_WINDOW` is omitted — `wm-x11` handles that client
///   message itself; on this stack root client messages land on
///   smithay's XWM connection, which does not translate it, so
///   advertising it would invite messages nobody handles.
/// * `_NET_WM_STATE_SHADED` is omitted — `wm-core` shades, but
///   nothing on this stack can publish the atom onto a client window
///   (smithay's `X11Surface` owns `_NET_WM_STATE` and has no shaded
///   setter; a second writer would race its whole-property rewrites)
///   and smithay's XWM accepts no shade requests either.
/// * `_NET_WM_STATE_MODAL`, `_NET_WM_STATE_FOCUSED` and
///   `_NET_CLIENT_LIST_STACKING` are *added*: smithay's XWM maintains
///   those itself on this display, and our REPLACE of
///   `_NET_SUPPORTED` must not retract what it advertised.
const SUPPORTED: &[&str] = &[
    "_NET_SUPPORTED",
    "_NET_SUPPORTING_WM_CHECK",
    "_NET_ACTIVE_WINDOW",
    "_NET_CLIENT_LIST",
    "_NET_CLIENT_LIST_STACKING",
    // Advertised because a client checks this list before it will
    // send one: smithay's XWM translates `_NET_WM_MOVERESIZE` into
    // the move/resize requests `xwayland.rs` forwards to `wm-core`.
    "_NET_WM_MOVERESIZE",
    "_NET_WM_STATE",
    "_NET_WM_STATE_FULLSCREEN",
    "_NET_WM_STATE_MAXIMIZED_HORZ",
    "_NET_WM_STATE_MAXIMIZED_VERT",
    "_NET_WM_STATE_HIDDEN",
    "_NET_WM_STATE_MODAL",
    "_NET_WM_STATE_FOCUSED",
    "_NET_WM_WINDOW_TYPE",
    "_NET_WM_WINDOW_TYPE_NORMAL",
    "_NET_WM_WINDOW_TYPE_DIALOG",
    "_NET_WM_WINDOW_TYPE_DESKTOP",
    "_NET_WM_WINDOW_TYPE_DOCK",
    "_NET_WM_WINDOW_TYPE_TOOLBAR",
    "_NET_WM_WINDOW_TYPE_MENU",
    "_NET_WM_WINDOW_TYPE_UTILITY",
    "_NET_WM_WINDOW_TYPE_SPLASH",
    "_NET_WM_WINDOW_TYPE_DROPDOWN_MENU",
    "_NET_WM_WINDOW_TYPE_POPUP_MENU",
    "_NET_WM_WINDOW_TYPE_TOOLTIP",
    "_NET_WM_WINDOW_TYPE_NOTIFICATION",
    "_NET_WM_WINDOW_TYPE_COMBO",
    "_NET_WM_WINDOW_TYPE_DND",
    "_NET_NUMBER_OF_DESKTOPS",
    "_NET_CURRENT_DESKTOP",
    "_NET_WM_DESKTOP",
    "_NET_WORKAREA",
    "_NET_FRAME_EXTENTS",
    "_NET_WM_NAME",
];

/// The buffered EWMH state on [`WaylandBackend`] — latest values plus
/// dirty flags for the root properties, pending per-window entries for
/// the client-window ones. Verbs overwrite (a pager only ever wants
/// the newest value, never a history), so a burst of workspace
/// switches costs one property write, and state recorded before the
/// connection exists is still publishable the moment it does
/// ([`XEwmh::connect`] re-dirties everything).
#[derive(Default)]
pub(crate) struct EwmhLedger {
    /// `wm-core`'s managed order, verbatim — both protocols; the flush
    /// filters to X11 because a `_NET_CLIENT_LIST` naming Wayland
    /// windows would hand tools XIDs that don't exist.
    client_list: Vec<WlWindowId>,
    client_list_dirty: bool,
    /// The focused window (`None` = nothing focused). A focused
    /// *Wayland* window publishes as 0 — to an X11 tool that is
    /// exactly "the active window is not one you can address", the
    /// same value the spec gives "no window".
    active: Option<WlWindowId>,
    active_dirty: bool,
    /// (count, current).
    workspaces: (usize, usize),
    workspaces_dirty: bool,
    /// (union workarea, workspace count) — `None` until the first
    /// `publish_workarea`, which `wm-core` fires during construction.
    workarea: Option<(Rect, usize)>,
    workarea_dirty: bool,
    /// Pending `_NET_WM_DESKTOP` per window, latest value only.
    /// Bounded: keyed by live window ids and pruned by
    /// `WaylandBackend::forget_window`, so a session where this
    /// connection failed cannot grow it past the open window count.
    window_desktops: HashMap<WlWindowId, usize>,
    /// Pending `_NET_FRAME_EXTENTS` per window, in EWMH order
    /// (left, right, top, bottom). Same bounds as `window_desktops`.
    frame_extents: HashMap<WlWindowId, (u32, u32, u32, u32)>,
}

impl EwmhLedger {
    pub(crate) fn note_client_list(&mut self, clients: &[WlWindowId]) {
        self.client_list = clients.to_vec();
        self.client_list_dirty = true;
    }

    pub(crate) fn note_active_window(&mut self, window: Option<WlWindowId>) {
        self.active = window;
        self.active_dirty = true;
    }

    pub(crate) fn note_workspaces(&mut self, count: usize, current: usize) {
        self.workspaces = (count, current);
        self.workspaces_dirty = true;
    }

    pub(crate) fn note_workarea(&mut self, area: Rect, workspace_count: usize) {
        self.workarea = Some((area, workspace_count));
        self.workarea_dirty = true;
    }

    pub(crate) fn note_window_desktop(&mut self, window: WlWindowId, desktop: usize) {
        self.window_desktops.insert(window, desktop);
    }

    pub(crate) fn note_frame_extents(
        &mut self,
        window: WlWindowId,
        left: u32,
        right: u32,
        top: u32,
        bottom: u32,
    ) {
        self.frame_extents.insert(window, (left, right, top, bottom));
    }

    /// Re-flags the client list without new content — the fix for the
    /// one interleaving with smithay's XWM that does NOT converge on
    /// its own. Smithay APPENDs every freshly mapped window to
    /// `_NET_CLIENT_LIST` when the MapNotify reaches its connection,
    /// which is reliably *after* this module's REPLACE (wm-core
    /// manages the window off the map *request*, a step earlier), so
    /// every fresh map left the window listed twice — observed live as
    /// `_NET_CLIENT_LIST(WINDOW): 0x600004, 0x600004` on the first
    /// zenity this was tested with. `xwayland.rs`'s
    /// `map_window_notify` (which smithay calls right after its
    /// append) calls this, and the next flush's REPLACE dedups.
    pub(crate) fn mark_client_list_dirty(&mut self) {
        self.client_list_dirty = true;
    }

    /// Drops everything pending for a window that is gone — called
    /// from `WaylandBackend::forget_window` beside the stacking-slot
    /// cleanup, and for the same reason: nothing else would ever
    /// collect these entries.
    pub(crate) fn prune_window(&mut self, window: WlWindowId) {
        self.window_desktops.remove(&window);
        self.frame_extents.remove(&window);
    }

    /// Re-dirties every root property, so a connection arriving late
    /// (XWayland readiness is asynchronous; `wm-core` published its
    /// startup workspace count and workarea long before it) publishes
    /// the current truth on its first flush. The per-window maps need
    /// no such treatment: an X11 window cannot predate the XWayland
    /// display this connects to.
    fn mark_all_dirty(&mut self) {
        self.client_list_dirty = true;
        self.active_dirty = true;
        self.workspaces_dirty = true;
        self.workarea_dirty = self.workarea.is_some();
    }
}

/// The atoms the flush writes with, interned once at connect — the
/// same idiom (and the same names) as `wm-x11`'s `EwmhAtoms`, cut down
/// to what this side writes directly; everything else is only ever a
/// member of the `_NET_SUPPORTED` payload, which is interned from
/// [`SUPPORTED`] wholesale.
struct WriteAtoms {
    net_supported: Atom,
    net_supporting_wm_check: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
    net_client_list: Atom,
    net_active_window: Atom,
    net_number_of_desktops: Atom,
    net_current_desktop: Atom,
    net_wm_desktop: Atom,
    net_workarea: Atom,
    net_frame_extents: Atom,
}

impl WriteAtoms {
    fn intern(conn: &RustConnection) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            net_supported: conn.intern_atom(false, b"_NET_SUPPORTED")?.reply()?.atom,
            net_supporting_wm_check: conn.intern_atom(false, b"_NET_SUPPORTING_WM_CHECK")?.reply()?.atom,
            net_wm_name: conn.intern_atom(false, b"_NET_WM_NAME")?.reply()?.atom,
            utf8_string: conn.intern_atom(false, b"UTF8_STRING")?.reply()?.atom,
            net_client_list: conn.intern_atom(false, b"_NET_CLIENT_LIST")?.reply()?.atom,
            net_active_window: conn.intern_atom(false, b"_NET_ACTIVE_WINDOW")?.reply()?.atom,
            net_number_of_desktops: conn.intern_atom(false, b"_NET_NUMBER_OF_DESKTOPS")?.reply()?.atom,
            net_current_desktop: conn.intern_atom(false, b"_NET_CURRENT_DESKTOP")?.reply()?.atom,
            net_wm_desktop: conn.intern_atom(false, b"_NET_WM_DESKTOP")?.reply()?.atom,
            net_workarea: conn.intern_atom(false, b"_NET_WORKAREA")?.reply()?.atom,
            net_frame_extents: conn.intern_atom(false, b"_NET_FRAME_EXTENTS")?.reply()?.atom,
        })
    }
}

/// The live connection: an ordinary X client that owns one tiny
/// never-mapped window and writes root (and client-window) properties.
pub(crate) struct XEwmh {
    conn: RustConnection,
    root: XWindow,
    atoms: WriteAtoms,
}

impl XEwmh {
    /// Connects to the freshly-ready XWayland display and performs the
    /// one-time publishing: the supporting-WM-check handshake (a 1x1
    /// InputOnly child of root whose `_NET_WM_NAME` is "chonkstep" —
    /// where `wmctrl -m` gets the name) and `_NET_SUPPORTED`. Both
    /// REPLACE what smithay's XWM wrote at `start_wm` time, which is
    /// the intent: the desktop is chonkstep, not "Smithay X WM", and
    /// the supported list must cover what this desktop actually
    /// publishes. Mirrors `wm-x11`'s connect-time block line for line
    /// where the two do the same thing.
    fn connect(display_number: u32) -> Result<Self, Box<dyn std::error::Error>> {
        // Named explicitly rather than read from `DISPLAY`, for the
        // reason `start_xsettings` documents: this runs inside the
        // handler that sets that variable.
        let display_name = format!(":{display_number}");
        let (conn, screen_num) = RustConnection::connect(Some(&display_name))?;
        let root = conn.setup().roots[screen_num].root;
        let atoms = WriteAtoms::intern(&conn)?;

        let check_window = conn.generate_id()?;
        conn.create_window(
            0, // InputOnly windows must use depth 0 (CopyFromParent)
            check_window,
            root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &CreateWindowAux::new().override_redirect(1),
        )?;
        conn.change_property32(PropMode::REPLACE, root, atoms.net_supporting_wm_check, AtomEnum::WINDOW, &[check_window])?;
        conn.change_property32(PropMode::REPLACE, check_window, atoms.net_supporting_wm_check, AtomEnum::WINDOW, &[check_window])?;
        conn.change_property8(PropMode::REPLACE, check_window, atoms.net_wm_name, atoms.utf8_string, b"chonkstep")?;

        let mut supported = Vec::with_capacity(SUPPORTED.len());
        for name in SUPPORTED {
            supported.push(conn.intern_atom(false, name.as_bytes())?.reply()?.atom);
        }
        conn.change_property32(PropMode::REPLACE, root, atoms.net_supported, AtomEnum::ATOM, &supported)?;
        // Inbound half: pagers address their control ClientMessages to
        // the root with the SubstructureNotify mask set, so selecting
        // it here is what makes them arrive on this connection at all
        // (SubstructureRedirect is smithay's alone; Notify is shared).
        conn.change_window_attributes(
            root,
            &x11rb::protocol::xproto::ChangeWindowAttributesAux::new()
                .event_mask(x11rb::protocol::xproto::EventMask::SUBSTRUCTURE_NOTIFY),
        )?;
        conn.flush()?;

        Ok(Self { conn, root, atoms })
    }

    /// Puts one batch of resolved writes on the wire. Any error kills
    /// the whole batch — the caller drops the connection on it, so
    /// there is no point continuing a batch against a display that is
    /// refusing requests.
    fn apply(&self, writes: &Writes) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(clients) = &writes.client_list {
            self.conn.change_property32(PropMode::REPLACE, self.root, self.atoms.net_client_list, AtomEnum::WINDOW, clients)?;
        }
        if let Some(active) = writes.active {
            // "No (addressable) focused window" is published as window
            // id 0, per the spec — not by deleting the property.
            self.conn.change_property32(PropMode::REPLACE, self.root, self.atoms.net_active_window, AtomEnum::WINDOW, &[active])?;
        }
        if let Some((count, current)) = writes.workspaces {
            self.conn.change_property32(PropMode::REPLACE, self.root, self.atoms.net_number_of_desktops, AtomEnum::CARDINAL, &[count])?;
            self.conn.change_property32(PropMode::REPLACE, self.root, self.atoms.net_current_desktop, AtomEnum::CARDINAL, &[current])?;
        }
        if let Some(values) = &writes.workarea {
            self.conn.change_property32(PropMode::REPLACE, self.root, self.atoms.net_workarea, AtomEnum::CARDINAL, values)?;
        }
        for &(window, desktop) in &writes.window_desktops {
            self.conn.change_property32(PropMode::REPLACE, window, self.atoms.net_wm_desktop, AtomEnum::CARDINAL, &[desktop])?;
        }
        for &(window, extents) in &writes.frame_extents {
            self.conn.change_property32(PropMode::REPLACE, window, self.atoms.net_frame_extents, AtomEnum::CARDINAL, &extents)?;
        }
        self.conn.flush()?;
        Ok(())
    }
}

/// One flush's worth of dirty state, resolved down to XIDs and wire
/// values — everything the write phase needs and nothing that still
/// needs the ledger, so taking it and writing it can borrow different
/// halves of the `Compositor`.
struct Writes {
    client_list: Option<Vec<XWindow>>,
    active: Option<XWindow>,
    workspaces: Option<(u32, u32)>,
    workarea: Option<Vec<u32>>,
    window_desktops: Vec<(XWindow, u32)>,
    frame_extents: Vec<(XWindow, [u32; 4])>,
}

impl Writes {
    fn is_empty(&self) -> bool {
        self.client_list.is_none()
            && self.active.is_none()
            && self.workspaces.is_none()
            && self.workarea.is_none()
            && self.window_desktops.is_empty()
            && self.frame_extents.is_empty()
    }
}

/// The XID behind a managed window, when it has one: X11 surfaces
/// only — a Wayland toplevel has no XID for any X tool to address.
fn x11_id(windows: &HashMap<WlWindowId, WindowRecord>, id: WlWindowId) -> Option<XWindow> {
    match windows.get(&id).map(|record| &record.surface) {
        Some(ManagedSurface::X11(surface)) if surface.alive() => Some(surface.window_id()),
        _ => None,
    }
}

/// The `_NET_WORKAREA` payload: one x,y,w,h quadruple per desktop, all
/// identical — the dock reserves the same strip on every workspace.
/// Kept textually in step with `wm-x11`'s `publish_workarea` (the
/// pairing convention the module doc describes).
fn workarea_values(area: Rect, workspace_count: usize) -> Vec<u32> {
    let mut values = Vec::with_capacity(workspace_count * 4);
    for _ in 0..workspace_count {
        values.extend_from_slice(&[area.pos.x.max(0) as u32, area.pos.y.max(0) as u32, area.size.w, area.size.h]);
    }
    values
}

/// Drains the ledger's dirty state into wire-ready writes. Per-window
/// entries whose window is gone or is not X11 are dropped here — the
/// verbs fire for every managed window, and only the X11 ones have a
/// property to carry the answer.
fn take_writes(backend: &mut WaylandBackend) -> Writes {
    // Disjoint field borrows: the ledger is mutated while the window
    // records are only read for id resolution.
    let ledger = &mut backend.ewmh;
    let windows = &backend.windows;
    Writes {
        client_list: ledger.client_list_dirty.then(|| {
            ledger.client_list_dirty = false;
            ledger
                .client_list
                .iter()
                .filter_map(|&id| x11_id(windows, id))
                .collect()
        }),
        active: ledger.active_dirty.then(|| {
            ledger.active_dirty = false;
            ledger.active.and_then(|id| x11_id(windows, id)).unwrap_or(0)
        }),
        workspaces: ledger.workspaces_dirty.then(|| {
            ledger.workspaces_dirty = false;
            (ledger.workspaces.0 as u32, ledger.workspaces.1 as u32)
        }),
        workarea: (ledger.workarea_dirty && ledger.workarea.is_some()).then(|| {
            ledger.workarea_dirty = false;
            let (area, count) = ledger.workarea.expect("checked above");
            workarea_values(area, count)
        }),
        window_desktops: ledger
            .window_desktops
            .drain()
            .filter_map(|(id, desktop)| Some((x11_id(windows, id)?, desktop as u32)))
            .collect(),
        frame_extents: ledger
            .frame_extents
            .drain()
            .filter_map(|(id, (l, r, t, b))| Some((x11_id(windows, id)?, [l, r, t, b])))
            .collect(),
    }
}

/// Opens the connection at XWayland-ready time. Failure is a logged
/// warning and a session that carries on — the exact posture
/// `start_xsettings` takes, and for the same reason: what is lost is
/// EWMH visibility for X tools, which is precisely what the session
/// had before this existed.
pub(crate) fn start(comp: &mut Compositor, display_number: u32) {
    match XEwmh::connect(display_number) {
        Ok(xewmh) => {
            tracing::info!(display = display_number, "publishing EWMH to the XWayland root");
            comp.xewmh = Some(xewmh);
            // Everything `wm-core` published before the display
            // existed (workspaces, workarea, possibly a client list)
            // goes out on the first flush.
            comp.wm.backend_mut().ewmh.mark_all_dirty();
        }
        Err(error) => {
            tracing::warn!(%error, display = display_number, "could not connect for EWMH publishing; X11 pagers and tools will not see this desktop");
        }
    }
}

/// Flushes dirty EWMH state to the display — called once per
/// `dispatch_pending` pass, after the event and notification drains so
/// tools read the state the desktop just settled into. A connection
/// error drops the connection for good (same one-way stand-down as
/// XSETTINGS): the likely causes are XWayland dying — in which case
/// the whole X11 side is going away anyway — and there is no
/// reconnect story worth a half-working retry loop.
pub(crate) fn flush(comp: &mut Compositor) {
    if comp.xewmh.is_none() {
        return;
    }
    // Inbound before outbound, and before the writes-empty early
    // return: a pager's click must be heard even on a pass where the
    // desktop published nothing.
    drain_inbound(comp);
    let writes = take_writes(comp.wm.backend_mut());
    if writes.is_empty() {
        return;
    }
    let Some(xewmh) = comp.xewmh.as_ref() else {
        return;
    };
    if let Err(error) = xewmh.apply(&writes) {
        tracing::warn!(%error, "EWMH publishing failed; giving up on it for this session");
        comp.xewmh = None;
    }
}

/// Drains this connection's event queue (non-blocking) and translates
/// the two control messages a pager sends — activate this window,
/// switch to this desktop — into the same `BackendEvent`s the
/// Wayland-native request paths queue, so both kinds of tool drive
/// one `wm-core` behavior. Every other event SubstructureNotify
/// delivers is dropped here; this connection redirects nothing and
/// manages nothing.
fn drain_inbound(comp: &mut Compositor) {
    use x11rb::protocol::Event;
    type WmEvent = wm_core::BackendEvent<WlWindowId, crate::state::WlFrameId>;
    // Collected first, applied after: `poll_for_event` borrows the
    // connection inside `comp`, and queueing borrows the backend.
    let mut requests: Vec<WmEvent> = Vec::new();
    {
        let Some(xewmh) = comp.xewmh.as_ref() else {
            return;
        };
        while let Ok(Some(event)) = xewmh.conn.poll_for_event() {
            let Event::ClientMessage(message) = event else {
                continue;
            };
            if message.type_ == xewmh.atoms.net_active_window {
                // The message names an X window; the ledger speaks
                // WlWindowId. A window this compositor is not managing
                // (already gone, or never mapped) is silently ignored,
                // exactly as `wm-x11` ignores stale ids.
                let target = comp
                    .wm
                    .backend()
                    .windows
                    .iter()
                    .find(|(_, record)| match &record.surface {
                        ManagedSurface::X11(surface) => surface.window_id() == message.window,
                        ManagedSurface::Xdg(_) => false,
                    })
                    .map(|(&id, _)| id);
                if let Some(id) = target {
                    requests.push(WmEvent::ActivateRequested(id));
                }
            } else if message.type_ == xewmh.atoms.net_current_desktop {
                let desktop = message.data.as_data32()[0] as usize;
                requests.push(WmEvent::DesktopSwitchRequested(desktop));
            }
        }
    }
    let backend = comp.wm.backend_mut();
    for request in requests {
        backend.queue(request);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::{Point, Size};

    #[test]
    fn the_supported_list_matches_what_this_stack_actually_does() {
        // The verbs this crate publishes and the requests smithay's
        // XWM translates — a tool believes exactly this list, so it
        // has to be honest in both directions.
        for name in [
            "_NET_SUPPORTING_WM_CHECK",
            "_NET_CLIENT_LIST",
            "_NET_ACTIVE_WINDOW",
            "_NET_WM_STATE_FULLSCREEN",
            "_NET_WM_STATE_MAXIMIZED_HORZ",
            "_NET_WM_STATE_MAXIMIZED_VERT",
            "_NET_CURRENT_DESKTOP",
            "_NET_WORKAREA",
            "_NET_FRAME_EXTENTS",
            "_NET_WM_DESKTOP",
        ] {
            assert!(SUPPORTED.contains(&name), "{name} missing from _NET_SUPPORTED");
        }
        // Nothing on this stack handles a close message or can
        // publish/accept the shaded atom (see SUPPORTED's doc) —
        // advertising either would be a lie a client acts on.
        assert!(!SUPPORTED.contains(&"_NET_CLOSE_WINDOW"));
        assert!(!SUPPORTED.contains(&"_NET_WM_STATE_SHADED"));
    }

    #[test]
    fn the_workarea_repeats_one_rect_per_desktop() {
        // `_NET_WORKAREA`'s format is one quadruple per desktop with
        // no sharing — a pager indexes it by desktop number, so a
        // three-desktop session with a short list would read garbage
        // for desktop 2.
        let area = Rect { pos: Point::new(0, 32), size: Size::new(1600, 968) };
        let values = workarea_values(area, 3);
        assert_eq!(values.len(), 12);
        assert_eq!(&values[0..4], &[0, 32, 1600, 968]);
        assert_eq!(&values[4..8], &values[8..12]);
    }

    #[test]
    fn a_negative_workarea_origin_clamps_rather_than_wrapping() {
        // The property is CARDINAL (unsigned); a negative origin cast
        // raw would publish a ~4-billion-pixel offset. Same clamp as
        // `wm-x11`'s `publish_workarea`.
        let area = Rect { pos: Point::new(-5, -7), size: Size::new(100, 100) };
        assert_eq!(&workarea_values(area, 1)[0..2], &[0, 0]);
    }

    #[test]
    fn ledger_verbs_keep_only_the_newest_value() {
        // Overwrite, not queue: a burst of workspace switches between
        // flushes must publish once, with the final value.
        let mut ledger = EwmhLedger::default();
        ledger.note_workspaces(2, 1);
        ledger.note_workspaces(3, 2);
        assert_eq!(ledger.workspaces, (3, 2));
        ledger.note_window_desktop(WlWindowId(7), 0);
        ledger.note_window_desktop(WlWindowId(7), 2);
        assert_eq!(ledger.window_desktops.get(&WlWindowId(7)), Some(&2));
    }

    #[test]
    fn pruning_a_window_drops_its_pending_properties() {
        // `forget_window` calls this; without it a session whose EWMH
        // connection failed would accumulate one dead entry per X11
        // window it ever managed.
        let mut ledger = EwmhLedger::default();
        ledger.note_window_desktop(WlWindowId(7), 1);
        ledger.note_frame_extents(WlWindowId(7), 1, 1, 24, 1);
        ledger.prune_window(WlWindowId(7));
        assert!(ledger.window_desktops.is_empty());
        assert!(ledger.frame_extents.is_empty());
    }
}
