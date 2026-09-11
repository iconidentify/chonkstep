"""Check real sysfs units and unavailable sensors without GPU privileges."""
import importlib.util
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location("gpu_sysfs", Path(__file__).parents[1] / "gpu-sysfs-telemetry.py")
telemetry = importlib.util.module_from_spec(spec)
spec.loader.exec_module(telemetry)


class SysfsTelemetryTests(unittest.TestCase):
    def test_apple_system_power_is_not_mislabeled_gpu_power(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            hwmon = root / "hwmon2"
            hwmon.mkdir()
            (hwmon / "name").write_text("macsmc_hwmon")
            (hwmon / "power1_label").write_text("Total System Power")
            (hwmon / "power1_input").write_text("7500000")
            self.assertEqual(telemetry.apple_sample(root)["metrics"], {"system_power_w": 7.5})

    def test_units_labels_and_missing_values(self):
        with tempfile.TemporaryDirectory() as temporary:
            device = Path(temporary)
            hwmon = device / "hwmon/hwmon3"
            hwmon.mkdir(parents=True)
            for name, value in {"gpu_busy_percent": "21", "mem_busy_percent": "N/A",
                                "mem_info_vram_used": str(256 * 1024 ** 2)}.items():
                (device / name).write_text(value)
            for stem, label, value in (("freq1", "sclk", "1200000000"), ("freq2", "mclk", "300000000"),
                                       ("power1", "PPT", "17500000"), ("temp1", "edge", "59000")):
                (hwmon / f"{stem}_label").write_text(label)
                (hwmon / f"{stem}_input").write_text(value)
            sample = telemetry.sample(device)
            self.assertEqual(sample["metrics"], {"utilization_percent": 21, "memory_mib": 256,
                "graphics_mhz": 1200, "memory_mhz": 300, "power_w": 17.5, "temperature_c": 59})
            self.assertEqual(sample["unavailable_paths"], [str(device / "mem_busy_percent")])
            self.assertLessEqual(sample["start_monotonic_ns"], sample["monotonic_ns"])

    def test_connectors_and_other_vendors_are_not_boards(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for name, vendor in (("card1", "0x1002"), ("card1-eDP-1", "0x1002"), ("card2", "0x10de")):
                (root / name / "device").mkdir(parents=True)
                (root / name / "device/vendor").write_text(vendor)
            self.assertEqual(telemetry.devices(root), [root / "card1/device"])
