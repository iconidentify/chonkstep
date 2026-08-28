//! System load instrument: CPU and memory pressure on one LED screen,
//! rendered by `wm_theme::sysload`. This module is the data half only
//! — `/proc/stat` and `/proc/meminfo` sampling, delta bookkeeping, and
//! quantization policy — with every parse step a pure function over a
//! string, so fixtures can test the arithmetic without a live kernel.
//!
//! CPU percent is the classic counter-delta: busy and total jiffies
//! from `/proc/stat`'s aggregate `cpu` line, differenced between
//! samples (the line is monotonic totals since boot, so a single
//! reading means nothing). Memory is `1 - MemAvailable/MemTotal` —
//! `MemAvailable` because the kernel already answers "how much could
//! be claimed without swapping" there, which is the pressure question,
//! where free-minus-buffers arithmetic famously is not.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::time::Instant;

use wm_theme::sysload::{render_sysload_tile, SYS_LOAD_COLUMNS, SYS_LOAD_MEM_SEGMENTS, SYS_LOAD_ROWS};
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

use super::{DockWidget, SAMPLE_INTERVAL};

/// Used-memory fraction at which the tile lights its alarm frame. 90%
/// of *available*-derived usage means the kernel is genuinely close to
/// reclaiming/swapping, not just holding page cache.
const MEM_ALERT_FRACTION: f32 = 0.90;

/// `/proc/stat` aggregate line -> `(busy, total)` jiffy counters.
/// Sums at most the first 8 value fields (user, nice, system, idle,
/// iowait, irq, softirq, steal): `guest`/`guest_nice` are already
/// included in `user`/`nice` per the kernel's own accounting, so
/// summing them would double-count VM time. Idle time is
/// `idle + iowait` — a core waiting on disk is not doing work.
fn parse_cpu_totals(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().next()?;
    let mut fields = line.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    let values: Vec<u64> = fields.take(8).map_while(|f| f.parse().ok()).collect();
    if values.len() < 4 {
        return None;
    }
    let total: u64 = values.iter().sum();
    let idle = values[3] + values.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// `/proc/meminfo` -> used fraction `0.0..=1.0`, `None` when either
/// key is missing (a kernel too old for `MemAvailable` gets a dead
/// reading rather than a wrong one).
fn parse_mem_used_fraction(meminfo: &str) -> Option<f32> {
    let field = |key: &str| {
        meminfo
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|v| v.parse::<u64>().ok())
    };
    let total = field("MemTotal:")?;
    let available = field("MemAvailable:")?;
    if total == 0 {
        return None;
    }
    Some((1.0 - available as f32 / total as f32).clamp(0.0, 1.0))
}

/// Busy fraction between two `(busy, total)` samples. Saturating on
/// purpose: counters can go backward across a suspend/resume or
/// counter reset, and the honest answer for a garbled interval is 0,
/// not a spike.
fn cpu_fraction(prev: (u64, u64), current: (u64, u64)) -> f32 {
    let busy = current.0.saturating_sub(prev.0);
    let total = current.1.saturating_sub(prev.1);
    if total == 0 {
        return 0.0;
    }
    (busy as f32 / total as f32).clamp(0.0, 1.0)
}

/// Fraction -> lit LED count out of `max`, round-to-nearest so the
/// display centers on the truth instead of systematically under- or
/// over-reporting.
fn quantize_level(fraction: f32, max: u32) -> u32 {
    (fraction.clamp(0.0, 1.0) * max as f32).round() as u32
}

pub struct SysLoadWidget {
    last_sample: Instant,
    prev_cpu: Option<(u64, u64)>,
    /// Busy fractions `0.0..=1.0`, oldest first, always exactly
    /// `SYS_LOAD_COLUMNS` long so the renderer's column count and the
    /// history length cannot drift apart.
    cpu_history: VecDeque<f32>,
    mem_used_frac: f32,
    font_system: RefCell<cosmic_text::FontSystem>,
    swash_cache: RefCell<cosmic_text::SwashCache>,
}

impl SysLoadWidget {
    pub fn new() -> Self {
        Self {
            last_sample: Instant::now() - SAMPLE_INTERVAL,
            prev_cpu: None,
            cpu_history: VecDeque::from(vec![0.0; SYS_LOAD_COLUMNS]),
            mem_used_frac: 0.0,
            font_system: RefCell::new(cosmic_text::FontSystem::new()),
            swash_cache: RefCell::new(cosmic_text::SwashCache::new()),
        }
    }
}

