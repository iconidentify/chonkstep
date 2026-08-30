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

/// One drained scroll gesture, counted in **whole wheel notches**.
///
/// The discrete-vs-continuous question is settled here, once, for
/// every backend, because the two we have push in opposite directions:
///
/// * X11 has no axis concept at all. The server reports a wheel as an
///   ordinary button press/release pair — 4/5 vertical, 6/7 horizontal —
///   one pair per detent. That is the *entire* signal: no distance, no
///   partial notch, no velocity. Asking an X11 backend for a continuous
///   delta forces it to invent a pixels-per-notch constant, i.e. to
///   fabricate precision the protocol never carried, which every
///   backend-blind caller would then have to divide back out.
/// * Wayland/libinput has the opposite problem: it reports continuous
///   amounts (a touchpad genuinely is continuous; a high-resolution
///   wheel reports 120ths of a detent). Continuous -> notches is a
///   well-defined accumulate-and-threshold, and `wm-wayland` owns that
///   accumulator so every caller gets the same answer. Notches ->
///   continuous has no defined answer at all.
///
/// So the trait speaks the unit whose conversion has a defined
/// direction. Both backends can state notches honestly; only one of
/// them could state pixels.
///
/// Rejected: carrying both a notch count and an optional continuous
/// delta. It has no consumer — every reader in the plan is a step
/// machine (volume +/-1, a page, a dock tile's next face), and the
/// out-of-process dockapp protocol's `Input` message already commits
/// to a single `delta: i32` on the wire — and an optional field that
/// one backend permanently leaves `None` makes callers branch on which
/// backend they are running under. Erasing exactly that is the only
/// reason this trait exists.
///
/// The fields are named for a DIRECTION, not an axis, deliberately. A
/// signed field called `vertical` is precisely the kind of thing two
/// backends implement with opposite signs while every reviewer nods
/// along, because the name never says which way is positive (and the
/// two platforms really do disagree: X11's button 4 is up, while
/// `wl_pointer.axis` defines its positive vertical value as *down*).
/// `up` and `right` cannot be implemented backwards quietly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScrollDelta {
    /// Notches scrolled away from the user: a wheel rolled forward, or
    /// two fingers moving up a touchpad. Negative is toward the user.
    ///
    /// Note this is the opposite sign to the `y` of the `Point` it
    /// arrives with — screen coordinates grow downward, while a scroll
    /// direction is named after the gesture. That mismatch is the
    /// reason the field is `up` rather than `y`.
    pub up: i32,
    /// Notches scrolled to the right: X11's button 7, a wheel tilted
    /// right, or two fingers moving right. Negative is left.
    pub right: i32,
}

impl ScrollDelta {
    /// Neither axis moved. Backends must never queue one of these —
    /// see `Backend::take_shell_scroll` — so a caller that drains an
    /// event may act on it unconditionally.
    pub fn is_zero(self) -> bool {
        self.up == 0 && self.right == 0
    }
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

/// Who drew this window's chrome.
///
/// Deliberately separate from [`WindowType`], because they answer
/// different questions and a single window answers both. `WindowType`
/// says *what kind of window this is* — a dialog, a dock, a tooltip.
/// This says *whether the client has already drawn a titlebar*, which
/// no window type implies: an ordinary `Normal` toplevel may or may
/// not have, and only the client knows.
///
/// Collapsing the two is what produces the two-titlebar bug. A client
/// that draws its own chrome and is framed anyway wears both, and the
/// window manager has no way to notice, because "Normal" was the only
/// thing it ever asked.
///
/// Every client kind answers in its own dialect and the backends
/// translate: Wayland toplevels through xdg-decoration, X11 and
/// XWayland clients through `_MOTIF_WM_HINTS`. A client that says
/// nothing at all is [`Self::ServerDrawn`] — the historic default, and
/// the one that keeps an ordinary X11 application framed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClientChrome {
    /// The window manager draws the frame. The default, and what every
    /// client that expresses no preference gets.
    #[default]
    ServerDrawn,
    /// The client drew its own titlebar and borders; this window must
    /// be managed (focused, moved, stacked, put on a workspace) but
    /// never framed.
    ClientDrawn,
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
    /// The client may have changed its mind about drawing its own
    /// chrome — re-read [`Backend::client_draws_own_chrome`] and add or
    /// drop the frame to match.
    ///
    /// A separate event rather than folding into `TitleChanged`,
    /// because the consequences are not comparable: a title change
    /// repaints a titlebar, this one creates or destroys a window and
    /// reparents a live client. Backends that cannot watch the hint
    /// never emit it, and every window then keeps whatever the answer
    /// was at map time.
    ///
    /// [`Backend::client_draws_own_chrome`]: crate::Backend::client_draws_own_chrome
    ChromeChanged(Win),
    /// The client asked the window manager to start moving it — X11's
    /// `_NET_WM_MOVERESIZE`, or a Wayland toplevel's `move` request.
    ///
    /// A window whose client draws its own chrome has no titlebar of
    /// *ours* to drag, so this is the only way it can ever be moved.
    /// Dropping it (as both backends used to) turns every
    /// client-decorated application into a rectangle pinned where it
    /// first mapped — which is a worse bug than the spare titlebar that
    /// removing our chrome was meant to fix.
    MoveRequest(Win),
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
