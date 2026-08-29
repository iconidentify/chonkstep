//! The socket: a `SOCK_SEQPACKET` Unix listener for the shell, a
//! connector for the dockapp, and the permission and non-blocking rules
//! both sides depend on.
//!
//! # Why SEQPACKET
//!
//! `SOCK_SEQPACKET` is connection-oriented like a stream — so a dead
//! peer is an EOF on the same event-loop pass, no timeout, no
//! guessing — but it preserves message boundaries like a datagram, so
//! there is no length-prefix parser to get wrong. Those are exactly the
//! two properties this protocol needs, and it is the only socket type
//! that has both. It is also, notably, not reachable from `std`:
//! `std::os::unix::net` creates `SOCK_STREAM` and `SOCK_DGRAM` only,
//! which is why this module talks to `libc` directly.
//!
//! # Why every fd here is non-blocking
//!
//! Read [`SeqpacketListener::accept`] and [`Seqpacket::send`]. In
//! short: a blocking `write()` to a dockapp that stopped calling
//! `recv()` parks the compositor's repaint thread, which is the bug
//! this architecture exists to make impossible. The property is
//! established three independent ways — `SOCK_NONBLOCK` at socket
//! creation, `accept4(SOCK_NONBLOCK)` at accept, and `MSG_DONTWAIT` on
//! every send — and asserted by tests, because "we were careful" is not
//! a mechanism.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{MAX_MESSAGE_BYTES, TOKEN_BYTES};

/// Where the shell tells a dockapp to connect.
pub const ENV_SOCKET: &str = "CHONKSTEP_DOCK_SOCKET";
/// The 128-bit nonce, lowercase hex, the dockapp must echo in `Hello`.
pub const ENV_TOKEN: &str = "CHONKSTEP_DOCK_TOKEN";

/// Listener backlog. A dockapp connects once at startup and once more
/// per reconnect; sixteen is "far more than can plausibly be pending"
/// while still being a bound.
const BACKLOG: libc::c_int = 16;

// ---------------------------------------------------------------------
// libc plumbing
// ---------------------------------------------------------------------

fn cvt(ret: libc::c_int) -> io::Result<libc::c_int> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret)
    }
}

fn cvt_size(ret: libc::ssize_t) -> io::Result<usize> {
    if ret < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(ret as usize)
    }
}

/// Builds a `sockaddr_un` for a pathname socket.
///
/// The 108-byte `sun_path` is a real, low limit that bites in practice
/// (`$XDG_RUNTIME_DIR` is usually `/run/user/1000`, leaving plenty, but
/// a test or a container can be deeper), so it is a checked error with
/// a message naming the path rather than a silent truncation to a
/// *different* socket than the caller asked for.
fn sockaddr_un(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.len() >= addr.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("socket path is {} bytes, the kernel's limit is {}: {}", bytes.len(), addr.sun_path.len() - 1, path.display()),
        ));
    }
    if bytes.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket path contains a NUL byte"));
    }
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (slot, &byte) in addr.sun_path.iter_mut().zip(bytes) {
        *slot = byte as libc::c_char;
    }
    Ok((addr, std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t))
}

/// Asks the kernel for socket buffers big enough that a whole tile
/// fits in one datagram with room to queue a few.
///
/// Best effort on purpose. `AF_UNIX` refuses a datagram larger than
/// `SO_SNDBUF - 32`, and `SO_SNDBUF` is silently clamped to
/// `net.core.wmem_max` (stock: 212992, which the kernel then doubles),
/// so asking for a megabyte reliably yields "as much as this kernel
/// allows" rather than an error. [`MAX_MESSAGE_BYTES`] is set below
/// even the un-tuned floor, so a failure here costs throughput
/// headroom, never correctness — which is why it does not propagate.
fn widen_socket_buffers(fd: RawFd) {
    let want = (2 * MAX_MESSAGE_BYTES) as libc::c_int;
    for option in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                (&want as *const libc::c_int).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }
}

// ---------------------------------------------------------------------
// Socket paths and permissions
// ---------------------------------------------------------------------

/// `$XDG_RUNTIME_DIR/chonkstep`, the directory the dock socket lives in.
///
/// `$XDG_RUNTIME_DIR` with no fallback, deliberately. It is the one
/// directory the spec guarantees is owned by this user, mode 0700, and
/// cleaned up at logout. Falling back to `/tmp` — the obvious
/// convenience — would put an authentication-bearing socket in a
/// world-writable directory where any local process can win a create
/// race against it. A session without `$XDG_RUNTIME_DIR` gets a clear
/// error and no dockapps, which is the correct amount of dockapps.
pub fn socket_dir() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is not set; dockapps need a private per-user runtime directory and will not fall back to /tmp",
        )
    })?;
    Ok(PathBuf::from(runtime).join("chonkstep"))
}

