use slotmap::new_key_type;
use wm_theme_api::{DecorationLayout, Rect};

use crate::backend::Backend;

new_key_type! {
    /// Core's own handle for a managed client, independent of any
    /// backend-specific window id — the primary key for all internal
    /// storage (client table, focus history, workspace membership).
    pub struct ClientId;
}

new_key_type! {
    pub struct MonitorId;
}

/// A physical output as reported by the backend. Note this has no
/// `MonitorId` — that's a slotmap key only the core can mint (backends
/// can't construct one), so a future monitor registry inside
/// `WindowManager` would ingest these and assign real `MonitorId`s.
/// `Client::monitor` already carries a `MonitorId` so per-client
/// monitor ownership is additive later, not a rewrite.
///
/// Identity is positional, not by name: `WindowManager::set_workareas`
/// addresses monitors by their index in `Backend::monitors()`, which is
/// why that method promises a stable order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonitorInfo {
    pub geometry: Rect,
    pub name: String,
    /// The output the desktop shell hangs its chrome on and that
    /// anything with no better anchor falls back to. At most one entry
    /// in a list may set this — see `Backend::monitors` for the full
    /// contract, including what the core does when a platform names no
    /// primary at all.
    pub primary: bool,
}

/// Where a client sits in its lifecycle. Deliberately WindowMaker-shaped:
/// `Miniaturized` means unmapped-and-represented-by-an-icon, not
/// "minimized to a taskbar" — no taskbar concept exists here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lifecycle {
    /// Known but unmanaged/unmapped — initial state and post-close state.
    Withdrawn,
    /// Mapped, decorated, live on a workspace.
    Normal,
    /// Iconified: frame unmapped, represented by a small icon.
    Miniaturized,
}

bitflags::bitflags! {
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct ClientFlags: u8 {
        const FOCUSED    = 1 << 0;
        const SHADED     = 1 << 1;
        /// A modern addition — classic WindowMaker predates EWMH
        /// fullscreen. An orthogonal presentation mode, not a lifecycle
        /// stage, hence a flag rather than a `Lifecycle` variant.
        const FULLSCREEN = 1 << 2;
        const STICKY     = 1 << 3;
        const URGENT     = 1 << 4;
        /// Set independently — a window can be maximized horizontally,
        /// vertically, or both ("full" maximize), matching WindowMaker's
        /// `MAX_HORIZONTAL`/`MAX_VERTICAL`.
        const MAXIMIZED_H = 1 << 5;
        const MAXIMIZED_V = 1 << 6;
        /// Ignores the client's own `ConfigureRequest` resize attempts
        /// (position changes and WM-driven resizes — drag, maximize,
        /// `resize_client_content` — are unaffected). For a client whose
        /// own size negotiation can't be trusted: e.g. a client that
        /// keeps re-asserting a stale computed size shortly after the WM
        /// sets a real one, fighting to a standstill. Off by default —
        /// most apps resize themselves for perfectly good reasons and
        /// should keep being able to.
        const SIZE_LOCKED = 1 << 7;
    }
}

bitflags::bitflags! {
    /// Which axes to maximize along — passed to
    /// `WindowManager::maximize`/`toggle_maximize`. WindowMaker has no
    /// titlebar button for this (see `ButtonKind`'s doc comment); it's
    /// invoked via titlebar double-click, optionally with Ctrl (vertical
    /// only) or Shift (horizontal only) held.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct MaximizeDirections: u8 {
        const HORIZONTAL = 1 << 0;
        const VERTICAL   = 1 << 1;
        const FULL = Self::HORIZONTAL.bits() | Self::VERTICAL.bits();
    }
}

/// A managed window: the client's own top-level window plus (once
/// decorated) the backend's frame, current geometry, cached decoration
/// layout, and where it lives.
pub struct Client<B: Backend> {
    pub window: B::WindowId,
    /// `None` until the theme engine is integrated and a decoration
    /// frame exists for this client (milestone step 4).
    pub frame: Option<B::FrameId>,
    pub title: String,
    /// `WM_CLASS`'s class field, captured at manage time — the
    /// application identity the shell's launcher matching keys on.
    /// Empty when the client set none.
    pub class: String,
    /// Root-relative content geometry (standard X11 convention).
    pub geometry: Rect,
    pub layout: DecorationLayout,
    pub lifecycle: Lifecycle,
    pub flags: ClientFlags,
    /// The content geometry to restore on `unmaximize` — set the first
    /// time a client is maximized (in either axis) and cleared once it's
    /// fully unmaximized. `None` means "not currently maximized".
    pub restore_geometry: Option<Rect>,
    /// A plain 0-based index into `WindowManager`'s workspace row —
    /// real WindowMaker uses exactly this (`int workspace` on its own
    /// window struct), not a generational handle: workspaces are never
    /// destroyed out from under a live client the way a frame or a
    /// monitor could be, so the extra safety a slotmap key buys
    /// elsewhere in this crate isn't needed here.
    pub workspace: usize,
    pub monitor: MonitorId,
}

impl<B: Backend> Client<B> {
    pub fn new(window: B::WindowId, title: String) -> Self {
        Self {
            window,
            frame: None,
            title,
            class: String::new(),
            geometry: Rect::default(),
            layout: DecorationLayout::default(),
            lifecycle: Lifecycle::Normal,
            flags: ClientFlags::empty(),
            restore_geometry: None,
            // `WindowManager::handle_map_request` overwrites this with
            // the *current* workspace right after construction — this
            // default only matters for a `Client` that's never mapped.
            workspace: 0,
            // Still unset (null slotmap key): multi-monitor policy
            // resolves a window's monitor geometrically, from its frame
            // center against `Backend::monitors()` (see
            // `WindowManager::monitor_index_at`), so nothing yet needs
            // a client to *remember* which monitor it is on — and a
            // remembered one would be the thing that goes stale when an
            // output is unplugged out from under it.
            monitor: MonitorId::default(),
        }
    }
}
