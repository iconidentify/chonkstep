#!/usr/bin/env python3
"""A stand-in for chonkstep's control socket, for developing clients.

Speaks docs/control-socket.md version 1 the way the shell does — hello and
a full snapshot on connect, complete events on every change, `snapshot`
and `focus-workspace` requests, `error` for anything else — without a
compositor. It is what the Omarchy plugins under omarchy/plugins/ were
built against before the server existed, and it stays around so a widget
author can watch their widget react to scripted state without disturbing
their own desktop.

Standard library only; no dependencies to install.

    fake-control-socket.py --socket /tmp/ctl-fake.sock --script
    CHONKSTEP_CONTROL_SOCKET=/tmp/ctl-fake.sock quickshell -p ...

Every request a client sends is logged to stderr, so "did my click reach
the socket" is answered by watching the terminal. State can also be
nudged from stdin: type a JSON object such as {"active": 2} or
{"windows": [3, 0, 1, 1]} or {"theme": {"name": "Ristretto"}} and the
matching facet is re-sent to every client, exactly as the shell would.
"""

import argparse
import json
import os
import selectors
import signal
import socket
import sys
import time

PROTOCOL = 1
LINE_LIMIT = 65536
OUTBOUND_LIMIT = 262144
FACETS = ("workspaces", "outputs", "focus", "theme")


def default_socket_path():
    exported = os.environ.get("CHONKSTEP_CONTROL_SOCKET")
    if exported:
        return exported
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if not runtime:
        return ""
    display = os.environ.get("WAYLAND_DISPLAY") or os.environ.get("DISPLAY") or "default"
    display = display.lstrip(":")
    display = "".join(c if (c.isascii() and (c.isalnum() or c in "_-")) else "_" for c in display)[:32]
    return os.path.join(runtime, "chonkstep", f"control-{display}.sock")


def log(msg):
    sys.stderr.write(f"[fake-control-socket {time.strftime('%H:%M:%S')}] {msg}\n")
    sys.stderr.flush()


class State:
    """The desktop as the fake believes it to be. Each facet method
    returns a complete event, never a diff, matching the spec's second
    invariant."""

    def __init__(self, windows, active, theme, protocol):
        self.windows = list(windows)
        self.active = active
        self.theme = dict(theme)
        self.protocol = protocol
        self.focused_id = 2147483650

    def hello(self):
        return {"event": "hello", "protocol": self.protocol, "session": "wayland", "pid": os.getpid()}

    def workspaces(self):
        return {
            "event": "workspaces",
            "active": self.active,
            "workspaces": [{"index": i, "windows": n} for i, n in enumerate(self.windows)],
        }

    def outputs(self):
        return {
            "event": "outputs",
            "focused": 0,
            "outputs": [{"index": 0, "name": "fake-1", "x": 0, "y": 0,
                         "width": 1600, "height": 900, "scale": 1.0}],
        }

    def focus(self):
        count = sum(self.windows)
        window = None
        if self.windows[self.active] > 0:
            window = {"id": self.focused_id, "title": "~ — foot", "app_id": "foot",
                      "workspace": self.active}
        return {"event": "focus", "window": window, "count": count}

    def theme_event(self):
        return {"event": "theme", **self.theme}

    def facet(self, name):
        return {"workspaces": self.workspaces, "outputs": self.outputs,
                "focus": self.focus, "theme": self.theme_event}[name]()

    def snapshot(self):
        return [self.facet(name) for name in FACETS]

    def apply(self, change):
        """Take a stdin/script nudge and return the facets it touched."""
        touched = []
        if "windows" in change:
            windows = [max(0, int(n)) for n in change["windows"]]
            if windows:
                self.windows = windows
                self.active = min(self.active, len(windows) - 1)
                touched += ["workspaces", "focus"]
        if "active" in change:
            index = int(change["active"])
            if 0 <= index < len(self.windows):
                self.active = index
                touched += ["workspaces", "focus"]
        if "theme" in change and isinstance(change["theme"], dict):
            self.theme.update(change["theme"])
            touched.append("theme")
        return list(dict.fromkeys(touched))


class Client:
    def __init__(self, conn):
        self.conn = conn
        self.inbuf = b""
        self.outbuf = b""


