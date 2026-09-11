"""Benchmark guards; no display server or user session required.

Run with: python3 -m unittest discover -s scripts/tests -v
"""

import contextlib
import hashlib
import importlib.util
import io
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import unittest
from unittest import mock


SPEC = importlib.util.spec_from_file_location(
    "bench_compositor", Path(__file__).resolve().parents[1] / "bench-compositor.py"
)
bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bench)


class FragmentedSocket:
    """One-byte reads exercise stream framing, independent of packet boundaries."""

    def __init__(self, data):
        self.data = bytearray(data)
        self.sent = bytearray()

    def recv(self, size):
        part = self.data[:min(size, 1)]
        del self.data[:len(part)]
        return bytes(part)

    def sendall(self, data):
        self.sent.extend(data)

    def connect(self, path):
        self.path = path

    def settimeout(self, timeout):
        self.timeout = timeout

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass


class WaylandReadinessTests(unittest.TestCase):
    def roundtrip(self, data):
        stream = FragmentedSocket(data)
        with mock.patch.object(bench.socket, "socket", return_value=stream):
            bench.wayland_roundtrip(Path("/not-a-real-session"))
        self.assertEqual(stream.sent, struct.pack("=III", 1, 12 << 16, 2))
        self.assertEqual(stream.timeout, 45)

    def test_fragmented_callback_after_unrelated_event(self):
        deleted_id = struct.pack("=III", 1, (12 << 16) | 1, 99)
        callback = struct.pack("=III", 2, 12 << 16, 123)
        self.roundtrip(deleted_id + callback)

    def test_closed_connection_is_not_readiness(self):
        with self.assertRaisesRegex(RuntimeError, "closed"):
            self.roundtrip(struct.pack("=I", 2))

    def test_protocol_error_is_not_readiness(self):
        with self.assertRaisesRegex(RuntimeError, "protocol error"):
            self.roundtrip(struct.pack("=II", 1, 8 << 16))

    def test_malformed_message_sizes_are_rejected(self):
        for size in (0, 4, 9, 11):
            with self.subTest(size=size), self.assertRaisesRegex(RuntimeError, "message size"):
                self.roundtrip(struct.pack("=II", 2, size << 16))

    def test_callback_requires_serial(self):
        with self.assertRaisesRegex(RuntimeError, "callback payload"):
            self.roundtrip(struct.pack("=II", 2, 8 << 16))

    def test_timeout_is_not_readiness(self):
        stream = mock.MagicMock()
        stream.__enter__.return_value = stream
        stream.recv.side_effect = socket.timeout("no callback")
        with mock.patch.object(bench.socket, "socket", return_value=stream):
            with self.assertRaises(TimeoutError):
                bench.wayland_roundtrip(Path("/not-a-real-session"))

    def test_missing_compatibility_socket_does_not_count_as_full_fixture(self):
        with tempfile.TemporaryDirectory(prefix="chonk-bench-test-") as temporary:
            with self.assertRaisesRegex(RuntimeError, "Hyprland IPC socket"):
                bench.hyprland_roundtrip(Path(temporary))

    def test_fragmented_hyprland_reply_is_checked_as_json(self):
        runtime = mock.Mock()
        runtime.glob.return_value = [Path("/private-hyprland-socket")]
        stream = FragmentedSocket(b'{"version":"test"}')
        with mock.patch.object(bench.socket, "socket", return_value=stream):
            self.assertEqual(bench.hyprland_roundtrip(runtime), {"version": "test"})
        self.assertEqual(stream.sent, b"j/version")


class IsolationTests(unittest.TestCase):
    def test_profiled_binary_cannot_supply_timing_measurements(self):
        version = "chonkstep test\ndiagnostics: memory-profile (allocation counters enabled; not a timing baseline)"
        with mock.patch.object(bench, "command_output", return_value=version):
            with self.assertRaisesRegex(ValueError, "uninstrumented binary"):
                bench.binary_metadata(Path("/not-even-opened"))

    def test_ordinary_binary_metadata_preserves_version_and_hash(self):
        path = Path(__file__)
        with mock.patch.object(bench, "command_output", return_value="chonkstep test"):
            metadata = bench.binary_metadata(path)
        self.assertEqual(metadata, {
            "path": str(path), "version": "chonkstep test",
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        })

    def test_child_context_reaps_its_process_when_measurement_raises(self):
        with tempfile.TemporaryDirectory(prefix="chonk-bench-test-") as temporary:
            with self.assertRaisesRegex(ValueError, "fixture failure"):
                with bench.child(
                    [bench.sys.executable, "-c", "import time; time.sleep(60)"],
                    os.environ.copy(), Path(temporary) / "child.log",
                ) as process:
                    raise ValueError("fixture failure")
            self.assertIsNotNone(process.poll())

    def test_environment_keeps_caller_unchanged_and_removes_session_handles(self):
        source = {
            "PATH": "/usr/bin", "HOME": "/not-the-test-home", "DISPLAY": ":987",
            "CHONKSTEP_TEST_SOCKET": "/live-test-socket", "WAYLAND_SOCKET": "9",
            "HYPRLAND_INSTANCE_SIGNATURE": "live-session", "AQ_DRM_DEVICES": "/dev/dri/card9",
            "WLR_BACKENDS": "drm", "LD_PRELOAD": "/not-a-library",
        }
        with tempfile.TemporaryDirectory(prefix="chonk-bench-test-") as temporary:
            root = Path(temporary)
            with mock.patch.dict(os.environ, source, clear=True):
                env = bench.isolated_environment(root, Path("/private-host"), True)
                self.assertEqual(dict(os.environ), source)
            for key in source.keys() - {"PATH", "HOME"}:
                self.assertNotIn(key, env)
            self.assertEqual(env["WAYLAND_DISPLAY"], "/private-host")
            self.assertEqual(env["GALLIUM_DRIVER"], "llvmpipe")
            self.assertEqual(env["CHONKSTEP_BACKEND"], "winit")
            self.assertTrue((root / "state/omarchy").is_dir())
            for directory in ("config", "state", "cache", "data", "runtime"):
                self.assertEqual((root / directory).stat().st_mode & 0o777, 0o700)

    def test_optional_metadata_command_can_be_missing_or_timeout(self):
        for error in (FileNotFoundError("absent"), subprocess.TimeoutExpired("probe", 15)):
            with self.subTest(error=error), mock.patch.object(bench.subprocess, "run", side_effect=error):
                self.assertTrue(bench.command_output(["optional-probe"]).startswith("unavailable:"))