/// Reduces a display name to something safe to put in a filename.
///
/// The X11 form is `:1` and the Wayland form is `wayland-1`; both may
/// arrive from the environment, which makes them attacker-influenced in
/// the same weak sense as any inherited variable. Anything outside
/// `[A-Za-z0-9_-]` becomes `_`, so no separator, no `/`, and no `..`
/// can reach the path join below.
pub fn sanitize_display(display: &str) -> String {
    let cleaned: String = display
        .trim_start_matches(':')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .take(32)
        .collect();
    if cleaned.is_empty() {
        "default".to_string()
    } else {
        cleaned
    }
}

/// `$XDG_RUNTIME_DIR/chonkstep/dock-<display>.sock`.
///
/// Per-display rather than per-session-pid so the path is *stable*
/// across a shell restart: a dockapp that sees EOF when the shell
/// restarts reconnects to the same name and is adopted by the new
/// shell. A pid in the path would make restart survival impossible by
/// construction.
pub fn socket_path(display: &str) -> io::Result<PathBuf> {
    Ok(socket_dir()?.join(format!("dock-{}.sock", sanitize_display(display))))
}

/// Creates the socket directory 0700, or verifies an existing one.
///
/// The verification is the point. The socket file gets `chmod 0600`
/// after `bind()`, but `bind()` itself creates it with the process
/// umask applied — a window, however brief, in which the socket may be
/// group- or world-accessible. A 0700 *directory* closes that window
/// properly: no other user can traverse into it to reach the socket at
/// any mode. So a pre-existing directory that is not a directory, not
/// ours, or not 0700 is a hard error rather than something to fix up
/// silently, because any of those means something unexpected is already
/// living at this path.
pub fn ensure_socket_dir(dir: &Path) -> io::Result<()> {
    let c_dir = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket directory path contains a NUL byte"))?;
    let made = unsafe { libc::mkdir(c_dir.as_ptr(), 0o700) };
    if made < 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::AlreadyExists {
            return Err(err);
        }
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        cvt(unsafe { libc::lstat(c_dir.as_ptr(), &mut stat) })?;
        let is_dir = stat.st_mode & libc::S_IFMT == libc::S_IFDIR;
        let is_ours = stat.st_uid == unsafe { libc::geteuid() };
        let is_private = stat.st_mode & 0o077 == 0;
        if !is_dir || !is_ours || !is_private {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} exists but is not a private directory owned by this user (dir={is_dir}, ours={is_ours}, mode={:o})",
                    dir.display(),
                    stat.st_mode & 0o777
                ),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Tokens
// ---------------------------------------------------------------------

/// Mints a fresh 128-bit connection token.
///
/// `getrandom(2)` with a `/dev/urandom` fallback for kernels or
/// sandboxes that block the syscall. Never a PRNG seeded from the
/// clock: this value is the credential a dockapp presents, and a
/// guessable one turns "processes of this user" into "processes of this
/// user that also know when the shell started".
pub fn mint_token() -> io::Result<[u8; TOKEN_BYTES]> {
    let mut token = [0u8; TOKEN_BYTES];
    let filled = unsafe { libc::getrandom(token.as_mut_ptr().cast(), token.len(), 0) };
    if filled == token.len() as libc::ssize_t {
        return Ok(token);
    }
    let bytes = std::fs::read("/dev/urandom")?;
    if bytes.len() < TOKEN_BYTES {
        return Err(io::Error::other("could not read 16 random bytes"));
    }
    token.copy_from_slice(&bytes[..TOKEN_BYTES]);
    Ok(token)
}

