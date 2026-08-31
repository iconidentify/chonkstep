//! Out-of-process dock tiles: the listener, the registry, and one
//! [`RemoteTile`] per registered dockapp.
//!
//! # What a dockapp is, and what it is not
//!
//! A dockapp is a separate process that draws one (or a few) dock tiles
//! and pushes the finished pixels to this shell over a private
//! `SOCK_SEQPACKET` socket. It is **neither an X client nor a Wayland
//! client**. It never opens a display connection — the shell removes
//! `WAYLAND_DISPLAY` and `DISPLAY` from its environment before `exec`
//! (see [`crate::spawn::DISPLAY_SERVER_ENV`]) — so there is nothing for
//! it to screenshot, no window list to enumerate, and no clipboard to
//! read. `wl_shm`, `zwlr_screencopy_v1` and
//! `zwlr_foreign_toplevel_management_v1` are *unreachable* rather than
//! denied, which is a stronger statement than Wayland's own security
//! model can make.
//!
//! That is also why there is no backend fork. The dock is not a
//! `wl_surface`; it is a scene record the renderer draws directly, and
//! a dockapp's frame is blitted into it exactly as a built-in
//! instrument's `DecorationBuffer` is. The *entire* backend-specific
//! cost of this feature is event-loop fd integration —
//! [`Desktop::extra_poll_fds`](crate::desktop::Desktop::extra_poll_fds)
//! goes into the X11 binary's `poll` set and into a calloop `Generic`
//! source on the Wayland side — which is about twenty lines per binary
//! and is exactly where backend differences belong.
//!
//! # The one invariant
//!
//! **The shell never blocks on a dockapp.** Not on a frame, not on a
//! reply, not on a socket becoming writable. A dockapp that hangs costs
//! the compositor nothing at all; its frames simply stop arriving. Read
//! [`tile`]'s module docs for why the liveness check therefore exists
//! to inform the *user* rather than to protect the desktop, and why
//! that inversion is the whole point of the boundary.
//!
//! # Rejected alternatives
//!
//! Recorded because each looks obviously better than a private socket
//! until the reason it is not is written down:
//!
//! * **A custom Wayland protocol** forks the backends — X11 would need
//!   a parallel property/`ClientMessage` protocol — *and* would
//!   composite dockapp pixels over the dock's own pixmap, putting the
//!   drag-pickup highlight ring permanently underneath the thing it is
//!   supposed to highlight.
//! * **wlr-layer-shell** inverts ownership: the compositor anchors a
//!   surface the client owns, so reordering the dock means moving
//!   another process's surface, with a tearing window while it
//!   reconfigures. And there is no X11 analogue.
//! * **xdg-foreign / subsurfaces** are structurally impossible. Both
//!   need a parent `wl_surface`, and the dock is a record in a
//!   `HashMap`.
//! * **XEmbed / reparent-swallow** is X11-only, so choosing it is
//!   choosing the fork.
//!
//! Consequence worth stating plainly: existing WindowMaker dockapps
//! (`wmclock`, `wmmon`, `wmnet`) will **not** run here. They are X
//! clients expecting to be swallowed. If that ever matters it is a
//! separate optional leaf binary that creates a 64x64 X window, lets
//! the legacy app reparent into it, `XGetImage`s at 4Hz and pushes
//! those pixels down this same socket — not a change to any of this.
//!
//! # What this boundary does not protect
//!
//! Say it plainly, because a security claim that overreaches is worse
//! than none: a dockapp is a normal process running as you. There is no
//! bubblewrap, no seccomp, no portal. It can read your home directory
//! exactly as any program you run can. What this boundary protects is
//! the desktop's *responsiveness* and its *pixels* — a dockapp cannot
//! freeze the compositor, cannot see another tile's content or events,
//! and cannot enumerate or capture your windows.

pub(crate) mod handoff;
pub(crate) mod panel;
pub(crate) mod registry;
pub(crate) mod tile;

use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::Instant;

use chonk_dock_proto::handshake::HANDSHAKE_TIMEOUT;
use chonk_dock_proto::transport::{Seqpacket, SeqpacketListener};
use chonk_dock_proto::wire::{GoodbyeReason, ThemeState};
use chonk_dock_proto::{ClientMessage, ServerMessage, MAX_MESSAGE_BYTES};
use wm_theme::model::Theme;

/// Why the session is letting go of its dockapps — and therefore
/// whether they are being *stopped* or *handed forward*.
///
/// One enum rather than two methods because the caller is a binary's
/// event loop that already knows which of the two it is doing, and
/// because the wrong answer is silent in both directions: stopping on a
/// restart loses the feature, handing forward on a logout leaves
/// processes running after the session that owns them is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Farewell {
    /// This process is about to `exec` its replacement. Dockapps stay
    /// running and their tokens are written where the incoming shell
    /// will find them — see [`handoff`].
    Restarting,
    /// The session is over. Dockapps are told `Goodbye { Shutdown }` and
    /// terminated, and any handoff file is cleared.
    SessionOver,
}

/// A connection that has been accepted but has not identified itself.
struct Pending {
    socket: Seqpacket,
    /// When it was accepted. A connection that never speaks is dropped
    /// after [`HANDSHAKE_TIMEOUT`] — otherwise any local process could
    /// hold file descriptors open in this compositor for free by
    /// connecting and saying nothing.
    since: Instant,
}

/// One dockapp presenting itself, for the caller to match against a
/// tile and its token.
pub(crate) struct Admission {
    pub socket: Seqpacket,
    pub hello: ClientMessage,
}

/// The listening half: the socket dockapps connect to, and the
/// connections that have not yet said who they are.
///
/// Deliberately does *not* own the tiles. A `Hello` names an id, and
/// resolving an id to a tile means looking at the dock's column — which
/// is `Desktop`'s, mixed with built-ins, and reorderable. Splitting it
/// this way keeps this type about sockets and keeps slot identity in
/// the one place that already owns it.
pub(crate) struct DockHost {
    listener: Option<SeqpacketListener>,
    socket_path: PathBuf,
    pending: Vec<Pending>,
    /// One receive buffer for the whole shell, [`MAX_MESSAGE_BYTES`]
    /// long. Allocating a quarter-megabyte per tile per frame on the
    /// repaint thread would be a worse use of it than anything a
    /// dockapp could do.
    scratch: Vec<u8>,
    /// The wire-shaped view of "what the dock looks like right now",
    /// recomputed only when it actually changed — see
    /// [`ThemeBroadcast`].
    broadcast: ThemeBroadcast,
}