class DecorationWorkloadTests(unittest.TestCase):
    def test_quoted_client_titles_do_not_become_geometry_fields(self):
        lines = ['window id=3 x=10 app="org.chonkstep.bench.2" title="a title x=999"',
                 'frame id=4 window=3 x=9 y=0 w=320 h=213 mapped=true']
        self.assertEqual(bench.scene_records(lines, "window"), [
            {"id": "3", "x": "10", "app": "org.chonkstep.bench.2", "title": "a title x=999"}])

    def drag(self, moved):
        class Door:
            def __init__(self):
                self.snapshots = 0
                self.sent = []

            def send(self, command):
                self.sent.append(command)

            def query(self, command, multiple=False):
                # Input commands have no reply. Accidentally using query for
                # them is a real protocol timeout, not a slower benchmark.
                if command == "barrier":
                    return "ok"
                if command == "frame-stats":
                    return "frame-stats render_calls=2"
                if command != "windows" or not multiple:
                    raise AssertionError(f"unexpected query: {command}")
                x = 100 + (17 if self.snapshots and moved else 0)
                self.snapshots += 1
                return ['window id=3 x=101 y=124 app="org.chonkstep.bench.2"',
                        f'frame id=4 window=3 x={x} y=100 w=320 h=213 mapped=true']

        door = Door()
        snapshots = [dict(sample_monotonic_ns=0, cpu_ticks=10),
                     dict(sample_monotonic_ns=20_000_000, cpu_ticks=11)]
        with mock.patch.object(bench, "proc_snapshot", side_effect=snapshots), \
                mock.patch.object(bench.time, "monotonic", side_effect=[1.0, 1.001, 1.009, 1.016]), \
                mock.patch.object(bench.time, "sleep"):
            result = bench.measure_decoration_drag(door, 123, Path("/unused"), 0.016)
        self.assertEqual(result["motion_samples"], 2)
        self.assertAlmostEqual(result["input_hz"], 125)
        self.assertEqual(door.sent[0], "motion 260 112")
        self.assertEqual(door.sent[1], "button left press")
        self.assertEqual(door.sent[-1], "button left release")

    def test_drag_sends_input_without_waiting_for_nonexistent_replies(self):
        self.drag(moved=True)

    def test_a_stationary_client_cannot_count_as_a_successful_drag(self):
        with self.assertRaisesRegex(RuntimeError, "did not move"):
            self.drag(moved=False)


class ArgumentTests(unittest.TestCase):
    def reject(self, *options):
        with tempfile.TemporaryDirectory(prefix="chonk-bench-test-") as temporary:
            output = Path(temporary) / "results"
            argv = ["bench-compositor.py", "--binary", "ok=/usr/bin/true", "--output", str(output), *options]
            with mock.patch.object(bench.sys, "argv", argv), contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    bench.main()
            self.assertEqual(error.exception.code, 2)
            self.assertFalse(output.exists())

    def test_invalid_intervals_do_not_launch_sessions(self):
        for key, values in (("--runs", ("0", "-1")),
                            ("--idle-seconds", ("0", "-1", "nan", "inf")),
                            ("--settle-seconds", ("-1", "nan", "inf"))):
            for value in values:
                with self.subTest(key=key, value=value):
                    self.reject(key, value)

    def test_invalid_or_duplicate_labels_do_not_launch_sessions(self):
        for value in ("oops", "../escape=/usr/bin/true", "a" * 25 + "=/usr/bin/true", "ok=/usr/bin/true"):
            with self.subTest(value=value):
                self.reject("--binary", value)

    def test_nonexecutable_path_is_rejected(self):
        self.reject("--binary", f"directory={Path(__file__).parent}")

    def test_unknown_style_or_label_and_duplicate_style_assignments_are_rejected(self):
        for value in ("system7", "missing=system7", "ok=unknown", 'ok=system7"\\nshow_dock=true'):
            with self.subTest(value=value):
                self.reject("--decoration-style", value)
        self.reject("--decoration-style", "ok=system7", "--decoration-style", "ok=windowmaker")

    def test_socket_path_limit_is_checked_before_startup(self):
        self.reject("--output", "/tmp/" + "x" * 100)


if __name__ == "__main__":
    unittest.main()
