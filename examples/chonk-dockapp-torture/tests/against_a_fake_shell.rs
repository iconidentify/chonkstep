//! The torture binary, run for real, against a shell that is only as
//! clever as the protocol requires.
//!
//! `chonk-dock-proto`'s own `tests/hostile_peer.rs` proves the
//! transport properties against a hostile *thread*. This file proves
//! them against a hostile *process* — a separate binary, a separate
//! address space, its own scheduler entity, launched exactly as the
//! dock would launch it — because the incident this whole design exists
//! to prevent was a process boundary problem, and a same-process test
//! shares too much with the thing it is testing to be the last word.
//!
//! The "shell" here is deliberately minimal: bind, accept, validate a
//! `Hello`, answer `Welcome`, then run a loop that does the four things
//! a repaint pass does for one remote tile. It is not `chonk-shell` and
//! does not try to be. What it *is* is a statement of the protocol's
//! obligations in a hundred lines, which makes it a useful reference
//! for the shell-side implementation and a place where a regression in
//! the transport shows up without a compositor in the way.
//!
//! # Why `try_wait` and never `wait`
//!
//! The workspace `clippy.toml` bans `Child::wait` because it blocks the
//! calling thread until the child exits, and this file's whole subject
//! is threads that must not block on children who may never cooperate.
//! Every child here is polled with `try_wait` against a deadline, so a
//! torture binary that fails to die makes a test *fail* rather than
//! hang. No `#[allow]` is needed and none is used — the point of the
//! lint is that reaching for it should be uncomfortable, and here it is
//! genuinely unnecessary.

use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use chonk_dock_proto::queue::{FrameLimiter, SendOutcome, SendQueue};
use chonk_dock_proto::transport::{mint_token, token_to_hex, wait_readable, Seqpacket, SeqpacketListener};
use chonk_dock_proto::wire::{
    frame_matches_tile, Button, ClientMessage, InputEvent, InputKind, ServerMessage, ThemeState,
};
use chonk_dock_proto::{handshake, MAX_MESSAGE_BYTES, TOKEN_BYTES};

/// One 16 ms frame — `chonk-shell`'s `HOUSEKEEPING_INTERVAL` — as the
/// budget for a thousand repaint passes. The same bound, for the same
/// reason, as `chonk-shell`'s
/// `a_sampler_blocked_in_a_child_process_costs_the_caller_nothing`.
const ONE_FRAME: Duration = Duration::from_millis(16);
const PASSES: u32 = 1_000;

/// The tile geometry the fake shell offers. 56 logical pixels is what
/// `desktop.rs` computes at scale 1.
const TILE_PX: u32 = 56;

/// How long any of these tests will wait for a child to do something
/// before giving up. Generous enough for a cold `cargo test` on a
/// loaded runner, short enough that a wedged test is a failure rather
/// than a hung CI job.
const PATIENCE: Duration = Duration::from_secs(20);

static COUNTER: AtomicU32 = AtomicU32::new(0);

// ---------------------------------------------------------------------
// A fake shell
// ---------------------------------------------------------------------

/// A private scratch directory. **Never** `$XDG_RUNTIME_DIR/chonkstep`:
/// whoever runs these tests is very likely running this compositor, and
/// binding over the live dock socket would take their real dockapps
/// down with them.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("chonk-torture-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        // 0700: `ensure_socket_dir` refuses anything looser, and
        // `create_dir_all` applies the process umask.
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).expect("private scratch");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A spawned torture process that is always killed, even if the test
/// panics first.
///
/// Without this, a failing `hang` test would leave a process parked on
/// a socket in `/tmp` for the lifetime of the machine. `Drop` runs on
/// unwind, which is exactly the case that matters.
struct Torture {
    child: Child,
    mode: &'static str,
}

