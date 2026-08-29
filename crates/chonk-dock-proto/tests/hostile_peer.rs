//! What a misbehaving dockapp costs the compositor: nothing.
//!
//! This file is the deliverable the Phase 4 design rests on. Its
//! central claim, quoted from that design, is:
//!
//! > SAY IT IN THE COMMENTS: a hung dockapp costs the compositor
//! > NOTHING. The shell never blocks on it; frames just stop arriving.
//! > The liveness check exists to tell the USER, not to protect the
//! > desktop. That inversion is the whole deliverable.
//!
//! and its top risk is:
//!
//! > Backpressure done wrong IS the original bug with a different
//! > syscall. A blocking `write()` to a dockapp that stopped reading
//! > parks the compositor.
//!
//! That is history, not theory. On 2026-08-29 a dock widget called
//! `nmcli dev wifi` from `DockWidget::tick()` on the compositor's
//! single repaint thread; `--rescan auto` blocked ~3.6s per hardware
//! scan, once every ~34s. The desktop stopped drawing, stopped reading
//! input, and left the page-flip completion unread in its DRM fd — so
//! the compositor's own stall watchdog blamed the display driver, and
//! four agents spent hours in DRM internals before anyone looked at the
//! wifi icon. See the workspace `clippy.toml` for the full post-mortem.
//!
//! A blocking `send()` to a wedged dockapp is that bug with `nmcli`
//! substituted out. So these tests are shaped exactly like the one that
//! guards the first version — `chonk-shell`'s
//! `a_sampler_blocked_in_a_child_process_costs_the_caller_nothing`,
//! which registers a source running `sleep 2` and requires a *thousand*
//! `refresh()` calls taken while the child is parked to fit inside one
//! 16 ms frame. Same claim, same numbers, applied to a hung socket peer
//! instead of a hung child process.
//!
//! # Why every test here runs on a worker thread
//!
//! A regression in the property under test is, by definition, a call
//! that never returns. Asserted inline, that is a test suite that hangs
//! forever — in CI, on a developer's machine, in the pre-commit hook —
//! and a test that hangs on failure is a test somebody eventually
//! disables. Every measurement below therefore happens on a spawned
//! thread that reports through an `mpsc` channel, and the assertion is
//! a `recv_timeout`: a regression fails in five seconds with a message
//! naming the freeze, instead of never failing at all. This mirrors
//! `transport.rs`'s own
//! `a_send_to_a_peer_that_stopped_reading_returns_wouldblock_instead_of_parking_the_caller`.
//!
//! # What this file cannot assert, and who should
//!
//! These tests own the *transport* half of the claim: the socket, the
//! send queue and the frame limiter, driven exactly as a repaint pass
//! would drive them. They cannot reach into `chonk-shell`'s dock,
//! because this crate must not depend on it (and the shell side is
//! being written concurrently). The equivalent test on that side is
//! named in the Phase 5 report; in one line, it should register a
//! `RemoteTile` whose peer is a live but non-reading socket and assert
//! that 1000 `redraw_dock()` passes fit in 16 ms.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use chonk_dock_proto::queue::{FrameLimiter, SendOutcome, SendQueue};
use chonk_dock_proto::transport::{Seqpacket, SeqpacketListener};
use chonk_dock_proto::wire::{Button, ClientMessage, InputEvent, InputKind, ServerMessage};
use chonk_dock_proto::MAX_MESSAGE_BYTES;

/// One whole frame at the shell's 16 ms `HOUSEKEEPING_INTERVAL`. The
/// budget for a *thousand* passes, deliberately: the same bound
/// `chonk-shell`'s sampler test uses, chosen loose enough that a debug
/// build on a loaded CI runner cannot fail it by accident and still
/// more than two orders of magnitude tighter than the failure it
/// guards against.
const ONE_FRAME: Duration = Duration::from_millis(16);

/// How many repaint passes to measure. A thousand, matching the
/// sampler test, so that per-pass noise averages out and the number
/// being compared is real work rather than one scheduler hiccup.
const PASSES: u32 = 1_000;

