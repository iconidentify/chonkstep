//! Geometry primitives and the contract between `wm-core` and `wm-theme`.
//!
//! Deliberately dependency-light: this lets `wm-core` depend on the
//! *shape* of decoration data without depending on the concrete
//! rendering stack (`wm-theme`, which pulls in tiny-skia and cosmic-text)
//! that produces it.

mod decoration;
mod geometry;
mod popup;

pub use decoration::{
    ButtonKind, ButtonRuntimeState, DecorationBuffer, DecorationLayout, DecorationPart,
    DecorationRequest, DecorationSurface, ResizeEdge, ThemeEngine,
};
pub use geometry::{
    clamp_client_size, client_size_limit, subsurface_link_exceeds_depth, Point, Rect, Size,
    MAX_CLIENT_WINDOW_DIMENSION, MAX_SUBSURFACE_DEPTH,
};
pub use popup::{PopupGrab, PopupHost};
