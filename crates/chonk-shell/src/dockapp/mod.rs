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

pub(crate) mod registry;
pub(crate) mod tile;

use std::os::fd::{AsRawFd, RawFd};
use std::path::PathBuf;
use std::time::Instant;

use chonk_dock_proto::handshake::HANDSHAKE_TIMEOUT;
use chonk_dock_proto::transport::{Seqpacket, SeqpacketListener};
use chonk_dock_proto::wire::GoodbyeReason;
use chonk_dock_proto::{ClientMessage, ServerMessage, MAX_MESSAGE_BYTES};

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
                return Self { listener: None, socket_path: PathBuf::new(), pending: Vec::new(), scratch: Vec::new() };
            }
        };
        match SeqpacketListener::bind(&socket_path) {
            Ok(listener) => {
                tracing::info!(socket = %socket_path.display(), "dockapp socket listening");
                Self { listener: Some(listener), socket_path, pending: Vec::new(), scratch: Vec::new() }
            }
            Err(error) => {
                tracing::warn!(?error, socket = %socket_path.display(), "could not bind the dockapp socket; dockapps are unavailable this session");
                Self { listener: None, socket_path, pending: Vec::new(), scratch: Vec::new() }
            }
        }
    }

    pub(crate) fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub(crate) fn is_listening(&self) -> bool {
        self.listener.is_some()
    }

    pub(crate) fn scratch(&mut self) -> &mut Vec<u8> {
        &mut self.scratch
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
