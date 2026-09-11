"""Recorder command isolation and failures; real video is checked in Wayland E2E."""
import fcntl
import os
from pathlib import Path
import socket
import signal
import subprocess
import sys
import tempfile
import time
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'chonkrec'


class RecorderCommands(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='chonkrec-test-')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root/'bin'
        self.bin.mkdir()
        self.env = {k:v for k,v in os.environ.items()
                    if not k.startswith('CHONKREC_') and k not in ('WAYLAND_DISPLAY', 'WAYLAND_SOCKET')}
        self.env.update(HOME=str(self.root), XDG_RUNTIME_DIR=str(self.root),
                        PATH=str(self.bin)+os.pathsep+self.env.get('PATH', ''),
                        CHONKREC_DIR=str(self.root/'videos'))
        for name in ('wf-recorder', 'ffmpeg', 'ffprobe'):
            self.program(name, 'raise SystemExit(97)\n')
        self.program('wlr-randr', 'print(\'HEADLESS-1 "fixture"\\n  Enabled: yes\\n  Transform: normal\')\n')
        self.program('wayland-info', 'print("interface: \'wl_output\', version: 4, name: 1\\n\\tname: HEADLESS-1\\n\\tsubpixel_orientation: unknown, output_transform: normal,")\n')

    def program(self, name, body):
        path = self.bin/name
        path.write_text(f'#!{sys.executable}\n'+body)
        path.chmod(0o755)

    def socket(self, name):
        sock = socket.socket(socket.AF_UNIX)
        sock.bind(str(self.root/name))
        sock.listen()
        self.addCleanup(sock.close)

    def run_command(self, *args, timeout=12):
        return subprocess.run([str(SCRIPT), *args], env=self.env,
                              capture_output=True, text=True, timeout=timeout)

    def test_help_does_not_require_a_running_desktop(self):
        self.env.pop('XDG_RUNTIME_DIR')
        result = self.run_command('--help')
        self.assertEqual(result.returncode, 0)
        self.assertIn('--demo', result.stderr)

    def test_demo_does_not_silently_fall_back_to_clean_capture(self):
        self.socket('wayland-1')
        self.env['WAYLAND_DISPLAY'] = 'wayland-1'
        result = self.run_command('start', '--demo')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('demo capture is unavailable', result.stderr)
        self.assertFalse((self.root/'chonkrec.pid').exists())

    def test_a_missing_selected_desktop_does_not_switch_to_another(self):
        self.socket('wayland-2')
        self.env['WAYLAND_DISPLAY'] = str(self.root/'wayland-1')
        result = self.run_command('start')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('set WAYLAND_DISPLAY', result.stderr)
        self.assertFalse((self.root/'videos').exists())

    def test_ambiguous_desktops_are_not_chosen_arbitrarily(self):
        self.socket('wayland-1')
        self.socket('wayland-2')
        self.socket('chonkstep-capture-wayland-1')
        result = self.run_command('start')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('unambiguous Wayland desktop', result.stderr)

    def test_multiple_monitors_require_an_explicit_selection(self):
        self.socket('wayland-1')
        self.env['WAYLAND_DISPLAY'] = 'wayland-1'
        self.program('wlr-randr', 'print(\'DP-1 "left"\\n  Enabled: yes\\n  Transform: normal\\nDP-2 "right"\\n  Enabled: yes\\n  Transform: 90\')\n')
        result = self.run_command('start')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('use -O NAME', result.stderr)
        self.assertFalse((self.root/'chonkrec.pid').exists())

    def test_a_disabled_selected_monitor_does_not_record_another(self):
        self.socket('wayland-1')
        self.env['WAYLAND_DISPLAY'] = 'wayland-1'
        self.program('wlr-randr', 'print(\'DP-1 "left"\\n  Enabled: yes\\n  Transform: normal\\nDP-2 "right"\\n  Enabled: no\\n  Transform: 90\')\n')
        result = self.run_command('start', '-O', 'DP-2')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('use -O NAME', result.stderr)
        self.assertFalse((self.root/'chonkrec.pid').exists())

    def test_stale_pid_file_cannot_identify_an_unrelated_process_as_the_recorder(self):
        (self.root/'chonkrec.pid').write_text(str(os.getpid()))
        (self.root/'chonkrec.work').write_text(str(self.root/'work'))
        result = self.run_command('status')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not recording', result.stderr)

    def test_a_second_command_cannot_race_start_or_stop(self):
        with (self.root/'chonkrec.command.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            result = self.run_command('start', '--demo')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('another chonkrec command is running', result.stderr)

    def test_encoder_start_failure_reaps_workers_and_keeps_diagnostics(self):
        self.socket('wayland-1')
        self.socket('chonkstep-capture-wayland-1')
        self.env['WAYLAND_DISPLAY'] = 'wayland-1'
        self.env['CHONKREC_TEST_PIDS'] = str(self.root/'workers')
        record_pid = "import os,sys\nwith open(os.environ['CHONKREC_TEST_PIDS'],'a') as f:f.write(str(os.getpid())+'\\n')\n"
        # Fail a real FIFO producer/consumer pair, without pretending to encode.
        self.program('wf-recorder', record_pid+
                     "with open(sys.argv[-1],'wb') as f:f.write(b'invalid input')\nraise SystemExit(7)\n")
        self.program('ffmpeg', record_pid+
                     "with open(sys.argv[sys.argv.index('-i')+1],'rb') as f:f.read()\nraise SystemExit(8)\n")
        result = self.run_command('start', '--demo', timeout=25)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('recording failed to start', result.stderr)
        self.assertFalse((self.root/'chonkrec.pid').exists())
        work = next(self.root.glob('chonkrec.*/supervisor.log'))
        self.assertTrue(work.parent.joinpath('stop').exists())
        self.assertTrue(work.parent.joinpath('finished').exists())
        for pid in map(int, (self.root/'workers').read_text().splitlines()):
            self.assertFalse(Path('/proc', str(pid)).exists(), f'worker {pid} survived startup failure')

    def test_interrupting_startup_reaps_its_workers(self):
        self.socket('wayland-1')
        self.env['WAYLAND_DISPLAY'] = 'wayland-1'
        self.env['CHONKREC_TEST_PIDS'] = str(self.root/'workers')
        body = """import os,signal,time
signal.signal(signal.SIGINT, lambda *_: exit(0))
with open(os.environ['CHONKREC_TEST_PIDS'],'a') as f:f.write(str(os.getpid())+'\\n')
while True:time.sleep(.1)
"""
        self.program('wf-recorder', body)
        self.program('ffmpeg', body)
        with subprocess.Popen([str(SCRIPT), 'start'], env=self.env,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as command:
            deadline = time.monotonic()+5
            while time.monotonic() < deadline:
                if (self.root/'workers').exists() and len((self.root/'workers').read_text().splitlines()) == 2:
                    break
                time.sleep(.05)
            else:
                command.terminate()
                self.fail('startup did not launch its workers')
            command.send_signal(signal.SIGINT)
            _, error = command.communicate(timeout=20)
        self.assertEqual(command.returncode, 130, error)
        self.assertIn('diagnostics retained', error)
        self.assertFalse((self.root/'chonkrec.pid').exists())
        for pid in map(int, (self.root/'workers').read_text().splitlines()):
            self.assertFalse(Path('/proc', str(pid)).exists(), f'worker {pid} survived interrupted startup')
