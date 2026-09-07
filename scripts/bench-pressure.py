#!/usr/bin/env python3
"""Constrain only a new private test fixture; observe it from outside its cgroup.

The COMMAND must itself create an isolated nested session (for example the
preserved capture benchmark or isolated E2E runner). No existing service is
modified. No allocation stressor is added. Memory ceilings are not proof that
the workload experienced memory pressure: inspect saved PSI/events/peak.

The worker keeps the scope alive after COMMAND ends until the outside observer
has saved final kernel statistics. A systemd runtime cap handles observer loss.
OOM may remove the worker/cgroup before that handshake; saved periodic samples
and retained unit Result/MemoryPeak then document this explicit limitation.
"""
import argparse
import contextlib
import hashlib
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import time
import uuid

CGROUP_FILES = (
    "cgroup.events", "cpu.max", "cpu.stat", "cpu.pressure", "memory.current",
    "memory.peak", "memory.max", "memory.high", "memory.events", "memory.events.local",
    "memory.pressure", "memory.stat", "memory.swap.current", "memory.swap.peak",
    "memory.swap.max", "pids.current", "pids.peak", "pids.events",
)


def write_json(path, value):
    temporary = path.with_name(path.name + ".tmp")
    temporary.write_text(json.dumps(value, indent=2) + "\n")
    os.replace(temporary, path)


def current_cgroup():
    for line in Path("/proc/self/cgroup").read_text().splitlines():
        hierarchy, controllers, relative = line.split(":", 2)
        if hierarchy == "0" and not controllers:
            return relative
    raise RuntimeError("a unified cgroup v2 hierarchy is required")


def validate_group(relative, unit, manager_group):
    path = Path(relative)
    if (not path.is_absolute() or ".." in path.parts or path.name != unit or
            not str(path).startswith(manager_group.rstrip("/") + "/")):
        raise RuntimeError(f"refusing unrelated cgroup: {relative!r}")
    return Path("/sys/fs/cgroup") / str(path).lstrip("/")


def controller_value(path):
    try:
        return path.read_text().strip()
    except (FileNotFoundError, ProcessLookupError, OSError) as error:
        return {"unavailable": str(error)}


def process_snapshot(pid, detailed=False):
    root = Path("/proc") / str(pid)
    try:
        stat = (root / "stat").read_text()
        fields = stat.rsplit(")", 1)[1].split()
        status = dict(line.split(":", 1) for line in (root / "status").read_text().splitlines())
        result = {"pid": pid, "comm": stat.split("(", 1)[1].rsplit(")", 1)[0],
                  "state": fields[0], "ppid": int(fields[1]),
                  "cpu_ticks": int(fields[11]) + int(fields[12]),
                  "start_ticks": int(fields[19]), "vsize_bytes": int(fields[20]),
                  "rss_pages": int(fields[21]),
                  "status": {k: status[k].strip() for k in (
                      "Threads", "VmRSS", "VmHWM", "RssAnon", "RssFile", "VmSwap",
                      "Cpus_allowed_list", "voluntary_ctxt_switches", "nonvoluntary_ctxt_switches")
                      if k in status}}
        if detailed:
            result["smaps_rollup"] = controller_value(root / "smaps_rollup")
        return result
    except (FileNotFoundError, ProcessLookupError):
        return {"pid": pid, "exited_during_snapshot": True}


def snapshot(group, detailed=False):
    result = {"monotonic_ns": time.monotonic_ns(), "wall_time_ns": time.time_ns(),
              "cgroup": str(group), "exists": group.exists(),
              "files": {name: controller_value(group / name) for name in CGROUP_FILES}}
    members = {}
    for source in [group / "cgroup.procs", *group.glob("**/cgroup.procs")]:
        try:
            for value in source.read_text().split():
                members[int(value)] = str(source.parent)
        except (FileNotFoundError, ProcessLookupError):
            pass
    result["processes"] = [{"cgroup": parent, **process_snapshot(pid, detailed)}
                           for pid, parent in sorted(members.items())]
    return result


