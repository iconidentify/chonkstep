"""Versioned, bounded session metadata. No provider payloads or model calls."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import threading
import time

ENGINES = ("codex", "claude", "opencode", "grok", "cursor", "antigravity", "unknown")
KINDS = frozenset(("session.start", "session.end", "session.snapshot", "session.disconnect",
    "turn.start", "turn.stopped", "turn.complete", "turn.error", "turn.interrupted",
    "tool.start", "tool.finish", "approval.requested", "approval.pending", "approval.resolved",
    "subagent.start", "subagent.end"))
MAX_SESSIONS = 32
MAX_PACKET = 65536


def text(value, limit=160):
    if not isinstance(value, str):
        raise ValueError("session text must be a string")
    return "".join(c for c in value if c.isprintable()).encode()[:limit].decode("utf-8", "ignore")


def process_identity(pid):
    """Linux start ticks disambiguate reused PIDs; no cmdlines or environments."""
    try:
        if type(pid) is not int or pid <= 1:
            return None
        path = Path(f"/proc/{pid}")
        if path.stat().st_uid != os.getuid():
            return None
        raw = (path / "stat").read_text()
        fields = raw[raw.rfind(")") + 2:].split()
        if fields[0] in ("Z", "X"):
            return None
        return {"pid": pid, "start_time": int(fields[19]), "ppid": int(fields[1])}
    except (OSError, ValueError, IndexError):
        return None


def live(row):
    current = process_identity(row.get("pid"))
    return bool(current and current["start_time"] == row.get("start_time"))


def normalize(event):
    if not isinstance(event, dict) or event.get("schema") != 2:
        raise ValueError("session schema 2 required")
    engine, kind = event.get("engine"), event.get("kind")
    if engine not in ENGINES or not isinstance(kind, str) or kind not in KINDS:
        raise ValueError("unsupported engine or session event")
    session = text(event.get("session_id", ""), 160)
    if not session:
        raise ValueError("native session id required")
    cwd = event.get("cwd", "")
    if not isinstance(cwd, str) or not os.path.isabs(cwd) or "\0" in cwd or len(os.fsencode(cwd)) > 1024:
        raise ValueError("absolute bounded workspace path required")
    client = event.get("client", "terminal")
    origin = event.get("origin", "hook")
    if client not in ("terminal", "t3", "desktop", "ide") or origin not in ("hook", "plugin", "t3"):
        raise ValueError("unsupported session source")
    stamp = event.get("observed_ns")
    if type(stamp) is not int or not 0 < stamp <= time.monotonic_ns() + 5_000_000_000:
        raise ValueError("invalid monotonic observation time")
    pid, start = event.get("pid", 0), event.get("start_time", 0)
    if type(pid) is not int or type(start) is not int or pid < 0 or start < 0:
        raise ValueError("invalid process identity")
    out = {"schema": 2, "engine": engine, "kind": kind, "client": client, "origin": origin,
        "session_id": session, "cwd": os.path.normpath(cwd), "observed_ns": stamp,
        "pid": pid, "start_time": start}
    for key, limit in (("agent_id", 160), ("parent_id", 160), ("turn_id", 160),
            ("native_session_id", 160), ("environment", 160), ("request_id", 160),
            ("event_id", 160), ("source", 40), ("source_event", 64),
            ("incarnation", 128), ("title", 100), ("detail", 240)):
        out[key] = text(event.get(key, ""), limit)
    if not out["parent_id"] and "parent_session_id" in event:
        out["parent_id"] = text(event["parent_session_id"], 160)
    sequence = event.get("sequence", 0)
    if type(sequence) is not int or not 0 <= sequence < 2**53:
        raise ValueError("invalid source sequence")
    out["sequence"] = sequence
    health = event.get("source_health", "connected")
    if health not in ("connected", "disconnected", "degraded", "unknown"):
        raise ValueError("invalid source health")
    out["source_health"] = health
    outcome = event.get("turn_outcome", "unknown")
    if outcome not in ("unknown", "completed", "interrupted", "error"):
        raise ValueError("invalid turn outcome")
    out["turn_outcome"] = outcome
    caps = event.get("capabilities", {"observe": True, "open_browser": False})
    if not isinstance(caps, dict) or any(type(v) is not bool for k, v in caps.items() if k in ("observe", "open_browser", "open_session")):
        raise ValueError("invalid adapter capabilities")
    out["capabilities"] = {k: caps.get(k, False) for k in ("observe", "open_browser", "open_session")}
    if origin != "t3":
        out["capabilities"]["open_browser"] = False
    out["tentative"] = event.get("tentative") is True
    out["navigation"] = {}
    navigation = event.get("navigation", {})
    if origin == "plugin" and engine == "opencode" and isinstance(navigation, dict) and navigation.get("kind") == "opencode":
        out["navigation"] = {"kind": "opencode", "socket": text(navigation.get("socket", ""), 107),
            "incarnation": text(navigation.get("incarnation", ""), 128)}
    if not out["navigation"]:
        out["capabilities"]["open_session"] = False
    terminal = event.get("terminal", {})
    if not isinstance(terminal, dict):
        raise ValueError("invalid terminal metadata")
    out["terminal"] = {key: text(terminal[key], 512) for key in ("tty", "tmux_socket", "tmux_pane") if key in terminal}
    # State snapshots are only used by the structured OpenCode/T3 adapters.
    if kind == "session.snapshot":
        phase = event.get("phase")
        if origin == "hook" or phase not in ("idle", "working", "waiting", "error", "ended", "unknown"):
            raise ValueError("invalid authoritative snapshot")
        out["phase"] = phase
    if origin == "t3":
        # The navigation adapter validates this reference against its configured
        # server. Never accept a URL or executable from a session payload.
        out["thread_id"] = text(event.get("thread_id", session), 160)
    return out


def identity(event):
    native = event["native_session_id"] or event["session_id"]
    # T3 with a proven provider ID and native hooks use the same key. An
    # unassociated T3 thread stays distinct; directories are never identities.
    namespace = "t3:" + event["environment"] if event["origin"] == "t3" and not event["native_session_id"] else "local"
    raw = "\0".join((namespace, "thread" if namespace.startswith("t3:") else event["engine"], native, event["agent_id"]))
    return hashlib.sha256(raw.encode()).hexdigest()[:32]


class Persistence:
    """One coalescing worker; disk I/O never runs on the broker event loop."""
    def __init__(self, path):
        self.path = path
        self.pending = None
        self.closed = False
        self.error = None
        self.condition = threading.Condition()
        self.worker = threading.Thread(target=self._run, daemon=True)
        self.worker.start()

    def put(self, rows):
        payload = json.dumps({"schema": 2, "rows": rows}, ensure_ascii=False).encode()
        if len(payload) > 262144:
            raise ValueError("session metadata exceeds persistence bound")
        with self.condition:
            self.pending = payload
            self.condition.notify()

    def close(self):
        with self.condition:
            self.closed = True
            self.condition.notify()
        self.worker.join(2)

    def _run(self):
        while True:
            with self.condition:
                self.condition.wait_for(lambda: self.pending is not None or self.closed)
                payload, self.pending = self.pending, None
                if payload is None and self.closed:
                    return
            try:
                parent = self.path.parent
                parent.mkdir(mode=0o700, parents=True, exist_ok=True)
                st = parent.lstat()
                if parent.is_symlink() or st.st_uid != os.getuid() or st.st_mode & 0o077:
                    raise OSError("session state directory is not private")
                fd, temporary_name = tempfile.mkstemp(prefix=self.path.name + ".", suffix=".tmp", dir=parent)
                temporary = Path(temporary_name)
                with os.fdopen(fd, "wb") as stream:
                    stream.write(payload)
                os.replace(temporary, self.path)
                self.error = None
            except OSError as error:
                self.error = str(error)


class RuntimeStore:
    def __init__(self, path=None):
        self.rows = {}
        self.events = {}  # bounded last native event ids, per runtime
        self.retired = {}
        self.retired_turns = {}
        self.writer = None
        self.restore_error = None
        if path is not None:
            self.restore(Path(path))
            self.writer = Persistence(Path(path))

    def restore(self, path):
        try:
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, "rb") as stream:
                info = os.fstat(stream.fileno())
                if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
                    raise ValueError("session state owner mismatch")
                raw = stream.read(262145)
            if len(raw) > 262144:
                raise ValueError("oversized saved session metadata")
            payload = json.loads(raw)
            rows = payload.get("rows")
            if payload.get("schema") != 2 or not isinstance(rows, list) or len(rows) > MAX_SESSIONS:
                raise ValueError("invalid saved session metadata")
            restored = {}
            for saved in rows:
                # Re-validate only the whitelisted original metadata. Never
                # trust saved actions, health, or activity after a restart.
                event = normalize({**saved["event"], "observed_ns": time.monotonic_ns()})
                row = self._new(event)
                row["hook_attached"] = saved.get("hook_attached") is True and event["origin"] == "hook"
                row.update(status="unknown", health="disconnected", confidence="unknown",
                    detail="Restored session · waiting for a fresh provider event",
                    updated=int(time.time()), last_ns=0, sequence=-1)
                restored[row["id"]] = row
            self.rows = restored
        except FileNotFoundError:
            pass
        except (OSError, ValueError, KeyError, TypeError, AttributeError, RecursionError) as error:
            self.restore_error = str(error)

    def _new(self, event):
        return {"id": identity(event), "schema": 2, "provider": event["engine"],
            "engine": event["engine"], "client": event["client"], "origin": event["origin"],
            "session_id": event["session_id"], "agent_id": event["agent_id"],
            "parent_id": event["parent_id"], "turn_id": event["turn_id"],
            "turn_outcome": "unknown",
            "hook_attached": False,
            "title": event["title"] or Path(event["cwd"]).name or "Agent session",
            "cwd": event["cwd"], "detail": "Session observed", "status": "unknown",
            "health": "unknown", "confidence": "observed", "updated": int(time.time()),
            "incarnation": event["incarnation"], "sequence": -1, "source_health": event["source_health"],
            "capabilities": event["capabilities"],
            "navigation": event["navigation"],
            "pid": event["pid"], "start_time": event["start_time"],
            "terminal": event["terminal"], "last_ns": 0, "event": event}

    def public_rows(self):
        hidden = {"last_ns", "event", "sequence", "hook_attached"}
        return [{key: value for key, value in row.items() if key not in hidden} for row in self.rows.values()]

    def persist(self):
        if self.writer:
            self.writer.put(list(self.rows.values()))

    def update(self, raw, validate=lambda rows: None):
        event = normalize(raw)
        key = identity(event)
        previous = self.rows.get(key)
        notify = event["engine"] == "codex" and event["source_event"] == "agent-turn-complete"
        # Codex also sends legacy notify for hidden, ephemeral title-generation
        # threads with hooks disabled. Completion alone cannot attach a session.
        if notify and (not previous or not previous.get("hook_attached") or
                (previous["pid"], previous["start_time"]) != (event["pid"], event["start_time"])):
            return False
        if notify and previous["turn_id"] and previous["turn_id"] != event["turn_id"]:
            return False
        if previous:
            reconciling = (previous["health"] == "disconnected" and event["kind"] == "session.snapshot"
                and event["incarnation"] == previous["incarnation"] and event["sequence"] == previous["sequence"])
            retired_turns = self.retired_turns.get(key, [])
            if event["turn_id"] and event["turn_id"] in retired_turns:
                return False
            same_owner = (previous["pid"], previous["start_time"]) == (event["pid"], event["start_time"])
            if same_owner and previous["status"] == "ended" and previous["confidence"] != "tentative" and event["kind"] not in ("session.start", "session.snapshot", "session.end"):
                return False
            if (same_owner and event["turn_id"] and previous["turn_id"] and event["turn_id"] != previous["turn_id"]
                    and previous["status"] in ("working", "waiting") and event["kind"] not in ("turn.start", "session.start", "session.snapshot")):
                return False
            if event["incarnation"] and event["incarnation"] in self.retired.get(key, []):
                return False
            if event["incarnation"] and event["incarnation"] == previous["incarnation"] and event["sequence"] <= previous["sequence"] and not reconciling:
                return False
            if event["observed_ns"] <= previous["last_ns"]:
                return False
            # Authoritative T3 state owns a proven association while connected.
            # A hook can refresh liveness metadata, but cannot override it.
            if previous["origin"] == "t3" and previous["health"] == "connected" and event["origin"] == "hook":
                return False
            same_runtime = (previous["pid"], previous["start_time"]) == (event["pid"], event["start_time"])
            if not same_runtime and live(previous) and not live(event):
                return False
            event_key = (key, event["pid"], event["start_time"])
            recent = self.events.get(event_key, [])
            if event["event_id"] and event["event_id"] in recent and not reconciling:
                return False
            row = dict(previous) if same_runtime else self._new(event)
        else:
            row = self._new(event)
            event_key, recent = (key, event["pid"], event["start_time"]), []
        if event["kind"] not in ("session.end", "subagent.end") and event["pid"] and not live(event):
            return False  # delayed event from an already exited/reused process
        row.update(pid=event["pid"], start_time=event["start_time"], terminal=event["terminal"],
            engine=event["engine"], provider=event["engine"], navigation=event["navigation"],
            client=event["client"], origin=event["origin"], cwd=event["cwd"],
            last_ns=event["observed_ns"], event=event,
            incarnation=event["incarnation"], sequence=event["sequence"], source_health=event["source_health"],
            capabilities=event["capabilities"],
            health="connected" if live(event) else "unknown")
        if event["origin"] == "hook" and not notify:
            row["hook_attached"] = True
        if event["kind"] == "turn.start":
            row["turn_id"] = event["turn_id"]
            row["turn_outcome"] = "unknown"
        elif event["turn_id"]:
            row["turn_id"] = event["turn_id"]
        if event["title"]:
            row["title"] = event["title"]
        if event["origin"] == "t3":
            row["thread_id"], row["environment"] = event["thread_id"], event["environment"]
        kind = event["kind"]
        row["confidence"] = "observed" if event["origin"] == "hook" else "confirmed"
        if kind == "session.start":
            if event["source"] not in ("compact", "clear") or row["status"] == "unknown":
                row.update(status="idle", detail="Session ready")
        elif kind in ("turn.start", "tool.start", "tool.finish"):
            row.update(status="working", detail=event["detail"] or "Working")
        elif kind == "approval.requested":
            row.update(status="waiting", confidence="observed", detail="Approval requested · check the session")
        elif kind == "approval.pending":
            row.update(status="waiting", detail="Waiting for your response in the session")
        elif kind == "approval.resolved":
            row.update(status="working", detail="Response received")
        elif kind == "turn.stopped":
            row.update(status="idle", confidence="tentative", detail="Turn stopped · other hooks may continue it")
        elif kind == "turn.complete":
            row.update(status="idle", turn_outcome="completed", confidence="confirmed", detail="Turn complete · ready for your next prompt")
        elif kind == "turn.interrupted":
            row.update(status="idle", turn_outcome="interrupted", detail="Turn interrupted")
        elif kind == "turn.error":
            row.update(status="error", turn_outcome="error", detail=event["detail"] or "Turn reported an error")
        elif kind in ("session.end", "subagent.end"):
            row.update(status="ended", health="ended", detail="Session ended")
            if event["tentative"]:
                row.update(confidence="tentative", detail="Subagent stopped · other hooks may continue it")
        elif kind == "session.disconnect":
            row.update(status="unknown", health="disconnected", confidence="unknown", detail="Source disconnected")
        elif kind == "session.snapshot":
            row.update(status=event["phase"], turn_outcome=event["turn_outcome"], detail=event["detail"] or event["phase"].capitalize())
            if event["phase"] == "ended":
                row["health"] = "ended"
        elif kind == "subagent.start":
            row.update(status="working", detail="Subagent started")
        if event["source_health"] == "disconnected" and row["health"] != "ended":
            row.update(status="unknown", health="disconnected", confidence="unknown")
        elif event["source_health"] in ("unknown", "degraded"):
            row["confidence"] = "observed"
        # Last observation time is not a reason to repaint. Repeated tool events
        # still advance the private ordering cursor without waking the dock.
        ignored = {"last_ns", "updated", "event", "sequence", "hook_attached"}
        changed = previous is None or any(row.get(k) != previous.get(k) for k in row if k not in ignored)
        if changed:
            row["updated"] = int(time.time())
        candidate = dict(self.rows)
        if key not in candidate and len(candidate) >= MAX_SESSIONS:
            removable = next((k for k, r in candidate.items() if r["health"] in ("ended", "disconnected")), None)
            if removable is None:
                raise ValueError("32 sessions already registered; end or dismiss a session first")
            del candidate[removable]
        candidate[key] = row
        validate([{k: v for k, v in r.items() if k not in ("event", "last_ns", "sequence", "hook_attached")} for r in candidate.values()])
        self.rows = candidate
        if previous and previous["turn_id"] and previous["turn_id"] != row["turn_id"]:
            self.retired_turns[key] = (self.retired_turns.get(key, []) + [previous["turn_id"]])[-16:]
        self.retired_turns = {k: v for k, v in self.retired_turns.items() if k in candidate}
        if previous and previous["incarnation"] and previous["incarnation"] != event["incarnation"]:
            self.retired[key] = (self.retired.get(key, []) + [previous["incarnation"]])[-8:]
        self.retired = {k: v for k, v in self.retired.items() if k in candidate}
        self.events = {k: v for k, v in self.events.items() if k[0] in candidate and k[0] != key}
        if event["event_id"]:
            self.events[event_key] = (recent + [event["event_id"]])[-64:]
        if changed:
            self.persist()
        return changed

    def disconnected(self, pid, start):
        changed = False
        for row in self.rows.values():
            if (row["pid"], row["start_time"]) == (pid, start) and row["health"] != "ended":
                row.update(health="disconnected", status="unknown", confidence="unknown",
                    detail="Agent process exited · final session state unavailable", updated=int(time.time()))
                changed = True
        if changed:
            self.persist()
        return changed

    def source_disconnected(self, keys):
        changed = False
        for key in keys:
            row = self.rows.get(key)
            if row is not None and row["health"] not in ("ended", "disconnected"):
                row.update(health="disconnected", status="unknown", confidence="unknown",
                    detail="Adapter disconnected · waiting for a fresh snapshot", updated=int(time.time()))
                changed = True
        if changed:
            self.persist()
        return changed

    def dismiss(self, key):
        row = self.rows.get(key)
        if not row:
            raise ValueError("session no longer exists")
        if row["health"] == "connected" and row["status"] not in ("ended", "idle", "error"):
            raise ValueError("active sessions cannot be dismissed")
        del self.rows[key]
        self.events = {k: v for k, v in self.events.items() if k[0] != key}
        self.persist()

    def close(self):
        if self.writer:
            self.writer.close()
