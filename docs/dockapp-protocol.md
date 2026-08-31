# The chonkstep dockapp protocol, version 1

This is the complete wire contract between a *dockapp* — a separate
process that draws one or a few dock tiles — and the chonkstep shell.
It is written for an author working in any language, with no access to
the Rust crates: everything here is stated in bytes and syscalls. The
normative implementation is `crates/chonk-dock-proto`; where this
document and that crate disagree, the crate is right and this document
has a bug worth reporting.

A dockapp is neither an X client nor a Wayland client. It never opens a
display connection — the shell launches it with `WAYLAND_DISPLAY` and
`DISPLAY` **removed** from its environment — so there is nothing for it
to screenshot, no window list to enumerate, and no clipboard to read.
It pushes finished tile pixels over a private socket, and the shell
blits them exactly as it blits a built-in instrument's buffer.

A dockapp is also **not sandboxed**. It is a normal process running as
the user, with their home directory and their network. The boundary
protects the desktop's responsiveness and its pixels; it is not a
security boundary around the user's files.

The one invariant everything below serves: **the shell never blocks on
a dockapp.** Every send on the shell's side is non-blocking; a dockapp
that hangs, floods, or stops reading costs the desktop one tile and
nothing else.

## 1. Transport

The transport is a `SOCK_SEQPACKET` Unix-domain socket. SEQPACKET is
connection-oriented like a stream (a dead peer is an EOF, not a
timeout) but preserves message boundaries like a datagram: **one
`send()` is one `recv()`**. There is no length prefix and no framing —
the kernel keeps the boundaries, so a whole class of length-prefix
parser bugs does not exist in this protocol.

Consequences you must honor:

- Send each encoded message as exactly one `send()`/`write()` on the
  socket. Never split or concatenate messages.
- Receive with a buffer of at least `MAX_MESSAGE_BYTES` (262144). The
  shell does not pass `MSG_TRUNC`; a datagram larger than the receive
  buffer is silently truncated by the kernel and then fails the length
  checks — an over-large message is a protocol violation either way.
- A `recv()` returning 0 means the peer is gone (EOF). A zero-length
  datagram is deliberately treated identically: its only sane response
  is also to close.

### 1.1 Socket path

The shell binds:

```
$XDG_RUNTIME_DIR/chonkstep/dock-<display>.sock
```

`<display>` is the sanitized display name of the session: the value of
`WAYLAND_DISPLAY` if set, else `DISPLAY`, else the literal `default`.
Sanitization: strip leading `:` characters, then map every character
outside `[A-Za-z0-9_-]` to `_`, keep at most 32 characters, and use
`default` if nothing is left. So a Wayland session on `wayland-1` uses
`dock-wayland-1.sock` and an X session on `:1` uses `dock-1.sock`.

The `chonkstep` directory is created mode 0700 and verified (owned by
this user, not group/world accessible); the socket file is chmod 0600
after bind. There is **no `/tmp` fallback**: a session without
`$XDG_RUNTIME_DIR` has no dockapps.

The path is per-display rather than per-shell-pid **on purpose**: it is
stable across a shell restart, which is what makes restart survival
(section 7) possible at all.

A dockapp should not derive this path itself. The shell hands it over
directly (next section).

### 1.2 Launch environment

The shell launches a dockapp (its `.dockapp` registration's `exec`
argv, see section 9) with stdout and stderr on `/dev/null` and with
this environment added:

| Variable | Value |
| --- | --- |
| `CHONKSTEP_DOCK_SOCKET` | absolute path of the socket to connect to |
| `CHONKSTEP_DOCK_TOKEN` | the slot's 128-bit token as 32 lowercase hex digits |
| `CHONKSTEP_SCALE` | the session scale, formatted with four decimals (e.g. `2.0000`) |
| `CHONKSTEP_THEME` | the active theme id (e.g. `nextstep-classic`) |

and this environment **removed**: `WAYLAND_DISPLAY`, `DISPLAY`.

The token is minted fresh per launch with `getrandom(2)`; it is the
credential that stops any other process of this user from claiming the
slot (the 0600 socket in the 0700 directory is the outer lock, and the
shell also checks `SO_PEERCRED` uid — none of the three is load-bearing
alone). Parse the hex into 16 raw bytes for the `Hello` message.

