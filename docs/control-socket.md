# The chonkstep control socket, version 1

This is the wire contract between the chonkstep shell and anything that
wants to *show* the desktop's state or *steer* it from outside — a bar
widget that draws the workspace strip, a panel that lists outputs, a
script that switches workspace. It is written for an author working in
any language: everything is stated in bytes and JSON. The normative
implementation is `crates/chonk-shell/src/control.rs`; where this
document and that module disagree, the module is right and this
document has a bug worth reporting.

It exists so that a desktop shell can be built for chonkstep the way
one is built for Hyprland — reading a socket the compositor owns —
without chonkstep pretending to *be* Hyprland. The first consumers are
the Omarchy plugins under `omarchy/plugins/`, which are ordinary
[omarchy-shell](https://github.com/basecamp/omarchy) bar widgets that
happen to read this socket instead of Hyprland's.

Two invariants shape everything below, and both are inherited from the
dockapp protocol next door (`docs/dockapp-protocol.md`), which has
already shipped one bug of each kind:

1. **The shell never blocks on a client.** Every read and write on the
   shell's side is non-blocking. A client that hangs, floods, or stops
   reading is disconnected; the desktop does not notice.
2. **Every event is a complete statement.** There are no diffs and no
   sequence numbers to reconcile. A client that misses an event, or
   connects late, is fully correct after the next one. This is what
   makes a QML `Socket` with a `SplitParser` a sufficient client.

## 1. Transport

A `SOCK_STREAM` Unix-domain socket — stream, not SEQPACKET, because the
intended clients (Quickshell's `Socket`, `socat`, a shell script with
`nc -U`) speak streams. Messages are framed by newlines:

- One message is one JSON object, UTF-8, followed by exactly one `\n`
  (0x0A). The object itself contains no raw newline.
- A client line longer than **65,536 bytes** including the newline is a
  framing violation; the shell disconnects the client.
- Empty lines are ignored.

### 1.1 Path

```
$XDG_RUNTIME_DIR/chonkstep/control-<display>.sock
```

`<display>` is `WAYLAND_DISPLAY` if set, else `DISPLAY`, else
`default`, passed through the same sanitisation the dockapp socket
uses: a leading `:` is dropped, every character outside `[A-Za-z0-9_-]`
becomes `_`, and the result is cut at 32 characters. So a Wayland
session on `wayland-1` listens at `control-wayland-1.sock` and an X11
session on `:0` at `control-0.sock`.

The path is keyed on the display rather than the pid so it is stable
across the shell's own hot restart: a client that loses the connection
reconnects to the same path and gets a fresh snapshot.

The shell also exports the full path as **`CHONKSTEP_CONTROL_SOCKET`**
in the environment of every process it launches. A client should
prefer that variable and fall back to deriving the path. There is no
`/tmp` fallback: `$XDG_RUNTIME_DIR` is per-user and 0700, and the
`chonkstep/` directory under it is created 0700 (or verified to be)
before the socket is bound.

### 1.2 Lifetime of a connection

On accept, the shell sends `hello` and then one event for **every
facet** (§3), in the order listed there. That is the snapshot; the
client has complete state before it reads its first delta. From then
on the shell sends a facet's event whenever that facet changes.

Requests (§4) may be sent at any time after connecting, including
before `hello` has been read. The shell answers each request with
either the events it caused or an `error`.

The shell disconnects a client when: the client closes; a line
overflows (§1); the client's outbound buffer on the shell's side
exceeds **262,144 bytes** — meaning the client has stopped reading;
or the shell exits. A hot restart (`Action::Restart`) is an exit: the
new process rebinds the same path and clients reconnect.

"Closes" means both directions. A client that shuts down only its
writing side — which is what `printf ... | socat` does the instant
the pipe drains, before the shell has serviced the connection once —
keeps receiving until it hangs up: the snapshot, the answer to
whatever it wrote, and every later event. A one-shot script therefore
reads its answer; a client that wants to stop must actually close.

There is no authentication. The socket is reachable only by the user
whose session it is, and everything it offers — switching workspace,
reading which windows exist — that user's own keyboard already offers.

## 2. Message shape

Every message from the shell has an `event` key. Every message from a
client has a `request` key. Both are lowercase kebab-case strings.
Other keys are as listed below.

Clients **must ignore** unknown keys and unknown `event` values; the
shell adds fields and facets within a protocol version. The shell
**rejects** unknown `request` values with an `error` event and keeps
the connection open.

Integers are JSON numbers without a fraction. Workspace and output
indices are **0-based** on the wire; a bar that labels workspaces from
1 adds one at the edge, exactly as `window_menu_items` does for the
Move To submenu.

## 3. Events (shell → client)

### 3.1 `hello`

Always first. Identifies the protocol and the session.

```json
{"event":"hello","protocol":1,"session":"wayland","pid":1441097}
```

- `protocol` — the integer version of this document. Breaking changes
  bump it; a client that reads a number it does not know should
  disconnect rather than guess.
- `session` — `"wayland"` or `"x11"`, which binary is running.
- `pid` — the shell's process id.

### 3.2 `workspaces`

The facet the workspace strip is drawn from.

```json
{"event":"workspaces","active":0,"workspaces":[{"index":0,"windows":3},{"index":1,"windows":0},{"index":2,"windows":1}]}
```

- `active` — index of the current workspace.
- `workspaces` — one entry per existing workspace, ascending by
  `index`, contiguous from 0. `windows` counts the managed clients on
  that workspace (miniaturised ones included, dock and shell surfaces
  excluded) — the number a widget dims an empty workspace on.

chonkstep grows workspaces on demand (a window moved one past the end
creates it), so the array length changes. A client should render
whatever it is sent, not a fixed 1–10.

### 3.3 `outputs`

```json
{"event":"outputs","focused":0,"outputs":[{"index":0,"name":"eDP-1","x":0,"y":0,"width":2560,"height":1600,"scale":2.0}]}
```

- `focused` — index of the output the pointer is on, or `null` when
  the shell cannot say. This is the output a keyboard-summoned panel
  belongs on.
- `x`, `y`, `width`, `height` — the output's logical rectangle, in the
  same coordinate space the shell lays windows out in.
- `scale` — the shell's UI scale for that output.

A session that knows only one screen sends one entry named after
whatever the backend calls it (`"screen"` if it has no name).

### 3.4 `focus`

```json
{"event":"focus","window":{"id":2147483650,"title":"~ — foot","app_id":"foot","workspace":0},"count":4}
```

- `window` — the focused managed window, or `null` when nothing is
  focused (the desktop, a shell surface, a locked session). `id` is an
  opaque integer stable for the window's lifetime — a full 64-bit
  value such as `4294967297`, so a client must not assume it fits a
  32-bit int or that consecutive windows have consecutive ids; `title`
  and `app_id` may be empty strings; `workspace` is its workspace
  index.
- `count` — the number of managed windows across all workspaces, so a
  client can show "4 windows" without a `windows` facet.

The active window's *identity* is also available through
`wlr-foreign-toplevel-management`, which omarchy-shell's ActiveWindow
widget already uses; this event exists so a client with only the
socket is not blind, and so it can correlate focus with a workspace.

### 3.5 `theme`

```json
{"event":"theme","id":"nextstep-classic","name":"NeXTSTEP Classic","appearance":"dark","following":null}
```

- `id`, `name` — the active theme.
- `appearance` — `"dark"` or `"light"`.
- `following` — `"omarchy"` when the session follows Omarchy's
  current palette (`theme = "omarchy"`, see `docs/appearance.md`),
  else `null`. It reports the choice, not the outcome: a follow whose
  palette is missing wears the flagship theme and still says
  `"omarchy"`, because that is what the desk will wear the moment
  Omarchy sets one.

### 3.6 `error`

```json
{"event":"error","request":"focus-workspace","message":"no workspace 7 (3 exist)"}
```

- `request` — the `request` value of the message that failed, or
  `null` if it could not be parsed at all.
- `message` — for a human. Not stable; do not match on it.

## 4. Requests (client → shell)

The verb set is deliberately closed and tiny: it covers what a bar
needs and nothing else. Anything a client would otherwise want — launch
a terminal, change the theme, restart — already has a route (a
`[commands]` entry, the Themes submenu, the `restart` marker) and is
not duplicated here.

### 4.1 `snapshot`

```json
{"request":"snapshot"}
```

Re-sends every facet event, in §3 order. A client uses this after
seeing something it does not understand, or never.

### 4.2 `focus-workspace`

```json
{"request":"focus-workspace","index":2}
```

Switches to workspace `index`. Answered with a `workspaces` event (and
a `focus` event if focus moved), or an `error` if `index` is not an
existing workspace. This is a *switch*, never a *create*: a bar cannot
grow the workspace list, only a window move can.

Naming the workspace that is already active changes nothing, so the
shell has nothing to broadcast; the asking client — and only that
client — is sent the current `workspaces` event again as its
acknowledgement. Every request gets an answer, even a redundant one.

## 5. What is deliberately absent

- **No `windows` facet.** The window list is what
  `wlr-foreign-toplevel-management` is for, and every Wayland shell
  toolkit already speaks it. Publishing a second copy would invite a
  second definition of "a window".
- **No keyboard layout.** chonkstep does not switch layouts; when it
  does, that becomes a facet.
- **No subscribe/unsubscribe.** A connected client receives everything.
  The full snapshot is a few hundred bytes; filtering is not worth a
  verb.
- **No batching or coalescing guarantees.** The shell may emit the
  same facet twice in one tick if it changed twice; each is complete,
  so a client that redraws on every event is correct, just busy.

## 6. Minimal clients

Watch the desktop from a terminal:

```
socat - UNIX-CONNECT:"$CHONKSTEP_CONTROL_SOCKET"
```

Switch to the third workspace:

```
printf '{"request":"focus-workspace","index":2}\n' | socat - UNIX-CONNECT:"$CHONKSTEP_CONTROL_SOCKET"
```

In QML (Quickshell), the shortest thing that works:

```qml
Socket {
  path: Quickshell.env("CHONKSTEP_CONTROL_SOCKET")
  connected: true
  parser: SplitParser {
    onRead: message => {
      const m = JSON.parse(message)
      if (m.event === "workspaces") root.workspaces = m
    }
  }
}
```

That sample neither falls back to the derived path (§1.1) nor
reconnects after the shell's hot restart — and a Quickshell `Socket`
whose first connect fails will not try again just because `connected`
is set. `omarchy/plugins/chonkstep.workspaces/ControlSocket.qml` is
the version that does both, and is the one to copy.
