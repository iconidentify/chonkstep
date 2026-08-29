//! The dockapp half of the SDK: run a dock tile from your own process.
//!
//! ```no_run
//! use chonk_ui::dockapp::{self, Handlers};
//!
//! dockapp::run("org.example.clock", Handlers {
//!     draw: |ctx: &dockapp::Ctx, pixmap: &mut dockapp::Pixmap| {
//!         // paint `pixmap` with `ctx.theme()`; return whether anything changed
//!         true
//!     },
//!     input: |_ctx: &dockapp::Ctx, _event| false,
//! })
//! .expect("dock connection");
//! ```
//!
//! # What a dockapp is
//!
//! A separate process that draws one (or a few) dock tiles. It is
//! neither an X client nor a Wayland client: it holds no display
//! connection, so `wl_shm`, `zwlr_screencopy_v1` and
//! `zwlr_foreign_toplevel_management_v1` are *unreachable* rather than
//! denied, and the shell additionally launches it with
//! `WAYLAND_DISPLAY` and `DISPLAY` unset so it cannot open one behind
//! the SDK's back. It sees its own tile size, the scale, the active
//! theme, and pointer events inside its own tile. It does not see other
//! tiles' pixels or events, the window list, or the screen.
//!
//! # What it is not
//!
//! **A dockapp is not sandboxed.** It is a normal process running as
//! you, with your home directory and your network. This boundary
//! protects the desktop's responsiveness and its pixels; it is not a
//! security boundary around your files, and bubblewrap, seccomp and
//! portals are explicitly out of scope. Install a dockapp with the same
//! care you would install any other program.
//!
//! # Why a hung dockapp costs the desktop nothing
//!
//! Worth stating plainly, because it is the entire deliverable of the
//! architecture underneath: if your dockapp deadlocks, the shell does
//! not notice in any way that matters. It never blocks on you. Frames
//! simply stop arriving and your tile keeps showing its last one,
//! dimmed, until the liveness ping gives up and marks it hung. That
//! check exists to tell the *user* something is wrong — not to protect
//! the desktop, which was never at risk. A built-in dock widget that
//! blocked for 3.6 seconds once froze this whole compositor; a dockapp
//! that blocks forever costs one tile.

use std::time::{Duration, Instant};

use chonk_dock_proto::transport::{Seqpacket, ENV_SOCKET, ENV_TOKEN};
use chonk_dock_proto::wire::{ClientMessage, InputEvent, InputMask, ServerMessage, ThemeState};
// `MAX_SCALE` comes from the protocol crate rather than being restated
// here: the codec enforces the same bound on decode
// (`wire::DecodeError::BadFloat`), and two ends that each define their
// own ceiling are two ends that will eventually disagree about it.
use chonk_dock_proto::{handshake, MAX_MESSAGE_BYTES, MAX_SCALE, TOKEN_BYTES};

use crate::model::Theme;

pub use chonk_dock_proto::wire::{Button, GoodbyeReason, InputKind, LogLevel};
/// Re-exported so a dockapp names the same `Pixmap` this SDK compiled
/// against — see [`crate::tiny_skia`].
pub use tiny_skia::Pixmap;

/// How long a dockapp keeps trying to reconnect after the shell goes
/// away, and how the wait grows.
///
/// Ten seconds because the thing being waited out is a *shell restart*
/// — `scripts/restart.sh`, `scripts/update.sh`, or a theme change in a
/// version that still restarts for one — which takes on the order of
/// 100 ms. Ten seconds is a hundredfold margin on a loaded machine; a
/// shell that has not come back by then is not coming back, and the
/// registry will relaunch this process when it does.
///
/// (This doc comment had drifted onto `MAX_SCALE`, which was declared
/// between it and the constant it describes; `MAX_SCALE` moved to
/// `chonk-dock-proto` in Phase 4c and the comment went back where it
/// belongs.)
const RECONNECT_WINDOW: Duration = Duration::from_secs(10);
const RECONNECT_FIRST_DELAY: Duration = Duration::from_millis(100);
const RECONNECT_MAX_DELAY: Duration = Duration::from_secs(1);

/// Default cadence for [`Handlers::draw`]. One second, because that is
/// what every built-in instrument uses and what a tile showing a number
/// needs.
pub const DEFAULT_REDRAW_INTERVAL: Duration = Duration::from_secs(1);

