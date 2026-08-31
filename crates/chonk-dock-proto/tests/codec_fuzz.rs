//! A deterministic, seeded fuzz harness for the wire codec.
//!
//! # Which fuzzing route this is, and why
//!
//! Phase 5 of the accepted design says "fuzz the frame parser". The
//! obvious reading is `cargo-fuzz`, and that was checked first and
//! rejected on evidence, not taste:
//!
//! - `cargo fuzz` is not installed on this machine (`cargo fuzz
//!   --version` -> "no such command"), and `rustup toolchain list`
//!   shows exactly one toolchain, `stable`. libFuzzer needs
//!   `-Z sanitizer=...`, which is nightly-only.
//! - Installing a nightly toolchain to satisfy it would put a nightly
//!   requirement into a workspace whose CI (`.github/workflows/ci.yml`)
//!   pins `dtolnay/rust-toolchain@stable` in all three jobs. The fuzz
//!   target would then be a thing nobody runs: not on a developer's
//!   machine, not in CI, and not on the desktop this compositor is
//!   running on right now.
//!
//! So: a seeded property harness that runs under `cargo test` on every
//! commit, on stable, with no new dependency. It gives up libFuzzer's
//! coverage-guided corpus evolution — genuinely a loss, and the honest
//! statement is that this finds shallow bugs thoroughly rather than
//! deep bugs eventually. It buys back the thing that actually matters
//! for a codec this small: **every single-byte mutation of every valid
//! message is checked exhaustively**, which is a stronger statement
//! than a random fuzzer makes in any bounded time, and it is checked by
//! everyone, every time.
//!
//! If someone later wants coverage-guided fuzzing, the shape to add is
//! `fuzz/fuzz_targets/wire.rs` calling exactly the four properties
//! below on `&[u8]`. They are written as free functions taking a byte
//! slice for that reason. Do not delete this file when that happens:
//! the value here is that it is unconditional.
//!
//! # The four properties
//!
//! 1. **Never panics.** A decode is the first thing a hostile process's
//!    bytes touch. An index-out-of-bounds here is a compositor abort
//!    driven by a dockapp, which is the whole class of bug this
//!    protocol's SEQPACKET framing was chosen to avoid having to review
//!    by hand.
//! 2. **Never `Ok` for input the encoder could not have produced.**
//!    This is the strong form of "strict decoding": if `decode(b)` is
//!    `Ok(m)`, then `encode(m)` must give back exactly `b`. Two byte
//!    strings that mean the same message is how smuggling bugs start,
//!    and a decoder that accepts a shape its own encoder refuses to
//!    emit is a decoder with an untested corner. There is exactly one
//!    documented exception, `Log`, and it is asserted rather than
//!    hand-waved — see [`canonical_or_sanitized`].
//! 3. **Round trips.** Every message the encoder emits decodes back to
//!    an equal message.
//! 4. **Bounded.** Nothing that decodes successfully is larger than the
//!    caps the crate documents, because the shell allocates against
//!    those caps.
//!
//! # Determinism
//!
//! The RNG is a fixed-seed xorshift written out below rather than a
//! dev-dependency, because `chonk-dock-proto`'s `Cargo.toml` argues at
//! length that anything it depends on is something a third-party
//! dockapp author inherits — and that argument should not quietly stop
//! applying to `[dev-dependencies]`, which still resolve in their
//! lockfile. Twenty lines of xorshift is cheaper than the discussion.
//!
//! A fixed seed means a failure here is reproducible from the test name
//! alone. `CHONK_FUZZ_SEED` overrides it for a longer local soak; the
//! failure message prints the seed and the offending bytes either way.

use chonk_dock_proto::wire::{
    is_valid_id, sanitize_text, Button, ClientMessage, DecodeError, GoodbyeReason, InputEvent, InputKind, InputMask,
    LogLevel, PanelCloseReason, ServerMessage, ThemeState,
};
use chonk_dock_proto::{MAX_FRAME_BYTES, MAX_ID_BYTES, MAX_LOG_BYTES, MAX_MESSAGE_BYTES, MAX_PANEL_PX, TOKEN_BYTES};

// ---------------------------------------------------------------------
// A deterministic RNG
// ---------------------------------------------------------------------

/// xorshift64*, seeded non-zero. Not cryptographic and does not need to
/// be: the only property required is that the same seed produces the
/// same sequence on every machine, so a failure is reproducible.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn next_u32(&mut self) -> u32 {
        (self.next_u64() >> 32) as u32
    }

    fn next_u8(&mut self) -> u8 {
        (self.next_u64() >> 56) as u8
    }

    /// Uniform-ish in `0..n`. The modulo bias is irrelevant for a
    /// generator picking between a dozen message shapes.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }

    fn bool(&mut self) -> bool {
        self.next_u64() & 1 == 1
    }
}

