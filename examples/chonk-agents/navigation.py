"""Bounded, explicit local session navigation. Call these functions on a worker.

No provider policy or compositor changes: use the graphical broker's existing
Hyprland compatibility socket. Titles and directories are display data, never
identity. Ambiguous shared-process windows have no automatic binding.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import selectors
import socket
import stat
import struct
import subprocess
import time
import uuid

LIMIT = 262144
TIMEOUT = 0.7
MAX_ANCESTORS = 64
MAX_CANDIDATES = 32
ADDRESS = re.compile(r"0x[0-9a-fA-F]{1,16}\Z")
PANE = re.compile(r"%[0-9]{1,12}\Z")
TTY = re.compile(r"/dev/(?:pts/[0-9]+|tty[0-9]+)\Z")
SIGNATURE = re.compile(r"[A-Za-z0-9_-]{1,128}\Z")
SESSION_ID = re.compile(r"[A-Za-z0-9_.:-]{1,160}\Z")
UUID = re.compile(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}\Z")


class NavigationError(ValueError):
    """A safe explanation suitable for the session UI."""


def _process(pid):
    if type(pid) is not int or not 0 < pid <= 2147483647:
        return None
    path = Path('/proc') / str(pid)
    try:
        if path.stat().st_uid != os.getuid():
            return None
        with (path / 'stat').open() as stream:
            text = stream.read(4097)
        if len(text) > 4096 or int(text.split(' ', 1)[0]) != pid:
            return None
        fields = text.rsplit(')', 1)[1].split()
        if fields[0] in ('Z', 'X'):
            return None
        return {'pid': pid, 'start_time': fields[19], 'ppid': int(fields[1]),
                'tty_device': int(fields[4]) & 0xffffffff}
    except (OSError, ValueError, IndexError):
        return None


def _identity(process):
    return {'pid': process['pid'], 'start_time': process['start_time']}


def _ancestors(pid, start_time=None):
    result = []
    seen = set()
    for _ in range(MAX_ANCESTORS):
        process = _process(pid)
        if process is None or pid in seen:
            break
        if not result and start_time is not None and process['start_time'] != start_time:
            return []
        result.append(process)
        seen.add(pid)
        pid = process['ppid']
    return result


def _source(row):
    if not isinstance(row, dict):
        return []
    start = row.get('start_time')
    if type(start) is int and 0 <= start <= 18446744073709551615:
        start = str(start)
    if not isinstance(start, str) or not re.fullmatch(r'[0-9]{1,20}', start):
        return []
    return _ancestors(row.get('pid'), str(int(start)))


def _socket_stat(path):
    if not isinstance(path, str) or not path.startswith('/') or '\0' in path or len(os.fsencode(path)) > 107:
        raise NavigationError('The local navigation socket is unavailable.')
    try:
        info = os.lstat(path)
    except OSError as error:
        raise NavigationError('The local navigation socket is unavailable.') from error
    if not stat.S_ISSOCK(info.st_mode) or info.st_uid != os.getuid():
        raise NavigationError('The navigation socket is not an owned local socket.')
    return (info.st_dev, info.st_ino)


def _connect(path):
    before = _socket_stat(path)
    client = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    client.settimeout(TIMEOUT)
    try:
        client.connect(path)
        pid, uid, _ = struct.unpack('3i', client.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        process = _process(pid)
        if uid != os.getuid() or process is None or before != _socket_stat(path):
            raise NavigationError('The navigation server changed; refresh the session list.')
        identity = {'path': path, 'device': before[0], 'inode': before[1], **_identity(process)}
        return client, identity
    except (OSError, NavigationError) as error:
        client.close()
        if isinstance(error, NavigationError):
            raise
        raise NavigationError('The desktop navigation service is unavailable.') from error


def _desktop_path():
    runtime = os.environ.get('XDG_RUNTIME_DIR', '')
    signature = os.environ.get('HYPRLAND_INSTANCE_SIGNATURE', '')
    if not runtime.startswith('/') or not SIGNATURE.fullmatch(signature):
        raise NavigationError('This broker is not attached to a desktop navigation service.')
    return str(Path(runtime) / 'hypr' / signature / '.socket.sock')


def _request(command, expected=None):
    client, identity = _connect(_desktop_path())
    try:
        if expected is not None and identity != expected:
            raise NavigationError('The desktop restarted; refresh the session list.')
        client.sendall(command.encode('ascii'))
        client.shutdown(socket.SHUT_WR)
        deadline = time.monotonic() + TIMEOUT
        output = bytearray()
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise NavigationError('The desktop did not answer in time.')
            client.settimeout(remaining)
            data = client.recv(min(16384, LIMIT + 1 - len(output)))
            if not data:
                break
            output.extend(data)
            if len(output) > LIMIT:
                raise NavigationError('The desktop response exceeded the navigation limit.')
        return output.decode('utf-8'), identity
    except (OSError, UnicodeError) as error:
        raise NavigationError('The desktop did not answer the navigation request.') from error
    finally:
        client.close()


def _json_request(command, expected=None):
    data, identity = _request(command, expected)
    try:
        return json.loads(data), identity
    except (ValueError, RecursionError) as error:
        raise NavigationError('The desktop returned an invalid navigation response.') from error


def _unlocked(expected=None):
    monitors, identity = _json_request('j/monitors', expected)
    if not isinstance(monitors, list) or not monitors or len(monitors) > 64:
        raise NavigationError('The desktop output state is unavailable.')
    if any(not isinstance(m, dict) or not isinstance(m.get('solitaryBlockedBy'), list) for m in monitors):
        raise NavigationError('The desktop lock state is unavailable.')
    if any('LOCK' in m['solitaryBlockedBy'] for m in monitors):
        raise NavigationError('Unlock the desktop before opening an agent session.')
    return identity


def _desktop():
    identity = _unlocked()
    rows, _ = _json_request('j/clients', identity)
    if not isinstance(rows, list) or len(rows) > 512:
        raise NavigationError('The desktop window list is unavailable or too large.')
    windows = []
    for row in rows:
        if not isinstance(row, dict) or type(row.get('pid')) is not int:
            continue
        address = row.get('address')
        if not isinstance(address, str) or not ADDRESS.fullmatch(address) or int(address, 16) == 0:
            continue
        # X11 PIDs are self-reported, unlike native Wayland peer credentials.
        if row.get('xwayland') is not False:
            continue
        windows.append({'address': '0x' + format(int(address, 16), 'x'), 'pid': row['pid'],
                        'label': _label(row.get('class'), 'Terminal')})
    return identity, windows


def _label(value, fallback):
    if not isinstance(value, str):
        return fallback
    return ''.join(c for c in value if c.isprintable())[:80] or fallback


def _window_for(ancestors, windows):
    for process in ancestors:
        matched = [window for window in windows if window['pid'] == process['pid']]
        if matched:
            return (process, matched[0]) if len(matched) == 1 else None
    return None


def _candidate(binding, label, kind):
    token = hashlib.sha256(json.dumps(binding, sort_keys=True, separators=(',', ':')).encode()).hexdigest()
    return {'token': token, 'label': label, 'kind': kind, 'binding': binding}


def _opencode_connection(source, navigation):
    """A capability is tied to the emitting TUI, never a supplied remote path."""
    runtime = os.environ.get('XDG_RUNTIME_DIR', '')
    incarnation = navigation.get('incarnation')
    if not runtime.startswith('/') or not isinstance(incarnation, str) or not UUID.fullmatch(incarnation):
        raise NavigationError('The OpenCode navigation attachment is unavailable.')
    directory = Path(runtime) / 'chonk-agents'
    expected = directory / f"opencode-{source['pid']}-{incarnation}.sock"
    if navigation.get('socket') != str(expected):
        raise NavigationError('The OpenCode navigation attachment changed.')
    try:
        folder = directory.lstat()
        endpoint = expected.lstat()
    except OSError as error:
        raise NavigationError('The OpenCode navigation attachment is unavailable.') from error
    if (not stat.S_ISDIR(folder.st_mode) or folder.st_uid != os.getuid()
            or stat.S_IMODE(folder.st_mode) != 0o700
            or not stat.S_ISSOCK(endpoint.st_mode) or endpoint.st_uid != os.getuid()
            or stat.S_IMODE(endpoint.st_mode) != 0o600):
        raise NavigationError('The OpenCode navigation attachment is not private.')
    connection, server = _connect(str(expected))
    if {k: server[k] for k in ('pid', 'start_time')} != source:
        connection.close()
        raise NavigationError('The OpenCode navigation process changed.')
    return connection, server


def _with_session_navigation(row, candidates):
    navigation = row.get('navigation')
    session = row.get('session_id')
    if (not candidates or not isinstance(navigation, dict) or navigation.get('kind') != 'opencode'
            or not isinstance(session, str) or not SESSION_ID.fullmatch(session)):
        return candidates
    source = candidates[0]['binding']['source']
    try:
        connection, server = _opencode_connection(source, navigation)
        connection.close()
    except NavigationError:
        # The terminal remains a real destination; an unavailable TUI seam does
        # not justify advertising exact thread selection.
        return candidates
    return [_candidate({**candidate['binding'], 'navigation': {
        'kind': 'opencode', 'socket': server['path'], 'server': server,
        'incarnation': navigation['incarnation'], 'session_id': session,
    }}, 'OpenCode session · ' + candidate['label'], 'session') for candidate in candidates]


def _select_session(binding, cancelled=None):
    navigation = binding['navigation']
    connection, server = _opencode_connection(binding['source'], navigation)
    try:
        if server != navigation['server']:
            raise NavigationError('The OpenCode navigation attachment changed; refresh the list.')
        request_id = str(uuid.uuid4())
        request = {'action': 'select_session', 'session_id': navigation['session_id'],
                   'request_id': request_id, 'incarnation': navigation['incarnation']}
        _check_cancelled(cancelled)
        connection.sendall(json.dumps(request, separators=(',', ':')).encode() + b'\n')
        deadline = time.monotonic() + 2.2
        output = bytearray()
        while b'\n' not in output:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise NavigationError('OpenCode did not confirm the selected session in time.')
            connection.settimeout(remaining)
            chunk = connection.recv(4096)
            if not chunk:
                raise NavigationError('OpenCode closed before confirming the selected session.')
            output.extend(chunk)
            if len(output) > 8192:
                raise NavigationError('The OpenCode navigation response is too large.')
        line, _, remainder = output.partition(b'\n')
        if remainder.strip():
            raise NavigationError('OpenCode returned an invalid navigation response.')
        reply = json.loads(line)
        if (not isinstance(reply, dict) or reply.get('request_id') != request_id
                or reply.get('ok') is not True or reply.get('session_id') != navigation['session_id']):
            raise NavigationError('OpenCode did not confirm that exact session.')
    except (OSError, ValueError, RecursionError) as error:
        if isinstance(error, NavigationError):
            raise
        raise NavigationError('OpenCode did not answer the navigation request.') from error
    finally:
        connection.close()


def _tmux(path, arguments):
    """Fixed argv, bounded pipe reads, wall-clock timeout, and no shell."""
    environment = {k: v for k, v in os.environ.items() if k not in ('TMUX', 'TMUX_PANE')}
    try:
        process = subprocess.Popen(['/usr/bin/tmux', '-S', path, *arguments], stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.STDOUT, env=environment)
    except OSError as error:
        raise NavigationError('tmux is unavailable.') from error
    deadline = time.monotonic() + TIMEOUT
    output = bytearray()
    try:
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0 or not selector.select(remaining):
                    raise NavigationError('tmux did not answer in time.')
                chunk = os.read(process.stdout.fileno(), 4096)
                if not chunk:
                    break
                output.extend(chunk)
                if len(output) > 65536:
                    raise NavigationError('The tmux session list is too large.')
        if process.wait(timeout=max(0.001, deadline - time.monotonic())):
            raise NavigationError('The tmux session changed or could not be opened.')
        return output.decode('utf-8')
    except (OSError, UnicodeError, subprocess.TimeoutExpired) as error:
        raise NavigationError('tmux did not answer the navigation request.') from error
    finally:
        if process.poll() is None:
            process.kill()
            process.wait()
        process.stdout.close()


def _tty_matches(path, process):
    if not isinstance(path, str) or not TTY.fullmatch(path):
        return False
    try:
        info = os.stat(path)
        return stat.S_ISCHR(info.st_mode) and info.st_uid == os.getuid() and info.st_rdev == process['tty_device']
    except OSError:
        return False


def _tmux_candidates(source, terminal, desktop, windows):
    path, pane = terminal.get('tmux_socket'), terminal.get('tmux_pane')
    if not isinstance(pane, str) or not PANE.fullmatch(pane):
        return []
    connection, server = _connect(path)
    connection.close()
    if not any(_identity(p) == {'pid': server['pid'], 'start_time': server['start_time']} for p in source):
        return []
    panes = _tmux(path, ['list-panes', '-a', '-F', '#{pane_id}\t#{pane_pid}\t#{pane_tty}'])
    found = []
    for line in panes.splitlines():
        fields = line.split('\t')
        if len(fields) == 3 and fields[0] == pane and fields[1].isdigit():
            found.append(fields)
    if len(found) != 1:
        return []
    _, pane_pid, pane_tty = found[0]
    if not any(p['pid'] == int(pane_pid) for p in source) or not _tty_matches(pane_tty, source[0]):
        return []
    claimed_tty = terminal.get('tty')
    if claimed_tty and claimed_tty != pane_tty:
        return []
    clients = _tmux(path, ['list-clients', '-F', '#{client_pid}\t#{client_tty}'])
    result = []
    for line in clients.splitlines():
        fields = line.split('\t')
        if len(fields) != 2 or not fields[0].isdigit():
            continue
        ancestors = _ancestors(int(fields[0]))
        if not ancestors or not _tty_matches(fields[1], ancestors[0]):
            continue
        target = _window_for(ancestors, windows)
        if target is None:
            continue
        process, window = target
        binding = {'source': _identity(source[0]), 'desktop': desktop,
                   'window': {'address': window['address'], **_identity(process)},
                   'tmux': {'server': server, 'pane': pane, 'pane_pid': int(pane_pid),
                            'pane_tty': pane_tty, 'client': _identity(ancestors[0]), 'client_tty': fields[1]}}
        result.append(_candidate(binding, f"{window['label']} · {fields[1]} · tmux {pane}", 'pane'))
        if len(result) > MAX_CANDIDATES:
            raise NavigationError('Too many attached terminals; choose a smaller local session.')
    # A server replacement cannot make an earlier pane/client list authoritative.
    connection, current_server = _connect(path)
    connection.close()
    if current_server != server:
        return []
    return list({candidate['token']: candidate for candidate in result}.values())


def discover(row):
    """Return validated choices, or [] for a stale/ambiguous/unbound source.

    NavigationError describes unavailable desktop/tmux services. This performs
    bounded blocking I/O and must run outside GTK/dock/compositor event loops.
    """
    source = _source(row)
    if not source:
        return []
    desktop, windows = _desktop()
    terminal = row.get('terminal') or {}
    if not isinstance(terminal, dict):
        return []
    if terminal.get('tmux_socket') or terminal.get('tmux_pane'):
        return _with_session_navigation(row, _tmux_candidates(source, terminal, desktop, windows))
    target = _window_for(source, windows)
    if target is None:
        return []
    process, window = target
    binding = {'source': _identity(source[0]), 'desktop': desktop,
               'window': {'address': window['address'], **_identity(process)}}
    return _with_session_navigation(row, [_candidate(binding, window['label'], 'window')])


def _validate_binding(binding):
    identities = [binding['source'], binding['window']]
    tmux = binding.get('tmux')
    if tmux:
        identities.extend([tmux['client'], tmux['server']])
    for identity in identities:
        process = _process(identity['pid'])
        if process is None or process['start_time'] != identity['start_time']:
            raise NavigationError('That session process changed; refresh the list.')
    if tmux:
        connection, identity = _connect(tmux['server']['path'])
        connection.close()
        if identity != tmux['server']:
            raise NavigationError('The tmux server changed; refresh the list.')
    navigation = binding.get('navigation')
    if navigation:
        connection, identity = _opencode_connection(binding['source'], navigation)
        connection.close()
        if identity != navigation['server']:
            raise NavigationError('The OpenCode navigation attachment changed; refresh the list.')


def _check_cancelled(cancelled):
    if cancelled is not None and cancelled():
        raise NavigationError('Opening this session was cancelled.')


def activate(row, candidate_token, *, cancelled=None):
    """Revalidate one previously displayed choice, then open that exact target."""
    _check_cancelled(cancelled)
    if not isinstance(candidate_token, str) or not re.fullmatch('[0-9a-f]{64}', candidate_token):
        raise NavigationError('Choose a current session destination.')
    candidate = next((c for c in discover(row) if c['token'] == candidate_token), None)
    _check_cancelled(cancelled)
    if candidate is None:
        raise NavigationError('That session destination changed; refresh the list.')
    binding = candidate['binding']
    desktop = binding['desktop']
    _unlocked(desktop)
    _validate_binding(binding)
    _check_cancelled(cancelled)
    if binding.get('navigation'):
        _select_session(binding, cancelled)
        _validate_binding(binding)
        _check_cancelled(cancelled)
    tmux = binding.get('tmux')
    if tmux:
        # Local tmux(1): a %pane target changes client session, window and pane
        # together. An explicit client tty avoids mutating an arbitrary client.
        _check_cancelled(cancelled)
        _tmux(tmux['server']['path'], ['switch-client', '-c', tmux['client_tty'], '-t', tmux['pane']])
        clients = _tmux(tmux['server']['path'], ['list-clients', '-F', '#{client_pid}\t#{client_tty}\t#{pane_id}'])
        expected = [str(tmux['client']['pid']), tmux['client_tty'], tmux['pane']]
        if not any(line.split('\t') == expected for line in clients.splitlines()):
            raise NavigationError('tmux did not select that pane in the chosen terminal.')
        _validate_binding(binding)
    # A route selection can take longer than an ordinary focus request.
    _unlocked(desktop)
    _check_cancelled(cancelled)
    address = binding['window']['address']
    reply, _ = _request('dispatch focuswindow address:' + address, desktop)
    if reply.strip() != 'ok':
        raise NavigationError('The desktop could not open that window.')
    focused, _ = _json_request('j/activewindow', desktop)
    focused_address = focused.get('address') if isinstance(focused, dict) else None
    if not isinstance(focused_address, str) or focused_address.lower() != address:
        raise NavigationError('The desktop did not activate that window; it may have a modal dialog or focus rule.')
    if binding.get('navigation'):
        return 'Opened OpenCode session.'
    return 'Opened tmux pane.' if tmux else 'Opened session window.'
