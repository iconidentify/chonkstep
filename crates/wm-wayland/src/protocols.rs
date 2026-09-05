//! The wlr protocol surface: what external tools talk to.
//!
//! The X11 session answers pagers, bars, screenshot tools and remote
//! desktop through EWMH — `_NET_CLIENT_LIST`, `_NET_ACTIVE_WINDOW`,
//! `_NET_WM_STATE` and the X server's own `XGetImage`. A Wayland
//! compositor has none of that for free: a client that is not told
//! about a protocol cannot use it, so without this module waybar has
//! no window list, `wlogout`-style pagers have nothing to click, and
//! `grim`/`wf-recorder`/OBS find no way to read a single pixel. Two
//! protocols close that gap:
//!
//! - **`zwlr_foreign_toplevel_management_v1`** (v3) — every window
//!   `wm-core` manages, with its title, app id, per-output visibility,
//!   parent, and state (activated/maximized/minimized/fullscreen), kept
//!   in step with the ledger, plus the requests a taskbar sends back.
//! - **`zwlr_screencopy_v1`** (v3, `wl_shm` buffers) — frame capture of
//!   any output or sub-region of one.
//!
//! # Which of these Smithay ships
//!
//! Neither. Smithay 0.7 implements *some* wlr protocols
//! (`wlr-layer-shell` as `wayland::shell::wlr_layer`, `wlr-data-control`
//! as `wayland::selection::wlr_data_control`) and it implements the
//! *successor* to foreign-toplevel-management —
//! `ext-foreign-toplevel-list-v1`, in `wayland::foreign_toplevel_list`
//! — but that one is a read-only *list*: it carries no state, no
//! outputs, and no requests, so a taskbar built on it can show window
//! titles and nothing else. Neither wlr protocol needed here exists in
//! the crate.
//!
//! Both are therefore implemented directly against
//! `wayland-protocols-wlr`, which Smithay re-exports
//! (`smithay::reexports::wayland_protocols_wlr`) — so this costs no new
//! dependency and no `Cargo.toml` change. The `Dispatch`/`GlobalDispatch`
//! impls are written for [`Compositor`] itself rather than delegated,
//! because there is exactly one state type in this process (see
//! `lib.rs`) and a delegate would only add a layer of indirection over
//! it. `wayland::foreign_toplevel_list`'s source is the shape these
//! follow; the differences are all protocol, not plumbing.
//!
//! # Integration contract
//!
//! One field, one init call, one call per dispatch pass. Copy-pasteable,
//! all of it in `state.rs`:
//!
//! 1. `Compositor` gains one field:
//!
//!    ```ignore
//!    /// The wlr protocol surface external tools bind: the
//!    /// foreign-toplevel window list and screencopy capture.
//!    pub(crate) protocols: crate::protocols::ProtocolState,
//!    ```
//!
//! 2. In `run`, one line beside the `dmabuf` one and for the same
//!    reason — a global that is missing when a client binds might as
//!    well not exist — so it must land *before* the
//!    `ListeningSocketSource` is inserted:
//!
//!    ```ignore
//!    let protocols = crate::protocols::init(&display_handle);
//!    ```
//!
//!    then move that local into the `Compositor { .. }` literal as
//!    `protocols,`.
//!
//! 3. In `Compositor::dispatch_pending`, one line, immediately after
//!    `self.apply_pending_focus();` and **before** the
//!    `if self.wm.backend().damage` test:
//!
//!    ```ignore
//!    crate::protocols::refresh(self);
//!    ```
//!
//!    The placement is load-bearing in both directions. After the event
//!    drain, the notification drain and `Shell::tick`, so what it
//!    publishes is the settled state of the pass rather than a
//!    half-applied one. Before the damage test, because
//!    `render_frame` clears `WaylandBackend::damage` and screencopy's
//!    `copy_with_damage` — the request a screen recorder uses to avoid
//!    duplicate frames — is answered exactly on the passes where that
//!    flag is set.
//!
//! ## Why one call and not a notification per event
//!
//! The obvious wiring would be hooks at every point a window appears,
//! disappears, is retitled, or changes focus or state. There is no such
//! set of points: `wm-core` changes `ClientFlags` and `Lifecycle` from a
//! dozen places (`maximize`, `unmaximize`, `shade`, `fullscreen`,
//! `miniaturize`, `focus_client`, the EWMH request handlers, the
//! titlebar buttons, the switcher), none of which this crate owns, and
//! adding a callback to each would put taskbar bookkeeping into the
//! policy brain. So [`refresh`] diffs instead: the compositor marks the
//! retained view dirty at the few event-loop boundaries through which
//! those transitions converge, then compares the settled ledger with
//! what was last sent. One integration point, a window that changes
//! twice in a pass costs one event rather than two, and an unchanged
//! client commit costs no table walk or string cloning. A newly bound
//! manager forces a snapshot independently, so skipping idle passes
//! can never withhold its initial state.
//!
//! # Screencopy: the same capture path as everything else
//!
//! Capture goes through [`crate::capture`]'s approach — build the scene
//! with [`build_scene`], draw it into an offscreen GLES texture, read
//! the pixels back with `ExportMem` — and inherits its three hard-won
//! facts verbatim: no vertical flip is needed (the renderer's baked-in
//! 180° projection and `glReadPixels`' bottom-up order cancel, which is
//! why the `y_invert` flag below is never set), `Fourcc::Abgr8888` is
//! the RGBA byte order, and the pixels come out premultiplied. Read
//! that module's header before touching [`render_offscreen`] here.
//!
//! It differs from `capture.rs` in exactly one thing, and that one
//! thing matters: this applies the source output's transform (see
//! [`capture_region`]), because a screencopy client is handed the
//! output's *buffer* and un-transforms it itself, whereas a screenshot
//! PNG is looked at directly and wants logical orientation.
//!
//! The other reason it does not simply *call* `capture.rs` is reach:
//! that module's offscreen renderer is a private helper and this file
//! cannot widen it. The duplication should collapse the moment someone
//! owns both files — lift `capture::render_offscreen` to `pub(crate)`,
//! give it the transform argument [`render_offscreen`] here takes, and
//! delete this copy.
//!
//! Only `wl_shm` buffers are offered. The `linux_dmabuf` event that
//! protocol version 3 also allows is deliberately never sent, which
//! clients read as "this compositor does not do dmabuf capture" — the
//! truth, and every consumer that matters (`grim`, `wf-recorder`, OBS's
//! `wlrobs`, the RDP/VNC bridges) implements the shm path because it is
//! the only one guaranteed to exist.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen};
use smithay::input::pointer::CursorImageStatus;
use smithay::output::Output;
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::{
    self, ZwlrForeignToplevelHandleV1,
};
use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::{
    self, ZwlrForeignToplevelManagerV1,
};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_frame_v1::{
    self, ZwlrScreencopyFrameV1,
};
use smithay::reexports::wayland_protocols_wlr::screencopy::v1::server::zwlr_screencopy_manager_v1::{
    self, ZwlrScreencopyManagerV1,
};
use smithay::reexports::wayland_protocols::ext::foreign_toplevel_list::v1::server::ext_foreign_toplevel_handle_v1::ExtForeignToplevelHandleV1;
use smithay::wayland::foreign_toplevel_list::{
    ForeignToplevelHandle, ForeignToplevelListHandler, ForeignToplevelListState,
};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::reexports::wayland_server::{Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource};
use smithay::utils::{Buffer as BufferCoords, Physical, Rectangle as SRect, Size as SSize, Transform};
use smithay::wayland::shm::{with_buffer_contents, with_buffer_contents_mut, BufferData};

use wm_core::{BackendEvent, ClientFlags, Lifecycle, NetState, NetStateAction};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use crate::renderer::{build_scene, SceneElement};
use crate::state::{Compositor, Graphics, ManagedSurface, WaylandBackend, WlFrameId, WlWindowId};

type WmEvent = BackendEvent<WlWindowId, WlFrameId>;

