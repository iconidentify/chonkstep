"""The action-table pattern: what a dockapp is allowed to execute,
frozen in one place and checkable by a test.

A dockapp is a separate process. It was started by you, it runs as you,
and it can do anything you can do — this module does **not** confine
it, and nothing in the dock does. What it buys is *auditability*: the
set of commands your instrument can run becomes one screen a reviewer
can read, and ``bindings/python/tests/test_actions.py`` is a harness
that asserts mechanically that it stayed that way. See
``docs/instrument-actions.md`` for the whole argument, and for the
worked example (``chonk-net``) this is the distilled shape of.

The pattern is four rules:

1. **A frozen table** of argv tuples, with runtime substitution points
   marked as :class:`Slot` — not a builder, not a format string.
2. **One call site.** :meth:`ActionTable.run` is the only place this
   package spawns a process, and it refuses any key not in the table.
3. **Validated slots.** A runtime value is checked before it is
   anywhere near an argv, by the same rule the compositor's own
   built-in widgets use (``chonk_dock_widget::sampling::Argv``): one
   whole operand, non-empty, no leading ``-``, no control characters,
   length-capped. A refused value produces *no command at all* rather
   than a shorter one.
4. **Guarantee tests** over the table, which is what makes 1–3 a
   property rather than a promise.

Usage::

    from chonkdock.actions import ActionTable, Slot

    ACTIONS = ActionTable({
        "switch_sink": ("pactl", "set-default-sink", Slot("sink")),
        "toggle_mute": ("pactl", "set-sink-mute", Slot("sink"), "toggle"),
    }, programs=("pactl",))

    ACTIONS.run("switch_sink", sink=row.name)

Stdlib only, like the rest of ``chonkdock``: vendor the directory next
to your script and ship it.
"""

from __future__ import annotations

import re
import subprocess
import types
from dataclasses import dataclass, field
from typing import Callable, Iterable, Mapping, Optional, Sequence

#: The longest a runtime value may be, in bytes — the same cap
#: ``Argv::MAX_VALUE`` uses, and comfortably above every identifier
#: these commands actually take (an SSID is at most 32 bytes, a UUID
#: 36, a PipeWire node name well under 100).
MAX_VALUE = 256

_CONTROL = re.compile(r"[\x00-\x1f\x7f]")


class ActionError(Exception):
    """Base for everything this module refuses to do."""


class UnknownAction(ActionError, KeyError):
    """A key that is not in the table. The table is the vocabulary."""


class RefusedValue(ActionError, ValueError):
    """A runtime value that failed validation. The command does not run
    in a shortened form; it does not run."""


@dataclass(frozen=True)
class Slot:
    """A runtime substitution point in an argv tuple.

    ``name`` is the keyword :meth:`ActionTable.run` fills it from.
    ``pattern`` optionally tightens the check beyond the baseline rule
    — an interface name is ``[A-Za-z0-9._-]{1,15}``, a UUID is a UUID,
    and a table that says so refuses more than this module can guess.
    """

    name: str
    pattern: Optional[re.Pattern] = None
    max_len: int = MAX_VALUE

    def validate(self, value: object) -> str:
        """The value as an argv word, or :class:`RefusedValue`.

        The baseline rule, and why it is exactly this list: nothing here
        goes through a shell (``subprocess.run`` takes an argv list, so
        there is no quoting and no metacharacter to escape), but the
        programs parse their own options — so a value that *looks* like
        an option is refused rather than smuggled in as one. Spaces,
        ``%``, quotes and UTF-8 are deliberately fine: sink names and
        SSIDs contain them routinely.
        """
        if not isinstance(value, str):
            raise RefusedValue(f"{self.name}: not a string: {value!r}")
        if not value:
            raise RefusedValue(f"{self.name}: empty")
        if len(value.encode("utf-8")) > self.max_len:
            raise RefusedValue(f"{self.name}: longer than {self.max_len} bytes")
        if value.startswith("-"):
            raise RefusedValue(
                f"{self.name}: starts with '-', which the program would "
                f"read as an option: {value!r}")
        if _CONTROL.search(value):
            raise RefusedValue(f"{self.name}: contains a control character")
        if self.pattern is not None and not self.pattern.match(value):
            raise RefusedValue(
                f"{self.name}: does not match {self.pattern.pattern}: {value!r}")
        return value