/// The `ThemeState` every dockapp is told, cached against the inputs
/// that produce it.
///
/// The cache is not a micro-optimisation. `theme_toml` is
/// `toml::to_string(theme)`, which walks and formats the whole palette
/// and allocates a few kilobytes; the servicing pass that needs this
/// value runs on the compositor's repaint thread once per housekeeping
/// tick (16 ms), so serializing unconditionally would be ~60 full theme
/// serializations a second, forever, to produce the same string.
///
/// So the comparison is on the *source* — the `Theme` itself, by value,
/// plus the tile edge and scale — and the serialization happens only
/// when that comparison fails. Comparing the `Theme` rather than just
/// its `id` is the correctness half: `theme_id` is the fast path a
/// dockapp resolves through `theme_by_id`, but `theme_toml` exists
/// precisely for palettes that path cannot name, and a cache keyed on
/// the id alone would never notice one of those change.
///
/// It also gives `Welcome` and `ThemeChanged` one producer. A dockapp
/// that connects and a dockapp that restyles are told the same thing by
/// the same code, so the two can never drift into disagreeing about
/// what the current theme is.
struct ThemeBroadcast {
    /// What `state` was built from. `None` until the first refresh.
    source: Option<Theme>,
    state: ThemeState,
}

impl ThemeBroadcast {
    fn new() -> Self {
        // Placeholder until the first `refresh`, which happens on the
        // first servicing pass — before any dockapp can have connected,
        // since connecting requires a tile that has launched.
        Self { source: None, state: ThemeState { tile_px: 0, scale: 1.0, proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION, theme_id: String::new(), theme_toml: String::new() } }
    }

    fn refresh(&mut self, tile_px: u32, scale: f32, theme: &Theme) {
        let scale = usable_scale(scale);
        if self.state.tile_px == tile_px
            && self.state.scale.to_bits() == scale.to_bits()
            && self.source.as_ref().is_some_and(|cached| cached == theme)
        {
            return;
        }
        self.source = Some(theme.clone());
        self.state = ThemeState {
            tile_px,
            scale,
            // How the shell advertises that the panel family is open
            // for business — the probe a client reads before its first
            // `OpenPanel`.
            proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION,
            theme_id: theme.id.clone(),
            // The correctness path beside the fast one: a dockapp built
            // against a different `wm-theme`, or a session running a
            // theme with no built-in id, still gets the real palette. An
            // unserializable theme is a shell bug and costs the dockapp
            // the fast path only — an empty string here means "I have
            // nothing to add", and the SDK falls back to `theme_by_id`
            // and then to its own default rather than failing to draw.
            theme_toml: toml::to_string(theme).unwrap_or_default(),
        };
    }

    fn state(&self) -> &ThemeState {
        &self.state
    }
}

/// The shell's last chance to stop an unusable scale becoming a wire
/// value.
///
/// The codec refuses to *decode* a NaN, infinite, non-positive or absurd
/// scale ([`chonk_dock_proto::wire::DecodeError::BadFloat`]), which is
/// the right place for it — but the shell is the sender, and a sender
/// does not decode its own messages. Without this clamp a bad
/// `Desktop::scale` would produce a `Welcome` every dockapp rejects, and
/// the SDK would treat the rejection as a protocol disagreement and
/// reconnect: a launch loop the user would experience as a dock full of
/// tiles that never appear, caused by one float.
///
/// Substituting rather than refusing, and only here: a desktop that
/// cannot tell its dockapps a scale is still a desktop, and 1.0 is what
/// every unscaled session already runs at. The log line is what makes it
/// findable — a silent substitution here would be the wrong-sized-tile
/// bug the SDK's `check_drawable` refuses to commit.
fn usable_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 && scale <= chonk_dock_proto::MAX_SCALE {
        return scale;
    }
    tracing::error!(scale, "the dock's scale is not a number a tile can be drawn at; telling dockapps 1.0 instead");
    1.0
}

impl DockHost {
    /// Binds `$XDG_RUNTIME_DIR/chonkstep/dock-<display>.sock`.
    ///
    /// A failure here is a session with no dockapps, never a session
    /// that will not start: the socket lives under `$XDG_RUNTIME_DIR`
    /// with no `/tmp` fallback (a world-writable directory is no place
    /// for an authentication-bearing socket), and a session without one
    /// gets a clear log line and the built-in instruments, which is a
    /// perfectly good desktop.
    pub(crate) fn new(display: &str) -> Self {
        let socket_path = match chonk_dock_proto::transport::socket_path(display) {
            Ok(path) => path,
            Err(error) => {
                tracing::warn!(?error, "no dockapp socket path; dockapps are unavailable this session");
                return Self::unbound(PathBuf::new());
            }
        };
        Self::bind_at(socket_path)
    }

    /// Binds a named socket. Split out of [`new`](Self::new) so a test
    /// can stand a real listener up in a scratch directory without
    /// touching `$XDG_RUNTIME_DIR` — the environment is process-global
    /// and this crate's tests share one binary, so an env-var dance here
    /// would be a test that fails whenever another one runs beside it.
    pub(crate) fn bind_at(socket_path: PathBuf) -> Self {
        match SeqpacketListener::bind(&socket_path) {
            Ok(listener) => {
                tracing::info!(socket = %socket_path.display(), "dockapp socket listening");
                Self { listener: Some(listener), socket_path, pending: Vec::new(), scratch: Vec::new(), broadcast: ThemeBroadcast::new() }
            }
            Err(error) => {
                tracing::warn!(?error, socket = %socket_path.display(), "could not bind the dockapp socket; dockapps are unavailable this session");
                Self::unbound(socket_path)
            }
        }
    }

    /// A session with no dockapp socket: every built-in instrument, no
    /// remote tiles, and no failure anybody has to handle.
    fn unbound(socket_path: PathBuf) -> Self {
        Self { listener: None, socket_path, pending: Vec::new(), scratch: Vec::new(), broadcast: ThemeBroadcast::new() }
    }

    pub(crate) fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    /// Where the tokens of dockapps left running across a restart are
    /// written and read. `None` for a session with no socket, which is a
    /// session with no dockapps.
    pub(crate) fn handoff_path(&self) -> Option<PathBuf> {
        handoff::beside(&self.socket_path)
    }

    pub(crate) fn is_listening(&self) -> bool {
        self.listener.is_some()
    }

    pub(crate) fn scratch(&mut self) -> &mut Vec<u8> {
        &mut self.scratch
    }

