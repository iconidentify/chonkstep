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
use smithay::output::{Mode, Output, PhysicalProperties, Subpixel};
use smithay::reexports::calloop::generic::Generic;
use smithay::reexports::calloop::{EventLoop, Interest, LoopHandle, Mode as TriggerMode, PostAction};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgToplevelState;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason, GlobalId};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Display, DisplayHandle};
use smithay::utils::{Logical, Physical, Transform, SERIAL_COUNTER};
use smithay::utils::{Point as SPoint, Size as SSize};
use smithay::wayland::compositor::{CompositorClientState, CompositorState};
use smithay::wayland::output::OutputManagerState;
use smithay::wayland::selection::data_device::DataDeviceState;
use smithay::wayland::shell::xdg::decoration::XdgDecorationState;
use smithay::wayland::shell::xdg::{ToplevelSurface, XdgShellState};
use smithay::wayland::shm::ShmState;
use smithay::wayland::socket::ListeningSocketSource;
use smithay::wayland::xwayland_shell::XWaylandShellState;
use smithay::xwayland::{X11Surface, X11Wm, XWayland, XWaylandEvent};

use wm_core::{
    Backend, BackendEvent, FocusPolicy, KeyCombo, MonitorInfo, MouseButton, WindowManager,
    WindowType,
};
use wm_theme::{RasterThemeEngine, Theme};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use chonk_shell::shell::{Shell, ShellOutcome};

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
    Shell(WlShellId),
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
    pub window_type: WindowType,
    /// Most recent preview of this window's contents, refreshed by
    /// [`crate::capture`] while rendering and served back through
    /// `Backend::capture_window_image`. `None` until the first
    /// snapshot (or forever, for a window that never maps), which the
    /// shell's icon and switcher renderers already handle.
    pub snapshot: Option<DecorationBuffer>,
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
}

/// The desktop background, as last painted by the shell through
/// `Backend::paint_root_color`/`paint_root_image`. On X11 this becomes
/// a root-window pixmap; here the renderer simply draws it as the
/// scene's bottom layer — same trait verbs, no pixmap machinery.
pub(crate) enum RootBackground {
    Color((u8, u8, u8)),
    Image(MemoryRenderBuffer),
}

/// The `Backend` the `WindowManager` owns: pure bookkeeping the
/// protocol handlers write into and the renderer reads out of. See the
/// module docs for why no display connection lives here.
pub struct WaylandBackend {
    /// Shared allocator for all three id spaces — window, frame, and
    /// shell ids never collide, which makes stray-id bugs loud in logs
    /// instead of silently aliasing. Starts at 1 so [`ROOT_SHELL`]
    /// (id 0) stays forever unallocated.
    next_id: u64,
    pub(crate) windows: HashMap<WlWindowId, WindowRecord>,
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
    /// Focus intent recorded by `Backend::set_input_focus`. The
    /// keyboard lives on the seat, the seat lives on [`Compositor`],
    /// and applying focus needs `&mut Compositor` — which a backend
    /// verb, running inside the `WindowManager`'s `&mut self`, can
    /// never have. So the verb records the intent here and
    /// [`Compositor::dispatch_pending`] applies it after the drain,
    /// the same each-loop cadence X11 focus changes effectively land
    /// on.
    pub(crate) pending_focus: Option<WlWindowId>,
}

