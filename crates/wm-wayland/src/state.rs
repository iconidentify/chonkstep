//! The compositor application's central state and entry point.
//!
//! Two types carry everything, and the split between them is the whole
//! architecture:
//!
//! - [`WaylandBackend`] is what `wm_core::WindowManager<B>` owns as its
//!   `B`. It is a *ledger*, not a connection: plain records of every
//!   window, decoration frame, and shell surface, a bottom-to-top
//!   stacking order, and queues of pending events. The `Backend` trait
//!   verbs (in `backend_impl.rs`) mutate these records and set
//!   [`WaylandBackend::damage`]; nothing here talks to a display
//!   server, because in this process *we are* the display server.
//!
//! - [`Compositor`] is the one calloop data type. Smithay's delegate
//!   macros (`delegate_compositor!`, `delegate_xdg_shell!`, ...)
//!   implement each protocol's dispatch traits for a single concrete
//!   type, so every per-protocol state object, the seat, the outputs,
//!   the winit backend with its GLES renderer, and the
//!   `WindowManager`/`Shell` pair all hang off this struct — there is
//!   no way to split them into separately-owned pieces without
//!   fighting the macros. Protocol handlers (in `xdg.rs`,
//!   `xwayland.rs`, `input.rs`) reach the ledger via
//!   `self.wm.backend_mut()` and queue `BackendEvent`s / records;
//!   [`Compositor::dispatch_pending`] then drains those queues in
//!   exactly the order the X11 binary's event loop drains its backend
//!   (read `crates/chonkstep/src/main.rs` — it is the reference), so
//!   the same `wm-core` brain and the same `chonk-shell` desktop see
//!   the same event discipline on both stacks.
//!
//! [`run`] wires it all together: winit + GLES, the Wayland globals,
//! XWayland, `WindowManager` + `Shell` construction with the same
//! theme/scale precedence as the X11 binary, and the dispatch loop.

use std::collections::{HashMap, VecDeque};
use std::os::fd::{BorrowedFd, RawFd};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::winit::{self, WinitEvent, WinitGraphicsBackend};
use smithay::desktop::PopupManager;
use smithay::input::keyboard::XkbConfig;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatState};
use smithay::output::{Mode, Output, PhysicalProperties, Scale as OutputScale, Subpixel};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode as TriggerMode, PostAction, RegistrationToken};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgToplevelState;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, DisplayHandle, Resource};
use smithay::utils::{Logical, Physical, Transform, SERIAL_COUNTER};
use smithay::utils::{Point as SPoint, Size as SSize};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::selection::primary_selection::PrimarySelectionState;
use smithay::wayland::shell::kde::decoration::KdeDecorationState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::xdg::{ToplevelSurface, XdgShellState};
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent};

use wm_core::{
    Backend, BackendEvent, KeyCombo, MonitorInfo, MouseButton, ScrollDelta, WindowManager,
    WindowType,
};
use wm_theme::{FontState, RasterThemeEngine};
use wm_theme_api::{DecorationBuffer, Point, Rect, ResizeEdge, Size};

use crate::input::DragGrab;

use chonk_shell::dockapp::Farewell;
use chonk_xsettings::{DesktopAppearance, ManagerState, XSettingsError, XSettingsManager};
use chonk_shell::shell::{Shell, ShellOutcome};
use chonk_shell::startup::{ensure_xcursor_size, recovering_from_crash, reload_requested, restart_requested, SessionState};

/// A managed client window (an xdg toplevel or an XWayland surface) in
/// the id space `wm-core` reasons about. Plain integers rather than
/// protocol handles so the ids stay `Copy + Eq + Hash` and the ledger
/// records own the actual handles exactly once.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WlWindowId(pub u64);

/// A decoration frame. On X11 this is a real second window the client
/// gets reparented into; here it is purely a ledger entry — a geometry
/// plus a rendered decoration buffer the renderer composes behind the
/// client's content. Distinct from [`WlWindowId`] for the same reason
/// the X11 backend keeps `XWindow`/`XFrame` distinct: the type system
/// rules out handing the wrong handle to the wrong `Backend` verb.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WlFrameId(pub u64);

/// A shell-owned surface (dock, Clip, launcher strip, icon tiles, menu
/// popups) — the id space `chonk-shell` draws the whole desktop
/// through. On X11 these are override-redirect windows; here they are
/// internal scene elements the renderer composes directly, which is
/// exactly the substitution `wm_core::Backend`'s docs promise the
/// shell cannot observe.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct WlShellId(pub u64);

/// The stand-in for "the root window" in shell-click routing. The X11
/// binary tells root presses apart from shell-surface clicks by
/// comparing against the real root window's XID; a Wayland session has
/// no root window, so the input code queues background presses (a
/// press that hit no shell surface, no frame, and no client) under
/// this sentinel id instead. Id 0 is never allocated —
/// [`WaylandBackend`]'s id counter starts at 1 — so the comparison in
/// [`Compositor::dispatch_pending`] can never collide with a real
/// surface.
pub(crate) const ROOT_SHELL: WlShellId = WlShellId(0);

/// One entry in the bottom-to-top stacking order. Frames and shell
/// surfaces interleave in a single sequence because that is what the
/// X11 server maintains for the X11 backend implicitly: the WM raises
/// and restacks frames *among* the shell's override-redirect windows.
/// The renderer partitions the walk (`above: false` shells, then
/// frames, then `above: true` shells) so docks and menus stay over
/// managed clients exactly as their X11 counterparts do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StackEntry {
    Frame(WlFrameId),
    /// A managed window that owns no frame because its client drew its
    /// own chrome (`wm_core::ClientChrome::ClientDrawn` — Edge,
    /// LibreOffice, anything that sets `_MOTIF_WM_HINTS` to ask not to
    /// be decorated). It sits in the *frame* band, not above it: such
    /// a window is managed in every other respect, so it has to layer
    /// against its framed neighbours and below the dock exactly as a
    /// framed window does. That is the whole reason it needs an entry
    /// of its own — an override-redirect menu is deliberately outside
    /// `stacking` and always on top, and a browser treated the same way
    /// would sit over the dock forever.
    Window(WlWindowId),
    Shell(WlShellId),
}

/// Re-spells one window's place in the frame band without moving it:
/// window slot to frame slot when chrome is created, frame slot to
/// window slot when it is released. Appends on top when there is no
/// slot yet, which is the ordinary first-map case.
///
/// The depth is the whole point. A window that grows or loses a
/// titlebar has not been raised, and re-appending it would put it in
/// front of everything the user had stacked over it — a browser
/// jumping to the foreground because the page went full-screen. It is
/// also the only place the two spellings can be swapped atomically:
/// leaving both in `stacking` draws the client twice, at two depths,
/// and dropping both leaves a mapped window with no slot at all,
/// invisible to the renderer and to the hit-test alike.
pub(crate) fn replace_stack_entry(
    stacking: &mut Vec<StackEntry>,
    old: StackEntry,
    new: StackEntry,
) {
    match stacking.iter().position(|entry| *entry == old) {
        Some(index) => stacking[index] = new,
        None => stacking.push(new),
    }
}

/// Gives `entry` a slot on top if it does not already hold one.
///
/// Idempotent on purpose: a frameless window is mapped again on every
/// return from another workspace and on every deminiaturize, and each
/// of those must keep the depth it had rather than count as a raise.
pub(crate) fn ensure_stack_entry(stacking: &mut Vec<StackEntry>, entry: StackEntry) {
    if !stacking.contains(&entry) {
        stacking.push(entry);
    }
}

/// Moves `entry` to the top of `stacking`. Answers whether it was in
/// there to move, which is every caller's damage test.
///
/// Top of the *vector*, which is top of the entry's own band: whether
/// that lands it over managed frames is decided at paint time from a
/// shell's `above` flag (see the module doc on stacking bands), exactly
/// as an override-redirect window's stacking versus reparented frames
/// is decided on X11.
///
/// All three raise verbs — `raise` on a frame, `raise_frameless` on a
/// window whose client drew its own chrome, `raise_shell_surface` on
/// the dock — are this one function under different names. That is the
/// point rather than tidiness: a framed and a client-decorated window
/// must restack *identically*, and the bug that made `raise_frameless`
/// necessary in the first place was two stacking paths that quietly
/// disagreed, so this one leaves no room for a second pair to.
pub(crate) fn raise_stack_entry(stacking: &mut Vec<StackEntry>, entry: StackEntry) -> bool {
    let Some(index) = stacking.iter().position(|held| *held == entry) else {
        return false;
    };
    let held = stacking.remove(index);
    stacking.push(held);
    true
}

/// The protocol handle behind a managed window. Both kinds flow
/// through the same [`WlWindowId`] space and the same `BackendEvent`
/// translations, which is what lets urxvt (X11, via XWayland) and a
/// native Wayland terminal receive identical management.
pub(crate) enum ManagedSurface {
    Xdg(ToplevelSurface),
    X11(X11Surface),
}

impl ManagedSurface {
    /// The `wl_surface` carrying this window's pixels — what the
    /// renderer draws and the seat focuses. `None` for an XWayland
    /// window whose surface association hasn't arrived yet (X11
    /// windows exist before XWayland binds a `wl_surface` to them; the
    /// renderer and focus plumbing simply skip them until it lands).
    pub(crate) fn wl_surface(&self) -> Option<WlSurface> {
        match self {
            ManagedSurface::Xdg(toplevel) => Some(toplevel.wl_surface().clone()),
            ManagedSurface::X11(surface) => surface.wl_surface(),
        }
    }

    /// Whether the client end of this handle still exists. Dead
    /// handles are skipped rather than drawn — a client can vanish
    /// between a protocol callback and the next render.
    pub(crate) fn alive(&self) -> bool {
        match self {
            ManagedSurface::Xdg(toplevel) => toplevel.alive(),
            ManagedSurface::X11(surface) => surface.alive(),
        }
    }
}

/// Ledger entry for one managed client window. The protocol handlers
/// (`xdg.rs`, `xwayland.rs`) create and update these; `backend_impl.rs`
/// reads them to answer the `Backend` property queries the X11 backend
/// answers with ICCCM round-trips — on Wayland the data is pushed to
/// us (xdg toplevel state, XWayland property events), so the record
/// caches it and the queries become lookups.
pub(crate) struct WindowRecord {
    pub surface: ManagedSurface,
    /// The client content's root-relative rectangle — the analogue of
    /// the client window's geometry inside its X11 frame. Written by
    /// `Backend::configure_client`/`position_client`; read by the
    /// renderer (where to draw the surface tree) and the input code
    /// (chrome vs. content hit-testing).
    pub content: Rect,
    pub mapped: bool,
    /// Cached title (xdg `set_title` / XWayland property events keep
    /// it current and queue `BackendEvent::TitleChanged`).
    pub title: Option<String>,
    /// xdg `app_id` / X11 `WM_CLASS` — what `Backend::window_class`
    /// reports so per-app shell behavior (dock matching, opacity
    /// rules) works identically on both stacks.
    pub app_id: Option<String>,
    /// Decoration policy class, decided at map time exactly as the X11
    /// backend decides it from `_NET_WM_WINDOW_TYPE`: override-redirect
    /// XWayland windows (menus, tooltips) come through as `Unmanaged`
    /// and are rendered as-is with no frame.
    ///
    /// `Unmanaged` is the renderer's and the hit-test's entire test for
    /// "outside `stacking`, above everything", so nothing else may
    /// borrow it. In particular a managed window that has no frame
    /// because its client draws its own chrome stays whatever it is
    /// (usually `Normal`) — it is frameless, not unmanaged, and it
    /// layers by its own [`StackEntry::Window`] slot like any framed
    /// neighbour.
    pub window_type: WindowType,
    /// Most recent preview of this window's contents, refreshed by
    /// [`crate::capture`] while rendering and served back through
    /// `Backend::capture_window_image`. `None` until the first
    /// snapshot (or forever, for a window that never maps), which the
    /// shell's icon and switcher renderers already handle.
    pub snapshot: Option<DecorationBuffer>,
    /// Everything the two decoration protocols have said about this
    /// toplevel — see [`crate::decoration`], which also holds the
    /// policy that reads it.
    ///
    /// The pair of fields this replaced (`negotiated_decoration` and
    /// `requested_client_side`) were written in three places and read
    /// in none: the decision had been moved to an `app_id` prefix list
    /// and the protocol's own answer was being collected and discarded.
    /// A Chrome web-app window asked for server-side decorations, was
    /// told server-side, and was then left unframed because its
    /// `app_id` began with "chrome".
    ///
    /// Always default for XWayland surfaces, which answer the same
    /// question through `_MOTIF_WM_HINTS` instead.
    pub decoration: crate::decoration::DecorationNegotiation,
    /// Where this surface's *window* starts inside its own buffer, from
    /// `xdg_surface.set_window_geometry`.
    ///
    /// A client drawing its own chrome usually draws a drop shadow
    /// around it, and that shadow is part of the buffer while being no
    /// part of the window: GTK declares `set_window_geometry(22, 22,
    /// w, h)` on a buffer 44px wider than `w`. Everything this
    /// compositor positions — the frame, the hit rect, `content` above
    /// — means the *window*, so the surface has to be drawn this much
    /// up and to the left of it for the two to line up. Ignoring it is
    /// what put the window visibly inside its own frame, hanging off
    /// the left of the screen.
    ///
    /// Zero for surfaces that declare no geometry, which is the
    /// overwhelming majority.
    pub content_offset: Point,
    /// The last few content sizes this compositor has *asked* the
    /// client to be (physical, newest last, capped small).
    ///
    /// Exists to answer one question on every commit: "is this the
    /// client obeying us, or the client asking for something?" A commit
    /// whose size matches any recent ask is an echo of our own
    /// configure — possibly a stale one, because clients ack a
    /// configure immediately but draw and commit asynchronously, so
    /// the commit in hand routinely pairs with the ack *before* the
    /// one most recently recorded. Reading `last_acked` at commit time
    /// therefore mis-pairs, and treating those echoes as
    /// client-initiated resizes put the two size authorities into a
    /// sustained ping-pong: maximize → stale old-size commit adopted →
    /// reconfigure → stale new-size commit adopted → forever, observed
    /// live as an alternating 1218/700 configure stream that outlived
    /// the maximize that started it. Membership here is what breaks
    /// the cycle; a size we never asked for (a terminal's cell snap, a
    /// spontaneous client resize) is in no ring and is adopted exactly
    /// as before.
    pub recent_asks: std::collections::VecDeque<Size>,
}

impl WindowRecord {
    /// A fresh record for a surface that just appeared: unmapped, no
    /// cached properties yet — the handlers fill fields in as the
    /// client provides them.
    pub(crate) fn new(surface: ManagedSurface, content: Rect) -> Self {
        Self {
            surface,
            content,
            mapped: false,
            title: None,
            app_id: None,
            window_type: WindowType::Normal,
            snapshot: None,
            decoration: crate::decoration::DecorationNegotiation::default(),
            content_offset: Point::new(0, 0),
            recent_asks: std::collections::VecDeque::new(),
        }
    }
}

/// Ledger entry for one decoration frame. `buffer` holds the imported
/// decoration pixels (`None` until the first `paint_decoration`).
/// No layout is cached here: chrome hit-testing happens in `wm-core`
/// against the client's own theme-authoritative layout, so the
/// backend only needs where the frame is, never what is drawn on it.
pub(crate) struct FrameRecord {
    pub window: WlWindowId,
    pub geometry: Rect,
    pub buffer: Option<MemoryRenderBuffer>,
    pub mapped: bool,
}

/// Ledger entry for one shell surface. `buffer: None` means "never
/// painted yet" — the renderer fills the geometry with `background`
/// then, mirroring the X11 backend's window-background-color behavior
/// between creation and first blit.
pub(crate) struct ShellRecord {
    pub geometry: Rect,
    pub buffer: Option<MemoryRenderBuffer>,
    pub background: (u8, u8, u8),
    pub above: bool,
    pub mapped: bool,
    /// A stable render-element id for the background fill drawn while
    /// `buffer` is `None`. Minted once at creation: the damage tracker
    /// keys element history by id, so a fresh `Id::new()` per frame
    /// reads as "everything old vanished, everything new appeared" —
    /// harmless under full-frame redraws, and exactly what makes
    /// incremental damage useless the moment they stop.
    pub fill_id: smithay::backend::renderer::element::Id,
}

/// The desktop background, as last painted by the shell through
/// `Backend::paint_root_color`/`paint_root_image`. On X11 this becomes
/// a root-window pixmap; here the renderer simply draws it as the
/// scene's bottom layer — same trait verbs, no pixmap machinery.
pub(crate) enum RootBackground {
    Color((u8, u8, u8)),
    Image(MemoryRenderBuffer),
}

/// What the seat's keyboard should hold after the current pass — a
/// window, or deliberately nothing.
///
/// `Nothing` is not a missing feature dressed up as a variant; its
/// absence was a shipped bug. When `wm-core` miniaturized the focused
/// window it set its own `focused = None` and published
/// `active_window(None)`, but the seat kept keyboard focus on the
/// hidden surface and its toplevel kept the `Activated` state. The
/// restore then re-focused the *same* window, so smithay deduplicated
/// the `set_focus` (no `wl_keyboard.enter`) and the unchanged
/// `Activated` produced no configure — the whole cycle was invisible
/// on the wire. A client that had minimized *itself*
/// (`xdg_toplevel.set_minimized` — Edge's own Minimize menu item)
/// was still waiting for a configure to tell it it was unminimized,
/// and until one arrived it discarded every key and click the seat
/// delivered. Clearing the seat and the `Activated` flags at
/// miniaturize time makes the restore a real re-enter and a real
/// state change, which is exactly the wake-up such a client listens
/// for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FocusIntent {
    Window(WlWindowId),
    Nothing,
}

