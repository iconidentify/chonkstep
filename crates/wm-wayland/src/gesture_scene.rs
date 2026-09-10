//! Transient scene ownership for finger-driven desktop transitions. WM topology
//! and focus are untouched until settlement; every frame reuses live textures.
use crate::overview::{Overview, Window};
use crate::renderer::SceneElement;
use crate::state::{Compositor, Graphics, StackEntry, WaylandBackend};
use smithay::backend::renderer::element::Id;
use smithay::backend::renderer::{gles::GlesRenderer, Color32F};
use std::time::{Duration, Instant};
use wm_core::{
    gesture_physics as physics, ClientFlags, DesktopGesture, Lifecycle, SwipeAxis, SwipeMotion,
};
use wm_theme_api::{Point, Rect};

struct Plane {
    workspace: usize,
    windows: Vec<Window>,
    overview: Option<Overview>,
    background: Id,
}

pub(crate) struct Transition {
    pub motion: SwipeMotion,
    pub origin: usize,
    pub output: Option<Rect>,
    count: usize,
    pub previous: Option<usize>,
    pub next: Option<usize>,
    pub position: f64,
    pub velocity: f64,
    pub projected: f64,
    pub spring: Option<physics::Spring>,
    overview_origin: bool,
    base: f64,
    planes: Vec<Plane>,
    monitors: Vec<(Rect, f64)>,
    last_frame: Instant,
    next_frame: Instant,
    frame_interval: Duration,
    overview_token: Option<Id>,
}
impl Transition {
    pub fn horizontal(&self) -> bool {
        self.motion.axis == SwipeAxis::Horizontal
    }
    fn bounds(&self) -> (f64, f64) {
        if self.horizontal() {
            (
                if self.previous.is_some() { -1.0 } else { 0.0 },
                if self.next.is_some() { 1.0 } else { 0.0 },
            )
        } else {
            (0.0, 1.0)
        }
    }
    fn follow(&mut self, motion: SwipeMotion) {
        self.motion = motion;
        let raw = self.base
            + motion.progress
            + if !self.horizontal() && self.overview_origin {
                1.0
            } else {
                0.0
            };
        let (min, max) = self.bounds();
        let (position, derivative) = physics::resisted(raw, min, max);
        self.position = position;
        self.velocity = motion.velocity * derivative;
        self.projected = physics::projected(position, self.velocity);
    }
}

