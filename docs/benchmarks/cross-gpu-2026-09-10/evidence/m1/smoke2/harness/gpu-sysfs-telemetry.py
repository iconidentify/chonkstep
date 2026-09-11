#!/usr/bin/env python3
"""Read AMD GPU or Apple SMC sensors without changing system policy.

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


def apple_sample(hwmon_root=Path("/sys/class/hwmon")):
    start = time.monotonic_ns()
    metrics = {}
    # Apple SMC exposes system/rail power, not AGX GPU power or GPU clocks.
    # Keep their names distinct from AMD/NVIDIA board metrics.
    names = {"Total System Power": "system_power_w", "AC Input Power": "ac_input_power_w",
             "Heatpipe Power": "heatpipe_power_w", "3.8 V Rail Power": "rail_3_8v_power_w"}
    for hwmon in sorted(hwmon_root.glob("hwmon*")):
        if read(hwmon / "name") != "macsmc_hwmon":
            continue
        for label in hwmon.glob("power*_label"):
            key = names.get(read(label))
            try:
                value = float(read(label.with_name(label.name.replace("_label", "_input")))) / 1e6
            except (TypeError, ValueError):
                continue
            if key and math.isfinite(value):
                metrics[key] = value
    return {"schema": 1, "source": "apple-smc-sysfs", "device": "apple-smc-system",
            "start_monotonic_ns": start, "monotonic_ns": time.monotonic_ns(),
            "wall_ns": time.time_ns(), "metrics": metrics}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--metadata", action="store_true")
    parser.add_argument("--apple", action="store_true")
    args = parser.parse_args()
    cards = devices()
    if not cards and not args.apple:
        raise SystemExit("no AMD DRM devices found")
    if args.metadata:
        info = {"driver": "apple-drm/asahi", "telemetry": "Apple SMC system power; includes other processes and components",
                "unavailable": ["GPU utilization", "GPU clock", "GPU memory clock", "GPU board power", "GPU temperature"]}
        print(json.dumps(info if args.apple else [identity(device) for device in cards]))
        return
    deadline = time.monotonic()
    while True:
        for result in ([apple_sample()] if args.apple else [sample(device) for device in cards]):
            if not result["metrics"]:
                raise SystemExit("no readable telemetry sensors")
            print(json.dumps(result), flush=True)
        deadline = max(deadline + 1, time.monotonic())
        time.sleep(max(0, deadline - time.monotonic()))


if __name__ == "__main__":
    main()
