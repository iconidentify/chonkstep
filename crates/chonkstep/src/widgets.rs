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

use wm_theme::{clock, netload, sysmon, Theme};
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

/// Which face [`SysMonWidget`] currently shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MonitorMode {
    /// The original small analog clock face — one tile, nothing else.
    Analog,
    /// A taller `btop`-inspired panel: digital clock, CPU, memory, and
    /// network all at once.
    HighTech,
}

/// The dock's clock/CPU/memory/network instrument, combined into one
/// widget with two faces (click to toggle): a plain one-tile analog
/// clock, or a taller "high tech" dashboard. Every sampler keeps
/// updating in the background regardless of which face is showing, so
/// switching to the dashboard never starts from a cold, empty graph.
pub struct SysMonWidget {
    mode: MonitorMode,
    clock: Option<(u32, u32, u32)>,
    cpu: CpuSampler,
    mem: MemSampler,
    net: NetSampler,
}

impl SysMonWidget {
    pub fn new() -> Self {
        Self { mode: MonitorMode::Analog, clock: None, cpu: CpuSampler::new(), mem: MemSampler::new(), net: NetSampler::new() }
    }
}

impl Default for SysMonWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for SysMonWidget {
    fn tick(&mut self) -> bool {
        let hms = now_hms();
        let clock_changed = self.clock != Some(hms);
        self.clock = Some(hms);

        // Every sampler always ticks, regardless of which face is
        // showing or short-circuit evaluation — a widget further down
        // this list still needs its own chance to sample even if an
        // earlier one had nothing new this iteration.
        let cpu_changed = self.cpu.tick();
        let mem_changed = self.mem.tick();
        let net_changed = self.net.tick();

        match self.mode {
            // The dashboard's readings are invisible in this face, so
            // their changing doesn't need to trigger a repaint here.
            MonitorMode::Analog => clock_changed,
            MonitorMode::HighTech => clock_changed || cpu_changed || mem_changed || net_changed,
        }
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let (h, m, _s) = self.clock.unwrap_or((0, 0, 0));
        match self.mode {
            MonitorMode::Analog => clock::render_clock_tile(theme, tile, h, m, _s),
            MonitorMode::HighTech => {
                let rx: Vec<f32> = self.net.rx.iter().copied().collect();
                let tx: Vec<f32> = self.net.tx.iter().copied().collect();
                sysmon::render_sysmon_panel(theme, tile, h, m, self.cpu.displayed, self.mem.fraction, &rx, &tx)
            }
        }
    }

    fn tile_height(&self) -> u32 {
        match self.mode {
            MonitorMode::Analog => 1,
            // Comfortably fits the dashboard's clock + CPU tile + memory
            // bar + network tile stack (see `sysmon::render_sysmon_panel`
            // for the exact proportions) with a little slack rather than
            // a razor-tight fit.
            MonitorMode::HighTech => 3,
        }
    }

    fn on_click(&mut self) -> bool {
        self.mode = match self.mode {
            MonitorMode::Analog => MonitorMode::HighTech,
            MonitorMode::HighTech => MonitorMode::Analog,
        };
        true
    }
}

struct CpuSampler {
    last_sample: Instant,
    last_anim: Instant,
    last_totals: Option<(u64, u64)>,
    target: f32,
    displayed: f32,
}

impl CpuSampler {
    fn new() -> Self {
        let past = Instant::now() - SAMPLE_INTERVAL;
        Self { last_sample: past, last_anim: past, last_totals: None, target: 0.0, displayed: 0.0 }
    }

    /// Returns whether `displayed` actually moved.
    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() >= SAMPLE_INTERVAL {
            self.last_sample = Instant::now();
            if let Some((idle, total)) = read_cpu_totals() {
                if let Some((prev_idle, prev_total)) = self.last_totals {
                    let d_total = total.saturating_sub(prev_total);
                    let d_idle = idle.saturating_sub(prev_idle);
                    if d_total > 0 {
                        self.target = 1.0 - (d_idle as f32 / d_total as f32);
                    }
                }
                self.last_totals = Some((idle, total));
            }
        }