pub(crate) fn update(comp: &mut Compositor, motion: SwipeMotion) {
    let Some(monitor) = comp.wm.monitors_ref().get(comp.wm.active_output_index()) else {
        cancel(comp);
        return;
    };
    let output = comp.wm.separate_spaces().then_some(monitor.geometry);
    if comp
        .wm
        .backend()
        .gesture_scene
        .as_ref()
        .is_some_and(|s| s.motion.axis != motion.axis)
    {
        cancel(comp);
    }
    if comp.wm.backend().gesture_scene.is_none() {
        let opened = comp.wm.backend().overview.is_some();
        if motion.axis == SwipeAxis::Vertical {
            if (opened && motion.progress >= 0.0) || (!opened && motion.progress <= 0.0) {
                return;
            }
            if !opened {
                comp.shell
                    .on_desktop_gesture(&mut comp.wm, DesktopGesture::OverviewOpen);
            }
            if comp.wm.backend().overview.is_none() {
                return;
            }
        }
        let origin = comp.wm.current_workspace();
        let count = comp.wm.workspace_count();
        let previous = if comp.wm.mac_mode() { comp.wm.neighboring_workspace(origin, -1) } else { origin.checked_sub(1) };
        let next = if comp.wm.mac_mode() { comp.wm.neighboring_workspace(origin, 1) } else { (origin + 1 < wm_core::MAX_WORKSPACES
            && (origin + 1 < count || comp.wm.workspace_has_windows(origin)))
        .then_some(origin + 1) };
        let mut planes = Vec::new();
        if motion.axis == SwipeAxis::Horizontal {
            for workspace in [Some(origin), previous, next].into_iter().flatten() {
                let windows: Vec<_> = comp
                    .wm
                    .backend()
                    .stacking
                    .iter()
                    .rev()
                    .filter_map(|entry| match entry {
                        StackEntry::Window(id) => comp.wm.client_for_window(*id),
                        StackEntry::Frame(id) => comp.wm.client_for_frame(*id),
                    })
                    .filter_map(|id| comp.wm.client(id))
                    .filter(|c| {
                        (if c.flags.contains(ClientFlags::STICKY) { workspace == origin } else { c.workspace == workspace })
                            && c.lifecycle == Lifecycle::Normal
                            && (!comp.wm.separate_spaces() || comp.wm.workspace_output_index(c.workspace) == comp.wm.workspace_output_index(origin))
                    })
                    .map(|c| {
                        let source = c.frame.and_then(|id| comp.wm.backend().frames.get(&id))
                            .map_or(c.geometry, |frame| frame.geometry);
                        let mut window = Window::snapshot(c.window, c.frame, source, comp.wm.backend());
                        window.draw_content = !c.flags.contains(ClientFlags::SHADED);
                        window.sticky = c.flags.contains(ClientFlags::STICKY);
                        window
                    })
                    .collect();
                let overview = comp
                    .wm
                    .backend()
                    .overview
                    .as_ref()
                    .filter(|_| workspace != origin)
                    .map(|o| {
                        let scene = comp
                            .shell
                            .desktop_gesture_overview_scene(&comp.wm, workspace, o.geometry);
                        Overview::new(o.surface, scene, comp.wm.backend())
                    });
                planes.push(Plane {
                    workspace,
                    windows,
                    overview,
                    background: Id::new(),
                });
            }
        }
        let now = Instant::now();
        let monitors = comp
            .wm
            .backend()
            .monitors
            .iter()
            .map(|m| (m.geometry, comp.wm.backend().scale_at(m.geometry)))
            .collect();
        let frame_interval = comp
            .outputs
            .iter()
            .filter_map(|o| o.output.current_mode())
            .filter(|m| m.refresh > 0)
            .map(|m| {
                Duration::from_nanos(1_000_000_000_000 / (m.refresh as u64).clamp(1000, 1_000_000))
            })
            .min()
            .unwrap_or(Duration::from_nanos(1_000_000_000 / 60));
        let overview_token = comp
            .wm
            .backend()
            .overview
            .as_ref()
            .map(|o| o.token().clone());
        comp.wm.backend_mut().gesture_scene = Some(Transition {
            motion,
            origin,
            output,
            count,
            previous,
            next,
            position: 0.0,
            velocity: 0.0,
            projected: 0.0,
            spring: None,
            overview_origin: opened,
            base: 0.0,
            planes,
            monitors,
            last_frame: now,
            next_frame: now,
            frame_interval,
            overview_token,
        });
        crate::input::sync_pointer_focus(comp);
    }
    if let Some(scene) = comp.wm.backend_mut().gesture_scene.as_mut() {
        scene.follow(motion);
    }
    sync_progress(comp);
}

/// Touching a moving desktop catches it at its exact current position. The
/// next locked-axis delta is relative to that position, not the logical origin.
pub(crate) fn catch(comp: &mut Compositor) -> bool {
    let Some(scene) = comp
        .wm
        .backend_mut()
        .gesture_scene
        .as_mut()
        .filter(|s| s.spring.is_some())
    else {
        return false;
    };
    let (min, max) = scene.bounds();
    scene.base = physics::unresisted(scene.position, min, max)
        - if !scene.horizontal() && scene.overview_origin {
            1.0
        } else {
            0.0
        };
    scene.spring = None;
    scene.velocity = 0.0;
    true
}

fn sync_progress(comp: &mut Compositor) {
    let backend = comp.wm.backend_mut();
    if let Some(scene) = &backend.gesture_scene {
        if !scene.horizontal() {
            if let Some(overview) = backend.overview.as_mut() {
                overview.progress = scene.position;
            }
        }
        backend.mark_damaged();
    }
}

pub(crate) fn release(comp: &mut Compositor, cancelled: bool, motion: Option<SwipeMotion>) {
    if let Some(scene) = comp.wm.backend_mut().gesture_scene.as_mut() {
        if let Some(motion) = motion {
            scene.follow(motion);
        }
        let target = if cancelled {
            if scene.horizontal() || !scene.overview_origin {
                0.0
            } else {
                1.0
            }
        } else {
            let (min, max) = scene.bounds();
            physics::settle_target(scene.position, scene.velocity, min, max)
        };
        scene.spring = Some(physics::Spring::new(scene.position, scene.velocity, target));
        scene.last_frame = Instant::now();
        scene.next_frame = scene.last_frame;
    }
    sync_progress(comp);
}

