"""A green Cargo exit must not hide lost native renderer coverage."""

from pathlib import Path
import subprocess
import sys
import unittest


GATE = Path(__file__).resolve().parents[1] / "check-native-tests.py"
REQUIRED = [f"renderer::regression_{n}" for n in range(5)]


class NativeGateTests(unittest.TestCase):
    def run_gate(self, lines, status=0):
        command = [sys.executable, str(GATE)]
        for name in REQUIRED:
            command.extend(["--require", name])
        command.extend(["--", sys.executable, "-c",
                        f"print({lines!r}); raise SystemExit({status})"])
        return subprocess.run(command, capture_output=True, text=True, timeout=10)

    def test_all_named_tests_must_pass(self):
        lines = "\n".join(f"test {name} ... ok" for name in REQUIRED)
        result = self.run_gate(lines)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(lines, result.stdout)
        self.assertIn("Native gate elapsed:", result.stdout)

    def test_green_zero_or_partial_selection_fails(self):
        for count in range(5):
            with self.subTest(count=count):
                lines = "\n".join(f"test {name} ... ok" for name in REQUIRED[:count])
                lines += f"\ntest result: ok. {count} passed; 0 failed; 0 ignored"
                result = self.run_gate(lines)
                self.assertEqual(result.returncode, 1)
                self.assertIn(REQUIRED[count], result.stderr)

    def test_equal_count_of_wrong_tests_cannot_replace_required_test(self):
        names = [*REQUIRED[:-1], "renderer::unrelated"]
        result = self.run_gate("\n".join(f"test {name} ... ok" for name in names))
        self.assertEqual(result.returncode, 1)
        self.assertIn(REQUIRED[-1], result.stderr)

    def test_ignored_required_test_fails(self):
        lines = "\n".join(f"test {name} ... ok" for name in REQUIRED[:-1])
        result = self.run_gate(lines + f"\ntest {REQUIRED[-1]} ... ignored")
        self.assertEqual(result.returncode, 1)

    def test_cargo_failure_is_preserved_even_with_all_required_success_lines(self):
        lines = "\n".join(f"test {name} ... ok" for name in REQUIRED)
        result = self.run_gate(lines, status=42)
        self.assertEqual(result.returncode, 42)

    def test_a_command_is_required(self):
        result = subprocess.run([sys.executable, str(GATE), "--require", REQUIRED[0]],
                                capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 2)
        self.assertIn("a test command is required", result.stderr)


if __name__ == "__main__":
    unittest.main()
