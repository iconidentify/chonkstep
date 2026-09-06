//! Overview is a view of the existing scene, never a screenshot of it. The
//! input-only shell keeps modal routing unchanged. GPU transforms reuse client
//! textures and sparse frame buffers; only small captions own new pixels.

use crate::backend_impl::import_buffer;
use crate::renderer::{push_surface_tree, SceneElement};
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
}

pub(crate) struct Overview {
    pub surface: WlShellId,
    pub geometry: Rect,
    pub windows: Vec<Window>,
    spaces: Vec<(Rect, Label, Id)>,
    pub selected: usize,
    workspace: usize,
    gap: u32,
    ring: [Id; 4],
    space_ring: [Id; 4],
    band: Id,
    backdrop: Id,
}

impl Overview {
    pub fn new(
        surface: WlShellId,
        scene: wm_core::OverviewScene<WlWindowId, WlFrameId>,
        backend: &WaylandBackend,
    ) -> Self {
        Self {
            surface,
            geometry: scene.geometry,
            selected: scene.selected,
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
                    }
                })
                .collect(),
            spaces: scene
                .spaces
                .into_iter()
                .map(|(rect, label)| (rect, Label::new(label), Id::new()))
                .collect(),
            ring: std::array::from_fn(|_| Id::new()),
            space_ring: std::array::from_fn(|_| Id::new()),
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
            .chain(self.spaces.iter().map(|(_, l, _)| l))
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

fn solid(elements: &mut Vec<SceneElement<GlesRenderer>>, id: &Id, rect: Rect, color: Color32F) {
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

fn outline(elements: &mut Vec<SceneElement<GlesRenderer>>, ids: &[Id; 4], rect: Rect, edge: u32) {
    let color = Color32F::new(0.23, 0.61, 1.0, 1.0);
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
        None,
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
    if let Some(window) = overview.windows.get(overview.selected) {
        let rect = local(window.destination);
        outline(elements, &overview.ring, rect, edge);
        label(elements, renderer, &window.label, rect, edge * 2);
    }
    for window in &overview.windows {
        let rect = local(window.destination);
        render_window(elements, renderer, backend, window, rect);
        solid(
            elements,
            &window.shadow,
            Rect::new(
                Point::new(rect.pos.x - edge as i32, rect.pos.y - edge as i32),
                Size::new(rect.size.w + edge * 2, rect.size.h + edge * 3),
            ),
            Color32F::new(0.0, 0.0, 0.0, 0.28),
        );
    }
    for (i, (rect, caption, id)) in overview.spaces.iter().enumerate() {
        let rect = local(*rect);
        label(elements, renderer, caption, rect, edge * 2);
        if i == overview.workspace {
            outline(elements, &overview.space_ring, rect, edge);
        }
        space_background(elements, renderer, backend, overview.geometry, rect, id);
    }
    if let Some((rect, caption, _)) = overview.spaces.first() {
        solid(
            elements,
            &overview.band,
            Rect::new(
                offset,
                Size::new(
                    overview.geometry.size.w,
                    rect.pos.y as u32 + rect.size.h + caption.size.h + overview.gap,
                ),
            ),
            Color32F::new(0.0, 0.0, 0.0, 0.3),
        );
    }
}

fn render_window(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    window: &Window,
    destination: Rect,
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
    let before = elements.len();
    if let Some(surface) = record.surface.wl_surface() {
        let origin = SPoint::<i32, Physical>::from((
            destination.pos.x
                + ((record.content.pos.x - record.content_offset.x - window.source.pos.x) as f64
                    * scale)
                    .round() as i32,
            destination.pos.y
                + ((record.content.pos.y - record.content_offset.y - window.source.pos.y) as f64
                    * scale)
                    .round() as i32,
        ));
        push_surface_tree(
            elements,
            renderer,
            &surface,
            origin,
            backend.window_surface_scale(record) * scale,
            1.0,
            Kind::Unspecified,
        );
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
            None,
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
                    + ((frame.geometry.pos.x + part.offset.x - window.source.pos.x) as f64 * scale)
                        .round() as i32,
                destination.pos.y
                    + ((frame.geometry.pos.y + part.offset.y - window.source.pos.y) as f64 * scale)
                        .round() as i32,
            ));
            if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                renderer,
                origin.to_f64(),
                &part.buffer,
                None,
                None,
                None,
                Kind::Unspecified,
            ) {
                elements.push(RescaleRenderElement::from_element(element, origin, scale).into());
            }
        }
        solid(
            elements,
            &frame.fill_id,
            destination,
            Color32F::new(0.0, 0.0, 0.0, 1.0),
        );
    }
}

fn space_background(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    source: Rect,
    destination: Rect,
    id: &Id,
) {
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
        push_surface_tree(
            elements,
            renderer,
            surface,
            origin.into(),
            factor * scale,
            1.0,
            Kind::Unspecified,
        );
    }
    match &backend.root_background {
        RootBackground::Color((r, g, b)) => solid(
            elements,
            id,
            destination,
            Color32F::new(*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0),
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
                None,
                Some(src),
                Some((destination.size.w as i32, destination.size.h as i32).into()),
                Kind::Unspecified,
            ) {
                elements.push(element.into());
            }
        }
    }
}
