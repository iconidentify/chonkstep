//! One out-of-process dock tile: its process, its socket, its pixels,
//! and the three faces it wears when any of those is missing.
//!
//! # The inversion this file exists to deliver
//!
//! **A hung dockapp costs the compositor nothing.** The shell never
//! blocks on it. It does not wait for a frame, it does not wait for a
//! reply, and it does not wait for the socket to become writable: every
//! read is `MSG_DONTWAIT`, every write is `MSG_DONTWAIT` behind a
//! bounded queue that drops rather than blocks, and a dockapp that
//! wedges simply stops appearing in the `recv` loop. Frames stop
//! arriving; nothing else happens.
//!
//! So the liveness check below — ping every [`PING_INTERVAL`], three
//! unanswered and the tile is [`TileState::Hung`] — is **not** there to
//! protect the desktop. The desktop is already safe, structurally,
//! whatever the dockapp does. It is there to tell the *user*, because
//! the failure mode it catches is silent by construction: a tile that
//! stopped updating looks exactly like a tile whose reading has not
//! changed. A frozen clock at 14:32 is a lie the user has no way to
//! detect, and it is worse than a blank square, because they will act
//! on it.
//!
//! That inversion is the whole deliverable of the dockapp boundary.
//! Contrast it with the incident that motivated the boundary: on
//! 2026-08-29 a *built-in* wifi tile called `nmcli dev wifi` from the
//! repaint thread, and one slow subsystem froze the entire desktop for
//! ~3.6s at a time while the stall watchdog blamed the display driver.
//! The same failure, out of process, costs one tile its updates and
//! produces a log line naming the dockapp. The watchdog for in-process
//! widgets (`crate::widgets::SupervisedWidget`) still exists and still
//! covers this tile's own render — but for a *remote* tile it can only
//! ever fire on the blit of a stored buffer, because that is the only
//! work the shell does on its behalf.
//!
//! # Faces
//!
//! * **Starting** — dead tile carrying the dockapp's tag. It has been
//!   launched and has not said `Hello` yet, or is waiting out a
//!   backoff.
//! * **Live** — the last frame it sent, blitted verbatim. A built-in's
//!   `DecorationBuffer` and a dockapp's `Frame` payload are the same
//!   bytes in the same layout, which is what makes the two
//!   indistinguishable at the dock's blit seam.
//! * **Hung** — the last good frame, blended ~50% toward
//!   `theme.tile.fill`. Deliberately *not* the frame as sent, and
//!   deliberately not a blank tile either. Showing a stale reading as
//!   though it were live is the worst of the three options; a dimmed
//!   last-good frame says "this stopped" while still showing what it
//!   last knew, which is often exactly what the user wants to see.
//! * **Dead / crash-looped** — dead tile, tag, and a dim cross. A
//!   left-click here is the retry gesture: it relaunches and resets the
//!   backoff, because a user clicking a dead tile is telling the shell
//!   they have fixed something.

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chonk_dock_proto::transport::{Seqpacket, ENV_SOCKET, ENV_TOKEN};
use chonk_dock_proto::wire::{frame_matches_tile, Button, InputEvent, InputKind, InputMask, LogLevel, PanelCloseReason, ThemeState};
use chonk_dock_proto::wire::GoodbyeReason;
use chonk_dock_proto::{transport, ClientMessage, FrameLimiter, SendOutcome, SendQueue, ServerMessage, TOKEN_BYTES};
use wm_theme::model::{Color, Fill};
use wm_theme::{panel, tile as tilekit, Theme};
use wm_theme_api::{DecorationBuffer, Point};

use crate::dockapp::registry::{DockappEntry, RestartPolicy};
use crate::spawn::{self, SpawnedChild, DOCKAPP_WITHHELD_ENV};
use crate::widgets::{DockInput, DockWidget, Effect, Samples};

/// How often the shell asks a connected dockapp whether it is still
/// there.
///
/// Two seconds, and the number is a user-interface decision rather than
/// a protocol one: it is how long a wedged tile may keep showing a
/// reading that reads as current. Six seconds (three of these) to the
/// dimmed face is short enough that nobody trusts a frozen number for
/// long, and long enough that a dockapp doing something briefly
/// expensive between frames is not accused of dying.
pub(crate) const PING_INTERVAL: Duration = Duration::from_secs(2);

/// Unanswered pings before the tile is declared hung. Three, so a
/// single lost wakeup — a machine coming out of suspend, a dockapp
/// mid-`fork` — is not a verdict.
pub(crate) const UNANSWERED_PINGS_BEFORE_HUNG: u32 = 3;

/// How long a launched process has to complete its handshake.
///
/// A dockapp that never connects is indistinguishable from one that
/// crashed before `main`, and both need the same answer: count it as a
/// failure, back off, try again — and, if it keeps happening, stop.
/// Ten seconds is generous against `HANDSHAKE_TIMEOUT`'s two, because
/// the client's timeout measures the *shell's* responsiveness while
/// this one measures a cold binary's startup on a loaded machine.
pub(crate) const HANDSHAKE_GRACE: Duration = Duration::from_secs(10);

/// How long a tile holds its slot open for a dockapp handed over from
/// the previous shell before giving up and launching a fresh one.
///
/// Ten seconds, matched deliberately to the SDK's `RECONNECT_WINDOW`
/// (`chonk_ui::dockapp`): the survivor tries for exactly that long, so a
/// shorter wait here would launch a second copy while the first was
/// still knocking, and a longer one would leave a hole in the dock after
/// the survivor had already given up and exited. The two numbers are one
/// number, and changing either without the other reintroduces the
/// double-launch this whole mechanism exists to remove.
pub(crate) const REJOIN_WINDOW: Duration = Duration::from_secs(10);

/// Notches beyond which one drained scroll gesture stops being
/// forwarded.
///
/// `ScrollDelta` is a count and a backend may legitimately fold several
/// notches into one entry, so the dock replays it as that many discrete
/// steps (see `Shell::on_shell_scroll`). "That many" therefore needs a
/// ceiling, or a backend bug — or a hostile one, on a platform where
/// the wheel value is client-influenced — turns one event into an
/// unbounded loop on the repaint thread. Thirty-two is far past the
/// hardest flick a real wheel produces in one report.
pub(crate) const MAX_SCROLL_STEPS: i32 = 32;

/// Why a tile is not coming back on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StopReason {
    /// [`LaunchBudget`]'s hard cutoff tripped. This is the one that is
    /// not negotiable — see that type.
    CrashLooped,
    /// `restart = "never"`, and it has had its one launch.
    PolicyNever,
    /// It exited zero under `restart = "on-crash"`. A dockapp that
    /// exits cleanly has decided it is done — a battery tile on a
    /// desktop with no battery, say — and relaunching it is an argument
    /// the shell cannot win.
    CleanExit,
    /// The user removed the tile from the dock.
    Removed,
}

/// What a tile is doing right now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TileState {
    /// Backing off. `until` is when the next launch attempt is due.
    Waiting { until: Instant },
    /// Launched; no `Hello` yet. `since` bounds it against
    /// [`HANDSHAKE_GRACE`].
    Starting { since: Instant },
    /// Handed forward across the shell's own restart: a process from
    /// the *previous* shell is still running with a token this tile has
    /// inherited, and this tile is holding its slot open instead of
    /// launching a second copy. `until` is when it gives up and does.
    ///
    /// Distinct from [`Starting`](Self::Starting) rather than folded
    /// into it because the difference is real and shows up in three
    /// places: there is no child process of ours to reap or terminate,
    /// giving up is not a *failure* (nothing crashed — a dockapp simply
    /// did not come back) so it must not spend the crash-loop budget,
    /// and the log lines say entirely different things.
    Rejoining { until: Instant },
    /// Connected, answering pings, sending frames.
    Live,
    /// Connected, but [`UNANSWERED_PINGS_BEFORE_HUNG`] pings have gone
    /// unanswered. The socket is still open — this is not a
    /// disconnection, it is a dockapp that has stopped running its own
    /// event loop.
    Hung { since: Instant },
    /// Stopped for good, until the user says otherwise.
    Stopped { reason: StopReason },
}

/// The restart policy's arithmetic: exponential backoff, plus a hard
/// cutoff that is a cutoff and not merely a longer backoff.
///
/// # Why the cutoff is not negotiable
///
/// A dockapp restarted forever is an invisible fork bomb. Backoff alone
/// does not fix that: a 30-second cap still means 2,880 process
/// launches a day for a tile the user cannot see failing, each one
/// forking, exec'ing, loading a text shaper, and dying. The user's only
/// symptom is a machine that is inexplicably busy, and nothing in the
/// dock says which tile is doing it.
///
/// So: five failures inside [`CRASH_LOOP_WINDOW`] and the tile stops
/// permanently, with a log line naming it and a dead face carrying its
/// tag. The user can restart it from its menu once they have fixed
/// whatever is wrong, which is the only thing that could actually
/// change the outcome.
///
/// The window is *not* cleared by a successful connection, and that is
/// the subtle part. A dockapp that connects, draws one frame and dies,
/// five times in a minute, is crash-looping just as surely as one that
/// never connects at all — and a "reset on success" rule would let it
/// do so forever at the shortest backoff. What a success does reset is
/// the *backoff exponent*, and only after the connection has survived a
/// full window: a tile that ran happily for ten minutes and then died
/// deserves its first retry a second later, not thirty.
#[derive(Debug)]
pub(crate) struct LaunchBudget {
    /// Failure instants inside the sliding window. Bounded by
    /// [`MAX_FAILURES`](LaunchBudget::MAX_FAILURES) — pruned on every
    /// record, so this cannot grow.
    failures: VecDeque<Instant>,
    /// Consecutive failures, for the backoff exponent.
    attempts: u32,
}

/// The sliding window the crash-loop cutoff counts within.
pub(crate) const CRASH_LOOP_WINDOW: Duration = Duration::from_secs(60);

/// What [`LaunchBudget::record_failure`] decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Fate {
    /// Try again at this instant.
    Retry { at: Instant },
    /// The cutoff tripped. Do not try again this session.
    CrashLooped,
}

impl LaunchBudget {
    /// Five in a minute. Four would catch a dockapp that fails once per
    /// startup phase on a slow machine; six is long enough that a
    /// genuinely broken tile has already forked six times before
    /// anybody notices. Neither number is magic — what matters is that
    /// there *is* one.
    pub(crate) const MAX_FAILURES: usize = 5;

    /// 1, 2, 4, 8, then 30 seconds forever. The cap is well under the
    /// crash-loop window on purpose: a tile still failing at the cap is
    /// producing two failures per window, so the window is what
    /// eventually stops it, not the cap.
    const BACKOFF_SECONDS: [u64; 5] = [1, 2, 4, 8, 30];

    pub(crate) fn new() -> Self {
        Self { failures: VecDeque::new(), attempts: 0 }
    }