`CHONKSTEP_SCALE` and `CHONKSTEP_THEME` are a convenience so the first
frame can be styled before the handshake completes; the authoritative
values arrive in `Welcome` and can change at any time via
`ThemeChanged`.

If `CHONKSTEP_DOCK_SOCKET` is absent, the binary was almost certainly
run from a terminal instead of being launched by the dock; say so and
exit.

## 2. Byte order, header, strictness

All multi-byte integers are **little-endian**, always. Both peers are
processes of the same user on the same kernel; network byte order would
buy nothing. The layout is fixed rather than native-endian so it is
documented rather than "whatever this build did".

Every message begins with a 4-byte header:

```
offset 0  kind      u8
offset 1  reserved  [u8; 3]   must be zero
```

Client→shell kinds use the low number space (`0x01`–`0x04`);
shell→client kinds have the high bit set (`0x81`–`0x86`), so a message
fed back down the socket it came from is a clean "unknown kind" rather
than a misparse.

Decoding is strict, and you should be too. The reference decoder
rejects — never clamps, truncates, or ignores:

- an unknown message kind;
- any non-zero reserved byte;
- trailing bytes after the last field (two byte strings must never
  mean the same message);
- a message ending mid-field;
- an out-of-range enum discriminant;
- any string over its cap, invalid UTF-8, or an id outside its charset;
- a `scale` float that is NaN, infinite, zero, negative, or above
  `MAX_SCALE` (a NaN scale would otherwise make a decoded message
  unequal to itself, and every "resend when changed" loop then resends
  forever);
- frame geometry outside the tile bounds, or a pixel payload whose
  length disagrees with `width * height * 4`.

There is no forward compatibility by design: the protocol version is
checked for **equality** at handshake, so there is no such thing as a
peer speaking a different version that gets to keep talking. A reserved
byte that starts meaning something is a version bump.

## 3. Limits

| Constant | Value | Why this value |
| --- | --- | --- |
| `PROTOCOL_VERSION` | 1 | Checked by equality, not `>=`: a dockapp built against a newer protocol is as unreadable as an older one, and "reject with a reason" beats "misparse a frame into garbage pixels". |
| `TOKEN_BYTES` | 16 | 128 bits, because the token is the only thing standing between a stray process of this user and a tile in the dock. |
| `MAX_MESSAGE_BYTES` | 262144 (256 KiB) | Derived, not picked. `AF_UNIX` refuses a datagram larger than `SO_SNDBUF - 32`; `SO_SNDBUF` is clamped to `net.core.wmem_max` (stock Linux: 212992) and then doubled by the kernel. Both ends ask for widened buffers (~416 KiB effective ceiling); 256 KiB is the largest round number that clears the widened floor with room to spare while staying close enough to the un-widened one (~208 KiB) that a failed widening is a bug, not a catastrophe. Enforced by every decode before a byte is copied. |
| `MAX_FRAME_BYTES` | 262080 (`MAX_MESSAGE_BYTES - 64`) | Ceiling on one `Frame`'s pixel payload. Covers every tile geometry this desktop plausibly has — a four-tile stack at scale 2, a two-tile stack at scale 3 — and stops short of a four-tile stack at scale 3 (451 KB), which is the documented trigger for a future v2 shared-memory transport. |
| `MAX_TILE_PX` | 256 | Widest tile edge in device pixels. A dock tile is 56 logical pixels; at scale 4 that is 224. 256 leaves headroom without letting a `Hello` claim a tile the size of the screen. |
| `MAX_SCALE` | 8.0 | Upper bound on the `scale` float. Every theme metric is multiplied by it before anything is drawn; eight is far past any display that exists while keeping the arithmetic sane. Enforced by the codec so both peers share one bound. |
| `MAX_TILE_UNITS` | 4 | How many stacked tiles one dockapp may occupy. The dock is a vertical strip on a real screen; asking for more than four is asking for the dock, not a slot in it. |
| `MAX_ID_BYTES` | 64 | A dockapp id keys the registry, appears in logs and the per-tile menu; every byte of it is attacker-chosen, so it is short and bounded. |
| `MAX_LOG_BYTES` | 256 | A `Log` line's text budget, bounded before it is ever shaped — a 10 MB "tooltip" handed to a text engine is a rendering denial of service that costs the attacker one `write()`. |
| `MAX_THEME_ID_BYTES` | 64 | Theme ids are kebab-case registry keys. |
| `MAX_THEME_TOML_BYTES` | 131072 (128 KiB) | The serialized-theme correctness path. The shell generates it, so this bound protects a *dockapp* from a hostile or broken shell — the SDK has no more reason to trust its peer than the shell has. |

