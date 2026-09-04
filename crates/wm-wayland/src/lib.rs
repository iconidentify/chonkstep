//! The Wayland half of chonkstep's dual architecture: a Smithay-based
//! compositor implementing `wm_core::Backend` (and the shell-surface
//! family plus `wm_theme_api::PopupHost`), so the same `wm-core`
//! policy brain and the same `chonk-shell` desktop drive a native
//! Wayland session exactly as they drive X11.
//!
//! The crate owns the *whole* compositor application, not just a
//! backend: on X11 the display server is someone else's process and
//! the WM is one client among many, but a Wayland compositor *is* the
//! display server, so everything — protocol globals, the GLES
//! renderer, input, XWayland, and the `WindowManager`/`Shell` pair —
//! has to live in one process behind one event loop. The binary
//! (`chonkstep-wayland`) therefore only calls [`run`].
//!
//! Everything hangs off one type, [`Compositor`], because Smithay's
//! delegate macros demand it: each Wayland protocol is wired up with a
//! `delegate_*!(Compositor)` invocation that implements the dispatch
//! traits *for that concrete type*, so the calloop data type, every
//! protocol handler, and the render loop must all agree on a single
//! struct. See `state.rs` for the full shape and the module split.
//!
//! Linux-only by nature; on any other host this crate is deliberately
//! empty so the workspace builds everywhere.
#![cfg(target_os = "linux")]

mod backend_impl;
mod capture;
mod core_protocols;
mod ctm;
mod data_control;
mod decoration;
mod dmabuf;
// hyprland-focus-grab-v1: click-outside-to-dismiss for shells that ask
// for it (Omarchy's Quickshell asks on every popup). The one module
// here that generates its own protocol bindings — see its module docs.
mod focus_grab;
mod hyprland_ipc;
// wlr-gamma-control-v1: the protocol wlsunset/gammastep/redshift warm
// the screen through — the only way this desktop can be tinted at all.
// Session backend only; see its module docs for the DRM mechanism and
// the honest nested answer.
mod gamma;
mod idle;
mod input;
mod layers;
mod lock;
mod output_mgmt;
mod protocols;
mod renderer;
mod session;
mod state;
// End-to-end test injection door — inert unless CHONKSTEP_TEST_SOCKET
// is set; see its module docs for the three regressions it exists for.
mod test_door;
mod toplevel_mapping;
// virtual-keyboard-v1: `wtype` and everything Omarchy builds on it.
mod virtual_keyboard;
mod xdg;
mod xewmh;
mod xwayland;

pub use state::{run, Compositor, WaylandBackend, WlFrameId, WlShellId, WlWindowId};
