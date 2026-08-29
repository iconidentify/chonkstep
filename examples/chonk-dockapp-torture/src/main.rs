//! The hostile dockapp. It misbehaves on demand, one named failure
//! mode at a time, so that the shell's handling of a misbehaving tile
//! is something you can *watch happen* instead of something the design
//! document asserts.
//!
//! ```text
//! chonk-dockapp-torture <mode>          # or --mode <mode>, or CHONKSTEP_TORTURE_MODE
//!
//!   hang            connect, draw once, then stop reading the socket forever
//!   flood           send frames as fast as the socket will take them
//!   crash           connect, draw once, then exit non-zero
//!   crash-loop      exit non-zero immediately, before connecting
//!   malformed       complete the handshake, then send a battery of illegal messages
//!   slow-handshake  connect and never send Hello
//!   wrong-token     present a token this shell never minted
//! ```
//!
//! # Why this exists
//!
//! The accepted design names one risk above all the others:
//!
//! > TOP RISK — backpressure done wrong IS the original bug with a
//! > different syscall. A blocking `write()` to a dockapp that stopped
//! > reading parks the compositor. [...] The Phase 5 torture example
//! > exists primarily to prove this.
//!
//! "The original bug" is not a hypothetical. On 2026-08-29 a dock
//! widget called `nmcli dev wifi` from `DockWidget::tick()`, which runs
//! on the compositor's single repaint thread. `--rescan auto` blocks
//! for a hardware scan whenever NetworkManager's cache is older than
//! thirty seconds: ~3.6 seconds of frozen desktop, once every ~34
//! seconds, during which the compositor drew nothing, read no input,
//! and left a page-flip completion sitting unread in its DRM fd — so
//! its own stall watchdog reported the display driver, which was idle
//! and correct the entire time. Four agents and a tour of DRM internals
//! later, the culprit was a wifi icon.
//!
//! `--mode hang` is that incident, rebuilt deliberately, on the far
//! side of a process boundary. The desktop is supposed to not care.
//! Run it and watch the clock keep ticking.
//!
//! # Using the SDK where the SDK will do, and bytes where it will not
//!
//! `flood` is a real [`chonk_ui::dockapp::run_with`] loop, because the
//! SDK can express "draw as fast as you can" honestly and a torture
//! client sharing no code with a real one would be testing a different
//! program.
//!
//! Everything else drops to `chonk-dock-proto` and assembles datagrams
//! by hand, because the SDK is *built* not to do these things: its
//! encoder refuses an over-long id, its event loop always reads its
//! socket, and its send path cannot lie about a frame's dimensions. A
//! torture client that could only do what the SDK permits could not
//! test the shell's hostility handling at all — the interesting inputs
//! are precisely the ones no correct client produces.
//!
//! # Registering it
//!
//! A dockapp is launched by the dock, not from a prompt: run it by hand
//! and it will tell you `CHONKSTEP_DOCK_SOCKET` is missing. Give it a
//! registry entry under `$XDG_DATA_DIRS/chonkstep/dockapps/` (or the
//! per-user `$XDG_CONFIG_HOME/chonkstep/dockapps/`) along the lines of
//!
//! ```toml
//! id = "chonk-dockapp-torture"
//! name = "Torture"
//! exec = "/path/to/chonk-dockapp-torture hang"
//! tile_units = 1
//! restart = "on-crash"     # "always" to watch the crash-loop cutoff fire
//! ```
//!
//! The exact schema belongs to the shell's registry loader; check it
//! against that rather than against this comment, which is a sketch of
//! the design's `id, name, exec, tile_units, restart` and nothing more
//! authoritative. `CHONKSTEP_TORTURE_ID` overrides the id this binary
//! presents in `Hello`, for a registry entry named something else.
//!
//! # What each mode should look like from the outside
//!
//! Stated here because "the test passed" is a weaker claim than "the
//! desktop behaved", and this is the file that says what behaving means:
//!
//! - `hang`: the tile freezes on its last frame, then dims (~50% toward
//!   `theme.tile.fill`) once three liveness pings go unanswered. Every
//!   other tile keeps updating. Input keeps working. The dock keeps
//!   drawing. **Nothing else changes at all** — that is the deliverable.
//! - `flood`: the tile updates smoothly at about 30 Hz no matter how
//!   fast this process writes, and the shell's log reports coalesced
//!   frames rather than a growing queue.
//! - `crash` / `crash-loop`: a dead tile face, relaunches on the
//!   1/2/4/8/30s backoff, and then a permanent stop after five failures
//!   in sixty seconds. The cutoff is the point: a dockapp restarted
//!   forever is an invisible fork bomb.
//! - `malformed`: every message is refused with a `Goodbye`, and the
//!   connection closes. Nothing this process sends should ever reach a
//!   pixel. Note in particular the two frames with *legal but wrong*
//!   dimensions: those must be rejected, never scaled or letterboxed,
//!   because a frame from before a monitor change is not a frame to
//!   stretch.
//! - `slow-handshake`: the slot stays in its "starting" face and the
//!   shell eventually gives up on it. The rest of the desktop never
//!   waits for it, not even for one frame.
//! - `wrong-token`: refused with `Goodbye { Unauthorized }` before it
//!   gets a tile at all.

