//! Window and output capture: the compositor's answer to "what does
//! that window look like right now".
//!
//! X11 could hand `wm-core` a window's pixels on demand (XGetImage on
//! a live drawable), which is how Alt-Tab thumbnails and miniaturized
//! icon previews are drawn. A compositor cannot answer that
//! synchronously from inside the backend - the renderer lives on the
//! `Compositor`, not on the ledger `wm-core` holds - so the flow is
//! inverted: rendering keeps a small, throttled snapshot per mapped
//! window in its `WindowRecord`, and `Backend::capture_window_image`
//! serves the most recent one. The shell asks for a preview at
//! miniaturize time, by which point a snapshot is already waiting.
//!
//! Both jobs here are the same three steps - build render elements,
//! draw them into an offscreen GLES buffer, read the pixels back -
//! and both run through `GlesRenderer`, which is the renderer on
//! *both* graphics arms, so the nested backend and the DRM session
//! share this code the way they share
//! [`crate::renderer::build_scene`].
//!
//! Three facts about that readback, each of which costs an afternoon
//! to rediscover:
//!
//! - **No vertical flip is needed.** `GlesRenderer::render` bakes a
//!   180-degree flip into its projection, so scene y=0 lands at the
//!   framebuffer's *bottom* in GL's coordinate system; `glReadPixels`
//!   then reads bottom-up and hands back the scene's top row first.
//!   The two cancel, which is exactly why smithay's `GlesMapping`
//!   reports `flipped() == true`. The `Flipped180` transform the winit
//!   output carries is a property of presenting to an EGL *surface*,
//!   not of the scene, so an offscreen capture must not apply it - a
//!   capture that copies the output transform comes out upside down on
//!   the nested backend and correct on hardware, which is the worst
//!   possible failure mode.
//! - **`Fourcc::Abgr8888` is the RGBA byte order**, mapping to
//!   `GL_RGBA`/`GL_UNSIGNED_BYTE`. `Argb8888` is BGRA and silently
//!   swaps red and blue - the same trap `state.rs` documents for the
//!   built-in cursor.
//! - **The pixels come out premultiplied**, because the GLES frame
//!   blends `ONE, ONE_MINUS_SRC_ALPHA` over a cleared buffer. That is
//!   what `DecorationBuffer` means everywhere else in this codebase
//!   and what `tiny_skia::Pixmap::from_vec` expects, so no conversion
//!   pass is needed in either direction.
//!
//! Screenshots are poll-triggered from a marker file, the same
//! mechanism (and for the same reason - no keybinding, no client, no
//! X server required) as the hot-restart marker the dispatch loop
//! polls:
//!
//! ```text
//! touch ~/.local/state/chonkstep/screenshot
//! ```
//!
//! is the whole interface. That writes `screenshot.png` next to the
//! marker; writing a path into the marker instead
//! (`echo /tmp/desk.png > ~/.local/state/chonkstep/screenshot`) sends
//! the PNG there. This is how a developer over SSH sees what a DRM
//! session is actually showing, with no X server and no Wayland client
//! in the picture.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::damage::OutputDamageTracker;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::{Bind, Color32F, ExportMem, Offscreen};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{Buffer as BufferCoords, Physical, Point as SPoint, Rectangle as SRect};
use smithay::utils::{Size as SSize, Transform};

use wm_core::WindowType;
use wm_theme_api::{DecorationBuffer, Point, Size};

use crate::renderer::{build_scene, SceneElement};
use crate::state::{Compositor, Graphics, WlWindowId};

/// Longest edge of a stored snapshot when nobody has asked for more.
/// These feed 56-112px icon tiles and switcher thumbnails, both of
/// which letterbox-scale whatever they are handed, so anything past
/// this is pixels the shell will throw away - a full-resolution copy
/// of every window once a second would be several megabytes of
/// readback per second for no visible gain. Bilinear downscaling
/// happens on the GPU during the capture render, not afterwards on
/// the CPU.
///
/// The Overview is the consumer this cap is *wrong* for - its cards
/// are a third of a monitor wide, and a 256px capture blown up into
/// one turns terminal text into mush - so it hints its card size
/// through `Backend::set_preview_edge` and the pass below captures at
/// that edge instead while the hint stands. See [`due_windows`] for
/// how the hint changes the schedule.
const MAX_SNAPSHOT_EDGE: u32 = 256;

