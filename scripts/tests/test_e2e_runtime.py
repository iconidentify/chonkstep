"""The headless display's sockets must not inherit an arbitrarily long TMPDIR."""

import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


E2E = Path(__file__).resolve().parents[1] / "e2e.sh"


class HeadlessRuntimeTests(unittest.TestCase):
    def test_long_artifact_directory_does_not_lengthen_display_socket_paths(self):
        with tempfile.TemporaryDirectory(prefix="chonk-e2e-runtime-") as temporary:
            root = Path(temporary)
            artifacts = root / ("artifacts-" + "x" * 100)
            artifacts.mkdir()
            commands = root / "commands"
            commands.mkdir()
            log = root / "observed.json"
            weston = commands / "weston"
            weston.write_text(f"#!{sys.executable}\n" + '''
import os
import signal
import socket
import sys

name = next(arg.split("=", 1)[1] for arg in sys.argv if arg.startswith("--socket="))
server = socket.socket(socket.AF_UNIX)
server.bind(os.path.join(os.environ["XDG_RUNTIME_DIR"], name))
signal.pause()
''')
            weston.chmod(0o755)
            cargo = commands / "cargo"
            cargo.write_text(f"#!{sys.executable}\n" + '''
import json
import os
from pathlib import Path
import stat

runtime = Path(os.environ["XDG_RUNTIME_DIR"])
Path(os.environ["CHONK_RUNTIME_TEST_LOG"]).write_text(json.dumps({
    "runtime": str(runtime), "mode": stat.S_IMODE(runtime.stat().st_mode),
    "artifacts": os.environ["TMPDIR"],
}))
raise SystemExit(42)
''')
            cargo.chmod(0o755)
            for command in ("foot", "alacritty", "zenity", "grim", "wlr-randr", "dbus-run-session"):
                (commands / command).symlink_to(cargo)
            env = {**os.environ, "PATH": f"{commands}{os.pathsep}{os.environ['PATH']}",
                   "TMPDIR": str(artifacts), "CHONK_RUNTIME_TEST_LOG": str(log)}
            env.pop("CHONKSTEP_WAYLAND_BIN", None)
            result = subprocess.run([str(E2E), "--headless", "--release"], env=env,
                                    capture_output=True, text=True, timeout=15)
            self.assertEqual(result.returncode, 42, result.stdout + result.stderr)
            observed = json.loads(log.read_text())
            self.assertEqual(observed["artifacts"], str(artifacts))
            self.assertEqual(observed["mode"], 0o700)
            # Include the terminating NUL required by pathname Unix sockets.
            longest = Path(observed["runtime"]) / "hypr" / (
                "chonkstep_18446744073709551615_4294967295") / ".hyprsunset.sock"
            self.assertLessEqual(len(os.fsencode(longest)) + 1, 108)
            self.assertFalse(Path(observed["runtime"]).exists(), "owned runtime is cleaned on failure")
            self.assertTrue(artifacts.is_dir(), "artifact directory is not the cleanup target")


if __name__ == "__main__":
    unittest.main()
