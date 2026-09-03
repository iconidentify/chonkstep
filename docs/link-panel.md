# The link panel

Right-click the dock's **LNK** tile and it folds out into the link
panel: every network this machine could be on, and the switches to move
between them. Wifi networks in range, saved connection profiles, wired
links, NetworkManager-native WireGuard tunnels, and the Tailscale
tailnet — one instrument, no settings app.

Left-click still means what it always meant on that tile (toggle the
wifi radio, or cycle which interface the tile watches on a machine with
no radio). The panel is the *detail view*, on the dock's reserved
button, and it closes the way every panel closes: click the tile again,
click away, or press Escape.

---

## What it shows

```
  HOMEBASE                  ▓▓▓▓▓▓░░    <- the current link, from the
  W I F I  ·  8 7 %                         tile's own reading
  ────────────────────────────────────

  C O N N E C T I O N S ──────────────
  ■  E   WIRED CONNECTION 1             <- lit lamp = active
  ■  W   HOMEBASE
  □  WG  WG-HOME                        <- a WireGuard tunnel
  ▪  VPN OFFICE-VPN              BUSY   <- dim lamp = in flight

  W I - F I  N E T W O R K S ─────────
  ▓▓▓▓▓▓░░  HOMEBASE        🔒 LINKED
  ▓▓▓▓░░░░  NEIGHBOUR 5G     🔒 JOIN…   <- secured, no saved profile
  ▓▓▓░░░░░  CAFE WIFI            OPEN
  ▓▓░░░░░░  LAB:5G          🔒  SAVED   <- one click connects

  T A I L S C A L E ──────────────────
  ■  TAILNET                       UP
     E X I T  N O D E   GATEWAY         <- where your traffic leaves

  ────────────────────────────────────
           ┌──────────┐
           │  RESCAN  │
           └──────────┘
```

Every row that can be clicked lifts under the pointer and sinks when
pressed. A press on one row released on another fires nothing — that
is a change of mind, not a command. A row that *cannot* act — a
network you are already on, a toggle already in flight, a locked
tailnet, a rescan still cooling — stays flat under the pointer, because
a lift is a promise.

### How it is drawn

Everything on the glass comes out of `wm_theme::instrument_panel`, the
design system the fold-out panels share with each other and with the
tiles behind them, so the panel reads as the LNK tile unfolded rather
than as a dialog that appeared beside it:

- **Three type steps, and only three.** Band labels are small, widely
  letter-spaced and dim; row names are the body; the verdict a row is
  *for* (`LINKED`, `UP`, `BUSY`) is the largest and hottest thing in
  the row. The header's link name sits a step above all of it.
- **Engraved dividers.** Every rule is a groove — a dark line with a
  lit one under it — not a hairline, which is what makes the panel look
  milled rather than drawn.
- **Levels are meters.** Signal strength is the same segmented LED
  meter the VOL tile's bars are made of, at the same currents: the
  network you are on burns at full, the alternatives are readable but
  idle. A percentage may sit *beside* a meter; it never replaces one.
- **Empty is designed.** A machine with no radio, or a scan that heard
  nothing, gets a socket milled into the glass with a dead meter in it
  and a legend beside it — the section says so rather than vanishing or
  dropping a sentence of prose where a row belongs.

### The rows

| Section | Row | Click does |
|---|---|---|
| CONNECTIONS | an inactive profile | brings it up |
| CONNECTIONS | an active profile | takes it down |
| WI-FI | a network with a saved profile (`SAVED`) | brings that profile up |
| WI-FI | an open network (`OPEN`) | connects to it |
| WI-FI | a secured network with no profile (`JOIN…`) | opens the join dialog |
| WI-FI | the network you are on (`LINKED`) | nothing — it is a fact, not a switch |
| TAILSCALE | the tailnet row | `tailscale up` / `down` |
| TAILSCALE | the `EXIT NODE` line | nothing — a reading, not a control |
| — | RESCAN | one explicit scan, then a cooldown |

`EXT` beside a connection means NetworkManager did not bring it up
(`connected (externally)`), so taking it down may not keep it down —
the row says so rather than pretending otherwise. Docker bridges,
`flannel`, and `tailscale0` itself normally look like this.

### WireGuard

WireGuard tunnels appear as ordinary connection rows tagged `WG`, and
toggle like any other profile. This works for tunnels **NetworkManager
manages** — the ones it lists as type `wireguard` (NM 1.16 and later),
whether you imported them with `nmcli connection import type wireguard`
or created them by hand.

