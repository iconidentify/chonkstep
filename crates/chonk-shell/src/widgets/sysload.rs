//! System load instrument: CPU and memory pressure on one LED screen,
//! rendered by `wm_theme::sysload`. This module is the data half only
//! — `/proc/stat` and `/proc/meminfo` parsing, delta bookkeeping, and
//! quantization policy — with every parse step a pure function over a
//! string, so fixtures can test the arithmetic without a live kernel.
//!
//! It no longer *reads* those two files. Both are [`Source::File`]s
//! declared to the dock, read on a sampler thread, and handed to
//! `update` as strings; the parsers below did not change, because they
//! never knew where the string came from. That is the whole migration:
//! two `read_to_string` calls that used to run on the compositor's
//! repaint path became two lines of declaration.
//!
//! CPU percent is the classic counter-delta: busy and total jiffies
//! from `/proc/stat`'s aggregate `cpu` line, differenced between
//! samples (the line is monotonic totals since boot, so a single
//! reading means nothing). Memory is `1 - MemAvailable/MemTotal` —
//! `MemAvailable` because the kernel already answers "how much could
//! be claimed without swapping" there, which is the pressure question,
//! where free-minus-buffers arithmetic famously is not.

use std::collections::VecDeque;
use std::path::PathBuf;

use wm_theme::sysload::{render_sysload_tile, SYS_LOAD_COLUMNS, SYS_LOAD_MEM_SEGMENTS, SYS_LOAD_ROWS};
use wm_theme::Theme;
use wm_theme_api::DecorationBuffer;

use super::{DockWidget, Samples, Source, SourceId, SAMPLE_INTERVAL};

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
    stat: SourceId,
    meminfo: SourceId,
    prev_cpu: Option<(u64, u64)>,
    /// Busy fractions `0.0..=1.0`, oldest first, always exactly
    /// `SYS_LOAD_COLUMNS` long so the renderer's column count and the
    /// history length cannot drift apart.
    cpu_history: VecDeque<f32>,
    mem_used_frac: f32,
}

impl SysLoadWidget {
    pub fn new() -> Self {
        Self {
            stat: SourceId::UNBOUND,
            meminfo: SourceId::UNBOUND,
            prev_cpu: None,
            cpu_history: VecDeque::from(vec![0.0; SYS_LOAD_COLUMNS]),
            mem_used_frac: 0.0,
        }
    }
}

impl Default for SysLoadWidget {
    fn default() -> Self {
        Self::new()
    }
}

