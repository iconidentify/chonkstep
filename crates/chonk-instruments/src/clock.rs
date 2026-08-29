//! The dock's clock: the classic one-tile analog face, nothing else.
//! It used to be one face of a two-face system-monitor widget (a click
//! toggled a taller btop-style dashboard with a seven-segment clock,
//! CPU, memory, and network readouts); the toggle was cut on request
//! to keep the clock a clock, and the dashboard's samplers and
//! renderers went with it (see git history to resurrect them).
//!
//! The smallest widget in the dock, and after Layer 3 the smallest
//! possible one: it declares a [`Source::Clock`], holds the last
//! `(h, m, s)` it drew, and its whole `update` is a comparison. Reading
//! the wall clock is a vDSO call rather than a syscall and would never
//! have frozen anything, but the clock going through the same
//! declaration as `/proc/stat` and `nmcli` is what makes "a widget's
//! entire input is its sources" true without an exception attached.

use std::time::Duration;

use wm_theme::{clock, Theme};
use wm_theme_api::DecorationBuffer;

use chonk_dock_widget::{DockWidget, Samples, Source, SourceId};

pub struct ClockWidget {
    time: SourceId,
    /// The last reading drawn. `None` before the first `update`, which
    /// only lasts until the first event-loop pass.
    clock: Option<(u32, u32, u32)>,
}

impl ClockWidget {
    pub fn new() -> Self {
        Self { time: SourceId::UNBOUND, clock: None }
    }
}

impl Default for ClockWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for ClockWidget {
    fn name(&self) -> &'static str {
        "CLK"
    }

    fn sources(&self) -> Vec<Source> {
        // One second, because the face has a second hand. The registry
        // truncates the reading to this interval, so the tile advances
        // on the second rather than a fraction of a second after
        // whenever the shell happened to start.
        vec![Source::Clock { interval: Duration::from_secs(1) }]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        if let Some(&id) = ids.first() {
            self.time = id;
        }
    }

    fn update(&mut self, samples: &Samples) -> bool {
        let hms = samples.hms(self.time);
        let changed = self.clock != Some(hms);
        self.clock = Some(hms);
        changed
    }

    fn render(&self, theme: &Theme, tile: u32, _fonts: &mut cosmic_text::FontSystem, _swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        let (h, m, s) = self.clock.unwrap_or((0, 0, 0));
        clock::render_clock_tile(theme, tile, h, m, s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::SampleBench;

    /// The whole widget as a pure fold: a reading in, a repaint
    /// decision out, no clock and no kernel involved.
    #[test]
    fn update_redraws_on_a_new_second_and_only_on_a_new_second() {
        let mut bench = SampleBench::new();
        let id = bench.clock((9, 41, 0));
        let mut widget = ClockWidget::new();
        widget.bind(&[id]);

        assert!(widget.update(&bench.samples()), "the first reading is always a change");
        assert_eq!(widget.clock, Some((9, 41, 0)));
        assert!(!widget.update(&bench.samples()), "the same second must not repaint the dock");

        bench.set_clock(id, (9, 41, 1));
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.clock, Some((9, 41, 1)));
    }

    /// A widget that never got its ids reads midnight rather than
    /// panicking — see `SourceId::UNBOUND`. Visibly wrong beats gone.
    #[test]
    fn an_unbound_clock_shows_midnight_instead_of_crashing_the_shell() {
        let bench = SampleBench::new();
        let mut widget = ClockWidget::new();
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.clock, Some((0, 0, 0)));
    }
}
