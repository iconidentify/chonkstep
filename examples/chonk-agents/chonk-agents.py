#!/usr/bin/env python3
"""Local agent session broker for the Omarchy menu bar.

All state originates in provider events or child exit status. No log scraping,
credentials, telemetry, automatic approvals, or compositor-side model calls.
"""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import selectors
import signal
import socket
import struct
import subprocess
import sys
import threading
import time
import tomllib
import uuid

HERE = Path(__file__).resolve().parent
if str(HERE) not in sys.path:
    sys.path.insert(0, str(HERE))
from sessions import RuntimeStore, process_identity, identity as session_identity, normalize as normalize_session
LIMIT = 65536
PROVIDERS = ("claude", "codex", "opencode", "grok")
STATES = ("working", "waiting", "complete", "error", "idle")
REQUEST_TIMEOUT = 2.0
COLOR_ROLES = ("background", "panel", "surface", "raised", "text", "muted", "line",
               "accent", "accent_text", "selection", "success", "danger")


def endpoint() -> Path:
    base = os.environ.get("XDG_RUNTIME_DIR")
    if not base:
        raise RuntimeError("XDG_RUNTIME_DIR is required")
    directory = Path(base) / "chonk-agents"
    directory.mkdir(mode=0o700, exist_ok=True)
    stat = directory.lstat()
    if not directory.is_dir() or directory.is_symlink() or stat.st_uid != os.getuid() or stat.st_mode & 0o077:
        raise RuntimeError("agent runtime directory must be private and owned by you")
    return directory / "events.sock"


def encode(value) -> bytes:
    data = json.dumps(value, ensure_ascii=False).encode()
    if len(data) > LIMIT:
        raise ValueError("agent message exceeds 64 KiB")
    return data


def connect() -> socket.socket:
    peer = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    peer.settimeout(2)
    try:
        peer.connect(str(endpoint()))
    except BaseException:
        peer.close()
        raise
    return peer


def request(value):
    with connect() as peer:
        peer.sendall(encode(value))
        raw, _, flags, _ = peer.recvmsg(LIMIT)
        if not raw or flags & socket.MSG_TRUNC:
            raise ValueError("invalid broker response")
        response = json.loads(raw)
        if not isinstance(response, dict) or "error" in response:
            raise ValueError(clean(response.get("error", "invalid broker response"))
                             if isinstance(response, dict) else "invalid broker response")
        return response


def clean(text, limit=160):
    # Limits are bytes, so 32 Unicode-rich sessions still fit one snapshot.
    return "".join(c for c in str(text) if c.isprintable()).encode("utf-8")[:limit].decode("utf-8", "ignore")


def normalize_palette(palette):
    if not isinstance(palette, dict) or len(encode(palette)) > 8192:
        raise ValueError("invalid palette")
    out = {}
    for role in COLOR_ROLES:
        if role not in palette:
            continue
        value = palette[role]
        if not isinstance(value, dict) or any(type(value.get(c)) is not int or not 0 <= value[c] <= 255 for c in "rgb"):
            raise ValueError("invalid palette color")
        out[role] = {c: value[c] for c in "rgb"}
    return out


def fit_label(value, width, measure, limit=120):
    """Bounded fitting, including grants too narrow to show even an ellipsis."""
    value = clean(value, limit)
    if width <= 0:
        return ""
    if measure(value) <= width:
        return value
    if measure("…") > width:
        return ""
    while value and measure(value + "…") > width:
        value = value[:-1]
    return value + "…"


def bundled_font(name="IBMPlexMono-Regular.ttf"):
    paths = (HERE / "fonts" / name,
             HERE.parent.parent / "crates/wm-theme/assets/fonts/modern" / name)
    return next((path for path in paths if path.is_file()), None)


def register_review_font():
    """Register only in this process, before GTK/Pango constructs font maps."""
    import ctypes
    paths = [path for name in ("IBMPlexMono-Regular.ttf", "IBMPlexSans[wdth,wght].ttf") if (path := bundled_font(name)) is not None]
    if not paths:
        return False
    try:
        config = ctypes.CDLL("libfontconfig.so.1")
        config.FcConfigGetCurrent.restype = ctypes.c_void_p
        config.FcConfigAppFontAddFile.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
        config.FcConfigAppFontAddFile.restype = ctypes.c_int
        current = config.FcConfigGetCurrent()
        registered = [bool(config.FcConfigAppFontAddFile(current, os.fsencode(path))) for path in paths]
        return all(registered)
    except (OSError, AttributeError):
        return False


