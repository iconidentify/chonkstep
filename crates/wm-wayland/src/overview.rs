//! Overview is a view of the existing scene, never a screenshot of it. The
//! input-only shell keeps modal routing unchanged. GPU transforms reuse client
//! textures and sparse frame buffers; only small captions own new pixels.

use crate::backend_impl::import_buffer;
use crate::renderer::{push_surface_tree_alpha, SceneElement};
use crate::state::{RootBackground, WaylandBackend, WlFrameId, WlShellId, WlWindowId};
use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::utils::RescaleRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::{gles::GlesRenderer, Color32F};
use smithay::utils::{Physical, Point as SPoint, Rectangle as SRect};
use smithay::wayland::shell::wlr_layer::Layer;
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

struct Label {
    buffer: Option<MemoryRenderBuffer>,
    size: Size,
}

struct Workspace {
    rect: Rect,
    label: Label,
    drop_label: Label,
    close: Option<(Rect, Label)>,
    background: Id,
}
impl Label {
    fn new(buffer: DecorationBuffer) -> Self {
        Self {
            size: Size::new(buffer.width, buffer.height),
            buffer: import_buffer(&buffer, false),
        }
    }
}

pub(crate) struct Window {
    pub window: WlWindowId,
    frame: Option<WlFrameId>,
    pub source: Rect,
    pub destination: Rect,
    label: Label,
    fallback: Option<MemoryRenderBuffer>,
    shadow: Id,
    pub desktop_visible: bool,
    pub draw_content: bool,
    pub sticky: bool,
}

impl Window {
    pub fn snapshot(window: WlWindowId, frame: Option<WlFrameId>, source: Rect, backend: &WaylandBackend) -> Self {
        Self { window, frame, source, destination: source,
            label: Label { buffer: None, size: Size::default() },
            fallback: None, shadow: Id::new(),
            desktop_visible: backend.windows.get(&window).is_some_and(|r| r.mapped),
            draw_content: true, sticky: false }
    }
}

pub(crate) struct Overview {
    pub surface: WlShellId,
    pub geometry: Rect,
    pub windows: Vec<Window>,
    spaces: Vec<Workspace>,
    pub selected: usize,
    pub drag: Option<wm_core::OverviewDrag>,
    /// Absolute desktop-to-Overview fraction, independent of output pixels.
    pub progress: f64,
    paint_order: Vec<usize>,
    workspace: usize,
    gap: u32,
    ring: [Id; 4],
    space_ring: [Id; 4],
    drop_ring: [Id; 4],
    drop_fill: Id,
    band: Id,
    backdrop: Id,
}

impl Overview {
    pub fn token(&self) -> &Id { &self.backdrop }
    pub fn new(
        surface: WlShellId,
        scene: wm_core::OverviewScene<WlWindowId, WlFrameId>,
        backend: &WaylandBackend,
    ) -> Self {
        Self {
            surface,
            geometry: scene.geometry,
            selected: scene.selected,
            drag: None,
            progress: 1.0,
            paint_order: {
                let mut indices: std::collections::HashMap<_, _> = scene.windows.iter().enumerate().map(|(i, w)| (w.window, i)).collect();
                let mut order = Vec::with_capacity(scene.windows.len());
                for entry in backend.stacking.iter().rev() {
                    let window = match entry {
                        crate::state::StackEntry::Window(id) => Some(*id),
                        crate::state::StackEntry::Frame(id) => backend.frames.get(id).map(|f| f.window),
                    };
                    if let Some(index) = window.and_then(|id| indices.remove(&id)) { order.push(index); }
                }
                for (i, w) in scene.windows.iter().enumerate() {
                    if indices.contains_key(&w.window) { order.push(i); }
                }
                order
            },
            workspace: scene.workspace,
            gap: scene.gap,
            windows: scene
                .windows
                .into_iter()
                .map(|w| {
                    // A minimized X11 client may have released its surface. Reuse
                    // its already-cached icon snapshot only in that case; never
                    // request a capture or retain a new full-size window copy.
                    let fallback = backend
                        .windows
                        .get(&w.window)
                        .filter(|r| !r.mapped)
                        .and_then(|r| r.snapshot.as_ref())
                        .and_then(|b| import_buffer(b, true));
                    Window {
                        window: w.window,
                        frame: w.frame,
                        source: w.source,
                        destination: w.destination,
                        label: Label::new(w.label),
                        fallback,
                        shadow: Id::new(),
                        desktop_visible: backend.scene_index.is_presented(w.window)
                            && backend.windows.get(&w.window).is_some_and(|r| r.mapped),
                        draw_content: true,
                        sticky: false,
                    }
                })
                .collect(),
            spaces: scene
                .spaces
                .into_iter()
                .map(|space| Workspace {
                    rect: space.rect,
                    label: Label::new(space.label),
                    drop_label: Label::new(space.drop_label),
                    close: space.close.map(|(rect, glyph)| (rect, Label::new(glyph))),
                    background: Id::new(),
                })
                .collect(),
            ring: std::array::from_fn(|_| Id::new()),
            space_ring: std::array::from_fn(|_| Id::new()),
            drop_ring: std::array::from_fn(|_| Id::new()),
            drop_fill: Id::new(),
            band: Id::new(),
            backdrop: Id::new(),
        }
    }