/// The seed every run uses unless `CHONK_FUZZ_SEED` says otherwise.
/// Printed in every failure message so a CI failure is reproducible
/// locally without guessing.
const DEFAULT_SEED: u64 = 0x0DEC_0DE0_F00D_1234;

fn seed() -> u64 {
    std::env::var("CHONK_FUZZ_SEED").ok().and_then(|s| s.parse().ok()).unwrap_or(DEFAULT_SEED)
}

/// Iteration counts.
///
/// Tuned against a measurement, not a guess: at these values the file
/// takes ~6.7s serially and ~3s under `cargo test`'s default
/// parallelism in a *debug* build on this machine, which is the budget
/// a hardening test gets before somebody marks it `#[ignore]`. The
/// generators are weighted towards small inputs for the same reason —
/// the header rejects most garbage in four bytes, so building a 256 KB
/// buffer to be rejected on byte one is runtime spent on nothing (that
/// weighting is worth ~10s on its own; see `lying_frame`).
///
/// `CHONK_FUZZ_ITERS=100` scales them for a deliberate soak, which is
/// the right way to spend an hour on this rather than making every
/// developer spend a minute.
fn iterations(base: usize) -> usize {
    let scale: usize = std::env::var("CHONK_FUZZ_ITERS").ok().and_then(|s| s.parse().ok()).unwrap_or(1);
    base.saturating_mul(scale.max(1))
}

// ---------------------------------------------------------------------
// The properties, as free functions over bytes
// ---------------------------------------------------------------------

/// Property 1 + 2 + 4 for one input, in the direction a *shell* reads.
///
/// Written to take a plain `&[u8]` so that a future libFuzzer target is
/// a two-line file that calls this.
fn check_client(bytes: &[u8], origin: &str) {
    match ClientMessage::decode(bytes) {
        Err(_) => {}
        Ok(message) => {
            bounded_client(&message, bytes, origin);
            canonical_or_sanitized(&message, bytes, origin);
        }
    }
    // Purity: a decoder with hidden state would make every other
    // assertion in this file conditional on call order.
    assert_eq!(ClientMessage::decode(bytes), ClientMessage::decode(bytes), "decode is not deterministic ({origin})");
}

/// Compares two decode results with NaN treated as equal to itself.
///
/// Not a convenience — a finding. `ThemeState::scale` is a raw `f32` on
/// the wire and `ThemeState` derives `PartialEq`, so a `Welcome`
/// carrying a NaN scale **is not equal to itself**, and the obvious
/// `assert_eq!(decode(b), decode(b))` fails on a decoder that is
/// perfectly deterministic. The codec is right and `==` is the wrong
/// tool; comparing `scale` by bits is both stricter (it distinguishes
/// NaN payloads, which is what canonicality needs) and reflexive.
///
/// The same trap is live for the *shell*: code shaped like
/// `if next != last_sent { push ThemeChanged }` would push forever with
/// a NaN scale. Recorded in `wire.rs`'s own tests as
/// `a_nan_scale_survives_the_wire_but_makes_theme_state_equality_non_reflexive`.
fn same_server(a: &Result<ServerMessage, DecodeError>, b: &Result<ServerMessage, DecodeError>) -> bool {
    match (a, b) {
        (Ok(x), Ok(y)) => server_eq(x, y),
        (Err(x), Err(y)) => x == y,
        _ => false,
    }
}

/// The `scale` in a message the decoder is entitled to reject, if it has
/// one. Mirrors `wire`'s own predicate rather than restating it loosely:
/// a test that accepted *any* `BadFloat` would pass while the decoder
/// rejected perfectly good scales.
fn unusable_scale(message: &ServerMessage) -> Option<f32> {
    let state = match message {
        ServerMessage::Welcome(state) | ServerMessage::ThemeChanged(state) => state,
        _ => return None,
    };
    let scale = state.scale;
    (!scale.is_finite() || scale <= 0.0 || scale > chonk_dock_proto::MAX_SCALE).then_some(scale)
}

fn server_eq(a: &ServerMessage, b: &ServerMessage) -> bool {
    match (a, b) {
        (ServerMessage::Welcome(x), ServerMessage::Welcome(y))
        | (ServerMessage::ThemeChanged(x), ServerMessage::ThemeChanged(y)) => {
            x.tile_px == y.tile_px
                && x.scale.to_bits() == y.scale.to_bits()
                && x.theme_id == y.theme_id
                && x.theme_toml == y.theme_toml
        }
        _ => a == b,
    }
}