    /// Books one failure and says what happens next.
    ///
    /// `stable` is whether the connection that just died had been up
    /// for at least [`CRASH_LOOP_WINDOW`] — see the type's docs for why
    /// that resets the exponent and nothing else.
    pub(crate) fn record_failure(&mut self, now: Instant, stable: bool) -> Fate {
        while self.failures.front().is_some_and(|at| now.saturating_duration_since(*at) >= CRASH_LOOP_WINDOW) {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        if self.failures.len() >= Self::MAX_FAILURES {
            return Fate::CrashLooped;
        }
        if stable {
            self.attempts = 0;
        }
        let step = (self.attempts as usize).min(Self::BACKOFF_SECONDS.len() - 1);
        self.attempts = self.attempts.saturating_add(1);
        Fate::Retry { at: now + Duration::from_secs(Self::BACKOFF_SECONDS[step]) }
    }

    /// Forgets everything. Only ever called for an explicit user
    /// gesture (a click on a dead tile, Restart from its menu), because
    /// the user saying "try again" is the one piece of evidence the
    /// shell has that the *cause* might have changed.
    pub(crate) fn reset(&mut self) {
        self.failures.clear();
        self.attempts = 0;
    }

    pub(crate) fn recent_failures(&self) -> usize {
        self.failures.len()
    }
}

impl Default for LaunchBudget {
    fn default() -> Self {
        Self::new()
    }
}

/// A live connection to one dockapp.
struct Connection {
    socket: Seqpacket,
    /// Bounded, drops rather than blocks. The comment on the send site
    /// is the important one — see [`RemoteTile::enqueue`].
    send: SendQueue,
    /// Which events this dockapp asked for. A hint, not a permission:
    /// middle and right are refused to every dockapp regardless (see
    /// [`RemoteTile::on_input`]).
    wants: InputMask,
    ping_seq: u32,
    last_ping: Instant,
    unanswered: u32,
    /// When this connection was established, for
    /// [`LaunchBudget::record_failure`]'s `stable` argument.
    since: Instant,
    /// The geometry and palette this connection has been *told* about —
    /// the `Welcome` it was adopted with, then whatever
    /// [`RemoteTile::push_theme`] has sent since.
    ///
    /// Kept per connection rather than per tile because it is a fact
    /// about a conversation: a dockapp that reconnects has been told
    /// nothing yet, whatever the tile knew about its predecessor.
    ///
    /// Held *unmasked* — the change comparison in
    /// [`RemoteTile::push_theme`] runs against the broadcast state once
    /// per pass, and masking is applied only at the send
    /// ([`ThemeState::for_client`]), so the comparison never sees two
    /// differently-masked copies of the same fact.
    told: ThemeState,
    /// The protocol version this connection's `Hello` announced.
    ///
    /// Two decisions key on it, and both are the same incident wearing
    /// different hats: what may be put in the formerly-reserved `proto`
    /// u16 of `Welcome`/`ThemeChanged` (a version-1 client gets the
    /// byte-exact v1 wire, zeros included — a strict v1 decoder
    /// rightly dies on anything else), and whether the panel family
    /// (`0x05`–`0x07`) is legal from this peer at all (it needs `>= 2`
    /// — a client that said 1 was told `proto` 0 and has no business
    /// sending them).
    proto: u32,
}

/// How often a streaming panel's pixels are re-presented to the
/// screen, at most. The panel equivalent of the tile's 30 Hz frame
/// budget — but where a tile frame is a whole picture the
/// [`FrameLimiter`] can coalesce newest-wins, a panel repaint is a
/// *sequence of bands* blitted into a persistent buffer, so the
/// metering moves from "which frame" to "when does the buffer reach
/// the screen". Bands are always blitted (each is one bounded memcpy);
/// this bounds the expensive half, the surface upload.
pub(crate) const PANEL_PRESENT_INTERVAL: Duration = Duration::from_millis(33);

/// The pixels and bookkeeping of one open instrument panel — the
/// dockapp-facing half. The shell surface it is drawn on, its chrome,
/// and its place beside the dock belong to the desktop
/// (`crate::dockapp::panel`); this half owns what came over the wire.
///
/// `PanelFrame` is banded — each message repaints a horizontal strip —
/// so the shell keeps one persistent, grant-sized buffer per panel
/// (allocation bounded by `MAX_PANEL_BYTES`) and blits bands into it
/// on receipt. There is no atomicity across bands, by contract: a
/// half-applied repaint is on screen for at most one present interval.
struct PanelState {
    /// The size the grant promised — every band's `width` must equal
    /// `granted.0` and its rows must stay inside `granted.1`.
    granted: (u32, u32),
    /// The persistent panel surface bands are blitted into.
    /// Transparent until streamed, so the desktop's empty well shows
    /// through regions the client has not painted yet.
    buffer: DecorationBuffer,
    /// Whether any band has ever landed — before that, the desktop
    /// draws the bare well rather than a transparent buffer.
    streamed: bool,
    /// The newest `generation` seen. Bands from an older repaint may be
    /// dropped under flow control (the contract's drop rule); newness
    /// is a wrapping comparison so the counter rolling over is not a
    /// stuck panel.
    newest_generation: u32,
    /// The buffer changed since the desktop last presented it.
    dirty: bool,
    /// When the desktop last presented, for [`PANEL_PRESENT_INTERVAL`].
    last_present: Option<Instant>,
    /// The desktop must (re)stage the surface: a fresh open, or a
    /// renegotiation that may have changed the size.
    just_opened: bool,
}

/// Clamps an `OpenPanel` request to what the shell will grant: the
/// protocol caps and the workarea beside the dock (`bounds`, the
/// largest *content* area available, in device pixels). `None` means
/// nothing sensible can be granted — a degenerate request or workarea —
/// and the request is refused.
pub(crate) fn clamp_panel_grant(width: u32, height: u32, bounds: (u32, u32)) -> Option<(u32, u32)> {
    let max_w = bounds.0.min(chonk_dock_proto::MAX_PANEL_PX);
    let max_h = bounds.1.min(chonk_dock_proto::MAX_PANEL_PX);
    if max_w == 0 || max_h == 0 || width == 0 || height == 0 {
        return None;
    }
    let (w, h) = (width.min(max_w), height.min(max_h));
    chonk_dock_proto::panel_fits(w, h).then_some((w, h))
}

/// Whether `candidate` is a newer generation than `reference`, in the
/// wrapping sense (the same arithmetic TCP sequence numbers use). Equal
/// counts as "not older", so several bands of one repaint all land.
fn generation_newer(candidate: u32, reference: u32) -> bool {
    candidate != reference && candidate.wrapping_sub(reference) < u32::MAX / 2
}

/// Everything one servicing pass needs from the shell.
pub(crate) struct ServiceContext<'a> {
    pub now: Instant,
    /// The geometry and palette the dock is laid out for *right now*,
    /// in the shape it goes on the wire.
    ///
    /// One field rather than the `tile_px` / `scale` / `&Theme` triple
    /// it replaced, because those three were the same three numbers a
    /// dockapp is told in `Welcome` and `ThemeChanged` and nothing
    /// guaranteed the two agreed. The tile's geometry check
    /// ([`RemoteTile::on_frame`]) and the message that tells a dockapp
    /// which geometry to draw at now read the same value, so "the dock
    /// relaid out but the dockapp was never told" is not a state that
    /// can be constructed.
    pub theme: &'a ThemeState,
    /// Where dockapps connect. Passed rather than recomputed so every
    /// launched child is told the same path the listener is actually
    /// bound to.
    pub socket_path: &'a PathBuf,
    /// One shared receive buffer for the whole pass, sized
    /// [`chonk_dock_proto::MAX_MESSAGE_BYTES`]. Shared because
    /// allocating a quarter-megabyte per tile per frame would be a
    /// worse use of the repaint thread than anything a dockapp could do
    /// to it.
    pub scratch: &'a mut Vec<u8>,
    /// The largest panel *content* area the workarea beside the dock
    /// can hold right now, in device pixels — what an `OpenPanel`
    /// request is clamped against. From the servicing context for the
    /// same reason `theme` is: a monitor change moves it, and a value
    /// recomputed per pass cannot be the one nobody updated.
    pub panel_bounds: (u32, u32),
}

/// One out-of-process dock tile.
pub(crate) struct RemoteTile {
    entry: DockappEntry,
    state: TileState,
    connection: Option<Connection>,
    /// The nonce whichever process currently owns this slot must present
    /// in its `Hello`.
    ///
    /// Written in exactly two places, which between them are the whole
    /// of the tile's identity story: [`launch`](Self::launch) mints a
    /// fresh one per launch, so a stale process from a previous launch
    /// cannot reclaim the slot from the one that replaced it; and
    /// [`rejoin`](Self::rejoin) inherits one from the previous *shell*
    /// (see [`super::handoff`]), which is what lets a survivor of a hot
    /// restart be readopted instead of replaced.
    ///
    /// All zeroes until one of those two runs, and that value is never
    /// presentable: `awaiting_hello` admits only
    /// [`TileState::Starting`] and [`TileState::Rejoining`], and the
    /// only paths into those states are the two writers above.
    token: [u8; TOKEN_BYTES],
    child: Option<SpawnedChild>,
    budget: LaunchBudget,
    /// The last frame the dock is entitled to draw. Kept across a
    /// disconnect on purpose: it is what the hung face dims, and what
    /// the tile shows for the instant between a crash and its restart.
    last_frame: Option<DecorationBuffer>,
    /// Rate limit on *incoming* frames, coalescing rather than queuing:
    /// a dockapp drawing at 200Hz costs one memcpy per repaint, not
    /// two hundred.
    limiter: FrameLimiter<DecorationBuffer>,
    /// Geometry the tile is currently sized for. A frame that does not
    /// match exactly is rejected, never scaled — see
    /// [`RemoteTile::on_frame`].
    tile_px: u32,
    /// `render` would now produce different pixels.
    dirty: bool,
    /// Whether the pointer is inside this tile, so `Enter`/`Leave` are
    /// each sent once per crossing rather than once per motion event.
    hovered: bool,
    /// The open instrument panel, if this dockapp has one. At most one
    /// per dockapp by construction (an `OpenPanel` renegotiates it in
    /// place); at most one desktop-wide by the desktop's arbitration
    /// (`Desktop::sync_instrument_panel`).
    panel: Option<PanelState>,
}

impl RemoteTile {
    pub(crate) fn new(entry: DockappEntry, tile_px: u32, now: Instant) -> Self {
        Self {
            entry,
            // Due immediately: the first launch is not a retry, and
            // making the dock wait a second for its own tiles at
            // startup would be a backoff applied to the one attempt
            // that has not failed yet.
            state: TileState::Waiting { until: now },
            connection: None,
            token: [0u8; TOKEN_BYTES],
            child: None,
            budget: LaunchBudget::new(),
            last_frame: None,
            limiter: FrameLimiter::new(now),
            tile_px,
            dirty: true,
            hovered: false,
            panel: None,
        }
    }

    pub(crate) fn id(&self) -> &str {
        &self.entry.id
    }

    pub(crate) fn entry(&self) -> &DockappEntry {
        &self.entry
    }

    pub(crate) fn state(&self) -> TileState {
        self.state
    }

    pub(crate) fn token(&self) -> &[u8; TOKEN_BYTES] {
        &self.token
    }

