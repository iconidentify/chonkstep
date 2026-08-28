//! Protocol-agnostic window-manager core: client state machine, focus
//! policy, and hit-testing.
//!
//! No backend (X11/Wayland) or rendering dependency lives here — see
//! `wm-x11` and `wm-theme`. That's what makes the state machine
//! unit-testable with an in-memory fake backend and zero X server (see
//! `fake_backend`, test-only).

mod backend;
mod client;
mod focus;
mod hittest;
mod manager;
mod placement;
mod resize;
mod snap;
mod types;

// The in-memory `Backend` double is compiled for this crate's own
// tests and for anyone who opts in through the `test-support` feature.
// `chonk-shell` is the reason it is not merely `#[cfg(test)]`: the
// shell is generic over `Backend` and had no way to exercise a real
// one, which is exactly how a missing update path (the launcher strip
// never learning that the monitor arrangement changed) reached review
// unnoticed. A test double that only its own crate can use leaves
// every consumer untestable.
#[cfg(any(test, feature = "test-support"))]
pub mod fake_backend;

pub use backend::Backend;
pub use client::{Client, ClientFlags, ClientId, Lifecycle, MaximizeDirections, MonitorId, MonitorInfo};
pub use focus::FocusPolicy;
pub use hittest::{hit_test, HitTarget};
pub use manager::{Notification, WindowManager};
pub use placement::{place_frame, PlacementPolicy};
pub use snap::snap_position;
pub use types::{
    BackendEvent, DragHandle, KeyCombo, Modifiers, MouseButton, NetState, NetStateAction,
    SizeHints, SurfaceRef, WindowType, WmClass, WmProtocol,
};
