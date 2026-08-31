//! The codec: message types and their exact byte layout.
//!
//! Pure — no I/O, no clock, no allocation beyond the message itself.
//! That is on purpose: this is the module a fuzzer points at (Phase 5),
//! and the one place where "a hostile process wrote these bytes" has to
//! be true in the author's head on every line.
//!
//! # Framing
//!
//! There is none, and that is the point. The transport is
//! `SOCK_SEQPACKET`, so one `send()` is one `recv()` and the kernel
//! preserves the boundary. Length-prefix parsers are the single most
//! productive source of protocol CVEs — every "read a length, trust it,
//! allocate it, read that many bytes" is a bug waiting for an author to
//! have a bad afternoon. Choosing SEQPACKET deletes that code path
//! instead of reviewing it. What is left is a fixed 4-byte header plus
//! fixed-width fields, in the spirit of `wm-wayland/src/protocols.rs`
//! writing wlr protocol messages out by hand.
//!
//! # Byte order
//!
//! Little-endian, always. This socket never leaves the machine — both
//! peers are processes of the same user on the same kernel — so
//! network byte order would be two `bswap`s per field bought with
//! nothing. Fixed rather than native-endian so the layout is
//! *documented* rather than "whatever this build did".
//!
//! # Strictness
//!
//! Every decoder rejects: an unknown message kind, a non-zero reserved
//! byte, trailing bytes after the last field, an out-of-range enum
//! discriminant, any string over its cap, and any float outside the
//! range a tile can actually be drawn at. Nothing is clamped,
//! truncated or ignored on the decode path.
//!
//! One consequence is worth stating because other code depends on it:
//! **a decoded message is always equal to itself.** `ServerMessage`
//! derives `PartialEq` over a struct containing an `f32`, and IEEE-754
//! says NaN equals nothing — so before the `scale` check existed,
//! `decode(b) == decode(b)` was false for a message a peer could send
//! at will. See [`DecodeError::BadFloat`] and [`ThemeState::same_as`].
//!
//! Rejecting unknown bits rather than ignoring them costs forward
//! compatibility, which is deliberate: [`crate::PROTOCOL_VERSION`] is
//! checked for *equality* at handshake, so there is no such thing as a
//! peer that speaks a different version and gets to keep talking. A
//! reserved byte that suddenly means something is a version bump. The
//! failure mode this buys out of is the bad one — a v1 shell silently
//! ignoring the field that said "these pixels are BGRA now".
//!
//! # Message table
//!
//! Header, every message: `kind:u8`, `reserved:[u8;3] = 0`.
//!
//! ```text
//! dockapp -> shell
//!   0x01 Hello   proto:u32 tile_units:u8 wants:u8 id_len:u8 rsv:u8
//!                token:[u8;16] id:[u8;id_len]
//!   0x02 Frame   generation:u32 width:u32 height:u32
//!                pixels:[u8; width*height*4]      premultiplied RGBA8,
//!                                                 top row first, no padding
//!   0x03 Pong    seq:u32
//!   0x04 Log     level:u8 rsv:u8 text_len:u16 text:[u8;text_len]
//!   0x05 OpenPanel   width:u32 height:u32     a request, answered by
//!                                             PanelOpened or PanelClosed{3}
//!   0x06 PanelFrame  generation:u32 y:u32 band_height:u32 width:u32
//!                    pixels:[u8; width*band_height*4]  one horizontal band
//!                                                      of the granted panel;
//!                                                      premultiplied RGBA8,
//!                                                      top row first
//!   0x07 ClosePanel  (no payload)
//! shell -> dockapp
//!   0x81 Welcome       tile_px:u32 scale:f32(bits) theme_id_len:u16 rsv:u16
//!                      theme_toml_len:u32 theme_id:[u8] theme_toml:[u8]
//!   0x82 ThemeChanged  (identical body to Welcome)
//!   0x83 Input         kind:u8 button:u8 rsv:u16 x:i32 y:i32 delta:i32
//!   0x84 Visibility    visible:u8 rsv:[u8;3]
//!   0x85 Ping          seq:u32
//!   0x86 Goodbye       reason:u8 rsv:[u8;3]
//!   0x87 PanelOpened   width:u32 height:u32   the granted size
//!   0x88 PanelClosed   reason:u8 rsv:[u8;3]
//!   0x89 PanelInput    (identical body to Input, panel-local coordinates;
//!                       additionally admits kind 6 = Motion, which 0x83
//!                       never carries)
//! ```

use std::fmt;

use crate::{
    MAX_FRAME_BYTES, MAX_ID_BYTES, MAX_LOG_BYTES, MAX_MESSAGE_BYTES, MAX_PANEL_PX,
    MAX_SCALE, MAX_THEME_ID_BYTES, MAX_THEME_TOML_BYTES, MAX_TILE_PX, MAX_TILE_UNITS,
    TOKEN_BYTES,
};

const KIND_HELLO: u8 = 0x01;
const KIND_FRAME: u8 = 0x02;
const KIND_PONG: u8 = 0x03;
const KIND_LOG: u8 = 0x04;
const KIND_OPEN_PANEL: u8 = 0x05;
const KIND_PANEL_FRAME: u8 = 0x06;
const KIND_CLOSE_PANEL: u8 = 0x07;
const KIND_WELCOME: u8 = 0x81;
const KIND_THEME_CHANGED: u8 = 0x82;
const KIND_INPUT: u8 = 0x83;
const KIND_VISIBILITY: u8 = 0x84;
const KIND_PING: u8 = 0x85;
const KIND_GOODBYE: u8 = 0x86;
const KIND_PANEL_OPENED: u8 = 0x87;
const KIND_PANEL_CLOSED: u8 = 0x88;
const KIND_PANEL_INPUT: u8 = 0x89;

const HEADER_BYTES: usize = 4;

// ---------------------------------------------------------------------
// Enums on the wire
// ---------------------------------------------------------------------

/// Which input events a dockapp wants delivered.
///
/// A hint, not a permission: the shell already refuses to send middle
/// and right button events to *any* dockapp (middle is the dock's
/// reorder gesture and right is reserved for the per-tile menu), so
/// this exists to let a tile that only paints say "do not wake me for
/// pointer motion" and save both sides the traffic.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct InputMask(u8);

impl InputMask {
    pub const PRESS: u8 = 1 << 0;
    pub const RELEASE: u8 = 1 << 1;
    pub const SCROLL: u8 = 1 << 2;
    /// `Enter`/`Leave` together — a tile that wants one always wants
    /// the other, or it latches into a permanent hover state the first
    /// time the pointer leaves.
    pub const CROSSING: u8 = 1 << 3;

    const ALL: u8 = Self::PRESS | Self::RELEASE | Self::SCROLL | Self::CROSSING;

    pub fn new(bits: u8) -> Option<Self> {
        (bits & !Self::ALL == 0).then_some(Self(bits))
    }

    pub fn all() -> Self {
        Self(Self::ALL)
    }

    pub fn none() -> Self {
        Self(0)
    }

    pub fn bits(self) -> u8 {
        self.0
    }

    pub fn wants(self, bit: u8) -> bool {
        self.0 & bit != 0
    }

    /// Whether an event of this kind should be delivered at all.
    pub fn accepts(self, kind: InputKind) -> bool {
        match kind {
            InputKind::Press => self.wants(Self::PRESS),
            InputKind::Release => self.wants(Self::RELEASE),
            InputKind::Scroll => self.wants(Self::SCROLL),
            InputKind::Enter | InputKind::Leave => self.wants(Self::CROSSING),
            // Panel-only on the wire, and the mask is a *tile* hint:
            // no mask bit exists to ask for it, so a tile never gets
            // one whatever it set.
            InputKind::Motion => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Press,
    Release,
    Scroll,
    Enter,
    Leave,
    /// The pointer moved inside the panel — coordinates are its
    /// position in panel device pixels, `button` 0, `delta` 0.
    ///
    /// **Valid only in `PanelInput` (0x89), never in tile `Input`
    /// (0x83)**, and both codecs enforce it. Tiles are 56 logical
    /// pixels of glanceable instrument, and per-motion wakeups there
    /// are traffic without a use; a panel is a detail view where hover
    /// UX (a highlighted row, a scrubbed graph) is the point. The
    /// shell throttles it to its dispatch cadence — never more than
    /// one per pointer-motion dispatch.
    Motion,
}

impl InputKind {
    fn code(self) -> u8 {
        match self {
            Self::Press => 1,
            Self::Release => 2,
            Self::Scroll => 3,
            Self::Enter => 4,
            Self::Leave => 5,
            Self::Motion => 6,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Press),
            2 => Some(Self::Release),
            3 => Some(Self::Scroll),
            4 => Some(Self::Enter),
            5 => Some(Self::Leave),
            6 => Some(Self::Motion),
            _ => None,
        }
    }
}

/// Mirrors `wm_core::types::MouseButton`, re-declared rather than
/// imported because this crate must not depend on `wm-core` (a dockapp
/// links this crate and nothing else of chonkstep's).
///
/// `Middle` and `Right` exist on the wire but the shell never sends
/// them: middle is the dock's reorder gesture and right is reserved for
/// the per-tile menu. A dockapp that could swallow middle-click could
/// make itself un-reorderable and un-removable, which is a tile holding
/// the dock hostage. They are encodable so that reserving them stays a
/// *policy* decision in the shell, visible at one call site, instead of
/// a hole in the wire format that would need a version bump to undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    fn code(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Middle => 2,
            Self::Right => 3,
        }
    }

    fn from_code(code: u8) -> Option<Option<Self>> {
        match code {
            0 => Some(None),
            1 => Some(Some(Self::Left)),
            2 => Some(Some(Self::Middle)),
            3 => Some(Some(Self::Right)),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
}

impl LogLevel {
    fn code(self) -> u8 {
        match self {
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Error),
            2 => Some(Self::Warn),
            3 => Some(Self::Info),
            4 => Some(Self::Debug),
            _ => None,
        }
    }
}

/// Why the shell is closing a connection. Sent best-effort before the
/// fd closes — a dockapp that gets one can say something useful in its
/// own log instead of reporting a bare EOF, and `CrashLooped` in
/// particular tells it not to bother reconnecting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoodbyeReason {
    /// The session is ending. Reconnecting is pointless.
    Shutdown,
    /// The dockapp sent something that did not decode, or violated a
    /// bound. The shell logs the specifics; the wire carries only this.
    ProtocolError,
    /// `Hello` presented the wrong token, or an id with no registry slot.
    Unauthorized,
    /// Another connection claimed this id. One connection per id.
    Replaced,
    /// The tile geometry this dockapp asked for cannot be carried
    /// inline by v1. See `crate::frame_fits`.
    TileTooLarge,
    /// The dockapp stopped reading and the shell's send queue stayed
    /// full — see `crate::queue::SendQueue`. Almost always followed by
    /// the fd closing immediately, since a peer in this state is by
    /// definition not reading this message either.
    Overflow,
    /// The user removed the tile from the dock.
    Removed,
}

impl GoodbyeReason {
    fn code(self) -> u8 {
        match self {
            Self::Shutdown => 1,
            Self::ProtocolError => 2,
            Self::Unauthorized => 3,
            Self::Replaced => 4,
            Self::TileTooLarge => 5,
            Self::Overflow => 6,
            Self::Removed => 7,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Shutdown),
            2 => Some(Self::ProtocolError),
            3 => Some(Self::Unauthorized),
            4 => Some(Self::Replaced),
            5 => Some(Self::TileTooLarge),
            6 => Some(Self::Overflow),
            7 => Some(Self::Removed),
            _ => None,
        }
    }
}

