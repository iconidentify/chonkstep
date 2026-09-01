use wm_theme_api::{DecorationBuffer, DecorationLayout, Rect, ResizeEdge, Size, Point};

use crate::client::MonitorInfo;
use crate::types::{BackendEvent, DecorationRules, DragHandle, KeyCombo, Modifiers, MouseButton, ScrollDelta, SizeHints, WmClass, WmProtocol, WindowType};

/// Everything the protocol-agnostic core needs from a windowing backend
/// (X11 today via `wm-x11`, a future Wayland/Smithay backend later).
/// Deliberately scoped to what a *window manager* needs, not a
/// compositor: the core never learns about reparenting, XIDs, or atoms —
/// those are how an X11 backend satisfies these calls, not what the core
/// conceptually wants.
///
/// `WindowId`/`FrameId` are distinct associated types (real distinct
/// XIDs on X11 — a client's own window vs. its reparented decoration
/// frame) so the type system rules out passing the wrong handle to the
/// wrong call.
pub trait Backend {
    type WindowId: Copy + Eq + std::hash::Hash + std::fmt::Debug;
    type FrameId: Copy + Eq + std::hash::Hash + std::fmt::Debug;

    // -- lifecycle --------------------------------------------------------
    fn scan_existing_windows(&mut self) -> Vec<Self::WindowId>;
    /// One entry per physical output, in a **stable order**: an output
    /// that stays connected must keep the same index across calls.
    /// That ordering is load-bearing, not a nicety —
    /// `WindowManager::set_workareas` addresses monitors positionally,
    /// so a list that reshuffles would silently hand one monitor's
    /// reserved strip to another.
    ///
    /// Exactly one entry carries `primary: true` where the platform
    /// names a primary output (RandR's primary, a compositor's
    /// configured main output). Where the platform names none, every
    /// entry is `primary: false` and the core treats index 0 as
    /// primary — so a list is never left without one, and a backend
    /// never has to invent a primary the platform did not state.
    ///
    /// A backend without real multi-monitor support yet reports a
    /// single primary entry spanning the whole screen.
    fn monitors(&self) -> Vec<MonitorInfo>;

    /// The identity of a shell-owned surface — the dock, the Clip, the
    /// launcher strip, icon tiles, menu popups. The same id space
    /// `wm_theme_api::PopupHost` uses on this backend, so the desktop
    /// shell's cascade menus and its other surfaces interoperate.
    ///
    /// This surface family is what makes the shell backend-portable:
    /// `chonk-shell` draws everything through `DecorationBuffer`s
    /// painted onto these, so an X11 backend maps them to
    /// override-redirect windows while a Wayland compositor maps them
    /// to internal scene elements — the shell cannot tell the
    /// difference, by construction.
    type ShellId: Copy + Eq + std::hash::Hash + std::fmt::Debug;

    /// Creates an (unmapped) shell surface. `above`: keep it over
    /// managed clients (docks, menus); `false` sits it at desktop
    /// level. `None` when the backend cannot create surfaces (a fake
    /// that doesn't model them).
    fn create_shell_surface(&mut self, geometry: Rect, background: (u8, u8, u8), above: bool) -> Option<Self::ShellId>;
    fn map_shell_surface(&mut self, id: Self::ShellId);
    fn unmap_shell_surface(&mut self, id: Self::ShellId);
    fn destroy_shell_surface(&mut self, id: Self::ShellId);
    fn raise_shell_surface(&mut self, id: Self::ShellId);
    fn configure_shell_surface(&mut self, id: Self::ShellId, geometry: Rect);
    /// Blits `buffer` onto the surface — the shell's one drawing verb.
    fn paint_shell_surface(&mut self, id: Self::ShellId, buffer: &DecorationBuffer);

    /// Paints the desktop background — solid color or a wallpaper
    /// image. On X11 this is the root window (plus the root-pixmap
    /// publishing compositors read); a Wayland compositor draws it as
    /// the scene's bottom layer.
    fn paint_root_color(&mut self, rgb: (u8, u8, u8));
    fn paint_root_image(&mut self, buffer: &DecorationBuffer);