/// Highest `zwlr_foreign_toplevel_manager_v1` we implement. Version 2
/// added the fullscreen state and the fullscreen requests; version 3
/// added the `parent` event, which [`sync_parents`] emits, so nothing
/// here is advertised and then withheld.
const FOREIGN_TOPLEVEL_VERSION: u32 = 3;

/// The `parent` event's `since`. A version-1 or -2 handle must never
/// see it — sending an event a resource's version does not know about
/// is a wire-level protocol violation, not a no-op.
const PARENT_SINCE: u32 = 3;

/// The fullscreen *state* enum entry's `since` (the `state` array is
/// filtered per handle version for exactly this reason).
const FULLSCREEN_STATE_SINCE: u32 = 2;

/// Highest `zwlr_screencopy_manager_v1` we implement. Version 3 adds
/// `buffer_done` and the optional `linux_dmabuf` advertisement; we send
/// the former and, deliberately, never the latter (see the module docs).
const SCREENCOPY_VERSION: u32 = 3;

/// The `buffer_done` event's `since`, guarding the same way
/// [`PARENT_SINCE`] does — a version-1 or -2 frame ends its buffer
/// enumeration by protocol convention, not by an event.
const BUFFER_DONE_SINCE: u32 = 3;

/// Bytes per pixel in every format either side of this module speaks.
/// Both the GLES readback and all four accepted `wl_shm` formats are
/// 32-bit; nothing here is written to survive that changing.
const BYTES_PER_PIXEL: usize = 4;

// ---------------------------------------------------------------------
// The integration surface: one state object, one constructor, one call.
// ---------------------------------------------------------------------

/// Everything the two globals need to keep between dispatch passes.
///
/// Deliberately *not* an `Option`: both globals are registered
/// unconditionally at startup and neither can fail to come up, so a
/// login session's protocol dispatch has no unreachable panic in it —
/// the same call `dmabuf.rs` makes for the same reason.
pub(crate) struct ProtocolState {
    /// Standard, read-only toplevel list used by the ext capture-source
    /// factory. The wlr management protocol below remains available for
    /// taskbars which also need window-control requests.
    ext_list: ForeignToplevelListState,
    ext_toplevels: HashMap<WlWindowId, ForeignToplevelHandle>,
    /// Live `zwlr_foreign_toplevel_manager_v1` instances. A manager that
    /// sent `stop` is dropped from here (it wants no further *new*
    /// toplevels) while the handles it already owns keep updating,
    /// which is exactly what the request means.
    managers: Vec<ManagerInstance>,
    /// One entry per window `wm-core` manages, holding the handles
    /// advertising it and the last values sent for each — the "what the
    /// taskbar has been told" half of [`refresh`]'s diff.
    toplevels: HashMap<WlWindowId, ToplevelEntry>,
    /// Minimize/unminimize asked for by a taskbar, applied at the top of
    /// [`refresh`]. Unlike the other four requests these have no
    /// `BackendEvent` shape to ride, so they
    /// are recorded here rather than queued on the ledger.
    minimize_requests: Vec<(WlWindowId, bool)>,
    /// Capture requests waiting for a frame to answer them.
    captures: Vec<PendingCapture>,
}

/// Resolve an ext foreign-toplevel resource back to its managed window.
///
/// The mirror of [`window_for_wlr_toplevel`] for the frozen protocol,
/// and the join key `hyprland-toplevel-mapping-v1` hangs its ext
/// request off. The window id rides in the handle's own user data,
/// planted where the handle is minted ([`sync_ext_toplevels`]), so this
/// is a map lookup rather than a scan — but the answer is still gated
/// on the window being *currently* listed. A handle whose window has
/// closed keeps its inner state alive for as long as a client holds the
/// resource, and resolving one of those would hand out an address for a
/// window that is gone; the wlr arm refuses that case by construction
/// and this one refuses it explicitly.
pub(crate) fn window_for_ext_toplevel(
    state: &ProtocolState,
    handle: &ExtForeignToplevelHandleV1,
) -> Option<WlWindowId> {
    let window = *ForeignToplevelHandle::from_resource(handle)?.user_data().get::<WlWindowId>()?;
    state.ext_toplevels.contains_key(&window).then_some(window)
}

/// Resolve a wlr foreign-toplevel resource back to its managed window.
/// A handle disappears from these vectors on destruction/unmap, so a
/// stale mapping request naturally returns `None`.
pub(crate) fn window_for_wlr_toplevel(
    state: &ProtocolState,
    handle: &ZwlrForeignToplevelHandleV1,
) -> Option<WlWindowId> {
    state
        .toplevels
        .iter()
        .find(|(_, entry)| entry.handles.iter().any(|candidate| candidate == handle))
        .map(|(window, _)| *window)
}

/// One bound foreign-toplevel manager.
struct ManagerInstance {
    resource: ZwlrForeignToplevelManagerV1,
    /// Whether this manager has been handed a handle for every toplevel
    /// that already existed when it bound. Binding cannot do that work
    /// itself — the bind callback runs mid-dispatch, before the pass
    /// that would reconcile the ledger — so it sets this to `false` and
    /// the next [`refresh`] catches the manager up. That also removes
    /// the only way this could go wrong: there is exactly one place
    /// handles are created, so a toplevel can never be announced twice
    /// to the same manager.
    announced: bool,
}

/// What has been published about one window, and to whom.
struct ToplevelEntry {
    /// Every `zwlr_foreign_toplevel_handle_v1` advertising this window —
    /// one per bound manager, across all clients. Pruned by
    /// `Dispatch::destroyed`, drained and closed when the window goes.
    handles: Vec<ZwlrForeignToplevelHandleV1>,
    title: String,
    app_id: String,
    states: ToplevelStates,
    parent: Option<WlWindowId>,
    /// Names of the outputs this window currently overlaps. Names
    /// rather than indices into `Compositor::outputs`: a stale index
    /// would silently name the wrong screen if the output set ever
    /// became dynamic, whereas a name that no longer matches simply
    /// reads as "left that output", which is the truth.
    outputs: Vec<String>,
}

/// The four states this protocol carries, as `wm-core` sees them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct ToplevelStates {
    /// Both axes. `wm-core` maximizes each axis independently and this
    /// protocol has one boolean, so a half-maximized window reads as
    /// unmaximized — which keeps the round trip honest: a taskbar that
    /// then asks for `set_maximized` gets a full maximize, and one that
    /// asks for `unset_maximized` on a full one gets the geometry back.
    maximized: bool,
    /// `Lifecycle::Miniaturized`. Iconifying to a desktop tile, which
    /// is what "minimized" means to every taskbar that will read it.
    minimized: bool,
    activated: bool,
    fullscreen: bool,
}

impl ToplevelStates {
    /// The `state` event's array: native-endian `u32`s, one per active
    /// state, exactly as wlroots encodes it. `version` filters entries
    /// the receiving handle is too old to know — only `fullscreen` is
    /// versioned today.
    fn encode(self, version: u32) -> Vec<u8> {
        use zwlr_foreign_toplevel_handle_v1::State;
        let mut array = Vec::with_capacity(4 * BYTES_PER_PIXEL);
        let mut push = |state: State| array.extend_from_slice(&(state as u32).to_ne_bytes());
        if self.maximized {
            push(State::Maximized);
        }
        if self.minimized {
            push(State::Minimized);
        }
        if self.activated {
            push(State::Activated);
        }
        if self.fullscreen && version >= FULLSCREEN_STATE_SINCE {
            push(State::Fullscreen);
        }
        array
    }
}

/// A capture the client has supplied a buffer for, waiting on a frame.
///
/// Held rather than serviced inline because a `copy` arrives in the
/// middle of protocol dispatch, where the renderer is not ours to
/// borrow and — for `copy_with_damage` — the answer is "not yet"
/// anyway. [`refresh`] drains this once per pass.
#[derive(Clone)]
struct PendingCapture {
    frame: ZwlrScreencopyFrameV1,
    buffer: WlBuffer,
    /// Source rectangle in compositor-global logical coordinates,
    /// already clipped to its output.
    region: Rect,
    /// Its output's transform — see [`capture_region`].
    transform: Transform,
    overlay_cursor: bool,
    /// `copy_with_damage` rather than `copy`: answer only on a pass
    /// where the scene actually changed, so a recorder polling in a
    /// tight loop does not capture the same still frame forever.
    with_damage: bool,
}