/// Minimum age of a snapshot before changed pixels are taken again. A
/// preview is consumed at miniaturize time and in the switcher,
/// neither of which needs live video; one second keeps an animating
/// thumbnail plausibly current while static windows pay no recurring
/// readback cost.
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(1);

/// How many windows may be captured in one frame. Reading pixels back
/// out of the GPU is a synchronous stall, so a desktop with a dozen
/// windows must not pay a dozen of them in the frame where their
/// snapshots all come due at once. Capping the batch also staggers the
/// fleet permanently: each window is stamped when it is actually
/// captured, so windows that started in lockstep drift into separate
/// frames and stay there.
const MAX_SNAPSHOTS_PER_FRAME: usize = 2;

/// Deadline-aware reader for the screenshot request marker.
///
/// The path is resolved once at startup and absent files are probed at
/// human-control cadence, not once per rendered frame. Keeping this on
/// the compositor also lets an entirely idle session answer a request:
/// [`crate::state::Compositor::dispatch_pending`] services it before
/// deciding whether the scene itself needs another frame.
pub(crate) struct ScreenshotRequestPoller {
    state_dir: Option<PathBuf>,
    marker: Option<PathBuf>,
    home: Option<PathBuf>,
    next_poll: Instant,
}

impl ScreenshotRequestPoller {
    /// Creates a poller whose first pass is immediately due.
    pub(crate) fn new(now: Instant) -> Self {
        Self::with_environment(now, std::env::var_os("XDG_STATE_HOME"), std::env::var_os("HOME"))
    }

    fn with_environment(now: Instant, xdg_state_home: Option<OsString>, home: Option<OsString>) -> Self {
        let state_dir = state_dir(xdg_state_home, home.clone());
        let marker = state_dir.as_ref().map(|dir| dir.join("screenshot"));
        let home = home.filter(|value| !value.is_empty()).map(PathBuf::from);
        Self { state_dir, marker, home, next_poll: now }
    }

    /// Exact next point at which [`Self::poll`] can touch the marker.
    pub(crate) fn next_deadline(&self) -> Instant {
        self.next_poll
    }

    /// Consumes a screenshot request when the polling deadline is due.
    /// A missing marker is the overwhelmingly common `None` path.
    fn poll(&mut self, now: Instant) -> Option<Result<PathBuf, String>> {
        if now < self.next_poll {
            return None;
        }
        self.next_poll = now + chonk_shell::startup::SESSION_REQUEST_POLL_INTERVAL;
        let (Some(state_dir), Some(marker)) = (&self.state_dir, &self.marker) else {
            return None;
        };
        let Ok(request) = std::fs::read_to_string(marker) else {
            return None;
        };
        if let Err(error) = std::fs::remove_file(marker) {
            return Some(Err(format!("could not consume {}: {error}", marker.display())));
        }
        Some(Ok(screenshot_target(&request, state_dir, self.home.as_deref())))
    }
}

/// Refreshes the per-window snapshots used for previews. Called once
/// per rendered frame; the implementation throttles its own work, so
/// this stays affordable at frame rate.
pub(crate) fn refresh_snapshots(comp: &mut Compositor) {
    let now = Instant::now();
    let boost = comp.wm.backend().preview_edge;
    let due = due_windows(comp, now, boost);
    if due.is_empty() {
        return;
    }

    // Disjoint field borrows: the renderer lives in `graphics` and the
    // records live under `wm`, and both are needed at once - the same
    // destructure `renderer::render_frame_winit` does, for the same
    // reason (there is no `&mut self` method that could hand out both).
    let Compositor { wm, graphics, .. } = comp;
    let renderer = graphics_renderer(graphics);
    let edge = boost.unwrap_or(MAX_SNAPSHOT_EDGE);
    let mut boosted_landed = false;
    for (window, surface, size, factor) in due {
        // Stamp before rendering, not after: a window whose capture
        // fails (a client that just died, an unimportable buffer) must
        // wait out the interval like any other, or it retries at frame
        // rate forever.
        if let Some(record) = wm.backend_mut().windows.get_mut(&window) {
            record.snapshot_attempted_at = Some(now);
        }
        let Some(buffer) = snapshot_window(renderer, &surface, size, factor, edge) else {
            continue;
        };
        if let Some(record) = wm.backend_mut().windows.get_mut(&window) {
            record.snapshot = Some(buffer);
            record.snapshot_dirty = false;
            boosted_landed = boost.is_some();
        }
    }
    if boosted_landed {
        // Tell the shell (through `Backend::preview_generation`) that
        // previews fetched before this pass are now beatable - the
        // Overview painted its first frame from the default snapshots
        // and refreshes its cards exactly once when this moves.
        let backend = wm.backend_mut();
        backend.preview_generation = backend.preview_generation.wrapping_add(1);
    }
}

