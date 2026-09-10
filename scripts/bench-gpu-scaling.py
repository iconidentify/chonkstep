#!/usr/bin/env python3
"""Measure real 5120x2880 nested GPU rendering with matched/mismatched buffers.

Weston's kiosk shell forces the unchanged baseline and candidate to the exact
same 5K framebuffer. This measures rendering, not physical KMS/plane scanout.
"""

import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import re
import socket
import statistics
import subprocess
import sys
import tempfile
import time

CASES = {"native": (2.0, 2), "fractional": (1.5, 2), "legacy": (2.0, 1)}


def source_size(output_scale, buffer_scale):
    fractional = abs(output_scale - round(output_scale)) > 1e-9
    factor = output_scale if fractional and buffer_scale == math.ceil(output_scale) else buffer_scale
    return [math.floor(pixels / factor + 0.5) * buffer_scale for pixels in (5120, 2880)]


spec = importlib.util.spec_from_file_location("compositor_bench", Path(__file__).with_name("bench-compositor.py"))
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


def ipc(runtime, command):
    paths = list(runtime.glob("hypr/*/.socket.sock"))
    if len(paths) != 1:
        raise RuntimeError(f"expected one private IPC socket: {paths}")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(20)
        stream.connect(str(paths[0]))
        stream.sendall(command.encode())
        return b"".join(iter(lambda: stream.recv(65536), b"")).decode()


def diagnostics(runtime):
    paths = list((runtime / "chonkstep").glob("control-*.sock"))
    if len(paths) != 1:
        raise RuntimeError(f"expected one private control socket: {paths}")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(20)
        stream.connect(str(paths[0]))
        stream.sendall(b'{"request":"debug","topic":"scene"}\n')
        with stream.makefile("rb") as reader:
            for _ in range(100):
                line = reader.readline()
                if not line:
                    break
                event = json.loads(line)
                if event.get("event") == "debug":
                    return event["data"]
    raise RuntimeError("no diagnostic response")


def gpu_sample(report):
    match = re.search(r"gpu_stage .*stage=composition_gpu samples=(\d+) total_ns=(\d+) max_ns=(\d+)", report)
    return dict(zip(("samples", "total_ns", "max_ns"), map(int, match.groups()))) if match else None


def frame_stats(line):
    return {key: int(value) for key, value in re.findall(r"(\w+)=(\d+)(?: |$)", line)}


def wait_for(what, predicate, timeout=30):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise TimeoutError(what)


def load_report():
    return {"load_average": os.getloadavg(), "gpus": bench.command_output([
        "nvidia-smi", "--query-gpu=uuid,name,utilization.gpu,memory.used,temperature.gpu,power.draw",
        "--format=csv,noheader,nounits"])}