/// Per-frame protocol state, parked on the `zwlr_screencopy_frame_v1`
/// resource's user data so its lifetime is exactly the frame's.
pub(crate) struct ScreencopyFrameData {
    /// What this frame will capture, in compositor-global logical
    /// coordinates — fixed when the frame was created, because the
    /// buffer parameters the client allocated against were derived from
    /// it.
    region: Rect,
    /// The transform of the output this region belongs to. See
    /// [`capture_region`]: the buffer a screencopy client is handed is
    /// in the output's *buffer* space, not its logical one.
    transform: Transform,
    overlay_cursor: bool,
    /// Set by the first `copy`/`copy_with_damage`; a second one is the
    /// protocol's `already_used` error. An atomic rather than a `Mutex`
    /// because `Dispatch` hands out only `&UserData` and this is the
    /// single bit that needs to change.
    used: AtomicBool,
}

/// Registers both globals. Called once from `run` — see the module's
/// integration contract for where.
///
/// Never fails, and neither global is gated: a screenshot tool and a
/// bar are ordinary desktop software, and chonkstep runs one user's
/// session rather than a multi-tenant kiosk. A `wp_security_context`
/// filter (Smithay supports one through `GlobalDispatch::can_view`) is
/// the hook to add if sandboxed clients ever need to be told "no".
pub(crate) fn init(display_handle: &DisplayHandle) -> ProtocolState {
    // The `GlobalId`s are dropped deliberately: dropping one does not
    // withdraw the global (that takes `DisplayHandle::remove_global`),
    // and nothing in a session's life withdraws these. Same call
    // `dmabuf.rs` makes with its `DmabufGlobal`.
    let _foreign_toplevel =
        display_handle.create_global::<Compositor, ZwlrForeignToplevelManagerV1, ()>(FOREIGN_TOPLEVEL_VERSION, ());
    let _screencopy = display_handle.create_global::<Compositor, ZwlrScreencopyManagerV1, ()>(SCREENCOPY_VERSION, ());
    tracing::info!(
        foreign_toplevel = FOREIGN_TOPLEVEL_VERSION,
        screencopy = SCREENCOPY_VERSION,
        "wlr protocols advertised"
    );
    ProtocolState {
        ext_list: ForeignToplevelListState::new::<Compositor>(display_handle),
        ext_toplevels: HashMap::new(),
        managers: Vec::new(),
        toplevels: HashMap::new(),
        minimize_requests: Vec::new(),
        captures: Vec::new(),
    }
}

/// Reconciles protocol work with the current session once per dispatch
/// pass. Foreign-toplevel state is snapshotted only after an
/// invalidation (or for a newly bound manager); screencopy requests are
/// still serviced on every pass. See the module's integration contract
/// for why this position matters.
pub(crate) fn refresh(comp: &mut Compositor) {
    apply_minimize_requests(comp);
    comp.notice_wm_protocol_changes();
    let new_manager = comp.protocols.managers.iter().any(|manager| !manager.announced);
    if comp.foreign_toplevel_dirty || new_manager {
        sync_ext_toplevels(comp);
        sync_toplevels(comp);
        comp.foreign_toplevel_dirty = false;
    }
    service_captures(comp);
}

/// Keeps ext-foreign-toplevel-list in step with the same authoritative
/// window ledger as the older wlr management protocol. Capture-source
/// objects recover the compositor window id from each handle's user data.
fn sync_ext_toplevels(comp: &mut Compositor) {
    let snapshots: Vec<(WlWindowId, String, String)> = comp
        .wm
        .clients()
        .map(|(_, client)| {
            let app_id = comp
                .wm
                .backend()
                .windows
                .get(&client.window)
                .and_then(|record| record.app_id.clone())
                .unwrap_or_else(|| client.class.clone());
            (client.window, client.title.clone(), app_id)
        })
        .collect();
    let live: HashSet<WlWindowId> = snapshots.iter().map(|(window, _, _)| *window).collect();

    let ProtocolState { ext_list, ext_toplevels, .. } = &mut comp.protocols;
    ext_toplevels.retain(|window, handle| {
        if live.contains(window) {
            true
        } else {
            handle.send_closed();
            false
        }
    });
    ext_list.cleanup_closed_handles();

    for (window, title, app_id) in snapshots {
        if let Some(handle) = ext_toplevels.get(&window) {
            let changed = handle.title() != title || handle.app_id() != app_id;
            handle.send_title(&title);
            handle.send_app_id(&app_id);
            if changed {
                handle.send_done();
            }
            continue;
        }
        let handle = ext_list.new_toplevel::<Compositor>(title, app_id);
        handle.user_data().insert_if_missing(|| window);
        ext_toplevels.insert(window, handle);
    }
}

impl ForeignToplevelListHandler for Compositor {
    fn foreign_toplevel_list_state(&mut self) -> &mut ForeignToplevelListState {
        &mut self.protocols.ext_list
    }
}

smithay::delegate_foreign_toplevel_list!(Compositor);

// ---------------------------------------------------------------------
// wlr-foreign-toplevel-management
// ---------------------------------------------------------------------

/// Applies the minimize/unminimize requests taskbars sent since the
/// last pass.
///
/// These are the one pair of requests with no `BackendEvent` to ride:
/// EWMH's `_NET_WM_STATE_HIDDEN` is not a request the X11 backend
/// translates either (see `xdg.rs`'s `minimize_request`), so
/// `BackendEvent` has no shape for it and the public verbs are called
/// directly. Both are no-ops when the client is already in the target
/// lifecycle, so a taskbar that double-clicks costs nothing.
///
/// Running here rather than inside the protocol handler is the same
/// deferral `WaylandBackend::pending_focus` documents: a request lands
/// mid-dispatch and takes effect once the pass settles. The visible
/// cost is one dispatch pass: `miniaturize` pushes
/// `Notification::Miniaturized`, and the shell's drain has already run
/// by the time this does, so the icon tile appears on the next frame
/// rather than the one where the window vanished.
fn apply_minimize_requests(comp: &mut Compositor) {
    if comp.protocols.minimize_requests.is_empty() {
        return;
    }
    for (window, minimize) in std::mem::take(&mut comp.protocols.minimize_requests) {
        let Some(id) = comp.wm.client_for_window(window) else {
            continue;
        };
        if minimize {
            comp.wm.miniaturize(id);
        } else {
            comp.wm.deminiaturize(id);
        }
    }
}

/// Updates only the output membership of the one toplevel moving under
/// the pointer. Drag motion cannot change its title, parent or state, so a
/// full O(windows) snapshot here would be pure churn.
pub(crate) fn sync_dragged_toplevel_outputs(comp: &mut Compositor, client: wm_core::ClientId) {
    let Some(client) = comp.wm.client(client) else {
        return;
    };
    if !comp.protocols.toplevels.contains_key(&client.window) {
        // A newly bound manager or newly mapped window is caught by the
        // ordinary full reconciliation later in this pass.
        return;
    }
    comp.protocol_publish_metrics.foreign_toplevel_drag_syncs = comp
        .protocol_publish_metrics
        .foreign_toplevel_drag_syncs
        .wrapping_add(1);
    let Some(entry) = comp.protocols.toplevels.get_mut(&client.window) else {
        // Kept defensive even though the membership check above and this
        // lookup run synchronously in the compositor thread.
        return;
    };

    let screens: Vec<(String, &Output)> = comp
        .outputs
        .iter()
        .map(|setup| (setup.output.name(), &setup.output))
        .collect();
    let current: Vec<String> = comp
        .outputs
        .iter()
        .filter(|setup| overlaps(client.geometry, Rect::new(setup.position, setup.size)))
        .map(|setup| setup.output.name())
        .collect();
    if entry.outputs == current {
        return;
    }

    for name in entry.outputs.iter().filter(|name| !current.contains(name)) {
        for handle in &entry.handles {
            send_output(handle, name, &screens, false);
        }
    }
    for name in current.iter().filter(|name| !entry.outputs.contains(name)) {
        for handle in &entry.handles {
            send_output(handle, name, &screens, true);
        }
    }
    entry.outputs = current;
    for handle in &entry.handles {
        handle.done();
    }
}

