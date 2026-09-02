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

/// `$XDG_RUNTIME_DIR/chonkstep/control-<display>.sock` — the shell's
/// control socket (`docs/control-socket.md`), beside the dock socket
/// and keyed the same way for the same reason: a bar that sees EOF
/// across a hot restart reconnects to the name it already knows.
pub fn control_socket_path(display: &str) -> io::Result<PathBuf> {
    Ok(socket_dir()?.join(format!("control-{}.sock", sanitize_display(display))))
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
    read_urandom(&mut token)?;
    Ok(token)
}

/// Reads exactly [`TOKEN_BYTES`] from `/dev/urandom`.
///
/// **`read_exact`, never `fs::read`.** This was a hang, found in Phase
/// 5 hardening and measured rather than reasoned about: `/dev/urandom`
/// is a character device that never reaches EOF, and `std::fs::read`
/// calls `read_to_end`, which loops until a read returns zero. It
/// therefore never returns — it allocates until the OOM killer picks a
/// winner. A five-second probe confirmed the thread was still inside it
/// with the buffer growing.
///
/// The path is narrow but it is exactly the path the fallback exists
/// for: `getrandom(2)` on a 16-byte buffer with `flags = 0` effectively
/// always succeeds on a running system, so this code only executes when
/// the syscall is *unavailable* — a seccomp policy, an old kernel, a
/// container profile. In other words, the fallback written to handle a
/// sandbox would have wedged the shell's startup inside that sandbox,
/// which is the least debuggable place it could possibly have happened.
///
/// Separate function so the fix is testable: see
/// `the_urandom_fallback_reads_sixteen_bytes_and_returns`.
fn read_urandom(token: &mut [u8; TOKEN_BYTES]) -> io::Result<()> {
    use std::io::Read;
    let mut file = std::fs::File::open("/dev/urandom")?;
    file.read_exact(token)
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
    /// `SOCK_NONBLOCK` at creation, so the `connect()` itself cannot
    /// block. This was previously a *blocking* connect, on the stated
    /// reasoning that "an `AF_UNIX` `connect()` to a listening socket
    /// completes without a round trip, so there is no latency to save".
    /// That reasoning is true right up until the listener's backlog is
    /// full, and then it is wrong in the worst available way: a
    /// blocking `AF_UNIX` `connect()` to a socket whose backlog is full
    /// **waits, indefinitely**, for the owner to call `accept()`
    /// (`unix_wait_for_peer` in the kernel). Measured in Phase 5
    /// hardening, not inferred: a probe that filled a `listen(1)`
    /// backlog and then made one blocking connect was still inside it
    /// three seconds later, with no timeout in sight.
    ///
    /// Two callers made that reachable. The SDK's, where the cost is
    /// one dockapp that never starts — bad but contained. And
    /// [`SeqpacketListener::bind`]'s own stale-socket probe, where the
    /// cost is the *shell* hanging at startup because some other
    /// process of this user is squatting the dock path with a backlog
    /// it never drains. A compositor that will not start is a worse
    /// outcome than one that stutters, and this whole crate exists on
    /// the principle that an unbounded wait is never the answer.
    ///
    /// A full backlog now surfaces as `ErrorKind::WouldBlock`. There is
    /// no `EINPROGRESS` case to handle: `AF_UNIX` connects are not
    /// asynchronous, so the call either completes or reports `EAGAIN`.
    /// The SDK's reconnect path already retries with backoff, and its
    /// first connect propagates the error to a supervisor that will
    /// relaunch — both strictly better than waiting forever on a peer
    /// that may never accept.
    pub fn connect(path: &Path) -> io::Result<Self> {
        let fd = connect_nonblocking(path, libc::SOCK_SEQPACKET)?;
        // After the connect rather than before: `SO_SNDBUF` is consulted
        // at each `send`, not captured at connect time, so the order is
        // free — and keeping the connect generic is what lets the
        // control socket's stream probe share it.
        widen_socket_buffers(fd.as_raw_fd());
        Ok(Self { fd })
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
        peer_credentials_of(self.fd.as_raw_fd())
    }

    /// Whether the peer is this same user. The shell refuses a
    /// connection from anyone else.
    pub fn peer_is_this_user(&self) -> io::Result<bool> {
        peer_is_this_user_on(self.fd.as_raw_fd())
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
    /// 0600, and listens — see [`bind_listener`], which is the body,
    /// shared with the control socket's [`StreamListener`] so the two
    /// cannot drift on the properties that matter.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let fd = bind_listener(path, libc::SOCK_SEQPACKET)?;
        Ok(Self { fd, path: path.to_path_buf() })
    }

    /// Accepts one pending connection, or `Ok(None)` if none is
    /// waiting — already `O_NONBLOCK`, by construction rather than by
    /// discipline; see [`accept_nonblocking`].
    pub fn accept(&self) -> io::Result<Option<Seqpacket>> {
        let Some(fd) = accept_nonblocking(self.fd.as_raw_fd())? else { return Ok(None) };
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
// The socket calls both flavours share
// ---------------------------------------------------------------------
//
// `SeqpacketListener` (the dock) and `StreamListener` (the control
// socket, `docs/control-socket.md`) differ in exactly one argument to
// `socket(2)`. Everything that makes either of them safe to run on the
// compositor's repaint thread — the 0700 directory, the stale-socket
// probe that refuses to evict a live owner, `SOCK_NONBLOCK` at creation
// and at `accept4`, the 0600 chmod — is written once here, so the two
// cannot drift apart on the property that matters.

/// A non-blocking `connect()` of a fresh `AF_UNIX` socket of `kind`.
///
/// `SOCK_NONBLOCK` at creation, so the `connect()` itself cannot
/// block. This was previously a *blocking* connect, on the stated
/// reasoning that "an `AF_UNIX` `connect()` to a listening socket
/// completes without a round trip, so there is no latency to save".
/// That reasoning is true right up until the listener's backlog is
/// full, and then it is wrong in the worst available way: a blocking
/// `AF_UNIX` `connect()` to a socket whose backlog is full **waits,
/// indefinitely**, for the owner to call `accept()`
/// (`unix_wait_for_peer` in the kernel). Measured in Phase 5
/// hardening, not inferred: a probe that filled a `listen(1)` backlog
/// and then made one blocking connect was still inside it three
/// seconds later, with no timeout in sight.
///
/// Two callers made that reachable. The SDK's, where the cost is one
/// dockapp that never starts — bad but contained. And the stale-socket
/// probe in [`bind_listener`], where the cost is the *shell* hanging at
/// startup because some other process of this user is squatting the
/// socket path with a backlog it never drains. A compositor that will
/// not start is a worse outcome than one that stutters, and this whole
/// crate exists on the principle that an unbounded wait is never the
/// answer.
///
/// A full backlog surfaces as `ErrorKind::WouldBlock`. There is no
/// `EINPROGRESS` case to handle: `AF_UNIX` connects are not
/// asynchronous, so the call either completes or reports `EAGAIN`.
fn connect_nonblocking(path: &Path, kind: libc::c_int) -> io::Result<OwnedFd> {
    let (addr, len) = sockaddr_un(path)?;
    let raw = cvt(unsafe { libc::socket(libc::AF_UNIX, kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) })?;
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    cvt(unsafe { libc::connect(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;
    Ok(fd)
}

/// Creates the directory, clears a stale socket, binds, chmods 0600,
/// and listens — the body of both `bind`s.
///
/// The stale-socket handling is the interesting part. A Unix socket
/// file outlives the process that bound it, so a crashed session
/// leaves one behind and the next `bind()` fails with `EADDRINUSE`.
/// Blindly unlinking would let a second live shell steal the first
/// one's clients, so this *connects to it first*
/// ([`clear_stale_socket`]): a socket that accepts a connection has a
/// live owner and is left alone (with an error naming the situation);
/// one that refuses is debris and is removed. The probe is made with
/// the same socket `kind` as the bind, because the kernel answers a
/// type mismatch (`EPROTOTYPE`) exactly as it answers debris, and a
/// live listener of the other flavour would otherwise be evicted as if
/// it were dead.
fn bind_listener(path: &Path, kind: libc::c_int) -> io::Result<OwnedFd> {
    if let Some(parent) = path.parent() {
        ensure_socket_dir(parent)?;
    }
    clear_stale_socket(path, kind)?;

    let (addr, len) = sockaddr_un(path)?;
    // SOCK_NONBLOCK at creation, so there is no instant in this
    // process's life where the listening fd could block an
    // `accept()` on an event-loop pass that turned out to have no
    // pending connection.
    let raw = cvt(unsafe { libc::socket(libc::AF_UNIX, kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) })?;
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
    Ok(fd)
}

fn clear_stale_socket(path: &Path, kind: libc::c_int) -> io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    match connect_nonblocking(path, kind) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("{} is already accepting connections; another chonkstep session owns this display", path.display()),
        )),
        // `EAGAIN` from the now-non-blocking probe means the socket
        // *is* listening and its backlog is momentarily full — a
        // live owner, not debris. This case has to be named
        // explicitly: falling into the `Err` arm below would unlink
        // a socket somebody is still serving, which is precisely
        // the "second shell silently steals the first one's
        // dockapps" outcome the probe exists to prevent. (Before
        // `connect` was made non-blocking this case could not
        // return at all; it hung instead, which is why the arm is
        // new and not merely rearranged.)
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "{} is listening but its backlog is full; something owns this display and is not accepting",
                path.display()
            ),
        )),
        // `EPROTOTYPE` means something *is* listening here, just not
        // with the socket type we probed with — a stream listener seen
        // through a SEQPACKET probe or vice versa. Two flavours now
        // share this directory (the dock socket and the control
        // socket), so the arm exists to make "wrong kind" read as
        // "live owner" rather than as debris to be unlinked. The two
        // never share a filename, so in practice this only fires when
        // something else has put its own socket where ours goes; the
        // right answer is still to refuse, not to steal.
        Err(e) if e.raw_os_error() == Some(libc::EPROTOTYPE) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("{} is a live socket of another type; refusing to replace it", path.display()),
        )),
        Err(_) => std::fs::remove_file(path),
    }
}

