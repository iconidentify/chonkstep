//! End-to-end conformance test for the dockapp SDK's client loop,
//! against a real `SOCK_SEQPACKET` socket with a hand-written "shell" on
//! the other end.
//!
//! There is exactly ONE `#[test]` in this file, deliberately. It sets
//! `CHONKSTEP_DOCK_SOCKET` and `CHONKSTEP_DOCK_TOKEN`, which are
//! process-global; cargo runs the tests within one test binary on
//! parallel threads, so a second test here could observe the first
//! one's environment. Separate test *binaries* are separate processes,
//! so the isolation this needs is "one test per file", not "one test
//! per crate".
//!
//! The shell half is written out longhand rather than factored into a
//! helper because it is the sequence `Phase 4b`'s event loop has to
//! reproduce, and a reader following the protocol should be able to
//! read it in order.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chonk_dock_proto::handshake::validate_hello;
use chonk_dock_proto::transport::{mint_token, token_to_hex, Seqpacket, SeqpacketListener, ENV_SOCKET, ENV_TOKEN};
use chonk_dock_proto::wire::{frame_matches_tile, ClientMessage, GoodbyeReason, ServerMessage, ThemeState};
use chonk_dock_proto::MAX_MESSAGE_BYTES;
use chonk_ui::dockapp::{self, Handlers, LogLevel, Options, Pixmap};

const DEADLINE: Duration = Duration::from_secs(10);

/// Receives client messages until one satisfies `want`, discarding
/// anything else.
///
/// The discarding matters: a dockapp is free to push a frame at any
/// moment, so a test that demanded the *next* message be a `Pong` would
/// fail whenever the redraw timer happened to fire first. A shell has
/// to tolerate exactly the same interleaving.
fn recv_until<T>(peer: &Seqpacket, mut want: impl FnMut(ClientMessage) -> Option<T>) -> T {
    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let deadline = Instant::now() + DEADLINE;
    loop {
        let n = peer
            .recv_until(&mut buffer, deadline)
            .expect("recv")
            .expect("the dockapp went quiet");
        assert_ne!(n, 0, "the dockapp closed the connection");
        let message = ClientMessage::decode(&buffer[..n]).expect("a well-formed client message");
        if let Some(found) = want(message) {
            return found;
        }
        assert!(Instant::now() < deadline, "timed out waiting for the expected message");
    }
}

fn theme_state(tile_px: u32, scale: f32, theme_id: &str) -> ThemeState {
    ThemeState { tile_px, scale, theme_id: theme_id.to_string(), theme_toml: String::new() }
}

