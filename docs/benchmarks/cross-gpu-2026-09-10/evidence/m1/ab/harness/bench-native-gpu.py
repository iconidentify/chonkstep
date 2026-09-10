#!/usr/bin/env python3
"""Compare native DRM sessions on an explicitly selected, available test VT.

Requires passwordless sudo, systemd/logind, a physical monitor, grim, Pillow,
and native-drm-info (see its C source for the build command). The test VT is
taken over; the return VT is restored after every sample and on failure.
No installed binary or user configuration is replaced. Use only on a machine
whose display is authorized for testing. Raw logs and hardware timestamps are
retained, including failed runs; experimental flags never imply plane usage.
"""

import argparse
import contextlib
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import pwd
import re
import shlex
import shutil
import signal
import statistics
import subprocess
import sys
import time

spec = importlib.util.spec_from_file_location("scaling_bench", Path(__file__).with_name("bench-gpu-scaling.py"))
gpu = importlib.util.module_from_spec(spec)
spec.loader.exec_module(gpu)

POLICIES = {
    "default": {},
    "overlay": {"CHONKSTEP_EXPERIMENTAL_OVERLAY_SCANOUT": "1"},
    "primary-any": {"CHONKSTEP_EXPERIMENTAL_PRIMARY_SCANOUT_ANY": "1"},
    "both": {"CHONKSTEP_EXPERIMENTAL_OVERLAY_SCANOUT": "1", "CHONKSTEP_EXPERIMENTAL_PRIMARY_SCANOUT_ANY": "1"},
    "composite": {"CHONKSTEP_NO_DIRECT_SCANOUT": "1"},
}
CASES = {"native": (2.0, 2), "direct": (2.0, 2), "fractional": (1.5, 2), "windowed": (2.0, 2), "idle": (2.0, 2)}


def write_json(path, data):
    path.write_text(json.dumps(data, indent=2) + "\n")


def telemetry_config(device):
    sys_device = Path("/sys/class/drm") / device.name / "device"
    if (sys_device / "driver").resolve().name == "apple-drm":
        command = [sys.executable, str(Path(__file__).with_name("gpu-sysfs-telemetry.py")), "--apple"]
        return command, "gpu.jsonl", json.loads(checked(command + ["--metadata"]))
    vendor = (sys_device / "vendor").read_text().strip()
    if vendor == "0x1002":
        command = [sys.executable, str(Path(__file__).with_name("gpu-sysfs-telemetry.py"))]
        return command, "gpu.jsonl", json.loads(checked(command + ["--metadata"]))
    if vendor == "0x10de":
        return (["nvidia-smi", "--query-gpu=timestamp,index,uuid,utilization.gpu,utilization.memory,power.draw,clocks.gr,clocks.mem,temperature.gpu,memory.used",
                 "--format=csv,noheader,nounits", "-lms", "1000"], "gpu.csv",
                checked(["nvidia-smi", "--query-gpu=index,pci.bus_id,name,driver_version", "--format=csv,noheader"]).strip())
    raise ValueError(f"no telemetry implementation for GPU vendor {vendor}")


def transition_modes(modes, width, height, hz):
    """Select two distinct advertised modes, preferring refresh-only changes."""
    current = min((m for m in modes if (m["width"], m["height"]) == (width, height)
                   and abs(m["refresh"] - hz) < 0.1), key=lambda m: abs(m["refresh"] - hz))
    alternate = [m for m in modes if (m["width"], m["height"]) != (width, height)
                 or abs(m["refresh"] - current["refresh"]) >= 0.1]
    if not alternate:
        raise ValueError("modeset stress requires two distinct advertised modes")
    chosen = min(alternate, key=lambda m: ((m["width"], m["height"]) != (width, height),
                 -(m["width"] * m["height"]), abs(m["refresh"] - 60)))
    return chosen, current