/// What one window looks like right now, gathered before anything is
/// sent so the diff below reads the ledger exactly once.
struct ToplevelSnapshot {
    window: WlWindowId,
    title: String,
    app_id: String,
    states: ToplevelStates,
    parent: Option<WlWindowId>,
    outputs: Vec<String>,
}

/// The whole foreign-toplevel reconciliation: snapshot the desktop,
/// close what is gone, catch up managers that just bound, create and
/// update handles, then settle parents.
///
/// Parents come last and in their own pass because a `parent` event
/// carries another *handle*, which must therefore already exist — and a
/// dialog and its parent commonly appear in the same pass.
fn sync_toplevels(comp: &mut Compositor) {
    if comp.protocols.managers.is_empty() && comp.protocols.toplevels.is_empty() {
        // Nobody to publish to and nothing published: the overwhelmingly
        // common case on a desktop with no bar running, and the reason
        // this call is affordable at dispatch rate. A later bind lands
        // in `managers`, and the pass after it walks the ledger in full
        // because every window is then "new".
        return;
    }
    comp.protocol_publish_metrics.foreign_toplevel_full_syncs = comp
        .protocol_publish_metrics
        .foreign_toplevel_full_syncs
        .wrapping_add(1);
    let Compositor { wm, protocols, outputs, display_handle, .. } = comp;
    let ProtocolState { managers, toplevels, .. } = protocols;

    // Output identity, resolved once: `output_enter`/`output_leave`
    // carry a `wl_output` belonging to the receiving client, so every
    // send needs both the name (to diff) and the `Output` (to find that
    // client's resources for it).
    let screens: Vec<(String, &Output)> = outputs.iter().map(|setup| (setup.output.name(), &setup.output)).collect();

    let backend = wm.backend();
    let focused = wm.focused_client();
    let snapshots: Vec<ToplevelSnapshot> = wm
        .clients()
        .map(|(id, client)| {
            let window = client.window;
            ToplevelSnapshot {
                window,
                title: client.title.clone(),
                // The ledger's `app_id`, not `Client::class`: `wm-core`
                // captures the class once at manage time, while a
                // toolkit may set or change `app_id` later, and this
                // protocol's whole job is being current.
                app_id: backend
                    .windows
                    .get(&window)
                    .and_then(|record| record.app_id.clone())
                    .unwrap_or_else(|| client.class.clone()),
                states: ToplevelStates {
                    maximized: client.flags.contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V),
                    minimized: client.lifecycle == Lifecycle::Miniaturized,
                    activated: focused == Some(id),
                    fullscreen: client.flags.contains(ClientFlags::FULLSCREEN),
                },
                parent: parent_window(backend, window),
                // Which screens this window is on, by its content rect
                // — including while miniaturized, since the geometry a
                // deminiaturize would restore it to is still the honest
                // answer to "where does this window live".
                outputs: outputs
                    .iter()
                    .filter(|setup| overlaps(client.geometry, Rect::new(setup.position, setup.size)))
                    .map(|setup| setup.output.name())
                    .collect(),
            }
        })
        .collect();

    // 1. Windows that are gone. `closed` makes the handle inert; the
    //    client destroys it when it is ready, and `Dispatch::destroyed`
    //    would then look for an entry that no longer exists, which it
    //    tolerates.
    let live: HashSet<WlWindowId> = snapshots.iter().map(|snapshot| snapshot.window).collect();
    toplevels.retain(|window, entry| {
        if live.contains(window) {
            return true;
        }
        for handle in entry.handles.drain(..) {
            handle.closed();
        }
        false
    });

    // 2. Managers that bound since the last pass, caught up on
    //    everything that already existed. Done before step 3 so a
    //    toplevel created in this same pass is not yet in `toplevels`
    //    and gets announced there instead — exactly once, either way.
    for manager in managers.iter_mut().filter(|manager| !manager.announced) {
        for (window, entry) in toplevels.iter_mut() {
            announce(&manager.resource, *window, entry, display_handle, &screens);
        }
        manager.announced = true;
    }

    // 3. New and changed windows.
    for snapshot in &snapshots {
        match toplevels.get_mut(&snapshot.window) {
            Some(entry) => update(entry, snapshot, &screens),
            None => {
                let mut entry = ToplevelEntry {
                    handles: Vec::with_capacity(managers.len()),
                    title: snapshot.title.clone(),
                    app_id: snapshot.app_id.clone(),
                    states: snapshot.states,
                    // Settled by step 4, which is where a parent's own
                    // handle is guaranteed to exist.
                    parent: None,
                    outputs: snapshot.outputs.clone(),
                };
                for manager in managers.iter() {
                    announce(&manager.resource, snapshot.window, &mut entry, display_handle, &screens);
                }
                toplevels.insert(snapshot.window, entry);
            }
        }
    }

    // 4. Parents.
    sync_parents(toplevels, &snapshots);
}

/// Creates one handle for one manager and sends the window's complete
/// current state on it. The only place a handle is ever minted.
fn announce(
    manager: &ZwlrForeignToplevelManagerV1,
    window: WlWindowId,
    entry: &mut ToplevelEntry,
    display_handle: &DisplayHandle,
    screens: &[(String, &Output)],
) {
    let Some(client) = manager.client() else {
        return;
    };
    let handle = match client.create_resource::<ZwlrForeignToplevelHandleV1, WlWindowId, Compositor>(
        display_handle,
        manager.version(),
        window,
    ) {
        Ok(handle) => handle,
        Err(error) => {
            tracing::warn!(?error, "could not create a foreign-toplevel handle");
            return;
        }
    };
    // The `toplevel` event must reach the client before anything on the
    // handle it announces.
    manager.toplevel(&handle);
    handle.title(entry.title.clone());
    handle.app_id(entry.app_id.clone());
    for name in &entry.outputs {
        send_output(&handle, name, screens, true);
    }
    handle.state(entry.states.encode(handle.version()));
    handle.done();
    entry.handles.push(handle);
}

/// Sends only what changed about an already-announced window, then one
/// `done` to close the atomic batch. Nothing at all when nothing moved,
/// which is the common case on most passes.
fn update(entry: &mut ToplevelEntry, snapshot: &ToplevelSnapshot, screens: &[(String, &Output)]) {
    let mut changed = false;

    if entry.title != snapshot.title {
        entry.title = snapshot.title.clone();
        for handle in &entry.handles {
            handle.title(entry.title.clone());
        }
        changed = true;
    }
    if entry.app_id != snapshot.app_id {
        entry.app_id = snapshot.app_id.clone();
        for handle in &entry.handles {
            handle.app_id(entry.app_id.clone());
        }
        changed = true;
    }
    if entry.outputs != snapshot.outputs {
        for name in entry.outputs.iter().filter(|name| !snapshot.outputs.contains(name)) {
            for handle in &entry.handles {
                send_output(handle, name, screens, false);
            }
        }
        for name in snapshot.outputs.iter().filter(|name| !entry.outputs.contains(name)) {
            for handle in &entry.handles {
                send_output(handle, name, screens, true);
            }
        }
        entry.outputs = snapshot.outputs.clone();
        changed = true;
    }
    if entry.states != snapshot.states {
        entry.states = snapshot.states;
        for handle in &entry.handles {
            handle.state(entry.states.encode(handle.version()));
        }
        changed = true;
    }

    if changed {
        for handle in &entry.handles {
            handle.done();
        }
    }
}

