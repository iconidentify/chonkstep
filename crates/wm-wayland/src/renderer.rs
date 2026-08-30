//! Scene composition: turns the [`WaylandBackend`] ledger into GLES
//! render elements and puts a frame on screen.
//!
//! The composition order is fixed, bottom to top: the root background
//! (solid color or wallpaper image), then the ledger's `stacking`
//! sequence partitioned so `above: false` shell surfaces come before
//! all frames and `above: true` shells after them, each frame drawing
//! its decoration buffer at its geometry with its client's surface
//! tree (and that client's xdg popups) on top at the window's content
//! rect, then XWayland override-redirect windows, then the pointer
//! cursor. That is exactly the stacking the X11 session produces with
//! real windows, reproduced here as a plain ordered walk.
//!
//! A managed window whose client draws its own chrome interleaves with
//! the frames, in the same band and by the same `stacking` order — it
//! is only the decoration buffer it lacks, not a place in the scene.
//!
//! Every redraw damages the full frame (`age = 0` to the damage
//! tracker, full-output submit) rather than tracking per-element
//! damage — correctness first: the X11 side made the same call by
//! running picom with `--no-use-damage`, trading a little GPU fill for
//! never chasing partial-damage artifacts. Revisit only with evidence.
//!
//! # One scene, one output at a time
//!
//! Every rect in the ledger is in *global* space — the coordinate
//! system spanning all monitors, which is the only space `wm-core` and
//! `chonk-shell` know about. A framebuffer, by contrast, always starts
//! at its own (0, 0): both `OutputDamageTracker::render_output` and
//! `DrmCompositor::render_frame` intersect elements against a rectangle
//! anchored at the origin and sized to that output's mode, so an
//! element placed at a global coordinate would be clipped away on every
//! output but the first.
//!
//! [`build_scene`] therefore takes a viewport offset — the output's
//! top-left corner in global space — and subtracts it from every
//! element it produces. Drawing a second monitor is then the same scene
//! built again with a different offset, which is why nothing else in
//! this module (or in `wm-core`, or in the shell) has to learn that
//! more than one screen exists. The nested backend passes a zero
//! offset, so its single output is the case where the subtraction does
//! nothing.

use std::sync::Mutex;
use std::time::Duration;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Color32F, ImportAll, ImportMem};
use smithay::desktop::utils::send_frames_surface_tree;
use smithay::desktop::PopupManager;
use smithay::input::pointer::{CursorImageAttributes, CursorImageStatus};
use smithay::render_elements;
use smithay::utils::{IsAlive, Physical, Point as SPoint, Rectangle as SRect};
use smithay::wayland::compositor::with_states;
use std::sync::atomic::{AtomicU32, Ordering};

use wm_theme_api::{Point, Rect};

use crate::state::{Compositor, Graphics, RootBackground, StackEntry, WaylandBackend};

render_elements! {
    /// Everything one frame is composed of. The macro generates the
    /// `Element`/`RenderElement` plumbing for the enum so a single
    /// `Vec` can carry client surfaces, decoration/wallpaper/cursor
    /// buffers, and solid fills through one `render_output` call.
    pub SceneElement<R> where R: ImportAll + ImportMem;
    Surface = WaylandSurfaceRenderElement<R>,
    Memory = MemoryRenderBufferRenderElement<R>,
    Solid = SolidColorRenderElement,
}