use std::io;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use chonk_dock_proto::transport::{token_from_hex, Seqpacket, ENV_SOCKET, ENV_TOKEN};
use chonk_dock_proto::wire::{ClientMessage, InputMask, LogLevel, ServerMessage, ThemeState};
use chonk_dock_proto::{handshake, MAX_MESSAGE_BYTES, TOKEN_BYTES};
use chonk_ui::dockapp::{self, Handlers, Options, Pixmap};

/// The id presented in `Hello`. Must match the registry entry that
/// declared this program, or the shell has no slot to give it.
const DEFAULT_ID: &str = "chonk-dockapp-torture";

/// Exit codes, distinct on purpose so that a shell-side log saying
/// "child exited 70" identifies *which* deliberate failure it was
/// rather than just "non-zero".
mod exit {
    /// `--mode crash`: ran, connected, then died. Exercises the
    /// restart backoff.
    pub const CRASHED_AFTER_CONNECTING: u8 = 70;
    /// `--mode crash-loop`: died before connecting. Exercises the hard
    /// crash-loop cutoff.
    pub const CRASHED_IMMEDIATELY: u8 = 69;
    /// Usage error — an unknown mode, or no mode at all.
    pub const USAGE: u8 = 2;
    /// The environment says this was not launched by a dock.
    pub const NOT_LAUNCHED_BY_THE_DOCK: u8 = 3;
    /// A mode that expected to complete a handshake did not.
    pub const HANDSHAKE_FAILED: u8 = 4;
}

fn main() -> ExitCode {
    let Some(mode) = requested_mode() else {
        usage();
        return ExitCode::from(exit::USAGE);
    };

    match mode.as_str() {
        // First, before anything else touches the environment or the
        // socket: this mode's entire behaviour is to be gone before the
        // shell can even observe a connection attempt, which is the
        // input the crash-loop cutoff has to survive.
        "crash-loop" => {
            eprintln!("torture: exiting {} immediately, before connecting", exit::CRASHED_IMMEDIATELY);
            ExitCode::from(exit::CRASHED_IMMEDIATELY)
        }
        "hang" => hang(),
        "flood" => flood(),
        "crash" => crash(),
        "malformed" => malformed(),
        "slow-handshake" => slow_handshake(),
        "wrong-token" => wrong_token(),
        other => {
            eprintln!("torture: unknown mode {other:?}");
            usage();
            ExitCode::from(exit::USAGE)
        }
    }
}

/// argv first, environment second.
///
/// Both, because the two ways a dockapp gets configured are the `exec`
/// line in its registry file (argv) and the environment the shell
/// launches it with — and during a debugging session you want to change
/// the mode without editing a TOML file the shell only rescans at
/// startup. argv wins so a registry entry can pin a mode that the
/// environment cannot silently override.
fn requested_mode() -> Option<String> {
    let mut args = std::env::args().skip(1);
    let from_argv = match args.next().as_deref() {
        // Nothing on the command line: fall through to the environment.
        None => None,
        Some("--mode" | "-m") => return args.next(),
        Some("--help" | "-h") => return None,
        // Any other flag is a usage error rather than a mode, so that a
        // typo produces the help text instead of "unknown mode
        // \"--hnag\"".
        Some(flag) if flag.starts_with('-') => return None,
        Some(mode) => Some(mode.to_string()),
    };
    from_argv.or_else(|| std::env::var("CHONKSTEP_TORTURE_MODE").ok().filter(|s| !s.is_empty()))
}

fn usage() {
    eprintln!(
        "\
chonk-dockapp-torture <mode>          (or --mode <mode>, or CHONKSTEP_TORTURE_MODE)

  hang            connect, draw once, then stop reading the socket forever
  flood           send frames as fast as the socket will take them
  crash           connect, draw once, then exit {crashed}
  crash-loop      exit {immediate} immediately, before connecting
  malformed       complete the handshake, then send a battery of illegal messages
  slow-handshake  connect and never send Hello
  wrong-token     present a token this shell never minted

Environment:
  CHONKSTEP_TORTURE_ID        the id presented in Hello (default {DEFAULT_ID:?})
  CHONKSTEP_TORTURE_DELAY_MS  how long `crash` stays alive first (default 1500)

This is a dockapp: it is launched by the dock, which sets
{ENV_SOCKET} and {ENV_TOKEN}. Running it from a prompt will
tell you they are missing.",
        crashed = exit::CRASHED_AFTER_CONNECTING,
        immediate = exit::CRASHED_IMMEDIATELY,
    );
}

