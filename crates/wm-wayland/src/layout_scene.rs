//! Cached presentation of core layout transactions. No layout policy lives here.
use crate::overview::{interpolate, Window};
use crate::renderer::SceneElement;
use crate::state::{Compositor, Graphics, StackEntry, WaylandBackend, WlFrameId, WlWindowId};
use smithay::backend::renderer::element::{memory::MemoryRenderBuffer, Id};
use smithay::backend::renderer::{gles::GlesRenderer, Color32F};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use wm_core::gesture_physics::Spring;
use wm_theme_api::{DecorationBuffer, Point, Rect};

struct Motion {
    from: Rect,
    to: Rect,
    spring: Spring,
}
impl Motion {
    fn geometry(&self) -> Rect {
        interpolate(self.from, self.to, self.spring.position)
    }
}

struct Presentation {
    window: Window,
    clip: Option<Rect>,
    motion: Option<Motion>,
    crop: Id,
}

struct Caption {
    buffer: MemoryRenderBuffer,
    geometry: Rect,
    expires: Instant,
}

pub(crate) struct Scene {
    pub setups: u64,
    pub setup_ns: u128,
    pub build_ns: std::cell::Cell<u128>,
    pub builds: std::cell::Cell<u64>,
    pub culled: std::cell::Cell<u64>,
    pub configures: u64,
    windows: HashMap<WlWindowId, Presentation>,
    caption: Option<Caption>,
    preview: Option<Rect>,
    drag: Option<Window>,
    preview_ids: [Id; 4],
    last_frame: Instant,
    next_frame: Instant,
}
impl Default for Scene {
    fn default() -> Self {
        Self {
            setups: 0,
            setup_ns: 0,
            build_ns: std::cell::Cell::new(0),
            builds: std::cell::Cell::new(0),
            culled: std::cell::Cell::new(0),
            configures: 0,
            windows: HashMap::new(),
            caption: None,
            preview: None,
            drag: None,
            preview_ids: std::array::from_fn(|_| Id::new()),
            last_frame: Instant::now(),
            next_frame: Instant::now(),
        }
    }
}
impl Scene {
    pub fn workarea(&self, window: WlWindowId) -> Option<Rect> {
        self.windows.get(&window).and_then(|p| p.clip)
    }

    pub fn transitioning(&self, window: WlWindowId) -> bool {
        self.windows
            .get(&window)
            .is_some_and(|p| p.motion.is_some())
    }

    pub fn animating(&self) -> bool {
        self.windows.values().any(|w| w.motion.is_some())
    }

    pub fn allows_pointer(&self, window: WlWindowId, at: Point) -> bool {
        self.windows
            .get(&window)
            .is_none_or(|p| p.motion.is_none() && p.clip.is_none_or(|clip| clip.contains(at)))
    }

    /// Idempotent: settle to already committed semantic geometry, discard
    /// transient effects, and retain only the output clips still in use.
    pub fn cancel(&mut self) {
        self.windows.retain(|_, p| {
            p.motion = None;
            p.clip.is_some()
        });
        self.caption = None;
        self.preview = None;
        self.drag = None;
    }
}

pub(crate) fn present(
    backend: &mut WaylandBackend,
    window: WlWindowId,
    frame: Option<WlFrameId>,
    source: Rect,
    destination: Rect,
    clip: Option<Rect>,
    animate: bool,
) {
    let started = Instant::now();
    if !backend.windows.contains_key(&window) {
        return;
    }
    if source == destination
        && clip.is_some()
        && !backend.layout_scene.windows.contains_key(&window)
    {
        let snapshot = Window::snapshot(window, frame, destination, backend);
        backend.layout_scene.windows.insert(
            window,
            Presentation {
                window: snapshot,
                clip,
                motion: None,
                crop: Id::new(),
            },
        );
    }
    let scene = &mut backend.layout_scene;
    if source == destination {
        if scene
            .windows
            .get(&window)
            .is_some_and(|p| p.window.source != destination)
        {
            scene.windows.remove(&window);
        }
        if let Some(p) = scene.windows.get_mut(&window) {
            p.clip = clip;
        }
        return;
    }
    let from = scene
        .drag
        .as_ref()
        .filter(|w| w.window == window)
        .map(|w| w.destination)
        .unwrap_or_else(|| {
            scene
                .windows
                .get(&window)
                .and_then(|p| p.motion.as_ref())
                .map_or(source, Motion::geometry)
        });
    let was_animating = scene.animating();
    let snapshot = Window::snapshot(window, frame, destination, backend);
    let scene = &mut backend.layout_scene;
    let crop = scene
        .windows
        .get(&window)
        .map_or_else(Id::new, |p| p.crop.clone());
    scene.windows.insert(
        window,
        Presentation {
            window: snapshot,
            clip,
            crop,
            motion: animate.then(|| Motion {
                from,
                to: destination,
                spring: Spring::new(0.0, 0.0, 1.0),
            }),
        },
    );
    if !was_animating {
        scene.last_frame = Instant::now();
    }
    scene.next_frame = Instant::now();
    backend.mark_damaged();
    backend.layout_scene.setups += 1;
    backend.layout_scene.setup_ns += started.elapsed().as_nanos();
}