/// Why the shell is (or is not) taking a dockapp's panel down.
///
/// Distinct from [`GoodbyeReason`] because the two end different
/// things: a `Goodbye` ends the *connection*, a `PanelClosed` ends one
/// panel and the tile keeps drawing. Codes start at 0, exactly as the
/// protocol document publishes them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelCloseReason {
    /// The dockapp sent `ClosePanel`; this is the acknowledgement.
    ClientRequest,
    /// The user dismissed it: a click away from the panel, Escape, or
    /// re-clicking the owning tile.
    Dismissed,
    /// The shell is shutting the panel's owner down — session end,
    /// eviction, crash teardown, or a structural change (a monitor
    /// rearrangement) that invalidated the panel's place on screen.
    Shutdown,
    /// The `OpenPanel` was refused outright; no panel exists.
    Refused,
}

impl PanelCloseReason {
    fn code(self) -> u8 {
        match self {
            Self::ClientRequest => 0,
            Self::Dismissed => 1,
            Self::Shutdown => 2,
            Self::Refused => 3,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::ClientRequest),
            1 => Some(Self::Dismissed),
            2 => Some(Self::Shutdown),
            3 => Some(Self::Refused),
            _ => None,
        }
    }
}

/// One pointer event, in coordinates local to the dockapp's own tile —
/// the dockapp is never told where its tile is on screen, or that other
/// tiles exist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputEvent {
    pub kind: InputKind,
    /// `None` for `Scroll`, `Enter` and `Leave`.
    pub button: Option<Button>,
    pub x: i32,
    pub y: i32,
    /// Notches, signed; `0` for everything but `Scroll`.
    pub delta: i32,
}

// ---------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum ClientMessage {
    Hello {
        proto: u32,
        id: String,
        tile_units: u8,
        token: [u8; TOKEN_BYTES],
        wants: InputMask,
    },
    /// Premultiplied RGBA8, top row first, `width * height * 4` bytes
    /// with no row padding — byte-identical to `tiny_skia::Pixmap`'s
    /// buffer and to `wm_theme_api::DecorationBuffer::pixels`, which is
    /// what makes a remote tile and a built-in tile the same thing at
    /// the dock's blit seam.
    ///
    /// `generation` is the dockapp's own counter. The shell echoes
    /// nothing back; it exists so a log line can say *which* frame was
    /// dropped by the rate limiter.
    Frame {
        generation: u32,
        width: u32,
        height: u32,
        pixels: Vec<u8>,
    },
    Pong {
        seq: u32,
    },
    Log {
        level: LogLevel,
        text: String,
    },
    /// A request for an instrument panel of this size in device pixels
    /// — a request, not a command: the shell answers `PanelOpened`
    /// (possibly clamped) or `PanelClosed { Refused }`. One panel per
    /// dockapp; an `OpenPanel` while one is open re-negotiates the size
    /// in place.
    OpenPanel {
        width: u32,
        height: u32,
    },
    /// One horizontal band of the granted panel — premultiplied RGBA8,
    /// top row first, no row padding, under the same
    /// length-must-agree strictness as `Frame`.
    ///
    /// Banded because a whole panel at the caps is sixteen datagrams'
    /// worth of pixels: the shell keeps one buffer per grant and blits
    /// each band at row `y` on receipt. A full repaint is a
    /// top-to-bottom sequence of bands with **no atomicity across
    /// them** — repaint fast and top-down, so a half-applied repaint
    /// reads as a shear for one pass, not as interleaved garbage.
    ///
    /// `width` MUST equal the granted width and `y + band_height` MUST
    /// stay within the granted height; the shell treats anything else
    /// like a wrong-sized `Frame` (logged and discarded, connection
    /// kept). `generation` carries the same drop-attribution semantics
    /// as `Frame`'s: the client's own repaint counter, echoed nowhere —
    /// under flow control the shell may drop a band whose generation is
    /// older than the newest it has seen.
    PanelFrame {
        generation: u32,
        y: u32,
        band_height: u32,
        width: u32,
        pixels: Vec<u8>,
    },
    /// The dockapp is done with its panel. No payload.
    ClosePanel,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ServerMessage {
    /// Sent once, immediately after a `Hello` is accepted.
    Welcome(ThemeState),
    /// Sent whenever the user picks a different theme or the scale
    /// changes. A dockapp *never* restarts for a theme change — that is
    /// the whole reason this message exists rather than the shell
    /// killing and relaunching the process.
    ThemeChanged(ThemeState),
    Input(InputEvent),
    /// `false` while the dock is hidden or the tile is scrolled out of
    /// the visible strip: a dockapp should stop sampling and stop
    /// drawing, not just stop being looked at.
    Visibility {
        visible: bool,
    },
    Ping {
        seq: u32,
    },
    Goodbye {
        reason: GoodbyeReason,
    },
    /// The answer to an accepted `OpenPanel`: the granted size, which
    /// is the request clamped to the caps and the current workarea.
    /// Every `PanelFrame` band must match its width and stay inside its
    /// height.
    PanelOpened {
        width: u32,
        height: u32,
    },
    /// The panel is gone (or was never opened, for `Refused`).
    PanelClosed {
        reason: PanelCloseReason,
    },
    /// A pointer event inside the panel, in panel device pixels —
    /// body identical to `Input`.
    PanelInput(InputEvent),
}

/// The tile geometry and palette a dockapp draws with. Two theme
/// payloads on purpose:
///
/// - `theme_id` is the fast path. A dockapp built against this
///   workspace's `wm-theme` calls `default_theme::theme_by_id` and
///   parses nothing at all, exactly as `startup.rs` does.
/// - `theme_toml` is the correctness path. A dockapp built against a
///   *different* `wm-theme` version, or a session running a future
///   user-defined theme with no built-in id, still gets the real
///   palette by deserializing it (`Theme` derives `Serialize` /
///   `Deserialize`).
///
/// A dockapp that can use neither falls back to its own default. The
/// worst case is a tile in the wrong colors, never a tile that fails to
/// draw.
#[derive(Clone, Debug, PartialEq)]
pub struct ThemeState {
    /// Device pixels per tile edge. The dockapp's surface is
    /// `tile_px` wide and `tile_px * tile_units` tall.
    pub tile_px: u32,
    /// The session's `CHONKSTEP_SCALE`. Present so a dockapp can size
    /// its own hand-computed geometry, the same job `chonk_ui::App::scale`
    /// does for a windowed SDK app.
    pub scale: f32,
    /// The shell's protocol version — [`crate::SHELL_PROTOCOL_VERSION`]
    /// from a current shell. Rides in the u16 that was reserved (and
    /// required zero) in protocol 1, which is why **zero means 1**: a
    /// protocol-1 shell always sent zero there, and this field is how a
    /// panel-capable client discovers whether `OpenPanel` is a request
    /// (`proto >= 2`) or a connection-costing unknown kind. Kept as the
    /// raw wire value rather than normalized, so encode/decode stays
    /// canonical; read it through [`ThemeState::panels_supported`].
    ///
    /// This is the one deliberate exception to the reserved-bytes rule,
    /// and it is exactly the shape the rule's own text predicts: "a
    /// reserved byte that starts meaning something is a version bump" —
    /// this byte *is* the version.
    pub proto: u16,
    pub theme_id: String,
    pub theme_toml: String,
}

impl ThemeState {
    /// Equality that is *reflexive*, which the derived `PartialEq` is
    /// not.
    ///
    /// `scale` is an `f32`, so `PartialEq` inherits IEEE-754's rule that
    /// NaN is equal to nothing including itself. That makes the derive
    /// unsafe for the one shape of code every consumer of this type
    /// naturally writes:
    ///
    /// ```text
    /// if next_state != last_sent { send ThemeChanged; last_sent = next_state }
    /// ```
    ///
    /// With a NaN scale that condition is true on *every* pass forever,
    /// so the shell would push a `ThemeChanged` at its repaint rate
    /// until the dockapp's send queue overflowed and the tile was
    /// disconnected — a compositor busy-loop provoked by one bad float.
    ///
    /// [`theme_state_decode`] now rejects an unusable scale outright
    /// ([`DecodeError::BadFloat`]), so a *decoded* `ThemeState` can no
    /// longer carry one. This is the second lock, on the side of the
    /// socket where the value is *constructed* rather than parsed: the
    /// shell builds its own `ThemeState` from its own `scale` field and
    /// never decodes it, so the codec's guard cannot cover the sender.
    /// Comparing the bits makes the answer total whatever the float is.
    pub fn same_as(&self, other: &Self) -> bool {
        self.tile_px == other.tile_px
            && self.scale.to_bits() == other.scale.to_bits()
            && self.proto == other.proto
            && self.theme_id == other.theme_id
            && self.theme_toml == other.theme_toml
    }

    /// Whether the shell that sent this state accepts the panel family.
    /// Zero is what a protocol-1 shell always put in the field (it was
    /// reserved), so zero and one both mean "tiles only".
    pub fn panels_supported(&self) -> bool {
        self.proto >= 2
    }
}

// ---------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------

