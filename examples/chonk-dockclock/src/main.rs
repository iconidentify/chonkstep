//! The reference dockapp: the dock's analog clock tile, drawn from a
//! separate process.
//!
//! This is a deliberate reimplementation of
//! `chonk-shell/src/widgets/clock.rs` — the same 49-line widget, the
//! same `wm_theme::clock::render_clock_tile` call, the same absence of
//! click handling and sampling — carried across the process boundary
//! and onto the public SDK. It is **not** shipped as *the* clock; the
//! built-in stays built in.
//!
//! It exists as a permanent conformance test. An out-of-process tile
//! path that nothing exercises is a path that rots: the day a change to
//! the protocol, the handshake, the theme push or the frame geometry
//! breaks third-party dockapps, this binary breaks with it, in CI and
//! on a developer's desktop, rather than in somebody else's program six
//! months later. Everything it uses is public SDK surface, and that is
//! the point of the exercise — if this file ever needs an internal API
//! to work, the SDK is missing something.
//!
//! Run it by hand and it will tell you it is meant to be launched by
//! the dock; that message is `CHONKSTEP_DOCK_SOCKET` being absent.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chonk_ui::dockapp::{self, Handlers, Options, Pixmap};

/// Local wall-clock hours/minutes/seconds, copied verbatim from the
/// built-in widget so the two tiles cannot disagree about the time.
fn now_hms() -> (u32, u32, u32) {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs_today = secs % 86_400;
    ((secs_today / 3600) as u32, ((secs_today % 3600) / 60) as u32, (secs_today % 60) as u32)
}

fn main() {
    // The second hand moves once a second, so the *sampling* interval
    // has to be shorter than a second or the hand lags by up to a full
    // one — a 1 Hz sampler and a 1 Hz signal beat against each other.
    // Sampling four times a second and returning `false` from `draw`
    // when the time has not changed keeps the wire at roughly one frame
    // per second regardless, which is exactly the split the SDK's
    // "return whether anything changed" contract exists to allow.
    let options = Options { redraw_interval: Duration::from_millis(250), ..Options::default() };

    let mut shown: Option<(u32, u32, u32)> = None;
    let result = dockapp::run_with(
        "chonk-dockclock",
        options,
        Handlers {
            draw: |ctx: &dockapp::Ctx, pixmap: &mut Pixmap| {
                let hms = now_hms();
                if shown == Some(hms) {
                    return false;
                }
                let (hour, minute, second) = hms;
                let tile = chonk_ui::clock::render_clock_tile(ctx.theme(), ctx.tile_px(), hour, minute, second);

                // The renderer clamps very small tiles (`size.max(8)`),
                // so its buffer can in principle disagree with the
                // pixmap the shell is expecting. Skipping the frame is
                // right: the shell rejects a mismatched frame outright
                // rather than blitting it at the wrong size, so sending
                // one would only cost a protocol error.
                if tile.pixels.len() != pixmap.data().len() {
                    return false;
                }
                pixmap.data_mut().copy_from_slice(&tile.pixels);
                shown = Some(hms);
                true
            },
            // The built-in clock has no click behavior, and neither
            // does this. Left and scroll would arrive here if it did;
            // middle and right belong to the dock.
            input: |_ctx: &dockapp::Ctx, _event| false,
        },
    );

    if let Err(e) = result {
        eprintln!("chonk-dockclock: {e}");
        std::process::exit(1);
    }
}
