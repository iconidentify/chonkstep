//! Bounded native-pipeline telemetry. CPU submission intervals and page-flip
//! latency are labelled separately from GPU execution time.

use smithay::backend::renderer::element::{RenderElementPresentationState, RenderElementStates, RenderingReason};
use std::{cell::RefCell, fmt::Write, rc::Rc, time::Duration};

pub(crate) const STAGES: [&str; 10] = [
    "scene",
    "capture_chrome",
    "prepare",
    "planes",
    "composition_submit",
    "cpu_fence_wait",
    "kms_queue",
    "presentation_feedback",
    "render_attempt",
    "queue_to_vblank",
];

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Timing {
    pub calls: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub histogram: [u64; 20],
}

impl Timing {
    pub fn record(&mut self, duration: Duration) {
        let ns = duration.as_nanos().min(u64::MAX as u128) as u64;
        self.calls = self.calls.saturating_add(1);
        self.total_ns = self.total_ns.saturating_add(ns);
        self.max_ns = self.max_ns.max(ns);
        let us = duration.as_micros().max(1);
        let bucket = (u128::BITS - (us - 1).leading_zeros()).min(19) as usize;
        self.histogram[bucket] = self.histogram[bucket].saturating_add(1);
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Frame {
    pub sequence: u64,
    pub outcome: &'static str,
    pub flags: u32,
    pub elements: usize,
    pub primary: bool,
    pub overlays: usize,
    pub cursor: bool,
    pub composited: usize,
    pub zero_copy: usize,
    pub skipped: usize,
    pub unsupported_format: usize,
    pub scanout_failed: usize,
    pub unclassified: usize,
    pub reasons: [usize; RenderingReason::LABELS.len()],
    pub full_damage: bool,
    pub cpu_ns: [u64; 9],
}

impl Default for Frame {
    fn default() -> Self {
        Self {
            sequence: 0,
            outcome: "uninitialized",
            flags: 0,
            elements: 0,
            primary: false,
            overlays: 0,
            cursor: false,
            composited: 0,
            zero_copy: 0,
            skipped: 0,
            unsupported_format: 0,
            scanout_failed: 0,
            unclassified: 0,
            reasons: [0; RenderingReason::LABELS.len()],
            full_damage: false,
            cpu_ns: [0; 9],
        }
    }
}

impl Frame {
    pub fn elements(&mut self, states: &RenderElementStates) {
        for state in states.states.values() {
            match state.presentation_state {
                RenderElementPresentationState::ZeroCopy => self.zero_copy += 1,
                RenderElementPresentationState::Skipped => self.skipped += 1,
                RenderElementPresentationState::Rendering { reason } => {
                    self.composited += 1;
                    if let Some(reason) = reason {
                        self.reasons[reason as usize] += 1;
                    }
                    match reason {
                        Some(RenderingReason::FormatUnsupported) => self.unsupported_format += 1,
                        Some(RenderingReason::ScanoutFailed) => self.scanout_failed += 1,
                        None => self.unclassified += 1,
                        _ => {}
                    }
                }
            }
        }
    }
}

#[derive(Default)]
pub(crate) struct Stats {
    pub stages: [Timing; 10],
    pub passes: u64,
    /// Off, pending flip, clean, inactive device, waiting for frame deadline.
    pub skips: [u64; 5],
    pub attempts: u64,
    pub queued: u64,
    pub empty: u64,
    pub render_failed: u64,
    pub queue_failed: u64,
    pub flips: u64,
    pub late_presentations: u64,
    pub primary: u64,
    pub overlay: u64,
    pub composited: u64,
    pub reasons: [u64; RenderingReason::LABELS.len()],
    pub current: Frame,
    history: [Frame; 32],
    history_len: usize,
    history_next: usize,
}

impl Stats {
    pub fn begin(&mut self, flags: u32) {
        self.attempts = self.attempts.saturating_add(1);
        self.current = Frame {
            sequence: self.attempts,
            outcome: "preparing",
            flags,
            ..Default::default()
        };
    }

    pub fn stage(&mut self, index: usize, duration: Duration) {
        self.stages[index].record(duration);
        if index < self.current.cpu_ns.len() {
            self.current.cpu_ns[index] = duration.as_nanos().min(u64::MAX as u128) as u64;
        }
    }

    pub fn finish(&mut self, outcome: &'static str, elapsed: Duration) {
        self.current.outcome = outcome;
        self.stage(8, elapsed);
        for (total, count) in self.reasons.iter_mut().zip(self.current.reasons) {
            *total = total.saturating_add(count as u64);
        }
        match outcome {
            "queued" => {
                self.queued = self.queued.saturating_add(1);
                if self.current.primary {
                    self.primary = self.primary.saturating_add(1);
                } else {
                    self.composited = self.composited.saturating_add(1);
                }
                if self.current.overlays > 0 {
                    self.overlay = self.overlay.saturating_add(1);
                }
            }
            "empty" => self.empty = self.empty.saturating_add(1),
            "render_failed" => self.render_failed = self.render_failed.saturating_add(1),
            "queue_failed" => self.queue_failed = self.queue_failed.saturating_add(1),
            _ => {}
        }
        self.history[self.history_next] = self.current;
        self.history_next = (self.history_next + 1) % self.history.len();
        self.history_len = (self.history_len + 1).min(self.history.len());
    }
}

/// Shared with the backend's read-only diagnostic snapshot. The output owns
/// updates; the control socket formats strings only on an explicit request.
#[derive(Clone)]
pub(crate) struct OutputStats {
    pub name: String,
    pub stats: Rc<RefCell<Stats>>,
}

impl OutputStats {
    pub fn new(name: String) -> Self {
        Self {
            name,
            stats: Rc::new(RefCell::new(Stats::default())),
        }
    }

    pub fn describe(&self, report: &mut String) {
        let stats = self.stats.borrow();
        let _ = writeln!(report, "native_pipeline output={:?} passes={} skips_off={} skips_flip={} skips_clean={} skips_inactive={} skips_deadline={} attempts={} queued={} empty={} render_failed={} queue_failed={} flips={} primary={} overlay={} composited={} late_presentations={}",
            self.name, stats.passes, stats.skips[0], stats.skips[1], stats.skips[2], stats.skips[3], stats.skips[4],
            stats.attempts, stats.queued, stats.empty, stats.render_failed, stats.queue_failed, stats.flips,
            stats.primary, stats.overlay, stats.composited, stats.late_presentations);
        for (name, timing) in STAGES.iter().zip(&stats.stages) {
            let _ = writeln!(
                report,
                "native_stage output={:?} stage={} calls={} total_ns={} max_ns={} histogram_us_pow2={:?}",
                self.name, name, timing.calls, timing.total_ns, timing.max_ns, timing.histogram
            );
        }
        let first = (stats.history_next + stats.history.len() - stats.history_len) % stats.history.len();
        for offset in 0..stats.history_len {
            let frame = stats.history[(first + offset) % stats.history.len()];
            let _ = writeln!(
                report,
                "native_frame output={:?} sequence={} policy_flags={} {:?}",
                self.name, frame.sequence, frame.flags, frame
            );
        }
        let _ = writeln!(report, "native_reason_order {:?}", RenderingReason::LABELS);
        let _ = writeln!(
            report,
            "native_reason_totals output={:?} {:?}",
            self.name, stats.reasons
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_sessions_keep_every_attempt_accounted_for_and_bound_history() {
        let output = OutputStats::new("DP-1".into());
        {
            let mut stats = output.stats.borrow_mut();
            for index in 0..10_000 {
                stats.begin(9);
                stats.current.primary = index % 3 == 0;
                stats.current.overlays = usize::from(index % 5 == 0);
                stats.current.reasons[RenderingReason::PrimaryFormatMismatch as usize] = 1;
                stats.finish(
                    ["queued", "empty", "render_failed", "queue_failed"][index % 4],
                    Duration::from_micros(20),
                );
            }
            assert_eq!(
                stats.attempts,
                stats.queued + stats.empty + stats.render_failed + stats.queue_failed
            );
            assert_eq!(stats.queued, stats.primary + stats.composited);
            assert_eq!(stats.history_len, 32);
            assert_eq!(stats.reasons[RenderingReason::PrimaryFormatMismatch as usize], 10_000);
        }
        let mut report = String::new();
        output.describe(&mut report);
        assert_eq!(
            report.lines().filter(|line| line.starts_with("native_frame ")).count(),
            32
        );
        assert!(report.contains("sequence=9969 "));
        assert!(report.contains("sequence=10000 "));
        assert!(!report.contains("sequence=9968 "));
    }

    #[test]
    fn histogram_edges_and_saturation_do_not_wrap() {
        let mut timing = Timing::default();
        for micros in [0, 1, 2, 3, 4, 5, 1_000_000] {
            timing.record(Duration::from_micros(micros));
        }
        assert_eq!(timing.histogram.iter().sum::<u64>(), timing.calls);
        assert_eq!(&timing.histogram[..4], &[2, 1, 2, 1]);
        assert_eq!(timing.histogram[19], 1);
        timing.total_ns = u64::MAX;
        timing.record(Duration::from_secs(60));
        assert_eq!(timing.total_ns, u64::MAX);
    }
}