A tile geometry "fits" when `tile_px` is in `1..=256`, `tile_units` is
in `1..=4`, **and** `tile_px * tile_px * tile_units * 4 <=
MAX_FRAME_BYTES`. The shell evaluates this while validating `Hello` and
refuses the connection (`Goodbye { TileTooLarge }`) rather than
accepting a dockapp whose every frame would then fail — and a client
should evaluate it again for every `Welcome`/`ThemeChanged` before
allocating a buffer.

## 4. Message catalog

Field layouts below start immediately after the 4-byte header.

### 4.1 Client → shell

#### `0x01 Hello`

```
proto       u32        must equal 1
tile_units  u8         stacked tiles requested, 1..=4
wants       u8         input-mask bits (below)
id_len      u8
reserved    u8         zero
token       [u8; 16]   raw bytes of CHONKSTEP_DOCK_TOKEN
id          [u8; id_len]
```

`id` must be 1–64 bytes of `[A-Za-z0-9._:-]` and must match the `id`
in the `.dockapp` registration that declared this program, or the shell
has no slot to give it. `:` is legal on the wire but the `builtin:`
prefix is reserved: the registry refuses registrations that claim it.

`wants` bits (a hint, not a permission — it lets a tile that only
paints avoid being woken for pointer traffic):

| bit | meaning |
| --- | --- |
| `1 << 0` | Press |
| `1 << 1` | Release |
| `1 << 2` | Scroll |
| `1 << 3` | Enter/Leave together (a tile that wants one always wants the other, or it latches into a permanent hover state) |

Any other bit set is a decode error.

Worked example — `Hello` for id `clock`, protocol 1, one tile, wanting
Press+Crossing, token `AB` repeated (33 bytes total):

```
01 00 00 00              kind + reserved
01 00 00 00              proto = 1
01                       tile_units
09                       wants = PRESS | CROSSING
05                       id_len
00                       reserved
AB x16                   token
63 6C 6F 63 6B           "clock"
```

#### `0x02 Frame`

```
generation  u32
width       u32        device pixels
height      u32        device pixels
pixels      [u8; width * height * 4]   the rest of the datagram
```

Pixels are **premultiplied RGBA8, top row first, no row padding** —
byte-identical to a `tiny_skia::Pixmap` buffer, which is what makes a
remote tile and a built-in tile the same thing at the shell's blit
seam.

`generation` is the dockapp's own counter; the shell echoes nothing
back. It exists so a log line can say *which* frame the rate limiter
dropped.

Bounds, checked in this order so the multiplication cannot overflow:
`width` in `1..=MAX_TILE_PX`, `height` in `1..=MAX_TILE_PX *
MAX_TILE_UNITS`, then `width * height * 4 <= MAX_FRAME_BYTES` (checked
separately — the per-edge caps alone do not imply it), then the payload
length must equal the product exactly.

**The reject-don't-rescale rule.** The shell compares every frame's
geometry against the geometry it allocated: `width == tile_px && height
== tile_px * tile_units`, as an equality with no clamping. A mismatched
frame is logged and discarded — the connection stays up, and the last
good frame stays on screen. Why: a monitor or scale change can resize
the tile mid-session, and a frame produced against the old size is a
frame from *before* the resize. Scaling it would draw a blurred, subtly
wrong tile that looks like a rendering bug in the dockapp; cropping
would be worse. The dockapp receives a `ThemeChanged` carrying the new
`tile_px` and its next frame is correct.

#### `0x03 Pong`

```
seq  u32   echoed from the Ping being answered
```

#### `0x04 Log`

```
level     u8    1 Error, 2 Warn, 3 Info, 4 Debug
reserved  u8    zero
text_len  u16
text      [u8; text_len]   UTF-8, at most 256 bytes
```