    pub fn presented(&self, backend: &WaylandBackend, viewport: Rect) -> bool {
        backend.shells.get(&self.surface).is_some_and(|s| s.mapped)
            && overlaps(self.geometry, viewport)
    }

    pub fn label_bytes(&self) -> usize {
        self.windows
            .iter()
            .map(|w| &w.label)
            .chain(self.spaces.iter().map(|space| &space.label))
            .chain(self.spaces.iter().map(|space| &space.drop_label))
            .chain(self.spaces.iter().filter_map(|space| space.close.as_ref().map(|(_, glyph)| glyph)))
            .map(|l| l.size.w as usize * l.size.h as usize * 4)
            .sum()
    }

    /// Ordinary output frames take the short scene path. A capture spanning
    /// other outputs must keep their desktops and compose this region over it.
    pub fn covers(&self, viewport: Rect) -> bool {
        self.geometry.pos.x <= viewport.pos.x && self.geometry.pos.y <= viewport.pos.y
            && self.geometry.pos.x + self.geometry.size.w as i32 >= viewport.pos.x + viewport.size.w as i32
            && self.geometry.pos.y + self.geometry.size.h as i32 >= viewport.pos.y + viewport.size.h as i32
    }
}

pub(crate) fn render_backdrop(elements: &mut Vec<SceneElement<GlesRenderer>>, renderer: &mut GlesRenderer,
    backend: &WaylandBackend, overview: &Overview, viewport: Rect) {
    space_background(elements, renderer, backend, overview.geometry,
        Rect::new(Point::new(overview.geometry.pos.x - viewport.pos.x, overview.geometry.pos.y - viewport.pos.y), overview.geometry.size),
        &overview.backdrop);
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.pos.x < b.pos.x + b.size.w as i32
        && b.pos.x < a.pos.x + a.size.w as i32
        && a.pos.y < b.pos.y + b.size.h as i32
        && b.pos.y < a.pos.y + a.size.h as i32
}

pub(crate) fn solid(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    id: &Id,
    rect: Rect,
    color: Color32F,
) {
    if rect.size.w == 0 || rect.size.h == 0 {
        return;
    }
    elements.push(
        SolidColorRenderElement::new(
            id.clone(),
            SRect::<i32, Physical>::new(
                (rect.pos.x, rect.pos.y).into(),
                (rect.size.w as i32, rect.size.h as i32).into(),
            ),
            CommitCounter::default(),
            color,
            Kind::Unspecified,
        )
        .into(),
    );
}

fn outline(elements: &mut Vec<SceneElement<GlesRenderer>>, ids: &[Id; 4], rect: Rect, edge: u32, alpha: f32) {
    let color = Color32F::new(0.23 * alpha, 0.61 * alpha, alpha, alpha);
    let x = rect.pos.x - edge as i32;
    let y = rect.pos.y - edge as i32;
    for (id, r) in ids.iter().zip([
        Rect::new(Point::new(x, y), Size::new(rect.size.w + edge * 2, edge)),
        Rect::new(
            Point::new(x, rect.pos.y + rect.size.h as i32),
            Size::new(rect.size.w + edge * 2, edge),
        ),
        Rect::new(Point::new(x, rect.pos.y), Size::new(edge, rect.size.h)),
        Rect::new(
            Point::new(rect.pos.x + rect.size.w as i32, rect.pos.y),
            Size::new(edge, rect.size.h),
        ),
    ]) {
        solid(elements, id, r, color);
    }
}

