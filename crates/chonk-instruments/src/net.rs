//! Network traffic instrument: per-interface up/down throughput on the
//! `wm_theme::nettraffic` instrument face — download story on top,
//! upload story on the bottom, mirrored history graph between them.
//! This file is strictly the data half: it samples `/proc/net/dev`,
//! maintains rates and normalized histories, and hands the renderer
//! plain quantized values, so the renderer stays unit-testable
//! pixel-for-pixel without a live network.
//!
//! The sampling layer keeps interfaces separate (click cycles them)
//! and normalizes each one's history against its own decaying peak, so
//! a gigabit burst doesn't flatten a quiet link's graph into nothing.
//! The absolute rates go to the renderer un-normalized — its digit
//! readouts exist precisely to carry the magnitude the normalized
//! graph gives up.
//!
//! `/proc/net/dev` is a [`Source::File`] now, read on a sampler thread
//! and handed to `update` as a string; `parse_interface_totals` is
//! unchanged, because it never knew where the string came from.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Instant;

use wm_theme::nettraffic::{self, TrafficLane};
use wm_theme::{panel, Theme};
use wm_theme_api::DecorationBuffer;

use chonk_dock_widget::{DockInput, DockWidget, Effect, Samples, Source, SourceId, SAMPLE_INTERVAL};

/// How many history samples the widget keeps per direction — enough
/// for any renderer column count up to this.
pub const HISTORY: usize = 32;

/// `/proc/net/dev`, kept per-interface rather than summed. Fields: rx
/// bytes at index 0 after the colon, tx bytes at index 8. `lo` is
/// excluded; there's nothing interesting to cycle to on loopback.
fn parse_interface_totals(contents: &str) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    for line in contents.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else { continue };
        let name = iface.trim();
        if name.is_empty() || name == "lo" {
            continue;
        }
        let fields: Vec<u64> = rest.split_whitespace().filter_map(|f| f.parse().ok()).collect();
        let Some(&rx) = fields.first() else { continue };
        let Some(&tx) = fields.get(8) else { continue };
        out.push((name.to_string(), rx, tx));
    }
    out
}

pub(crate) struct InterfaceLoad {
    pub name: String,
    last_totals: Option<(u64, u64)>,
    /// Decaying per-interface peak both direction histories normalize
    /// against — floors at 1 KiB/s so idle links don't divide by zero
    /// or show noise as full-scale.
    pub peak_bps: f32,
    pub rx_bps: f32,
    pub tx_bps: f32,
    /// Normalized `0.0..=1.0` history, oldest first.
    pub rx_history: VecDeque<f32>,
    pub tx_history: VecDeque<f32>,
}

impl InterfaceLoad {
    fn new(name: String) -> Self {
        Self {
            name,
            last_totals: None,
            peak_bps: 1024.0,
            rx_bps: 0.0,
            tx_bps: 0.0,
            rx_history: VecDeque::from(vec![0.0; HISTORY]),
            tx_history: VecDeque::from(vec![0.0; HISTORY]),
        }
    }
}

pub struct NetTrafficWidget {
    dev: SourceId,
    /// When the previous `/proc/net/dev` reading was folded in, purely
    /// as the denominator of a rate.
    ///
    /// This is a clock read, not I/O: `Instant::now` is a vDSO call
    /// costing tens of nanoseconds, and it is not the thing Layer 3
    /// took away from widgets. The widget needs it because bytes-per-
    /// second is bytes divided by *actual* seconds, and the sampler's
    /// interval is a request rather than a promise — a run that took
    /// 1.4s must not be reported as if it took 1.0s. Measuring between
    /// folds rather than between passes is what makes this the
    /// sampler's real cadence: `update` runs at 60Hz and folds at 1Hz.
    last_fold: Option<Instant>,
    pub(crate) interfaces: Vec<InterfaceLoad>,
    pub(crate) selected: usize,
}

impl NetTrafficWidget {
    pub fn new() -> Self {
        Self { dev: SourceId::UNBOUND, last_fold: None, interfaces: Vec::new(), selected: 0 }
    }
}

impl Default for NetTrafficWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for NetTrafficWidget {
    fn name(&self) -> &str {
        "NET"
    }

    fn sources(&self) -> Vec<Source> {
        vec![Source::File { path: PathBuf::from("/proc/net/dev"), interval: SAMPLE_INTERVAL }]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        self.dev = ids.first().copied().unwrap_or(SourceId::UNBOUND);
    }