/// Why a datagram from the peer could not be read.
///
/// `#[non_exhaustive]`, and `EncodeError` below deliberately is not.
/// The line between them is who made the mistake:
///
/// * A `DecodeError` describes *the peer's* bytes. Every variant means
///   the same thing to the code that receives one — "this peer sent
///   something this version cannot read" — and both existing consumers
///   act on it identically: `RemoteTile::receive` logs it and drops the
///   connection, `chonk_ui::dockapp::serve` logs it and reconnects.
///   Nobody can usefully handle `BadFloat` differently from
///   `Truncated`, so the exhaustiveness a downstream `match` buys is
///   worth less than being able to describe a new rejection without a
///   semver break. This very commit is the evidence: adding `BadFloat`
///   to an exhaustive published enum would have broken every consumer
///   for a variant they would all have wildcarded.
/// * An [`EncodeError`] describes *local* code building a message that
///   cannot legally exist. A new way to do that is something the author
///   of the calling code may genuinely want the compiler to point at,
///   so that one stays exhaustive.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DecodeError {
    /// A zero-length datagram. Not EOF — `recv` reports that separately
    /// — so this is a peer that actually sent nothing.
    Empty,
    /// Over [`MAX_MESSAGE_BYTES`]. Checked before anything else, so an
    /// oversized message costs one comparison.
    TooLarge { len: usize },
    UnknownKind { kind: u8 },
    /// The message ended in the middle of a field.
    Truncated { field: &'static str },
    /// Fields decoded, but bytes were left over. Rejected rather than
    /// ignored: a decoder that tolerates trailing bytes is a decoder
    /// where two different byte strings mean the same message, which is
    /// how smuggling bugs start.
    TrailingBytes { extra: usize },
    /// A reserved field was not zero. See the module's strictness note.
    ReservedNotZero { field: &'static str },
    BadUtf8 { field: &'static str },
    /// A string was over its cap, or empty where emptiness is illegal.
    StringLength { field: &'static str, len: usize, max: usize },
    /// An id contained something outside the permitted character set.
    IdCharset,
    /// An enum discriminant this version does not define.
    BadEnum { field: &'static str, value: u8 },
    /// `width`/`height` outside the permitted tile geometry.
    FrameGeometry { width: u32, height: u32 },
    /// `width`/`height` outside the permitted panel geometry — a zero
    /// edge, an edge past [`MAX_PANEL_PX`], a band outside the tallest
    /// grantable panel or over [`crate::MAX_FRAME_BYTES`]'s one-datagram
    /// budget. Its own variant rather than a reuse of
    /// [`FrameGeometry`](Self::FrameGeometry) because the two bounds
    /// differ by a factor of four per edge, and a log line that names
    /// the wrong limit sends the reader to the wrong table row.
    PanelGeometry { width: u32, height: u32 },
    /// `pixels.len()` disagreed with `width * height * 4`. Note this is
    /// impossible to get wrong *silently*: the pixel payload is the
    /// remainder of the datagram, so a lying header is a mismatch here
    /// rather than a short read somewhere downstream.
    FrameLengthMismatch { expected: usize, actual: usize },
    /// A float field was not a number a tile can be drawn at: NaN,
    /// infinite, zero, negative, or past [`crate::MAX_SCALE`].
    ///
    /// Carries the *bits* rather than the `f32` on purpose. An `f32` in
    /// here would drag NaN's non-reflexive equality up into
    /// `DecodeError` itself — `err == err` would be false for exactly
    /// the error that exists to stamp out that bug — and would cost the
    /// enum its `Eq`, which the tests and the fuzz harness compare with.
    BadFloat { field: &'static str, bits: u32 },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => write!(f, "empty message"),
            Self::TooLarge { len } => write!(f, "message of {len} bytes exceeds the {MAX_MESSAGE_BYTES}-byte cap"),
            Self::UnknownKind { kind } => write!(f, "unknown message kind {kind:#04x}"),
            Self::Truncated { field } => write!(f, "message ended inside field `{field}`"),
            Self::TrailingBytes { extra } => write!(f, "{extra} trailing bytes after the last field"),
            Self::ReservedNotZero { field } => write!(f, "reserved field `{field}` was not zero"),
            Self::BadUtf8 { field } => write!(f, "field `{field}` was not valid UTF-8"),
            Self::StringLength { field, len, max } => write!(f, "field `{field}` is {len} bytes, limit {max}"),
            Self::IdCharset => write!(f, "id contains characters outside [A-Za-z0-9._:-]"),
            Self::BadEnum { field, value } => write!(f, "field `{field}` has undefined value {value}"),
            Self::FrameGeometry { width, height } => write!(f, "frame geometry {width}x{height} is out of range"),
            Self::PanelGeometry { width, height } => write!(f, "panel geometry {width}x{height} is out of range"),
            Self::FrameLengthMismatch { expected, actual } => {
                write!(f, "frame declared {expected} pixel bytes but carried {actual}")
            }
            Self::BadFloat { field, bits } => {
                write!(f, "field `{field}` is {} (bits {bits:#010x}), which is not a usable value", f32::from_bits(*bits))
            }
        }
    }
}

impl std::error::Error for DecodeError {}

/// Encoding fails only when the *local* program built a message that
/// cannot legally exist. It is a programming error on this side of the
/// socket, surfaced rather than silently truncated: a dockapp that
/// tries to send a frame too big for the transport needs to be told
/// so at the call site, not left wondering why its tile is blank.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EncodeError {
    StringLength { field: &'static str, len: usize, max: usize },
    IdCharset,
    FrameGeometry { width: u32, height: u32 },
    FrameLengthMismatch { expected: usize, actual: usize },
    /// The encoded message would exceed [`MAX_MESSAGE_BYTES`].
    TooLarge { len: usize },
    /// `InputKind::Motion` in a tile `Input` — kind 6 is defined only
    /// for `PanelInput`.
    MotionOutsidePanel,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StringLength { field, len, max } => write!(f, "field `{field}` is {len} bytes, limit {max}"),
            Self::IdCharset => write!(f, "id contains characters outside [A-Za-z0-9._:-]"),
            Self::FrameGeometry { width, height } => write!(f, "frame geometry {width}x{height} is out of range"),
            Self::FrameLengthMismatch { expected, actual } => {
                write!(f, "frame geometry needs {expected} pixel bytes but was given {actual}")
            }
            Self::TooLarge { len } => write!(f, "message of {len} bytes exceeds the {MAX_MESSAGE_BYTES}-byte cap"),
            Self::MotionOutsidePanel => write!(f, "InputKind::Motion is defined only for PanelInput"),
        }
    }
}

impl std::error::Error for EncodeError {}

// ---------------------------------------------------------------------
// String hygiene
// ---------------------------------------------------------------------

/// Drops every character that could make a string do something other
/// than be read, and returns at most `max` bytes of what is left.
///
/// Applied by the decoder to free-text fields, so that a caller cannot
/// forget: by the time a `Log`'s text is in a `ClientMessage` it is
/// already safe to hand to `cosmic-text` or to `tracing`. Three
/// families go:
///
/// - C0/C1 controls (`char::is_control`): a `\n` in a log line forges a
///   second log entry; an ESC in a line that reaches a terminal is a
///   terminal escape sequence the dockapp did not earn.
/// - The Unicode line and paragraph separators, U+2028 and U+2029.
///   These are *not* `char::is_control` — they are category Zl/Zp — so
///   dropping the C0 newline without dropping them left the hole
///   half-closed: both break a line in every text engine that shapes
///   them, `cosmic-text` included, so either one forges exactly the
///   second log entry the `\n` rule exists to prevent. Found in Phase 5
///   hardening by asking what "no line breaks" actually means in
///   Unicode rather than in ASCII.
/// - Bidi overrides and isolates (U+202A..U+202E, U+2066..U+2069):
///   these reorder *surrounding* text when rendered, so a tile's name
///   can rewrite the label of the menu entry next to it.
/// - The zero-width joiner/non-joiner and U+200B: invisible characters
///   let two different ids render identically.
///
/// Truncation is on a `char` boundary, so the result is always valid
/// UTF-8 — `cosmic-text` shapes it, and a byte-sliced string would
/// panic long before it got there.
pub fn sanitize_text(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max));
    for c in text.chars() {
        let dangerous = c.is_control()
            // Zl / Zp: line and paragraph separator. Not `is_control`,
            // but they break a line just as hard as `\n` does.
            || matches!(c, '\u{2028}' | '\u{2029}')
            || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
            || matches!(c, '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}');
        if dangerous {
            continue;
        }
        if out.len() + c.len_utf8() > max {
            break;
        }
        out.push(c);
    }
    out
}

/// A dockapp id names a registry entry, keys the shell's slot table,
/// and is printed in logs and the per-tile menu. It is checked against
/// an allowlist rather than a blocklist because an allowlist is
/// auditable in one glance and a blocklist is a promise to have thought
/// of everything.
///
/// `:` is permitted so that built-in dock items can keep their reserved
/// `builtin:clock` form in the same namespace as remote ids without
/// needing a second validator.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// Whether a frame's own geometry is the geometry the shell allocated.
///
/// Deliberately an equality test with no clamping. `resize_to_screen`
/// can change the dock's tile size mid-session (a monitor change), and
/// a frame produced against the old size is not a frame that should be
/// scaled, cropped, or letterboxed — it is a frame from before the
/// resize, and blitting it at the new size paints garbage into the
/// dock. Reject it; the dockapp gets a `ThemeChanged` carrying the new
/// `tile_px` and its next frame is correct.
pub fn frame_matches_tile(width: u32, height: u32, tile_px: u32, tile_units: u8) -> bool {
    width == tile_px && height == tile_px.saturating_mul(u32::from(tile_units))
}

// ---------------------------------------------------------------------
// Reader / Writer
// ---------------------------------------------------------------------

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn take(&mut self, n: usize, field: &'static str) -> Result<&'a [u8], DecodeError> {
        let end = self.pos.checked_add(n).ok_or(DecodeError::Truncated { field })?;
        let slice = self.buf.get(self.pos..end).ok_or(DecodeError::Truncated { field })?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self, field: &'static str) -> Result<u8, DecodeError> {
        Ok(self.take(1, field)?[0])
    }

    fn u16(&mut self, field: &'static str) -> Result<u16, DecodeError> {
        let b = self.take(2, field)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self, field: &'static str) -> Result<u32, DecodeError> {
        let b = self.take(4, field)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self, field: &'static str) -> Result<i32, DecodeError> {
        Ok(self.u32(field)? as i32)
    }

    /// Reads an `f32` and refuses one a tile cannot be drawn at.
    ///
    /// The only float on this wire is `scale`, and it is not decoration:
    /// it reaches `Theme::scaled`, which multiplies every metric in the
    /// palette by it. A NaN turns every dimension into NaN and the tile
    /// renders as nothing, with no error anywhere to explain it; zero or
    /// a negative collapses or inverts it; 1e30 asks for a pixmap the
    /// allocator will refuse. So the check is here, in the one place
    /// both sides of the socket share, rather than only in the SDK's
    /// `check_drawable` — which stays as defence in depth, and which
    /// also checks geometry this cannot see.
    ///
    /// The sharper reason is the NaN. `ThemeState` derives `PartialEq`
    /// over a raw `f32`, so a message carrying a NaN scale is not equal
    /// to *itself*: `decode(b) == decode(b)` is false, and the natural
    /// "push a `ThemeChanged` when the state changed" loop then pushes
    /// on every pass forever. Rejecting at decode makes that
    /// unreachable for anything that came off a socket. See
    /// [`ThemeState::same_as`] for the sender's half.
    fn f32_usable(&mut self, field: &'static str, max: f32) -> Result<f32, DecodeError> {
        let bits = self.u32(field)?;
        let value = f32::from_bits(bits);
        if !value.is_finite() || value <= 0.0 || value > max {
            return Err(DecodeError::BadFloat { field, bits });
        }
        Ok(value)
    }

    fn reserved(&mut self, n: usize, field: &'static str) -> Result<(), DecodeError> {
        if self.take(n, field)?.iter().any(|&b| b != 0) {
            return Err(DecodeError::ReservedNotZero { field });
        }
        Ok(())
    }

    fn string(&mut self, len: usize, max: usize, field: &'static str) -> Result<String, DecodeError> {
        if len > max {
            return Err(DecodeError::StringLength { field, len, max });
        }
        let bytes = self.take(len, field)?;
        std::str::from_utf8(bytes).map(str::to_owned).map_err(|_| DecodeError::BadUtf8 { field })
    }

    fn rest(&mut self) -> &'a [u8] {
        let slice = &self.buf[self.pos..];
        self.pos = self.buf.len();
        slice
    }

    fn finish(self) -> Result<(), DecodeError> {
        let extra = self.buf.len() - self.pos;
        if extra != 0 {
            return Err(DecodeError::TrailingBytes { extra });
        }
        Ok(())
    }
}

fn header(buf: &[u8]) -> Result<(u8, Reader<'_>), DecodeError> {
    if buf.is_empty() {
        return Err(DecodeError::Empty);
    }
    if buf.len() > MAX_MESSAGE_BYTES {
        return Err(DecodeError::TooLarge { len: buf.len() });
    }
    let mut reader = Reader::new(buf);
    let kind = reader.u8("kind")?;
    reader.reserved(3, "header.reserved")?;
    Ok((kind, reader))
}

fn start(kind: u8, capacity: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES + capacity);
    out.push(kind);
    out.extend_from_slice(&[0, 0, 0]);
    out
}

fn check_string(field: &'static str, s: &str, max: usize) -> Result<(), EncodeError> {
    if s.len() > max {
        return Err(EncodeError::StringLength { field, len: s.len(), max });
    }
    Ok(())
}

fn finish(out: Vec<u8>) -> Result<Vec<u8>, EncodeError> {
    if out.len() > MAX_MESSAGE_BYTES {
        return Err(EncodeError::TooLarge { len: out.len() });
    }
    Ok(out)
}

fn theme_state_encode(kind: u8, state: &ThemeState) -> Result<Vec<u8>, EncodeError> {
    check_string("theme_id", &state.theme_id, MAX_THEME_ID_BYTES)?;
    check_string("theme_toml", &state.theme_toml, MAX_THEME_TOML_BYTES)?;
    let mut out = start(kind, 16 + state.theme_id.len() + state.theme_toml.len());
    out.extend_from_slice(&state.tile_px.to_le_bytes());
    out.extend_from_slice(&state.scale.to_bits().to_le_bytes());
    out.extend_from_slice(&(state.theme_id.len() as u16).to_le_bytes());
    out.extend_from_slice(&state.proto.to_le_bytes());
    out.extend_from_slice(&(state.theme_toml.len() as u32).to_le_bytes());
    out.extend_from_slice(state.theme_id.as_bytes());
    out.extend_from_slice(state.theme_toml.as_bytes());
    finish(out)
}

