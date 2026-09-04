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
//! Redraws are incrementally damaged on both backends: the winit path
//! renders at the EGL surface's real buffer age and the session path
//! trusts the `DrmCompositor`'s per-element tracking, so an idle
//! desktop's only per-frame work is whatever actually changed (a
//! blinking cursor's rectangle, a dock LED). This used to be a
//! deliberate full-frame repaint (`age = 0`, the picom
//! `--no-use-damage` trade); the evidence that retired it is in
//! `session.rs` at the `full_damage_forced` site, and that same
//! environment variable (`CHONKSTEP_FULL_DAMAGE=1`) restores the old
//! behaviour on every path without a compiler. What incremental damage
//! demands of THIS file is stable element identity: every element's id
//! must survive across frames (the buffers' own ids do; solid fills
//! carry `ShellRecord::fill_id`), because the tracker reads a fresh id
//! as "old element gone, new element appeared" and re-damages both.
//! `CHONKSTEP_DAMAGE_LOG=1` prints each drawn frame's damage.
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
//!
//! # Physical pixels everywhere, whatever the outputs advertise
//!
//! The `wl_output`s advertise the session's UI scale so native clients
//! render sharp (see `state.rs`'s `advertised_output_scale`), but the
//! composition here never hears about it: every damage tracker and
//! `DrmCompositor` in this crate is pinned to scale 1, so the scale an
//! element's `Element::geometry(scale)` is asked at is always 1.0 and
//! "logical" equals the device pixels this whole compositor works in.
//! That pin is what lets the chrome — memory buffers the theme already
//! rasterized in device pixels, placed at device-pixel positions — land
//! 1 buffer pixel : 1 screen pixel with no conversion at all.
//!
//! Client surfaces are the one place a second coordinate space leaks
//! in: smithay sizes a `WaylandSurfaceRenderElement` at the surface's
//! *logical* extent (buffer pixels divided by the scale the client
//! committed, or the viewport destination when one is set), times the
//! output scale it is rendered at. Under a scale-1 tracker a 2x
//! client's element would come out at half its buffer, so
//! [`push_surface_tree`] wraps every client element in a
//! [`RescaleRenderElement`] that multiplies it back up by that
//! surface's factor — fractional since fractional-scale-v1
//! (`xdg::committed_surface_scale` corrected by
//! `xdg::effective_surface_scale`), restoring 1 buffer pixel :
//! 1 screen pixel for a client that committed at the output's factor,
//! and a GPU down/upscale for one that committed at some other — which
//! is the size the ledger recorded for it
//! (`xdg::committed_content_size`) and the frame was drawn around.

use std::sync::Mutex;
use std::time::Duration;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::{render_elements_from_surface_tree, WaylandSurfaceRenderElement};
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{Color32F, ImportAll, ImportMem};
use smithay::desktop::utils::{
    send_frames_surface_tree, take_presentation_feedback_surface_tree, OutputPresentationFeedback,
};
use smithay::desktop::PopupManager;
use smithay::input::pointer::{CursorImageAttributes, CursorImageStatus};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::render_elements;
use smithay::utils::{IsAlive, Physical, Point as SPoint, Rectangle as SRect};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use wm_theme_api::{Point, Rect};

use crate::state::{Compositor, Graphics, RootBackground, StackEntry, WaylandBackend, WindowRecord};

render_elements! {
    /// Everything one frame is composed of. The macro generates the
    /// `Element`/`RenderElement` plumbing for the enum so a single
    /// `Vec` can carry client surfaces, decoration/wallpaper/cursor
    /// buffers, and solid fills through one `render_output` call.
    pub SceneElement<R> where R: ImportAll + ImportMem;
    Surface = RescaleRenderElement<WaylandSurfaceRenderElement<R>>,
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
    cursors: &crate::state::CursorSet,
    viewport: Point,
) -> (Vec<SceneElement<GlesRenderer>>, Color32F) {
    let mut elements = Vec::new();
    let clear_color = build_scene_into(
        &mut elements,
        backend,
        renderer,
        pointer_location,
        cursor_status,
        cursors,
        viewport,
    );
    (elements, clear_color)
}