/// Renders one full frame from the current ledger, submits it, sends
/// frame callbacks so clients keep animating, and clears the damage
/// flag. Failures log and leave the damage flag set — the next wakeup
/// simply tries again, which is the only sane recovery for a transient
/// GL hiccup.
/// The scene, front to back (the damage tracker's convention: earlier
/// elements occlude later ones) — and the single definition of what a
/// chonkstep Wayland session looks like. Both backends submit exactly
/// these elements: the nested winit one and the DRM/KMS session render
/// the same desktop because they share this function, not because two
/// implementations were kept in agreement. Returns the elements plus
/// the clear color the root background asks for.
///
/// `viewport` is the top-left corner, in global space, of the output
/// being drawn — subtracted from every element so the result is in that
/// output's framebuffer coordinates (see the module docs). Called once
/// per output per frame; the nested backend passes `(0, 0)`.
pub(crate) fn build_scene(
    backend: &WaylandBackend,
    renderer: &mut GlesRenderer,
    pointer_location: SPoint<f64, smithay::utils::Logical>,
    cursor_status: &CursorImageStatus,
    default_cursor: &smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    viewport: Point,
) -> (Vec<SceneElement<GlesRenderer>>, Color32F) {
    // Elements are assembled FRONT to BACK — the damage tracker's
    // convention (first element occludes later ones) — so this walk is
    // the module-doc composition order reversed: cursor, above-shells,
    // override-redirect windows, frames, below-shells, wallpaper.
    let mut elements: Vec<SceneElement<GlesRenderer>> = Vec::new();

    push_cursor_elements(
        &mut elements,
        renderer,
        pointer_location,
        cursor_status,
        default_cursor,
        viewport,
    );

    for entry in backend.stacking.iter().rev() {
        if let StackEntry::Shell(id) = entry {
            let Some(record) = backend.shells.get(id) else { continue };
            if record.above && record.mapped {
                push_shell_elements(&mut elements, renderer, record, viewport);
            }
        }
    }

    // XWayland override-redirect windows (menus, tooltips —
    // `WindowType::Unmanaged`, so they own no frame and no
    // stacking entry) draw above every managed frame, which is
    // where the X server would put a just-mapped override-redirect
    // window in practice.
    for record in backend.windows.values() {
        if record.window_type == wm_core::WindowType::Unmanaged && record.mapped {
            push_window_content(&mut elements, renderer, record.content, record, viewport);
        }
    }

    for entry in backend.stacking.iter().rev() {
        // A managed window whose client drew its own chrome has no
        // frame and no decoration buffer — just its content, at the
        // depth its own stacking slot gives it. Nothing else in this
        // walk would draw it: the override-redirect pass above keys on
        // `WindowType::Unmanaged`, which such a window is not, and the
        // frame band below reaches clients only through their frames.
        // Skipping it here is what would make Edge and LibreOffice
        // invisible the moment they stop being framed.
        if let StackEntry::Window(id) = entry {
            let Some(record) = backend.windows.get(id) else { continue };
            if record.mapped {
                push_window_content(&mut elements, renderer, record.content, record, viewport);
            }
        }
        if let StackEntry::Frame(id) = entry {
            let Some(frame) = backend.frames.get(id) else { continue };
            if !frame.mapped {
                continue;
            }
            let window = backend.windows.get(&frame.window);
            // Content above chrome: the client's tree first
            // (front-to-back), then the decoration buffer. A
            // shaded window keeps its frame mapped with the
            // content unmapped (`set_client_mapped(false)`), which
            // falls out naturally here.
            if let Some(record) = window {
                if record.mapped {
                    push_window_content(&mut elements, renderer, record.content, record, viewport);
                }
            }
            if let Some(buffer) = &frame.buffer {
                let location = SPoint::<f64, Physical>::from((
                    (frame.geometry.pos.x - viewport.x) as f64,
                    (frame.geometry.pos.y - viewport.y) as f64,
                ));
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location,
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    Ok(element) => elements.push(element.into()),
                    Err(error) => tracing::warn!(?error, "failed to import a decoration buffer"),
                }
            }
        }
    }

    for entry in backend.stacking.iter().rev() {
        if let StackEntry::Shell(id) = entry {
            let Some(record) = backend.shells.get(id) else { continue };
            if !record.above && record.mapped {
                push_shell_elements(&mut elements, renderer, record, viewport);
            }
        }
    }

    // Root background. A solid color is simply the clear color —
    // with full-frame damage every pixel gets cleared, so no
    // element is needed; a wallpaper image is the bottom-most
    // element over a black clear. The image is painted by the shell
    // at the size of the whole screen (the union of every monitor),
    // so it hangs off the global origin and each output shows its
    // own slice of it — which is what makes one wallpaper span the
    // desktop rather than repeating per monitor.
    let clear_color = match &backend.root_background {
        RootBackground::Color((r, g, b)) => {
            Color32F::new(*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0)
        }
        RootBackground::Image(buffer) => {
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                SPoint::<f64, Physical>::from((-viewport.x as f64, -viewport.y as f64)),
                buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(element) => elements.push(element.into()),
                Err(error) => tracing::warn!(?error, "failed to import the wallpaper buffer"),
            }
            Color32F::new(0.0, 0.0, 0.0, 1.0)
        }
    };

    (elements, clear_color)
}