pub fn token_to_hex(token: &[u8; TOKEN_BYTES]) -> String {
    let mut out = String::with_capacity(TOKEN_BYTES * 2);
    for byte in token {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

pub fn token_from_hex(hex: &str) -> Option<[u8; TOKEN_BYTES]> {
    let hex = hex.trim();
    if hex.len() != TOKEN_BYTES * 2 {
        return None;
    }
    let mut token = [0u8; TOKEN_BYTES];
    for (slot, pair) in token.iter_mut().zip(hex.as_bytes().as_chunks::<2>().0) {
        let text = std::str::from_utf8(pair).ok()?;
        *slot = u8::from_str_radix(text, 16).ok()?;
    }
    Some(token)
}

/// Compares two tokens in time independent of how many leading bytes
/// match.
///
/// The realistic attack this closes is thin — the attacker would need
/// to already be able to connect to a 0600 socket in a 0700 directory,
/// and would be measuring across a process boundary — but a constant
/// time comparison of sixteen bytes costs nothing, and "we decided the
/// timing side channel was probably fine" is not a sentence worth
/// having to defend later.
pub fn tokens_match(a: &[u8; TOKEN_BYTES], b: &[u8; TOKEN_BYTES]) -> bool {
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ---------------------------------------------------------------------
// Seqpacket
// ---------------------------------------------------------------------

/// The peer's credentials, from `SO_PEERCRED`. Unforgeable — the kernel
/// fills them in, not the peer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerCredentials {
    pub pid: i32,
    pub uid: u32,
    pub gid: u32,
}

/// One connected `SOCK_SEQPACKET` socket, either end.
#[derive(Debug)]
pub struct Seqpacket {
    fd: OwnedFd,
}

impl Seqpacket {
    /// Connects to a shell's dock socket. Used by the SDK.
    ///
    /// Connects in blocking mode and switches to non-blocking after:
    /// an `AF_UNIX` `connect()` to a listening socket completes without
    /// a round trip, so there is no latency to save, and it avoids the
    /// `EINPROGRESS` dance for no benefit.
    pub fn connect(path: &Path) -> io::Result<Self> {
        let (addr, len) = sockaddr_un(path)?;
        let raw = cvt(unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) })?;
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        widen_socket_buffers(fd.as_raw_fd());
        cvt(unsafe { libc::connect(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;
        let socket = Self { fd };
        socket.set_nonblocking(true)?;
        Ok(socket)
    }

    /// Adopts an already-connected fd (what [`SeqpacketListener::accept`]
    /// produces).
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }

    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let flags = cvt(unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) })?;
        let updated = if nonblocking { flags | libc::O_NONBLOCK } else { flags & !libc::O_NONBLOCK };
        cvt(unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFL, updated) })?;
        Ok(())
    }

    /// Whether `O_NONBLOCK` is actually set on this fd right now.
    ///
    /// Public because it is the subject of an assertion, not because
    /// anything needs to branch on it: `accept_returns_a_socket_that_
    /// cannot_block` in this module's tests reads it directly. The
    /// property is worth a public accessor precisely because losing it
    /// silently is the worst bug this codebase knows how to have.
    pub fn is_nonblocking(&self) -> io::Result<bool> {
        let flags = cvt(unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) })?;
        Ok(flags & libc::O_NONBLOCK != 0)
    }

    /// Sends one message as one datagram.
    ///
    /// THIS IS THE CALL THAT MUST NEVER BLOCK. A dockapp is an
    /// untrusted process; it can stop calling `recv()` by crashing, by
    /// deadlocking, by being `SIGSTOP`ped, or on purpose. Once its
    /// socket buffer fills, a blocking `send()` here would park
    /// whichever thread made it until the dockapp read something —
    /// which might be never. On the shell's side that thread is the
    /// single repaint thread: the desktop would stop drawing, stop
    /// reading input, and stop collecting page-flip completions, and
    /// the stall watchdog would once again blame the display driver.
    /// That is the incident this whole design was written after, with
    /// `send()` substituted for `nmcli`.
    ///
    /// Two flags, belt and braces, because one line of defense for that
    /// outcome is not enough:
    ///
    /// - `MSG_DONTWAIT` makes *this call* non-blocking regardless of
    ///   the fd's flags, so the property cannot be lost by a future
    ///   caller helpfully clearing `O_NONBLOCK` somewhere else.
    /// - `MSG_NOSIGNAL` suppresses `SIGPIPE`. Writing to a socket whose
    ///   peer has died raises it by default, and the default
    ///   disposition is *terminate the process* — a crashing dockapp
    ///   would take the compositor with it. This turns that into a
    ///   plain `EPIPE` the caller handles like any other disconnect.
    ///
    /// A full buffer surfaces as `ErrorKind::WouldBlock`; that is
    /// [`crate::SendQueue`]'s cue, not an error.
    pub fn send(&self, message: &[u8]) -> io::Result<usize> {
        send_on(self.fd.as_raw_fd(), message)
    }

    /// Receives one whole message. `Ok(0)` means the peer is gone.
    ///
    /// A zero-length datagram is indistinguishable from EOF through
    /// `recv`'s return value, and this treats both as "gone". That
    /// loses nothing: an empty datagram decodes to
    /// [`crate::DecodeError::Empty`], whose only sane response is also
    /// to close the connection. Rather than write a `recvmsg`/`MSG_EOR`
    /// dance to tell two cases apart that have one outcome, the
    /// equivalence is written down here.
    ///
    /// `MSG_TRUNC` is *not* passed and the buffer is
    /// [`MAX_MESSAGE_BYTES`]: a datagram larger than the buffer is
    /// silently truncated by the kernel, which then fails the codec's
    /// length checks. An over-large message is a protocol violation
    /// either way.
    pub fn recv(&self, buffer: &mut [u8]) -> io::Result<usize> {
        cvt_size(unsafe {
            libc::recv(self.fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len(), libc::MSG_DONTWAIT)
        })
    }

    /// `SO_PEERCRED`: who is on the other end, according to the kernel.
    ///
    /// Defence in depth behind the socket's own 0600 mode and the
    /// handshake token. Cheap, unforgeable, and it makes "only this
    /// user's processes" an enforced statement rather than an inference
    /// from filesystem permissions.
    pub fn peer_credentials(&self) -> io::Result<PeerCredentials> {
        let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        cvt(unsafe {
            libc::getsockopt(
                self.fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast(),
                &mut len,
            )
        })?;
        Ok(PeerCredentials { pid: cred.pid, uid: cred.uid, gid: cred.gid })
    }

    /// Whether the peer is this same user. The shell refuses a
    /// connection from anyone else.
    pub fn peer_is_this_user(&self) -> io::Result<bool> {
        Ok(self.peer_credentials()?.uid == unsafe { libc::geteuid() })
    }

    /// Blocks (with `poll`) until one message arrives or `deadline`
    /// passes.
    ///
    /// For the *client* only, and the doc comment is the enforcement:
    /// the SDK's loop has exactly one socket and a redraw deadline, so
    /// a bounded wait is the whole event loop it needs. The shell must
    /// never call this — it has a compositor to run — which is why the
    /// deadline is mandatory rather than an `Option`, so that even a
    /// misuse cannot wait forever.
    ///
    /// `Ok(None)` on timeout, `Ok(Some(0))` on EOF.
    pub fn recv_until(&self, buffer: &mut [u8], deadline: std::time::Instant) -> io::Result<Option<usize>> {
        loop {
            match self.recv(buffer) {
                Ok(n) => return Ok(Some(n)),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            if !wait_readable(self.as_raw_fd(), Some(deadline - now))? {
                // `poll` timed out or was interrupted; loop back so the
                // deadline check above is the single place that gives up.
                continue;
            }
        }
    }

    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsRawFd for Seqpacket {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

// ---------------------------------------------------------------------
// Listener
// ---------------------------------------------------------------------

/// The shell's end: a bound, listening socket that unlinks its path on
/// drop.
#[derive(Debug)]
pub struct SeqpacketListener {
    fd: OwnedFd,
    path: PathBuf,
}

impl SeqpacketListener {
    /// Creates the directory, clears a stale socket, binds, chmods
    /// 0600, and listens.
    ///
    /// The stale-socket handling is the interesting part. A Unix socket
    /// file outlives the process that bound it, so a crashed session
    /// leaves one behind and the next `bind()` fails with `EADDRINUSE`.
    /// Blindly unlinking would let a second live shell steal the first
    /// one's dockapps, so this *connects to it first*: a socket that
    /// accepts a connection has a live owner and is left alone (with an
    /// error naming the situation); one that refuses is debris and is
    /// removed.
    pub fn bind(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            ensure_socket_dir(parent)?;
        }
        Self::clear_stale_socket(path)?;

        let (addr, len) = sockaddr_un(path)?;
        // SOCK_NONBLOCK at creation, so there is no instant in this
        // process's life where the listening fd could block an
        // `accept()` on an event-loop pass that turned out to have no
        // pending connection.
        let raw = cvt(unsafe {
            libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0)
        })?;
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        cvt(unsafe { libc::bind(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;

        // `bind()` applied the process umask, so tighten explicitly.
        // The window between bind and chmod is real; the 0700 parent
        // directory is what actually makes it unreachable, and this is
        // the second lock.
        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket path contains a NUL byte"))?;
        cvt(unsafe { libc::chmod(c_path.as_ptr(), 0o600) })?;

        cvt(unsafe { libc::listen(fd.as_raw_fd(), BACKLOG) })?;
        Ok(Self { fd, path: path.to_path_buf() })
    }

    fn clear_stale_socket(path: &Path) -> io::Result<()> {
        if !path.exists() {
            return Ok(());
        }
        match Seqpacket::connect(path) {
            Ok(_) => Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("{} is already accepting connections; another chonkstep session owns this display", path.display()),
            )),
            Err(_) => std::fs::remove_file(path),
        }
    }

    /// Accepts one pending connection, or `Ok(None)` if none is
    /// waiting.
    ///
    /// `accept4` with `SOCK_NONBLOCK`, not `accept` followed by
    /// `fcntl`. The difference is that there is no interval — not even
    /// an unlikely one, not even one an early `return` could skip past
    /// — during which a connected dockapp socket exists in this process
    /// in blocking mode. Given that a blocking send on one of these is
    /// a frozen desktop (see [`Seqpacket::send`]), the property is
    /// worth having by construction rather than by discipline. This is
    /// what `accept_returns_a_socket_that_cannot_block` asserts.
    ///
    /// `SOCK_CLOEXEC` in the same call for the same reason in
    /// miniature: a dockapp launched between `accept` and a separate
    /// `fcntl` would inherit another dockapp's socket.
    pub fn accept(&self) -> io::Result<Option<Seqpacket>> {
        let raw = unsafe {
            libc::accept4(
                self.fd.as_raw_fd(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            )
        };
        if raw < 0 {
            let err = io::Error::last_os_error();
            return match err.kind() {
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(None),
                _ => Err(err),
            };
        }
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        widen_socket_buffers(fd.as_raw_fd());
        Ok(Some(Seqpacket::from_fd(fd)))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AsRawFd for SeqpacketListener {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Drop for SeqpacketListener {
    /// Unlinks the socket file. A leftover one is not fatal — `bind`
    /// clears debris — but leaving it means the next session's startup
    /// has to do a connect probe to find that out, and a stray socket
    /// in `$XDG_RUNTIME_DIR` invites someone to wonder whether a shell
    /// is running.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------
// Polling
// ---------------------------------------------------------------------

/// [`Seqpacket::send`] on a borrowed descriptor.
///
/// Exists for callers that hold an fd rather than the socket — the
/// SDK's `Ctx::log`, which lends a dockapp the connection for one
/// message without lending it the ability to close it. Read
/// [`Seqpacket::send`] for why both flags are here; this is the same
/// call, and losing either of them in either place is the same frozen
/// desktop.
pub fn send_on(fd: RawFd, message: &[u8]) -> io::Result<usize> {
    cvt_size(unsafe { libc::send(fd, message.as_ptr().cast(), message.len(), libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) })
}

/// Waits for `fd` to become readable, or for `timeout` to elapse.
///
/// Exists for the SDK's client loop, which has one socket and a redraw
/// deadline and needs neither `calloop` nor a thread to serve both. The
/// shell does not use it: it folds these fds into its own event loop
/// (`Shell::extra_poll_fds` on X11, a calloop `Generic` source on
/// Wayland), which is the whole reason the dockapp design costs ~20
/// lines per backend instead of a backend fork.
///
/// `EINTR` returns `Ok(false)` — "nothing happened, come back" — rather
/// than an error, because a caller looping on this would otherwise have
/// to special-case a signal that means nothing to it.
pub fn wait_readable(fd: RawFd, timeout: Option<Duration>) -> io::Result<bool> {
    let mut pollfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let timeout_ms = match timeout {
        Some(d) => d.as_millis().min(libc::c_int::MAX as u128) as libc::c_int,
        None => -1,
    };
    let ready = unsafe { libc::poll(&mut pollfd, 1, timeout_ms) };
    if ready < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(false);
        }
        return Err(err);
    }
    Ok(ready > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::mpsc;
    use std::time::Instant;

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    /// A private directory for one test's socket, removed on drop.
    /// Tests must not touch `$XDG_RUNTIME_DIR/chonkstep`: the developer
    /// running them is very likely also running this compositor.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("chonk-dock-test-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            // 0700, because `ensure_socket_dir` refuses anything looser
            // and `create_dir_all` applies the process umask (0755 on a
            // stock login). That refusal is the behavior under test in
            // `the_socket_directory_is_created_private_and_verified_on_reuse`,
            // so it must not also be tripping every other test here.
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).expect("private scratch dir");
            Self(dir)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("dock.sock")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn connected_pair(scratch: &Scratch) -> (SeqpacketListener, Seqpacket, Seqpacket) {
        let listener = SeqpacketListener::bind(&scratch.socket()).expect("bind");
        let client = Seqpacket::connect(&scratch.socket()).expect("connect");
        let server = listener.accept().expect("accept").expect("a connection was pending");
        (listener, client, server)
    }

    // -- the non-blocking property -------------------------------------

    #[test]
    fn accept_returns_a_socket_that_cannot_block() {
        // Risk #1 of the design this implements: "backpressure done
        // wrong IS the original bug with a different syscall". The
        // property is established by `accept4(SOCK_NONBLOCK)`, and this
        // is the assertion that it stayed established.
        let scratch = Scratch::new();
        let (_listener, client, server) = connected_pair(&scratch);
        assert!(server.is_nonblocking().unwrap(), "an accepted dockapp socket must be O_NONBLOCK");
        assert!(client.is_nonblocking().unwrap(), "the SDK's own socket too — a dockapp should not park either");
    }

    #[test]
    fn a_send_to_a_peer_that_stopped_reading_returns_wouldblock_instead_of_parking_the_caller() {
        // The behavioral half of the assertion above, and the one that
        // actually reproduces the incident's shape: a dockapp that
        // never calls recv(). If this property is ever lost, the shell
        // stops drawing here.
        //
        // Run on a worker thread with a bounded wait rather than
        // inline, so a regression fails the test in five seconds
        // instead of hanging the suite forever — a test that hangs on
        // failure is a test people learn to skip.
        let scratch = Scratch::new();
        let (_listener, _client, server) = connected_pair(&scratch);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // 50 KB each: one 112px tile at CHONKSTEP_SCALE 2.
            let tile = vec![0u8; 112 * 112 * 4];
            let started = Instant::now();
            let mut sent = 0u32;
            // The peer never reads, so its buffer fills after a handful
            // of these. The bound is a safety net, not the expectation.
            let outcome = loop {
                match server.send(&tile) {
                    Ok(_) => sent += 1,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => break Ok(sent),
                    Err(e) => break Err(e),
                }
                if sent > 10_000 {
                    break Err(io::Error::other("buffer never filled"));
                }
            };
            let _ = tx.send((outcome, started.elapsed()));
        });

        let (outcome, elapsed) = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("send() blocked on a peer that stopped reading — the compositor would be frozen right now");
        let sent = outcome.expect("sends should end in WouldBlock, not an error");
        assert!(sent > 0, "at least one message should fit in the buffer");
        assert!(elapsed < Duration::from_secs(1), "filling the buffer took {elapsed:?}; that is not a non-blocking send");
    }

    #[test]
    fn a_send_to_a_dead_peer_is_an_error_not_a_fatal_signal() {
        // Without MSG_NOSIGNAL the default SIGPIPE disposition
        // terminates the process: a dockapp crashing at the wrong
        // moment would kill the compositor. If this test ever fails it
        // will do so by killing the test binary, which is exactly the
        // symptom it guards against.
        let scratch = Scratch::new();
        let (_listener, client, server) = connected_pair(&scratch);
        drop(client);
        // The first send may still be accepted into the socket buffer;
        // the second sees the closed peer.
        let mut last = Ok(0);
        for _ in 0..4 {
            last = server.send(b"still here?");
        }
        assert!(last.is_err(), "sending to a closed peer must report an error");
    }

    #[test]
    fn the_listener_never_blocks_when_nothing_is_connecting() {
        let scratch = Scratch::new();
        let listener = SeqpacketListener::bind(&scratch.socket()).expect("bind");
        let started = Instant::now();
        assert!(listener.accept().expect("accept").is_none(), "no connection is pending");
        assert!(started.elapsed() < Duration::from_millis(500), "accept() blocked on an empty backlog");
    }

    // -- framing --------------------------------------------------------

    #[test]
    fn message_boundaries_are_preserved_and_never_coalesced() {
        // The single property SEQPACKET is chosen for: three sends are
        // three receives, in order, with no length prefix anywhere.
        let scratch = Scratch::new();
        let (_listener, client, server) = connected_pair(&scratch);
        client.send(b"one").unwrap();
        client.send(b"").unwrap_or(0);
        client.send(b"three three three").unwrap();

        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"one");
        // The empty datagram in the middle reads back as length zero,
        // which `recv`'s contract says to treat as EOF; see its doc.
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(n, 0);
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..n], b"three three three");
    }

    #[test]
    fn a_tile_sized_message_survives_one_datagram() {
        // 112x112 RGBA is a real tile at CHONKSTEP_SCALE 2, and it is
        // over the 64 KB an unwidened socket buffer might tempt someone
        // to assume. `widen_socket_buffers` exists for this.
        let scratch = Scratch::new();
        let (_listener, client, server) = connected_pair(&scratch);
        let tile = vec![0x5Au8; 112 * 112 * 4];
        assert_eq!(client.send(&tile).unwrap(), tile.len());
        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        let n = server.recv(&mut buffer).unwrap();
        assert_eq!(n, tile.len());
        assert_eq!(&buffer[..n], &tile[..]);
    }

    #[test]
    fn a_message_at_the_protocol_ceiling_survives_one_datagram() {
        // Proves `widen_socket_buffers` actually delivers what
        // `MAX_MESSAGE_BYTES` assumes. If a kernel ever refuses to give
        // us the buffer, this fails here — with an EMSGSIZE naming the
        // syscall — instead of as a dockapp whose tile mysteriously
        // never appears at high scale.
        let scratch = Scratch::new();
        let (_listener, client, server) = connected_pair(&scratch);
        let biggest = vec![0xA5u8; MAX_MESSAGE_BYTES];
        assert_eq!(client.send(&biggest).unwrap(), biggest.len(), "the kernel must accept a ceiling-sized datagram");
        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        assert_eq!(server.recv(&mut buffer).unwrap(), biggest.len());
        assert_eq!(buffer, biggest);
    }

    #[test]
    fn a_closed_peer_reads_back_as_eof_on_the_next_pass() {
        // Why SEQPACKET and not DGRAM: peer death is an EOF on the same
        // event-loop pass, with no timeout and no liveness guess.
        let scratch = Scratch::new();
        let (_listener, client, server) = connected_pair(&scratch);
        drop(client);
        let mut buffer = [0u8; 64];
        assert_eq!(server.recv(&mut buffer).unwrap(), 0);
    }

    #[test]
    fn recv_on_an_idle_socket_would_block_rather_than_wait() {
        let scratch = Scratch::new();
        let (_listener, _client, server) = connected_pair(&scratch);
        let mut buffer = [0u8; 64];
        let err = server.recv(&mut buffer).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn wait_readable_reports_a_pending_message_and_times_out_without_one() {
        let scratch = Scratch::new();
        let (_listener, client, server) = connected_pair(&scratch);
        assert!(!wait_readable(server.as_raw_fd(), Some(Duration::from_millis(20))).unwrap());
        client.send(b"wake up").unwrap();
        assert!(wait_readable(server.as_raw_fd(), Some(Duration::from_millis(500))).unwrap());
    }

    // -- permissions ----------------------------------------------------

    #[test]
    fn the_socket_is_private_to_this_user() {
        let scratch = Scratch::new();
        let listener = SeqpacketListener::bind(&scratch.socket()).expect("bind");
        let mode = std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(listener.path()).unwrap());
        assert_eq!(mode & 0o777, 0o600, "the dock socket must not be reachable by group or other");
    }

    #[test]
    fn the_socket_directory_is_created_private_and_verified_on_reuse() {
        let scratch = Scratch::new();
        let dir = scratch.0.join("nested");
        ensure_socket_dir(&dir).expect("first call creates it");
        let mode = std::os::unix::fs::MetadataExt::mode(&std::fs::metadata(&dir).unwrap());
        assert_eq!(mode & 0o777, 0o700);
        ensure_socket_dir(&dir).expect("second call accepts the private directory it made");

        // A directory anyone can walk into is refused rather than
        // quietly fixed: something unexpected made it.
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        assert!(ensure_socket_dir(&dir).is_err(), "a world-traversable socket directory must be refused");
    }

    #[test]
    fn the_listener_removes_its_socket_when_dropped() {
        let scratch = Scratch::new();
        let path = scratch.socket();
        {
            let _listener = SeqpacketListener::bind(&path).expect("bind");
            assert!(path.exists());
        }
        assert!(!path.exists(), "a stray socket makes the next session guess whether a shell is running");
    }

    #[test]
    fn a_stale_socket_from_a_crashed_session_is_replaced() {
        let scratch = Scratch::new();
        let path = scratch.socket();
        // Simulate the debris a SIGKILLed session leaves behind: the
        // file exists, nothing is listening.
        std::fs::write(&path, b"").unwrap();
        let listener = SeqpacketListener::bind(&path).expect("stale debris should be cleared");
        assert!(Seqpacket::connect(listener.path()).is_ok());
    }

    #[test]
    fn a_live_session_is_not_evicted_by_a_second_bind() {
        // The opposite failure: unlinking a socket someone is still
        // listening on would let a second shell silently steal the
        // first one's dockapps.
        let scratch = Scratch::new();
        let first = SeqpacketListener::bind(&scratch.socket()).expect("first bind");
        let err = SeqpacketListener::bind(&scratch.socket()).expect_err("second bind must fail");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(Seqpacket::connect(first.path()).is_ok(), "the first listener still works");
    }

    #[test]
    fn the_peer_of_a_local_connection_is_this_user() {
        let scratch = Scratch::new();
        let (_listener, _client, server) = connected_pair(&scratch);
        let cred = server.peer_credentials().unwrap();
        assert_eq!(cred.pid, std::process::id() as i32);
        assert!(server.peer_is_this_user().unwrap());
    }

    #[test]
    fn an_over_long_socket_path_is_a_clear_error_not_a_truncated_address() {
        // 108 bytes is the kernel's `sun_path`. Silently using a prefix
        // would talk to a socket the caller never named — which, for a
        // path that carries an authentication token, is worth an error
        // that says so. Exercised through `connect` because `bind`
        // would stop at the parent directory's permissions first.
        let scratch = Scratch::new();
        let path = scratch.0.join("x".repeat(200));
        let err = Seqpacket::connect(&path).expect_err("must not truncate");
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("107"), "the message should name the usable limit: {err}");
    }

    #[test]
    fn a_shared_directory_is_never_accepted_as_the_socket_directory() {
        // /tmp is the classic wrong answer: it exists, it is writable,
        // and any local process can win a create race in it. The
        // sticky bit does not make it private, so the check refuses it.
        assert!(
            ensure_socket_dir(Path::new("/tmp")).is_err(),
            "a world-writable directory must never hold an authentication-bearing socket"
        );
    }

    // -- paths and tokens ------------------------------------------------

    #[test]
    fn display_names_cannot_escape_the_socket_directory() {
        assert_eq!(sanitize_display(":1"), "1");
        assert_eq!(sanitize_display("wayland-1"), "wayland-1");
        assert_eq!(sanitize_display("../../etc/passwd"), "______etc_passwd", "no dot and no separator survives");
        assert_eq!(sanitize_display(""), "default");
        assert_eq!(sanitize_display("::::"), "default");
        assert!(!sanitize_display(&"a/b".repeat(100)).contains('/'));
        assert!(sanitize_display(&"a".repeat(1000)).len() <= 32, "a display name cannot blow the 108-byte sun_path");
    }

    #[test]
    fn the_socket_path_is_stable_across_a_shell_restart() {
        // Restart survival depends on the name not carrying a pid or a
        // start time; if it did, a reconnecting dockapp could never
        // find the new shell.
        let dir = std::env::temp_dir().join("chonk-dock-path-test");
        // SAFETY-ish: this test reads the variable it just set in the
        // same thread and does not race the others, which never touch
        // XDG_RUNTIME_DIR.
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        let first = socket_path(":1").unwrap();
        let second = socket_path(":1").unwrap();
        assert_eq!(first, second);
        assert_eq!(first, dir.join("chonkstep/dock-1.sock"));
    }

    #[test]
    fn tokens_survive_the_environment_round_trip() {
        // The token reaches a dockapp as a hex string in
        // CHONKSTEP_DOCK_TOKEN, so hex is part of the contract.
        let token = mint_token().unwrap();
        let hex = token_to_hex(&token);
        assert_eq!(hex.len(), 32);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_eq!(token_from_hex(&hex), Some(token));
        assert_eq!(token_from_hex(&format!("  {hex}\n")), Some(token), "trailing newline from a shell pipeline");
    }

    #[test]
    fn a_malformed_token_string_is_rejected_rather_than_padded() {
        for bad in ["", "zz", &"0".repeat(31), &"0".repeat(33), "gg00000000000000000000000000000000"] {
            assert_eq!(token_from_hex(bad), None, "{bad:?} should not parse");
        }
    }

    #[test]
    fn minted_tokens_are_not_all_the_same() {
        let a = mint_token().unwrap();
        let b = mint_token().unwrap();
        assert_ne!(a, b, "a token that repeats is not a credential");
        assert_ne!(a, [0u8; TOKEN_BYTES], "getrandom silently failing would look exactly like this");
    }

    #[test]
    fn token_comparison_accepts_only_an_exact_match() {
        let token = mint_token().unwrap();
        assert!(tokens_match(&token, &token));
        let mut off_by_one_bit = token;
        off_by_one_bit[TOKEN_BYTES - 1] ^= 0x01;
        assert!(!tokens_match(&token, &off_by_one_bit), "the last byte must matter as much as the first");
        let mut off_by_first = token;
        off_by_first[0] ^= 0x80;
        assert!(!tokens_match(&token, &off_by_first));
    }
}
