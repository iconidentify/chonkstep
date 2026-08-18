//! X11 backend for chonkstep — implements `wm_core::Backend` using
//! `x11rb`'s pure-Rust connection, plus a small set of desktop-shell
//! helpers (background painting, unmanaged shell windows for the dock
//! and root menu, root/dock click routing) that live outside the
//! `Backend` trait since `wm-core` has no notion of a desktop shell.

mod backend;

pub use backend::{X11Backend, X11BackendError, XFrame, XWindow};
