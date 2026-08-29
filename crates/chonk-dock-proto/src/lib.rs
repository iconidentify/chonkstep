//! The chonkstep dockapp wire protocol.
//!
//! A *dockapp* is a separate process that draws one (or a few) dock
//! tiles and pushes the finished pixels to the desktop shell over a
//! private `SOCK_SEQPACKET` Unix socket. It is neither an X client nor
//! a Wayland client: it never opens a display connection, so there is
//! nothing for it to screenshot, no window list to enumerate, and no
//! clipboard to read. The shell blits its pixels exactly as it blits a
//! built-in dock widget's `DecorationBuffer`.
//!
//! This crate is the whole contract between the two sides, and it is
//! deliberately the *only* thing they share:
//!
//! - [`wire`] is the codec — pure, allocation-bounded, no I/O. Every
//!   `decode` is written on the assumption that the bytes came from a
//!   hostile process, because in the third-party case they did.
//! - [`transport`] is the socket: path convention, permissions,
//!   non-blocking accept, `MSG_NOSIGNAL` sends, peer-credential checks.
//! - [`handshake`] is the admission decision — token, version, and
//!   whether the tile a dockapp asks for can physically be carried.
//! - [`queue`] is the backpressure policy — a bounded send queue that
//!   drops rather than blocks, and a token bucket that coalesces rather
//!   than queues.
//!
//! # The one invariant that matters
//!
//! **The shell must never block on a dockapp.** This desktop already
//! shipped one bug of exactly that shape: a dock widget called
//! `nmcli dev wifi` synchronously from the compositor's repaint thread,
//! which blocked for ~3.6s per hardware scan, and the resulting freeze
//! was misreported by the compositor's own watchdog as a display-driver
//! stall (see the workspace `clippy.toml` for the full post-mortem). A
//! blocking `write()` to a dockapp that has stopped calling `recv()` is
//! that same bug with a different syscall: the tile would freeze the
//! *whole desktop* instead of just itself.
//!
//! Every design decision below that looks paranoid is downstream of
//! that one sentence, and the properties are asserted by tests rather
//! than left as intentions:
//! [`transport::SeqpacketListener::accept`] hands back a socket that is
//! already `O_NONBLOCK` (`accept4`, so there is not even an instant
//! where it isn't), sends additionally pass `MSG_DONTWAIT` so the
//! property does not depend on a flag surviving, and
//! [`queue::SendQueue`] absorbs the `EAGAIN` that results.
//!
//! Three places make that claim checkable rather than merely stated,
//! and a change to this crate should keep all three passing:
//!
//! - `tests/hostile_peer.rs` — a thousand simulated repaint passes
//!   against a peer that stopped reading, required to fit inside one
//!   16 ms frame. The same shape, and the same numbers, as
//!   `chonk-shell`'s
//!   `a_sampler_blocked_in_a_child_process_costs_the_caller_nothing`,
//!   which guards the first version of this bug.
//! - `tests/codec_fuzz.rs` — a seeded, deterministic fuzz harness over
//!   [`wire`], including every single-byte mutation of every valid
//!   message. Its header explains why it is this and not `cargo-fuzz`.
//! - `examples/chonk-dockapp-torture` — the same properties against a
//!   real hostile *process*, with modes for hanging, flooding,
//!   crashing, lying and refusing to complete a handshake. Its own
//!   `tests/against_a_fake_shell.rs` runs it under a minimal shell, so
//!   the process-level claim is in CI and not only in a demo.
//!
//! # Versioning
//!
//! [`PROTOCOL_VERSION`] is presented in [`wire::ClientMessage::Hello`]
//! and checked by the shell. v1 carries tile pixels inline in the
//! `Frame` message; see [`MAX_FRAME_BYTES`] for the size ceiling that
//! implies and the condition under which a v2 shared-memory transport
//! becomes worth building.

pub mod handshake;
pub mod queue;
pub mod transport;
pub mod wire;

pub use handshake::{validate_hello, Accepted};
pub use queue::{FrameLimiter, SendOutcome, SendQueue};
pub use transport::{Seqpacket, SeqpacketListener};
pub use wire::{ClientMessage, DecodeError, EncodeError, ServerMessage};

/// Bumped whenever the byte layout in [`wire`] changes in a way an
/// older peer would misread. The shell rejects a `Hello` that does not
/// present exactly this value — deliberately an equality check and not
/// a `>=` range, because a dockapp built against a *newer* protocol is
/// just as unreadable as one built against an older one, and "reject
/// with a reason" beats "misparse a frame into garbage pixels".
pub const PROTOCOL_VERSION: u32 = 1;

/// A dockapp authenticates by echoing back a 128-bit nonce the shell
/// minted for its slot and passed in `CHONKSTEP_DOCK_TOKEN`. 128 bits
/// because the token is the *only* thing standing between a stray
/// process on this machine and a tile in the user's dock; the socket's
/// own 0600 mode already restricts it to this uid, so this is the
/// second lock, not the first.
pub const TOKEN_BYTES: usize = 16;

/// Hard ceiling on a single protocol message, enforced by every
/// `decode` before a single byte is copied out.
///
/// 256 KiB is derived, not picked. `AF_UNIX` refuses a datagram larger
/// than `SO_SNDBUF - 32`, and `SO_SNDBUF` is clamped to
/// `net.core.wmem_max` and then doubled by the kernel. On a stock Linux
/// (`wmem_max` 212992) that puts the real ceiling at ~416 KiB once
/// [`transport`] has widened the buffers, and at ~208 KiB if that
/// widening silently failed. 256 KiB is the largest round number that
/// clears the *widened* floor with room to spare while staying close
/// enough to the un-widened one to be a bug and not a catastrophe.
///
/// The practical consequence, worth stating because it is what a
/// dockapp author runs into: it covers every tile geometry this desktop
/// plausibly has — including a four-tile stack at `CHONKSTEP_SCALE` 2
/// and a two-tile stack at scale 3 — and stops short of a four-tile
/// stack at scale 3 (451 KB), which is the case that has to wait for
/// the v2 transport. See [`MAX_FRAME_BYTES`].
pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

