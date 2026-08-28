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
//! Every redraw damages the full frame (`age = 0` to the damage
//! tracker, full-output submit) rather than tracking per-element
//! damage — correctness first: the X11 side made the same call by
//! running picom with `--no-use-damage`, trading a little GPU fill for
//! never chasing partial-damage artifacts. Revisit only with evidence.

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

use wm_theme_api::Rect;

use crate::state::{Compositor, RootBackground, StackEntry, WaylandBackend};

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
pub(crate) fn render_frame(comp: &mut Compositor) {
    // Disjoint field borrows: the winit backend (renderer +
    // framebuffer) mutates while the ledger is read — both live on
    // `Compositor`, so destructure instead of going through `&mut
    // self` methods.
    let Compositor {
        wm,
        winit_backend,
        damage_tracker,
        output,
        pointer_location,
        cursor_status,
        default_cursor,
        start_time,
        ..
    } = comp;

    {
        let (renderer, mut framebuffer) = match winit_backend.bind() {
            Ok(bound) => bound,
            Err(error) => {
                tracing::warn!(?error, "could not bind the winit framebuffer; skipping frame");
                return;
            }
        };

        let backend = wm.backend();

        // Elements are assembled FRONT to BACK — the damage tracker's
        // convention (first element occludes later ones) — so this
        // walk is the module-doc composition order reversed: cursor,
        // above-shells, override-redirect windows, frames, below-
        // shells, wallpaper.
        let mut elements: Vec<SceneElement<GlesRenderer>> = Vec::new();

        push_cursor_elements(
            &mut elements,
            renderer,
            *pointer_location,
            cursor_status,
            default_cursor,
        );

        for entry in backend.stacking.iter().rev() {
            if let StackEntry::Shell(id) = entry {
                let Some(record) = backend.shells.get(id) else { continue };
                if record.above && record.mapped {
                    push_shell_elements(&mut elements, renderer, record);
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
                push_window_content(&mut elements, renderer, record.content, record);
            }
        }

        for entry in backend.stacking.iter().rev() {
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
                        push_window_content(&mut elements, renderer, record.content, record);
                    }
                }
                if let Some(buffer) = &frame.buffer {
                    let location = SPoint::<f64, Physical>::from((
                        frame.geometry.pos.x as f64,
                        frame.geometry.pos.y as f64,
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
                    push_shell_elements(&mut elements, renderer, record);
                }
            }
        }

        // Root background. A solid color is simply the clear color —
        // with full-frame damage every pixel gets cleared, so no
        // element is needed; a wallpaper image is the bottom-most
        // element over a black clear.
        let clear_color = match &backend.root_background {
            RootBackground::Color((r, g, b)) => {
                Color32F::new(*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0)
            }
            RootBackground::Image(buffer) => {
                match MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    SPoint::<f64, Physical>::from((0.0, 0.0)),
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

        // Age 0 = "assume every pixel stale": the deliberate
        // full-frame redraw described in the module docs.
        if let Err(error) =
            damage_tracker.render_output(renderer, &mut framebuffer, 0, &elements, clear_color)
        {
            tracing::warn!(?error, "render failed; keeping damage for a retry");
            return;
        }
    }

    let size = winit_backend.window_size();
    if let Err(error) = winit_backend.submit(Some(&[SRect::from_size(size)])) {
        tracing::warn!(?error, "swap failed; keeping damage for a retry");
        return;
    }

    // Frame callbacks: clients gate their next commit on these, so
    // every mapped window (and its popups, and a client-provided
    // cursor surface) hears about the frame it just appeared in. The
    // throttle mirrors smithay's reference compositors.
    let elapsed = start_time.elapsed();
    let backend: &WaylandBackend = wm.backend();
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
) {
    match &record.buffer {
        Some(buffer) => {
            let location = SPoint::<f64, Physical>::from((
                record.geometry.pos.x as f64,
                record.geometry.pos.y as f64,
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
                (record.geometry.pos.x, record.geometry.pos.y).into(),
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
/// The tree is drawn at the content origin directly — with server-side
/// decorations enforced via xdg-decoration, clients do not wrap their
/// buffers in shadow margins, so the surface origin and the xdg window
/// geometry origin coincide.
fn push_window_content(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    content: Rect,
    record: &crate::state::WindowRecord,
) {
    if !record.surface.alive() {
        return;
    }
    let Some(surface) = record.surface.wl_surface() else {
        return;
    };
    let origin = SPoint::<i32, Physical>::from((content.pos.x, content.pos.y));
    for (popup, offset) in PopupManager::popups_for_surface(&surface) {
        let location = origin + SPoint::<i32, Physical>::from((offset.x, offset.y));
        elements.extend(render_elements_from_surface_tree(
            renderer,
            popup.wl_surface(),
            location,
            1.0,
            1.0,
            Kind::Unspecified,
        ));
    }
    elements.extend(render_elements_from_surface_tree(
        renderer,
        &surface,
        origin,
        1.0,
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
) {
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
