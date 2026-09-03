# Bluetooth

The dock's BT instrument — the tile, the panel behind it, and the
`chonk-btpair` pairing dialog the panel spawns.

Three crates carry it:

| Piece | Where |
|---|---|
| Tile and panel faces (pure renderers) | `crates/wm-theme/src/bluetooth.rs` |
| The widget, the BlueZ reading, the panel brain | `crates/chonk-instruments/src/bluetooth.rs`, `src/bt_panel.rs`, `src/bt_panel/{bluez,json}.rs` |
| The pairing dialog | `crates/chonk-btpair/` |

---

## The parse surface: `busctl`, never `bluetoothctl`

This is the load-bearing decision, it was made against measurement, and
it is the opposite of the obvious one.

**`bluetoothctl` hangs forever on a machine with no adapter.** Measured
on the development host — no controller, `/sys/class/bluetooth` absent,
`bluetooth.service` inactive:

```text
bluetoothctl list                 rc=124 (killed at 6s)  no output
bluetoothctl show                 rc=124 (killed at 6s)  no output
bluetoothctl devices Connected    rc=124 (killed at 6s)  no output
bluetoothctl devices Paired       rc=124 (killed at 6s)  no output
```

It is not a parse-stability problem — there is nothing to parse. It is
a readline client waiting for `org.bluez` to appear on the bus, and
when nothing will bring it up, it waits as long as you let it.
Omarchy's own `omarchy-bluetooth-power` knows this: every call it makes
is wrapped in `timeout 2s`, without exception.

**The dock has no such wrapper.** `chonk-shell`'s `BackgroundCommand`
is a bare `Command::new(program).args(&args).output()` on a worker
thread — no timeout, no kill, no deadline. A `Source::Command` pointed
at `bluetoothctl` would wedge that worker *permanently* on its first
run: the source never produces another reading, the thread never
returns, the child is never reaped. `run_detached`, the `Effect::Run`
executor, is the same shape and leaks a thread and a process per click.

That is precisely the 2026-08-29 incident class the whole
`Source`/`Effect` architecture exists to make structurally unavailable
— except permanent instead of periodic. So **chonkstep never execs
`bluetoothctl` from the dock**, for reads or for writes.

`busctl` behaves the way a sampler must, on the same machine:

```text
busctl --system --json=short call org.bluez /org/bluez \
       org.freedesktop.DBus.ObjectManager GetManagedObjects
  → rc=1, ~0s, "Call failed: Could not activate remote peer 'org.bluez': unit failed"
```

Non-zero, immediately, and it did **not** leave `bluetoothd` running
afterwards — so sampling once a second does not conjure a daemon this
desk never asked for. `BackgroundCommand` keeps a reading only when
`status.success()`, so a failed call clears the slot, `Samples::text`
answers `None`, and the widget draws the face for "BlueZ is not
answering".

`--json=short` rather than `busctl`'s own nested-variant text format: a
machine-readable surface with a documented shape beats a pretty-printer.
The reader is `bt_panel::json` (hand-rolled — this crate may not take
dependencies) and the fold is `bt_panel::bluez`.

### Adapter presence is a sysfs question

"Does hardware exist" and "is a daemon answering" are different
questions with different honest answers. Folding them together would
let a stopped `bluetooth.service` render as *you own no Bluetooth
hardware*.

So presence comes from `/sys/class/bluetooth` as a `Source::Tree` — the
same shape, for the same reason, as the link tile's `/sys/class/net`
walk. A filesystem read cannot hang on an absent daemon, and it picks
up a USB dongle on the sample after it is plugged in.

### The three sources

| Source | Kind | Interval | Answers |
|---|---|---|---|
| `/sys/class/bluetooth` | `Tree` | 1s | is there a controller |
| `busctl … GetManagedObjects` | `Command` | 1s | power, devices, names, icons, batteries |
| `/sys/class/rfkill` | `Tree` (`type`,`soft`,`hard`) | 2s | which way a click should move |

---

## The action table

