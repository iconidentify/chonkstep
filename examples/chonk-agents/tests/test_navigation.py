"""Navigation never focuses the live desktop: real private sockets and fake processes."""
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location('agent_navigation', Path(__file__).resolve().parents[1] / 'navigation.py')
nav = importlib.util.module_from_spec(spec)
spec.loader.exec_module(nav)


def process(pid, parent=0, start=None, tty=0):
    return {'pid': pid, 'ppid': parent, 'start_time': start or str(pid * 10), 'tty_device': tty}


def window(pid=10, address='0x100000001', **fields):
    return {'pid': pid, 'address': address, 'class': 'foot', 'title': 'same project',
            'xwayland': False, 'mapped': True, **fields}


class Desktop:
    def __init__(self, root):
        self.path = root / 'hypr/test/.socket.sock'
        self.path.parent.mkdir(parents=True)
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(self.path))
        self.listener.listen()
        self.listener.settimeout(.05)
        self.windows = [window()]
        self.locked = False
        self.refuse_focus = False
        self.active = '0x0'
        self.commands = []
        self.overrides = {}
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.run)
        self.thread.start()

    def run(self):
        while not self.done.is_set():
            try:
                connection, _ = self.listener.accept()
            except (OSError, TimeoutError):
                continue
            with connection:
                connection.settimeout(.5)
                try:
                    raw = bytearray()
                    while True:
                        data = connection.recv(4096)
                        if not data:
                            break
                        raw.extend(data)
                    command = raw.decode()
                    self.commands.append(command)
                    if command in self.overrides:
                        reply = self.overrides[command]
                    elif command == 'j/monitors':
                        reply = json.dumps([{'solitaryBlockedBy': ['LOCK'] if self.locked else []}]).encode()
                    elif command == 'j/clients':
                        reply = json.dumps(self.windows).encode()
                    elif command.startswith('dispatch focuswindow address:'):
                        if not self.refuse_focus:
                            self.active = command.split('address:', 1)[1]
                        reply = b'ok'
                    elif command == 'j/activewindow':
                        reply = json.dumps({'address': self.active}).encode()
                    else:
                        reply = b'unsupported'
                    connection.sendall(reply)
                except (OSError, UnicodeError):
                    pass

    def close(self):
        self.done.set()
        self.listener.close()
        self.thread.join(1)


class OpenCode:
    def __init__(self, root, pid):
        self.incarnation = '12345678-abcd-1234-abcd-123456789abc'
        self.path = root / 'chonk-agents' / f'opencode-{pid}-{self.incarnation}.sock'
        self.path.parent.mkdir(mode=0o700)
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(str(self.path))
        self.path.chmod(0o600)
        self.listener.listen()
        self.listener.settimeout(.05)
        self.requests = []
        self.respond = lambda request: {'request_id': request['request_id'], 'ok': True,
                                       'session_id': request['session_id']}
        self.done = threading.Event()
        self.thread = threading.Thread(target=self.run)
        self.thread.start()

    def run(self):
        while not self.done.is_set():
            try:
                connection, _ = self.listener.accept()
            except (OSError, TimeoutError):
                continue
            with connection:
                connection.settimeout(.5)
                try:
                    raw = bytearray()
                    while b'\n' not in raw and len(raw) <= 8192:
                        chunk = connection.recv(4096)
                        if not chunk:
                            break
                        raw.extend(chunk)
                    if raw:
                        request = json.loads(raw)
                        self.requests.append(request)
                        response = self.respond(request)
                        connection.sendall(response if isinstance(response, bytes)
                                           else json.dumps(response).encode() + b'\n')
                except (OSError, ValueError):
                    pass

    def close(self):
        self.done.set()
        self.listener.close()
        self.thread.join(1)


