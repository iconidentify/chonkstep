//! The dock's widget SDK: one small trait, [`DockWidget`], plus the
//! built-in widgets. A widget owns whatever sampling/animation state it
//! needs and renders itself on demand — the same contract for every
//! widget, so `Desktop`'s own dock layout and drag-to-reorder logic
//! never needs to know a widget's internals. Adding a new one is just
//! implementing this trait and pushing it into `Desktop::new`'s widget
//! list; nothing else in the dock changes.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wm_theme::{clock, netload, Theme};
use wm_theme_api::DecorationBuffer;

/// A single dock widget. `tick` is called roughly once per event-loop
/// iteration — cheap by design, since every widget is responsible for
/// throttling its own expensive work (sampling `/proc`, easing an
/// animation) internally rather than assuming any particular call rate.
/// Returns whether `render` would now produce different pixels, so the
/// dock only repaints when something actually changed.
pub trait DockWidget {
    fn tick(&mut self) -> bool;
    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer;

    /// How many `tile`-tall units this widget currently occupies in the
    /// dock's vertical stack. Most widgets are exactly one square tile;
    /// override when a widget's rendered size varies (e.g. by mode).
    fn tile_height(&self) -> u32 {
        1
    }

    /// Left-click handling — returns whether the widget's appearance
    /// changed (so the dock knows to repaint). Most widgets have no
    /// click behavior; the default no-op covers them.
    fn on_click(&mut self) -> bool {
        false
    }
}

fn now_hms() -> (u32, u32, u32) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs_today = secs % 86_400;
    ((secs_today / 3600) as u32, ((secs_today % 3600) / 60) as u32, (secs_today % 60) as u32)
}

/// How often widgets that sample `/proc` actually re-read it — every
/// `tick()` call still runs (for animation easing), but the real syscall
/// cost is paid at most this often.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(1000);

/// How often the CPU meter's displayed level takes one easing step
/// toward the real sampled load. Deliberately much coarser than the
/// event loop's own ~60Hz tick rate: each step that actually moves
/// triggers a full dock repaint (a real `PutImage` over the wire), and a
/// meter genuinely doesn't need to visibly move more often than this to
/// read as smooth.
const ANIM_INTERVAL: Duration = Duration::from_millis(80);

/// How many recent samples the network graph keeps — one column per
/// sample at render time.
const NET_HISTORY: usize = 20;

/// The dock's clock: the classic one-tile analog face, nothing else.
/// It used to be one face of a two-face system-monitor widget (a click
/// toggled a taller btop-style dashboard with a seven-segment clock,
/// CPU, memory, and network readouts); the toggle was cut on request
/// to keep the clock a clock, and the dashboard's samplers and
/// renderers went with it (see git history to resurrect them).
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

struct NetSampler {
    last_sample: Instant,
    last_totals: Option<(u64, u64)>,
    peak_bps: f32,
    rx: VecDeque<f32>,
    tx: VecDeque<f32>,
}

impl NetSampler {
    fn new() -> Self {
        Self {
            last_sample: Instant::now() - SAMPLE_INTERVAL,
            last_totals: None,
            peak_bps: 1.0,
            rx: VecDeque::from(vec![0.0; NET_HISTORY]),
            tx: VecDeque::from(vec![0.0; NET_HISTORY]),
        }
    }

    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() < SAMPLE_INTERVAL {
            return false;
        }
        let elapsed = self.last_sample.elapsed().as_secs_f32();
        self.last_sample = Instant::now();

        let Some((rx, tx)) = read_net_totals() else { return false };
        let Some((prev_rx, prev_tx)) = self.last_totals.replace((rx, tx)) else {
            return false;
        };

        let rx_bps = rx.saturating_sub(prev_rx) as f32 / elapsed.max(0.1);
        let tx_bps = tx.saturating_sub(prev_tx) as f32 / elapsed.max(0.1);

        // Slowly-decaying peak so the graph rescales to recent activity
        // instead of staying squashed by one old spike forever.
        self.peak_bps = (self.peak_bps * 0.98).max(rx_bps).max(tx_bps).max(1024.0);