/// Writes the desktop's contents to `path` as a PNG - the session's
/// screenshot path, and the way a headless verifier (CI, a developer
/// over SSH) sees what a DRM session is actually showing.
///
/// One image covers *every* output, because the scene is drawn once at
/// the size of the whole global space (the union of the monitors) with
/// a zero viewport offset - exactly what an output's own render does,
/// minus the per-output translation. That is both the smaller change
/// and the more useful artifact: "what is my desktop showing" rarely
/// means one screen of it, and a per-output screenshot would need the
/// marker interface to grow a way to name which one.
///
/// With monitors of unequal height the union contains a region no
/// output covers; it comes out as the desktop's own background (the
/// clear color) rather than as a hole, which is the least surprising
/// thing a viewer can be handed.
pub(crate) fn capture_output_png(comp: &mut Compositor, path: &Path) -> Result<(), String> {
    let Compositor {
        wm,
        graphics,
        pointer_location,
        cursor_status,
        cursors,
        ..
    } = comp;
    let renderer = graphics_renderer(graphics);
    // The whole global space: one output's size on a single-monitor
    // session, the union bounding box on any other.
    let size = wm.backend().output_size;

    // The same scene the frame about to be drawn will submit - cursor
    // included, because "what is the session showing" includes where
    // the pointer is.
    let (elements, clear_color) = build_scene(
        wm.backend(),
        renderer,
        *pointer_location,
        cursor_status,
        cursors,
        // No viewport offset: the capture *is* the global space, so
        // every element stays at the coordinate the ledger holds it at.
        Point::new(0, 0),
    );
    let buffer = render_offscreen(renderer, &elements, size, 1.0, clear_color)
        .ok_or_else(|| "offscreen render of the desktop failed".to_string())?;
    write_png(buffer, path)
}

/// The GLES renderer behind whichever graphics stack is running —
/// there is always exactly one, which is what makes capture
/// backend-blind: a snapshot taken on hardware and one taken in a
/// nested window go through identical code. `dmabuf.rs` carries the
/// same three lines for the same reason; a shared helper would belong
/// on `Graphics` itself, in `state.rs`.
fn graphics_renderer(graphics: &mut Graphics) -> &mut GlesRenderer {
    match graphics {
        Graphics::Winit(backend) => backend.renderer(),
        Graphics::Session(session) => session.renderer(),
    }
}