fn id() -> String {
    std::env::var("CHONKSTEP_TORTURE_ID").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| DEFAULT_ID.to_string())
}

// ---------------------------------------------------------------------
// Raw connection helpers
// ---------------------------------------------------------------------

/// Reads the same two variables the SDK reads.
///
/// Duplicated rather than borrowed because the SDK's own
/// `connection_details` is private, and deliberately so: it is an
/// implementation detail of `dockapp::run`, not a contract. The
/// variable *names* are the contract, and they are public constants in
/// `chonk-dock-proto`, which is what this uses.
fn connection_details() -> Result<(std::path::PathBuf, [u8; TOKEN_BYTES]), String> {
    let path = std::env::var_os(ENV_SOCKET)
        .ok_or_else(|| format!("{ENV_SOCKET} is not set: a dockapp is launched by the dock, not run from a shell"))?;
    let hex = std::env::var(ENV_TOKEN).map_err(|_| format!("{ENV_TOKEN} is not set"))?;
    let token = token_from_hex(&hex).ok_or_else(|| format!("{ENV_TOKEN} is not 32 hex digits"))?;
    Ok((std::path::PathBuf::from(path), token))
}

/// Connects and completes a *correct* handshake, so that the misbehavior
/// that follows is the only variable.
///
/// Uses the public `handshake::client_handshake`: a torture client that
/// got its handshake subtly wrong would produce failures the shell was
/// right to reject, which is the least useful kind of test result.
fn connected() -> Result<(Seqpacket, ThemeState), ExitCode> {
    let (path, token) = connection_details().map_err(|e| {
        eprintln!("torture: {e}");
        ExitCode::from(exit::NOT_LAUNCHED_BY_THE_DOCK)
    })?;
    let socket = Seqpacket::connect(&path).map_err(|e| {
        eprintln!("torture: connect({}): {e}", path.display());
        ExitCode::from(exit::NOT_LAUNCHED_BY_THE_DOCK)
    })?;
    let state = handshake::client_handshake(&socket, &id(), 1, token, InputMask::all()).map_err(|e| {
        eprintln!("torture: handshake: {e}");
        ExitCode::from(exit::HANDSHAKE_FAILED)
    })?;
    eprintln!("torture: connected; tile is {}px at scale {}", state.tile_px, state.scale);
    Ok((socket, state))
}

/// One legitimate, well-formed frame, so a mode that is about to
/// misbehave leaves the shell something real to show.
///
/// It matters for `hang` in particular: the design says a hung tile
/// shows *its last good frame, dimmed*, and a tile that never sent one
/// would be exercising the "starting" face instead of the "hung" face.
fn good_frame(state: &ThemeState, generation: u32, tint: u8) -> Vec<u8> {
    let width = state.tile_px.max(1);
    let height = width;
    let mut pixmap = Pixmap::new(width, height).expect("a tile-sized pixmap");
    // Premultiplied by construction: fully opaque, so the premultiplied
    // and straight forms are the same bytes and there is nothing to get
    // wrong. `from_rgba8` on tiny-skia's `Color` takes straight values.
    pixmap.fill(chonk_ui::tiny_skia::Color::from_rgba8(tint, 0x30, 0x30, 0xFF));
    ClientMessage::Frame { generation, width, height, pixels: pixmap.data().to_vec() }
        .encode()
        .expect("a tile-sized frame encodes")
}

// ---------------------------------------------------------------------
// hang
// ---------------------------------------------------------------------