Programs are compile-time `&'static str` — the executor's whitelist is
the compiler. The only runtime-supplied arguments are D-Bus object
paths, which come from BlueZ's own reply rather than from anything
typed, and each rides as one argv element, never through a shell.

| Row | Action | Argv |
|---|---|---|
| power, soft-blocked | unblock | `rfkill unblock bluetooth` |
| power, off and unblocked | power on | `busctl --system set-property org.bluez <adapter> org.bluez.Adapter1 Powered b true` |
| power, on | power off | `rfkill block bluetooth` |
| power, hard-blocked | *(inert)* | — |
| device, connected | disconnect | `busctl --system call org.bluez <device> org.bluez.Device1 Disconnect` |
| device, paired idle | connect | `busctl --system call org.bluez <device> org.bluez.Device1 Connect` |
| device `[x]`, confirmed | forget | `busctl --system call org.bluez <adapter> org.bluez.Adapter1 RemoveDevice o <device>` |
| pair new | open dialog | `chonk-btpair` |

### Why the power click is three branches

`omarchy-bluetooth-power` is the reference — **read, reimplemented
natively, and never spawned**, because chonkstep must not require
Omarchy to be installed. Its argument, restated:

BlueZ never persists an adapter's `Powered` property, so powering off
through D-Bus lasts until the next boot. The rfkill soft block *does*
persist — `systemd-rfkill` saving and restoring every switch under
`/var/lib/systemd/rfkill` is its entire job. The block is also
all-or-nothing across every radio, where a `Powered` write addresses
one adapter.

So the block is the state and BlueZ follows it: unblocking leaves
`AutoEnable` at its stock default and `bluetoothd` powers the adapter up
by itself. The order matters in the other direction too — **a power-on
fails outright while the block is set** — which is why the click reads
rfkill before it decides.

A *hard* block (a physical switch) makes the click inert. Nothing this
desktop can run clears it, and a button that cannot work is worse than
no button.

> **Caveat, untested here.** `rfkill block/unblock` needs write access
> to `/dev/rfkill`, which is normally `CAP_NET_ADMIN` or a udev/polkit
> grant for the active seat. Omarchy's script calls it unprivileged and
> assumes that grant. On a desk without it the unblock silently fails
> and the tile keeps showing `OFF` — correctly, since the tile shows
> reality rather than the request. This has not been verified on
> hardware; see below.

---

## The four faces

| State | Face |
|---|---|
| powered, devices connected | count digits + lit rune, first device's name |
| powered, nothing connected | ghost digits + lit rune, `READY` |
| adapter present, off or daemon silent | ghost rune, dim `OFF` |
| no adapter at all | the SDK's dead screen, `BT` |

The last is a **first-class rendering**, not a fallback: it is what this
instrument looks like on the machine it was written on and on every
desktop without a controller. It is `wm_theme::panel::render_dead_tile`,
exactly as the link tile shows it for a machine with no NIC.

### The rune, and a defect the design pass caught

The mark started on a 9×13 dot grid and was **illegible at the size the
tile ships at**. The arithmetic is the whole argument: at the stock 56px
tile the glass is about 36px tall, so a 13-row grid gets 36/13 = 2.7px
per cell, of which the dot is 78% — a 2px speckle that read as noise.
Height was always what bound it, since the rune is taller than it is
wide while the glass is wider than it is tall.

Three changes, each verified by rendering and looking:

1. **7×9, trimmed to the mark's own extent.** Every column and row now
   lights somewhere; the two dead border columns the grid used to carry
   made every cell 29% narrower than the glass could afford.
2. **Digits beside the rune, not above it.** Stacked, the rune got a
   wide, short band, and a tall mark can only be as big as that band's
   height allows. Half the glass width and *all* of its height is the
   shape the rune wants — 4px cells instead of 2.7px.