    /// Recomputes the `ThemeState` every dockapp is told, if anything it
    /// is built from changed. Cheap when nothing did — see
    /// [`ThemeBroadcast`].
    pub(crate) fn refresh_theme(&mut self, tile_px: u32, scale: f32, theme: &Theme) {
        self.broadcast.refresh(tile_px, scale, theme);
    }

    /// The current one. Call [`refresh_theme`](Self::refresh_theme)
    /// first; a servicing pass does, once, before it touches any tile.
    pub(crate) fn theme(&self) -> &ThemeState {
        self.broadcast.state()
    }

    /// Every fd the event loop must watch on this side: the listener,
    /// plus one per connection that has not identified itself yet.
    /// (Established connections are a tile's, not this type's — see
    /// `RemoteTile::poll_fd`.)
    pub(crate) fn poll_fds(&self) -> Vec<RawFd> {
        let mut fds = Vec::with_capacity(self.pending.len() + 1);
        if let Some(listener) = &self.listener {
            fds.push(listener.as_raw_fd());
        }
        fds.extend(self.pending.iter().map(|pending| pending.socket.as_raw_fd()));
        fds
    }

    /// Accepts whatever is waiting and returns whoever has identified
    /// themselves since the last pass.
    ///
    /// Non-blocking throughout: `accept4` hands back a socket that is
    /// already `O_NONBLOCK` (so there is not even an instant in which
    /// one exists in blocking mode), and every `recv` is
    /// `MSG_DONTWAIT`.
    pub(crate) fn service(&mut self, now: Instant) -> Vec<Admission> {
        self.accept(now);
        self.collect_hellos(now)
    }

    fn accept(&mut self, now: Instant) {
        let Some(listener) = &self.listener else { return };
        loop {
            match listener.accept() {
                Ok(Some(socket)) => {
                    // `SO_PEERCRED`: defence in depth behind the
                    // socket's own 0600 mode and its 0700 parent
                    // directory. Cheap, unforgeable, and it makes "only
                    // this user's processes" an enforced statement
                    // rather than an inference from file permissions.
                    match socket.peer_is_this_user() {
                        Ok(true) => self.pending.push(Pending { socket, since: now }),
                        Ok(false) => tracing::warn!("refused a dockapp connection from another user"),
                        Err(error) => tracing::warn!(?error, "could not read a dockapp connection's peer credentials; refusing it"),
                    }
                }
                Ok(None) => return,
                Err(error) => {
                    tracing::warn!(?error, "dockapp accept failed");
                    return;
                }
            }
        }
    }

    fn collect_hellos(&mut self, now: Instant) -> Vec<Admission> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        if self.scratch.len() < MAX_MESSAGE_BYTES {
            self.scratch.resize(MAX_MESSAGE_BYTES, 0);
        }
        let mut admissions = Vec::new();
        let mut keep = Vec::with_capacity(self.pending.len());
        for pending in std::mem::take(&mut self.pending) {
            match pending.socket.recv(&mut self.scratch) {
                Ok(0) => continue, // Connected and hung up. Nothing to say about it.
                Ok(n) => match ClientMessage::decode(&self.scratch[..n]) {
                    Ok(hello @ ClientMessage::Hello { .. }) => admissions.push(Admission { socket: pending.socket, hello }),
                    // Anything else before a `Hello` — a `Frame` in
                    // particular — is a protocol error. A dockapp does
                    // not get to skip authentication by simply starting
                    // to draw.
                    Ok(other) => {
                        tracing::warn!(message = ?std::mem::discriminant(&other), "a dockapp connection spoke before saying Hello");
                        goodbye(&pending.socket, GoodbyeReason::ProtocolError);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "a dockapp connection's first message did not decode");
                        goodbye(&pending.socket, GoodbyeReason::ProtocolError);
                    }
                },
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if now.saturating_duration_since(pending.since) < HANDSHAKE_TIMEOUT {
                        keep.push(pending);
                    } else {
                        tracing::debug!("dropping a dockapp connection that never said Hello");
                    }
                }
                Err(error) => tracing::warn!(?error, "a pending dockapp connection failed"),
            }
        }
        self.pending = keep;
        admissions
    }
}

/// Matches one `Hello` to the tile whose token it presents, and adopts
/// it if it earns the slot.
///
/// # The adoption rule
///
/// A `Hello` is admitted when the id names a registered tile, that tile
/// is *expecting* a connection ([`RemoteTile::awaiting_hello`] — either
/// it just launched a process, or it inherited a token across the
/// shell's restart and is holding the slot open), and the token it
/// presents is still that tile's. All three, every time. The reconnect
/// case does not relax any of them; it only widens which tile *states*
/// count as expecting.
///
/// # A valid token does not displace a connected tile
///
/// The `Replaced` refusal below fires even for a `Hello` whose token
/// would have validated, and that is deliberate. A valid token proves
/// "you were launched for this slot" — not "you are the process
/// currently drawing it". It is a shared secret handed over in the
/// environment, so a forked copy of the dockapp, or any process of this
/// user that read `/proc/<pid>/environ`, holds one just as good.
/// Displacing on a token match would let a second instance of a dockapp
/// silently steal a working tile, and would hand anything that can read
/// the token a takeover at a moment of its choosing. Refusing costs the
/// genuine reconnect nothing, because socket EOF is instant and
/// definitive: by the time a survivor's `Hello` arrives, the tile it
/// wants has already seen its predecessor go.
///
/// # Rejections
///
/// Every one answers with a reason on the wire and a detail in the log,
/// never the other way around: a peer that failed authentication learns
/// "you were not launched by this shell" and nothing about the shell's
/// internals.
pub(crate) fn admit<'a>(
    mut tiles: impl Iterator<Item = &'a mut tile::RemoteTile>,
    admission: Admission,
    welcome: &ThemeState,
    now: Instant,
) {
    let Admission { socket, hello } = admission;
    let ClientMessage::Hello { id, .. } = &hello else {
        goodbye(&socket, GoodbyeReason::ProtocolError);
        return;
    };
    let id = id.clone();
    let Some(tile) = tiles.find(|tile| tile.id() == id) else {
        tracing::warn!(%id, "a dockapp presented an id with no registered slot");
        goodbye(&socket, GoodbyeReason::Unauthorized);
        return;
    };
    let rejoining = matches!(tile.state(), tile::TileState::Rejoining { .. });
    // One connection per registered id — see the note above on why a
    // valid token does not change this answer.
    if !tile.awaiting_hello() {
        tracing::warn!(%id, "a second connection claimed a dockapp id that is already connected");
        goodbye(&socket, GoodbyeReason::Replaced);
        return;
    }
    match chonk_dock_proto::validate_hello(&hello, tile.token(), welcome.tile_px) {
        Ok(accepted) => {
            if accepted.tile_units != tile.entry().tile_units {
                // The registry is the authority on how much of the
                // column a dockapp occupies — the dock laid out for that
                // number before the process even started. A survivor
                // that changed its mind across a restart is refused for
                // the same reason a fresh one is.
                tracing::warn!(%id, asked = accepted.tile_units, registered = tile.entry().tile_units, "a dockapp asked for a different tile height than it registered");
                goodbye(&socket, GoodbyeReason::TileTooLarge);
                return;
            }
            if rejoining {
                tracing::info!(%id, "readopted a dockapp that outlived the shell restart");
            }
            tile.adopt(socket, accepted.wants, welcome.clone(), now);
        }
        Err(reason) => {
            tracing::warn!(%id, ?reason, rejoining, "refusing a dockapp connection");
            goodbye(&socket, reason);
        }
    }
}