/// The 16-byte body `Input` and `PanelInput` share. One encoder and one
/// decoder for both kinds, so the two layouts cannot drift — "identical
/// to `Input`" is the protocol document's own definition of
/// `PanelInput`, and sharing the code is what keeps that sentence true
/// by construction.
fn input_event_encode(kind: u8, event: &InputEvent) -> Result<Vec<u8>, EncodeError> {
    // Motion is the panel family's kind alone: a tile `Input` carrying
    // it is a message this protocol does not define, and refusing to
    // build one keeps that a compile-visible local bug instead of a
    // peer-side rejection.
    if event.kind == InputKind::Motion && kind != KIND_PANEL_INPUT {
        return Err(EncodeError::MotionOutsidePanel);
    }
    let mut out = start(kind, 16);
    out.push(event.kind.code());
    out.push(event.button.map_or(0, Button::code));
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&event.x.to_le_bytes());
    out.extend_from_slice(&event.y.to_le_bytes());
    out.extend_from_slice(&event.delta.to_le_bytes());
    finish(out)
}

fn input_event_decode(r: &mut Reader<'_>, allow_motion: bool) -> Result<InputEvent, DecodeError> {
    let kind_code = r.u8("input.kind")?;
    let input_kind = InputKind::from_code(kind_code).ok_or(DecodeError::BadEnum { field: "input.kind", value: kind_code })?;
    if input_kind == InputKind::Motion && !allow_motion {
        // Kind 6 exists only in PanelInput; in a tile Input it is as
        // undefined as kind 7.
        return Err(DecodeError::BadEnum { field: "input.kind", value: kind_code });
    }
    let button_code = r.u8("input.button")?;
    let button = Button::from_code(button_code).ok_or(DecodeError::BadEnum { field: "input.button", value: button_code })?;
    r.reserved(2, "input.reserved")?;
    let x = r.i32("x")?;
    let y = r.i32("y")?;
    let delta = r.i32("delta")?;
    Ok(InputEvent { kind: input_kind, button, x, y, delta })
}

/// The bounds a whole-panel geometry (`PanelOpened`'s grant) must sit
/// inside — [`crate::panel_fits`], named locally so the decode arms
/// read like the rest of this module.
fn panel_geometry_ok(width: u32, height: u32) -> bool {
    crate::panel_fits(width, height)
}

/// The bounds one `PanelFrame` band must sit inside: a real width
/// within the panel edge cap, a real height, the band's *end row*
/// within the tallest panel the protocol can grant, and the band's
/// bytes within one datagram's frame budget. All arithmetic in u64 so
/// no header can overflow its way past a check.
fn panel_band_ok(y: u32, band_height: u32, width: u32) -> bool {
    width != 0
        && width <= MAX_PANEL_PX
        && band_height != 0
        && (y as u64) + (band_height as u64) <= MAX_PANEL_PX as u64
        && crate::panel_band_fits(width, band_height)
}

fn theme_state_decode(reader: &mut Reader<'_>) -> Result<ThemeState, DecodeError> {
    let tile_px = reader.u32("tile_px")?;
    let scale = reader.f32_usable("scale", MAX_SCALE)?;
    let theme_id_len = reader.u16("theme_id_len")? as usize;
    // The u16 that was reserved-and-zero in protocol 1 now carries the
    // shell's protocol version (zero therefore reads as a protocol-1
    // shell — see `ThemeState::proto`). Any value decodes: a version is
    // an advertisement, not a claim the codec can falsify.
    let proto = reader.u16("theme.proto")?;
    let theme_toml_len = reader.u32("theme_toml_len")? as usize;
    let theme_id = reader.string(theme_id_len, MAX_THEME_ID_BYTES, "theme_id")?;
    let theme_toml = reader.string(theme_toml_len, MAX_THEME_TOML_BYTES, "theme_toml")?;
    Ok(ThemeState { tile_px, scale, proto, theme_id, theme_toml })
}

// ---------------------------------------------------------------------
// ClientMessage codec
// ---------------------------------------------------------------------

impl ClientMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        match self {
            Self::Hello { proto, id, tile_units, token, wants } => {
                if !is_valid_id(id) {
                    return Err(if id.len() > MAX_ID_BYTES {
                        EncodeError::StringLength { field: "id", len: id.len(), max: MAX_ID_BYTES }
                    } else {
                        EncodeError::IdCharset
                    });
                }
                let mut out = start(KIND_HELLO, 24 + id.len());
                out.extend_from_slice(&proto.to_le_bytes());
                out.push(*tile_units);
                out.push(wants.bits());
                out.push(id.len() as u8);
                out.push(0);
                out.extend_from_slice(token);
                out.extend_from_slice(id.as_bytes());
                finish(out)
            }
            Self::Frame { generation, width, height, pixels } => {
                if *width == 0 || *height == 0 || *width > MAX_TILE_PX || *height > MAX_TILE_PX * u32::from(MAX_TILE_UNITS) {
                    return Err(EncodeError::FrameGeometry { width: *width, height: *height });
                }
                let expected = (*width as usize) * (*height as usize) * 4;
                if pixels.len() != expected {
                    return Err(EncodeError::FrameLengthMismatch { expected, actual: pixels.len() });
                }
                if expected > MAX_FRAME_BYTES {
                    return Err(EncodeError::TooLarge { len: expected });
                }
                let mut out = start(KIND_FRAME, 12 + pixels.len());
                out.extend_from_slice(&generation.to_le_bytes());
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                out.extend_from_slice(pixels);
                finish(out)
            }
            Self::Pong { seq } => {
                let mut out = start(KIND_PONG, 4);
                out.extend_from_slice(&seq.to_le_bytes());
                finish(out)
            }
            Self::Log { level, text } => {
                // Truncated here, rejected on decode. The asymmetry is
                // deliberate: a dockapp logging a long line is being
                // sloppy and should still get its first 256 bytes
                // through, but a *peer* sending an over-long one is
                // testing our bounds and gets nothing.
                let text = sanitize_text(text, MAX_LOG_BYTES);
                let mut out = start(KIND_LOG, 4 + text.len());
                out.push(level.code());
                out.push(0);
                out.extend_from_slice(&(text.len() as u16).to_le_bytes());
                out.extend_from_slice(text.as_bytes());
                finish(out)
            }
            Self::OpenPanel { width, height } => {
                // A request may exceed the caps — the shell clamps it —
                // but a zero edge is not a size anything can clamp to.
                if *width == 0 || *height == 0 {
                    return Err(EncodeError::FrameGeometry { width: *width, height: *height });
                }
                let mut out = start(KIND_OPEN_PANEL, 8);
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                finish(out)
            }
            Self::PanelFrame { generation, y, band_height, width, pixels } => {
                // A band the datagram cannot carry fails here, at the
                // call site, rather than as a mysterious EMSGSIZE from
                // `send` — the whole reason the band bound exists.
                if !panel_band_ok(*y, *band_height, *width) {
                    return Err(EncodeError::FrameGeometry { width: *width, height: *band_height });
                }
                let expected = (*width as usize) * (*band_height as usize) * 4;
                if pixels.len() != expected {
                    return Err(EncodeError::FrameLengthMismatch { expected, actual: pixels.len() });
                }
                let mut out = start(KIND_PANEL_FRAME, 16 + pixels.len());
                out.extend_from_slice(&generation.to_le_bytes());
                out.extend_from_slice(&y.to_le_bytes());
                out.extend_from_slice(&band_height.to_le_bytes());
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(pixels);
                finish(out)
            }
            Self::ClosePanel => finish(start(KIND_CLOSE_PANEL, 0)),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let (kind, mut r) = header(buf)?;
        let message = match kind {
            KIND_HELLO => {
                let proto = r.u32("proto")?;
                let tile_units = r.u8("tile_units")?;
                let wants_bits = r.u8("wants")?;
                let id_len = r.u8("id_len")? as usize;
                r.reserved(1, "hello.reserved")?;
                let mut token = [0u8; TOKEN_BYTES];
                token.copy_from_slice(r.take(TOKEN_BYTES, "token")?);
                let id = r.string(id_len, MAX_ID_BYTES, "id")?;
                if !is_valid_id(&id) {
                    return Err(DecodeError::IdCharset);
                }
                let wants = InputMask::new(wants_bits).ok_or(DecodeError::BadEnum { field: "wants", value: wants_bits })?;
                Self::Hello { proto, id, tile_units, token, wants }
            }
            KIND_FRAME => {
                let generation = r.u32("generation")?;
                let width = r.u32("width")?;
                let height = r.u32("height")?;
                // Geometry is bounds-checked *before* the multiply, so
                // `width * height * 4` cannot overflow into a small
                // "expected" that a short payload would then satisfy.
                if width == 0 || height == 0 || width > MAX_TILE_PX || height > MAX_TILE_PX * u32::from(MAX_TILE_UNITS) {
                    return Err(DecodeError::FrameGeometry { width, height });
                }
                let expected = (width as usize) * (height as usize) * 4;
                // ...and against MAX_FRAME_BYTES separately, because the
                // per-edge caps alone do not imply it.
                //
                // Found by the Phase 5 fuzz harness
                // (`tests/codec_fuzz.rs`), reproducer pinned there as
                // `a_frame_the_encoder_could_not_produce_is_not_accepted_
                // by_the_decoder`. The edge caps permit 254x258, whose
                // 262128 pixel bytes plus this message's 16-byte header
                // come to exactly MAX_MESSAGE_BYTES — so `header`'s cap
                // let it through, the geometry cap let it through, and
                // the length matched the declared size. The decoder
                // returned `Ok` for a frame its own `encode` refuses
                // with `EncodeError::TooLarge`, because MAX_FRAME_BYTES
                // is MAX_MESSAGE_BYTES - 64 and only 16 of those 64
                // bytes are header. A 48-byte window of geometries fell
                // between the two rules.
                //
                // The exposure was small (the shell's
                // `frame_matches_tile` would reject 254x258 as not
                // being any tile it allocated), but MAX_FRAME_BYTES is
                // documented as the ceiling on a Frame's payload and
                // the shell is entitled to size buffers against it. A
                // decoder that accepts what its encoder cannot emit is
                // a decoder with a corner nobody tests.
                if expected > MAX_FRAME_BYTES {
                    return Err(DecodeError::FrameGeometry { width, height });
                }
                let pixels = r.rest();
                if pixels.len() != expected {
                    return Err(DecodeError::FrameLengthMismatch { expected, actual: pixels.len() });
                }
                Self::Frame { generation, width, height, pixels: pixels.to_vec() }
            }
            KIND_PONG => Self::Pong { seq: r.u32("seq")? },
            KIND_LOG => {
                let level_code = r.u8("level")?;
                let level = LogLevel::from_code(level_code).ok_or(DecodeError::BadEnum { field: "level", value: level_code })?;
                r.reserved(1, "log.reserved")?;
                let text_len = r.u16("text_len")? as usize;
                let text = r.string(text_len, MAX_LOG_BYTES, "text")?;
                // Sanitized on the way *in*, so no later caller can
                // forget: by the time this value exists it is safe to
                // shape, print, or put in a tracing field.
                Self::Log { level, text: sanitize_text(&text, MAX_LOG_BYTES) }
            }
            KIND_OPEN_PANEL => {
                let width = r.u32("panel.width")?;
                let height = r.u32("panel.height")?;
                // A request above the caps is legal — the shell clamps
                // it when granting — so only the unclampable zero is
                // rejected here. Contrast `PanelFrame` below, whose
                // geometry claims real bytes and is bounds-checked in
                // full.
                if width == 0 || height == 0 {
                    return Err(DecodeError::PanelGeometry { width, height });
                }
                Self::OpenPanel { width, height }
            }
            KIND_PANEL_FRAME => {
                let generation = r.u32("panel.generation")?;
                let y = r.u32("panel.y")?;
                let band_height = r.u32("panel.band_height")?;
                let width = r.u32("panel.width")?;
                // Bounds before the multiply, exactly as for `Frame`,
                // with all the arithmetic in u64 so no header can
                // overflow its way past a check.
                if !panel_band_ok(y, band_height, width) {
                    return Err(DecodeError::PanelGeometry { width, height: band_height });
                }
                let expected = (width as usize) * (band_height as usize) * 4;
                let pixels = r.rest();
                if pixels.len() != expected {
                    return Err(DecodeError::FrameLengthMismatch { expected, actual: pixels.len() });
                }
                Self::PanelFrame { generation, y, band_height, width, pixels: pixels.to_vec() }
            }
            KIND_CLOSE_PANEL => Self::ClosePanel,
            _ => return Err(DecodeError::UnknownKind { kind }),
        };
        r.finish()?;
        Ok(message)
    }
}