def unit_status(unit):
    command = ["systemctl", "--user", "show", unit, "--no-pager"]
    result = subprocess.run(command, capture_output=True, text=True, timeout=10)
    allowed = {"Id", "LoadState", "ActiveState", "SubState", "Result", "ControlGroup",
               "CPUUsageNSec", "MemoryCurrent", "MemoryPeak", "MemoryHigh", "MemorySwapCurrent",
               "MemorySwapPeak", "CPUQuotaPerSecUSec", "MemoryMax", "MemorySwapMax",
               "RuntimeMaxUSec", "OOMPolicy", "TasksCurrent"}
    fields = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
    return {"returncode": result.returncode, "fields": {k: v for k, v in fields.items() if k in allowed},
            "stderr": result.stderr.strip()}


def worker(job_path):
    job = json.loads(job_path.read_text())
    artifact = job_path.parent
    write_json(artifact / "worker-ready.json", {
        "pid": os.getpid(), "cgroup": current_cgroup(),
        "cpu_affinity": sorted(os.sched_getaffinity(0)),
        "lp_num_threads": os.environ.get("LP_NUM_THREADS"),
        "monotonic_ns": time.monotonic_ns(),
    })
    deadline = time.monotonic() + 30
    while not (artifact / "start-command").exists():
        if time.monotonic() >= deadline:
            raise TimeoutError("observer did not validate and release the worker")
        time.sleep(0.025)
    start = time.monotonic_ns()
    result = subprocess.run(job["command"], check=False)
    write_json(artifact / "command-result.json", {
        "returncode": result.returncode, "start_monotonic_ns": start,
        "end_monotonic_ns": time.monotonic_ns(),
    })
    # Preserve cgroup membership until final cpu/memory/event statistics and a
    # process tree have been persisted outside the constrained unit.
    deadline = time.monotonic() + 45
    while not (artifact / "final-snapshot-saved").exists():
        if time.monotonic() >= deadline:
            raise TimeoutError("observer did not acknowledge the final snapshot")
        time.sleep(0.025)
    return result.returncode if result.returncode >= 0 else 128 - result.returncode


def distinct_cores(cpus):
    identities = []
    for cpu in cpus:
        topology = Path(f"/sys/devices/system/cpu/cpu{cpu}/topology")
        identities.append(((topology / "physical_package_id").read_text().strip(),
                           (topology / "core_id").read_text().strip()))
    return len(identities) == len(set(identities))


def numeric_delta(before, after):
    if not isinstance(before, str) or not isinstance(after, str):
        return None
    def parse(text):
        pairs = {}
        for line in text.splitlines():
            parts = line.split()
            if len(parts) == 2 and parts[1].isdigit():
                pairs[parts[0]] = int(parts[1])
            elif parts and parts[0] in ("some", "full"):
                for field in parts[1:]:
                    key, _, value = field.partition("=")
                    if key == "total" and value.isdigit():
                        pairs[parts[0] + "_total_us"] = int(value)
        return pairs
    initial, final = parse(before), parse(after)
    return {key: value - initial[key] for key, value in final.items() if key in initial}