/// The same, in the direction a *dockapp* reads. The SDK is third-party
/// code with no more reason to trust the shell than the shell has to
/// trust it, so this direction gets identical treatment.
fn check_server(bytes: &[u8], origin: &str) {
    match ServerMessage::decode(bytes) {
        Err(_) => {}
        Ok(message) => {
            bounded_server(&message, bytes, origin);
            canonical_server(&message, bytes, origin);
        }
    }
    assert!(
        same_server(&ServerMessage::decode(bytes), &ServerMessage::decode(bytes)),
        "decode is not deterministic ({origin})"
    );
}

fn check_both(bytes: &[u8], origin: &str) {
    check_client(bytes, origin);
    check_server(bytes, origin);
}

/// Property 4: everything the shell will go on to allocate against is
/// inside the cap the crate publishes for it.
fn bounded_client(message: &ClientMessage, bytes: &[u8], origin: &str) {
    match message {
        ClientMessage::Hello { id, .. } => {
            assert!(id.len() <= MAX_ID_BYTES, "id over cap from {origin}: {}", id.len());
            assert!(is_valid_id(id), "an id outside the allowlist decoded from {origin}: {id:?}");
        }
        ClientMessage::Frame { width, height, pixels, .. } => {
            assert_eq!(
                pixels.len(),
                (*width as usize) * (*height as usize) * 4,
                "a decoded frame's payload disagrees with its geometry ({origin})"
            );
            assert!(
                pixels.len() <= MAX_FRAME_BYTES,
                "a frame of {} pixel bytes decoded from {origin}, over the {MAX_FRAME_BYTES}-byte cap the shell \
                 allocates against; bytes[0..16]={:02x?}",
                pixels.len(),
                &bytes[..bytes.len().min(16)]
            );
        }
        ClientMessage::Log { text, .. } => {
            assert!(text.len() <= MAX_LOG_BYTES, "log text over cap from {origin}");
            assert!(
                !text.chars().any(char::is_control),
                "a control character survived the decoder ({origin}): {text:?}"
            );
        }
        ClientMessage::PanelFrame { y, band_height, width, pixels, .. } => {
            assert_eq!(
                pixels.len(),
                (*width as usize) * (*band_height as usize) * 4,
                "a decoded panel band's payload disagrees with its geometry ({origin})"
            );
            assert!(*width <= MAX_PANEL_PX, "a panel band wider than the cap decoded from {origin}");
            assert!(
                (*y as u64) + (*band_height as u64) <= MAX_PANEL_PX as u64,
                "a band past the tallest grantable panel decoded from {origin}"
            );
            assert!(pixels.len() <= MAX_FRAME_BYTES, "a band over the datagram frame budget decoded from {origin}");
        }
        ClientMessage::OpenPanel { width, height } => {
            // A request may exceed the caps (the shell clamps it), but
            // a zero edge must never decode.
            assert!(*width > 0 && *height > 0, "a zero-sized panel request decoded from {origin}");
        }
        ClientMessage::Pong { .. } | ClientMessage::ClosePanel => {}
    }
    assert!(bytes.len() <= MAX_MESSAGE_BYTES, "a message over the transport cap decoded ({origin})");
}

fn bounded_server(message: &ServerMessage, bytes: &[u8], origin: &str) {
    if let ServerMessage::Welcome(state) | ServerMessage::ThemeChanged(state) = message {
        assert!(state.theme_id.len() <= 64, "theme_id over cap from {origin}");
        assert!(state.theme_toml.len() <= 128 * 1024, "theme_toml over cap from {origin}");
    }
    assert!(bytes.len() <= MAX_MESSAGE_BYTES, "a message over the transport cap decoded ({origin})");
}

/// Property 2: `decode(b) == Ok(m)` implies `encode(m) == b`.
///
/// The one documented exception is `Log`. Its `text` is *sanitized on
/// the way in* (control characters, bidi overrides and zero-width
/// characters are dropped, see `wire::sanitize_text`), so a hostile
/// `Log` carrying `"a\nb"` decodes to `Log { text: "ab" }` and
/// re-encoding gives shorter bytes. That is deliberate and load
/// bearing — it is what makes "by the time this value exists it is safe
/// to shape or print" true — so the assertion for `Log` is the next
/// strongest thing: re-encoding must reach a *fixed point*, meaning the
/// sanitizer is idempotent and there is no input that decodes to a
/// message which re-encodes to something that decodes differently
/// again. A non-idempotent sanitizer would be a genuine bug: it would
/// mean the string the shell logs is not the string the shell would
/// have accepted.
fn canonical_or_sanitized(message: &ClientMessage, bytes: &[u8], origin: &str) {
    let reencoded = message.encode().unwrap_or_else(|e| {
        panic!(
            "a message that decoded from {origin} does not re-encode: {e}\n  message: {message:?}\n  \
             this means the decoder accepts a shape the encoder cannot produce"
        )
    });
    match message {
        ClientMessage::Log { level, text } => {
            assert_eq!(
                sanitize_text(text, MAX_LOG_BYTES),
                *text,
                "sanitize_text is not idempotent ({origin}); the decoded text is not what a re-decode would accept"
            );
            assert_eq!(
                ClientMessage::decode(&reencoded),
                Ok(ClientMessage::Log { level: *level, text: text.clone() }),
                "a sanitized Log is not a fixed point of encode/decode ({origin})"
            );
        }
        _ => assert_eq!(
            reencoded,
            bytes,
            "two distinct byte strings decode to the same message ({origin}): {message:?}"
        ),
    }
}

