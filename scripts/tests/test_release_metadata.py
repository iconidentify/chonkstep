"""A tag must promote the same commit identity and the complete verified asset set."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ReleaseMetadataTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="chonk-release-metadata-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        (self.root / "packaging/arch").mkdir(parents=True)
        for script in ("release-metadata.sh", "verify-release-assets.sh"):
            shutil.copyfile(ROOT / "scripts" / script, self.root / "scripts" / script)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "0.4.1"\n')
        self.recipe = self.root / "packaging/arch/PKGBUILD-release"
        self.recipe.write_text("pkgver=0.4.1\npkgrel=1\n")
        self.git("init", "-q")
        self.git("add", ".")
        self.git("-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "fixture")
        self.sha = self.git("rev-parse", "HEAD").strip()
        self.env = {**os.environ, "GITHUB_SHA": self.sha, "GITHUB_REF": "refs/heads/main",
                    "GITHUB_REPOSITORY": "owner/project"}

    def git(self, *args):
        return subprocess.run(["git", *args], cwd=self.root, check=True, capture_output=True,
                              text=True, timeout=10).stdout

    def run_script(self, name="release-metadata.sh", *args, expected_exit=0):
        result = subprocess.run(["bash", str(self.root / "scripts" / name), *args],
                                cwd=self.root, env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, expected_exit, result.stderr)
        return result.stdout

    def test_adding_a_release_tag_preserves_the_built_source_identity(self):
        before = self.run_script()
        self.git("tag", "preview-v0.4.1")
        self.env["GITHUB_REF"] = "refs/tags/preview-v0.4.1"
        after = self.run_script()
        self.assertEqual(before, after)
        self.assertIn(f"source_id=v0.4.1+git.{self.sha}", after)

    def test_tag_version_and_package_revision_must_match(self):
        for tag in ("preview-v0.4.0", "preview-v0.4.1-r2", "preview-v0.4.1-r0"):
            with self.subTest(tag=tag):
                self.env["GITHUB_REF"] = f"refs/tags/{tag}"
                self.run_script(expected_exit=1)
        self.recipe.write_text("pkgver=0.4.1\npkgrel=2\n")
        self.env["GITHUB_REF"] = "refs/tags/preview-v0.4.1-r2"
        self.assertIn("package_release=2", self.run_script())
        self.env["GITHUB_REF"] = "refs/tags/preview-v0.4.1"
        self.run_script(expected_exit=1)

    def test_mismatched_recipe_or_checkout_is_rejected_before_build_or_promotion(self):
        self.recipe.write_text("pkgver=0.4.0\npkgrel=1\n")
        self.run_script(expected_exit=1)
        self.recipe.write_text("pkgver=0.4.1\npkgrel=1\n")
        self.env["GITHUB_SHA"] = "b" * 40
        self.run_script(expected_exit=1)

    def prepare_assets(self):
        assets = self.root / "dist"
        assets.mkdir()
        for arch in ("x86_64", "aarch64"):
            for package in ("chonkstep", "chonkstep-debug"):
                (assets / f"{package}-0.4.1-1-{arch}.pkg.tar.zst").write_bytes(b"test asset")
        log = self.root / "attestation-calls.jsonl"
        gh = self.root / "gh"
        gh.write_text(f"#!{sys.executable}\n" + "\n".join([
            "import json, os, sys",
            "with open(os.environ['CHONK_ATTEST_LOG'], 'a') as f: f.write(json.dumps(sys.argv[1:]) + '\\n')",
            "sys.exit(1 if os.environ.get('CHONK_ATTEST_FAIL') in sys.argv[3] else 0)",
        ]))
        gh.chmod(0o755)
        self.env.update(PATH=f"{self.root}:{os.environ['PATH']}", CHONK_ATTEST_LOG=str(log), CHONK_ATTEST_FAIL="__none__")
        return assets, log

    def test_all_four_assets_require_the_exact_source_and_signer_digest(self):
        assets, log = self.prepare_assets()
        self.run_script("verify-release-assets.sh", str(assets))
        calls = [json.loads(line) for line in log.read_text().splitlines()]
        self.assertEqual(len(calls), 4)
        for call in calls:
            self.assertEqual(call[:2], ["attestation", "verify"])
            for flag in ("--source-digest", "--signer-digest"):
                self.assertEqual(call[call.index(flag) + 1], self.sha)
            self.assertEqual(call[call.index("--signer-workflow") + 1], "owner/project/.github/workflows/package.yml")
            self.assertIn("--deny-self-hosted-runners", call)

    def test_missing_symbols_or_wrong_version_prevents_promotion(self):
        assets, log = self.prepare_assets()
        package = assets / "chonkstep-debug-0.4.1-1-aarch64.pkg.tar.zst"
        package.rename(assets / "chonkstep-debug-0.4.0-1-aarch64.pkg.tar.zst")
        self.run_script("verify-release-assets.sh", str(assets), expected_exit=1)
        (assets / "chonkstep-debug-0.4.0-1-aarch64.pkg.tar.zst").unlink()
        log.unlink()
        self.run_script("verify-release-assets.sh", str(assets), expected_exit=1)
        self.assertFalse(log.exists())

    def test_failed_attestation_stops_promotion(self):
        assets, log = self.prepare_assets()
        self.env["CHONK_ATTEST_FAIL"] = "debug"
        self.run_script("verify-release-assets.sh", str(assets), expected_exit=1)
        self.assertEqual(len(log.read_text().splitlines()), 2)


if __name__ == "__main__":
    unittest.main()