    /// Drains one queued click on a shell surface: `(surface, surface-
    /// local position, button, pressed)`. Backends queue these from
    /// their input machinery; the binary's event loop drains and feeds
    /// them to the shell.
    fn take_shell_click(&mut self) -> Option<(Self::ShellId, Point, MouseButton, bool)> {
        None
    }
    /// Drains one queued pointer motion over a shell surface.
    fn take_shell_motion(&mut self) -> Option<(Self::ShellId, Point)> {
        None
    }
    /// Drains one queued scroll over a shell surface: `(surface,
    /// surface-local position, delta)`. The position is the pointer's,
    /// in the same surface-local space `take_shell_click` reports, so
    /// a caller resolves a scroll to a widget with the identical code
    /// it already uses for clicks.
    ///
    /// Queued like clicks, not coalesced like motion, and the
    /// distinction is load-bearing: for motion only the newest
    /// position means anything, but every notch is its own command —
    /// three notches on a volume tile is three steps — so keeping only
    /// the last would silently swallow input the user gave.
    ///
    /// What a caller may rely on:
    /// * `delta` is never zero. A backend with nothing to report
    ///   queues nothing.
    /// * `delta` is a COUNT, not a flag. A backend may fold notches
    ///   that arrived together into one entry with `up`/`right` beyond
    ///   +/-1 (a high-resolution wheel spun hard does this), so read it
    ///   as a number of steps; it must never drop them.
    /// * Both counts are in the direction the *user* perceives. Any
    ///   natural-scroll or direction configuration the platform holds
    ///   is already applied by the time it reaches here.
    /// * Nothing here is a wheel *button*: `take_shell_click` never
    ///   reports one, so the two drains cannot double-count the same
    ///   physical input. (`MouseButton` deliberately stays
    ///   `{Left, Middle, Right}` — a `MouseButton::WheelUp` would have
    ///   made "pressed: false" meaningless and forced every existing
    ///   click site to learn about a button that cannot be held.)
    fn take_shell_scroll(&mut self) -> Option<(Self::ShellId, Point, ScrollDelta)> {
        None
    }
    /// Drains a pending screen-size change (RandR on X11, output
    /// reconfiguration on Wayland).
    fn take_screen_resize(&mut self) -> Option<Size> {
        None
    }
    /// The UI scale changed: rebuild whatever this backend sized from
    /// the old one.
    ///
    /// Defaulted to a no-op because most backends size nothing from the
    /// UI scale — everything they draw arrives as an already-rasterized
    /// buffer. The exception is server-side resources a backend builds
    /// for itself: the X11 backend rasterizes its own pointer cursors
    /// at the scale it was connected with, and those are the only
    /// pixels in the session the theme engine does not produce.
    ///
    /// `wm-core` calls this and nothing else on a scale change; every
    /// other consequence reaches the backend as ordinary paint traffic.
    fn set_ui_scale(&mut self, scale: f32) {
        let _ = scale;
    }
    /// Where the pointer is right now, in root coordinates, asked of
    /// the display server rather than remembered.
    ///
    /// `WindowManager` tracks the last pointer position it was *told
    /// about*, which is not the same thing: motion over a client's own
    /// window is delivered to that client, not to the window manager.
    /// For a window whose client draws its own chrome that is most of
    /// the window, so by the time such a client asks to be dragged
    /// (`BackendEvent::MoveRequest`) the remembered position can be
    /// stale by the whole width of the screen — and the drag would
    /// begin by teleporting the window by exactly that error.
    ///
    /// Defaulted to `None`, which falls back to the remembered
    /// position; a backend that does deliver motion over client
    /// surfaces loses nothing by not implementing this.
    fn pointer_position(&self) -> Option<Point> {
        None
    }
    /// The current screen/output size.
    fn screen_size(&self) -> Size;
    /// Non-blocking. The event-loop driver calls this in a loop on fd
    /// readiness until it returns `None`.
    fn poll_event(&mut self) -> Option<BackendEvent<Self::WindowId, Self::FrameId>>;