/// The windows whose snapshot is stale - at most
/// [`MAX_SNAPSHOTS_PER_FRAME`] of them - with everything the capture
/// needs pulled out of the ledger up front (an owned surface handle
/// and a size) so the render loop needs no further look-ups while the
/// renderer is borrowed. Which windows a truncated batch picks is
/// unspecified (hash order), and it does not need to be fair:
/// everything left over is still due on the very next frame.
///
/// With a `boost` edge hinted (an Overview session is open), the
/// schedule changes shape entirely: every mapped window whose stored
/// snapshot is smaller than the hinted edge would produce is due *now*
/// and the batch is uncapped - the whole point of the hint is to have
/// card-resolution captures on the very next frame, and paying N
/// readbacks once at panel entry is the cost the Overview signed up
/// for. Windows already captured at the hinted edge are not due at
/// all, interval or no interval: the panel freezes its cards at entry
/// (it repaints on state change, not per frame), so keeping N
/// card-sized readbacks ticking behind a static picture would be pure
/// heat. The per-second cadence resumes when the hint clears, and its
/// first pass shrinks each oversized snapshot back to the default cap.
///
/// `Unmanaged` windows are skipped: XWayland override-redirect
/// surfaces are menus and tooltips that own no frame, never
/// miniaturize, and never appear in the switcher, so a preview of one
/// could not be asked for.
fn due_windows(
    comp: &Compositor,
    now: Instant,
    boost: Option<u32>,
) -> Vec<(WlWindowId, WlSurface, Size, f64)> {
    let backend = comp.wm.backend();
    let eligible = backend.windows.iter().filter(|(_, record)| {
        record.mapped && record.window_type != WindowType::Unmanaged && record.surface.alive()
    });
    if let Some(edge) = boost {
        return eligible
            .filter(|(_, record)| {
                let stored = record.snapshot.as_ref().map(|s| Size::new(s.width, s.height));
                needs_upgrade(stored, record.content.size, edge)
            })
            .filter_map(|(window, record)| {
                Some((
                    *window,
                    record.surface.wl_surface()?,
                    record.content.size,
                    backend.window_surface_scale(record),
                ))
            })
            .collect();
    }
    eligible
        .filter(|(_, record)| {
            let stored = record.snapshot.as_ref().map(|s| Size::new(s.width, s.height));
            let stale = record.snapshot_dirty
                || needs_downgrade(stored, record.content.size, MAX_SNAPSHOT_EDGE);
            snapshot_refresh_due(stale, record.snapshot_attempted_at, now)
        })
        .filter_map(|(window, record)| {
            Some((
                *window,
                record.surface.wl_surface()?,
                record.content.size,
                backend.window_surface_scale(record),
            ))
        })
        .take(MAX_SNAPSHOTS_PER_FRAME)
        .collect()
}

/// Whether stale preview pixels may be refreshed without violating
/// the readback throttle. Kept pure so dirty-state and retry timing
/// can be tested without constructing a Wayland compositor.
fn snapshot_refresh_due(stale: bool, attempted_at: Option<Instant>, now: Instant) -> bool {
    stale
        && attempted_at
            .is_none_or(|attempted| now.saturating_duration_since(attempted) >= SNAPSHOT_INTERVAL)
}

/// Whether a stored snapshot is worth retaking at `edge`: it is when
/// there is none, or when the one held is smaller on its long side
/// than a capture at `edge` would come out (never larger - captures
/// don't upscale, so a small window's full-size snapshot is already
/// everything there is). Pure so the boost schedule above is testable
/// without a GPU.
fn needs_upgrade(stored: Option<Size>, source: Size, edge: u32) -> bool {
    let Some((target, _)) = snapshot_target(source, edge) else {
        return false;
    };
    stored.is_none_or(|held| held.w.max(held.h) < target.w.max(target.h))
}

/// Whether a snapshot retained from an Overview boost is larger than
/// the normal preview target and should be replaced once the boost
/// ends. This preserves the bounded-memory policy without waking
/// clean, already-default-sized windows.
fn needs_downgrade(stored: Option<Size>, source: Size, edge: u32) -> bool {
    let (Some(held), Some((target, _))) = (stored, snapshot_target(source, edge)) else {
        return false;
    };
    held.w.max(held.h) > target.w.max(target.h)
}