class Server:
    def __init__(self, path, state):
        self.path = path
        self.state = state
        self.selector = selectors.DefaultSelector()
        self.clients = {}
        os.makedirs(os.path.dirname(path) or ".", mode=0o700, exist_ok=True)
        try:
            os.unlink(path)
        except FileNotFoundError:
            pass
        self.listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.listener.bind(path)
        os.chmod(path, 0o600)
        self.listener.listen(8)
        self.listener.setblocking(False)
        self.selector.register(self.listener, selectors.EVENT_READ)
        log(f"listening on {path}")

    # -------------------------------------------------------------- plumbing

    def accept(self, _mask):
        conn, _ = self.listener.accept()
        conn.setblocking(False)
        client = Client(conn)
        self.clients[conn.fileno()] = client
        self.selector.register(conn, selectors.EVENT_READ)
        log(f"client {conn.fileno()} connected")
        self.send(client, self.state.hello())
        for event in self.state.snapshot():
            self.send(client, event)

    def drop(self, client, why):
        fd = client.conn.fileno()
        log(f"client {fd} disconnected: {why}")
        try:
            self.selector.unregister(client.conn)
        except (KeyError, ValueError):
            pass
        client.conn.close()
        self.clients.pop(fd, None)

    def send(self, client, message):
        client.outbuf += (json.dumps(message, separators=(",", ":")) + "\n").encode()
        if len(client.outbuf) > OUTBOUND_LIMIT:
            self.drop(client, "outbound buffer over limit (client stopped reading)")
            return
        self.flush(client)

    def flush(self, client):
        if not client.outbuf:
            return
        try:
            sent = client.conn.send(client.outbuf)
            client.outbuf = client.outbuf[sent:]
        except BlockingIOError:
            pass
        except OSError as e:
            self.drop(client, f"write failed: {e}")
            return
        mask = selectors.EVENT_READ | (selectors.EVENT_WRITE if client.outbuf else 0)
        try:
            self.selector.modify(client.conn, mask)
        except (KeyError, ValueError):
            pass

    def broadcast(self, message):
        for client in list(self.clients.values()):
            self.send(client, message)

    def publish(self, facets):
        for name in facets:
            self.broadcast(self.state.facet(name))

    # --------------------------------------------------------------- requests

    def handle_line(self, client, raw):
        text = raw.decode("utf-8", "replace").strip()
        if not text:
            return
        log(f"client {client.conn.fileno()} -> {text}")
        try:
            message = json.loads(text)
        except ValueError:
            self.send(client, {"event": "error", "request": None, "message": "not a JSON object"})
            return
        if not isinstance(message, dict) or "request" not in message:
            self.send(client, {"event": "error", "request": None, "message": "missing request key"})
            return
        request = message["request"]
        if request == "snapshot":
            for event in self.state.snapshot():
                self.send(client, event)
        elif request == "focus-workspace":
            index = message.get("index")
            if not isinstance(index, int) or isinstance(index, bool) \
                    or not 0 <= index < len(self.state.windows):
                self.send(client, {"event": "error", "request": request,
                                   "message": f"no workspace {index} ({len(self.state.windows)} exist)"})
                return
            self.state.active = index
            self.publish(["workspaces", "focus"])
        else:
            self.send(client, {"event": "error", "request": request,
                               "message": f"unknown request {request!r}"})

    def read_client(self, client):
        try:
            data = client.conn.recv(65536)
        except BlockingIOError:
            return
        except OSError as e:
            self.drop(client, f"read failed: {e}")
            return
        if not data:
            self.drop(client, "closed by client")
            return
        client.inbuf += data
        while b"\n" in client.inbuf and client.conn.fileno() != -1:
            line, client.inbuf = client.inbuf.split(b"\n", 1)
            if len(line) + 1 > LINE_LIMIT:
                self.drop(client, "line over 65536 bytes")
                return
            self.handle_line(client, line)
        if len(client.inbuf) >= LINE_LIMIT:
            self.drop(client, "line over 65536 bytes without a newline")

    # ------------------------------------------------------------------ loop

    def serve(self, script=None, interval=2.0, stdin=None):
        if stdin is not None:
            self.selector.register(stdin, selectors.EVENT_READ)
        next_step = time.monotonic() + interval if script else None
        step = 0
        while True:
            timeout = None if next_step is None else max(0.0, next_step - time.monotonic())
            for key, mask in self.selector.select(timeout):
                if key.fileobj is self.listener:
                    self.accept(mask)
                elif stdin is not None and key.fileobj is stdin:
                    line = stdin.readline()
                    if not line:
                        self.selector.unregister(stdin)
                        stdin = None
                        continue
                    self.nudge(line)
                else:
                    client = self.clients.get(key.fd)
                    if client is None:
                        continue
                    if mask & selectors.EVENT_WRITE:
                        self.flush(client)
                    if mask & selectors.EVENT_READ and key.fd in self.clients:
                        self.read_client(client)
            if next_step is not None and time.monotonic() >= next_step:
                change = script[step % len(script)]
                step += 1
                log(f"script step {step}: {json.dumps(change)}")
                self.publish(self.state.apply(change))
                next_step = time.monotonic() + interval

    def nudge(self, line):
        line = line.strip()
        if not line:
            return
        try:
            change = json.loads(line)
        except ValueError:
            log(f"stdin: not JSON, ignored: {line!r}")
            return
        if not isinstance(change, dict):
            log("stdin: expected a JSON object, ignored")
            return
        touched = self.state.apply(change)
        log(f"stdin: {json.dumps(change)} -> {touched or 'nothing changed'}")
        self.publish(touched)

    def close(self):
        for client in list(self.clients.values()):
            self.drop(client, "server exiting")
        self.listener.close()
        try:
            os.unlink(self.path)
        except FileNotFoundError:
            pass
        log("stopped")