/// Emits `parent` for every window whose parent changed.
///
/// Split from [`update`] because the event's argument is the *parent's*
/// handle owned by the *same client*, so every handle in the pass has
/// to exist first. A window whose parent this client cannot see gets an
/// explicit `None`, which the protocol spells as "no parent" — the only
/// honest answer available.
fn sync_parents(toplevels: &mut HashMap<WlWindowId, ToplevelEntry>, snapshots: &[ToplevelSnapshot]) {
    for snapshot in snapshots {
        let unchanged = toplevels.get(&snapshot.window).is_some_and(|entry| entry.parent == snapshot.parent);
        if unchanged {
            continue;
        }
        // Cloned out of the map so the child's entry can be borrowed
        // mutably below; handles are cheap refcounted resource handles.
        let parent_handles: Vec<ZwlrForeignToplevelHandleV1> = snapshot
            .parent
            .and_then(|parent| toplevels.get(&parent))
            .map(|entry| entry.handles.clone())
            .unwrap_or_default();

        let Some(entry) = toplevels.get_mut(&snapshot.window) else {
            continue;
        };
        entry.parent = snapshot.parent;
        for handle in &entry.handles {
            if handle.version() < PARENT_SINCE {
                continue;
            }
            let parent = parent_handles.iter().find(|candidate| same_client(*candidate, handle));
            handle.parent(parent);
            handle.done();
        }
    }
}

/// Sends one `output_enter`/`output_leave` for every `wl_output` the
/// receiving client holds for `name`. A client can bind the same output
/// more than once and the protocol is per-object, so all of them hear
/// about it — the same fan-out wlroots does.
///
/// A client that has not bound the `wl_output` yet hears nothing, and
/// the entry still records the output as entered, so it is not told
/// later either. That costs nothing in practice: this runs from
/// [`refresh`], after a whole dispatch batch has been processed, so any
/// client that asked for the outputs in the same registry round trip as
/// the manager already has them — and one that never binds `wl_output`
/// has no use for the event. wlroots has the same shape, with a
/// per-output bind listener papering over it.
fn send_output(handle: &ZwlrForeignToplevelHandleV1, name: &str, screens: &[(String, &Output)], entering: bool) {
    let Some((_, output)) = screens.iter().find(|(candidate, _)| candidate == name) else {
        // The output went away between the snapshot and here, or has
        // already been dropped from `Compositor::outputs`. There is
        // nothing to name in the event, and the client's own
        // `wl_output` removal tells it the same thing.
        return;
    };
    let Some(client) = handle.client() else {
        return;
    };
    for wl_output in output.client_outputs(&client) {
        if entering {
            handle.output_enter(&wl_output);
        } else {
            handle.output_leave(&wl_output);
        }
    }
}

/// Whether two resources belong to the same client. A dead resource has
/// no client and matches nothing, which is the correct answer for the
/// one place this is used (choosing a parent handle to name).
fn same_client<A: Resource, B: Resource>(a: &A, b: &B) -> bool {
    match (a.client(), b.client()) {
        (Some(a), Some(b)) => a.id() == b.id(),
        _ => false,
    }
}

/// The managed window this one is a child of, if any.
///
/// xdg only. XWayland's `WM_TRANSIENT_FOR` is not plumbed through
/// `wm-core` (nothing in the WM's policy reads it yet), so an X11
/// dialog reports no parent rather than a guessed one.
fn parent_window(backend: &WaylandBackend, window: WlWindowId) -> Option<WlWindowId> {
    let record = backend.windows.get(&window)?;
    let ManagedSurface::Xdg(toplevel) = &record.surface else {
        return None;
    };
    backend.window_for_surface(&toplevel.parent()?)
}

/// Whether two rectangles share any area. Half-open on both far edges,
/// matching `Rect::contains`.
fn overlaps(a: Rect, b: Rect) -> bool {
    a.pos.x < far_edge(b.pos.x, b.size.w)
        && b.pos.x < far_edge(a.pos.x, a.size.w)
        && a.pos.y < far_edge(b.pos.y, b.size.h)
        && b.pos.y < far_edge(a.pos.y, a.size.h)
}

/// One rectangle edge, saturating. A `u32` extent is wider than `i32`
/// can hold and both of these arrive from clients, so the arithmetic
/// has to clamp rather than wrap — an overflowing `+` here is a panic
/// in a debug build and a rectangle inside out in a release one.
fn far_edge(origin: i32, extent: u32) -> i32 {
    origin.saturating_add(extent.min(i32::MAX as u32) as i32)
}

impl GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> for Compositor {
    fn bind(
        state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        let resource = data_init.init(resource, ());
        // Not announced here: see `ManagerInstance::announced` for why
        // the next `refresh` is the only place handles are minted.
        state.protocols.managers.push(ManagerInstance { resource, announced: false });
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Request::Stop = request {
            // "No further toplevels", not "no further events": the
            // handles this manager already owns stay live and keep
            // updating until the client destroys them.
            state.protocols.managers.retain(|manager| &manager.resource != resource);
            resource.finished();
        }
        // The enum is `#[non_exhaustive]`; a request from a future
        // version we did not advertise cannot reach us, and panicking
        // in a login session's dispatch is never the right answer.
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrForeignToplevelManagerV1,
        _data: &(),
    ) {
        state.protocols.managers.retain(|manager| &manager.resource != resource);
    }
}

impl Dispatch<ZwlrForeignToplevelHandleV1, WlWindowId> for Compositor {
    /// A taskbar acting on a window.
    ///
    /// Four of the six actions are queued as the `BackendEvent` their
    /// EWMH twin produces, so a click in waybar and a `wmctrl` command
    /// under X11 reach `wm-core` through byte-identical code — the same
    /// translation discipline `xdg.rs` follows for xdg-shell's own
    /// maximize/fullscreen requests. Minimize is the exception and is
    /// deferred to `apply_minimize_requests`.
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        window: &WlWindowId,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Request;

        if matches!(request, Request::Destroy) {
            return;
        }
        // The protocol makes a handle inert the moment its window is
        // gone: every request but `destroy` must be ignored, and the
        // entry disappearing is exactly that condition.
        if !state.protocols.toplevels.contains_key(window) {
            return;
        }

        let window = *window;
        let backend = state.wm.backend_mut();
        match request {
            Request::SetMaximized => backend.queue(WmEvent::NetStateRequested {
                window,
                action: NetStateAction::Add,
                first: NetState::MaximizedHorz,
                second: Some(NetState::MaximizedVert),
            }),
            Request::UnsetMaximized => backend.queue(WmEvent::NetStateRequested {
                window,
                action: NetStateAction::Remove,
                first: NetState::MaximizedHorz,
                second: Some(NetState::MaximizedVert),
            }),
            // The output hint is dropped: `wm-core` fullscreens a window
            // onto the monitor it is already on, and moving it somewhere
            // else first is a placement decision this protocol has no
            // way to express as one (`xdg.rs`'s `fullscreen_request`
            // drops the same hint for the same reason).
            Request::SetFullscreen { .. } => backend.queue(WmEvent::NetStateRequested {
                window,
                action: NetStateAction::Add,
                first: NetState::Fullscreen,
                second: None,
            }),
            Request::UnsetFullscreen => backend.queue(WmEvent::NetStateRequested {
                window,
                action: NetStateAction::Remove,
                first: NetState::Fullscreen,
                second: None,
            }),
            // The seat argument is dropped because there is one seat and
            // it is the one an activation would use anyway.
            Request::Activate { .. } => backend.queue(WmEvent::ActivateRequested(window)),
            Request::Close => backend.queue(WmEvent::CloseRequested(window)),
            Request::SetMinimized => state.protocols.minimize_requests.push((window, true)),
            Request::UnsetMinimized => state.protocols.minimize_requests.push((window, false)),
            Request::SetRectangle { width, height, .. } if width < 0 || height < 0 => {
                // Where the taskbar draws this window, offered as a hint
                // for a minimize animation. chonkstep miniaturizes to an
                // icon tile the shell places itself, so there is nothing
                // to aim — but the validity check is still owed, since
                // the protocol declares an error for it.
                resource.post_error(
                    zwlr_foreign_toplevel_handle_v1::Error::InvalidRectangle,
                    "set_rectangle with a negative width or height",
                );
            }
            Request::SetRectangle { .. } => {}
            _ => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrForeignToplevelHandleV1,
        window: &WlWindowId,
    ) {
        if let Some(entry) = state.protocols.toplevels.get_mut(window) {
            entry.handles.retain(|handle| handle != resource);
        }
    }
}

