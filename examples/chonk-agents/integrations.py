"""Local adapter setup and bounded stream supervision; no model execution."""
from __future__ import annotations

import json
import os
from pathlib import Path
import re
import selectors
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import sys
import time
import tomllib
from urllib.parse import urlsplit, urlunsplit, quote

from sessions import normalize, process_identity, MAX_SESSIONS

HERE = Path(__file__).resolve().parent
LIMIT = 65536
MANAGED = "chonk-agents"
CODEX_EVENTS = ("SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse",
    "PermissionRequest", "Stop", "Interrupt", "SubagentStart", "SubagentStop")
CLAUDE_EVENTS = ("SessionStart", "SessionEnd", "UserPromptSubmit", "PreToolUse", "PostToolUse",
    "PostToolUseFailure", "PermissionRequest", "PermissionDenied", "Notification", "Stop",
    "StopFailure", "SubagentStart", "SubagentStop", "Elicitation", "ElicitationResult")


def config_root():
    return Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config")))


def state_root():
    return Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "chonkstep/agents"


def codex_root():
    return Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))


def claude_root():
    return Path(os.environ.get("CLAUDE_CONFIG_DIR", str(Path.home() / ".claude")))


def read_json(path, default=None):
    if not path.exists():
        return {} if default is None else default
    if path.is_symlink() or not path.is_file() or path.stat().st_uid != os.getuid() or path.stat().st_size > 2 * 1024 * 1024:
        raise ValueError(f"Cannot safely update {path}")
    value = json.loads(path.read_text())
    if not isinstance(value, dict):
        raise ValueError(f"Expected a JSON object in {path}")
    return value


def write_atomic(path, data, expected=None):
    path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
    if path.is_symlink() or (path.exists() and path.stat().st_uid != os.getuid()):
        raise ValueError(f"Cannot safely replace {path}")
    current = path.read_bytes() if path.exists() else b""
    if expected is not None and current != expected:
        raise ValueError(f"Configuration changed during setup: {path}; rerun setup")
    temporary = path.with_name(path.name + f".{os.getpid()}.tmp")
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
        os.replace(temporary, path)
    finally:
        temporary.unlink(missing_ok=True)


def json_bytes(value):
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode()


def helper_path():
    return next((p for p in (HERE / "bin/chonk-agent-hook", HERE / "hook-helper/target/release/chonk-agent-hook") if p.is_file()), HERE / "bin/chonk-agent-hook")


def hook_command(engine):
    return shlex.join([str(helper_path()), engine])


def merge_hooks(config, engine, remove=False, previous_command=None):
    """Only our exact handlers are replaced; other matchers/settings survive."""
    config = json.loads(json.dumps(config))
    hooks = config.setdefault("hooks", {})
    if not isinstance(hooks, dict):
        raise ValueError("Existing hooks must be an object")
    command = hook_command(engine)
    owned = {command, previous_command} - {None}
    for event, groups in list(hooks.items()):
        if not isinstance(groups, list):
            raise ValueError("Existing hook event must be an array")
        kept = []
        for group in groups:
            if not isinstance(group, dict) or not isinstance(group.get("hooks"), list):
                raise ValueError("Existing hook group is invalid")
            handlers = [h for h in group["hooks"] if not (isinstance(h, dict) and h.get("type") == "command" and h.get("command") in owned)]
            if handlers or not group["hooks"]:
                kept.append({**group, "hooks": handlers})
        if kept:
            hooks[event] = kept
        else:
            hooks.pop(event, None)
    if not remove:
        for event in CODEX_EVENTS if engine == "codex" else CLAUDE_EVENTS:
            group = {"hooks": [{"type": "command", "command": command, "timeout": 1}]}
            if event == "Notification":
                group["matcher"] = "permission_prompt|elicitation_prompt"
            hooks.setdefault(event, []).append(group)
    return config


def replace_notify(source, value):
    """Preserve TOML bytes outside a semantically verified root assignment."""
    parsed = tomllib.loads(source)
    assignment = "notify = " + json.dumps(value, ensure_ascii=False) + "\n"
    if "notify" not in parsed:
        candidate = assignment + source
    else:
        expected = {k: v for k, v in parsed.items() if k != "notify"}
        lines = source.splitlines(keepends=True)
        candidate = None
        for start, line in enumerate(lines):
            if not re.match(r"^\s*(notify|\"notify\"|'notify')\s*=", line):
                continue
            for end in range(start + 1, min(len(lines), start + 64) + 1):
                remainder = "".join(lines[:start] + lines[end:])
                try:
                    if tomllib.loads(remainder) == expected:
                        candidate = assignment + remainder
                        break
                except tomllib.TOMLDecodeError:
                    pass
            if candidate is not None:
                break
        if candidate is None:
            raise ValueError("Cannot preserve this notify assignment automatically; its layout is unsupported")
    if tomllib.loads(candidate) != {**parsed, "notify": value}:
        raise ValueError("Notify update changed unrelated configuration")
    return candidate


