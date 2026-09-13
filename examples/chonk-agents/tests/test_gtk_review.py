"""Opt-in native GTK contract: CHONK_AGENTS_GTK_TEST=1 python -m unittest discover examples/chonk-agents/tests.

Uses a private D-Bus session and headless Weston; no provider requests or live
desktop settings. GTK must display both the initial event and a subsequent
palette/status update while the same review window remains open.
"""
import contextlib
import json
import os
from pathlib import Path
import runpy
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

APP = Path(__file__).resolve().parents[1] / "chonk-agents.py"


def request(value):
    with socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET) as peer:
        peer.settimeout(2)
        peer.connect(str(Path(os.environ["XDG_RUNTIME_DIR"]) / "chonk-agents/events.sock"))
        peer.sendall(json.dumps(value).encode())
        return json.loads(peer.recv(65536))


def publish(title, status, ink):
    request({"op": "palette", "palette": {"text": dict(zip("rgb", ink)),
             "panel": {"r": 32, "g": 36, "b": 33}}})
    request({"op": "event", "event": {"id": "gtk-contract", "provider": "codex",
             "title": title, "status": status, "cwd": str(APP.parent), "detail": "Offline UI test"}})


def gtk_child():
    ns = runpy.run_path(str(APP))
    assert ns["register_review_font"]()
    import gi
    gi.require_version("Gtk", "4.0")
    from gi.repository import Gtk, GLib, Gdk
    started = time.monotonic()
    state = {"phase": 0, "error": None, "ready": None, "done": False}

    def widgets(widget):
        yield widget
        child = widget.get_first_child()
        while child:
            yield from widgets(child)
            child = child.get_next_sibling()

    def inspect():
        app = Gtk.Application.get_default()
        if time.monotonic() - started > 10:
            state["error"] = "GTK did not apply broker session and palette updates within 10 seconds"
            if app:
                app.quit()
            return False
        if not app or not app.get_windows():
            return True
        window = next((window for window in app.get_windows() if window.get_title()=="Agent Review"),None)
        if window is None:
            return True
        text = [widget.get_label() for widget in widgets(window) if isinstance(widget,Gtk.Label)]
        color = window.get_style_context().get_color()
        actual = tuple(round(channel * 255) for channel in (color.red, color.green, color.blue))
        if state["phase"] == 0 and "First event" in text and actual == (111, 123, 145):
            state["ready"] = time.monotonic() - started
            state["phase"] = 1
            threading.Thread(target=publish, args=("Updated event", "complete", (201, 212, 223)), daemon=True).start()
        elif state["phase"] == 1 and "Updated event" in text and actual == (201, 212, 223):
            assert "First event" not in text
            assert any("COMPLETE" in value for value in text)
            assert isinstance(window.get_titlebar(),Gtk.HeaderBar)
            action=next(widget for widget in widgets(window) if isinstance(widget,Gtk.Button) and widget.get_label()=="Review workspace changes  ↗")
            action.emit("clicked")
            state["phase"]=2
        elif state["phase"] == 2:
            diff=next((window for window in app.get_windows() if window.get_title()=="Workspace changes"),None)
            if diff is None:
                return True
            header=diff.get_titlebar()
            assert isinstance(header,Gtk.HeaderBar)
            close=next(widget for widget in widgets(header) if isinstance(widget,Gtk.Button) and widget.has_css_class("close"))
            close.emit("clicked")
            state["phase"]=3
        elif state["phase"] == 3 and len(app.get_windows())==1:
            controllers=window.observe_controllers()
            keys=next(controllers.get_item(index) for index in range(controllers.get_n_items()) if isinstance(controllers.get_item(index),Gtk.EventControllerKey))
            state["done"]=True
            assert keys.emit("key-pressed",Gdk.KEY_w,0,Gdk.ModifierType.CONTROL_MASK)
            return False
        return True

    GLib.timeout_add(25, inspect)
    ns["review"]()
    assert state["done"], state["error"] or "GTK review exited before updates"
    print(json.dumps({"initial_ready_seconds": state["ready"], "updated_seconds": time.monotonic() - started}), flush=True)


def native_child():
    with tempfile.TemporaryDirectory(prefix="chonk-gtk-contract.") as temp, contextlib.ExitStack() as stack:
        root = Path(temp)
        for name in ("runtime", "config", "state", "data", "cache"):
            (root / name).mkdir(mode=0o700)
            os.environ["XDG_" + ("RUNTIME_DIR" if name == "runtime" else name.upper() + "_HOME")] = str(root / name)
        os.environ.update(WAYLAND_DISPLAY="wayland-gtk", GDK_BACKEND="wayland", GTK_A11Y="none",
                          GSETTINGS_BACKEND="memory", LIBGL_ALWAYS_SOFTWARE="1")
        os.environ.pop("DISPLAY", None)

        def launch(args, name):
            log = stack.enter_context((root / name).open("w+"))
            process = subprocess.Popen(args, stdout=log, stderr=log, start_new_session=True)
            def stop():
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try:
                        process.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait(timeout=2)
            stack.callback(stop)
            return process

        weston = launch(["weston", "--backend=headless-backend.so", "--socket=wayland-gtk",
                         "--idle-time=0", "--width=800", "--height=640", "--no-config"], "weston.log")
        broker = launch([sys.executable, str(APP), "serve"], "broker.log")
        deadline = time.monotonic() + 5
        while not (root / "runtime/wayland-gtk").is_socket() or not (root / "runtime/chonk-agents/events.sock").is_socket():
            assert weston.poll() is None and broker.poll() is None, "test services exited during startup"
            assert time.monotonic() < deadline, "test services did not start"
            time.sleep(0.01)
        publish("First event", "working", (111, 123, 145))
        result = subprocess.run([sys.executable, __file__, "--gtk-child"], capture_output=True, timeout=13)
        assert result.returncode == 0, result.stderr.decode(errors="replace")
        print(result.stdout.decode(), end="")


class NativeReview(unittest.TestCase):
    @unittest.skipUnless(os.environ.get("CHONK_AGENTS_GTK_TEST") == "1", "opt-in native GTK/Weston test")
    def test_real_review_updates_sessions_and_palette(self):
        # No activation directories: the private test bus must never launch
        # user portal/keyring services from the desktop's environment.
        with tempfile.TemporaryDirectory(prefix="chonk-gtk-bus.") as directory:
            config=Path(directory)/"bus.conf"
            config.write_text('<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen>'
                '<policy context="default"><allow send_destination="*"/><allow receive_sender="*"/><allow own="*"/></policy></busconfig>')
            result = subprocess.run(["dbus-run-session", "--config-file",str(config),"--", sys.executable, __file__, "--native-child"],
                                    capture_output=True, timeout=22)
        self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
        timing = json.loads(result.stdout)
        self.assertLess(timing["updated_seconds"], 10)


if __name__ == "__main__":
    if "--gtk-child" in sys.argv:
        gtk_child()
    elif "--native-child" in sys.argv:
        native_child()
    else:
        unittest.main()