/// `accept4(SOCK_NONBLOCK | SOCK_CLOEXEC)`, or `Ok(None)` when nothing
/// is waiting.
///
/// `accept4` with `SOCK_NONBLOCK`, not `accept` followed by `fcntl`.
/// The difference is that there is no interval — not even an unlikely
/// one, not even one an early `return` could skip past — during which
/// a connected peer socket exists in this process in blocking mode.
/// Given that a blocking send on one of these is a frozen desktop (see
/// [`Seqpacket::send`]), the property is worth having by construction
/// rather than by discipline. This is what
/// `accept_returns_a_socket_that_cannot_block` asserts, for both
/// flavours.
///
/// `SOCK_CLOEXEC` in the same call for the same reason in miniature: a
/// process launched between `accept` and a separate `fcntl` would
/// inherit another peer's socket.
fn accept_nonblocking(listener: RawFd) -> io::Result<Option<OwnedFd>> {
    let raw = unsafe {
        libc::accept4(listener, std::ptr::null_mut(), std::ptr::null_mut(), libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC)
    };
    if raw < 0 {
        let err = io::Error::last_os_error();
        return match err.kind() {
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted => Ok(None),
            _ => Err(err),
        };
    }
    Ok(Some(unsafe { OwnedFd::from_raw_fd(raw) }))
}