/// **The mode this whole example exists for.** Connect, draw once, then
/// stop reading the socket. Forever.
///
/// The shell will keep sending: liveness pings every two seconds, and
/// any pointer event that lands on this tile. Nobody is at the other
/// end to receive them, so this process's receive buffer fills, and
/// then every `send()` the shell makes returns `EAGAIN`. That is the
/// exact moment the desktop either survives or freezes:
///
/// - With a blocking `write()`, the shell's repaint thread parks here
///   and the desktop stops — stops drawing, stops reading input, stops
///   collecting page-flip completions — and the stall gets blamed on
///   the display driver, for the second time in this project's history.
/// - With `MSG_DONTWAIT` plus a bounded [`SendQueue`], the shell drops
///   the oldest queued message, notices after two seconds of sustained
///   overflow, and closes this connection. The user sees one dimmed
///   tile. Nothing else changes.
///
/// The assertion form of this is
/// `chonk-dock-proto/tests/hostile_peer.rs::a_hung_dockapp_costs_the_repaint_thread_nothing`.
/// This is the version you can watch.
///
/// [`SendQueue`]: chonk_dock_proto::SendQueue
fn hang() -> ExitCode {
    // `?`-style propagation, so that "you ran this from a prompt" (exit
    // 3) stays distinguishable from "the shell refused the handshake"
    // (exit 4). Collapsing them would make the most common user error
    // look like a shell bug.
    let (socket, state) = match connected() {
        Ok(pair) => pair,
        Err(code) => return code,
    };

    // One good frame first, so the shell has a last-good-frame to dim
    // rather than a slot that never started.
    if let Err(e) = socket.send(&good_frame(&state, 1, 0xC0)) {
        eprintln!("torture: could not send the first frame: {e}");
    }
    let _ = socket.send(
        &ClientMessage::Log { level: LogLevel::Warn, text: "torture: about to stop reading this socket forever".into() }
            .encode()
            .expect("a short log line encodes"),
    );

    eprintln!(
        "torture: hanging. The socket stays open and is never read again. \
         The desktop should not notice in any way that matters."
    );

    // Deliberately *not* `recv`. The socket stays open — this is a hung
    // peer, not a dead one, which is the harder case: EOF would tell the
    // shell what happened in one event-loop pass, whereas this tells it
    // nothing at all and has to be discovered by the liveness ping.
    //
    // `park_timeout` in a loop rather than `park()` alone because a bare
    // park can return spuriously and a bare `loop {}` would spin a core
    // hot, which would make this mode look like a *busy* dockapp and
    // muddy exactly the measurement it exists to make.
    loop {
        std::thread::park_timeout(Duration::from_secs(3600));
    }
}

// ---------------------------------------------------------------------
// flood
// ---------------------------------------------------------------------

