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

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Instant;

use wm_theme::nettraffic::{self, TrafficLane};
use wm_theme::{panel, Theme};
use wm_theme_api::{DecorationBuffer, Point};

use super::{DockWidget, SAMPLE_INTERVAL};

/// How many history samples the widget keeps per direction — enough
/// for any renderer column count up to this.
pub const HISTORY: usize = 32;

/// `/proc/net/dev`, kept per-interface rather than summed. Fields: rx
/// bytes at index 0 after the colon, tx bytes at index 8. `lo` is
/// excluded; there's nothing interesting to cycle to on loopback.
fn read_interface_totals() -> Vec<(String, u64, u64)> {
    parse_interface_totals(&std::fs::read_to_string("/proc/net/dev").unwrap_or_default())
}

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
    last_sample: Instant,
    pub(crate) interfaces: Vec<InterfaceLoad>,
    pub(crate) selected: usize,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl NetTrafficWidget {
    pub fn new() -> Self {
        Self {
            last_sample: Instant::now() - SAMPLE_INTERVAL,
            interfaces: Vec::new(),
            selected: 0,
            font_system: RefCell::new(cosmic_text::FontSystem::new()),
            swash_cache: RefCell::new(cosmic_text::SwashCache::new()),
        }
    }
}

impl Default for NetTrafficWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for NetTrafficWidget {
    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() < SAMPLE_INTERVAL {
            return false;
        }
        let elapsed = self.last_sample.elapsed().as_secs_f32();
        self.last_sample = Instant::now();

        let readings = read_interface_totals();
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

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let mut font_system = self.font_system.borrow_mut();
        let mut swash_cache = self.swash_cache.borrow_mut();
        // No interfaces at all is the SDK's dead-screen empty state,
        // not a zeroed instrument — a powered-off screen says "nothing
        // to measure", a zeroed one would say "measuring silence".
        let Some(iface) = self.interfaces.get(self.selected) else {
            return panel::render_dead_tile(theme, &mut font_system, &mut swash_cache, tile, "NET");
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
        nettraffic::render_nettraffic_tile(theme, &mut font_system, &mut swash_cache, tile, &iface.name, &down, &up)
    }

    fn on_click(&mut self, _local: Point, _tile: u32) -> bool {
        if self.interfaces.is_empty() {
            return false;
        }
        self.selected = (self.selected + 1) % self.interfaces.len();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    /// dead screen, and once one exists (populated the way `tick()`
    /// would) it switches to the live instrument face — the two must
    /// render differently or the renderer isn't actually hooked up.
    #[test]
    fn widget_face_goes_live_once_an_interface_exists() {
        let theme = wm_theme::default_theme::nextstep_classic();
        let mut widget = NetTrafficWidget::new();
        let dead = widget.render(&theme, 56);
        assert_eq!((dead.width, dead.height), (56, 56));

        let mut iface = InterfaceLoad::new("eth0".to_string());
        iface.rx_bps = 42.0 * 1024.0;
        iface.tx_bps = 3.0 * 1024.0;
        iface.rx_history.pop_front();
        iface.rx_history.push_back(0.8);
        widget.interfaces.push(iface);
        let live = widget.render(&theme, 56);
        assert_eq!((live.width, live.height), (56, 56));
        assert_ne!(dead.pixels, live.pixels);
    }
}
