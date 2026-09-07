"""Run the real login watcher against isolated logs, sockets, and activation stubs."""
import contextlib
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "wayland-session.sh"


def wait_for(condition, description, timeout=8):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = condition()
        if value:
            return value
        time.sleep(0.025)
    raise AssertionError("timed out waiting for " + description)


class Watcher:
    def __init__(self, directory, historical=b"", trace_reads=False):
        self.root = directory
        self.log = directory / "session.log"
        self.log.write_bytes(historical)
        self.calls = directory / "activation.jsonl"
        self.commands = directory / "commands.jsonl"
        self.sockets = []
        commands = directory / "bin"
        commands.mkdir()
        stub = "#!" + sys.executable + "\n" + '''
import json, os, sys
from pathlib import Path
name = Path(sys.argv[0]).name
with Path(os.environ["WATCHER_COMMANDS"]).open("a") as out:
    out.write(json.dumps([name, *sys.argv[1:]]) + "\\n")
if name in ("stat", "dd"):
    os.execv("/usr/bin/" + name, [name, *sys.argv[1:]])
if name == "dbus-update-activation-environment":
    with Path(os.environ["WATCHER_CALLS"]).open("a") as out:
        out.write(json.dumps(sys.argv[1:]) + "\\n")
elif name not in ("systemctl", "uwsm"):
    raise SystemExit("unexpected scanner subprocess: " + name)
'''
        for name in ("dbus-update-activation-environment", "systemctl", "uwsm", "stat", "dd", "tail", "awk"):
            path = commands / name
            path.write_text(stub)
            path.chmod(0o755)
        source = SCRIPT.read_text()
        body = source.split("publish_portal_env() {\n", 1)[1].split(
            "\n}\n# Capture the append boundary", 1)[0]
        trace = r'''
read() {
    local trace=0 flag key value before=0 after=0 status
    for flag in "$@"; do
        if [[ $flag == -n || $flag == -N ]]; then trace=1; fi
    done
    if [ "$trace" -eq 1 ] && [ -n "${log_fd:-}" ]; then
        while builtin read -r key value; do
            if [ "$key" = pos: ]; then before=$value; break; fi
        done < "/proc/$watcher_pid/fdinfo/$log_fd"
    fi
    builtin read "$@"
    status=$?
    if [ "$trace" -eq 1 ] && [ -n "${log_fd:-}" ]; then
        while builtin read -r key value; do
            if [ "$key" = pos: ]; then after=$value; break; fi
        done < "/proc/$watcher_pid/fdinfo/$log_fd"
        printf '%s\n' "$((after - before))" >> "$WATCHER_READS"
    fi
    return "$status"
}
''' if trace_reads else ""
        function = "set -u\n" + trace + "\npublish_portal_env() {\n" + body + "\n}\npublish_portal_env \"$1\" \"$2\"\n"
        info = self.log.stat()
        boundary = f"{info.st_dev}:{info.st_ino}|{info.st_size}|unused|unused"
        env = {**os.environ, "PATH": str(commands) + os.pathsep + os.environ["PATH"],
               "LOG": str(self.log), "XDG_RUNTIME_DIR": str(directory),
               "XDG_CURRENT_DESKTOP": "chonkstep", "XDG_SESSION_DESKTOP": "chonkstep",
               "XDG_SESSION_TYPE": "wayland", "XDG_MENU_PREFIX": "chonkstep-",
               "XDG_BACKEND": "wayland", "_CHONKSTEP_UWSM": "0",
               "WATCHER_CALLS": str(self.calls), "WATCHER_COMMANDS": str(self.commands),
               "WATCHER_READS": str(self.root / "read-bytes.log")}
        self.error_log = (directory / "watcher.log").open("w")
        self.process = subprocess.Popen(["bash", "-c", function, "watcher", boundary,
                                         os.fsdecode(historical[-256:])], env=env,
                                        stdout=self.error_log, stderr=subprocess.STDOUT,
                                        start_new_session=True)
        try:
            wait_for(lambda: any("count=0" in row for row in self.command_rows()), "initial seek")
        except AssertionError as error:
            self.close()
            raise AssertionError(str(error) + "\n" + (directory / "watcher.log").read_text()) from error

    def command_rows(self):
        return self.rows(self.commands)

    @staticmethod
    def rows(path):
        if not path.exists():
            return []
        rows = []
        for line in path.read_text().splitlines():
            with contextlib.suppress(json.JSONDecodeError):
                rows.append(json.loads(line))
        return rows

    def publications(self):
        return [dict(argument.split("=", 1) for argument in row if "=" in argument)
                for row in self.rows(self.calls)]

    def wait_publication(self, **fields):
        return wait_for(lambda: next((row for row in self.publications()
                                      if all(row.get(key) == value for key, value in fields.items())), None),
                        repr(fields))

    def append(self, data):
        with self.log.open("ab") as out:
            out.write(data.encode() if isinstance(data, str) else data)

    def poll_again(self):
        count = len([row for row in self.command_rows() if row[0] == "stat"])
        wait_for(lambda: len([row for row in self.command_rows() if row[0] == "stat"]) > count,
                 "next watcher poll")

    def bind(self, name):
        server = socket.socket(socket.AF_UNIX)
        server.bind(str(self.root / name))
        self.sockets.append(server)

    def close(self):
        with contextlib.suppress(ProcessLookupError):
            os.killpg(self.process.pid, signal.SIGTERM)
        self.process.wait(timeout=5)
        self.error_log.close()
        for server in self.sockets:
            server.close()


class PortalEnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="cpe-")
        self.root = Path(self.temporary.name)
        self.watcher = None

    def tearDown(self):
        if self.watcher is not None:
            self.watcher.close()
        self.temporary.cleanup()

    def watch(self, historical=b"", trace_reads=False):
        self.watcher = Watcher(self.root, historical, trace_reads=trace_reads)
        return self.watcher

    def test_historical_bytes_are_skipped_and_generation_fields_do_not_leak(self):
        historical = b"old unrelated line\n" * 60000 + (
            b'wayland socket listening socket="wayland-old"\n'
            b'hyprland ipc listening signature="old-signature"\nXWayland ready display=71\n')
        watcher = self.watch(historical)
        watcher.bind("wayland-old")
        watcher.bind("wayland-new")
        watcher.append('wayland socket listening socket="wayland-new"\n'
                       'hyprland ipc listening signature="new-signature"\n')
        first = watcher.wait_publication(WAYLAND_DISPLAY="wayland-new")
        self.assertNotIn("DISPLAY", first)
        self.assertEqual(first["HYPRLAND_INSTANCE_SIGNATURE"], "new-signature")
        watcher.append('\x1b[32mXWayland ready\x1b[0m display=\x1b[35m4\x1b[0m\n')
        watcher.wait_publication(WAYLAND_DISPLAY="wayland-new", DISPLAY=":4")
        watcher.bind("wayland-replacement")
        watcher.append('wayland socket listening socket="wayland-replacement"\n')
        replacement = watcher.wait_publication(WAYLAND_DISPLAY="wayland-replacement")
        self.assertNotIn("DISPLAY", replacement)
        self.assertNotIn("HYPRLAND_INSTANCE_SIGNATURE", replacement)
        self.assertFalse(any(row.get("WAYLAND_DISPLAY") == "wayland-old" for row in watcher.publications()))
        rows = watcher.command_rows()
        self.assertFalse(any(row[0] in ("tail", "awk") for row in rows))
        for row in (row for row in rows if row[0] == "dd"):
            count = int(next(value.split("=", 1)[1] for value in row if value.startswith("count=")))
            self.assertLessEqual(count, 256, "no historical log data is reread")

    def test_partial_line_is_retained_until_newline_and_late_socket_is_retried(self):
        watcher = self.watch()
        watcher.append('non-ASCII café 雪\nwayland socket listening socket="wayland-pa')
        watcher.poll_again()
        self.assertEqual(watcher.publications(), [])
        watcher.append('rtial"')
        watcher.poll_again()
        self.assertEqual(watcher.publications(), [])
        watcher.append('\n')
        watcher.poll_again()
        self.assertEqual(watcher.publications(), [])
        watcher.bind("wayland-partial")
        watcher.wait_publication(WAYLAND_DISPLAY="wayland-partial")

    def test_rotation_drops_partial_line_but_retains_current_generation(self):
        watcher = self.watch()
        watcher.bind("wayland-active")
        watcher.append('wayland socket listening socket="wayland-active"\n')
        watcher.wait_publication(WAYLAND_DISPLAY="wayland-active")
        watcher.append('wayland socket listening socket="unfin')
        watcher.poll_again()
        watcher.log.rename(self.root / "old.log")
        watcher.log.write_text('XWayland ready display=5\n')
        watcher.wait_publication(WAYLAND_DISPLAY="wayland-active", DISPLAY=":5")
        watcher.bind("wayland-next")
        replacement = self.root / "replacement.log"
        replacement.write_text('wayland socket listening socket="wayland-next"\n')
        replacement.replace(watcher.log)
        final = watcher.wait_publication(WAYLAND_DISPLAY="wayland-next")
        self.assertNotIn("DISPLAY", final)

    def test_copytruncate_and_regrowth_beyond_cursor_does_not_skip_replacement(self):
        watcher = self.watch()
        watcher.bind("wayland-before")
        watcher.bind("wayland-after")
        watcher.append('wayland socket listening socket="wayland-before"\n'
                       'hyprland ipc listening signature="before"\nXWayland ready display=9\n')
        watcher.wait_publication(WAYLAND_DISPLAY="wayland-before", DISPLAY=":9")
        size = watcher.log.stat().st_size
        watcher.log.write_text('wayland socket listening socket="wayland-after"\n' + "new harmless line\n" * size)
        self.assertGreater(watcher.log.stat().st_size, size)
        final = watcher.wait_publication(WAYLAND_DISPLAY="wayland-after")
        self.assertNotIn("DISPLAY", final)
        self.assertNotIn("HYPRLAND_INSTANCE_SIGNATURE", final)

    def test_binary_log_chunk_has_bounded_reads_and_recovers(self):
        watcher = self.watch(trace_reads=True)
        watcher.bind("wayland-after-binary")
        watcher.append(b"\0" * (512 * 1024) + b"\n")
        watcher.poll_again()
        watcher.poll_again()
        reads = [int(value) for value in (self.root / "read-bytes.log").read_text().splitlines()]
        self.assertTrue(reads)
        self.assertLessEqual(max(reads), 65536, "each read must honor the byte bound even through NULs")
        self.assertLess(sum(reads), 100 * 1024, "binary chunks must be skipped, not scanned byte by byte")
        watcher.append('wayland socket listening socket="wayland-after-binary"\n')
        watcher.wait_publication(WAYLAND_DISPLAY="wayland-after-binary")

    def test_overlong_or_nul_line_cannot_splice_an_environment_event(self):
        watcher = self.watch()
        watcher.bind("wayland-valid")
        watcher.append(b"x" * 70000 + b'wayland socket listening socket="wayland-valid"\n')
        watcher.poll_again()
        watcher.poll_again()
        self.assertEqual(watcher.publications(), [])
        watcher.append(b'wayland socket listen\x00ing socket="wayland-valid"\n')
        watcher.poll_again()
        watcher.poll_again()
        self.assertEqual(watcher.publications(), [])
        watcher.append('wayland socket listening socket="wayland-valid"\n')
        watcher.wait_publication(WAYLAND_DISPLAY="wayland-valid")


if __name__ == "__main__":
    unittest.main()
