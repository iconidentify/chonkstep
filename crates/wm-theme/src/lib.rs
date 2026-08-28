//! WindowMaker-style theme data model and rendering engine.
//!
//! Pure Rust with no X11/Wayland dependency — this crate only ever
//! produces `wm_theme_api::DecorationBuffer` (raw RGBA8 pixels) and
//! `DecorationLayout` (hit-test geometry), so the exact same crate is
//! reusable by any future backend. Rendering uses `tiny-skia` (fills,
//! gradients, the chiseled bevel border) and `cosmic-text` (title/menu
//! text, with font fallback).
//!
//! - [`RasterThemeEngine`] implements `wm_theme_api::ThemeEngine` —
//!   window decorations (titlebar, buttons, border, resize bar).
//! - [`menu::render_menu`] renders WindowMaker-style popup menus (the
//!   root menu, app menus): content-sized, per-entry relief strips
//!   under a titlebar-styled title, ported from the wmaker recipes.
//! - [`cascade::CascadeMenu`] is the reusable *behavior* on top of
//!   `menu::render_menu`: the popup-window stack, hover-to-open-submenu
//!   hysteresis, and cascade positioning for a nested `MenuItem` tree,
//!   generic over `wm_theme_api::PopupHost` so any host can reuse it.
//! - [`clock::render_clock_tile`] renders the dock's analog clock tile.
//! - [`netgraph::render_network_tile`] renders a mirrored up/down
//!   network-throughput history tile.
//! - [`digitalclock::render_digital_clock`] renders a vector-drawn
//!   seven-segment `HH:MM` readout.
//! - [`netload::render_netload_tile`] is a close port of the classic
//!   `wmnetload` WindowMaker dockapp: a monochrome LCD panel with a
//!   seven-segment throughput readout and a mirrored dot-matrix graph.
//! - [`tile`] is the common tile platform (face, relief, ink, sunken
//!   wells) every dock item and icon builds on - the core of the UI kit.
//! - [`workspace::render_clip_tile`] renders the top-left Clip tile
//!   (WindowMaker's workspace switcher, ported from its dock.c recipes).
//! - [`icon::render_icon_tile`] renders a themed square icon tile (what
//!   a miniaturized window collapses to; also useful for any app that
//!   wants a themed launcher/shelf icon).
//! - [`paint`] exposes the low-level `tiny-skia` drawing primitives
//!   (flat/gradient fills, the chisel bevel, themed text) that back all
//!   of the above — the reusable building blocks a third-party GUI app
//!   (or the desktop shell's own dock/menu compositing) can draw with to
//!   inherit the same NeXTSTEP look and feel, rather than re-deriving it.
//!
//! Not yet implemented: the native RON theme-file loader (still just
//! the hardcoded flagship theme in `default_theme`) and legacy
//! WindowMaker `.style`/`.themed` import (deferred past milestone 1).

pub mod cascade;
pub mod clock;
pub mod default_theme;
pub mod digitalclock;
pub mod icon;
pub mod launcher;
pub mod menu;
pub mod model;
pub mod netgraph;
pub mod netload;
pub mod nettraffic;
pub mod paint;
pub mod panel;
pub mod power;
mod raster;
pub mod soundctl;
pub mod switcher;
pub mod sysload;
pub mod tile;
pub mod wifi;
pub mod workspace;

pub use model::Theme;
pub use raster::RasterThemeEngine;
