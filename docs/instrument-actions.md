# Instrument Actions

A tile that only reads is easy. The moment an instrument *does*
something — switch the default sink, bring a connection up, join a
network, toggle the radio — it has to run a command, and the question
becomes: which commands, decided by whom, and how would anyone know?

This document is the answer in two halves, because there are two kinds
of instrument and they are not the same kind of thing at all.

## Built-in widgets: the compiler is the whitelist

The instruments that ship with the compositor run *inside* it,
against the `DockWidget` SDK (`chonk-dock-widget`). They are not
allowed to touch the system: the crate they are written against cannot
perform I/O, and `chonk-instruments` carries a `clippy.toml` that makes
`std::fs::File`, `std::process::Command`, `std::fs::read_to_string`,
`std::fs::read`, `std::fs::read_dir` and `std::thread::spawn` build
errors inside it. A widget *declares* what it needs (`Source`) and
*returns* what it wants done (`Effect`); the dock owns every thread.

Read `Effect::Run`'s two fields as the sentence they are:

```rust
Run { program: &'static str, args: Vec<String>, then: Option<SourceId> }
```

**The program is compile-time; the arguments are runtime.** The set of
programs a built-in widget can run is the set of string literals
written in the source — not a policy, not a runtime check, a property
you establish by grepping. That is what "the compiler is the whitelist"
means. The *arguments* are deliberately `String`s, because a control
that could not name a sink, a UUID or an SSID would be useless; what
governs them is the rule below, not the type.

Panels did not change any of it: a panel action is the same
`Effect::Run`, on the same detached executor, with the same `then:`
resample that makes the *next sample* — not an exit status — the
authority on what happened. And every command runs under a deadline
and is killed if it overruns, because a program that hangs instead of
exiting (`bluetoothctl` with no `org.bluez` on the bus is the standing
example) would otherwise wedge a dock worker for the life of the
session.

### The rule for runtime arguments

Panels are what put pressure on this, because a panel row is usually
about something the system named a moment ago. "Switch to this sink"
needs a PipeWire node name. "Bring this up" needs an `nmcli` UUID.
"Join this" needs an SSID that was broadcast by a stranger. None of
those are in the source, and no amount of reading the source tells you
what will be in them.

The rule, verbatim from `chonk_dock_widget::sampling::Argv`:

> **The program and the argv's shape are compile-time. A runtime value
> may only be one whole operand, and only through `Argv::value` /
> `Argv::number`, which validate it. A value that fails validation
> produces no command at all.**

```rust
Argv::new("nmcli")
    .word("connection")      // compile-time: a subcommand
    .word(verb)              // compile-time: "up" or "down", from the source
    .value(uuid)             // runtime: validated, one whole operand
    .effect(Some(confirm))   // -> Option<Effect>; None if a value was refused
```

`value` refuses, and the refusal kills the whole command rather than
one word:

- **a leading `-`** — the one that matters. Nothing here goes through a
  shell (`Effect::Run` is `Command::new(program)` with an argv vector,
  so there is no quoting, no globbing, no metacharacter), but every one
  of these programs parses its own options, and an SSID named
  `--terminate` handed to `nmcli` as an operand stops being an operand.
- **control characters**, NUL and newline included.
- **empty**, and anything longer than `Argv::MAX_VALUE` (256 bytes) —
  an SSID is at most 32 bytes and a UUID 36.

Spaces, `%`, quotes and UTF-8 are explicitly *fine*: PipeWire node
names and SSIDs contain them routinely, and with no shell in the path
they are ordinary bytes. `number` takes a `u64` for the same reason
`value` refuses a leading dash — a word that starts with `-` is a flag,
and a flag is compile-time.

### Actions that are several commands

Some actions are irreducibly plural, because the tool takes one command
per invocation and offers no chaining: switching the default audio sink
is `pactl set-default-sink <name>` *and* one
`pactl move-sink-input <index> <name>` per stream already playing, or
the sound keeps coming out of the old device. A panel returns them
together — `PanelReaction::RunAll(Vec<Effect>)`, of which
`PanelReaction::Run(effect)` is the single-effect shorthand — and the
shell dispatches them **in the order the widget listed them,
sequentially, on one worker thread**.

Every command's `then:` resample fires when *that* command exits;
there is no "the last one confirms". So put the confirming resample on
the command whose completion actually proves the change — the audio
panel hangs it on `set-default-sink` and gives the per-stream
migrations none, because they alter which streams play where, not
anything the panel draws.