/// Lets go of every out-of-process tile, in one of two entirely
/// different ways.
///
/// # [`Farewell::SessionOver`]
///
/// `Goodbye { Shutdown }`, then terminate. The reason code is actionable
/// on the far side: it says "reconnecting is pointless", as against the
/// bare EOF that means "try again". The handoff file is cleared, so the
/// *next* login does not hold slots open for processes that ended with
/// this one.
///
/// # [`Farewell::Restarting`]
///
/// The dockapps stay running, and their tokens are written where the
/// incoming shell will find them ([`handoff`]). This is what makes a
/// dockapp survive a theme pick, `scripts/restart.sh` and
/// `scripts/update.sh` — and it is worth naming that on the Wayland
/// session that is **strictly better than any ordinary client gets**: a
/// Wayland client dies with the compositor's socket and there is no
/// SaveSet equivalent to adopt it afterwards (see the README's "Restart
/// costs you your clients"). A dock tile that is not a display-server
/// client keeps running through the restart that kills every window on
/// the screen.
///
/// Before this existed, a restart briefly *doubled* every dockapp
/// instead: the old process saw EOF, spent ten seconds trying to reach a
/// shell that had been replaced, and was eventually refused because its
/// token had been minted by a process that no longer existed — while the
/// fresh shell had already launched its own copy from the registry.
/// Nothing was broken, but the user paid for two of everything for ten
/// seconds, on the gesture they perform most. Now the survivor *is* the
/// copy.
pub(crate) fn shut_down<'a>(
    tiles: impl Iterator<Item = &'a mut tile::RemoteTile>,
    handoff_path: Option<&std::path::Path>,
    farewell: Farewell,
) {
    let mut tokens = Vec::new();
    for tile in tiles {
        if farewell == Farewell::Restarting {
            if let Some(entry) = tile.hand_off() {
                tokens.push(entry);
                continue;
            }
            // Fell through: nothing worth handing over (never launched,
            // already stopped, or hung — see `RemoteTile::hand_off` for
            // why a hung tile is deliberately not left behind). It gets
            // the ordinary shutdown.
        }
        tile.shut_down(GoodbyeReason::Shutdown);
    }
    match handoff_path {
        Some(path) if farewell == Farewell::Restarting => handoff::write(path, &tokens),
        Some(path) => handoff::clear(path),
        // No socket this session, so there were no dockapps to hand over
        // in the first place.
        None => {}
    }
}

/// Best-effort refusal with a reason. The wire carries only the reason
/// code; the shell logs the detail — so a rejected dockapp learns
/// something it can act on ("rebuild me", "you were not launched by
/// this shell") without the shell narrating its internals to an
/// unauthenticated peer.
pub(crate) fn goodbye(socket: &Seqpacket, reason: GoodbyeReason) {
    if let Ok(bytes) = (ServerMessage::Goodbye { reason }).encode() {
        let _ = socket.send(&bytes);
    }
}

/// The display name the dockapp socket is keyed on.
///
/// Per-display rather than per-pid so the path is *stable* across the
/// shell's own hot restart, which is what makes reconnection possible
/// at all: a pid in the path would rule out restart survival by
/// construction. `WAYLAND_DISPLAY` first because the compositor sets it
/// before `Shell::new` runs and an XWayland session has both.
pub(crate) fn current_display() -> String {
    std::env::var("WAYLAND_DISPLAY")
        .or_else(|_| std::env::var("DISPLAY"))
        .unwrap_or_else(|_| "default".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme(id: &str) -> Theme {
        let mut theme = wm_theme::default_theme::all_themes().into_iter().next().expect("the theme set is never empty");
        theme.id = id.to_string();
        theme
    }

    #[test]
    fn the_serialized_theme_is_produced_once_and_reused() {
        // The property that makes it safe to evaluate this on the
        // repaint thread every 16 ms. `toml::to_string` walks the whole
        // palette; doing it per pass would be ~60 full serializations a
        // second to produce the same string.
        let mut broadcast = ThemeBroadcast::new();
        broadcast.refresh(56, 1.0, &theme("nextstep-classic"));
        let first = broadcast.state().clone();
        assert!(!first.theme_toml.is_empty(), "the correctness path carries a real palette, not an empty string");

        let before = broadcast.state().theme_toml.as_ptr();
        broadcast.refresh(56, 1.0, &theme("nextstep-classic"));
        assert_eq!(broadcast.state().theme_toml.as_ptr(), before, "an unchanged theme must not be re-serialized");
        assert!(broadcast.state().same_as(&first));
    }

    #[test]
    fn every_input_that_a_dockapp_can_see_change_triggers_a_refresh() {
        // Stated as the full set rather than as one example, because a
        // trigger that is missed here is a dockapp drawing at the wrong
        // size or in last week's colors with nothing to indicate it.
        let base = theme("nextstep-classic");
        let mut broadcast = ThemeBroadcast::new();
        broadcast.refresh(56, 1.0, &base);
        let start = broadcast.state().clone();

        broadcast.refresh(112, 1.0, &base);
        assert!(!broadcast.state().same_as(&start), "a relayout changes the tile edge");

        broadcast.refresh(56, 2.0, &base);
        assert!(!broadcast.state().same_as(&start), "a scale change");

        broadcast.refresh(56, 1.0, &theme("amber-phosphor"));
        assert!(!broadcast.state().same_as(&start), "a different theme id");

        // The one a cache keyed on the id alone would miss, and the
        // whole reason `theme_toml` exists: a palette that changed
        // without its name changing.
        let mut repainted = base.clone();
        repainted.tile.fill = wm_theme::model::Fill::Solid(wm_theme::model::Color::rgb(1, 2, 3));
        broadcast.refresh(56, 1.0, &repainted);
        assert_eq!(broadcast.state().theme_id, start.theme_id, "same id...");
        assert!(!broadcast.state().same_as(&start), "...different palette, and the dockapp has to be told");
    }

    #[test]
    fn a_scale_no_tile_can_be_drawn_at_never_reaches_the_wire() {
        // The shell is the *sender*, so the codec's `BadFloat` check
        // cannot cover it. Without this clamp a bad `Desktop::scale`
        // would produce a `Welcome` every dockapp rejects, and the SDK
        // would reconnect: a launch loop the user sees as a dock full of
        // tiles that never appear.
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0, -1.0, 1e30, chonk_dock_proto::MAX_SCALE + 0.1] {
            let mut broadcast = ThemeBroadcast::new();
            broadcast.refresh(56, bad, &theme("nextstep-classic"));
            let bytes = ServerMessage::Welcome(broadcast.state().clone()).encode().expect("encodable");
            assert!(ServerMessage::decode(&bytes).is_ok(), "the shell must not send a scale of {bad}, which its own peer would refuse");
            assert_eq!(broadcast.state().scale, 1.0);
        }
        for good in [0.5f32, 1.0, 1.5, 2.0, chonk_dock_proto::MAX_SCALE] {
            let mut broadcast = ThemeBroadcast::new();
            broadcast.refresh(56, good, &theme("nextstep-classic"));
            assert_eq!(broadcast.state().scale, good, "a real session's scale is passed through untouched");
        }
    }
}