/// Ceiling on the pixel payload of one [`wire::ClientMessage::Frame`].
///
/// A dock tile is 56 logical pixels square (`desktop.rs`: `56.0 *
/// scale`), so a 1-unit tile costs 12.5 KB at scale 1 and 50 KB at
/// scale 2 — comfortably inline. This is the number that decides when
/// the specified-but-unbuilt v2 transport (double-buffered `memfd`
/// passed once via `SCM_RIGHTS`, `Frame` degenerating to "slot N,
/// generation G is complete") stops being premature: **build v2 when a
/// shipped dockapp wants a tile this cannot carry, or updates faster
/// than ~5 Hz at scale 2.** Until then a CPU copy of 50 KB at 1 Hz is
/// not worth a shared-memory lifetime problem.
pub const MAX_FRAME_BYTES: usize = MAX_MESSAGE_BYTES - 64;

/// Widest tile edge accepted from a dockapp, in device pixels. 56
/// logical pixels at scale 4 is 224; 256 leaves headroom without
/// letting a `Hello` claim a tile the size of the screen.
pub const MAX_TILE_PX: u32 = 256;

/// How many stacked tiles one dockapp may occupy. The dock is a
/// vertical strip on a real screen; a dockapp asking for more than four
/// tiles is asking for the dock, not a slot in it.
pub const MAX_TILE_UNITS: u8 = 4;

/// A dockapp id is a registry key (it names the `.dockapp` file that
/// declared it) and appears in shell logs and the per-tile menu. Short
/// and bounded because every byte of it is attacker-chosen.
pub const MAX_ID_BYTES: usize = 64;

/// A `Log` line's text budget. Bounded before it is ever shaped: a
/// 10 MB "tooltip" handed to `cosmic-text` is a rendering denial of
/// service that costs the attacker one `write()`.
pub const MAX_LOG_BYTES: usize = 256;

/// Theme ids are kebab-case registry keys (`"nextstep-classic"`).
pub const MAX_THEME_ID_BYTES: usize = 64;

/// The serialized-`Theme` correctness path in `Welcome`/`ThemeChanged`.
/// The shell generates this, so the bound exists to protect a *dockapp*
/// from a hostile or broken shell — the SDK is third-party code and has
/// no more reason to trust its peer than the shell has to trust it.
pub const MAX_THEME_TOML_BYTES: usize = 128 * 1024;

/// Whether a tile of this geometry can be carried by the v1 inline
/// transport. The shell calls this while validating a `Hello` and
/// refuses the connection when it returns `false`, rather than
/// accepting a dockapp whose every frame would then fail to encode.
///
/// A `false` here is the documented trigger for the v2 `memfd`
/// transport described on [`MAX_FRAME_BYTES`].
pub fn frame_fits(tile_px: u32, tile_units: u8) -> bool {
    if tile_px == 0 || tile_px > MAX_TILE_PX || tile_units == 0 || tile_units > MAX_TILE_UNITS {
        return false;
    }
    let bytes = (tile_px as u64) * (tile_px as u64) * (tile_units as u64) * 4;
    bytes <= MAX_FRAME_BYTES as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tile_sizes_this_desktop_actually_uses_fit_inline() {
        // `desktop.rs` computes `tile = (56.0 * scale).round()`. These
        // are the scales a real session runs at, and a dockapp that
        // cannot draw at the user's scale is not a dockapp.
        for scale in [1.0f32, 1.5, 2.0, 3.0] {
            let tile = (56.0 * scale).round() as u32;
            assert!(frame_fits(tile, 1), "a single {tile}px tile must fit inline");
            assert!(frame_fits(tile, 2), "a two-unit {tile}px tile must fit inline");
        }
        // The tallest stack at the scale most HiDPI sessions use.
        assert!(frame_fits(112, MAX_TILE_UNITS), "a four-tile stack at scale 2");
    }

    #[test]
    fn a_tile_too_big_for_a_seqpacket_datagram_is_rejected_not_truncated() {
        // 224px (scale 4) x 4 units = 802 KB, twice what an AF_UNIX
        // datagram can carry even after `transport` widens the socket
        // buffers. These are the cases that must trip the documented v2
        // trigger rather than silently producing an unsendable frame.
        assert!(!frame_fits(224, 4), "four tiles at scale 4");
        assert!(!frame_fits(168, 4), "four tiles at scale 3: 451 KB");
        assert!(!frame_fits(MAX_TILE_PX, MAX_TILE_UNITS));
    }

    #[test]
    fn degenerate_geometry_never_fits() {
        assert!(!frame_fits(0, 1), "zero-width tile");
        assert!(!frame_fits(56, 0), "zero tile units");
        assert!(!frame_fits(MAX_TILE_PX + 1, 1), "wider than the cap");
        assert!(!frame_fits(56, MAX_TILE_UNITS + 1), "taller than the cap");
    }

    #[test]
    fn frame_payload_can_never_overflow_a_message() {
        const { assert!(MAX_FRAME_BYTES < MAX_MESSAGE_BYTES, "the header has to fit too") };
    }
}
