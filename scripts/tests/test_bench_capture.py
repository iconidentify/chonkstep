"""Capture benchmark input guards; invalid fixtures never launch a desktop."""
import contextlib
import importlib.util
import io
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock


SOURCE = Path(__file__).resolve().parents[1] / "bench-capture.py"
SPEC = importlib.util.spec_from_file_location("capture_benchmark", SOURCE)
CAPTURE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(CAPTURE)


class CaptureArgumentTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="capture-arguments-")
        self.root = Path(self.temporary.name)
        self.binary = self.root / "compositor"
        self.binary.write_text("#!/bin/sh\nexit 99\n")
        self.binary.chmod(0o755)
        self.output = self.root / "output"
        self.arguments = ["--binary", "before=" + str(self.binary), "--output", str(self.output)]

    def tearDown(self):
        self.temporary.cleanup()

    def reject(self, extra):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            CAPTURE.parse_arguments(self.arguments + extra)
        self.assertEqual(error.exception.code, 2)
        self.assertFalse(self.output.exists())

    def test_valid_fixture_resolves_binary_without_starting_it(self):
        with mock.patch.object(CAPTURE.subprocess, "run", side_effect=AssertionError("unexpected launch")):
            _, args, binaries = CAPTURE.parse_arguments(self.arguments)
        self.assertEqual(binaries, [("before", self.binary)])
        self.assertEqual(args.output, self.output)
        self.assertFalse(self.output.exists())

    def test_nonfinite_empty_or_overflowed_workload_is_rejected(self):
        for extra in (["--idle-seconds", "nan"], ["--motion-seconds", "inf"],
                      ["--motion-hz", "0"], ["--settle-seconds", "-1"],
                      ["--runs", "0"], ["--warm-opens", "0"],
                      ["--motion-seconds", "0.01", "--motion-hz", "0.01"],
                      ["--motion-seconds", "1e308", "--motion-hz", "1e308"]):
            with self.subTest(extra=extra):
                self.reject(extra)

    def test_duplicate_unsafe_or_oversized_labels_are_rejected(self):
        for label in ("before", "../elsewhere", "two words", "é", "x" * 25):
            with self.subTest(label=label):
                self.reject(["--binary", label + "=" + str(self.binary)])

    def test_missing_nonexecutable_and_directory_binaries_are_rejected(self):
        self.reject(["--binary", "missing=" + str(self.root / "missing")])
        self.reject(["--binary", "directory=" + str(self.root)])
        self.binary.chmod(0o644)
        self.reject([])

    def test_existing_output_is_rejected_without_overwriting_evidence(self):
        self.output.mkdir()
        sentinel = self.output / "sample.json"
        sentinel.write_text("previous evidence")
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            CAPTURE.parse_arguments(self.arguments)
        self.assertEqual(sentinel.read_text(), "previous evidence")

    def test_diagnostic_binary_cannot_silently_supply_timing_results(self):
        with mock.patch.object(sys, "argv", [str(SOURCE), *self.arguments]), \
                mock.patch.dict(os.environ, {"CHONKSTEP_BENCH_PRIVATE_BUS": "1"}), \
                mock.patch.object(CAPTURE.b, "command_output", return_value="diagnostics: memory-profile"), \
                mock.patch.object(CAPTURE.b, "child", side_effect=AssertionError("unexpected compositor")), \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            CAPTURE.main()
        self.assertEqual(error.exception.code, 2)
        self.assertFalse(self.output.exists())


if __name__ == "__main__":
    unittest.main()
