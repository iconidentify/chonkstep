//! Scene composition: turns the [`WaylandBackend`] ledger into GLES
//! render elements and puts a frame on screen.
//!
//! The composition order is fixed, bottom to top: the root background
//! (solid color or wallpaper image), then the ledger's two ordered
//! sequences: `above: false` shell surfaces, the managed `stacking`,
//! and `above: true` shells. Each frame draws
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
//! [`build_scene`] therefore takes the output's global viewport,
//! subtracts its origin from every element it produces, and omits
//! objects whose pixels cannot reach its extent. Ledger rectangles
//! answer the common case in constant time; a surface tree outside one
//! falls back to its exact subsurface bounds so a shadow or popup can
//! still cross a monitor edge. A two-output desktop no longer imports
//! and instantiates every element twice merely to have each damage
//! tracker clip half of them away.
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

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Duration;

use smithay::backend::renderer::element::memory::MemoryRenderBufferRenderElement;
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::surface::WaylandSurfaceRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{Kind, RenderElementStates};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{CommitCounter, RendererSurfaceStateUserData};
use smithay::backend::renderer::{Color32F, ImportAll, ImportMem};
use smithay::desktop::utils::{
    bbox_from_surface_tree, send_frames_surface_tree, take_presentation_feedback_surface_tree,
    surface_presentation_feedback_flags_from_states, OutputPresentationFeedback,
};
use smithay::input::pointer::{CursorImageAttributes, CursorImageStatus};
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::render_elements;
use smithay::utils::{IsAlive, Logical, Physical, Point as SPoint, Rectangle as SRect, Scale};
use smithay::wayland::compositor::{self, with_states, TraversalAction};
use smithay::wayland::shell::wlr_layer::Layer as WlrLayer;
use std::sync::atomic::{AtomicU32, Ordering};

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
/// `viewport` is the output or capture region in global space. Its
/// origin is subtracted from every element so the result is in that
/// target's framebuffer coordinates; its extent lets scene assembly
/// omit objects wholly owned by another output (see the module docs).
/// Called once per output per frame.
pub(crate) fn build_scene(
    backend: &WaylandBackend,
    renderer: &mut GlesRenderer,
    pointer_location: SPoint<f64, smithay::utils::Logical>,
    cursor_status: &CursorImageStatus,
    cursors: &crate::state::CursorSet,
    viewport: Rect,
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
    viewport: Rect,
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
            if overlap_area(monitor.geometry, viewport) == 0 {
                continue;
            }
            let origin = SPoint::<i32, Physical>::from((
                monitor.geometry.pos.x - viewport.pos.x,
                monitor.geometry.pos.y - viewport.pos.y,
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

    // A fullscreen application owns its output's desktop plane. Keep
    // protocol Overlay (OSDs and lock-adjacent surfaces) above it, but
    // suppress ordinary desktop furniture: the shell's dock/menus and
    // layer-shell Top bars. This answer is computed once for this
    // output and shared with the input walk below through the same
    // predicate, so invisible furniture can never keep a click target.
    let desktop_bands_occluded = fullscreen_occludes_desktop_bands(backend, viewport);
    if !desktop_bands_occluded {
        for id in backend.shell_stacking.iter().rev() {
            let Some(record) = backend.shells.get(id) else {
                continue;
            };
            if record.above && record.mapped {
                push_shell_elements(elements, renderer, record, viewport);
            }
        }

        // `Top` layer surfaces (bars, notification daemons) sit below
        // the shell's `above` band on purpose: a mako notification
        // must not cover the menu the user just opened or the dock's
        // tiles, while still floating over ordinary managed windows.
        // The input walk in `input.rs::hit_at` slots the band
        // identically.
        push_layer_band(elements, renderer, backend, WlrLayer::Top, viewport);
    }

    // XWayland override-redirect windows (menus, tooltips —
    // `WindowType::Unmanaged`, so they own no frame and no
    // stacking entry) draw above every managed frame, which is
    // where the X server would put a just-mapped override-redirect
    // window in practice.
    for window in backend.scene_index.unmanaged() {
        if let Some(record) = backend.windows.get(&window).filter(|record| record.mapped) {
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
            if record.mapped && backend.scene_index.is_presented(*id) {
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
            if overlap_area(frame.geometry, viewport) == 0 {
                continue;
            }
            for part in &frame.parts {
                let location = SPoint::<f64, Physical>::from((
                    (frame.geometry.pos.x + part.offset.x - viewport.pos.x) as f64,
                    (frame.geometry.pos.y + part.offset.y - viewport.pos.y) as f64,
                ));
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    location,
                    &part.buffer,
                    None,
                    None,
                    None,
                    Kind::Unspecified,
                ) {
                    Ok(element) => elements.push(element.into()),
                    Err(error) => tracing::warn!(?error, "failed to import a decoration buffer"),
                }
            }
            // Preserve the opaque mid-resize/unshade gap that the former
            // window-sized pixel buffer supplied, but as four floats plus a
            // stable id instead of frame-width * frame-height * 4 retained
            // bytes. It is behind both the client content and sparse chrome.
            let geometry = SRect::<i32, Physical>::new(
                (
                    frame.geometry.pos.x - viewport.pos.x,
                    frame.geometry.pos.y - viewport.pos.y,
                )
                    .into(),
                (frame.geometry.size.w as i32, frame.geometry.size.h as i32).into(),
            );
            elements.push(
                SolidColorRenderElement::new(
                    frame.fill_id.clone(),
                    geometry,
                    CommitCounter::default(),
                    Color32F::new(0.0, 0.0, 0.0, 1.0),
                    Kind::Unspecified,
                )
                .into(),
            );
        }
    }

    // `Bottom` layers ride under the managed windows and over the
    // shell's `below` furniture; `Background` (a wallpaper client like
    // swaybg, should someone run one) under everything but the root
    // wallpaper itself.
    push_layer_band(elements, renderer, backend, WlrLayer::Bottom, viewport);

    for id in backend.shell_stacking.iter().rev() {
        let Some(record) = backend.shells.get(id) else {
            continue;
        };
        if !record.above && record.mapped {
            push_shell_elements(elements, renderer, record, viewport);
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
            if overlap_area(Rect::new(Point::new(0, 0), backend.output_size), viewport) == 0 {
                return Color32F::new(0.0, 0.0, 0.0, 1.0);
            }
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                SPoint::<f64, Physical>::from((-viewport.pos.x as f64, -viewport.pos.y as f64)),
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
/// Called from one output's completed presentation boundary. Only
/// surfaces whose owning rectangle selects that output receive the
/// callback, so mixed-refresh heads pace independently.
pub(crate) fn send_frame_callbacks(
    backend: &WaylandBackend,
    output: &smithay::output::Output,
    output_rect: Rect,
    cursor_status: &CursorImageStatus,
    pointer_location: SPoint<f64, Logical>,
    elapsed: Duration,
) {
    let throttle = frame_interval(output);
    let send_tree = |surface: &WlSurface| {
        signal_pacing_barriers(surface);
        send_frames_surface_tree(surface, output, elapsed, throttle, |_, _| Some(output.clone()));
    };
    // While locked, ONLY lock surfaces hear about frames: withholding
    // callbacks from everything else freezes those clients for the
    // duration, which the spec explicitly sanctions and which stops a
    // fullscreen video burning a GPU behind a lock screen. (Their next
    // commit after unlock un-freezes them — commits were never
    // refused, only unanswered.)
    if backend.locked {
        for entry in &backend.lock_surfaces {
            if entry.surface.alive()
                && backend
                    .monitors
                    .get(entry.output)
                    .is_some_and(|monitor| monitor.geometry == output_rect)
            {
                send_tree(entry.surface.wl_surface());
            }
        }
        return;
    }
    // Presented layer surfaces animate too (a bar's clock, mako's
    // timeout fade), popups included. Omarchy layers disabled by the
    // namespace policy stay mapped but are absent from the scene, so
    // withholding callbacks is what makes them actually idle.
    for record in &backend.layers {
        if !backend.layer_presented(record)
            || !is_primary_rect(backend, record.geometry, output_rect)
        {
            continue;
        }
        let surface = record.surface.wl_surface();
        send_tree(surface);
        for (popup, _) in backend.popups_for_surface(surface) {
            send_tree(popup.wl_surface());
        }
    }
    // A window parked on another workspace is protocol-mapped but did
    // not appear in this frame. Sending its callback would make it
    // render another invisible buffer every time an unrelated visible
    // client produced a frame. The workspace transition frame supplies
    // the first callback when it becomes exposed again.
    for_each_presented_window(backend, |record| {
        if is_primary_rect(backend, record.content, output_rect) {
            if let Some(surface) = record.surface.wl_surface() {
                send_tree(&surface);
                for (popup, _) in backend.popups_for_surface(&surface) {
                    send_tree(popup.wl_surface());
                }
            }
        }
    });
    let pointer_location = Point::new(pointer_location.x.floor() as i32, pointer_location.y.floor() as i32);
    if output_rect.contains(pointer_location) {
        if let CursorImageStatus::Surface(surface) = cursor_status {
            send_tree(surface);
        }
    }
    for popup in &backend.ime_popups {
        let Some(parent) = popup.get_parent() else {
            continue;
        };
        let parent_rect = Rect::new(
            Point::new(parent.location.loc.x, parent.location.loc.y),
            wm_theme_api::Size::new(
                parent.location.size.w.max(0) as u32,
                parent.location.size.h.max(0) as u32,
            ),
        );
        if popup.alive() && is_primary_rect(backend, parent_rect, output_rect) {
            send_tree(popup.wl_surface());
        }
    }
}

/// Release a FIFO barrier only once its surface tree has actually appeared at
/// a presentation boundary. Commit timing is clock-driven instead and is
/// serviced by `Compositor::service_surface_pacing`; coupling it to a frame
/// would deadlock the commit whose pixels are waiting behind that timer.
fn signal_pacing_barriers(surface: &WlSurface) {
    compositor::with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_, states, &()| {
            if let Some(barrier) = states
                .cached_state
                .get::<smithay::wayland::fifo::FifoBarrierCachedState>()
                .current()
                .barrier
                .take()
            {
                barrier.signal();
            }
        },
        |_, _, &()| true,
    );
}

/// Drain presentation requests for the surfaces whose primary output
/// is `output`. The ownership choice is deterministic on multi-head:
/// whichever monitor contains the largest part of the owning window
/// wins, with monitor order breaking ties.
pub(crate) fn take_presentation_feedback(
    backend: &WaylandBackend,
    output: &smithay::output::Output,
    output_rect: Rect,
    render_states: Option<&RenderElementStates>,
) -> OutputPresentationFeedback {
    let mut feedback = OutputPresentationFeedback::new(output);
    let mut take_tree = |surface: &WlSurface| {
        take_presentation_feedback_surface_tree(
            surface,
            &mut feedback,
            |_, _| Some(output.clone()),
            |surface, _| {
                render_states.map_or_else(
                    smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::empty,
                    |states| surface_presentation_feedback_flags_from_states(surface, states),
                )
            },
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
                for (popup, _) in backend.popups_for_surface(&surface) {
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
        for (popup, _) in backend.popups_for_surface(surface) {
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
/// scene exactly once. Both bands come from the lifecycle index rather
/// than rediscovering visibility from all windows and all stacking
/// entries on every frame callback and presentation-feedback drain.
fn for_each_presented_window(backend: &WaylandBackend, mut visit: impl FnMut(&WindowRecord)) {
    for window in backend.scene_index.unmanaged() {
        if let Some(record) = backend
            .windows
            .get(&window)
            .filter(|record| record.mapped && record.surface.alive())
        {
            visit(record);
        }
    }
    for window in backend.scene_index.presented() {
        if let Some(record) = backend
            .windows
            .get(&window)
            .filter(|record| record.mapped && record.surface.alive())
        {
            visit(record);
        }
    }
}

fn overlap_area(a: Rect, b: Rect) -> u64 {
    let left = i64::from(a.pos.x.max(b.pos.x));
    let top = i64::from(a.pos.y.max(b.pos.y));
    let right = (i64::from(a.pos.x) + i64::from(a.size.w))
        .min(i64::from(b.pos.x) + i64::from(b.size.w));
    let bottom = (i64::from(a.pos.y) + i64::from(a.size.h))
        .min(i64::from(b.pos.y) + i64::from(b.size.h));
    (right - left).max(0) as u64 * (bottom - top).max(0) as u64
}

/// Whether desktop-level bands must stay behind the focused fullscreen
/// window on `viewport`.
///
/// `viewport` is one output's global rectangle. Testing the active
/// window's content against it makes the policy output-local without a
/// second fullscreen/output ledger: on a two-monitor desktop the other
/// output keeps its own bar and dock. The focused window is the same
/// authoritative value `hyprctl activewindow` reports, and `mapped`
/// excludes minimized, shaded, and parked-workspace content.
pub(crate) fn fullscreen_occludes_desktop_bands(backend: &WaylandBackend, viewport: Rect) -> bool {
    let Some(window) = backend.ewmh.active_window() else {
        return false;
    };
    backend.windows.get(&window).is_some_and(|record| {
        record.fullscreen
            && record.mapped
            && record.surface.alive()
            && fullscreen_rect_occludes_viewport(record.content, viewport)
    })
}

fn fullscreen_rect_occludes_viewport(content: Rect, viewport: Rect) -> bool {
    overlap_area(content, viewport) > 0
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

fn frame_interval(output: &smithay::output::Output) -> Option<Duration> {
    output
        .current_mode()
        .and_then(|mode| u64::try_from(mode.refresh).ok())
        .filter(|rate| *rate > 0)
        .map(|millihertz| Duration::from_nanos(1_000_000_000_000 / millihertz))
}

pub(crate) fn present_now(
    feedback: &mut OutputPresentationFeedback,
    refresh: smithay::wayland::presentation::Refresh,
    flags: smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind,
) {
    feedback.presented(
        smithay::utils::Clock::<smithay::utils::Monotonic>::new().now(),
        refresh,
        0,
        flags,
    );
}

pub(crate) fn present_at(
    feedback: &mut OutputPresentationFeedback,
    at: Duration,
    refresh: smithay::wayland::presentation::Refresh,
    sequence: u64,
    flags: smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind,
) {
    feedback.presented::<_, smithay::utils::Monotonic>(at, refresh, sequence, flags);
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
pub(crate) fn render_frame(comp: &mut Compositor) -> bool {
    // A plain screencopy must complete even when it lands on an idle desktop.
    // The coarse damage bit gets us here; forcing age zero below makes the
    // output damage tracker produce the actual presentation that paces it.
    let plain_capture_pending = crate::protocols::plain_capture_pending(&comp.protocols);
    match comp.graphics {
        Graphics::Session(_) => {
            // Snapshot readback is deliberately outside the deadline
            // path. It is synchronous on some drivers; doing it after
            // the flip is queued spends post-submit slack rather than
            // making fresh input miss the vblank we just scheduled for.
            let drew = crate::session::render_frame_session(comp, plain_capture_pending);
            if drew {
                crate::capture::refresh_snapshots(comp);
            }
            drew
        }
        Graphics::Winit(_) => {
            // The nested backend has no vblank clock of its own; keep
            // its existing immediate ordering under the host compositor.
            crate::capture::refresh_snapshots(comp);
            render_frame_winit(comp, plain_capture_pending)
        }
    }
}

fn render_frame_winit(comp: &mut Compositor, plain_capture_pending: bool) -> bool {
    // Disjoint field borrows: the winit backend (renderer +
    // framebuffer) mutates while the ledger is read — both live on
    // `Compositor`, so destructure instead of going through `&mut
    // self` methods.
    let Compositor { wm, graphics, outputs, pointer_location, cursor_status, cursors, start_time, .. } = comp;
    let Graphics::Winit(winit_backend) = graphics else {
        return false;
    };
    // The host window is the one and only output (see `state.rs`'s
    // `run`), so there is nothing to iterate and no viewport to offset
    // by — this arm is the multi-output path's degenerate case, not a
    // second implementation of it.
    let Some(entry) = outputs.first_mut() else {
        return false;
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
        return false;
    }
    // Real buffer age — the whole point of damage tracking. The age
    // says how many frames old this buffer's contents are, and the
    // tracker then repaints only what changed since; `None` (an EGL
    // stack without `EGL_EXT_buffer_age`) degrades to 0, which is the
    // old always-full-frame behaviour, honestly forced rather than
    // silently assumed. `CHONKSTEP_FULL_DAMAGE=1` forces 0 for the same
    // escape-hatch reason `session.rs` documents.
    let age = if crate::session::full_damage_forced() || plain_capture_pending {
        0
    } else {
        winit_backend.buffer_age().unwrap_or(0)
    };
    let render_states = {
        let (renderer, mut framebuffer) = match winit_backend.bind() {
            Ok(bound) => bound,
            Err(error) => {
                if note_frame_failure() {
                    tracing::warn!(?error, "could not bind the winit framebuffer; skipping frame");
                }
                return false;
            }
        };

        let clear_color = build_scene_into(
            scene_scratch,
            wm.backend(),
            renderer,
            *pointer_location,
            cursor_status,
            cursors,
            Rect::new(Point::new(0, 0), entry.size),
        );

        match damage_tracker.render_output(renderer, &mut framebuffer, age, scene_scratch, clear_color) {
            Ok(result) => {
                log_damage(age, result.damage.map(Vec::as_slice));
                result.damage.is_some().then_some(result.states)
            }
            Err(error) => {
                if note_frame_failure() {
                    tracing::warn!(?error, "render failed; keeping damage for a retry");
                }
                // Release element-owned client buffers on the same
                // boundary the former temporary vector did, while
                // retaining only its allocation for the retry.
                scene_scratch.clear();
                return false;
            }
        }
    };
    // The nested backend has no asynchronous page-flip ownership to
    // honor. Do not make reusable storage delay `wl_buffer.release`
    // until some future frame; clear the handles now, exactly where
    // the old per-frame vector was dropped.
    scene_scratch.clear();

    if render_states.is_some() {
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
            return false;
        }
    }

    let Some(render_states) = render_states else {
        wm.backend_mut().damage = false;
        return false;
    };

    note_frame_success();
    let output_rect = wm.backend().monitors.first().map(|monitor| monitor.geometry).unwrap_or_default();
    let mut feedback = take_presentation_feedback(wm.backend(), output, output_rect, Some(&render_states));
    present_now(
        &mut feedback,
        presentation_refresh(output),
        smithay::reexports::wayland_protocols::wp::presentation_time::server::wp_presentation_feedback::Kind::Vsync,
    );
    send_frame_callbacks(
        wm.backend(),
        output,
        output_rect,
        cursor_status,
        *pointer_location,
        start_time.elapsed(),
    );
    wm.backend_mut().damage = false;
    true
}

/// Damage-rect telemetry, on when `CHONKSTEP_DAMAGE_LOG` is set: one
/// line per drawn frame with the buffer age it rendered at, how many
/// rects the tracker produced, and their total area. This is the
/// honest measurement the damage work is judged by — an idle desktop
/// must log nothing (no frame at all), a blinking cursor a few hundred
/// square pixels, a fullscreen video the video's rectangle.
pub(crate) fn log_damage(age: usize, damage: Option<&[SRect<i32, Physical>]>) {
    if !crate::diagnostics::enabled("damage-log") {
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
    viewport: Rect,
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
        if !surface_tree_reaches_viewport(
            root,
            global,
            Rect::new(global, wm_theme_api::Size::default()),
            factor,
            viewport,
        ) {
            continue;
        }
        push_surface_tree(
            elements,
            renderer,
            root,
            SPoint::<i32, Physical>::from((global.x - viewport.pos.x, global.y - viewport.pos.y)),
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
    viewport: Rect,
) {
    for record in backend.layers.iter().rev() {
        if record.layer != band || !backend.layer_presented(record) {
            continue;
        }
        let surface = record.surface.wl_surface();
        let origin = SPoint::<i32, Physical>::from((
            record.geometry.pos.x - viewport.pos.x,
            record.geometry.pos.y - viewport.pos.y,
        ));
        // Popups above their parent, offsets converted by the parent's
        // committed factor — the identical arithmetic
        // `push_window_content` uses, for the identical reason.
        let factor = crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(surface),
            backend.scale_at(record.geometry),
        );
        for (popup, offset) in backend.popups_for_surface(surface) {
            let popup_surface = popup.wl_surface();
            let popup_factor = crate::xdg::effective_surface_scale(
                crate::xdg::committed_surface_scale(popup_surface),
                backend.scale_at(record.geometry),
            );
            let global = Point::new(
                record.geometry.pos.x.saturating_add(crate::xdg::scale_length(offset.x, factor)),
                record.geometry.pos.y.saturating_add(crate::xdg::scale_length(offset.y, factor)),
            );
            if !surface_tree_reaches_viewport(
                popup_surface,
                global,
                Rect::new(global, wm_theme_api::Size::default()),
                popup_factor,
                viewport,
            ) {
                continue;
            }
            let location = SPoint::<i32, Physical>::from((
                global.x - viewport.pos.x,
                global.y - viewport.pos.y,
            ));
            push_surface_tree(elements, renderer, popup_surface, location, popup_factor, 1.0, Kind::Unspecified);
        }
        if surface_tree_reaches_viewport(surface, record.geometry.pos, record.geometry, factor, viewport) {
            push_surface_tree(elements, renderer, surface, origin, factor, 1.0, Kind::Unspecified);
        }
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
    viewport: Rect,
) {
    if overlap_area(record.geometry, viewport) == 0 {
        return;
    }
    match &record.buffer {
        Some(buffer) => {
            let location = SPoint::<f64, Physical>::from((
                (record.geometry.pos.x - viewport.pos.x) as f64,
                (record.geometry.pos.y - viewport.pos.y) as f64,
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
                (
                    record.geometry.pos.x - viewport.pos.x,
                    record.geometry.pos.y - viewport.pos.y,
                )
                    .into(),
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
    viewport: Rect,
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
        content.pos.x - viewport.pos.x - record.content_offset.x,
        content.pos.y - viewport.pos.y - record.content_offset.y,
    ));
    let global_origin = Point::new(
        content.pos.x.saturating_sub(record.content_offset.x),
        content.pos.y.saturating_sub(record.content_offset.y),
    );
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
    for (popup, offset) in backend.popups_for_surface(&surface) {
        // The offset is surface-local to the parent, so it converts by
        // the parent's factor; the popup's tree then renders at the
        // popup's own, because a menu is a separate surface with its own
        // committed scale and the protocol never promises the two match.
        let popup_surface = popup.wl_surface();
        let popup_factor = crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(popup_surface),
            backend.scale_at(content),
        );
        let global = Point::new(
            global_origin.x.saturating_add(crate::xdg::scale_length(offset.x, factor)),
            global_origin.y.saturating_add(crate::xdg::scale_length(offset.y, factor)),
        );
        if !surface_tree_reaches_viewport(
            popup_surface,
            global,
            Rect::new(global, wm_theme_api::Size::default()),
            popup_factor,
            viewport,
        ) {
            continue;
        }
        let location = SPoint::<i32, Physical>::from((
            global.x - viewport.pos.x,
            global.y - viewport.pos.y,
        ));
        push_surface_tree(elements, renderer, popup_surface, location, popup_factor, 1.0, Kind::Unspecified);
    }
    if surface_tree_reaches_viewport(&surface, global_origin, content, factor, viewport) {
        push_surface_tree(elements, renderer, &surface, origin, factor, 1.0, Kind::Unspecified);
    }
}

/// Whether a surface tree contributes pixels to this output/capture.
///
/// The ledger rectangle is a cheap answer for the ordinary case. A
/// tree can legally draw beyond it through a drop shadow or subsurface,
/// so a miss falls back to smithay's exact tree bounds rather than
/// incorrectly clipping overflow at an output boundary. The expensive
/// import/element construction then runs only for outputs the tree can
/// actually reach.
fn surface_tree_reaches_viewport(
    surface: &WlSurface,
    global_origin: Point,
    ledger_rect: Rect,
    factor: f64,
    viewport: Rect,
) -> bool {
    if overlap_area(ledger_rect, viewport) > 0 {
        return true;
    }
    let logical = bbox_from_surface_tree(surface, (0, 0));
    let x = global_origin
        .x
        .saturating_add(crate::xdg::scale_length(logical.loc.x, factor));
    let y = global_origin
        .y
        .saturating_add(crate::xdg::scale_length(logical.loc.y, factor));
    let width = crate::xdg::scale_length(logical.size.w, factor).max(0) as u32;
    let height = crate::xdg::scale_length(logical.size.h, factor).max(0) as u32;
    overlap_area(Rect::new(Point::new(x, y), wm_theme_api::Size::new(width, height)), viewport) > 0
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
    // This is smithay's `render_elements_from_surface_tree` traversal
    // with its sink made caller-owned. That helper necessarily returns
    // a fresh Vec, which meant one allocator round trip per visible
    // Wayland tree, per output, per frame even after the outer scene
    // storage became reusable. Pushing the same elements straight into
    // the retained scene keeps ordering and import semantics identical
    // while removing the hidden inner allocation.
    //
    // It is also driven from an explicit stack rather than by recursing
    // on itself, so this walk's stack cost is a constant that does not
    // depend on how deep a client chose to nest its subsurfaces. Depth
    // is bounded at the protocol edge now (`xdg::Compositor::new_subsurface`
    // and `MAX_SUBSURFACE_DEPTH`), which is what protects the recursions
    // inside smithay that cannot be rewritten from here — but the walk
    // this compositor owns should not need that bound to be safe, and
    // does not.
    let tree_origin = location;
    let scale: Scale<f64> = render_scale.into();
    let mut pending = take_walk_stack();
    // The root is carried in hand rather than pushed, so a tree with no
    // subsurfaces at all — nearly every window — touches the stack zero
    // times and its scratch capacity stays at whatever the frame before
    // needed.
    let mut next = Some(TreeStep::Descend(surface.clone(), tree_origin.to_f64()));
    while let Some(step) = next.take().or_else(|| pending.pop()) {
        match step {
            TreeStep::Descend(node, location) => {
                // One level, and one level only: the filter answers for
                // `node` and refuses children to everything else, so
                // upstream's recursion turns around after a single step
                // no matter what the tree looks like below. What comes
                // back is the exact order upstream would have walked in,
                // including `node`'s own place among its children — a
                // subsurface may sit below its parent, and that position
                // lives in a child list this crate cannot read directly.
                let mark = pending.len();
                compositor::with_surface_tree_downward(
                    &node,
                    location,
                    |visited, states, location| {
                        if visited.id() != node.id() {
                            return TraversalAction::SkipChildren;
                        }
                        let mut location = *location;
                        let data = states.data_map.get::<RendererSurfaceStateUserData>();
                        if let Some(data) = data {
                            if let Some(view) = data.lock().unwrap().view() {
                                location += view.offset.to_f64().to_physical(scale);
                                TraversalAction::DoChildren(location)
                            } else {
                                TraversalAction::SkipChildren
                            }
                        } else {
                            TraversalAction::SkipChildren
                        }
                    },
                    |visited, _, location| {
                        pending.push(if visited.id() == node.id() {
                            TreeStep::Draw(node.clone(), *location)
                        } else {
                            TreeStep::Descend(visited.clone(), *location)
                        });
                    },
                    |_, _, _| true,
                );
                // A stack pops backwards; the level was collected
                // forwards. Reversing the segment this level just added
                // makes the pops come out in display order, and leaves
                // the siblings still queued below it untouched.
                pending[mark..].reverse();
            }
            TreeStep::Draw(node, location) => {
                let drawn = with_states(&node, |states| {
                    let mut location = location;
                    let data = states.data_map.get::<RendererSurfaceStateUserData>();
                    let has_view = data
                        .and_then(|data| data.lock().unwrap().view())
                        .map(|view| {
                            location += view.offset.to_f64().to_physical(scale);
                        })
                        .is_some();
                    if !has_view {
                        return None;
                    }
                    Some(WaylandSurfaceRenderElement::from_surface(
                        renderer, &node, states, location, 1.0, kind,
                    ))
                });
                match drawn {
                    Some(Ok(Some(element))) => elements.push(
                        RescaleRenderElement::from_element(element, tree_origin, factor).into(),
                    ),
                    Some(Ok(None)) | None => {}
                    Some(Err(error)) => tracing::warn!(%error, "failed to import a Wayland surface"),
                }
            }
        }
    }
    return_walk_stack(pending);
}

/// One entry of [`push_surface_tree`]'s explicit walk.
enum TreeStep {
    /// Expand this surface's own level: its children, and its own
    /// position among them.
    Descend(WlSurface, SPoint<f64, Physical>),
    /// Emit this surface's element, at the location the walk resolved
    /// for it.
    Draw(WlSurface, SPoint<f64, Physical>),
}

thread_local! {
    /// Scratch for the walk above, so replacing a recursion with a
    /// stack does not reintroduce the per-tree allocation the function
    /// was written to remove. Only the capacity is retained: the stack
    /// is emptied before it is parked, so no `WlSurface` outlives the
    /// frame that walked it.
    ///
    /// Taken rather than borrowed across the walk. `push_surface_tree`
    /// is not reentrant and runs on the repaint thread, but a `take`
    /// costs a pointer swap and makes that a fact about performance
    /// instead of a `RefCell` panic waiting for the day it stops being
    /// true.
    static WALK_STACK: RefCell<Vec<TreeStep>> = const { RefCell::new(Vec::new()) };
}

fn take_walk_stack() -> Vec<TreeStep> {
    WALK_STACK.with(|stack| std::mem::take(&mut *stack.borrow_mut()))
}

fn return_walk_stack(mut stack: Vec<TreeStep>) {
    stack.clear();
    WALK_STACK.with(|slot| *slot.borrow_mut() = stack);
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
pub(crate) fn push_cursor_elements(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    location: SPoint<f64, smithay::utils::Logical>,
    status: &CursorImageStatus,
    cursors: &crate::state::CursorSet,
    viewport: Rect,
) {
    if backend.cursor_hidden {
        return;
    }
    // The pointer has one position in global space. Build it in each
    // output's local coordinates, then retain it only where its pixels
    // intersect; including an off-screen cursor could otherwise defeat
    // direct scanout on a monitor the pointer is nowhere near.
    let offset = SPoint::<f64, smithay::utils::Logical>::from((
        viewport.pos.x as f64,
        viewport.pos.y as f64,
    ));
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
        ))
        .to_physical(1.0);
        if !memory_element_reaches_viewport(position.to_i32_round(), sprite.size, viewport.size) {
            return;
        }
        match MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            position,
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
            let global_position = (global.to_physical(1.0) - hotspot_physical).to_i32_round();
            if surface_tree_reaches_viewport(
                surface,
                Point::new(global_position.x, global_position.y),
                Rect::new(Point::new(global_position.x, global_position.y), wm_theme_api::Size::default()),
                cursor_scale,
                viewport,
            ) {
                let position = global_position
                    - SPoint::<i32, Physical>::from((viewport.pos.x, viewport.pos.y));
                push_surface_tree(elements, renderer, surface, position, cursor_scale, 1.0, Kind::Cursor);
            }
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
            let position = location.to_physical(1.0);
            let arrow = cursors.arrow();
            if !memory_element_reaches_viewport(position.to_i32_round(), arrow.size, viewport.size) {
                return;
            }
            match MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                position,
                &arrow.buffer,
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

fn memory_element_reaches_viewport(
    location: SPoint<i32, Physical>,
    size: wm_theme_api::Size,
    viewport: wm_theme_api::Size,
) -> bool {
    let rect = Rect::new(
        Point::new(location.x, location.y),
        size,
    );
    overlap_area(rect, Rect::new(Point::new(0, 0), viewport)) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use wm_theme_api::Size;

    #[test]
    fn viewport_overlap_is_half_open_and_overflow_safe() {
        let output = Rect::new(Point::new(100, 50), Size::new(1920, 1080));
        assert_eq!(overlap_area(output, Rect::new(Point::new(2020, 50), Size::new(20, 20))), 0);
        assert_eq!(overlap_area(output, Rect::new(Point::new(2019, 1129), Size::new(20, 20))), 1);
        assert_eq!(
            overlap_area(
                Rect::new(Point::new(i32::MAX - 4, i32::MAX - 4), Size::new(u32::MAX, u32::MAX)),
                Rect::new(Point::new(i32::MAX - 2, i32::MAX - 2), Size::new(2, 2)),
            ),
            4,
        );
    }

    #[test]
    fn fullscreen_occlusion_is_confined_to_the_intersected_output() {
        let left = Rect::new(Point::new(0, 0), Size::new(1920, 1080));
        let right = Rect::new(Point::new(1920, 0), Size::new(1920, 1080));
        let fullscreen = Rect::new(Point::new(0, 0), Size::new(1920, 1080));
        assert!(fullscreen_rect_occludes_viewport(fullscreen, left));
        assert!(!fullscreen_rect_occludes_viewport(fullscreen, right));
    }

    #[test]
    fn disjoint_output_admission_removes_cross_output_duplicates() {
        let outputs = [
            Rect::new(Point::new(0, 0), Size::new(1920, 1080)),
            Rect::new(Point::new(1920, 0), Size::new(1920, 1080)),
        ];
        let objects = [
            Rect::new(Point::new(100, 100), Size::new(800, 600)),
            Rect::new(Point::new(2200, 100), Size::new(800, 600)),
        ];
        let admitted = outputs
            .iter()
            .flat_map(|output| objects.iter().filter(move |object| overlap_area(**object, *output) > 0))
            .count();
        assert_eq!(admitted, objects.len(), "each disjoint object belongs to one output, not both");
    }
}
