"""Real local-process lifecycle/reconnect tests; no provider/model requests."""
import json
import os
from pathlib import Path
import socket
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
from sessions import process_identity


class RuntimeProcesses(unittest.TestCase):
    @unittest.skipUnless(shutil.which("node"), "Node.js required for the complete plugin bridge")
    def test_node_plugin_to_python_to_broker_reselection_at_capacity(self):
        script = '''
import { createInterface } from 'node:readline';
const { PluginBridge, OpenCodeSessions } = await import(process.env.CHONK_QA_MODULE);
const bridge = new PluginBridge({bridgeCommand:[process.env.CHONK_QA_PYTHON,process.env.CHONK_QA_MAIN,'stream','opencode']});
const sessions = new OpenCodeSessions({directory:'/tmp/project',publish:row=>bridge.publish(row)});
for await (const line of createInterface({input:process.stdin})) {
  for (const {id,status} of JSON.parse(line)) {
    sessions.selectedId=id;
    sessions.snapshot({id,directory:'/tmp/project'}, {type:status}, [], [], {attached:true});
  }
}
await bridge.close();
'''
        env = {**self.env, "CHONK_QA_MODULE": (ROOT / "adapters/opencode.mjs").as_uri(),
            "CHONK_QA_PYTHON": sys.executable, "CHONK_QA_MAIN": str(ROOT / "chonk-agents.py")}
        bridge = subprocess.Popen([shutil.which("node"), "--input-type=module", "--eval", script],
            env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(bridge)
        def send(rows):
            bridge.stdin.write(json.dumps(rows).encode() + b"\n");bridge.stdin.flush()
        send([{"id": str(n), "status": "idle"} for n in range(32)])
        self.wait_for(lambda s: len(s["sessions"]) == 32)
        send([{"id": "replacement", "status": "busy"}, {"id": "0", "status": "busy"}])
        snapshot = self.wait_for(lambda s: {r["session_id"] for r in s["sessions"] if r["status"] == "working"} == {"replacement", "0"})
        self.assertEqual(len(snapshot["sessions"]), 32)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="chonk-runtime.")
        self.env = {**os.environ, "XDG_RUNTIME_DIR": self.temp.name,
            "XDG_STATE_HOME": self.temp.name + "/state", "XDG_CONFIG_HOME": self.temp.name + "/config"}
        self.children = []
        self.broker = self.start_broker()

    def tearDown(self):
        for child in reversed(self.children):
            if child.poll() is None:
                child.terminate()
            child.communicate(timeout=3)
        self.temp.cleanup()

    def request(self, value):
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as peer:
            peer.settimeout(.5)
            peer.connect(self.temp.name + "/chonk-agents/events.sock")
            peer.sendall(json.dumps(value).encode())
            return json.loads(peer.recv(65536))

    def wait_for(self, predicate, timeout=4):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            try:
                last = self.request({"op": "snapshot"})
                if predicate(last):
                    return last
            except (OSError, ValueError):
                pass
            time.sleep(.02)
        self.fail(f"Broker did not reach expected state: {last}")

    def start_broker(self):
        child = subprocess.Popen([sys.executable, str(ROOT / "chonk-agents.py"), "serve"],
            env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(child)
        self.wait_for(lambda snap: True)
        return child

    def snapshot_event(self, sequence=1):
        return {"schema": 2, "engine": "opencode", "client": "terminal", "origin": "plugin",
            "session_id": "live-test", "kind": "session.snapshot", "phase": "working", "cwd": "/tmp/project",
            "incarnation": "fixture", "sequence": sequence, "source_health": "connected"}

    def test_quiet_plugin_replays_after_broker_restart_without_new_input(self):
        bridge = subprocess.Popen([sys.executable, str(ROOT / "chonk-agents.py"), "stream", "opencode"],
            env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(bridge)
        bridge.stdin.write(json.dumps(self.snapshot_event()).encode() + b"\n");bridge.stdin.flush()
        self.wait_for(lambda s: len(s["sessions"]) == 1 and s["sessions"][0]["status"] == "working")
        # Allow the bounded asynchronous persistence worker to finish.
        deadline = time.monotonic() + 2
        state = Path(self.env["XDG_STATE_HOME"]) / "chonkstep/agents/sessions-v2.json"
        while not state.exists() and time.monotonic() < deadline:
            time.sleep(.01)
        self.broker.terminate();self.broker.communicate(timeout=2)
        self.broker = self.start_broker()
        after = self.wait_for(lambda s: len(s["sessions"]) == 1 and s["sessions"][0]["health"] == "connected")
        self.assertEqual(after["sessions"][0]["status"], "working")
        self.assertIsNone(bridge.poll())

    def test_bridge_death_marks_source_disconnected_while_owner_stays_alive(self):
        bridge = subprocess.Popen([sys.executable, str(ROOT / "chonk-agents.py"), "stream", "opencode"],
            env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(bridge)
        bridge.stdin.write(json.dumps(self.snapshot_event()).encode() + b"\n");bridge.stdin.flush()
        self.wait_for(lambda s: len(s["sessions"]) == 1)
        bridge.kill();bridge.communicate(timeout=2)
        state = self.wait_for(lambda s: s["sessions"][0]["health"] == "disconnected")
        self.assertEqual(state["sessions"][0]["status"], "unknown")

    def test_pidfd_notices_abrupt_agent_exit(self):
        owner = subprocess.Popen(["/usr/bin/sleep", "30"])
        self.children.append(owner)
        identity = process_identity(owner.pid)
        event = {**self.snapshot_event(), "observed_ns": time.monotonic_ns(),
            "pid": identity["pid"], "start_time": identity["start_time"]}
        self.request({"op": "session", "event": event, "reply": True})
        self.wait_for(lambda s: len(s["sessions"]) == 1)
        owner.kill();owner.wait()
        self.wait_for(lambda s: s["sessions"][0]["health"] == "disconnected")

    def test_nested_malformed_packet_cannot_terminate_broker(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as peer:
            peer.settimeout(1);peer.connect(self.temp.name + "/chonk-agents/events.sock")
            peer.sendall(b"[" * 5000 + b"]" * 5000)
            self.assertIn("error", json.loads(peer.recv(65536)))
        self.assertIsNone(self.broker.poll())
        self.assertEqual(self.request({"op": "snapshot"})["sessions"], [])

    def test_stream_retires_idle_capacity_before_admitting_selected_session(self):
        bridge = subprocess.Popen([sys.executable, str(ROOT / "chonk-agents.py"), "stream", "opencode"],
            env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.children.append(bridge)
        for n in range(32):
            row = {**self.snapshot_event(n + 1), "session_id": str(n), "phase": "idle"}
            bridge.stdin.write(json.dumps(row).encode() + b"\n")
        bridge.stdin.flush()
        self.wait_for(lambda s: len(s["sessions"]) == 32)
        for n, session in ((33, "selected"), (34, "0")):
            bridge.stdin.write(json.dumps({**self.snapshot_event(n), "session_id": session}).encode() + b"\n")
        bridge.stdin.flush()
        snapshot = self.wait_for(lambda s: any(r["session_id"] == "selected" and r["status"] == "working" for r in s["sessions"])
            and any(r["session_id"] == "0" and r["status"] == "working" for r in s["sessions"]))
        self.assertEqual(len(snapshot["sessions"]), 32)


if __name__ == "__main__":
    unittest.main()