pub(crate) fn preview(
    backend: &mut WaylandBackend,
    drag: Option<wm_core::LayoutDrag<WlWindowId, WlFrameId>>,
    target: Option<Rect>,
) {
    if let Some(wm_core::LayoutDrag {
        window: id,
        frame,
        source,
        destination,
    }) = drag
    {
        if let Some(window) = backend
            .layout_scene
            .drag
            .as_mut()
            .filter(|w| w.window == id)
        {
            window.destination = destination;
        } else {
            let mut window = Window::snapshot(id, frame, source, backend);
            window.destination = destination;
            backend.layout_scene.drag = Some(window);
        }
    } else {
        backend.layout_scene.drag = None;
    }
    backend.layout_scene.preview = target;
}

pub(crate) fn caption(backend: &mut WaylandBackend, label: DecorationBuffer) {
    let Some(buffer) = crate::backend_impl::import_buffer(&label, false) else {
        return;
    };
    let Some(monitor) = backend
        .monitors
        .iter()
        .find(|m| m.primary)
        .or_else(|| backend.monitors.first())
    else {
        return;
    };
    let geometry = Rect::new(
        Point::new(
            monitor.geometry.pos.x
                + (monitor.geometry.size.w.saturating_sub(label.width) / 2) as i32,
            monitor.geometry.pos.y + (monitor.geometry.size.h / 12) as i32,
        ),
        wm_core::Size::new(label.width, label.height),
    );
    backend.layout_scene.caption = Some(Caption {
        buffer,
        geometry,
        expires: Instant::now() + Duration::from_millis(950),
    });
    backend.mark_damaged();
}

pub(crate) fn tick(comp: &mut Compositor) {
    let cancel = comp.wm.backend().locked
        || comp.wm.backend().gesture_scene.is_some()
        || comp.wm.backend().overview.is_some()
        || comp.wm.backend().keyboard_grabbed
        || comp.wm.backend().capture_ui.is_some()
        || comp.layer_shell.exclusive_focus.is_some()
        || comp.focus_grab.is_active()
        || !comp.running
        || comp.restart;
    if cancel && comp.wm.interactive_drag_active() {
        comp.wm.dispatch(wm_core::BackendEvent::DragCancelled);
    }
    let now = Instant::now();
    let interval = comp
        .outputs
        .iter()
        .filter_map(|o| o.output.current_mode())
        .filter(|m| m.refresh > 0)
        .map(|m| {
            Duration::from_nanos(1_000_000_000_000 / (m.refresh as u64).clamp(1000, 1_000_000))
        })
        .min()
        .unwrap_or(Duration::from_nanos(1_000_000_000 / 60));
    let backend = comp.wm.backend_mut();
    let scene = &mut backend.layout_scene;
    let mut damaged = scene.animating();
    if cancel {
        scene.cancel();
    }
    let dt = now
        .saturating_duration_since(scene.last_frame)
        .as_secs_f64();
    scene.windows.retain(|id, p| {
        let Some(record) = backend.windows.get(id).filter(|r| r.surface.alive()) else {
            return false;
        };
        if !record.mapped {
            p.motion = None;
        }
        if p.motion
            .as_mut()
            .is_some_and(|motion| motion.spring.advance(dt))
        {
            p.motion = None;
        }
        p.motion.is_some() || p.clip.is_some()
    });
    if scene.caption.as_ref().is_some_and(|c| now >= c.expires) {
        scene.caption = None;
        damaged = true;
    }
    scene.last_frame = now;
    scene.next_frame = now + interval;
    if damaged {
        backend.mark_damaged();
    }
}

pub(crate) fn deadline(comp: &Compositor) -> Option<Instant> {
    let scene = &comp.wm.backend().layout_scene;
    let animation = (matches!(comp.graphics, Graphics::Winit(_)) && scene.animating())
        .then_some(scene.next_frame);
    animation
        .into_iter()
        .chain(scene.caption.as_ref().map(|c| c.expires))
        .min()
}