fn canonical_server(message: &ServerMessage, bytes: &[u8], origin: &str) {
    let reencoded = message.encode().unwrap_or_else(|e| {
        panic!("a ServerMessage that decoded from {origin} does not re-encode: {e}\n  message: {message:?}")
    });
    // Byte equality, so this also asserts that a NaN `scale` survives
    // `from_bits`/`to_bits` with its payload intact rather than being
    // quieted to a canonical NaN — which would be a silent, invisible
    // corruption of a field the SDK divides by.
    assert_eq!(reencoded, bytes, "two distinct byte strings decode to the same ServerMessage ({origin})");
}

// ---------------------------------------------------------------------
// The corpus of valid messages
// ---------------------------------------------------------------------

/// Deliberately small geometries. The exhaustive mutation pass below is
/// `O(len * 256)` decodes per entry, and a 112x112 tile is 50 KB, which
/// would turn a sub-second test into a minute-long one for no extra
/// coverage: the frame decoder's branches are all in the 16-byte header,
/// and the payload is a length comparison.
fn client_corpus() -> Vec<(&'static str, Vec<u8>)> {
    let mut corpus: Vec<(&'static str, ClientMessage)> = vec![
        ("hello", ClientMessage::Hello {
            proto: chonk_dock_proto::PROTOCOL_VERSION,
            id: "org.example.weather-2".into(),
            tile_units: 1,
            token: [0x5A; TOKEN_BYTES],
            wants: InputMask::all(),
        }),
        ("hello/empty-mask", ClientMessage::Hello {
            proto: chonk_dock_proto::PROTOCOL_VERSION,
            id: "a".into(),
            tile_units: 4,
            token: [0x00; TOKEN_BYTES],
            wants: InputMask::none(),
        }),
        ("hello/max-id", ClientMessage::Hello {
            proto: u32::MAX,
            id: "b".repeat(MAX_ID_BYTES),
            tile_units: 255,
            token: [0xFF; TOKEN_BYTES],
            wants: InputMask::new(InputMask::SCROLL).unwrap(),
        }),
        ("frame/1x1", ClientMessage::Frame { generation: 0, width: 1, height: 1, pixels: vec![0; 4] }),
        ("frame/4x4", ClientMessage::Frame { generation: u32::MAX, width: 4, height: 4, pixels: vec![0xC3; 64] }),
        ("frame/2x8", ClientMessage::Frame { generation: 7, width: 2, height: 8, pixels: vec![0x11; 64] }),
        ("pong", ClientMessage::Pong { seq: 0 }),
        ("pong/max", ClientMessage::Pong { seq: u32::MAX }),
        ("log/empty", ClientMessage::Log { level: LogLevel::Error, text: String::new() }),
        ("log", ClientMessage::Log { level: LogLevel::Debug, text: "battery sampler timed out".into() }),
        ("log/max", ClientMessage::Log { level: LogLevel::Warn, text: "x".repeat(MAX_LOG_BYTES) }),
        ("log/wide", ClientMessage::Log { level: LogLevel::Info, text: "CPU 42% · 3.4 GHz ünïcödé".into() }),
        ("open-panel", ClientMessage::OpenPanel { width: 448, height: 168 }),
        ("open-panel/over-cap", ClientMessage::OpenPanel { width: u32::MAX, height: u32::MAX }),
        ("panel-frame/1x1", ClientMessage::PanelFrame { generation: 0, y: 0, band_height: 1, width: 1, pixels: vec![0x7E; 4] }),
        ("panel-frame/band", ClientMessage::PanelFrame { generation: 5, y: 30, band_height: 2, width: 4, pixels: vec![0x2A; 32] }),
        ("close-panel", ClientMessage::ClosePanel),
    ];
    corpus
        .drain(..)
        .map(|(name, message)| {
            let bytes = message.encode().expect("corpus entries are valid by construction");
            (name, bytes)
        })
        .collect()
}