A dockapp's stdout and stderr are `/dev/null`; this is its one channel
into the session journal, logged under the dockapp's id. The shell
sanitizes text on arrival (control characters, U+2028/U+2029, bidi
overrides and isolates, zero-width characters are stripped) so a
dockapp cannot forge log entries or drive a terminal; send plain
single-line text. The reference encoder truncates an over-long line to
256 bytes; the decoder rejects one — a sloppy local caller still gets
its first 256 bytes through, but a *peer* sending an over-long line is
testing the bounds and gets nothing.

### 4.2 Shell → client

#### `0x81 Welcome` and `0x82 ThemeChanged`

Identical bodies; `Welcome` is sent exactly once, immediately after a
`Hello` is accepted, and `ThemeChanged` whenever the user picks a
different theme, the scale changes, or the tile size changes.

```
tile_px        u32    device pixels per tile edge
scale          u32    IEEE-754 f32 bit pattern, little-endian
theme_id_len   u16
reserved       u16    zero
theme_toml_len u32
theme_id       [u8; theme_id_len]      at most 64 bytes
theme_toml     [u8; theme_toml_len]    at most 131072 bytes
```

This is the **ThemeState contract**:

- `tile_px` — your surface is `tile_px` wide and `tile_px *
  tile_units` tall (the `tile_units` you asked for in `Hello`). Every
  `Frame` must match exactly.
- `scale` — the session's scale factor, transmitted bit-exactly (1.5
  and 2.25 are real values; a tile drawn at 1.4999999 lands its bevel a
  pixel off). Decoders must reject NaN, infinities, zero, negatives,
  and values above 8.0.
- `theme_id` is the fast path: a client that ships the built-in
  palettes looks the id up and parses nothing.
- `theme_toml` is the correctness path: a serialized theme table, so a
  client built against a different theme version — or a session running
  a user-defined theme with no built-in id — still gets the real
  palette by deserializing it. It may be empty.
- A client that can use neither falls back to its own default palette.
  The worst case is a tile in the wrong colors, never a tile that fails
  to draw.

**A dockapp never restarts for a theme change** — that is the entire
reason `ThemeChanged` exists rather than the shell killing and
relaunching the process. On receiving one: re-derive your palette,
reallocate your pixel buffer if `tile_px` changed, and send a fresh
frame (the shell has nothing current to show for you until you do).
The same connection continues; in-app state survives.

#### `0x83 Input`

```
kind      u8    1 Press, 2 Release, 3 Scroll, 4 Enter, 5 Leave
button    u8    0 none, 1 Left, 2 Middle, 3 Right
reserved  u16   zero
x         i32   tile-local device pixels
y         i32   tile-local device pixels
delta     i32   scroll notches, signed; 0 for everything but Scroll
```

Fixed 20 bytes. Coordinates are local to your own tile — a dockapp is
never told where its tile is on screen, or that other tiles exist.
`button` is 0 for Scroll/Enter/Leave.

`Middle` and `Right` exist on the wire but **the shell never sends
them**: middle is the dock's reorder gesture and right opens the
per-tile menu, and a dockapp that could swallow either could make
itself un-reorderable and un-removable. Expect Left and Scroll only.
(The shell also caps a single scroll gesture at 32 notches.)

#### `0x84 Visibility`

```
visible   u8      0 or 1; anything else is a decode error
reserved  [u8;3]  zero
```

`0` while the dock is hidden or your tile is scrolled out of the
visible strip. Stop sampling and stop drawing — not just stop being
looked at; a hidden tile polling a device is the same waste as a
visible one. On becoming visible again, send a frame even if nothing
changed: the shell may have nothing to show.

#### `0x85 Ping`

```
seq  u32
```

The shell pings every connected dockapp every **2 seconds**. Answer
promptly with `Pong` carrying the same `seq`. After **3 unanswered
pings** the tile is drawn dimmed as hung — that exists to tell the
*user* something is wrong, not to protect the desktop, which was never
at risk. The connection is not closed for hanging; answering again
un-dims the tile.

#### `0x86 Goodbye`

```
reason    u8      table below
reserved  [u8;3]  zero
```

Sent best-effort before the shell closes the fd, so a dockapp can log
something better than a bare EOF.