/// Ownership loss is immediate, unlike a libinput cancellation which settles.
pub(crate) fn cancel(comp: &mut Compositor) {
    let Some(scene) = comp.wm.backend_mut().gesture_scene.take() else {
        return;
    };
    if !scene.horizontal() && !scene.overview_origin {
        comp.shell
            .finish_desktop_gesture_overview(&mut comp.wm, false);
    } else if let Some(overview) = comp.wm.backend_mut().overview.as_mut() {
        overview.progress = 1.0;
    }
    comp.wm.backend_mut().mark_damaged();
    crate::input::sync_pointer_focus(comp);
}

pub(crate) fn deadline(comp: &Compositor) -> Option<Instant> {
    // Native KMS wakes at vblank and uses its existing FrameClock deadlines.
    // Only the nested, non-vsynced backend needs a finite animation deadline.
    if !matches!(comp.graphics, Graphics::Winit(_)) {
        return None;
    }
    comp.wm
        .backend()
        .gesture_scene
        .as_ref()
        .filter(|s| s.spring.is_some())
        .map(|s| s.next_frame)
}

pub(crate) fn validate(comp: &mut Compositor) {
    let Some(scene) = comp.wm.backend().gesture_scene.as_ref() else {
        return;
    };
    if comp.wm.current_workspace() != scene.origin
        || comp.wm.workspace_count() != scene.count
        || (scene.next == Some(scene.count) && !comp.wm.workspace_has_windows(scene.origin))
        || !crate::input::gestures::available(comp)
        || scene.monitors.len() != comp.wm.backend().monitors.len()
        || scene
            .monitors
            .iter()
            .zip(&comp.wm.backend().monitors)
            .any(|((rect, scale), monitor)| {
                *rect != monitor.geometry || *scale != comp.wm.backend().scale_at(monitor.geometry)
            })
        || scene.overview_token.as_ref() != comp.wm.backend().overview.as_ref().map(|o| o.token())
    {
        crate::input::gestures::cancel(comp);
    }
}
pub(crate) fn tick(comp: &mut Compositor) {
    validate(comp);
    let Some(scene) = comp.wm.backend().gesture_scene.as_ref() else {
        return;
    };
    if scene.spring.is_none() {
        return;
    }
    let now = Instant::now();
    let scene = comp.wm.backend_mut().gesture_scene.as_mut().unwrap();
    let spring = scene.spring.as_mut().unwrap();
    let finished = spring.advance(
        now.saturating_duration_since(scene.last_frame)
            .as_secs_f64(),
    );
    scene.last_frame = now;
    scene.next_frame = now + scene.frame_interval;
    scene.position = spring.position;
    scene.velocity = spring.velocity;
    if finished {
        let scene = comp.wm.backend_mut().gesture_scene.take().unwrap();
        let target = scene.spring.unwrap().target;
        if scene.horizontal() {
            if target > 0.0 {
                comp.shell
                    .on_desktop_gesture(&mut comp.wm, DesktopGesture::WorkspaceNext);
            } else if target < 0.0 {
                comp.shell
                    .on_desktop_gesture(&mut comp.wm, DesktopGesture::WorkspacePrevious);
            }
        } else {
            if let Some(overview) = comp.wm.backend_mut().overview.as_mut() {
                overview.progress = target;
            }
            comp.shell
                .finish_desktop_gesture_overview(&mut comp.wm, target > 0.5);
        }
        comp.wm.backend_mut().mark_damaged();
        crate::input::sync_pointer_focus(comp);
    } else {
        sync_progress(comp);
    }
}