    // -- properties (ICCCM reads) ------------------------------------------
    fn window_title(&self, window: Self::WindowId) -> Option<String>;
    fn window_class(&self, window: Self::WindowId) -> Option<WmClass>;
    /// `_NET_WM_PID` — lets the shell correlate a freshly mapped window
    /// with the specific process it just spawned (e.g. to apply a
    /// default size only to *that* window, not any other window of the
    /// same class that happens to map around the same time). `None` if
    /// the client never set it — not every app does, so callers need a
    /// fallback that doesn't depend on it.
    fn window_pid(&self, window: Self::WindowId) -> Option<u32>;
    fn size_hints(&self, window: Self::WindowId) -> SizeHints;
    fn supports_protocol(&self, window: Self::WindowId, protocol: WmProtocol) -> bool;
    /// The window's own (root-relative) geometry at the time of the
    /// call — queried once at map time to know how big a fresh client
    /// wants to be before any decoration exists.
    fn window_geometry(&self, window: Self::WindowId) -> Rect;
    /// A snapshot of the window's currently-rendered pixels, at its own
    /// size — `None` on any failure (the window vanished, an X error,
    /// a backend that can't support this). Must be called while the
    /// window is still mapped and viewable: most backends have no way
    /// to read pixel content back from a window that isn't (X11 doesn't
    /// retain backing content for unmapped windows without a compositor,
    /// which this WM doesn't have). Used for the icon preview a
    /// miniaturized window shows — captured once, right before the
    /// window unmaps, not continuously refreshed.
    fn capture_window_image(&self, window: Self::WindowId, size: Size) -> Option<DecorationBuffer>;

    /// Asks the backend to serve `capture_window_image` at (up to)
    /// `edge` pixels on a window's longest side, `None` to return to
    /// its own default. The Overview sets this for the life of a
    /// session: its cards are card-sized, not icon-sized, and a
    /// backend that keeps throttled snapshots (the compositor) needs
    /// to know before it takes them. A hint, not a contract — the X11
    /// backend captures live at full size and ignores it, which is
    /// the default here.
    fn set_preview_edge(&mut self, edge: Option<u32>) {
        let _ = edge;
    }

    /// A counter that advances when previews taken at the hinted edge
    /// become available — how a caller that consumed
    /// `capture_window_image` before the backend could honor
    /// `set_preview_edge` learns it is worth asking again. Backends
    /// that answer captures synchronously never advance it (the answer
    /// was never stale), which is the default.
    fn preview_generation(&self) -> u64 {
        0
    }

    // -- decoration realization ---------------------------------------------
    fn create_decoration(&mut self, window: Self::WindowId, layout: &DecorationLayout) -> Self::FrameId;
    fn destroy_decoration(&mut self, frame: Self::FrameId);
    /// Take `window` back out of `frame` and destroy the frame, leaving
    /// the client itself alive, mapped, and parented as an unframed
    /// window — what a client asking to draw its own chrome *after* it
    /// was already framed requires.
    ///
    /// Distinct from [`Self::destroy_decoration`], which is the
    /// teardown path for a window that is going away and may destroy
    /// the client along with the frame. On X11 that difference is not
    /// cosmetic: destroying a parent destroys its children, so a frame
    /// dropped without reparenting the client to the root first takes
    /// the application's window with it.
    ///
    /// Defaulted to plain destruction for backends where a frame owns
    /// no client window (the Wayland compositor draws frames into its
    /// own scene; there is nothing to reparent).
    fn release_decoration(&mut self, window: Self::WindowId, frame: Self::FrameId) {
        let _ = window;
        self.destroy_decoration(frame);
    }
    fn paint_decoration(&mut self, frame: Self::FrameId, buffer: &DecorationBuffer);
    /// Sets the frame's cursor to indicate a resize is available along
    /// `edge` — `None` for the plain default cursor. Called on hover
    /// (see `WindowManager`'s handling of `BackendEvent::PointerMotion`'s
    /// `surface_local`), not just once at creation, since which edge (if
    /// any) the pointer is over changes as it moves within the frame.
    fn set_frame_cursor(&mut self, frame: Self::FrameId, edge: Option<ResizeEdge>);