// ---------------------------------------------------------------------
// ServerMessage codec
// ---------------------------------------------------------------------

impl ServerMessage {
    pub fn encode(&self) -> Result<Vec<u8>, EncodeError> {
        match self {
            Self::Welcome(state) => theme_state_encode(KIND_WELCOME, state),
            Self::ThemeChanged(state) => theme_state_encode(KIND_THEME_CHANGED, state),
            Self::Input(event) => input_event_encode(KIND_INPUT, event),
            Self::Visibility { visible } => {
                let mut out = start(KIND_VISIBILITY, 4);
                out.push(u8::from(*visible));
                out.extend_from_slice(&[0, 0, 0]);
                finish(out)
            }
            Self::Ping { seq } => {
                let mut out = start(KIND_PING, 4);
                out.extend_from_slice(&seq.to_le_bytes());
                finish(out)
            }
            Self::Goodbye { reason } => {
                let mut out = start(KIND_GOODBYE, 4);
                out.push(reason.code());
                out.extend_from_slice(&[0, 0, 0]);
                finish(out)
            }
            Self::PanelOpened { width, height } => {
                // A grant is the shell's own construction: refusing to
                // encode one outside the caps keeps a shell bug at the
                // shell, instead of shipping a size every conformant
                // client will refuse to draw at.
                if !panel_geometry_ok(*width, *height) {
                    return Err(EncodeError::FrameGeometry { width: *width, height: *height });
                }
                let mut out = start(KIND_PANEL_OPENED, 8);
                out.extend_from_slice(&width.to_le_bytes());
                out.extend_from_slice(&height.to_le_bytes());
                finish(out)
            }
            Self::PanelClosed { reason } => {
                // Same shape as `Goodbye` and `Visibility`: a one-byte
                // code padded to four with reserved zeros, so every
                // fixed-body message in the catalog keeps one
                // convention.
                let mut out = start(KIND_PANEL_CLOSED, 4);
                out.push(reason.code());
                out.extend_from_slice(&[0, 0, 0]);
                finish(out)
            }
            Self::PanelInput(event) => input_event_encode(KIND_PANEL_INPUT, event),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self, DecodeError> {
        let (kind, mut r) = header(buf)?;
        let message = match kind {
            KIND_WELCOME => Self::Welcome(theme_state_decode(&mut r)?),
            KIND_THEME_CHANGED => Self::ThemeChanged(theme_state_decode(&mut r)?),
            KIND_INPUT => Self::Input(input_event_decode(&mut r, false)?),
            KIND_VISIBILITY => {
                let visible = r.u8("visible")?;
                if visible > 1 {
                    return Err(DecodeError::BadEnum { field: "visible", value: visible });
                }
                r.reserved(3, "visibility.reserved")?;
                Self::Visibility { visible: visible == 1 }
            }
            KIND_PING => Self::Ping { seq: r.u32("seq")? },
            KIND_GOODBYE => {
                let code = r.u8("reason")?;
                let reason = GoodbyeReason::from_code(code).ok_or(DecodeError::BadEnum { field: "reason", value: code })?;
                r.reserved(3, "goodbye.reserved")?;
                Self::Goodbye { reason }
            }
            KIND_PANEL_OPENED => {
                let width = r.u32("panel.width")?;
                let height = r.u32("panel.height")?;
                // The client's protection against a hostile or broken
                // shell, exactly as the theme bounds are: a grant no
                // panel can be drawn at is rejected, never allocated.
                if !panel_geometry_ok(width, height) {
                    return Err(DecodeError::PanelGeometry { width, height });
                }
                Self::PanelOpened { width, height }
            }
            KIND_PANEL_CLOSED => {
                let code = r.u8("panel.reason")?;
                let reason =
                    PanelCloseReason::from_code(code).ok_or(DecodeError::BadEnum { field: "panel.reason", value: code })?;
                r.reserved(3, "panel_closed.reserved")?;
                Self::PanelClosed { reason }
            }
            KIND_PANEL_INPUT => Self::PanelInput(input_event_decode(&mut r, true)?),
            _ => return Err(DecodeError::UnknownKind { kind }),
        };
        r.finish()?;
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> ClientMessage {
        ClientMessage::Hello {
            proto: crate::PROTOCOL_VERSION,
            id: "clock".to_string(),
            tile_units: 1,
            token: [0xAB; TOKEN_BYTES],
            wants: InputMask::new(InputMask::PRESS | InputMask::CROSSING).unwrap(),
        }
    }

    fn hello_with_id(id: &str) -> ClientMessage {
        // `ClientMessage` is an enum, so there is no struct-update
        // shorthand to lean on here.
        ClientMessage::Hello {
            proto: crate::PROTOCOL_VERSION,
            id: id.to_string(),
            tile_units: 1,
            token: [0xAB; TOKEN_BYTES],
            wants: InputMask::none(),
        }
    }

    fn frame(width: u32, height: u32) -> ClientMessage {
        ClientMessage::Frame {
            generation: 7,
            width,
            height,
            pixels: vec![0x80; (width as usize) * (height as usize) * 4],
        }
    }

    fn theme_state() -> ThemeState {
        ThemeState {
            tile_px: 112,
            scale: 2.0,
            proto: crate::SHELL_PROTOCOL_VERSION,
            theme_id: "nextstep-classic".to_string(),
            theme_toml: "id = \"nextstep-classic\"\n".to_string(),
        }
    }

    // -- round trips ---------------------------------------------------

    #[test]
    fn every_client_message_survives_a_round_trip() {
        let messages = [
            hello(),
            frame(56, 56),
            ClientMessage::Pong { seq: u32::MAX },
            ClientMessage::Log { level: LogLevel::Warn, text: "battery sampler timed out".to_string() },
            ClientMessage::OpenPanel { width: 448, height: 168 },
            // Above the caps on purpose: a request is clamped by the
            // shell, not rejected by the codec.
            ClientMessage::OpenPanel { width: u32::MAX, height: u32::MAX },
            ClientMessage::PanelFrame { generation: 3, y: 8, band_height: 2, width: 4, pixels: vec![0x42; 32] },
            ClientMessage::ClosePanel,
        ];
        for message in messages {
            let bytes = message.encode().expect("encodes");
            assert_eq!(ClientMessage::decode(&bytes), Ok(message.clone()), "round trip of {message:?}");
        }
    }

    #[test]
    fn every_server_message_survives_a_round_trip() {
        let messages = [
            ServerMessage::Welcome(theme_state()),
            ServerMessage::ThemeChanged(theme_state()),
            ServerMessage::Input(InputEvent {
                kind: InputKind::Press,
                button: Some(Button::Left),
                x: 12,
                y: -3,
                delta: 0,
            }),
            ServerMessage::Input(InputEvent { kind: InputKind::Scroll, button: None, x: 1, y: 2, delta: -1 }),
            ServerMessage::Visibility { visible: true },
            ServerMessage::Visibility { visible: false },
            ServerMessage::Ping { seq: 9 },
            ServerMessage::Goodbye { reason: GoodbyeReason::Shutdown },
            ServerMessage::PanelOpened { width: 448, height: 168 },
            ServerMessage::PanelClosed { reason: PanelCloseReason::Dismissed },
            ServerMessage::PanelClosed { reason: PanelCloseReason::Refused },
            ServerMessage::PanelInput(InputEvent { kind: InputKind::Press, button: Some(Button::Left), x: 40, y: 12, delta: 0 }),
            ServerMessage::PanelInput(InputEvent { kind: InputKind::Motion, button: None, x: 3, y: 4, delta: 0 }),
        ];
        for message in messages {
            let bytes = message.encode().expect("encodes");
            assert_eq!(ServerMessage::decode(&bytes), Ok(message.clone()), "round trip of {message:?}");
        }
    }

    #[test]
    fn the_scale_float_survives_bit_exactly() {
        // Encoded as `to_bits`, not as text: 1.5 and 2.25 are the real
        // fractional scales this desktop runs at, and a tile drawn at
        // 1.4999999 instead of 1.5 lands its bevel a pixel off.
        for scale in [1.0f32, 1.5, 2.0, 2.25, 3.0] {
            let state = ThemeState { scale, ..theme_state() };
            let bytes = ServerMessage::Welcome(state).encode().unwrap();
            let ServerMessage::Welcome(decoded) = ServerMessage::decode(&bytes).unwrap() else { panic!("kind") };
            assert_eq!(decoded.scale.to_bits(), scale.to_bits());
        }
    }

    // -- pinned wire layout --------------------------------------------

    #[test]
    fn hello_has_the_documented_byte_layout() {
        // Pinned deliberately. The module doc publishes this table and
        // a third-party dockapp may be built against it; a refactor
        // that reorders two fields must fail here rather than in
        // somebody else's binary.
        let bytes = hello().encode().unwrap();
        assert_eq!(&bytes[0..4], &[0x01, 0, 0, 0], "kind + reserved");
        assert_eq!(&bytes[4..8], &crate::PROTOCOL_VERSION.to_le_bytes(), "proto");
        assert_eq!(bytes[8], 1, "tile_units");
        assert_eq!(bytes[9], InputMask::PRESS | InputMask::CROSSING, "wants");
        assert_eq!(bytes[10], 5, "id_len");
        assert_eq!(bytes[11], 0, "reserved");
        assert_eq!(&bytes[12..28], &[0xAB; 16], "token");
        assert_eq!(&bytes[28..], b"clock", "id");
        assert_eq!(bytes.len(), 33);
    }

    #[test]
    fn input_is_a_fixed_twenty_bytes() {
        let bytes = ServerMessage::Input(InputEvent {
            kind: InputKind::Scroll,
            button: None,
            x: 1,
            y: 2,
            delta: -1,
        })
        .encode()
        .unwrap();
        assert_eq!(bytes.len(), 20);
        assert_eq!(&bytes[0..4], &[0x83, 0, 0, 0]);
        assert_eq!(bytes[4], 3, "InputKind::Scroll");
        assert_eq!(bytes[5], 0, "no button");
        assert_eq!(&bytes[16..20], &(-1i32).to_le_bytes());
    }

    #[test]
    fn client_and_server_kinds_live_in_separate_number_spaces() {
        // The high bit separates the directions, so feeding a message
        // back down the socket it came from is a clean `UnknownKind`
        // rather than an accidental reinterpretation.
        for bytes in [
            ServerMessage::Ping { seq: 1 }.encode().unwrap(),
            ServerMessage::Goodbye { reason: GoodbyeReason::Shutdown }.encode().unwrap(),
        ] {
            assert!(matches!(ClientMessage::decode(&bytes), Err(DecodeError::UnknownKind { .. })));
        }
        for bytes in [hello().encode().unwrap(), ClientMessage::Pong { seq: 1 }.encode().unwrap()] {
            assert!(matches!(ServerMessage::decode(&bytes), Err(DecodeError::UnknownKind { .. })));
        }
    }

    // -- the panel family ----------------------------------------------

    #[test]
    fn panel_messages_have_the_documented_byte_layouts() {
        // Pinned like `Hello`: the protocol document publishes these
        // tables and the language bindings are built against them.
        let bytes = ClientMessage::OpenPanel { width: 448, height: 168 }.encode().unwrap();
        assert_eq!(&bytes[0..4], &[0x05, 0, 0, 0], "kind + reserved");
        assert_eq!(&bytes[4..8], &448u32.to_le_bytes(), "width");
        assert_eq!(&bytes[8..12], &168u32.to_le_bytes(), "height");
        assert_eq!(bytes.len(), 12);

        let bytes = ClientMessage::PanelFrame { generation: 9, y: 100, band_height: 1, width: 2, pixels: vec![0xAA; 8] }
            .encode()
            .unwrap();
        assert_eq!(&bytes[0..4], &[0x06, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &9u32.to_le_bytes(), "generation");
        assert_eq!(&bytes[8..12], &100u32.to_le_bytes(), "y");
        assert_eq!(&bytes[12..16], &1u32.to_le_bytes(), "band_height");
        assert_eq!(&bytes[16..20], &2u32.to_le_bytes(), "width");
        assert_eq!(&bytes[20..], &[0xAA; 8], "pixels are the rest of the datagram");

        let bytes = ClientMessage::ClosePanel.encode().unwrap();
        assert_eq!(bytes, vec![0x07, 0, 0, 0], "ClosePanel is the bare header");

        let bytes = ServerMessage::PanelOpened { width: 300, height: 200 }.encode().unwrap();
        assert_eq!(&bytes[0..4], &[0x87, 0, 0, 0]);
        assert_eq!(&bytes[4..8], &300u32.to_le_bytes());
        assert_eq!(&bytes[8..12], &200u32.to_le_bytes());
        assert_eq!(bytes.len(), 12);

        let bytes = ServerMessage::PanelClosed { reason: PanelCloseReason::Refused }.encode().unwrap();
        assert_eq!(bytes, vec![0x88, 0, 0, 0, 3, 0, 0, 0], "reason padded to four bytes, the Goodbye convention");
    }

    #[test]
    fn the_version_u16_lives_at_body_offset_ten_of_welcome() {
        // The interop-critical byte: both SDKs read the shell's
        // protocol version from the u16 at offset 10 of the
        // Welcome/ThemeChanged *body* (offset 14 of the datagram) —
        // the u16 that was reserved-and-zero in protocol 1. A current
        // shell emits 2 there; zero decodes as 1.
        let bytes = ServerMessage::Welcome(theme_state()).encode().unwrap();
        assert_eq!(&bytes[14..16], &crate::SHELL_PROTOCOL_VERSION.to_le_bytes(), "proto at body offset 10");

        let mut v1 = bytes.clone();
        v1[14] = 0;
        v1[15] = 0;
        let ServerMessage::Welcome(decoded) = ServerMessage::decode(&v1).unwrap() else { panic!("kind") };
        assert_eq!(decoded.proto, 0, "kept raw for canonical re-encoding");
        assert!(!decoded.panels_supported(), "and zero reads as a protocol-1, tiles-only shell");
    }

    #[test]
    fn motion_is_a_panel_only_input_kind() {
        // Kind 6 exists for hover UX inside a panel and nowhere else:
        // the tile Input codec treats it exactly like kind 7.
        let motion = InputEvent { kind: InputKind::Motion, button: None, x: 30, y: 40, delta: 0 };
        let bytes = ServerMessage::PanelInput(motion).encode().unwrap();
        assert_eq!(bytes[4], 6);
        assert_eq!(ServerMessage::decode(&bytes), Ok(ServerMessage::PanelInput(motion)));

        assert_eq!(ServerMessage::Input(motion).encode(), Err(EncodeError::MotionOutsidePanel));
        let mut forged = bytes.clone();
        forged[0] = 0x83;
        assert_eq!(ServerMessage::decode(&forged), Err(DecodeError::BadEnum { field: "input.kind", value: 6 }));
    }

    #[test]
    fn panel_input_is_byte_identical_to_input_except_for_its_kind() {
        // "Payload layout identical to Input" is the published
        // definition; this is that sentence as an assertion.
        let event = InputEvent { kind: InputKind::Press, button: Some(Button::Left), x: 17, y: -4, delta: 0 };
        let input = ServerMessage::Input(event).encode().unwrap();
        let panel = ServerMessage::PanelInput(event).encode().unwrap();
        assert_eq!(panel.len(), 20, "same fixed twenty bytes");
        assert_eq!(panel[0], 0x89);
        assert_eq!(&panel[1..], &input[1..], "identical after the kind byte");
    }

    #[test]
    fn panel_close_reasons_cover_exactly_the_published_codes() {
        for (reason, code) in [
            (PanelCloseReason::ClientRequest, 0u8),
            (PanelCloseReason::Dismissed, 1),
            (PanelCloseReason::Shutdown, 2),
            (PanelCloseReason::Refused, 3),
        ] {
            let bytes = ServerMessage::PanelClosed { reason }.encode().unwrap();
            assert_eq!(bytes[4], code);
            assert_eq!(ServerMessage::decode(&bytes), Ok(ServerMessage::PanelClosed { reason }));
        }
        let bad = vec![0x88, 0, 0, 0, 4, 0, 0, 0];
        assert_eq!(ServerMessage::decode(&bad), Err(DecodeError::BadEnum { field: "panel.reason", value: 4 }));
        let unpadded = vec![0x88, 0, 0, 0, 1];
        assert!(ServerMessage::decode(&unpadded).is_err(), "the reserved padding is part of the layout");
    }

    #[test]
    fn a_zero_sized_panel_request_is_rejected_but_an_oversized_one_is_not() {
        // Zero cannot be clamped into a size; over the caps is the
        // shell's clamp to make. The decoder polices only the first.
        let zero = [&[0x05u8, 0, 0, 0][..], &0u32.to_le_bytes(), &10u32.to_le_bytes()].concat();
        assert_eq!(ClientMessage::decode(&zero), Err(DecodeError::PanelGeometry { width: 0, height: 10 }));
        let big = ClientMessage::OpenPanel { width: MAX_PANEL_PX * 4, height: 2 };
        assert!(ClientMessage::decode(&big.encode().unwrap()).is_ok(), "an over-cap request decodes; the grant clamps it");
    }

    #[test]
    fn a_panel_grant_outside_the_caps_never_decodes() {
        // The client's protection against a hostile or broken shell,
        // exactly as the theme bounds are.
        let mut bytes = vec![0x87u8, 0, 0, 0];
        bytes.extend_from_slice(&(MAX_PANEL_PX + 1).to_le_bytes());
        bytes.extend_from_slice(&4u32.to_le_bytes());
        assert_eq!(
            ServerMessage::decode(&bytes),
            Err(DecodeError::PanelGeometry { width: MAX_PANEL_PX + 1, height: 4 })
        );
        assert!(matches!(
            ServerMessage::PanelOpened { width: 0, height: 4 }.encode(),
            Err(EncodeError::FrameGeometry { .. })
        ));
    }

    fn band(generation: u32, y: u32, band_height: u32, width: u32) -> Vec<u8> {
        let mut bytes = vec![0x06u8, 0, 0, 0];
        bytes.extend_from_slice(&generation.to_le_bytes());
        bytes.extend_from_slice(&y.to_le_bytes());
        bytes.extend_from_slice(&band_height.to_le_bytes());
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes
    }

    #[test]
    fn panel_band_geometry_is_bounds_checked_before_the_multiply() {
        // An edge past the cap, whatever the payload says.
        assert_eq!(
            ClientMessage::decode(&band(1, 0, 1, MAX_PANEL_PX + 1)),
            Err(DecodeError::PanelGeometry { width: MAX_PANEL_PX + 1, height: 1 })
        );
        // A band whose end row runs past the tallest grantable panel —
        // including the u32-overflow shape, which the u64 sum catches.
        assert_eq!(
            ClientMessage::decode(&band(1, MAX_PANEL_PX, 1, 4)),
            Err(DecodeError::PanelGeometry { width: 4, height: 1 })
        );
        assert_eq!(
            ClientMessage::decode(&band(1, u32::MAX, 2, 4)),
            Err(DecodeError::PanelGeometry { width: 4, height: 2 })
        );
        // A band over the one-datagram budget is refused as geometry
        // even when (as here, truncated) the payload is short.
        assert_eq!(
            ClientMessage::decode(&band(1, 0, 200, 1024)),
            Err(DecodeError::PanelGeometry { width: 1024, height: 200 })
        );
    }

    #[test]
    fn a_panel_band_whose_payload_disagrees_with_its_header_is_rejected() {
        let mut bytes = band(1, 0, 2, 2);
        bytes.extend_from_slice(&[0u8; 15]); // 16 expected
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::FrameLengthMismatch { expected: 16, actual: 15 }));
    }

    #[test]
    fn close_panel_with_a_payload_is_two_messages_pretending_to_be_one() {
        let bytes = vec![0x07u8, 0, 0, 0, 0x00];
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::TrailingBytes { extra: 1 }));
    }