        self.rx.pop_front();
        self.rx.push_back((rx_bps / self.peak_bps).clamp(0.0, 1.0));
        self.tx.pop_front();
        self.tx.push_back((tx_bps / self.peak_bps).clamp(0.0, 1.0));
        true
    }
}

/// `/proc/net/dev`: two header lines, then one line per interface —
/// `iface: rx_bytes rx_packets ... tx_bytes tx_packets ...` (rx bytes is
/// field 0 after the colon, tx bytes is field 8). `lo` is excluded so
/// purely local traffic doesn't register as network activity.
fn read_net_totals() -> Option<(u64, u64)> {
    parse_net_totals(&std::fs::read_to_string("/proc/net/dev").ok()?)
}

fn parse_net_totals(contents: &str) -> Option<(u64, u64)> {
    let mut rx_total = 0u64;
    let mut tx_total = 0u64;
    for line in contents.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else { continue };
        if iface.trim() == "lo" {
            continue;
        }
        let fields: Vec<u64> = rest.split_whitespace().filter_map(|f| f.parse().ok()).collect();
        let Some(&rx) = fields.first() else { continue };
        let Some(&tx) = fields.get(8) else { continue };
        rx_total += rx;
        tx_total += tx;
    }
    Some((rx_total, tx_total))
}

/// `/proc/net/dev`, kept per-interface rather than summed — see
/// [`parse_net_totals`] for the field-layout explanation (rx bytes at
/// field 0, tx bytes at field 8). `lo` is excluded; there's nothing
/// interesting to cycle to on a loopback link.
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

struct InterfaceLoad {
    name: String,
    last_totals: Option<(u64, u64)>,
    peak_bps: f32,
    combined_bps: f32,
    rx_levels: VecDeque<u32>,
    tx_levels: VecDeque<u32>,
}

impl InterfaceLoad {
    fn new(name: String) -> Self {
        Self {
            name,
            last_totals: None,
            peak_bps: 1.0,
            combined_bps: 0.0,
            rx_levels: VecDeque::from(vec![0u32; netload::NET_LOAD_COLUMNS]),
            tx_levels: VecDeque::from(vec![0u32; netload::NET_LOAD_COLUMNS]),
        }
    }
}

/// A close port of the classic WindowMaker dockapp `wmnetload` — see
/// `wm_theme::netload` for the actual rendering (the monochrome LCD
/// panel, seven-segment readout, and dot-matrix graph). Owns per-
/// interface sampling and a click-to-cycle interaction, quantized to
/// the discrete dot-levels and three-digit rate the LCD readout needs.
pub struct NetLoadWidget {
    last_sample: Instant,
    interfaces: Vec<InterfaceLoad>,
    selected: usize,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl NetLoadWidget {
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

impl Default for NetLoadWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for NetLoadWidget {
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
                    existing.combined_bps = rx_bps + tx_bps;

                    let half_rows = netload::NET_LOAD_HALF_ROWS as f32;
                    let rx_level = ((rx_bps / existing.peak_bps).clamp(0.0, 1.0) * half_rows).round() as u32;
                    let tx_level = ((tx_bps / existing.peak_bps).clamp(0.0, 1.0) * half_rows).round() as u32;
                    existing.rx_levels.pop_front();
                    existing.rx_levels.push_back(rx_level);
                    existing.tx_levels.pop_front();
                    existing.tx_levels.push_back(tx_level);
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
        let Some(iface) = self.interfaces.get(self.selected) else {
            return netload::render_netload_tile(theme, &mut font_system, &mut swash_cache, tile, "---", [None; 3], netload::NetloadUnit::Kilo, &[], &[]);
        };
        let (digits, unit) = format_rate_digits(iface.combined_bps);
        let rx: Vec<u32> = iface.rx_levels.iter().copied().collect();
        let tx: Vec<u32> = iface.tx_levels.iter().copied().collect();
        netload::render_netload_tile(theme, &mut font_system, &mut swash_cache, tile, &iface.name, digits, unit, &rx, &tx)
    }

    fn on_click(&mut self) -> bool {
        if self.interfaces.is_empty() {
            return false;
        }
        self.selected = (self.selected + 1) % self.interfaces.len();
        true
    }
}

/// Picks the smallest of K/M/G that keeps the value under 1000 (matching
/// the original's fixed three-digit readout) and splits it into digits
/// with leading positions blanked — a real LCD readout shows blank
/// space, not `0`s, before the first significant digit, though the
/// final position always shows something (`0` reads as "idle," not
/// "no data").
fn format_rate_digits(bytes_per_sec: f32) -> ([Option<u8>; 3], netload::NetloadUnit) {
    let kbps = bytes_per_sec / 1024.0;
    let (value, unit) = if kbps < 1000.0 {
        (kbps, netload::NetloadUnit::Kilo)
    } else if kbps / 1024.0 < 1000.0 {
        (kbps / 1024.0, netload::NetloadUnit::Mega)
    } else {
        (kbps / 1024.0 / 1024.0, netload::NetloadUnit::Giga)
    };

    let whole = (value.round() as u32).min(999);
    let d = [whole / 100, (whole / 10) % 10, whole % 10];
    let mut digits = [None; 3];
    let mut leading = true;
    for (i, &digit) in d.iter().enumerate() {
        if digit != 0 || i == 2 {
            leading = false;
        }
        digits[i] = if leading { None } else { Some(digit as u8) };
    }
    (digits, unit)
}

#[cfg(test)]
mod tests {
    use super::*;