def checked(command, **kwargs):
    result = subprocess.run(command, capture_output=True, text=True, timeout=30, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{shlex.join(command)} failed ({result.returncode}): {result.stderr.strip()} {result.stdout.strip()}")
    return result.stdout


def snapshot(pid):
    # stat remains readable when logind's privileged device acquisition makes
    # smaps inaccessible. utime/stime cover every compositor thread, not clients.
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    return {"monotonic_ns": time.monotonic_ns(), "wall_ns": time.time_ns(), "cpu_ticks": int(fields[11]) + int(fields[12]),
            "rss_bytes": int(fields[21]) * os.sysconf("SC_PAGE_SIZE")}


def percentile(values, percent):
    if not values:
        return None
    ordered = sorted(values)
    return ordered[max(0, math.ceil(len(ordered) * percent / 100) - 1)]


def presentation_stats(log, start_ns, end_ns, hz):
    clocks = re.findall(r"^presentation clock_id=(\d+)$", log, re.M)
    if clocks != [str(time.CLOCK_MONOTONIC)]:
        raise ValueError(f"expected CLOCK_MONOTONIC presentation feedback, got {clocks}")
    events = []
    for line in log.splitlines():
        if line.startswith("presentation presented "):
            item = gpu.frame_stats(line)
            stamp = item["seconds"] * 1_000_000_000 + item["nanoseconds"]
            if start_ns <= stamp <= end_ns:
                events.append((stamp, item))
    if len(events) < 2:
        raise ValueError("insufficient actual presentation feedback")
    intervals = [(b[0] - a[0]) / 1e6 for a, b in zip(events, events[1:])]
    if min(intervals) <= 0:
        raise ValueError("presentation timestamps did not increase")
    # Vsync + hardware clock + hardware completion are required for native
    # measurements. Zero-copy is a separate fourth flag, never assumed.
    if any(item["flags"] & 7 != 7 for _, item in events):
        raise ValueError("feedback does not assert native hardware presentation")
    expected_ms = 1000 / hz
    return {"count": len(events), "fps": (len(events) - 1) * 1e9 / (events[-1][0] - events[0][0]),
            "interval_ms_p50": percentile(intervals, 50), "interval_ms_p95": percentile(intervals, 95),
            "interval_ms_p99": percentile(intervals, 99), "interval_ms_p99_9": percentile(intervals, 99.9), "interval_ms_max": max(intervals),
            "intervals_over_1_5_refresh": sum(value > 1.5 * expected_ms for value in intervals),
            "estimated_missed_refreshes": sum(max(0, round(value / expected_ms) - 1) for value in intervals),
            "zero_copy_count": sum(bool(item["flags"] & 8) for _, item in events),
            "advertised_refresh_ns": sorted({item["refresh"] for _, item in events})}


def counters(report, prefix):
    lines = [line for line in report.splitlines() if line.startswith(prefix + " ")]
    if len(lines) > 1:
        raise ValueError(f"single-output benchmark got multiple {prefix} lines")
    return gpu.frame_stats(lines[0]) if lines else {}


def delta(before, after):
    result = {key: value - before[key] for key, value in after.items() if key in before}
    if any(value < 0 for value in result.values()):
        raise ValueError("cumulative counter moved backwards")
    return result


def stage_stats(before, after):
    def parse(report):
        result = {}
        for line in report.splitlines():
            match = re.match(r"(?:native|gpu)_stage .*stage=(\w+) (?:calls|samples)=(\d+) total_ns=(\d+).*histogram_us_pow2=(\[[^\]]+\])", line)
            if match:
                name, calls, ns, histogram = match.groups()
                result[name] = (int(calls), int(ns), json.loads(histogram))
        return result
    left, right = parse(before), parse(after)
    result = {}
    for name, (calls, ns, hist) in right.items():
        if name not in left:
            continue
        old_calls, old_ns, old_hist = left[name]
        counts = [new - old for new, old in zip(hist, old_hist)]
        count = calls - old_calls
        if not count:
            continue
        if count < 0 or min(counts) < 0 or sum(counts) != count:
            raise ValueError("invalid stage histogram delta")
        quantiles = {}
        for p in (50, 95, 99):
            total = 0
            for bucket, amount in enumerate(counts):
                total += amount
                if total >= math.ceil(count * p / 100):
                    # The last bucket is unbounded overflow, not an upper bound.
                    # Instrumentation bins floor(microseconds), so add one
                    # microsecond to preserve a strict duration upper bound.
                    quantiles[f"p{p}_upper_us"] = 2 ** bucket + 1 if bucket < len(counts) - 1 else None
                    break
        result[name] = {"count": count, "mean_us": (ns - old_ns) / count / 1000,
                        "histogram": counts, **quantiles}
    return result


def verify_pixels(path, size, fullscreen, content_rect=None, frame_marker=False):
    from PIL import Image, ImageStat
    with Image.open(path) as image:
        if list(image.size) != size:
            raise ValueError(f"wrong capture dimensions: {image.size}")
        rgb = image.convert("RGB")
        if fullscreen or content_rect is not None:
            if content_rect is not None:
                x, y, w, h = content_rect
                if min(x, y) < 0 or min(w, h) <= 0 or x + w > size[0] or y + h > size[1]:
                    raise ValueError(f"invalid client capture rectangle: {content_rect}")
                rgb = rgb.crop((x, y, x + w, y + h))
            w, h = rgb.size
            for point in ((w // 100, h // 100), (w // 2, h // 2), (w * 99 // 100, h * 99 // 100)):
                if rgb.getpixel(point) != (32, 64, 128):
                    raise ValueError(f"incorrect GPU calibration pixel at {point}: {rgb.getpixel(point)}")
            spread = ImageStat.Stat(rgb.crop((w // 10, h // 10, w // 10 + 128, h // 10 + 128))).stddev
            if min(spread) < 10:
                raise ValueError(f"missing texture: {spread}")
        marker = {}
        if frame_marker:
            if not fullscreen and content_rect is None:
                raise ValueError("frame marker requires a known client rectangle")
            red, green, blue = rgb.getpixel((rgb.width // 4, rgb.height // 2))
            marker["frame_marker"] = (red << 16) | (green << 8) | blue
        return {"size": list(image.size), "fullscreen_pattern_verified": fullscreen,
                "client_pattern_verified": fullscreen or content_rect is not None,
                "sha256": hashlib.sha256(path.read_bytes()).hexdigest(), **marker}


def stress_session(args, runtime, door, client_env, root):
    """Correctness operations after timing; never included in a render sample."""
    evidence = root / "stress"
    evidence.mkdir()
    results = []

    def clients():
        return json.loads(gpu.ipc(runtime, "j/clients"))

    def dispatch(command):
        reply = gpu.ipc(runtime, "/dispatch " + command)
        if reply != "ok":
            raise ValueError(f"{command}: {reply}")

    def key(code):
        door.stream.sendall(f"key {code} press\nkey {code} release\n".encode())
        door.query("barrier")

    def callbacks():
        values = re.findall(r"^frame callback=(\d+)$", (root / "client.log").read_text(), re.M)
        return int(values[-1]) if values else 0

    def progressing():
        before = callbacks()
        gpu.wait_for("frame callbacks resumed", lambda: callbacks() >= before + 20, timeout=10)

    def snapshot_phase(label, fullscreen=True, size=None):
        progressing()
        report = gpu.diagnostics(runtime)
        (evidence / f"{label}.txt").write_text(report)
        path = evidence / f"{label}.png"
        checked(["grim", str(path)], env=client_env)
        mapped = clients()
        rect = (*mapped[0]["at"], *mapped[0]["size"]) if label.startswith("windowed-") else None
        # A popup has its own overlap test; other phases must show a newer
        # client frame, not merely the same valid texture from before a pause.
        witness = args.frame_marker and label != "popup"
        pixels = verify_pixels(path, size or [args.width, args.height], fullscreen, rect, witness)
        if witness:
            submitted = [int(value) for value in re.findall(r"^GPU frame_marker=(\d+)$", (root / "client.log").read_text(), re.M)]
            previous = [result["pixels"]["frame_marker"] for result in results
                        if "frame_marker" in result.get("pixels", {})]
            if not submitted or not (previous[-1] if previous else 0) < pixels["frame_marker"] <= submitted[-1]:
                raise ValueError(f"stale or invalid GPU frame marker: {pixels['frame_marker']}")
        if label == "popup":
            from PIL import Image
            with Image.open(path) as image:
                # The fixture's popup is solid RGB(25,230,58). Its Wayland
                # position is scaled from the parent's coordinates, so find
                # the pixels rather than assuming the renderer's scale model.
                green = sum(count for count, color in image.convert("RGB").getcolors(args.width * args.height)
                            if color == (25, 230, 58))
                if green < 120 * 80:
                    raise ValueError(f"popup pixels are missing: {green}")
                pixels["popup_pixels"] = green
        result = {"phase": label, "pixels": pixels,
                  "clients": mapped, "native": counters(report, "native_pipeline"),
                  "readback": counters(report, "readback")}
        results.append(result)
        write_json(evidence / "results.json", results)

    snapshot_phase("initial")
    for cycle in range(5):
        key(33)
        gpu.wait_for("leave fullscreen", lambda: clients() and clients()[0]["fullscreen"] == 0)
        snapshot_phase(f"windowed-{cycle}", fullscreen=False)
        key(33)
        gpu.wait_for("enter fullscreen", lambda: clients() and clients()[0]["fullscreen"] != 0)
        # Focus repair/resize may change the pointer target. Re-enter the
        # fullscreen client so its ordinary pointer request hides the cursor.
        door.stream.sendall(f"motion {args.width // 2} {args.height // 2}\n".encode())
        door.query("barrier")
        snapshot_phase(f"fullscreen-{cycle}")

    # An actual xdg popup overlaps the fullscreen buffer, exercising plane
    # reassignment and retained client-buffer lifetimes during capture.
    key(25)
    gpu.wait_for("native popup", lambda: "popup painted" in (root / "client.log").read_text())
    snapshot_phase("popup", fullscreen=False)
    key(25)
    gpu.wait_for("popup closes", lambda: "popup closed" in (root / "client.log").read_text())
    snapshot_phase("popup-closed")

    exclusive = clients()[0]["workspace"]["id"]
    normal = next(item["id"] for item in json.loads(gpu.ipc(runtime, "j/workspaces")) if item["id"] != exclusive)
    dispatch(f"workspace {normal}")
    time.sleep(0.6)
    parked = callbacks()
    time.sleep(0.4)
    if callbacks() != parked:
        raise ValueError("hidden fullscreen client continued receiving frame callbacks")
    results.append({"phase": "parked-space", "callbacks_stopped": True})
    dispatch(f"workspace {exclusive}")
    snapshot_phase("space-return")

    for cycle in range(3):
        dispatch("dpms off " + args.connector)
        gpu.wait_for("output powered off", lambda: not json.loads(gpu.ipc(runtime, "j/monitors"))[0]["dpmsStatus"])
        time.sleep(0.3)
        paused = callbacks()
        time.sleep(0.3)
        if callbacks() != paused:
            raise ValueError("powered-off output continued frame callbacks")
        dispatch("dpms on " + args.connector)
        snapshot_phase(f"dpms-return-{cycle}")

    expected_vt_queue_failures = 0
    for cycle in range(3):
        before_vt = counters(gpu.diagnostics(runtime), "native_pipeline")
        log_offset = (root / "compositor.log").stat().st_size
        checked(["sudo", "-n", "chvt", str(args.return_vt)])
        time.sleep(0.6)
        paused = callbacks()
        time.sleep(0.3)
        if callbacks() != paused:
            raise ValueError("inactive native session continued frame callbacks")
        checked(["sudo", "-n", "chvt", str(args.test_vt)])
        snapshot_phase(f"vt-return-{cycle}")
        after_vt = counters(gpu.diagnostics(runtime), "native_pipeline")
        failed = after_vt["queue_failed"] - before_vt["queue_failed"]
        with (root / "compositor.log").open("rb") as log:
            log.seek(log_offset)
            segment = log.read().decode()
        errors = [line for line in segment.splitlines() if "queueing the page flip failed" in line]
        # DRM master can be revoked between the active-seat check and the
        # atomic ioctl. EACCES during this deliberate VT handoff is expected;
        # validate recovery and retain its count, rather than hiding errors or
        # requiring an impossible atomic check+ioctl across logind's revoke.
        if failed != len(errors) or any("code: 13, kind: PermissionDenied" not in line for line in errors):
            raise ValueError(f"unexpected VT queue failure: {errors}")
        expected_vt_queue_failures += failed

    if args.skip_modesets:
        results.append({"phase": "modesets-skipped", "reason": args.skip_modesets})
        modes = []
    else:
        outputs = json.loads(checked(["wlr-randr", "--json"], env=client_env))
        output = next(output for output in outputs if output["name"] == args.connector)
        modes = transition_modes(output["modes"], args.width, args.height, args.hz)
    for selected in modes:
        width, height, hz = selected["width"], selected["height"], selected["refresh"]
        mode = f"{width}x{height}@{hz:.6f}Hz"
        checked(["wlr-randr", "--output", args.connector, "--mode", mode], env=client_env)
        gpu.wait_for("changed physical mode", lambda: any(c["active"] and (c["width"], c["height"]) == (width, height)
                     and abs(c["hz"] - hz) < 0.1 for c in json.loads(checked([str(args.drm_info), str(args.device)]))["crtcs"]))
        snapshot_phase(f"mode-{mode}", size=[width, height])

    final_report = gpu.diagnostics(runtime)
    final_native = counters(final_report, "native_pipeline")
    if final_native.get("render_failed") or final_native.get("queue_failed") != expected_vt_queue_failures:
        raise ValueError(f"native pipeline errors during transitions: {final_native}")
    readback = counters(final_report, "readback")
    if readback.get("synchronous_fallbacks") or readback.get("queued", 0) < 20:
        raise ValueError(f"asynchronous readback did not service the captures: {readback}")
    (evidence / "final.txt").write_text(final_report)
    results.append({"phase": "complete", "passed": True, "native": final_native, "readback": readback,
                    "expected_vt_permission_retries": expected_vt_queue_failures})
    write_json(evidence / "results.json", results)
    print(json.dumps({"stress_passed": True, "phases": len(results), "directory": str(evidence)}), flush=True)


@contextlib.contextmanager
def native_session(args, binary, root, scale, policy):
    for sub in ("config/chonkstep", "config/hypr", "state/omarchy", "cache", "data", "runtime"):
        (root / sub).mkdir(parents=True, mode=0o700, exist_ok=True)
    (root / "config/chonkstep/config.toml").write_text(
        'interaction_mode = "mac"\nomarchy_shell = false\nomarchy_menu = false\nhyprland_config = true\n'
        'show_dock = false\nrestore_session = false\ntheme = "nextstep-classic"\n')
    (root / "config/hypr/hyprland.conf").write_text(
        f"monitor = {args.connector},{args.width}x{args.height}@{args.hz},0x0,{scale}\n")
    runtime = root / "runtime"
    env = {key: value for key, value in os.environ.items() if key in ("HOME", "USER", "LOGNAME", "PATH", "LANG")}
    env.update({"XDG_CONFIG_HOME": str(root / "config"), "XDG_STATE_HOME": str(root / "state"),
                "XDG_CACHE_HOME": str(root / "cache"), "XDG_DATA_HOME": str(root / "data"), "XDG_RUNTIME_DIR": str(runtime),
                "CHONKSTEP_BACKEND": "drm", "CHONKSTEP_DRM_DEVICE": str(args.device),
                "CHONKSTEP_GPU_TIMINGS": "1" if args.gpu_timings else "0",
                "CHONKSTEP_NO_VRR": "1" if args.vrr == "off" else "0",
                "CHONKSTEP_NO_APPEARANCE_PROPAGATION": "1", "CHONKSTEP_TEST_SOCKET": str(runtime / "door.sock"),
                "RUST_LOG": args.log_filter, "NO_COLOR": "1", "GSETTINGS_BACKEND": "memory", **POLICIES[policy]})
    if args.render_device:
        env["CHONKSTEP_RENDER_DEVICE"] = str(args.render_device)
    inner = f'printf "%s\\n" "$$" > {shlex.quote(str(root / "pid"))}; exec {shlex.quote(str(binary))}'
    launcher = root / "launch.sh"
    launcher.write_text('#!/bin/bash\nset -eu\n'
        f'printf "%s\\n" "$XDG_SESSION_ID" > {shlex.quote(str(root / "session"))}\n'
        + f'while [ ! -e {shlex.quote(str(root / "go"))} ]; do sleep 0.02; done\n'
        + "exec /usr/bin/env -i " + " ".join(shlex.quote(f"{key}={value}") for key, value in env.items())
        + ' XDG_SESSION_ID="$XDG_SESSION_ID" XDG_SESSION_TYPE=wayland XDG_SEAT=seat0'
        + f' XDG_VTNR={args.test_vt} dbus-run-session -- /bin/sh -c {shlex.quote(inner)}'
        + f' > {shlex.quote(str(root / "compositor.log"))} 2>&1\n')
    launcher.chmod(0o700)
    unit = f"chonk-gpu-bench-{os.getpid()}"
    started = False
    try:
        checked(["sudo", "-n", "systemd-run", f"--unit={unit}", "--collect", "--service-type=exec",
                 f"--uid={pwd.getpwuid(os.getuid()).pw_name}", "-p", "PAMName=login", "-p", f"TTYPath=/dev/tty{args.test_vt}",
                 "-p", "StandardInput=tty", "-p", "TTYReset=yes", "-p", "TTYVHangup=yes",
                 "-p", f"RuntimeMaxSec={max(300, math.ceil(args.seconds + args.settle_seconds + 150))}",
                 "-p", f"ExecStopPost=+/usr/bin/chvt {args.return_vt}", str(launcher)])
        started = True
        checked(["sudo", "-n", "chvt", str(args.test_vt)])
        gpu.wait_for("test PAM session", lambda: (root / "session").exists())
        session = (root / "session").read_text().strip()
        checked(["sudo", "-n", "loginctl", "activate", session])
        gpu.wait_for("active test VT", lambda: Path("/sys/class/tty/tty0/active").read_text().strip() == f"tty{args.test_vt}")
        # Allow the previous compositor to acknowledge logind's asynchronous
        # pause and drop DRM master before the new one probes the device.
        time.sleep(0.5)
        (root / "go").touch()
        def socket_ready():
            if (root / "pid").exists() and not Path(f"/proc/{(root / 'pid').read_text().strip()}").exists():
                raise RuntimeError(f"native compositor exited; inspect {root / 'compositor.log'}")
            return (runtime / "wayland-1").is_socket()
        gpu.wait_for("native Wayland socket", socket_ready, timeout=45)
        gpu.bench.wayland_roundtrip(runtime / "wayland-1")
        gpu.wait_for("native test door", lambda: (runtime / "door.sock").is_socket())
        yield runtime, int((root / "pid").read_text()), env
    finally:
        if started:
            # PAM moves children into a session scope. Stop that specific test
            # session as well as the wrapper; never signal by executable name.
            session_path = root / "session"
            if session_path.exists() and re.fullmatch(r"[0-9]+\n?", session_path.read_text()):
                session = session_path.read_text().strip()
                info = subprocess.run(["loginctl", "show-session", session, "-p", "VTNr", "--value"], capture_output=True, text=True, timeout=10)
                if info.stdout.strip() == str(args.test_vt):
                    subprocess.run(["sudo", "-n", "loginctl", "terminate-session", session], capture_output=True, timeout=15)
                    # The private dbus-daemon may ignore TERM after its wrapper
                    # exits. Escalation is restricted to this recorded test scope.
                    deadline = time.monotonic() + 3
                    while time.monotonic() < deadline:
                        status = subprocess.run(["loginctl", "show-session", session], capture_output=True, timeout=10)
                        if status.returncode:
                            break
                        time.sleep(0.1)
                    else:
                        subprocess.run(["sudo", "-n", "loginctl", "kill-session", "--signal=SIGKILL", session], capture_output=True, timeout=15)
            subprocess.run(["sudo", "-n", "systemctl", "stop", unit], capture_output=True, timeout=30)
            checked(["sudo", "-n", "chvt", str(args.return_vt)])


def measure(args, label, binary, case, policy, root):
    scale, buffer_scale = CASES[case]
    with native_session(args, binary, root, scale, policy) as (runtime, pid, env):
        def monitor_ready():
            monitors = json.loads(gpu.ipc(runtime, "j/monitors"))
            if len(monitors) == 1 and monitors[0]["name"] == args.connector and monitors[0]["width"] == args.width and monitors[0]["height"] == args.height and abs(monitors[0]["refreshRate"] - args.hz) < 0.1 and monitors[0]["scale"] == scale:
                return monitors
            return None
        monitors = gpu.wait_for("requested monitor mode", monitor_ready)
        kms = json.loads(checked([str(args.drm_info), str(args.device)]))
        active = [c for c in kms["crtcs"] if c["active"]]
        if len(active) != 1 or [active[0]["width"], active[0]["height"]] != [args.width, args.height] or abs(active[0]["hz"] - args.hz) > 0.1:
            raise ValueError(f"KMS did not apply the requested physical mode: {active}")
        write_json(root / "kms-before.json", kms)
        client_env = env | {"WAYLAND_DISPLAY": str(runtime / "wayland-1"), "CHONKSTEP_PROBE_BUFFER_SCALE": str(buffer_scale),
                            "CHONKSTEP_PROBE_RENDERER": "egl", "CHONKSTEP_PROBE_GPU_PATTERN": "texture", "CHONKSTEP_PROBE_PRESENTATION": "1", "CHONKSTEP_PROBE_OPAQUE": "1"}
        if case == "direct":
            client_env["CHONKSTEP_PROBE_HIDE_CURSOR"] = "1"
        if args.frame_marker:
            client_env["CHONKSTEP_PROBE_GPU_FRAME_MARKER"] = "1"
        with contextlib.ExitStack() as stack:
            door = gpu.bench.Door(runtime / "door.sock")
            stack.callback(door.close)
            if case != "idle":
                client = stack.enter_context(gpu.bench.child([str(args.probe), "NativeGpuProbe", "native-gpu-probe", "animate-frame"], client_env, root / "client.log"))
                gpu.wait_for("GPU client", lambda: json.loads(gpu.ipc(runtime, "j/clients")))
                if case != "windowed":
                    door.stream.sendall(b"key 33 press\nkey 33 release\n")
                    door.query("barrier")
                    gpu.wait_for("client fullscreen confirmation", lambda: "answer granted: asked fullscreen=true, told fullscreen=true" in (root / "client.log").read_text())
                    factor = scale if case == "fractional" else buffer_scale
                    source = [math.floor(value / factor + 0.5) * buffer_scale for value in (args.width, args.height)]
                    gpu.wait_for("expected source buffer", lambda: f"buffer scale={buffer_scale} size={source[0]}x{source[1]}" in (root / "client.log").read_text())
                if case == "direct":
                    door.stream.sendall(f"motion {args.width // 2} {args.height // 2}\n".encode())
                    door.query("barrier")
                    gpu.wait_for("client hidden cursor", lambda: "cursor hidden" in (root / "client.log").read_text())
                gpu.wait_for("GPU presentation", lambda: "presentation presented" in (root / "client.log").read_text())
            time.sleep(args.settle_seconds)
            before_report = gpu.diagnostics(runtime)
            (root / "diagnostics-before.txt").write_text(before_report)
            if case != "idle" and "surface_buffer " in before_report and "kind=Some(Dma)" not in before_report:
                raise ValueError("EGL fixture did not deliver DMA-BUFs")
            # Use the same read-only, once-per-second collector for each A/B
            # sample. Its board totals include the producer and other apps.
            telemetry_command, telemetry_file, _ = telemetry_config(args.device)
            telemetry = stack.enter_context(gpu.bench.child(telemetry_command, os.environ.copy(), root / telemetry_file))
            door.query("frame-stats")
            before = snapshot(pid)
            start_ns = before["monotonic_ns"]
            captures = []
            end_at = time.monotonic() + args.seconds
            next_capture = time.monotonic() + 1
            while time.monotonic() < end_at:
                if args.capture_every and time.monotonic() >= next_capture:
                    stamp = time.monotonic_ns()
                    path = root / "capture-during.png"
                    checked(["grim", str(path)], env=client_env)
                    captures.append({"monotonic_ns": stamp, "latency_ms": (time.monotonic_ns() - stamp) / 1e6,
                                     "verification": verify_pixels(path, [args.width, args.height], case not in ("idle", "windowed"))})
                    next_capture += args.capture_every
                else:
                    time.sleep(min(0.05, max(0, end_at - time.monotonic())))
            after = snapshot(pid)
            if telemetry.poll() is not None or not (root / telemetry_file).stat().st_size:
                raise ValueError(f"GPU telemetry failed; inspect {telemetry_file}")
            gpu.bench.stop(telemetry)
            frames = gpu.frame_stats(door.query("frame-stats"))
            after_report = gpu.diagnostics(runtime)
            (root / "diagnostics-after.txt").write_text(after_report)
            duration = (after["monotonic_ns"] - start_ns) / 1e9
            if case != "idle" and client.poll() is not None:
                raise ValueError("GPU client exited during measurement")
            feedback = presentation_stats((root / "client.log").read_text(), start_ns, after["monotonic_ns"], args.hz) if case != "idle" else None
            kms_after = json.loads(checked([str(args.drm_info), str(args.device)]))
            monitors_after = json.loads(gpu.ipc(runtime, "j/monitors"))
            if args.vrr == "off" and any(monitor.get("vrr") for monitor in monitors_after):
                raise ValueError("VRR was enabled during a fixed-refresh measurement")
            write_json(root / "kms-after.json", kms_after)
            if [(c["width"], c["height"], c["hz"]) for c in kms_after["crtcs"] if c["active"]] != [(c["width"], c["height"], c["hz"]) for c in active]:
                raise ValueError("physical mode changed during measurement")
            checked(["grim", str(root / "pixels.png")], env=client_env)
            mapped = json.loads(gpu.ipc(runtime, "j/clients"))
            rect = (*mapped[0]["at"], *mapped[0]["size"]) if case == "windowed" else None
            pixels = verify_pixels(root / "pixels.png", [args.width, args.height], case not in ("idle", "windowed"), rect)
            (root / "diagnostics-after-capture.txt").write_text(gpu.diagnostics(runtime))
            log = (root / "compositor.log").read_text()
            renderers = re.findall(r'GL Renderer: "([^"]+)"', log)
            if not renderers or any("llvmpipe" in name.lower() or "softpipe" in name.lower() for name in renderers):
                raise ValueError(f"not a hardware renderer: {renderers}")
            result = {"label": label, "case": case, "policy": policy, "seconds": duration, "cpu_percent": (after["cpu_ticks"] - before["cpu_ticks"]) / os.sysconf("SC_CLK_TCK") / duration * 100,
                      "render_fps": frames.get("render_calls", 0) / duration, "render_wall_us_per_frame": frames.get("render_us", 0) / max(1, frames.get("render_calls", 0)),
                      "presentation": feedback, "frames": frames, "before": before, "after": after,
                      "native_delta": delta(counters(before_report, "native_pipeline"), counters(after_report, "native_pipeline")),
                      "stages": stage_stats(before_report, after_report), "captures": captures,
                      "readback_before": counters(before_report, "readback"), "readback_after": counters(after_report, "readback"),
                      "multi_gpu_delta": delta(counters(before_report, "multi_gpu_copies"), counters(after_report, "multi_gpu_copies")),
                      "monitors": monitors, "monitors_after": monitors_after, "renderers": renderers, "pixels": pixels, "load_average": os.getloadavg(),
                      "active_hardware_planes": [{key: plane[key] for key in ("id", "type", "crtc", "framebuffer")} for plane in kms_after["planes"] if plane["crtc"] and plane["framebuffer"]]}
            write_json(root / "sample.json", result)
            if args.gpu_timings and case != "idle" and not result["stages"].get("composition_gpu", {}).get("count"):
                raise ValueError("GPU timings requested but no completed GPU samples were observed")
            if any(result["native_delta"].get(key, 0) for key in ("render_failed", "queue_failed")):
                raise ValueError("native rendering or queueing failed; raw sample retained")
            if args.require_scanout:
                used = any(plane["type"] == 2 for plane in result["active_hardware_planes"]) if args.require_scanout == "cursor" else result["native_delta"].get(args.require_scanout, 0) > 0
                if not used:
                    raise ValueError(f"required {args.require_scanout} scanout was never used; raw sample retained")
            if args.stress:
                stress_session(args, runtime, door, client_env, root)
            print(json.dumps({key: result[key] for key in ("label", "case", "policy", "cpu_percent", "render_fps", "presentation", "native_delta")}), flush=True)
            return result


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", action="append", required=True, metavar="LABEL=PATH")
    p.add_argument("--probe", type=Path, required=True)
    p.add_argument("--drm-info", type=Path, required=True)
    p.add_argument("--device", type=Path, required=True)
    p.add_argument("--render-device", type=Path)
    p.add_argument("--connector", required=True)
    p.add_argument("--test-vt", type=int, required=True)
    p.add_argument("--return-vt", type=int, required=True)
    p.add_argument("--width", type=int, default=3840)
    p.add_argument("--height", type=int, default=2160)
    p.add_argument("--hz", type=float, default=144)
    p.add_argument("--cases", nargs="+", choices=tuple(CASES), default=["native", "fractional"])
    p.add_argument("--policies", nargs="+", choices=tuple(POLICIES), default=["default"])
    p.add_argument("--seconds", type=float, default=30)
    p.add_argument("--settle-seconds", type=float, default=5)
    p.add_argument("--runs", type=int, default=5)
    p.add_argument("--gpu-timings", action="store_true")
    p.add_argument("--vrr", choices=("off", "auto"), default="off", help="disable adaptive refresh for controlled comparisons")
    p.add_argument("--log-filter", default="info")
    p.add_argument("--require-scanout", choices=("primary", "overlay", "cursor"))
    p.add_argument("--stress", action="store_true", help="test transitions, Spaces, captures, DPMS, VT switches and modesets after sampling")
    p.add_argument("--skip-modesets", metavar="REASON", help="explicitly record an untested modeset path during stress")
    p.add_argument("--frame-marker", action="store_true", help="opt-in changing GPU pixels to reject stale captures during --stress")
    p.add_argument("--capture-every", type=float, default=0)
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    if len(set(args.cases)) != len(args.cases) or len(set(args.policies)) != len(args.policies):
        p.error("cases and policies must be unique; use --runs for repetitions")
    if args.stress and args.cases != ["direct"]:
        p.error("--stress requires --cases direct")
    if args.frame_marker and not args.stress:
        p.error("--frame-marker requires --stress for freshness verification")
    if args.skip_modesets is not None and (not args.stress or not args.skip_modesets.strip()):
        p.error("--skip-modesets requires --stress and a nonempty reason")
    if args.test_vt == args.return_vt or min(args.test_vt, args.return_vt) < 1 or max(args.test_vt, args.return_vt) > 63:
        p.error("test and recovery VTs must be different numbers from 1 to 63")
    if any(not math.isfinite(v) for v in (args.seconds, args.settle_seconds, args.hz, args.capture_every)) or min(args.seconds, args.hz, args.width, args.height, args.runs) <= 0 or min(args.settle_seconds, args.capture_every) < 0:
        p.error("invalid mode or measurement duration")
    args.output = args.output.resolve()
    if len(os.fsencode(args.output / "000/runtime/hypr/chonkstep_9999999999_4294967295/.socket2.sock")) >= 108:
        p.error("output path too long for Unix sockets; use a short /tmp path")
    args.probe = args.probe.resolve(strict=True)
    args.drm_info = args.drm_info.resolve(strict=True)
    args.device = args.device.resolve(strict=True)
    binaries = []
    for value in args.binary:
        label, separator, path = value.partition("=")
        if not separator or not re.fullmatch(r"[a-zA-Z0-9_-]+", label) or any(label == old for old, _ in binaries):
            p.error("binary labels must be unique letters/numbers/underscores/hyphens")
        binaries.append((label, Path(path).resolve(strict=True)))
    checked(["sudo", "-n", "true"])
    sessions = json.loads(checked(["loginctl", "list-sessions", "--json=short"]))
    for session in sessions:
        vt = checked(["loginctl", "show-session", str(session["session"]), "-p", "VTNr", "--value"]).strip()
        if vt == str(args.test_vt):
            p.error(f"test VT already belongs to session {session['session']}; clean up that session explicitly first")
    args.output.mkdir(parents=True, exist_ok=False)
    source_dir = args.output / "harness"
    source_dir.mkdir()
    for name in ("bench-native-gpu.py", "bench-gpu-scaling.py", "bench-compositor.py", "native-drm-info.c", "gpu-sysfs-telemetry.py"):
        shutil.copy2(Path(__file__).with_name(name), source_dir / name)
    metadata = {"date_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()), "backend": "native DRM/KMS",
                "arguments": {key: str(value) if isinstance(value, Path) else value for key, value in vars(args).items()},
                "binaries": {label: gpu.bench.binary_metadata(binary) for label, binary in binaries},
                "probe_sha256": hashlib.sha256(args.probe.read_bytes()).hexdigest(), "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
                "kernel": checked(["uname", "-a"]).strip(), "load_average": os.getloadavg(),
                "driver": telemetry_config(args.device)[2],
                "limitations": "Synthetic frame-paced opaque EGL producer uses glFinish before handoff. Single physical output. Presentation is hardware completion, not photon/input latency. GPU board telemetry includes other processes. Histogram percentiles are upper bounds."}
    write_json(args.output / "metadata.json", metadata)
    samples = []
    def interrupted(signum, _):
        raise KeyboardInterrupt(f"signal {signum}")
    signal.signal(signal.SIGTERM, interrupted)
    try:
        for run in range(args.runs):
            cells = [(label, binary, case, policy) for case in args.cases for policy in args.policies for label, binary in binaries]
            if run % 2:
                cells.reverse()
            for label, binary, case, policy in cells:
                root = args.output / f"{len(samples):03d}"
                result = measure(args, label, binary, case, policy, root)
                result["run"] = run
                result["directory"] = root.name
                samples.append(result)
                write_json(args.output / "samples.json", samples)
    finally:
        checked(["sudo", "-n", "chvt", str(args.return_vt)])
    summary = {}
    for key in sorted({(s["label"], s["case"], s["policy"]) for s in samples}):
        group = [s for s in samples if (s["label"], s["case"], s["policy"]) == key]
        summary["/".join(key)] = {metric: {"median": statistics.median(values), "min": min(values), "max": max(values)}
                                  for metric in ("cpu_percent", "render_fps", "render_wall_us_per_frame")
                                  if (values := [s[metric] for s in group])}
    write_json(args.output / "summary.json", summary)
    print(json.dumps(summary, indent=2), flush=True)


if __name__ == "__main__":
    main()