3. **A fuller dot (0.92 of the cell, against the bar meters' 0.7).** A
   signal stair is a row of separate readings and wants the gap; this is
   one continuous glyph whose cells are mostly diagonal neighbours, and
   at 0.7 the strokes broke into unrelated speckles.

The count readout dropped from three digit positions to two to pay for
it. A signal percentage genuinely needs three to say 100; a controller
that grants seven simultaneous connections will never need a hundreds
column.

`the_rune_cell_stays_legible_at_the_stock_tile` in
`wm-theme/src/bluetooth.rs` is the regression that would have caught the
original.

Render the sheet yourself:

```sh
cargo run -p wm-theme --example preview_bluetooth -- /tmp/bt
```

---

## The panel

Rows, in the order that is the panel's grammar: power, then what is
connected, then what is merely known, then the way to add something new.

**Optimism with a deadline.** A Bluetooth connect takes seconds — a
headset has to be woken, negotiated with, and its profiles brought up. A
clicked row dims and gains an ellipsis immediately, but the toggle is a
request, not a fact. The truth is the next sample: every `Effect::Run`
sets `then:` to the BlueZ source that can confirm it, a pending row
reconciles the moment that sample agrees, and a pending that outlives
`PENDING_DEADLINE_SAMPLES` fresh samples reverts to showing reality.
The budget counts *readings*, not repaints — the dock ticks a panel at
~60Hz and a stale pass must not spend it.

**Two clicks to forget.** Forgetting is destructive and unrecoverable
from the panel: the pairing keys go with it. `PanelEvent` has no
long-press — it is press, release, scroll, motion and crossings, and a
panel takes no keyboard *ever*, by design — so the confirm is two clicks
on the same `[x]` within `FORGET_GRACE` (3s). The first arms it, which
inverts the cell to full ink so the pending question is on the face
rather than in someone's memory; the second commits; anything else
disarms it.

---

## `chonk-btpair`

### Why the dialog may run `bluetoothctl` when the dock may not

Same reasoning that puts a third-party dock tile in its own process: **a
separate process may block itself and nobody else.** `chonk-btpair` is a
window someone opened on purpose, it does one thing, and its failure
mode when BlueZ never answers is a dialog that says so and a close
button — not a frozen desktop. The panel spawns it detached and never
waits on it.

The exception is also unavoidable. **Pairing requires an agent**: BlueZ
will not pair without a process registering an `org.bluez.Agent1` object
it can call back into to ask "is the passkey the same on both screens?".
Registering a D-Bus *object* means serving a bus name, and `busctl` — a
client tool — cannot do it. The alternatives were to implement `Agent1`
against a D-Bus library (a dependency this workspace does not have and
would acquire for one dialog) or to use the agent BlueZ already ships in
its own control program. This takes the second.

### The agent mode: `DisplayYesNo`

The capabilities describe a device's *input and output*.
`KeyboardDisplay` (the default) promises both; `NoInputNoOutput`
promises neither and silently accepts anything; `DisplayOnly` and
`KeyboardOnly` promise one each. `DisplayYesNo` promises a screen and
exactly two buttons.

That is this dialog, exactly: a screen, a pointer, and **no keyboard at
all** — the SDK's `App` masks `EXPOSURE | BUTTON_PRESS` and nothing
else, and the dock panel that spawns it takes no keyboard by design. It
is also the capability Secure Simple Pairing's numeric comparison — the
modern default for anything with a display — wants.

Claiming `KeyboardDisplay` would be a lie with a consequence: BlueZ
would be entitled to ask for a PIN, and there would be no way to type
one. When a legacy device demands exactly that, the dialog reaches
`Phase::NeedsKeyboard`, says so, and names the tool that can do it,
rather than hanging on a prompt it cannot answer.

### The command stream

```text
agent DisplayYesNo
default-agent
scan on                     ← only after "Agent registered"
scan off                    ← scanning during a pair is noise on the same radio
pair <addr>
yes                         ← the human answered the numeric comparison
trust <addr>                ← so it reconnects on its own
connect <addr>              ← pairing without connecting leaves a silent headset
scan off / quit             ← on close, so the adapter is not left discovering
```

---

## What has and has not been tested

**The machine this was written on has no Bluetooth adapter.**
`/sys/class/bluetooth` does not exist, `bluetooth.service` is inactive,
`busctl` cannot activate `org.bluez`, and every `bluetoothctl`
subcommand hangs until killed.

So, plainly:

- **Tested.** Everything on this side of the sampler boundary: the fold
  from canned `busctl` replies, the row grammar, the hit-tests, the
  pending/deadline machine, the two-click confirm, the exact argv of
  every action, the pairing state machine against canned transcripts,
  and every face rendered at every size and theme. 84 tests
  (56 in `chonk-instruments`, 28 in `chonk-btpair`), plus the
  `wm-theme` renderer's own.
- **Not tested, and not testable here.** Any of it against a real
  radio. No pairing has ever been performed. The `busctl` reply shapes
  are written from BlueZ's documented interfaces and from the
  `--json=short` envelope captured live from a service this machine
  *does* run (`Properties.GetAll` on systemd) — not from a live BlueZ.
  The `bluetoothctl` transcripts are written from 5.87's documented and
  observed output, not captured from a live pairing. The `rfkill`
  permission caveat above is unverified.

"The state machine is correct" and "pairing works on your headset" are
two different claims. Only the first has evidence here.

---

## Wiring status

The tile is built, tested, re-exported from `chonk-shell`'s `widgets`
module, and **in the dock's default column** — directly under the link
tile, since the two radios read as one family and their panels are the
same shape:

```rust
DockItem::builtin("builtin:bluetooth", Box::new(BluetoothWidget::new())),
```

That makes seven instruments in the stock stack; `README.md` and
`desktop.rs`'s `builtin_items` doc were updated with it, and the pinned
id list in `builtin_ids_are_the_ones_already_written_to_users_dock_item_files`
carries the new id. A session with a remembered dock order that predates
the tile keeps its arrangement and gains the Bluetooth tile at the end,
which is what `dock_order::merge` is for. On a machine with no adapter
the tile shows the SDK's dead screen rather than vanishing — the same
answer the power tile gives a desktop with no battery, and the reason
the column's geometry does not depend on the hardware.

`chonk-btpair` builds and is on the CI clippy gate but is not in
`packaging/arch/PKGBUILD` or `scripts/install.sh` — no SDK app is,
`chonk-about` included, so shipping it is its own decision with no
precedent to copy.

## Known friction, for whoever fixes it

The two friction items this instrument reported against the widget SDK
have both been fixed, and the notes are kept here because the *shape*
of each answer is worth knowing:

* **`panel_spec` and `panel_input` now take the tile size**, exactly as
  `render` and `on_input` do — `fn panel_spec(&self, tile: u32)` and
  `fn panel_input(&mut self, event: PanelEvent, tile: u32)`. This
  instrument's `TILE_HINT` of 56 is gone; the panel is spec'd and
  hit-tested against the same metric it is drawn at, so its cells sit
  under their own clicks on a scaled desk.
* **`Effect::Run`'s runtime-argument rule is written down**, in
  `chonk_dock_widget::sampling::Argv` and in
  `docs/instrument-actions.md`: the program is the compile-time
  whitelist, the arguments are runtime, and a runtime word rides in a
  validated slot that refuses an operand shaped like an option. A BT
  action's object path is exactly the case it was written for.

One remains open:

* **`chonk_ui::App` cannot drive a window whose content changes on its
  own.** `App::run` blocks in `wait_for_event()` with no timeout, no
  timer and no wakeup channel, and keeps its connection and window id
  private so nothing outside can wake it. That is right for
  `chonk-about`, whose content never changes; it cannot serve a
  discovery list filling in from a child process over several seconds
  with no pointer input at all. `chonk-btpair` therefore carries its
  own loop, built from the same pieces in the same order so the two
  can be reconciled. **The fix belongs in the SDK**: `App` wants a
  `run_with(Options { redraw_interval })`, the affordance
  `chonk_ui::dockapp` already has on the socket side. When it grows
  one, `chonk-btpair/src/main.rs` should lose its loop.