impl Torture {
    /// Launches the binary the way the dock would: the two environment
    /// variables, the mode on the command line, and no terminal.
    ///
    /// `DISPLAY` and `WAYLAND_DISPLAY` are cleared for the same reason
    /// the design makes the shell clear them — a dockapp holds no
    /// display connection, and the honest way to say that is to make it
    /// unable to open one rather than to ask it not to. It also keeps
    /// this test from accidentally talking to the developer's live
    /// session.
    fn spawn(mode: &'static str, socket: &std::path::Path, token: &[u8; TOKEN_BYTES]) -> Self {
        Self::spawn_with(mode, socket, token, &[])
    }

    /// [`Torture::spawn`] with extra environment. Separate constructor
    /// rather than an `Option` parameter on one, so the common call
    /// reads as one line.
    fn spawn_with(
        mode: &'static str,
        socket: &std::path::Path,
        token: &[u8; TOKEN_BYTES],
        extra_env: &[(&str, &str)],
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_chonk-dockapp-torture"));
        for (key, value) in extra_env {
            command.env(key, value);
        }
        let child = command
            .arg(mode)
            .env("CHONKSTEP_DOCK_SOCKET", socket)
            .env("CHONKSTEP_DOCK_TOKEN", token_to_hex(token))
            .env("CHONKSTEP_TORTURE_ID", "torture")
            .env_remove("DISPLAY")
            .env_remove("WAYLAND_DISPLAY")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            // Silenced rather than inherited: these modes are chatty by
            // design and their commentary is for a human watching a
            // live dock, not for a test log. Flip this to `inherit()`
            // when debugging one.
            .stderr(Stdio::null())
            .spawn()
            .expect("the torture binary should have been built by cargo before this test ran");
        Self { child, mode }
    }

