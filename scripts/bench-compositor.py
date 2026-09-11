#!/usr/bin/env python3
"""Measure isolated release compositor sessions under a headless Weston host.

Starts a private D-Bus session; no user's configuration, bus, or display is used.
The JSON records individual runs, executable hashes, environment and raw /proc
snapshots. These are nested software-renderer measurements, not DRM/KMS claims.
"""

import argparse
import contextlib
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import re
import shlex
import signal
import socket
import statistics
import struct
import subprocess
import sys
import tempfile
import time


def command_output(args):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=15)
    except (OSError, subprocess.TimeoutExpired) as error:
        # Machine-description helpers (e.g. rustc for an installed
        # binary) are optional; missing metadata is not missing Weston.
        return f"unavailable: {error}"
    return (result.stdout + result.stderr).strip()


def binary_metadata(path):
    version = command_output([str(path), "--version"])
    if "diagnostics: memory-profile" in version:
        raise ValueError(f"{path} is a memory-profile build; use an uninstrumented binary for timing")
    with path.open("rb") as executable:
        digest = hashlib.file_digest(executable, "sha256").hexdigest()
    return {"path": str(path), "version": version, "sha256": digest}


def stop(process):
    if process.poll() is None:
        process.terminate()
        try:
            process.wait(timeout=8)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
    # Catch children a compositor detached without reaping. Every process
    # launched here has its own session; never signal the caller's group.
    with contextlib.suppress(ProcessLookupError):
        os.killpg(process.pid, signal.SIGTERM)


@contextlib.contextmanager
def child(args, env, log_path):
    with log_path.open("wb") as log:
        process = subprocess.Popen(args, env=env, stdout=log, stderr=log,
                                   start_new_session=True)
        try:
            yield process
        finally:
            stop(process)


def wait_for_socket(runtime, process, timeout=45):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise RuntimeError(f"compositor exited with {process.returncode}")
        for path in runtime.glob("wayland-*"):
            if path.is_socket():
                return path
        time.sleep(0.001)
    raise TimeoutError("Wayland socket did not appear")


def receive_exact(stream, size):
    result = bytearray()
    while len(result) < size:
        part = stream.recv(size - len(result))
        if not part:
            raise RuntimeError("Wayland connection closed before sync callback")
        result.extend(part)
    return result


def wayland_roundtrip(path):
    # wl_display.sync(new_id=2), using the public core protocol. A bound
    # listening socket alone does not prove the event loop is running.
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(45)
        stream.connect(str(path))
        stream.sendall(struct.pack("=III", 1, (12 << 16), 2))
        while True:
            object_id, header = struct.unpack("=II", receive_exact(stream, 8))
            size, opcode = header >> 16, header & 0xFFFF
            if size < 8 or size > 65535 or size % 4:
                raise RuntimeError(f"invalid Wayland message size: {size}")
            payload = receive_exact(stream, size - 8)
            if object_id == 1 and opcode == 0:
                raise RuntimeError(f"Wayland protocol error: {payload!r}")
            if object_id == 2 and opcode == 0:
                if len(payload) != 4:
                    raise RuntimeError("invalid Wayland sync callback payload")
                return


def hyprland_roundtrip(runtime):
    paths = list(runtime.glob("hypr/*/.socket.sock"))
    if len(paths) != 1:
        raise RuntimeError(f"expected one isolated Hyprland IPC socket, found {len(paths)}")
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
        stream.settimeout(45)
        stream.connect(str(paths[0]))
        stream.sendall(b"j/version")
        reply = bytearray()
        while part := stream.recv(4096):
            reply.extend(part)
        return json.loads(reply)


class Door:
    def __init__(self, path):
        self.stream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.stream.settimeout(45)
        self.stream.connect(str(path))
        self.reader = self.stream.makefile("rb")

    def send(self, command):
        self.stream.sendall((command + "\n").encode())

    def query(self, command, multiple=False):
        self.send(command)
        lines = []
        while True:
            line = self.reader.readline().decode().strip()
            if not line:
                raise RuntimeError("test door closed")
            if line.startswith("err "):
                raise RuntimeError(line)
            if line == "done":
                return lines
            lines.append(line)
            if not multiple:
                return line

    def close(self):
        self.reader.close()
        self.stream.close()