#[derive(Debug)]
pub enum Error {
    /// `CHONKSTEP_DOCK_SOCKET` / `CHONKSTEP_DOCK_TOKEN` missing or
    /// malformed — almost always "this binary was run from a shell
    /// prompt instead of being launched by the dock".
    Environment(String),
    Io(std::io::Error),
    /// The shell declined the connection, or ended it deliberately.
    Refused(GoodbyeReason),
    /// The tile geometry produced a `Pixmap` this machine could not
    /// allocate.
    Geometry { width: u32, height: u32 },
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Environment(what) => write!(f, "{what}"),
            Self::Io(e) => write!(f, "dock connection: {e}"),
            Self::Refused(reason) => write!(f, "the shell closed this dockapp's connection: {reason:?}"),
            Self::Geometry { width, height } => write!(f, "could not allocate a {width}x{height} tile"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// Everything a dockapp is told about its surroundings.
///
/// Handed to both callbacks by reference and rebuilt whenever the shell
/// sends a `ThemeChanged` — which is how a theme switch reaches a
/// dockapp *without restarting it*.
///
/// Deliberately neither `Clone` nor `'static`: it borrows the live dock
/// connection (for [`Ctx::log`]) as a raw descriptor, and a copy that
/// outlived the connection would be a copy holding a stale fd number.
/// Callbacks receive `&Ctx`, which cannot escape the call, so the
/// borrow is confined by the signature rather than by a rule.
#[derive(Debug)]
pub struct Ctx {
    theme: Theme,
    tile_px: u32,
    tile_units: u8,
    scale: f32,
    visible: bool,
    socket: std::os::fd::RawFd,
}

impl Ctx {
    /// The active theme, already scaled by the session's
    /// `CHONKSTEP_SCALE` — pass it straight to `panel::*`, `tile::*` or
    /// `clock::render_clock_tile`.
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// Device pixels along one tile edge, and the width of the pixmap
    /// handed to `draw`.
    pub fn tile_px(&self) -> u32 {
        self.tile_px
    }

    pub fn tile_units(&self) -> u8 {
        self.tile_units
    }

    /// `tile_px * tile_units` — the pixmap's height.
    pub fn height(&self) -> u32 {
        self.tile_px.saturating_mul(u32::from(self.tile_units))
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// `false` while the dock is hidden or this tile is scrolled out of
    /// view. A dockapp should stop sampling as well as stop drawing —
    /// nobody is looking, and a hidden tile polling a device is the
    /// same waste as a visible one.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Says something in the shell's log.
    ///
    /// A dockapp is launched with its stdout and stderr on `/dev/null`
    /// (`spawn::spawn_detached_with_env`), which is the right default
    /// for a program with no terminal but leaves it with nowhere to
    /// report that its sensor disappeared. This is that channel, and it
    /// lands in the same journal as the rest of the desktop.
    ///
    /// Best effort and infallible on purpose: `text` is truncated to
    /// 256 bytes and stripped of control characters by the encoder, and
    /// a send that would block is dropped. A diagnostic that could fail
    /// a tile, or block a redraw, would be worse than no diagnostic.
    pub fn log(&self, level: LogLevel, text: &str) {
        let message = ClientMessage::Log { level, text: text.to_string() };
        if let Ok(bytes) = message.encode() {
            let _ = chonk_dock_proto::transport::send_on(self.socket, &bytes);
        }
    }
}

/// The two callbacks a dockapp provides.
///
/// `draw` paints the tile and returns whether anything changed;
/// returning `false` skips the send entirely, which is what keeps a
/// 1 Hz clock at one message per second instead of one per loop pass.
/// The SDK overrides that on the first frame after a connect or a theme
/// change, when the shell has nothing to show and "unchanged" would
/// mean "blank".
///
/// `input` receives one pointer event in tile-local coordinates and
/// returns whether it wants a repaint. Note the dock reserves middle
/// and right click for itself (reorder and the per-tile menu), so a
/// dockapp only ever sees left and scroll.
pub struct Handlers<D, I> {
    pub draw: D,
    pub input: I,
}

/// Knobs with defaults good enough that [`run`] does not expose them.
#[derive(Clone, Debug)]
pub struct Options {
    /// How many stacked tiles to ask for. The shell refuses a geometry
    /// its transport cannot carry, so this is not unbounded.
    pub tile_units: u8,
    /// How often `draw` is called. This is a *ceiling on effort*, not a
    /// frame rate: a `draw` that returns `false` sends nothing.
    pub redraw_interval: Duration,
    /// Which pointer events to receive at all.
    pub wants: InputMask,
}

impl Default for Options {
    fn default() -> Self {
        Self { tile_units: 1, redraw_interval: DEFAULT_REDRAW_INTERVAL, wants: InputMask::all() }
    }
}

/// Connects to the dock and runs until the shell says goodbye.
///
/// `id` must match the `id` in the `.dockapp` registry file that
/// declared this program, or the shell has no slot to give it.
pub fn run<D, I>(id: &str, handlers: Handlers<D, I>) -> Result<(), Error>
where
    D: FnMut(&Ctx, &mut Pixmap) -> bool,
    I: FnMut(&Ctx, InputEvent) -> bool,
{
    run_with(id, Options::default(), handlers)
}

/// [`run`] with the knobs exposed.
pub fn run_with<D, I>(id: &str, options: Options, mut handlers: Handlers<D, I>) -> Result<(), Error>
where
    D: FnMut(&Ctx, &mut Pixmap) -> bool,
    I: FnMut(&Ctx, InputEvent) -> bool,
{
    let (socket_path, token) = connection_details()?;

    let mut socket = Seqpacket::connect(&socket_path)?;
    let mut state = handshake::client_handshake(&socket, id, options.tile_units, token, options.wants)?;
    let mut visible = true;

    loop {
        match serve(&socket, &state, &options, &mut handlers, visible)? {
            Outcome::Goodbye(GoodbyeReason::Shutdown) => return Ok(()),
            Outcome::Goodbye(reason) => return Err(Error::Refused(reason)),
            Outcome::Retheme(next, still_visible) => {
                // Not a reconnect: the same socket, a new palette. This
                // is the case that makes a theme switch invisible to a
                // dockapp's own state.
                state = next;
                visible = still_visible;
            }
            Outcome::Disconnected => {
                let Some(reconnected) = reconnect(&socket_path) else {
                    return Ok(());
                };
                socket = reconnected;
                state = handshake::client_handshake(&socket, id, options.tile_units, token, options.wants)?;
                // A fresh shell has not told us anything about this
                // tile's visibility yet, and it sends `Welcome` for a
                // tile it intends to show.
                visible = true;
            }
        }
    }
}

/// Reads `CHONKSTEP_DOCK_SOCKET` and `CHONKSTEP_DOCK_TOKEN`.
///
/// Both are set by the shell when it launches a dockapp. The error text
/// says so, because the overwhelmingly likely reason either is missing
/// is that somebody ran the binary from a terminal to see what it does.
fn connection_details() -> Result<(std::path::PathBuf, [u8; TOKEN_BYTES]), Error> {
    let path = std::env::var_os(ENV_SOCKET).ok_or_else(|| {
        Error::Environment(format!("{ENV_SOCKET} is not set: a dockapp is launched by the dock, not run from a shell"))
    })?;
    let token_hex = std::env::var(ENV_TOKEN)
        .map_err(|_| Error::Environment(format!("{ENV_TOKEN} is not set; the dock mints it per slot at launch")))?;
    let token = chonk_dock_proto::transport::token_from_hex(&token_hex)
        .ok_or_else(|| Error::Environment(format!("{ENV_TOKEN} is not 32 hex digits")))?;
    Ok((std::path::PathBuf::from(path), token))
}

enum Outcome {
    Goodbye(GoodbyeReason),
    /// A new palette or geometry on the *same* connection. Carries the
    /// visibility along with it: a theme switch while the dock is
    /// hidden must not make a hidden tile start drawing again.
    Retheme(ThemeState, bool),
    Disconnected,
}

/// One connection's worth of event loop.
///
/// Returns rather than looping forever on a theme change so that the
/// `Ctx` and the pixmap are rebuilt in one place, from one code path,
/// whether the trigger was a fresh connection or a new palette. A
/// dockapp resizing its own tile in-place halfway down a match arm is
/// how "old-size frames blitted at the new size" bugs happen.
/// Whether a tile can actually be drawn at the geometry and scale the
/// shell just sent.
///
/// Split out of [`serve`] so the policy is testable without a socket —
/// the same reason `spawn::apply_env` is a free function over a
/// `Command` rather than logic inside a spawn.
fn check_drawable(state: &ThemeState, tile_units: u8) -> Result<(), Error> {
    // Kept after the codec learned the same rule, not replaced by it.
    // `ThemeState` is a public struct with public fields, so a caller
    // that builds one by hand — a test, a fake shell, a future in-
    // process path — reaches `serve` without passing a decoder at all;
    // and `check_drawable` additionally checks a geometry the codec
    // cannot see, because `tile_units` is this dockapp's own request and
    // never appears in the message.
    //
    // `scale` is the one that actually bites. It reaches
    // `Theme::scaled`, which multiplies every metric in the palette by
    // it, so a NaN silently turns every dimension into NaN and the tile
    // renders as nothing with no error anywhere to explain it. Rejecting
    // rather than clamping is deliberate: a shell sending an unusable
    // scale is one this dockapp cannot correctly draw for, and quietly
    // substituting 1.0 would put a wrongly-sized tile on screen and call
    // it success.
    if !state.scale.is_finite() || state.scale <= 0.0 || state.scale > MAX_SCALE {
        return Err(Error::Geometry { width: state.tile_px, height: 0 });
    }
    // `frame_fits` is the protocol's own predicate, not a second
    // opinion: it decides whether a frame at this geometry can cross the
    // socket at all. A dockapp that allocated a pixmap it could never
    // send would draw happily into a buffer every `Frame` send then
    // rejected as `TooLarge`.
    if !chonk_dock_proto::frame_fits(state.tile_px, tile_units) {
        return Err(Error::Geometry {
            width: state.tile_px,
            height: state.tile_px.saturating_mul(u32::from(tile_units)),
        });
    }
    Ok(())
}

fn serve<D, I>(
    socket: &Seqpacket,
    state: &ThemeState,
    options: &Options,
    handlers: &mut Handlers<D, I>,
    visible: bool,
) -> Result<Outcome, Error>
where
    D: FnMut(&Ctx, &mut Pixmap) -> bool,
    I: FnMut(&Ctx, InputEvent) -> bool,
{
    // Everything in `state` arrived over the socket, and the next two
    // uses of it are an allocation and a float multiplication — the two
    // places where an unvetted number stops being data and starts being
    // behaviour. The codec bounds both now: `tile_px` against
    // `MAX_TILE_PX`, and `scale` against `MAX_SCALE` via
    // `DecodeError::BadFloat` (which Phase 4c added, because the crate
    // is unpublished and a breaking change to `DecodeError` will never
    // again be as cheap as it is today). This is the second lock, at the
    // last point before the values are believed — see `check_drawable`
    // for why it is worth keeping.
    //
    // `scale` is the one that actually bites. It reaches
    // `Theme::scaled`, which multiplies every metric in the palette by
    // it: a NaN silently turns every dimension into NaN and the tile
    // renders as nothing, with no error anywhere to explain it. A
    // negative or absurd value is the same story with a different
    // shape. Rejecting is right rather than clamping — a shell sending
    // an unusable scale is a shell this dockapp cannot correctly draw
    // for, and quietly substituting 1.0 would put a wrongly-sized tile
    // on screen and call it success.
    //
    // `frame_fits` is the protocol's own predicate, not a second
    // opinion: it is what decides whether a frame at this geometry can
    // cross the socket at all. A dockapp that allocated a pixmap it
    // could never send would draw happily into a buffer every `Frame`
    // send then rejected as `TooLarge`.
    check_drawable(state, options.tile_units)?;

    let mut ctx = Ctx {
        theme: theme_from(state),
        tile_px: state.tile_px,
        tile_units: options.tile_units,
        scale: state.scale,
        visible,
        socket: std::os::fd::AsRawFd::as_raw_fd(socket),
    };
    let mut pixmap = Pixmap::new(ctx.tile_px(), ctx.height())
        .ok_or(Error::Geometry { width: ctx.tile_px(), height: ctx.height() })?;

    let mut buffer = vec![0u8; MAX_MESSAGE_BYTES];
    let mut generation: u32 = 0;
    // The shell has nothing to show until the first frame arrives, so
    // the first `draw` is sent whatever it returns.
    let mut must_present = true;
    let mut next_draw = Instant::now();

    loop {
        let now = Instant::now();
        if ctx.visible() && (must_present || now >= next_draw) {
            let changed = (handlers.draw)(&ctx, &mut pixmap);
            if changed || must_present {
                generation = generation.wrapping_add(1);
                present(socket, generation, &ctx, &pixmap)?;
            }
            must_present = false;
            next_draw = now + options.redraw_interval;
        }

        // While hidden, wake only for messages: there is nothing to
        // draw and nobody to draw it for.
        let deadline = if ctx.visible() { next_draw } else { Instant::now() + options.redraw_interval };
        let Some(n) = socket.recv_until(&mut buffer, deadline)? else {
            continue;
        };
        if n == 0 {
            return Ok(Outcome::Disconnected);
        }
        match ServerMessage::decode(&buffer[..n]) {
            Ok(ServerMessage::Welcome(next)) | Ok(ServerMessage::ThemeChanged(next)) => {
                return Ok(Outcome::Retheme(next, ctx.visible));
            }
            Ok(ServerMessage::Input(event)) => {
                if (handlers.input)(&ctx, event) {
                    must_present = true;
                }
            }
            Ok(ServerMessage::Visibility { visible }) => {
                let became_visible = visible && !ctx.visible;
                ctx.visible = visible;
                must_present |= became_visible;
            }
            Ok(ServerMessage::Ping { seq }) => {
                send(socket, &ClientMessage::Pong { seq })?;
            }
            Ok(ServerMessage::Goodbye { reason }) => return Ok(Outcome::Goodbye(reason)),
            Err(e) => {
                // The shell is the trusted end of this socket, so a
                // message that does not decode means the two ends
                // genuinely disagree about the protocol. Continuing
                // would be guessing; drop the connection and let the
                // reconnect path (or the registry) sort it out.
                tracing::warn!(error = %e, "undecodable message from the shell");
                return Ok(Outcome::Disconnected);
            }
        }
    }
}

/// Sends one frame, dropping it if the shell is momentarily behind.
///
/// A `WouldBlock` here means the shell's receive buffer is full, which
/// for a 1 Hz tile means it is busy with something more important than
/// this tile. Dropping is right: the *next* frame supersedes this one
/// anyway, and the shell's own rate limiter would have coalesced them
/// into exactly the same outcome. Blocking, on the other hand, would
/// make a busy compositor into a stalled dockapp for no benefit.
fn present(socket: &Seqpacket, generation: u32, ctx: &Ctx, pixmap: &Pixmap) -> Result<(), Error> {
    let message = ClientMessage::Frame {
        generation,
        width: ctx.tile_px(),
        height: ctx.height(),
        // `Pixmap`'s buffer is already premultiplied RGBA8, top row
        // first, with no row padding — byte-identical to what the wire
        // format and `DecorationBuffer` both want, so this is a copy
        // and not a conversion.
        pixels: pixmap.data().to_vec(),
    };
    send(socket, &message)
}

fn send(socket: &Seqpacket, message: &ClientMessage) -> Result<(), Error> {
    let bytes = message.encode().map_err(|e| Error::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string())))?;
    match socket.send(&bytes) {
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(()),
        Err(e) => Err(Error::Io(e)),
    }
}

/// Retries `connect()` against the stable socket path after the shell
/// goes away.
///
/// The payoff, now that the shell honours it (Phase 4c): **a dockapp
/// survives a theme switch, `scripts/restart.sh` and
/// `scripts/update.sh`** — the outgoing shell leaves it running and
/// hands its token to its replacement, which holds the slot open and
/// readopts the survivor rather than launching a second copy. On the
/// Wayland session that is strictly better than any ordinary client
/// gets: a Wayland client dies with the compositor's socket and there is
/// no SaveSet equivalent to adopt it afterwards.
///
/// Two details of that contract are load-bearing here. The shell waits
/// exactly [`RECONNECT_WINDOW`] for the knock, so shortening this
/// without shortening `dockapp::tile::REJOIN_WINDOW` leaves a hole in
/// the dock and lengthening it launches a second copy. And a restart
/// deliberately sends no `Goodbye` — a bare EOF means "try again", which
/// is why the loop below is entered on EOF and not on
/// `Goodbye { Shutdown }`.
///
/// `None` means the window elapsed: the caller exits, and the shell's
/// registry relaunches the process when it comes back.
fn reconnect(path: &std::path::Path) -> Option<Seqpacket> {
    let deadline = Instant::now() + RECONNECT_WINDOW;
    let mut delay = RECONNECT_FIRST_DELAY;
    while Instant::now() < deadline {
        // Sleeping here is fine in a way it would never be in the
        // shell: this is the dockapp's own process and its own thread,
        // and the only thing a slow retry delays is this one tile.
        std::thread::sleep(delay);
        if let Ok(socket) = Seqpacket::connect(path) {
            tracing::info!("reconnected to the dock");
            return Some(socket);
        }
        delay = (delay * 2).min(RECONNECT_MAX_DELAY);
    }
    tracing::info!("the dock did not come back within {RECONNECT_WINDOW:?}; exiting");
    None
}

/// Resolves the two theme payloads into one `Theme`, scaled.
///
/// Fast path first: a dockapp built against this workspace's `wm-theme`
/// recognizes the id and parses nothing at all, exactly as
/// `startup.rs:96` does. `theme_toml` is the correctness path for a
/// dockapp built against a *different* `wm-theme` — or a session
/// running a user-defined theme with no built-in id — and it is worth
/// the bytes precisely because the fast path silently degrades to the
/// wrong colors otherwise. Neither available means the flagship theme,
/// which is wrong but never blank.
pub fn theme_from(state: &ThemeState) -> Theme {
    let theme = wm_theme::default_theme::theme_by_id(&state.theme_id)
        .or_else(|| match toml::from_str::<Theme>(&state.theme_toml) {
            Ok(theme) => Some(theme),
            Err(e) => {
                if !state.theme_toml.is_empty() {
                    tracing::warn!(error = %e, "could not deserialize the shell's theme; falling back to the default");
                }
                None
            }
        })
        .unwrap_or_else(crate::nextstep_theme);
    theme.scaled(state.scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(theme_id: &str, theme_toml: String) -> ThemeState {
        ThemeState { tile_px: 56, scale: 1.0, theme_id: theme_id.to_string(), theme_toml }
    }

    /// A `ThemeState` at the stock geometry with `scale` replaced — the
    /// field the codec cannot bound and the one that reaches
    /// `Theme::scaled`.
    fn at_scale(scale: f32) -> ThemeState {
        ThemeState { scale, ..state("nextstep-classic", String::new()) }
    }

    #[test]
    fn a_nan_scale_is_refused_rather_than_multiplied_through_the_palette() {
        // The specific trap: NaN propagates silently. Every metric in a
        // scaled theme becomes NaN, the tile draws as nothing, and no
        // error is raised anywhere to say why — which is the worst
        // possible failure for a third-party dockapp author to debug.
        assert!(check_drawable(&at_scale(f32::NAN), 1).is_err());
        assert!(check_drawable(&at_scale(f32::INFINITY), 1).is_err());
        assert!(check_drawable(&at_scale(f32::NEG_INFINITY), 1).is_err());
    }

    #[test]
    fn a_non_positive_or_absurd_scale_is_refused() {
        assert!(check_drawable(&at_scale(0.0), 1).is_err());
        assert!(check_drawable(&at_scale(-2.0), 1).is_err());
        assert!(check_drawable(&at_scale(MAX_SCALE + 0.1), 1).is_err());
    }

    #[test]
    fn every_scale_a_real_desktop_uses_is_accepted() {
        // The bound exists to reject hostile input, not to have an
        // opinion about displays. 1.0 and 2.0 are the stock and HiDPI
        // settings; 1.5 is the fractional case the config documents.
        for scale in [0.5, 1.0, 1.5, 2.0, 3.0, MAX_SCALE] {
            assert!(check_drawable(&at_scale(scale), 1).is_ok(), "scale {scale} should draw");
        }
    }

    #[test]
    fn a_geometry_that_could_never_cross_the_socket_is_refused_before_it_is_allocated() {
        // Deferring to the protocol's own predicate rather than
        // re-deriving a bound here: allocating a pixmap whose frames
        // `Frame` would reject as TooLarge means drawing happily into a
        // buffer nobody ever sees.
        let huge = ThemeState { tile_px: chonk_dock_proto::MAX_TILE_PX, ..at_scale(1.0) };
        assert_eq!(
            check_drawable(&huge, chonk_dock_proto::MAX_TILE_UNITS).is_err(),
            !chonk_dock_proto::frame_fits(chonk_dock_proto::MAX_TILE_PX, chonk_dock_proto::MAX_TILE_UNITS),
            "the SDK must agree with the transport about what fits"
        );
        assert!(check_drawable(&ThemeState { tile_px: 0, ..at_scale(1.0) }, 1).is_err());
        assert!(check_drawable(&at_scale(1.0), 0).is_err(), "a zero-tall tile is not a tile");
    }

    #[test]
    fn a_known_theme_id_is_the_fast_path_and_parses_nothing() {
        // Deliberately paired with garbage TOML: if the id path were
        // not taken first, this would fall through and fail.
        let theme = theme_from(&state("amber-phosphor", "!!! not toml !!!".into()));
        assert_eq!(theme.id, "amber-phosphor");
    }

    #[test]
    fn an_unknown_theme_id_falls_back_to_the_serialized_palette() {
        // The case this exists for: a user-defined theme, or a dockapp
        // built against an older `wm-theme` that has never heard of the
        // id the shell just sent.
        let original = wm_theme::default_theme::theme_by_id("teal-blueprint").unwrap();
        let serialized = toml::to_string(&original).expect("Theme serializes");
        let theme = theme_from(&state("some-future-user-theme", serialized));
        assert_eq!(theme, original.scaled(1.0), "the real palette, not a guess");
    }

    #[test]
    fn neither_payload_usable_means_the_flagship_theme_not_a_blank_tile() {
        let theme = theme_from(&state("no-such-theme", String::new()));
        assert_eq!(theme.id, "nextstep-classic");
        let theme = theme_from(&state("no-such-theme", "garbage = [".into()));
        assert_eq!(theme.id, "nextstep-classic");
    }

    #[test]
    fn the_theme_arrives_already_scaled() {
        // A dockapp that had to remember to call `.scaled()` itself
        // would draw a crisp tile at scale 1 and a blurry one at 2.
        let unscaled = theme_from(&ThemeState { scale: 1.0, ..state("nextstep-classic", String::new()) });
        let doubled = theme_from(&ThemeState { scale: 2.0, ..state("nextstep-classic", String::new()) });
        assert_ne!(unscaled, doubled, "scale must reach the theme");
        assert_eq!(doubled, unscaled.scaled(2.0));
    }

    #[test]
    fn the_context_reports_the_pixmap_geometry_the_shell_will_expect() {
        // Frame dimensions must EQUAL tile_px x (tile_px * tile_units)
        // exactly or the shell rejects the frame, so these two
        // accessors are the contract.
        let ctx = Ctx {
            theme: crate::nextstep_theme(),
            tile_px: 112,
            tile_units: 2,
            scale: 2.0,
            visible: true,
            // Not connected to anything: this test only asks about
            // geometry, and `log` is the only thing that would use it.
            socket: -1,
        };
        assert_eq!(ctx.tile_px(), 112);
        assert_eq!(ctx.height(), 224);
        assert!(chonk_dock_proto::wire::frame_matches_tile(ctx.tile_px(), ctx.height(), 112, 2));
        let pixmap = Pixmap::new(ctx.tile_px(), ctx.height()).unwrap();
        assert_eq!(pixmap.data().len(), (112 * 224 * 4) as usize);
    }

    #[test]
    fn a_missing_socket_variable_says_what_actually_went_wrong() {
        // The overwhelmingly common way to hit this is running the
        // binary from a shell prompt to see what it does.
        if std::env::var_os(ENV_SOCKET).is_some() {
            return; // running inside a live dock; nothing to assert
        }
        let err = connection_details().expect_err("no socket in the environment");
        let text = err.to_string();
        assert!(text.contains(ENV_SOCKET), "{text}");
        assert!(text.contains("launched by the dock"), "{text}");
    }

    #[test]
    fn default_options_ask_for_one_tile_at_one_hertz() {
        let options = Options::default();
        assert_eq!(options.tile_units, 1);
        assert_eq!(options.redraw_interval, Duration::from_secs(1));
        assert!(chonk_dock_proto::frame_fits(56, options.tile_units));
        assert!(chonk_dock_proto::frame_fits(168, options.tile_units), "and at scale 3");
    }
}