fn server_corpus() -> Vec<(&'static str, Vec<u8>)> {
    let state = |toml: &str| ThemeState {
        tile_px: 56,
        scale: 1.5,
        proto: chonk_dock_proto::SHELL_PROTOCOL_VERSION,
        theme_id: "nextstep-classic".into(),
        theme_toml: toml.to_string(),
    };
    let mut corpus: Vec<(&'static str, ServerMessage)> = vec![
        ("welcome", ServerMessage::Welcome(state("id = \"nextstep-classic\"\n"))),
        ("welcome/empty-toml", ServerMessage::Welcome(state(""))),
        ("theme-changed", ServerMessage::ThemeChanged(state("[tile]\n"))),
        ("input/press", ServerMessage::Input(InputEvent {
            kind: InputKind::Press,
            button: Some(Button::Left),
            x: 12,
            y: -3,
            delta: 0,
        })),
        ("input/scroll", ServerMessage::Input(InputEvent {
            kind: InputKind::Scroll,
            button: None,
            x: i32::MIN,
            y: i32::MAX,
            delta: -1,
        })),
        ("input/leave", ServerMessage::Input(InputEvent {
            kind: InputKind::Leave,
            button: None,
            x: 0,
            y: 0,
            delta: 0,
        })),
        ("visibility/true", ServerMessage::Visibility { visible: true }),
        ("visibility/false", ServerMessage::Visibility { visible: false }),
        ("ping", ServerMessage::Ping { seq: 12345 }),
        ("goodbye", ServerMessage::Goodbye { reason: GoodbyeReason::Overflow }),
        ("panel-opened", ServerMessage::PanelOpened { width: 448, height: 168 }),
        ("panel-closed", ServerMessage::PanelClosed { reason: PanelCloseReason::Dismissed }),
        ("panel-input", ServerMessage::PanelInput(InputEvent {
            kind: InputKind::Release,
            button: Some(Button::Left),
            x: 7,
            y: 9,
            delta: 0,
        })),
        ("panel-input/motion", ServerMessage::PanelInput(InputEvent {
            kind: InputKind::Motion,
            button: None,
            x: 320,
            y: 200,
            delta: 0,
        })),
    ];
    corpus
        .drain(..)
        .map(|(name, message)| (name, message.encode().expect("corpus entries are valid by construction")))
        .collect()
}

// ---------------------------------------------------------------------
// Structured generation
// ---------------------------------------------------------------------

/// Builds a byte string that *looks* like a message: a plausible kind
/// byte, plausible field widths, and a hostile tail.
///
/// Purely random bytes almost never get past the four-byte header (a
/// random kind byte is one of ten valid values 10/256 of the time, and
/// the three reserved bytes must all be zero), so a harness that only
/// threw random bytes at the decoder would spend 99.99% of its budget
/// re-testing `UnknownKind`. This generator spends its budget past the
/// header, where the interesting branches are.
fn structured(rng: &mut Rng) -> Vec<u8> {
    const KINDS: [u8; 16] =
        [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89];
    let mut out = Vec::with_capacity(64);
    out.push(if rng.below(8) == 0 { rng.next_u8() } else { KINDS[rng.below(KINDS.len())] });
    // Usually the well-formed zeros, occasionally not, so the
    // reserved-byte rejection stays exercised.
    for _ in 0..3 {
        out.push(if rng.below(16) == 0 { rng.next_u8() } else { 0 });
    }

    // A body of a length drawn from the sizes the real messages use,
    // plus a long tail now and then to reach the string-length and
    // trailing-bytes paths.
    let body = match rng.below(8) {
        0 => 0,
        1 => 4,
        2 => 12,
        3 => 16,
        4 => 20,
        5 => 24 + rng.below(64),
        6 => rng.below(512),
        _ => rng.below(4096),
    };
    for _ in 0..body {
        // A byte stream that is mostly small values reaches valid enum
        // discriminants, valid lengths and valid ASCII far more often
        // than a uniform one does.
        out.push(match rng.below(4) {
            0 => rng.next_u8(),
            1 => rng.below(8) as u8,
            2 => 0,
            _ => b'a' + (rng.below(26) as u8),
        });
    }
    out
}