/// Draw and send as fast as the socket will take it — far above the
/// shell's 30 Hz limiter.
///
/// Written on the public SDK on purpose. The SDK can express this
/// honestly (`redraw_interval: Duration::ZERO` and a `draw` that always
/// reports a change), so writing it by hand would be testing a program
/// that no real dockapp resembles. What it proves is on the shell's
/// side: the token bucket must *coalesce* — process the newest frame
/// and discard the ones it superseded — rather than queue, because
/// queuing means spending the compositor's repaint budget drawing
/// frames that were obsolete before they were read.
///
/// Note the SDK's own send drops a frame on `WouldBlock` instead of
/// blocking, so this process throttles itself against the socket rather
/// than parking. That is the SDK holding up its half of the same
/// bargain: a busy compositor must not become a stalled dockapp either.
fn flood() -> ExitCode {
    eprintln!("torture: flooding. Frames go out as fast as the socket accepts them; the shell should show ~30/s.");
    let options = Options { redraw_interval: Duration::ZERO, ..Options::default() };
    let mut generation: u32 = 0;
    let started = Instant::now();
    let result = dockapp::run_with(
        &id(),
        options,
        Handlers {
            draw: |_ctx: &dockapp::Ctx, pixmap: &mut Pixmap| {
                generation = generation.wrapping_add(1);
                // A colour that actually changes every frame, so a
                // limiter that silently delivered every frame would be
                // visible as a strobe rather than hidden as a repaint of
                // identical pixels.
                let phase = (generation % 256) as u8;
                pixmap.fill(chonk_ui::tiny_skia::Color::from_rgba8(phase, 0x20, 255 - phase, 0xFF));
                if generation.is_multiple_of(1000) {
                    let rate = f64::from(generation) / started.elapsed().as_secs_f64().max(f64::MIN_POSITIVE);
                    eprintln!("torture: {generation} frames offered, {rate:.0}/s");
                }
                true
            },
            input: |_ctx: &dockapp::Ctx, _event| true,
        },
    );
    if let Err(e) = result {
        eprintln!("torture: {e}");
        // Same distinction the raw modes make by hand: "you ran this
        // from a prompt" is a different answer from "the shell refused
        // me", and a supervisor reading exit codes should not have to
        // guess which.
        return ExitCode::from(match e {
            dockapp::Error::Environment(_) => exit::NOT_LAUNCHED_BY_THE_DOCK,
            _ => exit::HANDSHAKE_FAILED,
        });
    }
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------
// crash
// ---------------------------------------------------------------------

/// Connect, draw once, then die non-zero.
///
/// Distinct from `crash-loop`: this one *works* for a moment first, so
/// the shell sees a healthy tile go away and applies the 1/2/4/8/30s
/// restart backoff. `crash-loop` never connects at all, which is the
/// input the hard "five failures in sixty seconds, then stop
/// permanently" cutoff exists for. Both are needed, because a backoff
/// that never becomes a cutoff is still a fork bomb, just a polite one.
///
/// `CHONKSTEP_TORTURE_DELAY_MS` tunes how long it lives — set it below
/// the backoff's first step to watch the cutoff arrive quickly, or
/// above it to watch the backoff reset.
fn crash() -> ExitCode {
    let (socket, state) = match connected() {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    let _ = socket.send(&good_frame(&state, 1, 0xE0));
    let delay = std::env::var("CHONKSTEP_TORTURE_DELAY_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or(Duration::from_millis(1500), Duration::from_millis);
    eprintln!("torture: alive for {delay:?}, then exiting {}", exit::CRASHED_AFTER_CONNECTING);
    std::thread::sleep(delay);
    // A hard `exit` rather than an unwind: this is meant to look to the
    // shell exactly like a dockapp that fell over, and a tidy shutdown
    // that closed the socket first would be testing the graceful path.
    std::process::exit(i32::from(exit::CRASHED_AFTER_CONNECTING));
}

// ---------------------------------------------------------------------
// slow-handshake and wrong-token
// ---------------------------------------------------------------------

/// Connect, then never send `Hello`.
///
/// The handshake is the one exchange where the shell is waiting for a
/// *specific* message, which is the shape that most tempts an
/// implementation into "just block here, it will only be a moment".
/// This is the peer for whom it is never a moment. The shell should
/// leave the slot on its "starting" face, keep drawing everything else
/// at full rate, and eventually give up on the connection.
fn slow_handshake() -> ExitCode {
    let (path, _token) = match connection_details() {
        Ok(details) => details,
        Err(e) => {
            eprintln!("torture: {e}");
            return ExitCode::from(exit::NOT_LAUNCHED_BY_THE_DOCK);
        }
    };
    let socket = match Seqpacket::connect(&path) {
        Ok(socket) => socket,
        Err(e) => {
            eprintln!("torture: connect: {e}");
            return ExitCode::from(exit::NOT_LAUNCHED_BY_THE_DOCK);
        }
    };
    eprintln!("torture: connected and saying nothing. No Hello is coming.");
    // Held open, never written to. Reading is fine here — the point of
    // this mode is the *absence of a Hello*, not a wedged buffer, and
    // reading lets it report what the shell does about it, which is the
    // diagnostic a shell author wants.
    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    loop {
        match socket.recv_until(&mut buffer, Instant::now() + Duration::from_secs(5)) {
            Ok(None) => eprintln!("torture: still silent, still connected"),
            Ok(Some(0)) => {
                eprintln!("torture: the shell closed the connection on a handshake that never arrived");
                return ExitCode::SUCCESS;
            }
            Ok(Some(n)) => match ServerMessage::decode(&buffer[..n]) {
                Ok(message) => eprintln!("torture: the shell sent {message:?} before any Hello"),
                Err(e) => eprintln!("torture: undecodable message from the shell: {e}"),
            },
            Err(e) => {
                eprintln!("torture: recv: {e}");
                return ExitCode::SUCCESS;
            }
        }
    }
}

/// Present a token the shell never minted.
///
/// The token is the only part of the handshake that is security rather
/// than hygiene: the socket's 0600 mode in a 0700 directory keeps other
/// users out, and `SO_PEERCRED` confirms it, but only the token stops a
/// *stray process of this same user* from claiming a dock slot it was
/// not launched for. The expected outcome is
/// `Goodbye { Unauthorized }` and no tile — and, just as importantly,
/// no information about which of the other checks would also have
/// failed.
fn wrong_token() -> ExitCode {
    let (path, token) = match connection_details() {
        Ok(details) => details,
        Err(e) => {
            eprintln!("torture: {e}");
            return ExitCode::from(exit::NOT_LAUNCHED_BY_THE_DOCK);
        }
    };
    let mut wrong = token;
    // One bit. A wholly random token would also be refused, but flipping
    // a single bit of the real one is the case that would slip past a
    // comparison someone had "optimized" into a prefix check.
    wrong[TOKEN_BYTES - 1] ^= 0x01;

    let socket = match Seqpacket::connect(&path) {
        Ok(socket) => socket,
        Err(e) => {
            eprintln!("torture: connect: {e}");
            return ExitCode::from(exit::NOT_LAUNCHED_BY_THE_DOCK);
        }
    };
    match handshake::client_handshake(&socket, &id(), 1, wrong, InputMask::none()) {
        Ok(state) => {
            eprintln!(
                "torture: DEFECT — the shell accepted a token it never minted and offered a {}px tile. \
                 A wrong token must be refused with Goodbye {{ Unauthorized }}.",
                state.tile_px
            );
            ExitCode::from(exit::HANDSHAKE_FAILED)
        }
        Err(e) => {
            eprintln!("torture: refused, as it should be: {e}");
            ExitCode::SUCCESS
        }
    }
}

// ---------------------------------------------------------------------
// malformed
// ---------------------------------------------------------------------

/// Complete a correct handshake, then send everything a correct client
/// never would.
///
/// Every datagram below is assembled by hand. That is not stubbornness:
/// `ClientMessage::encode` refuses to build most of them (an id outside
/// the allowlist, a frame whose payload disagrees with its geometry, a
/// log line over its cap), and the ones it would build it *sanitizes*
/// on the way out. The shell's decoder has to survive bytes from a peer
/// that is not running this SDK — a shell script with `socat`, a
/// deliberately hostile binary, or a dockapp written in another
/// language against the published byte-layout table — and only
/// hand-assembly produces those.
///
/// The mode reports which message the shell disconnected on, which is
/// the diagnostic a shell author actually wants: "it tolerated 11 of 21
/// and closed on `frame/lying-long`" is a bug report; "it closed the
/// connection" is not.
fn malformed() -> ExitCode {
    let (socket, state) = match connected() {
        Ok(pair) => pair,
        Err(code) => return code,
    };

    // Something real first, so the shell has a working tile to lose and
    // the disconnect below is visibly caused by what follows.
    let _ = socket.send(&good_frame(&state, 1, 0x40));

    let battery = battery(&state);
    let total = battery.len();
    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];

    for (index, (name, bytes)) in battery.into_iter().enumerate() {
        eprintln!("torture: [{}/{total}] sending {name} ({} bytes)", index + 1, bytes.len());
        match socket.send(&bytes) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                eprintln!("torture:   the shell's receive buffer is full; skipping");
            }
            Err(e) => {
                // EMSGSIZE for the deliberately oversized entries is the
                // *kernel* refusing to carry them, which is a correct
                // outcome and worth naming as such rather than as a
                // failure of this program.
                eprintln!("torture:   the kernel refused to send it: {e}");
                continue;
            }
        }

        // Give the shell a pass of its event loop to react, then see
        // whether it said anything or simply went away.
        match socket.recv_until(&mut buffer, Instant::now() + Duration::from_millis(250)) {
            Ok(None) => {}
            Ok(Some(0)) => {
                eprintln!("torture: the shell closed the connection after {name}; it tolerated {index} before it.");
                return ExitCode::SUCCESS;
            }
            Ok(Some(n)) => match ServerMessage::decode(&buffer[..n]) {
                Ok(ServerMessage::Goodbye { reason }) => {
                    eprintln!("torture: Goodbye {{ {reason:?} }} after {name}. That is the correct answer.");
                    return ExitCode::SUCCESS;
                }
                Ok(message) => eprintln!("torture:   shell said {message:?}"),
                Err(e) => eprintln!("torture:   DEFECT? undecodable message from the shell: {e}"),
            },
            Err(e) => {
                eprintln!("torture: recv failed: {e}");
                return ExitCode::SUCCESS;
            }
        }
    }

    eprintln!(
        "torture: the shell survived all {total} malformed messages without closing the connection. \
         That is acceptable if and only if it also drew none of them — check the tile and the log."
    );
    // Keep the connection alive so the tile can be inspected; a process
    // that exited here would look like a crash and confuse the very
    // supervisor being tested.
    loop {
        std::thread::park_timeout(Duration::from_secs(3600));
    }
}

