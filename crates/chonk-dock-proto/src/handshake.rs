//! The handshake: what a dockapp has to prove before the shell gives it
//! a tile.
//!
//! Three separate things are being checked, and it is worth naming them
//! apart because they fail for different reasons and deserve different
//! answers:
//!
//! 1. **Who are you.** The 128-bit token from `CHONKSTEP_DOCK_TOKEN`,
//!    minted per slot by the shell. This is the only check that is
//!    security rather than hygiene, and it is what stops a stray
//!    process of this user from claiming a dock slot it was not
//!    launched for. (The socket's 0600 mode in a 0700 directory is the
//!    outer lock; `SO_PEERCRED` is the third. None of them is load
//!    bearing alone.)
//! 2. **Do we speak the same protocol.** The `Hello` version must be
//!    in `1..=`[`crate::PROTOCOL_VERSION`] — every version this build
//!    knows is accepted (and remembered: what the shell may put in a
//!    formerly-reserved field is keyed on it, see
//!    [`crate::wire::ThemeState::for_client`]), while a *newer* one is
//!    refused, because a peer from the future is as unreadable as one
//!    presenting garbage — see [`crate::wire`].
//! 3. **Can this tile physically work.** A geometry the v1 inline
//!    transport cannot carry is rejected *here*, at connect time, with
//!    a reason — rather than accepted and then failing on every single
//!    frame for the rest of the session.
//!
//! The validator is pure and takes a decoded message, so the shell can
//! drive it from its own event loop without this crate having an
//! opinion about how the shell waits for anything.

use std::io;
use std::time::{Duration, Instant};

use crate::transport::{tokens_match, Seqpacket};
use crate::wire::{ClientMessage, GoodbyeReason, InputMask, ServerMessage, ThemeState};
use crate::{frame_fits, MAX_MESSAGE_BYTES, PROTOCOL_VERSION, TOKEN_BYTES};

/// What the shell learned from an accepted `Hello`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Accepted {
    pub id: String,
    pub tile_units: u8,
    pub wants: InputMask,
    /// The protocol version the client announced, `1..=`
    /// [`PROTOCOL_VERSION`]. The shell keys two things on it: what it
    /// may put in the formerly-reserved `proto` u16 of
    /// `Welcome`/`ThemeChanged` ([`ThemeState::for_client`] — a
    /// version-1 client must see the byte-exact v1 wire, zeros
    /// included), and whether the panel family (`0x05`–`0x07`) is
    /// legal from this connection at all (it needs `>= 2`).
    pub proto: u32,
}

/// Decides whether a `Hello` earns a tile.
///
/// Returns the reason to put in `Goodbye` rather than an opaque error,
/// so a rejected dockapp learns something it can act on: a version
/// mismatch means "rebuild me", a bad token means "you were not
/// launched by this shell", and `TileTooLarge` means "ask for fewer
/// tile units". The shell logs the detail; the wire carries the reason.
///
/// Anything that is not a `Hello` — including a `Frame` arriving before
/// the handshake — is a protocol error. A dockapp does not get to skip
/// authentication by simply starting to draw.
pub fn validate_hello(
    message: &ClientMessage,
    expected_token: &[u8; TOKEN_BYTES],
    tile_px: u32,
) -> Result<Accepted, GoodbyeReason> {
    let ClientMessage::Hello { proto, id, tile_units, token, wants } = message else {
        return Err(GoodbyeReason::ProtocolError);
    };
    // Every version this build knows is welcome; only a *newer* one is
    // refused (a peer from the future is unreadable, and "reject with a
    // reason" beats misparsing). Rejecting version 1 here would be the
    // Welcome-field incident from the other side: a wire change that
    // orphans every deployed conformant client. Which version was said
    // is carried out in `Accepted::proto` — the shell keys the
    // formerly-reserved `Welcome` field and the panel-family gate on it.
    if *proto == 0 || *proto > PROTOCOL_VERSION {
        return Err(GoodbyeReason::ProtocolError);
    }
    // Checked before the geometry so that a wrong token never learns
    // anything from *which* rejection it got.
    if !tokens_match(token, expected_token) {
        return Err(GoodbyeReason::Unauthorized);
    }
    if !frame_fits(tile_px, *tile_units) {
        return Err(GoodbyeReason::TileTooLarge);
    }
    Ok(Accepted { id: id.clone(), tile_units: *tile_units, wants: *wants, proto: *proto })
}

/// Builds the `Hello` a dockapp opens with.
pub fn hello(id: &str, tile_units: u8, token: [u8; TOKEN_BYTES], wants: InputMask) -> ClientMessage {
    ClientMessage::Hello { proto: PROTOCOL_VERSION, id: id.to_string(), tile_units, token, wants }
}