A refused action is an action that did not happen: the panel repaints,
the sampler reports what is actually true, and nothing half-formed
reaches a process. Widgets have no way to raise an error dialog and
should not grow one for this; the honest feedback is the next reading.

## Third-party dockapps: a convention, and what it is not

A dockapp is a separate process. It was started by you, it runs as you,
and it can do anything you can do. **The dock is not a sandbox**, and
nothing in this section confines a dockapp — it could not, and pretending
otherwise would be worse than saying so. `Source::Command` is
arbitrary-argv-by-declaration, which is exactly why the built-in SDK is
for tiles that ship with the compositor and everyone else gets a socket
and their own process: the accountability line is the process boundary,
drawn where it can actually be seen.

What the convention below buys is **auditability, not confinement**:
the set of things a dockapp can execute becomes one screen you can
read, and a test asserts mechanically that it stayed that way. That is
worth having — a reviewer, a packager, or a user with `chonk-get` can
check the claim in a minute instead of reading a thousand lines — and
it is worth being precise about what it is not.

### The convention

1. **A frozen action table.** Every command the dockapp can run, as
   immutable argv tuples, in one place, with the substitution points
   marked. Not a builder, not a format string: a table.
2. **One call site.** Exactly one function in the whole program spawns
   a process, and it refuses any key not in the table.
3. **Validated runtime slots.** A substituted value is checked against
   a pattern before it is anywhere near an argv — the same rule the
   built-in `Argv` enforces, for the same reason.
4. **Guarantee tests.** Tests that walk the table asserting the
   property — that it is immutable, that no entry carries a
   state-changing verb, that the call site refuses off-table keys and
   bad values, and that `subprocess` is spoken exactly once in the
   whole source tree. Structural assertions, so the property is checked
   rather than promised.

### The worked example

`chonk-net` — the network instrument — is the fully worked version:
a frozen `COMMANDS` table of read-only `nmcli`/`iw` invocations, a
single `run_command` call site that validates `{ifname}` against a
regex, and a `ReadOnlyGuarantee` test class that walks the table
against a list of forbidden verbs (`connect`, `disconnect`, `up`,
`down`, `radio`, `set`, …) and greps the source for stray subprocess
calls. It is read-only *by construction*: it can look at your network
all day and cannot touch it.

It is not in the tree today; recover it in full with

```
git show 4e16a31:examples/chonk-net/netdata.py        # the table + call site
git show 4e16a31:examples/chonk-net/tests/test_net.py # the guarantee tests
```

The shape, in miniature:

```python
COMMANDS = types.MappingProxyType({
    "nm_devices": ("nmcli", "-t", "-f", "DEVICE,TYPE,STATE", "device", "status"),
    "nm_ip4":     ("nmcli", "-t", "-f", "IP4.ADDRESS", "device", "show", "{ifname}"),
    "iw_link":    ("iw", "dev", "{ifname}", "link"),
})

class ReadOnlyGuarantee(unittest.TestCase):
    def test_every_command_is_a_whitelisted_reader(self):
        for key, argv in COMMANDS.items():
            self.assertIn(argv[0], ("nmcli", "iw"), key)
            for word in argv[1:]:
                self.assertNotIn(word, FORBIDDEN_WORDS,
                                 f"{key} carries a state-changing verb")
```

### The copyable template

`bindings/python/chonkdock/actions.py` is that pattern as a small
module you can vendor next to your script:

```python
from chonkdock.actions import ActionTable, Slot

ACTIONS = ActionTable({
    "switch_sink": ("pactl", "set-default-sink", Slot("sink")),
    "toggle_mute": ("pactl", "set-sink-mute", Slot("sink"), "toggle"),
})

ACTIONS.run("switch_sink", sink=row.name)   # the only call site there is
```

`Slot` values are validated exactly as `Argv::value` validates them
(non-empty, no leading `-`, no control characters, length-capped) and
refuse rather than truncate. `ActionTable` is immutable after
construction, records what it ran (`calls`), and takes a `runner` so
tests drive it without spawning anything.

`bindings/python/tests/test_actions.py` is the guarantee harness:
point `ActionTableGuarantee` at your own table by subclassing it with
`TABLE` and `PROGRAMS` set, and you inherit the structural assertions —
frozen, no state-changing verb outside your declared list, off-table
keys refused, hostile slot values refused, one call site.

None of that makes a dockapp safe to install from a stranger. It makes
a dockapp's actions *reviewable*, which is the honest thing to offer.