/// Renders one window's client content into a downscaled RGBA buffer.
///
/// Only the toplevel's own surface tree is drawn - no decoration, and
/// no xdg popups. That is the X11 contract this substitutes for:
/// `XGetImage` on the *client* drawable returns the client's pixels,
/// with the frame and any override-redirect menu being separate
/// windows it never sees.
///
/// `source` is the ledger's content rect, so the capture covers what
/// `wm-core` believes the window occupies rather than what the client
/// most recently committed: a buffer that overruns the content rect
/// (an unacknowledged resize) is cropped to it, and one that falls
/// short leaves the remainder transparent. Both beat rescaling the
/// thumbnail's aspect ratio out from under the shell mid-resize.
fn snapshot_window(
    renderer: &mut GlesRenderer,
    surface: &WlSurface,
    source: Size,
    factor: f64,
    max_edge: u32,
) -> Option<DecorationBuffer> {
    let (size, scale) = snapshot_target(source, max_edge)?;
    tracing::trace!(width = size.w, height = size.h, "capturing window preview");
    // The scene's own constructor, at capture scale: the GPU does the
    // downscale as part of drawing, so there is exactly one code path
    // that turns a wayland surface into pixels — including the
    // per-surface buffer-scale correction, which is what keeps a 2x
    // client's thumbnail filling the content-rect-shaped target
    // instead of its top-left quarter.
    let mut elements: Vec<SceneElement<GlesRenderer>> = Vec::new();
    crate::renderer::push_surface_tree(
        &mut elements,
        renderer,
        surface,
        SPoint::<i32, Physical>::from((0, 0)),
        factor,
        scale,
        Kind::Unspecified,
    );
    if elements.is_empty() {
        // Nothing importable yet (a mapped window whose first buffer
        // has not committed). Leave the previous snapshot in place.
        return None;
    }
    // Transparent clear: whatever the surface tree does not cover
    // stays empty, which the icon tile composites over its well floor
    // and the switcher over its own backing - both better than a black
    // border baked into the thumbnail.
    render_offscreen(renderer, &elements, size, scale, Color32F::new(0.0, 0.0, 0.0, 0.0))
}

/// Draws `elements` into a fresh offscreen texture and downloads the
/// result. `scale` must be the scale the elements were built at - the
/// damage tracker re-derives each element's on-target geometry from
/// it, so a mismatch silently crops or shrinks the capture.
///
/// A fresh [`OutputDamageTracker`] per call, at age 0, is deliberate:
/// the target texture is new and therefore entirely stale, and a
/// tracker carried across calls would compute incremental damage
/// against a buffer that no longer exists and skip drawing outright.
fn render_offscreen(
    renderer: &mut GlesRenderer,
    elements: &[SceneElement<GlesRenderer>],
    size: Size,
    scale: f64,
    clear_color: Color32F,
) -> Option<DecorationBuffer> {
    if size.w == 0 || size.h == 0 {
        return None;
    }
    let width = size.w as i32;
    let height = size.h as i32;

    let mut texture: GlesTexture = match renderer
        .create_buffer(Fourcc::Abgr8888, SSize::<i32, BufferCoords>::from((width, height)))
    {
        Ok(texture) => texture,
        Err(error) => {
            tracing::warn!(?error, width, height, "could not allocate an offscreen capture buffer");
            return None;
        }
    };
    let mut framebuffer = match renderer.bind(&mut texture) {
        Ok(framebuffer) => framebuffer,
        Err(error) => {
            tracing::warn!(?error, "could not bind the offscreen capture buffer");
            return None;
        }
    };

    let mut damage_tracker = OutputDamageTracker::new(
        SSize::<i32, Physical>::from((width, height)),
        scale,
        Transform::Normal,
    );
    if let Err(error) =
        damage_tracker.render_output(renderer, &mut framebuffer, 0, elements, clear_color)
    {
        tracing::warn!(?error, "offscreen capture render failed");
        return None;
    }

    let region = SRect::from_size(SSize::<i32, BufferCoords>::from((width, height)));
    let mapping = match renderer.copy_framebuffer(&framebuffer, region, Fourcc::Abgr8888) {
        Ok(mapping) => mapping,
        Err(error) => {
            tracing::warn!(?error, "could not read back the offscreen capture buffer");
            return None;
        }
    };
    let pixels = match renderer.map_texture(&mapping) {
        Ok(pixels) => pixels.to_vec(),
        Err(error) => {
            tracing::warn!(?error, "could not map the captured pixels");
            return None;
        }
    };
    Some(DecorationBuffer { width: size.w, height: size.h, pixels })
}