fn label(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    label: &Label,
    rect: Rect,
    gap: u32,
    alpha: f32,
) {
    let Some(buffer) = &label.buffer else { return };
    let location = (
        (rect.pos.x + (rect.size.w as i32 - label.size.w as i32) / 2) as f64,
        (rect.pos.y + rect.size.h as i32 + gap as i32) as f64,
    );
    if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
        renderer,
        location,
        buffer,
        Some(alpha),
        None,
        None,
        Kind::Unspecified,
    ) {
        elements.push(element.into());
    }
}

pub(crate) fn render(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    overview: &Overview,
    viewport: Rect,
) {
    let offset = Point::new(
        overview.geometry.pos.x - viewport.pos.x,
        overview.geometry.pos.y - viewport.pos.y,
    );
    let local = |rect: Rect| {
        Rect::new(
            Point::new(rect.pos.x + offset.x, rect.pos.y + offset.y),
            rect.size,
        )
    };
    let edge = (overview.gap / 6).max(2);
    // Geometry follows the spring, including its small elastic excursions.
    // Only opacity saturates; clamping geometry would clip release velocity.
    let progress = overview.progress;
    let alpha = progress.clamp(0.0, 1.0) as f32;
    let placed = |window: &Window| interpolate(
        Rect::new(Point::new(window.source.pos.x - viewport.pos.x,
            window.source.pos.y - viewport.pos.y), window.source.size),
        local(window.destination), progress);
    if let Some(drag) = overview.drag {
        if let Some(window) = overview.windows.get(drag.index) {
            let rect = local(drag.destination);
            // Frontmost, translucent and bounded: the destination remains
            // visible through the live image. No capture or client configure.
            render_window(elements, renderer, backend, window, rect, 0.82);
            solid(elements, &window.shadow, Rect::new(
                Point::new(rect.pos.x - edge as i32, rect.pos.y - edge as i32),
                Size::new(rect.size.w + edge * 2, rect.size.h + edge * 3)),
                Color32F::new(0.0, 0.0, 0.0, 0.24));
        }
    } else if let Some(window) = overview.windows.get(overview.selected) {
        let rect = placed(window);
        outline(elements, &overview.ring, rect, edge, alpha);
        label(elements, renderer, &window.label, rect, edge * 2, alpha);
    }
    for &index in &overview.paint_order {
        let window = &overview.windows[index];
        if overview.drag.is_some_and(|drag| drag.index == index) { continue; }
        let rect = placed(window);
        render_window(elements, renderer, backend, window, rect, if window.desktop_visible { 1.0 } else { alpha });
        solid(
            elements,
            &window.shadow,
            Rect::new(
                Point::new(rect.pos.x - edge as i32, rect.pos.y - edge as i32),
                Size::new(rect.size.w + edge * 2, rect.size.h + edge * 3),
            ),
            Color32F::new(0.0, 0.0, 0.0, 0.28 * alpha),
        );
    }
    for (i, space) in overview.spaces.iter().enumerate() {
        let rect = local(space.rect);
        // A desktop is a single generous drop target during a drag, including
        // its close-control corner. Never imply that dropping will delete it.
        if let Some((close, glyph)) = space.close.as_ref().filter(|_| overview.drag.is_none()) {
            let close = local(*close);
            if let Some(buffer) = &glyph.buffer {
                if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer, (close.pos.x as f64, close.pos.y as f64), buffer,
                    Some(alpha), None, None, Kind::Unspecified,
                ) {
                    elements.push(element.into());
                }
            }
        }
        let targeted = overview.drag.is_some_and(|drag| drag.workspace == Some(i));
        label(elements, renderer, if targeted { &space.drop_label } else { &space.label }, rect, edge * 2, alpha);
        if overview.drag.is_some_and(|drag| drag.workspace == Some(i)) {
            outline(elements, &overview.drop_ring, rect, edge * 2, alpha);
            solid(elements, &overview.drop_fill, rect, Color32F::new(0.03, 0.09, 0.16, 0.24));
        }
        if i == overview.workspace {
            outline(elements, &overview.space_ring, rect, edge, alpha);
        }
        space_background_alpha(elements, renderer, backend, overview.geometry, rect, &space.background, alpha);
    }
    if let Some(space) = overview.spaces.first() {
        solid(
            elements,
            &overview.band,
            Rect::new(
                offset,
                Size::new(
                    overview.geometry.size.w,
                    space.rect.pos.y as u32 + space.rect.size.h + space.label.size.h + overview.gap,
                ),
            ),
            Color32F::new(0.0, 0.0, 0.0, 0.3 * alpha),
        );
    }
}