/// Rebuilds a scene in caller-owned storage, retaining the vector's
/// allocation across frames. On-screen rendering uses one instance per
/// output; one-shot offscreen consumers use [`build_scene`] instead.
pub(crate) fn build_scene_into(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    backend: &WaylandBackend,
    renderer: &mut GlesRenderer,
    pointer_location: SPoint<f64, smithay::utils::Logical>,
    cursor_status: &CursorImageStatus,
    cursors: &crate::state::CursorSet,
    viewport: Point,
) -> Color32F {
    // Elements are assembled FRONT to BACK — the damage tracker's
    // convention (first element occludes later ones) — so this walk is
    // the module-doc composition order reversed: cursor, above-shells,
    // override-redirect windows, frames, below-shells, wallpaper.
    elements.clear();

    push_cursor_elements(elements, renderer, backend, pointer_location, cursor_status, cursors, viewport);

    // Input-method candidate windows belong above every application
    // surface (including overlay layers) and below only the pointer.
    // They live in the same ledger the hit-test reads, so the visible
    // popup and the clickable popup can never drift apart.
    push_ime_popups(elements, renderer, backend, viewport);

    // A locked session is a different scene, not a filtered one: only
    // the lock client's surfaces exist, over a black clear. The branch
    // sits here — after the cursor, before anything of the desktop —
    // so no code path below can leak a frame of the scene behind the
    // lock, and every consumer of this function (the on-screen frame,
    // screencopy, the screenshot marker) inherits the same blank. An
    // output the locker has not covered (its surface not committed
    // yet, its process dead, a monitor that just appeared) simply
    // finds no element inside its viewport and clears to black, which
    // is what the protocol demands for the gap.
    if backend.locked {
        for entry in &backend.lock_surfaces {
            if !entry.surface.alive() {
                continue;
            }
            let Some(monitor) = backend.monitors.get(entry.output) else {
                continue;
            };
            let origin = SPoint::<i32, Physical>::from((
                monitor.geometry.pos.x - viewport.x,
                monitor.geometry.pos.y - viewport.y,
            ));
            // A locker commits at whatever scale its output told it —
            // the same effective-factor rule as every other client.
            let factor = crate::xdg::effective_surface_scale(
                crate::xdg::committed_surface_scale(entry.surface.wl_surface()),
                backend.scale_at(monitor.geometry),
            );
            push_surface_tree(
                elements,
                renderer,
                entry.surface.wl_surface(),
                origin,
                factor,
                1.0,
                Kind::Unspecified,
            );
        }
        return Color32F::new(0.0, 0.0, 0.0, 1.0);
    }

    // The `Overlay` layer band beats everything but the cursor —
    // that is what the protocol reserves it for (OSDs, screen
    // annotations) — including the desktop's own dock and menus.
    push_layer_band(elements, renderer, backend, WlrLayer::Overlay, viewport);

    for entry in backend.stacking.iter().rev() {
        if let StackEntry::Shell(id) = entry {
            let Some(record) = backend.shells.get(id) else {
                continue;
            };
            if record.above && record.mapped {
                push_shell_elements(elements, renderer, record, viewport);
            }
        }
    }

    // `Top` layer surfaces (bars, notification daemons) sit below the
    // shell's `above` band on purpose: a mako notification must not
    // cover the menu the user just opened or the dock's tiles, while
    // still floating over every managed window. The input walk in
    // `input.rs::hit_at` slots the band identically.
    push_layer_band(elements, renderer, backend, WlrLayer::Top, viewport);

    // XWayland override-redirect windows (menus, tooltips —
    // `WindowType::Unmanaged`, so they own no frame and no
    // stacking entry) draw above every managed frame, which is
    // where the X server would put a just-mapped override-redirect
    // window in practice.
    for record in backend.windows.values() {
        if record.window_type == wm_core::WindowType::Unmanaged && record.mapped {
            push_window_content(elements, renderer, backend, record.content, record, viewport);
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
            let Some(record) = backend.windows.get(id) else {
                continue;
            };
            if record.mapped {
                push_window_content(elements, renderer, backend, record.content, record, viewport);
            }
        }
        if let StackEntry::Frame(id) = entry {
            let Some(frame) = backend.frames.get(id) else {
                continue;
            };
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
                    push_window_content(elements, renderer, backend, record.content, record, viewport);
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

    // `Bottom` layers ride under the managed windows and over the
    // shell's `below` furniture; `Background` (a wallpaper client like
    // swaybg, should someone run one) under everything but the root
    // wallpaper itself.
    push_layer_band(elements, renderer, backend, WlrLayer::Bottom, viewport);

    for entry in backend.stacking.iter().rev() {
        if let StackEntry::Shell(id) = entry {
            let Some(record) = backend.shells.get(id) else {
                continue;
            };
            if !record.above && record.mapped {
                push_shell_elements(elements, renderer, record, viewport);
            }
        }
    }

    push_layer_band(elements, renderer, backend, WlrLayer::Background, viewport);

    // Root background. A solid color is simply the clear color —
    // with full-frame damage every pixel gets cleared, so no
    // element is needed; a wallpaper image is the bottom-most
    // element over a black clear. The image is painted by the shell
    // at the size of the whole screen (the union of every monitor),
    // so it hangs off the global origin and each output shows its
    // own slice of it — which is what makes one wallpaper span the
    // desktop rather than repeating per monitor.
    let clear_color = match &backend.root_background {
        RootBackground::Color((r, g, b)) => Color32F::new(*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0),
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

    clear_color
}

/// Tells every surface in the rendered scene which frame it just
/// appeared in — clients gate their next commit on these, so a visible
/// surface must hear them while a parked window or policy-hidden layer
/// should sleep. Shared by both backends for exactly that reason.
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
    // While locked, ONLY lock surfaces hear about frames: withholding
    // callbacks from everything else freezes those clients for the
    // duration, which the spec explicitly sanctions and which stops a
    // fullscreen video burning a GPU behind a lock screen. (Their next
    // commit after unlock un-freezes them — commits were never
    // refused, only unanswered.)
    if backend.locked {
        for entry in &backend.lock_surfaces {
            if entry.surface.alive() {
                send_frames_surface_tree(entry.surface.wl_surface(), output, elapsed, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            }
        }
        return;
    }
    // Presented layer surfaces animate too (a bar's clock, mako's
    // timeout fade), popups included. Omarchy layers disabled by the
    // namespace policy stay mapped but are absent from the scene, so
    // withholding callbacks is what makes them actually idle.
    for record in &backend.layers {
        if !backend.layer_presented(record) {
            continue;
        }
        let surface = record.surface.wl_surface();
        send_frames_surface_tree(surface, output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
        for (popup, _) in PopupManager::popups_for_surface(surface) {
            send_frames_surface_tree(popup.wl_surface(), output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
    }
    // A window parked on another workspace is protocol-mapped but did
    // not appear in this frame. Sending its callback would make it
    // render another invisible buffer every time an unrelated visible
    // client produced a frame. The workspace transition frame supplies
    // the first callback when it becomes exposed again.
    for_each_presented_window(backend, |record| {
        if let Some(surface) = record.surface.wl_surface() {
            send_frames_surface_tree(&surface, output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
            for (popup, _) in PopupManager::popups_for_surface(&surface) {
                send_frames_surface_tree(popup.wl_surface(), output, elapsed, Some(Duration::ZERO), |_, _| {
                    Some(output.clone())
                });
            }
        }
    });
    if let CursorImageStatus::Surface(surface) = cursor_status {
        send_frames_surface_tree(surface, output, elapsed, Some(Duration::ZERO), |_, _| Some(output.clone()));
    }
    for popup in &backend.ime_popups {
        if popup.alive() {
            send_frames_surface_tree(popup.wl_surface(), output, elapsed, Some(Duration::ZERO), |_, _| {
                Some(output.clone())
            });
        }
    }
}

/// Drain presentation requests for the surfaces whose primary output
/// is `output`. The ownership choice is deterministic on multi-head:
/// whichever monitor contains the largest part of the owning window
/// wins, with monitor order breaking ties.
pub(crate) fn take_presentation_feedback(
    backend: &WaylandBackend,
    output: &smithay::output::Output,
    output_rect: Rect,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    let mut take_tree = |surface: &WlSurface| {
        take_presentation_feedback_surface_tree(
            surface,
            &mut feedback,
            |_, _| Some(output.clone()),
            |_, _| smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::empty(),
        );
    };

    if backend.locked {
        for entry in &backend.lock_surfaces {
            if entry.surface.alive()
                && backend.monitors.get(entry.output).is_some_and(|monitor| monitor.geometry == output_rect)
            {
                take_tree(entry.surface.wl_surface());
            }
        }
        return feedback;
    }

    for_each_presented_window(backend, |record| {
        if is_primary_rect(backend, record.content, output_rect) {
            if let Some(surface) = record.surface.wl_surface() {
                take_tree(&surface);
                for (popup, _) in PopupManager::popups_for_surface(&surface) {
                    take_tree(popup.wl_surface());
                }
            }
        }
    });
    for record in &backend.layers {
        if !backend.layer_presented(record) || !is_primary_rect(backend, record.geometry, output_rect) {
            continue;
        }
        let surface = record.surface.wl_surface();
        take_tree(surface);
        for (popup, _) in PopupManager::popups_for_surface(surface) {
            take_tree(popup.wl_surface());
        }
    }
    for popup in &backend.ime_popups {
        let Some(parent) = popup.get_parent() else { continue };
        let parent_rect = Rect::new(
            Point::new(parent.location.loc.x, parent.location.loc.y),
            wm_theme_api::Size::new(parent.location.size.w.max(0) as u32, parent.location.size.h.max(0) as u32),
        );
        if popup.alive() && is_primary_rect(backend, parent_rect, output_rect) {
            take_tree(popup.wl_surface());
        }
    }
    feedback
}

/// Visits each application surface tree represented by the current
/// scene exactly once, in linear time. Override-redirect X11 surfaces
/// occupy their renderer-owned top band; managed clients are resolved
/// through their one direct-window or mapped-frame stacking slot.
///
/// This is deliberately a traversal rather than `windows.filter` plus
/// `xdg::window_is_in_scene`: that predicate is ideal for one
/// commit's known owner but scans the stack, so applying it to every
/// window would make per-frame protocol routing quadratic.
fn for_each_presented_window(backend: &WaylandBackend, mut visit: impl FnMut(&WindowRecord)) {
    for record in backend.windows.values() {
        if record.window_type == wm_core::WindowType::Unmanaged && record.mapped && record.surface.alive() {
            visit(record);
        }
    }
    for entry in &backend.stacking {
        let record = match entry {
            StackEntry::Window(window) => backend.windows.get(window),
            StackEntry::Frame(frame) => backend
                .frames
                .get(frame)
                .filter(|record| record.mapped)
                .and_then(|record| backend.windows.get(&record.window)),
            StackEntry::Shell(_) => None,
        };
        if let Some(record) = record.filter(|record| record.mapped && record.surface.alive()) {
            visit(record);
        }
    }
}

fn overlap_area(a: Rect, b: Rect) -> u64 {
    let left = a.pos.x.max(b.pos.x);
    let top = a.pos.y.max(b.pos.y);
    let right = (a.pos.x + a.size.w as i32).min(b.pos.x + b.size.w as i32);
    let bottom = (a.pos.y + a.size.h as i32).min(b.pos.y + b.size.h as i32);
    right.saturating_sub(left) as u64 * bottom.saturating_sub(top) as u64
}

fn is_primary_rect(backend: &WaylandBackend, surface: Rect, candidate: Rect) -> bool {
    backend
        .monitors
        .iter()
        .max_by_key(|monitor| overlap_area(surface, monitor.geometry))
        .is_none_or(|monitor| monitor.geometry == candidate)
}

pub(crate) fn presentation_refresh(output: &smithay::output::Output) -> smithay::wayland::presentation::Refresh {
    output.current_mode().and_then(|mode| u64::try_from(mode.refresh).ok()).filter(|rate| *rate > 0).map_or(
        smithay::wayland::presentation::Refresh::Unknown,
        |millihertz| smithay::wayland::presentation::Refresh::fixed(Duration::from_nanos(1_000_000_000_000 / millihertz)),
    )
}

pub(crate) fn present_now(
    feedback: &mut OutputPresentationFeedback,
    refresh: smithay::wayland::presentation::Refresh,
    flags: smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind,
) {
    static SEQUENCE: AtomicU64 = AtomicU64::new(1);
    feedback.presented(
        smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
        refresh,
        SEQUENCE.fetch_add(1, Ordering::Relaxed),
        flags,
    );
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
    previous < FRAME_FAILURES_BEFORE_THROTTLE || previous.is_multiple_of(100)
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
    let Compositor { wm, graphics, outputs, pointer_location, cursor_status, cursors, start_time, .. } = comp;
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
    let scene_scratch = &mut entry.scene_scratch;

    // Make the EGL surface current before asking its buffer age:
    // `EGL_BUFFER_AGE_EXT` is defined only for the current surface, and
    // asked without this the driver answers `BAD_SURFACE` once per
    // frame. The first bind's borrow ends on this line; the render bind
    // below re-binds, which is a cheap make-current of an
    // already-current context.
    if let Err(error) = winit_backend.bind() {
        if note_frame_failure() {
            tracing::warn!(?error, "could not bind the winit framebuffer; skipping frame");
        }
        return;
    }
    // Real buffer age — the whole point of damage tracking. The age
    // says how many frames old this buffer's contents are, and the
    // tracker then repaints only what changed since; `None` (an EGL
    // stack without `EGL_EXT_buffer_age`) degrades to 0, which is the
    // old always-full-frame behaviour, honestly forced rather than
    // silently assumed. `CHONKSTEP_FULL_DAMAGE=1` forces 0 for the same
    // escape-hatch reason `session.rs` documents.
    let age = if crate::session::full_damage_forced() { 0 } else { winit_backend.buffer_age().unwrap_or(0) };
    let drew = {
        let (renderer, mut framebuffer) = match winit_backend.bind() {
            Ok(bound) => bound,
            Err(error) => {
                if note_frame_failure() {
                    tracing::warn!(?error, "could not bind the winit framebuffer; skipping frame");
                }
                return;
            }
        };

        let clear_color = build_scene_into(
            scene_scratch,
            wm.backend(),
            renderer,
            *pointer_location,
            cursor_status,
            cursors,
            Point::new(0, 0),
        );

        match damage_tracker.render_output(renderer, &mut framebuffer, age, scene_scratch, clear_color) {
            Ok(result) => {
                log_damage(age, result.damage.map(Vec::as_slice));
                result.damage.is_some()
            }
            Err(error) => {
                if note_frame_failure() {
                    tracing::warn!(?error, "render failed; keeping damage for a retry");
                }
                // Release element-owned client buffers on the same
                // boundary the former temporary vector did, while
                // retaining only its allocation for the retry.
                scene_scratch.clear();
                return;
            }
        }
    };
    // The nested backend has no asynchronous page-flip ownership to
    // honor. Do not make reusable storage delay `wl_buffer.release`
    // until some future frame; clear the handles now, exactly where
    // the old per-frame vector was dropped.
    scene_scratch.clear();

    if drew {
        // The buffer holds a complete frame either way (the tracker
        // repainted every stale pixel for this buffer's age), so the
        // full-window swap rect is correct; the damage rects computed
        // above only bounded the GPU work. Handing the host compositor
        // the precise rects would additionally shrink *its* recomposite,
        // but the winit swap's damage is in a different orientation
        // than the tracker's output space under this backend's
        // `Flipped180`, and a wrong rect here shows as smearing on the
        // host — full is the honest choice for a dev backend.
        let size = winit_backend.window_size();
        if let Err(error) = winit_backend.submit(Some(&[SRect::from_size(size)])) {
            if note_frame_failure() {
                tracing::warn!(?error, "swap failed; keeping damage for a retry");
            }
            return;
        }
    }

    note_frame_success();
    let output_rect = wm.backend().monitors.first().map(|monitor| monitor.geometry).unwrap_or_default();
    let mut feedback = take_presentation_feedback(wm.backend(), output, output_rect);
    present_now(
        &mut feedback,
        presentation_refresh(output),
        smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync,
    );
    send_frame_callbacks(wm.backend(), output, cursor_status, start_time.elapsed());
    wm.backend_mut().damage = false;
}

/// Damage-rect telemetry, on when `CHONKSTEP_DAMAGE_LOG` is set: one
/// line per drawn frame with the buffer age it rendered at, how many
/// rects the tracker produced, and their total area. This is the
/// honest measurement the damage work is judged by — an idle desktop
/// must log nothing (no frame at all), a blinking cursor a few hundred
/// square pixels, a fullscreen video the video's rectangle.
pub(crate) fn log_damage(age: usize, damage: Option<&[SRect<i32, Physical>]>) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let enabled = *ENABLED.get_or_init(|| std::env::var_os("CHONKSTEP_DAMAGE_LOG").is_some_and(|value| value != "0"));
    if !enabled {
        return;
    }
    match damage {
        Some(rects) => {
            let area: i64 = rects.iter().map(|rect| rect.size.w as i64 * rect.size.h as i64).sum();
            tracing::info!(age, rects = rects.len(), area, "frame damage");
        }
        None => tracing::info!(age, "frame damage: none (no repaint)"),
    }
}

/// Pushes one layer band's surfaces (front to back — newest record
/// first, so the newest surface in a band draws on top, matching the
/// hit walk), each with its xdg popups floating above it exactly as a
/// managed window's do. Layer surfaces are ordinary client surfaces:
/// they go through [`push_surface_tree`] like every other, or a 2x
/// client's bar would land at half size.
fn push_ime_popups(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    viewport: Point,
) {
    for popup in backend.ime_popups.iter().rev() {
        if !popup.alive() {
            continue;
        }
        let Some(parent) = popup.get_parent() else { continue };
        let root = popup.wl_surface();
        let location = popup.location();
        let global = Point::new(parent.location.loc.x + location.x, parent.location.loc.y + location.y);
        let parent_rect = Rect::new(
            Point::new(parent.location.loc.x, parent.location.loc.y),
            wm_theme_api::Size::new(parent.location.size.w.max(0) as u32, parent.location.size.h.max(0) as u32),
        );
        let factor = crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(root),
            backend.scale_at(parent_rect),
        );
        push_surface_tree(
            elements,
            renderer,
            root,
            SPoint::<i32, Physical>::from((global.x - viewport.x, global.y - viewport.y)),
            factor,
            1.0,
            Kind::Unspecified,
        );
    }
}

fn push_layer_band(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    band: WlrLayer,
    viewport: Point,
) {
    for record in backend.layers.iter().rev() {
        if record.layer != band || !backend.layer_presented(record) {
            continue;
        }
        let surface = record.surface.wl_surface();
        let origin =
            SPoint::<i32, Physical>::from((record.geometry.pos.x - viewport.x, record.geometry.pos.y - viewport.y));
        // Popups above their parent, offsets converted by the parent's
        // committed factor — the identical arithmetic
        // `push_window_content` uses, for the identical reason.
        let factor = crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(surface),
            backend.scale_at(record.geometry),
        );
        for (popup, offset) in PopupManager::popups_for_surface(surface) {
            let popup_surface = popup.wl_surface();
            let popup_factor = crate::xdg::effective_surface_scale(
                crate::xdg::committed_surface_scale(popup_surface),
                backend.scale_at(record.geometry),
            );
            let location = origin
                + SPoint::<i32, Physical>::from((
                    crate::xdg::scale_length(offset.x, factor),
                    crate::xdg::scale_length(offset.y, factor),
                ));
            push_surface_tree(elements, renderer, popup_surface, location, popup_factor, 1.0, Kind::Unspecified);
        }
        push_surface_tree(elements, renderer, surface, origin, factor, 1.0, Kind::Unspecified);
    }
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
            // The record's own stable id: the damage tracker keys
            // element history by it, and this compositor now renders
            // with real buffer ages (see `render_frame_winit` and the
            // session backend's per-element default), so a fresh id per
            // frame would re-damage every never-painted shell surface
            // every frame.
            elements.push(
                SolidColorRenderElement::new(
                    record.fill_id.clone(),
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
    backend: &WaylandBackend,
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
    // which is not the same thing as a scale for the session — see
    // `push_surface_tree` for how that lands 1 buffer pixel on 1
    // screen pixel.
    //
    // Reading the scale from the surface rather than the desktop is
    // what leaves everything else alone. An Xwayland window, or a
    // toolkit that trusts only `wl_output`, commits a 1x buffer,
    // reports 1, and is drawn precisely as before. A session-wide
    // factor would instead stretch those to double size over ledger
    // rectangles that never grew with them.
    //
    // Chrome is not drawn through here — frames and shell surfaces are
    // memory buffers the theme already rasterized in physical pixels,
    // pushed at 1:1 elsewhere in this file — so the two do not have to
    // agree beyond meeting at the same rectangle.
    let factor = backend.window_surface_scale(record);
    for (popup, offset) in PopupManager::popups_for_surface(&surface) {
        // The offset is surface-local to the parent, so it converts by
        // the parent's factor; the popup's tree then renders at the
        // popup's own, because a menu is a separate surface with its own
        // committed scale and the protocol never promises the two match.
        let popup_surface = popup.wl_surface();
        let popup_factor = crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(popup_surface),
            backend.scale_at(content),
        );
        let location = origin
            + SPoint::<i32, Physical>::from((
                crate::xdg::scale_length(offset.x, factor),
                crate::xdg::scale_length(offset.y, factor),
            ));
        push_surface_tree(elements, renderer, popup_surface, location, popup_factor, 1.0, Kind::Unspecified);
    }
    push_surface_tree(elements, renderer, &surface, origin, factor, 1.0, Kind::Unspecified);
}

/// Pushes one wayland surface tree (front to back), drawn so that each
/// of its committed buffer pixels lands on exactly one screen pixel at
/// `location` — the only size at which the tree agrees with the
/// physical rectangle the ledger keeps for it.
///
/// The multiplication has to happen here because smithay's element
/// machinery mixes two spaces (verified against the vendored 0.7
/// source, `element/surface.rs` and `element/memory.rs`): an element's
/// *location* is `Point<f64, Physical>` and passes through untouched,
/// but its *size* is the surface's logical extent — buffer pixels
/// divided by the `wl_surface.set_buffer_scale` the client committed —
/// multiplied at draw time by whatever scale the damage tracker passes
/// to `Element::geometry(scale)`. Every tracker in this crate is pinned
/// to 1.0 so the chrome's device-pixel buffers stay exact (see the
/// module docs), which leaves a 2x client's element at half its buffer.
/// The [`RescaleRenderElement`] wrap multiplies the element's geometry
/// (and its subsurface offsets, which the tree walk left logical by
/// passing 1.0 below) back up by the tree's committed buffer scale,
/// around `location` so the tree's own anchor never moves.
///
/// `render_scale` is 1.0 for the on-screen scene; `capture.rs` passes
/// its thumbnail downscale, matching the tracker it then renders with.
///
/// One factor for the whole tree, passed in by the caller: every site
/// that anchors a tree also measured its rectangle, and the two must
/// multiply by the same number (`WaylandBackend::window_surface_scale`
/// for managed windows, `xdg::effective_surface_scale` compositions for
/// the rest). The factor is fractional since fractional-scale-v1: a
/// client told 1.5 commits a viewport-backed buffer whose ratio *is*
/// 1.5, and an integral-fallback client's 2x buffer is composed at the
/// output's real 1.5 (a GPU downscale — the sharp direction).
/// The protocol permits a subsurface to commit a different scale than
/// its parent, but the offsets between them convert by a single number
/// either way, and no toolkit this desktop runs mixes scales within
/// one window.
pub(crate) fn push_surface_tree(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    surface: &WlSurface,
    location: SPoint<i32, Physical>,
    factor: f64,
    render_scale: f64,
    kind: Kind,
) {
    let tree: Vec<WaylandSurfaceRenderElement<GlesRenderer>> =
        render_elements_from_surface_tree(renderer, surface, location, render_scale, 1.0, kind);
    elements
        .extend(tree.into_iter().map(|element| RescaleRenderElement::from_element(element, location, factor).into()));
}

/// Pushes the pointer's elements, picking the image by what the
/// pointer is over ([`crate::input::pointer_subject`]) rather than by
/// the last `CursorImageStatus` alone: the client-set cursor surface
/// applies only over that client's own content (offset by its hotspot,
/// which smithay stashes in the surface's data map); over our frames
/// the compositor's own arrow — or the resize double-arrow the frame
/// asked for through `Backend::set_frame_cursor` — is drawn; over the
/// desktop and shell surfaces, the arrow. A status is a *statement by
/// a client*, and it outlives the pointer's visit (no client un-sets a
/// cursor on leave; leave means it may not), so trusting it everywhere
/// kept LibreOffice's pointer on screen over the dock and every frame
/// the pointer crossed after leaving it. `Named` cursor shapes also
/// fall back to the arrow — shipping an Xcursor theme loader is not
/// worth it for a nested dev backend, and clients that care set
/// surface cursors.
fn push_cursor_elements(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    location: SPoint<f64, smithay::utils::Logical>,
    status: &CursorImageStatus,
    cursors: &crate::state::CursorSet,
    viewport: Point,
) {
    // The pointer has one position in global space and is pushed into
    // every output's scene; the ones it is not over clip it away. That
    // is the same treatment every other element gets, and it is what
    // makes the cursor cross a monitor boundary without anything
    // tracking which screen it is on.
    let offset = SPoint::<f64, smithay::utils::Logical>::from((viewport.x as f64, viewport.y as f64));
    let global = location;
    let location = location - offset;
    // `Hidden` stays absolute, over every subject: screencopy relies on
    // substituting it to capture a cursorless frame (see
    // `protocols.rs`'s `capture_region`), and a client hiding the
    // pointer over its own video is the one client statement that must
    // not be second-guessed while the pointer is there.
    if matches!(status, CursorImageStatus::Hidden) {
        return;
    }
    let subject = crate::input::pointer_subject(backend, global);
    let sprite = match subject {
        crate::input::PointerSubject::Client => None,
        crate::input::PointerSubject::Frame(Some(edge)) => Some(cursors.for_edge(edge)),
        crate::input::PointerSubject::Frame(None) | crate::input::PointerSubject::Desktop => Some(cursors.arrow()),
    };
    if let Some(sprite) = sprite {
        // The compositor's own image, hotspot-corrected: the resize
        // double-arrows mark their center, not their corner, and
        // drawing them uncorrected puts the visible crosshair half a
        // glyph below-right of the edge the user is aiming at.
        let position = SPoint::<f64, smithay::utils::Logical>::from((
            location.x - sprite.hotspot.0 as f64,
            location.y - sprite.hotspot.1 as f64,
        ));
        match MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            position.to_physical(1.0),
            &sprite.buffer,
            None,
            None,
            None,
            Kind::Cursor,
        ) {
            Ok(element) => elements.push(element.into()),
            Err(error) => tracing::warn!(?error, "failed to import a compositor cursor"),
        }
        return;
    }
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
            // A client cursor is drawn buffer pixel : screen pixel like
            // every other surface (`push_surface_tree`), never
            // multiplied by the UI scale on top. The outputs advertise
            // that scale, so a native client commits its cursor at 2x
            // into a buffer already sized for it; an Xwayland client
            // hears `XCURSOR_SIZE` instead (24 x scale,
            // `chonk_shell::startup::ensure_xcursor_size`) and commits
            // the same pixels at buffer scale 1. Both land the same
            // size. The hotspot is surface-local — the client's own
            // logical units — so it converts by the same committed
            // factor as the pixels it points into, or a 2x cursor
            // would click half its arrow's length away from its tip.
            let cursor_scale = crate::xdg::committed_surface_scale(surface);
            let hotspot_physical =
                SPoint::<f64, Physical>::from((hotspot.x as f64 * cursor_scale, hotspot.y as f64 * cursor_scale));
            let position = (location.to_physical(1.0) - hotspot_physical).to_i32_round();
            push_surface_tree(elements, renderer, surface, position, cursor_scale, 1.0, Kind::Cursor);
        }
        _ => {
            // A client that never set a cursor (or set a `Named` shape)
            // gets the arrow. No hotspot offset and no size override:
            // the arrow's tip is its (0, 0) pixel, and the cursor set
            // has already rasterized the shape at the UI scale from
            // that same origin, so the tip stays under the pointer at
            // every scale. Sizing the element here instead would ask
            // the GLES renderer to filter a 1-bit shape up, blurring
            // the halo the arrow reads against dark windows by.
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                location.to_physical(1.0),
                &cursors.arrow().buffer,
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
