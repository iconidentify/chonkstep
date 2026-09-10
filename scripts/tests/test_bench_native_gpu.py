"""Reject misleading native benchmark results before they reach a report."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native_bench", Path(__file__).parents[1] / "bench-native-gpu.py")
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


def feedback(stamps, flags=7, clock=1):
    return f"presentation clock_id={clock}\n" + "\n".join(
        f"presentation presented seconds={ns // 1_000_000_000} nanoseconds={ns % 1_000_000_000} refresh=6944444 sequence=0 flags={flags}"
        for ns in stamps)


class NativeMeasurementTests(unittest.TestCase):
    def test_modesets_choose_real_changes_on_60_hz_panel(self):
        current = {"width": 3840, "height": 2160, "refresh": 59.997}
        smaller = {"width": 3200, "height": 1800, "refresh": 59.982}
        self.assertEqual(bench.transition_modes([current, smaller], 3840, 2160, 60), (smaller, current))
        with self.assertRaisesRegex(ValueError, "two distinct"):
            bench.transition_modes([current], 3840, 2160, 60)
        fast = current | {"refresh": 144}
        self.assertEqual(bench.transition_modes([current, smaller, fast], 3840, 2160, 144), (current, fast))

    def test_uses_hardware_timestamps_and_excludes_warmup(self):
        result = bench.presentation_stats(feedback([1, 1_000_000_000, 1_006_944_444, 1_013_888_888, 2_000_000_000]),
                                          1_000_000_000, 1_020_000_000, 144)
        self.assertEqual(result["count"], 3)
        self.assertAlmostEqual(result["fps"], 144, places=4)
        self.assertEqual(result["estimated_missed_refreshes"], 0)
        self.assertEqual(result["zero_copy_count"], 0)

    def test_missed_refreshes_are_distinct_from_late_intervals(self):
        result = bench.presentation_stats(feedback([1_000_000_000, 1_006_944_444, 1_027_777_776], flags=15),
                                          0, 2_000_000_000, 144)
        self.assertEqual(result["intervals_over_1_5_refresh"], 1)
        self.assertEqual(result["estimated_missed_refreshes"], 2)
        self.assertEqual(result["zero_copy_count"], 3)

    def test_rejects_non_hardware_feedback(self):
        with self.assertRaisesRegex(ValueError, "native hardware"):
            bench.presentation_stats(feedback([1, 6_944_445], flags=1), 0, 20_000_000, 144)

    def test_rejects_other_clock_and_repeated_timestamps(self):
        with self.assertRaisesRegex(ValueError, "CLOCK_MONOTONIC"):
            bench.presentation_stats(feedback([1, 2], clock=0), 0, 10, 144)
        with self.assertRaisesRegex(ValueError, "did not increase"):
            bench.presentation_stats(feedback([1, 1]), 0, 10, 144)

    def test_rejects_missing_feedback(self):
        with self.assertRaisesRegex(ValueError, "insufficient"):
            bench.presentation_stats(feedback([1]), 0, 10, 144)

    def test_histogram_subtraction_omits_startup_and_respects_floor(self):
        before = 'native_stage output="DP-1" stage=planes calls=1 total_ns=200000 max_ns=200000 histogram_us_pow2=[0, 0, 0, 1]'
        after = 'native_stage output="DP-1" stage=planes calls=3 total_ns=205998 max_ns=200000 histogram_us_pow2=[0, 2, 0, 1]'
        result = bench.stage_stats(before, after)["planes"]
        self.assertEqual(result["count"], 2)
        self.assertEqual(result["p99_upper_us"], 3)
        self.assertEqual(result["mean_us"], 2.999)

    def test_histogram_overflow_has_no_finite_upper_bound(self):
        before = 'gpu_stage output="DP-1" stage=composition_gpu samples=0 total_ns=0 max_ns=0 histogram_us_pow2=[0, 0]'
        after = 'gpu_stage output="DP-1" stage=composition_gpu samples=1 total_ns=9000000 max_ns=9000000 histogram_us_pow2=[0, 1]'
        self.assertIsNone(bench.stage_stats(before, after)["composition_gpu"]["p99_upper_us"])

    def test_rejects_reset_counters_and_multiple_outputs(self):
        with self.assertRaisesRegex(ValueError, "backwards"):
            bench.delta({"queued": 10}, {"queued": 1})
        with self.assertRaisesRegex(ValueError, "multiple"):
            bench.counters('native_pipeline output="DP-1" queued=1\nnative_pipeline output="DP-2" queued=2', "native_pipeline")

    def test_flat_or_wrong_capture_cannot_pass_texture_verification(self):
        from PIL import Image
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "pixels.png"
            Image.new("RGB", (384, 216), (32, 64, 128)).save(path)
            with self.assertRaisesRegex(ValueError, "missing texture"):
                bench.verify_pixels(path, [384, 216], True)
            with self.assertRaisesRegex(ValueError, "wrong capture dimensions"):
                bench.verify_pixels(path, [3840, 2160], False)


if __name__ == "__main__":
    unittest.main()