pub(crate) fn interpolate(source: Rect, target: Rect, progress: f64) -> Rect {
    let mix = |a: f64, b: f64| a + (b - a) * progress;
    Rect::new(Point::new(mix(source.pos.x as f64, target.pos.x as f64).round() as i32,
        mix(source.pos.y as f64, target.pos.y as f64).round() as i32),
        Size::new(mix(source.size.w as f64, target.size.w as f64).round().max(1.0) as u32,
        mix(source.size.h as f64, target.size.h as f64).round().max(1.0) as u32))
}

pub(crate) fn render_window(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    window: &Window,
    destination: Rect,
    alpha: f32,
) {
    render_window_scaled(
        elements,
        renderer,
        backend,
        window,
        destination,
        alpha,
        false,
        None,
    );
}

pub(crate) fn render_layout_window(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    window: &Window,
    destination: Rect,
    viewport: Rect,
) {
    render_window_scaled(elements, renderer, backend, window, destination, 1.0, true, Some(viewport));
}

#[allow(clippy::too_many_arguments)]
fn render_window_scaled(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    window: &Window,
    destination: Rect,
    alpha: f32,
    stretch: bool,
    viewport: Option<Rect>,
) {
    let Some(record) = backend
        .windows
        .get(&window.window)
        .filter(|r| r.surface.alive())
    else {
        return;
    };
    let scale = (destination.size.w as f64 / window.source.size.w.max(1) as f64)
        .min(destination.size.h as f64 / window.source.size.h.max(1) as f64);
    if scale <= 0.0 {
        return;
    }
    let sx = if stretch {
        destination.size.w as f64 / window.source.size.w.max(1) as f64
    } else {
        scale
    };
    let sy = if stretch {
        destination.size.h as f64 / window.source.size.h.max(1) as f64
    } else {
        scale
    };
    let before = elements.len();
    if let Some(surface) = record.surface.wl_surface().filter(|_| window.draw_content) {
        let origin = SPoint::<i32, Physical>::from((
            destination.pos.x
                + ((record.content.pos.x - record.content_offset.x - window.source.pos.x) as f64
                    * sx)
                    .round() as i32,
            destination.pos.y
                + ((record.content.pos.y - record.content_offset.y - window.source.pos.y) as f64
                    * sy)
                    .round() as i32,
        ));
        let factor = backend.window_surface_scale(record);
        let committed = if stretch {
            crate::xdg::committed_content_size(&surface, factor, backend.output_size)
                .unwrap_or(record.content.size)
        } else {
            record.content.size
        };
        let surface_scale = smithay::utils::Scale::from((
            factor * sx * record.content.size.w as f64 / committed.w.max(1) as f64,
            factor * sy * record.content.size.h as f64 / committed.h.max(1) as f64,
        ));
        // Managed presentation owns the same popup plane as ordinary windows.
        // Each popup keeps its own committed scale, anchored through its
        // parent's transform. Overview retains its existing thumbnail policy.
        if stretch {
            for (popup, offset) in backend.popups_for_surface(&surface) {
                let popup_surface = popup.wl_surface();
                let popup_factor = crate::xdg::effective_surface_scale(
                    crate::xdg::committed_surface_scale(popup_surface),
                    backend.scale_at(record.content),
                );
                let popup_scale = smithay::utils::Scale::from((popup_factor * sx, popup_factor * sy));
                let at = Point::new(
                    origin.x.saturating_add((offset.x as f64 * factor * sx).round() as i32),
                    origin.y.saturating_add((offset.y as f64 * factor * sy).round() as i32),
                );
                if viewport.is_none_or(|v| crate::renderer::surface_tree_reaches_viewport(
                    popup_surface, at, Rect::new(at, Size::default()), popup_scale, v,
                )) {
                    push_surface_tree_alpha(elements, renderer, popup_surface,
                        (at.x, at.y).into(), popup_scale, 1.0, Kind::Unspecified, alpha);
                }
            }
        }
        if viewport.is_none_or(|v| crate::renderer::surface_tree_reaches_viewport(
            &surface, Point::new(origin.x, origin.y), destination, surface_scale, v,
        )) {
            push_surface_tree_alpha(
                elements, renderer, &surface, origin, surface_scale, 1.0, Kind::Unspecified, alpha,
            );
        }
    }
    // Offscreen Flow cells still get the exact surface-tree check above: a
    // subsurface or popup may extend into view. Their chrome and fallback
    // cannot, so skip their imports and element construction entirely.
    if viewport.is_some_and(|v| destination.intersection(v).is_none()) {
        return;
    }
    if let Some(buffer) = window
        .fallback
        .as_ref()
        .filter(|_| elements.len() == before)
    {
        // The source remains the cached icon's actual size. Rescale the element
        // instead of asking MemoryRenderBuffer to crop a thumbnail-sized source.
        if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (destination.pos.x as f64, destination.pos.y as f64),
            buffer,
            Some(alpha),
            None,
            None,
            Kind::Unspecified,
        ) {
            use smithay::backend::renderer::element::Element;
            let size = element.geometry(1.0.into()).size;
            let factor = (destination.size.w as f64 / size.w.max(1) as f64)
                .min(destination.size.h as f64 / size.h.max(1) as f64);
            elements.push(
                RescaleRenderElement::from_element(
                    element,
                    (destination.pos.x, destination.pos.y).into(),
                    factor,
                )
                .into(),
            );
        }
    }
    if let Some(frame) = window.frame.and_then(|f| backend.frames.get(&f)) {
        for part in &frame.parts {
            let origin = SPoint::<i32, Physical>::from((
                destination.pos.x
                    + ((frame.geometry.pos.x + part.offset.x - window.source.pos.x) as f64 * sx)
                        .round() as i32,
                destination.pos.y
                    + ((frame.geometry.pos.y + part.offset.y - window.source.pos.y) as f64 * sy)
                        .round() as i32,
            ));
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                origin.to_f64(),
                &part.buffer,
                Some(alpha),
                None,
                None,
                Kind::Unspecified,
            ) {
                elements.push(
                    RescaleRenderElement::from_element(
                        element,
                        origin,
                        smithay::utils::Scale::from((sx, sy)),
                    )
                    .into(),
                );
            }
        }
        solid(
            elements,
            &frame.fill_id,
            destination,
            Color32F::new(0.0, 0.0, 0.0, alpha),
        );
    }
}