    #[test]
    fn a_band_the_transport_cannot_carry_fails_at_the_encoder() {
        // 512 x 512 in one band is over the datagram frame budget: the
        // encoder refuses at the call site instead of letting `send`
        // fail mysteriously. The same panel repainted in 127-row bands
        // (the tallest legal band at this width) encodes fine, which is
        // the whole point of banding.
        let message = ClientMessage::PanelFrame { generation: 1, y: 0, band_height: 512, width: 512, pixels: vec![0; 512 * 512 * 4] };
        assert!(matches!(message.encode(), Err(EncodeError::FrameGeometry { .. })));
        let band_ok = ClientMessage::PanelFrame { generation: 1, y: 385, band_height: 127, width: 512, pixels: vec![0; 512 * 127 * 4] };
        assert!(band_ok.encode().is_ok());
    }

    // -- malformed input -----------------------------------------------

    #[test]
    fn an_empty_datagram_is_an_error_not_a_panic() {
        assert_eq!(ClientMessage::decode(&[]), Err(DecodeError::Empty));
        assert_eq!(ServerMessage::decode(&[]), Err(DecodeError::Empty));
    }

    #[test]
    fn an_oversized_datagram_is_rejected_before_anything_is_parsed() {
        let huge = vec![KIND_FRAME; MAX_MESSAGE_BYTES + 1];
        assert_eq!(ClientMessage::decode(&huge), Err(DecodeError::TooLarge { len: MAX_MESSAGE_BYTES + 1 }));
    }

    #[test]
    fn unknown_kinds_are_rejected() {
        for kind in [0x00u8, 0x08, 0x7F, 0x80, 0x8A, 0xFF] {
            let buf = [kind, 0, 0, 0];
            assert!(matches!(ClientMessage::decode(&buf), Err(DecodeError::UnknownKind { .. })), "kind {kind:#04x}");
            assert!(matches!(ServerMessage::decode(&buf), Err(DecodeError::UnknownKind { .. })), "kind {kind:#04x}");
        }
    }

