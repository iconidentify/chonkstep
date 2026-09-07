#!/usr/bin/env python3
"""Isolated capture UI benchmark. Rendering barriers include nested frame wait.

First-open means first capture UI in each new process, with a fresh private
shader cache; kernel filesystem caches are uncontrolled. Timing builds and
memory-profile builds must be measured separately. All tools target the private
nested session. No exported capture is created and no default application opens.
"""
import argparse
import hashlib
import importlib.util
import json
import math
import os
import re
from pathlib import Path
import statistics
import subprocess
import sys
import tempfile
import time

BASE = Path(__file__).with_name("bench-compositor.py")
spec = importlib.util.spec_from_file_location("compositor_bench", BASE)
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)


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
    pending = marker.with_suffix(".pending")
    # Empty markers request the default path: hide the truncate/write gap.
    pending.write_text(str(output))
    os.replace(pending, marker)
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
            try:
                assert door.query("barrier") == "ok"
                startup_frame_ms = (time.monotonic_ns() - started) / 1e6
                ipc = b.hyprland_roundtrip(root / "runtime")
                world = door.query("windows", multiple=True)
                (artifact / "world.txt").write_text("\n".join(world) + "\n")
                if "output 1280 800" not in world:
                    raise RuntimeError(f"benchmark fixture needs output 1280 800; got {world}")
                time.sleep(args.settle_seconds)
                phases = []
                def run(name, action):
                    data = phase(door, process.pid, artifact, name, action, args.memory)
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
                sample = {"label": label, "socket_ms": socket_ms, "ready_ms": ready_ms,
                          "startup_frame_ms": startup_frame_ms, "first_open_ms": first["result"],
                          "warm_open_ms": warm["result"], "phases": phases,
                          "hyprland_version": ipc, "world": world}
                (artifact / "sample.json").write_text(json.dumps(sample, indent=2) + "\n")
                return sample
            finally:
                door.close()


def parse_arguments(argv=None):
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
    args = parser.parse_args(argv)
    for name in ("runs", "warm_opens", "idle_seconds", "motion_seconds", "motion_hz"):
        value = getattr(args, name)
        if not math.isfinite(value) or value <= 0:
            parser.error(f"{name} must be positive and finite")
    if args.settle_seconds < 0 or not math.isfinite(args.settle_seconds):
        parser.error("settle_seconds must be nonnegative and finite")
    events = args.motion_seconds * args.motion_hz
    if not math.isfinite(events) or events < 1:
        parser.error("motion duration and rate must produce at least one finite event")
    binaries = []
    labels = set()
    for value in args.binary:
        label, separator, path = value.partition("=")
        if not separator or not re.fullmatch(r"[A-Za-z0-9_-]{1,24}", label):
            parser.error("binary needs LABEL=PATH; label is 1–24 ASCII letters, digits, underscores or hyphens")
        if label in labels:
            parser.error(f"duplicate binary label: {label}")
        labels.add(label)
        try:
            binary = Path(path).resolve(strict=True)
        except OSError as error:
            parser.error(f"cannot resolve binary {path!r}: {error}")
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"binary is not an executable file: {binary}")
        binaries.append((label, binary))
    args.output = args.output.resolve()
    if args.output.exists():
        parser.error("output directory must not already exist")
    return parser, args, binaries


def main():
    parser, args, binaries = parse_arguments()
    if os.environ.get("CHONKSTEP_BENCH_PRIVATE_BUS") != "1":
        raise SystemExit(subprocess.run(["dbus-run-session", "--", sys.executable,
                         str(Path(__file__).resolve()), *sys.argv[1:]],
                         env=os.environ | {"CHONKSTEP_BENCH_PRIVATE_BUS": "1"}).returncode)
    metadata = {}
    for label, binary in binaries:
        version = b.command_output([str(binary), "--version"])
        if ("diagnostics: memory-profile" in version) != args.memory:
            parser.error("--memory must match whether executable has memory-profile diagnostics")
        with binary.open("rb") as executable:
            digest = hashlib.file_digest(executable, "sha256").hexdigest()
        metadata[label] = {"path": str(binary), "version": version, "sha256": digest}
    args.output.mkdir(parents=True, exist_ok=False)
    (args.output / "metadata.json").write_text(json.dumps({
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "base_harness_sha256": hashlib.sha256(BASE.read_bytes()).hexdigest(),
        "binary": metadata, "memory_profile": args.memory,
        "fixture": "1280x800 scale1 nested winit on private headless Weston pixman; llvmpipe",
        "page_cache": "uncontrolled; fresh shader cache per process",
        "latency": "final shortcut key press through rendering barrier; includes nested frame scheduling",
        "allocation_scope": "whole-process Rust counters including test door, separate diagnostic builds only",
        "cpu": b.command_output(["lscpu"]), "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "load_average": os.getloadavg(), "args": vars(args) | {"output": str(args.output)},
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
