#!/usr/bin/env python3
"""Read AMD GPU sensors without changing clocks, power policy or permissions.

JSONL timestamps bracket each collection with CLOCK_MONOTONIC. Missing sensors
remain missing, never zero. Power is the driver's PPT sensor, not wall power.
"""
import argparse
import json
import math
from pathlib import Path
import time


def read(path):
    try:
        return path.read_text().strip()
    except OSError:
        return None


def devices(root=Path("/sys/class/drm")):
    return [card / "device" for card in sorted(root.glob("card[0-9]*"))
            if card.name[4:].isdigit() and read(card / "device/vendor") == "0x1002"]


def identity(device):
    driver = device / "driver"
    return {"pci": device.resolve().name, "vendor": read(device / "vendor"),
            "device": read(device / "device"), "subsystem_vendor": read(device / "subsystem_vendor"),
            "subsystem_device": read(device / "subsystem_device"),
            "driver": driver.resolve().name if driver.exists() else None,
            "power_policy": read(device / "power_dpm_force_performance_level"),
            "power_scope": "amdgpu hwmon PPT sensor; not system or wall power"}


def sample(device):
    start = time.monotonic_ns()
    metrics, missing = {}, []

    def metric(key, path, divisor=1):
        try:
            value = float(read(path)) / divisor
            if not math.isfinite(value):
                raise ValueError("nonfinite sensor")
            metrics[key] = value
        except (TypeError, ValueError):
            missing.append(str(path))

    for key, name, divisor in (("utilization_percent", "gpu_busy_percent", 1),
                               ("memory_utilization_percent", "mem_busy_percent", 1),
                               ("memory_mib", "mem_info_vram_used", 1024 ** 2)):
        metric(key, device / name, divisor)
    sensors = {"sclk": ("graphics_mhz", 1e6), "mclk": ("memory_mhz", 1e6),
               "PPT": ("power_w", 1e6), "edge": ("temperature_c", 1000)}
    for hwmon in sorted((device / "hwmon").glob("hwmon*")):
        for label in sorted(hwmon.glob("*_label")):
            if (name := read(label)) in sensors:
                key, divisor = sensors[name]
                metric(key, label.with_name(label.name.replace("_label", "_input")), divisor)
    return {"schema": 1, "source": "amdgpu-sysfs", "pci": device.resolve().name,
            "start_monotonic_ns": start, "monotonic_ns": time.monotonic_ns(),
            "wall_ns": time.time_ns(), "metrics": metrics, "unavailable_paths": missing}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", action="store_true")
    args = parser.parse_args()
    cards = devices()
    if not cards:
        raise SystemExit("no AMD DRM devices found")
    if args.metadata:
        print(json.dumps([identity(device) for device in cards]))
        return
    deadline = time.monotonic()
    while True:
        for device in cards:
            result = sample(device)
            if not result["metrics"]:
                raise SystemExit(f"no readable GPU sensors: {device}")
            print(json.dumps(result), flush=True)
        deadline = max(deadline + 1, time.monotonic())
        time.sleep(max(0, deadline - time.monotonic()))


if __name__ == "__main__":
    main()