/// The `Backend` the `WindowManager` owns: pure bookkeeping the
/// protocol handlers write into and the renderer reads out of. See the
/// module docs for why no display connection lives here.
pub struct WaylandBackend {
    /// The whole-number scale every `wl_output` in this session
    /// advertises, and therefore the factor smithay applies to every
    /// element it composes.
    ///
    /// Shared allocator for all three id spaces — window, frame, and
    /// shell ids never collide, which makes stray-id bugs loud in logs
    /// instead of silently aliasing. Starts at 1 so [`ROOT_SHELL`]
    /// (id 0) stays forever unallocated.
    next_id: u64,
    pub(crate) windows: HashMap<WlWindowId, WindowRecord>,
    /// Per-application decoration overrides — see
    /// `wm_config::DecorationRules`. Held on the backend because both
    /// the decoration decision (`client_draws_own_chrome`) and the
    /// answers sent on both decoration protocols consult it, and a live
    /// config reload must move all of them at once.
    pub(crate) decoration_rules: wm_config::DecorationRules,
    pub(crate) frames: HashMap<WlFrameId, FrameRecord>,
    pub(crate) shells: HashMap<WlShellId, ShellRecord>,
    /// Bottom-to-top: frames and shell surfaces interleaved — see
    /// [`StackEntry`].
    pub(crate) stacking: Vec<StackEntry>,
    /// Events queued by protocol handlers and backend verbs, drained
    /// by `Backend::poll_event` exactly as the X11 backend's socket is.
    pub(crate) pending: VecDeque<BackendEvent<WlWindowId, WlFrameId>>,
    /// Clicks on shell surfaces (surface, surface-local position,
    /// button, pressed) — plus background presses under [`ROOT_SHELL`].
    /// Drained by `Backend::take_shell_click` for the loop's shell
    /// routing.
    pub(crate) shell_clicks: VecDeque<(WlShellId, Point, MouseButton, bool)>,
    /// Pointer motion over shell surfaces, drained by
    /// `Backend::take_shell_motion` (the shell itself drains this
    /// inside `Shell::on_motion`, same as on X11).
    pub(crate) shell_motions: VecDeque<(WlShellId, Point)>,
    /// Whole wheel notches over shell surfaces (surface,
    /// surface-local position, delta) — plus notches over the desktop
    /// background under [`ROOT_SHELL`], mirroring `shell_clicks`.
    /// Drained by `Backend::take_shell_scroll`.
    ///
    /// Queued rather than summed: a caller reading three separate
    /// one-notch entries and a caller reading one three-notch entry
    /// both behave correctly (the delta is a count), but only the
    /// queue preserves the positions, and on a dock the position is
    /// which tile the user was pointing at when each notch landed.
    pub(crate) shell_scrolls: VecDeque<(WlShellId, Point, ScrollDelta)>,
    /// Output size change waiting for the loop's
    /// `take_screen_resize` drain (the winit window was resized).
    pub(crate) pending_resize: Option<Size>,
    /// Key combos the WM asked to intercept (`Backend::grab_key`).
    /// There is no server to register grabs with — the compositor sees
    /// every key anyway — so "grabbing" is just this filter list the
    /// input code consults before deciding whether a press becomes a
    /// `BackendEvent::KeyPress` or is forwarded to the focused client.
    pub(crate) grabbed_combos: Vec<KeyCombo>,
    /// The modal exclusive grab (`Backend::grab_keyboard`, the Alt-Tab
    /// switcher): while set, *every* press becomes a `KeyPress` and
    /// releases additionally queue `KeyRelease` — see wm-x11's
    /// KeyRelease commentary, mirrored by `input.rs`.
    pub(crate) keyboard_grabbed: bool,
    pub(crate) root_background: RootBackground,
    /// Every output this session drives, in the order `run` discovered
    /// them (connector order on the session backend, one entry on the
    /// nested one) — the primary first. Served verbatim by
    /// `Backend::monitors`, and consulted by the input code to confine
    /// the pointer, so this is the ledger's whole idea of the physical
    /// screen layout.
    pub(crate) monitors: Vec<MonitorInfo>,
    /// The fractional UI scale of each monitor, parallel to
    /// [`WaylandBackend::monitors`] (same indices, same order). On the
    /// ledger rather than on [`Compositor`] because the `Backend` verbs
    /// that convert between a client's logical units and this ledger's
    /// physical ones (`resize_client`, `size_hints`) run inside the
    /// `WindowManager`'s `&mut self` and can reach nothing else — the
    /// standing rule every deferred field above documents. Kept in step
    /// with `Compositor::outputs` by `advertise_scale` and the
    /// output-management apply path; nowhere else writes it.
    ///
    /// Not a field on `MonitorInfo`, deliberately: that type belongs to
    /// `wm-core` and is shared with the X11 backend, where a per-monitor
    /// scale has no meaning (the X session is scaled by rasterizing the
    /// theme larger, not by telling anyone a factor).
    pub(crate) monitor_scales: Vec<f64>,
    /// The union bounding box of [`WaylandBackend::monitors`] — what
    /// `Backend::screen_size` reports, and the space every rect in this
    /// ledger lives in. With one output it is that output's size, which
    /// is why the single-monitor path never notices the distinction.
    pub(crate) output_size: Size,
    /// Set by every mutating verb (paint, map, move, restack, ...);
    /// the renderer clears it after drawing. This is the whole
    /// redraw-scheduling protocol: no damage, no render.
    pub(crate) damage: bool,
    /// Handle to the wayland display, for verbs that must touch
    /// protocol state directly (client credentials for `window_pid`,
    /// disconnecting a client for `kill_client`).
    pub(crate) display_handle: DisplayHandle,
    /// Focus intent recorded by `Backend::set_input_focus` (a window)
    /// or by `publish_active_window(None)` (nothing). The keyboard
    /// lives on the seat, the seat lives on [`Compositor`], and
    /// applying focus needs `&mut Compositor` — which a backend verb,
    /// running inside the `WindowManager`'s `&mut self`, can never
    /// have. So the verb records the intent here and
    /// [`Compositor::dispatch_pending`] applies it after the drain,
    /// the same each-loop cadence X11 focus changes effectively land
    /// on.
    ///
    /// The `Nothing` intent exists because "no window is focused" is a
    /// real state `wm-core` enters (miniaturize, the focused window
    /// closing, a workspace switch away) and the seat must follow it.
    /// See [`FocusIntent`] for the Edge bug that shipped while it
    /// didn't.
    pub(crate) pending_focus: Option<FocusIntent>,
    /// The preview edge the shell hinted through
    /// `Backend::set_preview_edge` — the Overview's card size while a
    /// session is open, `None` the rest of the time. Read by the
    /// capture pass (`crate::capture`), which sizes the per-window
    /// snapshots from it; the reason it is a ledger field and not a
    /// capture-module local is the same `pending_focus` story: the
    /// verb runs inside the `WindowManager`'s `&mut self`, and the
    /// capture pass runs later with the renderer in hand.
    pub(crate) preview_edge: Option<u32>,
    /// Advanced by the capture pass each time it lands snapshots taken
    /// at the hinted [`WaylandBackend::preview_edge`] — the shell polls
    /// it (`Backend::preview_generation`) to learn that previews it
    /// fetched before those captures ran are worth fetching again.
    pub(crate) preview_generation: u64,
    /// A UI scale change `Shell::apply_session_state` has announced,
    /// waiting for [`Compositor::dispatch_pending`] to rebuild the
    /// compositor's own pointer from it.
    ///
    /// Same detour, and the same reason for it, as
    /// [`WaylandBackend::pending_focus`]: the thing that has to change
    /// hangs off [`Compositor`], and a `Backend` verb runs inside the
    /// `WindowManager`'s `&mut self` and can never reach it. What is
    /// particular to this one is *why* the announcement is worth
    /// recording at all rather than acting on where the reload was
    /// noticed — see the drain in `dispatch_pending`.
    pub(crate) pending_cursor_scale: Option<f32>,
    /// The interactive drag currently holding the pointer, if any.
    ///
    /// This *is* `Backend::grab_pointer_for_drag` — there is no server
    /// to ask, so the grab is a fact recorded here and consulted by the
    /// routing in `input.rs`, exactly as [`WaylandBackend::
    /// keyboard_grabbed`] is for the modal keyboard grab. See
    /// [`DragGrab`] for what goes wrong without one.
    pub(crate) pointer_grab: Option<DragGrab>,
    /// A pointer-grab transition waiting for
    /// [`Compositor::dispatch_pending`] to tell the seat about it.
    ///
    /// The same detour, and the same reason for it, as
    /// [`WaylandBackend::pending_focus`]: taking the pointer away from
    /// the client under it and giving it back are both seat operations,
    /// and the `Backend` verbs that decide them run inside the
    /// `WindowManager`'s `&mut self`, which can never reach a seat.
    /// Routing needs no such detour and does not wait for this — the
    /// flag above is live the instant the verb returns.
    pub(crate) pending_pointer_grab: Option<PointerGrabChange>,
    /// Where the pointer is, mirrored from `input.rs` on every motion —
    /// what `Backend::pointer_position` answers with.
    ///
    /// The mirror exists because `wm-core` genuinely cannot remember
    /// this for itself: motion over a client's own content is the
    /// client's and is never queued as a `PointerMotion`, so by the
    /// time a client-decorated window asks to be dragged
    /// (`MoveRequest`), the position `wm-core` last heard can be stale
    /// by the whole width of that window — and the drag began by
    /// teleporting the window by exactly that error on its first
    /// motion. On X11 the backend asks the server (`query_pointer`);
    /// here the compositor is the server, and this field is its answer.
    /// `None` only before the first motion of the session, when there
    /// is no position to be had from anywhere.
    pub(crate) pointer: Option<Point>,
    /// The cursor each frame most recently asked to show, from
    /// `Backend::set_frame_cursor`: an entry per frame whose pointer is
    /// (or was last) over a resize hitbox, absent meaning the plain
    /// arrow. The renderer reads this — through
    /// [`crate::input::pointer_subject`] — to pick the pointer image
    /// while the pointer is over that frame's chrome; on X11 the server
    /// did this from a per-window cursor attribute, and this map is
    /// that attribute's ledger spelling.
    pub(crate) frame_cursors: HashMap<WlFrameId, ResizeEdge>,
    /// Every wlr-layer-shell surface, in creation order (which is the
    /// z-order within a layer band — newest on top). On the ledger,
    /// not the [`Compositor`], because the renderer's scene walk and
    /// the input hit walk read only the ledger, and a surface family
    /// either of them cannot see would be drawn but unclickable or the
    /// reverse. See `layers.rs`.
    pub(crate) layers: Vec<crate::layers::LayerRecord>,
    /// Whether an ext-session-lock holds the session. THE flag the
    /// renderer and the input path branch on: while set, only
    /// [`WaylandBackend::lock_surfaces`] render and receive input —
    /// everything else in this ledger is treated as nonexistent. Set
    /// and cleared only by `lock.rs`'s handler; a locker crashing
    /// clears the surfaces but never this flag (see `lock.rs`'s module
    /// docs for why that asymmetry is the security property).
    pub(crate) locked: bool,
    /// The lock client's surfaces, one per output it has covered.
    pub(crate) lock_surfaces: Vec<crate::lock::LockSurfaceEntry>,
    /// Buffered EWMH publishes waiting for `dispatch_pending` to flush
    /// them to the XWayland root — the record-now/act-later detour the
    /// `Backend::publish_*` verbs take for the reason `pending_focus`
    /// documents: the connection (when it exists at all) hangs off
    /// [`Compositor`], which a backend verb can never reach. See
    /// `xewmh.rs`.
    pub(crate) ewmh: crate::xewmh::EwmhLedger,
}

/// Which way a drag grab just moved, for the seat-side half that
/// [`crate::input::apply_pointer_grab_change`] performs.
pub(crate) enum PointerGrabChange {
    Taken,
    Released,
}

impl WaylandBackend {
    /// Whether this window's client draws its own chrome, from what it
    /// has actually told us — the decoration protocols first, and a
    /// `[decorations]` override above them.
    ///
    /// The whole policy lives in [`crate::decoration`]; this is the
    /// lookup that feeds it the record's evidence and identity. The
    /// KDE-manager bind is a property of the *client*, not the surface,
    /// so it is folded in here rather than duplicated onto every
    /// record the client owns.
    pub(crate) fn xdg_client_draws_own_chrome(&self, record: &WindowRecord) -> bool {
        crate::decoration::client_draws_own_chrome(
            &self.decoration_rules,
            record.app_id.as_deref(),
            self.decoration_evidence(record),
        )
    }

    /// This record's negotiation, with `kde_manager_bound` resolved
    /// against the live client set.
    pub(crate) fn decoration_evidence(&self, record: &WindowRecord) -> crate::decoration::DecorationEvidence {
        let mut negotiation = record.decoration;
        negotiation.kde_manager_bound = self.client_bound_kde_decoration(record);
        negotiation.evidence()
    }

    /// Whether the client owning this surface has bound the KDE
    /// decoration manager.
    fn client_bound_kde_decoration(&self, record: &WindowRecord) -> bool {
        let ManagedSurface::Xdg(toplevel) = &record.surface else {
            return false;
        };
        toplevel
            .wl_surface()
            .client()
            .and_then(|client| client.get_data::<ClientState>().map(|data| data.kde_decoration_bound.load(std::sync::atomic::Ordering::Relaxed)))
            .unwrap_or(false)
    }

    pub(crate) fn new(display_handle: DisplayHandle, monitors: Vec<MonitorInfo>, scale: f32) -> Self {
        let output_size = union_size(&monitors);
        let monitor_scales = vec![scale.max(0.125) as f64; monitors.len()];
        Self {
            next_id: 1,
            windows: HashMap::new(),
            decoration_rules: wm_config::DecorationRules::default(),
            frames: HashMap::new(),
            shells: HashMap::new(),
            stacking: Vec::new(),
            pending: VecDeque::new(),
            shell_clicks: VecDeque::new(),
            shell_motions: VecDeque::new(),
            shell_scrolls: VecDeque::new(),
            pending_resize: None,
            grabbed_combos: Vec::new(),
            keyboard_grabbed: false,
            root_background: RootBackground::Color((0, 0, 0)),
            monitors,
            monitor_scales,
            output_size,
            damage: true,
            display_handle,
            pending_focus: None,
            preview_edge: None,
            preview_generation: 0,
            pending_cursor_scale: None,
            pointer_grab: None,
            pending_pointer_grab: None,
            pointer: None,
            frame_cursors: HashMap::new(),
            layers: Vec::new(),
            locked: false,
            lock_surfaces: Vec::new(),
            ewmh: crate::xewmh::EwmhLedger::default(),
        }
    }

    /// Allocates the next id in the shared counter — used by the
    /// protocol handlers for window ids and by `backend_impl` for
    /// frame/shell ids.
    pub(crate) fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Queues an event for the next `poll_event` drain.
    pub(crate) fn queue(&mut self, event: BackendEvent<WlWindowId, WlFrameId>) {
        self.pending.push_back(event);
    }

    /// Marks the scene dirty. Every mutating verb and every handler
    /// that changes anything visible must call this (or set the field)
    /// or its change waits for the next unrelated damage to appear.
    pub(crate) fn mark_damaged(&mut self) {
        self.damage = true;
    }

    /// Ends whatever drag grab is in flight and asks the seat to hand
    /// the pointer back to the client under it.
    ///
    /// The single exit, so that "no drag holds the pointer" and "the
    /// client under it has been told" cannot come apart: `Backend::
    /// ungrab_pointer` calls it when the drag's owner is finished, and
    /// `input.rs` calls it when it finds a grab whose drag is already
    /// over. Idempotent — no grab means nothing to announce either.
    pub(crate) fn end_pointer_grab(&mut self) {
        if self.pointer_grab.take().is_some() {
            self.pending_pointer_grab = Some(PointerGrabChange::Released);
        }
    }

    /// The fractional UI scale of the monitor containing `rect`'s
    /// center — the factor a surface anchored there is composed at.
    /// Falls back to the primary monitor's scale (index 0) for a rect
    /// in dead space or an empty layout, and to 1.0 when there are no
    /// monitors at all, which no running session has.
    ///
    /// The center, not the top-left corner: a window straddling two
    /// screens has to be measured by *one* factor, and the monitor
    /// holding most of it is the least surprising choice — the same
    /// tie-break `wm-core` uses for placement.
    pub(crate) fn scale_at(&self, rect: Rect) -> f64 {
        scale_for_rect(&self.monitors, &self.monitor_scales, rect)
    }

    /// The factor one managed window's surface is composed at: what the
    /// client itself committed, corrected only for the integral-fallback
    /// case (`xdg::effective_surface_scale`) on the output the window
    /// lives on. The single definition the renderer, the hit-test, the
    /// ledger measurement and the configure path all call — four sites
    /// describing one rectangle must multiply by one number.
    pub(crate) fn window_surface_scale(&self, record: &WindowRecord) -> f64 {
        let Some(surface) = record.surface.wl_surface() else {
            return 1.0;
        };
        // Xwayland is the one client this compositor never invites to
        // scale itself (it is told through XSETTINGS instead and keeps
        // committing 1x buffers over 1x rectangles), so the fallback
        // correction must not apply to it either.
        if matches!(record.surface, ManagedSurface::X11(_)) {
            return crate::xdg::committed_buffer_scale(&surface) as f64;
        }
        crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(&surface),
            self.scale_at(record.content),
        )
    }

    /// Resolves a `wl_surface` back to the managed window it belongs
    /// to, comparing against each record's root surface. Linear over
    /// the window count — fine at WM scale, and it avoids a second
    /// index that could drift from the authoritative `windows` map.
    pub(crate) fn window_for_surface(&self, surface: &WlSurface) -> Option<WlWindowId> {
        self.windows
            .iter()
            .find(|(_, record)| record.surface.wl_surface().as_ref() == Some(surface))
            .map(|(id, _)| *id)
    }

    /// Drops a window's ledger entry *and* the stacking slot a
    /// frameless one holds.
    ///
    /// Every removal site has to go through here now that
    /// [`StackEntry::Window`] exists. A framed window's slot belongs to
    /// its frame and `Backend::destroy_decoration` prunes it during
    /// `wm-core`'s teardown, but a client-decorated window's slot is
    /// keyed by the window id itself, and nothing else would ever
    /// collect it: the renderer and the hit-test would keep walking a
    /// stale entry, find no record behind it, and skip — quietly
    /// growing the stack by one dead slot per client-decorated window
    /// the session ever opened.
    pub(crate) fn forget_window(&mut self, window: WlWindowId) {
        self.windows.remove(&window);
        self.stacking
            .retain(|entry| !matches!(entry, StackEntry::Window(w) if *w == window));
        // Same collection duty for the pending EWMH properties: keyed
        // by window id, and nothing downstream would ever drain an
        // entry whose window no longer resolves.
        self.ewmh.prune_window(window);
    }
}