/// A message header with a chosen kind and chosen reserved bytes.
///
/// The published layout is `kind:u8, reserved:[u8;3] = 0`. `reserved`
/// is a parameter here precisely because a correct client always sends
/// zeros and the decoder is documented to *reject* anything else rather
/// than ignore it — which is a claim only a client willing to send
/// non-zero can check.
fn header(kind: u8, reserved: [u8; 3]) -> Vec<u8> {
    vec![kind, reserved[0], reserved[1], reserved[2]]
}

/// The hostile datagrams, in the order they are sent.
///
/// Ordered gently-to-savagely so that the *first* thing the shell
/// rejects is the most specific information available: a shell that
/// closes on entry 1 tells you much less than one that survives to
/// entry 17.
fn battery(state: &ThemeState) -> Vec<(&'static str, Vec<u8>)> {
    let tile = state.tile_px.max(1);
    let mut out: Vec<(&'static str, Vec<u8>)> = Vec::new();

    // -- lying dimensions ------------------------------------------
    // The frame payload is the remainder of the datagram, so a header
    // that lies about its geometry can only ever be a length mismatch —
    // there is no separate length field to desynchronize from. Both
    // directions of the lie are here because a decoder that checks only
    // "did I get at least enough bytes" passes the short case and reads
    // garbage on the long one.
    let mut short = header(0x02, [0, 0, 0]);
    short.extend_from_slice(&2u32.to_le_bytes());
    short.extend_from_slice(&tile.to_le_bytes());
    short.extend_from_slice(&tile.to_le_bytes());
    short.extend_from_slice(&[0xFF; 4]);
    out.push(("frame/lying-short: claims a full tile, carries 4 bytes", short));

    let mut long = header(0x02, [0, 0, 0]);
    long.extend_from_slice(&3u32.to_le_bytes());
    long.extend_from_slice(&1u32.to_le_bytes());
    long.extend_from_slice(&1u32.to_le_bytes());
    long.resize(long.len() + 4096, 0xAA);
    out.push(("frame/lying-long: claims 1x1, carries 4 KB", long));

    // -- legal geometry, wrong tile --------------------------------
    // Risk #4 of the design: `resize_to_screen` can change the dock's
    // tile size mid-session on a monitor change, and a frame produced
    // against the old size must be REJECTED, not scaled, cropped or
    // letterboxed. Blitting it at the new size paints garbage into the
    // dock. Both a smaller and a larger tile, since a "clamp to fit"
    // bug survives only one of them.
    for (name, edge) in [
        ("frame/wrong-tile-larger: a legal frame for a tile this dock does not have", tile.saturating_add(1)),
        ("frame/wrong-tile-smaller: ditto, one pixel under", tile.saturating_sub(1).max(1)),
    ] {
        if let Ok(bytes) = (ClientMessage::Frame {
            generation: 4,
            width: edge,
            height: edge,
            pixels: vec![0x77; (edge as usize) * (edge as usize) * 4],
        })
        .encode()
        {
            out.push((name, bytes));
        }
    }

    // -- the transport ceiling -------------------------------------
    // 254 x 258 x 4 = 262128 bytes, which with the 16-byte header is
    // exactly MAX_MESSAGE_BYTES. Both edges are inside the per-edge
    // caps, so every geometry rule passes and only the payload budget
    // rejects it. This exact geometry was a real decoder bug, found by
    // the Phase 5 fuzz harness and fixed in `wire.rs`; it is in the
    // battery so the *shell's* copy of the check is exercised too.
    let mut ceiling = header(0x02, [0, 0, 0]);
    ceiling.extend_from_slice(&5u32.to_le_bytes());
    ceiling.extend_from_slice(&254u32.to_le_bytes());
    ceiling.extend_from_slice(&258u32.to_le_bytes());
    ceiling.resize(16 + 254 * 258 * 4, 0x11);
    out.push(("frame/ceiling: 254x258, exactly MAX_MESSAGE_BYTES on the wire", ceiling));

    // Over the ceiling: the kernel should refuse this at `send()` with
    // EMSGSIZE, and the codec should refuse it if any kernel does not.
    let mut oversized = header(0x02, [0, 0, 0]);
    oversized.extend_from_slice(&6u32.to_le_bytes());
    oversized.extend_from_slice(&256u32.to_le_bytes());
    oversized.extend_from_slice(&1024u32.to_le_bytes());
    oversized.resize(16 + 256 * 1024 * 4, 0x22);
    out.push(("frame/oversized: 1 MB of pixels in one datagram", oversized));

    // -- invalid ids -----------------------------------------------
    // A second Hello, which is a protocol violation on its own, and one
    // carrying an id the encoder refuses to build. Ids are printed in
    // shell logs and in the per-tile menu, so every byte of one is
    // attacker-chosen text heading for a renderer.
    for (name, raw) in [
        ("hello/id-newline: forges a second line in any log", b"clock\nnet".to_vec()),
        ("hello/id-bidi: rewrites the text beside it when rendered", "clock\u{202E}kcolc".as_bytes().to_vec()),
        ("hello/id-traversal: an id used as a path component", b"../../etc/passwd".to_vec()),
        ("hello/id-nul", b"clock\0net".to_vec()),
        ("hello/id-empty", Vec::new()),
        ("hello/id-over-cap: 200 bytes where 64 is the limit", vec![b'a'; 200]),
    ] {
        let mut bytes = header(0x01, [0, 0, 0]);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(1);
        bytes.push(0);
        // `id_len` is a u8, so a 200-byte id is expressible; that is
        // exactly why the decoder has to check it against MAX_ID_BYTES
        // rather than trusting the field width to do it.
        bytes.push(raw.len().min(255) as u8);
        bytes.push(0);
        bytes.extend_from_slice(&[0x00; TOKEN_BYTES]);
        bytes.extend_from_slice(&raw);
        out.push((name, bytes));
    }

    // -- hostile log text ------------------------------------------
    // A 10 MB "tooltip" handed to cosmic-text is a rendering denial of
    // service that costs the attacker one write(), and control
    // characters in a log line are a forged journal entry. Sent raw
    // because the SDK's encoder sanitizes both away — which is correct
    // for the SDK and exactly what makes it useless for this test.
    for (name, text) in [
        ("log/control-chars: ESC[2J and a newline", "wipe\u{1b}[2Jscreen\nsecond line".to_string()),
        ("log/bidi-override", "battery \u{202E}%08 gnigrahc\u{202C}".to_string()),
        ("log/zero-width: two ids that render identically", "cl\u{200B}ock".to_string()),
        // U+2028/U+2029 are category Zl/Zp, not C0 controls, so a
        // sanitizer that only drops `char::is_control` lets them
        // through — and every text engine still breaks a line on them,
        // which forges exactly the second journal entry that dropping
        // `\n` exists to prevent. This was a real gap in
        // `wire::sanitize_text`, found and closed in Phase 5.
        ("log/unicode-line-separator: a line break that is not a control character",
         "battery ok\u{2028}ERROR: disk failing".to_string()),
        ("log/unicode-paragraph-separator", "one\u{2029}two".to_string()),
        ("log/over-cap: 4 KB where 256 is the limit", "x".repeat(4096)),
    ] {
        let mut bytes = header(0x04, [0, 0, 0]);
        bytes.push(3); // Info
        bytes.push(0);
        bytes.extend_from_slice(&(text.len().min(u16::MAX as usize) as u16).to_le_bytes());
        bytes.extend_from_slice(text.as_bytes());
        out.push((name, bytes));
    }

    let mut bad_utf8 = header(0x04, [0, 0, 0]);
    bad_utf8.push(3);
    bad_utf8.push(0);
    bad_utf8.extend_from_slice(&4u16.to_le_bytes());
    bad_utf8.extend_from_slice(&[0xFF, 0xFE, 0xC0, 0x80]);
    out.push(("log/invalid-utf8: including an overlong NUL encoding", bad_utf8));

    // -- undefined enum discriminants ------------------------------
    // The protocol version is checked for *equality*, so there is no
    // such thing as a peer legitimately speaking a dialect with more
    // enum values. An unknown discriminant is a peer to hang up on, not
    // a field to ignore — ignoring it is how a v1 shell ends up
    // silently accepting "these pixels are BGRA now".
    for (name, level) in [("log/level-zero", 0u8), ("log/level-undefined", 7), ("log/level-max", 255)] {
        let mut bytes = header(0x04, [0, 0, 0]);
        bytes.push(level);
        bytes.push(0);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(b"hi");
        out.push((name, bytes));
    }

    let mut bad_mask = header(0x01, [0, 0, 0]);
    bad_mask.extend_from_slice(&1u32.to_le_bytes());
    bad_mask.push(1);
    bad_mask.push(0xF0); // bits outside the four defined ones
    bad_mask.push(5);
    bad_mask.push(0);
    bad_mask.extend_from_slice(&[0x00; TOKEN_BYTES]);
    bad_mask.extend_from_slice(b"clock");
    out.push(("hello/undefined-input-mask-bits", bad_mask));

    // -- non-zero reserved bytes -----------------------------------
    // The decoder is documented to reject these rather than ignore
    // them, which costs forward compatibility on purpose: a reserved
    // byte that suddenly means something is a version bump, and a peer
    // setting one today is a peer that disagrees with us about the
    // protocol right now.
    let mut pong_reserved = header(0x03, [0x01, 0x00, 0x00]);
    pong_reserved.extend_from_slice(&7u32.to_le_bytes());
    out.push(("pong/header-reserved-nonzero", pong_reserved));

    let mut pong_reserved_high = header(0x03, [0x00, 0x00, 0x80]);
    pong_reserved_high.extend_from_slice(&7u32.to_le_bytes());
    out.push(("pong/header-reserved-high-bit", pong_reserved_high));

    let mut hello_reserved = header(0x01, [0, 0, 0]);
    hello_reserved.extend_from_slice(&1u32.to_le_bytes());
    hello_reserved.push(1);
    hello_reserved.push(0);
    hello_reserved.push(5);
    hello_reserved.push(0xFF); // the body's own reserved byte
    hello_reserved.extend_from_slice(&[0x00; TOKEN_BYTES]);
    hello_reserved.extend_from_slice(b"clock");
    out.push(("hello/body-reserved-nonzero", hello_reserved));

    // -- structural nonsense ---------------------------------------
    out.push(("unknown-kind/0x00", header(0x00, [0, 0, 0])));
    out.push(("unknown-kind/0x7f", header(0x7F, [0, 0, 0])));
    out.push(("unknown-kind/0xff", header(0xFF, [0, 0, 0])));

    // A *server*-direction message sent up the client-to-shell socket.
    // The high bit separates the two number spaces precisely so this is
    // a clean `UnknownKind` rather than an accidental reinterpretation
    // of one message as another.
    out.push((
        "wrong-direction: a shell-to-dockapp Ping sent the other way",
        ServerMessage::Ping { seq: 1 }.encode().expect("encodes"),
    ));

    let mut truncated = header(0x03, [0, 0, 0]);
    truncated.push(0x01); // one byte of a four-byte seq
    out.push(("pong/truncated: the message ends inside a field", truncated));

    let mut trailing = ClientMessage::Pong { seq: 1 }.encode().expect("encodes");
    trailing.extend_from_slice(b"smuggled");
    out.push(("pong/trailing-bytes: two byte strings meaning one message", trailing));

    out.push(("header-only: a bare header with no body at all", header(0x02, [0, 0, 0])));
    out.push(("empty: a zero-length datagram", Vec::new()));

    out
}