/// `SO_PEERCRED` on a borrowed descriptor — see
/// [`Seqpacket::peer_credentials`].
pub fn peer_credentials_of(fd: RawFd) -> io::Result<PeerCredentials> {
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    cvt(unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) })?;
    Ok(PeerCredentials { pid: cred.pid, uid: cred.uid, gid: cred.gid })
}

/// [`Seqpacket::peer_is_this_user`] on a borrowed descriptor.
pub fn peer_is_this_user_on(fd: RawFd) -> io::Result<bool> {
    Ok(peer_credentials_of(fd)?.uid == unsafe { libc::geteuid() })
}

// ---------------------------------------------------------------------
// Stream
// ---------------------------------------------------------------------

/// The `SOCK_STREAM` sibling of [`SeqpacketListener`], for the shell's
/// control socket (`docs/control-socket.md`).
///
/// Stream rather than SEQPACKET because the control socket's clients
/// are not SDK processes this project ships: they are Quickshell's
/// `Socket`, `socat`, and `nc -U`, and every one of those speaks a
/// byte stream. Framing is the protocol's problem there (one JSON
/// object per newline); the transport's job is unchanged — the same
/// 0700 directory, the same stale-socket probe, the same
/// non-blocking-by-construction accept — which is why this is a second
/// type over the same [`bind_listener`] rather than a second copy of
/// it.
#[derive(Debug)]
pub struct StreamListener {
    fd: OwnedFd,
    path: PathBuf,
}