    fn update(&mut self, samples: &Samples) -> bool {
        if !samples.fresh(self.dev) {
            return false;
        }
        // The very first fold has no previous reading to difference
        // against, so it has no interval either; `SAMPLE_INTERVAL` is
        // the honest guess and it is only ever used for interfaces that
        // already had a `last_totals`, which on the first fold is none
        // of them.
        let elapsed = self.last_fold.replace(Instant::now()).map_or(SAMPLE_INTERVAL, |at| at.elapsed()).as_secs_f32();

        let readings = parse_interface_totals(samples.text(self.dev).unwrap_or_default());
        for (name, rx_total, tx_total) in &readings {
            if let Some(existing) = self.interfaces.iter_mut().find(|i| &i.name == name) {
                if let Some((prev_rx, prev_tx)) = existing.last_totals.replace((*rx_total, *tx_total)) {
                    let rx_bps = rx_total.saturating_sub(prev_rx) as f32 / elapsed.max(0.1);
                    let tx_bps = tx_total.saturating_sub(prev_tx) as f32 / elapsed.max(0.1);
                    existing.peak_bps = (existing.peak_bps * 0.98).max(rx_bps).max(tx_bps).max(1024.0);
                    existing.rx_bps = rx_bps;
                    existing.tx_bps = tx_bps;
                    existing.rx_history.pop_front();
                    existing.rx_history.push_back((rx_bps / existing.peak_bps).clamp(0.0, 1.0));
                    existing.tx_history.pop_front();
                    existing.tx_history.push_back((tx_bps / existing.peak_bps).clamp(0.0, 1.0));
                }
            } else {
                let mut fresh = InterfaceLoad::new(name.clone());
                fresh.last_totals = Some((*rx_total, *tx_total));
                self.interfaces.push(fresh);
            }
        }
        self.interfaces.retain(|i| readings.iter().any(|(name, _, _)| name == &i.name));
        if self.selected >= self.interfaces.len() {
            self.selected = 0;
        }
        true
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        // No interfaces at all is the SDK's dead-screen empty state,
        // not a zeroed instrument — a powered-off screen says "nothing
        // to measure", a zeroed one would say "measuring silence".
        let Some(iface) = self.interfaces.get(self.selected) else {
            return panel::render_dead_tile(theme, fonts, swash, tile, "NET");
        };
        // The renderer wants the newest NET_TRAFFIC_COLUMNS samples
        // quantized to dot-rows; the widget keeps a longer float
        // history so a future renderer (or scale) can ask for more.
        let quantize = |history: &VecDeque<f32>| -> Vec<u32> {
            let skip = history.len().saturating_sub(nettraffic::NET_TRAFFIC_COLUMNS);
            history.iter().skip(skip).map(|&v| nettraffic::quantize_level(v, nettraffic::NET_TRAFFIC_HALF_ROWS)).collect()
        };
        let rx_history = quantize(&iface.rx_history);
        let tx_history = quantize(&iface.tx_history);
        let down = TrafficLane {
            readout: nettraffic::format_rate(iface.rx_bps),
            now: nettraffic::quantize_level(iface.rx_bps / iface.peak_bps, nettraffic::NET_TRAFFIC_HALF_ROWS),
            history: &rx_history,
        };
        let up = TrafficLane {
            readout: nettraffic::format_rate(iface.tx_bps),
            now: nettraffic::quantize_level(iface.tx_bps / iface.peak_bps, nettraffic::NET_TRAFFIC_HALF_ROWS),
            history: &tx_history,
        };
        nettraffic::render_nettraffic_tile(theme, fonts, swash, tile, &iface.name, &down, &up)
    }

