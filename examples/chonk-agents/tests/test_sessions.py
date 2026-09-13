import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import sessions
import integrations


def event(kind="session.start", **fields):
    process = sessions.process_identity(os.getpid())
    return {"schema": 2, "engine": "codex", "client": "terminal", "origin": "hook",
        "session_id": "thread-1", "kind": kind, "cwd": "/tmp/project", "title": "Project",
        "observed_ns": time.monotonic_ns(), "pid": process["pid"], "start_time": process["start_time"], **fields}


class SessionContracts(unittest.TestCase):
    def test_notify_only_hidden_codex_threads_do_not_create_sessions(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "private/state.json"
            store = sessions.RuntimeStore(path)
            complete = event("turn.complete", source_event="agent-turn-complete")
            self.assertFalse(store.update(complete))
            store.update(event("turn.start"))
            self.assertTrue(store.update({**complete, "observed_ns": time.monotonic_ns()}))
            self.assertFalse(store.update({**complete, "session_id": "hidden-title"}))
            store.close()
            restored = sessions.RuntimeStore(path)
            try:
                self.assertTrue(restored.update({**complete, "observed_ns": time.monotonic_ns()}))
                self.assertEqual(restored.public_rows()[0]["status"], "idle")
            finally:
                restored.close()

    def test_turn_outcome_remains_separate_from_session_phase(self):
        store = sessions.RuntimeStore()
        store.update(event("session.snapshot", engine="opencode", origin="plugin", phase="idle", turn_outcome="interrupted"))
        self.assertEqual(store.public_rows()[0]["turn_outcome"], "interrupted")
        store.update(event("session.snapshot", engine="opencode", origin="plugin", phase="working", turn_outcome="unknown"))
        self.assertEqual(store.public_rows()[0]["turn_outcome"], "unknown")
        with self.assertRaises(ValueError):
            store.update(event(turn_outcome="unvalidated"))

    def test_restored_new_turn_rejects_delayed_old_notify(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "private/state.json"
            store = sessions.RuntimeStore(path)
            store.update(event("turn.start", turn_id="old"))
            store.update(event("turn.start", turn_id="new"))
            store.close()
            restored = sessions.RuntimeStore(path)
            try:
                self.assertFalse(restored.update(event("turn.complete", source_event="agent-turn-complete", turn_id="old")))
                self.assertEqual(restored.public_rows()[0]["turn_id"], "new")
                self.assertEqual(restored.public_rows()[0]["status"], "unknown")
            finally:
                restored.close()

    def test_turn_completion_is_idle_and_process_end_is_separate(self):
        store = sessions.RuntimeStore()
        for kind, phase in (("session.start", "idle"), ("turn.start", "working"),
                ("approval.requested", "waiting"), ("tool.finish", "working"),
                ("turn.stopped", "idle"), ("turn.complete", "idle"), ("session.end", "ended")):
            self.assertTrue(store.update(event(kind)))
            row = store.public_rows()[0]
            self.assertEqual(row["status"], phase)
            if kind == "turn.stopped":
                self.assertEqual(row["confidence"], "tentative")
            if kind == "approval.requested":
                self.assertEqual(row["confidence"], "observed")
        self.assertEqual(row["health"], "ended")

    def test_late_completion_and_tools_cannot_reopen_or_settle_wrong_turn(self):
        store = sessions.RuntimeStore()
        store.update(event("turn.start", turn_id="old"))
        store.update(event("turn.start", turn_id="new"))
        self.assertFalse(store.update(event("turn.complete", turn_id="old")))
        self.assertEqual(store.public_rows()[0]["status"], "working")
        store.update(event("session.end", turn_id="new"))
        self.assertFalse(store.update(event("tool.finish", turn_id="new")))
        self.assertEqual(store.public_rows()[0]["status"], "ended")
        self.assertTrue(store.update(event("session.start", source="resume")))
        self.assertEqual(store.public_rows()[0]["status"], "idle")

    def test_prompt_without_turn_id_retires_previous_notify_target(self):
        store = sessions.RuntimeStore()
        store.update(event("turn.start", turn_id="old"))
        store.update(event("turn.complete", turn_id="old"))
        store.update(event("turn.start"))
        self.assertFalse(store.update(event("turn.complete", turn_id="old")))
        self.assertEqual(store.public_rows()[0]["status"], "working")
        self.assertTrue(store.update(event("tool.start", turn_id="next")))

    def test_compaction_does_not_reset_working_and_subagents_have_own_identity(self):
        store = sessions.RuntimeStore()
        store.update(event("turn.start", turn_id="one"))
        store.update(event("session.start", source="compact"))
        self.assertEqual(store.public_rows()[0]["status"], "working")
        store.update(event("subagent.start", agent_id="child"))
        self.assertEqual(len(store.public_rows()), 2)
        store.update(event("subagent.end", agent_id="child", tentative=True))
        self.assertEqual(next(r for r in store.public_rows() if r["agent_id"])["confidence"], "tentative")

    def test_duplicate_ordered_events_do_not_repaint_and_retired_attachment_stays_retired(self):
        store = sessions.RuntimeStore()
        first = event("session.snapshot", origin="plugin", engine="opencode", phase="working", incarnation="a", sequence=1)
        self.assertTrue(store.update(first))
        self.assertFalse(store.update({**first, "observed_ns": time.monotonic_ns()}))
        self.assertFalse(store.update({**first, "observed_ns": time.monotonic_ns(), "sequence": 2}))
        self.assertTrue(store.update({**first, "observed_ns": time.monotonic_ns(), "incarnation": "b", "sequence": 1, "phase": "idle"}))
        self.assertFalse(store.update({**first, "observed_ns": time.monotonic_ns(), "sequence": 99}))
        self.assertEqual(store.public_rows()[0]["status"], "idle")

    def test_unproven_t3_thread_and_native_provider_are_not_merged(self):
        store = sessions.RuntimeStore()
        store.update(event())
        store.update(event("session.snapshot", origin="t3", client="t3", phase="idle", environment="env", thread_id="t3thread"))
        self.assertEqual(len(store.public_rows()), 2)
        store.update(event("session.snapshot", engine="unknown", origin="t3", client="t3", phase="working", environment="env", thread_id="t3thread"))
        self.assertEqual(len(store.public_rows()), 2)

    def test_metadata_is_whitelisted_and_dead_identity_is_refused(self):
        store = sessions.RuntimeStore()
        store.update(event(prompt="SECRET", tool_input={"password": "SECRET"}, transcript_path="SECRET"))
        self.assertNotIn("SECRET", json.dumps(store.rows))
        self.assertFalse(store.update(event("turn.start", start_time=1)))
        self.assertEqual(store.public_rows()[0]["status"], "idle")
        for change in ({"observed_ns": True}, {"pid": True}, {"sequence": -1}, {"cwd": "relative"},
                {"capabilities": {"open_browser": "yes"}}, {"schema": 1}):
            with self.assertRaises(ValueError):
                store.update(event(**change))

    def test_process_disconnect_and_restart_restore_no_fabricated_live_activity(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "private/state.json"
            store = sessions.RuntimeStore(path)
            store.update(event("turn.start"))
            store.close()
            restored = sessions.RuntimeStore(path)
            try:
                self.assertEqual(restored.public_rows()[0]["health"], "disconnected")
                self.assertEqual(restored.public_rows()[0]["status"], "unknown")
                restored.update(event("tool.start"))
                row = restored.public_rows()[0]
                self.assertTrue(restored.disconnected(row["pid"], row["start_time"]))
                self.assertEqual(restored.public_rows()[0]["status"], "unknown")
            finally:
                restored.close()

    def test_capacity_and_packet_admission_are_atomic(self):
        store = sessions.RuntimeStore()
        for n in range(32):
            store.update(event(session_id=str(n)))
        before = store.public_rows()
        with self.assertRaises(ValueError):
            store.update(event(session_id="overflow"))
        self.assertEqual(store.public_rows(), before)
        def reject(_):
            raise ValueError("packet limit")
        with self.assertRaises(ValueError):
            store.update(event("turn.start", session_id="1"), reject)
        self.assertEqual(store.public_rows(), before)


class SetupContracts(unittest.TestCase):
    def test_hook_merge_preserves_existing_decision_hooks_and_removes_only_ours(self):
        existing = {"other": {"x": 1}, "hooks": {"Stop": [{"matcher": "x", "hooks": [
            {"type": "command", "command": "my-policy", "timeout": 5}]}]}}
        installed = integrations.merge_hooks(existing, "codex")
        self.assertEqual(installed["other"], existing["other"])
        self.assertEqual(installed["hooks"]["Stop"][0], existing["hooks"]["Stop"][0])
        self.assertEqual(integrations.merge_hooks(installed, "codex"), installed)
        self.assertEqual(integrations.merge_hooks(installed, "codex", remove=True), existing)

    def test_notify_edit_preserves_all_other_toml_and_existing_notifier(self):
        import tomllib
        for source in ('# comment\n[features]\nhooks=true\n',
                       '# comment\nnotify = [\n "old-notify", # keep\n "argument"\n]\n[features]\nhooks=true\n',
                       "'notify' = ['old']\n[features]\nhooks=true\n"):
            before = tomllib.loads(source)
            after = integrations.replace_notify(source, ["new", "path with spaces"])
            self.assertEqual(tomllib.loads(after), {**before, "notify": ["new", "path with spaces"]})
            self.assertIn("[features]\nhooks=true\n", after)

    def test_setup_is_repeatable_and_uninstall_preserves_later_user_edits(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            helper = root / "hook"
            helper.write_text("fixture")
            with mock.patch.object(integrations, "codex_root", return_value=root / "codex"), \
                 mock.patch.object(integrations, "claude_root", return_value=root / "claude"), \
                 mock.patch.object(integrations, "state_root", return_value=root / "state"), \
                 mock.patch.object(integrations, "config_root", return_value=root / "config"), \
                 mock.patch.object(integrations, "helper_path", return_value=helper):
                integrations.setup(("codex", "claude", "opencode"))
                first = (root / "codex/hooks.json").read_bytes()
                integrations.setup(("codex", "claude", "opencode"))
                self.assertEqual((root / "codex/hooks.json").read_bytes(), first)
                path = root / "claude/settings.json"
                config = json.loads(path.read_text());config["userPreference"] = "keep"
                path.write_text(json.dumps(config))
                integrations.setup(("codex", "claude", "opencode"), remove=True)
                self.assertEqual(json.loads(path.read_text())["userPreference"], "keep")
                self.assertNotIn("notify", (root / "codex/config.toml").read_text())

    def test_t3_urls_cannot_carry_credentials_or_plaintext_remote_tokens(self):
        for value in ("http://127.0.0.1:3773", "https://my-t3.example"):
            self.assertEqual(integrations.validate_url(value), value)
        for value in ("javascript:alert(1)", "http://remote.example", "https://u:p@host", "https://host?token=x", "https://host/path"):
            with self.assertRaises(ValueError):
                integrations.validate_url(value)


if __name__ == "__main__":
    unittest.main()