/// The instrument panel's wire contract, end to end over a real socket.
///
/// Same shape as [`restart_tests`]: the shell half is the *real* one —
/// a real `SeqpacketListener`, a real `DockHost`, a real `RemoteTile`,
/// the real `admit` and the real servicing pass — and the dockapp is a
/// scripted peer doing exactly what a conformant panel client does:
/// handshake, read the version advert, open a panel, stream banded
/// frames, receive input, get dismissed. What a unit test on
/// `RemoteTile` cannot see (and these can) is the socket seam itself:
/// that every reply actually reaches the wire, in order, on the same
/// connection.
#[cfg(test)]
mod panel_wire_tests {
    use super::*;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use chonk_dock_proto::handshake::hello;
    use chonk_dock_proto::transport::mint_token;
    use chonk_dock_proto::wire::{InputEvent, InputKind, InputMask, PanelCloseReason};

    use crate::dockapp::registry::{DockappEntry, RestartPolicy};
    use crate::dockapp::tile::{RemoteTile, ServiceContext, TileState};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("chonk-panel-wire-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
            Self(dir)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("dock-test.sock")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// One shell: host, one registered tile, and the servicing pass its
    /// event loop runs — including the panel bounds the desktop would
    /// compute from its workarea.
    struct Harness {
        host: DockHost,
        tile: RemoteTile,
        scratch: Vec<u8>,
        theme: ThemeState,
    }

    impl Harness {
        fn start(socket: &Path, now: Instant) -> Self {
            let host = DockHost::bind_at(socket.to_path_buf());
            let entry = DockappEntry {
                id: "gauge".to_string(),
                name: "GGE".to_string(),
                exec: vec!["/nonexistent/chonk-test-gauge".to_string()],
                tile_units: 1,
                restart: RestartPolicy::Never,
                source: PathBuf::from("/test/gauge.dockapp"),
            };
            let theme = ThemeState {
                tile_px: 56,
                scale: 1.0,
                proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION,
                theme_id: "nextstep-classic".into(),
                theme_toml: String::new(),
            };
            Self { host, tile: RemoteTile::new(entry, 56, now), scratch: Vec::new(), theme }
        }

        fn pass(&mut self, now: Instant) {
            for admission in self.host.service(now) {
                admit(std::iter::once(&mut self.tile), admission, &self.theme, now);
            }
            let socket_path = self.host.socket_path().clone();
            let mut ctx = ServiceContext {
                now,
                theme: &self.theme,
                socket_path: &socket_path,
                scratch: &mut self.scratch,
                panel_bounds: (800, 600),
            };
            self.tile.service(&mut ctx);
        }
    }

    fn drain(peer: &Seqpacket) -> Vec<ServerMessage> {
        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        let mut messages = Vec::new();
        loop {
            match peer.recv(&mut buffer) {
                Ok(0) => return messages,
                Ok(n) => messages.push(ServerMessage::decode(&buffer[..n]).expect("decodable")),
                Err(_) => return messages,
            }
        }
    }

    /// The scripted client's whole session, the way the concept demo
    /// runs it: connect, prove the shell advertises panels, open one,
    /// stream a banded repaint, receive input, be dismissed — with the
    /// tile alive and drawing throughout.
    #[test]
    fn a_scripted_dockapp_opens_streams_receives_input_and_is_dismissed() {
        let scratch = Scratch::new();
        let base = Instant::now();
        let mut shell = Harness::start(&scratch.socket(), base);
        let token = mint_token().unwrap();
        shell.tile.pretend_launched(token, base);

        // -- handshake, and the version probe ---------------------------
        let peer = Seqpacket::connect(&scratch.socket()).expect("connect");
        peer.send(&hello("gauge", 1, token, InputMask::all()).encode().unwrap()).unwrap();
        shell.pass(base);
        let welcome = drain(&peer)
            .into_iter()
            .find_map(|message| match message {
                ServerMessage::Welcome(state) => Some(state),
                _ => None,
            })
            .expect("welcomed");
        assert!(welcome.panels_supported(), "the Welcome advertises protocol {} — the probe a client reads before OpenPanel", welcome.proto);

        // -- the tile draws, then asks for its panel --------------------
        peer.send(&ClientMessage::Frame { generation: 1, width: 56, height: 56, pixels: vec![9; 56 * 56 * 4] }.encode().unwrap()).unwrap();
        peer.send(&ClientMessage::OpenPanel { width: 4000, height: 120 }.encode().unwrap()).unwrap();
        shell.pass(base + Duration::from_millis(16));
        assert_eq!(
            drain(&peer),
            vec![ServerMessage::PanelOpened { width: 800, height: 120 }],
            "the grant is the request clamped to the workarea, on the wire"
        );

        // -- a banded repaint -------------------------------------------
        for (step, y) in (0..120u32).step_by(40).enumerate() {
            let band = ClientMessage::PanelFrame {
                generation: 1,
                y,
                band_height: 40,
                width: 800,
                pixels: vec![step as u8 + 1; 800 * 40 * 4],
            };
            peer.send(&band.encode().unwrap()).unwrap();
        }
        shell.pass(base + Duration::from_millis(32));
        let frame = shell.tile.panel_frame().expect("all three bands assembled");
        assert_eq!((frame.width, frame.height), (800, 120));
        assert_eq!(frame.pixels[0], 1, "top band");
        assert_eq!(frame.pixels[(800 * 119 * 4) + 3], 3, "bottom band");

        // -- input flows back as PanelInput -----------------------------
        let press = InputEvent { kind: InputKind::Press, button: Some(chonk_dock_proto::wire::Button::Left), x: 700, y: 90, delta: 0 };
        let motion = InputEvent { kind: InputKind::Motion, button: None, x: 701, y: 91, delta: 0 };
        shell.tile.panel_input(press, base);
        shell.tile.panel_input(motion, base);
        assert_eq!(
            drain(&peer),
            vec![ServerMessage::PanelInput(press), ServerMessage::PanelInput(motion)],
            "panel input — the panel-only Motion included — reaches the wire in order"
        );

        // -- the user clicks away ---------------------------------------
        shell.tile.close_panel(PanelCloseReason::Dismissed, base);
        assert_eq!(drain(&peer), vec![ServerMessage::PanelClosed { reason: PanelCloseReason::Dismissed }]);
        assert!(shell.tile.poll_fd().is_some(), "the dismissal cost the panel, not the tile");
        assert_eq!(shell.tile.state(), TileState::Live, "which is still drawing");

        // -- and a late band for the closed panel is quietly nothing ----
        let late = ClientMessage::PanelFrame { generation: 2, y: 0, band_height: 1, width: 800, pixels: vec![7; 800 * 4] };
        peer.send(&late.encode().unwrap()).unwrap();
        shell.pass(base + Duration::from_millis(48));
        assert!(shell.tile.poll_fd().is_some(), "streaming against an unseen PanelClosed is a race, not a crime");
        assert!(drain(&peer).is_empty(), "and provokes no reply");
    }
}

/// Restart survival, end to end over a real socket.
///
/// The shell half here is the *real* one — a real `SeqpacketListener`, a
/// real `DockHost`, real `RemoteTile`s, and the real `admit` and
/// `shut_down` the event loop calls. Only two things are stood in for:
/// the dockapp's process (a `Seqpacket` this test drives directly, doing
/// exactly what `chonk_ui::dockapp` does — `tests/dockapp_conformance.rs`
/// is the proof of that, running the real SDK loop against a
/// hand-written shell), and the `fork`/`exec` (`pretend_launched`).
///
/// The sequence being reproduced is the one a user performs when they
/// pick a theme: shell, dockapp, `exec`, second shell, same dockapp.
#[cfg(test)]
mod restart_tests {
    use super::*;
    use std::path::Path;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    use chonk_dock_proto::handshake::hello;
    use chonk_dock_proto::transport::mint_token;
    use chonk_dock_proto::wire::InputMask;
    use chonk_dock_proto::TOKEN_BYTES;