def review_css(palette):
    palette = normalize_palette(palette)
    def color(name, default):
        value = palette.get(name, {})
        return "#" + "".join(f"{value.get(c, d):02x}" for c, d in zip("rgb", default))
    return (f"window {{background:{color('panel',(32,36,33))};color:{color('text',(224,227,215))};font-family:'IBM Plex Mono';font-size:12px;}}"
        f".title {{font-family:'IBM Plex Sans','IBM Plex Mono';font-size:22px;}} .muted {{color:{color('muted',(164,173,159))};}}"
        f".card {{background:{color('surface',(25,29,26))};border:1px solid {color('line',(65,73,62))};padding:18px;border-radius:3px;margin-top:8px;}}"
        f"headerbar {{background:{color('surface',(25,29,26))};color:{color('text',(224,227,215))};box-shadow:none;}}"
        f"headerbar label {{font-size:12px;}} headerbar button {{background:transparent;color:{color('muted',(164,173,159))};padding:4px;box-shadow:none;}}"
        f"headerbar button:hover {{background:{color('raised',(43,48,43))};color:{color('text',(224,227,215))};}}"
        f"button {{background:{color('accent',(227,181,102))};color:{color('accent_text',(23,27,23))};border:0;padding:10px;}}").encode()


def reported_at(row):
    return time.strftime("%b %d %H:%M", time.localtime(row["updated"]))