/// A `Frame` whose declared geometry and actual payload are chosen
/// independently — the shape the torture example's `malformed` mode
/// sends, generated exhaustively enough to cover the arithmetic.
fn lying_frame(rng: &mut Rng) -> Vec<u8> {
    let mut out = vec![0x02u8, 0, 0, 0];
    out.extend_from_slice(&rng.next_u32().to_le_bytes());
    let (w, h) = match rng.below(6) {
        0 => (rng.next_u32(), rng.next_u32()),
        1 => (rng.below(300) as u32, rng.below(1100) as u32),
        2 => (0, rng.next_u32()),
        3 => (rng.next_u32(), 0),
        // The window that matters: geometries whose byte count lands
        // near the transport ceiling, where a wrong comparison turns
        // into an allocation the shell did not budget for.
        4 => (254, 258),
        _ => (256, 1024),
    };
    out.extend_from_slice(&w.to_le_bytes());
    out.extend_from_slice(&h.to_le_bytes());
    // Weighted towards small payloads, with the exact-match case (which
    // is the only way to reach `Ok`) and the ceiling case kept in.
    // A uniform draw over 0..256 KB would spend this test's entire
    // runtime in `memset` rather than in the decoder.
    let payload = match rng.below(8) {
        0 => 0,
        1..=3 => rng.below(256),
        4..=5 => (w as usize).saturating_mul(h as usize).saturating_mul(4).min(MAX_MESSAGE_BYTES + 8),
        6 => MAX_MESSAGE_BYTES - 16,
        _ => rng.below(4096),
    };
    out.resize(out.len() + payload.min(MAX_MESSAGE_BYTES + 8), 0xAA);
    out
}

// ---------------------------------------------------------------------
// The tests
// ---------------------------------------------------------------------

#[test]
fn every_single_byte_mutation_of_every_valid_message_is_handled() {
    // The property libFuzzer would take a while to reach and this
    // reaches by construction: for every valid message, every position,
    // every one of the 256 possible byte values. Nothing here asserts
    // an *outcome* — a mutation can legitimately produce another valid
    // message — only that the decoder holds its four properties on all
    // of it.
    let mut cases = 0usize;
    for (name, bytes) in client_corpus().into_iter().chain(server_corpus()) {
        assert!(bytes.len() <= 512, "{name} is too large for the exhaustive pass; shrink it");
        for index in 0..bytes.len() {
            let original = bytes[index];
            let mut mutated = bytes.clone();
            for value in 0..=u8::MAX {
                if value == original {
                    continue;
                }
                mutated[index] = value;
                check_both(&mutated, name);
                cases += 1;
            }
            mutated[index] = original;
        }
    }
    assert!(cases > 100_000, "the exhaustive pass covered only {cases} mutations; the corpus shrank");
}

#[test]
fn every_truncation_and_extension_of_every_valid_message_is_handled() {
    // A hostile peer picks the datagram length, so both directions off
    // the end of a valid message are its choice. Truncation is the
    // classic index-past-the-end; extension is the trailing-bytes
    // smuggling path.
    for (name, bytes) in client_corpus().into_iter().chain(server_corpus()) {
        for len in 0..=bytes.len() {
            check_both(&bytes[..len], name);
        }
        for extra in 1..=8usize {
            let mut extended = bytes.clone();
            extended.resize(bytes.len() + extra, 0);
            check_both(&extended, name);
            let mut extended = bytes.clone();
            extended.resize(bytes.len() + extra, 0xFF);
            check_both(&extended, name);
        }
    }
}

#[test]
fn seeded_structured_generation_never_breaks_the_decoder() {
    let seed = seed();
    let mut rng = Rng::new(seed);
    let origin = format!("structured/seed={seed:#x}");
    for _ in 0..iterations(120_000) {
        let bytes = structured(&mut rng);
        check_both(&bytes, &origin);
    }
}

#[test]
fn seeded_lying_frame_headers_never_break_the_decoder() {
    // Split out from the generator above because a `Frame` is the one
    // message whose payload length is implied rather than stated, and
    // it is the one the design names: "fuzz the frame parser".
    let seed = seed().wrapping_add(1);
    let mut rng = Rng::new(seed);
    let origin = format!("lying-frame/seed={seed:#x}");
    for _ in 0..iterations(8_000) {
        let bytes = lying_frame(&mut rng);
        check_client(&bytes, &origin);
    }
}