    use crate::dockapp::registry::{DockappEntry, RestartPolicy};
    use crate::dockapp::tile::{RemoteTile, ServiceContext, TileState};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("chonk-dock-restart-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
            Self(dir)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("dock-test.sock")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn entry(id: &str) -> DockappEntry {
        DockappEntry {
            id: id.to_string(),
            name: "TST".to_string(),
            // Deliberately not runnable: every one of these tests
            // asserts that no fresh process was launched, and a launch
            // that failed to spawn is visible in the tile's state.
            exec: vec!["/nonexistent/chonk-test-dockapp".to_string()],
            tile_units: 1,
            restart: RestartPolicy::Always,
            source: PathBuf::from("/test/test.dockapp"),
        }
    }

    fn state(tile_px: u32, theme_id: &str) -> ThemeState {
        ThemeState { tile_px, scale: 1.0, proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION, theme_id: theme_id.into(), theme_toml: String::new() }
    }

    /// One shell instance: its listener, its tiles, and the servicing
    /// pass its event loop runs. `Desktop` is the same code plus a
    /// backend, which a unit test cannot have.
    struct FakeShell {
        host: DockHost,
        tiles: Vec<RemoteTile>,
        scratch: Vec<u8>,
    }

    impl FakeShell {
        /// Starts a shell the way `Desktop::new` does: bind, read
        /// whatever the previous one handed over, and give an inherited
        /// token to the tile it names.
        fn start(socket: &Path, ids: &[&str], now: Instant) -> Self {
            let host = DockHost::bind_at(socket.to_path_buf());
            let mut inherited = host.handoff_path().map(|path| handoff::take(&path)).unwrap_or_default();
            let tiles = ids
                .iter()
                .map(|id| {
                    let mut tile = RemoteTile::new(entry(id), 56, now);
                    if let Some(token) = inherited.remove(*id) {
                        tile.rejoin(token, now);
                    }
                    tile
                })
                .collect();
            Self { host, tiles, scratch: Vec::new() }
        }

        fn pass(&mut self, now: Instant, theme: &ThemeState) {
            for admission in self.host.service(now) {
                admit(self.tiles.iter_mut(), admission, theme, now);
            }
            let socket_path = self.host.socket_path().clone();
            let mut ctx = ServiceContext { now, theme, socket_path: &socket_path, scratch: &mut self.scratch, panel_bounds: (1024, 1024) };
            for tile in &mut self.tiles {
                tile.service(&mut ctx);
            }
        }

        fn finish(&mut self, farewell: Farewell) {
            let path = self.host.handoff_path();
            shut_down(self.tiles.iter_mut(), path.as_deref(), farewell);
        }
    }

    /// Connects and says `Hello` exactly as `chonk_ui::dockapp` does.
    fn dockapp_connects(socket: &Path, id: &str, token: [u8; TOKEN_BYTES]) -> Seqpacket {
        let peer = Seqpacket::connect(socket).expect("a dockapp can reach the dock socket");
        peer.send(&hello(id, 1, token, InputMask::all()).encode().unwrap()).expect("Hello");
        peer
    }

    /// Everything queued for the dockapp right now, plus whether the
    /// socket has reached EOF. Both halves matter: the shell interleaves
    /// `Ping`s with anything a test is looking for, and one test's whole
    /// assertion is about which of "a `Goodbye`" and "a bare EOF" arrived.
    /// [`drain`], but giving a close a bounded moment to become
    /// visible before concluding it has not happened.
    ///
    /// `drain` reads a non-blocking socket, where "nothing to read
    /// right now" and "the peer is gone" are the same `EAGAIN` until
    /// the kernel has actually torn the far end down. That teardown is
    /// not synchronous with the `drop` that triggers it: on a loaded
    /// machine the reading thread can reach its `recv` first and
    /// conclude the socket is still open. This turns the assertion from
    /// "is it closed at this instant", which nothing guarantees, into
    /// "does it close at all", which is the property being tested.
    ///
    /// Found the hard way: the one caller failed about one run in three
    /// once the suite got heavy enough to contend for CPU.
    fn drain_until_eof(peer: &Seqpacket) -> (Vec<ServerMessage>, bool) {
        let mut collected = Vec::new();
        for _ in 0..500 {
            let (messages, eof) = drain(peer);
            collected.extend(messages);
            if eof {
                return (collected, true);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        (collected, false)
    }

    fn drain(peer: &Seqpacket) -> (Vec<ServerMessage>, bool) {
        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        let mut messages = Vec::new();
        loop {
            match peer.recv(&mut buffer) {
                Ok(0) => return (messages, true),
                Ok(n) => messages.push(ServerMessage::decode(&buffer[..n]).expect("decodable")),
                // `connect` returns an `O_NONBLOCK` socket, so this is
                // "nothing more right now" rather than a wait.
                Err(_) => return (messages, false),
            }
        }
    }

    fn goodbye_reason(messages: &[ServerMessage]) -> Option<GoodbyeReason> {
        messages.iter().find_map(|message| match message {
            ServerMessage::Goodbye { reason } => Some(*reason),
            _ => None,
        })
    }

    fn welcomed_with(peer: &Seqpacket) -> Option<ThemeState> {
        drain(peer).0.into_iter().find_map(|message| match message {
            ServerMessage::Welcome(state) => Some(state),
            _ => None,
        })
    }

    /// **The deliverable, stated as a test.** A dockapp outlives the
    /// shell that launched it and is readopted by its replacement — not
    /// killed, not relaunched, not doubled.
    #[test]
    fn a_dockapp_survives_the_shells_own_restart_and_is_readopted() {
        let scratch = Scratch::new();
        let socket = scratch.socket();
        let base = Instant::now();
        let at_56 = state(56, "nextstep-classic");

        // --- the shell the user is running -----------------------------
        let mut first = FakeShell::start(&socket, &["clock"], base);
        let token = mint_token().unwrap();
        first.tiles[0].pretend_launched(token, base);
        let peer = dockapp_connects(&socket, "clock", token);
        first.pass(base, &at_56);
        assert!(welcomed_with(&peer).is_some(), "the dockapp is admitted and welcomed");

        peer.send(&ClientMessage::Frame { generation: 1, width: 56, height: 56, pixels: vec![3; 56 * 56 * 4] }.encode().unwrap()).unwrap();
        first.pass(base + Duration::from_millis(16), &at_56);
        assert_eq!(first.tiles[0].state(), TileState::Live, "and is drawing");

        // --- the user picks a theme, so the shell re-execs --------------
        first.finish(Farewell::Restarting);
        drop(first); // `exec`: the listener and every fd go with the image

        // The dockapp sees a bare EOF, which is the SDK's signal to retry
        // rather than exit. `Goodbye { Shutdown }` would have told it the
        // opposite, which is why the restart path deliberately sends
        // nothing.
        let (parting, eof) = drain_until_eof(&peer);
        assert!(eof, "the socket really is gone");
        assert_eq!(goodbye_reason(&parting), None, "a bare EOF means \"try again\"; a Goodbye would have told it the opposite");
        drop(peer);

        // --- the replacement shell -------------------------------------
        let later = base + Duration::from_millis(120);
        let after = state(56, "amber-phosphor");
        let mut second = FakeShell::start(&socket, &["clock"], later);
        assert!(
            matches!(second.tiles[0].state(), TileState::Rejoining { .. }),
            "the slot is held open for the survivor, not filled with a second copy"
        );

        second.pass(later, &after);
        assert!(matches!(second.tiles[0].state(), TileState::Rejoining { .. }), "and nothing is launched while it waits");

        // The survivor knocks again, with the token it was given at
        // launch by a process that no longer exists.
        let peer = dockapp_connects(&socket, "clock", token);
        second.pass(later + Duration::from_millis(16), &after);
        assert!(second.tiles[0].poll_fd().is_some(), "readopted into the existing tile");
        assert!(second.tiles[0].pid().is_none(), "and no second process was ever spawned");
        assert_eq!(
            welcomed_with(&peer).map(|state| state.theme_id),
            Some("amber-phosphor".to_string()),
            "welcomed with the new shell's theme, so a restart-for-a-theme-pick lands the theme too"
        );

        peer.send(&ClientMessage::Frame { generation: 2, width: 56, height: 56, pixels: vec![4; 56 * 56 * 4] }.encode().unwrap()).unwrap();
        second.pass(later + Duration::from_millis(32), &after);
        assert_eq!(second.tiles[0].state(), TileState::Live, "the same process, drawing into the same slot");
    }

    /// A session that is genuinely ending stops its dockapps and leaves
    /// nothing for the next login to adopt. The two farewells have to be
    /// different or one of them is wrong.
    #[test]
    fn a_session_that_ends_stops_its_dockapps_and_leaves_no_tokens_behind() {
        let scratch = Scratch::new();
        let socket = scratch.socket();
        let base = Instant::now();
        let at_56 = state(56, "nextstep-classic");

        let mut shell = FakeShell::start(&socket, &["clock"], base);
        let token = mint_token().unwrap();
        shell.tiles[0].pretend_launched(token, base);
        let peer = dockapp_connects(&socket, "clock", token);
        shell.pass(base, &at_56);
        assert!(welcomed_with(&peer).is_some());

        shell.finish(Farewell::SessionOver);
        // `Goodbye { Shutdown }` is actionable: it says "reconnecting is
        // pointless", where the bare EOF of a restart says "try again".
        assert_eq!(goodbye_reason(&drain(&peer).0), Some(GoodbyeReason::Shutdown));

        let handoff = shell.host.handoff_path().unwrap();
        assert!(!handoff.exists(), "a logout must not leave credentials for a session that is over");
        drop(shell);

        let next = FakeShell::start(&socket, &["clock"], base);
        assert!(matches!(next.tiles[0].state(), TileState::Waiting { .. }), "the next login launches its own, immediately");
    }

    /// **A valid token does not displace a connected tile.**
    ///
    /// The token proves "you were launched for this slot", not "you are
    /// the process currently drawing it" — it is a shared secret in an
    /// environment any process of this user can read. Displacing on a
    /// match would let a second instance of a dockapp silently steal a
    /// working tile, and hand anything that could read the token a
    /// takeover at a moment of its choosing. The incumbent keeps the
    /// slot; the challenger is told `Replaced` and exits.
    #[test]
    fn a_valid_token_does_not_displace_a_dockapp_that_is_already_connected() {
        let scratch = Scratch::new();
        let socket = scratch.socket();
        let base = Instant::now();
        let at_56 = state(56, "nextstep-classic");

        let mut shell = FakeShell::start(&socket, &["clock"], base);
        let token = mint_token().unwrap();
        shell.tiles[0].pretend_launched(token, base);
        let incumbent = dockapp_connects(&socket, "clock", token);
        shell.pass(base, &at_56);
        assert!(welcomed_with(&incumbent).is_some());
        let held = shell.tiles[0].poll_fd();

        // A second process with a perfectly good copy of the token.
        let challenger = dockapp_connects(&socket, "clock", token);
        shell.pass(base + Duration::from_millis(16), &at_56);

        assert_eq!(
            goodbye_reason(&drain(&challenger).0),
            Some(GoodbyeReason::Replaced),
            "a valid token is not a claim on an occupied slot"
        );
        assert_eq!(shell.tiles[0].poll_fd(), held, "and the tile that was working keeps working");

        incumbent.send(&ClientMessage::Frame { generation: 1, width: 56, height: 56, pixels: vec![1; 56 * 56 * 4] }.encode().unwrap()).unwrap();
        shell.pass(base + Duration::from_millis(32), &at_56);
        assert_eq!(shell.tiles[0].state(), TileState::Live);
    }

    /// The reconnect path widens *which tile states* accept a `Hello`.
    /// It does not weaken what a `Hello` has to prove. A held-open slot
    /// still refuses a token it did not inherit — otherwise the handoff
    /// would be a ten-second authentication hole at every theme pick.
    #[test]
    fn a_held_open_slot_still_refuses_a_token_it_did_not_inherit() {
        let scratch = Scratch::new();
        let socket = scratch.socket();
        let base = Instant::now();
        let at_56 = state(56, "nextstep-classic");

        let real = mint_token().unwrap();
        handoff::write(&handoff::beside(&socket).unwrap(), &[("clock".into(), real)]);
        let mut shell = FakeShell::start(&socket, &["clock"], base);
        assert!(matches!(shell.tiles[0].state(), TileState::Rejoining { .. }));

        let mut guessed = real;
        guessed[0] ^= 0xFF;
        let impostor = dockapp_connects(&socket, "clock", guessed);
        shell.pass(base, &at_56);

        assert_eq!(goodbye_reason(&drain(&impostor).0), Some(GoodbyeReason::Unauthorized));
        assert!(shell.tiles[0].poll_fd().is_none(), "the slot was not given away");
        assert!(matches!(shell.tiles[0].state(), TileState::Rejoining { .. }), "and is still being held for the one that can prove it");

        // ...and the real survivor still gets in afterwards.
        let survivor = dockapp_connects(&socket, "clock", real);
        shell.pass(base + Duration::from_millis(16), &at_56);
        assert!(welcomed_with(&survivor).is_some());
    }

    /// A dockapp that stopped answering is not left behind by a restart.
    /// It would not notice the EOF, would never reconnect, and nothing
    /// would ever collect it — an orphan for the rest of the login.
    #[test]
    fn a_hung_dockapp_is_stopped_by_a_restart_rather_than_handed_forward() {
        let scratch = Scratch::new();
        let socket = scratch.socket();
        let base = Instant::now();
        let at_56 = state(56, "nextstep-classic");

        let mut shell = FakeShell::start(&socket, &["clock"], base);
        let token = mint_token().unwrap();
        shell.tiles[0].pretend_launched(token, base);
        let peer = dockapp_connects(&socket, "clock", token);
        shell.pass(base, &at_56);
        assert!(welcomed_with(&peer).is_some());

        // Stops answering pings without closing its socket, which is
        // exactly what `TileState::Hung` means.
        for step in 1..=4 {
            shell.pass(base + Duration::from_secs(2 * step), &at_56);
        }
        assert!(matches!(shell.tiles[0].state(), TileState::Hung { .. }));

        shell.finish(Farewell::Restarting);
        let handoff = shell.host.handoff_path().unwrap();
        assert!(!handoff.exists(), "nothing was handed forward, so the next shell launches a live tile instead");
    }

    /// The handoff is consumed, not kept. A file that survived would be
    /// read again at the *next* restart, holding a slot open for a
    /// process that stopped existing two restarts ago.
    #[test]
    fn a_handoff_is_spent_by_the_shell_that_reads_it() {
        let scratch = Scratch::new();
        let socket = scratch.socket();
        let base = Instant::now();
        let token = mint_token().unwrap();
        handoff::write(&handoff::beside(&socket).unwrap(), &[("clock".into(), token)]);

        let first = FakeShell::start(&socket, &["clock"], base);
        assert!(matches!(first.tiles[0].state(), TileState::Rejoining { .. }));
        drop(first);

        let second = FakeShell::start(&socket, &["clock"], base);
        assert!(matches!(second.tiles[0].state(), TileState::Waiting { .. }), "the token was spent; this tile launches its own");
    }

    /// An id nobody registered gets nothing, handoff or no handoff. The
    /// registry is still the only source of dock slots.
    #[test]
    fn a_handoff_for_an_id_that_is_no_longer_registered_is_ignored() {
        let scratch = Scratch::new();
        let socket = scratch.socket();
        let base = Instant::now();
        let at_56 = state(56, "nextstep-classic");
        let token = mint_token().unwrap();
        // The user uninstalled `clock` between the two shells.
        handoff::write(&handoff::beside(&socket).unwrap(), &[("clock".into(), token)]);

        let mut shell = FakeShell::start(&socket, &["net"], base);
        let survivor = dockapp_connects(&socket, "clock", token);
        shell.pass(base, &at_56);
        assert_eq!(goodbye_reason(&drain(&survivor).0), Some(GoodbyeReason::Unauthorized));
    }
}
