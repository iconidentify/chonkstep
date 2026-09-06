"""Regression coverage for the native release-package verifier."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
import unittest


VERIFIER = Path(__file__).resolve().parents[1] / "verify-release-package.sh"


class ReleaseVerifierTests(unittest.TestCase):
    def test_early_reader_sigpipe_cannot_be_misreported_as_missing_elf_data(self):
        with tempfile.TemporaryDirectory(prefix="chonk-release-verifier-") as temporary:
            root = Path(temporary)
            tools = root / "tools"
            tools.mkdir()
            package = root / "chonkstep-9.8.7-1-x86_64.pkg.tar.zst"
            debug_package = root / "chonkstep-debug-9.8.7-1-x86_64.pkg.tar.zst"
            package.touch()
            debug_package.touch()

            self.write_tool(tools, "bsdtar", r'''
                from pathlib import Path

                binaries = ("chonkstep", "chonkstep-wayland", "chonk-about",
                            "chonk-netjoin", "omarchy-export-themes")
                if sys.argv[1] == "-xOf":
                    print("pkgname = chonkstep")
                    print("pkgver = 9.8.7-1")
                    print("arch = x86_64")
                elif sys.argv[1] == "-tf":
                    if "debug" in Path(sys.argv[2]).name:
                        for binary in binaries:
                            print(f"usr/lib/debug/usr/bin/{binary}.debug")
                    else:
                        for binary in binaries:
                            print(f"usr/bin/{binary}")
                elif sys.argv[1] == "-xf":
                    stage = Path(sys.argv[sys.argv.index("-C") + 1])
                    (stage / "usr/bin").mkdir(parents=True)
                    (stage / "usr/lib/chonkstep").mkdir(parents=True)
                    verifier = stage / "usr/lib/chonkstep/verify-install.sh"
                    verifier.write_text("#!/bin/sh\nexit 0\n")
                    verifier.chmod(0o755)
                    for binary in binaries:
                        path = stage / "usr/bin" / binary
                        path.write_text(
                            "#!/bin/sh\n"
                            "printf '%s\\n' '" + binary + " 9.8.7' "
                            "'source: test-source' 'build id: abc123'\n"
                        )
                        path.chmod(0o755)
                else:
                    sys.exit(2)
            ''')
            self.write_tool(tools, "readelf", r'''
                import time

                if sys.argv[1] == "-SW":
                    print("[ 1] .eh_frame_hdr PROGBITS")
                    print("[ 2] .eh_frame PROGBITS", flush=True)
                    # An early `grep -q` closes its pipe before this write. The
                    # old verifier therefore returned the producer's SIGPIPE
                    # status under `pipefail`, despite finding the section.
                    time.sleep(0.05)
                    print("[ 3] .padding PROGBITS " + "x" * 131072, flush=True)
                elif sys.argv[1] == "-n":
                    print("    Build ID: abc123")
                else:
                    sys.exit(2)
            ''')
            self.write_tool(
                tools,
                "file",
                "print(f'{sys.argv[-1]}: ELF 64-bit LSB pie executable, x86-64')",
            )
            self.write_tool(tools, "ldd", "print('libc.so.6 => /usr/lib/libc.so.6')")
            # The entire fixture describes synthetic x86_64 ELF/package data.
            # Match its host too, so the pipe regression runs on ARM builders.
            self.write_tool(tools, "uname", "print('x86_64')")
            for command in ("desktop-file-validate", "qmllint"):
                self.write_tool(tools, command, "sys.exit(0)")

            environment = {
                **os.environ,
                "PATH": f"{tools}{os.pathsep}{os.environ['PATH']}",
            }
            unsafe = subprocess.run(
                ["bash", "-o", "pipefail", "-c",
                 "readelf -SW ignored | grep -q '\\.eh_frame'"],
                env=environment, capture_output=True, text=True, timeout=5,
            )
            self.assertNotEqual(
                unsafe.returncode,
                0,
                "fixture must reproduce the old SIGPIPE failure",
            )

            result = subprocess.run(
                [str(VERIFIER), str(package), "x86_64", "9.8.7-1", "test-source"],
                env=environment, capture_output=True, text=True, timeout=10,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("verify-release-package: x86_64 9.8.7-1 passed", result.stdout)

    @staticmethod
    def write_tool(directory, name, body):
        path = directory / name
        source = "#!" + sys.executable + "\nimport sys\n" + textwrap.dedent(body).lstrip()
        path.write_text(source)
        path.chmod(0o755)


if __name__ == "__main__":
    unittest.main()
