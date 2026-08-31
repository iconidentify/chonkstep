# chonk-switch

The appearance switch: light and dark mode as a piece of dock
hardware. One tile, one recessed slot sunk into the themed face, one
machined knob riding in it — sun engraved on the light throw, moon on
the dark one, and the exposed track lit paper-bright or slate-dark so
the desktop's mode reads from across the room. Click it and the knob
presses in, one atomic `toggle` request goes to the shell, and the
throw animates over a quarter second once the shell confirms.

It is also the Python SDK's worked example beyond hello-world: the
whole thing is one stdlib-only script on `bindings/python/chonkdock`,
drawing the desktop's own chiseled relief recipes (+80/−40 relative
bevels, hard black outer lines, a diagonal gradient face parsed from
the session's `theme_toml`) into a premultiplied RGBA buffer.

## The contract it speaks

The switch owns no policy — it reads and writes the shell's
appearance files:

- `$XDG_STATE_HOME/chonkstep/appearance` holds the current mode,
  `light` or `dark` (absent or unreadable reads as `light`).
- A click writes `toggle` to `appearance-request` in the same
  directory — temp file + rename, so the shell can never see half a
  request. The shell consumes the file and updates the mode file.
- The lever follows the *mode file*, not its own click: a mode changed
  by anyone — a keybinding, a config reload — moves this switch too.
  Its own click gets an optimistic throw with a deadline; if the shell
  has not confirmed in a couple of seconds, the lever settles back to
  what the file says. The file is the truth; the animation is a
  prediction.

Polling rides the SDK's draw tick: a small file read a few times a
second while idle (and no frame at all unless something changed),
~30 Hz only for the few frames the knob is moving, and nothing
whatsoever while the tile is hidden.

## Install

From a chonkstep checkout:

```
$ scripts/chonk-get install examples/chonk-switch
```

`build.sh` vendors the `chonkdock` SDK next to the script so the
installed copy is self-contained; the tile appears at the next shell
restart. Or register it in place by copying `chonk-switch.dockapp`
into `~/.config/chonkstep/dockapps/` with the exec path absolutized.

## Look at it without a dock

The renderer runs headless — this is how the design was iterated:

```
$ ./chonk-switch.py --render switch.png --size 112 --pos 0.0   # light
$ ./chonk-switch.py --render switch.png --size 112 --pos 1.0   # dark
```

`--pos` is the lever's travel (0 light … 1 dark); add `--pressed` for
the held-down face.

## Tests

Protocol-level and headless: a fake shell speaks real
`SOCK_SEQPACKET` datagrams to the real process — handshake, a
byte-exact first frame in each mode, exactly one atomic `toggle` per
click, an externally flipped mode moving the lever, answered pings,
and a settled switch sending nothing at all.

```
$ python3 -m unittest discover examples/chonk-switch/tests
```