def proc_snapshot(pid, output):
    output.mkdir()
    for name in ("stat", "status", "smaps_rollup", "maps", "smaps", "schedstat"):
        (output / name).write_text(Path(f"/proc/{pid}/{name}").read_text())
    rollup = {}
    for line in (output / "smaps_rollup").read_text().splitlines()[1:]:
        key, value = line.split(":", 1)
        rollup[key] = int(value.split()[0])
    status = dict(line.split(":", 1) for line in (output / "status").read_text().splitlines())
    # Pair CPU accounting with a timestamp at the read, so time spent
    # collecting mappings is not silently omitted from the denominator.
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    sampled_ns = time.monotonic_ns()
    switches = 0
    threads = list(Path(f"/proc/{pid}/task").iterdir())
    for task in threads:
        try:
            for line in (task / "status").read_text().splitlines():
                if line.startswith(("voluntary_ctxt_switches:", "nonvoluntary_ctxt_switches:")):
                    switches += int(line.split()[1])
        except FileNotFoundError:
            pass
    children = []
    for task in threads:
        with contextlib.suppress(FileNotFoundError):
            children.extend(int(value) for value in (task / "children").read_text().split())
    child_memory = []
    for child_pid in sorted(set(children)):
        try:
            snap = proc_snapshot(child_pid, output / f"child-{child_pid}")
            child_memory.append({"pid": child_pid, **snap})
        except (FileNotFoundError, ProcessLookupError):
            pass
    return {
        "rss_kib": rollup["Rss"], "pss_kib": rollup["Pss"],
        "private_kib": rollup["Private_Clean"] + rollup["Private_Dirty"],
        "swap_kib": rollup["Swap"],
        "peak_rss_kib": int(status["VmHWM"].split()[0]),
        "threads": len(threads), "fds": len(list(Path(f"/proc/{pid}/fd").iterdir())),
        "cpu_ticks": int(fields[11]) + int(fields[12]), "context_switches": switches,
        "sample_monotonic_ns": sampled_ns,
        "reaped_child_cpu_ticks": int(fields[13]) + int(fields[14]),
        "children": child_memory,
        "tree_pss_kib": rollup["Pss"] + sum(c["tree_pss_kib"] for c in child_memory),
    }


def isolated_environment(root, host_socket, software):
    env = os.environ.copy()
    for key in list(env):
        if key.startswith(("CHONKSTEP_", "HYPRLAND_", "AQ_", "WLR_")) or key in (
            "DISPLAY", "WAYLAND_SOCKET", "RUST_LOG", "WAYLAND_DEBUG", "LD_PRELOAD",
        ):
            env.pop(key)
    for key, directory in (("XDG_CONFIG_HOME", "config"), ("XDG_STATE_HOME", "state"),
                           ("XDG_CACHE_HOME", "cache"), ("XDG_DATA_HOME", "data"),
                           ("XDG_RUNTIME_DIR", "runtime")):
        path = root / directory
        path.mkdir(mode=0o700, parents=True)
        env[key] = str(path)
    # Prevent Omarchy's legacy state lookup falling back to the user's HOME.
    (root / "state/omarchy").mkdir()
    env.update(WAYLAND_DISPLAY=str(host_socket), WINIT_UNIX_BACKEND="wayland",
               CHONKSTEP_BACKEND="winit", CHONKSTEP_NO_APPEARANCE_PROPAGATION="1",
               CHONKSTEP_HYPRLAND_IPC="1", RUST_LOG="info", NO_COLOR="1",
               GSETTINGS_BACKEND="memory")
    if software:
        env.update(LIBGL_ALWAYS_SOFTWARE="1", GALLIUM_DRIVER="llvmpipe")
    return env