| code | reason | what to do |
| --- | --- | --- |
| 1 | `Shutdown` | The session is ending. Exit cleanly; reconnecting is pointless. |
| 2 | `ProtocolError` | You sent something that did not decode or violated a bound (including a second `Hello` on an open connection, or anything before `Hello`). The shell logs specifics; the wire carries only this. |
| 3 | `Unauthorized` | Wrong token, or an id with no registry slot. You were not launched by this shell. |
| 4 | `Replaced` | Another connection claimed this id. One connection per id. |
| 5 | `TileTooLarge` | The geometry you asked for cannot be carried inline by v1. Ask for fewer tile units. |
| 6 | `Overflow` | You stopped reading and the shell's send queue stayed full (section 6). Almost always followed by the fd closing immediately — a peer in this state is by definition not reading this message either. |
| 7 | `Removed` | The user removed the tile from the dock. |

## 5. The handshake

1. `connect()` to `CHONKSTEP_DOCK_SOCKET`. Use a non-blocking connect:
   an `AF_UNIX` connect to a listener with a full backlog otherwise
   waits *indefinitely*. A `WouldBlock`/`EAGAIN` connect means try
   again later.
2. Send `Hello` as your very first message. Anything else first —
   including a `Frame` — is a protocol error; a dockapp does not get to
   skip authentication by starting to draw.