# The built-in script walks the states a workspace widget must get right:
# switching focus, a workspace filling and emptying, the list growing by
# one and shrinking back, and a theme change. Two seconds a step is slow
# enough to watch and fast enough to sit through.
DEFAULT_SCRIPT = [
    {"active": 1},
    {"active": 2},
    {"windows": [3, 2, 1, 1]},
    {"active": 3, "theme": {"id": "ristretto", "name": "Ristretto", "appearance": "dark", "following": "omarchy"}},
    {"windows": [3, 0, 1]},
    {"active": 0, "theme": {"id": "nextstep-classic", "name": "NeXTSTEP Classic", "appearance": "dark", "following": None}},
    {"windows": [3, 0, 1]},
]


def parse_args():
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--socket", default=default_socket_path(),
                   help="path to listen on (default: $CHONKSTEP_CONTROL_SOCKET, else the spec's derivation)")
    p.add_argument("--windows", default="3,0,1",
                   help="comma-separated window count per workspace; the count is the workspace count (default 3,0,1)")
    p.add_argument("--active", type=int, default=0, help="0-based active workspace (default 0)")
    p.add_argument("--theme", default="nextstep-classic", help="theme id (default nextstep-classic)")
    p.add_argument("--theme-name", default="NeXTSTEP Classic", help="theme display name")
    p.add_argument("--appearance", choices=("dark", "light"), default="dark")
    p.add_argument("--script", nargs="?", const="-", metavar="FILE",
                   help="cycle through state changes on a timer; with no FILE, a built-in sequence. "
                        "FILE is a JSON array of nudge objects (same shape as stdin accepts)")
    p.add_argument("--interval", type=float, default=2.0, help="seconds between script steps (default 2)")
    p.add_argument("--protocol", type=int, default=PROTOCOL,
                   help="protocol number to claim in hello; anything but 1 lets you watch a client refuse")
    p.add_argument("--no-stdin", action="store_true", help="do not read nudges from stdin")
    return p.parse_args()


def nudge_source():
    """stdin, unless reading it would be a mistake. A pipe from a test is a
    fine source of nudges. A terminal is too — but only while this process
    is in the foreground: a background job that reads its tty is stopped by
    SIGTTIN, and a fake server that silently freezes the moment it is
    backgrounded with `&` is worse than one that ignores stdin."""
    stdin = sys.stdin
    if stdin is None or stdin.closed:
        return None
    try:
        if stdin.isatty() and os.tcgetpgrp(stdin.fileno()) != os.getpgrp():
            return None
    except OSError:
        return None
    return stdin


def main():
    args = parse_args()
    if not args.socket:
        sys.exit("fake-control-socket: no --socket, no CHONKSTEP_CONTROL_SOCKET, no XDG_RUNTIME_DIR")
    windows = [max(0, int(n)) for n in args.windows.split(",") if n.strip() != ""]
    if not windows:
        sys.exit("fake-control-socket: --windows needs at least one workspace")
    active = min(max(0, args.active), len(windows) - 1)
    theme = {"id": args.theme, "name": args.theme_name, "appearance": args.appearance, "following": None}
    state = State(windows, active, theme, args.protocol)

    script = None
    if args.script is not None:
        if args.script == "-":
            script = DEFAULT_SCRIPT
        else:
            with open(args.script) as f:
                script = json.load(f)
            if not isinstance(script, list) or not script:
                sys.exit("fake-control-socket: --script FILE must hold a non-empty JSON array")

    stdin = None if args.no_stdin else nudge_source()

    server = Server(args.socket, state)
    stop = lambda *_: (_ for _ in ()).throw(KeyboardInterrupt)
    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        server.serve(script=script, interval=args.interval, stdin=stdin)
    except KeyboardInterrupt:
        pass
    finally:
        server.close()


if __name__ == "__main__":
    main()
