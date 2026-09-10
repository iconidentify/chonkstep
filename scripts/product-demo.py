#!/usr/bin/env python3
"""Capture reproducible product media from real, isolated compositor sessions.

Weston -> recording ChonkStep -> demonstrated ChonkStep. The extra parent
captures the child overlay, which user exports correctly exclude. No ambient
display, clipboard, config, or session bus is targeted. See docs/product-demos.md.
"""
import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time

SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('demo_base', SCRIPTS / 'bench-compositor.py')
b = importlib.util.module_from_spec(spec)
spec.loader.exec_module(b)


def wait(description, predicate, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = predicate()
        if result:
            return result
        time.sleep(.02)
    raise TimeoutError(description)


def send(door, command):
    door.stream.sendall((command+'\n').encode())


def chord(door, modifiers, code):
    for key in modifiers:
        send(door, f'key {key} press')
    send(door, f'key {code} press')
    send(door, f'key {code} release')
    for key in reversed(modifiers):
        send(door, f'key {key} release')
    assert door.query('barrier') == 'ok'


def window(door, app=None):
    rows = door.query('windows', multiple=True)
    for row in rows:
        if not row.startswith('window '):
            continue
        fields = dict(word.split('=', 1) for word in shlex.split(row)[1:])
        if fields.get('mapped') == 'true' and (app is None or fields.get('app') == app):
            return fields
    return None


def move(door, start, end, seconds=.65):
    frames = max(1, int(seconds*30))
    for i in range(frames+1):
        t = i/frames
        t = t*t*(3-2*t)
        send(door, f'motion {start[0]+(end[0]-start[0])*t:.2f} {start[1]+(end[1]-start[1])*t:.2f}')
        time.sleep(seconds/frames)
    assert door.query('barrier') == 'ok'


def drag(door, start, end, seconds=1):
    send(door, f'motion {start[0]} {start[1]}')
    send(door, 'button left press')
    assert door.query('barrier') == 'ok'
    time.sleep(.15)  # Give a real GTK header time to request its interactive grab.
    move(door, start, end, seconds)
    send(door, 'button left release')
    assert door.query('barrier') == 'ok'


def ipc_request(runtime, command):
    ipc = next(Path(runtime).glob('hypr/*/.socket.sock'))
    with socket.socket(socket.AF_UNIX) as connection:
        connection.settimeout(10)
        connection.connect(str(ipc))
        connection.sendall(command.encode())
        with connection.makefile('rb') as reader:
            return reader.read(1024*1024).decode()


def dispatch(runtime, command):
    response = ipc_request(runtime, '/dispatch '+command)
    if response != 'ok':
        raise RuntimeError(f'demo command {command}: {response}')


def overview_rows(door, kind):
    return [dict(word.split('=',1) for word in shlex.split(row)[1:])
            for row in door.query('windows',multiple=True) if row.startswith(kind+' ')]


def spaces_walkthrough(door, clients, step, still, metadata):
    runtime = clients['XDG_RUNTIME_DIR']
    def choose(index):
        space = next(r for r in overview_rows(door, 'overview-space') if int(r['index']) == index)
        point = (int(space['x'])+int(space['w'])/2, int(space['y'])+int(space['h'])/2)
        move(door, (960,540), point)
        send(door, 'button left press')
        send(door, 'button left release')
        assert door.query('barrier') == 'ok'
    def verify(name):
        rows = overview_rows(door,'overview-space-window')
        metadata.setdefault('spaces_checks',[]).append({'name':name,'membership':rows,
            'clients':json.loads(ipc_request(runtime,'j/clients'))})
        return rows
    step('Desktop 1: terminals together',2)
    chord(door,[29],103)
    step('Overview shows the actual windows on all three desktops',3)
    rows = verify('initial')
    assert len(overview_rows(door,'overview-space')) == 3
    assert len(rows) == 4
    still('spaces-overview')
    choose(1)
    step('Desktop 2: the design studio',3)
    still('spaces-design')
    choose(2)
    step('Desktop 3: release notes',3)
    choose(0)
    step('Return to the terminal workspace',1)
    note = window(door,'demo-notes')
    card = next(r for r in overview_rows(door,'overview-window') if r['id'] == note['id'])
    target = next(r for r in overview_rows(door,'overview-space') if r['index'] == '1')
    start = (int(card['x'])+int(card['w'])/2,int(card['y'])+int(card['h'])/2)
    end = (int(target['x'])+int(target['w'])/2,int(target['y'])+int(target['h'])/2)
    drag(door,start,end,1.8)
    step('Drag a window to another Space; both thumbnails update',3)
    rows = verify('after-drag')
    assert {'index':'1','id':note['id']} in rows
    assert {'index':'0','id':note['id']} not in rows
    still('spaces-window-moved')
    choose(1)
    chord(door,[],1)
    dispatch(runtime,'focuswindow class:^org.chonkstep.DemoBoard$')
    step('The moved terminal and design board share Desktop 2',2)
    chord(door,[29,125],33)
    step('Fullscreen creates a dedicated Space',2)
    chord(door,[29],103)
    step('The fullscreen Space also shows its actual window',3)
    assert len(overview_rows(door,'overview-space')) == 4
    verify('fullscreen')
    still('spaces-fullscreen')
    chord(door,[29],105)
    step('Control-Left switches to the neighboring desktop',2)
    chord(door,[29],106)
    chord(door,[],1)
    chord(door,[29,125],33)
    chord(door,[29],103)
    step('Exit fullscreen: the original desktops and window geometry return',3)
    assert len(overview_rows(door,'overview-space')) == 3
    verify('restored')
    still('spaces-restored')
    chord(door,[],1)
    step('A place for every project',2)


def place(door, app, x, y, runtime):
    w = wait(f'{app} mapped', lambda: window(door, app))
    frames = door.query('windows', multiple=True)
    frame = next((row for row in frames if row.startswith('frame ') and f'window={w["id"]} ' in row), None)
    if frame:
        f = dict(word.split('=', 1) for word in shlex.split(frame)[1:])
        start = (int(f['x'])+130, int(f['y'])+10)
        drag(door, start, (start[0]+x-int(w['x']), start[1]+y-int(w['y'])), .6)
    else:
        # Arrange CSD fixtures through the public IPC. GTK may negotiate its
        # header geometry while mapping; this gives layout a stable boundary.
        address = next(c['address'] for c in json.loads(ipc_request(runtime, 'j/clients')) if c['class'] == app)
        response = ipc_request(runtime, f'/dispatch movewindowpixel exact {x} {y},address:{address}')
        if response != 'ok':
            raise RuntimeError(f'demo placement: {response}')
    wait(f'{app} reaches its demo position', lambda: (lambda current:
        current and abs(int(current['x'])-x) <= 2 and abs(int(current['y'])-y) <= 2)(window(door, app)))


def environment(root, host):
    env = b.isolated_environment(root, host, False)
    home = root/'home'
    home.mkdir()
    env['HOME'] = str(home)
    env.update(GTK_USE_PORTAL='0', GIO_USE_VFS='local', GTK_A11Y='none', NO_AT_BRIDGE='1')
    for key in ('GTK_IM_MODULE', 'QT_IM_MODULE', 'XMODIFIERS', 'SESSION_MANAGER'):
        env.pop(key, None)
    return env


def boot(stack, root, host, binary, logs, name):
    env = environment(root, host)
    config = root/'config/chonkstep'
    config.mkdir()
    (config/'config.toml').write_text(
        "interaction_mode='mac'\nhyprland_config=false\nomarchy_shell=false\nomarchy_menu=false\n"
        "show_dock=false\nrestore_session=false\ntheme='nextstep-classic'\nscale=1\n")
    captures = logs/'exports'
    captures.mkdir(exist_ok=True)
    env.update(CHONKSTEP_TEST_SOCKET=str(root/'runtime/door.sock'),
               OMARCHY_SCREENSHOT_DIR=str(captures), OMARCHY_SCREENRECORD_DIR=str(captures))
    process = stack.enter_context(b.child([str(binary)], env, logs/f'{name}.log'))
    socket = b.wait_for_socket(root/'runtime', process)
    b.wayland_roundtrip(socket)
    door = b.Door(root/'runtime/door.sock')
    stack.callback(door.close)
    assert door.query('barrier') == 'ok'
    client = env | {'WAYLAND_DISPLAY': str(socket)}
    client.pop('CHONKSTEP_TEST_SOCKET')
    return process, door, client, socket


def caption_file(timeline, output):
    def stamp(seconds):
        milliseconds = max(0, round(seconds*1000))
        seconds, milliseconds = divmod(milliseconds,1000)
        minutes, seconds = divmod(seconds,60)
        hours, minutes = divmod(minutes,60)
        return f'{hours:02}:{minutes:02}:{seconds:02},{milliseconds:03}'
    blocks = []
    for index, (entry, following) in enumerate(zip(timeline,timeline[1:]),1):
        blocks.append(f"{index}\n{stamp(entry['seconds'])} --> {stamp(following['seconds'])}\n{entry['action']}\n")
    (output/'timeline.srt').write_text('\n'.join(blocks),encoding='utf-8')


def run(args):
    binary = args.binary.resolve(strict=True)
    for command in ('weston', 'foot', 'wf-recorder', 'grim', 'ffmpeg', 'ffprobe'):
        if not shutil.which(command):
            raise RuntimeError(f'missing required demo tool: {command}')
    subprocess.run([sys.executable, '-c', "import gi, cairo; gi.require_version('Gtk','4.0'); from gi.repository import Gtk"], check=True)
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    metadata = {'scenario': args.scenario+'-desktop', 'binary': b.binary_metadata(binary),
                'backend': 'nested GL; scripted fixture clients; actual production overlay and exports',
                'timeline': [], 'dimensions': [1920, 1080]}
    with tempfile.TemporaryDirectory(prefix='chonk-demo-') as temporary, contextlib.ExitStack() as stack:
        root = Path(temporary)
        host = root/'host'
        host.mkdir(mode=0o700)
        hostenv = environment(host, host/'runtime/unused')
        weston = stack.enter_context(b.child(['weston', '--backend=headless-backend.so', '--renderer=gl',
            '--shell=kiosk-shell.so', '--socket=wayland-demo', '--width=1920', '--height=1080',
            '--idle-time=0', '--no-config'], hostenv, output/'weston.log'))
        host_socket = b.wait_for_socket(host/'runtime', weston)
        outer, parent, recording_env, outer_socket = boot(stack, root/'parent', host_socket, binary, output, 'parent')
        inner, door, clients, _ = boot(stack, root/'desktop', outer_socket, binary, output, 'desktop')
        wait('nested desktop maps', lambda: window(parent))
        chord(parent, [29,125], 33)  # Real fullscreen action on the recording parent.
        wait('1920x1080 demo output', lambda: 'output 1920 1080' in door.query('windows', multiple=True))
        fixture = SCRIPTS/'demos/capture-desktop.py'
        version = metadata['binary']['version'].splitlines()[0].split()[1]
        for role, pos in [('terminal', (75,105)), ('notes',(120,575))]:
            stack.enter_context(b.child(['foot','--config=/dev/null','--font=monospace:size=12',
                f'--title=Studio / {role}',f'--app-id=demo-{role}', '--window-size-pixels=710x330',
                sys.executable, '-B', str(fixture), role, '--version', version, '--scenario', args.scenario], clients, output/f'{role}.log'))
            place(door, f'demo-{role}', *pos, clients['XDG_RUNTIME_DIR'])
        stack.enter_context(b.child([sys.executable,'-B',str(fixture),'board'], clients, output/'board.log'))
        place(door, 'org.chonkstep.DemoBoard', 1000, 205, clients['XDG_RUNTIME_DIR'])
        if args.scenario == 'spaces':
            runtime = clients['XDG_RUNTIME_DIR']
            dispatch(runtime,'movetoworkspacesilent 2,class:^org.chonkstep.DemoBoard$')
            dispatch(runtime,'workspace 3')
            stack.enter_context(b.child([sys.executable,'-B',str(fixture),'editor'],clients,output/'editor.log'))
            place(door,'org.chonkstep.DemoNotes',500,230,runtime)
            dispatch(runtime,'workspace 1')
        time.sleep(1)
        raw = output/'demo.raw.mp4'
        recorder = stack.enter_context(b.child(['wf-recorder','--no-dmabuf','-D','-r','30','-c','libx264',
            '-p','preset=fast','-p','crf=18','-x','yuv420p','-f',str(raw)], recording_env, output/'recorder.log'))
        started = time.monotonic()

        def step(name, pause=1):
            if any(p.poll() is not None for p in (inner,outer,recorder)):
                raise RuntimeError('a demo compositor or recorder exited')
            metadata['timeline'].append({'seconds':round(time.monotonic()-started,3),'action':name})
            print(name, flush=True)
            time.sleep(pause)

        def still(name):
            # Parent screencopy contains the nested child's capture controls.
            subprocess.run(['grim', str(output/f'{name}.png')], env=recording_env, check=True, timeout=15)
            marker = root/'desktop/state/chonkstep/screenshot'
            pending = marker.with_suffix('.pending')
            diagnostic = output/f'{name}-diagnostic.png'
            pending.write_text(str(diagnostic))
            pending.replace(marker)
            wait('diagnostic screenshot', lambda: diagnostic.exists() and diagnostic.stat().st_size > 8)

        if args.scenario == 'spaces':
            spaces_walkthrough(door, clients, step, still, metadata)
        else:
            step('Desktop with two terminals and a live design board', 2)
            still('desktop')
            chord(door,[125,42],6)
            step('Command-Shift-5 opens capture controls',2)
            chord(door,[],4) # 3: region
            drag(door,(952,151),(1820,862),1.5)
            step('Select a region',2)
            still('capture-area-overlay')
            chord(door,[],3) # 2: window
            move(door,(1820,862),(1300,460))
            step('Choose a window',2)
            still('capture-window-overlay')
            chord(door,[],28)
            step('Save the window screenshot',2)
            wait('saved screenshot', lambda: list((output/'exports').glob('*.png')))
            chord(door,[125,42],6)
            chord(door,[],6) # 5: record region
            drag(door,(990,180),(1800,825),1.2)
            step('Select a recording region',2)
            still('capture-record-overlay')
            chord(door,[],28)
            step('Record the live design board',5)
            chord(door,[125,42],6) # Capture shortcut stops an active recording.
            step('Stop recording and save',2)
            wait('saved recording', lambda: list((output/'exports').glob('*.mp4')))
        recorder.send_signal(signal.SIGINT)
        recorder.wait(timeout=20)
        if recorder.returncode != 0:
            raise RuntimeError(f'recorder exit {recorder.returncode}')
        metadata['timeline'].append({'seconds':round(time.monotonic()-started,3),'action':'End'})
        (output/'world.txt').write_text('\n'.join(door.query('windows',multiple=True))+'\n')
    final = output/f'chonkstep-{args.scenario}-demo.mp4'
    subprocess.run(['ffmpeg','-v','error','-i',str(raw),'-c','copy','-movflags','+faststart',str(final)],check=True,timeout=60)
    raw.unlink()
    caption_file(metadata['timeline'],output)
    captioned = output/f'chonkstep-{args.scenario}-captioned.mp4'
    subtitle_filter = ("subtitles=timeline.srt:force_style='Fontname=DejaVu Sans,Fontsize=20,"
                       "Outline=1,Shadow=0,BorderStyle=3,BackColour=&H90000000,MarginV=22'")
    subprocess.run(['ffmpeg','-v','error','-i',final.name,'-vf',subtitle_filter,
                    '-c:v','libx264','-preset','fast','-crf','18','-pix_fmt','yuv420p',
                    '-movflags','+faststart',captioned.name],cwd=output,check=True,timeout=60)
    metadata['editorial_captions'] = {'video':captioned.name,'subtitles':'timeline.srt',
        'description':'Action labels added to the same uncut recording; the raw walkthrough remains available.'}
    metadata['artifacts'] = []
    for path in sorted(output.rglob('*')):
        if path.suffix not in ('.png','.mp4','.srt'):
            continue
        item = {'file':str(path.relative_to(output)), 'bytes':path.stat().st_size,
                }
        with path.open('rb') as source:
            item['sha256'] = hashlib.file_digest(source,'sha256').hexdigest()
        if path.suffix == '.mp4':
            info = json.loads(subprocess.check_output(['ffprobe','-v','error','-show_streams','-show_format','-of','json',str(path)],timeout=15))
            item['video'] = info
            subprocess.run(['ffmpeg','-v','error','-xerror','-i',str(path),'-f','null','-'],
                           check=True,timeout=60)
            streams = [stream for stream in info['streams'] if stream['codec_type'] == 'video']
            if len(streams) != 1 or streams[0]['codec_name'] != 'h264':
                raise RuntimeError(f'invalid video stream: {path}')
            if path == final and (streams[0]['width'], streams[0]['height']) != (1920,1080):
                raise RuntimeError('walkthrough is not full HD')
            if float(info['format']['duration']) < 1:
                raise RuntimeError(f'empty demo recording: {path}')
        metadata['artifacts'].append(item)
    (output/'manifest.json').write_text(json.dumps(metadata,indent=2)+'\n')
    print(f'Demo artifacts: {output}',flush=True)


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary',type=Path,required=True)
    parser.add_argument('--output',type=Path,required=True,help='new directory; existing output is never overwritten')
    parser.add_argument('--scenario', choices=['capture','spaces'], default='capture')
    parser.add_argument('--private-bus',action='store_true',help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not args.private_bus:
        # D-Bus activated helpers inherit these private paths too. Child-only
        # isolation would leave activation services using the caller's HOME.
        with tempfile.TemporaryDirectory(prefix='chonk-demo-bus-') as temporary:
            root = Path(temporary)
            env = environment(root, root/'runtime/unavailable')
            env.pop('DBUS_SESSION_BUS_ADDRESS',None)
            config = root/'bus.conf'
            # No service directories: unrelated portal/notification daemons
            # must not autostart, mount FUSE filesystems, or survive this demo.
            config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                '<auth>EXTERNAL</auth><policy context="default"><allow send_destination="*"/>'
                '<allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            raise SystemExit(subprocess.call(['dbus-run-session',f'--config-file={config}','--',sys.executable,'-B',str(Path(__file__).resolve()),
                *sys.argv[1:],'--private-bus'],env=env))
    run(args)