        if self.last_anim.elapsed() < ANIM_INTERVAL {
            return false;
        }
        let delta = self.target - self.displayed;
        if delta.abs() < 0.002 {
            return false;
        }
        self.last_anim = Instant::now();
        self.displayed += delta * 0.35;
        true
    }
}

/// `/proc/stat`'s first line: `cpu  user nice system idle iowait irq
/// softirq steal ...`. Returns `(idle, total)` jiffy counters — the
/// caller diffs two samples to get a utilization fraction, since a
/// single snapshot alone is meaningless (it's a cumulative counter since
/// boot, not an instantaneous reading).
fn read_cpu_totals() -> Option<(u64, u64)> {
    parse_cpu_totals(&std::fs::read_to_string("/proc/stat").ok()?)
}

fn parse_cpu_totals(contents: &str) -> Option<(u64, u64)> {
    let line = contents.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.filter_map(|f| f.parse().ok()).collect();
    // idle + iowait — both count as "not doing work" for load purposes.
    let idle = *values.get(3)? + values.get(4).copied().unwrap_or(0);
    let total: u64 = values.iter().sum();
    Some((idle, total))
}

struct MemSampler {
    last_sample: Instant,
    fraction: f32,
}

impl MemSampler {
    fn new() -> Self {
        Self { last_sample: Instant::now() - SAMPLE_INTERVAL, fraction: 0.0 }
    }

    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() < SAMPLE_INTERVAL {
            return false;
        }
        self.last_sample = Instant::now();
        let Some(fraction) = read_mem_fraction() else { return false };
        let changed = (fraction - self.fraction).abs() >= 0.002;
        self.fraction = fraction;
        changed
    }
}

/// `/proc/meminfo`: `MemAvailable` (not the naive `MemFree`) is what
/// actually reflects memory the kernel could hand to a new process
/// without swapping — the same figure `htop`/`btop` use for their
/// "used" percentage.
fn read_mem_fraction() -> Option<f32> {
    parse_mem_fraction(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_mem_fraction(contents: &str) -> Option<f32> {
    let mut total = None;
    let mut available = None;
    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        let key = parts.next()?;
        let Some(value) = parts.next().and_then(|v| v.parse::<u64>().ok()) else { continue };
        match key {
            "MemTotal:" => total = Some(value),
            "MemAvailable:" => available = Some(value),
            _ => {}
        }
        if total.is_some() && available.is_some() {
            break;
        }
    }
    let total = total? as f32;
    let available = available? as f32;
    if total <= 0.0 {
        return None;
    }
    Some((1.0 - available / total).clamp(0.0, 1.0))
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
    fn parses_idle_and_total_jiffies_from_the_cpu_line() {
        let stat = "cpu  100 0 50 800 20 0 0 0 0 0\ncpu0 50 0 25 400 10 0 0 0 0 0\n";
        let (idle, total) = parse_cpu_totals(stat).expect("cpu line should parse");
        assert_eq!(idle, 800 + 20, "idle should include iowait");
        assert_eq!(total, 100 + 50 + 800 + 20);
    }

    #[test]
    fn missing_cpu_line_parses_to_none() {
        assert_eq!(parse_cpu_totals("not the stat format"), None);
    }

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
    fn mem_fraction_uses_available_not_free() {
        let meminfo = "MemTotal:       1000000 kB\nMemFree:          10000 kB\nMemAvailable:    600000 kB\n";
        let fraction = parse_mem_fraction(meminfo).expect("meminfo should parse");
        assert!((fraction - 0.4).abs() < 0.001, "used = 1 - available/total = 0.4, got {fraction}");
    }

    #[test]
    fn missing_mem_available_parses_to_none() {
        assert_eq!(parse_mem_fraction("MemTotal: 1000000 kB\n"), None);
    }

    #[test]
    fn clicking_the_widget_toggles_between_analog_and_high_tech() {
        let mut widget = SysMonWidget::new();
        assert_eq!(widget.tile_height(), 1, "starts in the compact analog face");
        assert!(widget.on_click());
        assert_eq!(widget.tile_height(), 3, "switches to the taller dashboard face");
        assert!(widget.on_click());
        assert_eq!(widget.tile_height(), 1, "and back again");
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