    // -- geometry / visibility ------------------------------------------------
    fn set_frame_geometry(&mut self, frame: Self::FrameId, geometry: Rect);
    /// Resizes an already-decorated client's own window in place (its
    /// position within the frame, at `DecorationLayout::client_offset`,
    /// doesn't change from a content resize alone).
    fn resize_client(&mut self, window: Self::WindowId, size: Size);
    /// Honors a `ConfigureRequest` from a window the WM doesn't manage
    /// yet (no frame exists) — ICCCM requires acknowledging these even
    /// before the first `MapRequest`.
    fn configure_unmanaged(&mut self, window: Self::WindowId, geometry: Rect);
    fn map_frame(&mut self, frame: Self::FrameId);
    fn unmap_frame(&mut self, frame: Self::FrameId);
    /// Show a *managed* window that has no frame — one whose client
    /// draws its own chrome (see [`ClientChrome`]). The frame verbs
    /// above cannot serve: there is no frame to map.
    ///
    /// Deliberately not [`Self::map_unmanaged`], which means something
    /// else. That verb is for windows the core does not track at all
    /// (docks, menus, tooltips), and at least one backend records the
    /// window as unmanaged when it is called — which is right for a
    /// tooltip and wrong for a browser, since the two then differ in
    /// how they are rendered, hit-tested and focused. A window can be
    /// frameless and fully managed at the same time, and this is the
    /// verb for that.
    ///
    /// [`ClientChrome`]: crate::ClientChrome
    fn map_frameless(&mut self, window: Self::WindowId) {
        self.map_unmanaged(window);
    }
    /// Hide a managed frameless window again — the workspace-switch and
    /// miniaturize path for one, where a framed window would have its
    /// frame unmapped.
    ///
    /// Defaulted to nothing, which is a visible degradation rather than
    /// a silent one: a backend that has not implemented it leaves such
    /// windows on screen across a workspace switch. That is preferable
    /// to guessing at a mechanism the backend may not have, and it is
    /// obvious the first time anyone tries it.
    fn unmap_frameless(&mut self, window: Self::WindowId) {
        let _ = window;
    }
    /// Shows/hides the client's own content window in place, independent
    /// of the frame — used for "shading" (rolling a window up to just
    /// its titlebar): the frame stays mapped and visible at its
    /// (shrunk) shaded height, only the content underneath disappears.
    /// A backend must not report this as the client unmapping/
    /// withdrawing (see `wm-x11`'s implementation, which suppresses the
    /// resulting `UnmapNotify`) — from `wm-core`'s perspective the
    /// client is still fully managed the whole time.
    fn set_client_mapped(&mut self, window: Self::WindowId, mapped: bool);

    // -- stacking ---------------------------------------------------------------
    fn raise(&mut self, frame: Self::FrameId);
    /// Raise a managed window that has no frame to raise.
    ///
    /// Without this a client-decorated window is stuck at whatever
    /// depth it mapped at, for the rest of its life: every raise in
    /// this crate names a `FrameId`, and such a window has none. The
    /// symptom is precise and maddening — clicking the window focuses
    /// it and does not bring it forward — and it would have shipped
    /// invisibly, because the only applications affected are the ones
    /// that draw their own titlebars.
    fn raise_frameless(&mut self, window: Self::WindowId) {
        let _ = window;
    }
    fn restack(&mut self, order_back_to_front: &[Self::FrameId]);

    // -- focus / close ------------------------------------------------------------
    fn set_input_focus(&mut self, window: Self::WindowId);
    /// `WM_DELETE_WINDOW` if the client supports it, force-kill otherwise.
    fn send_close(&mut self, window: Self::WindowId);