def measure(args, label, binary, case, host_socket, directory):
    output_scale, scale = CASES[case]
    source = source_size(output_scale, scale)
    directory.mkdir()
    env = bench.isolated_environment(directory, host_socket, software=False)
    env["CHONKSTEP_GPU_TIMINGS"] = "1"
    config = directory / "config/chonkstep"
    config.mkdir()
    (config / "config.toml").write_text(
        'omarchy_shell = false\nomarchy_menu = false\nhyprland_config = false\n'
        f'theme = "nextstep-classic"\nscale = {output_scale}\nrestore_session = false\nshow_dock = false\n')
    runtime = Path(env["XDG_RUNTIME_DIR"])
    env["CHONKSTEP_TEST_SOCKET"] = str(runtime / "door.sock")
    with bench.child([str(binary)], env, directory / "compositor.log") as process:
        display = bench.wait_for_socket(runtime, process)
        bench.wayland_roundtrip(display)
        door = bench.Door(runtime / "door.sock")
        try:
            door.query("barrier")
            monitors = wait_for("the kiosk host to configure a 5K framebuffer", lambda: (
                value if (value := json.loads(ipc(runtime, "j/monitors")))
                and value[0]["width"] == 5120 and value[0]["height"] == 2880 else None))
            client_env = env | {"WAYLAND_DISPLAY": str(display), "CHONKSTEP_PROBE_BUFFER_SCALE": str(scale), "CHONKSTEP_PROBE_RENDERER": args.client_renderer}
            with bench.child([str(args.probe), "ScalingProbe", "scaling-probe", "animate-frame"],
                             client_env, directory / "client.log"):
                wait_for("the scaling client to map", lambda: json.loads(ipc(runtime, "j/clients")))
                # Actual seat input asks the client's fullscreen control.
                door.stream.sendall(b"key 33 press\nkey 33 release\n")
                door.query("barrier")
                wait_for("the client to accept fullscreen", lambda:
                         "answer granted: asked fullscreen=true, told fullscreen=true" in (directory / "client.log").read_text())
                wait_for("the requested source resolution", lambda:
                         f"buffer scale={scale} size={source[0]}x{source[1]}" in (directory / "client.log").read_text())
                time.sleep(args.settle_seconds)
                before_report = diagnostics(runtime)
                (directory / "diagnostics-before.txt").write_text(before_report)
                if args.client_renderer == "egl" and "surface_buffer " in before_report and "kind=Some(Dma)" not in before_report:
                    raise RuntimeError("EGL producer did not deliver a DMA-BUF; refusing a mislabeled sample")
                before_gpu = gpu_sample(before_report)
                if args.require_gpu_timing and before_gpu is None:
                    raise RuntimeError("requested GPU timings are unavailable; refusing a CPU-only result")
                before_load = load_report()
                door.query("frame-stats")
                before = bench.proc_snapshot(process.pid, directory / "before")
                time.sleep(args.seconds)
                after = bench.proc_snapshot(process.pid, directory / "after")
                frames = frame_stats(door.query("frame-stats"))
                after_report = diagnostics(runtime)
                (directory / "diagnostics-after.txt").write_text(after_report)
                after_gpu = gpu_sample(after_report)
                after_load = load_report()
                # Verify the delivered image after sampling, so capture does
                # not contaminate the rendering measurements.
                subprocess.run(["grim", str(directory / "pixels.png")], env=client_env, check=True,
                               timeout=30, capture_output=True)
                from PIL import Image
                with Image.open(directory / "pixels.png") as image:
                    if image.size != (5120, 2880):
                        raise RuntimeError(f"capture is not 5K: {image.size}")
                    for point in ((100, 100), (2560, 1440), (5000, 2700)):
                        if image.convert("RGB").getpixel(point) != (0x20, 0x40, 0x80):
                            raise RuntimeError(f"client pixels do not fill the physical framebuffer at {point}")
                interval = (after["sample_monotonic_ns"] - before["sample_monotonic_ns"]) / 1e9
                samples = after_gpu["samples"] - before_gpu["samples"] if before_gpu and after_gpu else 0
                sample = {
                    "label": label, "case": case, "buffer_scale": scale, "output_scale": output_scale,
                    "client_renderer": args.client_renderer, "framebuffer": [5120, 2880], "source_buffer": source,
                    "seconds": interval, "cpu_percent": (after["cpu_ticks"] - before["cpu_ticks"]) /
                    os.sysconf("SC_CLK_TCK") / interval * 100,
                    "render_frames_per_second": frames["render_calls"] / interval,
                    "render_cpu_us_per_frame": frames["render_us"] / max(1, frames["render_calls"]),
                    "composition_gpu_us_per_sample": (after_gpu["total_ns"] - before_gpu["total_ns"]) / samples / 1000 if samples else None,
                    "gpu_samples": samples, "gpu_before": before_gpu, "gpu_after": after_gpu,
                    "frames": frames, "before": before, "after": after,
                    "competing_load_before": before_load, "competing_load_after": after_load,
                    "monitors": monitors, "pixels_verified": True,
                }
                log = (directory / "compositor.log").read_text()
                renderers = re.findall(r'GL Renderer: "([^"]+)"', log)
                sample["renderers"] = renderers
                if not renderers or any("llvmpipe" in name.lower() or "softpipe" in name.lower() for name in renderers):
                    raise RuntimeError(f"a hardware renderer was required: {renderers}")
                (directory / "sample.json").write_text(json.dumps(sample, indent=2) + "\n")
                print(json.dumps({key: sample[key] for key in (
                    "label", "case", "buffer_scale", "cpu_percent", "render_frames_per_second", "composition_gpu_us_per_sample", "gpu_samples")}), flush=True)
                return sample
        finally:
            door.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    parser.add_argument("--client-renderer", choices=("shm", "egl"), default="egl")
    parser.add_argument("--probe", type=Path, default=Path("target/release/chonk-fullscreen-probe"))
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--seconds", type=float, default=15)
    parser.add_argument("--settle-seconds", type=float, default=3)
    parser.add_argument("--cases", nargs="+", choices=tuple(CASES), default=list(CASES))
    parser.add_argument("--require-gpu-timing", action="store_true")
    args = parser.parse_args()
    if args.runs < 1 or not math.isfinite(args.seconds) or args.seconds <= 0 or not math.isfinite(args.settle_seconds) or args.settle_seconds < 0:
        parser.error("invalid measurement duration/runs")
    args.output = args.output.resolve()
    args.probe = args.probe.resolve(strict=True)
    binaries = []
    for value in args.binary:
        label, separator, path = value.partition("=")
        if not separator or not re.fullmatch(r"[a-zA-Z0-9_-]{1,12}", label) or any(old == label for old, _ in binaries):
            parser.error("binary labels must be unique, with 1–12 letters, numbers, underscores or hyphens")
        binaries.append((label, Path(path).resolve(strict=True)))
        longest = args.output / f"{args.runs - 1:02d}-{label}-c2/runtime/hypr/chonkstep_9999999999_4294967295/.socket2.sock"
        if len(os.fsencode(longest)) >= 108:
            parser.error("output path is too long for private Unix sockets; use a short /tmp path")
    if os.environ.get("CHONKSTEP_BENCH_PRIVATE_BUS") != "1":
        raise SystemExit(subprocess.run(["dbus-run-session", "--", sys.executable, __file__, *sys.argv[1:]],
                         env=os.environ | {"CHONKSTEP_BENCH_PRIVATE_BUS": "1"}).returncode)
    args.output.mkdir(parents=True, exist_ok=False)
    metadata = {"date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "backend": "nested-winit / Weston GL headless kiosk / physical framebuffer 5120x2880",
                "limitation": "No physical KMS, plane scanout, or monitor latency measurement. Competing GPU load is recorded, not stopped.",
                "binary": {label: bench.binary_metadata(path) for label, path in binaries},
                "probe": {"path": str(args.probe), "sha256": hashlib.sha256(args.probe.read_bytes()).hexdigest()}, "runs": args.runs,
                "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "weston": bench.command_output(["weston", "--version"])}
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    samples = []
    with tempfile.TemporaryDirectory(prefix="cg5-host-") as temporary:
        root = Path(temporary)
        env = bench.isolated_environment(root / "host", root / "unused", software=False)
        runtime = Path(env["XDG_RUNTIME_DIR"])
        with bench.child(["weston", "--backend=headless-backend.so", "--renderer=gl", "--shell=kiosk-shell.so",
                          "--width=5120", "--height=2880", "--socket=wayland-5k", "--idle-time=0", "--no-config"],
                         env, args.output / "weston.log") as host:
            display = bench.wait_for_socket(runtime, host)
            for run in range(args.runs):
                pairs = [(label, binary, case) for case in args.cases for label, binary in binaries]
                if run % 2:
                    pairs.reverse()
                for label, binary, case in pairs:
                    samples.append(measure(args, label, binary, case, display, args.output / f"{run:02d}-{label}-c{args.cases.index(case)}"))
    summary = {}
    for label, _ in binaries:
        for case in args.cases:
            group = [sample for sample in samples if sample["label"] == label and sample["case"] == case]
            summary[f"{label}-{case}"] = {key: {"median": statistics.median(values), "min": min(values), "max": max(values), "samples": values}
                for key in ("cpu_percent", "render_frames_per_second", "render_cpu_us_per_frame", "composition_gpu_us_per_sample")
                if (values := [sample[key] for sample in group if sample[key] is not None])}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
