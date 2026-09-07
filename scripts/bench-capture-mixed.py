#!/usr/bin/env python3
"""Isolated capture UI benchmark. Rendering barriers include nested frame wait.

First-open means first capture UI in each new process, with a fresh private
shader cache; kernel filesystem caches are uncontrolled. Timing builds and
memory-profile builds must be measured separately. All tools target the private
nested session. No exported capture is created and no default application opens. This variant
adds explicitly synthetic memory in a real private foot terminal, a frame-paced
Wayland animation, and an input observer. It must run inside bench-pressure.py.
Normal no-stressor benchmark results are a separate experiment.
"""
import argparse
import contextlib
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import time

BASE = Path(__file__).with_name("bench-compositor.py")
spec = importlib.util.spec_from_file_location("compositor_bench", BASE)
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)
MIXED_PATH = Path(__file__).with_name("mixed-capture-fixture.py")
mixed_spec = importlib.util.spec_from_file_location("mixed_capture_fixture", MIXED_PATH)
mixed = importlib.util.module_from_spec(mixed_spec)
mixed_spec.loader.exec_module(mixed)


def counters(line):
    return {k: ([int(n) for n in v.split(",")] if "," in v else int(v))
            for k, v in (word.split("=", 1) for word in line.split()[1:])}


def send(door, command):
    door.stream.sendall((command + "\n").encode())


def key(door, code, down):
    send(door, f"key {code} {'press' if down else 'release'}")


def tap(door, code):
    key(door, code, True)
    key(door, code, False)


def capture_open(door):
    for code in (125, 29, 42):
        key(door, code, True)
    start = time.monotonic_ns()
    key(door, 6, True)
    assert door.query("barrier") == "ok"
    duration = (time.monotonic_ns() - start) / 1e6
    for code in (6, 42, 29, 125):
        key(door, code, False)
    assert door.query("barrier") == "ok"
    return duration


def capture_close(door):
    tap(door, 1)
    assert door.query("barrier") == "ok"


def screenshot(root, artifact, name):
    output = artifact / f"{name}.png"
    marker = root / "state/chonkstep/screenshot"
    temporary = marker.with_name(marker.name + ".tmp")
    temporary.write_text(str(output))
    os.replace(temporary, marker)
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        if output.exists() and output.stat().st_size > 8:
            # PNG marker publication is not used as a timing measurement.
            return
        time.sleep(0.02)
    raise TimeoutError(f"diagnostic screenshot was not saved: {output}")


def phase(door, pid, artifact, name, action, memory):
    before = b.proc_snapshot(pid, artifact / f"{name}-before")
    allocation_before = counters(door.query("memory-stats")) if memory else None
    door.query("frame-stats")
    result = action()
    frame_stats = counters(door.query("frame-stats"))
    allocation_after = counters(door.query("memory-stats")) if memory else None
    after = b.proc_snapshot(pid, artifact / f"{name}-after")
    interval = (after["sample_monotonic_ns"] - before["sample_monotonic_ns"]) / 1e9
    data = {
        "name": name, "seconds": interval, "result": result,
        "cpu_percent": (after["cpu_ticks"] - before["cpu_ticks"]) /
                       os.sysconf("SC_CLK_TCK") / interval * 100,
        "context_switches_per_second":
            (after["context_switches"] - before["context_switches"]) / interval,
        "before": before, "after": after, "frame_stats": frame_stats,
    }
    if memory:
        data.update(allocation_before=allocation_before, allocation_after=allocation_after,
                    allocation_delta={key: allocation_after[key] - value
                                      for key, value in allocation_before.items()})
    return data


def motion(door, seconds, path, rate):
    start = time.monotonic()
    latencies = []
    for i in range(int(seconds * rate)):
        due = start + i / rate
        time.sleep(max(0, due - time.monotonic()))
        x, y = path(i)
        latencies.append((time.monotonic() - due) * 1000)
        send(door, f"motion {x:.3f} {y:.3f}")
    barrier_start = time.monotonic_ns()
    assert door.query("barrier") == "ok"
    return {"events": len(latencies), "requested_hz": rate,
            "wall_seconds": time.monotonic() - start,
            "sender_schedule_lateness_ms": latencies,
            "final_barrier_ms": (time.monotonic_ns() - barrier_start) / 1e6}