impl StreamListener {
    /// See [`SeqpacketListener::bind`]; identical apart from the socket
    /// type, including the refusal to evict a live owner.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let fd = bind_listener(path, libc::SOCK_STREAM)?;
        Ok(Self { fd, path: path.to_path_buf() })
    }

    /// Accepts one pending connection as a socket that is already
    /// `O_NONBLOCK` and `CLOEXEC`, or `Ok(None)` if none is waiting.
    /// See [`accept_nonblocking`] for why it is `accept4` and not
    /// `accept` + `fcntl`.
    pub fn accept(&self) -> io::Result<Option<Stream>> {
        Ok(accept_nonblocking(self.fd.as_raw_fd())?.map(Stream::from_fd))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl AsRawFd for StreamListener {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl Drop for StreamListener {
    /// Unlinks the socket file — the same reasoning as
    /// [`SeqpacketListener`]'s `Drop`.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// One connected `SOCK_STREAM` socket, either end, non-blocking on
/// every call by construction.
#[derive(Debug)]
pub struct Stream {
    fd: OwnedFd,
}

impl Stream {
    /// A non-blocking connect — see [`connect_nonblocking`]. Used by
    /// tests and by anything in this workspace that wants to read the
    /// control socket without being a shell.
    pub fn connect(path: &Path) -> io::Result<Self> {
        Ok(Self { fd: connect_nonblocking(path, libc::SOCK_STREAM)? })
    }

    pub fn from_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Whether `O_NONBLOCK` is actually set on this fd right now — the
    /// subject of an assertion, as for [`Seqpacket::is_nonblocking`].
    pub fn is_nonblocking(&self) -> io::Result<bool> {
        let flags = cvt(unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) })?;
        Ok(flags & libc::O_NONBLOCK != 0)
    }

    /// Writes as much of `bytes` as the socket will take right now and
    /// returns how much that was. `MSG_DONTWAIT | MSG_NOSIGNAL`, for
    /// the reasons [`Seqpacket::send`] spells out; the one difference
    /// from the datagram send is that a stream *may* take part of the
    /// buffer, so the caller keeps the remainder for the next pass
    /// rather than treating a short write as a failure.
    pub fn send(&self, bytes: &[u8]) -> io::Result<usize> {
        send_on(self.fd.as_raw_fd(), bytes)
    }

    /// Reads whatever is available. `Ok(0)` is EOF: the peer closed.
    /// `MSG_DONTWAIT`, so an idle socket answers `WouldBlock` rather
    /// than parking the caller.
    pub fn recv(&self, buffer: &mut [u8]) -> io::Result<usize> {
        cvt_size(unsafe { libc::recv(self.fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len(), libc::MSG_DONTWAIT) })
    }

    pub fn peer_is_this_user(&self) -> io::Result<bool> {
        peer_is_this_user_on(self.fd.as_raw_fd())
    }

    /// Blocks (with `poll`) until bytes arrive or `deadline` passes —
    /// for a *client* only, exactly as [`Seqpacket::recv_until`] is.
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
                continue;
            }
        }
    }

    pub fn into_fd(self) -> OwnedFd {
        self.fd
    }
}