    /// Force-kills the client owning `window` (X11: `XKillClient`) —
    /// the escalation for an application that no longer answers
    /// `send_close`'s polite `WM_DELETE_WINDOW`. Defaulted no-op so
    /// backends without a kill concept still compile.
    fn kill_client(&mut self, window: Self::WindowId) {
        let _ = window;
    }

    // -- input grabs --------------------------------------------------------------
    fn grab_pointer_for_drag(&mut self) -> DragHandle;
    fn ungrab_pointer(&mut self, handle: DragHandle);
    fn grab_key(&mut self, combo: KeyCombo);
    fn ungrab_key(&mut self, combo: KeyCombo);
    /// Exclusive keyboard grab for a modal interaction (the Alt-Tab
    /// switcher): every key press and release reaches the WM until
    /// `ungrab_keyboard`, letting it see the plain Tab repeats and the
    /// Alt release no passive grab covers. Defaulted to a no-op so
    /// backends (and test fakes) without modal input needs compile
    /// unchanged.
    fn grab_keyboard(&mut self) {}
    fn ungrab_keyboard(&mut self) {}
    /// Asks a client to repaint its entire content (a synthetic
    /// full-window Expose on X11) — used after remapping a window whose
    /// pixels the server did not retain (unshade, deminiaturize), where
    /// some clients otherwise leave stale buffer garbage visible until
    /// their next incidental redraw. Defaulted to a no-op.
    fn refresh_client(&mut self, _window: Self::WindowId, _size: Size) {}

    // -- EWMH ---------------------------------------------------------------
    // All defaulted to no-ops: a backend without a concept of EWMH (the
    // test fake) compiles unchanged, and `wm-core` calls these
    // unconditionally at the state-change sites without caring.

    /// Publish the thickness of the chrome drawn around `window`, in
    /// EWMH's `_NET_FRAME_EXTENTS` order: left, right, top, bottom.
    ///
    /// This is how a client learns how much bigger than its content the
    /// thing on screen actually is — which it needs to place its own
    /// dialogs and menus correctly, to save and restore its geometry
    /// across sessions, and to reason about the screen edges. A window
    /// manager that never publishes it leaves every client guessing,
    /// and the guesses are wrong by exactly a titlebar.
    ///
    /// All four are zero for a window with no frame — one whose client
    /// draws its own chrome, or one that is fullscreen. That is not a
    /// degenerate case to skip: publishing the zeros is what tells a
    /// client its frame has *gone*.
    fn publish_frame_extents(&mut self, window: Self::WindowId, left: u32, right: u32, top: u32, bottom: u32) {
        let _ = (window, left, right, top, bottom);
    }

