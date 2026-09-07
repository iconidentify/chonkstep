#!/usr/bin/env python3
"""Synthetic background memory + real private Wayland clients. Artifact-only.

The worker refuses to allocate unless it is inside a fresh pressure scope with
at most 2 GiB MemoryMax and no swap. No existing desktop/session is a target.
"""
import argparse
import contextlib
import hashlib
import json
import mmap
import os
from pathlib import Path
import re
import shlex
import sys
import time


def write_json(path, value):
    temporary = path.with_name(path.name + '.tmp')
    temporary.write_text(json.dumps(value, indent=2) + '\n')
    os.replace(temporary, path)


def scope_limits():
    relative = next(line.split(':', 2)[2] for line in Path('/proc/self/cgroup').read_text().splitlines()
                    if line.startswith('0::'))
    path = Path(relative)
    if not re.fullmatch(r'chonk-capture-pressure-[0-9a-f]{32}\.scope', path.name):
        raise RuntimeError('mixed workload requires its own bench-pressure.py scope')
    group = Path('/sys/fs/cgroup') / str(path).lstrip('/')
    ceiling = (group / 'memory.max').read_text().strip()
    if ceiling == 'max' or not 128 * 1024**2 <= int(ceiling) <= 2 * 1024**3:
        raise RuntimeError('mixed workload needs a 128 MiB–2 GiB explicit memory ceiling')
    if (group / 'memory.swap.max').read_text().strip() != '0':
        raise RuntimeError('mixed workload requires MemorySwapMax=0')
    return {'cgroup': str(group), 'memory_max_bytes': int(ceiling)}


def touched(size, generation):
    value = mmap.mmap(-1, size, flags=mmap.MAP_PRIVATE | mmap.MAP_ANONYMOUS)
    for offset in range(0, size, mmap.PAGESIZE):
        value[offset] = generation % 254 + 1
    return value


def memory_worker(args):
    limits = scope_limits()
    if not 1 <= args.payload_mib <= 1800 or not 1 <= args.churn_mib <= 64:
        raise ValueError('payload must be 1–1800 MiB and churn 1–64 MiB')
    peak = (args.payload_mib + 2 * args.churn_mib) * 1024**2
    if peak > limits['memory_max_bytes'] - 96 * 1024**2:
        raise ValueError('worker peak must leave at least 96 MiB under group MemoryMax')
    if not 1 <= args.duration_seconds <= 1800:
        raise ValueError('worker duration must be 1–1800 seconds')
    start = time.monotonic()
    payload = touched(args.payload_mib * 1024**2, 1)
    churn = touched(args.churn_mib * 1024**2, 1)
    with args.events.open('x', buffering=1) as events:
        generation = 0
        while time.monotonic() - start < args.duration_seconds:
            generation += 1
            if generation > 1:
                replacement = touched(args.churn_mib * 1024**2, generation)
                churn.close()
                churn = replacement
                # Retain resident private pages, without unbounded allocations.
                for offset in range(0, len(payload), mmap.PAGESIZE):
                    payload[offset] = generation % 254 + 1
            record = {'generation': generation, 'pid': os.getpid(),
                      'monotonic_ns': time.monotonic_ns(), 'payload_mib': args.payload_mib,
                      'churn_mib': args.churn_mib, 'peak_worker_mapping_bytes': peak, **limits}
            events.write(json.dumps(record) + '\n')
            write_json(args.ready, record)
            print(f'Synthetic bounded memory workload: {args.payload_mib} MiB retained; heartbeat {generation}', flush=True)
            time.sleep(2)
    churn.close()
    payload.close()
    raise TimeoutError('bounded worker runtime ended before fixture cleanup')


def records(path):
    if not path.exists():
        return []
    text = path.read_text()
    # A writer can be observed midway through its final record.
    lines = text[:text.rfind('\n') + 1].splitlines()
    return [json.loads(line) for line in lines if line.strip()]


