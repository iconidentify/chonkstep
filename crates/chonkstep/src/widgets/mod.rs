//! The dock's widget SDK: one small trait, [`DockWidget`], plus one
//! module per built-in widget. A widget owns whatever sampling and
//! animation state it needs and renders itself on demand — the same
//! contract for every widget, so `Desktop`'s dock layout and
//! drag-to-reorder logic never needs to know a widget's internals.
//! Adding a new one is implementing this trait in its own module and
//! pushing it into `Desktop::new`'s widget list; nothing else in the
//! dock changes.
//!
//! The rendering side of the SDK lives in `wm-theme`: widgets here are
//! the *data* half (sampling `/proc`, talking to `wpctl`, easing
//! animations) and stay free of drawing code, calling a pure
//! `wm_theme` renderer with plain values instead. That split is what
//! keeps renderers unit-testable pixel-for-pixel without a live
//! system, and it is the pattern third-party `chonk-ui` dockapps
//! should copy. Instrument-style widgets (screens with LED readouts)
//! build on `wm_theme::panel`, the theme-reactive glass-and-LED kit.

use std::time::Duration;

use wm_theme::Theme;
use wm_theme_api::{DecorationBuffer, Point};

mod clock;
mod net;
mod power;
mod sound;
mod sysload;
mod wifi;

pub use clock::ClockWidget;
pub use net::NetTrafficWidget;
pub use power::PowerWidget;
pub use sound::SoundWidget;
pub use sysload::SysLoadWidget;
pub use wifi::WifiWidget;

/// A single dock widget. `tick` is called roughly once per event-loop
/// iteration — cheap by design, since every widget is responsible for
/// throttling its own expensive work (sampling `/proc`, easing an
/// animation) internally rather than assuming any particular call rate.
/// Returns whether `render` would now produce different pixels, so the
/// dock only repaints when something actually changed.
pub trait DockWidget {
    fn tick(&mut self) -> bool;
    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer;

    /// How many `tile`-tall units this widget currently occupies in the
    /// dock's vertical stack. Most widgets are exactly one square tile;
    /// override when a widget's rendered size varies (e.g. by mode).
    fn tile_height(&self) -> u32 {
        1
    }

    /// Left-click handling — `local` is the click position within this
    /// widget's own tile (origin at its top-left) and `tile` the tile
    /// edge length, so a widget can carve its face into control zones
    /// (a volume widget's louder/softer halves) without knowing where
    /// the dock put it. Returns whether the widget's appearance changed
    /// (so the dock knows to repaint). Most widgets have no click
    /// behavior; the default no-op covers them.
    fn on_click(&mut self, local: Point, tile: u32) -> bool {
        let _ = (local, tile);
        false
    }
}

/// How often widgets that sample the system actually re-read it —
/// every `tick()` call still runs (for animation easing), but the real
/// sampling cost is paid at most this often.
pub const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

/// The workspace state the Clip tile and the `Desktop` share
/// through one `Rc<RefCell<...>>`: the WM's event loop pushes the
/// authoritative `(current, count)` in through
/// `Desktop::set_workspace_display`, and the Clip's click handler
/// pushes a switch request out through `requested` for the loop to
/// drain via `Desktop::take_workspace_request`. A shared cell instead
/// of widget methods because `Desktop` stores widgets as
/// `Box<dyn DockWidget>` — by design the dock can't reach a specific
/// widget's internals, so state that crosses that boundary travels
/// beside the trait object, not through it.
pub(crate) struct WorkspaceShared {
    pub current: usize,
    pub count: usize,
    /// A workspace index the user clicked their way toward, waiting for
    /// the WM to actually perform the switch — `Some` is a request, not
    /// a fact, which is why the click handler never repaints: the tile
    /// keeps showing the real current workspace until the WM confirms
    /// the switch by updating `current`/`count`.
    pub requested: Option<usize>,
}