    pub(crate) fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(SpawnedChild::pid)
    }

    /// The fd the event loop must poll for this tile, if it has one.
    ///
    /// This is the whole of the backend-specific work the dockapp
    /// boundary costs: the X11 binary adds it to its `poll` set, the
    /// Wayland binary wraps it in a calloop `Generic`. Everything else
    /// about a dockapp is identical on both stacks, because a dockapp
    /// is not a display-server client at all.
    pub(crate) fn poll_fd(&self) -> Option<RawFd> {
        self.connection.as_ref().map(|c| c.socket.as_raw_fd())
    }

    /// Everything [`launch`](Self::launch) does to this tile's own state
    /// except the `fork`/`exec`.
    ///
    /// Exists so the restart-survival tests can stand a tile in the
    /// state a launched one is in — token minted, awaiting a `Hello` —
    /// and then drive a real socket against it, without a real dockapp
    /// binary in the loop. Deliberately mirrors `launch` line for line;
    /// if that ever does more to `self` than this does, this is wrong.
    #[cfg(test)]
    pub(crate) fn pretend_launched(&mut self, token: [u8; TOKEN_BYTES], now: Instant) {
        self.token = token;
        self.state = TileState::Starting { since: now };
        self.dirty = true;
    }

    // -----------------------------------------------------------------
    // Handshake
    // -----------------------------------------------------------------

    /// Whether this tile is waiting for a `Hello` — i.e. whether an
    /// incoming connection presenting this id should be considered at
    /// all.
    /// # The adoption rule, and the token
    ///
    /// The set is deliberately narrow: a tile admits a `Hello` only
    /// while it is *expecting* one and has no connection. `Starting`
    /// means this shell launched a process that has not spoken yet;
    /// `Rejoining` means the previous shell did, and handed this one the
    /// token so the survivor can be readopted rather than replaced.
    ///
    /// What this does **not** widen is how many connections a tile has.
    /// A `Hello` for an id that is already connected is still refused
    /// with `Replaced`, *even when it presents a valid token*, and that
    /// is the deliberate call:
    ///
    /// * A valid token proves "you were launched for this slot". It does
    ///   not prove "you are the process currently drawing it". The token
    ///   is a shared secret handed over in the environment, so a forked
    ///   copy of the dockapp, or any process of this user that read
    ///   `/proc/<pid>/environ`, holds one just as good.
    /// * The failure mode of the alternative is worse. Displacing on a
    ///   token match would let a second instance of a dockapp — a user
    ///   starting it by hand, a `.dockapp` whose `exec` forks — silently
    ///   steal a working tile, and would give anything that can read the
    ///   token a takeover at a moment of its choosing.
    /// * Refusing costs the loser almost nothing. The genuine reconnect
    ///   case never collides, because socket EOF is instant and
    ///   definitive: by the time a survivor's `Hello` arrives, the tile
    ///   it wants has already seen its predecessor go. A dockapp refused
    ///   here is one whose slot really is occupied, and it exits.
    pub(crate) fn awaiting_hello(&self) -> bool {
        matches!(self.state, TileState::Starting { .. } | TileState::Rejoining { .. }) && self.connection.is_none()
    }

    /// Inherits a token from the previous shell and holds this tile's
    /// slot open for the process that already has it.
    ///
    /// Called at startup for every registered id that appears in the
    /// handoff file (see [`super::handoff`]). Nothing is launched while
    /// the tile is [`TileState::Rejoining`]; if nobody claims the slot
    /// within [`REJOIN_WINDOW`] the tile falls back to a normal launch,
    /// so the worst case of a handoff for a process that died in the gap
    /// is a tile that appears a few seconds late.
    pub(crate) fn rejoin(&mut self, token: [u8; TOKEN_BYTES], now: Instant) {
        tracing::info!(id = %self.entry.id, "holding a dock slot open for a dockapp that survived the shell restart");
        self.token = token;
        self.state = TileState::Rejoining { until: now + REJOIN_WINDOW };
        self.dirty = true;
    }

    /// Lets go of a running dockapp *without* killing it, so it can be
    /// readopted by the shell that replaces this one. Returns the token
    /// the replacement will need, or `None` if there is nothing worth
    /// handing over.
    ///
    /// Three deliberate omissions, each of which would break the
    /// handoff if it were here:
    ///
    /// * No `Goodbye`. The reason codes are actionable on the far side —
    ///   `Shutdown` means "reconnecting is pointless" — and a bare EOF
    ///   means "try again", which is exactly the instruction. This is
    ///   the one place the shell deliberately says nothing.
    /// * No `terminate`. The whole point is that the process outlives
    ///   this shell.
    /// * No token re-mint. The survivor will present the one it was
    ///   given at launch.
    ///
    /// A [`TileState::Hung`] tile is *not* handed over. It has stopped
    /// running its own event loop, so it will not notice the EOF, will
    /// never reconnect, and nothing would ever collect it — an orphan
    /// for the rest of the login session. It gets the ordinary shutdown
    /// instead.
    pub(crate) fn hand_off(&mut self) -> Option<(String, [u8; TOKEN_BYTES])> {
        let worth_keeping = matches!(self.state, TileState::Starting { .. } | TileState::Live | TileState::Rejoining { .. });
        if !worth_keeping {
            return None;
        }
        self.connection = None;
        // The panel does not survive the restart: the incoming shell
        // inherits the token, not the panel state, and the survivor's
        // reconnect starts panel-less by contract. Dropped silently for
        // the same reason no `Goodbye` is sent — the bare EOF is the
        // whole message here.
        self.panel = None;
        // Dropped, not terminated. `SpawnedChild` has no `Drop` of its
        // own — killing is `terminate()`, which is exactly what is not
        // being called here — so letting go of the handle leaves the
        // process running. It was spawned detached to begin with, so it
        // simply reparents to init like any other detached child when
        // this shell `exec`s away.
        self.child = None;
        self.dirty = true;
        tracing::info!(id = %self.entry.id, "leaving a dockapp running across the shell restart");
        Some((self.entry.id.clone(), self.token))
    }

    /// Accepts an authenticated connection. The caller has already run
    /// `chonk_dock_proto::validate_hello` against
    /// [`token`](Self::token); `client_proto` is the version that
    /// `Hello` announced (`Accepted::proto`), which decides what the
    /// `Welcome` may carry in its formerly-reserved `proto` field —
    /// see [`Connection::proto`].
    pub(crate) fn adopt(&mut self, socket: Seqpacket, wants: InputMask, client_proto: u32, welcome: ThemeState, now: Instant) {
        let mut connection = Connection {
            socket,
            send: SendQueue::new(),
            wants,
            ping_seq: 0,
            last_ping: now,
            unanswered: 0,
            since: now,
            told: welcome.clone(),
            proto: client_proto,
        };
        // Queued, not sent inline, for exactly the reason every other
        // send is queued — see `flush`. The first message is not an
        // exception worth carving out, and a `Welcome` that blocked
        // would be a frozen desktop at the moment a tile appears.
        //
        // `for_client` is the incident gate: to a version-1 Hello this
        // Welcome must be byte-identical to a protocol-1 shell's,
        // reserved zeros included, or a strict v1 decoder dies at the
        // handshake and crash-loops into the crash brake.
        for message in [ServerMessage::Welcome(welcome.for_client(client_proto)), ServerMessage::Visibility { visible: true }] {
            if let Ok(bytes) = message.encode() {
                let _ = connection.send.push(bytes, now);
            }
        }
        self.connection = Some(connection);
        // Connected but not yet drawn: the tile stays on its starting
        // face until a frame arrives, because an empty square is a
        // worse answer than a labelled dead one.
        self.state = TileState::Starting { since: now };
        self.dirty = true;
        tracing::info!(id = %self.entry.id, pid = ?self.pid(), "dockapp connected");
    }

    // -----------------------------------------------------------------
    // One servicing pass
    // -----------------------------------------------------------------

    /// Reads whatever arrived, checks liveness, flushes what is queued,
    /// and launches if a backoff has expired.
    ///
    /// Called once per event-loop iteration for every remote tile.
    /// Nothing in it can block: see the module docs.
    pub(crate) fn service(&mut self, ctx: &mut ServiceContext) {
        // The dock's current tile edge, applied before anything is
        // read. `resize_to_screen` can change it mid-life (a monitor
        // change, a scale change) and a dockapp learns one round trip
        // later, so frames sized for the old geometry are in flight
        // exactly when this matters. Taking the answer from the
        // servicing context on every pass — rather than from a
        // notification the relayout has to remember to send — means
        // `on_frame`'s equality check can never be comparing against a
        // number nobody updated.
        self.set_tile_px(ctx.theme.tile_px);
        self.receive(ctx);
        self.push_theme(ctx);
        self.check_liveness(ctx.now);
        // A frame parked by the rate limiter becomes visible on the
        // pass after its token refills. `next_ready_in` exists for a
        // loop that wants to size its poll timeout; this one already
        // wakes every 16ms, which is finer than the 33ms the limiter
        // meters at, so calling it would buy nothing.
        if let Some(frame) = self.limiter.take_ready(ctx.now) {
            self.last_frame = Some(frame);
            self.dirty = true;
        }
        self.flush(ctx.now);
        self.maybe_launch(ctx);
    }

    /// Drains the socket. Every `recv` is `MSG_DONTWAIT`; the loop ends
    /// on `WouldBlock`, which is the normal exit.
    fn receive(&mut self, ctx: &mut ServiceContext) {
        if self.connection.is_none() {
            return;
        }
        if ctx.scratch.len() < chonk_dock_proto::MAX_MESSAGE_BYTES {
            ctx.scratch.resize(chonk_dock_proto::MAX_MESSAGE_BYTES, 0);
        }
        loop {
            // The socket borrow is confined to this statement so the
            // handling below can take `&mut self` — a message can end
            // the connection, and the borrow checker is right to insist
            // the reader has let go before that happens.
            let read = match self.connection.as_ref() {
                Some(connection) => connection.socket.recv(ctx.scratch),
                None => return,
            };
            match read {
                Ok(0) => {
                    // EOF: the peer is gone. Instant and definitive,
                    // and it arrives on the same event-loop pass as the
                    // process's death — which is why the socket, not
                    // `waitpid`, is the primary crash signal.
                    self.disconnected(ctx.now, "the dockapp closed its connection");
                    return;
                }
                Ok(n) => match ClientMessage::decode(&ctx.scratch[..n]) {
                    Ok(message) => {
                        if !self.handle(message, ctx) {
                            return;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(id = %self.entry.id, %error, "dockapp sent an undecodable message");
                        self.stop_connection(GoodbyeReason::ProtocolError);
                        self.disconnected(ctx.now, "protocol error");
                        return;
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    tracing::warn!(id = %self.entry.id, ?error, "dockapp socket failed");
                    self.disconnected(ctx.now, "socket error");
                    return;
                }
            }
        }
    }

    /// Returns whether the connection survived this message.
    fn handle(&mut self, message: ClientMessage, ctx: &ServiceContext) -> bool {
        // The panel family exists only for a connection whose `Hello`
        // announced protocol 2 or newer. A client that said 1 was told
        // `proto` 0 in its `Welcome` — the field its protocol declared
        // reserved — so it has been told, in its own wire dialect, that
        // panels do not exist here; sending one anyway is the same
        // protocol error the document has always promised for an
        // unknown kind on a v1 connection.
        let panel_family = matches!(
            message,
            ClientMessage::OpenPanel { .. } | ClientMessage::PanelFrame { .. } | ClientMessage::ClosePanel
        );
        if panel_family && self.connection.as_ref().is_some_and(|connection| connection.proto < 2) {
            tracing::warn!(id = %self.entry.id, "a protocol-1 dockapp sent an instrument-panel message");
            self.stop_connection(GoodbyeReason::ProtocolError);
            self.disconnected(ctx.now, "panel message from a protocol-1 client");
            return false;
        }
        match message {
            ClientMessage::Frame { generation, width, height, pixels } => self.on_frame(generation, width, height, pixels, ctx),
            ClientMessage::Pong { seq } => {
                if let Some(connection) = self.connection.as_mut() {
                    // Any pong clears the count, not just the newest
                    // one's: the question being asked is "is your event
                    // loop running", and any answer at all settles it.
                    // Comparing sequence numbers would add a way to be
                    // wrong about a dockapp that is demonstrably alive.
                    let _ = seq;
                    connection.unanswered = 0;
                }
                if let TileState::Hung { since } = self.state {
                    tracing::info!(id = %self.entry.id, hung_for = ?ctx.now.saturating_duration_since(since), "dockapp is answering again");
                    self.state = TileState::Live;
                    self.dirty = true;
                }
                true
            }
            ClientMessage::Log { level, text } => {
                // Already bounded and control-stripped by the codec;
                // logged under the dockapp's id so a noisy tile is
                // attributable. Never rendered — a dockapp's log is not
                // a channel into the shell's own chrome.
                match level {
                    LogLevel::Error => tracing::error!(dockapp = %self.entry.id, "{text}"),
                    LogLevel::Warn => tracing::warn!(dockapp = %self.entry.id, "{text}"),
                    LogLevel::Info => tracing::info!(dockapp = %self.entry.id, "{text}"),
                    LogLevel::Debug => tracing::debug!(dockapp = %self.entry.id, "{text}"),
                }
                true
            }
            ClientMessage::Hello { .. } => {
                // A second `Hello` on an established connection. Not a
                // renegotiation — the protocol has no such thing — so
                // it is either a confused client or one probing for
                // one.
                tracing::warn!(id = %self.entry.id, "dockapp sent a second Hello on an open connection");
                self.stop_connection(GoodbyeReason::ProtocolError);
                self.disconnected(ctx.now, "duplicate Hello");
                false
            }
            ClientMessage::OpenPanel { width, height } => {
                self.on_open_panel(width, height, ctx);
                true
            }
            ClientMessage::PanelFrame { generation, y, band_height, width, pixels } => {
                self.on_panel_frame(generation, y, band_height, width, pixels, ctx)
            }
            ClientMessage::ClosePanel => {
                // A `ClosePanel` racing a dismissal the client has not
                // seen yet is expected traffic, not an error: with no
                // panel open it is silently nothing.
                if self.panel.is_some() {
                    self.close_panel(PanelCloseReason::ClientRequest, ctx.now);
                }
                true
            }
        }
    }

    /// An `OpenPanel` arrived: grant a clamped size, or refuse.
    ///
    /// A request while a panel is already open re-negotiates in place —
    /// same connection, same panel slot, a fresh grant. The stored
    /// frame survives only if the new grant is the same size; a frame
    /// for the old grant is exactly the wrong-sized blit the equality
    /// check exists to refuse.
    fn on_open_panel(&mut self, width: u32, height: u32, ctx: &ServiceContext) {
        match clamp_panel_grant(width, height, ctx.panel_bounds) {
            Some(granted) => {
                // Renegotiation keeps the streamed pixels only when the
                // grant is byte-compatible; a buffer for the old grant
                // is exactly the wrong-sized blit the band checks
                // refuse.
                let kept = self.panel.take().filter(|panel| panel.granted == granted);
                tracing::info!(
                    id = %self.entry.id,
                    asked = format!("{width}x{height}"),
                    granted = format!("{}x{}", granted.0, granted.1),
                    "opening an instrument panel"
                );
                self.panel = Some(match kept {
                    Some(mut panel) => {
                        panel.just_opened = true;
                        panel.dirty = true;
                        panel
                    }
                    None => PanelState {
                        granted,
                        buffer: DecorationBuffer {
                            width: granted.0,
                            height: granted.1,
                            // Transparent until streamed: the chrome's
                            // well shows through unpainted regions.
                            pixels: vec![0; (granted.0 as usize) * (granted.1 as usize) * 4],
                        },
                        streamed: false,
                        newest_generation: 0,
                        dirty: true,
                        last_present: None,
                        just_opened: true,
                    },
                });
                self.enqueue(ServerMessage::PanelOpened { width: granted.0, height: granted.1 }, ctx.now);
                self.flush(ctx.now);
            }
            None => {
                // Nothing sensible can be granted — a degenerate
                // request or a workarea with no room beside the dock.
                tracing::warn!(id = %self.entry.id, asked = format!("{width}x{height}"), "refusing an instrument panel request");
                self.enqueue(ServerMessage::PanelClosed { reason: PanelCloseReason::Refused }, ctx.now);
                self.flush(ctx.now);
            }
        }
    }

    /// One panel band arrived. Same reject-don't-rescale rule as
    /// [`on_frame`](Self::on_frame): the band's width is compared
    /// against the grant by equality and its rows against the granted
    /// height, a mismatch is logged and discarded, and the connection
    /// stays up — a band drawn against a superseded grant is in flight
    /// at exactly the moment a renegotiation lands.
    ///
    /// A band with no panel open is also silently dropped: the client
    /// may legitimately still be streaming against a `PanelClosed` it
    /// has not read yet, and punishing that race would make every
    /// dismissal a coin-flip disconnection.
    fn on_panel_frame(&mut self, generation: u32, y: u32, band_height: u32, width: u32, pixels: Vec<u8>, ctx: &ServiceContext) -> bool {
        let Some(panel) = self.panel.as_mut() else {
            tracing::debug!(id = %self.entry.id, "dropping a panel band for a panel that is no longer open");
            return true;
        };
        if width != panel.granted.0 || (y as u64) + (band_height as u64) > panel.granted.1 as u64 {
            tracing::warn!(
                id = %self.entry.id,
                generation,
                got = format!("rows {y}..{} at width {width}", y as u64 + band_height as u64),
                want = format!("{}x{}", panel.granted.0, panel.granted.1),
                "rejecting a panel band outside the granted geometry"
            );
            return true;
        }
        // The codec already guarantees the length; asserted here at the
        // one place a wrong answer would blit out of bounds, exactly as
        // the tile's frame path does.
        if pixels.len() != (width as usize) * (band_height as usize) * 4 {
            tracing::warn!(id = %self.entry.id, "rejecting a panel band whose payload does not match its header");
            self.stop_connection(GoodbyeReason::ProtocolError);
            self.disconnected(ctx.now, "malformed panel band");
            return false;
        }
        // The contract's flow-control drop: a band from an older
        // repaint than the newest seen may be dropped, and is — the
        // newest repaint will cover those rows again anyway, and
        // blitting stale rows over fresh ones would repaint backwards.
        if generation_newer(panel.newest_generation, generation) {
            tracing::debug!(id = %self.entry.id, generation, newest = panel.newest_generation, "dropping a stale-generation panel band");
            return true;
        }
        panel.newest_generation = generation;
        // Bands are full-width, so one band is one contiguous memcpy.
        let offset = (y as usize) * (width as usize) * 4;
        panel.buffer.pixels[offset..offset + pixels.len()].copy_from_slice(&pixels);
        panel.streamed = true;
        panel.dirty = true;
        true
    }

    /// A frame arrived. The geometry check is an equality test and the
    /// failure is a disconnect, not a rescale.
    ///
    /// `resize_to_screen` can change the tile size mid-life (a monitor
    /// change, a scale change), and a dockapp learns about it one round
    /// trip later. Scaling an old-size frame to fit would draw a
    /// blurred, subtly wrong tile that looks like a rendering bug in
    /// the dockapp; clamping would draw a cropped one. Rejecting says
    /// exactly what happened, keeps the last good frame on screen, and
    /// costs one frame at a moment when the whole screen is
    /// relaying out anyway.
    fn on_frame(&mut self, generation: u32, width: u32, height: u32, pixels: Vec<u8>, ctx: &ServiceContext) -> bool {
        if !frame_matches_tile(width, height, self.tile_px, self.entry.tile_units) {
            tracing::warn!(
                id = %self.entry.id,
                generation,
                got = format!("{width}x{height}"),
                want = format!("{}x{}", self.tile_px, self.tile_px * self.entry.tile_units as u32),
                "rejecting a dockapp frame of the wrong size"
            );
            return true;
        }
        // The codec already guarantees `pixels.len() == width * height
        // * 4`; this is the assertion that the guarantee is the one
        // being relied on, at the point where a wrong answer would blit
        // out of bounds.
        if pixels.len() != (width as usize) * (height as usize) * 4 {
            tracing::warn!(id = %self.entry.id, "rejecting a dockapp frame whose payload does not match its header");
            self.stop_connection(GoodbyeReason::ProtocolError);
            self.disconnected(ctx.now, "malformed frame");
            return false;
        }
        let buffer = DecorationBuffer { width, height, pixels };
        if let Some(ready) = self.limiter.offer(buffer, ctx.now) {
            self.last_frame = Some(ready);
            self.dirty = true;
        }
        // The first frame is what turns "starting" into a tile.
        if matches!(self.state, TileState::Starting { .. }) {
            self.state = TileState::Live;
            self.dirty = true;
        }
        true
    }

    /// Ping, and decide whether silence has gone on long enough to tell
    /// the user.
    fn check_liveness(&mut self, now: Instant) {
        let Some(connection) = self.connection.as_mut() else { return };
        if now.saturating_duration_since(connection.last_ping) < PING_INTERVAL {
            return;
        }
        connection.last_ping = now;
        connection.ping_seq = connection.ping_seq.wrapping_add(1);
        connection.unanswered = connection.unanswered.saturating_add(1);
        let seq = connection.ping_seq;
        let unanswered = connection.unanswered;
        self.enqueue(ServerMessage::Ping { seq }, now);

        // Note what is *not* here: any waiting. The ping is queued and
        // the answer is read on some later pass, or never. A hung
        // dockapp costs this function one `encode` and one non-blocking
        // `send` every two seconds, forever, and costs the compositor
        // nothing else at all.
        if unanswered >= UNANSWERED_PINGS_BEFORE_HUNG && !matches!(self.state, TileState::Hung { .. }) {
            tracing::warn!(
                id = %self.entry.id,
                pid = ?self.pid(),
                unanswered,
                "dockapp stopped answering; dimming its tile so a stale reading is not shown as a live one"
            );
            self.state = TileState::Hung { since: now };
            self.dirty = true;
            // The crash-isolation invariant, panel edition: a hung
            // instrument's panel dies by the same ping machinery as its
            // tile. The tile stays (dimmed, informative); the panel is a
            // transient detail view and a frozen one is a stale reading
            // at ten times the size, so it comes down. `Shutdown` on the
            // wire — the process is not being asked to reopen it — and
            // the message is a courtesy to a peer that has, by
            // definition, stopped reading.
            if self.panel.is_some() {
                self.close_panel(PanelCloseReason::Shutdown, now);
            }
        }
    }

    /// Tells a connected dockapp that its geometry or its palette
    /// changed, so it restyles in place.
    ///
    /// **A dockapp never restarts for a theme change.** That is the
    /// whole reason `ThemeChanged` exists rather than the shell killing
    /// and relaunching the process: a theme pick is the most routine
    /// thing a user does to this desktop, and a tile that lost its
    /// in-app state (and cost a fork, an exec and a fontconfig scan)
    /// every time one happened would be a worse tile than a built-in.
    /// The SDK's half is already there — `chonk_ui::dockapp::serve`
    /// returns `Outcome::Retheme`, rebuilding its `Ctx` and its pixmap
    /// on the *same* socket.
    ///
    /// # Why this is polled rather than notified
    ///
    /// There is no "the theme changed" call site to keep in sync. The
    /// state is recomputed once per servicing pass and compared, for
    /// the same reason [`set_tile_px`](Self::set_tile_px) takes its
    /// answer from the context: the triggers are diffuse — a theme pick,
    /// a scale change, and `Desktop::resize_to_screen` changing the tile
    /// edge when a monitor is plugged in — and a notification is
    /// something a future fourth trigger can forget to send. A
    /// comparison cannot be forgotten. The cost is one `ThemeState`
    /// comparison per tile per pass, which is four integer/string
    /// compares against a value `DockHost` has already cached; the
    /// expensive part (serializing the theme to TOML) happens once, in
    /// `DockHost::refresh_theme`, and only when the theme actually
    /// differs.
    ///
    /// # Why `same_as` and not `!=`
    ///
    /// `ThemeState` derives `PartialEq` over an `f32`, so `!=` against a
    /// NaN scale is *always* true and this function would push a message
    /// on every pass forever — filling the send queue at the repaint
    /// rate until the tile was disconnected for overflow. The codec now
    /// refuses to decode such a scale, but the shell *constructs* this
    /// value rather than decoding it, so that guard does not cover this
    /// side. [`ThemeState::same_as`] compares the bits and is therefore
    /// reflexive whatever the float is; `a_theme_state_the_shell_cannot_
    /// compare_is_pushed_once_not_forever` pins it.
    fn push_theme(&mut self, ctx: &ServiceContext) {
        let Some(connection) = self.connection.as_ref() else { return };
        if connection.told.same_as(ctx.theme) {
            return;
        }
        let next = ctx.theme.clone();
        tracing::debug!(id = %self.entry.id, tile_px = next.tile_px, theme = %next.theme_id, "telling a dockapp to restyle in place");
        // Recorded before the enqueue can fail and *whatever* it does
        // with the message. The invariant that matters is "we do not ask
        // again until the answer changes again": a dockapp whose queue
        // was full and dropped this one gets the next change, not a
        // retry storm of this one. `enqueue` may itself disconnect on
        // sustained overflow, which takes the connection (and this
        // record) with it — which is also correct, since a reconnect is
        // told everything again in its `Welcome`.
        if let Some(connection) = self.connection.as_mut() {
            connection.told = next.clone();
        }
        // Masked per connection at the send, exactly as the `Welcome`
        // was: a version-1 client must never see a nonzero value in the
        // u16 its protocol declared reserved. `told` above stays
        // unmasked so the change comparison keeps comparing like with
        // like.
        let client_proto = self.connection.as_ref().map_or(0, |connection| connection.proto);
        self.enqueue(ServerMessage::ThemeChanged(next.for_client(client_proto)), ctx.now);
    }

    /// Sends whatever the socket will take.
    ///
    /// # The bug this shape exists to avoid
    ///
    /// A blocking `write()` to a dockapp that has stopped calling
    /// `recv()` parks the caller until it starts again, which may be
    /// never. On the shell's side that caller is the compositor's
    /// single repaint thread: the desktop would stop drawing, stop
    /// reading input, and stop collecting page-flip completions, and
    /// the stall watchdog would blame the display driver — which is
    /// precisely the incident this whole design was written after, with
    /// `send()` substituted for `nmcli`. Backpressure done wrong *is*
    /// the original bug with a different syscall.
    ///
    /// Three things make that unreachable, and none of them is
    /// discipline: the socket is `O_NONBLOCK` from `accept4` so there
    /// is not an instant where it isn't, every send additionally passes
    /// `MSG_DONTWAIT` so the property does not depend on a flag
    /// surviving, and [`SendQueue`] is bounded so "peer stopped
    /// reading" cannot become "compositor allocates until the OOM
    /// killer picks a winner".
    fn flush(&mut self, now: Instant) {
        let Some(connection) = self.connection.as_mut() else { return };
        let result = connection.send.flush(|message| connection.socket.send(message));
        if let Err(error) = result {
            tracing::warn!(id = %self.entry.id, ?error, "dockapp send failed");
            self.disconnected(now, "send failed");
        }
    }

    /// Queues one message. Never sends inline — see [`flush`](Self::flush).
    fn enqueue(&mut self, message: ServerMessage, now: Instant) {
        let Some(connection) = self.connection.as_mut() else { return };
        let bytes = match message.encode() {
            Ok(bytes) => bytes,
            Err(error) => {
                // The shell generates every one of these, so an encode
                // failure is a shell bug, not a dockapp one. Log it
                // against the shell and drop the message.
                tracing::error!(id = %self.entry.id, %error, "the shell failed to encode a message for a dockapp");
                return;
            }
        };
        match connection.send.push(bytes, now) {
            SendOutcome::Queued => {}
            SendOutcome::DroppedOldest => {
                tracing::debug!(id = %self.entry.id, "dockapp is not keeping up; dropped its oldest queued message");
            }
            SendOutcome::Disconnect => {
                tracing::warn!(id = %self.entry.id, dropped = connection.send.dropped(), "dockapp has not read anything for seconds; disconnecting it");
                self.stop_connection(GoodbyeReason::Overflow);
                self.disconnected(now, "sustained send overflow");
            }
        }
    }

    // -----------------------------------------------------------------
    // Lifecycle
    // -----------------------------------------------------------------

    /// Best-effort `Goodbye`, then drop the socket.
    ///
    /// Best-effort in the strict sense: it is pushed through the same
    /// non-blocking send as everything else and is *expected* to fail
    /// for the peer this most matters to, since a dockapp that filled
    /// the queue is by definition not reading this either. It costs one
    /// syscall and occasionally saves a dockapp author a confusing bare
    /// EOF.
    fn stop_connection(&mut self, reason: GoodbyeReason) {
        let Some(connection) = self.connection.as_mut() else { return };
        if let Ok(bytes) = (ServerMessage::Goodbye { reason }).encode() {
            let _ = connection.socket.send(&bytes);
        }
    }

    /// The connection is gone. Decide whether, and when, to try again.
    fn disconnected(&mut self, now: Instant, why: &str) {
        let stable = self
            .connection
            .as_ref()
            .is_some_and(|connection| now.saturating_duration_since(connection.since) >= CRASH_LOOP_WINDOW);
        self.connection = None;
        // A dead dockapp's panel is torn down with it. There is nobody
        // left to send `PanelClosed { Shutdown }` to — the EOF that
        // brought us here is the same fact — so the state is simply
        // dropped, and the desktop unmaps the surface on its next pass.
        self.panel = None;
        self.dirty = true;

        // Three crash signals, and this is where the second one is
        // read. The socket EOF that brought us here says *that* the
        // process is gone; `SpawnedChild::exited` says *how* — which is
        // the only thing that distinguishes a dockapp that finished
        // from one that died, and therefore the only thing
        // `restart = "on-crash"` can act on. It never blocks: the wait
        // happens on the per-child reaper thread `spawn` already
        // creates, and this reads the answer it left behind.
        let report = self.child.as_ref().and_then(SpawnedChild::exited);
        let exited_cleanly = report.is_some_and(|report| report.is_success());
        if let Some(child) = self.child.take() {
            // Still running with its socket closed: it is not coming
            // back into the dock (its token is spent) and nothing else
            // will ever collect it. Ask it to leave before its
            // replacement is launched, or the user pays for two.
            if report.is_none() {
                child.terminate();
            }
        }

        let fate = match self.entry.restart {
            RestartPolicy::Never => Some(StopReason::PolicyNever),
            RestartPolicy::OnCrash if exited_cleanly => Some(StopReason::CleanExit),
            _ => None,
        };
        if let Some(reason) = fate {
            tracing::info!(id = %self.entry.id, why, ?reason, "dockapp will not be relaunched");
            self.state = TileState::Stopped { reason };
            return;
        }

        match self.budget.record_failure(now, stable) {
            Fate::Retry { at } => {
                tracing::info!(
                    id = %self.entry.id,
                    why,
                    exited_cleanly,
                    retry_in = ?at.saturating_duration_since(now),
                    failures = self.budget.recent_failures(),
                    "dockapp went away; backing off before relaunching"
                );
                self.state = TileState::Waiting { until: at };
            }
            Fate::CrashLooped => {
                tracing::error!(
                    id = %self.entry.id,
                    why,
                    failures = LaunchBudget::MAX_FAILURES,
                    window = ?CRASH_LOOP_WINDOW,
                    "dockapp crash-looped; it will not be launched again this session. \
                     A dockapp restarted forever is an invisible fork bomb — fix it and pick \
                     Restart from its tile menu."
                );
                self.state = TileState::Stopped { reason: StopReason::CrashLooped };
            }
        }
    }

    /// Launches the process if a backoff has expired, and gives up on
    /// one that was launched but never said `Hello`.
    fn maybe_launch(&mut self, ctx: &ServiceContext) {
        match self.state {
            TileState::Waiting { until } if ctx.now >= until => self.launch(ctx),
            TileState::Starting { since } if self.connection.is_none() && ctx.now.saturating_duration_since(since) >= HANDSHAKE_GRACE => {
                self.disconnected(ctx.now, "launched but never completed a handshake");
            }
            // The handed-off process never came back. Not a failure —
            // nothing crashed, and the likeliest cause is that it had
            // already exited before the restart — so this goes straight
            // to a launch rather than through `disconnected`, which
            // would spend one of the five failures the crash-loop cutoff
            // counts.
            TileState::Rejoining { until } if ctx.now >= until => {
                tracing::info!(id = %self.entry.id, waited = ?REJOIN_WINDOW, "no dockapp reclaimed this slot after the restart; launching a fresh one");
                self.launch(ctx);
            }
            _ => {}
        }
    }

    /// Spawns the dockapp's process.
    ///
    /// The environment is the security boundary, and two halves of it
    /// matter:
    ///
    /// * What it gains: the socket path and a freshly minted 128-bit
    ///   token, plus the scale and theme so its first frame is drawn
    ///   correctly rather than redrawn after a round trip.
    /// * What it *loses*: `WAYLAND_DISPLAY` and `DISPLAY`, removed
    ///   rather than merely unset — see
    ///   [`DISPLAY_SERVER_ENV`](crate::spawn::DISPLAY_SERVER_ENV) — and
    ///   `CHONKSTEP_CONTROL_SOCKET` with them, for the reason
    ///   [`DOCKAPP_WITHHELD_ENV`] gives. This is mandatory, not tidy. The headline claim of the dockapp
    ///   boundary is that a dockapp holds no display connection, so
    ///   `wl_shm`, `zwlr_screencopy_v1` and
    ///   `zwlr_foreign_toplevel_management_v1` are *unreachable* rather
    ///   than denied — and that claim is simply false if the process
    ///   inherits a display to open for itself.
    fn launch(&mut self, ctx: &ServiceContext) {
        let token = match transport::mint_token() {
            Ok(token) => token,
            Err(error) => {
                // No token means no authentication, and launching
                // without one would admit any process on this machine
                // to the slot. Refusing is the only safe answer, and it
                // is a hard stop rather than a retry because a
                // `getrandom` that fails is not going to succeed in
                // eight seconds.
                tracing::error!(id = %self.entry.id, ?error, "cannot mint a dockapp token; refusing to launch it unauthenticated");
                self.state = TileState::Stopped { reason: StopReason::CrashLooped };
                return;
            }
        };
        self.token = token;

        let mut env = vec![
            (ENV_SOCKET.to_string(), ctx.socket_path.to_string_lossy().into_owned()),
            (ENV_TOKEN.to_string(), transport::token_to_hex(&token)),
            ("CHONKSTEP_SCALE".to_string(), format!("{:.4}", ctx.theme.scale)),
            ("CHONKSTEP_THEME".to_string(), ctx.theme.theme_id.clone()),
        ];
        // Like CHONKSTEP_THEME: only how a freshly spawned dockapp
        // learns the mood it starts in — a running one is pushed the
        // full resolved palette (appearance tag included) through
        // `ThemeChanged`. Read from the published state file, which the
        // shell wrote before any tile could launch; absent (never a
        // session's normal state) means say nothing rather than guess.
        if let Some(mode) = crate::appearance::load_published() {
            env.push(("CHONKSTEP_APPEARANCE".to_string(), mode.name().to_string()));
        }
        let args: Vec<&str> = self.entry.exec[1..].iter().map(String::as_str).collect();
        match spawn::spawn_supervised(&self.entry.exec[0], &args, &env, &DOCKAPP_WITHHELD_ENV) {
            Some(child) => {
                tracing::info!(id = %self.entry.id, pid = child.pid(), program = %self.entry.exec[0], "launched dockapp");
                self.child = Some(child);
                self.state = TileState::Starting { since: ctx.now };
            }
            None => {
                // The program is missing or not executable. That is a
                // failure like any other and goes through the same
                // budget: a `.dockapp` pointing at a binary that was
                // uninstalled would otherwise fork-and-fail forever.
                self.disconnected(ctx.now, "could not launch the dockapp's program");
            }
        }
        self.dirty = true;
    }

    /// The user asked for this tile to start again — a click on its
    /// dead face, or Restart from its menu.
    ///
    /// Resets the crash-loop budget, which nothing else does. The user
    /// is the only source of evidence the shell has that the *cause*
    /// might have changed since the last five failures.
    pub(crate) fn user_restart(&mut self, now: Instant) {
        tracing::info!(id = %self.entry.id, "user asked to restart a dockapp; clearing its crash-loop budget");
        if self.panel.is_some() {
            self.close_panel(PanelCloseReason::Shutdown, now);
        }
        self.stop_connection(GoodbyeReason::Removed);
        self.connection = None;
        if let Some(child) = self.child.take() {
            child.terminate();
        }
        self.budget.reset();
        self.state = TileState::Waiting { until: now };
        self.dirty = true;
    }

    /// The user removed the tile, or the session is ending. Stops the
    /// process and stays stopped.
    pub(crate) fn shut_down(&mut self, reason: GoodbyeReason) {
        if self.panel.is_some() {
            self.close_panel(PanelCloseReason::Shutdown, Instant::now());
        }
        self.stop_connection(reason);
        self.connection = None;
        if let Some(child) = self.child.take() {
            child.terminate();
        }
        self.state = TileState::Stopped { reason: StopReason::Removed };
        self.dirty = true;
    }

    /// The dock relaid out at a different tile size. Frames already in
    /// flight for the old size will be rejected by
    /// [`on_frame`](Self::on_frame); this is what makes that check
    /// notice.
    pub(crate) fn set_tile_px(&mut self, tile_px: u32) {
        if self.tile_px == tile_px {
            return;
        }
        self.tile_px = tile_px;
        // The stored frame is the wrong size now too, and drawing it
        // would be the same wrong-sized blit `on_frame` refuses. Drop
        // it: the tile shows its starting face for one round trip,
        // which is the correct amount of wrong.
        self.last_frame = None;
        self.dirty = true;
    }

    // -----------------------------------------------------------------
    // The instrument panel
    // -----------------------------------------------------------------

    /// Whether this dockapp has an open panel.
    pub(crate) fn panel_open(&self) -> bool {
        self.panel.is_some()
    }

    /// The size every panel frame must match — the last grant.
    pub(crate) fn panel_granted(&self) -> Option<(u32, u32)> {
        self.panel.as_ref().map(|panel| panel.granted)
    }

    /// The panel buffer the desktop is entitled to draw. `None` while
    /// the panel is open but no band has landed yet — the desktop shows
    /// the empty well, never a transparent buffer pretending to be
    /// content.
    pub(crate) fn panel_frame(&self) -> Option<&DecorationBuffer> {
        self.panel.as_ref().filter(|panel| panel.streamed).map(|panel| &panel.buffer)
    }

    /// True once per open/renegotiation: the desktop must (re)stage the
    /// panel surface. Take-semantics so one open is one staging.
    pub(crate) fn take_panel_just_opened(&mut self) -> bool {
        self.panel.as_mut().map(|panel| std::mem::take(&mut panel.just_opened)).unwrap_or(false)
    }

    /// Whether the desktop should present the panel buffer now: the
    /// pixels changed, and at least [`PANEL_PRESENT_INTERVAL`] has
    /// passed since the last present (the first present is immediate —
    /// the meter starts full, like the tile limiter's bucket).
    /// Take-semantics: answering `true` books the present.
    pub(crate) fn take_panel_ready(&mut self, now: Instant) -> bool {
        let Some(panel) = self.panel.as_mut() else { return false };
        if !panel.dirty {
            return false;
        }
        if panel.last_present.is_some_and(|at| now.saturating_duration_since(at) < PANEL_PRESENT_INTERVAL) {
            return false;
        }
        panel.dirty = false;
        panel.last_present = Some(now);
        true
    }

    /// Closes the panel and tells the client why. Idempotent: closing a
    /// panel that is not open is nothing. The message is queued *and*
    /// flushed (both non-blocking) so an immediately following
    /// connection teardown cannot strand it in the queue, and so it
    /// stays ordered behind any `PanelOpened` queued this same pass.
    pub(crate) fn close_panel(&mut self, reason: PanelCloseReason, now: Instant) {
        if self.panel.take().is_none() {
            return;
        }
        tracing::info!(id = %self.entry.id, ?reason, "closing an instrument panel");
        self.enqueue(ServerMessage::PanelClosed { reason }, now);
        self.flush(now);
    }

    /// One pointer event inside the panel, in panel device pixels.
    ///
    /// Deliberately not gated on the `Hello` input mask: the mask is a
    /// wake-avoidance hint for the *tile*, and a dockapp that asked for
    /// a panel has asked for its input. The dock's reserved-button
    /// policy still applies upstream — the shell routes only Left and
    /// Scroll here, exactly as for tiles.
    pub(crate) fn panel_input(&mut self, event: InputEvent, now: Instant) {
        if self.panel.is_none() || self.connection.is_none() {
            return;
        }
        self.enqueue(ServerMessage::PanelInput(event), now);
        self.flush(now);
    }
}

/// The tile face's fill, as one colour.
///
/// A gradient averages its endpoints, exactly as `wm_theme::tile`'s own
/// ink derivation and the menu renderer's `fill_average` do — the hung
/// blend is a wash toward "the colour of a tile", and a tile with a
/// gradient face has one of those even though it has no single fill.
fn tile_fill_colour(theme: &Theme) -> Color {
    match &theme.tile.fill {
        Fill::Solid(colour) => *colour,
        Fill::Gradient(gradient) => Color::rgb(
            ((gradient.from.r as u16 + gradient.to.r as u16) / 2) as u8,
            ((gradient.from.g as u16 + gradient.to.g as u16) / 2) as u8,
            ((gradient.from.b as u16 + gradient.to.b as u16) / 2) as u8,
        ),
    }
}

/// Blends `buffer` `mix`/255 of the way toward `toward`, in place.
///
/// Tiles are opaque, so premultiplied and straight RGBA agree and the
/// bytes can be walked directly — the same reasoning `paint::draw_text`
/// and `launchdock`'s `ghost_slot` already rely on.
fn wash_toward(buffer: &mut DecorationBuffer, toward: Color, mix: u16) {
    let blend = |channel: u8, target: u8| -> u8 { ((channel as u16 * (255 - mix) + target as u16 * mix) / 255) as u8 };
    for pixel in buffer.pixels.as_chunks_mut::<4>().0 {
        pixel[0] = blend(pixel[0], toward.r);
        pixel[1] = blend(pixel[1], toward.g);
        pixel[2] = blend(pixel[2], toward.b);
    }
}

/// How far the hung face is washed toward the tile fill, out of 255.
///
/// "~50%" from the spec, and the number is doing real work rather than
/// being a taste. Less and a frozen reading still looks live, which is
/// the failure this face exists to prevent — a clock stuck at 14:32 is
/// a lie the user acts on. More and the last-good frame stops being
/// readable at all, at which point a blank tile would have been
/// simpler and would have thrown away information the user often
/// wants: *what* it last said is frequently the clue to why it stopped.
const HUNG_WASH: u16 = 128;

/// A dead-screen face for a tile of this height, carrying `label`.
///
/// `panel::render_dead_tile` draws one square, which is the whole story
/// for a one-unit tile and most of it for a taller one: the dead screen
/// goes at the top where the eye lands, and the remaining units are
/// plain tile base, so a two-unit dockapp reads as one object that is
/// off rather than as a screen with a gap under it.
fn dead_face(theme: &Theme, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache, tile: u32, units: u32, label: &str) -> DecorationBuffer {
    let height = tile * units.max(1);
    let Some(mut pixmap) = tiny_skia::Pixmap::new(tile.max(1), height.max(1)) else {
        return DecorationBuffer { width: tile, height, pixels: vec![0; (tile * height * 4) as usize] };
    };
    for unit in 0..units.max(1) {
        tilekit::draw_tile_base(&mut pixmap, 0, (unit * tile) as i32, tile, theme);
    }
    let screen = panel::render_dead_tile(theme, fonts, swash, tile, label);
    crate::desktop::blit_into(&mut pixmap, 0, 0, &screen);
    DecorationBuffer { width: pixmap.width(), height: pixmap.height(), pixels: pixmap.data().to_vec() }
}

/// Marks a dead face as *permanently* dead rather than merely
/// starting: a dim cross drawn with the relief's own relative light
/// delta (`tile::op_line`), so it darkens whatever face it lands on
/// instead of stamping an absolute chrome colour over it. Same
/// reasoning as the drag-pickup highlight in `redraw_dock`: the tile
/// family's own vocabulary, not the titlebar's.
fn mark_stopped(buffer: &mut DecorationBuffer) {
    let Some(mut pixmap) = tiny_skia::Pixmap::new(buffer.width.max(1), buffer.height.max(1)) else { return };
    pixmap.data_mut().copy_from_slice(&buffer.pixels);
    let (w, h) = (buffer.width as i32, buffer.height as i32);
    let inset = (w / 5).max(2);
    // Only across the top square: on a multi-unit tile the dead screen
    // is up there, and a cross spanning the whole column would read as
    // chrome rather than as this tile's state.
    let bottom = w.min(h) - inset;
    tilekit::op_line(&mut pixmap, inset, inset, w - inset, bottom, -60);
    tilekit::op_line(&mut pixmap, w - inset, inset, inset, bottom, -60);
    buffer.pixels.copy_from_slice(pixmap.data());
}

impl DockWidget for RemoteTile {
    fn name(&self) -> &str {
        &self.entry.name
    }

    /// A remote tile declares no sources and folds nothing: its data
    /// arrives on a socket, and the sampling SDK is deliberately
    /// built-in only (`Source::Command` is arbitrary-argv-by-
    /// declaration, and the dock executing an argv on a third party's
    /// behalf would blur the exact accountability line this boundary
    /// draws). All this reports is whether `service` changed anything
    /// the dock can see.
    fn update(&mut self, _samples: &Samples) -> bool {
        std::mem::take(&mut self.dirty)
    }

    fn render(&self, theme: &Theme, tile: u32, fonts: &mut cosmic_text::FontSystem, swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
        let units = self.entry.tile_units as u32;
        let expected = (tile, tile * units);
        // Drawn only when it is exactly the right size. `on_frame`
        // already refuses a mismatched frame, and `set_tile_px` already
        // drops a stored one — this is the third check, at the one
        // place where being wrong would blit out of bounds, and it is
        // cheap.
        let frame = self.last_frame.as_ref().filter(|frame| (frame.width, frame.height) == expected);

        match (self.state, frame) {
            (TileState::Live, Some(frame)) => frame.clone(),
            (TileState::Hung { .. }, Some(frame)) => {
                let mut washed = frame.clone();
                wash_toward(&mut washed, tile_fill_colour(theme), HUNG_WASH);
                washed
            }
            // A tile that has never drawn, or has just lost the frame
            // it had to a relayout: its label on a dead screen, which
            // is the same face the built-in instruments already use for
            // "no sink", "no interface", "no battery". A starting
            // dockapp should read as an instrument warming up, not as a
            // hole in the column.
            (TileState::Waiting { .. } | TileState::Starting { .. } | TileState::Rejoining { .. } | TileState::Live | TileState::Hung { .. }, _) => {
                dead_face(theme, fonts, swash, tile, units, &self.entry.name)
            }
            (TileState::Stopped { .. }, _) => {
                let mut face = dead_face(theme, fonts, swash, tile, units, &self.entry.name);
                mark_stopped(&mut face);
                face
            }
        }
    }

    fn tile_height(&self) -> u32 {
        self.entry.tile_units as u32
    }

    /// Forwards pointer input to the dockapp, or treats it as the retry
    /// gesture when there is nothing there to forward it to.
    ///
    /// What never reaches a dockapp, whatever it asked for in its
    /// `Hello`:
    ///
    /// * **Middle**, which is the dock's drag-to-reorder gesture. A
    ///   tile that could swallow it could make itself un-reorderable
    ///   and un-removable — a dockapp holding the dock hostage. It is
    ///   filtered by `Shell::on_shell_click` before this is reached and
    ///   filtered again here, because a reservation enforced in one
    ///   place is a reservation one refactor away from being gone.
    /// * **Right**, reserved for this tile's own menu (Restart, Remove,
    ///   About). A dockapp that had already been given it could not
    ///   have it taken back.
    fn on_input(&mut self, input: DockInput, tile: u32) -> Vec<Effect> {
        let now = Instant::now();
        let _ = tile;

        // No input to a hung or dead tile: a click there is the user
        // asking for it back, not something to deliver to a process
        // that is not listening. `Waiting` and `Starting` count as dead
        // for this purpose — there is no connection to send down — but
        // a click on one is *not* a retry, because it is already
        // retrying and resetting the budget would defeat the backoff.
        match self.state {
            TileState::Live => {}
            TileState::Hung { .. } | TileState::Stopped { .. } => {
                if matches!(input, DockInput::Press { .. }) {
                    self.user_restart(now);
                    return vec![Effect::Repaint];
                }
                return Vec::new();
            }
            TileState::Waiting { .. } | TileState::Starting { .. } | TileState::Rejoining { .. } => return Vec::new(),
        }

        let Some(event) = wire_event(&input) else { return Vec::new() };
        let wants = self.connection.as_ref().map(|connection| connection.wants).unwrap_or_else(InputMask::none);
        if !wants.accepts(event.kind) {
            return Vec::new();
        }
        if event.kind == InputKind::Enter || event.kind == InputKind::Leave {
            self.hovered = event.kind == InputKind::Enter;
        }
        self.enqueue(ServerMessage::Input(event), now);
        // No `Effect::Repaint`: the dockapp answers a click with a
        // frame, or it does not, and inventing a repaint here would
        // draw the same pixels again for nothing.
        Vec::new()
    }
}

/// Translates one dock input into the wire's shape, or `None` for the
/// ones a dockapp never receives.
fn wire_event(input: &DockInput) -> Option<InputEvent> {
    let point = |local: Point| (local.x, local.y);
    match *input {
        DockInput::Press { local, button } => {
            let (x, y) = point(local);
            Some(InputEvent { kind: InputKind::Press, button: Some(reserved_filter(button)?), x, y, delta: 0 })
        }
        DockInput::Release { local, button } => {
            let (x, y) = point(local);
            Some(InputEvent { kind: InputKind::Release, button: Some(reserved_filter(button)?), x, y, delta: 0 })
        }
        DockInput::Scroll { local, delta } => {
            let (x, y) = point(local);
            Some(InputEvent { kind: InputKind::Scroll, button: None, x, y, delta })
        }
        DockInput::Enter => Some(InputEvent { kind: InputKind::Enter, button: None, x: 0, y: 0, delta: 0 }),
        DockInput::Leave => Some(InputEvent { kind: InputKind::Leave, button: None, x: 0, y: 0, delta: 0 }),
    }
}

/// The dock's reserved buttons, refused here as well as at the routing
/// layer. `Button` can encode all three on the wire deliberately, so
/// that reserving them stays a *policy* decision visible at a call site
/// rather than a hole in the format that would need a version bump to
/// undo.
pub(crate) fn reserved_filter(button: wm_core::MouseButton) -> Option<Button> {
    match button {
        wm_core::MouseButton::Left => Some(Button::Left),
        wm_core::MouseButton::Middle | wm_core::MouseButton::Right => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dockapp::registry::DockappEntry;

    fn entry(restart: RestartPolicy, units: u8) -> DockappEntry {
        DockappEntry {
            id: "test-tile".to_string(),
            name: "TST".to_string(),
            exec: vec!["/nonexistent/test-tile".to_string()],
            tile_units: units,
            restart,
            source: PathBuf::from("/test/test-tile.dockapp"),
        }
    }

    fn theme() -> Theme {
        wm_theme::default_theme::all_themes().into_iter().next().expect("the theme set is never empty")
    }

    // -----------------------------------------------------------------
    // The crash-loop cutoff
    // -----------------------------------------------------------------

    /// The backoff sequence, stated as the sequence rather than as the
    /// constants, so a change to either one shows up here as a change
    /// to the user-visible behaviour it produces.
    #[test]
    fn backoff_grows_to_a_cap_rather_than_forever() {
        let start = Instant::now();
        let mut budget = LaunchBudget::new();
        let mut waits = Vec::new();
        // Spread far enough apart that the sliding window never
        // accumulates — this test is about the exponent, not the
        // cutoff, and the two are separate mechanisms.
        for step in 0..4 {
            let now = start + Duration::from_secs(step * 120);
            match budget.record_failure(now, false) {
                Fate::Retry { at } => waits.push(at.saturating_duration_since(now).as_secs()),
                Fate::CrashLooped => panic!("failures a window apart must never trip the cutoff"),
            }
        }
        assert_eq!(waits, [1, 2, 4, 8]);
    }

    #[test]
    fn the_backoff_caps_at_thirty_seconds() {
        let start = Instant::now();
        let mut budget = LaunchBudget::new();
        let mut last = 0;
        for step in 0..10 {
            let now = start + Duration::from_secs(step * 120);
            if let Fate::Retry { at } = budget.record_failure(now, false) {
                last = at.saturating_duration_since(now).as_secs();
            }
        }
        assert_eq!(last, 30, "a tile still failing after ten tries retries every 30s, not every 17 minutes");
    }

    /// The headline property, and the one the design calls
    /// non-negotiable: this is a **cutoff**, not a longer backoff. A
    /// dockapp restarted forever is an invisible fork bomb — the user's
    /// only symptom is a machine that is mysteriously busy, and nothing
    /// in the dock says which tile is doing it.
    #[test]
    fn five_failures_in_a_minute_stop_the_tile_permanently() {
        let start = Instant::now();
        let mut budget = LaunchBudget::new();
        for step in 0..LaunchBudget::MAX_FAILURES - 1 {
            let now = start + Duration::from_secs(step as u64);
            assert!(matches!(budget.record_failure(now, false), Fate::Retry { .. }), "failure {step} is still a retry");
        }
        let fifth = start + Duration::from_secs(LaunchBudget::MAX_FAILURES as u64);
        assert_eq!(budget.record_failure(fifth, false), Fate::CrashLooped);
    }

    /// The subtle half: a dockapp that *connects successfully* each
    /// time and then dies is crash-looping just as surely as one that
    /// never connects, and must trip the same cutoff. Resetting the
    /// window on a successful connection — the obvious thing to do —
    /// would let exactly that case relaunch forever at the shortest
    /// backoff.
    #[test]
    fn connecting_successfully_between_crashes_does_not_buy_more_attempts() {
        let start = Instant::now();
        let mut budget = LaunchBudget::new();
        let mut fates = Vec::new();
        for step in 0..LaunchBudget::MAX_FAILURES {
            // `stable: false` is what a short-lived connection reports:
            // it came up, drew, and died well inside the window.
            fates.push(budget.record_failure(start + Duration::from_secs(step as u64 * 2), false));
        }
        assert_eq!(fates.last(), Some(&Fate::CrashLooped));
    }

    /// ...and the other side of it: a tile that ran for a long time and
    /// then died is not crash-looping, and deserves its retry in a
    /// second rather than in thirty.
    #[test]
    fn a_long_lived_connection_resets_the_backoff_but_not_the_window() {
        let start = Instant::now();
        let mut budget = LaunchBudget::new();
        for step in 0..3 {
            budget.record_failure(start + Duration::from_secs(step), false);
        }
        // Still inside the window, so the failures are all still
        // counted — but this one followed a stable connection.
        let now = start + Duration::from_secs(4);
        assert_eq!(budget.record_failure(now, true), Fate::Retry { at: now + Duration::from_secs(1) });
        assert_eq!(budget.recent_failures(), 4, "a stable run forgives the backoff, never the window");
    }

    #[test]
    fn failures_spread_beyond_the_window_never_trip_the_cutoff() {
        let start = Instant::now();
        let mut budget = LaunchBudget::new();
        for step in 0..50 {
            let now = start + CRASH_LOOP_WINDOW * (step + 1);
            assert!(matches!(budget.record_failure(now, false), Fate::Retry { .. }), "a tile that fails once an hour is not a crash loop");
        }
    }

    #[test]
    fn the_user_asking_again_clears_everything() {
        let start = Instant::now();
        let mut budget = LaunchBudget::new();
        for step in 0..LaunchBudget::MAX_FAILURES - 1 {
            budget.record_failure(start + Duration::from_secs(step as u64), false);
        }
        budget.reset();
        assert_eq!(budget.recent_failures(), 0);
        let now = start + Duration::from_secs(10);
        assert_eq!(budget.record_failure(now, false), Fate::Retry { at: now + Duration::from_secs(1) }, "and the backoff starts over at one second");
    }

    // -----------------------------------------------------------------
    // Faces
    // -----------------------------------------------------------------

    #[test]
    fn the_hung_wash_moves_a_frame_halfway_toward_the_tile_fill_and_leaves_alpha_alone() {
        let mut buffer = DecorationBuffer { width: 1, height: 1, pixels: vec![0, 0, 0, 255] };
        wash_toward(&mut buffer, Color::rgb(255, 255, 255), HUNG_WASH);
        // 128/255 of the way from 0 to 255.
        assert_eq!(buffer.pixels[..3], [128, 128, 128]);
        assert_eq!(buffer.pixels[3], 255, "tiles are opaque and stay opaque");
    }

    /// The property the hung face exists for, stated as a property: a
    /// tile that has stopped updating must not look like one that is
    /// updating. Showing a stale reading as though it were live is
    /// worse than showing nothing, because the user acts on it.
    #[test]
    fn a_hung_tile_does_not_draw_the_same_pixels_as_a_live_one() {
        let theme = theme();
        let mut fonts = cosmic_text::FontSystem::new();
        let mut swash = cosmic_text::SwashCache::new();
        let now = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);

        // A frame the dockapp "sent": mid-grey, so a wash in either
        // direction is visible.
        tile.last_frame = Some(DecorationBuffer { width: 56, height: 56, pixels: vec![128; 56 * 56 * 4] });
        tile.state = TileState::Live;
        let live = tile.render(&theme, 56, &mut fonts, &mut swash);

        tile.state = TileState::Hung { since: now };
        let hung = tile.render(&theme, 56, &mut fonts, &mut swash);

        assert_eq!((hung.width, hung.height), (live.width, live.height));
        assert_ne!(hung.pixels, live.pixels, "a hung tile must be visibly distinguishable from a live one");
    }

    /// The other half of the same rule: a frame is either exactly the
    /// right size or it is not drawn. A relayout can change the tile
    /// edge under a dockapp, and scaling an old frame to fit would
    /// paint a blurred, subtly wrong tile that reads as the dockapp's
    /// bug.
    #[test]
    fn a_frame_of_the_wrong_size_is_never_drawn() {
        let theme = theme();
        let mut fonts = cosmic_text::FontSystem::new();
        let mut swash = cosmic_text::SwashCache::new();
        let now = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);
        tile.last_frame = Some(DecorationBuffer { width: 40, height: 40, pixels: vec![7; 40 * 40 * 4] });
        tile.state = TileState::Live;

        let drawn = tile.render(&theme, 56, &mut fonts, &mut swash);
        assert_eq!((drawn.width, drawn.height), (56, 56), "the dock's slot is 56px whatever the dockapp sent");
        assert_ne!(drawn.pixels, vec![7; 56 * 56 * 4]);
    }

    #[test]
    fn a_relayout_drops_a_frame_that_no_longer_fits() {
        let now = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);
        tile.last_frame = Some(DecorationBuffer { width: 56, height: 56, pixels: vec![9; 56 * 56 * 4] });
        tile.set_tile_px(112);
        assert!(tile.last_frame.is_none(), "the stored frame is the wrong size now too");
        assert!(tile.dirty, "and the dock has to redraw the slot");
    }

    /// Every state has to produce a buffer of exactly the slot's size,
    /// including the multi-tile case where `render_dead_tile` only
    /// covers the top square. A short buffer would leave a hole in the
    /// column; a tall one would blit into the tile below.
    #[test]
    fn every_face_fills_its_whole_slot_at_every_registered_height() {
        let theme = theme();
        let mut fonts = cosmic_text::FontSystem::new();
        let mut swash = cosmic_text::SwashCache::new();
        let now = Instant::now();
        for units in 1..=4u8 {
            for state in [
                TileState::Waiting { until: now },
                TileState::Starting { since: now },
                TileState::Live,
                TileState::Hung { since: now },
                TileState::Stopped { reason: StopReason::CrashLooped },
            ] {
                let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, units), 56, now);
                tile.state = state;
                let face = tile.render(&theme, 56, &mut fonts, &mut swash);
                assert_eq!((face.width, face.height), (56, 56 * units as u32), "{state:?} at {units} units");
                assert_eq!(face.pixels.len(), (face.width * face.height * 4) as usize);
            }
        }
    }

    #[test]
    fn a_stopped_tile_is_marked_differently_from_a_starting_one() {
        let theme = theme();
        let mut fonts = cosmic_text::FontSystem::new();
        let mut swash = cosmic_text::SwashCache::new();
        let now = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);

        tile.state = TileState::Starting { since: now };
        let starting = tile.render(&theme, 56, &mut fonts, &mut swash);
        tile.state = TileState::Stopped { reason: StopReason::CrashLooped };
        let stopped = tile.render(&theme, 56, &mut fonts, &mut swash);

        assert_ne!(starting.pixels, stopped.pixels, "'warming up' and 'this is not coming back' must not look the same");
    }

    // -----------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------

    /// The dock's reserved buttons, refused at the encoding layer as
    /// well as at the routing layer. A dockapp that could swallow
    /// middle-click could make itself un-reorderable and un-removable —
    /// a tile holding the dock hostage — and one given right-click
    /// could not have its own menu taken back.
    #[test]
    fn middle_and_right_never_reach_a_dockapp() {
        let at = Point::new(3, 4);
        for button in [wm_core::MouseButton::Middle, wm_core::MouseButton::Right] {
            assert!(wire_event(&DockInput::Press { local: at, button }).is_none(), "{button:?} press");
            assert!(wire_event(&DockInput::Release { local: at, button }).is_none(), "{button:?} release");
        }
        let left = wire_event(&DockInput::Press { local: at, button: wm_core::MouseButton::Left }).unwrap();
        assert_eq!(left.kind, InputKind::Press);
        assert_eq!(left.button, Some(Button::Left));
        assert_eq!((left.x, left.y), (3, 4));
    }

    #[test]
    fn scroll_and_crossing_carry_what_the_wire_expects() {
        let scroll = wire_event(&DockInput::Scroll { local: Point::new(1, 2), delta: -1 }).unwrap();
        assert_eq!(scroll.kind, InputKind::Scroll);
        assert_eq!(scroll.button, None, "a wheel is not a button");
        assert_eq!(scroll.delta, -1);

        for (input, kind) in [(DockInput::Enter, InputKind::Enter), (DockInput::Leave, InputKind::Leave)] {
            let event = wire_event(&input).unwrap();
            assert_eq!(event.kind, kind);
            assert_eq!((event.x, event.y), (0, 0), "a crossing is about the tile, not a position in it");
        }
    }

    /// A click on a dead tile is the retry gesture, and it is the *only*
    /// thing that clears the crash-loop budget: the user is the one
    /// piece of evidence the shell has that the cause might have
    /// changed.
    #[test]
    fn a_press_on_a_stopped_tile_restarts_it_and_clears_the_budget() {
        let now = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);
        for step in 0..LaunchBudget::MAX_FAILURES - 1 {
            tile.budget.record_failure(now + Duration::from_secs(step as u64), false);
        }
        tile.state = TileState::Stopped { reason: StopReason::CrashLooped };

        let effects = tile.on_input(DockInput::Press { local: Point::new(1, 1), button: wm_core::MouseButton::Left }, 56);
        assert_eq!(effects.len(), 1, "the tile's face changed, so the dock repaints");
        assert!(matches!(tile.state, TileState::Waiting { .. }), "and it is queued to launch again");
        assert_eq!(tile.budget.recent_failures(), 0);
    }

    /// ...but a click on a tile that is *already* retrying must not
    /// reset the backoff, or holding the mouse button down would defeat
    /// it entirely.
    #[test]
    fn a_press_on_a_tile_that_is_already_backing_off_changes_nothing() {
        let now = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);
        tile.budget.record_failure(now, false);
        let waiting_until = now + Duration::from_secs(30);
        tile.state = TileState::Waiting { until: waiting_until };

        let effects = tile.on_input(DockInput::Press { local: Point::new(1, 1), button: wm_core::MouseButton::Left }, 56);
        assert!(effects.is_empty());
        assert_eq!(tile.state, TileState::Waiting { until: waiting_until });
        assert_eq!(tile.budget.recent_failures(), 1);
    }

    /// A tile with no connection has nowhere to put an event, and must
    /// not pretend otherwise by reporting a repaint the dock would then
    /// perform for nothing.
    #[test]
    fn input_to_a_disconnected_tile_is_dropped_rather_than_queued() {
        let now = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);
        tile.state = TileState::Live;
        assert!(tile.on_input(DockInput::Scroll { local: Point::new(1, 1), delta: 1 }, 56).is_empty());
    }

    // -----------------------------------------------------------------
    // The invariant
    // -----------------------------------------------------------------

    /// A connected `Seqpacket` pair, so a test can play the dockapp's
    /// end — or, as below, deliberately fail to.
    fn seqpacket_pair() -> (Seqpacket, Seqpacket) {
        use std::os::fd::FromRawFd;
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: `fds` is a two-element array, which is what
        // `socketpair` writes through this pointer.
        let made = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
        assert_eq!(made, 0, "socketpair failed: {}", std::io::Error::last_os_error());
        // SAFETY: both are freshly created descriptors this test owns.
        unsafe { (Seqpacket::from_fd(std::os::fd::OwnedFd::from_raw_fd(fds[0])), Seqpacket::from_fd(std::os::fd::OwnedFd::from_raw_fd(fds[1]))) }
    }

    fn welcome() -> ThemeState {
        ThemeState { tile_px: 56, scale: 1.0, proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION, theme_id: "nextstep-classic".to_string(), theme_toml: String::new() }
    }

    /// **The whole deliverable, stated as a test.**
    ///
    /// A dockapp that stops reading its socket must cost the
    /// compositor's repaint thread nothing. This is not a hypothetical
    /// shape of bug: on 2026-08-29 a dock widget blocked that thread
    /// for ~3.6s at a time on `nmcli`, and the compositor's own stall
    /// watchdog blamed the display driver for it. A blocking `write()`
    /// to a wedged dockapp is that same bug with a different syscall,
    /// and it would be *worse*, because the peer that provokes it is
    /// third-party code.
    ///
    /// So the peer here is the most hostile thing a real dockapp can
    /// be without malice: it connects, and then never calls `recv`
    /// again. Every ping the shell sends piles into the kernel's socket
    /// buffer until it is full, at which point `send` returns `EAGAIN`
    /// and the bounded queue absorbs it, drops its oldest, and
    /// eventually disconnects the peer. None of that may take
    /// measurable time.
    ///
    /// The bound is one whole frame (16ms, the housekeeping interval)
    /// for a thousand servicing passes carrying a thousand pings —
    /// loose enough that a debug build on a loaded runner cannot fail
    /// it by accident, and still tighter than the failure it guards
    /// against by more than two orders of magnitude.
    #[test]
    fn a_dockapp_that_never_reads_costs_the_servicing_pass_nothing() {
        let (ours, _peer) = seqpacket_pair();
        let base = Instant::now();
        // `Never`, so the disconnect this test provokes does not send
        // the tile off trying to launch a program: the property under
        // test is about the socket, not about `fork`.
        let mut tile = RemoteTile::new(entry(RestartPolicy::Never, 1), 56, base);
        tile.adopt(ours, InputMask::all(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);

        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let start = Instant::now();
        for step in 0..1_000u64 {
            // Advance the clock past the ping interval every pass, so
            // every one of these queues another message at a peer that
            // is not reading.
            let now = base + PING_INTERVAL * (step as u32 + 1);
            let mut ctx = ServiceContext { now, theme: &welcome(), socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
            tile.service(&mut ctx);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(16),
            "a thousand servicing passes against a dockapp that never reads took {elapsed:?}; \
             the shell must never block on a dockapp"
        );
    }

    /// ...and the same for a peer that has gone away entirely, which is
    /// the case that would raise `SIGPIPE` and *terminate the
    /// compositor* if `MSG_NOSIGNAL` were ever dropped from the send
    /// path. A crashing dockapp must not be able to take the desktop
    /// with it.
    #[test]
    fn a_dockapp_whose_process_vanished_disconnects_rather_than_signalling() {
        let (ours, peer) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Never, 1), 56, base);
        tile.adopt(ours, InputMask::all(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        drop(peer);

        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let mut ctx = ServiceContext { now: base + PING_INTERVAL, theme: &welcome(), socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
        tile.service(&mut ctx);

        assert!(tile.poll_fd().is_none(), "the connection is gone");
        assert_eq!(tile.state, TileState::Stopped { reason: StopReason::PolicyNever }, "and `restart = never` means it stays gone");
    }

    /// The liveness check, end to end, and the sentence it exists to
    /// make true: **this tells the user, it does not protect the
    /// desktop.** The desktop was never at risk — every call in the
    /// loop above is non-blocking. What was at risk is the user's
    /// belief that a tile showing 14:32 means it is 14:32.
    #[test]
    fn a_tile_that_stops_answering_is_dimmed_and_recovers_when_it_answers_again() {
        let (ours, theirs) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Never, 1), 56, base);
        tile.adopt(ours, InputMask::all(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        tile.state = TileState::Live;

        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let mut pass = |tile: &mut RemoteTile, step: u32| {
            let now = base + PING_INTERVAL * step;
            let mut ctx = ServiceContext { now, theme: &welcome(), socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
            tile.service(&mut ctx);
        };

        for step in 1..UNANSWERED_PINGS_BEFORE_HUNG {
            pass(&mut tile, step);
            assert_eq!(tile.state, TileState::Live, "{step} unanswered pings is not yet a verdict");
        }
        pass(&mut tile, UNANSWERED_PINGS_BEFORE_HUNG);
        assert!(matches!(tile.state, TileState::Hung { .. }), "{UNANSWERED_PINGS_BEFORE_HUNG} unanswered pings is");

        // The dockapp comes back. Any pong at all settles the question
        // being asked — "is your event loop running" — so the sequence
        // number deliberately does not have to match the newest ping.
        let pong = ClientMessage::Pong { seq: 1 }.encode().unwrap();
        theirs.send(&pong).unwrap();
        pass(&mut tile, UNANSWERED_PINGS_BEFORE_HUNG + 1);
        assert_eq!(tile.state, TileState::Live);
    }

    /// Every datagram the peer has been sent since the last drain, as
    /// raw bytes.
    ///
    /// Raw rather than decoded because one test is specifically about a
    /// message the *dockapp's own decoder* refuses, and a helper that
    /// unwrapped the decode would hide exactly the thing it is asserting.
    fn drain(peer: &Seqpacket) -> Vec<Vec<u8>> {
        let mut buffer = vec![0u8; chonk_dock_proto::MAX_MESSAGE_BYTES];
        let mut datagrams = Vec::new();
        loop {
            match peer.recv(&mut buffer) {
                Ok(0) => return datagrams,
                Ok(n) => datagrams.push(buffer[..n].to_vec()),
                Err(_) => return datagrams,
            }
        }
    }

    /// The `ThemeChanged`s a dockapp would actually act on. A servicing
    /// pass may legitimately interleave a `Ping`, so tests filter rather
    /// than demand an exact sequence — the same tolerance a real dockapp
    /// has to have.
    fn themes_pushed(datagrams: &[Vec<u8>]) -> Vec<ThemeState> {
        datagrams
            .iter()
            .filter_map(|bytes| match ServerMessage::decode(bytes) {
                Ok(ServerMessage::ThemeChanged(state)) => Some(state),
                _ => None,
            })
            .collect()
    }

    // -----------------------------------------------------------------
    // Theming: a dockapp never restarts for a theme change
    // -----------------------------------------------------------------

    /// **A theme change restyles a running dockapp; it does not relaunch
    /// it.** The whole reason `ThemeChanged` exists rather than the
    /// shell killing and respawning the process on every theme pick.
    ///
    /// Asserted on the socket, not on an internal flag: the peer here is
    /// the dockapp's end of a real `SOCK_SEQPACKET` pair, and what it
    /// receives is exactly what a real dockapp's `serve` loop would
    /// decode into an `Outcome::Retheme`.
    #[test]
    fn a_theme_change_restyles_a_running_dockapp_without_relaunching_it() {
        let (ours, peer) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Always, 1), 56, base);
        tile.adopt(ours, InputMask::all(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        tile.state = TileState::Live;
        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let _ = drain(&peer); // the Welcome and Visibility from `adopt`

        // A theme pick: new id, new palette, and — because the theme
        // menu is also how the scale is changed — a new tile edge.
        let next = ThemeState { tile_px: 112, scale: 2.0, proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION, theme_id: "amber-phosphor".into(), theme_toml: "id = \"amber-phosphor\"".into() };
        let mut ctx = ServiceContext { now: base, theme: &next, socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
        tile.service(&mut ctx);

        assert_eq!(themes_pushed(&drain(&peer)), vec![next.clone()], "the dockapp is told, over its existing connection");
        assert_eq!(tile.state, TileState::Live, "and is not restarted, or even disturbed");
        assert!(tile.poll_fd().is_some(), "the same socket it had before");
        assert!(tile.child.is_none(), "nothing was spawned");
    }

    /// Design risk #4, end to end: `resize_to_screen` can change the
    /// tile edge under a running dockapp (a monitor plugged in, a scale
    /// change), and frames drawn for the old edge are in flight at
    /// exactly that moment. Those must be **rejected, not blitted at the
    /// wrong size** — and `ThemeChanged` carrying the new `tile_px` is
    /// the thing that lets the dockapp catch up rather than being stuck
    /// sending frames the shell will refuse forever.
    #[test]
    fn a_relayout_rejects_the_old_size_and_theme_changed_is_how_the_dockapp_catches_up() {
        let (ours, peer) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Always, 1), 56, base);
        tile.adopt(ours, InputMask::none(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let _ = drain(&peer);

        let at_56 = welcome();
        let ctx = ServiceContext { now: base, theme: &at_56, socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
        assert!(tile.on_frame(1, 56, 56, vec![9; 56 * 56 * 4], &ctx), "the dock is laid out at 56");
        assert!(tile.last_frame.is_some());

        // The relayout. The tile edge is now 112 and the frame the
        // dockapp is about to send was drawn for 56.
        let at_112 = ThemeState { tile_px: 112, scale: 2.0, ..welcome() };
        let mut ctx = ServiceContext { now: base, theme: &at_112, socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
        tile.service(&mut ctx);

        assert!(tile.last_frame.is_none(), "the stored 56px frame is dropped rather than drawn into a 112px slot");
        assert!(tile.on_frame(2, 56, 56, vec![9; 56 * 56 * 4], &ctx), "an in-flight old-size frame is refused, not fatal");
        assert!(tile.last_frame.is_none(), "and above all not blitted at the wrong size");
        assert!(tile.poll_fd().is_some(), "refusing a frame does not cost the connection");

        // And this is what tells the dockapp to start drawing 112.
        let pushed = themes_pushed(&drain(&peer));
        assert_eq!(pushed.len(), 1, "exactly one ThemeChanged for one relayout");
        assert_eq!(pushed[0].tile_px, 112, "carrying the new geometry, which is what ends the rejections");

        assert!(tile.on_frame(3, 112, 112, vec![7; 112 * 112 * 4], &ctx), "the dockapp redraws at the size it was told");
        assert_eq!(tile.last_frame.as_ref().map(|frame| frame.width), Some(112));
    }

    /// A dock whose theme is not changing must not chatter. The push is
    /// evaluated on every servicing pass — ~60 times a second — so
    /// "nothing changed" has to be genuinely nothing.
    #[test]
    fn an_unchanged_theme_is_never_pushed_again() {
        let (ours, peer) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Always, 1), 56, base);
        tile.adopt(ours, InputMask::none(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        tile.state = TileState::Live;
        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let _ = drain(&peer);

        let steady = welcome();
        for step in 0..1_000u32 {
            let mut ctx =
                ServiceContext { now: base + Duration::from_millis(16 * step as u64), theme: &steady, socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
            tile.service(&mut ctx);
        }
        assert!(themes_pushed(&drain(&peer)).is_empty(), "a dock that did not change told the dockapp nothing");
    }

    /// **The loop this cannot be allowed to become.**
    ///
    /// `ThemeState` derives `PartialEq` over an `f32`, and IEEE-754 says
    /// NaN equals nothing — including itself. So `if next != last_sent
    /// { push }`, the shape this feature naturally takes, would push a
    /// `ThemeChanged` on *every* servicing pass forever if a NaN scale
    /// ever reached it: a compositor busy-loop, filling a dockapp's send
    /// queue at the repaint rate until the tile was disconnected for
    /// overflow, provoked by one bad float.
    ///
    /// Two things stop it and both are asserted: the codec refuses to
    /// decode such a scale (`DecodeError::BadFloat`), and
    /// `ThemeState::same_as` compares bits so the *sender* — which
    /// constructs its state rather than decoding it — is reflexive too.
    /// This test bypasses the first deliberately, by handing the tile a
    /// hand-built context, because the point is that the second one
    /// holds on its own.
    #[test]
    fn a_theme_state_the_shell_cannot_compare_is_pushed_once_not_forever() {
        let (ours, peer) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Always, 1), 56, base);
        tile.adopt(ours, InputMask::none(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        tile.state = TileState::Live;
        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let _ = drain(&peer);

        let nan = ThemeState { scale: f32::NAN, ..welcome() };
        assert_ne!(nan, nan, "the premise: derived equality is not reflexive here");
        for step in 0..1_000u32 {
            let mut ctx =
                ServiceContext { now: base + Duration::from_millis(16 * step as u64), theme: &nan, socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
            tile.service(&mut ctx);
        }
        // Layered, and both layers are asserted. `same_as` stopped the
        // loop: exactly one datagram went out, not a thousand. The codec
        // stopped the value: that one datagram is the one a dockapp's
        // decoder refuses, so the bad scale never reaches a `Theme` on
        // the far side either.
        let sent = drain(&peer);
        let refused: Vec<_> = sent.iter().filter_map(|bytes| ServerMessage::decode(bytes).err()).collect();
        assert_eq!(
            refused,
            vec![chonk_dock_proto::DecodeError::BadFloat { field: "scale", bits: f32::NAN.to_bits() }],
            "one change is one message whatever the float, and that message is one the peer refuses"
        );
        assert!(themes_pushed(&sent).is_empty(), "nothing usable was claimed to be a theme");
        assert!(tile.poll_fd().is_some(), "and the tile was never disconnected for overflow");
    }

    /// A frame of the registered size is adopted; one of any other size
    /// is refused without disturbing the connection. Old-size frames
    /// are in flight exactly when a relayout happens, and blitting one
    /// would draw out of bounds.
    #[test]
    fn a_frame_is_taken_only_at_exactly_the_registered_geometry() {
        let (ours, _peer) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Never, 2), 56, base);
        tile.adopt(ours, InputMask::none(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let ctx = ServiceContext { now: base, theme: &welcome(), socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };

        assert!(tile.on_frame(1, 56, 112, vec![0; 56 * 112 * 4], &ctx), "two units of 56px is exactly this tile");
        assert!(tile.last_frame.is_some());
        assert_eq!(tile.state, TileState::Live, "the first frame is what turns starting into a tile");

        assert!(tile.on_frame(2, 56, 56, vec![1; 56 * 56 * 4], &ctx), "a one-unit frame is refused, not fatal");
        assert_eq!(tile.last_frame.as_ref().map(|frame| frame.pixels[0]), Some(0), "and the good frame is still what is drawn");
        assert!(tile.poll_fd().is_some(), "a wrong-sized frame does not cost the connection");
    }

    // -----------------------------------------------------------------
    // Policy
    // -----------------------------------------------------------------

    /// A handed-off dockapp that never came back is not a *failure*.
    /// Nothing crashed — the likeliest cause is that it had already
    /// exited before the restart — so giving up on the rejoin must not
    /// spend one of the five attempts the crash-loop cutoff counts.
    /// Charging it would mean five theme picks in a minute could stop a
    /// perfectly healthy tile permanently.
    #[test]
    fn giving_up_on_a_rejoin_launches_without_spending_the_crash_budget() {
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Always, 1), 56, base);
        tile.rejoin([7u8; TOKEN_BYTES], base);
        assert_eq!(tile.state, TileState::Rejoining { until: base + REJOIN_WINDOW });
        assert_eq!(tile.token(), &[7u8; TOKEN_BYTES], "the inherited token is what a survivor has to present");

        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let steady = welcome();

        // Still inside the window: held open, nothing launched.
        let mut ctx = ServiceContext { now: base + REJOIN_WINDOW / 2, theme: &steady, socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
        tile.service(&mut ctx);
        assert_eq!(tile.state, TileState::Rejoining { until: base + REJOIN_WINDOW });
        assert_eq!(tile.budget.recent_failures(), 0);

        // Past it: a fresh launch. The entry's program does not exist, so
        // the launch itself fails and books exactly one failure — the
        // number to look at, because going through `disconnected` for the
        // expiry as well would have made it two.
        let mut ctx = ServiceContext { now: base + REJOIN_WINDOW, theme: &steady, socket_path: &socket_path, scratch: &mut scratch, panel_bounds: (1024, 1024) };
        tile.service(&mut ctx);
        assert_eq!(tile.budget.recent_failures(), 1, "the failed launch, and only the failed launch");
        assert!(matches!(tile.state, TileState::Waiting { .. }), "backing off to try the program again");
    }

    /// The rejoin window and the SDK's reconnect window are one number.
    /// A shorter wait here launches a second copy while the survivor is
    /// still knocking; a longer one leaves a hole in the dock after the
    /// survivor has already given up and exited.
    #[test]
    fn the_shell_waits_exactly_as_long_as_a_dockapp_keeps_knocking() {
        assert_eq!(REJOIN_WINDOW, Duration::from_secs(10), "chonk_ui::dockapp::RECONNECT_WINDOW is the same ten seconds");
    }

    #[test]
    fn a_tile_starts_due_immediately_rather_than_after_a_backoff() {
        let now = Instant::now();
        let tile = RemoteTile::new(entry(RestartPolicy::OnCrash, 1), 56, now);
        assert_eq!(tile.state, TileState::Waiting { until: now }, "the first launch is not a retry");
        assert!(tile.dirty, "and its starting face has to reach the screen");
    }

    #[test]
    fn a_tile_reports_the_height_it_registered_for() {
        let now = Instant::now();
        assert_eq!(RemoteTile::new(entry(RestartPolicy::Never, 3), 56, now).tile_height(), 3);
    }

    /// Nothing about a dockapp goes through the built-in sampling
    /// registry — `Source::Command` is arbitrary-argv-by-declaration,
    /// and the dock executing an argv on a third party's behalf would
    /// blur the exact accountability line this boundary draws.
    #[test]
    fn a_remote_tile_declares_no_sources() {
        let now = Instant::now();
        assert!(RemoteTile::new(entry(RestartPolicy::Always, 1), 56, now).sources().is_empty());
    }

    // -----------------------------------------------------------------
    // The instrument panel
    // -----------------------------------------------------------------

    /// A connected tile with an adopted socket pair, ready for panel
    /// traffic, plus the context one servicing pass needs.
    fn panel_fixture() -> (RemoteTile, Seqpacket, Instant) {
        let (ours, peer) = seqpacket_pair();
        let base = Instant::now();
        let mut tile = RemoteTile::new(entry(RestartPolicy::Never, 1), 56, base);
        tile.adopt(ours, InputMask::all(), chonk_dock_proto::PROTOCOL_VERSION, welcome(), base);
        tile.state = TileState::Live;
        // One pass to flush the queued Welcome/Visibility, then drain
        // them so every test starts from a quiet wire.
        service_once(&mut tile, base, (1024, 1024));
        let _ = drain(&peer);
        (tile, peer, base)
    }

    fn service_once(tile: &mut RemoteTile, now: Instant, bounds: (u32, u32)) {
        let socket_path = PathBuf::from("/test/dock.sock");
        let mut scratch = Vec::new();
        let mut ctx =
            ServiceContext { now, theme: &welcome(), socket_path: &socket_path, scratch: &mut scratch, panel_bounds: bounds };
        tile.service(&mut ctx);
    }

    fn server_messages(datagrams: &[Vec<u8>]) -> Vec<ServerMessage> {
        datagrams.iter().filter_map(|bytes| ServerMessage::decode(bytes).ok()).collect()
    }

    #[test]
    fn a_panel_request_is_clamped_to_the_caps_and_the_workarea() {
        assert_eq!(clamp_panel_grant(300, 200, (1024, 1024)), Some((300, 200)), "a fitting request is granted verbatim");
        assert_eq!(clamp_panel_grant(4000, 4000, (1024, 1024)), Some((1024, 1024)), "the protocol caps clamp");
        assert_eq!(clamp_panel_grant(600, 400, (500, 300)), Some((500, 300)), "the workarea clamps");
        assert_eq!(clamp_panel_grant(0, 200, (1024, 1024)), None, "a zero edge cannot be clamped into a size");
        assert_eq!(clamp_panel_grant(300, 200, (0, 300)), None, "a degenerate workarea refuses");
    }

    /// The open handshake over a real socket: `OpenPanel` in,
    /// `PanelOpened` out with the clamped grant.
    #[test]
    fn an_open_panel_request_is_answered_with_a_clamped_grant() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 4000, height: 300 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (600, 400));

        assert!(tile.panel_open());
        assert_eq!(tile.panel_granted(), Some((600, 300)));
        assert_eq!(
            server_messages(&drain(&peer)),
            vec![ServerMessage::PanelOpened { width: 600, height: 300 }],
            "the grant is the request clamped, told to the client"
        );
        assert!(tile.panel_frame().is_none(), "nothing streamed yet: the desktop shows the well, not a fake frame");
    }

    #[test]
    fn a_panel_request_nothing_can_satisfy_is_refused_not_granted() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 300, height: 200 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (0, 0));
        assert!(!tile.panel_open());
        assert_eq!(
            server_messages(&drain(&peer)),
            vec![ServerMessage::PanelClosed { reason: PanelCloseReason::Refused }]
        );
        assert!(tile.poll_fd().is_some(), "a refusal is an answer, not a disconnection");
    }

    /// Bands land in the persistent buffer at their row offset — the
    /// assembled picture is what the desktop blits.
    #[test]
    fn panel_bands_assemble_into_the_granted_buffer() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 4, height: 4 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        let _ = drain(&peer);

        let top = ClientMessage::PanelFrame { generation: 1, y: 0, band_height: 2, width: 4, pixels: vec![0x11; 32] };
        let bottom = ClientMessage::PanelFrame { generation: 1, y: 2, band_height: 2, width: 4, pixels: vec![0x22; 32] };
        peer.send(&top.encode().unwrap()).unwrap();
        peer.send(&bottom.encode().unwrap()).unwrap();
        service_once(&mut tile, base + Duration::from_millis(16), (1024, 1024));

        let frame = tile.panel_frame().expect("streamed");
        assert_eq!((frame.width, frame.height), (4, 4));
        assert!(frame.pixels[..32].iter().all(|&b| b == 0x11), "rows 0..2 are the first band");
        assert!(frame.pixels[32..].iter().all(|&b| b == 0x22), "rows 2..4 are the second");
        assert!(tile.take_panel_ready(base + Duration::from_millis(16)), "new pixels are ready to present");
    }

    /// The reject-don't-rescale rule, band edition: wrong width or rows
    /// past the grant are discarded, the connection and the buffer both
    /// survive.
    #[test]
    fn a_band_outside_the_grant_is_refused_without_costing_the_connection() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 4, height: 4 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        peer.send(&ClientMessage::PanelFrame { generation: 1, y: 0, band_height: 4, width: 4, pixels: vec![0x33; 64] }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        assert!(tile.panel_frame().is_some());

        // Wrong width, and rows off the bottom of the grant.
        let wrong_width = ClientMessage::PanelFrame { generation: 2, y: 0, band_height: 1, width: 2, pixels: vec![0xFF; 8] };
        let off_bottom = ClientMessage::PanelFrame { generation: 2, y: 3, band_height: 2, width: 4, pixels: vec![0xFF; 32] };
        peer.send(&wrong_width.encode().unwrap()).unwrap();
        peer.send(&off_bottom.encode().unwrap()).unwrap();
        service_once(&mut tile, base + Duration::from_millis(16), (1024, 1024));

        assert!(tile.poll_fd().is_some(), "a wrong-sized band does not cost the connection");
        let frame = tile.panel_frame().unwrap();
        assert!(frame.pixels.iter().all(|&b| b == 0x33), "and above all is never blitted");
    }

    /// The contract's flow-control drop: a band from an older repaint
    /// than the newest seen is dropped rather than painted backwards.
    #[test]
    fn a_stale_generation_band_is_dropped() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 4, height: 2 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));

        let newer = ClientMessage::PanelFrame { generation: 7, y: 0, band_height: 2, width: 4, pixels: vec![0x77; 32] };
        let stale = ClientMessage::PanelFrame { generation: 6, y: 0, band_height: 2, width: 4, pixels: vec![0x66; 32] };
        peer.send(&newer.encode().unwrap()).unwrap();
        peer.send(&stale.encode().unwrap()).unwrap();
        service_once(&mut tile, base + Duration::from_millis(16), (1024, 1024));

        assert!(tile.panel_frame().unwrap().pixels.iter().all(|&b| b == 0x77), "the newest repaint's rows stand");
    }

    #[test]
    fn generation_newness_wraps_like_a_sequence_number() {
        assert!(generation_newer(1, 0));
        assert!(!generation_newer(0, 1));
        assert!(generation_newer(0, u32::MAX), "the counter rolling over is not a stuck panel");
        assert!(!generation_newer(u32::MAX, 0));
        assert!(!generation_newer(5, 5), "equal is not older: several bands of one repaint all land");
    }

    /// `ClosePanel` is acknowledged with `PanelClosed { ClientRequest }`;
    /// a shell-side dismissal says `Dismissed`. The client can always
    /// tell whose decision ended its panel.
    #[test]
    fn a_panel_close_is_acknowledged_and_a_dismissal_is_attributed() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 8, height: 8 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        let _ = drain(&peer);

        peer.send(&ClientMessage::ClosePanel.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        assert!(!tile.panel_open());
        assert_eq!(server_messages(&drain(&peer)), vec![ServerMessage::PanelClosed { reason: PanelCloseReason::ClientRequest }]);

        // Reopen, then the user clicks away.
        peer.send(&ClientMessage::OpenPanel { width: 8, height: 8 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        let _ = drain(&peer);
        tile.close_panel(PanelCloseReason::Dismissed, base);
        assert_eq!(server_messages(&drain(&peer)), vec![ServerMessage::PanelClosed { reason: PanelCloseReason::Dismissed }]);
        assert!(tile.poll_fd().is_some(), "a dismissal costs the panel, never the tile");
    }

    /// An `OpenPanel` while one is open renegotiates in place: a fresh
    /// grant, and the streamed pixels survive only a size-identical
    /// renegotiation.
    #[test]
    fn a_second_open_renegotiates_in_place() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 4, height: 2 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        peer.send(&ClientMessage::PanelFrame { generation: 1, y: 0, band_height: 2, width: 4, pixels: vec![0x44; 32] }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        assert!(tile.panel_frame().is_some());
        let _ = drain(&peer);

        // Same size: the pixels survive.
        peer.send(&ClientMessage::OpenPanel { width: 4, height: 2 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        assert_eq!(server_messages(&drain(&peer)), vec![ServerMessage::PanelOpened { width: 4, height: 2 }]);
        assert!(tile.panel_frame().is_some(), "a size-identical renegotiation keeps the streamed pixels");

        // Different size: fresh buffer, stale pixels gone.
        peer.send(&ClientMessage::OpenPanel { width: 8, height: 2 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        assert_eq!(tile.panel_granted(), Some((8, 2)));
        assert!(tile.panel_frame().is_none(), "pixels for the old grant are exactly the wrong-sized blit the checks refuse");
    }

    /// The crash-isolation invariant, panel edition: a hung instrument's
    /// panel dies by the same ping machinery as its tile.
    #[test]
    fn a_hung_dockapp_loses_its_panel_by_the_ping_machinery() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 8, height: 8 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        assert!(tile.panel_open());
        let _ = drain(&peer);

        // Stops answering pings without closing its socket.
        for step in 1..=UNANSWERED_PINGS_BEFORE_HUNG + 1 {
            service_once(&mut tile, base + PING_INTERVAL * step, (1024, 1024));
        }
        assert!(matches!(tile.state(), TileState::Hung { .. }));
        assert!(!tile.panel_open(), "a frozen detail view is a stale reading at ten times the size");
        let closes: Vec<_> = server_messages(&drain(&peer))
            .into_iter()
            .filter(|message| matches!(message, ServerMessage::PanelClosed { .. }))
            .collect();
        assert_eq!(closes, vec![ServerMessage::PanelClosed { reason: PanelCloseReason::Shutdown }]);
    }

    /// A dockapp that dies takes its panel state with it — nothing for
    /// the desktop to keep staging.
    #[test]
    fn a_dead_dockapps_panel_is_torn_down_with_it() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 8, height: 8 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        assert!(tile.panel_open());
        drop(peer);
        // The kernel's teardown of the far end is not synchronous with
        // the `drop`; poll boundedly, as the restart tests do.
        for step in 0..500 {
            service_once(&mut tile, base + Duration::from_millis(16 + step), (1024, 1024));
            if tile.poll_fd().is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(tile.poll_fd().is_none(), "the connection is gone");
        assert!(!tile.panel_open(), "and the panel with it");
    }

    /// Panel input goes down the wire as `0x89` with content-local
    /// coordinates, untouched.
    #[test]
    fn panel_input_reaches_the_wire_as_panel_input() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 100, height: 50 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));
        let _ = drain(&peer);

        let event = InputEvent { kind: InputKind::Press, button: Some(Button::Left), x: 40, y: 12, delta: 0 };
        tile.panel_input(event, base);
        assert_eq!(server_messages(&drain(&peer)), vec![ServerMessage::PanelInput(event)]);

        // ...and none at all once the panel is gone: input to a closed
        // panel is dropped, not queued for a surface that left.
        tile.close_panel(PanelCloseReason::Dismissed, base);
        let _ = drain(&peer);
        tile.panel_input(event, base);
        assert!(server_messages(&drain(&peer)).is_empty());
    }

    /// The present meter: pixels reach the screen at most once per
    /// [`PANEL_PRESENT_INTERVAL`], however fast the client streams, and
    /// the first present is immediate.
    #[test]
    fn panel_presents_are_metered_not_per_band() {
        let (mut tile, peer, base) = panel_fixture();
        peer.send(&ClientMessage::OpenPanel { width: 4, height: 2 }.encode().unwrap()).unwrap();
        service_once(&mut tile, base, (1024, 1024));

        let mut presents = 0;
        for step in 0..10u32 {
            let now = base + Duration::from_millis(4 * step as u64);
            let band = ClientMessage::PanelFrame { generation: step, y: 0, band_height: 2, width: 4, pixels: vec![step as u8; 32] };
            peer.send(&band.encode().unwrap()).unwrap();
            service_once(&mut tile, now, (1024, 1024));
            if tile.take_panel_ready(now) {
                presents += 1;
            }
        }
        // Ten repaints across 36ms: the first present is immediate, the
        // meter allows at most one more.
        assert!(presents <= 2, "{presents} presents for a 250Hz stream is not a meter");
        assert!(presents >= 1, "a streaming panel must reach the screen at all");
    }
}