`wg-quick` tunnels are deliberately out of scope: `wg-quick up` needs
root, and a panel that popped a password prompt for one row and not the
others would be a worse experience than a panel that is honest about
what it covers. Import the config into NetworkManager once and the row
appears.

---

## Joining a new secured network

A dock panel takes no keyboard, ever. That is a deliberate part of the
panel protocol and not an omission: a popover that could grab the
keyboard is a popover that can phish, so the panel vocabulary is
pointer events and nothing else.

Which leaves exactly one hole — a new secured network needs a
passphrase — and the panel does not try to paper over it. Clicking a
`JOIN…` row spawns **`chonk-netjoin`**, a real window with a real focus:
SSID at the top, a passphrase field with a show/hide toggle, Join and
Cancel. Tab moves between the four controls, Enter joins from anywhere,
Escape closes, and `Ctrl-R` reveals the passphrase without reaching for
the mouse.

The result is reported in the dialog itself — NetworkManager's own
words on failure, so a wrong passphrase says "secrets were required"
and hands the field back rather than closing on you. The panel finds
out the same way it finds out everything: from the next scan.

### Where the passphrase is not

`chonk-netjoin` runs:

```
nmcli --ask device wifi connect -- <ssid>
```

and writes the passphrase to that process's **standard input**.

The obvious spelling — `nmcli device wifi connect <ssid> password <pass>` —
would put the passphrase in the process's argument list, and argument
lists are world-readable through `/proc/<pid>/cmdline` for the whole
life of the process. Any account on the machine, and anything sampling
the process table, could read it. That is exactly why the panel refuses
to handle the secret itself, and it would be a poor joke to hand it to
a separate window and then leak it there.

`--ask` makes nmcli prompt instead, and a prompt reads standard input
whether or not there is a terminal behind it. The dialog also sends
nmcli's **stdout** to `/dev/null` and keeps only stderr: a prompt read
from a pipe has no terminal on which to disable echo, so stdout is the
one stream on which the passphrase could conceivably reappear, and the
simplest way not to show it is not to have it.

---

## Tailscale, and the operator grant

Reading the tailnet needs no privilege — `tailscale status` works as
any user — so the panel can always *show* whether you are up, whether
you are actually online, any health warnings, and your exit node if you
have one.

Changing it is different. `tailscale up` and `tailscale down` talk to
tailscaled's IPN bus, which is root-or-operator, so from an ordinary
account they come back:

```
Access denied: watch IPN bus access denied, must be root or Operator
Use 'sudo tailscale up' or 'sudo tailscale set --operator=$USER' to not require root.
```

The panel could have hidden this and shown a toggle that quietly did
nothing. Instead the row goes **`LOCKED`** and draws that second line —
the CLI's own remedy — underneath it, because the honest answer to a
click that cannot work is the command that would make it work:

```
  TAILSCALE ───────────────────────
  □ TAILNET                    LOCKED
  Use 'sudo tailscale set --operator=chris' to not require root.
```

Run that once, log the panel's opinion out of date by toggling
successfully, and the row is a working switch from then on.

`scripts/install.sh` offers to run it for you at install time —
explained in full, defaulting to **no**, and skipped entirely when
there is no terminal to ask at. It is a real privilege (that account
can then change the machine's VPN state with no further
authentication), so it is asked, never assumed.

If the `tailscale` binary is not installed at all, the section simply
does not appear. Until the first status sample lands the row reads
`SENSING`, and every other state it can be in says a word too — `UP`,
`DOWN`, `BUSY`, `LOGIN`, `AUTH`, `LOCKED`, `UNKNOWN`. It never says
`…`: an ellipsis is a promise of a menu, and this row has none to open.

### The exit node

When the tailnet is up **and traffic is actually leaving through an
exit node**, the row carries a labelled reading under it — `EXIT NODE
GATEWAY`, or `EXIT NODE  GATEWAY · OFFLINE` when the node your traffic
is routed through has stopped answering, which is the state that
silently breaks browsing and so is the one worth spelling out.

It is a fact, not a control: choosing an exit node is `tailscale set
--exit-node=…`, which is not in this panel's action table, and a row
that looked pressable but was not would be worse than a row that reads
honestly. When no exit node is in use the line is simply absent.

An earlier cut also counted the peers *offering* themselves as exit
nodes and drew `EXIT NODE  NONE · 3 OFFERED`. That is gone twice over.
It gestured at a picker this panel cannot open — the same sin as the
bare `…` it was brought in to replace — and it cost the sampler its
bound; see below.

