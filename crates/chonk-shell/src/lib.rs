//! The chonkstep desktop shell, backend-generic: the dock and its
//! instruments, the Clip, the root and window menus, the launcher
//! strip, icon tiles, wallpapers, themes, and application discovery —
//! everything a chonkstep desktop *is* above the window manager core,
//! written once against `wm_core::Backend`'s shell-surface family and
//! shared verbatim by every backend binary.
//!
//! This crate is the dual-architecture keystone: the X11 binary
//! (`chonkstep`) and the Wayland compositor binary
//! (`chonkstep-wayland`) both drive the one [`shell::Shell`] here, so
//! "the desktop behaves identically on both stacks" is a property of
//! the crate graph, not a porting promise. Nothing in this crate may
//! name X11, Wayland, or any concrete backend type — surfaces are
//! `Backend::ShellId`, windows are `Backend::WindowId`, drawing is
//! `DecorationBuffer`s, and anything a backend cannot express belongs
//! in that backend's binary, not here.

pub mod apps;
pub mod desktop;
pub mod launchdock;
pub mod shell;
pub mod spawn;
pub mod startup;
pub mod theme_select;
pub mod wallpaper;
pub mod widgets;