// ---------------------------------------------------------------------
// wlr-screencopy
// ---------------------------------------------------------------------

impl GlobalDispatch<ZwlrScreencopyManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrScreencopyManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZwlrScreencopyManagerV1, ()> for Compositor {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _resource: &ZwlrScreencopyManagerV1,
        request: zwlr_screencopy_manager_v1::Request,
        _data: &(),
        _dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_manager_v1::Request;
        match request {
            Request::CaptureOutput { frame, overlay_cursor, output } => {
                let source = output_geometry(&output).map(|(rect, transform, _scale)| (rect, transform));
                new_frame(data_init, frame, source, overlay_cursor != 0);
            }
            Request::CaptureOutputRegion { frame, overlay_cursor, output, x, y, width, height } => {
                // Output-LOCAL coordinates, per the protocol's
                // "region is given in output logical coordinates" and
                // wlroots' own clipping against a box at the output's
                // origin — which is why `grim` subtracts the output's
                // logical position before asking. Translated into the
                // compositor-global space here, since that is what the
                // scene is drawn in — which includes multiplying by the
                // output's advertised scale, because "logical" on the
                // wire is the client's mode-over-scale view of a screen
                // this compositor keeps in device pixels (see
                // [`output_geometry`]).
                //
                // Saturating throughout: every number here came off the
                // wire, and a compositor that a client can overflow into
                // a panic is a client that can end the session.
                let source = output_geometry(&output).and_then(|(output_rect, transform, scale)| {
                    let physical = |v: i32| (v as f64 * scale).round() as i32;
                    let requested = Rect::new(
                        Point::new(
                            output_rect.pos.x.saturating_add(physical(x)),
                            output_rect.pos.y.saturating_add(physical(y)),
                        ),
                        Size::new(physical(width.max(0)) as u32, physical(height.max(0)) as u32),
                    );
                    Some((intersection(requested, output_rect)?, transform))
                });
                new_frame(data_init, frame, source, overlay_cursor != 0);
            }
            Request::Destroy => {}
            _ => {}
        }
    }
}

/// Initializes a freshly requested frame and tells the client what
/// buffer to bring.
///
/// The `New<_>` is initialized on every path, failure included:
/// wayland-server panics on an uninitialized new object, and "this
/// capture cannot happen" is a `failed` event, not a dead compositor.
fn new_frame(
    data_init: &mut DataInit<'_, Compositor>,
    frame: New<ZwlrScreencopyFrameV1>,
    source: Option<(Rect, Transform)>,
    overlay_cursor: bool,
) {
    let (region, transform) = source.unwrap_or((Rect::default(), Transform::Normal));
    let frame =
        data_init.init(frame, ScreencopyFrameData { region, transform, overlay_cursor, used: AtomicBool::new(false) });
    if region.size.w == 0 || region.size.h == 0 {
        // An unknown output, or a region entirely off its edge. Failing
        // now beats advertising a zero-sized buffer the client cannot
        // allocate.
        frame.failed();
        return;
    }

    // Buffer space, not logical space — a rotated or flipped output
    // hands out a buffer with its own axes (see [`capture_region`]).
    let size = buffer_size(region.size, transform);
    // One format offered: the scene is opaque, so the alpha channel of
    // a screen capture carries nothing, and `Xrgb8888` is the format
    // every consumer of this protocol handles. `copy` is lenient about
    // what actually arrives (see `shm_layout`) — advertising one format
    // and accepting four costs nothing and rejects nobody.
    frame.buffer(wl_shm::Format::Xrgb8888, size.w, size.h, size.w.saturating_mul(BYTES_PER_PIXEL as u32));
    // No `linux_dmabuf` event: shm only, deliberately (module docs).
    if frame.version() >= BUFFER_DONE_SINCE {
        frame.buffer_done();
    }
}

impl Dispatch<ZwlrScreencopyFrameV1, ScreencopyFrameData> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &ZwlrScreencopyFrameV1,
        request: zwlr_screencopy_frame_v1::Request,
        data: &ScreencopyFrameData,
        _dhandle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        use zwlr_screencopy_frame_v1::Request;
        let (buffer, with_damage) = match request {
            Request::Copy { buffer } => (buffer, false),
            Request::CopyWithDamage { buffer } => (buffer, true),
            Request::Destroy => {
                state.protocols.captures.retain(|capture| &capture.frame != resource);
                return;
            }
            _ => return,
        };

        if data.used.swap(true, Ordering::Relaxed) {
            resource.post_error(
                zwlr_screencopy_frame_v1::Error::AlreadyUsed,
                "this frame has already been used to copy a buffer",
            );
            return;
        }
        if let Err(reason) = shm_layout(&buffer, buffer_size(data.region.size, data.transform)) {
            // A protocol error rather than `failed`, matching wlroots:
            // the buffer's attributes contradict what this very frame
            // advertised, which is a client bug, and letting it retry
            // forever helps nobody.
            resource.post_error(zwlr_screencopy_frame_v1::Error::InvalidBuffer, reason);
            return;
        }

        state.protocols.captures.push(PendingCapture {
            frame: resource.clone(),
            buffer,
            region: data.region,
            transform: data.transform,
            overlay_cursor: data.overlay_cursor,
            with_damage,
        });
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrScreencopyFrameV1,
        _data: &ScreencopyFrameData,
    ) {
        state.protocols.captures.retain(|capture| &capture.frame != resource);
    }
}

/// Answers every capture that is due this pass.
///
/// `copy` is answered on the next pass regardless; `copy_with_damage`
/// waits for a pass where the scene actually changed, which is what
/// stops a recorder from spinning at dispatch rate over a still
/// desktop. `WaylandBackend::damage` is the compositor's single
/// scene-changed flag, and this runs before `render_frame` clears it.
fn service_captures(comp: &mut Compositor) {
    if comp.protocols.captures.is_empty() {
        return;
    }
    let damaged = comp.wm.backend().damage;
    let mut due: Vec<PendingCapture> = Vec::new();
    comp.protocols.captures.retain(|capture| {
        if !capture.frame.is_alive() {
            return false;
        }
        if capture.with_damage && !damaged {
            return true;
        }
        due.push(capture.clone());
        false
    });

    for capture in due {
        let Some(pixels) = capture_region(comp, capture.region, capture.transform, capture.overlay_cursor) else {
            capture.frame.failed();
            continue;
        };
        if let Err(error) = write_capture(&capture.buffer, &pixels) {
            tracing::warn!(%error, "screencopy could not write into the client's buffer");
            capture.frame.failed();
            continue;
        }
        // Never y-inverted: `capture.rs` explains why the renderer's
        // baked-in flip and `glReadPixels`' bottom-up order cancel.
        capture.frame.flags(zwlr_screencopy_frame_v1::Flags::empty());
        if capture.with_damage {
            // The whole region, because that is the truth: this
            // compositor repaints the full scene every frame on purpose
            // (see `renderer`'s module docs), so there is no smaller
            // damage rectangle to report.
            capture.frame.damage(0, 0, pixels.width, pixels.height);
        }
        // Monotonic since session start. The protocol allows "an
        // arbitrary offset at start" precisely so a compositor can use
        // its own presentation clock, and this is the one every other
        // timestamp in this process is measured against.
        let stamp = comp.start_time.elapsed();
        capture.frame.ready(
            (stamp.as_secs() >> 32) as u32,
            (stamp.as_secs() & 0xFFFF_FFFF) as u32,
            stamp.subsec_nanos(),
        );
    }
}