def measure(args, binary, label, host_socket, directory):
    directory.mkdir()
    env = isolated_environment(directory, host_socket, not args.hardware)
    config = directory / "config/chonkstep"
    config.mkdir()
    config_text = (
        'omarchy_shell = false\nomarchy_menu = false\nhyprland_config = false\n'
        'theme = "nextstep-classic"\nscale = 1\nrestore_session = false\n'
        f'show_dock = {str(args.dock).lower()}\n'
        f'decoration_style = "{args.decoration_styles.get(label, "windowmaker")}"\n'
    )
    (config / "config.toml").write_text(config_text)
    door_path = directory / "runtime/door.sock"
    env["CHONKSTEP_TEST_SOCKET"] = str(door_path)
    started = time.monotonic_ns()
    with child([str(binary)], env, directory / "compositor.log") as process, contextlib.ExitStack() as fixtures:
        socket_path = wait_for_socket(Path(env["XDG_RUNTIME_DIR"]), process)
        socket_ms = (time.monotonic_ns() - started) / 1e6
        wayland_roundtrip(socket_path)
        ready_ms = (time.monotonic_ns() - started) / 1e6
        door = Door(door_path)
        try:
            assert door.query("barrier") == "ok"
            frame_ms = (time.monotonic_ns() - started) / 1e6
            # Compatibility is part of this fixture. A socket-path bind
            # failure must not silently become a faster, reduced session.
            ipc_version = hyprland_roundtrip(Path(env["XDG_RUNTIME_DIR"]))
            fixture_ready_ms = None
            if args.decoration_workload:
                start_decoration_clients(fixtures, directory, env, socket_path, door)
                assert door.query("barrier") == "ok"
                fixture_ready_ms = (time.monotonic_ns() - started) / 1e6
            world = door.query("windows", multiple=True)
            (directory / "world.txt").write_text("\n".join(world) + "\n")
            time.sleep(args.settle_seconds)
            door.query("frame-stats")
            before = proc_snapshot(process.pid, directory / "before")
            time.sleep(args.idle_seconds)
            after = proc_snapshot(process.pid, directory / "after")
            frame_stats = door.query("frame-stats")
            interval = (after["sample_monotonic_ns"] - before["sample_monotonic_ns"]) / 1e9
            metrics = {
                "label": label, "decoration_style": args.decoration_styles.get(label, "windowmaker"),
                "socket_ms": socket_ms, "ready_ms": ready_ms,
                "frame_ms": frame_ms, "idle_seconds": interval,
                "cpu_percent": (after["cpu_ticks"] - before["cpu_ticks"]) /
                               os.sysconf("SC_CLK_TCK") / interval * 100,
                "context_switches_per_second":
                    (after["context_switches"] - before["context_switches"]) / interval,
                "before": before, "after": after, "frame_stats": frame_stats,
                "world": world,
                "hyprland_version": ipc_version,
            }
            if args.decoration_workload:
                metrics["three_client_ready_ms"] = fixture_ready_ms
                metrics["drag"] = measure_decoration_drag(door, process.pid, directory, args.drag_seconds)
            (directory / "sample.json").write_text(json.dumps(metrics, indent=2) + "\n")
            print(json.dumps({key: metrics[key] for key in (
                "label", "ready_ms", "frame_ms", "cpu_percent", "context_switches_per_second")}
                | {key: after[key] for key in ("rss_kib", "pss_kib", "tree_pss_kib")}), flush=True)
            return metrics
        finally:
            door.close()


def scene_records(lines, kind):
    """Parse the door's quoted fields without treating a client's title as code."""
    return [dict(field.split("=", 1) for field in shlex.split(line)[1:])
            for line in lines if line.startswith(kind + " ")]


def start_decoration_clients(fixtures, directory, env, socket_path, door):
    applications = set()
    processes = []
    # Map one framed client before launching the next. Concurrent startup made
    # app.2 sometimes the middle window and sometimes the rightmost one, changing
    # occlusion/damage during the same nominal drag workload between samples.
    for index in range(3):
        application = f"org.chonkstep.bench.{index}"
        applications.add(application)
        processes.append(fixtures.enter_context(child([
            "foot", "--config=/dev/null", f"--app-id={application}",
            f"--title=Decoration benchmark {index + 1}", "--window-size-pixels=320x180",
            "sh", "-c", "printf 'Decoration benchmark\\n'; exec sleep 86400",
        ], env | {"WAYLAND_DISPLAY": str(socket_path)}, directory / f"foot-{index}.log")))
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            if any(process.poll() is not None for process in processes):
                raise RuntimeError("decoration fixture terminal exited before mapping")
            world = door.query("windows", multiple=True)
            windows = scene_records(world, "window")
            mapped = {window["app"] for window in windows if window.get("mapped") == "true"}
            frames = {frame["window"] for frame in scene_records(world, "frame") if frame.get("mapped") == "true"}
            clients = [window for window in windows if window.get("app") in applications]
            if applications <= mapped and len(clients) == len(applications) and all(window["id"] in frames for window in clients):
                break
            time.sleep(0.02)
        else:
            raise TimeoutError("each decoration client must map with its frame before launching the next")
    ordered = sorted(clients, key=lambda window: window["app"])
    if not (len({window["y"] for window in ordered}) == 1
            and all(int(left["x"]) < int(right["x"]) for left, right in zip(ordered, ordered[1:]))):
        raise RuntimeError("decoration fixture must place clients in application order on one row")


