//! The Wayland half of chonkstep's dual architecture: a Smithay-based
//! compositor implementing `wm_core::Backend` (and the shell-surface
//! family plus `wm_theme_api::PopupHost`), so the same `wm-core`
//! policy brain and the same `chonk-shell` desktop drive a native
//! Wayland session exactly as they drive X11.
//!
//! Linux-only by nature; on any other host this crate is deliberately
//! empty so the workspace builds everywhere.
#![cfg(target_os = "linux")]
