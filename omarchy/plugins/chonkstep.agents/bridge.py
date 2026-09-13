#!/usr/bin/env python3
"""Omarchy frontend for the existing local session broker.

One subscription streams snapshots into QML. Navigation runs in a short-lived
worker and revalidates both the live session and its chosen destination.
"""
import importlib.util
import json
import os
from pathlib import Path
import sys

LIMIT = 65536


def backend():
    data = Path(os.environ.get("XDG_DATA_HOME", str(Path.home() / ".local/share")))
    candidates = [data / "chonkstep/dockapps/chonk-agents/chonk-agents.py",
                  Path(__file__).resolve().parents[3] / "examples/chonk-agents/chonk-agents.py"]
    for path in candidates:
        if path.is_file():
            spec = importlib.util.spec_from_file_location("chonk_agent_broker", path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            return module
    raise ValueError("Agent session service is not installed.")


def current(agent, session_id):
    if not isinstance(session_id, str) or not 1 <= len(session_id) <= 160:
        raise ValueError("Choose a session from the list.")
    rows = agent.request({"op": "snapshot"}).get("sessions", [])
    row = next((row for row in rows if row.get("id") == session_id), None)
    if row is None or row.get("schema") != 2 or row.get("health") != "connected":
        raise ValueError("This session is no longer connected. Resume it in your agent to reconnect.")
    return row


def action(agent, message, navigation=None, open_browser=None):
    if not isinstance(message, dict) or message.get("op") not in ("open", "activate"):
        raise ValueError("Unknown session action.")
    row = current(agent, message.get("id"))
    if row.get("client") == "t3":
        if message["op"] != "open" or row.get("capabilities", {}).get("open_browser") is not True:
            raise ValueError("This thread has no connected browser destination.")
        if open_browser is None:
            from integrations import open_t3_thread
            open_browser = open_t3_thread
        open_browser(row)
        return {"ok": True, "opened": True}
    if navigation is None:
        import navigation
    if message["op"] == "open":
        choices = navigation.discover(row)
        if not choices:
            raise ValueError("No local terminal could be verified. Open this session in your agent.")
        if len(choices) > 1:
            return {"ok": True, "choices": choices, "id": row["id"]}
        token = choices[0]["token"]
    else:
        token = message.get("token")
        if not isinstance(token, str) or not 1 <= len(token) <= 8192:
            raise ValueError("Choose a destination from the list.")
    # An agent can exit or reconnect while discovery is in flight.
    row = current(agent, row["id"])
    navigation.activate(row, token)
    return {"ok": True, "opened": True}


def emit(value):
    print(json.dumps(value, ensure_ascii=True, separators=(",", ":")), flush=True)


def main():
    agent = backend()
    if sys.argv[1:] == ["stream"]:
        agent.watch(emit)
    elif sys.argv[1:] == ["action"]:
        raw = sys.stdin.buffer.readline(LIMIT + 1)
        if len(raw) > LIMIT:
            raise ValueError("Session action is too large.")
        emit(action(agent, json.loads(raw)))
    else:
        raise ValueError("Expected stream or action.")


if __name__ == "__main__":
    try:
        main()
    except BrokenPipeError:
        # The shell closed or reloaded its plugin; no reconnect is needed.
        os._exit(0)
    except (OSError, ValueError, RuntimeError) as error:
        emit({"ok": False, "error": str(error)[:400]})
        sys.exit(1)
