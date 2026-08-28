use wm_theme_api::{Point, Rect, Size};

/// `WM_CLASS`: instance + class name pair (e.g. `("xterm", "XTerm")`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WmClass {
    pub instance: String,
    pub class: String,
}

/// The subset of ICCCM `WM_NORMAL_HINTS` a window manager actually acts
/// on. `None` means "no hint given" for that field, not "zero".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeHints {
    pub min_size: Option<Size>,
    pub max_size: Option<Size>,
    pub resize_increment: Option<Size>,
}

/// A `WM_PROTOCOLS` atom the WM cares about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WmProtocol {
    DeleteWindow,
    TakeFocus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
    pub struct Modifiers: u8 {
        const SHIFT   = 1 << 0;
        const CONTROL = 1 << 1;
        const ALT     = 1 << 2;
        const SUPER   = 1 << 3;
    }
}

/// A keyboard shortcut: a keysym plus modifier mask. Intentionally
/// simple (no chords) — the classic desktop binds single combos only.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub keysym: u32,
    pub modifiers: Modifiers,
}

/// Opaque token returned by `Backend::grab_pointer_for_drag`, passed
/// back to `ungrab_pointer` — core never inspects it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DragHandle(pub u64);

/// Which surface a pointer/button event happened on: the client's own
/// window (rare — most WM-relevant clicks land on the decoration frame)
/// or the frame itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceRef<Win, Frame> {
    Client(Win),
    Frame(Frame),
}

/// EWMH `_NET_WM_STATE` action field, verbatim from the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetStateAction {
    Remove,
    Add,
    Toggle,
}

/// The `_NET_WM_STATE` properties this WM acts on. Anything else in a
/// message is ignored (never rejected — EWMH wants unknown properties
/// skipped, not the whole message dropped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetState {
    Fullscreen,
    MaximizedHorz,
    MaximizedVert,
}

/// Coarse EWMH `_NET_WM_WINDOW_TYPE` classification — just enough to
/// decide decoration policy, deliberately not a 1:1 mirror of every
/// type atom.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WindowType {
    /// Decorate and manage normally (also the fallback for windows
    /// that declare no type, per the spec).
    #[default]
    Normal,
    /// Decorated and managed like Normal today; kept distinct so a
    /// future transient-for/placement policy has the information.
    Dialog,
    /// Docks, menus, tooltips, splashes, notifications: map as-is,
    /// no frame, no management — these draw their own chrome and
    /// position themselves.
    Unmanaged,
}

/// Events a `Backend` reports back to the core event loop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BackendEvent<Win, Frame> {
    MapRequest(Win),
    Unmapped(Win),
    Destroyed(Win),
    ConfigureRequest { window: Win, requested: Rect },
    /// The client's `WM_NAME` changed — most apps set an initial title
    /// well after their first `MapRequest` (a shell-hosting terminal
    /// commonly sets it once the shell's prompt is ready), so a WM that
    /// only reads the title once at map time shows a permanently blank
    /// titlebar for those. Backends that can't watch property changes
    /// (a hypothetical future one) simply never emit this — the titlebar
    /// then just keeps whatever title was known at map time.
    TitleChanged(Win),
    PointerButton {
        surface: SurfaceRef<Win, Frame>,
        local: Point,
        button: MouseButton,
        pressed: bool,
        /// Server timestamp in milliseconds (X11: the event's own `time`
        /// field — monotonic enough for delta math even though its epoch
        /// is arbitrary). Lets `wm-core` detect double-clicks itself
        /// (e.g. titlebar double-click to maximize) without a backend
        /// having to reimplement that protocol-agnostic hysteresis logic.
        time_ms: u32,
        /// Modifier keys held at the time of the event (Ctrl/Shift for
        /// vertical-only/horizontal-only maximize, per the classic
        /// NeXTSTEP-style bindings).
        mods: Modifiers,
    },
    /// Root-relative pointer position — needed for interactive move,
    /// where "local to the moving frame" would be meaningless since the
    /// frame itself is what's being repositioned.
    PointerMotion {
        root: Point,
        /// The surface the motion was reported against and the
        /// position local to it, when that surface is a managed frame
        /// or client — lets the WM hit-test which part of a window
        /// (titlebar, resize corner, ...) the pointer is currently over
        /// to update the cursor shape, independent of `root`'s use for
        /// active-drag tracking. `None` for motion the backend can't or
        /// doesn't need to attribute this way (e.g. over the desktop).
        surface_local: Option<(SurfaceRef<Win, Frame>, Point)>,
    },
    PointerEnter { surface: SurfaceRef<Win, Frame> },
    PointerLeave { surface: SurfaceRef<Win, Frame> },
    KeyPress(KeyCombo),
    /// An EWMH `_NET_ACTIVE_WINDOW` client message: a pager, launcher,
    /// or tool (xdotool, say) asked for this window to be activated —
    /// deminiaturized/unshaded if needed, focused, and raised.
    ActivateRequested(Win),
    /// An EWMH `_NET_CLOSE_WINDOW` client message — close exactly as if
    /// the titlebar close button had been pressed.
    CloseRequested(Win),
    /// An EWMH `_NET_WM_STATE` client message. The protocol carries an
    /// action plus up to two state properties in one message (a
    /// maximize request commonly toggles horizontal and vertical
    /// together).
    NetStateRequested { window: Win, action: NetStateAction, first: NetState, second: Option<NetState> },
    /// An EWMH `_NET_CURRENT_DESKTOP` client message: a pager or tool
    /// (xdotool set_desktop) asked to switch to this workspace.
    DesktopSwitchRequested(usize),
    /// An EWMH `_NET_WM_DESKTOP` client message: move this window to
    /// that workspace. The spec's 0xFFFFFFFF "all desktops" value is
    /// not delivered (this WM has no sticky windows yet); backends
    /// swallow it.
    WindowDesktopRequested { window: Win, desktop: usize },
    /// The backend's connection to the display server is gone for good.
    /// The event loop must exit: continuing to poll a dead connection
    /// just spins (two zombie WMs burning CPU after a display restart —
    /// confirmed live).
    ShutdownRequested,
    /// A key was released. Backends only need to deliver these while a
    /// modal keyboard grab is active (the Alt-Tab switcher listens for
    /// the Alt release that commits the selection); releases outside a
    /// grab may simply never be emitted.
    KeyRelease(KeyCombo),
}
