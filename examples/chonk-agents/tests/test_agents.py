"""Offline contract tests. Provider commands are inspected or replaced by stubs."""
import importlib.util
import json
import os
from pathlib import Path
import queue
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock

APP = Path(__file__).resolve().parents[1] / "chonk-agents.py"
spec = importlib.util.spec_from_file_location("chonk_agents", APP)
agents = importlib.util.module_from_spec(spec)
spec.loader.exec_module(agents)


def event(session="one", **fields):
    return {"id": session, "provider": "codex", "status": "working", "title": "Theme changes",
            "detail": "Checking the frame", "cwd": "/tmp/project", **fields}


class Contracts(unittest.TestCase):
    def test_events_are_bounded_deduplicated_and_namespaced_by_provider(self):
        store = agents.Store()
        self.assertTrue(store.update(event()))
        self.assertFalse(store.update(event()))
        self.assertTrue(store.update(event(provider="claude")))
        self.assertEqual(len(store.sessions), 2)
        self.assertEqual(store.revision, 2)
        for index in range(30):
            store.update(event(str(index), title="四"*1000, detail="😀"*1000))
        self.assertLessEqual(len(agents.encode(store.snapshot())), agents.LIMIT)
        for row in store.sessions.values():
            self.assertLessEqual(len(row["title"].encode()), 100)
            self.assertLessEqual(len(row["detail"].encode()), 400)
        before = store.snapshot()
        with self.assertRaises(ValueError):
            store.update(event("thirty-third"))
        self.assertEqual(store.snapshot(), before)

    def test_snapshot_overflow_is_refused_atomically(self):
        store = agents.Store()
        for index in range(32):
            before = store.snapshot()
            try:
                store.update(event(str(index), cwd="/" + "\\"*1000, title='"'*100, detail='"'*400))
            except ValueError:
                self.assertEqual(store.snapshot(), before)
                break
        else:
            self.fail("hostile JSON escaping should hit the packet limit")
        self.assertLessEqual(len(agents.encode(store.snapshot())), agents.LIMIT)

    def test_malformed_provider_events_cannot_escape_parser(self):
        values = [None, [], "text", 4, {}, {"type": []}, {"type": "item.started", "item": None},
                  {"type": "assistant", "message": None},
                  {"type": "assistant", "message": {"content": {}}},
                  {"type": "assistant", "message": {"content": [None, 4, "text"]}}]
        for provider in agents.PROVIDERS:
            for value in values:
                self.assertIsNone(agents.event_detail(provider, value), repr(value))
        self.assertEqual(agents.event_detail("codex", {"type": "permission.asked"})[0], "waiting")
        self.assertEqual(agents.event_detail("claude", {"type": "result", "is_error": True})[0], "error")

    def test_invalid_geometry_text_and_palette_are_rejected(self):
        for change in ({"cwd": None}, {"cwd": "x"*1025}, {"title": []}, {"detail": {}}, {"status": "99%"}):
            with self.assertRaises(ValueError):
                agents.normalize(event(**change))
        for palette in ([], {"accent": []}, {"accent": {"r": 256, "g": 0, "b": 0}},
                        {"accent": {"r": True, "g": 0, "b": 0}}):
            with self.assertRaises(ValueError):
                agents.normalize_palette(palette)
        store = agents.Store()
        palette = {"accent": {"r": 1, "g": 2, "b": 3}}
        self.assertTrue(store.set_palette(palette))
        self.assertFalse(store.set_palette(palette))
        self.assertEqual(store.revision, 1)

    def test_text_fitting_terminates_for_grants_smaller_than_ellipsis(self):
        for width in (-10, 0, 0.5, 1, 3, 100):
            fitted = agents.fit_label("A long workspace title", width, len)
            self.assertLessEqual(len(fitted), max(0, width))
        self.assertEqual(agents.fit_label("abc", 2, len), "a…")

    def test_review_css_uses_each_new_validated_palette(self):
        first=agents.review_css({"panel":{"r":1,"g":2,"b":3}})
        second=agents.review_css({"panel":{"r":200,"g":210,"b":220}})
        self.assertIn(b"background:#010203", first)
        self.assertIn(b"background:#c8d2dc", second)
        self.assertIn(b"IBM Plex Mono", second)
        self.assertIn(b"IBM Plex Sans", second)
        self.assertTrue(agents.bundled_font().is_file())
        self.assertTrue(agents.bundled_font("IBMPlexSans[wdth,wght].ttf").is_file())
        with self.assertRaises(ValueError):
            agents.review_css({"panel":{"r":"red;}","g":0,"b":0}})

    def test_diff_review_never_runs_repository_filter_helpers(self):
        with tempfile.TemporaryDirectory(prefix="chonk-review-tests.") as directory:
            repo=Path(directory)
            def git(*args):
                return subprocess.run(["git",*args],cwd=repo,check=True,capture_output=True)
            git("init","-q")
            (repo/"sample.txt").write_text("before\n")
            git("add","sample.txt")
            git("-c","user.name=Offline test","-c","user.email=test@example.invalid","commit","-qm","base")
            (repo/"sample.txt").write_text("after\n")
            (repo/".gitattributes").write_text("sample.txt filter=review-test\n")
            helper=repo/"helper"
            helper.write_text("#!/bin/sh\ntouch helper-ran\ncat\n")
            helper.chmod(0o700)
            git("config","filter.review-test.clean",str(helper))
            git("config","filter.review-test.process",str(helper))
            git("config","filter.review-test.required","true")
            git("config","core.fsmonitor",str(helper))
            output=agents.read_workspace_diff(directory)
            self.assertIn("-before",output)
            self.assertIn("+after",output)
            self.assertFalse((repo/"helper-ran").exists())

    def test_commands_preserve_prompt_as_operand_and_never_request_auto_approval(self):
        prompt = "--danger; $(touch /tmp/should-not-run)"
        for provider in agents.PROVIDERS:
            model = "xai/test-model" if provider == "grok" else None
            command = agents.provider_command(provider, prompt, model)
            self.assertEqual(command[-2:], ["--", prompt])
            self.assertNotIn("--dangerously-skip-permissions", command)
            self.assertNotIn("--full-auto", command)
            self.assertNotIn("--yolo", command)
        with self.assertRaises(ValueError):
            agents.provider_command("grok", "hello")
        with self.assertRaises(ValueError):
            agents.provider_command("unknown", "hello")