    /// Polls for exit until `deadline`. Never `wait()` — see this
    /// module's header.
    fn exit_code_within(&mut self, patience: Duration) -> Option<i32> {
        let deadline = Instant::now() + patience;
        loop {
            match self.child.try_wait().expect("try_wait") {
                Some(status) => return Some(status.code().unwrap_or(-1)),
                None if Instant::now() >= deadline => return None,
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }
}

impl Drop for Torture {
    fn drop(&mut self) {
        let _ = self.child.kill();
        // Reap, so the test process does not accumulate zombies across
        // a run. Bounded, for the same reason everything else here is.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        eprintln!("torture[{}] did not die after SIGKILL; leaving it to the OS", self.mode);
    }
}

/// The shell's half: a listener, an accepted peer, and the theme state
/// it welcomed that peer with.
struct FakeShell {
    _scratch: Scratch,
    listener: SeqpacketListener,
    peer: Seqpacket,
    state: ThemeState,
}

fn theme_state() -> ThemeState {
    ThemeState { tile_px: TILE_PX, scale: 1.0, proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION, theme_id: "nextstep-classic".into(), theme_toml: String::new() }
}

/// Binds a socket, launches the given torture mode, and completes the
/// handshake.
///
/// Every wait in here is bounded. A dockapp that never connects, never
/// sends a `Hello`, or sends something else entirely must cost the
/// shell a timer and nothing more — so a test helper that used a
/// blocking accept or a blocking read would be quietly asserting the
/// opposite of what this file is for.
fn welcomed(mode: &'static str, tag: &str) -> (FakeShell, Torture) {
    let scratch = Scratch::new(tag);
    let socket_path = scratch.0.join("dock.sock");
    let listener = SeqpacketListener::bind(&socket_path).expect("bind");
    let token = mint_token().expect("mint");
    let torture = Torture::spawn(mode, &socket_path, &token);

    let deadline = Instant::now() + PATIENCE;
    let peer = loop {
        if let Some(peer) = listener.accept().expect("accept") {
            break peer;
        }
        assert!(Instant::now() < deadline, "the torture binary never connected");
        // Poll the listener rather than spinning: this is what the
        // shell's event loop does with `extra_poll_fds`.
        let _ = wait_readable(std::os::fd::AsRawFd::as_raw_fd(&listener), Some(Duration::from_millis(50)));
    };

    assert!(peer.peer_is_this_user().expect("SO_PEERCRED"), "a dockapp must be a process of this same user");
    assert!(peer.is_nonblocking().expect("F_GETFL"), "accept4 must hand back an O_NONBLOCK socket");

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let n = peer
        .recv_until(&mut buffer, deadline)
        .expect("recv")
        .expect("the torture binary should have sent a Hello");
    let hello = ClientMessage::decode(&buffer[..n]).expect("a well-formed Hello");
    let accepted = handshake::validate_hello(&hello, &token, TILE_PX).expect("the handshake should be accepted");
    assert_eq!(accepted.id, "torture");

    let state = theme_state();
    peer.send(&ServerMessage::Welcome(state.clone()).encode().expect("encodes")).expect("send Welcome");

    (FakeShell { _scratch: scratch, listener, peer, state }, torture)
}

/// Fills the peer's receive buffer so that the next send is guaranteed
/// to be the `EAGAIN` case rather than an accidental success.
///
/// The analogue of the sampler test's `sleep(50ms)`: it makes the
/// measurement happen *while* the failure condition holds instead of
/// racing it.
fn wedge(peer: &Seqpacket) -> u32 {
    let filler = vec![0u8; 112 * 112 * 4];
    let mut sent = 0;
    loop {
        match peer.send(&filler) {
            Ok(_) => sent += 1,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return sent,
            Err(e) => panic!("filling a live peer's buffer failed with {e}"),
        }
        assert!(sent < 10_000, "the peer's buffer never filled");
    }
}

// ---------------------------------------------------------------------
// hang
// ---------------------------------------------------------------------

/// **The Phase 5 headline**, at the process level: a real dockapp
/// process that stops reading its socket costs the repaint thread
/// nothing measurable.
///
/// This is the 2026-08-29 incident rebuilt on purpose and moved across
/// a process boundary. Then: a widget blocked the repaint thread for
/// ~3.6s every ~34s, the compositor stopped drawing and stopped
/// collecting page-flip completions, and its own watchdog blamed the
/// display driver. Now: a whole separate process refuses to read, for
/// as long as this test cares to measure, and a thousand repaint passes
/// still fit inside one 16 ms frame.
///
/// A regression would not fail this test, it would *hang* it — so the
/// measurement runs on a worker thread reporting through a channel, and
/// the assertion is a `recv_timeout`. Five seconds to a failure with a
/// message naming the freeze beats never failing at all.
#[test]
fn a_real_dockapp_process_that_stops_reading_costs_the_repaint_thread_nothing() {
    let (shell, mut torture) = welcomed("hang", "hang");

    // The hang mode sends one good frame and one log line before it
    // stops reading, so that the shell has a last-good-frame to dim.
    // Collect them first: the measurement should be against a peer that
    // is silent because it is wedged, not one whose first frame is
    // still in flight.
    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let deadline = Instant::now() + PATIENCE;
    let mut got_frame = false;
    while Instant::now() < deadline && !got_frame {
        match shell.peer.recv_until(&mut buffer, Instant::now() + Duration::from_millis(200)) {
            Ok(Some(n)) if n > 0 => {
                if let Ok(ClientMessage::Frame { width, height, .. }) = ClientMessage::decode(&buffer[..n]) {
                    assert!(
                        frame_matches_tile(width, height, shell.state.tile_px, 1),
                        "the last good frame should be exactly the tile it was given"
                    );
                    got_frame = true;
                }
            }
            Ok(_) => {}
            Err(e) => panic!("recv: {e}"),
        }
    }
    assert!(got_frame, "the hang mode should draw once before it wedges");

    let filled = wedge(&shell.peer);
    assert!(filled > 0, "at least one message should fit before the buffer fills");

    let peer = shell.peer;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut queue = SendQueue::new();
        let mut limiter: FrameLimiter<ClientMessage> = FrameLimiter::new(Instant::now());
        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        let event = ServerMessage::Input(InputEvent {
            kind: InputKind::Press,
            button: Some(Button::Left),
            x: 3,
            y: 4,
            delta: 0,
        })
        .encode()
        .expect("encodes");
        let ping = ServerMessage::Ping { seq: 1 }.encode().expect("encodes");

        // The policy clock is virtual (10 ms a pass) so the two-second
        // sustained-overflow window is actually crossed; the clock being
        // *measured* is the real one outside the loop. Doing both with
        // `Instant::now()` would mean either a two-second test or one
        // that never reaches the interesting state.
        let epoch = Instant::now();
        let start = Instant::now();
        let mut disconnects = 0u32;
        for pass in 0..PASSES {
            let now = epoch + Duration::from_millis(10 * u64::from(pass));
            // Liveness ping plus an input event: exactly the traffic a
            // wedged tile attracts.
            if queue.push(ping.clone(), now) == SendOutcome::Disconnect {
                disconnects += 1;
            }
            if queue.push(event.clone(), now) == SendOutcome::Disconnect {
                disconnects += 1;
            }
            // The call that must never block.
            let _ = queue.flush(|bytes| peer.send(bytes));
            // ...and neither must this one.
            match peer.recv(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(message) = ClientMessage::decode(&buffer[..n]) {
                        let _ = limiter.offer(message, now);
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => break,
            }
            let _ = limiter.take_ready(now);
        }
        let _ = tx.send((start.elapsed(), queue.dropped(), disconnects));
    });

    let (elapsed, dropped, disconnects) = rx.recv_timeout(Duration::from_secs(5)).expect(
        "a repaint pass blocked on a dockapp process that stopped reading. This is the 2026-08-29 freeze with \
         send() substituted for nmcli: the desktop would be frozen right now, and its own stall watchdog would \
         be blaming the display driver again.",
    );

    assert!(
        elapsed < ONE_FRAME,
        "{PASSES} repaint passes took {elapsed:?} against a real dockapp process that stopped reading; the \
         budget for all of them is one 16ms frame"
    );
    assert!(dropped > 0, "a wedged peer should have cost dropped messages; nothing backed up, so nothing was tested");
    assert!(disconnects > 0, "the shell must eventually give up on a peer whose queue never drains");

    // And the process really is still alive and wedged — not, say,
    // quietly dead, which would make all of the above a measurement of
    // an EOF.
    assert!(torture.exit_code_within(Duration::from_millis(200)).is_none(), "the hang mode must not exit");
    drop(shell.listener);
}

// ---------------------------------------------------------------------
// flood
// ---------------------------------------------------------------------

/// A real process pushing frames as fast as it can is coalesced to the
/// limiter's rate, not queued.
///
/// The assertion is deliberately a *ratio* rather than a wall-clock
/// rate: "at most 30 a second" measured on a loaded CI runner is a
/// flaky test, whereas "far fewer were delivered than were received" is
/// the property that matters and is true at any speed. Queueing instead
/// of coalescing would mean the compositor spending its repaint budget
/// drawing frames that were obsolete before it read them.
#[test]
fn a_real_flooding_dockapp_is_coalesced_rather_than_queued() {
    let (shell, _torture) = welcomed("flood", "flood");

    let mut limiter: FrameLimiter<ClientMessage> = FrameLimiter::with_rate(30.0, Instant::now());
    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let mut received = 0u32;
    let mut delivered = 0u32;
    let deadline = Instant::now() + Duration::from_secs(3);

    // Bounded work per pass, as a repaint pass must be: a "drain until
    // WouldBlock" loop against a peer that refills faster than the shell
    // drains never exits, which is a busy freeze rather than a blocked
    // one but a freeze all the same.
    const READ_BUDGET: usize = 64;
    while received < 600 && Instant::now() < deadline {
        let now = Instant::now();
        for _ in 0..READ_BUDGET {
            match shell.peer.recv(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    received += 1;
                    if let Ok(message) = ClientMessage::decode(&buffer[..n]) {
                        if limiter.offer(message, now).is_some() {
                            delivered += 1;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        if limiter.take_ready(now).is_some() {
            delivered += 1;
        }
    }

    assert!(received > 100, "the flood mode produced only {received} frames; it is not flooding");
    assert!(
        delivered < received / 2,
        "the limiter passed {delivered} of {received} frames through; a flood must be coalesced to the newest \
         frame, not queued behind the ones it already superseded"
    );
    assert!(limiter.coalesced() > 0, "frames were superseded but the limiter did not count them");
    drop(shell.listener);
}

// ---------------------------------------------------------------------
// crash and crash-loop
// ---------------------------------------------------------------------

/// `crash-loop` dies before it ever connects, and does it fast.
///
/// This is the input the *hard cutoff* exists for. A backoff alone —
/// even a well-behaved 1/2/4/8/30s one — restarts a dockapp forever,
/// which is an invisible fork bomb with a polite waiting period. The
/// design's answer is five failures in sixty seconds and then a
/// permanent stop, and the thing that makes that testable is a binary
/// guaranteed to fail instantly every time.
#[test]
fn crash_loop_mode_dies_before_connecting_and_says_so_in_its_exit_code() {
    let scratch = Scratch::new("crashloop");
    let socket_path = scratch.0.join("dock.sock");
    let listener = SeqpacketListener::bind(&socket_path).expect("bind");
    let token = mint_token().expect("mint");

    // Five in a row, the cutoff's own budget, to show that the cost of
    // one attempt is bounded and repeatable rather than degrading.
    let started = Instant::now();
    for attempt in 0..5 {
        let mut torture = Torture::spawn("crash-loop", &socket_path, &token);
        assert_eq!(
            torture.exit_code_within(PATIENCE),
            Some(69),
            "attempt {attempt}: crash-loop must exit 69 rather than lingering"
        );
        assert!(
            listener.accept().expect("accept").is_none(),
            "attempt {attempt}: crash-loop must die before it connects, so the shell never sees a slot start"
        );
    }
    assert!(
        started.elapsed() < PATIENCE,
        "five crash-loop launches took {:?}; each one is supposed to be immediate",
        started.elapsed()
    );
}

/// `crash` connects, draws, and *then* dies — the case the exponential
/// backoff is for, as distinct from the cutoff above.
#[test]
fn crash_mode_draws_once_and_then_exits_non_zero() {
    let scratch = Scratch::new("crash");
    let socket_path = scratch.0.join("dock.sock");
    let listener = SeqpacketListener::bind(&socket_path).expect("bind");
    let token = mint_token().expect("mint");

    // `CHONKSTEP_TORTURE_DELAY_MS` is short enough to keep the test
    // quick and long enough that the frame read below is not a race.
    let mut torture = Torture::spawn_with("crash", &socket_path, &token, &[("CHONKSTEP_TORTURE_DELAY_MS", "100")]);

    let deadline = Instant::now() + PATIENCE;
    let peer = loop {
        if let Some(peer) = listener.accept().expect("accept") {
            break peer;
        }
        assert!(Instant::now() < deadline, "crash mode never connected");
        let _ = wait_readable(std::os::fd::AsRawFd::as_raw_fd(&listener), Some(Duration::from_millis(20)));
    };

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let n = peer.recv_until(&mut buffer, deadline).expect("recv").expect("a Hello");
    let hello = ClientMessage::decode(&buffer[..n]).expect("a well-formed Hello");
    handshake::validate_hello(&hello, &token, TILE_PX).expect("accepted");
    peer.send(&ServerMessage::Welcome(theme_state()).encode().unwrap()).expect("Welcome");

    // One good frame, then the process is gone and the socket reads EOF
    // on the very next pass — the "instant, definitive" crash signal the
    // design leans on, with no timeout and no liveness guess.
    let n = peer.recv_until(&mut buffer, deadline).expect("recv").expect("a frame");
    assert!(matches!(ClientMessage::decode(&buffer[..n]), Ok(ClientMessage::Frame { .. })), "crash mode draws once");

    assert_eq!(torture.exit_code_within(PATIENCE), Some(70), "crash mode must exit 70");
    let eof_deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match peer.recv(&mut buffer) {
            Ok(0) => break,
            Ok(_) => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < eof_deadline, "a dead peer must read back as EOF");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(e) => panic!("recv: {e}"),
        }
    }
}

// ---------------------------------------------------------------------
// malformed
// ---------------------------------------------------------------------

/// Everything the torture binary's `malformed` battery sends is either
/// refused by the decoder or refused by the geometry check, and nothing
/// in it ever becomes a tile.
///
/// The assertion worth reading twice is the last one: exactly *one*
/// frame in the whole session is blittable, the good one sent before
/// the battery starts. Two of the battery's entries are frames that
/// decode perfectly well and are simply the wrong size for this tile —
/// the case a monitor change produces in real life — and the design is
/// explicit that those must be rejected rather than scaled, cropped or
/// letterboxed, because blitting a stale-geometry frame at the new size
/// paints garbage into the dock.
#[test]
fn nothing_in_the_malformed_battery_ever_becomes_a_tile() {
    let (shell, _torture) = welcomed("malformed", "malformed");

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let mut received = 0u32;
    let mut decoded_ok = 0u32;
    let mut blittable = 0u32;
    let mut rejected_frames = 0u32;
    let mut log_lines: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);

    // The battery ends by parking rather than exiting, so this reads
    // until the traffic stops rather than until EOF.
    let mut idle_since = Instant::now();
    while Instant::now() < deadline && idle_since.elapsed() < Duration::from_millis(750) {
        match shell.peer.recv(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                received += 1;
                idle_since = Instant::now();
                match ClientMessage::decode(&buffer[..n]) {
                    Ok(ClientMessage::Frame { width, height, .. }) => {
                        decoded_ok += 1;
                        if frame_matches_tile(width, height, shell.state.tile_px, 1) {
                            blittable += 1;
                        } else {
                            rejected_frames += 1;
                        }
                    }
                    Ok(ClientMessage::Log { text, .. }) => {
                        decoded_ok += 1;
                        log_lines.push(text);
                    }
                    Ok(_) => decoded_ok += 1,
                    Err(_) => {}
                }
                // Answer every datagram, so the torture binary's own
                // per-message wait returns immediately and the battery
                // runs at full speed. A real shell would send a
                // `Goodbye` and close on the first violation; this one
                // deliberately keeps listening, because the interesting
                // question is what the *whole* battery does, not which
                // entry a strict shell hangs up on first.
                let _ = shell.peer.send(&ServerMessage::Ping { seq: received }.encode().unwrap());
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(5)),
            Err(_) => break,
        }
    }

    assert!(received >= 20, "only {received} datagrams arrived; the battery should be much longer than that");
    assert!(
        decoded_ok < received,
        "every one of the {received} datagrams decoded; a battery of illegal messages that the codec accepts is \
         not a battery of illegal messages"
    );
    assert!(rejected_frames >= 2, "the two legal-but-wrong-geometry frames must decode and then be refused");

    // The hostile log lines *do* decode — they are supposed to, that is
    // what makes them dangerous — and what matters is what survives.
    // The battery sends an `ESC[2J` terminal-clear, an embedded newline
    // that would forge a second journal entry, a bidi override that
    // rewrites the text rendered beside it, and a zero-width space that
    // makes two different ids look identical. None of them may still be
    // in the string by the time it is a `ClientMessage`, because from
    // that point on it is handed to `cosmic-text` and to `tracing`
    // without another check.
    assert!(!log_lines.is_empty(), "the battery's log lines should decode; sanitizing happens on the way in");
    for text in &log_lines {
        assert!(!text.chars().any(char::is_control), "a control character reached the shell: {text:?}");
        assert!(!text.contains('\u{202E}'), "a bidi override reached the shell: {text:?}");
        assert!(!text.contains('\u{200B}'), "a zero-width space reached the shell: {text:?}");
        assert!(
            !text.contains('\u{2028}') && !text.contains('\u{2029}'),
            "a Unicode line separator reached the shell: {text:?} — these are Zl/Zp, not control characters, \
             and a sanitizer that checks only `char::is_control` misses them"
        );
        assert!(text.len() <= 256, "an over-long log line reached the shell: {} bytes", text.len());
    }
    assert_eq!(
        blittable, 1,
        "exactly one frame in this session — the good one sent before the battery — may reach the dock; \
         {blittable} did"
    );
    drop(shell.listener);
}

// ---------------------------------------------------------------------
// slow-handshake and wrong-token
// ---------------------------------------------------------------------

/// A peer that connects and never sends `Hello` costs the shell a timer
/// and nothing else.
///
/// The handshake is the one exchange where the shell is waiting for a
/// specific message, which is precisely the shape that tempts an
/// implementation into a blocking read "just this once".
#[test]
fn a_peer_that_never_sends_hello_never_delays_a_pass() {
    let scratch = Scratch::new("slow");
    let socket_path = scratch.0.join("dock.sock");
    let listener = SeqpacketListener::bind(&socket_path).expect("bind");
    let token = mint_token().expect("mint");
    let mut torture = Torture::spawn("slow-handshake", &socket_path, &token);

    let deadline = Instant::now() + PATIENCE;
    let peer = loop {
        if let Some(peer) = listener.accept().expect("accept") {
            break peer;
        }
        assert!(Instant::now() < deadline, "slow-handshake never connected");
        let _ = wait_readable(std::os::fd::AsRawFd::as_raw_fd(&listener), Some(Duration::from_millis(20)));
    };

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let start = Instant::now();
    for _ in 0..PASSES {
        match peer.recv(&mut buffer) {
            Ok(0) => panic!("the peer closed; it is supposed to sit there"),
            Ok(n) => panic!("a Hello was not supposed to arrive, got {n} bytes"),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
            Err(e) => panic!("recv: {e}"),
        }
        let _ = listener.accept();
    }
    let elapsed = start.elapsed();

    assert!(
        elapsed < ONE_FRAME,
        "{PASSES} passes took {elapsed:?} while a connected peer sat silent; an unfinished handshake must cost \
         the shell a timer, not a frame"
    );
    assert!(torture.exit_code_within(Duration::from_millis(200)).is_none(), "slow-handshake should still be sitting");
}

/// A token this shell never minted buys nothing.
///
/// The token is the only part of the handshake that is security rather
/// than hygiene: the 0600 socket in a 0700 directory keeps other users
/// out and `SO_PEERCRED` confirms it, but only the token stops a stray
/// process of this *same user* from claiming a dock slot it was not
/// launched for.
#[test]
fn a_wrong_token_is_refused_and_the_dockapp_is_told_why() {
    let scratch = Scratch::new("token");
    let socket_path = scratch.0.join("dock.sock");
    let listener = SeqpacketListener::bind(&socket_path).expect("bind");
    let token = mint_token().expect("mint");
    let mut torture = Torture::spawn("wrong-token", &socket_path, &token);

    let deadline = Instant::now() + PATIENCE;
    let peer = loop {
        if let Some(peer) = listener.accept().expect("accept") {
            break peer;
        }
        assert!(Instant::now() < deadline, "wrong-token never connected");
        let _ = wait_readable(std::os::fd::AsRawFd::as_raw_fd(&listener), Some(Duration::from_millis(20)));
    };

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let n = peer.recv_until(&mut buffer, deadline).expect("recv").expect("a Hello");
    let hello = ClientMessage::decode(&buffer[..n]).expect("a well-formed Hello — only the token is wrong");
    let reason = handshake::validate_hello(&hello, &token, TILE_PX)
        .expect_err("a token off by a single bit must not be accepted");
    assert_eq!(
        reason,
        chonk_dock_proto::wire::GoodbyeReason::Unauthorized,
        "and the reason must be Unauthorized, not something that leaks which other check would also have failed"
    );
    peer.send(&ServerMessage::Goodbye { reason }.encode().unwrap()).expect("Goodbye");

    assert_eq!(
        torture.exit_code_within(PATIENCE),
        Some(0),
        "the torture binary reports success when it is correctly refused — a non-zero exit here means it got in"
    );
}