def bounded_command(command, cwd, limit, timeout=5):
    """Worker-only subprocess capture with bounded output and lifetime."""
    process = subprocess.Popen(command, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    expired = threading.Event()
    def expire():
        expired.set()
        process.kill()
    timer = threading.Timer(timeout, expire)
    timer.start()
    try:
        data = process.stdout.read(limit + 1)
        if len(data) > limit:
            process.kill()
        result = process.wait()
        return result, data, expired.is_set()
    finally:
        timer.cancel()
        process.stdout.close()
        if process.poll() is None:
            process.kill()
            process.wait()


def read_workspace_diff(cwd):
    # --no-ext-diff and --no-textconv do not disable clean/process filters
    # from .gitattributes. Enumerate configuration keys without running them,
    # and override every such driver for this one review command.
    base = ["git", "--no-pager", "--no-optional-locks", "-c", "core.fsmonitor=false"]
    result, keys, expired = bounded_command(base + ["config", "--name-only", "--null", "--get-regexp",
        r"^filter\..*\.(clean|process|required)$"], cwd, LIMIT)
    if expired or len(keys) > LIMIT or result not in (0, 1):
        raise ValueError("Could not safely read workspace filter configuration")
    overrides = []
    for key in keys.split(b"\0"):
        if not key:
            continue
        key = os.fsdecode(key)
        overrides.extend(["-c", key + ("=false" if key.endswith(".required") else "=")])
    _, data, expired = bounded_command(base + overrides + ["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--"], cwd, 131072)
    suffix = "\n[Preview timed out]" if expired else ""
    if len(data) > 131072:
        data = data[:131072]
        suffix += "\n[Preview limited to 128 KiB]"
    return (data.decode(errors="replace") or "No tracked changes. Untracked files are not part of this diff.") + suffix


def normalize(event: dict) -> dict:
    """Small provider-independent contract; reject arbitrary status values."""
    if not isinstance(event, dict) or event.get("provider") not in PROVIDERS or event.get("status") not in STATES:
        raise ValueError("unsupported provider/status")
    session = clean(event.get("id", ""), 80)
    if not session:
        raise ValueError("session id is required")
    cwd = event.get("cwd", ".")
    if not isinstance(cwd, str) or not cwd or "\0" in cwd or len(os.fsencode(cwd)) > 1024:
        raise ValueError("invalid workspace path")
    # Do not resolve symlinks or query a possibly remote filesystem in the
    # broker event loop. The review process resolves the path when opened.
    cwd = os.path.abspath(cwd)
    if len(os.fsencode(cwd)) > 1024:
        raise ValueError("workspace path is too long")
    if any(not isinstance(event.get(key, ""), str) for key in ("id", "title", "detail")):
        raise ValueError("session text must be strings")
    return {"id": session, "provider": event["provider"], "status": event["status"],
            "title": clean(event.get("title", "Agent session"), 100), "cwd": cwd,
            "detail": clean(event.get("detail", ""), 400), "updated": int(time.time())}


class Store:
    def __init__(self, state_path=None):
        self.sessions = {}
        self.palette = {}
        self.revision = 0
        self.runtime = RuntimeStore(state_path)

    def update(self, event):
        event = normalize(event)
        key = (event["provider"], event["id"])
        prior = self.sessions.get(key)
        if prior and all(prior.get(key) == event[key] for key in event if key != "updated"):
            return False
        candidate = self.sessions.copy()
        if key not in candidate and len(candidate) >= 32:
            removable = next((key for key, row in self.sessions.items() if row["status"] in ("complete", "error", "idle")), None)
            if removable is None:
                raise ValueError("32 live sessions already registered")
            del candidate[removable]
        candidate[key] = event
        # JSON escaping can expand otherwise bounded text. Refuse a change
        # atomically if the whole subscriber snapshot would exceed one packet.
        encode({**self.snapshot(), "revision": self.revision + 1,
                "sessions": list(candidate.values()) + self.runtime.public_rows()})
        self.sessions = candidate
        self.revision += 1
        return True

    def snapshot(self):
        return {"protocol": 2, "revision": self.revision,
                "persistence": "error" if self.runtime.restore_error or (self.runtime.writer and self.runtime.writer.error) else "ready" if self.runtime.writer else "disabled",
                "sessions": list(self.sessions.values()) + self.runtime.public_rows(), "palette": self.palette}

    def update_session(self, event):
        def validate(rows):
            encode({**self.snapshot(), "revision": self.revision + 1,
                "sessions": list(self.sessions.values()) + rows})
        changed = self.runtime.update(event, validate)
        if changed:
            self.revision += 1
        return changed

    def set_palette(self, palette):
        palette = normalize_palette(palette)
        if palette == self.palette:
            return False
        encode({**self.snapshot(), "revision": self.revision + 1, "palette": palette})
        self.palette = palette
        self.revision += 1
        return True




def serve():
    path = endpoint()
    # Refuse a live socket, remove only a stale socket owned by this uid.
    if path.exists():
        try:
            with connect():
                raise RuntimeError("agent broker is already running")
        except (ConnectionRefusedError, FileNotFoundError):
            import stat
            if not stat.S_ISSOCK(path.lstat().st_mode) or path.lstat().st_uid != os.getuid():
                raise RuntimeError("refusing to replace non-socket runtime entry")
            path.unlink()
    server = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    server.bind(str(path))
    os.chmod(path, 0o600)
    server.listen(16)
    server.setblocking(False)
    selector = selectors.DefaultSelector()
    selector.register(server, selectors.EVENT_READ)
    subscribers = set()
    monitors = set()
    peer_pids = {}
    session_sources = {}
    deadlines = {}
    state_root = Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state")))
    store = Store(state_root / "chonkstep/agents/sessions-v2.json")
    processes = {}
    review = None
    t3 = None

    def reload_adapters():
        nonlocal t3
        from integrations import t3_config
        if t3 is not None and t3.poll() is None:
            t3.terminate()
        t3 = None
        try:
            if t3_config().get("enabled"):
                t3 = subprocess.Popen([sys.executable, str(HERE / "chonk-agents.py"), "t3"],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except (OSError, ValueError, RecursionError):
            # A broken optional connection cannot take down the local agents.
            print("T3 adapter could not start; check chonk-agents doctor", file=sys.stderr)

    reload_adapters()

    def monitor_processes():
        desired = {(row["pid"], row["start_time"]) for row in store.runtime.rows.values()
                   if row["health"] == "connected" and row["pid"] > 1}
        for identity, fd in tuple(processes.items()):
            if identity not in desired:
                selector.unregister(fd)
                os.close(fd)
                del processes[identity]
        for identity in desired - processes.keys():
            try:
                fd = os.pidfd_open(identity[0])
                current = process_identity(identity[0])
                if current is None or current["start_time"] != identity[1]:
                    os.close(fd)
                    if store.runtime.disconnected(*identity):
                        store.revision += 1
                    continue
                selector.register(fd, selectors.EVENT_READ, ("process", identity))
                processes[identity] = fd
            except (OSError, AttributeError):
                # Last-observed status remains explicit if pidfd is unavailable.
                pass

    def drop(peer):
        watched = peer in monitors
        producer = peer_pids.pop(peer, None)
        subscribers.discard(peer)
        monitors.discard(peer)
        deadlines.pop(peer, None)
        try:
            selector.unregister(peer)
        except (KeyError, ValueError):
            pass
        peer.close()
        if watched and producer:
            keys = [key for key, value in session_sources.items() if value == producer]
            if store.runtime.source_disconnected(keys):
                store.revision += 1
                broadcast()

    def broadcast():
        payload = encode(store.snapshot())
        for peer in tuple(subscribers):
            try:
                peer.send(payload)
            except (BlockingIOError, OSError):
                drop(peer)  # bounded memory; clients can reconnect for a snapshot

    try:
        while True:
            now = time.monotonic()
            for peer, deadline in tuple(deadlines.items()):
                if deadline <= now:
                    drop(peer)
            timeout = max(0, min(deadlines.values()) - now) if deadlines else None
            for key, _ in selector.select(timeout):
                if key.data and key.data[0] == "process":
                    identity = key.data[1]
                    selector.unregister(key.fileobj)
                    os.close(key.fileobj)
                    processes.pop(identity, None)
                    if store.runtime.disconnected(*identity):
                        store.revision += 1
                        broadcast()
                    continue
                if key.fileobj is server:
                    peer, _ = server.accept()
                    producer, uid, _ = struct.unpack("3i", peer.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
                    if uid != os.getuid() or len(subscribers) + len(monitors) + len(deadlines) >= 32:
                        peer.close()
                        continue
                    peer.setblocking(False)
                    selector.register(peer, selectors.EVENT_READ)
                    peer_pids[peer] = producer
                    deadlines[peer] = time.monotonic() + REQUEST_TIMEOUT
                    continue
                peer = key.fileobj
                try:
                    raw, _, flags, _ = peer.recvmsg(LIMIT)
                    if not raw:
                        drop(peer)
                        continue
                    if flags & socket.MSG_TRUNC:
                        raise ValueError("oversized message")
                    message = json.loads(raw)
                    if not isinstance(message, dict):
                        raise ValueError("request must be an object")
                    changed = False
                    op = message.get("op")
                    if op == "event":
                        changed = store.update(message["event"])
                    elif op == "session":
                        changed = store.update_session(message["event"])
                        if message["event"].get("origin") in ("plugin", "t3"):
                            session_key = session_identity(normalize_session(message["event"]))
                            if (session_key in store.runtime.rows and
                                    store.runtime.rows[session_key]["last_ns"] == message["event"].get("observed_ns")):
                                session_sources[session_key] = peer_pids[peer]
                                session_sources = {k: v for k, v in session_sources.items() if k in store.runtime.rows}
                        monitor_processes()
                    elif op == "dismiss":
                        store.runtime.dismiss(message["id"])
                        store.revision += 1
                        changed = True
                        monitor_processes()
                    elif op == "reload-adapters":
                        reload_adapters()
                    elif op == "subscribe":
                        subscribers.add(peer)
                        deadlines.pop(peer, None)
                    elif op == "watch":
                        monitors.add(peer)
                        deadlines.pop(peer, None)
                    elif op == "palette":
                        changed = store.set_palette(message.get("palette", {}))
                    elif op == "review":
                        # The broker was launched by the session and owns its
                        # display environment. A dockapp never receives it.
                        if review is None or review.poll() is not None:
                            review = subprocess.Popen([sys.executable, str(HERE / "chonk-agents.py"), "review"], close_fds=True)
                    elif op != "snapshot":
                        raise ValueError("unknown operation")
                    # Hook emitters intentionally do not wait for replies.
                    # A closed sender must not suppress publication to the dock.
                    if changed:
                        broadcast()
                    if op != "session" or message.get("reply"):
                        peer.send(encode({"ok": True, "revision": store.revision} if op in ("session", "watch") else store.snapshot()))
                    if peer not in subscribers and peer not in monitors:
                        drop(peer)
                except (OSError, ValueError, TypeError, KeyError, RecursionError) as error:
                    try:
                        peer.send(encode({"error": str(error)}))
                    except OSError:
                        pass
                    drop(peer)
    finally:
        for key in list(selector.get_map().values()):
            if isinstance(key.fileobj, int):
                os.close(key.fileobj)
            else:
                key.fileobj.close()
        selector.close()
        store.runtime.close()
        if t3 is not None and t3.poll() is None:
            t3.terminate()
        path.unlink(missing_ok=True)


def provider_command(provider, prompt, model=None):
    if provider not in PROVIDERS:
        raise ValueError("unsupported provider")
    if provider == "codex":
        return ["codex", "exec", "--json", *(["-m", model] if model else []), "--", prompt]
    if provider == "claude":
        return ["claude", "--print", "--verbose", "--output-format", "stream-json", *(["--model", model] if model else []), "--", prompt]
    if provider == "grok" and (not model or not model.startswith("xai/")):
        raise ValueError("Grok requires --model xai/<your-model-id>; uses OpenCode's xAI provider")
    return ["opencode", "run", "--format", "json", *(["--model", model] if model else []), "--", prompt]


def event_detail(provider, event):
    """Only semantic status; unknown events do not invent progress."""
    if not isinstance(event, dict):
        return None
    def obj(value):
        return value if isinstance(value, dict) else {}
    kind = event.get("type", "")
    if kind in ("error", "turn.failed"):
        return "error", clean(event.get("message", event.get("error", "Agent reported an error")))
    if kind in ("permission", "permission.updated", "permission.asked"):
        return "waiting", "Permission requested · respond in the provider"
    if kind in ("item.started", "item.completed"):
        item = obj(event.get("item"))
        if item.get("type") == "command_execution":
            return "working", clean(item.get("command", "Running command"))
        if item.get("type") == "file_change":
            return "working", "Updating files"
        if item.get("type") == "agent_message":
            return "working", clean(item.get("text", ""))
    if kind == "tool_use":
        part = obj(event.get("part"))
        return "working", clean(part.get("tool", "Using tool"))
    if kind == "assistant":
        content = obj(event.get("message")).get("content", [])
        if not isinstance(content, list):
            return None
        for part in content:
            if not isinstance(part, dict):
                continue
            if part.get("type") == "tool_use":
                return "working", clean(part.get("name", "Using tool"))
            if part.get("type") == "text":
                return "working", clean(part.get("text", ""))
    if kind == "result":
        return ("error" if event.get("is_error") else "working"), clean(event.get("result", ""))
    if kind == "text":
        return "working", clean(obj(event.get("part")).get("text", ""))
    return None


def run_agent(args):
    command = provider_command(args.provider, args.prompt, args.model)
    event = {"id": str(uuid.uuid4()), "provider": args.provider, "status": "working",
             "title": clean(args.prompt, 100), "cwd": str(Path.cwd()), "detail": "Starting session"}
    request({"op": "event", "event": event})
    failed = False
    child = None
    try:
        child = subprocess.Popen(command, stdout=subprocess.PIPE, stdin=subprocess.DEVNULL)
        # Provider errors remain on the caller's terminal; preserve stdout too.
        while True:
            raw = child.stdout.readline(LIMIT)
            if not raw:
                break
            sys.stdout.buffer.write(raw)
            sys.stdout.buffer.flush()
            if not raw.endswith(b"\n"):
                # Drop a long record as one unit; never parse a truncated tail.
                while raw and not raw.endswith(b"\n"):
                    raw = child.stdout.readline(LIMIT)
                    sys.stdout.buffer.write(raw)
                continue
            try:
                detail = event_detail(args.provider, json.loads(raw))
                if detail:
                    event["status"], event["detail"] = detail
                    failed |= event["status"] == "error"
                    request({"op": "event", "event": event})
            except (ValueError, OSError):
                pass
        result = child.wait()
    except (KeyboardInterrupt, OSError) as error:
        if child is not None and child.poll() is None:
            child.send_signal(signal.SIGINT)
            try:
                child.wait(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.wait()
        result = 130 if isinstance(error, KeyboardInterrupt) else 127
        event["detail"] = clean(error) or "Interrupted"
    event["status"] = "error" if result or failed else "complete"
    if event["status"] == "complete":
        event["detail"] = "Session finished · review changes"
    try:
        request({"op": "event", "event": event})
    except (OSError, ValueError):
        pass
    return result or int(failed)


def watch(callback):
    """Blocking subscriber, used only on a worker thread; no log-file polling."""
    while True:
        try:
            with connect() as peer:
                peer.sendall(encode({"op": "subscribe"}))
                peer.settimeout(None)
                while True:
                    raw, _, flags, _ = peer.recvmsg(LIMIT)
                    if not raw:
                        break
                    if flags & socket.MSG_TRUNC:
                        raise ValueError("oversized broker snapshot")
                    snapshot = json.loads(raw)
                    if not isinstance(snapshot, dict) or not isinstance(snapshot.get("sessions"), list):
                        raise ValueError("invalid broker snapshot")
                    callback(snapshot)
                callback({"sessions": [], "offline": True, "revision": -1})
        except (OSError, ValueError):
            callback({"sessions": [], "offline": True, "revision": -1})
        time.sleep(2)  # bounded reconnect backoff, outside the compositor




def review():
    register_review_font()
    import gi
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gtk, GLib, Gdk
    app = Gtk.Application(application_id="org.chonkstep.Agents")
    state = {"snapshot": {"sessions":[]}, "box": None, "css": None, "palette": None,
             "cards": {}, "rows": {}, "window": None, "watching": False, "choosers": {}}

    def titlebar(window, title):
        header=Gtk.HeaderBar()
        header.set_show_title_buttons(True)
        header.set_title_widget(Gtk.Label(label=title))
        window.set_titlebar(header)
        keys=Gtk.EventControllerKey()
        def close_key(_, keyval, _keycode, modifiers):
            if keyval==Gdk.KEY_Escape or (keyval in (Gdk.KEY_w,Gdk.KEY_W) and modifiers&Gdk.ModifierType.CONTROL_MASK):
                window.close()
                return True
            return False
        keys.connect("key-pressed",close_key)
        window.add_controller(keys)

    def refresh(snapshot):
        state["snapshot"] = snapshot
        box = state["box"]
        if box is None:
            return False
        palette = snapshot.get("palette", {})
        if palette != state["palette"]:
            state["css"].load_from_data(review_css(palette))
            state["palette"] = palette
        rows = sorted(snapshot.get("sessions",[]),key=lambda row:row["updated"],reverse=True)
        state["rows"] = {row["id"]: row for row in rows}
        for session_id in list(state["cards"]):
            if session_id not in state["rows"]:
                box.remove(state["cards"].pop(session_id)["box"])
        state["empty"].set_visible(not rows)
        previous = state["empty"]
        for row in rows:
            session_id = row["id"]
            card = state["cards"].get(session_id)
            if card is None:
                card_box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=9)
                card_box.add_css_class("card")
                card = {"box": card_box}
                for name in ("status", "timestamp", "title", "detail", "location"):
                    label = Gtk.Label(xalign=0,wrap=name in ("title", "detail", "location"),
                                      selectable=name in ("title", "detail", "location"))
                    if name in ("timestamp", "detail", "location"):
                        label.add_css_class("muted")
                    card[name] = label
                    card_box.append(label)
                card["open"] = Gtk.Button()
                card["open"].connect("clicked",lambda _,key=session_id:open_session(key))
                card_box.append(card["open"])
                changes = Gtk.Button(label="Review workspace changes  ↗")
                changes.connect("clicked",lambda _,key=session_id:review_changes(key))
                card_box.append(changes)
                state["cards"][session_id] = card
                box.append(card_box)
            client = "T3 · " if row.get("client") == "t3" else ""
            qualifier = {"observed": " · reported", "tentative": " · unconfirmed", "unknown": " · unknown"}.get(row.get("confidence"), "")
            values = {"status": f"{client}{row['provider'].upper()}  /  {row['status'].upper()}{qualifier}",
                      "timestamp": f"Last reported {reported_at(row)}", "title": row["title"],
                      "detail": row["detail"], "location": row["cwd"]}
            for name, value in values.items():
                # Unchanged labels retain selection as well as widget identity.
                if card[name].get_label() != value:
                    card[name].set_label(value)
            current = row.get("schema") == 2
            browser = row.get("client") == "t3"
            caps = row.get("capabilities", {})
            exact = caps.get("open_session") is True and row.get("navigation", {}).get("kind") == "opencode"
            label = "Open thread in browser  ↗" if browser else "Open session  ↗" if exact else "Open terminal  ↗"
            if card["open"].get_label() != label:
                card["open"].set_label(label)
            card["open"].set_visible(current)
            card["location"].set_visible(current)
            available = row.get("health") == "connected" and (not browser or caps.get("open_browser") is True)
            card["open"].set_sensitive(available)
            unavailable = "Browser navigation is unavailable for this thread." if browser and caps.get("open_browser") is not True else "Reconnect this session to open it."
            card["open"].set_tooltip_text(None if available else unavailable)
            if card["box"].get_prev_sibling() != previous:
                box.reorder_child_after(card["box"], previous)
            previous = card["box"]
        return False

    def review_changes(session_id):
        row = state["rows"].get(session_id)
        if row is not None:
            show_diff(row["cwd"])

    def open_session(session_id):
        # Resolve on click, away from GTK; no window-list polling or discovery
        # per provider event. Selection windows survive list refreshes.
        existing = state["choosers"].get(session_id)
        if existing is not None:
            existing.present()
            return
        window=Gtk.Window(application=app,title="Open agent session")
        state["choosers"][session_id] = window
        window.set_transient_for(state["window"])
        window.set_destroy_with_parent(True)
        titlebar(window,"Open agent session")
        window.set_default_size(440,200)
        content=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=12)
        for setter in (content.set_margin_top,content.set_margin_bottom,content.set_margin_start,content.set_margin_end):setter(20)
        status=Gtk.Label(label="Finding your session…",xalign=0,wrap=True)
        content.append(status);window.set_child(content);window.present()
        closed = threading.Event()
        chooser = {"busy": True, "buttons": []}
        def on_close(_):
            closed.set()
            if state["choosers"].get(session_id) is window:
                state["choosers"].pop(session_id)
            return False
        window.connect("close-request", on_close)
        window.connect("notify::visible", lambda widget, _: on_close(widget) if not widget.get_visible() else None)

        def current():
            snapshot=request({"op":"snapshot"})
            row=next((row for row in snapshot["sessions"] if row["id"]==session_id and row.get("schema")==2),None)
            if row is None or row.get("health")!="connected":
                raise ValueError("This session is no longer attached. Resume it in the agent to reconnect.")
            return row

        def finish(message, close=False):
            if closed.is_set():
                return False
            chooser["busy"] = False
            if close:
                window.close()
            else:
                status.set_text(message)
                for button in chooser["buttons"]:
                    button.set_sensitive(True)
            return False

        def choose(candidate):
            if closed.is_set() or chooser["busy"]:
                return
            chooser["busy"] = True
            for button in chooser["buttons"]:
                button.set_sensitive(False)
            def run():
                try:
                    from navigation import activate
                    row = current()
                    if closed.is_set():
                        return
                    result=activate(row,candidate["token"],cancelled=closed.is_set)
                    GLib.idle_add(finish,result,True)
                except (OSError,ValueError,RuntimeError) as error:
                    GLib.idle_add(finish,str(error))
            kind = candidate.get("kind")
            status.set_text("Opening the exact session…" if kind == "session" else
                            "Opening the tmux pane…" if kind == "pane" else "Opening the terminal…")
            threading.Thread(target=run,daemon=True).start()

        def show_choices(choices):
            if closed.is_set():
                return False
            chooser["busy"] = False
            if not choices:
                return finish("No unique local terminal could be verified for this session. Open it in the agent directly.")
            if len(choices)==1:
                choose(choices[0])
                return False
            status.set_text("Choose a destination:")
            for candidate in choices:
                action = {"session": "Open session", "pane": "Open tmux pane"}.get(candidate.get("kind"), "Open terminal")
                button=Gtk.Button(label=action + " · " + candidate["label"])
                button.connect("clicked",lambda _,choice=candidate:choose(choice))
                chooser["buttons"].append(button)
                content.append(button)
            return False

        def find():
            try:
                row=current()
                if closed.is_set():
                    return
                if row.get("client")=="t3":
                    if row.get("capabilities", {}).get("open_browser") is not True:
                        raise ValueError("This thread has no connected browser destination.")
                    from integrations import open_t3_thread
                    open_t3_thread(row,cancelled=closed.is_set)
                    GLib.idle_add(finish,"Opened thread in browser.",True)
                else:
                    from navigation import discover
                    GLib.idle_add(show_choices,discover(row))
            except (OSError,ValueError,RuntimeError) as error:
                GLib.idle_add(finish,str(error))
        threading.Thread(target=find,daemon=True).start()

    def show_diff(cwd):
        window=Gtk.Window(application=app,title="Workspace changes")
        titlebar(window,"Workspace changes")
        window.set_default_size(820,580)
        view=Gtk.TextView(editable=False,monospace=True)
        view.get_buffer().set_text("Reading tracked workspace changes…")
        scroll=Gtk.ScrolledWindow();scroll.set_child(view);window.set_child(scroll);window.present()
        def read():
            try:
                text=read_workspace_diff(cwd)
            except (OSError, ValueError) as error:
                text=str(error)
            GLib.idle_add(lambda:view.get_buffer().set_text(text))
        threading.Thread(target=read,daemon=True).start()

    def activate(_):
        if state["window"] is not None:
            state["window"].present()
            return
        window=Gtk.ApplicationWindow(application=app,title="Agent Review")
        state["window"] = window
        def main_closed(_):
            state.update(window=None, box=None, cards={}, palette=None)
            return False
        window.connect("close-request", main_closed)
        titlebar(window,"Agent Review")
        window.set_default_size(680,560)
        css=Gtk.CssProvider()
        state["css"]=css
        Gtk.StyleContext.add_provider_for_display(Gdk.Display.get_default(),css,Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
        scroll=Gtk.ScrolledWindow();box=Gtk.Box(orientation=Gtk.Orientation.VERTICAL,spacing=16)
        for method in (box.set_margin_top,box.set_margin_bottom,box.set_margin_start,box.set_margin_end):method(24)
        title = Gtk.Label(label="Your agents, in one place", xalign=0)
        title.add_css_class("title"); box.append(title)
        sub = Gtk.Label(label="Launch your agents normally. Connected hooks and plugins report activity here; prompts and approvals stay in your agent.", xalign=0, wrap=True)
        sub.add_css_class("muted"); box.append(sub)
        empty = Gtk.Label(label="No connected sessions\n\nRun chonk-agents setup, then start or resume your agent normally.\nUse chonk-agents doctor to check the adapters.", xalign=0, wrap=True)
        box.append(empty); state["empty"] = empty
        state["box"]=box;scroll.set_child(box);window.set_child(scroll)
        refresh(state["snapshot"]);window.present()
        if not state["watching"]:
            state["watching"] = True
            threading.Thread(target=watch,args=(lambda snapshot:GLib.idle_add(refresh,snapshot),),daemon=True).start()
    app.connect("activate",activate);app.run([])


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    commands=parser.add_subparsers(dest="command",required=True)
    for name in ("serve","review","snapshot"):
        commands.add_parser(name)
    runner=commands.add_parser("run");runner.add_argument("provider",choices=PROVIDERS)
    runner.add_argument("prompt");runner.add_argument("--model")
    hook=commands.add_parser("hook");hook.add_argument("provider",choices=PROVIDERS)
    installer=commands.add_parser("setup");installer.add_argument("providers",nargs="*")
    uninstall=commands.add_parser("uninstall");uninstall.add_argument("providers",nargs="*")
    commands.add_parser("doctor")
    streamer=commands.add_parser("stream");streamer.add_argument("provider",choices=("opencode","t3"))
    notify=commands.add_parser("notify-codex");notify.add_argument("payload")
    commands.add_parser("t3")
    connector=commands.add_parser("connect");connector.add_argument("provider",choices=("t3",))
    connector.add_argument("--url",required=True);connector.add_argument("--token-file",required=True)
    args=parser.parse_args()
    if args.command in ("setup","uninstall","doctor","stream","notify-codex","t3","connect"):
        import integrations
        if args.command in ("setup","uninstall"):
            print(json.dumps(integrations.setup(args.providers or ("codex","claude","opencode"),args.command=="uninstall"),indent=2))
        elif args.command=="doctor":print(json.dumps(integrations.doctor(),indent=2))
        elif args.command=="stream":return integrations.stream(args.provider)
        elif args.command=="notify-codex":integrations.notify_codex(args.payload)
        elif args.command=="t3":return integrations.run_t3()
        elif args.command=="connect":print(json.dumps(integrations.connect_t3(args.url,args.token_file),indent=2))
        return 0
    if args.command=="run":return run_agent(args)
    if args.command=="serve":serve()
    elif args.command=="review":review()
    elif args.command=="snapshot":print(json.dumps(request({"op":"snapshot"}),indent=2))
    elif args.command=="hook":
        from integrations import helper_path
        if args.provider in ("codex","claude"):
            try:
                os.execv(str(helper_path()),[str(helper_path()),args.provider])
            except OSError:
                return 0
    return 0


if __name__=="__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, RuntimeError) as error:
        print(f"chonk-agents: {error}",file=sys.stderr);sys.exit(1)