def measure(args, binary, label, host_socket, artifact):
    artifact.mkdir()
    with tempfile.TemporaryDirectory(prefix="ccb-") as temporary:
        root = Path(temporary)
        env = b.isolated_environment(root, host_socket, True)
        config = root / "config/chonkstep"
        config.mkdir()
        config_text = ('omarchy_shell = false\nomarchy_menu = false\nhyprland_config = false\n'
                       'desktop = "omarchy"\nomarchy_bar = false\ntheme = "nextstep-classic"\n'
                       'scale = 1\nrestore_session = false\nshow_dock = false\n')
        (config / "config.toml").write_text(config_text)
        (artifact / "config.toml").write_text(config_text)
        door_path = root / "runtime/door.sock"
        env["CHONKSTEP_TEST_SOCKET"] = str(door_path)
        started = time.monotonic_ns()
        with b.child([str(binary)], env, artifact / "compositor.log") as process:
            socket_path = b.wait_for_socket(root / "runtime", process)
            socket_ms = (time.monotonic_ns() - started) / 1e6
            b.wayland_roundtrip(socket_path)
            ready_ms = (time.monotonic_ns() - started) / 1e6
            door = b.Door(door_path)
            fixture = None
            try:
                assert door.query("barrier") == "ok"
                startup_frame_ms = (time.monotonic_ns() - started) / 1e6
                ipc = b.hyprland_roundtrip(root / "runtime")
                world = door.query("windows", multiple=True)
                (artifact / "world.txt").write_text("\n".join(world) + "\n")
                if "output 1280 800" not in world:
                    raise RuntimeError(f"benchmark fixture needs output 1280 800; got {world}")
                fixture = mixed.Fixture(b, sys.modules[__name__], args, root, artifact,
                                        env, socket_path, door).start()
                time.sleep(args.settle_seconds)
                phases = []
                def run(name, action):
                    data = fixture.phase(name, lambda: phase(
                        door, process.pid, artifact, name, action, args.memory))
                    phases.append(data)
                    print(json.dumps({"label": label, "run": artifact.name, "phase": name,
                                      "cpu_percent": data["cpu_percent"],
                                      "render_calls": data["frame_stats"]["render_calls"]}), flush=True)
                    return data
                first = run("first-open", lambda: capture_open(door))
                capture_close(door)
                def repeated():
                    times = []
                    for _ in range(args.warm_opens):
                        times.append(capture_open(door))
                        capture_close(door)
                    return times
                warm = run("warm-open", repeated)
                capture_open(door)
                # 1280x800 scale-one fixture, matching the existing startup benchmark.
                send(door, "motion 640 717")
                door.query("barrier")
                screenshot(root, artifact, "toolbar-hover")
                time.sleep(args.settle_seconds)
                run("overlay-idle", lambda: time.sleep(args.idle_seconds))
                run("toolbar-motion", lambda: motion(door, args.motion_seconds,
                    lambda i: (290 + ((i * 7) % 680), 717), args.motion_hz))
                send(door, "motion 100 100")
                send(door, "button left press")
                run("selection-draw", lambda: motion(door, args.motion_seconds,
                    lambda i: (430 + 210 * math.sin(i / 35), 320 + 140 * math.cos(i / 29)), args.motion_hz))
                send(door, "button left release")
                door.query("barrier")
                screenshot(root, artifact, "selection")
                # Deterministic new selection before resizing its bottom-right handle.
                capture_close(door)
                capture_open(door)
                send(door, "motion 100 100")
                send(door, "button left press")
                send(door, "motion 430 320")
                send(door, "button left release")
                door.query("barrier")
                send(door, "motion 430 320")
                send(door, "button left press")
                run("selection-resize", lambda: motion(door, args.motion_seconds,
                    lambda i: (430 + 100 * math.sin(i / 35), 320 + 80 * math.sin(i / 29)), args.motion_hz))
                send(door, "button left release")
                door.query("barrier")
                time.sleep(args.settle_seconds)
                run("selection-settled-idle", lambda: time.sleep(args.idle_seconds))
                capture_close(door)
                time.sleep(args.settle_seconds)
                run("closed-idle", lambda: time.sleep(args.idle_seconds))
                mixed_result = fixture.finish()
                sample = {"mixed": mixed_result, "label": label, "socket_ms": socket_ms, "ready_ms": ready_ms,
                          "startup_frame_ms": startup_frame_ms, "first_open_ms": first["result"],
                          "warm_open_ms": warm["result"], "phases": phases,
                          "hyprland_version": ipc, "world": world}
                (artifact / "sample.json").write_text(json.dumps(sample, indent=2) + "\n")
                return sample
            finally:
                if fixture is not None:
                    fixture.close()
                door.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warm-opens", type=int, default=10)
    parser.add_argument("--settle-seconds", type=float, default=2)
    parser.add_argument("--idle-seconds", type=float, default=10)
    parser.add_argument("--motion-seconds", type=float, default=5)
    parser.add_argument("--motion-hz", type=float, default=120)
    parser.add_argument("--memory", action="store_true")
    parser.add_argument("--animation-probe", type=Path, required=True)
    parser.add_argument("--input-probe", type=Path, required=True)
    parser.add_argument("--foot", type=Path, default=Path("/usr/bin/foot"))
    parser.add_argument("--payload-mib", type=int, required=True)
    parser.add_argument("--churn-mib", type=int, default=32)
    parser.add_argument("--worker-seconds", type=int, default=1200)
    parser.add_argument("--input-deadline-ms", type=float, default=500)
    parser.add_argument("--capture-deadline-ms", type=float, default=500)
    args = parser.parse_args()
    if not 1 <= args.payload_mib <= 1800 or not 1 <= args.churn_mib <= 64:
        parser.error("payload must be 1–1800 MiB; churn must be 1–64 MiB")
    if not 1 <= args.worker_seconds <= 1800:
        parser.error("worker duration must be 1–1800 seconds")
    for key in ("input_deadline_ms", "capture_deadline_ms"):
        if not math.isfinite(getattr(args, key)) or getattr(args, key) <= 0:
            parser.error(f"{key} must be positive and finite")
    for key in ("animation_probe", "input_probe", "foot"):
        value = getattr(args, key).resolve(strict=True)
        if not value.is_file() or not os.access(value, os.X_OK):
            parser.error(f"{key} must be an executable regular file")
        setattr(args, key, value)
    mixed.scope_limits()
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error("output directory must not already exist")
    for name in ("runs", "warm_opens", "idle_seconds", "motion_seconds", "motion_hz"):
        value = getattr(args, name)
        if not math.isfinite(value) or value <= 0:
            parser.error(f"{name} must be positive and finite")
    if args.settle_seconds < 0 or not math.isfinite(args.settle_seconds):
        parser.error("settle_seconds must be nonnegative and finite")
    if os.environ.get("CHONKSTEP_BENCH_PRIVATE_BUS") != "1":
        raise SystemExit(subprocess.run(["dbus-run-session", "--", sys.executable,
                         str(Path(__file__).resolve()), *sys.argv[1:]],
                         env=os.environ | {"CHONKSTEP_BENCH_PRIVATE_BUS": "1"}).returncode)
    binaries = []
    metadata = {}
    for value in args.binary:
        label, separator, path = value.partition("=")
        if not separator or not label.replace("-", "").replace("_", "").isalnum():
            parser.error("binary needs a safe LABEL=PATH")
        binary = Path(path).resolve(strict=True)
        version = b.command_output([str(binary), "--version"])
        if ("diagnostics: memory-profile" in version) != args.memory:
            parser.error("--memory must match whether executable has memory-profile diagnostics")
        binaries.append((label, binary))
        metadata[label] = {"path": str(binary), "version": version,
                           "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "metadata.json").write_text(json.dumps({
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "mixed_fixture_sha256": hashlib.sha256(MIXED_PATH.read_bytes()).hexdigest(),
        "workload": "synthetic bounded anonymous memory worker in private foot; frame-paced native animation; input observer",
        "client_binaries": {key: {"path": str(getattr(args, key)),
                             "sha256": hashlib.sha256(getattr(args, key).read_bytes()).hexdigest()}
                            for key in ("animation_probe", "input_probe", "foot")},
        "base_harness_sha256": hashlib.sha256(BASE.read_bytes()).hexdigest(),
        "binary": metadata, "memory_profile": args.memory,
        "fixture": "1280x800 scale1 nested winit on private headless Weston pixman; llvmpipe",
        "page_cache": "uncontrolled; fresh shader cache per process",
        "latency": "final shortcut key press through rendering barrier; includes nested frame scheduling",
        "allocation_scope": "whole-process Rust counters including test door, separate diagnostic builds only",
        "cpu": b.command_output(["lscpu"]), "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "load_average": os.getloadavg(), "args": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
        "renderer_environment": {k: v for k, v in os.environ.items()
                                 if k.startswith(("LIBGL_", "MESA_", "LP_", "GALLIUM_", "DRI_", "__EGL_", "GBM_"))},
    }, indent=2) + "\n")
    samples = []
    with tempfile.TemporaryDirectory(prefix="ccbhost-") as temporary:
        root = Path(temporary)
        env = b.isolated_environment(root, "unused", True)
        host_socket = root / "runtime/wayland-bench"
        with b.child(["weston", "--backend=headless-backend.so", "--socket=wayland-bench",
                      "--idle-time=0", "--width=2560", "--height=1600", "--renderer=pixman",
                      "--no-config"], env, args.output / "weston.log") as host:
            b.wait_for_socket(root / "runtime", host)
            for run in range(args.runs):
                for label, binary in (binaries if run % 2 == 0 else list(reversed(binaries))):
                    samples.append(measure(args, binary, label, host_socket,
                                           args.output / f"{run:02d}-{label}"))
    summary = {}
    for label, _ in binaries:
        group = [s for s in samples if s["label"] == label]
        metrics = {key: [s[key] for s in group] for key in ("startup_frame_ms", "first_open_ms")}
        metrics["warm_open_ms"] = [n for s in group for n in s["warm_open_ms"]]
        for phase_name in ("overlay-idle", "toolbar-motion", "selection-draw", "selection-resize", "selection-settled-idle", "closed-idle"):
            phases = [p for s in group for p in s["phases"] if p["name"] == phase_name]
            metrics[phase_name + "_cpu_percent"] = [p["cpu_percent"] for p in phases]
            metrics[phase_name + "_render_calls"] = [p["frame_stats"]["render_calls"] for p in phases]
            metrics[phase_name + "_pss_kib"] = [p["after"]["pss_kib"] for p in phases]
        summary[label] = {k: {"median": statistics.median(v), "min": min(v), "max": max(v), "samples": v}
                          for k, v in metrics.items()}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