class BrokerProtocol(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix="chonk-agents-tests.")
        self.env = mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": self.tmp.name,
            "XDG_STATE_HOME": self.tmp.name + "/state", "XDG_CONFIG_HOME": self.tmp.name + "/config"})
        self.env.start()
        bootstrap = "import runpy,sys; ns=runpy.run_path(sys.argv[1]); ns['serve'].__globals__['REQUEST_TIMEOUT']=0.15; ns['serve']()"
        self.process = subprocess.Popen([sys.executable, "-c", bootstrap, str(APP)],
                                        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        deadline = time.monotonic()+3
        while True:
            try:
                agents.request({"op": "snapshot"})
                break
            except OSError:
                if time.monotonic() > deadline or self.process.poll() is not None:
                    self.fail("test broker did not start")
                time.sleep(0.01)

    def tearDown(self):
        self.process.terminate()
        self.process.communicate(timeout=2)
        self.env.stop()
        self.tmp.cleanup()

    def test_malformed_request_is_refused_without_killing_broker(self):
        for value in ([], None, 5, {"op": "event", "event": []},
                      {"op": "event", "event": event(cwd=None)},
                      {"op": "palette", "palette": {"accent": None}}):
            with agents.connect() as peer:
                peer.sendall(agents.encode(value))
                self.assertIn(b'"error"', peer.recv(agents.LIMIT))
            self.assertEqual(agents.request({"op": "snapshot"})["sessions"], [])

    def test_idle_request_peers_expire_and_release_connection_slots(self):
        peers = [agents.connect() for _ in range(8)]
        try:
            for peer in peers:
                peer.settimeout(1)
                self.assertEqual(peer.recv(1), b"")
            self.assertEqual(agents.request({"op": "snapshot"})["sessions"], [])
        finally:
            for peer in peers:
                peer.close()

    def test_identical_events_do_not_wake_subscribers_again(self):
        with agents.connect() as subscriber:
            subscriber.sendall(agents.encode({"op": "subscribe"}))
            subscriber.recv(agents.LIMIT)
            agents.request({"op": "event", "event": event()})
            self.assertIn(b'"working"', subscriber.recv(agents.LIMIT))
            agents.request({"op": "event", "event": event()})
            subscriber.settimeout(0.08)
            with self.assertRaises(socket.timeout):
                subscriber.recv(agents.LIMIT)

    def test_real_wrapper_processes_report_stub_provider_outcomes(self):
        stub_dir=Path(self.tmp.name)/"bin"
        stub_dir.mkdir()
        source=(f"#!{sys.executable}\nimport json,os,sys\n"
                "open(os.environ['STUB_ARGS'],'w').write(json.dumps(sys.argv[1:]))\n"
                "sys.stdout.write(os.environ['STUB_STREAM'])\n"
                "sys.exit(int(os.environ['STUB_EXIT']))\n")
        for name in ("codex","claude","opencode"):
            stub=stub_dir/name;stub.write_text(source);stub.chmod(0o700)
        cases=[("codex",0,'{"type":"item.completed","item":{"type":"agent_message","text":"Done"}}\n',0,"complete"),
               ("claude",0,'{"type":"result","is_error":true,"result":"Denied"}\n',1,"error"),
               ("opencode",7,'{"type":"tool_use","part":{"tool":"edit"}}\n',7,"error"),
               ("grok",0,'{"type":"text","part":{"text":"Done"}}\n',0,"complete")]
        prompt="test ; $(not-a-shell)"
        for provider,exitcode,record,want_exit,want_status in cases:
            stream='not json\n{"type":"assistant","message":null}\n'+record
            args_path=Path(self.tmp.name)/"arguments.json"
            env={**os.environ,"PATH":str(stub_dir),"STUB_STREAM":stream,"STUB_EXIT":str(exitcode),"STUB_ARGS":str(args_path)}
            argv=[sys.executable,str(APP),"run",provider,prompt]
            if provider=="grok":argv.extend(["--model","xai/stub"])
            result=subprocess.run(argv,env=env,capture_output=True,timeout=3)
            self.assertEqual(result.returncode,want_exit,result.stderr)
            self.assertEqual(result.stdout.decode(),stream)
            self.assertEqual(json.loads(args_path.read_text())[-2:],["--",prompt])
            row=next(row for row in agents.request({"op":"snapshot"})["sessions"] if row["provider"]==provider)
            self.assertEqual(row["status"],want_status)
        (stub_dir/"codex").unlink()
        result=subprocess.run([sys.executable,str(APP),"run","codex","missing executable"],
                              env={**os.environ,"PATH":str(stub_dir)},capture_output=True,timeout=3)
        self.assertEqual(result.returncode,127,result.stderr)
        row=next(row for row in agents.request({"op":"snapshot"})["sessions"] if row["title"]=="missing executable")
        self.assertEqual(row["status"],"error")


if __name__ == "__main__":
    unittest.main()
