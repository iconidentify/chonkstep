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

#[cfg(test)]
mod fake_backend;

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