/// Per-client data attached when a wayland client connects. Smithay's
/// compositor protocol machinery requires each client to carry its
/// `CompositorClientState`; the decoration flag rides along beside it.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
    /// Whether this client has bound
    /// `org_kde_kwin_server_decoration_manager`.
    ///
    /// Load-bearing evidence, not a statistic: a GTK4 client that binds
    /// the manager and then creates no decoration object for a toplevel
    /// is declining this desktop's chrome for it, and telling that
    /// apart from a client that never spoke at all is what keeps a
    /// libadwaita headerbar from wearing our titlebar above it. See
    /// `crate::decoration`.
    ///
    /// Per-client rather than a set on the backend so it cannot outlive
    /// the client: `ClientData::disconnected` gets no access to the
    /// compositor, so a set would have leaked one entry per client that
    /// ever spoke this protocol. Atomic because the bind handler holds
    /// only `&ClientState`.
    pub kde_decoration_bound: std::sync::atomic::AtomicBool,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}

    /// Says why a client went away.
    ///
    /// This was an empty stub, and the silence cost real debugging
    /// time: a client that vanishes mid-session is indistinguishable
    /// from one that exited on purpose, so "the shell disappeared when
    /// I closed a menu" had no first clue to follow at all — not even
    /// whether the compositor had hung up or the client had walked
    /// away. A protocol error carries the offending object and the
    /// interface's own message, which is the difference between a
    /// bisect and a read.
    ///
    /// Levels are chosen so a normal session stays quiet: a client
    /// closing its own connection is the ordinary way programs exit
    /// and is logged at debug, while a protocol error means *this
    /// compositor* rejected something and killed the client for it,
    /// which is a bug here until proven otherwise.
    fn disconnected(&self, client_id: ClientId, reason: DisconnectReason) {
        match reason {
            DisconnectReason::ConnectionClosed => {
                tracing::debug!(?client_id, "client closed its connection");
            }
            DisconnectReason::ProtocolError(error) => tracing::warn!(
                ?client_id,
                object = %error.object_interface,
                object_id = error.object_id,
                code = error.code,
                message = %error.message,
                "client killed for a protocol error"
            ),
        }
    }
}

/// How often the dispatch loop wakes with zero protocol activity, to
/// run `Shell::tick` and the restart-marker check — same value and
/// same rationale as the X11 binary's `HOUSEKEEPING_INTERVAL`: ~60Hz
/// is far more than the clock or menu timers need, but cheap, and
/// real events (client commits, input) wake calloop immediately
/// regardless of this bound.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(16);

/// The one calloop data type — see the module docs for why everything
/// must live on a single struct. Fields are `pub` (not `pub(crate)`)
/// only where the re-exported API surface needs them; the protocol
/// handler modules are in-crate and reach everything either way.
/// Which graphics stack this session is driving. The two arms are the
/// same desktop with different plumbing underneath: `Winit` renders
/// into a window on somebody else's desktop (the development and
/// preview mode), `Session` owns a DRM/KMS device, its outputs, and
/// its input devices through libseat (the real login session). The
/// scene they draw is one function, [`crate::renderer::build_scene`],
/// so the choice never reaches anything visual.
pub(crate) enum Graphics {
    Winit(WinitGraphicsBackend<GlesRenderer>),
    // Constructed by `session::init` once the DRM backend lands; the
    // allow keeps the staged build warning-free until then.
    #[allow(dead_code)]
    Session(Box<crate::session::SessionGraphics>),
}

/// One output, as the graphics backend hands it to [`run`] before any
/// wayland global exists: the `Output` with its mode already set, plus
/// where it sits in the global coordinate space. The session backend
/// produces one of these per connected connector; the nested backend
/// produces exactly one, at the origin.
pub(crate) struct OutputSetup {
    pub output: Output,
    pub position: Point,
    pub size: Size,
    /// Every mode this output can drive, current/preferred first. One
    /// entry on the nested backend (the host window has no modes to
    /// offer); the connector's full EDID mode list on the session
    /// backend. Carried so wlr-output-management can enumerate honest
    /// alternatives instead of pretending the current mode is the only
    /// one.
    pub modes: Vec<Mode>,
}

/// One output the compositor drives, everything the session needs to
/// know about it in one place.
///
/// The order of [`Compositor::outputs`] is the contract: index 0 is the
/// primary monitor, and the same index selects the matching entry in
/// `Backend::monitors` and (on the session backend) in
/// `SessionGraphics::outputs`. Nothing keys outputs by name, so nothing
/// has to agree on one.
pub(crate) struct OutputEntry {
    pub output: Output,
    /// Top-left corner in global compositor space. Every rect in the
    /// ledger is global, so this is what the renderer subtracts to put
    /// the one shared scene into this output's framebuffer.
    pub position: Point,
    pub size: Size,
    /// This output's fractional UI scale — what fractional-scale-v1
    /// tells clients on it, what `wl_output.scale` advertises the
    /// ceiling of, and what `WaylandBackend::monitor_scales` mirrors
    /// for the ledger's arithmetic. Starts at the session scale
    /// (`advertise_scale` writes every entry); wlr-output-management
    /// can then move one output's value on its own.
    pub scale: f64,
    /// The modes this output can drive — see [`OutputSetup::modes`].
    pub modes: Vec<Mode>,
    /// Only the nested backend renders through this: the session
    /// backend's `DrmCompositor` owns damage tracking per crtc itself.
    /// It is built from the `Output`, so a resize of that output
    /// retunes it with no work here.
    pub damage_tracker: OutputDamageTracker,
    /// The `wl_output` global clients bind to. Dropping the id does not
    /// take the global down — that needs
    /// `DisplayHandle::remove_global` — so this is held for the one
    /// caller that would ever pass it there: a connector-hot-unplug
    /// path taking an output back off the wire.
    _global: GlobalId,
}

impl OutputEntry {
    /// Advertises one output to clients and prepares it for rendering.
    fn new(setup: OutputSetup, display_handle: &DisplayHandle) -> Self {
        let _global = setup.output.create_global::<Compositor>(display_handle);
        let damage_tracker = physical_damage_tracker(&setup.output, setup.size);
        Self {
            output: setup.output,
            position: setup.position,
            size: setup.size,
            scale: 1.0,
            modes: setup.modes,
            damage_tracker,
            _global,
        }
    }
}

/// A damage tracker for `output` pinned to scale 1, whatever scale the
/// output advertises. Rebuilt (never `from_output`) on a mode change —
/// see [`Compositor::on_output_resized`].
///
/// The pin is the whole reason advertising a real scale on the
/// `wl_output` is safe, and it earns the space to say precisely why.
/// What `OutputDamageTracker::render_output` reads from its mode source
/// (smithay 0.7, `damage/mod.rs` + `output.rs`) is the mode size in
/// physical pixels, the output's *fractional scale*, and its transform.
/// The output rectangle it clips against is the physical mode size —
/// that part is safe — but every element is asked for its rectangle
/// through `Element::geometry(scale)` with that fractional scale, and
/// both `MemoryRenderBufferRenderElement` and
/// `WaylandSurfaceRenderElement` implement `geometry` as "my stored
/// physical location, unchanged, with my *logical* size multiplied by
/// the scale I was just passed" (`element/memory.rs`,
/// `element/surface.rs`). A memory buffer's logical size is its pixel
/// size divided by the buffer scale it was constructed with — 1 for
/// every buffer this compositor makes, because the theme rasterizes in
/// device pixels.
///
/// So the day `change_current_state(scale = 2)` was tried on the real
/// 4K session with `from_output` trackers, every chrome element kept
/// its device-pixel position and doubled in size: the 3840x2160
/// wallpaper became a 7680x4320 element with only its top-left quarter
/// inside the output, the dock's tiles grew out past the right edge
/// and vanished, and the frames' titlebars painted at twice their
/// width over the windows beside them. Mixed spaces, one multiply.
///
/// Pinning the tracker to 1.0 makes `geometry(1.0)` the identity on
/// logical sizes: the compositor composes in physical pixels end to
/// end, exactly as it did when the outputs said 1, and the advertised
/// scale becomes what it should be — protocol metadata for clients,
/// with the per-surface correction in `renderer::push_surface_tree`
/// putting their scaled buffers back at 1 buffer pixel : 1 screen
/// pixel. The session backend pins its `DrmCompositor`s the same way
/// (`session::attach_output`); the two must never disagree.
pub(crate) fn physical_damage_tracker(output: &Output, size: Size) -> OutputDamageTracker {
    OutputDamageTracker::new(
        SSize::<i32, Physical>::from((size.w as i32, size.h as i32)),
        1.0,
        output.current_transform(),
    )
}

/// The scale every `wl_output` advertises for a session UI scale, and
/// the only channel a native Wayland client actually listens on:
/// verified under `WAYLAND_DEBUG`, GTK with `GDK_SCALE=2` against
/// outputs at scale 1 makes no `set_buffer_scale` call at all. A
/// client that hears 2 here answers `set_buffer_scale(2)`, renders
/// twice the pixels, and the rest of this crate already meets it — the
/// ledger measures its commits by the factor it committed
/// (`xdg::committed_content_size`), configures it back in its own
/// logical pixels (`resize_client`), hit-tests through the same factor
/// (`input.rs`), and draws its buffer 1:1
/// (`renderer::push_surface_tree`).
///
/// Integer, because `wl_output.scale` is an integer: a fractional
/// session scale rounds UP to the next whole step (1.5 — and 1.25 —
/// advertise 2), clamped to 1 and up. Rounding up is deliberate and
/// the direction matters: this integer is the *fallback* for clients
/// that never bind `fractional-scale-v1`, and a fallback client told
/// the ceiling renders MORE pixels than the output needs, which the
/// compositor then downscales to the true factor
/// (`xdg::effective_surface_scale`) — crisp. Told the floor it would
/// render too few and be upscaled — blurry. Clients that do bind
/// fractional-scale hear the exact fraction and the ceiling never
/// applies to them. (`f32::max` returns 1.0 for a NaN scale, so the
/// cast is always sane — same guard as `default_cursor_pixels`.)
pub(crate) fn advertised_output_scale(scale: f32) -> OutputScale {
    OutputScale::Integer(scale.max(1.0).ceil() as i32)
}

/// (Re-)advertises the session's UI scale on every output. smithay
/// broadcasts the change to every bound `wl_output` (and its
/// `xdg_output`) and clients follow with new buffers at the new scale;
/// the damage trackers stay pinned at 1 (see
/// [`physical_damage_tracker`]) so the compositor's own composition
/// never hears about it.
/// A session-wide scale change: every output's fractional scale moves
/// to the session's (any per-output value wlr-output-management set is
/// deliberately superseded — a config reload states the whole desktop's
/// scale, and honoring half of it would leave no way to say "put
/// everything back").
fn advertise_scale(outputs: &mut [OutputEntry], scale: f32) {
    let advertised = advertised_output_scale(scale);
    for entry in outputs.iter_mut() {
        entry.scale = scale.max(0.125) as f64;
        entry.output.change_current_state(None, None, Some(advertised), None);
    }
}

/// Re-advertises ONE output's own fractional scale: the integer ceiling
/// on its `wl_output`, the exact fraction to every fractional-scale
/// client. The per-output half of what [`advertise_scale`] does for the
/// session; wlr-output-management's apply path is the caller.
pub(crate) fn advertise_output_scale_change(entry: &OutputEntry) {
    let advertised = advertised_output_scale(entry.scale as f32);
    entry.output.change_current_state(None, None, Some(advertised), None);
}

/// The union bounding box of every monitor: what `Backend::screen_size`
/// answers, and the extent of the coordinate space the ledger, the
/// renderer and the pointer all share.
///
/// Only the far corner is measured because outputs are laid out from
/// the origin rightwards (see `session::init`). A layout that ever put
/// an output at a negative coordinate — a monitor placed to the *left*
/// of the primary, which is exactly what an output-management protocol
/// would let a user ask for — would need the near corner too, and would
/// need `wm-core` and the shell to stop assuming the screen starts at
/// (0, 0). Moving that assumption is the real cost of arbitrary output
/// positioning; the layout code is the easy half.
/// The pure half of [`WaylandBackend::scale_at`]: the scale of the
/// monitor containing `rect`'s center, else the primary's (index 0),
/// else 1.0. The center rather than a corner because a window
/// straddling two screens has to be measured by ONE factor, and the
/// monitor holding most of it is the least surprising choice.
pub(crate) fn scale_for_rect(monitors: &[MonitorInfo], scales: &[f64], rect: Rect) -> f64 {
    let center = Point::new(
        rect.pos.x + (rect.size.w / 2) as i32,
        rect.pos.y + (rect.size.h / 2) as i32,
    );
    let index = monitors
        .iter()
        .position(|monitor| monitor.geometry.contains(center))
        .unwrap_or(0);
    scales.get(index).copied().unwrap_or(1.0)
}

pub(crate) fn union_size(monitors: &[MonitorInfo]) -> Size {
    let mut width: i32 = 0;
    let mut height: i32 = 0;
    for monitor in monitors {
        width = width.max(monitor.geometry.pos.x + monitor.geometry.size.w as i32);
        height = height.max(monitor.geometry.pos.y + monitor.geometry.size.h as i32);
    }
    Size::new(width.max(0) as u32, height.max(0) as u32)
}

pub struct Compositor {
    /// The policy brain, owning the [`WaylandBackend`] ledger. Protocol
    /// handlers reach the ledger through `self.wm.backend_mut()`.
    pub wm: WindowManager<WaylandBackend>,
    /// The whole desktop — dock, Clip, menus, launcher, wallpaper —
    /// identical by construction to the X11 session's.
    pub shell: Shell<WaylandBackend>,

    pub display_handle: DisplayHandle,
    pub loop_handle: LoopHandle<'static, Compositor>,
    /// One calloop source per file descriptor the shell asked to be
    /// woken on that this compositor knows nothing else about: the
    /// dockapp listener, and one per connected out-of-process dock
    /// tile. Reconciled against `Shell::extra_poll_fds` at the end of
    /// every dispatch pass — see [`Compositor::sync_dock_sources`].
    dock_sources: Vec<(RawFd, RegistrationToken)>,

    // Per-protocol smithay state. Constructed once in `run`; the
    // handler impls in `xdg.rs`/`input.rs`/`xwayland.rs` return these
    // from their `*_state()` accessors.
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    /// KDE's older `org_kde_kwin_server_decoration`, advertised beside
    /// the xdg one because GTK — GTK3 and GTK4 alike — implements only
    /// this interface and never binds the standard one. Offering only
    /// xdg-decoration made every GTK application on the system a silent
    /// client, which is what put two titlebars on LibreOffice. KWin,
    /// Sway, labwc and Hyprland all advertise it too. See
    /// `crate::decoration`.
    pub kde_decoration: KdeDecorationState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Compositor>,
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    /// The middle-click clipboard. Advertised because the X11 half of
    /// the session has always had one — PRIMARY is an X server concept
    /// that XWayland clients use whether or not the Wayland side knows
    /// about it — and the XWayland selection bridge in `xwayland.rs`
    /// needs somewhere to put an X selection it is handed and somewhere
    /// to read one from. Without the global, selecting text in xterm and
    /// middle-clicking into a Wayland editor has nothing to travel
    /// through.
    pub primary_selection_state: PrimarySelectionState,
    /// The clipboard-manager protocols, wlr and ext alike: the only way
    /// a client that does not have keyboard focus can read or write the
    /// two selections above. Without them `wl-paste --watch` refuses to
    /// run and nothing keeps a clipboard history — see
    /// `crate::data_control`.
    pub(crate) data_control: crate::data_control::DataControl,
    pub xwayland_shell_state: XWaylandShellState,
    /// fractional-scale-v1: the channel that can say "1.5" to a client,
    /// where `wl_output.scale` can only say its ceiling. Serving it is
    /// most of what makes a fractional session scale first-class — see
    /// `xdg.rs`'s commit handler and `xdg::committed_surface_scale`.
    pub fractional_scale_state: smithay::wayland::fractional_scale::FractionalScaleManagerState,
    /// wp_viewporter: how a fractional-scale client actually commits at
    /// 1.5x — a `round(w × 1.5)` px buffer with a viewport destination
    /// of `w` logical. smithay's surface state resolves the viewport;
    /// the ledger recovers the factor from the ratio.
    pub viewporter_state: smithay::wayland::viewporter::ViewporterState,
    /// Tracks xdg popups (client menus, tooltips) so the renderer can
    /// draw them above their parent window — `wm-core` never learns
    /// about them, exactly as it never learns about X11
    /// override-redirect windows.
    pub popups: PopupManager,

    pub seat: Seat<Compositor>,
    /// Every output, primary first — see [`OutputEntry`] for what that
    /// order binds together. Never empty: a session with no output
    /// never gets built (`session::init` fails, and the nested backend
    /// always has its host window).
    pub(crate) outputs: Vec<OutputEntry>,

    /// The X11 window-manager connection into XWayland, once
    /// `XWaylandEvent::Ready` has arrived (`None` before that, or if
    /// XWayland failed to start — native Wayland clients work either
    /// way).
    pub xwm: Option<X11Wm>,
    /// XWayland's display number, mirrored into `DISPLAY` so children
    /// the shell spawns find it.
    pub xdisplay: Option<u32>,
    /// The XSETTINGS manager publishing this session's DPI, scaling
    /// factor and cursor size to every X client on the XWayland
    /// display, once XWayland is up.
    ///
    /// `None` before that, and `None` for good if the selection was
    /// already owned — a degraded session, never a dead one. See
    /// [`Compositor::start_xsettings`] for what it publishes and, more
    /// importantly, what it deliberately does not.
    pub(crate) xsettings: Option<XSettingsManager>,
    /// The EWMH publisher for the XWayland root — a second ordinary X
    /// connection beside the XSETTINGS one, same lifecycle: `None`
    /// before XWayland is ready, `None` for good after a failure
    /// (degraded to exactly what the session did before it existed —
    /// X tools see nothing). See `xewmh.rs`.
    pub(crate) xewmh: Option<crate::xewmh::XEwmh>,
    /// The UI scale everything in this session is drawn at.
    ///
    /// Held here because two things outside the theme engine are sized
    /// from it and neither can ask anyone else: the compositor's own
    /// pointer (rebuilt in [`Compositor::dispatch_pending`]) and the
    /// XSETTINGS properties above, which have to be publishable at
    /// XWayland-ready time — a moment that arrives asynchronously, long
    /// after `run` has handed its `SessionState` to the shell and
    /// dropped it.
    pub(crate) ui_scale: f32,