#[test]
fn uniformly_random_bytes_never_break_the_decoder() {
    // Low yield per iteration — see `structured`'s note — but it is the
    // one generator with no assumptions baked into it at all, which is
    // exactly what makes it worth keeping alongside the guided one.
    let seed = seed().wrapping_add(2);
    let mut rng = Rng::new(seed);
    let origin = format!("uniform/seed={seed:#x}");
    for _ in 0..iterations(30_000) {
        // Same weighting argument as `lying_frame`: the header rejects
        // almost all of these in four bytes, so spending time building
        // a 256 KB buffer to be rejected on byte one is waste. The big
        // sizes stay in at 1-in-16 because the `TooLarge` path has to
        // be reached by something.
        let len = match rng.below(16) {
            0 => rng.below(MAX_MESSAGE_BYTES + 16),
            1..=4 => rng.below(1024),
            5..=9 => rng.below(64),
            _ => rng.below(8),
        };
        let mut bytes = Vec::with_capacity(len);
        // Filled 8 bytes at a time: at 256 KB a per-byte RNG call is
        // most of this test's runtime.
        while bytes.len() < len {
            bytes.extend_from_slice(&rng.next_u64().to_le_bytes());
        }
        bytes.truncate(len);
        check_both(&bytes, &origin);
    }
}

#[test]
fn seeded_random_valid_messages_round_trip() {
    // The other direction: not "does garbage break the decoder" but
    // "does the encoder ever emit something its own decoder rejects".
    // Both halves are needed — a codec can be robust and still be
    // wrong.
    let seed = seed().wrapping_add(3);
    let mut rng = Rng::new(seed);
    for _ in 0..iterations(20_000) {
        let message = random_client_message(&mut rng);
        let Ok(bytes) = message.encode() else { continue };
        assert_eq!(
            ClientMessage::decode(&bytes),
            Ok(sanitized(message.clone())),
            "round trip failed for {message:?} (seed {seed:#x})"
        );
        check_client(&bytes, "round-trip");

        let message = random_server_message(&mut rng);
        let Ok(bytes) = message.encode() else { continue };
        // The one place the two directions are deliberately not
        // symmetric, so it is asserted rather than assumed: `encode`
        // carries any `scale` bit pattern, `decode` refuses the ones a
        // tile cannot be drawn at. Keeping the encoder a pure
        // serializer is what lets this harness put every float a
        // hostile peer could actually send in front of the decoder — an
        // encoder that refused them would quietly delete its own most
        // interesting corpus. (The alternative, an `EncodeError`
        // variant, would also be a second breaking change to an enum
        // that is deliberately exhaustive; see `DecodeError`'s note.)
        match ServerMessage::decode(&bytes) {
            Err(DecodeError::BadFloat { field, bits }) => {
                let scale = unusable_scale(&message).unwrap_or_else(|| panic!("BadFloat for {message:?} which has no float"));
                assert_eq!((field, bits), ("scale", scale.to_bits()), "rejected the wrong float (seed {seed:#x})");
            }
            other => assert!(
                same_server(&other, &Ok(message.clone())),
                "round trip failed for {message:?} (seed {seed:#x})"
            ),
        }
        check_server(&bytes, "round-trip");
    }
}

/// What the encoder/decoder pair is expected to produce for a message
/// whose text the encoder sanitizes on the way out.
fn sanitized(message: ClientMessage) -> ClientMessage {
    match message {
        ClientMessage::Log { level, text } => ClientMessage::Log { level, text: sanitize_text(&text, MAX_LOG_BYTES) },
        other => other,
    }
}

fn random_client_message(rng: &mut Rng) -> ClientMessage {
    match rng.below(4) {
        0 => {
            let len = 1 + rng.below(MAX_ID_BYTES);
            const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_.:";
            let id: String = (0..len).map(|_| ALPHABET[rng.below(ALPHABET.len())] as char).collect();
            let mut token = [0u8; TOKEN_BYTES];
            for byte in &mut token {
                *byte = rng.next_u8();
            }
            ClientMessage::Hello {
                proto: rng.next_u32(),
                id,
                tile_units: rng.next_u8(),
                token,
                wants: InputMask::new(rng.next_u8() & 0b1111).expect("masked to the defined bits"),
            }
        }
        1 => {
            // Constrained to geometries the encoder accepts, since this
            // test is about the *valid* path; the hostile geometries are
            // `lying_frame`'s job.
            let width = 1 + rng.below(64) as u32;
            let height = 1 + rng.below(64) as u32;
            ClientMessage::Frame {
                generation: rng.next_u32(),
                width,
                height,
                pixels: vec![rng.next_u8(); (width as usize) * (height as usize) * 4],
            }
        }
        2 => ClientMessage::Pong { seq: rng.next_u32() },
        _ => {
            // Deliberately includes the characters the sanitizer drops:
            // the encoder is supposed to remove them, and this is where
            // "the encoder and decoder agree about that" is checked.
            const CHARS: [char; 10] = ['a', 'Z', '9', ' ', '·', 'ü', '\n', '\u{1b}', '\u{202E}', '\u{200B}'];
            let len = rng.below(400);
            let text: String = (0..len).map(|_| CHARS[rng.below(CHARS.len())]).collect();
            let level = match rng.below(4) {
                0 => LogLevel::Error,
                1 => LogLevel::Warn,
                2 => LogLevel::Info,
                _ => LogLevel::Debug,
            };
            ClientMessage::Log { level, text }
        }
    }
}

