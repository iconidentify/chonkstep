"""Exercise the real preflight dispatcher without compiling or starting a desktop."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


CHECK = Path(__file__).resolve().parents[1] / "check.sh"


class PreflightTests(unittest.TestCase):
    def run_check(self, *arguments, fail=""):
        with tempfile.TemporaryDirectory(prefix="chonk-check-test-") as temporary:
            root = Path(temporary)
            log = root / "commands.jsonl"
            stub = root / "stub"
            stub.write_text(f"#!{sys.executable}\n" + '''
import json
import os
from pathlib import Path
import sys

name = Path(sys.argv[0]).name
with open(os.environ["CHONK_CHECK_TEST_LOG"], "a") as log:
    log.write(json.dumps({"tool": name, "args": sys.argv[1:],
                          "rustdocflags": os.environ.get("RUSTDOCFLAGS", ""),
                          "cwd": os.getcwd()}) + "\\n")
if name + ":" + sys.argv[1] == os.environ.get("CHONK_CHECK_TEST_FAIL"):
    sys.exit(42)
''')
            stub.chmod(0o755)
            for name in ("cargo", "python3"):
                (root / name).symlink_to(stub)
            env = {**os.environ, "PATH": f"{root}{os.pathsep}{os.environ['PATH']}",
                   "CHONK_CHECK_TEST_LOG": str(log), "CHONK_CHECK_TEST_FAIL": fail,
                   "RUSTDOCFLAGS": "--cfg preflight_test"}
            result = subprocess.run([str(CHECK), *arguments], cwd=root, env=env,
                                    capture_output=True, text=True, timeout=10)
            commands = [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []
            return result, commands

    def test_default_runs_every_gate_including_debug_wayland_tests(self):
        result, commands = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([(item["tool"], item["args"][0]) for item in commands],
                         [("cargo", "clippy"), ("cargo", "doc"), ("cargo", "test"),
                          ("cargo", "test"), ("python3", "-B")])
        for item in commands:
            self.assertEqual(item["cwd"], str(CHECK.parent.parent))
            if item["tool"] == "cargo":
                self.assertIn("--locked", item["args"])
                self.assertNotIn("--release", item["args"])
        self.assertEqual(commands[3]["args"], ["test", "--locked", "-p", "wm-wayland"])

    def test_lint_keeps_every_ci_safety_gate_enabled(self):
        result, commands = self.run_check("lint")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(commands), 1)
        self.assertEqual(commands[0]["args"], [
            "clippy", "--locked", "--workspace", "--all-targets", "--no-deps", "--",
            "-D", "warnings", "-D", "clippy::disallowed_methods", "-D", "clippy::disallowed_types",
            "-D", "clippy::undocumented_unsafe_blocks",
        ])

    def test_documentation_keeps_strict_flags_and_private_items(self):
        result, commands = self.run_check("docs")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len(commands), 1)
        self.assertEqual(commands[0]["rustdocflags"],
                         "--cfg preflight_test -D warnings -A rustdoc::private_intra_doc_links")
        self.assertIn("--document-private-items", commands[0]["args"])

    def test_individual_test_gates_do_not_invoke_unrelated_tools(self):
        for mode, tool, expected in [
            ("unit", "cargo", ["test", "--locked", "--workspace", "--exclude", "wm-wayland",
                               "--exclude", "chonkstep-wayland"]),
            ("wayland-unit", "cargo", ["test", "--locked", "-p", "wm-wayland"]),
            ("harness", "python3", ["-B", "-m", "unittest", "discover", "-s", "scripts/tests", "-v"]),
        ]:
            with self.subTest(mode=mode):
                result, commands = self.run_check(mode)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([(item["tool"], item["args"]) for item in commands], [(tool, expected)])

    def test_all_fails_immediately_without_retrying_or_masking_errors(self):
        for stage, count in [("clippy", 1), ("doc", 2), ("test", 3)]:
            with self.subTest(stage=stage):
                result, commands = self.run_check("all", fail=f"cargo:{stage}")
                self.assertEqual(result.returncode, 42, result.stderr)
                self.assertEqual(len(commands), count)

    def test_invalid_arguments_do_not_start_any_check(self):
        for arguments in [("typo",), ("lint", "--release")]:
            with self.subTest(arguments=arguments):
                result, commands = self.run_check(*arguments)
                self.assertEqual(result.returncode, 2)
                self.assertIn("Usage:", result.stderr)
                self.assertEqual(commands, [])


if __name__ == "__main__":
    unittest.main()