    #[test]
    fn a_nonzero_reserved_byte_is_rejected_rather_than_ignored() {
        let mut bytes = ClientMessage::Pong { seq: 1 }.encode().unwrap();
        bytes[2] = 0x01;
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::ReservedNotZero { field: "header.reserved" }));

        let mut bytes = hello().encode().unwrap();
        bytes[11] = 0xFF;
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::ReservedNotZero { field: "hello.reserved" }));

        let mut bytes = ServerMessage::Ping { seq: 1 }.encode().unwrap();
        bytes[1] = 0x80;
        assert_eq!(ServerMessage::decode(&bytes), Err(DecodeError::ReservedNotZero { field: "header.reserved" }));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = ClientMessage::Pong { seq: 1 }.encode().unwrap();
        bytes.push(0);
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::TrailingBytes { extra: 1 }));

        let mut bytes = ServerMessage::Welcome(theme_state()).encode().unwrap();
        bytes.extend_from_slice(b"smuggled");
        assert_eq!(ServerMessage::decode(&bytes), Err(DecodeError::TrailingBytes { extra: 8 }));
    }

    #[test]
    fn every_truncation_of_every_message_is_an_error_and_never_a_panic() {
        // The cheap systematic version of what Phase 5's fuzzer will do
        // properly: a hostile peer controls the datagram length, so
        // every prefix of every message must land in `Err` without
        // indexing off the end of the buffer.
        let client = [
            hello().encode().unwrap(),
            frame(8, 8).encode().unwrap(),
            ClientMessage::Pong { seq: 3 }.encode().unwrap(),
            ClientMessage::Log { level: LogLevel::Info, text: "hi".into() }.encode().unwrap(),
        ];
        for bytes in &client {
            for len in 0..bytes.len() {
                assert!(ClientMessage::decode(&bytes[..len]).is_err(), "prefix {len} of {bytes:?} decoded");
            }
            assert!(ClientMessage::decode(bytes).is_ok());
        }

        let server = [
            ServerMessage::Welcome(theme_state()).encode().unwrap(),
            ServerMessage::Input(InputEvent { kind: InputKind::Enter, button: None, x: 0, y: 0, delta: 0 })
                .encode()
                .unwrap(),
            ServerMessage::Visibility { visible: true }.encode().unwrap(),
            ServerMessage::Ping { seq: 3 }.encode().unwrap(),
            ServerMessage::Goodbye { reason: GoodbyeReason::Removed }.encode().unwrap(),
        ];
        for bytes in &server {
            for len in 0..bytes.len() {
                assert!(ServerMessage::decode(&bytes[..len]).is_err(), "prefix {len} decoded");
            }
            assert!(ServerMessage::decode(bytes).is_ok());
        }
    }

    #[test]
    fn flipping_any_single_byte_never_panics_the_decoder() {
        let originals = [hello().encode().unwrap(), frame(4, 4).encode().unwrap(), ServerMessage::Welcome(theme_state()).encode().unwrap()];
        for bytes in &originals {
            for index in 0..bytes.len() {
                for mask in [0x01u8, 0x80, 0xFF] {
                    let mut mutated = bytes.clone();
                    mutated[index] ^= mask;
                    // The result is uninteresting; not unwinding is the
                    // assertion.
                    let _ = ClientMessage::decode(&mutated);
                    let _ = ServerMessage::decode(&mutated);
                }
            }
        }
    }

    #[test]
    fn a_frame_header_that_lies_about_its_size_is_rejected() {
        // Declares a 56x56 tile, carries four bytes. The pixel payload
        // is the datagram remainder, so a lying header can only ever be
        // a length mismatch — there is no separate length field to
        // desynchronize from.
        let mut bytes = start(KIND_FRAME, 16);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&56u32.to_le_bytes());
        bytes.extend_from_slice(&56u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(
            ClientMessage::decode(&bytes),
            Err(DecodeError::FrameLengthMismatch { expected: 56 * 56 * 4, actual: 4 })
        );
    }

    #[test]
    fn frame_dimensions_that_would_overflow_the_pixel_count_are_rejected_by_geometry() {
        // `width * height * 4` on a 32-bit count would wrap; the
        // geometry bound is checked first precisely so the multiply
        // below it can never be reached with these values.
        for (w, h) in [(u32::MAX, u32::MAX), (0x4000_0000, 4), (0, 56), (56, 0), (MAX_TILE_PX + 1, 56)] {
            let mut bytes = start(KIND_FRAME, 12);
            bytes.extend_from_slice(&0u32.to_le_bytes());
            bytes.extend_from_slice(&w.to_le_bytes());
            bytes.extend_from_slice(&h.to_le_bytes());
            assert_eq!(
                ClientMessage::decode(&bytes),
                Err(DecodeError::FrameGeometry { width: w, height: h }),
                "geometry {w}x{h}"
            );
        }
    }

    #[test]
    fn a_frame_bigger_than_the_transport_can_carry_fails_to_encode() {
        // The v1-inline ceiling, surfaced at the call site instead of
        // as a surprise `EMSGSIZE` from the kernel.
        let too_big = frame(MAX_TILE_PX, MAX_TILE_PX * u32::from(MAX_TILE_UNITS));
        assert!(matches!(too_big.encode(), Err(EncodeError::TooLarge { .. })));
    }

    #[test]
    fn a_frame_over_the_payload_ceiling_is_refused_even_when_both_its_edges_are_legal() {
        // The Phase 5 fuzz finding, kept in-crate as well as in
        // `tests/codec_fuzz.rs` so that `cargo test -p chonk-dock-proto
        // --lib` alone still covers it.
        //
        // 254 and 258 are both inside the per-edge caps (MAX_TILE_PX
        // and MAX_TILE_PX * MAX_TILE_UNITS), but 254*258*4 = 262128 is
        // over MAX_FRAME_BYTES while 262128 + 16 is exactly
        // MAX_MESSAGE_BYTES — so every other bound in the decoder was
        // satisfied. The encoder always refused this frame; the decoder
        // used to accept it.
        let (w, h) = (254u32, 258u32);
        let payload = (w as usize) * (h as usize) * 4;
        assert!(payload > MAX_FRAME_BYTES && payload + HEADER_BYTES + 12 == MAX_MESSAGE_BYTES, "the reproducer still sits on the ceiling");

        let mut bytes = start(KIND_FRAME, 12 + payload);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&w.to_le_bytes());
        bytes.extend_from_slice(&h.to_le_bytes());
        bytes.resize(HEADER_BYTES + 12 + payload, 0xAA);
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::FrameGeometry { width: w, height: h }));

        // And the property the fix restores, stated directly: the
        // decoder never accepts what the encoder could not have
        // produced.
        assert!(frame(w, h).encode().is_err(), "the encoder always refused this one");
    }

    #[test]
    fn every_decodable_frame_fits_the_payload_budget_the_shell_allocates_against() {
        // The general form of the test above, over the whole geometry
        // space the per-edge caps permit. Cheap because it never
        // materializes a payload — it only asks the arithmetic whether
        // a frame of these dimensions could decode at all.
        for width in 1..=MAX_TILE_PX {
            for height in 1..=MAX_TILE_PX * u32::from(MAX_TILE_UNITS) {
                let payload = (width as usize) * (height as usize) * 4;
                let decodable = payload <= MAX_FRAME_BYTES && payload + HEADER_BYTES + 12 <= MAX_MESSAGE_BYTES;
                let encodable = payload <= MAX_FRAME_BYTES;
                assert_eq!(decodable, encodable, "the two ends disagree about {width}x{height}");
            }
        }
    }

    // -- hostile strings -----------------------------------------------

    #[test]
    fn an_id_can_look_like_a_relative_path_so_the_shell_must_never_join_one_to_a_path() {
        // Not a defect in this crate, and deliberately not "fixed" here
        // — it is a written obligation on the consumer, which is what a
        // test in the crate that defines the charset is for.
        //
        // The allowlist permits `.` (`org.example.weather`) and `:`
        // (`builtin:clock`), which means `..` and `.` are themselves
        // valid ids. Nothing in this crate turns an id into a path, and
        // the design's uses of one — a HashMap key, a line in
        // `$XDG_STATE_HOME/chonkstep/dock-items`, a label in the
        // per-tile menu — are all safe. But the first time somebody
        // writes `dockapps_dir.join(id)`, a dockapp that declares
        // `id = ".."` is reading a directory it was not offered.
        //
        // Rejecting them here was considered and not done: the id
        // charset is wire-visible policy that the shell side is being
        // written against right now, and the correct place to refuse a
        // path component is the code that builds a path. If a path join
        // ever does appear, tighten it *there*, and note that
        // `format!("{id}.dockapp")` is already safe because the suffix
        // makes `..` into `...dockapp`.
        assert!(is_valid_id(".."), "documented, not endorsed");
        assert!(is_valid_id("."));
        assert!(!is_valid_id("../etc"), "a separator is still refused, which is what stops traversal proper");
    }

    // -- floats ----------------------------------------------------------

    #[test]
    fn a_nan_scale_is_rejected_at_decode_rather_than_handed_on() {
        // Found by the Phase 5 fuzz harness. A NaN `scale` is not a
        // cosmetic problem: `ThemeState` derives `PartialEq` over a raw
        // `f32`, so a message carrying one is not equal to *itself*, and
        // the shape every consumer of this type naturally writes —
        // `if next_state != last_sent { push ThemeChanged }` — then
        // pushes on every pass forever, at the compositor's repaint
        // rate, until the peer's send queue overflows.
        //
        // Phase 5 left this as a note for the consumer because
        // `DecodeError` had no variant for a float and adding one was a
        // breaking change to a crate the shell was being written
        // against. It is cheaper now than it will ever be again, so the
        // check moved to the one place both sides share.
        let state = ThemeState { tile_px: 56, scale: f32::NAN, proto: 2, theme_id: "x".into(), theme_toml: String::new() };
        let bytes = ServerMessage::Welcome(state).encode().unwrap();
        assert_eq!(
            ServerMessage::decode(&bytes),
            Err(DecodeError::BadFloat { field: "scale", bits: f32::NAN.to_bits() }),
            "a NaN scale must not reach a consumer that will compare it"
        );
    }

    /// The property the `BadFloat` check exists to establish, stated
    /// over every scale bit pattern that is interesting or adversarial:
    /// **whatever a peer sends, a message that decodes is equal to
    /// itself.** Without it `ServerMessage`'s derived `PartialEq` is not
    /// an equivalence relation, and every `!=` written against it is a
    /// latent infinite loop.
    #[test]
    fn every_message_that_decodes_is_equal_to_itself() {
        let interesting = [
            f32::NAN,
            -f32::NAN,
            f32::from_bits(0x7f80_0001), // a signalling NaN
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -0.0,
            -1.0,
            f32::MIN_POSITIVE,
            f32::from_bits(1), // the smallest subnormal
            1.0,
            1.5,
            2.0,
            MAX_SCALE,
            MAX_SCALE + 0.1,
            1e30,
        ];
        for scale in interesting {
            let state = ThemeState { tile_px: 56, scale, proto: 2, theme_id: "x".into(), theme_toml: String::new() };
            let bytes = ServerMessage::ThemeChanged(state).encode().unwrap();
            let Ok(decoded) = ServerMessage::decode(&bytes) else { continue };
            assert_eq!(decoded, decoded, "scale {scale} decoded to something not equal to itself");
        }
    }

    #[test]
    fn an_unusable_scale_is_rejected_rather_than_carried() {
        // Zero and negative collapse or invert every metric in the
        // palette; the infinities and 1e30 ask for a pixmap no allocator
        // will produce. Rejecting rather than clamping is the same call
        // the SDK's `check_drawable` already makes: a peer sending an
        // unusable scale is one this end cannot correctly draw for, and
        // quietly substituting 1.0 would put a wrongly-sized tile on
        // screen and call it success.
        for scale in [0.0f32, -0.0, -1.0, f32::INFINITY, f32::NEG_INFINITY, 1e30, MAX_SCALE + 0.1] {
            let state = ThemeState { tile_px: 56, scale, proto: 2, theme_id: "x".into(), theme_toml: String::new() };
            let bytes = ServerMessage::ThemeChanged(state).encode().unwrap();
            assert_eq!(
                ServerMessage::decode(&bytes),
                Err(DecodeError::BadFloat { field: "scale", bits: scale.to_bits() }),
                "scale {scale} must not reach a dockapp"
            );
        }
    }

    #[test]
    fn the_scales_a_real_session_runs_at_all_decode() {
        // The bound has to be generous enough never to reject a real
        // desktop; that is half of what makes rejecting the rest safe.
        for scale in [0.5f32, 1.0, 1.25, 1.5, 2.0, 2.25, 3.0, 4.0, MAX_SCALE] {
            let state = ThemeState { tile_px: 56, scale, proto: 2, theme_id: "x".into(), theme_toml: String::new() };
            let bytes = ServerMessage::Welcome(state.clone()).encode().unwrap();
            assert_eq!(ServerMessage::decode(&bytes), Ok(ServerMessage::Welcome(state)), "scale {scale} is a real session");
        }
    }

    #[test]
    fn same_as_is_reflexive_where_derived_equality_is_not() {
        // The sender's half of the same guarantee. The shell builds its
        // own `ThemeState` and never decodes it, so `BadFloat` cannot
        // cover it; `same_as` is what makes "has it changed?" a total
        // question there.
        let nan = ThemeState { tile_px: 56, scale: f32::NAN, proto: 2, theme_id: "x".into(), theme_toml: String::new() };
        assert_ne!(nan, nan, "the derive is not reflexive, which is exactly the trap");
        assert!(nan.same_as(&nan), "`same_as` is");

        let ordinary = ThemeState { scale: 2.0, ..nan.clone() };
        assert!(ordinary.same_as(&ordinary));
        assert!(!ordinary.same_as(&nan), "and it still separates two different states");
        assert!(!ordinary.same_as(&ThemeState { tile_px: 112, ..ordinary.clone() }));
        assert!(!ordinary.same_as(&ThemeState { theme_id: "y".into(), ..ordinary.clone() }));
        assert!(!ordinary.same_as(&ThemeState { theme_toml: "a = 1".into(), ..ordinary.clone() }));
    }

    #[test]
    fn an_over_long_id_is_rejected_on_decode() {
        let long = "a".repeat(MAX_ID_BYTES + 1);
        let mut bytes = start(KIND_HELLO, 24 + long.len());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(1);
        bytes.push(0);
        bytes.push(long.len() as u8);
        bytes.push(0);
        bytes.extend_from_slice(&[0u8; TOKEN_BYTES]);
        bytes.extend_from_slice(long.as_bytes());
        assert_eq!(
            ClientMessage::decode(&bytes),
            Err(DecodeError::StringLength { field: "id", len: MAX_ID_BYTES + 1, max: MAX_ID_BYTES })
        );
    }

    #[test]
    fn an_id_at_exactly_the_cap_is_accepted() {
        let id = "a".repeat(MAX_ID_BYTES);
        let bytes = hello_with_id(&id).encode().unwrap();
        let ClientMessage::Hello { id: decoded, .. } = ClientMessage::decode(&bytes).unwrap() else { panic!() };
        assert_eq!(decoded, id);
    }

    #[test]
    fn ids_outside_the_allowlist_are_rejected_at_both_ends() {
        for hostile in [
            "clock\nnet",             // forges a second line in any log
            "clock\u{202E}kcolc",     // bidi override rewrites text beside it
            "clock net",              // a space breaks the one-id-per-line registry file
            "../../etc/passwd",       // an id is used to key files; keep `/` out
            "clock\u{0}",             // NUL
            "",                       // empty
        ] {
            assert!(!is_valid_id(hostile), "{hostile:?} should not be a valid id");
            let message = hello_with_id(hostile);
            assert!(message.encode().is_err(), "{hostile:?} should not encode");
        }
        assert!(is_valid_id("builtin:clock"), "reserved built-in ids share the namespace");
        assert!(is_valid_id("org.example.weather-2"));
    }

    #[test]
    fn a_hostile_id_smuggled_past_the_encoder_is_still_caught_on_decode() {
        // The encoder refuses to build one, so this hand-assembles the
        // bytes a non-Rust or malicious client would send.
        let id = b"clock\nnet";
        let mut bytes = start(KIND_HELLO, 24 + id.len());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(1);
        bytes.push(0);
        bytes.push(id.len() as u8);
        bytes.push(0);
        bytes.extend_from_slice(&[0u8; TOKEN_BYTES]);
        bytes.extend_from_slice(id);
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::IdCharset));
    }

    #[test]
    fn invalid_utf8_in_a_string_field_is_rejected() {
        let mut bytes = start(KIND_LOG, 4 + 2);
        bytes.push(LogLevel::Info.code());
        bytes.push(0);
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&[0xFF, 0xFE]);
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::BadUtf8 { field: "text" }));
    }

    #[test]
    fn an_over_long_log_line_is_rejected_on_decode_and_truncated_on_encode() {
        // A 10MB "tooltip" handed to cosmic-text is a rendering denial
        // of service, so the bound is enforced before the string is
        // ever materialized.
        let long = "x".repeat(MAX_LOG_BYTES + 1);
        let mut bytes = start(KIND_LOG, 4 + long.len());
        bytes.push(LogLevel::Info.code());
        bytes.push(0);
        bytes.extend_from_slice(&(long.len() as u16).to_le_bytes());
        bytes.extend_from_slice(long.as_bytes());
        assert_eq!(
            ClientMessage::decode(&bytes),
            Err(DecodeError::StringLength { field: "text", len: MAX_LOG_BYTES + 1, max: MAX_LOG_BYTES })
        );

        let encoded = ClientMessage::Log { level: LogLevel::Info, text: long }.encode().unwrap();
        let ClientMessage::Log { text, .. } = ClientMessage::decode(&encoded).unwrap() else { panic!() };
        assert_eq!(text.len(), MAX_LOG_BYTES);
    }

    #[test]
    fn control_characters_never_survive_the_decoder() {
        let hostile = "line\u{1b}[2Jone\nline two\u{202E}reversed\u{200B}";
        let encoded = ClientMessage::Log { level: LogLevel::Error, text: hostile.to_string() }.encode().unwrap();
        let ClientMessage::Log { text, .. } = ClientMessage::decode(&encoded).unwrap() else { panic!() };
        assert!(!text.chars().any(char::is_control), "no C0/C1 controls survive: {text:?}");
        assert!(!text.contains('\u{202E}'), "no bidi override survives");
        assert!(!text.contains('\u{200B}'), "no zero-width space survives");
        assert_eq!(
            text, "line[2Joneline tworeversed",
            "the dangerous characters are dropped and the surrounding text closes up behind them"
        );
    }

    #[test]
    fn sanitize_truncates_on_a_char_boundary() {
        // A byte-sliced multi-byte string is not valid UTF-8 and
        // panics `String::from_utf8` long before cosmic-text sees it.
        let wide = "ü".repeat(10); // two bytes each
        let out = sanitize_text(&wide, 5);
        assert_eq!(out, "üü", "4 bytes fit, the fifth char would be 6");
        assert!(std::str::from_utf8(out.as_bytes()).is_ok());
    }

    #[test]
    fn unicode_line_separators_are_dropped_along_with_the_ascii_one() {
        // A Phase 5 finding. `char::is_control` covers C0 and C1 but
        // not U+2028 (LINE SEPARATOR, category Zl) or U+2029
        // (PARAGRAPH SEPARATOR, Zp), so a `Log` carrying one used to
        // arrive with a line break intact — forging exactly the second
        // journal entry that dropping `\n` exists to prevent. "No
        // control characters" is an ASCII answer to a Unicode question.
        let hostile = "battery ok\u{2028}ERROR: disk failing\u{2029}second forged line";
        let clean = sanitize_text(hostile, MAX_LOG_BYTES);
        assert!(!clean.contains('\u{2028}') && !clean.contains('\u{2029}'), "{clean:?}");
        assert_eq!(clean, "battery okERROR: disk failingsecond forged line", "the text closes up behind them");

        // ...and end to end through the codec, which is where it
        // matters: by the time a `ClientMessage::Log` exists it is
        // documented as safe to shape and to print.
        let encoded = ClientMessage::Log { level: LogLevel::Info, text: hostile.to_string() }.encode().unwrap();
        let ClientMessage::Log { text, .. } = ClientMessage::decode(&encoded).unwrap() else { panic!() };
        assert!(!text.contains('\u{2028}') && !text.contains('\u{2029}'));
    }

    #[test]
    fn sanitize_leaves_ordinary_text_alone() {
        assert_eq!(sanitize_text("CPU 42% · 3.4 GHz", 256), "CPU 42% · 3.4 GHz");
    }

    // -- enums ---------------------------------------------------------

    #[test]
    fn undefined_enum_discriminants_are_rejected() {
        let mut bytes = ServerMessage::Input(InputEvent { kind: InputKind::Press, button: Some(Button::Left), x: 0, y: 0, delta: 0 })
            .encode()
            .unwrap();
        bytes[4] = 99;
        assert_eq!(ServerMessage::decode(&bytes), Err(DecodeError::BadEnum { field: "input.kind", value: 99 }));

        let mut bytes = ServerMessage::Input(InputEvent { kind: InputKind::Press, button: Some(Button::Left), x: 0, y: 0, delta: 0 })
            .encode()
            .unwrap();
        bytes[5] = 4;
        assert_eq!(ServerMessage::decode(&bytes), Err(DecodeError::BadEnum { field: "input.button", value: 4 }));

        let mut bytes = ServerMessage::Goodbye { reason: GoodbyeReason::Shutdown }.encode().unwrap();
        bytes[4] = 0;
        assert_eq!(ServerMessage::decode(&bytes), Err(DecodeError::BadEnum { field: "reason", value: 0 }));

        let mut bytes = ClientMessage::Log { level: LogLevel::Info, text: String::new() }.encode().unwrap();
        bytes[4] = 7;
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::BadEnum { field: "level", value: 7 }));
    }

    #[test]
    fn a_bool_on_the_wire_is_zero_or_one_and_nothing_else() {
        let mut bytes = ServerMessage::Visibility { visible: true }.encode().unwrap();
        bytes[4] = 2;
        assert_eq!(ServerMessage::decode(&bytes), Err(DecodeError::BadEnum { field: "visible", value: 2 }));
    }

    #[test]
    fn unknown_input_mask_bits_are_rejected() {
        assert_eq!(InputMask::new(0xFF), None);
        assert_eq!(InputMask::all().bits(), 0b1111);
        let mut bytes = hello().encode().unwrap();
        bytes[9] = 0xF0;
        assert_eq!(ClientMessage::decode(&bytes), Err(DecodeError::BadEnum { field: "wants", value: 0xF0 }));
    }

    #[test]
    fn an_input_mask_gates_exactly_the_events_it_names() {
        let crossing_only = InputMask::new(InputMask::CROSSING).unwrap();
        assert!(crossing_only.accepts(InputKind::Enter));
        assert!(crossing_only.accepts(InputKind::Leave), "wanting Enter without Leave latches hover forever");
        assert!(!crossing_only.accepts(InputKind::Press));
        assert!(!InputMask::none().accepts(InputKind::Scroll));
        for kind in [InputKind::Press, InputKind::Release, InputKind::Scroll, InputKind::Enter, InputKind::Leave] {
            assert!(InputMask::all().accepts(kind));
        }
    }

    // -- geometry policy -----------------------------------------------

    #[test]
    fn a_frame_from_before_a_monitor_change_does_not_match_the_new_tile() {
        assert!(frame_matches_tile(56, 56, 56, 1));
        assert!(frame_matches_tile(56, 112, 56, 2));
        assert!(!frame_matches_tile(56, 56, 112, 1), "the dock rescaled under it");
        assert!(!frame_matches_tile(56, 56, 56, 2), "one tile's worth of pixels for a two-tile slot");
        assert!(!frame_matches_tile(112, 56, 56, 1));
    }
}