/// Tells every mapped surface which frame it just appeared in —
/// clients gate their next commit on these, so a session that never
/// sends them freezes after one frame. Shared by both backends for
/// exactly that reason.
///
/// Called once per frame with a single `output`, even when several were
/// drawn: a surface is told about the frame, and which output's clock
/// that frame belongs to is the pacing question `session.rs` answers
/// (the primary's) rather than a per-surface one this could answer
/// better without knowing which screens each window is actually on.
pub(crate) fn send_frame_callbacks(
    backend: &WaylandBackend,
    output: &smithay::output::Output,
    cursor_status: &CursorImageStatus,
    elapsed: Duration,
) {
    // Frame callbacks: clients gate their next commit on these, so
    // every mapped window (and its popups, and a client-provided
    // cursor surface) hears about the frame it just appeared in. The
    // throttle mirrors smithay's reference compositors.
    for record in backend.windows.values() {
        if !record.mapped || !record.surface.alive() {
            continue;
        }
        if let Some(surface) = record.surface.wl_surface() {
            send_frames_surface_tree(&surface, output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
            for (popup, _) in PopupManager::popups_for_surface(&surface) {
                send_frames_surface_tree(
                    popup.wl_surface(),
                    output,
                    elapsed,
                    Some(Duration::ZERO),
                    |_, _| Some(output.clone()),
                );
            }
        }
    }
    if let CursorImageStatus::Surface(surface) = cursor_status {
        send_frames_surface_tree(surface, output, elapsed, Some(Duration::ZERO), |_, _| {
            Some(output.clone())
        });
    }

}

/// How many consecutive failed frames are reported before the warning
/// is throttled to one in a hundred.
///
/// Every failure path here keeps the damage flag set so the next pass
/// retries, which is right for a transient fault (a VT switch landing
/// mid-frame) and wrong for a permanent one: a wedged renderer would
/// otherwise write a warning per frame forever, burning a CPU and
/// filling the log of a session whose screen is already black. The
/// count resets on the first frame that succeeds.
const FRAME_FAILURES_BEFORE_THROTTLE: u32 = 5;
static CONSECUTIVE_FRAME_FAILURES: AtomicU32 = AtomicU32::new(0);

/// Records a failed frame and answers whether this one should be
/// logged.
pub(crate) fn note_frame_failure() -> bool {
    let previous = CONSECUTIVE_FRAME_FAILURES.fetch_add(1, Ordering::Relaxed);
    previous < FRAME_FAILURES_BEFORE_THROTTLE || previous % 100 == 0
}

/// Clears the failure streak after a frame reaches the screen.
pub(crate) fn note_frame_success() {
    CONSECUTIVE_FRAME_FAILURES.store(0, Ordering::Relaxed);
}

/// Draws one frame through whichever backend this session is running
/// on. The submission halves differ (a winit swap versus a DRM page
/// flip) and live with their backends; everything visible above them
/// is [`build_scene`].
pub(crate) fn render_frame(comp: &mut Compositor) {
    // Window previews for the switcher and icon tiles, refreshed off
    // the same cadence as drawing and throttled internally.
    crate::capture::refresh_snapshots(comp);
    match comp.graphics {
        Graphics::Session(_) => crate::session::render_frame_session(comp),
        Graphics::Winit(_) => render_frame_winit(comp),
    }
}

fn render_frame_winit(comp: &mut Compositor) {
    // Disjoint field borrows: the winit backend (renderer +
    // framebuffer) mutates while the ledger is read — both live on
    // `Compositor`, so destructure instead of going through `&mut
    // self` methods.
    let Compositor {
        wm,
        graphics,
        outputs,
        pointer_location,
        cursor_status,
        default_cursor,
        start_time,
        ..
    } = comp;
    let Graphics::Winit(winit_backend) = graphics else {
        return;
    };
    // The host window is the one and only output (see `state.rs`'s
    // `run`), so there is nothing to iterate and no viewport to offset
    // by — this arm is the multi-output path's degenerate case, not a
    // second implementation of it.
    let Some(entry) = outputs.first_mut() else {
        return;
    };
    let output = &entry.output;
    let damage_tracker = &mut entry.damage_tracker;

    {
        let (renderer, mut framebuffer) = match winit_backend.bind() {
            Ok(bound) => bound,
            Err(error) => {
                if note_frame_failure() {
                    tracing::warn!(?error, "could not bind the winit framebuffer; skipping frame");
                }
                return;
            }
        };

        let (elements, clear_color) = build_scene(
            wm.backend(),
            renderer,
            *pointer_location,
            cursor_status,
            default_cursor,
            Point::new(0, 0),
        );

        // Age 0 = "assume every pixel stale": the deliberate
        // full-frame redraw described in the module docs.
        if let Err(error) =
            damage_tracker.render_output(renderer, &mut framebuffer, 0, &elements, clear_color)
        {
            if note_frame_failure() {
                tracing::warn!(?error, "render failed; keeping damage for a retry");
            }
            return;
        }
    }

    let size = winit_backend.window_size();
    if let Err(error) = winit_backend.submit(Some(&[SRect::from_size(size)])) {
        if note_frame_failure() {
            tracing::warn!(?error, "swap failed; keeping damage for a retry");
        }
        return;
    }

    note_frame_success();
    send_frame_callbacks(wm.backend(), output, cursor_status, start_time.elapsed());
    wm.backend_mut().damage = false;
}

/// Pushes one shell surface's elements (front to back): its painted
/// buffer, or a solid fill of its background color when it has never
/// been painted — the same visual the X11 backend gets from a fresh
/// window's background pixel before the first blit.
fn push_shell_elements(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    record: &crate::state::ShellRecord,
    viewport: Point,
) {
    match &record.buffer {
        Some(buffer) => {
            let location = SPoint::<f64, Physical>::from((
                (record.geometry.pos.x - viewport.x) as f64,
                (record.geometry.pos.y - viewport.y) as f64,
            ));
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location,
                buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                Ok(element) => elements.push(element.into()),
                Err(error) => tracing::warn!(?error, "failed to import a shell surface buffer"),
            }
        }
        None => {
            let (r, g, b) = record.background;
            let geometry = SRect::<i32, Physical>::new(
                (record.geometry.pos.x - viewport.x, record.geometry.pos.y - viewport.y).into(),
                (record.geometry.size.w as i32, record.geometry.size.h as i32).into(),
            );
            // A fresh Id each frame would normally defeat damage
            // tracking, but full-frame redraws (age 0) never consult
            // element history — see the module docs.
            elements.push(
                SolidColorRenderElement::new(
                    Id::new(),
                    geometry,
                    CommitCounter::default(),
                    Color32F::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0),
                    Kind::Unspecified,
                )
                .into(),
            );
        }
    }
}