    /// The graphics stack: a host window, or the hardware itself.
    pub(crate) graphics: Graphics,
    /// linux-dmabuf: the format set we advertise and the protocol
    /// state behind it. Always present; "this renderer cannot do
    /// dmabuf" is represented inside, not by an `Option`, so protocol
    /// dispatch in a login session never has an unreachable panic on
    /// a screen with no console to read it from.
    pub(crate) dmabuf: crate::dmabuf::DmabufSupport,
    /// Explicit sync (`wp_linux_drm_syncobj_v1`), present only when a
    /// DRM session backend's device can wait on syncobj timelines.
    /// `None` on the nested backend and on devices without
    /// `syncobj_eventfd` — see `dmabuf::init_syncobj` for why the
    /// global is the difference between Edge flickering and not on
    /// NVIDIA.
    pub(crate) syncobj: Option<smithay::wayland::drm_syncobj::DrmSyncobjState>,
    /// The wlr protocol surface external tools bind: the
    /// foreign-toplevel window list and screencopy capture. The
    /// Wayland counterpart to the X11 session's EWMH properties.
    pub(crate) protocols: crate::protocols::ProtocolState,
    /// wlr-output-management: what `wlr-randr` and `kanshi` list and
    /// configure outputs through — see `output_mgmt.rs`.
    pub(crate) output_mgmt: crate::output_mgmt::OutputManagement,
    /// wlr-layer-shell: launchers, bars, notification daemons, OSDs —
    /// protocol state plus the focus/reservation bookkeeping in
    /// `layers.rs` (the surface records themselves live on the ledger).
    pub(crate) layer_shell: crate::layers::LayerShell,
    /// ext-session-lock: the lock lifecycle machine and the
    /// confirmation owed to a locking client — see `lock.rs`.
    pub(crate) session_lock: crate::lock::SessionLock,
    /// hyprland-focus-grab-v1: the surface whitelists a shell asks the
    /// compositor to enforce so a click away dismisses its popups.
    /// Omarchy's Quickshell uses one per popout; without the global it
    /// disables the feature and the popouts never close. See
    /// `focus_grab.rs`.
    pub(crate) focus_grab: crate::focus_grab::FocusGrab,
    /// ext-idle-notify + idle-inhibit: the timers `swayidle` runs on,
    /// reset from the input path — see `idle.rs`.
    pub(crate) idle: crate::idle::Idle,
    /// virtual-keyboard-v1: the global `wtype` looks for, and with it
    /// paste-as-keystrokes, emoji insertion and voice dictation. Held
    /// so the global's id outlives `run`, never read again — a virtual
    /// keyboard's whole state lives on its own protocol object, so
    /// there is nothing here to reconcile per pass. Same shape as
    /// `Idle::_inhibit`. See `virtual_keyboard.rs`.
    pub(crate) _virtual_keyboard:
        smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState,

    /// Latest pointer position in compositor space, maintained by
    /// `input.rs` — the renderer draws the cursor here, and hit-tests
    /// run against it.
    pub pointer_location: SPoint<f64, Logical>,
    /// What the cursor should look like, per the focused client's
    /// `wl_pointer.set_cursor` (maintained by `input.rs`'s
    /// `SeatHandler::cursor_image`). Honored only while the pointer is
    /// over client content; the renderer falls back to
    /// [`Compositor::cursors`] for `Named` shapes and everywhere else
    /// (see `push_cursor_elements`).
    pub cursor_status: CursorImageStatus,
    /// The compositor's own pointer images — the arrow, and the resize
    /// double-arrows shown over frame edges — drawn whenever no client
    /// cursor surface applies: a compositor draws its own cursor, there
    /// is no server to inherit one from. Which member is drawn is the
    /// renderer's per-frame decision (see `push_cursor_elements`), fed
    /// by what the pointer is over and what `Backend::set_frame_cursor`
    /// recorded on the ledger.
    pub(crate) cursors: CursorSet,

    /// Monotonic session clock for frame-callback timestamps.
    pub start_time: Instant,
    /// Cleared to exit the dispatch loop (root-menu Exit, host window
    /// closed, display teardown).
    pub running: bool,
    /// Set (with `running` cleared) to re-exec the on-disk binary
    /// after teardown — the theme-menu / `scripts/restart.sh` hot
    /// restart. Unlike X11 there is no SaveSet: Wayland clients die
    /// with the compositor's socket, so a restart costs the session's
    /// clients. That matches what a compositor *can* do today; a
    /// future session handoff could improve it.
    pub restart: bool,
}

impl Compositor {
    /// Drains everything the protocol handlers queued since the last
    /// pass, in exactly the X11 binary loop's order — keymap
    /// interception before dispatch, motion coalescing, notification
    /// and resize drains, shell-click routing, `Shell::tick` — then
    /// applies deferred focus and renders if anything was damaged.
    /// Called once per calloop wakeup from [`run`]'s loop; keeping the
    /// order identical to `crates/chonkstep/src/main.rs` is what makes
    /// "the desktop behaves identically" a structural property rather
    /// than a porting promise, so change that file first if this order
    /// ever needs to move.
    pub(crate) fn dispatch_pending(&mut self) {
        // Consecutive `PointerMotion` events coalesce to the most
        // recent one — same rationale as the X11 loop: during a fast
        // drag every intermediate position is stale by the time it
        // would draw, and the held-back motion still commits before
        // any later non-motion event in the same burst.
        let mut pending_motion = None;
        while let Some(event) = self.wm.backend_mut().poll_event() {
            if matches!(event, BackendEvent::ShutdownRequested) {
                // The display is going away for good; nothing below
                // can succeed.
                self.running = false;
                return;
            }
            if matches!(event, BackendEvent::PointerMotion { .. }) {
                pending_motion = Some(event);
                continue;
            }
            // Configured keybindings resolve BEFORE `wm.dispatch`, and
            // misses MUST keep flowing through unchanged — during a
            // modal Alt+Tab session every key arrives here as a
            // `KeyPress`, and swallowing unbound ones would wedge the
            // switcher open (see the X11 loop's longer commentary;
            // `KeyRelease` is never intercepted at all).
            if let BackendEvent::KeyPress(combo) = &event {
                if let Some(action) = self.shell.keymap_action(combo) {
                    if let Some(motion) = pending_motion.take() {
                        self.dispatch_motion(motion);
                    }
                    let outcome = self.shell.run_action(&mut self.wm, &action);
                    self.note_outcome(outcome);
                    continue;
                }
            }
            if let Some(motion) = pending_motion.take() {
                self.dispatch_motion(motion);
            }
            self.wm.dispatch(event);
        }
        if let Some(motion) = pending_motion.take() {
            self.dispatch_motion(motion);
        }

        while let Some(notification) = self.wm.take_notification() {
            self.shell.on_notification(&mut self.wm, notification);
        }

        if let Some(new_size) = self.wm.backend_mut().take_screen_resize() {
            tracing::info!(width = new_size.w, height = new_size.h, "output resized");
            self.shell.on_screen_resize(&mut self.wm, new_size);
            self.wm.set_workarea(self.shell.workarea(new_size));
        }

        // Shell-surface clicks drain to the shell, with background
        // presses split off to `on_root_press` under the [`ROOT_SHELL`]
        // sentinel — the same press/release routing asymmetry as the
        // X11 loop (root reacts on press; releases still flow through
        // `on_shell_click` so an in-progress launcher-strip drag sees
        // them).
        while let Some((surface, local, button, pressed)) = self.wm.backend_mut().take_shell_click() {
            let outcome = if surface == ROOT_SHELL && pressed {
                self.shell.on_root_press(&mut self.wm, local, button)
            } else {
                self.shell.on_shell_click(&mut self.wm, surface, local, button, pressed)
            };
            self.note_outcome(outcome);
        }
        // Scroll drains beside the clicks and separately from them: an
        // axis event is not a button, so `take_shell_click` never
        // reports one and the two drains cannot double-count the same
        // gesture. Queued rather than coalesced, unlike motion, because
        // every notch is its own command — three notches on a volume
        // tile is three steps, and keeping only the last would swallow
        // input the user gave. A scroll produces no `ShellOutcome`, so
        // it sits on this side of the exit check with the clicks that
        // do.
        while let Some((surface, local, delta)) = self.wm.backend_mut().take_shell_scroll() {
            self.shell.on_shell_scroll(&mut self.wm, surface, local, delta);
        }

        if !self.running {
            // Exit/restart was requested somewhere above — mirror the
            // X11 loop's break-before-tick. Sources are reconciled even
            // on the way out: a menu pick above (Remove on a dock
            // tile's menu) can have closed a dockapp socket, and
            // leaving its source registered would hand calloop a closed
            // descriptor if anything dispatched again.
            self.sync_dock_sources();
            return;
        }

        // No separate shell-motion drain, same as X11: the shell
        // drains `take_shell_motion` itself inside `on_motion`, which
        // every coalesced `PointerMotion` above passes through.

        self.shell.tick(&mut self.wm);

        // Wayland-side housekeeping with no X11 counterpart: dead xdg
        // popups age out, and the focus intent a backend verb recorded
        // lands on the seat (see `WaylandBackend::pending_focus` for
        // why it cannot land inline).
        self.popups.cleanup();
        self.apply_pending_focus();
        // Beside the focus intent and for the same reason: a drag that
        // began or ended anywhere above has to reach the seat, and only
        // this side of the loop can reach one. Before the flush at the
        // bottom of this pass, so the leave a starting drag owes its
        // client — and the enter an ending one owes whoever is under
        // the pointer now — travels with everything else this pass
        // decided rather than waiting for the next event.
        crate::input::apply_pointer_grab_change(self);
        // Publish the window list and serve screencopy requests: after
        // the event and notification drains so external tools see the
        // same state the desktop just settled into, before the damage
        // test so a capture request can mark the frame it needs.
        crate::protocols::refresh(self);
        // Output management settles beside the other protocol
        // reconciliations and before the damage test, so an applied
        // configuration's re-layout renders on this very pass.
        crate::output_mgmt::refresh(self);
        // The X11 tools' equivalent of the wlr window list above rides
        // the same timing: flush the buffered EWMH publishes to the
        // XWayland root after the drains, so `xprop`/`wmctrl` read the
        // state the desktop just settled into, never a half-applied
        // pass. A no-op until XWayland is ready (see `xewmh::flush`).
        crate::xewmh::flush(self);
        // Layer surfaces settle beside them and before the damage test
        // for the same reason: a bar that just changed its exclusive
        // zone must reflow the workareas and render on this frame, not
        // the next. After `Shell::tick` and the drains deliberately —
        // anything in this pass that re-applied the shell's baseline
        // workareas has already run, so the layer-composed rects land
        // last (see `layers::apply_workareas`).
        crate::layers::refresh(self);
        // Focus grabs settle after the layer pass and before the lock
        // one, which is the ordering their own module documents read as
        // a sequence: the layer surfaces a grab whitelists have just
        // been arranged and had their exclusive-focus claim resolved,
        // so the keyboard decision here sees the settled answer; and
        // `lock::refresh` runs immediately after, so a lock that
        // engages on this very pass still lands on top of a grab this
        // one just ended.
        crate::focus_grab::refresh(self);
        // Lock upkeep (re-configures on resize, keyboard onto a late
        // lock surface); a no-op the instant the test above it — the
        // ledger's `locked` flag — is clear.
        crate::lock::refresh(self);
        // Idle inhibition follows visibility, which everything above
        // may have changed.
        crate::idle::refresh(self);

        // The UI scale moved: rebuild the built-in pointer, the one
        // thing this session draws that is sized from that scale and
        // does not come out of the theme engine (see
        // `build_default_cursor`). Damage because the arrow that is
        // already on screen is the wrong size until it is redrawn, and
        // a pointer sitting still over an idle desktop produces none of
        // its own.
        //
        // Drained here, every pass, rather than rebuilt next to
        // wherever a reload was noticed. A scale change reaches the
        // session from at least two directions — the `reload` marker
        // `run`'s loop polls, and an `Action::Reload` keybinding that
        // `Shell::run_action` applies without ever passing through that
        // poll — and both of them end in
        // `Shell::apply_session_state`, which tells the backend and
        // nothing else. So the announcement is the only point both
        // paths cross, and a rebuild written into either one of them is
        // a pointer that silently stays the wrong size on the other.
        //
        // The XSETTINGS republish rides the same drain, for the same
        // reason and not merely out of convenience: it answers the same
        // question ("what is sized from the scale and cannot ask the
        // theme engine?") for X clients that the pointer rebuild answers
        // for this compositor, and it has the identical hazard of being
        // written next to one of the two paths a scale change arrives
        // by. One announcement, one drain, both consumers.
        // Before the drain below, so a republish is never attempted on a
        // selection this session has just been told it no longer owns.
        self.poll_xsettings();
        if let Some(scale) = self.wm.backend_mut().pending_cursor_scale.take() {
            tracing::info!(scale, "rebuilding the compositor's own pointer for the new UI scale");
            self.ui_scale = scale;
            self.cursors = CursorSet::build(scale);
            self.wm.backend_mut().mark_damaged();
            self.republish_xsettings();
            // Native Wayland clients ride the same drain, through the
            // one channel they listen on: the outputs re-advertise the
            // scale (broadcast to every bound `wl_output`), and every
            // managed surface is told its new preferred buffer scale
            // directly, because `send_surface_state` only sends on
            // change *per surface* and a window idling in the
            // background commits nothing that would make the
            // commit-time send in `xdg.rs` fire. Clients answer with
            // rescaled buffers; the ledger reflows around those commits
            // exactly as it does around any client resize.
            advertise_scale(&mut self.outputs, scale);
            self.sync_monitor_scales();
            let advertised = advertised_output_scale(scale).integer_scale();
            let surfaces: Vec<WlSurface> = self
                .wm
                .backend()
                .windows
                .values()
                // Xdg only: an Xwayland window's wl_surface belongs to
                // the Xwayland server, which is told the scale through
                // XSETTINGS/XCURSOR_SIZE above and must keep committing
                // 1x buffers over the ledger's 1x rectangles.
                .filter(|record| matches!(record.surface, ManagedSurface::Xdg(_)))
                .filter(|record| record.surface.alive())
                .filter_map(|record| record.surface.wl_surface())
                .collect();
            for surface in surfaces {
                smithay::wayland::compositor::with_states(&surface, |states| {
                    smithay::wayland::compositor::send_surface_state(
                        &surface,
                        states,
                        advertised,
                        Transform::Normal,
                    );
                    // The exact fraction, for clients that bound
                    // fractional-scale-v1 — the integer above is only
                    // their fallback. Dedup'd per surface by smithay.
                    smithay::wayland::fractional_scale::with_fractional_scale(states, |fractional| {
                        fractional.set_preferred_scale(scale.max(0.125) as f64);
                    });
                });
            }
        }

        // Damage means the scene changed; `redraw_pending` means a
        // change already accounted for has not reached every screen yet
        // (a page flip was still in flight on one of them last pass).
        // The second condition only ever fires on the session backend
        // with more than one output — see `session::redraw_pending`.
        if self.wm.backend().damage || crate::session::redraw_pending(&self.graphics) {
            crate::renderer::render_frame(self);
        }

        // A locking client is owed its `locked` event only after a
        // frame built under the lock has been presented — which, if it
        // happened at all this pass, happened just above.
        crate::lock::confirm_after_frame(self);

        // The outputs remember every surface that entered them (the
        // `wl_surface.enter` in `xdg.rs`'s commit handler) so they can
        // dedup; smithay asks that the dead ones be pruned "at best
        // before every wayland socket flush", which is here.
        for entry in &self.outputs {
            entry.output.cleanup();
        }

        // Protocol replies queued by everything above (configures,
        // frame callbacks, focus enter/leave) only reach clients on a
        // flush.
        let _ = self.display_handle.flush_clients();

        // Last, after every socket this pass was going to close has
        // been closed. See `sync_dock_sources` for why that ordering is
        // the safety argument and not a tidiness one.
        self.sync_dock_sources();

        // Test-door barriers ack only after the frame above has landed
        // and the flush has gone out — a no-op in a user session (the
        // door never opens without CHONKSTEP_TEST_SOCKET).
        crate::test_door::after_frame(self);
    }

    /// What this session tells X clients about its own appearance.
    ///
    /// Scale and nothing else, deliberately. `DesktopAppearance` can
    /// also carry a widget theme, an icon theme, a cursor theme and a
    /// default font, and every one of those is left unstated because
    /// this desktop does not ship them: there is no GTK theme named
    /// "chonkstep" and no Xcursor theme either, so publishing the name
    /// would not make applications look like chonkstep — it would make
    /// every GTK client on the display fail to find the theme, fall
    /// back to its default, and in the process *override* whatever the
    /// user had configured in their own `gtk-3.0/settings.ini`. Saying
    /// nothing leaves that setting alone, which is the honest answer to
    /// a question this desktop has no opinion on. (`DesktopAppearance`
    /// treats an empty theme name as exactly that — see its `Default`.)
    ///
    /// The scale it does state is the same number, from the same base,
    /// that `chonk_shell::startup::xcursor_size_for` derives
    /// `XCURSOR_SIZE` from. The two mechanisms overlap on purpose and
    /// must not disagree: a client can be reached by either one, and a
    /// pointer that changes size as it crosses a window border is what
    /// disagreement looks like.
    fn appearance(&self) -> DesktopAppearance {
        DesktopAppearance::new(self.ui_scale, "")
    }

    /// Takes the XSETTINGS manager selection on the freshly-started
    /// XWayland display and publishes this session's scale to it.
    ///
    /// # Why a second X connection
    ///
    /// This process already speaks X to Xwayland — that is what
    /// `X11Wm` is — but that connection is smithay's, driven by
    /// smithay's own calloop source, and `XSettingsManager` consumes
    /// the connection it is given and reads its event queue. Two
    /// readers on one queue would each swallow events meant for the
    /// other, which on the window-manager connection means dropped map
    /// requests. So the manager opens its own, exactly as an external
    /// settings daemon would.
    ///
    /// # Why failure is not fatal
    ///
    /// Something else owning `_XSETTINGS_S0` is a legitimate
    /// configuration — a user running `xsettingsd` for their own
    /// reasons — and the crate reports it as a clean `AlreadyOwned`
    /// rather than an error. Standing down is then the correct
    /// behaviour, not a degraded one: two managers fighting over the
    /// selection would leave clients following whichever wrote last.
    /// Everything else that can go wrong here (a display that vanished
    /// between `Ready` and this call, an X server refusing the window)
    /// costs the session its live scale publishing and nothing else,
    /// which is precisely what the session had before this existed.
    fn start_xsettings(&mut self, display_number: u32) {
        // The display is named explicitly rather than inherited from
        // `DISPLAY`: this runs inside the same handler that sets that
        // variable, and letting which display gets the settings depend
        // on the order of two lines in one function is a trap worth not
        // laying. (Called `display_name` because a bare `display` field
        // in a `tracing` macro resolves to `tracing::field::display`,
        // which the expansion has in scope, and a local of that name
        // loses to it — silently, as a type error about `Value`.)
        let display_name = format!(":{display_number}");
        // TakeOverPlaceholder: XWayland claims this selection at startup
        // and publishes an empty settings block — a squatter, not a
        // manager, and its emptiness is why X11 toolkits under this
        // compositor got no DPI at all. The policy takes over only an
        // owner whose property is absent or a valid zero-settings
        // block; a real manager (a user's own xsettingsd) still gets
        // the same respectful refusal as before.
        let mut manager = match XSettingsManager::acquire_with_policy(
            Some(&display_name),
            chonk_xsettings::AcquisitionPolicy::TakeOverPlaceholder,
        ) {
            Ok(manager) => manager,
            Err(error @ XSettingsError::AlreadyOwned { .. }) => {
                tracing::info!(%error, display = display_name, "another XSETTINGS manager owns this display; leaving it alone");
                return;
            }
            Err(error) => {
                tracing::warn!(%error, display = display_name, "could not publish XSETTINGS; X11 clients will only get the scale their launcher gave them");
                return;
            }
        };
        let appearance = self.appearance();
        if let Err(error) = manager.publish_appearance(&appearance) {
            tracing::warn!(%error, "could not publish the initial XSETTINGS");
            return;
        }
        tracing::info!(
            display = display_name,
            scale = appearance.ui_scale,
            cursor_px = appearance.effective_cursor_size(),
            "publishing XSETTINGS to XWayland clients"
        );
        self.xsettings = Some(manager);
    }