def setup(engines, remove=False):
    state = state_root()
    manifest_path = state / "setup.json"
    manifest = read_json(manifest_path)
    changes = []
    for engine in engines:
        if engine in ("codex", "claude"):
            path = codex_root() / "hooks.json" if engine == "codex" else claude_root() / "settings.json"
            before = path.read_bytes() if path.exists() else b""
            merged = merge_hooks(read_json(path), engine, remove, manifest.get(engine, {}).get("command"))
            after = json_bytes(merged)
            changes.append((path, before, after))
            manifest[engine] = {"command": hook_command(engine), "path": str(path), "enabled": not remove}
            if engine == "codex":
                toml_path = codex_root() / "config.toml"
                toml_before = toml_path.read_bytes() if toml_path.exists() else b""
                source = toml_before.decode()
                values = tomllib.loads(source)
                ours = [sys.executable, str(HERE / "chonk-agents.py"), "notify-codex"]
                existing = values.get("notify")
                prior = manifest.get("notify", {})
                if remove:
                    if existing == prior.get("command"):
                        restored = prior.get("previous")
                        if restored is None:
                            rewritten = replace_notify(source, [])
                            # replace_notify puts the exact assignment first.
                            rewritten = rewritten.split("\n", 1)[1]
                        else:
                            rewritten = replace_notify(source, restored)
                        changes.append((toml_path, toml_before, rewritten.encode()))
                    manifest.pop("notify", None)
                else:
                    if existing != prior.get("command"):
                        if is_managed_notifier(existing):
                            raise ValueError("Managed Codex notify exists without its setup record; restore setup.json before reinstalling")
                        if existing is not None and (not isinstance(existing, list) or not all(isinstance(x, str) for x in existing)):
                            raise ValueError("Unsupported existing Codex notify command")
                        prior = {"previous": existing}
                    rewritten = replace_notify(source, ours)
                    changes.append((toml_path, toml_before, rewritten.encode()))
                    manifest["notify"] = {**prior, "command": ours}
        elif engine == "opencode":
            # Only the TUI observer is installed by default. Installing a second
            # server observer for the same CLI would duplicate attachment state.
            directory = config_root() / "opencode"
            if (directory / "tui.jsonc").exists():
                raise ValueError("OpenCode tui.jsonc exists; JSONC setup support is required before updating it")
            path = directory / "tui.json"
            before = path.read_bytes() if path.exists() else b""
            config = read_json(path)
            plugins = config.get("plugin", [])
            if not isinstance(plugins, list):
                raise ValueError("OpenCode TUI plugin configuration must be an array")
            loader = directory / "chonk-agents-tui.mjs"
            url = loader.as_uri()
            prior_url = manifest.get("opencode", {}).get("url")
            config["plugin"] = [entry for entry in plugins if entry not in (url, prior_url)]
            if not remove:
                config["plugin"].append(url)
                source = (f"import {{ createTuiPlugin }} from {json.dumps((HERE / 'adapters/opencode-tui.mjs').as_uri())};\n"
                    f"export default {{id:'chonk-agents',tui:createTuiPlugin({{bridgeCommand:{json.dumps([sys.executable, str(HERE / 'chonk-agents.py'), 'stream', 'opencode'])}}})}};\n")
                loader_before = loader.read_bytes() if loader.exists() else b""
                if loader_before and manifest.get("opencode", {}).get("loader") != loader_before.decode():
                    raise ValueError("The OpenCode adapter loader was edited; preserve it before setup")
                changes.append((loader, loader_before, source.encode()))
                manifest["opencode"] = {"url": url, "loader": source, "enabled": True}
            else:
                manifest.setdefault("opencode", {})["enabled"] = False
            changes.append((path, before, json_bytes(config)))
        else:
            raise ValueError("Supported setup adapters: codex, claude, opencode")
    # Prepare every result before touching any provider configuration.
    if not remove and not helper_path().is_file():
        raise ValueError("Build the native hook helper before running setup")
    state.mkdir(parents=True, exist_ok=True, mode=0o700)
    info = state.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid():
        raise ValueError("Agent metadata directory must be owned by you")
    # This dedicated directory may predate schema 2 or have been created as a
    # parent of the backups directory. Its session metadata stays private.
    state.chmod(0o700)
    backup = state / "backups" / time.strftime("%Y%m%dT%H%M%S")
    committed = []
    existed = {path: path.exists() for path, _, _ in changes}
    try:
        for index, (path, before, after) in enumerate(changes):
            if before == after:
                continue
            write_atomic(backup / f"{index}-{path.name}", before)
            write_atomic(path, after, before)
            committed.append((path, before, after))
        write_atomic(manifest_path, json_bytes(manifest))
    except BaseException:
        for path, before, after in reversed(committed):
            if path.read_bytes() == after:
                if existed[path]:
                    write_atomic(path, before, after)
                else:
                    path.unlink()
        raise
    return {"configured": list(engines), "enabled": not remove, "backup": str(backup),
        "next": "Start or resume sessions to load adapters. In Codex, review the new definitions with /hooks if prompted."}


