"""Opt-in real GTK interaction checks on private Weston, with fake navigation.

CHONK_AGENTS_GTK_TEST=1 python -m unittest discover -s examples/chonk-agents/tests -p test_gtk_sessions.py
No provider request, browser launch, or live desktop focus is performed.
"""
import contextlib
import copy
import json
import os
from pathlib import Path
import runpy
import signal
import subprocess
import sys
import tempfile
import threading
import time
import types
import unittest

APP = Path(__file__).resolve().parents[1] / 'chonk-agents.py'


def gtk_child():
    import gi
    gi.require_version('Gtk', '4.0')
    from gi.repository import Gtk, GLib
    namespace = runpy.run_path(str(APP))
    started = time.monotonic()
    state = {'phase': 0, 'error': None, 'done': False, 'watch': None,
             'mode': 'cancel-discovery', 'calls': [], 'effects': [], 'failed': False, 'discoveries': 0}
    discover_started, discover_release = threading.Event(), threading.Event()
    action_started, action_release = threading.Event(), threading.Event()

    def row(key, updated, **extra):
        return {'id': key, 'schema': 2, 'provider': 'codex', 'client': 'terminal',
                'health': 'connected', 'status': 'working', 'confidence': 'observed',
                'title': key, 'detail': 'Original activity', 'cwd': str(APP.parent),
                'updated': updated, 'capabilities': {}, **extra}
    rows = [row('Terminal A', 30), row('OpenCode B', 20, provider='opencode',
            capabilities={'open_session': True}, navigation={'kind': 'opencode'}),
            row('Browser C', 10, client='t3', capabilities={'open_browser': False})]
    state['snapshot'] = {'sessions': rows, 'palette': {}}

    def watch(callback):
        state['watch'] = callback
        callback(copy.deepcopy(state['snapshot']))

    def discover(_):
        state['discoveries'] += 1
        if state['mode'] == 'cancel-discovery':
            discover_started.set()
            assert discover_release.wait(3), 'discovery test was never released'
            return [{'token': 'single', 'kind': 'window', 'label': 'Only terminal'}]
        return [{'token': 'first', 'kind': 'window', 'label': 'First terminal'},
                {'token': 'second', 'kind': 'pane', 'label': 'Second terminal · %2'}]

    def activate(_, token, *, cancelled=None):
        state['calls'].append(token)
        if state['mode'] == 'cancel-action':
            action_started.set()
            assert action_release.wait(3), 'activation test was never released'
        if cancelled and cancelled():
            raise ValueError('Cancelled')
        if state['mode'] == 'retry' and not state['failed']:
            state['failed'] = True
            raise ValueError('Destination changed; choose again.')
        state['effects'].append(token)
        return 'Opened terminal.'

    sys.modules['navigation'] = types.SimpleNamespace(discover=discover, activate=activate)
    namespace['review'].__globals__.update(watch=watch,
        request=lambda _: copy.deepcopy(state['snapshot']))

    def widgets(widget):
        yield widget
        child = widget.get_first_child()
        while child:
            yield from widgets(child)
            child = child.get_next_sibling()

    def labels(widget):
        return [w for w in widgets(widget) if isinstance(w, Gtk.Label)]

    def buttons(widget):
        return [w for w in widgets(widget) if isinstance(w, Gtk.Button) and w.get_label()]

    def chooser(app):
        return next((w for w in app.get_windows() if w.get_title() == 'Open agent session'), None)

    def step():
        app = Gtk.Application.get_default()
        if time.monotonic() - started > 10:
            raise AssertionError(f'GTK interaction test timed out at phase {state["phase"]}')
        if not app:
            return True
        main = next((w for w in app.get_windows() if w.get_title() == 'Agent Review'), None)
        if main is None:
            return True
        phase = state['phase']
        if phase == 0:
            titles = {w.get_label(): w for w in labels(main) if w.get_label() in {r['id'] for r in rows}}
            if len(titles) != 3 or not state['watch']:
                return True
            cards = {key: label.get_parent() for key, label in titles.items()}
            state['cards'] = cards
            state['open'] = {key: next(b for b in buttons(card) if b.get_label().startswith('Open '))
                             for key, card in cards.items()}
            assert state['open']['Terminal A'].get_label() == 'Open terminal  ↗'
            assert state['open']['OpenCode B'].get_label() == 'Open session  ↗'
            assert state['open']['Browser C'].get_label() == 'Open thread in browser  ↗'
            assert not state['open']['Browser C'].get_sensitive()
            state['open']['Terminal A'].grab_focus()
            assert main.get_focus() == state['open']['Terminal A']
            rows[1].update(updated=50, detail='Updated without replacing the card')
            rows[2]['capabilities']['open_browser'] = True
            state['watch'](copy.deepcopy(state['snapshot']))
            state['phase'] = 1
        elif phase == 1:
            if not any(w.get_label() == 'Updated without replacing the card' for w in labels(main)):
                return True
            assert main.get_focus() == state['open']['Terminal A'], 'refresh lost keyboard focus'
            assert sum(w.get_label() == 'Your agents, in one place' for w in labels(main)) == 1
            for key, old in state['cards'].items():
                assert next(w for w in labels(main) if w.get_label() == key).get_parent() == old
            assert state['cards']['OpenCode B'].get_next_sibling() == state['cards']['Terminal A']
            assert state['open']['Browser C'].get_sensitive()
            state['open']['Terminal A'].emit('clicked')
            state['open']['Terminal A'].emit('clicked')
            state['phase'] = 2
        elif phase == 2:
            window = chooser(app)
            if not discover_started.is_set() or window is None:
                return True
            assert sum(w.get_title() == 'Open agent session' for w in app.get_windows()) == 1
            assert state['discoveries'] == 1, 'repeated main action started duplicate discovery'
            window.close()
            discover_release.set()
            state['after_close'] = time.monotonic()
            state['phase'] = 3
        elif phase == 3:
            if time.monotonic() - state['after_close'] < .15:
                return True
            assert not state['calls'], 'closing discovery launched a late navigation action'
            assert chooser(app) is None
            state['mode'] = 'cancel-action'
            state['open']['Terminal A'].emit('clicked')
            state['phase'] = 4
        elif phase == 4:
            window = chooser(app)
            if window is None:
                return True
            choices = [b for b in buttons(window) if b.get_label().startswith(('Open terminal ·', 'Open tmux pane ·'))]
            if len(choices) != 2:
                return True
            state['choices'] = choices
            choices[0].emit('clicked')
            choices[1].emit('clicked')  # Even a queued/programmatic click must obey the busy guard.
            state['phase'] = 5
        elif phase == 5:
            if not action_started.is_set():
                return True
            assert state['calls'] == ['first'], 'chooser allowed simultaneous navigation'
            assert all(not button.get_sensitive() for button in state['choices'])
            chooser(app).close()
            action_release.set()
            state['after_close'] = time.monotonic()
            state['phase'] = 6
        elif phase == 6:
            if time.monotonic() - state['after_close'] < .15:
                return True
            assert not state['effects'], 'closing the pending action allowed a late effect'
            assert chooser(app) is None
            state['mode'] = 'retry'
            state['open']['Terminal A'].emit('clicked')
            state['phase'] = 7
        elif phase == 7:
            window = chooser(app)
            if window is None:
                return True
            choices = [b for b in buttons(window) if b.get_label().startswith(('Open terminal ·', 'Open tmux pane ·'))]
            if len(choices) != 2:
                return True
            state['choices'] = choices
            choices[0].emit('clicked')
            state['phase'] = 8
        elif phase == 8:
            window = chooser(app)
            if window is None or not any(w.get_label() == 'Destination changed; choose again.' for w in labels(window)):
                return True
            assert all(button.get_sensitive() for button in state['choices']), 'failed action left chooser disabled'
            state['choices'][1].emit('clicked')
            state['phase'] = 9
        elif phase == 9:
            if chooser(app) is not None:
                return True
            assert state['effects'] == ['second']
            assert state['calls'] == ['first', 'first', 'second']
            state['snapshot']['sessions'] = [rows[1], rows[2]]
            state['watch'](copy.deepcopy(state['snapshot']))
            state['phase'] = 10
        elif phase == 10:
            if any(w.get_label() == 'Terminal A' for w in labels(main)):
                return True
            assert next(w for w in labels(main) if w.get_label() == 'OpenCode B').get_parent() == state['cards']['OpenCode B']
            state['done'] = True
            main.close()
            return False
        return True

    def inspect():
        try:
            return step()
        except Exception as error:
            state['error'] = repr(error)
            discover_release.set(); action_release.set()
            app = Gtk.Application.get_default()
            if app:
                app.quit()
            return False

    GLib.timeout_add(15, inspect)
    namespace['review']()
    assert state['done'], state['error'] or f'GTK exited at phase {state["phase"]}'
    print(json.dumps({'passed': 'stable focus, keyed cards, truthful actions, cancellation, singleflight, retry',
                      'seconds': time.monotonic() - started}), flush=True)