impl DockWidget for SysLoadWidget {
    fn name(&self) -> &'static str {
        "LOAD"
    }

    fn sources(&self) -> Vec<Source> {
        vec![
            Source::File { path: PathBuf::from("/proc/stat"), interval: SAMPLE_INTERVAL },
            Source::File { path: PathBuf::from("/proc/meminfo"), interval: SAMPLE_INTERVAL },
        ]
    }

    fn bind(&mut self, ids: &[SourceId]) {
        self.stat = ids.first().copied().unwrap_or(SourceId::UNBOUND);
        self.meminfo = ids.get(1).copied().unwrap_or(SourceId::UNBOUND);
    }

    fn update(&mut self, samples: &Samples) -> bool {
        let mut changed = false;

        // One column per completed `/proc/stat` run, not per elapsed
        // second: the graph's x-axis is "samples", and folding on
        // freshness is what keeps that true when a sampler runs late.
        // No elapsed time is needed for the arithmetic — both jiffy
        // counters are differenced against each other, so the interval
        // cancels out (see `cpu_fraction`).
        if samples.fresh(self.stat) {
            let cpu = samples.text(self.stat).and_then(parse_cpu_totals);
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
            // The history shifted, so the graph did — true even when
            // every column holds the same value, because the column
            // that fell off the front may not have.
            changed = true;
        }

        if samples.fresh(self.meminfo) {
            // A kernel too old for `MemAvailable`, or an unreadable
            // read, reports zero rather than holding the last figure:
            // the memory bar is an absolute reading, and a stale one is
            // indistinguishable from a current one on the face.
            let fraction = samples.text(self.meminfo).and_then(parse_mem_used_fraction).unwrap_or(0.0);
            changed |= fraction != self.mem_used_frac;
            self.mem_used_frac = fraction;
        }

        changed
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        let levels: Vec<u32> = self.cpu_history.iter().map(|f| quantize_level(*f, SYS_LOAD_ROWS)).collect();
        let mem_lit = quantize_level(self.mem_used_frac, SYS_LOAD_MEM_SEGMENTS);
        let mem_alert = self.mem_used_frac >= MEM_ALERT_FRACTION;
        render_sysload_tile(theme, fonts, swash, tile, &levels, mem_lit, mem_alert)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::widgets::sampling::SampleBench;

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

    /// Two `/proc` strings in, one graph column and one memory bar out.
    /// The first `/proc/stat` reading has nothing to difference against
    /// and must contribute a zero column rather than a spike the size
    /// of uptime.
    #[test]
    fn update_folds_two_proc_readings_into_a_column_and_a_bar() {
        let mut bench = SampleBench::new();
        let stat = bench.text("cpu  1000 0 0 4000 0 0 0 0\n");
        let meminfo = bench.text("MemTotal: 8000000 kB\nMemAvailable: 2000000 kB\n");
        let mut widget = SysLoadWidget::new();
        widget.bind(&[stat, meminfo]);

        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.cpu_history.back().copied(), Some(0.0), "the first sample has no delta to report");
        assert_eq!(widget.prev_cpu, Some((1000, 5000)));
        assert!((widget.mem_used_frac - 0.75).abs() < 1e-6);

        // 100 busy jiffies out of 400 elapsed, exactly as
        // `cpu_fraction`'s own test measures it — the widget adds only
        // the bookkeeping between the two readings.
        bench.set_text(stat, "cpu  1100 0 0 4300 0 0 0 0\n");
        bench.set_text(meminfo, "MemTotal: 8000000 kB\nMemAvailable: 4000000 kB\n");
        assert!(widget.update(&bench.samples()));
        assert!((widget.cpu_history.back().copied().unwrap() - 0.25).abs() < 1e-6);
        assert!((widget.mem_used_frac - 0.5).abs() < 1e-6);
    }

    /// The common case at 60Hz against a 1Hz source: nothing is fresh,
    /// so nothing is folded and the dock is not asked to repaint. A
    /// widget that returned `true` here would repaint the whole dock
    /// sixty times a second for no reason.
    #[test]
    fn a_pass_with_no_fresh_reading_changes_nothing() {
        let mut bench = SampleBench::new();
        let stat = bench.text("cpu  1000 0 0 4000 0 0 0 0\n");
        let meminfo = bench.text("MemTotal: 8000000 kB\nMemAvailable: 2000000 kB\n");
        let mut widget = SysLoadWidget::new();
        widget.bind(&[stat, meminfo]);
        widget.update(&bench.samples());

        let history = widget.cpu_history.clone();
        bench.all_stale();
        for _ in 0..60 {
            assert!(!widget.update(&bench.samples()));
        }
        assert_eq!(widget.cpu_history, history, "a stale pass must not shift the graph");
    }

    /// An unreadable `/proc` file is a `None` reading, not a parse of
    /// the empty string somewhere upstream: the fold has to survive it
    /// with a zero column and a retained `prev_cpu`, so one bad read
    /// costs one column rather than restarting the counter baseline.
    #[test]
    fn an_absent_proc_reading_costs_one_column_and_keeps_the_baseline() {
        let mut bench = SampleBench::new();
        let stat = bench.text("cpu  1000 0 0 4000 0 0 0 0\n");
        let meminfo = bench.missing();
        let mut widget = SysLoadWidget::new();
        widget.bind(&[stat, meminfo]);
        widget.update(&bench.samples());
        assert_eq!(widget.mem_used_frac, 0.0, "no meminfo is an unlit bar, not a guess");

        bench.set_text(stat, "");
        assert!(widget.update(&bench.samples()));
        assert_eq!(widget.cpu_history.back().copied(), Some(0.0));
        assert_eq!(widget.prev_cpu, Some((1000, 5000)), "a garbled read must not become the new baseline");
    }
}