pub(crate) fn render(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    scene: &Transition,
    viewport: Rect,
) -> Color32F {
    if !scene.overview_origin {
        crate::renderer::push_furniture(elements, renderer, backend, viewport, 1.0, true);
        if let Some(origin) = scene.planes.first() {
            for window in origin.windows.iter().filter(|w| w.sticky) {
                let rect = Rect::new(Point::new(window.source.pos.x - viewport.pos.x,
                    window.source.pos.y - viewport.pos.y), window.source.size);
                crate::overview::render_window(elements, renderer, backend, window, rect, 1.0);
            }
        }
    }
    // Each output is its own viewport into the same normalized transition.
    // Clipping to its translated source prevents windows on another monitor
    // from leaking across a mixed-scale output boundary during the slide.
    for plane in &scene.planes {
        let direction = if Some(plane.workspace) == scene.previous { -1.0 } else if Some(plane.workspace) == scene.next { 1.0 } else { 0.0 };
        let shift = plane_shift(direction, scene.position, viewport.size.w);
        if shift.unsigned_abs() >= viewport.size.w {
            continue;
        }
        let clip = Rect::new(Point::new(shift, 0), viewport.size);
        let shifted_view = Rect::new(
            Point::new(viewport.pos.x - shift, viewport.pos.y),
            viewport.size,
        );
        let start = elements.len();
        let overview = if plane.workspace == scene.origin {
            backend.overview.as_ref()
        } else {
            plane.overview.as_ref()
        };
        if let Some(overview) = overview.filter(|o| o.covers(viewport)) {
            crate::overview::render(elements, renderer, backend, overview, shifted_view);
        } else {
            for window in &plane.windows {
                if window.sticky { continue; }
                if !overlaps(window.source, viewport) {
                    continue;
                }
                let rect = Rect::new(
                    Point::new(
                        window.source.pos.x - shifted_view.pos.x,
                        window.source.pos.y - viewport.pos.y,
                    ),
                    window.source.size,
                );
                crate::overview::render_window(elements, renderer, backend, window, rect, 1.0);
            }
        }
        if !scene.overview_origin && plane.workspace == scene.origin {
            crate::renderer::push_furniture(elements, renderer, backend, shifted_view, 1.0, false);
        }
        crate::overview::space_background(
            elements,
            renderer,
            backend,
            viewport,
            clip,
            &plane.background,
        );
        crate::renderer::clip_plane(elements, start, clip, &plane.background);
    }
    Color32F::new(0.015, 0.015, 0.018, 1.0)
}
fn plane_shift(neighbor: f64, progress: f64, width: u32) -> i32 {
    ((neighbor - progress) * f64::from(width)).round() as i32
}
fn overlaps(a: Rect, b: Rect) -> bool {
    i64::from(a.pos.x) < i64::from(b.pos.x) + i64::from(b.size.w)
        && i64::from(b.pos.x) < i64::from(a.pos.x) + i64::from(a.size.w)
        && i64::from(a.pos.y) < i64::from(b.pos.y) + i64::from(b.size.h)
        && i64::from(b.pos.y) < i64::from(a.pos.y) + i64::from(a.size.h)
}

pub(crate) fn for_each_neighbor(
    backend: &WaylandBackend,
    mut visit: impl FnMut(&crate::state::WindowRecord),
) {
    let Some(scene) = &backend.gesture_scene else {
        return;
    };
    let target = if scene.position > 0.0 {
        scene.next
    } else if scene.position < 0.0 {
        scene.previous
    } else {
        None
    };
    if let Some(plane) = scene.planes.iter().find(|p| Some(p.workspace) == target) {
        for window in &plane.windows {
            if !backend.scene_index.is_presented(window.window) {
                if let Some(record) = backend
                    .windows
                    .get(&window.window)
                    .filter(|r| r.surface.alive())
                {
                    visit(record);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalized_motion_has_identical_sensitivity_on_mixed_outputs() {
        // 1x 1080p, fractional 1440p, 2x 4K: scale is deliberately not
        // an input to the transform. Per-output widths affect travel only.
        for width in [1920, 2560, 3840] {
            for progress in [-0.75, -0.25, 0.0, 0.125, 0.5, 1.0] {
                assert!(
                    (f64::from(plane_shift(0.0, progress, width)) / f64::from(width) + progress)
                        .abs()
                        < 1e-6
                );
                assert_eq!(
                    plane_shift(1.0, progress, width) - plane_shift(0.0, progress, width),
                    width as i32
                );
            }
        }
    }
    #[test]
    fn overview_interpolation_is_reversible_and_preserves_endpoints() {
        let source = Rect::new(Point::new(2000, 70), wm_theme_api::Size::new(800, 600));
        let target = Rect::new(Point::new(180, 250), wm_theme_api::Size::new(400, 300));
        assert_eq!(crate::overview::interpolate(source, target, 0.0), source);
        assert_eq!(crate::overview::interpolate(source, target, 1.0), target);
        let half = crate::overview::interpolate(source, target, 0.5);
        assert_eq!(half.pos, Point::new(1090, 160));
        assert_eq!(half.size, wm_theme_api::Size::new(600, 450));
        assert_eq!(
            crate::overview::interpolate(target, source, 0.75),
            crate::overview::interpolate(source, target, 0.25)
        );
    }
}