#[test]
fn a_dockapp_handshakes_draws_answers_pings_and_rethemes_without_restarting() {
    let dir = std::env::temp_dir().join(format!("chonk-ui-dockapp-conformance-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).expect("private scratch");
    let path: PathBuf = dir.join("dock.sock");

    let token = mint_token().expect("token");
    let listener = SeqpacketListener::bind(&path).expect("bind");
    std::env::set_var(ENV_SOCKET, &path);
    std::env::set_var(ENV_TOKEN, token_to_hex(&token));

    // The dockapp under test. `draw` paints the whole tile with an
    // opaque color derived from the tile size, so the shell side can
    // tell a scale-1 frame from a scale-2 one by looking at a pixel.
    let draws = Arc::new(AtomicU32::new(0));
    let draw_count = Arc::clone(&draws);
    let dockapp = std::thread::spawn(move || {
        dockapp::run_with(
            "conformance-tile",
            Options { redraw_interval: Duration::from_millis(50), ..Options::default() },
            Handlers {
                draw: move |ctx: &dockapp::Ctx, pixmap: &mut Pixmap| {
                    if draw_count.fetch_add(1, Ordering::Relaxed) == 0 {
                        // A dockapp's stdout and stderr are /dev/null,
                        // so this is its only way to say anything. The
                        // escape sequence is deliberate: the shell must
                        // receive it stripped.
                        ctx.log(LogLevel::Info, "tile up\u{1b}[2J\nsecond line");
                    }
                    let mark = ctx.tile_px() as u8;
                    for pixel in pixmap.data_mut().as_chunks_mut::<4>().0 {
                        pixel.copy_from_slice(&[mark, mark, mark, 0xFF]);
                    }
                    true
                },
                input: |_ctx: &dockapp::Ctx, _event| false,
            },
        )
    });

    // --- the shell half -------------------------------------------------

    let started = Instant::now();
    let peer = loop {
        if let Some(peer) = listener.accept().expect("accept") {
            break peer;
        }
        assert!(started.elapsed() < DEADLINE, "the dockapp never connected");
    };
    assert!(peer.is_nonblocking().expect("flags"), "an accepted dockapp socket must never be able to block the shell");
    assert!(peer.peer_is_this_user().expect("peer credentials"));

    // 1. Hello, validated exactly as the shell will validate it.
    let hello = recv_until(&peer, |m| matches!(m, ClientMessage::Hello { .. }).then_some(m));
    let accepted = validate_hello(&hello, &token, 56).expect("a correct Hello");
    assert_eq!(accepted.id, "conformance-tile");
    assert_eq!(accepted.tile_units, 1);

    // 2. Welcome, carrying the tile geometry and the theme.
    peer.send(&ServerMessage::Welcome(theme_state(56, 1.0, "nextstep-classic")).encode().unwrap()).unwrap();

    // 3. The tile's own diagnostics, sanitized on arrival. A dockapp
    //    that could put an ESC or a newline in the shell's journal
    //    could forge log entries or drive a terminal.
    let text = recv_until(&peer, |m| match m {
        ClientMessage::Log { text, .. } => Some(text),
        _ => None,
    });
    assert!(!text.chars().any(char::is_control), "control characters must not survive the wire: {text:?}");
    assert!(text.starts_with("tile up"), "{text:?}");

    // 4. The first frame arrives unprompted: the shell has nothing to
    //    show until it does, so the SDK sends one regardless of whether
    //    `draw` reported a change.
    let (width, height, pixels) = recv_until(&peer, |m| match m {
        ClientMessage::Frame { width, height, pixels, .. } => Some((width, height, pixels)),
        _ => None,
    });
    assert!(frame_matches_tile(width, height, 56, 1), "{width}x{height} is not the tile the shell allocated");
    assert_eq!(pixels.len(), 56 * 56 * 4);
    assert_eq!(&pixels[..4], &[56, 56, 56, 0xFF], "the dockapp's own pixels, premultiplied RGBA, top row first");

    // 5. Liveness. The Pong may be preceded by more frames; a shell has
    //    to cope with that, so this does too.
    peer.send(&ServerMessage::Ping { seq: 0xABCD }.encode().unwrap()).unwrap();
    let seq = recv_until(&peer, |m| match m {
        ClientMessage::Pong { seq } => Some(seq),
        _ => None,
    });
    assert_eq!(seq, 0xABCD);

    // 6. A theme change with a new scale and a new tile size. The whole
    //    point: the dockapp is NOT restarted, and its next frame is at
    //    the new geometry rather than the old one.
    peer.send(&ServerMessage::ThemeChanged(theme_state(112, 2.0, "amber-phosphor")).encode().unwrap()).unwrap();
    let (width, height, pixels) = recv_until(&peer, |m| match m {
        ClientMessage::Frame { width, height, pixels, .. } if width == 112 => Some((width, height, pixels)),
        _ => None,
    });
    assert!(frame_matches_tile(width, height, 112, 1));
    assert_eq!(pixels.len(), 112 * 112 * 4);
    assert_eq!(&pixels[..4], &[112, 112, 112, 0xFF], "the resized tile, drawn fresh rather than scaled up");

    // 7. Visibility off, then on. A hidden tile stops drawing entirely;
    //    becoming visible again forces a frame even though the drawn
    //    content has not changed.
    peer.send(&ServerMessage::Visibility { visible: false }.encode().unwrap()).unwrap();
    let quiet_at = draws.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(draws.load(Ordering::Relaxed), quiet_at, "a hidden dockapp should stop drawing, not just stop being seen");
    peer.send(&ServerMessage::Visibility { visible: true }.encode().unwrap()).unwrap();
    recv_until(&peer, |m| matches!(m, ClientMessage::Frame { .. }).then_some(()));

    // 8. The shell vanishes without saying goodbye — a crash, or
    //    `scripts/restart.sh`. The dockapp must see the EOF and come
    //    back to the same socket path rather than exiting.
    //
    //    This is the client half of restart survival. The shell half —
    //    a fresh shell inheriting the token and readopting the survivor
    //    into the same tile instead of launching a second copy — landed
    //    in Phase 4c and is tested against the real `DockHost`, real
    //    `RemoteTile`s and the real `admit` in
    //    `chonk-shell`'s `dockapp::restart_tests`. It cannot be tested
    //    from here: `chonk-ui` is the third-party SDK and must not
    //    depend on `chonk-shell` (lib.rs states that constraint), so
    //    the two halves are proven against each other's contract rather
    //    than in one process. What this file guarantees for that test is
    //    that the sequence it hand-writes is the sequence the real SDK
    //    performs.
    drop(peer);
    let reconnect_started = Instant::now();
    let peer = loop {
        if let Some(peer) = listener.accept().expect("accept") {
            break peer;
        }
        assert!(reconnect_started.elapsed() < DEADLINE, "the dockapp gave up instead of reconnecting");
    };
    let hello = recv_until(&peer, |m| matches!(m, ClientMessage::Hello { .. }).then_some(m));
    let readopted = validate_hello(&hello, &token, 56).expect("the reconnect re-presents its id and token");
    assert_eq!(readopted.id, "conformance-tile", "the same id, so a fresh shell can match it to the same slot");
    peer.send(&ServerMessage::Welcome(theme_state(56, 1.0, "nextstep-classic")).encode().unwrap()).unwrap();
    let (width, height, _) = recv_until(&peer, |m| match m {
        ClientMessage::Frame { width, height, pixels, .. } => Some((width, height, pixels)),
        _ => None,
    });
    assert!(frame_matches_tile(width, height, 56, 1), "the readopted tile draws at the new shell's geometry");

    // 9. Goodbye. `Shutdown` is a clean exit, not an error: the session
    //    is ending, and a dockapp that treated it as a failure would
    //    log noise on every logout.
    peer.send(&ServerMessage::Goodbye { reason: GoodbyeReason::Shutdown }.encode().unwrap()).unwrap();
    dockapp.join().expect("the dockapp thread should not panic").expect("Shutdown is a clean exit");

    let _ = std::fs::remove_dir_all(&dir);
}
