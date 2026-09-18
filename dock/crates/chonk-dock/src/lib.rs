//! Standalone dock. Nothing in this crate is linked by the compositor.
pub mod apps;
pub mod client;
pub mod control;
pub mod desktop;
pub mod launchdock;
pub mod spawn;
pub mod startup;
pub mod surface;
pub mod wayland;
pub mod x11;
mod appearance {
    pub fn load_published() -> Option<wm_theme::Appearance> {
        let text = std::fs::read_to_string(crate::startup::state_file("appearance")?).ok()?;
        wm_theme::Appearance::from_name(text.trim())
    }
}
mod dockapp;
pub mod runtime;
pub mod widgets;
