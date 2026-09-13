"""Failure-path contracts: fixtures never touch installed provider settings."""
from contextlib import ExitStack
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import integrations
import sessions


class SetupFailures(unittest.TestCase):
    def setUp(self):
        self.stack = ExitStack()
        self.addCleanup(self.stack.close)
        self.directory = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.state = self.directory / "state/agents"
        self.codex = self.directory / "codex"
        self.claude = self.directory / "claude"
        self.helper = self.directory / "chonk-agent-hook"
        self.helper.write_bytes(b"test fixture; never executed")
        for name, value in (("state_root", self.state), ("codex_root", self.codex),
                ("claude_root", self.claude), ("config_root", self.directory / "config"),
                ("helper_path", self.helper)):
            self.stack.enter_context(mock.patch.object(integrations, name, return_value=value))

    def fail_write(self, target, before_failure=None):
        original = integrations.write_atomic

        def write(path, *args, **kwargs):
            if path == target:
                if before_failure:
                    before_failure()
                raise OSError("injected configuration commit failure")
            return original(path, *args, **kwargs)

        return mock.patch.object(integrations, "write_atomic", side_effect=write)

    def test_failed_second_provider_removes_previously_absent_settings(self):
        with self.fail_write(self.codex / "hooks.json"):
            with self.assertRaisesRegex(OSError, "injected"):
                integrations.setup(["claude", "codex"])
        self.assertFalse((self.claude / "settings.json").exists())
        self.assertFalse((self.codex / "hooks.json").exists())
        self.assertFalse((self.state / "setup.json").exists())

    def test_failed_manifest_restores_original_bytes_and_original_absence(self):
        self.claude.mkdir()
        original = b'{ "permissions": {"defaultMode": "default"}, "userPreference": true }\n'
        settings = self.claude / "settings.json"
        settings.write_bytes(original)
        with self.fail_write(self.state / "setup.json"):
            with self.assertRaisesRegex(OSError, "injected"):
                integrations.setup(["claude", "codex"])
        self.assertEqual(settings.read_bytes(), original)
        self.assertFalse((self.codex / "hooks.json").exists())
        self.assertFalse((self.codex / "config.toml").exists())

    def test_rollback_preserves_a_user_edit_made_during_commit(self):
        settings = self.claude / "settings.json"
        replacement = b'{"userPreference":"concurrent edit"}\n'
        with self.fail_write(self.codex / "hooks.json", lambda: settings.write_bytes(replacement)):
            with self.assertRaisesRegex(OSError, "injected"):
                integrations.setup(["claude", "codex"])
        self.assertEqual(settings.read_bytes(), replacement)

    def test_lost_manifest_does_not_chain_our_notifier_to_itself(self):
        self.codex.mkdir()
        # Also covers a prior installation path and a different Python binary.
        ours = ["/usr/bin/python3", "/old/install/chonk-agents.py", "notify-codex"]
        original = ('notify = ' + json.dumps(ours) + '\nmodel = "preserve-me"\n').encode()
        config = self.codex / "config.toml"
        config.write_bytes(original)
        with self.assertRaisesRegex(ValueError, "without its setup record"):
            integrations.setup(["claude", "codex"])
        self.assertEqual(config.read_bytes(), original)
        self.assertFalse((self.claude / "settings.json").exists())
        self.assertFalse((self.codex / "hooks.json").exists())
        self.assertFalse((self.state / "setup.json").exists())

    def test_missing_helper_fails_before_changing_provider_settings(self):
        self.helper.unlink()
        with self.assertRaisesRegex(ValueError, "native hook helper"):
            integrations.setup(["claude", "codex"])
        self.assertFalse((self.claude / "settings.json").exists())
        self.assertFalse((self.codex / "config.toml").exists())

    def test_notifier_observation_failure_does_not_suppress_prior_notifier(self):
        payload = '{"type":"agent-turn-complete","thread-id":"fixture"}'
        self.state.mkdir(parents=True, mode=0o700)
        (self.state / "setup.json").write_text(json.dumps({"notify": {"previous": ["user-notifier", "--quiet"]}}))
        for failure in (FileNotFoundError("helper absent"), subprocess.TimeoutExpired("helper", .2)):
            with self.subTest(failure=type(failure).__name__), \
                    mock.patch.object(integrations.subprocess, "run", side_effect=failure), \
                    mock.patch.object(integrations.subprocess, "Popen") as launch:
                integrations.notify_codex(payload)
                launch.assert_called_once_with(["user-notifier", "--quiet", payload],
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)

    def test_damaged_manifest_cannot_start_recursive_notifier(self):
        self.state.mkdir(parents=True, mode=0o700)
        (self.state / "setup.json").write_text(json.dumps({"notify": {"previous":
            [sys.executable, "/previous/install/chonk-agents.py", "notify-codex"]}}))
        with mock.patch.object(integrations.subprocess, "run"), \
                mock.patch.object(integrations.subprocess, "Popen") as launch:
            integrations.notify_codex("{}")
        launch.assert_not_called()

    def test_fresh_setup_state_accepts_private_session_persistence(self):
        integrations.setup(["claude"])
        self.assertEqual(stat.S_IMODE(self.state.stat().st_mode), 0o700)
        path = self.state / "sessions.json"
        store = sessions.RuntimeStore(path)
        try:
            process = sessions.process_identity(os.getpid())
            store.update({"schema": 2, "engine": "claude", "client": "terminal", "origin": "hook",
                "session_id": "setup-fixture", "kind": "turn.start", "cwd": str(self.directory),
                "observed_ns": time.monotonic_ns(), **process})
        finally:
            store.close()
        self.assertIsNone(store.writer.error)
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        self.assertEqual(json.loads(path.read_text())["rows"][0]["session_id"], "setup-fixture")


class RestoreFailures(unittest.TestCase):
    def test_fifo_without_writer_is_rejected_without_waiting_for_input(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "saved.json"
            os.mkfifo(path, 0o600)
            # A subprocess timeout makes a future blocking-open regression fail
            # the test instead of wedging the entire broker test suite.
            result = subprocess.run([sys.executable, "-c",
                "import sys; from pathlib import Path; sys.path.insert(0, sys.argv[1]); "
                "from sessions import RuntimeStore; s=RuntimeStore(); s.restore(Path(sys.argv[2])); "
                "assert s.restore_error and not s.rows", str(ROOT), str(path)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=2)
            self.assertEqual(result.returncode, 0, result.stderr.decode())

    def test_deep_small_json_is_a_reported_restore_error(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "saved.json"
            path.write_text('{"schema":2,"rows":' + '[' * 5000 + '0' + ']' * 5000 + '}')
            self.assertLess(path.stat().st_size, 262144)
            store = sessions.RuntimeStore()
            store.restore(path)
            self.assertTrue(store.restore_error)
            self.assertEqual(store.public_rows(), [])

    def test_restore_rejects_symlink_and_oversized_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / "target.json"
            target.write_text('{"schema":2,"rows":[]}')
            link = Path(directory) / "saved.json"
            link.symlink_to(target)
            store = sessions.RuntimeStore()
            store.restore(link)
            self.assertTrue(store.restore_error)
            self.assertEqual(store.rows, {})
            target.write_bytes(b" " * 262145)
            store = sessions.RuntimeStore()
            store.restore(target)
            self.assertIn("oversized", store.restore_error)
            self.assertEqual(store.rows, {})


if __name__ == "__main__":
    unittest.main()