impl Default for SysLoadWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for SysLoadWidget {
    fn tick(&mut self) -> bool {
        if self.last_sample.elapsed() < SAMPLE_INTERVAL {
            return false;
        }
        self.last_sample = Instant::now();

        let cpu = parse_cpu_totals(&std::fs::read_to_string("/proc/stat").unwrap_or_default());
        let fraction = match (self.prev_cpu, cpu) {
            (Some(prev), Some(current)) => cpu_fraction(prev, current),
            // First sample (or an unreadable one): no interval to
            // difference over yet, so the honest column is zero.
            _ => 0.0,
        };
        if cpu.is_some() {
            self.prev_cpu = cpu;
        }
        self.cpu_history.pop_front();
        self.cpu_history.push_back(fraction);

        self.mem_used_frac = parse_mem_used_fraction(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default()).unwrap_or(0.0);
        true
    }

    fn render(&self, theme: &Theme, tile: u32) -> DecorationBuffer {
        let levels: Vec<u32> = self.cpu_history.iter().map(|f| quantize_level(*f, SYS_LOAD_ROWS)).collect();
        let mem_lit = quantize_level(self.mem_used_frac, SYS_LOAD_MEM_SEGMENTS);
        let mem_alert = self.mem_used_frac >= MEM_ALERT_FRACTION;
        let mut font_system = self.font_system.borrow_mut();
        let mut swash_cache = self.swash_cache.borrow_mut();
        render_sysload_tile(theme, &mut font_system, &mut swash_cache, tile, &levels, mem_lit, mem_alert)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_totals_sum_the_right_fields_and_exclude_guest_time() {
        // user nice system idle iowait irq softirq steal guest guest_nice
        let stat = "cpu  100 20 50 700 30 5 15 10 9999 9999\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
        let (busy, total) = parse_cpu_totals(stat).unwrap();
        assert_eq!(total, 100 + 20 + 50 + 700 + 30 + 5 + 15 + 10);
        assert_eq!(busy, total - 700 - 30);
    }

    #[test]
    fn cpu_totals_accept_a_short_pre_2_6_line() {
        // Ancient kernels emit only user/nice/system/idle.
        let (busy, total) = parse_cpu_totals("cpu 10 0 20 70\n").unwrap();
        assert_eq!((busy, total), (30, 100));
    }

    #[test]
    fn cpu_totals_reject_garbage() {
        assert_eq!(parse_cpu_totals(""), None);
        assert_eq!(parse_cpu_totals("intr 12345\n"), None);
        assert_eq!(parse_cpu_totals("cpu one two three four\n"), None);
    }

    #[test]
    fn mem_fraction_is_one_minus_available_over_total() {
        let meminfo = "MemTotal:       8000000 kB\nMemFree:         500000 kB\nMemAvailable:   2000000 kB\nBuffers:         100000 kB\n";
        let frac = parse_mem_used_fraction(meminfo).unwrap();
        assert!((frac - 0.75).abs() < 1e-6, "expected 0.75, got {frac}");
    }

    #[test]
    fn mem_fraction_needs_both_keys_and_a_nonzero_total() {
        assert_eq!(parse_mem_used_fraction("MemTotal: 8000000 kB\n"), None);
        assert_eq!(parse_mem_used_fraction("MemAvailable: 2000000 kB\n"), None);
        assert_eq!(parse_mem_used_fraction("MemTotal: 0 kB\nMemAvailable: 0 kB\n"), None);
    }

    #[test]
    fn cpu_fraction_differences_the_counters() {
        // 100 busy jiffies out of 400 elapsed.
        assert!((cpu_fraction((1000, 4000), (1100, 4400)) - 0.25).abs() < 1e-6);
    }

    #[test]
    fn cpu_fraction_survives_counter_resets() {
        assert_eq!(cpu_fraction((1100, 4400), (1000, 4000)), 0.0);
        assert_eq!(cpu_fraction((1000, 4000), (1000, 4000)), 0.0);
    }

    #[test]
    fn quantize_rounds_to_nearest_and_clamps() {
        assert_eq!(quantize_level(0.0, 10), 0);
        assert_eq!(quantize_level(0.04, 10), 0);
        assert_eq!(quantize_level(0.06, 10), 1);
        assert_eq!(quantize_level(0.95, 10), 10);
        assert_eq!(quantize_level(2.0, 10), 10);
        assert_eq!(quantize_level(-1.0, 10), 0);
    }
}