    /// The window's advertised `_NET_WM_WINDOW_TYPE`, read at manage
    /// time to pick a decoration policy.
    fn window_type(&self, _window: Self::WindowId) -> WindowType {
        WindowType::Normal
    }
    /// Maps a window this WM has decided not to manage (see
    /// `WindowType::Unmanaged`) exactly as the client created it.
    fn map_unmanaged(&mut self, _window: Self::WindowId) {}
    /// The window this one is a transient child of — its dialog parent:
    /// `xdg_toplevel.set_parent` on Wayland, `WM_TRANSIENT_FOR` on X11.
    ///
    /// Asked fresh rather than cached at map time, because a client is
    /// free to state it late and several do: LibreOffice maps its
    /// "Welcome" dialog and only then parents it to the document window
    /// it belongs to.
    ///
    /// Defaulted to `None` for a backend that cannot report it; such a
    /// backend simply gets flat stacking, which is what every backend
    /// had before this existed.
    fn window_parent(&self, _window: Self::WindowId) -> Option<Self::WindowId> {
        None
    }
    /// Installs (or removes) the passive pointer grabs the move/resize
    /// modifier-drag needs on one managed client's own window.
    ///
    /// Only a backend that does not already see every input event needs
    /// this. The compositor sees them all and does nothing here; an X11
    /// window manager sees a click on a client's own window only if it
    /// has grabbed it, and the whole point of this gesture is to work
    /// over a client's content — including a focused client's, which is
    /// not passively grabbed for focus.
    ///
    /// `None` removes the grabs. Called when a window is managed and
    /// whenever the configured modifier changes.
    fn set_drag_gesture_grab(&mut self, _window: Self::WindowId, _modifier: Option<Modifiers>) {}
    /// Hands the backend the user's per-application decoration
    /// overrides, so [`Self::client_draws_own_chrome`] can consult them.
    ///
    /// On the backend rather than in `wm-core` because the rules have
    /// to reach further than the framing decision: both decoration
    /// protocols answer clients on the wire, and a rule that changed
    /// the frame without changing the answer would tell a client
    /// "decorate yourself" and then decorate it anyway.
    ///
    /// Defaulted to a no-op for a backend with no decoration protocol
    /// to override.
    fn set_decoration_rules(&mut self, _rules: DecorationRules) {}
    /// Whether the client has already drawn its own window chrome, and
    /// so must not be framed. See [`ClientChrome`] for why this is not
    /// derivable from [`Self::window_type`].
    ///
    /// Read at map time and again whenever the backend reports the
    /// answer may have changed (`BackendEvent::ChromeChanged`) — X11
    /// clients are entitled to change their mind by rewriting
    /// `_MOTIF_WM_HINTS` on a mapped window, and several real ones do.
    ///
    /// Defaulted to `false`: a backend that cannot ask keeps the
    /// historic behaviour of framing everything it manages.
    fn client_draws_own_chrome(&self, window: Self::WindowId) -> bool {
        let _ = window;
        false
    }
    /// Moves the client window within its frame. Reparenting fixes the
    /// client at the theme's chrome offset and normal reflows never
    /// change it, so this only matters when the offset itself changes:
    /// entering fullscreen (content at 0,0, no chrome) and leaving it
    /// (back to the theme's offset).
    fn position_client(&mut self, _window: Self::WindowId, _pos: Point) {}
    /// Publishes `_NET_CLIENT_LIST` (managed clients, oldest first).
    fn publish_client_list(&mut self, _clients: &[Self::WindowId]) {}
    /// Publishes `_NET_ACTIVE_WINDOW` (`None` = no window focused).
    fn publish_active_window(&mut self, _window: Option<Self::WindowId>) {}
    /// Publishes `_NET_NUMBER_OF_DESKTOPS` and `_NET_CURRENT_DESKTOP`.
    fn publish_workspaces(&mut self, _count: usize, _current: usize) {}
    /// Publishes `_NET_WORKAREA` — the same rectangle for every
    /// desktop, since the dock reserves the same strip on all of them.
    /// `area` is the *union* of the per-monitor workareas, not any one
    /// monitor's: the property's format is one rect per desktop with no
    /// per-monitor dimension at all (EWMH predates multi-head), so the
    /// bounding box is the only rect a multi-monitor session can state
    /// without lying about some output. Clients that need real
    /// per-monitor reserved space read the panels' own
    /// `_NET_WM_STRUT_PARTIAL`. See `WindowManager::set_workareas`.
    fn publish_workarea(&mut self, _area: Rect, _workspace_count: usize) {}
    /// Publishes a client's `_NET_WM_STATE` property from the WM's own
    /// authoritative flags.
    fn publish_net_state(&mut self, _window: Self::WindowId, _fullscreen: bool, _max_h: bool, _max_v: bool, _shaded: bool, _hidden: bool) {}
    /// Publishes a client's `_NET_WM_DESKTOP` property — which
    /// workspace the window lives on, for pagers and taskbars.
    fn publish_window_desktop(&mut self, _window: Self::WindowId, _desktop: usize) {}
    /// Passive per-window button grab so the WM sees the first click on
    /// an unfocused window (click-to-focus) without stealing later
    /// clicks from the app. The one honest X11-ism on this trait — a
    /// no-op on backends (a future Wayland one) whose compositor already
    /// sees every click unconditionally.
    fn grab_button_passive(&mut self, window: Self::WindowId, button: MouseButton);
    fn ungrab_button_passive(&mut self, window: Self::WindowId, button: MouseButton);
    fn replay_pointer(&mut self);
}