def oom_increased(before, after):
    delta = numeric_delta(before, after)
    return bool(delta and any(delta.get(name, 0) > 0 for name in ("oom", "oom_kill", "oom_group_kill")))


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "--internal-worker":
        return worker(Path(sys.argv[2]))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--memory", choices=("1G", "2G"), default="1G")
    parser.add_argument("--memory-high-percent", type=int, choices=range(1, 100), metavar="1..99",
                        help="optional reclaim threshold as a percentage of MemoryMax")
    parser.add_argument("--cpus", default="2,3")
    parser.add_argument("--timeout-seconds", type=float, default=1800)
    parser.add_argument("--sample-seconds", type=float, default=0.25)
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command:
        parser.error("supply the isolated nested fixture command after --")
    try:
        cpus = sorted(set(int(value) for value in args.cpus.split(",")))
    except ValueError:
        parser.error("cpus must be two comma-separated integers")
    if len(cpus) != 2 or not set(cpus) <= os.sched_getaffinity(0) or not distinct_cores(cpus):
        parser.error("choose exactly two available CPUs on distinct physical cores")
    if (not math.isfinite(args.timeout_seconds) or args.timeout_seconds <= 0 or
            not math.isfinite(args.sample_seconds) or not 0.1 <= args.sample_seconds <= 5):
        parser.error("timeout must be finite and positive; sample interval must be 0.1–5 seconds")
    artifact = args.output.resolve()
    if artifact.exists():
        parser.error("output directory must not already exist")
    manager = subprocess.run(["systemctl", "--user", "show", "-p", "ControlGroup", "--value"],
                             capture_output=True, text=True, timeout=10, check=True).stdout.strip()
    if not manager.startswith("/user.slice/") or not manager.endswith(f"/user@{os.getuid()}.service"):
        parser.error(f"unexpected user manager cgroup: {manager!r}")
    unit = "chonk-capture-pressure-" + uuid.uuid4().hex + ".scope"
    properties = ["CPUQuota=200%", f"MemoryMax={args.memory}", "MemorySwapMax=0",
                  "MemoryAccounting=yes", "CPUAccounting=yes", "OOMPolicy=continue",
                  f"RuntimeMaxSec={math.ceil(args.timeout_seconds + 60)}s"]
    memory_high = (int(args.memory[0]) * 1024**3 * args.memory_high_percent // 100 // os.sysconf("SC_PAGE_SIZE")
                   * os.sysconf("SC_PAGE_SIZE")
                   if args.memory_high_percent is not None else None)
    if memory_high is not None:
        properties.append(f"MemoryHigh={memory_high}")
    launch = ["systemd-run", "--user", "--scope", "--quiet", "--no-ask-password", f"--unit={unit}"]
    for value in properties:
        launch += ["--property", value]
    launch += ["--", "taskset", "-c", ",".join(map(str, cpus)), sys.executable, "-B",
               str(Path(__file__).resolve()), "--internal-worker", str(artifact / "job.json")]
    job = {"command": command, "unit": unit, "manager_cgroup": manager,
           "launch_command": launch, "properties": properties, "cpu_affinity": cpus,
           "memory_max_bytes": int(args.memory[0]) * 1024**3, "memory_swap_max_bytes": 0,
           "memory_high_bytes": memory_high,
           "sample_seconds": args.sample_seconds, "timeout_seconds": args.timeout_seconds,
           "lp_num_threads": 2, "harness_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
           "observer_cgroup": current_cgroup(),
           "scope_boundary": "new isolated fixture command and all inherited descendants only",
           "limitations": ["The runner adds no stressor; the explicit fixture command may. Ceilings alone are not pressure proof.",
                           "Older hardware cache/IPC/storage/GPU behavior is not emulated.",
                           "OOM may destroy cgroup before final snapshot; periodic samples and unit result remain."]}
    if args.dry_run:
        print(json.dumps(job, indent=2))
        return 0
    artifact.mkdir(parents=True, exist_ok=False)
    write_json(artifact / "job.json", job)
    group = None
    first = last = None
    result = {"command": command, "unit": unit, "completed": False, "oom_event_observed": False}
    process = None
    start = time.monotonic()
    try:
        with (artifact / "command.log").open("wb") as log:
            process = subprocess.Popen(launch, env=os.environ | {"LP_NUM_THREADS": "2"},
                                       stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            deadline = time.monotonic() + 25
            while not (artifact / "worker-ready.json").exists():
                if process.poll() is not None:
                    raise RuntimeError(f"scope launch exited {process.returncode}; inspect command.log")
                if time.monotonic() >= deadline:
                    raise TimeoutError("scoped worker did not report readiness")
                time.sleep(0.025)
            ready = json.loads((artifact / "worker-ready.json").read_text())
            group = validate_group(ready["cgroup"], unit, manager)
            if ready["cpu_affinity"] != cpus or ready["lp_num_threads"] != "2":
                raise RuntimeError(f"worker affinity/renderer settings do not match: {ready}")
            quota, period = (group / "cpu.max").read_text().split()
            if quota == "max" or int(quota) != 2 * int(period):
                raise RuntimeError("scope CPU quota is not two CPU equivalents")
            if ((group / "memory.max").read_text().strip() != str(job["memory_max_bytes"]) or
                    (group / "memory.swap.max").read_text().strip() != "0"):
                raise RuntimeError("scope memory/swap settings do not match requested limits")
            if memory_high is not None and (group / "memory.high").read_text().strip() != str(memory_high):
                raise RuntimeError("scope reclaim threshold does not match requested MemoryHigh")
            first = snapshot(group, detailed=True)
            write_json(artifact / "before.json", first)
            write_json(artifact / "unit-before.json", unit_status(unit))
            (artifact / "start-command").touch(exist_ok=False)
            with (artifact / "samples.jsonl").open("w") as stream:
                while True:
                    last = snapshot(group)
                    result["oom_event_observed"] |= oom_increased(
                        first["files"]["memory.events"], last["files"]["memory.events"])
                    stream.write(json.dumps(last) + "\n")
                    stream.flush()
                    if (artifact / "command-result.json").exists() or process.poll() is not None:
                        break
                    if time.monotonic() - start >= args.timeout_seconds:
                        raise TimeoutError("isolated fixture reached configured runtime limit")
                    time.sleep(args.sample_seconds)
            final = snapshot(group, detailed=True)
            result["oom_event_observed"] |= oom_increased(
                first["files"]["memory.events"], final["files"]["memory.events"])
            write_json(artifact / "after.json", final)
            write_json(artifact / "unit-after.json", unit_status(unit))
            (artifact / "final-snapshot-saved").touch(exist_ok=False)
            launcher_status = process.wait(timeout=15)
            result.update(completed=(artifact / "command-result.json").exists(),
                          launcher_returncode=launcher_status,
                          command_result=(json.loads((artifact / "command-result.json").read_text())
                                          if (artifact / "command-result.json").exists() else None),
                          cgroup_final_snapshot_available=final["exists"],
                          deltas={name: numeric_delta(first["files"][name], final["files"][name])
                                  for name in ("cpu.stat", "cpu.pressure", "memory.events", "memory.pressure")})
    except (Exception, KeyboardInterrupt) as error:
        result["error"] = repr(error)
        if group is not None:
            with contextlib.suppress(Exception):
                write_json(artifact / "failure-snapshot.json", snapshot(group, detailed=True))
    finally:
        # Every invocation owns a fresh, random unit name; no caller-supplied or
        # pre-existing unit can enter this stop path. The scope's own runtime
        # cap also cleans it up if this observer is killed before finally.
        with contextlib.suppress(Exception):
            write_json(artifact / "unit-final.json", unit_status(unit))
        try:
            stop = subprocess.run(["systemctl", "--user", "stop", unit],
                                  capture_output=True, text=True, timeout=15)
            result["cleanup"] = {"returncode": stop.returncode, "stderr": stop.stderr.strip()}
        except (OSError, subprocess.TimeoutExpired) as error:
            result["cleanup"] = {"error": repr(error), "runtime_cap_remains_enabled": True}
        if process is not None:
            with contextlib.suppress(subprocess.TimeoutExpired):
                process.wait(timeout=5)
        result["passed"] = (result.get("completed", False)
                            and result.get("launcher_returncode") == 0
                            and not result["oom_event_observed"])
        if result["oom_event_observed"]:
            result["failure_reason"] = "The isolated scope reported an OOM event."
        result["wall_seconds"] = time.monotonic() - start
        write_json(artifact / "result.json", result)
    return 0 if result["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
