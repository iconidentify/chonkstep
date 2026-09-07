"""Release promotion reuses only complete, recent evidence for the exact commit."""

from datetime import datetime, timedelta, timezone
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SHA = "a" * 40
REPO = "owner/project"


def timestamp(**delta):
    return (datetime.now(timezone.utc) + timedelta(**delta)).strftime("%Y-%m-%dT%H:%M:%SZ")


def run_record(**overrides):
    return {
        "id": 123, "head_sha": SHA, "head_branch": "main", "event": "push",
        "repository": {"full_name": REPO, "id": 55},
        "status": "completed", "conclusion": "success",
        "run_started_at": timestamp(minutes=-5),
        **overrides,
    }


def artifact(arch="x86_64", **overrides):
    return {
        "id": 456 if arch == "x86_64" else 789,
        "name": f"chonkstep-package-{arch}", "expired": False,
        "expires_at": timestamp(days=14),
        "workflow_run": {"id": 123, "head_sha": SHA, "head_branch": "main",
                         "repository_id": 55, "head_repository_id": 55},
        **overrides,
    }


class ReleaseValidationTests(unittest.TestCase):
    def proof(self, runs, artifacts=None, completed=None, fail=None, expected_exit=0):
        with tempfile.TemporaryDirectory(prefix="chonk-release-proof-") as directory:
            root = Path(directory)
            responses = {
                "runs": runs if isinstance(runs, str) else {"workflow_runs": runs},
                "artifacts": {"artifacts": artifacts if artifacts is not None else [artifact(), artifact("aarch64")]},
                "completed": completed if completed is not None else run_record(),
            }
            response = root / "responses.json"
            response.write_text(json.dumps(responses))
            log = root / "calls.jsonl"
            gh = root / "gh"
            gh.write_text(f"#!{sys.executable}\n" + "\n".join([
                "import json, os, sys",
                "from pathlib import Path",
                "args = sys.argv[1:]",
                "with open(os.environ['CHONK_PROOF_LOG'], 'a') as f: f.write(json.dumps(args) + '\\n')",
                "key = 'watch' if args[:2] == ['run', 'watch'] else ('runs' if any('/workflows/' in a for a in args) else ('artifacts' if any(a.endswith('/artifacts') for a in args) else 'completed'))",
                "if key == os.environ.get('CHONK_PROOF_FAIL'): sys.exit(7)",
                "if key == 'watch': sys.exit(0)",
                "result = json.loads(Path(os.environ['CHONK_PROOF_RESPONSE']).read_text())[key]",
                "print(result if isinstance(result, str) else json.dumps(result))",
            ]))
            gh.chmod(0o755)
            output = root / "outputs"
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/release-validation.sh")],
                capture_output=True, text=True, timeout=10,
                env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                     "GITHUB_REPOSITORY": REPO, "GITHUB_SHA": SHA,
                     "GITHUB_OUTPUT": str(output), "CHONK_PROOF_RESPONSE": str(response),
                     "CHONK_PROOF_LOG": str(log), "CHONK_PROOF_FAIL": fail or ""},
            )
            self.assertEqual(result.returncode, expected_exit, result.stderr)
            outputs = dict(line.split("=", 1) for line in output.read_text().splitlines()) if output.exists() else {}
            return outputs, result.stdout, [json.loads(line) for line in log.read_text().splitlines()]

    def test_recent_exact_main_commit_promotes_both_immutable_artifacts(self):
        output, log, calls = self.proof([run_record()])
        self.assertEqual(output, {"reusable": "true", "package_run_id": "123", "package_artifact_ids": "789,456"})
        self.assertIn("/actions/runs/123", log)
        self.assertIn(f"head_sha={SHA}", calls[0])
        self.assertNotIn("status=completed", calls[0])

    def test_wrong_revision_context_stale_or_unsuccessful_run_requires_fresh_ci(self):
        for override in [
            {"head_sha": "b" * 40}, {"head_branch": "feature"}, {"event": "pull_request"},
            {"repository": {"full_name": "someone/fork"}}, {"conclusion": "failure"},
            {"conclusion": "cancelled"}, {"conclusion": None},
            {"run_started_at": "2020-01-01T00:00:00Z"}, {"id": None},
        ]:
            with self.subTest(override=override):
                output, _, _ = self.proof([run_record(**override)])
                self.assertEqual(output["reusable"], "false")
                self.assertEqual(output["package_run_id"], "")

    def test_latest_failure_cannot_be_hidden_by_an_earlier_success(self):
        earlier = run_record(run_started_at=timestamp(hours=-1))
        output, _, _ = self.proof([run_record(conclusion="failure"), earlier])
        self.assertEqual(output["reusable"], "false")

    def test_tag_waits_for_active_main_instead_of_starting_competing_builds(self):
        for status in ("queued", "in_progress", "waiting"):
            with self.subTest(status=status):
                earlier = run_record(id=122, run_started_at=timestamp(hours=-1))
                output, _, calls = self.proof([earlier, run_record(status=status, conclusion=None)])
                self.assertEqual(output["package_run_id"], "123")
                self.assertEqual(calls[1][:3], ["run", "watch", "123"])
                self.assertIn(f"repos/{REPO}/actions/runs/123", calls[2])

    def test_failed_main_after_wait_cannot_be_promoted(self):
        output, _, _ = self.proof([run_record(status="in_progress", conclusion=None)],
                                  completed=run_record(conclusion="failure"))
        self.assertEqual(output["reusable"], "false")
        self.assertEqual(output["package_artifact_ids"], "")

    def test_failed_wait_or_status_refresh_stops_release_with_no_reuse_outputs(self):
        for fail in ("watch", "completed"):
            with self.subTest(fail=fail):
                output, _, _ = self.proof([run_record(status="in_progress", conclusion=None)],
                                          fail=fail, expected_exit=7)
                self.assertEqual(output, {})

    def test_missing_or_malformed_api_data_falls_back_to_all_validation(self):
        for response, fail in [("not json", None), ([], None), ("{}", None), ([run_record()], "runs")]:
            with self.subTest(response=response, fail=fail):
                output, _, _ = self.proof(response, fail=fail)
                self.assertEqual(output["reusable"], "false")
                self.assertEqual(output["package_run_id"], "")

    def test_partial_expired_duplicate_or_wrong_source_artifacts_rebuild_packages_only(self):
        bad_pairs = [[], [artifact()], [artifact(), artifact()],
                     [artifact(), artifact("aarch64", expired=True)],
                     [artifact(), artifact("aarch64", expires_at=timestamp(minutes=-1))]]
        for field, value in [("id", 999), ("head_sha", "b" * 40), ("head_branch", "feature"), ("head_repository_id", 99)]:
            wrong = artifact("aarch64")
            wrong["workflow_run"][field] = value
            bad_pairs.append([artifact(), wrong])
        for artifacts in bad_pairs:
            with self.subTest(artifacts=artifacts):
                output, _, _ = self.proof([run_record()], artifacts=artifacts)
                self.assertEqual(output, {"reusable": "true", "package_run_id": "", "package_artifact_ids": ""})

    def test_artifact_api_failure_preserves_validation_but_requires_package_builds(self):
        output, _, _ = self.proof([run_record()], fail="artifacts")
        self.assertEqual(output, {"reusable": "true", "package_run_id": "", "package_artifact_ids": ""})


if __name__ == "__main__":
    unittest.main()