/// How long a measurement thread gets before the test declares the
/// caller parked. Five seconds is long enough that a slow machine
/// never trips it and short enough that a genuine regression is a
/// failure, not a hang.
const FAIL_FAST: Duration = Duration::from_secs(5);

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A private directory for one test's socket, removed on drop.
///
/// Tests must not touch `$XDG_RUNTIME_DIR/chonkstep`: whoever is
/// running them is very likely also running this compositor, and a test
/// that unlinks the live dock socket would take the developer's own
/// dockapps down with it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("chonk-dock-hostile-{tag}-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        // 0700 explicitly: `ensure_socket_dir` refuses anything looser,
        // and `create_dir_all` applies the process umask.
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).expect("private scratch");
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

/// A listener plus one accepted connection: the shell's end and the
/// dockapp's end of the same socket.
fn connected_pair(scratch: &Scratch) -> (SeqpacketListener, Seqpacket, Seqpacket) {
    let listener = SeqpacketListener::bind(&scratch.socket()).expect("bind");
    let dockapp = Seqpacket::connect(&scratch.socket()).expect("connect");
    let shell_side = listener.accept().expect("accept").expect("a connection was pending");
    (listener, dockapp, shell_side)
}

/// Runs `body` on a worker thread and fails the test if it does not
/// finish inside [`FAIL_FAST`].
///
/// The `context` string is what a future reader sees when the property
/// breaks, so it says what the freeze *means*, not which function did
/// not return.
fn within_deadline<T: Send + 'static>(context: &str, body: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(body());
    });
    rx.recv_timeout(FAIL_FAST).unwrap_or_else(|_| panic!("{context}"))
}

/// Fills a peer's socket buffer so the next send is guaranteed to be
/// the interesting case (`EAGAIN`) rather than an accidental success.
///
/// The analogue of the sampler test's `sleep(50ms)` before measuring:
/// it makes the measurement happen *while* the failure condition holds,
/// instead of racing it.
fn wedge(shell_side: &Seqpacket) -> u32 {
    // 50 KB each: one 112px tile at CHONKSTEP_SCALE 2, so the buffer
    // fills in the same handful of messages a real dock would need.
    let filler = vec![0u8; 112 * 112 * 4];
    let mut sent = 0;
    loop {
        match shell_side.send(&filler) {
            Ok(_) => sent += 1,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return sent,
            Err(e) => panic!("filling a live peer's buffer failed with {e}"),
        }
        assert!(sent < 10_000, "the peer's socket buffer never filled; this test is not testing anything");
    }
}

// ---------------------------------------------------------------------
// The deliverable
// ---------------------------------------------------------------------