/// The region of the compositor-global scene this output covers and
/// the transform it advertises, or `None` for a `wl_output` that is not
/// one of ours (or has no mode yet).
///
/// Derived from the `Output` itself rather than from
/// `Compositor::outputs`, because the client named a specific
/// `wl_output` and `Output::from_resource` answers exactly that
/// question — no index to keep in step with anything.
/// The third element is the output's advertised scale: the factor
/// between the protocol's "output logical coordinates" (what a client
/// like `grim` measures a region in, having read `wl_output.scale`)
/// and the device pixels this compositor's scene — and therefore the
/// returned rectangle — is in. The mode is *not* divided by it here:
/// doing so is exactly how the first scale-advertising build handed
/// `grim` a quarter-size capture of a scale-2 session, because the
/// scene never shrank to match the story the protocol tells clients.
fn output_geometry(output: &WlOutput) -> Option<(Rect, Transform, f64)> {
    let output = Output::from_resource(output)?;
    let mode = output.current_mode()?;
    let transform = output.current_transform();
    let scale = output.current_scale().fractional_scale();
    let size = transform.transform_size(mode.size);
    if size.w <= 0 || size.h <= 0 {
        return None;
    }
    let location = output.current_location();
    Some((Rect::new(Point::new(location.x, location.y), Size::new(size.w as u32, size.h as u32)), transform, scale))
}

/// The size of the buffer a logically-`size` region is captured into.
/// A quarter-turn output swaps the axes; everything else keeps them.
fn buffer_size(size: Size, transform: Transform) -> Size {
    let transformed = transform.transform_size(SSize::<i32, Physical>::from((size.w as i32, size.h as i32)));
    Size::new(transformed.w.max(0) as u32, transformed.h.max(0) as u32)
}

/// The overlapping part of two rectangles, or `None` when they are
/// disjoint.
fn intersection(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.pos.x.max(b.pos.x);
    let top = a.pos.y.max(b.pos.y);
    let right = far_edge(a.pos.x, a.size.w).min(far_edge(b.pos.x, b.size.w));
    let bottom = far_edge(a.pos.y, a.size.h).min(far_edge(b.pos.y, b.size.h));
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect::new(Point::new(left, top), Size::new((right - left) as u32, (bottom - top) as u32)))
}

/// Draws the scene as it stands into an RGBA buffer covering `region`.
///
/// The cursor is composited only when the client asked for it:
/// `CursorImageStatus::Hidden` is the one status [`build_scene`] draws
/// nothing for, so suppressing the pointer needs no special case in the
/// shared scene builder.
///
/// `transform` is the source output's, and applying it is what makes
/// this *not* the same call [`crate::capture`] makes. That module
/// deliberately ignores the output transform, because its output is a
/// PNG a human looks at and wants the right way up; a screencopy client
/// is handed the output's *buffer*, and every one of them (grim,
/// wf-recorder, the OBS plugin) then un-transforms it with the
/// transform `wl_output.geometry` advertised. Skipping it here is
/// therefore not a neutral simplification: it produces captures that
/// are correct on hardware — where `session.rs` sets every output to
/// `Transform::Normal` — and vertically mirrored under the nested
/// backend, whose `Flipped180` exists to square the winit EGL surface's
/// origin with the output's. That is precisely the "correct on hardware,
/// wrong in the preview" failure mode `capture.rs` warns about, arrived
/// at from the other direction, and it was observed before it was
/// argued: `grim` against a nested session came back upside down.
pub(crate) fn capture_region(
    comp: &mut Compositor,
    region: Rect,
    transform: Transform,
    overlay_cursor: bool,
) -> Option<DecorationBuffer> {
    let Compositor { wm, graphics, pointer_location, cursor_status, cursors, .. } = comp;
    let renderer = graphics_renderer(graphics);
    let hidden = CursorImageStatus::Hidden;
    let status = if overlay_cursor { &*cursor_status } else { &hidden };
    let (elements, clear_color) = build_scene(wm.backend(), renderer, *pointer_location, status, cursors, region);
    render_offscreen(renderer, &elements, region.size, transform, clear_color)
}

/// The session's `GlesRenderer`, whichever graphics stack is running.
///
/// `capture.rs` and `dmabuf.rs` each carry these same three lines; none
/// of the three owns the others' file, and the shared helper belongs on
/// `Graphics` itself (in `state.rs`) whenever someone consolidates.
fn graphics_renderer(graphics: &mut Graphics) -> &mut GlesRenderer {
    match graphics {
        Graphics::Winit(backend) => backend.renderer(),
        Graphics::Session(session) => session.renderer(),
    }
}

/// Draws `elements` into a fresh offscreen texture and downloads the
/// result, at scale 1 — the scale every element in this compositor's
/// scene is built at.
///
/// `size` is the region in *logical* coordinates, which is the space
/// `elements` are in; the texture is allocated at the transformed size,
/// and [`OutputDamageTracker`] applies `transform` while drawing
/// (it inverts the output transform internally, exactly as a real
/// output's frame does).
///
/// Otherwise a near-duplicate of `capture::render_offscreen`, for reach
/// rather than for difference: see the module docs for the change that
/// lets this be deleted. The fresh tracker per call, at age 0, is
/// deliberate for the same reason it is there — the target texture is
/// new and entirely stale, and a carried tracker would compute
/// incremental damage against a buffer that no longer exists and skip
/// drawing outright.
fn render_offscreen(
    renderer: &mut GlesRenderer,
    elements: &[SceneElement<GlesRenderer>],
    size: Size,
    transform: Transform,
    clear_color: Color32F,
) -> Option<DecorationBuffer> {
    let target = buffer_size(size, transform);
    if target.w == 0 || target.h == 0 {
        return None;
    }
    let width = target.w as i32;
    let height = target.h as i32;

    let mut texture: GlesTexture =
        match renderer.create_buffer(Fourcc::Abgr8888, SSize::<i32, BufferCoords>::from((width, height))) {
            Ok(texture) => texture,
            Err(error) => {
                tracing::warn!(?error, width, height, "could not allocate a screencopy buffer");
                return None;
            }
        };
    let mut framebuffer = match renderer.bind(&mut texture) {
        Ok(framebuffer) => framebuffer,
        Err(error) => {
            tracing::warn!(?error, "could not bind the screencopy buffer");
            return None;
        }
    };

    let mut damage_tracker = OutputDamageTracker::new(SSize::<i32, Physical>::from((width, height)), 1.0, transform);
    if let Err(error) = damage_tracker.render_output(renderer, &mut framebuffer, 0, elements, clear_color) {
        tracing::warn!(?error, "screencopy render failed");
        return None;
    }

    let region = SRect::from_size(SSize::<i32, BufferCoords>::from((width, height)));
    let mapping = match renderer.copy_framebuffer(&framebuffer, region, Fourcc::Abgr8888) {
        Ok(mapping) => mapping,
        Err(error) => {
            tracing::warn!(?error, "could not read back the screencopy buffer");
            return None;
        }
    };
    let pixels = match renderer.map_texture(&mapping) {
        Ok(pixels) => pixels.to_vec(),
        Err(error) => {
            tracing::warn!(?error, "could not map the captured pixels");
            return None;
        }
    };
    Some(DecorationBuffer { width: target.w, height: target.h, pixels })
}

/// Validates a client buffer against what this frame advertised and
/// returns its layout.
///
/// Strict about the dimensions (they are what the client was told to
/// allocate) and about the format being one this code can write.
/// Lenient about the stride: a toolkit that pads its rows is not
/// wrong, and honouring the stride it reports costs nothing.
fn shm_layout(buffer: &WlBuffer, expected: Size) -> Result<BufferData, String> {
    let data =
        with_buffer_contents(buffer, |_, _, data| data).map_err(|error| format!("not a wl_shm buffer ({error})"))?;
    if data.width != expected.w as i32 || data.height != expected.h as i32 {
        return Err(format!(
            "buffer is {}x{}, this frame advertised {}x{}",
            data.width, data.height, expected.w, expected.h
        ));
    }
    if data.stride < data.width.saturating_mul(BYTES_PER_PIXEL as i32) {
        return Err(format!("stride {} is short for a {}px row", data.stride, data.width));
    }
    if pixel_layout(data.format).is_none() {
        return Err(format!("unsupported buffer format {:?}", data.format));
    }
    Ok(data)
}