def _spawn(argv: Sequence[str], timeout: float) -> Optional[str]:
    """The one place this package starts a process.

    Returns stdout, or ``None`` when the tool is absent, fails, or
    times out — callers treat ``None`` as "this did not happen", which
    is also the honest thing to draw: the authority on what a command
    did is the next reading, never its exit status.
    """
    try:
        proc = subprocess.run(list(argv), capture_output=True, text=True,
                              timeout=timeout, check=False)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout


@dataclass
class ActionTable:
    """An immutable table of everything a dockapp may execute.

    ``actions`` maps a key to an argv tuple of ``str`` words and
    :class:`Slot` substitution points. ``programs``, if given, is the
    set of program names the table may use — an extra assertion in the
    constructor rather than only in a test, so a table that grows a new
    binary says so at import time.

    ``runner`` is the function that actually spawns, ``(argv, timeout)
    -> str | None``; the default is the module's single
    ``subprocess.run`` call site. Tests pass their own and touch
    nothing.
    """

    actions: Mapping[str, tuple]
    programs: Optional[Iterable[str]] = None
    runner: Callable[[Sequence[str], float], Optional[str]] = _spawn
    #: Keys actually run, oldest first — for a test that asserts an
    #: interaction ran one command and not three.
    calls: list = field(default_factory=list, init=False, repr=False)

    def __post_init__(self) -> None:
        allowed = tuple(self.programs) if self.programs is not None else None
        frozen = {}
        for key, argv in dict(self.actions).items():
            if not isinstance(key, str) or not key:
                raise ActionError(f"not an action key: {key!r}")
            argv = tuple(argv)
            if not argv:
                raise ActionError(f"{key}: empty argv")
            program = argv[0]
            if not isinstance(program, str) or not program:
                raise ActionError(f"{key}: the program must be a literal string")
            if allowed is not None and program not in allowed:
                raise ActionError(
                    f"{key}: {program!r} is not one of the declared programs "
                    f"{allowed!r}")
            for part in argv[1:]:
                if not isinstance(part, (str, Slot)):
                    raise ActionError(
                        f"{key}: argv words are literals or Slots, got {part!r}")
            frozen[key] = argv
        # MappingProxyType over tuples: immutable at runtime, so "the
        # code can only run these" is checkable by reading one screen.
        self.actions = types.MappingProxyType(frozen)

    def keys(self) -> Iterable[str]:
        return self.actions.keys()

    def slots(self, key: str) -> tuple:
        """The slot names ``key`` requires, in argv order."""
        return tuple(part.name for part in self._argv_template(key)
                     if isinstance(part, Slot))

    def argv(self, key: str, **values: str) -> list:
        """The fully substituted argv, validated — without running it.

        Useful on its own: a test can assert what an interaction *would*
        run, and a caller can log it.
        """
        template = self._argv_template(key)
        wanted = {part.name for part in template if isinstance(part, Slot)}
        extra = set(values) - wanted
        if extra:
            raise ActionError(f"{key}: no such slot(s): {sorted(extra)}")
        argv = []
        for part in template:
            if isinstance(part, Slot):
                if part.name not in values:
                    raise RefusedValue(f"{key}: missing slot {part.name!r}")
                argv.append(part.validate(values[part.name]))
            else:
                argv.append(part)
        return argv

    def run(self, key: str, timeout: float = 8.0, **values: str) -> Optional[str]:
        """Runs one action from the table. The only call site there is.

        Raises :class:`UnknownAction` for a key that is not in the
        table and :class:`RefusedValue` for a value that fails
        validation — both *before* anything is spawned, so a refused
        action is an action that did not happen.
        """
        argv = self.argv(key, **values)
        self.calls.append(key)
        return self.runner(argv, timeout)

    def _argv_template(self, key: str) -> tuple:
        try:
            return self.actions[key]
        except KeyError:
            raise UnknownAction(f"not a declared action: {key!r}") from None