impl WaylandBackend {
    pub(crate) fn new(display_handle: DisplayHandle, monitors: Vec<MonitorInfo>) -> Self {
        let output_size = union_size(&monitors);
        Self {
            next_id: 1,
            windows: HashMap::new(),
            frames: HashMap::new(),
            shells: HashMap::new(),
            stacking: Vec::new(),
            pending: VecDeque::new(),
            shell_clicks: VecDeque::new(),
            shell_motions: VecDeque::new(),
            pending_resize: None,
            grabbed_combos: Vec::new(),
            keyboard_grabbed: false,
            root_background: RootBackground::Color((0, 0, 0)),
            monitors,
            output_size,
            damage: true,
            display_handle,
            pending_focus: None,
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
}

/// Per-client data attached when a wayland client connects. Smithay's
/// compositor protocol machinery requires each client to carry its
/// `CompositorClientState`; nothing else is needed per-client yet.
#[derive(Default)]
pub struct ClientState {
    pub compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
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
        let damage_tracker = OutputDamageTracker::from_output(&setup.output);
        Self {
            output: setup.output,
            position: setup.position,
            size: setup.size,
            damage_tracker,
            _global,
        }
    }
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
fn union_size(monitors: &[MonitorInfo]) -> Size {
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

    // Per-protocol smithay state. Constructed once in `run`; the
    // handler impls in `xdg.rs`/`input.rs`/`xwayland.rs` return these
    // from their `*_state()` accessors.
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub xdg_decoration_state: XdgDecorationState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Compositor>,
    pub output_manager_state: OutputManagerState,
    pub data_device_state: DataDeviceState,
    pub xwayland_shell_state: XWaylandShellState,
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

    /// The graphics stack: a host window, or the hardware itself.
    pub(crate) graphics: Graphics,
    /// linux-dmabuf: the format set we advertise and the protocol
    /// state behind it. Always present; "this renderer cannot do
    /// dmabuf" is represented inside, not by an `Option`, so protocol
    /// dispatch in a login session never has an unreachable panic on
    /// a screen with no console to read it from.
    pub(crate) dmabuf: crate::dmabuf::DmabufSupport,
    /// The wlr protocol surface external tools bind: the
    /// foreign-toplevel window list and screencopy capture. The
    /// Wayland counterpart to the X11 session's EWMH properties.
    pub(crate) protocols: crate::protocols::ProtocolState,

    /// Latest pointer position in compositor space, maintained by
    /// `input.rs` — the renderer draws the cursor here, and hit-tests
    /// run against it.
    pub pointer_location: SPoint<f64, Logical>,
    /// What the cursor should look like, per the focused client's
    /// `wl_pointer.set_cursor` (maintained by `input.rs`'s
    /// `SeatHandler::cursor_image`). The renderer falls back to
    /// [`Compositor::default_cursor`] for `Named` shapes.
    pub cursor_status: CursorImageStatus,
    /// Built-in arrow drawn whenever no client cursor surface applies
    /// — a compositor draws its own cursor, there is no server to
    /// inherit one from.
    pub(crate) default_cursor: MemoryRenderBuffer,

    /// The active theme — kept for the pieces the loop owner still
    /// needs after `Shell::new` (mirrors the X11 binary keeping its
    /// copy for engine construction and per-app rules).
    pub theme: Theme,
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
        if !self.running {
            // Exit/restart was requested somewhere above — mirror the
            // X11 loop's break-before-tick.
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
        // Publish the window list and serve screencopy requests: after
        // the event and notification drains so external tools see the
        // same state the desktop just settled into, before the damage
        // test so a capture request can mark the frame it needs.
        crate::protocols::refresh(self);

        // Damage means the scene changed; `redraw_pending` means a
        // change already accounted for has not reached every screen yet
        // (a page flip was still in flight on one of them last pass).
        // The second condition only ever fires on the session backend
        // with more than one output — see `session::redraw_pending`.
        if self.wm.backend().damage || crate::session::redraw_pending(&self.graphics) {
            crate::renderer::render_frame(self);
        }

        // Protocol replies queued by everything above (configures,
        // frame callbacks, focus enter/leave) only reach clients on a
        // flush.
        let _ = self.display_handle.flush_clients();
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
        entry.output.change_current_state(Some(mode), None, None, None);
        entry.output.set_preferred(mode);
        entry.size = logical;
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

    /// Lands a deferred `set_input_focus` on the seat's keyboard. A
    /// window with no `wl_surface` yet (an XWayland window before its
    /// association) or one that died in the meantime clears focus
    /// rather than leaving it on the previous window — matching what
    /// the X11 server does when a focused window disappears.
    fn apply_pending_focus(&mut self) {
        let Some(id) = self.wm.backend_mut().pending_focus.take() else {
            return;
        };
        // Focus is two things to a client: the seat's keyboard focus,
        // which decides where keys go, and an "I am the active window"
        // flag it reads for its own styling - a title bar, a caret that
        // blinks, an unfocused-dim treatment. Keyboard focus alone
        // leaves every client permanently drawing itself as background
        // furniture, so both kinds of surface get the flag here, and
        // every window gets it (not just the newly focused one) so the
        // one losing focus repaints inactive.
        for (window_id, record) in self.wm.backend().windows.iter() {
            let active = *window_id == id;
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
        let surface = self
            .wm
            .backend()
            .windows
            .get(&id)
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
    let compositor_state = CompositorState::new::<Compositor>(&display_handle);
    let xdg_shell_state = XdgShellState::new::<Compositor>(&display_handle);
    // xdg-decoration is what lets us tell clients "the server draws
    // your chrome" — without it every GTK/Qt app draws its own
    // titlebar and the WindowMaker frames would double up.
    let xdg_decoration_state = XdgDecorationState::new::<Compositor>(&display_handle);
    let shm_state = ShmState::new::<Compositor>(&display_handle, vec![]);
    let output_manager_state = OutputManagerState::new_with_xdg_output::<Compositor>(&display_handle);
    let data_device_state = DataDeviceState::new::<Compositor>(&display_handle);
    let xwayland_shell_state = XWaylandShellState::new::<Compositor>(&display_handle);

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
            vec![OutputSetup { output, position: Point::new(0, 0), size }],
        )
    } else {
        tracing::info!("session backend: taking over the DRM device and input");
        let init = crate::session::init(&loop_handle, &display_handle)?;
        (init.graphics, init.outputs)
    };
    let outputs: Vec<OutputEntry> = output_setups
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
    // Same timing rule as dmabuf: bound before any client can connect.
    let protocols = crate::protocols::init(&display_handle);

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

    // Children the shell spawns find the session through the
    // environment, so it must be set before `Shell::new` (which may
    // autostart things) and before XWayland comes up. `DISPLAY`
    // follows once XWayland reports ready.
    std::env::set_var("WAYLAND_DISPLAY", &socket_name);
    tracing::info!(socket = ?socket_name, "wayland socket listening");

    // Scale and theme. Scale comes from the config alone (the
    // CHONKSTEP_SCALE env override is an X11-session concern — its
    // consumers are the nested-Xephyr dev scripts); theme precedence
    // is identical to the X11 binary: persisted theme-menu choice,
    // else the config's theme, else the flagship.
    // Session policy comes from `chonk_shell::startup`, shared with the
    // X11 binary: same env-over-config precedence, same theme
    // resolution, same cursor sizing. A compositor that resolved these
    // its own way is exactly how the two sessions would drift.
    let scale = chonk_shell::startup::read_scale_factor(config.scale);
    tracing::info!(scale, "UI scale (config `scale`; CHONKSTEP_SCALE overrides)");
    // XWayland clients draw their own Xcursor pointers and have no way
    // to learn this session's scale otherwise.
    chonk_shell::startup::ensure_xcursor_size(scale);
    let theme = chonk_shell::startup::resolve_theme(config.theme.as_deref()).scaled(scale);
    tracing::info!(theme = %theme.id, "theme loaded");
    let engine = RasterThemeEngine::new(theme.clone());

    // The desktop shell is built against the mutable backend before
    // `WindowManager::new` takes ownership — the exact construction
    // order the X11 binary uses, for the exact same borrow reason.
    let mut backend = WaylandBackend::new(display_handle.clone(), monitors);
    // The whole screen, as the shell sizes the desktop against it: the
    // union of every monitor. Where the dock and the workareas land
    // inside that union is the shell's decision, not this loop's.
    let output_size = backend.output_size;
    let shell = Shell::new(&mut backend, &config, theme.clone(), scale);
    // No `scan_existing_windows` here: a compositor's clients cannot
    // predate the compositor. (Hot-restart adoption is impossible for
    // the same reason — see `Compositor::restart`.)
    let mut wm = WindowManager::new(backend, Box::new(engine));
    wm.set_workarea(shell.workarea(output_size));
    wm.bind_default_keys();
    // Same contract as X11: every configured combo is registered on
    // top of the defaults. Here a "grab" is only a filter entry (the
    // compositor sees all keys regardless), but registering through
    // the same `grab_key` path keeps `wm-core`'s bookkeeping — and any
    // future per-combo policy — identical on both stacks.
    for (combo, _) in &config.keybindings {
        wm.grab_key(*combo);
    }
    if chonk_shell::startup::read_focus_follows_mouse(config.focus_follows_mouse) {
        tracing::info!("focus-follows-mouse enabled (config `focus_follows_mouse`; CHONKSTEP_FOCUS_FOLLOWS_MOUSE overrides)");
        wm.set_focus_policy(FocusPolicy::FocusFollowsMouse);
    }
    wm.set_placement_policy(config.placement);
    wm.set_snap_threshold(config.edge_resistance);

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
                        let backend = comp.wm.backend_mut();
                        let orphaned: Vec<WlWindowId> = backend
                            .windows
                            .iter()
                            .filter(|(_, record)| matches!(record.surface, ManagedSurface::X11(_)))
                            .map(|(id, _)| *id)
                            .collect();
                        for id in orphaned {
                            backend.windows.remove(&id);
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
        shm_state,
        seat_state,
        output_manager_state,
        data_device_state,
        xwayland_shell_state,
        popups: PopupManager::default(),
        seat,
        outputs,
        xwm: None,
        xdisplay: None,
        graphics,
        dmabuf,
        protocols,
        pointer_location: (0.0, 0.0).into(),
        cursor_status: CursorImageStatus::default_named(),
        default_cursor: build_default_cursor(),
        theme,
        start_time: Instant::now(),
        running: true,
        restart: false,
    };

    tracing::info!("entering compositor loop");
    while comp.running {
        // Hot-restart marker from `scripts/restart.sh`, polled once
        // per wakeup exactly as the X11 loop polls it.
        if restart_requested() {
            tracing::info!("restart requested — re-executing in place");
            comp.restart = true;
            break;
        }
        // Blocks on every source at once (wayland clients, winit
        // input, XWayland) with the housekeeping bound — the calloop
        // equivalent of the X11 loop's poll-on-the-socket-fd.
        event_loop.dispatch(Some(HOUSEKEEPING_INTERVAL), &mut comp)?;
        comp.dispatch_pending();
    }

    if comp.restart {
        // `nested` is the decision this process made at startup, so the
        // replacement makes the same one rather than re-deriving it
        // from an environment this compositor itself wrote.
        restart_in_place(nested);
    }
    tracing::info!("compositor session over");
    Ok(())
}



/// `true` at most once per `touch` of `scripts/restart.sh`'s marker
/// file — `remove_file` both checks and consumes in one step, same as
/// the X11 binary.
fn restart_requested() -> bool {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let path = std::path::PathBuf::from(home).join(".local/state/chonkstep/restart");
    std::fs::remove_file(path).is_ok()
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
fn build_default_cursor() -> MemoryRenderBuffer {
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
    let height = ARROW.len();
    let width = ARROW.iter().map(|row| row.len()).max().unwrap_or(1);
    let mut pixels = vec![0u8; width * height * 4];
    for (y, row) in ARROW.iter().enumerate() {
        for (x, cell) in row.bytes().enumerate() {
            let value: Option<[u8; 4]> = match cell {
                b'#' => Some([0, 0, 0, 0xFF]),
                b'o' => Some([0xFF, 0xFF, 0xFF, 0xFF]),
                _ => None,
            };
            if let Some(rgba) = value {
                let at = (y * width + x) * 4;
                pixels[at..at + 4].copy_from_slice(&rgba);
            }
        }
    }
    // RGBA byte order is the little-endian DRM fourcc Abgr8888, NOT
    // Argb8888 — mixing those up swaps red and blue. Fully opaque or
    // fully transparent pixels only, so these bytes are also already
    // valid premultiplied alpha, which is what the GLES renderer's
    // blending expects (and what tiny-skia's `data()` provides for the
    // decoration buffers `backend_impl` imports the same way).
    MemoryRenderBuffer::from_slice(
        &pixels,
        Fourcc::Abgr8888,
        (width as i32, height as i32),
        1,
        Transform::Normal,
        None,
    )
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

    #[test]
    fn no_monitors_is_an_empty_screen() {
        // Not reachable in a running session (`run` always builds at
        // least one output); the arithmetic must not panic on it.
        assert_eq!(union_size(&[]), Size::new(0, 0));
    }
}