class Navigation(unittest.TestCase):
    def setUp(self):
        self.stack = contextlib.ExitStack()
        self.addCleanup(self.stack.close)
        self.root = Path(self.stack.enter_context(tempfile.TemporaryDirectory()))
        self.desktop = Desktop(self.root)
        self.stack.callback(self.desktop.close)
        self.stack.enter_context(mock.patch.dict(os.environ, {'XDG_RUNTIME_DIR': str(self.root), 'HYPRLAND_INSTANCE_SIGNATURE': 'test'}))
        self.processes = {30: process(30, 20), 20: process(20, 10), 10: process(10)}
        real_process = nav._process
        self.stack.enter_context(mock.patch.object(nav, '_process', side_effect=lambda pid: self.processes.get(pid) if pid != os.getpid() else real_process(pid)))
        self.row = {'pid': 30, 'start_time': '300', 'terminal': {}, 'cwd': '/same/project'}

    def test_unique_ancestor_binds_address_independent_of_titles_and_cwd(self):
        candidate, = nav.discover(self.row)
        self.assertEqual(candidate['binding']['window'], {'pid': 10, 'start_time': '100', 'address': '0x100000001'})
        self.assertEqual(candidate['kind'], 'window')
        self.desktop.windows[0]['title'] = 'new title'
        self.desktop.windows.append(window(99, '0x900', title='same project'))
        self.assertEqual(nav.discover({**self.row, 'cwd': '/elsewhere'})[0]['token'], candidate['token'])

    def test_stale_or_missing_source_never_queries_desktop(self):
        for row in [None, {}, {**self.row, 'pid': True}, {**self.row, 'start_time': '299'},
                    {**self.row, 'start_time': True}, {**self.row, 'start_time': -1}]:
            self.assertEqual(nav.discover(row), [])
        self.assertEqual(self.desktop.commands, [])

    def test_native_integer_and_decimal_start_times_have_the_same_identity(self):
        expected = nav.discover(self.row)[0]['token']
        for start_time in [300, '300', '000300']:
            self.assertEqual(nav.discover({**self.row, 'start_time': start_time})[0]['token'], expected)

    def test_shared_terminal_process_is_ambiguous_without_guessing(self):
        self.desktop.windows.append(window(10, '0x200', title='different title'))
        self.assertEqual(nav.discover(self.row), [])

    def test_missing_or_self_reported_x11_pid_is_not_a_native_binding(self):
        self.desktop.windows[0]['xwayland'] = True
        self.assertEqual(nav.discover(self.row), [])
        self.desktop.windows[0].pop('xwayland')
        self.assertEqual(nav.discover(self.row), [])

    def test_hidden_native_window_remains_an_activation_candidate(self):
        self.desktop.windows[0].update(mapped=False, hidden=True)
        self.assertEqual(len(nav.discover(self.row)), 1)

    def test_activation_uses_only_the_revalidated_literal_address(self):
        token = nav.discover(self.row)[0]['token']
        self.assertEqual(nav.activate(self.row, token), 'Opened session window.')
        self.assertEqual([c for c in self.desktop.commands if c.startswith('dispatch')], ['dispatch focuswindow address:0x100000001'])

    def test_cancelling_during_rediscovery_prevents_late_focus(self):
        token = nav.discover(self.row)[0]['token']
        cancelled = threading.Event()
        discover = nav.discover
        def delayed(row):
            result = discover(row)
            cancelled.set()
            return result
        with mock.patch.object(nav, 'discover', side_effect=delayed):
            with self.assertRaisesRegex(nav.NavigationError, 'cancelled'):
                nav.activate(self.row, token, cancelled=cancelled.is_set)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))

    def test_pid_reuse_or_window_replacement_invalidates_old_choice(self):
        token = nav.discover(self.row)[0]['token']
        self.processes[10]['start_time'] = '101'
        with self.assertRaises(nav.NavigationError):
            nav.activate(self.row, token)
        self.processes[10]['start_time'] = '100'
        self.desktop.windows[0]['address'] = '0x200000001'
        with self.assertRaises(nav.NavigationError):
            nav.activate(self.row, token)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))

    def test_lock_and_focus_refusal_are_not_reported_as_success(self):
        token = nav.discover(self.row)[0]['token']
        self.desktop.locked = True
        with self.assertRaisesRegex(nav.NavigationError, 'Unlock'):
            nav.activate(self.row, token)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))
        self.desktop.locked = False
        self.desktop.refuse_focus = True
        with self.assertRaisesRegex(nav.NavigationError, 'did not activate'):
            nav.activate(self.row, token)

    def test_unknown_lock_state_and_oversized_reply_fail_closed(self):
        self.desktop.overrides['j/monitors'] = b'[{}]'
        with self.assertRaisesRegex(nav.NavigationError, 'lock state'):
            nav.discover(self.row)
        self.desktop.overrides.clear()
        self.desktop.overrides['j/clients'] = b' ' * (nav.LIMIT + 1)
        with self.assertRaisesRegex(nav.NavigationError, 'limit'):
            nav.discover(self.row)

    def test_socket_identity_change_refuses_request(self):
        _, identity = nav._request('j/clients')
        identity = {**identity, 'start_time': '0'}
        with self.assertRaisesRegex(nav.NavigationError, 'restarted'):
            nav._request('dispatch focuswindow address:0x1', identity)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))

    def test_event_socket_paths_are_ignored_and_graphical_signature_is_required(self):
        row = {**self.row, 'socket': '/some/other/socket', 'terminal': {'hyprland_socket': '/some/other/socket'}}
        self.assertEqual(len(nav.discover(row)), 1)
        with mock.patch.dict(os.environ, {'HYPRLAND_INSTANCE_SIGNATURE': '../other'}):
            with self.assertRaises(nav.NavigationError):
                nav.discover(row)

    def tmux_setup(self):
        path = str(self.root / 'tmux.sock')
        server = {'path': path, 'device': 1, 'inode': 2, 'pid': 50, 'start_time': '500'}
        self.processes.update({20: process(20, 50), 50: process(50), 60: process(60, 10), 61: process(61, 11), 11: process(11)})
        self.row['terminal'] = {'tmux_socket': path, 'tmux_pane': '%2', 'tty': '/dev/pts/7'}
        self.desktop.windows.append(window(11, '0x100000002'))
        real_connect = nav._connect
        self.stack.enter_context(mock.patch.object(nav, '_connect', side_effect=lambda candidate: (mock.Mock(), server.copy()) if candidate == path else real_connect(candidate)))
        self.stack.enter_context(mock.patch.object(nav, '_tty_matches', side_effect=lambda tty, p: (tty, p['pid']) in {('/dev/pts/7', 30), ('/dev/pts/1', 60), ('/dev/pts/2', 61)}))
        calls = []
        def tmux(candidate, args):
            self.assertEqual(candidate, path)
            calls.append(args)
            if args[0] == 'list-panes':
                return '%2\t20\t/dev/pts/7\n'
            if args[0] == 'switch-client':
                return ''
            if '#{pane_id}' in args[-1]:
                return '60\t/dev/pts/1\t%2\n61\t/dev/pts/2\t%2\n'
            return '60\t/dev/pts/1\n61\t/dev/pts/2\n'
        self.stack.enter_context(mock.patch.object(nav, '_tmux', side_effect=tmux))
        return calls

    def test_tmux_multiple_clients_are_distinct_explicit_choices(self):
        calls = self.tmux_setup()
        choices = nav.discover(self.row)
        self.assertEqual(len(choices), 2)
        self.assertEqual({c['kind'] for c in choices}, {'pane'})
        chosen = next(c for c in choices if c['binding']['tmux']['client']['pid'] == 61)
        self.assertEqual(nav.activate(self.row, chosen['token']), 'Opened tmux pane.')
        self.assertIn(['switch-client', '-c', '/dev/pts/2', '-t', '%2'], calls)
        self.assertEqual(self.desktop.active, '0x100000002')

    def test_tmux_wrong_pane_source_or_reused_client_never_switches(self):
        calls = self.tmux_setup()
        token = nav.discover(self.row)[0]['token']
        self.processes[60]['start_time'] = '601'
        with self.assertRaises(nav.NavigationError):
            nav.activate(self.row, token)
        self.row['terminal']['tty'] = '/dev/pts/999'
        self.assertEqual(nav.discover(self.row), [])
        self.assertFalse(any(c[0] == 'switch-client' for c in calls))

    def test_injected_pane_and_unrelated_tmux_server_are_refused(self):
        self.tmux_setup()
        self.row['terminal']['tmux_pane'] = '%2;kill-server'
        self.assertEqual(nav.discover(self.row), [])
        self.row['terminal']['tmux_pane'] = '%2'
        self.processes[20]['ppid'] = 10
        self.assertEqual(nav.discover(self.row), [])

    def opencode_setup(self, claimed_pid=None):
        current = nav._process(os.getpid())
        self.processes[os.getpid()] = {**current, 'ppid': 10}
        self.stack.enter_context(mock.patch.object(nav, '_process', side_effect=self.processes.get))
        self.row.update(pid=os.getpid(), start_time=int(current['start_time']), session_id='ses_123')
        service = OpenCode(self.root, claimed_pid or os.getpid())
        self.stack.callback(service.close)
        self.row['navigation'] = {'kind': 'opencode', 'socket': str(service.path),
                                  'incarnation': service.incarnation}
        return service

    def test_exact_session_capability_requires_live_owned_peer_and_confirms_before_focus(self):
        service = self.opencode_setup()
        candidate, = nav.discover(self.row)
        self.assertEqual(candidate['kind'], 'session')
        self.assertEqual(service.requests, [], 'discovery does not select a session')
        def response(request):
            self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))
            return {'request_id': request['request_id'], 'ok': True, 'session_id': request['session_id']}
        service.respond = response
        self.assertEqual(nav.activate(self.row, candidate['token']), 'Opened OpenCode session.')
        self.assertEqual(len(service.requests), 1)
        self.assertEqual(service.requests[0]['action'], 'select_session')
        self.assertEqual(service.requests[0]['session_id'], 'ses_123')
        self.assertEqual(service.requests[0]['incarnation'], service.incarnation)
        self.assertEqual(self.desktop.active, '0x100000001')

    def test_session_seam_disappearance_invalidates_session_token_but_keeps_window_available(self):
        service = self.opencode_setup()
        token = nav.discover(self.row)[0]['token']
        service.path.unlink()
        candidate, = nav.discover(self.row)
        self.assertEqual(candidate['kind'], 'window')
        with self.assertRaisesRegex(nav.NavigationError, 'changed'):
            nav.activate(self.row, token)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))

    def test_session_socket_wrong_peer_path_and_public_permissions_never_claim_exact_selection(self):
        service = self.opencode_setup(claimed_pid=30)
        self.assertEqual(nav.discover(self.row)[0]['kind'], 'window', 'filename must identify source')
        self.row.update(pid=30, start_time='300')
        self.assertEqual(nav.discover(self.row)[0]['kind'], 'window', 'actual peer must identify source')
        self.row.update(pid=os.getpid(), start_time=self.processes[os.getpid()]['start_time'])
        for path in ['/tmp/other.sock', str(service.path) + '/..']:
            self.row['navigation']['socket'] = path
            self.assertEqual(nav.discover(self.row)[0]['kind'], 'window')
        correct = service.path.with_name(f'opencode-{os.getpid()}-{service.incarnation}.sock')
        service.path.rename(correct)
        self.row['navigation']['socket'] = str(correct)
        correct.chmod(0o666)
        self.assertEqual(nav.discover(self.row)[0]['kind'], 'window')
        correct.chmod(0o600)
        correct.parent.chmod(0o755)
        self.assertEqual(nav.discover(self.row)[0]['kind'], 'window')

    def test_wrong_session_reply_correlation_and_rejection_never_focus(self):
        service = self.opencode_setup()
        token = nav.discover(self.row)[0]['token']
        replies = [lambda r: {'request_id': 'other', 'ok': True, 'session_id': r['session_id']},
                   lambda r: {'request_id': r['request_id'], 'ok': True, 'session_id': 'ses_other'},
                   lambda r: {'request_id': r['request_id'], 'ok': False, 'session_id': r['session_id']},
                   lambda r: b'[]\n', lambda r: b'x' * 9000 + b'\n']
        for reply in replies:
            service.respond = reply
            with self.assertRaises(nav.NavigationError):
                nav.activate(self.row, token)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))

    def test_lock_arriving_during_session_selection_prevents_desktop_focus(self):
        service = self.opencode_setup()
        token = nav.discover(self.row)[0]['token']
        def response(request):
            self.desktop.locked = True
            return {'request_id': request['request_id'], 'ok': True, 'session_id': request['session_id']}
        service.respond = response
        with self.assertRaisesRegex(nav.NavigationError, 'Unlock'):
            nav.activate(self.row, token)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))

    def test_cancellation_during_session_acknowledgement_prevents_late_focus(self):
        service = self.opencode_setup()
        token = nav.discover(self.row)[0]['token']
        cancelled = threading.Event()
        def response(request):
            cancelled.set()
            return {'request_id': request['request_id'], 'ok': True, 'session_id': request['session_id']}
        service.respond = response
        with self.assertRaisesRegex(nav.NavigationError, 'cancelled'):
            nav.activate(self.row, token, cancelled=cancelled.is_set)
        self.assertFalse(any(c.startswith('dispatch') for c in self.desktop.commands))

    def test_exact_session_then_explicit_tmux_client_then_window(self):
        service = self.opencode_setup()
        calls = self.tmux_setup()
        self.processes[os.getpid()]['ppid'] = 20
        self.stack.enter_context(mock.patch.object(nav, '_tty_matches', side_effect=lambda tty, p:
            (tty, p['pid']) in {('/dev/pts/7', os.getpid()), ('/dev/pts/1', 60), ('/dev/pts/2', 61)}))
        choices = nav.discover(self.row)
        self.assertEqual(len(choices), 2)
        self.assertEqual({c['kind'] for c in choices}, {'session'})
        chosen = next(c for c in choices if c['binding']['tmux']['client']['pid'] == 61)
        def response(request):
            self.assertFalse(any(c[0] == 'switch-client' for c in calls))
            return {'request_id': request['request_id'], 'ok': True, 'session_id': request['session_id']}
        service.respond = response
        self.assertEqual(nav.activate(self.row, chosen['token']), 'Opened OpenCode session.')
        self.assertIn(['switch-client', '-c', '/dev/pts/2', '-t', '%2'], calls)
        self.assertEqual(self.desktop.active, '0x100000002')