3. Wait up to **2 seconds** for one message:
   - `Welcome` — you are admitted; its ThemeState is your geometry and
     palette. Start drawing.
   - `Goodbye { reason }` — you were refused; the reason is actionable
     (version mismatch means "rebuild me", bad token means "you were
     not launched by this shell", `TileTooLarge` means "ask for fewer
     units").
   - anything else, or a decode failure — the two ends disagree about
     the protocol badly enough that continuing would be guessing.
     Close and exit.
4. If nothing arrives in 2 seconds, give up: a shell pointed at answers
   from its event loop in a repaint pass or two, so two seconds is
   three orders of magnitude of slack.

The shell validates in this order: `proto == 1` (else
`ProtocolError`), then the token in constant time (checked before
geometry, so a wrong token never learns anything from *which* rejection
it got — else `Unauthorized`), then that the tile fits (else
`TileTooLarge`).

On the shell's side, a launched process has **10 seconds**
(`HANDSHAKE_GRACE`) to complete the handshake before it is counted as a
failed launch.

## 6. Flow control: what gets dropped

Both directions answer the same question — *what gets dropped?* —
because the one answer that is not available is "the shell waits".

**Outbound (shell → dockapp).** Sends are non-blocking; undeliverable
messages go into a bounded queue of **64** per connection. When the
queue is full the *oldest* entry is dropped (every message is either a
pointer event, where stale is worse than useless, or a state update
whose newest value supersedes the rest). If the queue stays full for a
sustained **2 seconds**, the shell sends `Goodbye { Overflow }`
best-effort and closes the connection. Keep calling `recv()`; a healthy
dockapp never sees more than one or two queued messages.

**Inbound (dockapp → shell).** Frames pass a token bucket at **30
frames/second** (bucket depth one second's worth; it starts full so the
first frame after a handshake is never delayed). Above the budget,
frames are *coalesced*, newest-wins — not queued, and not a
disconnection. A dockapp pushing 1000 fps gets 30 blitted, and the 970
it does not get are ones it already overwrote. Frames also do not
accumulate credit while idle: a tile that sat still for an hour cannot
then blast a backlog.

**Sending frames from the client.** If your own `send()` returns
`EAGAIN`/`WouldBlock`, the shell's receive buffer is momentarily full —
drop the frame and move on. The next frame supersedes it anyway, and
the shell's limiter would have coalesced them to the same outcome.

Practical cadence: built-in instruments update at 1 Hz. Draw only when
something changed; the 30 Hz cap is a ceiling on abuse, not a target.

## 7. Restart survival and readoption

The socket path is stable per display, and a shell restart (an update,
a crash) deliberately sends **no `Goodbye`** — a bare EOF means "try
again", while `Goodbye { Shutdown }` means "stay down".

Client half, on EOF: retry `connect()` against the same path with
backoff — first delay 100 ms, doubling, capped at 1 s per attempt — for
a window of **10 seconds**. On success, redo the whole handshake (same
id, same token from the environment) and treat the new `Welcome` as
authoritative; assume you are visible until told otherwise. If the
window elapses, exit: the shell is not coming back, and when it does,
its registry relaunches you.

Shell half: the outgoing shell leaves dockapp processes running and
hands each slot's token to its replacement, which holds the slot open
for the same **10 seconds** and readopts the survivor instead of
launching a second copy. The two windows are deliberately one number —
a shorter shell wait double-launches while the survivor is still
knocking; a longer one leaves a hole in the dock after the survivor
gave up.

The payoff: a dockapp survives theme switches (no reconnect at all —
just `ThemeChanged`), shell restarts, and shell upgrades, keeping its
in-process state throughout. On the Wayland session that is strictly
better than any ordinary client gets.

## 8. Supervision, crashes, and the crash-loop cutoff

The shell supervises the processes it launches:

- Exit behavior follows the registration's `restart` policy:
  `on-crash` (the default — relaunch after a crash, but a clean exit 0
  means "I decided I am done" and is honored), `always` (relaunch
  whatever the status), `never` (one launch per session).
- Failed launches back off exponentially: 1, 2, 4, 8, then 30 seconds.
- **Five failures inside a 60-second window stop the tile permanently**
  (dead face, log line naming it), whatever the policy — a dockapp
  restarted forever is an invisible fork bomb. The user can restart it
  from the tile's menu once the cause is fixed. The window is not
  cleared by a successful connection: a tile that comes up, runs for a
  moment, and dies is crash-looping just as surely as one that dies at
  `exec`.

A *hung* dockapp (connected, not answering pings) is dimmed, not
killed; a *crashed* one (EOF without `Goodbye` from the shell's side)
goes through the restart policy.

## 9. The `.dockapp` registry

The shell only launches dockapps it finds registered. One TOML file per
dockapp, named `<anything>.dockapp`, scanned at shell startup from:

1. `$XDG_DATA_DIRS/chonkstep/dockapps/` (each entry, in order; default
   `XDG_DATA_DIRS` is `/usr/local/share:/usr/share`), then
2. `$XDG_CONFIG_HOME/chonkstep/dockapps/` (default `~/.config/...`),
   which wins an id collision — the user's copy shadows the system's.

```toml
id = "chonk-dockclock"       # required; 1-64 chars of [A-Za-z0-9._-];
                             # may not begin with the reserved "builtin:" prefix
name = "CLK"                 # optional label for the starting/dead/hung face;
                             # defaults to the id upper-cased, clipped to 5 chars
exec = ["chonk-dockclock"]   # required argv array; exec[0] is the program.
                             # An array, never a command string: nothing splits,
                             # so paths with spaces cannot go wrong.
tile_units = 1               # optional, 1..=4; default 1
restart = "on-crash"         # optional: "on-crash" (default) | "always" | "never"
```

`id` is the registry key, the dock-order persistence line, the `Hello`
id, and the per-tile menu title — keep it stable; renaming the `name`
label must not reset the user's dock arrangement. A file that fails
validation is dropped with a log line naming the file and the reason.
Dockapps installed mid-session appear at the next shell restart.

## 10. Conformance checklist

A client implementation is conformant when it:

- [ ] connects with `SOCK_SEQPACKET`, one message per datagram;
- [ ] sends a correct `Hello` first, and times out the `Welcome` wait
      at ~2 s;
- [ ] encodes exactly the layouts above, little-endian, reserved bytes
      zero;
- [ ] rejects (by disconnecting) any server message that does not
      decode strictly;
- [ ] validates every `Welcome`/`ThemeChanged` geometry and scale
      before allocating, and rebuilds its buffer without restarting;
- [ ] sends frames that exactly match `tile_px` × `tile_px *
      tile_units`, premultiplied RGBA8, top row first;
- [ ] sends its first frame after every (re)handshake and theme change
      unprompted, and thereafter only when something changed;
- [ ] answers every `Ping` with a matching `Pong`;
- [ ] stops drawing *and sampling* while invisible, and forces a frame
      on becoming visible;
- [ ] treats `Goodbye { Shutdown }` as a clean exit and EOF as
      "reconnect for 10 s, then exit";
- [ ] drops (does not block on) its own `EAGAIN` sends.

The reference client is `crates/chonk-ui/src/dockapp.rs`; the
end-to-end sequence a shell expects is written out longhand in
`crates/chonk-ui/tests/dockapp_conformance.rs`, and
`examples/chonk-dockclock` is the shipped conformance dockapp. Language
bindings that implement this document live in `bindings/`.