/// How one of the accepted `wl_shm` formats stores a pixel, relative to
/// the RGBA byte order the GLES readback produces:
/// `(swap red and blue, force the fourth byte opaque)`.
///
/// The `x` variants have no alpha channel, and while its byte is
/// nominally ignored, plenty of consumers treat the buffer as ARGB
/// anyway — writing `0xFF` there rather than whatever the scene's alpha
/// happened to be is what keeps a screenshot from opening
/// semi-transparent.
fn pixel_layout(format: wl_shm::Format) -> Option<(bool, bool)> {
    match format {
        wl_shm::Format::Argb8888 => Some((true, false)),
        wl_shm::Format::Xrgb8888 => Some((true, true)),
        wl_shm::Format::Abgr8888 => Some((false, false)),
        wl_shm::Format::Xbgr8888 => Some((false, true)),
        _ => None,
    }
}

/// Copies a captured frame into the client's shm buffer, converting
/// byte order as the buffer's format requires.
///
/// Both sides are premultiplied — the GLES frame blends
/// `ONE, ONE_MINUS_SRC_ALPHA` over a cleared buffer, and `wl_shm`'s
/// `argb8888` is defined premultiplied — so nothing but the channel
/// order changes.
pub(crate) fn write_capture(buffer: &WlBuffer, capture: &DecorationBuffer) -> Result<(), String> {
    let data = shm_layout(buffer, Size::new(capture.width, capture.height))?;
    let (swap_rb, opaque) = pixel_layout(data.format).ok_or("unsupported buffer format")?;
    let width = capture.width as usize;
    let height = capture.height as usize;
    let row_bytes = width * BYTES_PER_PIXEL;
    let stride = data.stride as usize;
    let offset = data.offset.max(0) as usize;

    with_buffer_contents_mut(buffer, |ptr, len, _| {
        // The last row needs only its own bytes, not a full stride —
        // accepting that is what lets a client allocate exactly
        // `(h - 1) * stride + w * 4`.
        let needed = offset.saturating_add(height.saturating_sub(1).saturating_mul(stride)).saturating_add(row_bytes);
        if len < needed {
            return Err(format!("buffer holds {len} bytes, the capture needs {needed}"));
        }
        for y in 0..height {
            let source = &capture.pixels[y * row_bytes..y * row_bytes + row_bytes];
            // SAFETY: `ptr`/`len` describe the client's mapped pool for
            // the duration of this closure (that is the contract of
            // `with_buffer_contents_mut`, which also traps SIGBUS on a
            // pool the client shrank). The bounds check above proves
            // every row this loop forms lies inside `len`, and the
            // slice is dropped before the next iteration, so no two
            // live slices ever alias.
            let destination = unsafe { std::slice::from_raw_parts_mut(ptr.add(offset + y * stride), row_bytes) };
            let (source_pixels, source_remainder) = source.as_chunks::<BYTES_PER_PIXEL>();
            let (destination_pixels, destination_remainder) = destination.as_chunks_mut::<BYTES_PER_PIXEL>();
            debug_assert!(source_remainder.is_empty() && destination_remainder.is_empty());
            for (source, destination) in source_pixels.iter().zip(destination_pixels) {
                let alpha = if opaque { 0xFF } else { source[3] };
                if swap_rb {
                    *destination = [source[2], source[1], source[0], alpha];
                } else {
                    *destination = [source[0], source[1], source[2], alpha];
                }
            }
        }
        Ok(())
    })
    .map_err(|error| format!("could not map the client's buffer ({error})"))?
}

#[cfg(test)]
mod tests {
    use super::*;

    // The protocol halves of this module need a wayland display and a
    // client on the other end of it, and the capture half needs an EGL
    // context; both are exercised by running a session and pointing
    // `grim` and a bar at it. What is unit-testable is the arithmetic
    // and the encodings those two halves get wrong silently.

    #[test]
    fn the_state_array_is_native_endian_u32s_in_enum_order() {
        let states = ToplevelStates { maximized: true, minimized: false, activated: true, fullscreen: true };
        let mut expected = Vec::new();
        expected.extend_from_slice(&0u32.to_ne_bytes()); // maximized
        expected.extend_from_slice(&2u32.to_ne_bytes()); // activated
        expected.extend_from_slice(&3u32.to_ne_bytes()); // fullscreen
        assert_eq!(states.encode(3), expected);
    }

    #[test]
    fn a_version_one_handle_never_hears_about_fullscreen() {
        // The enum entry is `since = 2`; sending it to an older handle
        // would name a state that version of the protocol has no word
        // for.
        let states = ToplevelStates { maximized: false, minimized: false, activated: false, fullscreen: true };
        assert!(states.encode(1).is_empty());
        assert_eq!(states.encode(2), 3u32.to_ne_bytes().to_vec());
    }

    #[test]
    fn no_state_at_all_encodes_to_an_empty_array() {
        assert!(ToplevelStates::default().encode(3).is_empty());
    }

    #[test]
    fn a_region_is_clipped_to_its_output() {
        let output = Rect::new(Point::new(1920, 0), Size::new(1920, 1080));
        // Fully inside.
        assert_eq!(
            intersection(Rect::new(Point::new(2000, 100), Size::new(100, 100)), output),
            Some(Rect::new(Point::new(2000, 100), Size::new(100, 100)))
        );
        // Hanging off the right and bottom edges.
        assert_eq!(
            intersection(Rect::new(Point::new(3800, 1000), Size::new(200, 200)), output),
            Some(Rect::new(Point::new(3800, 1000), Size::new(40, 80)))
        );
        // Entirely on the neighbouring screen.
        assert_eq!(intersection(Rect::new(Point::new(0, 0), Size::new(1920, 1080)), output), None);
        // Touching an edge is not overlapping it.
        assert_eq!(intersection(Rect::new(Point::new(1820, 0), Size::new(100, 100)), output), None);
    }

    #[test]
    fn overlap_is_half_open_on_both_far_edges() {
        let screen = Rect::new(Point::new(0, 0), Size::new(1920, 1080));
        assert!(overlaps(Rect::new(Point::new(1919, 1079), Size::new(10, 10)), screen));
        assert!(!overlaps(Rect::new(Point::new(1920, 0), Size::new(10, 10)), screen));
        assert!(!overlaps(Rect::new(Point::new(0, 1080), Size::new(10, 10)), screen));
        // A window straddling two screens is on both of them.
        let right = Rect::new(Point::new(1920, 0), Size::new(1920, 1080));
        let straddling = Rect::new(Point::new(1800, 100), Size::new(400, 400));
        assert!(overlaps(straddling, screen));
        assert!(overlaps(straddling, right));
    }

    #[test]
    fn only_a_quarter_turn_output_swaps_the_buffer_axes() {
        let region = Size::new(1280, 800);
        // The two transforms this compositor actually produces: the
        // session backend's, and the nested backend's EGL-surface
        // correction. Neither changes the buffer's dimensions — which
        // is exactly why a missing transform showed up as a mirrored
        // capture rather than a size mismatch.
        assert_eq!(buffer_size(region, Transform::Normal), region);
        assert_eq!(buffer_size(region, Transform::Flipped180), region);
        assert_eq!(buffer_size(region, Transform::_90), Size::new(800, 1280));
        assert_eq!(buffer_size(region, Transform::Flipped270), Size::new(800, 1280));
    }

    #[test]
    fn the_accepted_shm_formats_map_to_the_right_channel_order() {
        // The readback is RGBA; `argb8888`/`xrgb8888` are BGRA in
        // memory and therefore need the swap, the `bgr` pair does not.
        assert_eq!(pixel_layout(wl_shm::Format::Argb8888), Some((true, false)));
        assert_eq!(pixel_layout(wl_shm::Format::Xrgb8888), Some((true, true)));
        assert_eq!(pixel_layout(wl_shm::Format::Abgr8888), Some((false, false)));
        assert_eq!(pixel_layout(wl_shm::Format::Xbgr8888), Some((false, true)));
        // Anything else is refused rather than written wrong.
        assert_eq!(pixel_layout(wl_shm::Format::Rgb565), None);
    }
}
