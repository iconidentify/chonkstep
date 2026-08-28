//! The dock's widget SDK: one small trait, [`DockWidget`], plus the
//! built-in widgets. A widget owns whatever sampling/animation state it
//! needs and renders itself on demand — the same contract for every
//! widget, so `Desktop`'s own dock layout and drag-to-reorder logic
//! never needs to know a widget's internals. Adding a new one is just
//! implementing this trait and pushing it into `Desktop::new`'s widget
//! list; nothing else in the dock changes.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use wm_theme::{clock, netload, workspace, Theme};
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

/// The workspace state the [`WorkspaceWidget`] and the `Desktop` share
/// through one `Rc<RefCell<...>>`: the WM's event loop pushes the
/// authoritative `(current, count)` in through
/// `Desktop::set_workspace_display`, and the widget's click handler
/// pushes a switch request out through `requested` for the loop to
/// drain via `Desktop::take_workspace_request`. A shared cell instead
/// of widget methods because `Desktop` stores widgets as
/// `Box<dyn DockWidget>` — by design the dock can't reach a specific
/// widget's internals, so state that crosses that boundary travels
/// beside the trait object, not through it.
pub(crate) struct WorkspaceShared {
    pub current: usize,
    pub count: usize,
    /// A workspace index the user clicked their way toward, waiting for
    /// the WM to actually perform the switch — `Some` is a request, not
    /// a fact, which is why the click handler never repaints: the tile
    /// keeps showing the real current workspace until the WM confirms
    /// the switch by updating `current`/`count`.
    pub requested: Option<usize>,
}

/// The dock's workspace indicator — see `wm_theme::workspace` for the
/// Clip-flavored tile it draws. A left click cycles to the next
/// workspace (wrapping past the last back to the first) by *requesting*
/// the switch through [`WorkspaceShared`]; the repaint happens when the
/// WM reports the new workspace, never optimistically.
pub struct WorkspaceWidget {
    shared: Rc<RefCell<WorkspaceShared>>,
    /// The `(current, count)` pair the last `render` drew — `tick`
    /// compares against this so the dock repaints exactly when the
    /// visible state changed, matching the `DockWidget` contract.
    rendered: Option<(usize, usize)>,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl WorkspaceWidget {
    pub(crate) fn new(shared: Rc<RefCell<WorkspaceShared>>) -> Self {
        Self {
            shared,
            rendered: None,
            font_system: RefCell::new(cosmic_text::FontSystem::new()),
            swash_cache: RefCell::new(cosmic_text::SwashCache::new()),
        }
    }
}

impl DockWidget for WorkspaceWidget {
    fn tick(&mut self) -> bool {
        let (current, count) = {
            let shared = self.shared.borrow();
            (shared.current, shared.count)
        };
        let changed = self.rendered != Some((current, count));
        self.rendered = Some((current, count));
        changed
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let shared = self.shared.borrow();
        let mut font_system = self.font_system.borrow_mut();
        let mut swash_cache = self.swash_cache.borrow_mut();
        workspace::render_workspace_tile(theme, &mut font_system, &mut swash_cache, tile, shared.current, shared.count)
    }

    fn on_click(&mut self) -> bool {
        let mut shared = self.shared.borrow_mut();
        shared.requested = Some((shared.current + 1) % shared.count.max(1));
        false
    }
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

    fn workspace_widget(current: usize, count: usize) -> (Rc<RefCell<WorkspaceShared>>, WorkspaceWidget) {
        let shared = Rc::new(RefCell::new(WorkspaceShared { current, count, requested: None }));
        let widget = WorkspaceWidget::new(Rc::clone(&shared));
        (shared, widget)
    }

    #[test]
    fn clicking_the_workspace_widget_requests_the_next_workspace_wrapping() {
        let (shared, mut widget) = workspace_widget(1, 3);
        assert!(!widget.on_click(), "a click is a request, not a repaint — the WM confirms the switch");
        assert_eq!(shared.borrow().requested, Some(2));

        shared.borrow_mut().current = 2;
        widget.on_click();
        assert_eq!(shared.borrow().requested, Some(0), "past the last workspace wraps to the first");
    }

    #[test]
    fn clicking_the_workspace_widget_with_a_zero_count_does_not_divide_by_zero() {
        // `count` should never actually be 0 (the WM always has at least
        // one workspace), but a modulo by it must not panic the shell.
        let (shared, mut widget) = workspace_widget(0, 0);
        widget.on_click();
        assert_eq!(shared.borrow().requested, Some(0));
    }

    #[test]
    fn workspace_widget_ticks_true_exactly_when_the_visible_state_changed() {
        let (shared, mut widget) = workspace_widget(0, 2);
        assert!(widget.tick(), "first tick has never rendered, so anything is a change");
        assert!(!widget.tick(), "nothing changed since");

        shared.borrow_mut().current = 1;
        assert!(widget.tick(), "the WM switched workspaces");

        shared.borrow_mut().count = 3;
        assert!(widget.tick(), "a grown workspace count changes the position row");
        assert!(!widget.tick());
    }
}