    /// Services the XSETTINGS connection: answers selection requests
    /// and notices if another manager has taken over.
    ///
    /// Not optional bookkeeping. Two things go wrong without it, and
    /// only one of them is ours. A client that asks to *convert* the
    /// selection and gets no answer does not fail — it waits out its own
    /// timeout, which the user experiences as an application that hangs
    /// on startup for no reason. And a manager that never learns it was
    /// superseded goes on rewriting a property it no longer owns, which
    /// ICCCM forbids a former owner from doing and which leaves clients
    /// following whichever of the two wrote last.
    ///
    /// Driven off this loop's existing wakeups rather than a calloop
    /// source on the connection's descriptor: `poll` is non-blocking and
    /// drains whatever has arrived, the loop already wakes at least
    /// every `HOUSEKEEPING_INTERVAL`, and the crate's own documentation
    /// says a timer is a sufficient home for it. The cost of being up to
    /// 16ms late to notice a takeover is nothing; the cost of a second
    /// event source is a second thing to unregister on teardown.
    fn poll_xsettings(&mut self) {
        let Some(manager) = self.xsettings.as_mut() else {
            return;
        };
        match manager.poll() {
            Ok(ManagerState::Owner) => {}
            Ok(ManagerState::Superseded) => {
                // The crate has already logged the takeover and latched
                // itself into refusing writes; dropping the handle is
                // this session agreeing, and stops every later scale
                // change asking again.
                tracing::info!("another XSETTINGS manager took the selection; standing down");
                self.xsettings = None;
            }
            Err(error) => {
                tracing::warn!(%error, "the XSETTINGS connection failed; giving up on it for this session");
                self.xsettings = None;
            }
        }
    }

    /// Republishes the appearance after a live scale change.
    ///
    /// The whole reason the XSETTINGS crate exists: an environment
    /// variable is read once at launch, so before this, changing the
    /// scale left every already-running X application at the size it
    /// started at until it was restarted. `publish_appearance` writes
    /// the property only when a value actually moved, so calling this
    /// on a reload that changed nothing else costs one map walk and no
    /// round trip — which matters, because writing the property wakes
    /// every client on the display and a GTK application answers by
    /// re-laying out every window it has.
    fn republish_xsettings(&mut self) {
        let appearance = self.appearance();
        let Some(manager) = self.xsettings.as_mut() else {
            return;
        };
        match manager.publish_appearance(&appearance) {
            Ok(true) => tracing::info!(
                scale = appearance.ui_scale,
                "told X11 clients about the new UI scale through XSETTINGS"
            ),
            Ok(false) => {}
            Err(error) => {
                // Losing the selection to another manager is one of the
                // ways this fails, and the crate has already latched
                // itself into standing down; dropping our handle stops
                // this session asking again once per scale change for
                // the rest of its life.
                tracing::warn!(%error, "could not republish XSETTINGS; giving up on it for this session");
                self.xsettings = None;
            }
        }
    }

    /// Brings the set of registered dockapp sources in line with what
    /// the shell is currently waiting on.
    ///
    /// # Why a source per fd rather than one for the listener
    ///
    /// A dockapp is not a Wayland client. It never opens a display
    /// connection — the shell strips `WAYLAND_DISPLAY` and `DISPLAY`
    /// from its environment before `exec` — so nothing in this
    /// compositor's own protocol machinery will ever hear from it. It
    /// is a process on the end of a `SOCK_SEQPACKET` socket, and the
    /// only thing this loop has to do about that is wake up when one
    /// has something to say. That is the *entire* Wayland-side cost of
    /// out-of-process dock tiles; the X11 binary pays the same cost by
    /// adding the same descriptors to its `poll` set.
    ///
    /// # Why nothing is read in the callback
    ///
    /// The callback is empty. Its only job is to end the
    /// `event_loop.dispatch` wait; `dispatch_pending` then services
    /// every dockapp regardless of which fd woke us, and each of those
    /// reads until `EAGAIN`. Level-triggered polling is therefore safe
    /// rather than a spin: the socket that woke us is always drained
    /// before the next wait begins.
    ///
    /// # Why the reconciliation runs *here*
    ///
    /// The registered `BorrowedFd` outlives the borrow checker's
    /// ability to prove it is valid, so validity is a property of this
    /// ordering: `dispatch_pending` services the dockapps (which is the
    /// only place a dockapp socket is ever closed) and *then* runs
    /// this, which unregisters the sources for any fd that went away —
    /// all before control returns to `event_loop.dispatch`, which is
    /// the only place calloop touches a registered descriptor. There is
    /// no point at which a source names a closed fd and something polls
    /// it.
    fn sync_dock_sources(&mut self) {
        let wanted = self.shell.extra_poll_fds();
        // Removals first, so a descriptor that was closed this pass is
        // unregistered before anything else can be inserted at the same
        // number.
        let mut kept = Vec::with_capacity(wanted.len());
        for (fd, token) in std::mem::take(&mut self.dock_sources) {
            if wanted.contains(&fd) {
                kept.push((fd, token));
            } else {
                self.loop_handle.remove(token);
            }
        }
        self.dock_sources = kept;

        for fd in wanted {
            if self.dock_sources.iter().any(|(known, _)| *known == fd) {
                continue;
            }
            // SAFETY: `fd` is owned by a `Seqpacket` or
            // `SeqpacketListener` inside the shell, and the source is
            // removed above on the same pass that drops the owner and
            // before calloop next polls — see the ordering argument in
            // this method's docs.
            let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
            let source = Generic::new(borrowed, Interest::READ, TriggerMode::Level);
            match self.loop_handle.insert_source(source, |_, _, _| Ok(PostAction::Continue)) {
                Ok(token) => self.dock_sources.push((fd, token)),
                Err(error) => {
                    // Not fatal, and worth being precise about why: the
                    // loop already wakes on a 16ms housekeeping bound,
                    // so an unregistered dockapp fd costs that tile up
                    // to 16ms of frame latency and nothing else. This
                    // is a latency optimisation, not a correctness
                    // requirement.
                    tracing::warn!(fd, ?error, "could not watch a dockapp socket; its frames will arrive on the housekeeping tick instead");
                }
            }
        }
    }

    /// Feeds one already-coalesced `PointerMotion` to the shell's drag
    /// trackers and then to `wm-core` — identical to the X11 binary's
    /// `dispatch_motion`, and split out for the same two call sites
    /// (mid-burst before a non-motion event, and once after the burst).
    fn dispatch_motion(&mut self, event: BackendEvent<WlWindowId, WlFrameId>) {
        if let BackendEvent::PointerMotion { root, .. } = &event {
            self.shell.on_motion(&mut self.wm, *root);
        }
        self.wm.dispatch(event);
    }

    /// Applies a [`ShellOutcome`] to the session. `Exit` and `Restart`
    /// both end the dispatch loop; `Restart` additionally asks [`run`]
    /// to re-exec the on-disk binary after teardown, which is the
    /// config/theme hot-reload gesture on both stacks.
    fn note_outcome(&mut self, outcome: ShellOutcome) {
        match outcome {
            ShellOutcome::Continue => {}
            ShellOutcome::Exit => self.running = false,
            ShellOutcome::Restart => {
                self.restart = true;
                self.running = false;
            }
        }
    }

    /// The host winit window changed size: retune the wayland output's
    /// mode and queue the resize for the loop's `take_screen_resize`
    /// drain, which is exactly where an X11 RandR change lands.
    ///
    /// Only the first output is touched, and that is not a shortcut:
    /// this is the nested backend's path, where the host window *is*
    /// the one output. The session backend never calls it — its outputs
    /// keep the mode they were set to at startup, and a real mode change
    /// (or a connector appearing) would have to re-lay-out every output
    /// and re-advertise all of them, which is the connector-hot-plug
    /// work `session.rs`'s module docs scope out.
    pub(crate) fn on_output_resized(&mut self, size: SSize<i32, Physical>) {
        let mode = Mode { size, refresh: 60_000 };
        let logical = Size::new(size.w.max(0) as u32, size.h.max(0) as u32);
        let Some(entry) = self.outputs.first_mut() else {
            return;
        };
        // A resize to the size the output already has changes nothing
        // and must cost nothing. This is not a hypothetical: winit
        // replays the host's configure as a `Resized` event on the
        // loop's first pass, so a nested session's very first dispatch
        // used to rebuild the damage tracker and — through
        // `pending_resize` → `Shell::on_screen_resize` — re-decode and
        // re-scale the wallpaper and repaint every piece of chrome it
        // had all just painted at this exact size during `Shell::new`.
        // At 2560x1600 in a debug build that is ~11 seconds of the
        // event loop answering nobody: a client's `get_registry` sat
        // queued behind a repaint of pixels that did not change.
        if entry.size == logical {
            return;
        }
        entry.output.change_current_state(Some(mode), None, None, None);
        entry.output.set_preferred(mode);
        entry.size = logical;
        // The damage tracker was sized to the old mode and is pinned
        // rather than output-tracking (see `physical_damage_tracker`),
        // so a resize rebuilds it. A fresh tracker starts at age 0,
        // which is what every frame here renders at anyway.
        entry.damage_tracker = physical_damage_tracker(&entry.output, logical);
        let backend = self.wm.backend_mut();
        if let Some(monitor) = backend.monitors.first_mut() {
            monitor.geometry.size = logical;
        }
        // Through the union rather than assigned directly, so the one
        // place that decides what "the screen" measures stays one place
        // even while only a single output can resize.
        backend.output_size = union_size(&backend.monitors);
        backend.pending_resize = Some(backend.output_size);
        backend.damage = true;
    }

    /// Copies each output's fractional scale onto the ledger
    /// (`WaylandBackend::monitor_scales`), which is where the `Backend`
    /// verbs and the renderer read it from. Called after anything
    /// changes an `OutputEntry::scale` — the two lists must never
    /// disagree, and this is the one direction data flows.
    pub(crate) fn sync_monitor_scales(&mut self) {
        let scales: Vec<f64> = self.outputs.iter().map(|entry| entry.scale).collect();
        self.wm.backend_mut().monitor_scales = scales;
    }

    /// Lands a deferred focus intent on the seat's keyboard — a
    /// window from `set_input_focus`, or nothing from
    /// `publish_active_window(None)` (see [`FocusIntent`] for the bug
    /// the second kind exists to prevent). A window that died in the
    /// meantime clears focus rather than leaving it on the previous
    /// window — matching what the X11 server does when a focused
    /// window disappears. A window that is still alive but has no
    /// `wl_surface` *yet* is retried instead, see below.
    fn apply_pending_focus(&mut self) {
        // While a session lock holds the seat, the intent stays parked
        // (an `Option` check per pass) rather than being applied or
        // dropped: applied, it would point the keyboard at a client
        // behind the lock; dropped, the unlock would restore focus to
        // a window `wm-core` had already moved on from.
        if self.wm.backend().locked {
            return;
        }
        let Some(intent) = self.wm.backend_mut().pending_focus.take() else {
            return;
        };
        let target = match intent {
            FocusIntent::Window(id) => Some(id),
            FocusIntent::Nothing => None,
        };
        // An XWayland window exists as an X11 window before Xwayland
        // binds a `wl_surface` to it, and that bind can land one or
        // more passes after the map that focused it. Clearing the seat
        // in that window (which this used to do) left `wm-core`
        // believing the window was focused while the seat held nothing
        // — a divergence `focus_client` cannot repair, because its
        // "already focused" early return is exactly the path a user
        // takes to try: clicking the window that already looks focused.
        // The window stayed keyboard-dead until they focused something
        // else and came back. So put the request back and retry on the
        // next pass; the association arrives (and this lands), or the
        // window dies (and the lookup below clears focus for real).
        let awaiting_surface = target.is_some_and(|id| {
            self.wm
                .backend()
                .windows
                .get(&id)
                .filter(|record| record.surface.alive())
                .is_some_and(|record| record.surface.wl_surface().is_none())
        });
        if awaiting_surface {
            self.wm.backend_mut().pending_focus = Some(intent);
            return;
        }
        // Focus is two things to a client: the seat's keyboard focus,
        // which decides where keys go, and an "I am the active window"
        // flag it reads for its own styling - a title bar, a caret that
        // blinks, an unfocused-dim treatment. Keyboard focus alone
        // leaves every client permanently drawing itself as background
        // furniture, so both kinds of surface get the flag here, and
        // every window gets it (not just the newly focused one) so the
        // one losing focus repaints inactive. When the intent is
        // `Nothing`, every window loses the flag — which is the half of
        // this that wakes a self-minimized Chromium on restore: the
        // unset here is a real state change, so the restore's re-set
        // produces a real configure instead of a dedup.
        for (window_id, record) in self.wm.backend().windows.iter() {
            let active = Some(*window_id) == target;
            match &record.surface {
                // xdg-shell carries it as a toplevel state on the next
                // configure; `send_pending_configure` dedups, so an
                // unchanged flag costs nothing.
                ManagedSurface::Xdg(toplevel) => {
                    if toplevel.alive() {
                        toplevel.with_pending_state(|state| {
                            if active {
                                state.states.set(XdgToplevelState::Activated);
                            } else {
                                state.states.unset(XdgToplevelState::Activated);
                            }
                        });
                        let _ = toplevel.send_pending_configure();
                    }
                }
                ManagedSurface::X11(surface) => {
                    if surface.alive() {
                        let _ = surface.set_activated(active);
                    }
                }
            }
        }
        // A layer surface holding *exclusive* keyboard interactivity
        // outranks window focus on the seat (the protocol's demand —
        // it is what a launcher types into). The activated flags above
        // still applied, so `wm-core`'s idea of the focused window
        // stays current and the seat returns to it the moment the
        // layer surface lets go (`layers::sync_keyboard`).
        if self.layer_shell.exclusive_focus.is_some() {
            return;
        }
        // A focus grab (`focus_grab.rs`) pins the keyboard to its
        // whitelist for as long as it holds, so window focus stops here
        // too — one rung below exclusive interactivity in that module's
        // ordering table. The activated flags above still applied, for
        // the same reason they do under an exclusive layer surface:
        // `wm-core`'s idea of the focused window stays current, and the
        // seat returns to it when the grab ends — re-derived from
        // `wm-core` at that moment rather than replayed, which is why
        // this intent is dropped here and not parked the way the lock
        // branch above parks it. Without this line the
        // click-to-focus a dismissing press queues would drag the
        // keyboard off an Omarchy menu the instant it opened over a
        // window.
        if self.focus_grab.is_active() {
            return;
        }
        // A `Nothing` intent falls through here with no surface, which
        // is the point: the seat drops the hidden window, so the keys
        // typed while it is miniaturized stop landing on it, and the
        // eventual restore is a real `wl_keyboard.enter` rather than a
        // smithay-deduplicated no-op.
        let surface = target
            .and_then(|id| self.wm.backend().windows.get(&id))
            .filter(|record| record.surface.alive())
            .and_then(|record| record.surface.wl_surface());
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, surface, SERIAL_COUNTER.next_serial());
        }
    }
}

/// Builds and runs the entire compositor session; returns when the
/// session ends (root-menu Exit, host window closed) or re-execs in
/// place on a requested restart. The Wayland analogue of everything
/// `crates/chonkstep/src/main.rs` does after config load — read that
/// file for the loop-order rationale this mirrors.
/// Whether some other desktop is already running here to nest inside.
/// A bare TTY login has neither variable set; a terminal inside
/// Hyprland, GNOME, or an X session has at least one.
fn nesting_desktop_present() -> bool {
    std::env::var_os("WAYLAND_DISPLAY").is_some() || std::env::var_os("DISPLAY").is_some()
}

