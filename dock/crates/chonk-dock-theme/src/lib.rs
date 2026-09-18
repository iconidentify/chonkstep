//! Dock-only renderers, kept outside the compositor dependency graph.
pub use chonk_theme::*;
pub mod bluetooth;
pub mod clock;
pub mod digitalclock;
pub mod instrument_panel;
pub mod launcher;
pub mod netgraph;
pub mod netload;
pub mod nettraffic;
pub mod paint;
pub mod panel;
pub mod power;
pub mod soundctl;
pub mod sysload;
pub mod wifi;
pub mod workspace;