/// How long a dockapp waits for its `Welcome` before giving up.
///
/// The shell answers a `Hello` from its event loop, so the honest
/// bound is "one or two repaint passes" (16 ms each). Two seconds is
/// three orders of magnitude of slack for a machine under load, and
/// still short enough that a dockapp pointed at a wedged shell exits
/// while the user is still looking at it.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// The client half: send `Hello`, wait for `Welcome`.
///
/// Deliberately strict about what may arrive first. The shell sends
/// `Welcome` before anything else, so a `Goodbye` here is a rejection
/// worth reporting with its reason, and anything *else* means the two
/// ends disagree about the protocol badly enough that continuing would
/// be guessing.
pub fn client_handshake(
    socket: &Seqpacket,
    id: &str,
    tile_units: u8,
    token: [u8; TOKEN_BYTES],
    wants: InputMask,
) -> io::Result<ThemeState> {
    let bytes = hello(id, tile_units, token, wants)
        .encode()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))?;
    socket.send(&bytes)?;

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    let Some(n) = socket.recv_until(&mut buffer, deadline)? else {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "no Welcome from the shell"));
    };
    if n == 0 {
        return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the shell closed the connection during the handshake"));
    }
    match ServerMessage::decode(&buffer[..n]) {
        Ok(ServerMessage::Welcome(state)) => Ok(state),
        Ok(ServerMessage::Goodbye { reason }) => Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the shell refused this dockapp: {reason:?}"),
        )),
        Ok(other) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("expected Welcome, got {other:?}"),
        )),
        Err(e) => Err(io::Error::new(io::ErrorKind::InvalidData, e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{mint_token, SeqpacketListener};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    static COUNTER: AtomicU32 = AtomicU32::new(0);

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("chonk-dock-handshake-{}-{unique}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
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

    fn theme_state() -> ThemeState {
        ThemeState { tile_px: 56, scale: 1.0, proto: crate::SHELL_PROTOCOL_VERSION, theme_id: "nextstep-classic".into(), theme_toml: String::new() }
    }

    #[test]
    fn a_correct_hello_is_accepted() {
        let token = mint_token().unwrap();
        let message = hello("clock", 1, token, InputMask::all());
        assert_eq!(
            validate_hello(&message, &token, 56),
            Ok(Accepted { id: "clock".into(), tile_units: 1, wants: InputMask::all(), proto: PROTOCOL_VERSION })
        );
    }

    #[test]
    fn a_wrong_token_is_unauthorized() {
        let token = mint_token().unwrap();
        let mut wrong = token;
        wrong[0] ^= 0xFF;
        let message = hello("clock", 1, wrong, InputMask::none());
        assert_eq!(validate_hello(&message, &token, 56), Err(GoodbyeReason::Unauthorized));
    }

    #[test]
    fn every_hello_version_this_build_knows_is_accepted_and_remembered() {
        // The range check, not equality: refusing version 1 here would
        // be the Welcome-field incident from the other side — a
        // deployed, conformant v1 instrument orphaned by an upgrade it
        // never asked for. The version is carried out in `Accepted`
        // because the shell keys the formerly-reserved `Welcome` field
        // and the panel-family gate on it.
        let token = mint_token().unwrap();
        for proto in 1..=PROTOCOL_VERSION {
            let message =
                ClientMessage::Hello { proto, id: "clock".into(), tile_units: 1, token, wants: InputMask::none() };
            assert_eq!(
                validate_hello(&message, &token, 56),
                Ok(Accepted { id: "clock".into(), tile_units: 1, wants: InputMask::none(), proto }),
                "version {proto} is one this build speaks and must keep working"
            );
        }
    }

    #[test]
    fn a_protocol_version_from_the_future_is_refused() {
        // A newer peer is as unreadable as one presenting garbage, and
        // "reject with a reason" beats misparsing. Zero is not a
        // version anything ever announced.
        let token = mint_token().unwrap();
        for proto in [0, PROTOCOL_VERSION + 1, u32::MAX] {
            let message =
                ClientMessage::Hello { proto, id: "clock".into(), tile_units: 1, token, wants: InputMask::none() };
            assert_eq!(
                validate_hello(&message, &token, 56),
                Err(GoodbyeReason::ProtocolError),
                "proto {proto} must not be accepted"
            );
        }
    }

    #[test]
    fn a_tile_the_transport_cannot_carry_is_refused_at_connect_time() {
        // Not once per frame for the rest of the session.
        let token = mint_token().unwrap();
        let message = hello("huge", 4, token, InputMask::none());
        assert_eq!(validate_hello(&message, &token, 168), Err(GoodbyeReason::TileTooLarge));
        assert_eq!(
            validate_hello(&message, &token, 112),
            Ok(Accepted { id: "huge".into(), tile_units: 4, wants: InputMask::none(), proto: PROTOCOL_VERSION })
        );
    }

    #[test]
    fn a_dockapp_cannot_skip_the_handshake_by_drawing() {
        let token = mint_token().unwrap();
        let frame = ClientMessage::Frame { generation: 0, width: 56, height: 56, pixels: vec![0; 56 * 56 * 4] };
        assert_eq!(validate_hello(&frame, &token, 56), Err(GoodbyeReason::ProtocolError));
        assert_eq!(validate_hello(&ClientMessage::Pong { seq: 0 }, &token, 56), Err(GoodbyeReason::ProtocolError));
    }

    #[test]
    fn a_zero_tile_unit_hello_is_refused() {
        // Zero tiles is not a tile; `frame_fits` says so, and this is
        // the path where that matters.
        let token = mint_token().unwrap();
        assert_eq!(validate_hello(&hello("z", 0, token, InputMask::none()), &token, 56), Err(GoodbyeReason::TileTooLarge));
    }

    #[test]
    fn the_whole_handshake_works_over_a_real_socket() {
        // End to end through the actual transport, with the shell's
        // half written out inline: this is the sequence Phase 4b's
        // event loop has to reproduce, and the SDK's `client_handshake`
        // is the client of record for it.
        let scratch = Scratch::new();
        let token = mint_token().unwrap();
        let listener = SeqpacketListener::bind(&scratch.socket()).unwrap();

        let path = scratch.socket();
        let client = std::thread::spawn(move || {
            let socket = Seqpacket::connect(&path).unwrap();
            client_handshake(&socket, "clock", 1, token, InputMask::all())
        });

        // The "shell": poll for a connection, read the Hello, validate,
        // answer with Welcome.
        let peer = loop {
            if let Some(peer) = listener.accept().unwrap() {
                break peer;
            }
        };
        assert!(peer.peer_is_this_user().unwrap());
        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        let deadline = Instant::now() + Duration::from_secs(5);
        let n = peer.recv_until(&mut buffer, deadline).unwrap().expect("a Hello should arrive");
        let message = ClientMessage::decode(&buffer[..n]).expect("a well-formed Hello");
        let accepted = validate_hello(&message, &token, 56).expect("accepted");
        assert_eq!(accepted.id, "clock");
        peer.send(&ServerMessage::Welcome(theme_state()).encode().unwrap()).unwrap();

        let state = client.join().unwrap().expect("the client should be welcomed");
        assert_eq!(state, theme_state());
    }

    #[test]
    fn a_refused_dockapp_learns_why_instead_of_seeing_a_bare_eof() {
        let scratch = Scratch::new();
        let token = mint_token().unwrap();
        let listener = SeqpacketListener::bind(&scratch.socket()).unwrap();
        let path = scratch.socket();
        let client = std::thread::spawn(move || {
            let socket = Seqpacket::connect(&path).unwrap();
            client_handshake(&socket, "clock", 1, [0u8; TOKEN_BYTES], InputMask::none())
        });

        let peer = loop {
            if let Some(peer) = listener.accept().unwrap() {
                break peer;
            }
        };
        let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
        let n = peer.recv_until(&mut buffer, Instant::now() + Duration::from_secs(5)).unwrap().unwrap();
        let message = ClientMessage::decode(&buffer[..n]).unwrap();
        let reason = validate_hello(&message, &token, 56).expect_err("the token is wrong");
        peer.send(&ServerMessage::Goodbye { reason }.encode().unwrap()).unwrap();

        let err = client.join().unwrap().expect_err("the handshake must fail");
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        assert!(err.to_string().contains("Unauthorized"), "{err}");
    }

    #[test]
    fn a_shell_that_never_answers_times_out_rather_than_hanging_forever() {
        let scratch = Scratch::new();
        let listener = SeqpacketListener::bind(&scratch.socket()).unwrap();
        let socket = Seqpacket::connect(&scratch.socket()).unwrap();
        let _peer = loop {
            if let Some(peer) = listener.accept().unwrap() {
                break peer;
            }
        };
        // Deliberately never answers. `client_handshake` uses
        // HANDSHAKE_TIMEOUT, so this test costs that.
        let started = Instant::now();
        let err = client_handshake(&socket, "clock", 1, [0u8; TOKEN_BYTES], InputMask::none()).expect_err("no Welcome");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= HANDSHAKE_TIMEOUT);
        assert!(started.elapsed() < HANDSHAKE_TIMEOUT * 3, "the wait should be bounded by the deadline, not by luck");
    }
}