pub fn run(config: wm_config::Config) -> Result<(), Box<dyn std::error::Error>> {
    let mut event_loop: EventLoop<Compositor> = EventLoop::try_new()?;
    let loop_handle = event_loop.handle();

    let display: Display<Compositor> = Display::new()?;
    let display_handle = display.handle();

    // Wayland globals. Each `new::<Compositor>` registers the global
    // against the delegate impls in `xdg.rs`/`input.rs`/`xwayland.rs`.
    // v6, not smithay's default v5: version 6 is what carries
    // `wl_surface.preferred_buffer_scale`, the direct statement of the
    // session's scale that `xdg.rs`'s commit handler (and the
    // live-rescale drain in `dispatch_pending`) sends per surface. The
    // `wl_output.scale` advertisement alone also works for a client's
    // first mapping, but a toolkit holding an already-mapped surface
    // follows a *change* of scale far more reliably when told about
    // its own surface than when left to re-derive it from the outputs
    // it has entered.
    let compositor_state = CompositorState::new_v6::<Compositor>(&display_handle);
    let xdg_shell_state = XdgShellState::new::<Compositor>(&display_handle);
    // xdg-decoration is what lets us tell clients "the server draws
    // your chrome" — without it every GTK/Qt app draws its own
    // titlebar and our chiseled frames would double up.
    let xdg_decoration_state = XdgDecorationState::new::<Compositor>(&display_handle);
    // `Server` is the whole point: `gdk_wayland_display_prefers_ssd()`
    // is a plain equality test against this value, and it decides
    // whether every GTK window on the desk draws its own titlebar.
    let kde_decoration = KdeDecorationState::new::<Compositor>(&display_handle, crate::decoration::KDE_DEFAULT_MODE);
    let shm_state = ShmState::new::<Compositor>(&display_handle, vec![]);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<Compositor>(&display_handle);
    let data_device_state = DataDeviceState::new::<Compositor>(&display_handle);
    let primary_selection_state = PrimarySelectionState::new::<Compositor>(&display_handle);
    // Data control rides on both selections above, so it is built from
    // the primary state rather than beside it — passing that state is
    // what makes middle-click selections visible to a clipboard
    // manager (see `data_control`'s module docs for the silent
    // half-working session the alternative produces).
    let data_control = crate::data_control::init(&display_handle, &primary_selection_state);
    let xwayland_shell_state = XWaylandShellState::new::<Compositor>(&display_handle);
    // The fractional-scale pair. Both are plain globals with no failure
    // mode; registered here so they exist before any client can bind —
    // the same timing rule as every global below.
    let fractional_scale_state =
        smithay::wayland::fractional_scale::FractionalScaleManagerState::new::<Compositor>(
            &display_handle,
        );
    let viewporter_state =
        smithay::wayland::viewporter::ViewporterState::new::<Compositor>(&display_handle);

    let mut seat_state: SeatState<Compositor> = SeatState::new();
    let mut seat: Seat<Compositor> = seat_state.new_wl_seat(&display_handle, "chonkstep");
    // Keyboard and pointer are assumed present — this is a desktop
    // compositor; hot-plug tracking can come with the session backend.
    // Keyboard layout from the environment, using libxkbcommon's own
    // XKB_DEFAULT_* convention: a session started from a TTY has no
    // desktop settings daemon to ask, and a compositor that hardcodes
    // a US layout is unusable for everyone else. `scripts/wayland-
    // session.sh` is where a login session sets these; the nested
    // backend inherits whatever the host desktop already exported.
    let xkb_env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let xkb_rules = xkb_env("XKB_DEFAULT_RULES").unwrap_or_default();
    let xkb_model = xkb_env("XKB_DEFAULT_MODEL").unwrap_or_default();
    let xkb_layout = xkb_env("XKB_DEFAULT_LAYOUT").unwrap_or_default();
    let xkb_variant = xkb_env("XKB_DEFAULT_VARIANT").unwrap_or_default();
    let xkb_options = xkb_env("XKB_DEFAULT_OPTIONS");
    let xkb_config = XkbConfig {
        rules: &xkb_rules,
        model: &xkb_model,
        layout: &xkb_layout,
        variant: &xkb_variant,
        options: xkb_options.clone(),
    };
    seat.add_keyboard(xkb_config, 200, 25)
        .map_err(|error| format!("failed to initialize the seat keyboard: {error}"))?;
    seat.add_pointer();

    // Which kind of session this process is. Owning the hardware and
    // living in a window on somebody else's desktop are the same
    // desktop with different plumbing, so the choice is made here, at
    // startup, rather than at build time - one binary has to serve
    // both "I am logging in from a TTY" and "I am previewing this
    // inside my existing desktop". `CHONKSTEP_BACKEND` forces the
    // decision ("drm"/"session" or "winit"/"nested"); otherwise an
    // existing `WAYLAND_DISPLAY` or `DISPLAY` means there is already a
    // desktop here to nest inside, and their absence means a bare TTY.
    let nested = match std::env::var("CHONKSTEP_BACKEND").ok().as_deref() {
        Some("winit") | Some("nested") => true,
        Some("drm") | Some("session") => false,
        Some(other) => {
            tracing::warn!(backend = other, "unknown CHONKSTEP_BACKEND value; deciding automatically");
            nesting_desktop_present()
        }
        None => nesting_desktop_present(),
    };

    let (mut graphics, output_setups) = if nested {
        tracing::info!("nested backend: rendering into a window on the host desktop");
        let (winit_backend, winit_source) = winit::init::<GlesRenderer>()
            .map_err(|error| format!("winit backend init failed: {error}"))?;
        let window_size = winit_backend.window_size();

        // Flipped180 is deliberately copied from Smithay's own winit
        // references (anvil, smallvil): the winit EGL surface's
        // coordinate origin differs from the output's, and this
        // transform is how upstream squares the two. The session
        // backend's outputs need no such correction, which is why the
        // transform lives in this arm and not in the shared scene.
        let mode = Mode { size: window_size, refresh: 60_000 };
        let output = Output::new(
            "chonkstep".to_string(),
            PhysicalProperties {
                size: (0, 0).into(),
                subpixel: Subpixel::Unknown,
                make: "chonkstep".into(),
                model: "winit".into(),
            },
        );
        output.change_current_state(Some(mode), Some(Transform::Flipped180), None, Some((0, 0).into()));
        output.set_preferred(mode);

        // Host-window events: input feeds the translation layer in
        // `input.rs` (the seam where raw winit input becomes seat
        // events plus `BackendEvent`s per the wm-x11 contract);
        // everything else is resize/redraw/close plumbing.
        loop_handle
            .insert_source(winit_source, |event, _, comp| match event {
                WinitEvent::Resized { size, .. } => comp.on_output_resized(size),
                WinitEvent::Input(event) => crate::input::process_input_event(comp, event),
                // The host asked us to repaint (the window was exposed
                // or resized): full-frame damage, same as any scene
                // change.
                WinitEvent::Redraw => comp.wm.backend_mut().mark_damaged(),
                WinitEvent::CloseRequested => comp.running = false,
                WinitEvent::Focus(_) => {}
            })
            .map_err(|error| format!("failed to register the winit event source: {error}"))?;

        let size = Size::new(window_size.w.max(0) as u32, window_size.h.max(0) as u32);
        // One output at the origin: a host window is one screen by
        // construction, and everything downstream — the ledger's
        // monitor list, the renderer's viewport offset, the pointer
        // clamp — then takes the degenerate single-monitor case of the
        // multi-monitor path rather than a path of its own.
        (
            Graphics::Winit(winit_backend),
            vec![OutputSetup { output, position: Point::new(0, 0), size, modes: vec![mode] }],
        )
    } else {
        tracing::info!("session backend: taking over the DRM device and input");
        let init = crate::session::init(&loop_handle, &display_handle)?;
        (init.graphics, init.outputs)
    };
    let mut outputs: Vec<OutputEntry> = output_setups
        .into_iter()
        .map(|setup| OutputEntry::new(setup, &display_handle))
        .collect();
    // The ledger's copy of the layout, which is what `wm-core` and the
    // shell see through `Backend::monitors`. Built here, from the same
    // list the renderer draws through, so the two can never disagree
    // about where a monitor is. The first output is primary: on the
    // session backend that is the first connected connector in kernel
    // enumeration order, and there is no protocol or config that could
    // currently say otherwise (see `session.rs`'s module docs).
    let monitors: Vec<MonitorInfo> = outputs
        .iter()
        .enumerate()
        .map(|(index, entry)| MonitorInfo {
            geometry: Rect { pos: entry.position, size: entry.size },
            name: entry.output.name(),
            primary: index == 0,
        })
        .collect();
    for monitor in &monitors {
        tracing::info!(
            output = %monitor.name,
            primary = monitor.primary,
            x = monitor.geometry.pos.x,
            y = monitor.geometry.pos.y,
            width = monitor.geometry.size.w,
            height = monitor.geometry.size.h,
            "monitor in the desktop layout"
        );
    }

    // linux-dmabuf, before the listening socket exists: a global that
    // is missing when a client binds might as well never exist, and
    // GPU clients read its absence as "this compositor has no GPU".
    // Both backends own a `GlesRenderer`, so one call serves both.
    let dmabuf = crate::dmabuf::init_for_graphics(&display_handle, &mut graphics);
    // Explicit sync rides the same timing rule, and additionally only
    // exists where a DRM device backs the session — its global is what
    // lets a client (Chromium on NVIDIA, most famously) tell us when a
    // dmabuf is actually finished instead of us sampling mid-render.
    let syncobj = crate::dmabuf::init_syncobj(&display_handle, &graphics);
    // Same timing rule as dmabuf: bound before any client can connect.
    let protocols = crate::protocols::init(&display_handle);
    let output_mgmt = crate::output_mgmt::init(&display_handle);
    // And again for the one protocol with no crate behind it: this is
    // the global Omarchy's Quickshell looks for the moment it connects,
    // and finding it absent is what makes every popout in that shell
    // impossible to dismiss by clicking away. See `focus_grab.rs`.
    let focus_grab = crate::focus_grab::init(&display_handle);
    // The ecosystem protocols, under the same timing rule. Layer-shell
    // is what fuzzel/mako/waybar look for the moment they connect;
    // session-lock is unfiltered (any client may lock — swaylock is
    // just a client, and a filter would only be worth its complexity
    // with a sandboxing story this desktop does not have); the idle
    // notifier's timers live on this very event loop.
    let layer_shell = crate::layers::LayerShell::new(
        smithay::wayland::shell::wlr_layer::WlrLayerShellState::new::<Compositor>(&display_handle),
    );
    let session_lock = crate::lock::SessionLock::new(
        smithay::wayland::session_lock::SessionLockManagerState::new::<Compositor, _>(
            &display_handle,
            |_| true,
        ),
    );
    let idle = crate::idle::Idle::new(
        smithay::wayland::idle_notify::IdleNotifierState::<Compositor>::new(
            &display_handle,
            loop_handle.clone(),
        ),
        smithay::wayland::idle_inhibit::IdleInhibitManagerState::new::<Compositor>(&display_handle),
    );
    tracing::info!("layer-shell, session-lock and idle-notify advertised");
    // Same timing rule again, plus one of its own: the seat this is
    // handed must already carry a keyboard, because smithay's handler
    // unwraps it on the first synthetic key. `add_keyboard` above is
    // what guarantees that, and `virtual_keyboard::init` says so out
    // loud if it ever stops being true.
    let virtual_keyboard = crate::virtual_keyboard::init(&display_handle, &seat);

    // The listening socket clients connect to, plus the display's own
    // fd so wayland-server processes client requests — both plain
    // calloop sources.
    let listening_socket = ListeningSocketSource::new_auto()?;
    let socket_name = listening_socket.socket_name().to_os_string();
    loop_handle
        .insert_source(listening_socket, |client_stream, _, comp| {
            if let Err(error) = comp
                .display_handle
                .insert_client(client_stream, Arc::new(ClientState::default()))
            {
                tracing::warn!(?error, "failed to admit a wayland client");
            }
        })
        .map_err(|error| format!("failed to register the wayland socket source: {error}"))?;
    loop_handle
        .insert_source(
            Generic::new(display, Interest::READ, TriggerMode::Level),
            |_, display, comp| {
                // SAFETY: the display is owned by this source and never
                // moved out of it; `get_mut` is the documented access
                // pattern for dispatching from inside calloop.
                if let Err(error) = unsafe { display.get_mut().dispatch_clients(comp) } {
                    tracing::error!(?error, "wayland display dispatch failed, shutting down");
                    comp.running = false;
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|error| format!("failed to register the wayland display source: {error}"))?;

    // This binary IS the Wayland session, and says so rather than
    // leaving the shell to deduce it from `/proc/self/exe` — which
    // answers " (deleted)" for any session whose binary has been
    // rebuilt under it, and silently launched every browser on
    // XWayland at double scale when it did.
    chonk_shell::spawn::declare_display_stack(chonk_shell::spawn::DisplayStack::Wayland);

    // Take the hot-restart marker out of the environment beside the
    // stack declaration, for the same two reasons: both are one-shot
    // facts about this process, and both have to be settled before any
    // thread exists — this one because `remove_var` is only sound while
    // single-threaded, and because everything this session spawns
    // inherits whatever is left behind here.
    chonk_shell::startup::consume_session_continuation();
    // Children the shell spawns find the session through the
    // environment, so it must be set before `Shell::new` (which may
    // autostart things) and before XWayland comes up. `DISPLAY`
    // follows once XWayland reports ready.
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    tracing::info!(socket = ?socket_name, "wayland socket listening");

    // Session policy comes from `chonk_shell::startup`, shared with the
    // X11 binary: same env-over-config precedence, same theme
    // resolution, same cursor sizing. A compositor that resolved these
    // its own way is exactly how the two sessions would drift.
    //
    // Resolved as one `SessionState` rather than value by value, and by
    // the same call the reload below makes, so a session that has been
    // reloaded a dozen times is indistinguishable from one that started
    // where it now stands.
    let state = SessionState::resolve(&config);
    // Kept separately because `state` is handed off to the applier
    // below, and the compositor's own pointer is sized from the scale
    // here (and only here — every later change to it arrives through
    // `pending_cursor_scale`).
    let scale = state.scale;
    tracing::info!(scale, "UI scale (config `scale`; CHONKSTEP_SCALE overrides)");
    // XWayland clients draw their own Xcursor pointers and have no way
    // to learn this session's scale otherwise.
    ensure_xcursor_size(scale);
    let theme = state.theme();
    tracing::info!(theme = %theme.id, "theme loaded");
    // The font database is built out here rather than inside the engine
    // so the shell can hold on to it and build this engine's
    // replacements around the same one on every later restyle. A
    // restyle must restyle and not re-scan fontconfig — doubly so on
    // this stack, where the process that would block on that scan is
    // the display server every client is waiting on. See
    // `wm_theme::FontState`.
    let fonts = FontState::new();
    let engine = RasterThemeEngine::with_fonts(theme, fonts.clone());

    // The outputs advertise the session's scale from here on — the only
    // way a native Wayland client ever learns this desktop is scaled
    // (the per-child `GDK_SCALE`/`QT_SCALE_FACTOR` environment the
    // launcher sets is ignored by toolkits for buffer scale). The first
    // attempt at this set `change_current_state(scale = 2)` alone and
    // was reverted: with the damage trackers reading their scale from
    // the outputs, every chrome element — a memory buffer at a
    // device-pixel position with a device-pixel size — was multiplied
    // to double size while the wallpaper stayed anchored at the origin
    // and the dock at the right edge, so the wallpaper showed a
    // quarter of itself and the dock grew off the screen. The full
    // account of which coordinate space each half of smithay's
    // pipeline works in lives on `physical_damage_tracker`, and the
    // fix is split exactly along it: the advertisement is protocol
    // metadata (here and in `advertise_scale`), the compositor's own
    // composition stays pinned to physical pixels
    // (`physical_damage_tracker`, `session::attach_output`), and
    // client buffers are put back at 1 buffer pixel : 1 screen pixel
    // per surface (`renderer::push_surface_tree`). `wm-core`, the
    // theme and the shell remain physical end to end and never hear
    // about any of it. Fractional-scale clients additionally hear the
    // exact fraction per surface (see `xdg.rs`'s commit handler).
    advertise_scale(&mut outputs, scale);

    // The desktop shell is built against the mutable backend before
    // `WindowManager::new` takes ownership — the exact construction
    // order the X11 binary uses, for the exact same borrow reason.
    let mut backend = WaylandBackend::new(display_handle.clone(), monitors, scale);
    // The whole screen, as the shell sizes the desktop against it: the
    // union of every monitor. Where the dock and the workareas land
    // inside that union is the shell's decision, not this loop's.
    let output_size = backend.output_size;
    let mut shell = Shell::new(&mut backend, &state, fonts);
    // No `scan_existing_windows` here: a compositor's clients cannot
    // predate the compositor. (Hot-restart adoption is impossible for
    // the same reason — see `Compositor::restart`.)
    let mut wm = WindowManager::new(backend, Box::new(engine));
    // Session policy — focus, placement, edge resistance, the keymap
    // and the grabs that go with it — is applied through the very call
    // a live reload makes, in place of the setter block that used to
    // stand here. Two places that set a setting is how a setting ends
    // up reloadable but not startable, or the reverse. The look half
    // costs nothing: the shell was just built from this same state, so
    // the applier finds nothing changed there and repaints nothing.
    //
    // The config's combos still reach `grab_key`, one layer down, and
    // that still matters here for the reason it always did: a "grab" on
    // this stack is only a filter entry — the compositor sees every key
    // regardless — but going through the same path keeps `wm-core`'s
    // bookkeeping, and any future per-combo policy, identical on both
    // stacks. `wm-core`'s own modal Alt+Tab grabs belong to
    // `bind_default_keys` below; the applier only ever reconciles the
    // grabs the *config* asked for.
    shell.apply_session_state(&mut wm, state);
    wm.set_workarea(shell.workarea(output_size));
    wm.bind_default_keys();

    // XWayland: spawned here, attached (X11Wm::start_wm) when it
    // reports ready. Failure to start is a degraded session — X11
    // apps unavailable — not a dead one, so it logs instead of
    // erroring out.
    match XWayland::spawn(
        &display_handle,
        None,
        std::iter::empty::<(String, String)>(),
        true,
        Stdio::null(),
        Stdio::null(),
        |_| (),
    ) {
        Ok((xwayland, xwayland_client)) => {
            loop_handle
                .insert_source(xwayland, move |event, _, comp| match event {
                    XWaylandEvent::Ready { x11_socket, display_number } => {
                        match X11Wm::start_wm(comp.loop_handle.clone(), x11_socket, xwayland_client.clone()) {
                            Ok(xwm) => {
                                comp.xwm = Some(xwm);
                                comp.xdisplay = Some(display_number);
                                // Children the shell spawns (terminals,
                                // X11 apps) inherit this, same as the
                                // X11 session's DISPLAY inheritance.
                                std::env::set_var("DISPLAY", format!(":{display_number}"));
                                tracing::info!(display = display_number, "XWayland ready");
                                // The earliest moment an X selection can
                                // be taken, and the only one worth
                                // taking it at: there is no display to
                                // own before this, and every X client
                                // that will ever run in this session
                                // connects after it.
                                comp.start_xsettings(display_number);
                                // And the EWMH publisher, on its own
                                // connection for the same two-readers
                                // reason `start_xsettings` gives.
                                crate::xewmh::start(comp, display_number);
                            }
                            Err(error) => {
                                tracing::error!(?error, "failed to attach the X11 window manager to XWayland");
                            }
                        }
                    }
                    XWaylandEvent::Error => {
                        tracing::warn!("XWayland exited or failed to start; X11 apps unavailable");
                        // Every X11 window died with it. Nothing else
                        // will ever report those surfaces destroyed -
                        // the destroy notifications came through the
                        // X11 WM connection that just went away - so
                        // without this their ledger entries, frames and
                        // stacking slots outlive them: chrome painted
                        // around windows that no longer exist, and
                        // clicks routed into them. Tearing them down
                        // through the normal Destroyed path lets
                        // `wm-core` retract focus and drop decorations
                        // exactly as it would for one window closing.
                        comp.xwm = None;
                        comp.xdisplay = None;
                        // The EWMH connection pointed at the display
                        // that just died; its next write would only
                        // fail noisily.
                        comp.xewmh = None;
                        let backend = comp.wm.backend_mut();
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
                    }
                })
                .map_err(|error| format!("failed to register the XWayland event source: {error}"))?;
        }
        Err(error) => {
            tracing::warn!(?error, "could not spawn XWayland; X11 apps unavailable");
        }
    }

    let mut comp = Compositor {
        wm,
        shell,
        display_handle,
        loop_handle,
        compositor_state,
        xdg_shell_state,
        xdg_decoration_state,
        kde_decoration,
        shm_state,
        seat_state,
        output_manager_state,
        data_device_state,
        primary_selection_state,
        data_control,
        xwayland_shell_state,
        fractional_scale_state,
        viewporter_state,
        popups: PopupManager::default(),
        dock_sources: Vec::new(),
        seat,
        outputs,
        xwm: None,
        xdisplay: None,
        xsettings: None,
        xewmh: None,
        ui_scale: scale,
        graphics,
        dmabuf,
        syncobj,
        protocols,
        output_mgmt,
        layer_shell,
        session_lock,
        focus_grab,
        idle,
        _virtual_keyboard: virtual_keyboard,
        pointer_location: (0.0, 0.0).into(),
        cursor_status: CursorImageStatus::default_named(),
        cursors: CursorSet::build(scale),
        start_time: Instant::now(),
        running: true,
        restart: false,
    };

    // The end-to-end test door: a control socket for injected input,
    // opened only when CHONKSTEP_TEST_SOCKET is set (a user session
    // pays one env lookup here and nothing else). See `test_door.rs`.
    crate::test_door::init(&comp.loop_handle);

    // Crash recovery, the moment the session is otherwise up: the
    // supervisor in `scripts/wayland-session.sh` drops the `recovery`
    // marker only when it re-execs us after an *abnormal* exit, and
    // consuming it here (a destructive read, beside the restart/reload
    // markers the loop below polls) is how this process learns it is a
    // resurrection. A crashed desktop comes back with the user quite
    // possibly away from the keyboard, so if a locker is configured it
    // is spawned right now — it connects to the socket exported above
    // and locks through the ext-session-lock implementation in
    // `lock.rs` like any other locker would. Spawned before the first
    // dispatch on purpose: the lock lands before any client the
    // restored session relaunches can draw a frame of the user's work.
    if recovering_from_crash() {
        tracing::error!(
            "RECOVERED FROM A CRASH: the previous compositor process exited abnormally and the session supervisor restarted it"
        );
        match config.lock_command.as_deref() {
            Some(command) => {
                let mut parts = command.split_whitespace();
                // The config layer already rejects empty strings, so
                // `parts` always yields a program here — but a config
                // key is user input, and "impossible" input does not
                // get to panic the recovered session it exists to
                // protect.
                if let Some(program) = parts.next() {
                    let args: Vec<&str> = parts.collect();
                    tracing::warn!(locker = command, "locking the recovered session");
                    chonk_shell::spawn::spawn_detached(program, &args);
                }
            }
            None => tracing::warn!(
                "no lock_command configured — the recovered session is coming back UNLOCKED; set lock_command in config.toml to lock after a crash"
            ),
        }
    }

    tracing::info!("entering compositor loop");
    while comp.running {
        // Hot-restart marker from `scripts/restart.sh`, polled once
        // per wakeup exactly as the X11 loop polls it.
        if restart_requested() {
            tracing::info!("restart requested — re-executing in place");
            comp.restart = true;
            break;
        }
        // Its cheaper sibling, polled beside it, and on this stack
        // almost always the one a user wants: a reload re-reads the
        // config and moves the running session onto it, where a restart
        // here costs every client on the screen (there is no SaveSet to
        // hand them forward — see `restart_in_place`). `wm_config::load`
        // cannot fail, so the worst a mistyped edit does to a live
        // session is move it to the defaults, which is exactly what a
        // restart with the same file would have done.
        if reload_requested() {
            tracing::info!("reload requested — re-reading the config and applying it in place");
            let reloaded = wm_config::load();
            // Everything this reload touches — decoration rules and
            // the drag modifier included — travels through
            // `SessionState`, so the marker file and the bound `reload`
            // key apply exactly the same set. They did not: the
            // decoration policy was assigned here and nowhere else, so
            // the key silently skipped it.
            let state = SessionState::resolve(&reloaded);
            comp.shell.apply_session_state(&mut comp.wm, state);
        }
        // Blocks on every source at once (wayland clients, winit
        // input, XWayland) with the housekeeping bound — the calloop
        // equivalent of the X11 loop's poll-on-the-socket-fd.
        event_loop.dispatch(Some(HOUSEKEEPING_INTERVAL), &mut comp)?;
        comp.dispatch_pending();
    }

    // Whatever ended the loop — the root menu's Exit, a theme pick, a
    // touched restart marker — the dockapps this session launched are
    // its responsibility. On a restart they are left running and their
    // tokens are handed to the incoming compositor, which readopts them;
    // on a real exit they are stopped. See `Shell::shut_down`.
    //
    // Worth naming here of all places, because this is the backend where
    // it is remarkable: a Wayland client dies with the compositor's
    // socket and there is no SaveSet equivalent to adopt it afterwards
    // (README, "Restart costs you your clients"). A dockapp is not a
    // Wayland client, so it survives the restart that kills every window
    // on the screen — strictly more than any ordinary client here gets.
    comp.shell.shut_down(if comp.restart { Farewell::Restarting } else { Farewell::SessionOver });

    if comp.restart {
        // `nested` is the decision this process made at startup, so the
        // replacement makes the same one rather than re-deriving it
        // from an environment this compositor itself wrote.
        restart_in_place(nested);
    }
    tracing::info!("compositor session over");
    Ok(())
}

/// Re-execs the on-disk binary in place — resolved from `argv[0]`, not
/// `current_exe()`, for the same pick-up-the-fresh-build reason the
/// X11 binary documents at length. The one behavioral difference is
/// unavoidable: Wayland clients die with our socket (no SaveSet), so
/// the fresh process starts with an empty session.
fn restart_in_place(nested: bool) -> ! {
    use std::os::unix::process::CommandExt;
    let bin = std::env::args_os().next().unwrap_or_else(|| "chonkstep-wayland".into());
    let mut command = std::process::Command::new(&bin);
    // Pin the backend across the re-exec instead of letting the new
    // process guess again.
    //
    // A compositor exports `WAYLAND_DISPLAY` (and `DISPLAY`, through
    // XWayland) into its own environment so the apps it spawns find it.
    // `exec` keeps that environment, so a session restarted from a TTY
    // would wake up seeing both variables set, conclude from them that
    // a desktop is already running here, and try to nest inside the
    // compositor it just replaced - which is gone, taking the session
    // with it. Passing the decision explicitly is the fix; the sockets
    // themselves are stale after the exec either way, so they are
    // cleared rather than handed to the new process.
    command.env("CHONKSTEP_BACKEND", if nested { "winit" } else { "drm" });
    // A deliberate hot restart is a continuation of the same session,
    // not a fresh login — session-layout restore must not fire from
    // it. On this stack the distinction is subtle (the clients die
    // with the socket either way), but relaunching here would also
    // relaunch on every `scripts/update.sh`, turning "pick up a new
    // build" into "and also spawn last week's window list". The crash
    // path never runs through here — a crashed compositor is re-execed
    // by the session supervisor with a fresh environment, which is
    // exactly when restore *should* fire.
    command.env("CHONKSTEP_SESSION_CONTINUES", "1");
    if !nested {
        command.env_remove("WAYLAND_DISPLAY");
        command.env_remove("DISPLAY");
    }
    let err = command.exec();
    tracing::error!(?err, bin = ?bin, "re-exec failed; exiting instead of restarting");
    std::process::exit(1);
}

/// The built-in pointer: the classic black arrow with a white halo,
/// matching the scaled cursor the X11 backend draws for the root
/// window. Hand-authored rather than loaded from an Xcursor theme —
/// the compositor must have a cursor before any theme machinery could
/// run, and clients that care set their own via `wl_pointer.set_cursor`
/// anyway.
///
/// `scale` is the session's UI scale, and it has to be baked into the
/// pixels here because nothing downstream will apply it: the buffer is
/// drawn by `renderer::push_cursor_elements` with no explicit size, so
/// smithay sizes the element at the buffer's own dimensions divided by
/// its buffer scale (`element::memory`'s `from_buffer`), and every
/// damage tracker in this session renders at scale 1 whatever the
/// outputs advertise (see `physical_damage_tracker`), so that size
/// reaches the screen as-is. A fixed 1x arrow therefore reached the
/// screen at 1x pixels next to `scaled(scale)` chrome and clients'
/// `XCURSOR_SIZE`-sized pointers — the "tiny cursor over the desktop,
/// right-sized cursor over a window" report.
///
/// Raising the *buffer scale* argument instead would move the bug, not
/// fix it: a buffer scale of 2 tells smithay these pixels are 2 per
/// logical unit, which halves the element on a scale-1 output.
/// One pointer image plus the pixel of it that sits on the pointer's
/// position. The arrow's hotspot is its (0, 0) tip; a resize
/// double-arrow's is its center, because the shape *points across* the
/// position it marks — drawn from its corner, the visible crosshair of
/// the arrows would float below and right of the edge the user is
/// actually about to grab.
pub(crate) struct CursorSprite {
    pub(crate) buffer: MemoryRenderBuffer,
    pub(crate) hotspot: (i32, i32),
}

/// Every cursor this compositor draws itself, pre-rendered once per UI
/// scale (not lazily per-hover) — the same set, from the same
/// hand-authored shapes, as `wm-x11`'s `Cursors`. Diagonals are shared
/// between opposite corners because the cursor shows the resize *axis*:
/// SouthEast and NorthWest stretch along the same ↘ line.
pub(crate) struct CursorSet {
    arrow: CursorSprite,
    resize_vertical: CursorSprite,
    resize_horizontal: CursorSprite,
    resize_southeast: CursorSprite,
    resize_southwest: CursorSprite,
}

impl CursorSet {
    pub(crate) fn build(scale: f32) -> Self {
        let right_angle = 90.0_f32.to_radians();
        Self {
            arrow: CursorSprite { buffer: build_default_cursor(scale), hotspot: (0, 0) },
            resize_vertical: build_resize_cursor(scale, 0.0),
            // East/West: the same double-arrow turned to horizontal;
            // the diagonals are the 45° rotations between them — the
            // construction `wm-x11`'s `Cursors::create` uses. The
            // angles are assigned by the picture they produce, not
            // copied from there: with this rotation's sign convention
            // (screen coordinates, y down), -45° is the ⤡ arrow along
            // the NW–SE axis and +45° the ⤢ along NE–SW — verified by
            // rasterizing both, because the first draft copied the X11
            // literals and put each diagonal on the corner the OTHER
            // one resizes.
            resize_horizontal: build_resize_cursor(scale, right_angle),
            resize_southeast: build_resize_cursor(scale, -right_angle / 2.0),
            resize_southwest: build_resize_cursor(scale, right_angle / 2.0),
        }
    }

    pub(crate) fn arrow(&self) -> &CursorSprite {
        &self.arrow
    }

    /// The sprite for a resize edge — the mapping `wm-x11`'s
    /// `Cursors::for_edge` uses, minus the `None` arm the caller
    /// spells as [`CursorSet::arrow`].
    pub(crate) fn for_edge(&self, edge: ResizeEdge) -> &CursorSprite {
        match edge {
            ResizeEdge::North | ResizeEdge::South => &self.resize_vertical,
            ResizeEdge::East | ResizeEdge::West => &self.resize_horizontal,
            ResizeEdge::SouthEast | ResizeEdge::NorthWest => &self.resize_southeast,
            ResizeEdge::SouthWest | ResizeEdge::NorthEast => &self.resize_southwest,
        }
    }
}

/// Imports one cursor's pixels. RGBA byte order is the little-endian
/// DRM fourcc Abgr8888, NOT Argb8888 — mixing those up swaps red and
/// blue. Fully opaque or fully transparent pixels only, so these bytes
/// are also already valid premultiplied alpha, which is what the GLES
/// renderer's blending expects (and what tiny-skia's `data()` provides
/// for the decoration buffers `backend_impl` imports the same way).
///
/// Buffer scale 1 for the reason `backend_impl::import_buffer`
/// documents for decoration buffers: this session's ledger is in
/// physical pixels, so a buffer already rasterized at the UI scale is
/// 1 buffer pixel per unit of that space.
fn import_cursor(pixels: &[u8], width: i32, height: i32) -> MemoryRenderBuffer {
    MemoryRenderBuffer::from_slice(
        pixels,
        Fourcc::Abgr8888,
        (width, height),
        1,
        Transform::Normal,
        None,
    )
}

fn build_default_cursor(scale: f32) -> MemoryRenderBuffer {
    let (pixels, width, height) = default_cursor_pixels(scale);
    import_cursor(&pixels, width, height)
}

fn build_resize_cursor(scale: f32, angle_rad: f32) -> CursorSprite {
    let (pixels, width, height, hotspot) = resize_cursor_pixels(scale, angle_rad);
    CursorSprite { buffer: import_cursor(&pixels, width, height), hotspot }
}

/// The arrow's premultiplied RGBA8 pixels at `scale`, with its width
/// and height. Split out from [`build_default_cursor`] because the
/// scaling is the part worth testing and `MemoryRenderBuffer` exposes
/// no dimensions to assert on.
///
/// Nearest-neighbour pixel replication rather than a filtered resample:
/// the source is a hand-placed 1-bit shape with a one-pixel halo, and
/// interpolating it would blur that halo into grey fringing at exactly
/// the theme's hard-edged aesthetic. Sampling from the source origin
/// also keeps the hotspot correct for free — the tip sits at pixel
/// (0, 0) and `push_cursor_elements` draws this buffer at the pointer
/// position with no hotspot offset, and (0, 0) maps to (0, 0) under any
/// scale factor.
fn default_cursor_pixels(scale: f32) -> (Vec<u8>, i32, i32) {
    // '#' = black shape, 'o' = white halo, anything else transparent.
    // Rows may be ragged; missing trailing cells are transparent.
    const ARROW: [&str; 19] = [
        "o",
        "oo",
        "o#o",
        "o##o",
        "o###o",
        "o####o",
        "o#####o",
        "o######o",
        "o#######o",
        "o########o",
        "o#####oooo",
        "o##o##o",
        "o#o o##o",
        "oo  o##o",
        "o    o##o",
        "     o##o",
        "      o##o",
        "      o##o",
        "       oo",
    ];
    let source_height = ARROW.len();
    let source_width = ARROW.iter().map(|row| row.len()).max().unwrap_or(1);
    // Never below 1x, matching `wm-x11`'s `create_scaled_cursor`: a
    // sub-1 scale is a config typo, and shrinking the pointer below the
    // hand-placed shape loses the halo entirely. (`f32::max` also
    // returns 1.0 for a NaN scale, so the cast below is always sane.)
    let scale = scale.max(1.0) as f64;
    let width = ((source_width as f64 * scale).round() as usize).max(1);
    let height = ((source_height as f64 * scale).round() as usize).max(1);
    let mut pixels = vec![0u8; width * height * 4];
    for y in 0..height {
        // `min` guards the row a rounded-up edge pixel would sample
        // past the source for.
        let row = ARROW[((y as f64 / scale) as usize).min(source_height - 1)].as_bytes();
        for x in 0..width {
            let value: Option<[u8; 4]> = match row.get((x as f64 / scale) as usize) {
                Some(b'#') => Some([0, 0, 0, 0xFF]),
                Some(b'o') => Some([0xFF, 0xFF, 0xFF, 0xFF]),
                _ => None,
            };
            if let Some(rgba) = value {
                let at = (y * width + x) * 4;
                pixels[at..at + 4].copy_from_slice(&rgba);
            }
        }
    }
    (pixels, width as i32, height as i32)
}

/// The double-headed resize arrow (⇕), the same polygon `wm-x11`'s
/// `CURSOR_RESIZE_ARROW` hands the X server, traced as one outline:
/// apex of the top triangle, down its right side, along the shaft, out
/// to the bottom triangle, back up the other side. All four resize
/// cursors are rotations of this one shape about its center.
const CURSOR_RESIZE_ARROW: &[(f32, f32)] = &[
    (5.0, 0.0),
    (10.0, 6.0),
    (7.0, 6.0),
    (7.0, 14.0),
    (10.0, 14.0),
    (5.0, 20.0),
    (0.0, 14.0),
    (3.0, 14.0),
    (3.0, 6.0),
    (0.0, 6.0),
];
const CURSOR_RESIZE_ARROW_CENTER: (f32, f32) = (5.0, 10.0);

/// Even-odd point-in-polygon (ray casting toward +x). Sampled at pixel
/// centers below, so a pixel is part of the shape when its center is
/// inside the outline — the software spelling of the X server's
/// `FillPoly` this replaces.
fn polygon_contains(polygon: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    let mut j = polygon.len() - 1;
    for i in 0..polygon.len() {
        let (xi, yi) = polygon[i];
        let (xj, yj) = polygon[j];
        if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The resize arrow's premultiplied RGBA8 pixels at `scale`, rotated by
/// `angle_rad` about its center, with the buffer size and the hotspot —
/// the rotated center — in buffer coordinates. The rendering follows
/// `wm-x11`'s `create_scaled_cursor` step for step (scale, rotate,
/// shift into a non-negative box, black shape over a white halo) so the
/// two backends' resize pointers are one drawing; the halo is the
/// shape dilated by the same clamped radius, and it exists for the same
/// reason as the arrow's — a flat black glyph disappears against a
/// dark window at exactly the edge the user is squinting at.
fn resize_cursor_pixels(scale: f32, angle_rad: f32) -> (Vec<u8>, i32, i32, (i32, i32)) {
    let s = scale.max(1.0);
    let (cx, cy) = CURSOR_RESIZE_ARROW_CENTER;
    let (sin, cos) = angle_rad.sin_cos();
    let outline: Vec<(f32, f32)> = CURSOR_RESIZE_ARROW
        .iter()
        .map(|&(x, y)| {
            let (dx, dy) = (x - cx, y - cy);
            ((dx * cos - dy * sin + cx) * s, (dx * sin + dy * cos + cy) * s)
        })
        .collect();
    let hotspot_f = (cx * s, cy * s);

    let min_x = outline.iter().map(|p| p.0).fold(hotspot_f.0, f32::min);
    let min_y = outline.iter().map(|p| p.1).fold(hotspot_f.1, f32::min);
    let max_x = outline.iter().map(|p| p.0).fold(hotspot_f.0, f32::max);
    let max_y = outline.iter().map(|p| p.1).fold(hotspot_f.1, f32::max);

    // Same halo radius and margin as the X11 rasterizer, so the two
    // stay the same picture at every scale.
    let halo = (s.round() as i32).clamp(1, 3);
    let margin = halo + 1;
    let shift_x = margin as f32 - min_x;
    let shift_y = margin as f32 - min_y;
    let width = (((max_x - min_x).round() as i32) + margin * 2).max(1) as usize;
    let height = (((max_y - min_y).round() as i32) + margin * 2).max(1) as usize;
    let hotspot = (
        (hotspot_f.0 + shift_x).round() as i32,
        (hotspot_f.1 + shift_y).round() as i32,
    );

    let shifted: Vec<(f32, f32)> =
        outline.iter().map(|&(x, y)| (x + shift_x, y + shift_y)).collect();
    let shape: Vec<bool> = (0..width * height)
        .map(|i| {
            let (x, y) = ((i % width) as f32 + 0.5, (i / width) as f32 + 0.5);
            polygon_contains(&shifted, x, y)
        })
        .collect();

    let mut pixels = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let rgba: [u8; 4] = if shape[y * width + x] {
                [0, 0, 0, 0xFF]
            } else {
                // The halo: any pixel within `halo` of the shape in
                // Chebyshev distance — the same dilation the X11 side
                // draws as offset copies of the polygon.
                let mut near = false;
                'scan: for dy in -halo..=halo {
                    for dx in -halo..=halo {
                        let (nx, ny) = (x as i32 + dx, y as i32 + dy);
                        if nx >= 0
                            && ny >= 0
                            && (nx as usize) < width
                            && (ny as usize) < height
                            && shape[ny as usize * width + nx as usize]
                        {
                            near = true;
                            break 'scan;
                        }
                    }
                }
                if near {
                    [0xFF, 0xFF, 0xFF, 0xFF]
                } else {
                    continue;
                }
            };
            let at = (y * width + x) * 4;
            pixels[at..at + 4].copy_from_slice(&rgba);
        }
    }
    (pixels, width as i32, height as i32, hotspot)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The rest of this module needs a wayland display, a GPU or a host
    // window; the layout arithmetic needs none of them, and it is the
    // part a single-connector test machine cannot demonstrate.

    fn monitor(x: i32, y: i32, w: u32, h: u32) -> MonitorInfo {
        MonitorInfo {
            geometry: Rect { pos: Point::new(x, y), size: Size::new(w, h) },
            name: format!("test-{x}x{y}"),
            primary: x == 0 && y == 0,
        }
    }

    #[test]
    fn one_monitor_is_its_own_screen() {
        assert_eq!(union_size(&[monitor(0, 0, 1920, 1080)]), Size::new(1920, 1080));
    }

    #[test]
    fn side_by_side_monitors_span_their_widths() {
        // The layout `session::init` builds: left to right, from the
        // origin, at each output's mode size.
        assert_eq!(
            union_size(&[monitor(0, 0, 1920, 1080), monitor(1920, 0, 1280, 1024)]),
            Size::new(3200, 1080)
        );
    }

    #[test]
    fn the_union_is_the_farthest_corner_not_the_last_monitor() {
        // The tallest screen sets the height even when it is not the
        // last one, and the box covers dead space rather than tracking
        // the outline of the monitors — which is what makes it a
        // bounding box and why the pointer needs its own confinement
        // (see `input::confine_to_outputs`).
        assert_eq!(
            union_size(&[monitor(0, 0, 1280, 1024), monitor(1280, 0, 1920, 1080)]),
            Size::new(3200, 1080)
        );
    }

    // The cursor pixels are pure arithmetic over a const string
    // table — no display, no GPU — which is the half of
    // `build_default_cursor` that got the size wrong.

    /// Alpha of the pixel at (x, y) in a `default_cursor_pixels` buffer.
    fn alpha_at(pixels: &[u8], width: i32, x: i32, y: i32) -> u8 {
        pixels[((y * width + x) * 4 + 3) as usize]
    }

    #[test]
    fn the_default_cursor_grows_with_the_ui_scale() {
        // The bug this pins: at scale 2.0 the arrow reached the screen
        // at its 1x size (10x19) because nothing multiplied it, leaving
        // a half-size pointer next to `scaled(2.0)` chrome.
        let (_, base_width, base_height) = default_cursor_pixels(1.0);
        assert_eq!((base_width, base_height), (10, 19));
        let (_, width, height) = default_cursor_pixels(2.0);
        assert_eq!((width, height), (base_width * 2, base_height * 2));
    }

    #[test]
    fn a_fractional_scale_rounds_to_whole_pixels() {
        let (pixels, width, height) = default_cursor_pixels(1.5);
        assert_eq!((width, height), (15, 29));
        assert_eq!(pixels.len(), (width * height * 4) as usize);
    }

    #[test]
    fn the_hotspot_pixel_stays_at_the_origin() {
        // `push_cursor_elements` draws this buffer at the pointer
        // position with no hotspot offset, so the tip must remain at
        // (0, 0) at every scale or the pointer would click low and to
        // the right of where it points.
        for scale in [1.0, 1.5, 2.0, 3.0] {
            let (pixels, width, _) = default_cursor_pixels(scale);
            assert_eq!(alpha_at(&pixels, width, 0, 0), 0xFF, "scale {scale}");
        }
    }

    #[test]
    fn scaling_replicates_pixels_rather_than_blending_them() {
        // Every destination pixel is one source pixel verbatim: a
        // filtered resample would put partially transparent grey
        // between the black shape and its white halo.
        let (pixels, width, height) = default_cursor_pixels(2.0);
        for y in 0..height {
            for x in 0..width {
                let at = ((y * width + x) * 4) as usize;
                let rgba = &pixels[at..at + 4];
                assert!(
                    rgba == [0, 0, 0, 0] || rgba == [0, 0, 0, 0xFF] || rgba == [0xFF; 4],
                    "({x}, {y}) is {rgba:?}"
                );
            }
        }
        // The source's transparent gap at (3, 12) ("o#o o##o") becomes
        // a 2x2 transparent block, which is the replication itself.
        assert_eq!(alpha_at(&pixels, width, 6, 24), 0);
        assert_eq!(alpha_at(&pixels, width, 7, 25), 0);
        assert_eq!(alpha_at(&pixels, width, 5, 24), 0xFF);
    }

    #[test]
    fn a_scale_below_one_is_clamped() {
        // A config typo must not shrink the pointer past the shape the
        // halo was hand-placed on — same floor `wm-x11`'s
        // `create_scaled_cursor` applies.
        let (_, width, height) = default_cursor_pixels(1.0);
        assert_eq!(default_cursor_pixels(0.25).1, width);
        assert_eq!(default_cursor_pixels(0.0).2, height);
    }

    // -- the resize cursors ------------------------------------------
    // Rasterized from the same polygon `wm-x11` hands the X server, so
    // what is pinned here is the software half: the rotations land the
    // shape the right way round, the hotspot stays its center, and the
    // buffer grows with the scale.

    /// Alpha of the pixel at (x, y) in a `resize_cursor_pixels` buffer.
    fn resize_alpha_at(pixels: &[u8], width: i32, x: i32, y: i32) -> u8 {
        pixels[((y * width + x) * 4 + 3) as usize]
    }

    #[test]
    fn the_vertical_arrow_is_taller_than_wide_and_the_horizontal_is_its_transpose() {
        let (_, vw, vh, _) = resize_cursor_pixels(1.0, 0.0);
        assert!(vh > vw, "a ⇕ cursor is taller than it is wide ({vw}x{vh})");
        let (_, hw, hh, _) = resize_cursor_pixels(1.0, 90.0_f32.to_radians());
        // The 90° turn swaps the extents (give or take a rounding
        // pixel, since the outline is rotated in floats and re-boxed).
        assert!((hw - vh).abs() <= 1 && (hh - vw).abs() <= 1, "{vw}x{vh} vs {hw}x{hh}");
    }

    /// The hotspot is the shaft's center — the pixel the user is told
    /// they are pointing at must be part of the drawing, or the cursor
    /// visibly floats beside the edge it marks.
    #[test]
    fn the_hotspot_is_inside_the_shape_at_every_rotation() {
        for angle in [0.0_f32, 45.0, 90.0, -45.0] {
            let (pixels, width, height, (hx, hy)) =
                resize_cursor_pixels(2.0, angle.to_radians());
            assert!(hx > 0 && hx < width && hy > 0 && hy < height);
            assert_eq!(
                resize_alpha_at(&pixels, width, hx, hy),
                0xFF,
                "hotspot ({hx}, {hy}) must land on the shaft at {angle}°"
            );
        }
    }

    #[test]
    fn the_resize_cursor_grows_with_the_ui_scale() {
        let (_, base_width, base_height, _) = resize_cursor_pixels(1.0, 0.0);
        let (_, width, height, _) = resize_cursor_pixels(2.0, 0.0);
        assert!(width > base_width && height > base_height);
    }

    /// The two diagonals run along opposite axes: -45° is the ⤡
    /// arrow, its shaft passing through the quadrants northwest and
    /// southeast of the hotspot, and +45° is the ⤢ through the other
    /// two. A sign slip in either rotation shows a corner the axis it
    /// does not resize along — the mistake `CursorSet::build`'s
    /// comment records nearly shipping — and this is the assertion
    /// that catches it. (Exact pixel-mirror equality between the two
    /// is deliberately not asserted: the outline is rotated in floats,
    /// so the buffers' boundary pixels round independently.)
    #[test]
    fn each_diagonal_runs_through_its_own_corners() {
        let (se_pixels, se_width, _, (sx, sy)) = resize_cursor_pixels(2.0, -45.0_f32.to_radians());
        let (sw_pixels, sw_width, _, (wx, wy)) = resize_cursor_pixels(2.0, 45.0_f32.to_radians());
        let d = se_width / 4;
        // ↘: shape on the NW–SE diagonal, nothing on the NE–SW one.
        assert_eq!(resize_alpha_at(&se_pixels, se_width, sx - d, sy - d), 0xFF);
        assert_eq!(resize_alpha_at(&se_pixels, se_width, sx + d, sy + d), 0xFF);
        assert_eq!(resize_alpha_at(&se_pixels, se_width, sx + d, sy - d), 0);
        assert_eq!(resize_alpha_at(&se_pixels, se_width, sx - d, sy + d), 0);
        // ↙: the mirror.
        assert_eq!(resize_alpha_at(&sw_pixels, sw_width, wx + d, wy - d), 0xFF);
        assert_eq!(resize_alpha_at(&sw_pixels, sw_width, wx - d, wy + d), 0xFF);
        assert_eq!(resize_alpha_at(&sw_pixels, sw_width, wx - d, wy - d), 0);
        assert_eq!(resize_alpha_at(&sw_pixels, sw_width, wx + d, wy + d), 0);
    }

    // The stacking transforms behind a client-decorated window's place
    // in the scene. They are pure `Vec<StackEntry>` arithmetic, which is
    // the only part of that path a unit test can reach: `WindowRecord`
    // holds a live `ToplevelSurface` or `X11Surface`, so the renderer's
    // and the hit-test's walks over real records need a client on a
    // socket, not a test.

    fn frame(id: u64) -> StackEntry {
        StackEntry::Frame(WlFrameId(id))
    }

    fn window(id: u64) -> StackEntry {
        StackEntry::Window(WlWindowId(id))
    }

    #[test]
    fn releasing_chrome_keeps_the_window_at_its_own_depth() {
        // Edge rewrites `_MOTIF_WM_HINTS` while sitting in the middle of
        // the stack. Losing its frame must not promote it over the two
        // windows above it.
        let mut stacking = vec![frame(1), frame(2), frame(3)];
        replace_stack_entry(&mut stacking, frame(2), window(20));
        assert_eq!(stacking, vec![frame(1), window(20), frame(3)]);
    }

    #[test]
    fn growing_chrome_keeps_the_window_at_its_own_depth() {
        // And the way back, which is the same requirement: a window that
        // asks to be decorated again has not asked to be raised.
        let mut stacking = vec![window(10), frame(2), window(30)];
        replace_stack_entry(&mut stacking, window(30), frame(3));
        assert_eq!(stacking, vec![window(10), frame(2), frame(3)]);
    }

    #[test]
    fn a_window_is_never_left_in_the_stack_twice() {
        // The double-draw this guards: both spellings present means the
        // renderer paints the client at two depths and the hit-test
        // resolves clicks at whichever it reaches first.
        let mut stacking = vec![frame(1), window(20)];
        replace_stack_entry(&mut stacking, window(20), frame(2));
        assert_eq!(stacking.iter().filter(|entry| **entry == window(20)).count(), 0);
        assert_eq!(stacking, vec![frame(1), frame(2)]);
    }

    #[test]
    fn a_first_map_lands_on_top() {
        // No slot to inherit: a window mapping for the first time goes
        // where `create_decoration` puts a fresh frame.
        let mut stacking = vec![frame(1)];
        replace_stack_entry(&mut stacking, window(20), frame(2));
        assert_eq!(stacking, vec![frame(1), frame(2)]);

        let mut stacking = vec![frame(1)];
        ensure_stack_entry(&mut stacking, window(20));
        assert_eq!(stacking, vec![frame(1), window(20)]);
    }

    #[test]
    fn remapping_a_frameless_window_is_not_a_raise() {
        // Coming back from another workspace, or from the icon well,
        // calls `map_frameless` again on a window that never lost its
        // slot — which must stay exactly where it was.
        let mut stacking = vec![window(10), frame(2), frame(3)];
        ensure_stack_entry(&mut stacking, window(10));
        assert_eq!(stacking, vec![window(10), frame(2), frame(3)]);
    }

    #[test]
    fn raising_a_frameless_window_puts_it_above_a_framed_one() {
        // The bug the `raise_frameless` verb was added for: every raise
        // site in `wm-core` named a `FrameId`, so a client-decorated
        // window stayed at the depth it mapped at forever — clicking it
        // focused it and left it behind whatever was in front. Invisible
        // to every application that lets us draw its titlebar, which is
        // why it needs pinning here.
        let mut stacking = vec![window(10), frame(2), frame(3)];
        assert!(raise_stack_entry(&mut stacking, window(10)));
        assert_eq!(stacking, vec![frame(2), frame(3), window(10)]);
    }

    #[test]
    fn a_framed_window_still_raises_over_a_frameless_one() {
        // And the converse, which is the actual requirement: the two
        // kinds restack against each other by one order, not two. A
        // frameless window that could only ever be raised *among* its
        // own kind would be a separate stacking path pretending to be
        // the same one.
        let mut stacking = vec![frame(1), window(20)];
        assert!(raise_stack_entry(&mut stacking, frame(1)));
        assert_eq!(stacking, vec![window(20), frame(1)]);
    }

    #[test]
    fn raising_something_with_no_slot_reports_nothing_to_redraw() {
        // The answer is the caller's damage test (see
        // `Backend::raise_frameless`), so a raise of a window that was
        // never mapped — or was destroyed a pass ago — must not mark the
        // scene dirty and wake a redraw for a frame that is identical.
        let mut stacking = vec![frame(1)];
        assert!(!raise_stack_entry(&mut stacking, window(20)));
        assert_eq!(stacking, vec![frame(1)]);
    }

    #[test]
    fn raising_the_top_entry_leaves_the_order_alone() {
        // Click-to-focus re-raises the already-front window constantly
        // (`focus_client`'s re-assert path does it on every click), and
        // the removal-then-push must be a no-op there rather than a
        // rotation.
        let mut stacking = vec![frame(1), frame(2), window(30)];
        assert!(raise_stack_entry(&mut stacking, window(30)));
        assert_eq!(stacking, vec![frame(1), frame(2), window(30)]);
    }

    #[test]
    fn shell_slots_are_never_disturbed() {
        // Shell surfaces share the one `stacking` vector with frames
        // (see `StackEntry`), and the bands are decided at paint time
        // from `above`, so a chrome change must leave their relative
        // order alone rather than shuffling the dock.
        let mut stacking =
            vec![StackEntry::Shell(WlShellId(9)), frame(2), StackEntry::Shell(WlShellId(8))];
        replace_stack_entry(&mut stacking, frame(2), window(20));
        assert_eq!(
            stacking,
            vec![StackEntry::Shell(WlShellId(9)), window(20), StackEntry::Shell(WlShellId(8))]
        );
    }

    // Per-output scale resolution: the mixed-DPI question ("which
    // factor does this window get?") answered without a display, which
    // is the only way a single-monitor test box can pin the two-monitor
    // arithmetic.

    #[test]
    fn a_window_takes_the_scale_of_the_monitor_holding_its_center() {
        let monitors = [monitor(0, 0, 1920, 1080), monitor(1920, 0, 2560, 1440)];
        let scales = [1.0, 1.5];
        // Entirely on the second monitor.
        let rect = Rect { pos: Point::new(2000, 100), size: Size::new(400, 300) };
        assert_eq!(scale_for_rect(&monitors, &scales, rect), 1.5);
        // Entirely on the first.
        let rect = Rect { pos: Point::new(100, 100), size: Size::new(400, 300) };
        assert_eq!(scale_for_rect(&monitors, &scales, rect), 1.0);
        // Straddling, with most of it (its center) on the right: one
        // factor, the right screen's.
        let rect = Rect { pos: Point::new(1800, 100), size: Size::new(400, 300) };
        assert_eq!(scale_for_rect(&monitors, &scales, rect), 1.5);
        // Straddling with the center on the left keeps the left's.
        let rect = Rect { pos: Point::new(1600, 100), size: Size::new(400, 300) };
        assert_eq!(scale_for_rect(&monitors, &scales, rect), 1.0);
    }

    #[test]
    fn dead_space_and_unplaced_windows_take_the_primary_scale() {
        let monitors = [monitor(0, 0, 1920, 1080), monitor(1920, 0, 1280, 720)];
        let scales = [2.0, 1.0];
        // Below the shorter second monitor: covered by no screen.
        let rect = Rect { pos: Point::new(2000, 900), size: Size::new(100, 100) };
        assert_eq!(scale_for_rect(&monitors, &scales, rect), 2.0);
        // A fresh window's empty rect at the origin resolves to the
        // primary too — the honest guess at first-commit time.
        assert_eq!(scale_for_rect(&monitors, &scales, Rect::default()), 2.0);
        // No monitors at all (never true in a running session) is 1.0,
        // not a panic.
        assert_eq!(scale_for_rect(&[], &[], Rect::default()), 1.0);
    }

    #[test]
    fn the_integral_fallback_advertises_the_ceiling() {
        // Round UP: the fallback client renders more pixels than the
        // output needs and is downscaled — the crisp direction. 1.25
        // used to round to 1 (blurry upscale); now both fractional
        // steps advertise 2.
        assert_eq!(advertised_output_scale(1.0).integer_scale(), 1);
        assert_eq!(advertised_output_scale(1.25).integer_scale(), 2);
        assert_eq!(advertised_output_scale(1.5).integer_scale(), 2);
        assert_eq!(advertised_output_scale(2.0).integer_scale(), 2);
        assert_eq!(advertised_output_scale(2.25).integer_scale(), 3);
        // Degenerate inputs stay sane (the NaN guard `f32::max`
        // documents).
        assert_eq!(advertised_output_scale(0.0).integer_scale(), 1);
        assert_eq!(advertised_output_scale(f32::NAN).integer_scale(), 1);
    }

    #[test]
    fn no_monitors_is_an_empty_screen() {
        // Not reachable in a running session (`run` always builds at
        // least one output); the arithmetic must not panic on it.
        assert_eq!(union_size(&[]), Size::new(0, 0));
    }
}
