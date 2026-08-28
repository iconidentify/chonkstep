//! The dock's clock: the classic one-tile analog face, nothing else.
//! It used to be one face of a two-face system-monitor widget (a click
//! toggled a taller btop-style dashboard with a seven-segment clock,
//! CPU, memory, and network readouts); the toggle was cut on request
//! to keep the clock a clock, and the dashboard's samplers and
//! renderers went with it (see git history to resurrect them).

use std::time::{SystemTime, UNIX_EPOCH};

use wm_theme::{clock, Theme};
use wm_theme_api::DecorationBuffer;

use super::DockWidget;

fn now_hms() -> (u32, u32, u32) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs_today = secs % 86_400;
    ((secs_today / 3600) as u32, ((secs_today % 3600) / 60) as u32, (secs_today % 60) as u32)
}

pub struct ClockWidget {
    clock: Option<(u32, u32, u32)>,
}

impl ClockWidget {
    pub fn new() -> Self {
        Self { clock: None }
    }
}

impl Default for ClockWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for ClockWidget {
    fn tick(&mut self) -> bool {
        let hms = now_hms();
        let changed = self.clock != Some(hms);
        self.clock = Some(hms);
        changed
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let (h, m, s) = self.clock.unwrap_or((0, 0, 0));
        clock::render_clock_tile(theme, tile, h, m, s)
    }
}