pub(crate) fn space_background(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    source: Rect,
    destination: Rect,
    id: &Id,
) {
    space_background_alpha(elements, renderer, backend, source, destination, id, 1.0);
}
#[allow(clippy::too_many_arguments)]
fn space_background_alpha(elements: &mut Vec<SceneElement<GlesRenderer>>, renderer: &mut GlesRenderer,
    backend: &WaylandBackend, source: Rect, destination: Rect, id: &Id, alpha: f32) {
    let scale = destination.size.w as f64 / source.size.w.max(1) as f64;
    for record in backend.layers.iter().rev().filter(|r| {
        r.layer == Layer::Background && backend.layer_presented(r) && overlaps(r.geometry, source)
    }) {
        let surface = record.surface.wl_surface();
        let factor = crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(surface),
            backend.scale_at(record.geometry),
        );
        let origin = (
            destination.pos.x
                + ((record.geometry.pos.x - source.pos.x) as f64 * scale).round() as i32,
            destination.pos.y
                + ((record.geometry.pos.y - source.pos.y) as f64 * scale).round() as i32,
        );
        push_surface_tree_alpha(
            elements,
            renderer,
            surface,
            origin.into(),
            factor * scale,
            1.0,
            Kind::Unspecified,
            alpha,
        );
    }
    match &backend.root_background {
        RootBackground::Color((r, g, b)) => solid(
            elements,
            id,
            destination,
            Color32F::new(*r as f32 / 255.0 * alpha, *g as f32 / 255.0 * alpha, *b as f32 / 255.0 * alpha, alpha),
        ),
        RootBackground::Image(buffer) => {
            let src = SRect::new(
                (source.pos.x as f64, source.pos.y as f64).into(),
                (source.size.w as f64, source.size.h as f64).into(),
            );
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                (destination.pos.x as f64, destination.pos.y as f64),
                buffer,
                Some(alpha),
                Some(src),
                Some((destination.size.w as i32, destination.size.h as i32).into()),
                Kind::Unspecified,
            ) {
                elements.push(element.into());
            }
        }
    }
}
