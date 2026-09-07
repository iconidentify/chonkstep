"""A release can reuse proof, never another revision's or a failed validation."""

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


def run_record(**overrides):
    return {
        "head_sha": SHA, "head_branch": "main", "event": "push",
        "repository": {"full_name": REPO}, "conclusion": "success",
        "run_started_at": (datetime.now(timezone.utc) - timedelta(minutes=5)).strftime("%Y-%m-%dT%H:%M:%SZ"),
        "html_url": f"https://github.com/{REPO}/actions/runs/123",
        **overrides,
    }


class ReleaseValidationTests(unittest.TestCase):
    def proof(self, runs, api_exit=0):
        with tempfile.TemporaryDirectory(prefix="chonk-release-proof-") as directory:
            root = Path(directory)
            response = root / "response.json"
            response.write_text(runs if isinstance(runs, str) else json.dumps({"workflow_runs": runs}))
            gh = root / "gh"
            gh.write_text(f"#!{sys.executable}\n" + "\n".join([
                "import os, sys",
                "from pathlib import Path",
                "print(Path(os.environ['CHONK_PROOF_RESPONSE']).read_text())",
                "sys.exit(int(os.environ['CHONK_PROOF_EXIT']))",
            ]))
            gh.chmod(0o755)
            output = root / "outputs"
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/release-validation.sh")],
                capture_output=True, text=True, timeout=10,
                env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                     "GITHUB_REPOSITORY": REPO, "GITHUB_SHA": SHA,
                     "GITHUB_OUTPUT": str(output), "CHONK_PROOF_RESPONSE": str(response),
                     "CHONK_PROOF_EXIT": str(api_exit)},
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            return output.read_text().strip(), result.stdout

    def test_recent_exact_main_commit_is_reused_with_traceable_run(self):
        output, log = self.proof([run_record()])
        self.assertEqual(output, "reusable=true")
        self.assertIn("/actions/runs/123", log)

    def test_wrong_revision_context_stale_or_unsuccessful_run_requires_fresh_ci(self):
        for override in [
            {"head_sha": "b" * 40}, {"head_branch": "feature"}, {"event": "pull_request"},
            {"repository": {"full_name": "someone/fork"}}, {"conclusion": "failure"},
            {"conclusion": "cancelled"}, {"conclusion": None},
            {"run_started_at": "2020-01-01T00:00:00Z"}, {"html_url": None},
        ]:
            with self.subTest(override=override):
                output, _ = self.proof([run_record(**override)])
                self.assertEqual(output, "reusable=false")

    def test_latest_failure_cannot_be_hidden_by_an_earlier_success(self):
        earlier = run_record(run_started_at=(datetime.now(timezone.utc) - timedelta(hours=1)).strftime("%Y-%m-%dT%H:%M:%SZ"))
        output, _ = self.proof([run_record(conclusion="failure"), earlier])
        self.assertEqual(output, "reusable=false")

    def test_missing_or_malformed_api_data_falls_back_to_all_validation(self):
        for response, exit_code in [("not json", 0), ([], 0), ("{}", 0), ([run_record()], 1)]:
            with self.subTest(response=response, exit_code=exit_code):
                output, _ = self.proof(response, exit_code)
                self.assertEqual(output, "reusable=false")


if __name__ == "__main__":
    unittest.main()