def is_managed_notifier(command):
    return bool(isinstance(command, list) and len(command) >= 3 and all(isinstance(x, str) for x in command)
        and Path(command[1]).name == "chonk-agents.py" and command[2] == "notify-codex")


def notify_codex(payload):
    # A completion notifier preserves the user's prior notifier, if any.
    try:
        subprocess.run([str(helper_path()), "codex-notify", payload], timeout=.2,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        pass
    try:
        previous = read_json(state_root() / "setup.json").get("notify", {}).get("previous")
        if isinstance(previous, list) and previous and all(isinstance(x, str) for x in previous) and not is_managed_notifier(previous):
            subprocess.Popen([*previous, payload], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        pass


def terminal_metadata(pid):
    out = {}
    try:
        tty = os.readlink(f"/proc/{pid}/fd/0")
        if tty.startswith(("/dev/pts/", "/dev/tty")):
            out["tty"] = tty
    except OSError:
        pass
    tmux, pane = os.environ.get("TMUX", ""), os.environ.get("TMUX_PANE", "")
    parts = tmux.rsplit(",", 2)
    if len(parts) == 3 and parts[0].startswith("/") and re.fullmatch(r"%[0-9]+", pane):
        out.update(tmux_socket=parts[0], tmux_pane=pane)
    return out


def send_event(event):
    runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    if not runtime:
        return False
    path = Path(runtime) / "chonk-agents/events.sock"
    with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as peer:
        peer.settimeout(.08)
        try:
            peer.connect(str(path))
            payload = json.dumps({"op": "session", "event": event, "reply": True}, ensure_ascii=False).encode()
            if len(payload) > LIMIT:
                return False
            peer.sendall(payload)
            reply = peer.recv(LIMIT)
            return bool(reply and "error" not in json.loads(reply))
        except (OSError, ValueError):
            return False


def stream(engine, stream_file=None, owner_pid=None):
    """One bounded bridge per plugin. Cache semantic snapshots, never transcripts."""
    stream_file = stream_file or sys.stdin.buffer
    owner = process_identity(owner_pid or os.getppid())
    if not owner:
        return 0
    terminal = terminal_metadata(owner["pid"])
    pending, latest, retiring = {}, {}, {}
    buffer = bytearray()
    dropping = False
    retry_at = time.monotonic()
    delay = .2
    monitor = None

    def watch_broker():
        peer = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        peer.settimeout(.08)
        try:
            peer.connect(str(Path(os.environ["XDG_RUNTIME_DIR"]) / "chonk-agents/events.sock"))
            peer.sendall(b'{"op":"watch"}')
            if not json.loads(peer.recv(LIMIT)).get("ok"):
                raise ValueError("broker watch refused")
            peer.setblocking(False)
            return peer
        except (OSError, ValueError, KeyError):
            peer.close()
            return None

    with selectors.DefaultSelector() as selector:
        selector.register(stream_file, selectors.EVENT_READ)
        while True:
            timeout = max(0, retry_at - time.monotonic()) if retry_at is not None else None
            ready = selector.select(timeout)
            if monitor is not None and any(key.fileobj is monitor for key, _ in ready):
                try:
                    raw = monitor.recv(LIMIT)
                except OSError:
                    raw = b""
                if not raw:
                    selector.unregister(monitor)
                    monitor.close();monitor = None
                    pending = dict(latest)
                    retry_at = time.monotonic()
            if any(key.fileobj is stream_file for key, _ in ready):
                data = os.read(stream_file.fileno(), 16384)
                if not data:
                    break
                buffer.extend(data)
                while b"\n" in buffer:
                    line, _, tail = buffer.partition(b"\n")
                    buffer = bytearray(tail)
                    if dropping:
                        dropping = False
                        continue
                    if len(line) > LIMIT:
                        continue
                    try:
                        raw = json.loads(line)
                        if not isinstance(raw, dict):
                            continue
                        if (engine == "opencode" and raw.get("origin") != "plugin") or (engine == "t3" and raw.get("origin") != "t3"):
                            continue
                        event = normalize({**raw, "observed_ns": time.monotonic_ns(),
                            "pid": owner["pid"], "start_time": owner["start_time"], "terminal": terminal})
                        key = (("t3", event["environment"], event["session_id"]) if engine == "t3" else
                               (event["engine"], event["session_id"], event["agent_id"]))
                        if key not in latest and len(latest) >= MAX_SESSIONS:
                            oldest = next((k for k, v in latest.items() if v.get("kind") in ("session.end", "session.disconnect")
                                or v.get("phase") in ("ended", "idle", "unknown")), None)
                            if oldest is None or len(retiring) >= MAX_SESSIONS:
                                continue
                            retired = latest.pop(oldest)
                            retiring[oldest] = {**retired, "kind": "session.disconnect", "observed_ns": time.monotonic_ns(),
                                "sequence": retired["sequence"] + 1, "source_health": "disconnected"}
                            pending.pop(oldest, None)
                        latest[key] = event
                        pending[key] = event
                        retry_at = retry_at or time.monotonic()
                    except (ValueError, TypeError, RecursionError):
                        continue
                if len(buffer) > LIMIT:
                    buffer.clear();dropping = True
            if retry_at is not None and retry_at <= time.monotonic():
                if monitor is None:
                    monitor = watch_broker()
                    if monitor is None:
                        retry_at = time.monotonic() + delay
                        delay = min(5, delay * 2)
                        continue
                    selector.register(monitor, selectors.EVENT_READ)
                # Explicit native retirements share the coalesced pending map.
                # A reselected ID can cancel one; flush every remaining retire-
                # ment before replacements so full brokers have capacity.
                ordered = sorted(pending.items(), key=lambda item: item[1]["kind"] not in ("session.end", "session.disconnect"))
                deliveries = [(True, key, event) for key, event in retiring.items()] + [(False, key, event) for key, event in ordered]
                for retirement, key, event in deliveries:
                    if not send_event(event):
                        # A replacement broker needs every retained session.
                        pending = dict(latest)
                        retry_at = time.monotonic() + delay
                        delay = min(5, delay * 2)
                        break
                    (retiring if retirement else pending).pop(key, None)
                else:
                    retry_at = None
                    delay = .2
        if monitor is not None:
            monitor.close()
    for event in latest.values():
        send_event({**event, "kind": "session.disconnect", "observed_ns": time.monotonic_ns(),
                    "sequence": event["sequence"] + 1, "source_health": "disconnected"})
    return 0


def t3_config():
    config = read_json(config_root() / "chonkstep/agents.json").get("t3", {})
    if not isinstance(config, dict):
        raise ValueError("T3 configuration must be an object")
    return config


def validate_url(url):
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https") or not parts.hostname or parts.username or parts.password or parts.query or parts.fragment:
        raise ValueError("T3 URL must be an HTTP(S) origin without credentials, query, or fragment")
    if parts.path not in ("", "/"):
        raise ValueError("T3 URL must be its server origin")
    if parts.scheme == "http" and parts.hostname not in ("localhost", "127.0.0.1", "::1"):
        raise ValueError("Remote T3 connections require HTTPS")
    return urlunsplit((parts.scheme, parts.netloc, "", "", ""))


def connect_t3(url, token_file):
    origin = validate_url(url)
    token = Path(token_file).expanduser().absolute()
    info = token.lstat()
    if token.is_symlink() or not token.is_file() or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError("T3 token file must be private and owned by you")
    node = shutil.which("node")
    if not node:
        raise ValueError("Node.js with built-in WebSocket support is required for T3")
    found = subprocess.run([node, "-p", "process.execPath"], capture_output=True, text=True, timeout=3, check=True)
    node = found.stdout.strip()
    if not os.path.isabs(node) or len(node) > 1024 or not Path(node).is_file():
        raise ValueError("Could not resolve the installed Node.js runtime")
    path = config_root() / "chonkstep/agents.json"
    before = path.read_bytes() if path.exists() else b""
    config = read_json(path)
    config["t3"] = {"url": origin, "token_file": str(token), "node": node, "enabled": True}
    write_atomic(path, json_bytes(config), before)
    try:
        broker_request({"op": "reload-adapters"})
        following = "The broker started the T3 adapter; check chonk-agents doctor."
    except (OSError, ValueError):
        following = "The adapter will start with the broker; chonk-agents t3 connects it now."
    return {"configured": "t3", "url": origin, "next": following}


def run_t3():
    config = t3_config()
    if not config.get("enabled"):
        raise ValueError("Connect T3 first with chonk-agents connect t3 --url URL --token-file PATH")
    names = ("PATH", "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TZ", "XDG_RUNTIME_DIR",
             "XDG_CONFIG_HOME", "XDG_STATE_HOME", "SSL_CERT_FILE", "NODE_EXTRA_CA_CERTS")
    environment = {name: os.environ[name] for name in names if name in os.environ}
    environment.update(CHONK_T3_URL=validate_url(config["url"]), CHONK_T3_TOKEN_FILE=config["token_file"])
    child = subprocess.Popen([config["node"], str(HERE / "adapters/t3.mjs")], stdout=subprocess.PIPE, env=environment)
    previous = signal.getsignal(signal.SIGTERM)
    def terminate(_signum, _frame):
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, terminate)
    try:
        return stream("t3", child.stdout, child.pid)
    except KeyboardInterrupt:
        return 0
    finally:
        signal.signal(signal.SIGTERM, previous)
        child.terminate()
        try:
            child.wait(timeout=2)
        except subprocess.TimeoutExpired:
            child.kill();child.wait()
        child.stdout.close()


def open_t3_thread(row, *, cancelled=None):
    config = t3_config()
    if not config.get("enabled") or not row.get("capabilities", {}).get("open_browser"):
        raise ValueError("This T3 connection does not currently support opening its thread")
    origin = validate_url(config["url"])
    environment, thread = row.get("environment"), row.get("thread_id")
    if not isinstance(environment, str) or not environment or not isinstance(thread, str) or not thread:
        raise ValueError("T3 has not supplied a verified thread destination")
    url = origin + "/" + quote(environment, safe="") + "/" + quote(thread, safe="")
    if cancelled is not None and cancelled():
        return
    subprocess.Popen(["xdg-open", url], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def broker_request(value):
    path = Path(os.environ.get("XDG_RUNTIME_DIR", "/nonexistent")) / "chonk-agents/events.sock"
    with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as peer:
        peer.settimeout(.4)
        peer.connect(str(path))
        peer.sendall(json.dumps(value).encode())
        raw, _, flags, _ = peer.recvmsg(LIMIT)
        if flags & socket.MSG_TRUNC:
            raise ValueError("Broker response exceeds the protocol limit")
        return json.loads(raw)


def doctor():
    manifest = read_json(state_root() / "setup.json")
    try:
        snapshot = broker_request({"op": "snapshot"})
        rows = snapshot.get("sessions", [])
        running = True
    except (OSError, ValueError):
        rows, running, snapshot = [], False, {}
    try:
        t3_status = {"configured": bool(t3_config().get("enabled"))}
    except (OSError, ValueError, RecursionError):
        t3_status = {"configured": False, "error": "Invalid T3 configuration; check ~/.config/chonkstep/agents.json"}
    result = {"native_helper": os.access(helper_path(), os.X_OK), "broker_running": running,
        "persistence": snapshot.get("persistence", "unavailable"),
        "adapters": {}, "t3": {**t3_status,
            "connected_threads": sum(row.get("client") == "t3" and row.get("health") == "connected" for row in rows)}}
    for engine in ("codex", "claude", "opencode"):
        entry = manifest.get(engine, {})
        configured = False
        if engine == "opencode":
            configured = entry.get("url") in read_json(config_root() / "opencode/tui.json").get("plugin", []) if entry.get("url") else False
        elif entry.get("path"):
            config = read_json(Path(entry["path"]))
            events = CODEX_EVENTS if engine == "codex" else CLAUDE_EVENTS
            configured = all(any(any(h.get("command") == entry.get("command") for h in group.get("hooks", []) if isinstance(h, dict))
                for group in config.get("hooks", {}).get(event, []) if isinstance(group, dict))
                for event in events)
        seen = [r for r in rows if r.get("engine") == engine and r.get("health") == "connected" and r.get("client") != "t3"]
        result["adapters"][engine] = {"configured": configured and bool(entry.get("enabled")),
            "executable_available": bool(shutil.which(engine)), "connected_sessions": len(seen),
            "status": "Reporting sessions" if seen else "Awaiting a connected session"}
    result["adapters"]["codex"]["hook_trust"] = "Review definitions with /hooks if Codex reports untrusted hooks"
    result["adapters"]["claude"]["limitations"] = "Stop is tentative; streaming interrupts outside tools have no native hook"
    return result