def measure_decoration_drag(door, pid, directory, seconds):
    world = door.query("windows", multiple=True)
    clients = scene_records(world, "window")
    window = next(window for window in clients if window.get("app") == "org.chonkstep.bench.2")
    frame = next(frame for frame in scene_records(world, "frame") if frame["window"] == window["id"])
    start_x = int(frame["x"]) + int(frame["w"]) // 2
    start_y = int(frame["y"]) + (int(window["y"]) - int(frame["y"])) // 2
    door.send(f"motion {start_x} {start_y}")
    assert door.query("barrier") == "ok"
    door.send("button left press")
    assert door.query("barrier") == "ok"
    door.query("frame-stats")
    before = proc_snapshot(pid, directory / "drag-before")
    started = time.monotonic()
    count = max(1, round(seconds * 125))
    try:
        for sample in range(count):
            phase = sample % 250
            distance = 16 + (phase if phase <= 125 else 250 - phase)
            door.send(f"motion {start_x + distance} {start_y + distance // 2}")
            delay = started + (sample + 1) / 125 - time.monotonic()
            if delay > 0:
                time.sleep(delay)
    finally:
        door.send("button left release")
    elapsed = time.monotonic() - started
    assert door.query("barrier") == "ok"
    after = proc_snapshot(pid, directory / "drag-after")
    frame_stats = door.query("frame-stats")
    ending = door.query("windows", multiple=True)
    end_frame = next(part for part in scene_records(ending, "frame") if part["window"] == window["id"])
    if (end_frame["x"], end_frame["y"]) == (frame["x"], frame["y"]):
        raise RuntimeError("drag workload did not move its framed client")
    interval = (after["sample_monotonic_ns"] - before["sample_monotonic_ns"]) / 1e9
    return {
        "motion_samples": count, "elapsed_seconds": elapsed, "input_hz": count / elapsed,
        "cpu_percent": (after["cpu_ticks"] - before["cpu_ticks"]) / os.sysconf("SC_CLK_TCK") / interval * 100,
        "before": before, "after": after, "frame_stats": frame_stats,
        "initial_frame": frame, "final_frame": end_frame,
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--runs", type=int, default=7)
    parser.add_argument("--idle-seconds", type=float, default=10)
    parser.add_argument("--settle-seconds", type=float, default=3)
    parser.add_argument("--dock", action="store_true")
    parser.add_argument("--decoration-workload", action="store_true",
                        help="Map three real terminals; measure loaded idle and a 125 Hz titlebar drag")
    parser.add_argument("--decoration-style", action="append", default=[], metavar="LABEL=STYLE",
                        help="Select windowmaker or system7 for a binary label (default windowmaker)")
    parser.add_argument("--drag-seconds", type=float, default=5)
    parser.add_argument("--hardware", action="store_true", help="Use the host's default GPU driver")
    parser.add_argument("--host-renderer", choices=("pixman", "gl"), default="pixman",
                        help="Weston's headless renderer; gl permits hardware-nested EGL clients")
    args = parser.parse_args()
    if (args.runs < 1 or not math.isfinite(args.idle_seconds) or args.idle_seconds <= 0
            or not math.isfinite(args.settle_seconds) or args.settle_seconds < 0
            or not math.isfinite(args.drag_seconds) or args.drag_seconds <= 0):
        parser.error("runs/idle-seconds/drag-seconds must be positive and finite; settle-seconds must be nonnegative and finite")
    binaries = []
    for value in args.binary:
        label, separator, path = value.partition("=")
        if not separator or not re.fullmatch(r"[A-Za-z0-9_-]{1,24}", label):
            parser.error("each --binary needs LABEL=PATH; labels use 1–24 letters, digits, underscores or hyphens")
        if any(existing == label for existing, _ in binaries):
            parser.error(f"duplicate binary label: {label}")
        try:
            binary = Path(path).resolve(strict=True)
        except OSError as error:
            parser.error(f"cannot resolve binary {path!r}: {error}")
        if not binary.is_file() or not os.access(binary, os.X_OK):
            parser.error(f"binary is not an executable file: {binary}")
        binaries.append((label, binary))
    args.decoration_styles = {}
    for value in args.decoration_style:
        label, separator, style = value.partition("=")
        if (not separator or label not in dict(binaries) or style not in ("windowmaker", "system7")
                or label in args.decoration_styles):
            parser.error("--decoration-style needs a known, unique LABEL=windowmaker or LABEL=system7")
        args.decoration_styles[label] = style
    args.output = args.output.resolve()
    for label, _ in binaries:
        # The compatibility socket is longer than wayland-N. Reserve a
        # ten-digit PID so PID reuse/namespace size cannot change validity.
        signature = f"chonkstep_{int(time.time())}_2147483647"
        socket_path = args.output / f"{args.runs - 1:02d}-{label}/runtime/hypr/{signature}/.socket2.sock"
        if len(os.fsencode(socket_path)) >= 108:
            parser.error("output path is too long for a Linux Unix socket; use a shorter --output path")
    if os.environ.get("CHONKSTEP_BENCH_PRIVATE_BUS") != "1":
        result = subprocess.run(
            ["dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
            env=os.environ | {"CHONKSTEP_BENCH_PRIVATE_BUS": "1"},
        )
        raise SystemExit(result.returncode)
    try:
        executable_metadata = {label: binary_metadata(path) for label, path in binaries}
    except ValueError as error:
        parser.error(str(error))
    args.output.mkdir(parents=True, exist_ok=False)
    metadata = {
        "date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "uname": platform.uname()._asdict(), "cpu": command_output(["lscpu"]),
        "rustc": command_output(["rustc", "-Vv"]),
        "weston": command_output(["weston", "--version"]),
        "backend": "nested-winit-on-headless-weston",
        "renderer": "default-GPU" if args.hardware else "llvmpipe",
        "host_renderer": args.host_renderer,
        "host_shader_cache": "private per experiment; reused by that experiment's host",
        "page_cache": "warm/uncontrolled; no system cache dropping",
        "dock": args.dock, "runs_per_binary": args.runs,
        "decoration_workload": args.decoration_workload,
        "decoration_styles": {label: args.decoration_styles.get(label, "windowmaker") for label, _ in binaries},
        "drag_seconds": args.drag_seconds if args.decoration_workload else None,
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "load_average_at_start": os.getloadavg(),
        "renderer_environment": {key: value for key, value in os.environ.items()
                                 if key.startswith(("LIBGL_", "MESA_", "LP_", "GALLIUM_", "DRI_", "__EGL_", "GBM_"))},
        "binary": executable_metadata,
    }
    (args.output / "metadata.json").write_text(json.dumps(metadata, indent=2) + "\n")
    samples = []
    with tempfile.TemporaryDirectory(prefix="chonk-bench-host-") as temporary:
        runtime = Path(temporary)
        host_socket = runtime / "wayland-bench"
        host_env = os.environ | {"XDG_RUNTIME_DIR": str(runtime)}
        # The host is private too, including its GL shader cache when
        # --host-renderer=gl is used. Only process-independent kernel
        # filesystem caches remain uncontrolled.
        for key, name in (("XDG_CONFIG_HOME", "config"), ("XDG_STATE_HOME", "state"),
                          ("XDG_CACHE_HOME", "cache"), ("XDG_DATA_HOME", "data")):
            directory = runtime / name
            directory.mkdir(mode=0o700)
            host_env[key] = str(directory)
        with child(["weston", "--backend=headless-backend.so", "--socket=wayland-bench",
                    "--idle-time=0", "--width=2560", "--height=1600", f"--renderer={args.host_renderer}",
                    "--no-config"], host_env, args.output / "weston.log") as host:
            wait_for_socket(runtime, host)
            for run in range(args.runs):
                # Alternating order limits systematic temperature/cache bias.
                ordered = binaries if run % 2 == 0 else list(reversed(binaries))
                for label, binary in ordered:
                    directory = args.output / f"{run:02d}-{label}"
                    samples.append(measure(args, binary, label, host_socket, directory))
    summary = {}
    for label, _ in binaries:
        group = [sample for sample in samples if sample["label"] == label]
        metrics = {key: [sample[key] for sample in group]
                   for key in ("ready_ms", "frame_ms", "cpu_percent", "context_switches_per_second")}
        metrics.update({key: [sample["after"][key] for sample in group]
                        for key in ("rss_kib", "pss_kib", "private_kib", "tree_pss_kib", "fds", "threads")})
        if args.decoration_workload:
            metrics["three_client_ready_ms"] = [sample["three_client_ready_ms"] for sample in group]
            metrics["drag_cpu_percent"] = [sample["drag"]["cpu_percent"] for sample in group]
            metrics["drag_input_hz"] = [sample["drag"]["input_hz"] for sample in group]
        summary[label] = {key: {"median": statistics.median(values), "min": min(values),
                               "max": max(values), "samples": values}
                          for key, values in metrics.items()}
    (args.output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
    print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
