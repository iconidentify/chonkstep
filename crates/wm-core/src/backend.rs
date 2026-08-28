use wm_theme_api::{DecorationBuffer, DecorationLayout, Rect, ResizeEdge, Size};

use crate::client::MonitorInfo;
use crate::types::{BackendEvent, DragHandle, KeyCombo, MouseButton, SizeHints, WmClass, WmProtocol};

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
    /// One entry per physical output. A backend without real
    /// multi-monitor support yet (e.g. `wm-x11` before RandR is wired
    /// up) reports a single entry spanning the whole screen.
    fn monitors(&self) -> Vec<MonitorInfo>;
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

    // -- decoration realization ---------------------------------------------
    fn create_decoration(&mut self, window: Self::WindowId, layout: &DecorationLayout) -> Self::FrameId;
    fn destroy_decoration(&mut self, frame: Self::FrameId);
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
    fn restack(&mut self, order_back_to_front: &[Self::FrameId]);

    // -- focus / close ------------------------------------------------------------
    fn set_input_focus(&mut self, window: Self::WindowId);
    /// `WM_DELETE_WINDOW` if the client supports it, force-kill otherwise.
    fn send_close(&mut self, window: Self::WindowId);

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
    /// Passive per-window button grab so the WM sees the first click on
    /// an unfocused window (click-to-focus) without stealing later
    /// clicks from the app. The one honest X11-ism on this trait — a
    /// no-op on backends (a future Wayland one) whose compositor already
    /// sees every click unconditionally.
    fn grab_button_passive(&mut self, window: Self::WindowId, button: MouseButton);
    fn ungrab_button_passive(&mut self, window: Self::WindowId, button: MouseButton);
    fn replay_pointer(&mut self);
}
