"""The scaling report must distinguish GPU execution from CPU submission."""

import importlib.util
from pathlib import Path
import unittest

PATH = Path(__file__).resolve().parents[1] / "bench-gpu-scaling.py"
SPEC = importlib.util.spec_from_file_location("gpu_scaling", PATH)
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)


class TimingReports(unittest.TestCase):
    def test_cases_distinguish_integer_native_pixels_from_fractional_fallback(self):
        self.assertEqual(BENCH.source_size(*BENCH.CASES["native"]), [5120, 2880])
        self.assertEqual(BENCH.source_size(*BENCH.CASES["legacy"]), [5120, 2880])
        self.assertEqual(BENCH.source_size(*BENCH.CASES["fractional"]), [6826, 3840])

    def test_cpu_intervals_cannot_be_reported_as_gpu_execution(self):
        report = 'native_stage output="eDP-1" stage=composition_submit calls=40 total_ns=800 max_ns=35\n'
        self.assertIsNone(BENCH.gpu_sample(report))
        self.assertIsNone(BENCH.gpu_sample("gpu_timer status=unavailable"))
        report += 'gpu_stage output="eDP-1" stage=composition_gpu samples=37 total_ns=123456 max_ns=6543 histogram_us_pow2=[0]\n'
        self.assertEqual(BENCH.gpu_sample(report), {"samples": 37, "total_ns": 123456, "max_ns": 6543})

    def test_frame_counters_do_not_parse_histogram_entries_as_scalar_work(self):
        report = 'frame-stats render_calls=12 render_us=150 render_hist=1,2,3 dispatch_calls=22'
        self.assertEqual(BENCH.frame_stats(report), {"render_calls": 12, "render_us": 150, "dispatch_calls": 22})


if __name__ == "__main__":
    unittest.main()