impl AsRawFd for Stream {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
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
    fn the_control_socket_lives_beside_the_dock_socket_under_the_same_display_key() {
        // The spec (`docs/control-socket.md` §1) promises the control
        // socket is sanitised exactly as the dock socket is, so a bar
        // author can derive one path from the other; pin the two to
        // the same sanitiser rather than trusting two format strings
        // to agree.
        //
        // Both paths hang off `socket_dir()`, which reads the
        // environment; a runner without `XDG_RUNTIME_DIR` must see the
        // two refuse together rather than one of them fall back.
        match (socket_path("wayland-1"), control_socket_path("wayland-1")) {
            (Ok(dock), Ok(control)) => {
                assert_eq!(dock.parent(), control.parent());
                assert_eq!(control.file_name().unwrap(), "control-wayland-1.sock");
                assert_eq!(control_socket_path(":1").unwrap().file_name().unwrap(), "control-1.sock");
            }
            (Err(_), Err(_)) => {}
            (dock, control) => panic!("the two paths must agree on whether a runtime dir exists: {dock:?} vs {control:?}"),
        }
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
    fn the_urandom_fallback_reads_sixteen_bytes_and_returns() {
        // The regression test for a hang, not for a wrong value.
        // `mint_token`'s fallback used to be `std::fs::read
        // ("/dev/urandom")`, and `/dev/urandom` is a character device
        // that never reaches EOF: `read_to_end` loops until a read
        // returns zero, so the call never returned and the buffer grew
        // until the OOM killer intervened. Measured, not reasoned
        // about — a probe thread was still inside it after five
        // seconds.
        //
        // Run on a worker thread with a bounded wait for the same
        // reason every other property in this crate is: a regression
        // here does not fail, it hangs, and a test that hangs on
        // failure is a test somebody eventually disables.
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut token = [0u8; TOKEN_BYTES];
            let result = read_urandom(&mut token);
            let _ = tx.send(result.map(|()| token));
        });
        let token = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("read_urandom did not return; /dev/urandom has no EOF and must be read with read_exact")
            .expect("/dev/urandom should be readable");
        assert_ne!(token, [0u8; TOKEN_BYTES], "sixteen zero bytes is not a credential");
    }

    #[test]
    fn probing_a_socket_whose_backlog_is_full_does_not_hang_and_does_not_delete_it() {
        // A blocking `AF_UNIX` connect to a listener whose backlog is
        // full waits, indefinitely, for the owner to call `accept()`.
        // `SeqpacketListener::bind` probes an existing socket with
        // exactly such a connect to decide whether it is debris — so
        // before `Seqpacket::connect` was made non-blocking, any
        // process of this user could wedge the *compositor's startup*
        // permanently by squatting the dock path with a backlog it
        // never drained.
        //
        // Two things are asserted: the probe returns, and it returns
        // `AddrInUse` rather than unlinking a socket someone is still
        // serving. The second matters as much as the first — the
        // failure mode of "treat EAGAIN as debris" is a second shell
        // silently stealing the first one's dockapps.
        let scratch = Scratch::new();
        let path = scratch.socket();
        let owner = SeqpacketListener::bind(&path).expect("bind");

        // Fill the backlog. Held in a Vec so none of them is closed and
        // frees a slot underneath the probe.
        let mut pending = Vec::new();
        for _ in 0..(BACKLOG + 8) {
            match Seqpacket::connect(&path) {
                Ok(socket) => pending.push(socket),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("filling the backlog: {e}"),
            }
        }
        assert!(pending.len() >= BACKLOG as usize, "the backlog should have accepted at least {BACKLOG} connections");

        let probe_path = path.clone();
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(SeqpacketListener::bind(&probe_path).map(|_| ()));
        });
        let outcome = rx.recv_timeout(Duration::from_secs(5)).expect(
            "bind() blocked probing a socket whose backlog is full; a squatting process would wedge the              compositor's startup forever",
        );
        let err = outcome.expect_err("a live owner must not be evicted");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(path.exists(), "the probe must not unlink a socket that is still being served");
        assert_eq!(owner.path(), path);
    }

    #[test]
    fn connect_reports_a_full_backlog_instead_of_waiting_for_it() {
        // The same property from the client's side, which is where the
        // SDK lives. A dockapp that cannot get in should be told so and
        // retry (the SDK's reconnect path backs off), never park.
        //
        // On a worker thread with a bounded wait, like every other
        // non-blocking assertion in this crate: a regression makes
        // `connect` never return, so measured inline this would hang
        // the suite instead of failing it.
        let scratch = Scratch::new();
        let path = scratch.socket();
        let _listener = SeqpacketListener::bind(&path).expect("bind");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            // The accepted sockets are held so that none of them closes
            // and frees a backlog slot underneath the next connect.
            let mut pending = Vec::new();
            let outcome = loop {
                match Seqpacket::connect(&path) {
                    Ok(socket) => pending.push(socket),
                    Err(e) => break Ok(e.kind()),
                }
                if pending.len() >= 1_000 {
                    break Err("the backlog appears to be unbounded");
                }
            };
            let _ = tx.send(outcome);
        });
        let kind = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("connect() blocked on a full backlog instead of reporting it")
            .expect("the backlog should be bounded");
        assert_eq!(kind, io::ErrorKind::WouldBlock, "a full backlog is EAGAIN, not a wait");
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

    // -----------------------------------------------------------------
    // The stream flavour — what the control socket rides on
    // -----------------------------------------------------------------
    //
    // The bind/probe/accept machinery is shared with the SEQPACKET
    // listener above, so most of the properties are inherited. These
    // tests pin the places where the flavour matters: the probe must
    // use a stream socket (a SEQPACKET probe against a stream listener
    // fails with EPROTOTYPE, which looks like debris), and the accepted
    // fd must still come back non-blocking.

    #[test]
    fn an_accepted_stream_is_nonblocking_on_both_ends() {
        let scratch = Scratch::new();
        let listener = StreamListener::bind(&scratch.socket()).unwrap();
        let client = Stream::connect(listener.path()).unwrap();
        let server = wait_for_stream(&listener);
        assert!(server.is_nonblocking().unwrap(), "an accepted control client must be O_NONBLOCK");
        assert!(client.is_nonblocking().unwrap());
    }

    #[test]
    fn a_stream_carries_bytes_in_order_and_reports_eof() {
        let scratch = Scratch::new();
        let listener = StreamListener::bind(&scratch.socket()).unwrap();
        let client = Stream::connect(listener.path()).unwrap();
        let server = wait_for_stream(&listener);
        assert_eq!(client.send(b"{\"request\":\"snapshot\"}\n").unwrap(), 23);
        let mut buffer = [0u8; 64];
        let n = wait_recv(&server, &mut buffer);
        assert_eq!(&buffer[..n], b"{\"request\":\"snapshot\"}\n");
        drop(client);
        // A closed peer reads as EOF, never as an error and never as a
        // wait — the shell's tick must be able to tell "gone" from
        // "quiet" without parking.
        assert_eq!(wait_recv(&server, &mut buffer), 0);
    }

    #[test]
    fn a_stale_stream_socket_is_replaced_and_a_live_one_is_kept() {
        let scratch = Scratch::new();
        let path = scratch.socket();
        std::fs::write(&path, b"").unwrap();
        let first = StreamListener::bind(&path).expect("debris left by a killed session is cleared");
        assert!(Stream::connect(first.path()).is_ok());
        // With a live owner the second bind must refuse rather than
        // steal — two shells on one display is the bug, not the fix.
        let err = StreamListener::bind(&path).expect_err("a live listener must not be evicted");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(Stream::connect(first.path()).is_ok(), "the refused bind must not have unlinked the live socket");
    }

    #[test]
    fn a_stream_listener_removes_its_socket_when_dropped() {
        let scratch = Scratch::new();
        let path = scratch.socket();
        let listener = StreamListener::bind(&path).unwrap();
        assert!(path.exists());
        drop(listener);
        assert!(!path.exists(), "a clean shutdown must not leave debris for the next session to probe");
    }

    #[test]
    fn a_seqpacket_probe_does_not_mistake_a_live_stream_listener_for_debris() {
        // The reason `clear_stale_socket` takes the socket kind: the
        // dock and control sockets live in the same directory, and a
        // probe of the wrong flavour returns EPROTOTYPE, which a naive
        // "any error means stale" rule would treat as permission to
        // unlink someone else's live socket.
        let scratch = Scratch::new();
        let path = scratch.socket();
        let stream = StreamListener::bind(&path).unwrap();
        let err = SeqpacketListener::bind(&path).expect_err("a live stream listener is not debris");
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        assert!(path.exists());
        assert!(Stream::connect(stream.path()).is_ok());
    }

    fn wait_for_stream(listener: &StreamListener) -> Stream {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(stream) = listener.accept().unwrap() {
                return stream;
            }
            assert!(Instant::now() < deadline, "no connection arrived");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_recv(stream: &Stream, buffer: &mut [u8]) -> usize {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match stream.recv(buffer) {
                Ok(n) => return n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("recv failed: {e}"),
            }
            assert!(Instant::now() < deadline, "nothing arrived");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}