/// Pushes one managed window's client content (front to back): its
/// xdg popups above, then the surface tree itself at the content rect.
///
/// Two corrections separate the surface's origin from the content rect,
/// and both used to be assumed away. A client drawing its own chrome
/// wraps its buffer in a drop shadow and says so through
/// `xdg_surface.set_window_geometry`, so the buffer starts up and left
/// of the window (`record.content_offset`). And a client rendering at
/// 2x works in its own logical pixels while this ledger is in physical
/// ones, so everything it reports is its buffer scale times smaller
/// than what is drawn.
fn push_window_content(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    content: Rect,
    record: &crate::state::WindowRecord,
    viewport: Point,
) {
    if !record.surface.alive() {
        return;
    }
    let Some(surface) = record.surface.wl_surface() else {
        return;
    };
    // Drawn up and to the left by the client's own window-geometry
    // offset, so that the *window* lands at `content.pos` rather than
    // the top-left of a buffer that also contains a drop shadow. For a
    // client that declares no geometry this is zero and the expression
    // is the plain one it used to be. See `WindowRecord::content_offset`.
    let origin = SPoint::<i32, Physical>::from((
        content.pos.x - viewport.x - record.content_offset.x,
        content.pos.y - viewport.y - record.content_offset.y,
    ));
    // Each surface is drawn at the buffer scale it itself committed,
    // which is not the same thing as a scale for the session. Smithay
    // divides a surface's buffer by its own `set_buffer_scale` to reach
    // a logical size and then multiplies by whatever is passed here, so
    // handing back that same scale is exactly what puts one buffer
    // pixel on one screen pixel. Passing 1.0 instead is the bug this
    // replaced: LibreOffice, launched with `GDK_SCALE=2`, drew four
    // times the pixels and had them shrunk straight back down, so the
    // whole application sat at half the size of the chrome around it.
    //
    // Reading the scale from the surface rather than the desktop is
    // what leaves everything else alone. An Xwayland window, or a
    // toolkit that trusts only `wl_output` (pinned to scale 1 here),
    // commits a 1x buffer, reports 1, and is drawn precisely as before.
    // A session-wide factor would instead stretch those to double size
    // over ledger rectangles that never grew with them.
    //
    // Chrome is not drawn through here — frames and shell surfaces are
    // memory buffers the theme already rasterized in physical pixels,
    // pushed at 1:1 elsewhere in this file — so the two do not have to
    // agree beyond meeting at the same rectangle.
    let scale = crate::xdg::committed_buffer_scale(&surface);
    for (popup, offset) in PopupManager::popups_for_surface(&surface) {
        // The offset is surface-local to the parent, so it converts by
        // the parent's factor; the popup's tree then renders at the
        // popup's own, because a menu is a separate surface with its own
        // committed scale and the protocol never promises the two match.
        let popup_surface = popup.wl_surface();
        let location = origin
            + SPoint::<i32, Physical>::from((
                crate::xdg::logical_to_physical(offset.x, scale),
                crate::xdg::logical_to_physical(offset.y, scale),
            ));
        elements.extend(render_elements_from_surface_tree(
            renderer,
            popup_surface,
            location,
            crate::xdg::committed_buffer_scale(popup_surface) as f64,
            1.0,
            Kind::Unspecified,
        ));
    }
    elements.extend(render_elements_from_surface_tree(
        renderer,
        &surface,
        origin,
        scale as f64,
        1.0,
        Kind::Unspecified,
    ));
}

