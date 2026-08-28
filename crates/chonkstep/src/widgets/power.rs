//! Power instrument: battery capacity, charge state, AC presence.
//! Currently the SDK's dead-screen placeholder face - the sampling and
//! the `wm_theme::power` renderer are being built on the
//! `wm_theme::panel` SDK.

use std::cell::RefCell;

use wm_theme::{panel, Theme};
use wm_theme_api::DecorationBuffer;

use super::DockWidget;

pub struct PowerWidget {
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl PowerWidget {
    pub fn new() -> Self {
        Self { font_system: RefCell::new(cosmic_text::FontSystem::new()), swash_cache: RefCell::new(cosmic_text::SwashCache::new()) }
    }
}

impl Default for PowerWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for PowerWidget {
    fn tick(&mut self) -> bool {
        false
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let mut font_system = self.font_system.borrow_mut();
        let mut swash_cache = self.swash_cache.borrow_mut();
        panel::render_dead_tile(theme, &mut font_system, &mut swash_cache, tile, "PWR")
    }
}