class Boundaries(unittest.TestCase):
    def test_socket_requires_absolute_owned_unix_socket_without_symlink(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'socket'
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
                server.bind(str(path))
                self.assertEqual(len(nav._socket_stat(str(path))), 2)
                link = Path(directory) / 'link'
                link.symlink_to(path)
                regular = Path(directory) / 'regular'
                regular.write_text('not a socket')
                for invalid in [None, 'relative', '/a\0b', str(link), str(regular)]:
                    with self.assertRaises(nav.NavigationError):
                        nav._socket_stat(invalid)
                info = path.stat()
                with mock.patch.object(nav.os, 'getuid', return_value=info.st_uid + 1):
                    with self.assertRaisesRegex(nav.NavigationError, 'owned'):
                        nav._socket_stat(str(path))

    def test_tmux_argv_has_no_shell_and_output_and_time_are_bounded(self):
        real_popen = subprocess.Popen
        seen = []
        def run_script(script):
            def spawn(argv, **kwargs):
                seen.append(argv)
                self.assertNotIn('shell', kwargs)
                return real_popen([sys.executable, '-c', script], **kwargs)
            return mock.patch.object(nav.subprocess, 'Popen', side_effect=spawn)
        with run_script("print('ok')"):
            self.assertEqual(nav._tmux('/tmp/socket;literal', ['list-clients', '-F', '#{client_pid}']).strip(), 'ok')
        self.assertEqual(seen[0][:3], ['/usr/bin/tmux', '-S', '/tmp/socket;literal'])
        with run_script("import sys;sys.stdout.write('x'*100000);sys.stdout.flush()"):
            with self.assertRaisesRegex(nav.NavigationError, 'too large'):
                nav._tmux('/tmp/socket', ['list-clients'])
        started = time.monotonic()
        with mock.patch.object(nav, 'TIMEOUT', .05), run_script('import time;time.sleep(10)'):
            with self.assertRaisesRegex(nav.NavigationError, 'in time'):
                nav._tmux('/tmp/socket', ['list-clients'])
        self.assertLess(time.monotonic() - started, 1)


if __name__ == '__main__':
    unittest.main()