/// Encodes a captured buffer as a PNG. `tiny-skia` is already the
/// rasterizer the theme engine draws with and takes premultiplied
/// RGBA8 directly, so this needs no second image stack and no pixel
/// conversion.
fn write_png(buffer: DecorationBuffer, path: &Path) -> Result<(), String> {
    let size = tiny_skia::IntSize::from_wh(buffer.width, buffer.height)
        .ok_or_else(|| format!("captured size {}x{} is not encodable", buffer.width, buffer.height))?;
    let pixmap = tiny_skia::Pixmap::from_vec(buffer.pixels, size)
        .ok_or_else(|| "captured pixels do not match the captured size".to_string())?;
    if let Some(parent) = path.parent() {
        // A relative request resolves under the state directory, which
        // may name a subdirectory that does not exist yet.
        if let Err(error) = std::fs::create_dir_all(parent) {
            return Err(format!("could not create {}: {error}", parent.display()));
        }
    }
    pixmap
        .save_png(path)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

/// Checks the screenshot marker and services it. Called once per
/// compositor dispatch; [`ScreenshotRequestPoller`] bounds filesystem
/// work to ten probes per second regardless of client frame rate.
///
/// The marker is consumed *before* the capture runs, exactly as
/// `state.rs`'s restart marker is: a capture that fails (an
/// unwritable path, a renderer hiccup) must not re-fire forever.
pub(crate) fn poll_screenshot_marker(comp: &mut Compositor) {
    let Some(request) = comp.screenshot_poller.poll(Instant::now()) else { return };
    let target = match request {
        Ok(target) => target,
        Err(error) => {
            tracing::warn!(%error, "screenshot request failed");
            return;
        }
    };
    match capture_output_png(comp, &target) {
        Ok(()) => tracing::info!(path = %target.display(), "screenshot written"),
        Err(error) => tracing::warn!(%error, path = %target.display(), "screenshot failed"),
    }
}

/// chonkstep's state directory, by the same precedence
/// `state.rs`'s theme and restart markers use: `XDG_STATE_HOME` when
/// set, else `~/.local/state`. Takes its environment as arguments so
/// the precedence is testable without mutating process state.
fn state_dir(xdg_state_home: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(root) = xdg_state_home.filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(root).join("chonkstep"));
    }
    home.filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".local/state/chonkstep"))
}

/// Where a marker's contents ask the PNG to go. Empty (the `touch`
/// case) means `screenshot.png` beside the marker; an absolute path is
/// taken as-is; `~/...` expands against `home`; anything else resolves
/// under the state directory rather than the compositor's working
/// directory, which for a session started by a display manager is
/// nowhere the user can predict.
fn screenshot_target(request: &str, state_dir: &Path, home: Option<&Path>) -> PathBuf {
    let request = request.trim();
    if request.is_empty() {
        return state_dir.join("screenshot.png");
    }
    if let Some(rest) = request.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    let path = PathBuf::from(request);
    if path.is_absolute() {
        path
    } else {
        state_dir.join(path)
    }
}

/// The capture size and render scale for a window of `source` size:
/// aspect preserved, longest edge capped at `max_edge`, never scaled
/// up (a 64x64 window is worth exactly 64x64 of thumbnail). `None` for
/// a degenerate window - a zero-sized capture has no valid GL
/// framebuffer and nothing to show.
fn snapshot_target(source: Size, max_edge: u32) -> Option<(Size, f64)> {
    if source.w == 0 || source.h == 0 || max_edge == 0 {
        return None;
    }
    let longest = source.w.max(source.h);
    if longest <= max_edge {
        return Some((source, 1.0));
    }
    let scale = max_edge as f64 / longest as f64;
    // Rounding can take a very thin window's short edge to zero;
    // clamping to one pixel keeps the buffer allocatable and the
    // aspect ratio as close as a pixel grid allows.
    let width = ((source.w as f64 * scale).round() as u32).max(1);
    let height = ((source.h as f64 * scale).round() as u32).max(1);
    Some((Size::new(width, height), scale))
}

#[cfg(test)]
mod tests {
    use super::*;

    // The GPU halves of this module (the offscreen render, the
    // readback, the PNG encode of a real frame) are not unit-testable
    // here: they need an EGL context, which needs a graphics device.
    // They are exercised by taking an actual screenshot of a running
    // session - which is precisely the marker interface these tests
    // cover the addressing half of.