def native_child():
    with tempfile.TemporaryDirectory(prefix='chonk-gtk-sessions.') as temp, contextlib.ExitStack() as stack:
        root = Path(temp)
        for name in ('runtime', 'config', 'state', 'data', 'cache'):
            (root / name).mkdir(mode=0o700)
            os.environ['XDG_' + ('RUNTIME_DIR' if name == 'runtime' else name.upper() + '_HOME')] = str(root / name)
        os.environ.update(WAYLAND_DISPLAY='wayland-gtk', GDK_BACKEND='wayland', GTK_A11Y='none',
                          GSETTINGS_BACKEND='memory', LIBGL_ALWAYS_SOFTWARE='1')
        os.environ.pop('DISPLAY', None)
        log = stack.enter_context((root / 'weston.log').open('w+'))
        weston = subprocess.Popen(['weston', '--backend=headless-backend.so', '--socket=wayland-gtk',
            '--idle-time=0', '--width=900', '--height=760', '--no-config'], stdout=log, stderr=log, start_new_session=True)
        def stop():
            if weston.poll() is None:
                os.killpg(weston.pid, signal.SIGTERM)
                try:
                    weston.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    os.killpg(weston.pid, signal.SIGKILL)
                    weston.wait(timeout=2)
        stack.callback(stop)
        deadline = time.monotonic() + 5
        while not (root / 'runtime/wayland-gtk').is_socket():
            assert weston.poll() is None, 'private Weston exited'
            assert time.monotonic() < deadline, 'private Weston did not start'
            time.sleep(.01)
        result = subprocess.run([sys.executable, __file__, '--gtk-child'], capture_output=True, timeout=13)
        assert result.returncode == 0, result.stderr.decode(errors='replace')
        print(result.stdout.decode(), end='')


class NativeSessions(unittest.TestCase):
    @unittest.skipUnless(os.environ.get('CHONK_AGENTS_GTK_TEST') == '1', 'opt-in native GTK/Weston test')
    def test_keyed_cards_and_cancellable_singleflight_navigation(self):
        with tempfile.TemporaryDirectory(prefix='chonk-gtk-bus.') as directory:
            config = Path(directory) / 'bus.conf'
            config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                '<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            result = subprocess.run(['dbus-run-session', '--config-file', str(config), '--',
                sys.executable, __file__, '--native-child'], capture_output=True, timeout=22)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors='replace'))
        self.assertLess(json.loads(result.stdout)['seconds'], 10)


if __name__ == '__main__':
    if '--gtk-child' in sys.argv:
        gtk_child()
    elif '--native-child' in sys.argv:
        native_child()
    else:
        unittest.main()