/// **The Phase 5 deliverable**: a dockapp that stopped reading its
/// socket costs the repaint thread nothing measurable.
///
/// The shape is `chonk-shell`'s
/// `a_sampler_blocked_in_a_child_process_costs_the_caller_nothing`,
/// with the hung child process replaced by a hung socket peer, because
/// those are the same bug wearing different syscalls.
///
/// What each pass does is deliberately everything a repaint pass would
/// do for one remote tile:
///
/// 1. queue an outbound message (a pointer event — the traffic that
///    actually arrives at human speed while a tile is wedged),
/// 2. flush the queue to the socket, which is where a blocking `send()`
///    would park the thread forever,
/// 3. drain inbound frames non-blockingly, which is where a blocking
///    `recv()` would park it instead,
/// 4. ask the frame limiter whether a parked frame is due.
///
/// The peer is a real process-external socket with a real full kernel
/// buffer and a real thread that will never call `recv()`. Nothing here
/// is mocked; if `MSG_DONTWAIT` or `accept4(SOCK_NONBLOCK)` were lost,
/// this test would stop returning rather than start failing — which is
/// exactly why it runs under a five-second deadline.
#[test]
fn a_hung_dockapp_costs_the_repaint_thread_nothing() {
    let scratch = Scratch::new("hung");
    let (listener, dockapp, shell_side) = connected_pair(&scratch);

    // The dockapp: connected, alive, and never reading. A real one
    // reaches this state by deadlocking, by being SIGSTOPped, or on
    // purpose — see `examples/chonk-dockapp-torture --mode hang`, which
    // does exactly this against a live dock.
    let stop = Arc::new(AtomicBool::new(false));
    let hung = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            // Holds the fd open and never touches it. `park_timeout`
            // rather than a plain loop so this thread costs no CPU
            // while the measurement below is timed.
            while !stop.load(Ordering::Relaxed) {
                std::thread::park_timeout(Duration::from_millis(10));
            }
            drop(dockapp);
        })
    };

    let filled = wedge(&shell_side);
    assert!(filled > 0, "at least one message should fit before the buffer fills");

    let (elapsed, drops) = within_deadline(
        "a repaint pass blocked on a dockapp that stopped reading. This is the original 2026-08-29 freeze with \
         send() substituted for nmcli: the desktop would be frozen right now, and its own watchdog would be \
         blaming the display driver.",
        move || {
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
            .expect("a pointer event encodes");

            // The policy clock is virtual, advancing 10 ms per pass, so
            // that `SUSTAINED_OVERFLOW` (2 s) is actually crossed
            // inside the run. The clock being measured is the real one
            // outside it. Doing this with `Instant::now()` would mean
            // either a test that takes two seconds to reach the
            // interesting state, or one that never reaches it — and the
            // "gives up on a wedged peer" assertion below is not
            // optional garnish: without it a fast loop could be fast
            // precisely because it was quietly dropping every event
            // forever.
            let epoch = Instant::now();
            let start = Instant::now();
            let mut disconnects = 0u32;
            for pass in 0..PASSES {
                let now = epoch + Duration::from_millis(10 * u64::from(pass));
                if queue.push(event.clone(), now) == SendOutcome::Disconnect {
                    disconnects += 1;
                }
                // The call that must never block.
                let _ = queue.flush(|bytes| shell_side.send(bytes));
                // ...and neither must this one.
                match shell_side.recv(&mut buffer) {
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
            (start.elapsed(), (queue.dropped(), disconnects))
        },
    );

    stop.store(true, Ordering::Relaxed);
    let _ = hung.join();
    drop(listener);

    assert!(
        elapsed < ONE_FRAME,
        "{PASSES} repaint passes took {elapsed:?} against a dockapp that stopped reading; the budget is one \
         16ms frame for all of them. The whole point of this protocol is that the compositor never waits for a \
         tile."
    );

    // Not incidental — the reason the loop above is fast is that the
    // policy threw work away rather than waiting for it, and a version
    // of this test where nothing was dropped would be measuring an
    // empty queue instead of backpressure.
    let (dropped, disconnects) = drops;
    assert!(dropped > 0, "a wedged peer should have cost dropped messages; nothing was dropped, so nothing backed up");
    assert!(
        disconnects > 0,
        "the queue stayed full for the whole run but never asked for a disconnect; a tile that silently eats \
         every event forever is worse than one the shell gives up on"
    );
}

/// The same claim for the other direction: a dockapp *flooding* the
/// shell also costs one bounded pass, not an unbounded one.
///
/// A hostile dockapp cannot be made to send politely, so the shell's
/// protection has to be that it reads a bounded number of messages per
/// pass and coalesces the rest. Without a budget, "drain until
/// WouldBlock" against a peer that refills faster than the shell drains
/// is a loop that never exits — a busy freeze rather than a blocked
/// one, but a freeze either way, and one that would look exactly like
/// the original incident from outside.
#[test]
fn a_flooding_dockapp_costs_one_bounded_pass_not_an_unbounded_one() {
    let scratch = Scratch::new("flood");
    let (listener, dockapp, shell_side) = connected_pair(&scratch);

    let stop = Arc::new(AtomicBool::new(false));
    let flooder = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            // A 4x4 tile: small on purpose, so the flooder can refill
            // the buffer faster than the reader empties it. The point
            // is to lose the race deliberately.
            let frame = ClientMessage::Frame { generation: 0, width: 4, height: 4, pixels: vec![0x7F; 64] }
                .encode()
                .expect("encodes");
            let mut sent = 0u64;
            while !stop.load(Ordering::Relaxed) {
                match dockapp.send(&frame) {
                    Ok(_) => sent += 1,
                    // Its own sends are non-blocking too, so a flooder
                    // that outruns the shell spins rather than parking
                    // — which is the dockapp's own problem, exactly as
                    // designed.
                    Err(_) => std::thread::yield_now(),
                }
            }
            sent
        })
    };

    // Let the flooder get ahead so the measurement happens against a
    // full receive buffer rather than racing an empty one.
    std::thread::sleep(Duration::from_millis(50));

    /// What one repaint pass will read before moving on. The shell picks
    /// this; 64 is the send queue's own depth, i.e. "more than a human
    /// can generate in a frame".
    const READ_BUDGET: usize = 64;

    let (elapsed, delivered, read) = within_deadline(
        "a repaint pass never finished while a dockapp was flooding it. A bounded read budget is what stops a \
         hostile tile from owning the repaint thread.",
        move || {
            let mut limiter: FrameLimiter<ClientMessage> = FrameLimiter::with_rate(30.0, Instant::now());
            let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
            let start = Instant::now();
            let mut delivered = 0u32;
            let mut read = 0u64;
            for _ in 0..PASSES {
                let now = Instant::now();
                for _ in 0..READ_BUDGET {
                    match shell_side.recv(&mut buffer) {
                        Ok(0) => break,
                        Ok(n) => {
                            read += 1;
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
            (start.elapsed(), delivered, read)
        },
    );

    stop.store(true, Ordering::Relaxed);
    let sent = flooder.join().expect("the flooder thread");
    drop(listener);

    assert!(read > 0, "the flooder sent {sent} frames and the shell read none of them");
    // The bound is generous — a thousand passes each reading up to 64
    // messages is 64000 decodes, which is real work — but it is a
    // *bound*, and the property is that it exists.
    assert!(
        elapsed < Duration::from_secs(2),
        "{PASSES} bounded passes took {elapsed:?} while a dockapp flooded the socket"
    );
    // The frames the compositor would actually have blitted are capped
    // by the token bucket, not by how fast the dockapp can write.
    // Deliberately compared against the *received* count rather than a
    // wall-clock rate: this test is about coalescing, and turning it
    // into a timing assertion would make it flaky on a loaded runner.
    assert!(
        u64::from(delivered) < read,
        "the limiter delivered {delivered} of {read} frames read; a flood must be coalesced, not queued"
    );
}

/// A dockapp that connects and then says nothing must not hold anything
/// up either.
///
/// This is the `slow-handshake` torture mode. It matters because the
/// handshake is the one exchange where the shell is *waiting* for a
/// specific message, which is precisely the shape that tempts an
/// implementation into a blocking read "just this once".
#[test]
fn a_peer_that_connects_and_never_speaks_never_delays_a_pass() {
    let scratch = Scratch::new("silent");
    let listener = SeqpacketListener::bind(&scratch.socket()).expect("bind");
    // Connected, holding the fd, sending nothing. `_silent` is bound
    // rather than dropped so the connection stays open for the whole
    // measurement.
    let _silent = Seqpacket::connect(&scratch.socket()).expect("connect");

    let elapsed = within_deadline(
        "accept() or recv() blocked on a peer that connected and then sent nothing — the shape a dockapp gets \
         for free by calling connect() and then sleeping.",
        move || {
            let peer = loop {
                if let Some(peer) = listener.accept().expect("accept") {
                    break peer;
                }
            };
            let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
            let start = Instant::now();
            for _ in 0..PASSES {
                // The pending-Hello read, as the shell's event loop
                // would do it.
                match peer.recv(&mut buffer) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(_) => break,
                }
                // ...and the listener poll on the same pass, which must
                // also not wait for a connection that is not coming.
                let _ = listener.accept();
            }
            start.elapsed()
        },
    );

    assert!(
        elapsed < ONE_FRAME,
        "{PASSES} passes took {elapsed:?} while a connected peer sat silent; a handshake nobody completes must \
         cost the shell nothing but a timer"
    );
}

/// A dockapp that dies mid-send must not take the compositor with it.
///
/// Without `MSG_NOSIGNAL`, writing to a socket whose peer has gone
/// raises `SIGPIPE`, whose default disposition is *terminate the
/// process*. A dockapp crashing at the wrong moment would then kill the
/// desktop — a strictly worse outcome than the freeze this whole design
/// is about, and one that would be reported as "the compositor
/// crashed", pointing at everything except the tile that did it.
///
/// If this property is ever lost the test does not fail, it *dies*:
/// the whole test binary is terminated by the signal. That is a bad
/// failure mode for a test and an unavoidable one — the alternative is
/// installing a signal handler, which would mean the test no longer
/// tests the default disposition anybody else runs under.
#[test]
fn a_dockapp_that_dies_mid_send_does_not_signal_the_shell_to_death() {
    let scratch = Scratch::new("crash");
    let (listener, dockapp, shell_side) = connected_pair(&scratch);

    // The `crash` torture mode, in miniature: the peer is simply gone.
    drop(dockapp);

    let outcome = within_deadline("sending to a dead peer blocked", move || {
        let mut errors = 0;
        let mut last = String::new();
        // The first send may still be accepted into the socket buffer;
        // a later one sees the closed peer. Sending a hundred times
        // rather than twice because the property being checked is "no
        // signal ever", not "an error on send number two".
        for _ in 0..100 {
            if let Err(e) = shell_side.send(b"are you still there?") {
                errors += 1;
                last = e.kind().to_string();
            }
        }
        (errors, last)
    });

    drop(listener);
    let (errors, last) = outcome;
    assert!(errors > 0, "sending to a closed peer must report an error rather than silently succeeding");
    assert!(
        last.contains("broken pipe") || last.contains("connection reset"),
        "a dead peer should surface as EPIPE/ECONNRESET, got {last:?}"
    );
}

/// A dockapp cannot make the shell allocate without bound by sending
/// enormous messages.
///
/// The kernel is the first line here — an `AF_UNIX` datagram larger
/// than the socket buffer is refused at `send()` — and the codec's
/// `MAX_MESSAGE_BYTES` check is the second. This asserts the pair
/// actually meets in the middle rather than leaving a gap, which is
/// where the `malformed` torture mode's oversized frames aim.
#[test]
fn an_oversized_message_is_refused_by_the_kernel_or_the_codec_and_never_allocated() {
    let scratch = Scratch::new("oversize");
    let (_listener, dockapp, shell_side) = connected_pair(&scratch);

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    for size in [MAX_MESSAGE_BYTES + 1, MAX_MESSAGE_BYTES * 2, 4 * 1024 * 1024] {
        let huge = vec![0x02u8; size];
        match dockapp.send(&huge) {
            // The usual outcome: EMSGSIZE, the kernel refusing to carry
            // it at all.
            Err(_) => continue,
            Ok(_) => {
                // A kernel with very large buffers might carry it. The
                // codec must then be the one to say no — and note that
                // `recv` into a MAX_MESSAGE_BYTES buffer truncates
                // silently (no `MSG_TRUNC`), so what arrives is a
                // ceiling-sized message whose contents are a lie. Both
                // roads end in `Err`.
                let n = shell_side.recv(&mut buffer).expect("a datagram the kernel accepted should arrive");
                assert!(
                    ClientMessage::decode(&buffer[..n]).is_err(),
                    "a {size}-byte message must not decode into anything the shell then blits"
                );
            }
        }
    }
}