class Fixture:
    def __init__(self, base, capture, args, root, artifact, env, socket_path, door):
        self.b, self.c, self.args = base, capture, args
        self.root, self.artifact, self.door = root, artifact, door
        self.env = env | {'WAYLAND_DISPLAY': str(socket_path)}
        self.env.pop('CHONKSTEP_TEST_SOCKET', None)
        self.children = {}
        self.stack = contextlib.ExitStack()
        self.phase_samples = []
        self.started = time.monotonic_ns()
        self.limits = scope_limits()

    def launch(self, name, command):
        process = self.stack.enter_context(self.b.child(command, self.env, self.artifact / f'{name}.log'))
        self.children[name] = process
        return process

    def alive(self):
        exits = {name: process.poll() for name, process in self.children.items() if process.poll() is not None}
        if exits:
            write_json(self.artifact / 'mixed-failure.json', {'unexpected_client_exits': exits})
            raise RuntimeError(f'mixed workload client exited unexpectedly: {exits}')

    def world(self):
        return self.door.query('windows', multiple=True)

    def window(self, app):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            self.alive()
            world = self.world()
            for line in world:
                if line.startswith('window ') and f'app="{app}"' in line and 'mapped=true' in line:
                    fields = dict(word.split('=', 1) for word in shlex.split(line)[1:])
                    return {key: int(fields[key]) for key in ('id', 'x', 'y', 'w', 'h')}
            time.sleep(.01)
        raise TimeoutError(f'mixed client {app} did not map')

    def place(self, app, x, y):
        window = self.window(app)
        frame = next((line for line in self.world()
                      if line.startswith('frame ') and f'window={window["id"]} ' in line), None)
        if frame is None:
            raise RuntimeError(f'cannot place decorated mixed client {app}')
        fields = dict(word.split('=', 1) for word in frame.split()[1:])
        anchor_x, anchor_y = int(fields['x']) + 100, int(fields['y']) + 10
        self.c.send(self.door, f'motion {anchor_x} {anchor_y}')
        self.c.send(self.door, 'button left press')
        self.door.query('barrier')
        for step in range(1, 4):
            self.c.send(self.door, f'motion {anchor_x + (x-window["x"])*step/3} {anchor_y + (y-window["y"])*step/3}')
            self.door.query('barrier')
        self.c.send(self.door, 'button left release')
        self.door.query('barrier')

    def start(self):
        try:
            self.launch('animation', [str(self.args.animation_probe), 'pressure-animation',
                                     'pressure-animation', 'animate-frame'])
            self.place('pressure-animation', 25, 75)
            self.launch('foot', [str(self.args.foot), '--config=/dev/null', '--title=pressure-memory',
                                '--app-id=pressure-memory', '--window-size-pixels=420x260',
                                sys.executable, '-B', str(Path(__file__).resolve()), '--memory-worker',
                                '--payload-mib', str(self.args.payload_mib), '--churn-mib', str(self.args.churn_mib),
                                '--duration-seconds', str(self.args.worker_seconds),
                                '--events', str(self.artifact / 'worker-events.jsonl'),
                                '--ready', str(self.artifact / 'worker-ready.json')])
            self.place('pressure-memory', 700, 75)
            self.launch('input', [str(self.args.input_probe), '1', '--app-id=pressure-input'])
            self.place('pressure-input', 700, 430)
            deadline = time.monotonic() + 45
            while not (self.artifact / 'worker-ready.json').exists():
                self.alive()
                if time.monotonic() >= deadline:
                    raise TimeoutError('bounded memory worker did not become ready')
                time.sleep(.01)
            self.ready_ms = (time.monotonic_ns() - self.started) / 1e6
            self.initial = self.snapshot()
            self.input_before = self.input_receipt()
            write_json(self.artifact / 'mixed-ready.json', {'mixed_setup_ms': self.ready_ms,
                       'world': self.world(), 'snapshot': self.initial, 'input_receipt': self.input_before,
                       'memory_workload': 'synthetic bounded anonymous memory; real private foot terminal',
                       'scope': self.limits})
        except BaseException:
            self.close()
            raise
        return self

    def snapshot(self):
        self.alive()
        events = records(self.artifact / 'worker-events.jsonl')
        log = (self.artifact / 'animation.log').read_text(errors='replace')
        callbacks = [int(value) for value in re.findall(r'frame callback=(\d+)', log)]
        return {'monotonic_ns': time.monotonic_ns(), 'frame_callbacks': max(callbacks, default=0),
                'worker': events[-1] if events else None,
                'worker_process': (self.b.proc_snapshot(events[-1]['pid'], self.artifact / f'mixed-memory-worker-{time.monotonic_ns()}')
                                   if events else None),
                'children': {name: self.b.proc_snapshot(process.pid, self.artifact / f'mixed-{name}-{time.monotonic_ns()}')
                             for name, process in self.children.items()}}

    def input_receipt(self):
        window = self.window('pressure-input')
        self.c.send(self.door, f'motion {window["x"]+window["w"]/2} {window["y"]+window["h"]/2}')
        self.c.send(self.door, 'button left press')
        self.c.send(self.door, 'button left release')
        self.door.query('barrier')
        log = self.artifact / 'input.log'
        offset = log.stat().st_size
        start = time.monotonic_ns()
        self.c.tap(self.door, 30)  # a, with no modifiers or capture UI active
        deadline = time.monotonic() + self.args.input_deadline_ms / 1000
        while time.monotonic() < deadline:
            self.alive()
            with log.open('rb') as stream:
                stream.seek(offset)
                if b'keyboard key 30 down' in stream.read():
                    return {'receipt_ms': (time.monotonic_ns() - start) / 1e6,
                            'measurement': 'injected key through real client log receipt; 1 ms polling',
                            'deadline_ms': self.args.input_deadline_ms}
            time.sleep(.001)
        raise TimeoutError('input observer did not receive key before configured deadline')

    def phase(self, name, action):
        before = self.snapshot()
        started = time.monotonic_ns()
        result = action()
        ended = time.monotonic_ns()
        after = self.snapshot()
        event = {'name': name, 'before': before, 'after': after,
                 'start_monotonic_ns': started, 'end_monotonic_ns': ended,
                 'frame_callbacks': after['frame_callbacks'] - before['frame_callbacks']}
        self.phase_samples.append(event)
        write_json(self.artifact / 'mixed-phases.json', self.phase_samples)
        if name in ('first-open', 'warm-open'):
            durations = result['result'] if name == 'warm-open' else [result['result']]
            if any(value > self.args.capture_deadline_ms for value in durations):
                raise TimeoutError(f'{name} exceeded {self.args.capture_deadline_ms} ms capture deadline')
        return result

    def finish(self):
        receipt = self.input_receipt()
        final = self.snapshot()
        if final['frame_callbacks'] <= self.initial['frame_callbacks']:
            raise RuntimeError('frame-paced animation made no progress')
        closed = next(item for item in self.phase_samples if item['name'] == 'closed-idle')
        if closed['frame_callbacks'] == 0:
            raise RuntimeError('visible animation stopped during closed capture phase')
        if (final['worker'] is None or final['worker']['generation'] <= self.initial['worker']['generation']
                or (time.monotonic_ns() - final['worker']['monotonic_ns']) / 1e9 > 6):
            raise RuntimeError('bounded worker heartbeat stalled')
        result = {'setup_ms': self.ready_ms, 'input_before': self.input_before, 'input_after': receipt,
                  'initial': self.initial, 'final': final, 'phase_samples': self.phase_samples,
                  'capture_deadline_ms': self.args.capture_deadline_ms}
        write_json(self.artifact / 'mixed-result.json', result)
        return result

    def close(self):
        self.stack.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--memory-worker', action='store_true', required=True)
    parser.add_argument('--payload-mib', type=int, required=True)
    parser.add_argument('--churn-mib', type=int, required=True)
    parser.add_argument('--duration-seconds', type=int, required=True)
    parser.add_argument('--events', type=Path, required=True)
    parser.add_argument('--ready', type=Path, required=True)
    memory_worker(parser.parse_args())
