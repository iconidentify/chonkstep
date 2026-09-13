//! Nonblocking, same-user Unix stream sockets for compositor IPC.
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;
const BACKLOG: libc::c_int = 16;
#[derive(Debug, Clone, Copy)]
pub struct PeerCredentials { pub pid: libc::pid_t, pub uid: libc::uid_t, pub gid: libc::gid_t }
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

fn sockaddr_un(path: &Path) -> io::Result<(libc::sockaddr_un, libc::socklen_t)> {
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: this C socket-address structure contains only integer and
    // byte-array fields, for which all-zero is a valid initial value.
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

pub fn socket_dir() -> io::Result<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "XDG_RUNTIME_DIR is not set; IPC sockets need a private per-user runtime directory and will not fall back to /tmp",
        )
    })?;
    Ok(PathBuf::from(runtime).join("chonkstep"))
}

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

pub fn control_socket_path(display: &str) -> io::Result<PathBuf> {
    Ok(socket_dir()?.join(format!("control-{}.sock", sanitize_display(display))))
}

pub fn ensure_socket_dir(dir: &Path) -> io::Result<()> {
    let c_dir = std::ffi::CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket directory path contains a NUL byte"))?;
    // SAFETY: `c_dir` is NUL-terminated and remains live for the call.
    let made = unsafe { libc::mkdir(c_dir.as_ptr(), 0o700) };
    if made < 0 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::AlreadyExists {
            return Err(err);
        }
        // SAFETY: all-zero is a valid initial representation for this C
        // output structure; `lstat` fills it before any field is read.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: the path is a live NUL-terminated string and `stat` points
        // to writable storage of the exact structure size.
        cvt(unsafe { libc::lstat(c_dir.as_ptr(), &mut stat) })?;
        let is_dir = stat.st_mode & libc::S_IFMT == libc::S_IFDIR;
        // SAFETY: `geteuid` takes no arguments and has no memory preconditions.
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

fn connect_nonblocking(path: &Path, kind: libc::c_int) -> io::Result<OwnedFd> {
    let (addr, len) = sockaddr_un(path)?;
    // SAFETY: `socket` takes only integer arguments and returns a new raw fd,
    // whose error sentinel is checked by `cvt`.
    let raw = cvt(unsafe { libc::socket(libc::AF_UNIX, kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) })?;
    // SAFETY: `raw` is newly created and nonnegative, with no other Rust
    // owner; ownership transfers to this `OwnedFd` exactly once.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `addr` remains live for the computed initialized length and the
    // owned descriptor remains open for the duration of the syscall.
    cvt(unsafe { libc::connect(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;
    Ok(fd)
}

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
    // SAFETY: `socket` takes only integer arguments and returns a new raw fd,
    // whose error sentinel is checked by `cvt`.
    let raw = cvt(unsafe { libc::socket(libc::AF_UNIX, kind | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK, 0) })?;
    // SAFETY: `raw` is newly created and nonnegative, with no other Rust
    // owner; ownership transfers to this `OwnedFd` exactly once.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    // SAFETY: `addr` remains live for the computed initialized length and the
    // owned descriptor remains open for the duration of the syscall.
    cvt(unsafe { libc::bind(fd.as_raw_fd(), (&addr as *const libc::sockaddr_un).cast(), len) })?;

    // `bind()` applied the process umask, so tighten explicitly.
    // The window between bind and chmod is real; the 0700 parent
    // directory is what actually makes it unreachable, and this is
    // the second lock.
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socket path contains a NUL byte"))?;
    // SAFETY: `c_path` is NUL-terminated and remains live for the call.
    cvt(unsafe { libc::chmod(c_path.as_ptr(), 0o600) })?;

    // SAFETY: `fd` owns the live socket; `listen` has no userspace pointer
    // arguments or additional memory-safety preconditions.
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

fn accept_nonblocking(listener: RawFd) -> io::Result<Option<OwnedFd>> {
    // SAFETY: `listener` is used only as a kernel handle; both optional
    // address outputs are null, and `accept4` returns a new fd or -1.
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
    // SAFETY: successful `accept4` returned a new nonnegative descriptor with
    // no other Rust owner; ownership transfers exactly once.
    Ok(Some(unsafe { OwnedFd::from_raw_fd(raw) }))
}

pub fn peer_credentials_of(fd: RawFd) -> io::Result<PeerCredentials> {
    // SAFETY: all-zero is a valid initial representation for this C output
    // structure, and the kernel fills it before fields are read.
    let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: `cred` points to writable storage of `len` bytes and `len`
    // itself is a live writable socklen; `fd` is only a kernel handle.
    cvt(unsafe { libc::getsockopt(fd, libc::SOL_SOCKET, libc::SO_PEERCRED, (&mut cred as *mut libc::ucred).cast(), &mut len) })?;
    Ok(PeerCredentials { pid: cred.pid, uid: cred.uid, gid: cred.gid })
}

pub fn peer_is_this_user_on(fd: RawFd) -> io::Result<bool> {
    // SAFETY: `geteuid` takes no arguments and has no memory preconditions.
    Ok(peer_credentials_of(fd)?.uid == unsafe { libc::geteuid() })
}

pub fn send_on(fd: RawFd, message: &[u8]) -> io::Result<usize> {
    // SAFETY: `message` is readable for the supplied length and remains live;
    // the raw descriptor is passed only as a kernel handle.
    cvt_size(unsafe { libc::send(fd, message.as_ptr().cast(), message.len(), libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL) })
}

pub fn wait_readable(fd: RawFd, timeout: Option<Duration>) -> io::Result<bool> {
    let mut pollfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    let timeout_ms = match timeout {
        Some(d) => d.as_millis().min(libc::c_int::MAX as u128) as libc::c_int,
        None => -1,
    };
    // SAFETY: `pollfd` is one initialized value, writable for the exact
    // element count supplied, and remains live until `poll` returns.
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

#[derive(Debug)]
pub struct StreamListener {
    fd: OwnedFd,
    path: PathBuf,
}

impl StreamListener {
    /// See `Stream`; identical apart from the socket
    /// type, including the refusal to evict a live owner.
    pub fn bind(path: &Path) -> io::Result<Self> {
        let fd = bind_listener(path, libc::SOCK_STREAM)?;
        Ok(Self { fd, path: path.to_path_buf() })
    }

    /// Accepts one pending connection as a socket that is already
    /// `O_NONBLOCK` and `CLOEXEC`, or `Ok(None)` if none is waiting.
    /// See the private `accept_nonblocking` helper for why it is `accept4` and not
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
    /// `Stream`'s `Drop`.
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
    /// A non-blocking connect — see the private `connect_nonblocking`. Used by
    /// tests and by anything in this workspace that wants to read the
    /// control socket without being a shell.
    pub fn connect(path: &Path) -> io::Result<Self> {
        Ok(Self { fd: connect_nonblocking(path, libc::SOCK_STREAM)? })
    }

    pub fn from_fd(fd: OwnedFd) -> Self {
        Self { fd }
    }

    /// Whether `O_NONBLOCK` is actually set on this fd right now — the
    /// subject of an assertion, as for `Stream`.
    pub fn is_nonblocking(&self) -> io::Result<bool> {
        // SAFETY: this object owns a live descriptor; `F_GETFL` has no third
        // argument and does not dereference userspace memory.
        let flags = cvt(unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFL) })?;
        Ok(flags & libc::O_NONBLOCK != 0)
    }

    /// Writes as much of `bytes` as the socket will take right now and
    /// returns how much that was. `MSG_DONTWAIT | MSG_NOSIGNAL`, for
    /// the reasons `Stream` spells out; the one difference
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
        // SAFETY: `buffer` is writable for the supplied length and remains
        // exclusively borrowed; the object owns the live descriptor.
        cvt_size(unsafe { libc::recv(self.fd.as_raw_fd(), buffer.as_mut_ptr().cast(), buffer.len(), libc::MSG_DONTWAIT) })
    }

    pub fn peer_is_this_user(&self) -> io::Result<bool> {
        peer_is_this_user_on(self.fd.as_raw_fd())
    }

    /// Whether the peer has fully closed the connection.
    ///
    /// This is the server-side liveness check for a write-only event
    /// stream: when there are no bytes queued, `send` has nothing to
    /// call and `recv` would violate the protocol's direction, but a
    /// dead peer still leaves `POLLHUP` permanently asserted. Zero
    /// timeout means this is a query, never a wait. A failed or
    /// interrupted poll is deliberately "not known dead"; keeping an
    /// ambiguous connection is safer than dropping a live one.
    pub fn peer_gone(&self) -> bool {
        let mut fd = libc::pollfd { fd: self.fd.as_raw_fd(), events: 0, revents: 0 };
        // SAFETY: `fd` is one initialized `pollfd`, writable for the exact
        // element count supplied, and remains live for this nonblocking call.
        let ready = unsafe { libc::poll(&mut fd, 1, 0) };
        ready > 0 && fd.revents & (libc::POLLHUP | libc::POLLERR) != 0
    }

    /// Blocks (with `poll`) until bytes arrive or `deadline` passes —
    /// for a *client* only, exactly as `Stream` is.
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