/// Pushes the pointer's elements: the client-set cursor surface when
/// one is active (offset by its hotspot, which smithay stashes in the
/// surface's data map), the built-in arrow otherwise. `Named` cursor
/// shapes also fall back to the arrow — shipping an Xcursor theme
/// loader is not worth it for a nested dev backend, and clients that
/// care set surface cursors.
fn push_cursor_elements(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    location: SPoint<f64, smithay::utils::Logical>,
    status: &CursorImageStatus,
    default_cursor: &smithay::backend::renderer::element::memory::MemoryRenderBuffer,
    viewport: Point,
) {
    // The pointer has one position in global space and is pushed into
    // every output's scene; the ones it is not over clip it away. That
    // is the same treatment every other element gets, and it is what
    // makes the cursor cross a monitor boundary without anything
    // tracking which screen it is on.
    let offset =
        SPoint::<f64, smithay::utils::Logical>::from((viewport.x as f64, viewport.y as f64));
    let location = location - offset;
    match status {
        CursorImageStatus::Hidden => {}
        CursorImageStatus::Surface(surface) if surface.alive() => {
            let hotspot = with_states(surface, |states| {
                states
                    .data_map
                    .get::<Mutex<CursorImageAttributes>>()
                    .map(|attrs| attrs.lock().unwrap().hotspot)
                    .unwrap_or_default()
            });
            // A client cursor is drawn 1:1, never multiplied by the UI
            // scale. Outputs here advertise scale 1 (nothing passes a
            // scale to `change_current_state`), so a client's cursor
            // buffer pixels *are* screen pixels; the session tells
            // clients how large to draw through `XCURSOR_SIZE` (24 x
            // scale, `chonk_shell::startup::ensure_xcursor_size`),
            // which is the channel every toolkit actually reads.
            // Scaling here would double-size exactly the clients that
            // already look right. A client that commits a cursor at
            // buffer scale 2 anyway is halved by smithay — and its
            // hotspot, being surface-local, is in those same halved
            // units, so the two stay consistent.
            let position = (location - hotspot.to_f64()).to_physical(1.0).to_i32_round();
            elements.extend(render_elements_from_surface_tree(
                renderer,
                surface,
                position,
                1.0,
                1.0,
                Kind::Cursor,
            ));
        }
        _ => {
            // No hotspot offset and no size override: the arrow's tip
            // is its (0, 0) pixel, and `state::build_default_cursor`
            // has already rasterized the shape at the UI scale from
            // that same origin, so the tip stays under the pointer at
            // every scale. Sizing the element here instead would ask
            // the GLES renderer to filter a 1-bit shape up, blurring
            // the halo the arrow reads against dark windows by.
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location.to_physical(1.0),
                default_cursor,
                None,
                None,
                None,
                Kind::Cursor,
            ) {
                Ok(element) => elements.push(element.into()),
                Err(error) => tracing::warn!(?error, "failed to import the default cursor"),
            }
        }
    }
}