### Why the status sample passes `--peers=false`

The panel samples `tailscale status --json --peers=false`, and the flag
is load-bearing rather than tidy.

The dock's command sampler reads a child's stdout *after* the child
exits. A command that writes more than a pipe buffer (64KB) without
exiting therefore blocks on the pipe forever and is killed when the 8s
sample deadline expires. Plain `tailscale status --json` is dominated
by its peer map at roughly 1KB per peer, so on a tailnet of any size it
walks straight into that: on the machine where this was found, 56 peers
made a 78KB document, the sampler was SIGKILLed on every single cycle
(`killing a command that never exited program="tailscale"`, every ~13
seconds — a 5s interval plus an 8s deadline), a worker thread was tied
up for eight of every thirteen seconds, and the TAILNET row had never
once shown a real reading. It only ever said `SENSING`.

What made it hide is that nothing is slow: run by hand the same command
answers in 8ms. The document was simply bigger than the transport.

`--peers=false` leaves a ~2.4KB document whose size does not depend on
how big anyone's tailnet is. That is the point — the fix is a bound,
not a bigger number. Everything the row draws survives it, because
`BackendState`, `Self.Online`, `Health` and the top-level
`ExitNodeStatus` all sit outside the peer map. The peer count of exit
nodes on offer did not survive, and is no loss for the reason above.

### How the panel decides it is locked

It tries. There is no way to ask "may I?" in advance, so the first
toggle is attempted for real:

- If the next status sample shows the state you asked for, the grant is
  **proven** and remembered. One flaky toggle later will not un-prove
  it.
- If a toggle is never confirmed, and the grant has never been proven,
  the row locks with the remedy.

That inference is right whenever the grant is really the problem. It
can lock the row for the wrong reason if a toggle fails some rarer way
before you have ever toggled successfully — reopen the session to clear
it. The precise detection (reading the denial out of the command's own
output) is written and tested; it is unwired only because the dock's
effect runner discards command output today.

---

## What it costs

The panel's four queries are all reads, and all cache reads:

| Query | Every |
|---|---|
| `nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device status` | 3s |
| `nmcli -t -f NAME,TYPE,ACTIVE,UUID connection show` | 3s |
| `nmcli -t -f IN-USE,SSID,SIGNAL,SECURITY dev wifi list --rescan no` | 3s |
| `tailscale status --json --peers=false` | 5s |

`--rescan no` is load-bearing. nmcli's default `--rescan auto` triggers
a real hardware scan and blocks for seconds; that is the bug that
froze the desktop for ~3.6s every ~34s in August 2026 and got the
instruments moved into a crate whose lints make I/O a compile error.

`--peers=false` is load-bearing for a different reason — output size
rather than latency; see "Why the status sample passes `--peers=false`"
above. Two of the four flags on this table are there to stop a *read*
from wedging the desktop, which is the shape of bug this instrument
keeps finding: nothing here is slow, and both bugs still stalled a
worker for seconds at a time.
The only scan this panel ever asks for is the explicit RESCAN row, and
that row disarms itself for about fifteen seconds afterwards.

After any action the confirming query is asked to sample immediately
rather than waiting out its interval, so the panel is quick exactly
when somebody is watching it.

### Optimism with a deadline

`nmcli connection up` takes seconds. A clicked row shows a dim `BUSY`
lamp at once — but the toggle is a *request*, and the truth is the next
sample. A pending row resolves the moment the system agrees with it,
and a pending row that outlives its deadline goes back to showing
reality, because an instrument that keeps saying BUSY after the system
said no is lying with extra steps.

---

## Known gaps

*(The dialog wearing the flagship theme was one of these and is fixed:
the dock's effect runner now spawns every command with the desktop's
launch environment — `CHONKSTEP_THEME`, `CHONKSTEP_APPEARANCE`,
`CHONKSTEP_SCALE` and the cursor/toolkit variables every GUI the shell
starts gets — so `chonk-netjoin` opens wearing what the desk is
wearing. See `chonk-shell`'s `shell::launch_env`.)*

- **Hidden networks** are not joinable from the panel — they do not
  appear in a scan by definition. `nmcli device wifi connect <ssid>
  hidden yes` still works.
- **802.1X / enterprise networks** need more than a passphrase and are
  not offered; the row shows as secured and joining it is a job for
  `nmcli` or a NetworkManager profile.
- **The wifi list shows the strongest eight networks.** A dock panel
  has no scroll gesture yet, and the tail of a dense apartment block is
  noise.