    #[test]
    fn net_totals_sum_every_interface_except_loopback() {
        let dev = "Inter-|   Receive\n face |bytes\n\
            \x20 lo: 999 0 0 0 0 0 0 0 999 0 0 0 0 0 0 0\n\
            \x20 eth0: 1000 5 0 0 0 0 0 0 200 3 0 0 0 0 0 0\n\
            \x20 wlan0: 500 2 0 0 0 0 0 0 100 1 0 0 0 0 0 0\n";
        let (rx, tx) = parse_net_totals(dev).expect("dev lines should parse");
        assert_eq!(rx, 1000 + 500, "loopback traffic must not count");
        assert_eq!(tx, 200 + 100);
    }

    #[test]
    fn empty_dev_contents_do_not_panic() {
        assert_eq!(parse_net_totals(""), Some((0, 0)));
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

    #[test]
    fn rate_digits_blank_leading_zeros_but_always_show_the_last_digit() {
        assert_eq!(format_rate_digits(0.0).0, [None, None, Some(0)]);
        let (digits, unit) = format_rate_digits(42.0 * 1024.0);
        assert_eq!(digits, [None, Some(4), Some(2)]);
        assert_eq!(unit, netload::NetloadUnit::Kilo);
    }

    #[test]
    fn rate_digits_pick_the_smallest_unit_that_keeps_it_under_1000() {
        let (digits, unit) = format_rate_digits(1500.0 * 1024.0);
        assert_eq!(unit, netload::NetloadUnit::Mega, "1500 KB/s should roll over into MB/s");
        assert_eq!(digits, [None, None, Some(1)], "1500 KiB/s is ~1.46 MiB/s, rounds to 1");

        let (_, unit) = format_rate_digits(1500.0 * 1024.0 * 1024.0);
        assert_eq!(unit, netload::NetloadUnit::Giga);
    }

    #[test]
    fn rate_digits_clamp_at_999_rather_than_overflowing() {
        let (digits, _) = format_rate_digits(50_000.0 * 1024.0 * 1024.0 * 1024.0);
        assert_eq!(digits, [Some(9), Some(9), Some(9)]);
    }

    #[test]
    fn clicking_the_load_widget_with_no_interfaces_discovered_yet_is_a_harmless_no_op() {
        let mut widget = NetLoadWidget::new();
        assert!(!widget.on_click());
    }
}