    #[test]
    fn a_touched_marker_writes_beside_itself() {
        let dir = Path::new("/home/ada/.local/state/chonkstep");
        assert_eq!(
            screenshot_target("", dir, None),
            PathBuf::from("/home/ada/.local/state/chonkstep/screenshot.png")
        );
        // `touch` leaves an empty file, but an `echo` with nothing to
        // say leaves a newline - both mean "the default place".
        assert_eq!(
            screenshot_target("\n  \n", dir, None),
            PathBuf::from("/home/ada/.local/state/chonkstep/screenshot.png")
        );
    }

    #[test]
    fn an_absolute_request_is_taken_as_written() {
        let dir = Path::new("/state/chonkstep");
        // Trailing newline: `echo path > marker` is the whole gesture.
        assert_eq!(
            screenshot_target("/tmp/desk.png\n", dir, None),
            PathBuf::from("/tmp/desk.png")
        );
    }

    #[test]
    fn a_tilde_request_expands_against_home() {
        let dir = Path::new("/state/chonkstep");
        let home = PathBuf::from("/home/ada");
        assert_eq!(
            screenshot_target("~/shots/desk.png", dir, Some(&home)),
            PathBuf::from("/home/ada/shots/desk.png")
        );
        // With no home to expand against, the request stays relative
        // and lands under the state directory rather than being
        // silently dropped.
        assert_eq!(
            screenshot_target("~/shots/desk.png", dir, None),
            PathBuf::from("/state/chonkstep/~/shots/desk.png")
        );
    }

    #[test]
    fn a_relative_request_resolves_under_the_state_directory() {
        let dir = Path::new("/state/chonkstep");
        assert_eq!(
            screenshot_target("shots/now.png", dir, None),
            PathBuf::from("/state/chonkstep/shots/now.png")
        );
    }

