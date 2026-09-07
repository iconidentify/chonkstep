"""The fast build must still provide every executable that packaging installs."""

import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ShippingBuildTests(unittest.TestCase):
    def test_build_matches_both_package_manifests_and_propagates_failure(self):
        with tempfile.TemporaryDirectory(prefix="chonk-shipping-build-") as directory:
            root = Path(directory)
            cargo = root / "cargo"
            log = root / "arguments.json"
            cargo.write_text(f"#!{sys.executable}\n" + "\n".join([
                "import json, os, sys",
                "from pathlib import Path",
                "Path(os.environ['CHONK_BUILD_TEST_LOG']).write_text(json.dumps(sys.argv[1:]))",
                "sys.exit(42)",
            ]))
            cargo.chmod(0o755)
            result = subprocess.run(
                ["bash", str(ROOT / "scripts/build-release.sh"), "--frozen"],
                cwd=root, timeout=10, capture_output=True, text=True,
                env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                     "CHONK_BUILD_TEST_LOG": str(log)},
            )
            self.assertEqual(result.returncode, 42, result.stderr)
            args = json.loads(log.read_text())
            binaries = {args[i + 1] for i, value in enumerate(args) if value == "--bin"}
            for recipe in ("PKGBUILD", "PKGBUILD-release"):
                installed = set(re.findall(r"install -Dm755 target/release/([\w-]+)",
                                          (ROOT / "packaging/arch" / recipe).read_text()))
                self.assertTrue(installed)
                self.assertEqual(binaries, installed)
            self.assertIn("--frozen", args)
            self.assertIn("--release", args)


if __name__ == "__main__":
    unittest.main()
