"""Exercise installation transactions and real bundled Python examples."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import tomllib
import unittest


DOCK = Path(__file__).resolve().parents[1]


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='dock "installer" ')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.env = {**os.environ, "XDG_DATA_HOME": str(self.root / "data"),
                    "XDG_CONFIG_HOME": str(self.root / "config")}
        self.registry = self.root / "config/chonkstep/dockapps"
        self.installed = self.root / "data/chonkstep/dockapps"

    def run_tool(self, *args, ok=True):
        result = subprocess.run([sys.executable, str(DOCK / "chonk-get"), *map(str, args)],
                                env=self.env, text=True, capture_output=True)
        self.assertEqual(result.returncode == 0, ok, result.stdout + result.stderr)
        return result

    def source(self):
        source = self.root / "checkout"
        source.mkdir()
        (source / "clock.py").write_text("print('clock')\n")
        (source / "clock.dockapp").write_text('id = "my-clock"\nexec = ["clock.py", "a \\\"quote\\\""]\n')
        return source

    def test_shipped_id_paths_quotes_list_and_remove(self):
        self.run_tool("install", self.source())
        data = tomllib.loads((self.registry / "my-clock.dockapp").read_text())
        self.assertEqual(data["exec"], ["python3", str(self.installed / "my-clock/clock.py"), 'a "quote"'])
        self.assertIn("my-clock", self.run_tool("list").stdout)
        self.run_tool("remove", "my-clock")
        self.assertFalse((self.installed / "my-clock").exists())
        self.assertFalse((self.registry / "my-clock.dockapp").exists())

    def test_failed_upgrade_keeps_previous_program_and_registration(self):
        source = self.source()
        self.run_tool("install", source)
        previous = (self.registry / "my-clock.dockapp").read_bytes()
        (source / "clock.py").write_text("changed\n")
        (source / "build.sh").write_text("#!/bin/sh\nexit 9\n")
        (source / "build.sh").chmod(0o755)
        self.run_tool("install", source, ok=False)
        self.assertEqual((self.registry / "my-clock.dockapp").read_bytes(), previous)
        self.assertEqual((self.installed / "my-clock/clock.py").read_text(), "print('clock')\n")

    def test_successful_upgrade_replaces_program(self):
        source = self.source()
        self.run_tool("install", source)
        (source / "clock.py").write_text("upgraded\n")
        self.run_tool("install", source)
        self.assertEqual((self.installed / "my-clock/clock.py").read_text(), "upgraded\n")

    def test_traversal_names_cannot_remove_parent(self):
        for name in (".", "..", "../other", "builtin:clock", "/tmp/no"):
            self.run_tool("remove", name, ok=False)
        self.assertTrue(self.root.exists())
        source = self.source()
        (source / "clock.dockapp").write_text('id = ".."\nexec = ["clock.py"]\n')
        self.run_tool("install", source, ok=False)

    def test_explicit_program_and_options(self):
        self.run_tool("install", self.source(), "--name", "renamed", "--exec", "clock.py",
                      "--label", 'a "label"', "--tile-units", "2", "--restart", "never")
        data = tomllib.loads((self.registry / "renamed.dockapp").read_text())
        self.assertEqual((data["name"], data["tile_units"], data["restart"]), ('a "label"', 2, "never"))

    def test_bundled_python_clock_installs_sdk_and_valid_executable(self):
        self.run_tool("install", DOCK / "bindings/python")
        data = tomllib.loads((self.registry / "py-dockclock.dockapp").read_text())
        self.assertTrue(Path(data["exec"][0]).is_file())
        self.assertTrue((self.installed / "py-dockclock/chonkdock/__init__.py").is_file())

    def test_bundled_switch_vendors_sdk(self):
        self.run_tool("install", DOCK / "examples/chonk-switch")
        self.assertTrue((self.installed / "chonk-switch/chonkdock/__init__.py").is_file())
        self.assertTrue((self.registry / "chonk-switch.dockapp").is_file())

    def test_workspace_member_build_uses_original_manifest_and_copies_binary(self):
        source = self.source()
        (source / "Cargo.toml").write_text('[package]\nname="my-clock"\nversion.workspace=true\n')
        (source / "clock.dockapp").write_text('id="my-clock"\nexec=["my-clock"]\n')
        target = self.root / "workspace-target"
        (target / "release").mkdir(parents=True)
        (target / "release/my-clock").write_text("#!/bin/sh\nexit 0\n")
        (target / "release/my-clock").chmod(0o755)
        tools = self.root / "bin"
        tools.mkdir()
        metadata = {"packages": [{"manifest_path": str(source / "Cargo.toml"), "name": "my-clock",
                                   "targets": [{"kind": ["bin"], "name": "my-clock"}]}],
                    "target_directory": str(target)}
        (tools / "cargo").write_text(f"#!{sys.executable}\nimport sys\n"
                                    f"assert sys.argv[sys.argv.index('--manifest-path') + 1] == {str(source / 'Cargo.toml')!r}\n"
                                    f"if sys.argv[1] == 'metadata': print({json.dumps(metadata)!r})\n")
        (tools / "cargo").chmod(0o755)
        self.env["PATH"] = str(tools) + os.pathsep + self.env["PATH"]
        self.run_tool("install", source)
        data = tomllib.loads((self.registry / "my-clock.dockapp").read_text())
        self.assertEqual(data["exec"], [str(self.installed / "my-clock/target/release/my-clock")])
        self.assertTrue(os.access(data["exec"][0], os.X_OK))


if __name__ == "__main__":
    unittest.main()