    #[test]
    fn screenshot_markers_are_cached_throttled_and_consumed_once() {
        let root = std::env::temp_dir().join(format!("chonk-screenshot-poller-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("chonkstep");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("screenshot");
        let start = Instant::now();
        let mut poller = ScreenshotRequestPoller::with_environment(start, Some(root.clone().into_os_string()), None);

        std::fs::write(&marker, []).unwrap();
        assert_eq!(poller.poll(start).unwrap().unwrap(), dir.join("screenshot.png"));
        assert!(!marker.exists(), "a delivered request is consumed exactly once");
        assert_eq!(poller.next_deadline(), start + chonk_shell::startup::SESSION_REQUEST_POLL_INTERVAL);

        std::fs::write(&marker, "custom.png\n").unwrap();
        assert!(poller.poll(start + chonk_shell::startup::SESSION_REQUEST_POLL_INTERVAL / 2).is_none());
        assert!(marker.exists(), "an early event leaves the marker waiting for its deadline");
        assert_eq!(
            poller.poll(start + chonk_shell::startup::SESSION_REQUEST_POLL_INTERVAL).unwrap().unwrap(),
            dir.join("custom.png")
        );
        assert!(!marker.exists());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn the_state_directory_prefers_xdg_state_home() {
        assert_eq!(
            state_dir(Some("/xdg/state".into()), Some("/home/ada".into())),
            Some(PathBuf::from("/xdg/state/chonkstep"))
        );
        assert_eq!(
            state_dir(None, Some("/home/ada".into())),
            Some(PathBuf::from("/home/ada/.local/state/chonkstep"))
        );
        // An empty variable is an unset variable here, not a request
        // to write into the filesystem root.
        assert_eq!(
            state_dir(Some("".into()), Some("/home/ada".into())),
            Some(PathBuf::from("/home/ada/.local/state/chonkstep"))
        );
        assert_eq!(state_dir(None, None), None);
    }

    #[test]
    fn a_small_window_is_captured_at_its_own_size() {
        let (size, scale) = snapshot_target(Size::new(200, 100), 256).unwrap();
        assert_eq!(size, Size::new(200, 100));
        assert_eq!(scale, 1.0);
        // Exactly at the cap is still no upscale and no downscale.
        let (size, scale) = snapshot_target(Size::new(256, 128), 256).unwrap();
        assert_eq!(size, Size::new(256, 128));
        assert_eq!(scale, 1.0);
    }

    #[test]
    fn a_large_window_is_capped_on_its_long_edge_with_aspect_kept() {
        let (size, scale) = snapshot_target(Size::new(1024, 768), 256).unwrap();
        assert_eq!(size, Size::new(256, 192));
        assert_eq!(scale, 0.25);

        // Portrait caps on height, and the short edge rounds.
        let (size, _) = snapshot_target(Size::new(1000, 2000), 256).unwrap();
        assert_eq!(size, Size::new(128, 256));

        let (size, _) = snapshot_target(Size::new(300, 100), 256).unwrap();
        assert_eq!(size, Size::new(256, 85));
    }

    #[test]
    fn an_extreme_aspect_ratio_still_yields_an_allocatable_buffer() {
        // 1/5000 of 256 rounds to zero pixels; a zero-height GL
        // framebuffer is not a thing.
        let (size, _) = snapshot_target(Size::new(5000, 1), 256).unwrap();
        assert_eq!(size, Size::new(256, 1));
    }

    #[test]
    fn a_missing_or_undersized_snapshot_wants_the_boosted_edge() {
        // Nothing stored yet: worth taking.
        assert!(needs_upgrade(None, Size::new(2560, 1440), 1200));
        // A default 256-edge snapshot against a 1200-edge target.
        assert!(needs_upgrade(Some(Size::new(256, 144)), Size::new(2560, 1440), 1200));
        // Already at the target: not due, interval or no interval.
        assert!(!needs_upgrade(Some(Size::new(1200, 675)), Size::new(2560, 1440), 1200));
        // Larger than the target (a stale boost from a bigger card
        // set): still not an upgrade.
        assert!(!needs_upgrade(Some(Size::new(1600, 900)), Size::new(2560, 1440), 1200));
    }

    #[test]
    fn a_window_smaller_than_the_boost_is_not_recaptured_past_its_own_size() {
        // A 640x400 window captured whole is all the pixels there are;
        // a 1200 boost must not retake it forever.
        assert!(!needs_upgrade(Some(Size::new(640, 400)), Size::new(640, 400), 1200));
        assert!(needs_upgrade(None, Size::new(640, 400), 1200));
        // And a degenerate window is never due.
        assert!(!needs_upgrade(None, Size::new(0, 400), 1200));
    }

    #[test]
    fn only_stale_snapshots_past_the_throttle_are_refreshed() {
        let now = Instant::now();
        assert!(snapshot_refresh_due(true, None, now));
        assert!(snapshot_refresh_due(true, Some(now - SNAPSHOT_INTERVAL), now));
        assert!(!snapshot_refresh_due(true, Some(now - SNAPSHOT_INTERVAL / 2), now));
        assert!(!snapshot_refresh_due(false, None, now));
        assert!(!snapshot_refresh_due(false, Some(now - SNAPSHOT_INTERVAL * 2), now));
        // A clock sample earlier than the recorded attempt is safely
        // treated as too recent rather than panicking.
        assert!(!snapshot_refresh_due(true, Some(now + SNAPSHOT_INTERVAL), now));
    }

    #[test]
    fn boosted_snapshots_are_downgraded_but_default_ones_are_not() {
        let source = Size::new(2560, 1440);
        assert!(needs_downgrade(Some(Size::new(1200, 675)), source, MAX_SNAPSHOT_EDGE));
        assert!(!needs_downgrade(Some(Size::new(256, 144)), source, MAX_SNAPSHOT_EDGE));
        assert!(!needs_downgrade(None, source, MAX_SNAPSHOT_EDGE));
        // Captures never upscale, so the window's full-size snapshot
        // is already the normal target even below the global cap.
        assert!(!needs_downgrade(Some(Size::new(160, 90)), Size::new(160, 90), MAX_SNAPSHOT_EDGE));
    }

    #[test]
    fn a_degenerate_window_is_not_captured() {
        assert_eq!(snapshot_target(Size::new(0, 600), 256), None);
        assert_eq!(snapshot_target(Size::new(800, 0), 256), None);
        assert_eq!(snapshot_target(Size::new(800, 600), 0), None);
    }
}