pub(crate) fn render_window(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    entry: &StackEntry,
    viewport: Rect,
) -> bool {
    let id = match entry {
        StackEntry::Window(id) => *id,
        StackEntry::Frame(id) => match backend.frames.get(id) {
            Some(f) if f.mapped => f.window,
            _ => return false,
        },
    };
    if backend
        .layout_scene
        .drag
        .as_ref()
        .is_some_and(|w| w.window == id)
    {
        return true;
    }
    let Some(p) = backend.layout_scene.windows.get(&id) else {
        return false;
    };
    if !backend.windows.get(&id).is_some_and(|r| r.mapped) {
        return false;
    }
    let Some(mut visible) = p
        .clip
        .map_or(Some(viewport), |clip| clip.intersection(viewport))
    else {
        return true;
    };
    let started = Instant::now();
    let mut destination = p.motion.as_ref().map_or(p.window.source, Motion::geometry);
    destination.pos.x -= viewport.pos.x;
    destination.pos.y -= viewport.pos.y;
    visible.pos.x -= viewport.pos.x;
    visible.pos.y -= viewport.pos.y;
    let start = elements.len();
    crate::overview::render_layout_window(
        elements,
        renderer,
        backend,
        &p.window,
        destination,
        visible,
    );
    if let Some(mut clip) = p.clip {
        clip.pos.x -= viewport.pos.x;
        clip.pos.y -= viewport.pos.y;
        crate::renderer::clip_plane(elements, start, clip, &p.crop);
    }
    let scene = &backend.layout_scene;
    scene
        .build_ns
        .set(scene.build_ns.get() + started.elapsed().as_nanos());
    scene.builds.set(scene.builds.get() + 1);
    if start == elements.len() {
        scene.culled.set(scene.culled.get() + 1);
    }
    true
}

pub(crate) fn render_feedback(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    viewport: Rect,
) {
    use smithay::backend::renderer::element::{memory::MemoryRenderBufferRenderElement, Kind};
    let scene = &backend.layout_scene;
    if let Some(drag) = &scene.drag {
        let mut destination = drag.destination;
        destination.pos.x -= viewport.pos.x;
        destination.pos.y -= viewport.pos.y;
        crate::overview::render_window(elements, renderer, backend, drag, destination, 0.82);
    }
    if let Some(caption) = &scene.caption {
        if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (
                (caption.geometry.pos.x - viewport.pos.x) as f64,
                (caption.geometry.pos.y - viewport.pos.y) as f64,
            ),
            &caption.buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ) {
            elements.push(element.into());
        }
    }
    if let Some(mut rect) = scene.preview {
        rect.pos.x -= viewport.pos.x;
        rect.pos.y -= viewport.pos.y;
        let thickness = (backend.scale_at(viewport) * 3.0).round().max(1.0) as u32;
        let w = rect.size.w;
        let h = rect.size.h;
        for (id, (x, y, w, h)) in scene.preview_ids.iter().zip([
            (0, 0, w, thickness),
            (0, h.saturating_sub(thickness), w, thickness),
            (0, 0, thickness, h),
            (w.saturating_sub(thickness), 0, thickness, h),
        ]) {
            crate::overview::solid(
                elements,
                id,
                Rect::new(
                    Point::new(rect.pos.x + x as i32, rect.pos.y + y as i32),
                    wm_core::Size::new(w, h),
                ),
                Color32F::new(0.6, 0.75, 0.9, 0.8),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[global_allocator]
    static ALLOCATOR: chonk_test_support::AllocationCounter = chonk_test_support::AllocationCounter;

    #[test]
    fn cached_layout_motion_allocates_nothing_while_interpolating_live_frames() {
        let from = Rect::new(Point::new(-800, 80), wm_core::Size::new(800, 600));
        let to = Rect::new(Point::new(100, 0), wm_core::Size::new(600, 1000));
        let (_, allocations) = chonk_test_support::measure(|| {
            for _ in 0..1000 {
                let mut motion = Motion {
                    from,
                    to,
                    spring: Spring::new(0.0, 0.0, 1.0),
                };
                for _ in 0..144 {
                    motion.spring.advance(std::hint::black_box(1.0 / 144.0));
                    std::hint::black_box(motion.geometry());
                }
            }
        });
        assert_eq!((allocations.calls, allocations.requested_bytes), (0, 0));
    }

    #[test]
    fn live_geometry_interpolates_interrupts_and_settles_at_actual_frame_rates() {
        let from = Rect::new(Point::new(80, 100), wm_core::Size::new(900, 600));
        let to = Rect::new(Point::new(0, 0), wm_core::Size::new(1280, 1000));
        for hz in [60, 120, 144, 240] {
            let mut m = Motion {
                from,
                to,
                spring: Spring::new(0.0, 0.0, 1.0),
            };
            assert_eq!(m.geometry(), from);
            m.spring.advance(0.05);
            let middle = m.geometry();
            assert_ne!(middle, from);
            assert_ne!(middle, to);
            let mut replacement = Motion {
                from: middle,
                to: from,
                spring: Spring::new(0.0, 0.0, 1.0),
            };
            assert_eq!(replacement.geometry(), middle);
            for _ in 0..hz * 2 {
                m.spring.advance(1.0 / hz as f64);
            }
            assert_eq!(m.geometry(), to);
            replacement.spring.advance(10.0);
            assert_eq!(replacement.geometry(), from);
        }
    }
}
