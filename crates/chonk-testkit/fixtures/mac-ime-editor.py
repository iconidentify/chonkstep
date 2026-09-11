"""GTK text editor observed by the real input-method/Command-key regression."""
import json
import pathlib
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, GLib, Gtk

role, directory, initial = sys.argv[1:]
root = pathlib.Path(directory)
app = Gtk.Application(application_id="org.chonkstep.imeprobe." + role)


def activate(application):
    window = Gtk.ApplicationWindow(application=application, title="IME Probe " + role)
    window.set_default_size(500, 300)
    view = Gtk.TextView()
    view.set_left_margin(20)
    view.set_top_margin(20)
    window.set_child(view)
    buffer = view.get_buffer()
    buffer.set_text(initial)
    events = []

    def pressed(controller, keyval, keycode, state):
        events.append({"key": Gdk.keyval_name(keyval), "code": keycode, "modifiers": int(state)})
        return False

    controller = Gtk.EventControllerKey()
    controller.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    controller.connect("key-pressed", pressed)
    window.add_controller(controller)

    def observe():
        value = {
            "text": buffer.get_text(*buffer.get_bounds(), True),
            "selected": bool(buffer.get_selection_bounds()),
            "events": events,
        }
        temporary = root / (role + ".tmp")
        temporary.write_text(json.dumps(value))
        temporary.replace(root / (role + ".json"))
        return True

    GLib.timeout_add(50, observe)
    window.present()
    view.grab_focus()


app.connect("activate", activate)
app.run([])