    /// Left-press cycles which interface the tile watches. Release and
    /// everything else are ignored: cycling on both edges would advance
    /// twice per click.
    fn on_input(&mut self, input: DockInput, _tile: u32) -> Vec<Effect> {
        if !matches!(input, DockInput::Press { .. }) || self.interfaces.is_empty() {
            return Vec::new();
        }
        self.selected = (self.selected + 1) % self.interfaces.len();
        vec![Effect::Repaint]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chonk_dock_widget::SampleBench;
    use chonk_dock_widget::MouseButton;
    use wm_theme_api::Point;

    fn press() -> DockInput {
        DockInput::Press { local: Point::new(4, 4), button: MouseButton::Left }
    }

    #[test]
    fn per_interface_totals_keep_each_interface_separate_and_skip_loopback() {
        let dev = "Inter-|   Receive\n face |bytes\n\
            \x20 lo: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0\n\
            \x20 eth0: 1000 5 0 0 0 0 0 0 200 3 0 0 0 0 0 0\n\
            \x20 wlan0: 500 2 0 0 0 0 0 0 100 1 0 0 0 0 0 0\n";
        let readings = parse_interface_totals(dev);
        assert_eq!(readings, vec![("eth0".to_string(), 1000, 200), ("wlan0".to_string(), 500, 100)]);
    }

    #[test]
    fn empty_interface_totals_do_not_panic() {
        assert_eq!(parse_interface_totals(""), Vec::<(String, u64, u64)>::new());
    }

    /// The wiring claim: with no interfaces the widget shows the SDK's
    /// dead screen, and once one exists (populated the way `update()`
    /// would) it switches to the live instrument face — the two must
    /// render differently or the renderer isn't actually hooked up.
    #[test]
    fn widget_face_goes_live_once_an_interface_exists() {
        let theme = wm_theme::default_theme::nextstep_classic();
        let (mut fonts, mut swash) = (cosmic_text::FontSystem::new(), cosmic_text::SwashCache::new());
        let mut widget = NetTrafficWidget::new();
        let dead = widget.render(&theme, 56, &mut fonts, &mut swash);
        assert_eq!((dead.width, dead.height), (56, 56));

        let mut iface = InterfaceLoad::new("eth0".to_string());
        iface.rx_bps = 42.0 * 1024.0;
        iface.tx_bps = 3.0 * 1024.0;
        iface.rx_history.pop_front();
        iface.rx_history.push_back(0.8);
        widget.interfaces.push(iface);
        let live = widget.render(&theme, 56, &mut fonts, &mut swash);
        assert_eq!((live.width, live.height), (56, 56));
        assert_ne!(dead.pixels, live.pixels);
    }

    fn dev(eth_rx: u64, eth_tx: u64) -> String {
        format!(
            "Inter-|   Receive\n face |bytes\n  lo: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0\n  eth0: {eth_rx} 5 0 0 0 0 0 0 {eth_tx} 3 0 0 0 0 0 0\n  wlan0: 500 2 0 0 0 0 0 0 100 1 0 0 0 0 0 0\n"
        )
    }

    /// Two `/proc/net/dev` readings in, one rate out — and the first
    /// one deliberately produces no rate at all, because there is
    /// nothing to difference it against and reporting the counter as a
    /// rate would show a gigabyte per second on every startup.
    #[test]
    fn update_differences_two_readings_into_a_rate() {
        let mut bench = SampleBench::new();
        let id = bench.text(&dev(1_000, 200));
        let mut widget = NetTrafficWidget::new();
        widget.bind(&[id]);

        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.interfaces.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), vec!["eth0", "wlan0"]);
        assert_eq!(widget.interfaces[0].rx_bps, 0.0, "the first reading is a baseline, not a rate");

        bench.set_text(id, &dev(1_000 + 4_096, 200 + 1_024));
        assert!(widget.update(&bench.samples()));
        // The elapsed time between two folds in a test is microseconds,
        // so the exact rate is meaningless — that both directions moved
        // and stayed in proportion is the claim the fold is responsible
        // for. (`peak_bps` normalizes the graph against the larger.)
        let eth = &widget.interfaces[0];
        assert!(eth.rx_bps > 0.0 && eth.tx_bps > 0.0);
        assert!((eth.rx_bps / eth.tx_bps - 4.0).abs() < 1e-3, "4096 bytes down against 1024 up is 4:1");
        assert!(eth.peak_bps >= eth.rx_bps);
    }

    #[test]
    fn interfaces_that_disappear_are_dropped_and_the_selection_survives_it() {
        let mut bench = SampleBench::new();
        let id = bench.text(&dev(1_000, 200));
        let mut widget = NetTrafficWidget::new();
        widget.bind(&[id]);
        widget.update(&bench.samples());

        assert_eq!(widget.on_input(press(), 56).len(), 1, "a press over two interfaces cycles and repaints");
        assert_eq!(widget.selected, 1);

        bench.set_text(id, "Inter-|\n face |\n  eth0: 1 2 0 0 0 0 0 0 3 4 0 0 0 0 0 0\n");
        widget.update(&bench.samples());
        assert_eq!(widget.interfaces.len(), 1, "wlan0 went away");
        assert_eq!(widget.selected, 0, "and the selection must not point past the end");
    }

    /// A stale pass and an empty reading are different: the first is
    /// "nothing happened", the second is "the machine has no
    /// interfaces". Only the second may clear the tile.
    #[test]
    fn a_stale_pass_folds_nothing_and_an_empty_reading_clears_the_tile() {
        let mut bench = SampleBench::new();
        let id = bench.text(&dev(1_000, 200));
        let mut widget = NetTrafficWidget::new();
        widget.bind(&[id]);
        widget.update(&bench.samples());

        bench.all_stale();
        assert!(!widget.update(&bench.samples()));
        assert_eq!(widget.interfaces.len(), 2);

        bench.set_text(id, "");
        widget.update(&bench.samples());
        assert!(widget.interfaces.is_empty());
        assert!(widget.on_input(press(), 56).is_empty(), "a dead screen has no interfaces to cycle");
    }
}
