"""Validate pressure scope boundaries and dry runs without creating any scope."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest import mock


SOURCE = Path(__file__).resolve().parents[1] / "bench-pressure.py"
SPEC = importlib.util.spec_from_file_location("pressure_benchmark", SOURCE)
PRESSURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PRESSURE)


class PressureScopeTests(unittest.TestCase):
    def test_scope_validation_rejects_parent_sibling_traversal_and_relative_paths(self):
        manager = "/user.slice/user-1000.slice/user@1000.service"
        unit = "chonk-capture-pressure-owned.scope"
        for path in (manager, manager + "/other.scope", manager + "-sibling/" + unit,
                     manager + "/child/../" + unit, "relative/" + unit,
                     "/system.slice/" + unit):
            with self.subTest(path=path), self.assertRaises(RuntimeError):
                PRESSURE.validate_group(path, unit, manager)
        self.assertEqual(PRESSURE.validate_group(manager + "/app.slice/" + unit, unit, manager),
                         Path("/sys/fs/cgroup" + manager + "/app.slice/" + unit))

    def test_counter_and_pressure_deltas_preserve_unavailable_data(self):
        self.assertEqual(PRESSURE.numeric_delta("usage_usec 12\nnr_throttled 1", "usage_usec 25\nnr_throttled 3"),
                         {"usage_usec": 13, "nr_throttled": 2})
        self.assertEqual(PRESSURE.numeric_delta("some avg10=0.0 total=20\nfull avg10=0.0 total=8",
                                               "some avg10=0.1 total=35\nfull avg10=0.0 total=11"),
                         {"some_total_us": 15, "full_total_us": 3})
        self.assertIsNone(PRESSURE.numeric_delta("oom 0", {"unavailable": "scope removed"}))

    def test_oom_is_failure_even_when_command_survives(self):
        self.assertTrue(PRESSURE.oom_increased("oom 0\noom_kill 0", "oom 1\noom_kill 0"))
        self.assertTrue(PRESSURE.oom_increased("oom 2\noom_kill 0", "oom 2\noom_kill 1"))
        self.assertFalse(PRESSURE.oom_increased("oom 2", "oom 2"))
        self.assertFalse(PRESSURE.oom_increased("oom 0", {"unavailable": "removed"}))

    def test_dry_run_never_creates_scope_or_artifact_directory(self):
        with tempfile.TemporaryDirectory(prefix="pressure-dry-run-") as temporary:
            output = Path(temporary) / "artifacts"
            manager = f"/user.slice/user-{os.getuid()}.slice/user@{os.getuid()}.service"
            argv = [str(SOURCE), "--output", str(output), "--memory", "2G",
                    "--memory-high-percent", "75", "--dry-run", "--",
                    "bash", "scripts/e2e.sh", "--headless", "--release", "--test", "capture_tool"]
            stdout = io.StringIO()
            with mock.patch.object(sys, "argv", argv), \
                    mock.patch.object(PRESSURE.os, "sched_getaffinity", return_value={2, 3}), \
                    mock.patch.object(PRESSURE, "distinct_cores", return_value=True), \
                    mock.patch.object(PRESSURE, "current_cgroup", return_value=manager + "/observer.scope"), \
                    mock.patch.object(PRESSURE.subprocess, "Popen", side_effect=AssertionError("unexpected scope")), \
                    mock.patch.object(PRESSURE.subprocess, "run", return_value=subprocess.CompletedProcess(
                        [], 0, stdout=manager + "\n", stderr="")) as run, \
                    contextlib.redirect_stdout(stdout):
                self.assertEqual(PRESSURE.main(), 0)
            run.assert_called_once_with(["systemctl", "--user", "show", "-p", "ControlGroup", "--value"],
                                        capture_output=True, text=True, timeout=10, check=True)
            plan = json.loads(stdout.getvalue())
            self.assertEqual(plan["memory_max_bytes"], 2 * 1024**3)
            self.assertEqual(plan["memory_swap_max_bytes"], 0)
            self.assertEqual(plan["memory_high_bytes"], 1536 * 1024**2)
            self.assertIn(f"MemoryHigh={1536 * 1024**2}", plan["properties"])
            self.assertIn("CPUQuota=200%", plan["properties"])
            self.assertIn("MemoryMax=2G", plan["properties"])
            self.assertIn("MemorySwapMax=0", plan["properties"])
            self.assertRegex(plan["unit"], r"^chonk-capture-pressure-[0-9a-f]{32}\.scope$")
            self.assertFalse(output.exists())

    def test_invalid_quotas_and_affinity_fail_before_manager_or_process_calls(self):
        for extra in (["--memory", "0"], ["--timeout-seconds", "nan"],
                      ["--sample-seconds", "0.01"], ["--cpus", "2,2"], ["--cpus", "2,99"],
                      ["--memory-high-percent", "0"], ["--memory-high-percent", "100"]):
            with self.subTest(extra=extra), tempfile.TemporaryDirectory() as temporary:
                argv = [str(SOURCE), "--output", str(Path(temporary) / "new"), *extra, "--", "true"]
                with mock.patch.object(sys, "argv", argv), \
                        mock.patch.object(PRESSURE.os, "sched_getaffinity", return_value={2, 3}), \
                        mock.patch.object(PRESSURE, "distinct_cores", return_value=True), \
                        mock.patch.object(PRESSURE.subprocess, "run", side_effect=AssertionError("unexpected manager")), \
                        contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                    PRESSURE.main()
                self.assertEqual(error.exception.code, 2)


if __name__ == "__main__":
    unittest.main()