fn random_server_message(rng: &mut Rng) -> ServerMessage {
    match rng.below(6) {
        0 | 1 => {
            let state = ThemeState {
                tile_px: rng.next_u32(),
                // Includes the pathological floats on purpose: `scale`
                // is encoded as raw bits, and a NaN that does not
                // survive `to_bits`/`from_bits` unchanged would break
                // canonicality. See the report note on `scale`.
                scale: match rng.below(6) {
                    0 => f32::NAN,
                    1 => f32::INFINITY,
                    2 => -0.0,
                    3 => f32::from_bits(rng.next_u32()),
                    4 => 0.0,
                    _ => 1.0 + (rng.below(400) as f32) / 100.0,
                },
                proto: (rng.next_u32() & 0xFFFF) as u16,
                theme_id: "abcdef".chars().take(1 + rng.below(6)).collect(),
                theme_toml: "k = 1\n".repeat(rng.below(20)),
            };
            if rng.bool() {
                ServerMessage::Welcome(state)
            } else {
                ServerMessage::ThemeChanged(state)
            }
        }
        2 => {
            let kind = match rng.below(5) {
                0 => InputKind::Press,
                1 => InputKind::Release,
                2 => InputKind::Scroll,
                3 => InputKind::Enter,
                _ => InputKind::Leave,
            };
            let button = match rng.below(4) {
                0 => None,
                1 => Some(Button::Left),
                2 => Some(Button::Middle),
                _ => Some(Button::Right),
            };
            ServerMessage::Input(InputEvent {
                kind,
                button,
                x: rng.next_u32() as i32,
                y: rng.next_u32() as i32,
                delta: rng.next_u32() as i32,
            })
        }
        3 => ServerMessage::Visibility { visible: rng.bool() },
        4 => ServerMessage::Ping { seq: rng.next_u32() },
        _ => {
            let reason = match rng.below(7) {
                0 => GoodbyeReason::Shutdown,
                1 => GoodbyeReason::ProtocolError,
                2 => GoodbyeReason::Unauthorized,
                3 => GoodbyeReason::Replaced,
                4 => GoodbyeReason::TileTooLarge,
                5 => GoodbyeReason::Overflow,
                _ => GoodbyeReason::Removed,
            };
            ServerMessage::Goodbye { reason }
        }
    }
}

// ---------------------------------------------------------------------
// Regressions the harness found
// ---------------------------------------------------------------------

#[test]
fn a_frame_the_encoder_could_not_produce_is_not_accepted_by_the_decoder() {
    // Found by `seeded_lying_frame_headers_never_break_the_decoder` on
    // its first run, and pinned here because the reproducer is exact
    // and a fuzz finding without a regression test is a finding that
    // comes back.
    //
    // 254 x 258 x 4 = 262128 bytes of pixels, which with the 16-byte
    // header is exactly MAX_MESSAGE_BYTES. The header cap therefore
    // passed, the geometry cap passed (254 <= MAX_TILE_PX, 258 <=
    // MAX_TILE_PX * MAX_TILE_UNITS), and the payload length matched the
    // declared geometry — so the decoder returned `Ok` for a frame its
    // own encoder refuses with `EncodeError::TooLarge`, because
    // MAX_FRAME_BYTES is MAX_MESSAGE_BYTES - 64 and the frame header is
    // only 16 bytes. A 48-byte window of geometries fell through it.
    //
    // The practical exposure was small — the shell's `frame_matches_tile`
    // would reject 254x258 anyway — but `MAX_FRAME_BYTES` is documented
    // as "ceiling on the pixel payload of one Frame" and the shell is
    // entitled to size buffers against it. The fix is in `wire.rs`:
    // the geometry check now also refuses a byte count over
    // MAX_FRAME_BYTES.
    let (w, h) = (254u32, 258u32);
    let mut bytes = vec![0x02u8, 0, 0, 0];
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(&w.to_le_bytes());
    bytes.extend_from_slice(&h.to_le_bytes());
    bytes.resize(16 + (w as usize) * (h as usize) * 4, 0xAA);
    assert_eq!(bytes.len(), MAX_MESSAGE_BYTES, "the reproducer sits exactly on the transport ceiling");
    assert!(
        ClientMessage::decode(&bytes).is_err(),
        "a frame larger than MAX_FRAME_BYTES must not decode: the shell allocates against that constant"
    );
}
