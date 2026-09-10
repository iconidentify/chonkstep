"""Check that telemetry and paired comparisons use the measured samples."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("native_analysis", Path(__file__).parents[1] / "analyze-native-gpu.py")
analysis = importlib.util.module_from_spec(spec)
spec.loader.exec_module(analysis)


class NativeAnalysisTests(unittest.TestCase):
    def test_sysfs_uses_monotonic_window_and_entire_read_interval(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "000").mkdir()
            (root / "000/gpu.jsonl").write_text("\n".join(json.dumps({"schema": 1, "source": "amdgpu-sysfs",
                "pci": "0000:01:00.0", "wall_ns": 999999999, "start_monotonic_ns": start * 10**9,
                "monotonic_ns": end * 10**9, "metrics": {"power_w": power}})
                for start, end, power in ((1.9, 2.1, 999), (2, 2.01, 50), (3, 3.01, 70), (3.9, 4.1, 999))))
            sample = {"directory": "000", "before": {"monotonic_ns": 10**9}, "after": {"monotonic_ns": 5 * 10**9}}
            self.assertEqual(analysis.board_metrics(root, sample, None)["0000:01:00.0"]["power_w"]["mean"], 60)

    def test_telemetry_excludes_warmup_capture_and_other_board(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "000").mkdir()
            (root / "000/gpu.csv").write_text("\n".join(
                f"1970/01/01 00:00:{second:02d}.000,0,{board},10,20,{power},1000,9000,40,100"
                for second, board, power in ((0, "display", 999), (2, "display", 50),
                                             (3, "display", 70), (3, "other", 200), (5, "display", 999))))
            sample = {"directory": "000", "before": {"wall_ns": 1_000_000_000}, "after": {"wall_ns": 5_000_000_000}}
            boards = analysis.board_metrics(root, sample, 0)
            self.assertEqual(boards["display"]["power_w"]["mean"], 60)
            self.assertEqual(boards["display"]["power_w"]["n"], 2)
            self.assertEqual(boards["other"]["power_w"]["mean"], 200)

    def test_old_clock_mapping_and_local_timezone(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / "000").mkdir()
            (root / "000/gpu.csv").write_text("1970/01/01 01:00:02.000,0,display,10,20,50,1000,9000,40,100\n")
            sample = {"directory": "000", "before": {"monotonic_ns": 11_000_000_000}, "after": {"monotonic_ns": 15_000_000_000}}
            self.assertIn("unavailable", analysis.board_metrics(root, sample, 3600))
            (root / "wall-monotonic-calibration.json").write_text(json.dumps({"wall_ns": 0, "monotonic_ns": 10_000_000_000}))
            self.assertEqual(analysis.board_metrics(root, sample, 3600)["display"]["power_w"]["mean"], 50)

    def test_pairs_by_run_instead_of_order_or_ratio_of_averages(self):
        samples = [{"case": "native", "policy": "default", "run": run, "label": label, "cpu": value}
                   for run, label, value in ((1, "final", 9), (0, "baseline", 10),
                                             (1, "baseline", 30), (0, "final", 5))]
        result = analysis.paired_changes(samples, "baseline", "final", "cpu")["native/default"]
        self.assertEqual(result["relative_change_percent"]["values"], [-50, -70])
        self.assertEqual(result["relative_change_percent"]["mean"], -60)
        low, high = result["paired_mean_bootstrap_95_percent"]
        self.assertLessEqual(low, -60)
        self.assertGreaterEqual(high, -60)

    def test_duplicate_cells_cannot_silently_replace_a_measurement(self):
        sample = {"case": "native", "policy": "default", "run": 0, "label": "baseline", "cpu": 10}
        with self.assertRaisesRegex(ValueError, "duplicate campaign cell"):
            analysis.paired_changes([sample, sample], "baseline", "final", "cpu")


if __name__ == "__main__":
    unittest.main()
