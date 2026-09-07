//! User-facing capture. Selection is compositor-owned; PNG, clipboard and
//! recorder I/O run on one bounded worker. The overlay is added only to scanout,
//! never to the scenes exported through screencopy / image-copy-capture.

mod worker;

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Instant;

use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::Color32F;
use smithay::input::pointer::CursorImageStatus;
use smithay::utils::{Physical, Rectangle as SRect};
use wm_config::CaptureMode;
use wm_core::{Backend, KeyCombo, Modifiers};
use wm_theme::model::{Color, FontSpec, FontStyle, FontWeight, TextAlign};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use crate::renderer::SceneElement;
use crate::state::{Compositor, StackEntry, WaylandBackend, WlWindowId};
use worker::{Job, Update};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Screen,
    Window,
    Area,
    RecordScreen,
    RecordArea,
}

impl Mode {
    fn recording(self) -> bool {
        matches!(self, Self::RecordScreen | Self::RecordArea)
    }
    fn area(self) -> bool {
        matches!(self, Self::Area | Self::RecordArea)
    }
}

pub(crate) struct Overlay {
    mode: Mode,
    quick: bool,
    /// A recording indicator is clickable but must never grab client input.
    badge: bool,
    monitor: Rect,
    selection: Option<Rect>,
    window: Option<WlWindowId>,
    drag: Option<(Point, Option<Rect>)>,
    move_selection: bool,
    toolbar: Rect,
    label: Option<MemoryRenderBuffer>,
    ids: [Id; 16],
    armed: Option<usize>,
}

pub(crate) struct Service {
    jobs: SyncSender<Job>,
    updates: Receiver<Update>,
    worker: Option<std::thread::JoinHandle<()>>,
    fonts: Option<(cosmic_text::FontSystem, cosmic_text::SwashCache)>,
    buttons: HashSet<u32>,
    recording: bool,
    finishing: bool,
    started: Instant,
    last_second: u64,
    screenshots: usize,
    label_dirty: bool,
    label_deadline: Instant,
}

impl Service {
    pub fn new() -> Self {
        let (jobs, updates, worker) = worker::start();
        Self {
            jobs,
            updates,
            worker: Some(worker),
            fonts: None,
            buttons: HashSet::new(),
            recording: false,
            finishing: false,
            started: Instant::now(),
            last_second: 0,
            screenshots: 0,
            label_dirty: false,
            label_deadline: Instant::now(),
        }
    }

    fn submit(&self, job: Job) -> bool {
        if let Err(error) = self.jobs.try_send(job) {
            tracing::warn!(%error, "capture worker busy; request not queued");
            return false;
        }
        true
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        // Only at orderly logout, never in the input/render loop. The worker
        // signals and reaps its own children, preserving unfinished recordings.
        let _ = self.jobs.send(Job::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

pub(crate) fn modal(backend: &WaylandBackend) -> bool {
    backend.capture_ui.as_ref().is_some_and(|ui| !ui.badge)
}

pub(crate) fn crosshair(backend: &WaylandBackend) -> Option<Point> {
    let ui = backend.capture_ui.as_ref().filter(|ui| !ui.badge)?;
    backend.pointer.filter(|p| !ui.toolbar.contains(*p))
}

pub(crate) fn begin(comp: &mut Compositor, mode: CaptureMode) {
    if mode == CaptureMode::Stop {
        stop(comp);
        return;
    }
    if comp.wm.backend().locked {
        return;
    }
    if comp.capture_tool.recording || comp.capture_tool.finishing {
        if mode == CaptureMode::Toolbar {
            stop(comp);
        }
        return;
    }
    if modal(comp.wm.backend()) {
        dismiss(comp);
        return;
    }
    // Never steal an existing modal session or a drag's held buttons.
    if comp.wm.backend().keyboard_grabbed
        || comp.wm.backend().pointer_grab.is_some()
        || comp.wm.interactive_drag_active()
        || crate::input::capture_pointer_busy(&comp.seat)
        || comp.focus_grab.is_active()
        || comp.layer_shell.exclusive_focus.is_some()
        || comp.seat.get_pointer().is_some_and(|p| p.is_grabbed())
    {
        return;
    }
    if mode == CaptureMode::Screen {
        let rect = Rect::new(Point::new(0, 0), comp.wm.backend().output_size);
        photograph(comp, rect, None);
        return;
    }
    let at = pointer(comp);
    let Some(monitor) = comp
        .wm
        .backend()
        .monitors
        .iter()
        .find(|m| m.geometry.contains(at))
        .or_else(|| comp.wm.backend().monitors.first())
        .map(|m| m.geometry)
    else {
        return;
    };
    let scale = comp.wm.backend().scale_at(monitor).clamp(1.0, 3.0) as f32;
    let width = (720.0 * scale).min(monitor.size.w.saturating_sub(16) as f32) as u32;
    let height = (86.0 * scale) as u32;
    let toolbar = Rect::new(
        Point::new(
            monitor.pos.x + (monitor.size.w - width) as i32 / 2,
            monitor.pos.y + monitor.size.h as i32 - height as i32 - (24.0 * scale) as i32,
        ),
        Size::new(width, height),
    );
    let selected_mode = if mode == CaptureMode::Window {
        Mode::Window
    } else {
        Mode::Area
    };
    comp.wm.backend_mut().capture_ui = Some(Overlay {
        mode: selected_mode,
        quick: mode != CaptureMode::Toolbar,
        badge: false,
        monitor,
        selection: None,
        window: None,
        drag: None,
        move_selection: false,
        toolbar,
        label: None,
        ids: std::array::from_fn(|_| Id::new()),
        armed: None,
    });
    crate::input::release_pointer_constraint(comp);
    comp.wm.backend_mut().grab_keyboard();
    crate::input::reset_client_input_focus(comp);
    comp.cursor_status = CursorImageStatus::default_named();
    motion(comp, at);
    repaint(comp);
}

fn pointer(comp: &Compositor) -> Point {
    Point::new(
        comp.pointer_location.x.floor() as i32,
        comp.pointer_location.y.floor() as i32,
    )
}

fn dismiss(comp: &mut Compositor) {
    let was_modal = modal(comp.wm.backend());
    comp.wm.backend_mut().capture_ui = None;
    if was_modal {
        comp.wm.backend_mut().ungrab_keyboard();
    }
    comp.wm.backend_mut().mark_damaged();
    crate::input::sync_pointer_focus(comp);
}

fn photograph(comp: &mut Compositor, rect: Rect, window: Option<WlWindowId>) -> bool {
    // Bound readback memory as well as worker queue length, including the image
    // currently being encoded. Repeated shortcuts cannot accumulate 4K buffers.
    if comp.capture_tool.screenshots >= 2 {
        comp.capture_tool.submit(Job::Error(
            "Finishing previous screenshots; please try again shortly".into(),
        ));
        return false;
    }
    match crate::capture::capture_user_pixels(comp, rect, window) {
        Some(pixels) => {
            if comp.capture_tool.submit(Job::Screenshot(pixels)) {
                comp.capture_tool.screenshots += 1;
                return true;
            }
        }
        None => {
            comp.capture_tool
                .submit(Job::Error("Could not capture the selected pixels".into()));
        }
    }
    false
}

fn commit(comp: &mut Compositor) {
    let Some(ui) = comp.wm.backend().capture_ui.as_ref() else {
        return;
    };
    let mode = ui.mode;
    let Some(rect) = ui.selection.filter(|r| r.size.w > 0 && r.size.h > 0) else {
        return;
    };
    let window = ui.window;
    if mode.recording() {
        // wf-recorder owns one output. Refuse a cross-output selection rather
        // than silently cutting off the part on the other monitor.
        let output = comp
            .wm
            .backend()
            .monitors
            .iter()
            .find(|m| contains_rect(m.geometry, rect))
            .map(|m| (m.name.clone(), m.geometry));
        let Some((output, monitor)) = output else {
            comp.capture_tool.submit(Job::Error(
                "Keep a recording area inside one display".into(),
            ));
            return;
        };
        let Some(entry) = comp
            .outputs
            .iter()
            .find(|entry| entry.position == monitor.pos)
        else {
            return;
        };
        let Some((geometry, filter)) = recording_region(
            rect,
            monitor,
            entry.output.current_scale().integer_scale(),
            entry.transform,
        ) else {
            comp.capture_tool.submit(Job::Error(
                "This recording region cannot be represented by the output's capture grid".into(),
            ));
            return;
        };
        if comp.capture_tool.submit(Job::Record {
            output,
            geometry,
            filter,
        }) {
            dismiss(comp);
            comp.capture_tool.recording = true;
            comp.capture_tool.started = Instant::now();
            comp.capture_tool.last_second = 0;
            let scale = comp.wm.backend().scale_at(monitor).clamp(1.0, 3.0);
            let width = ((280.0 * scale) as u32).min(monitor.size.w);
            comp.wm.backend_mut().capture_ui = Some(Overlay {
                mode,
                quick: false,
                badge: true,
                monitor,
                selection: None,
                window: None,
                drag: None,
                move_selection: false,
                toolbar: Rect::new(
                    Point::new(
                        monitor.pos.x + (monitor.size.w - width) as i32 / 2,
                        monitor.pos.y + (12.0 * scale) as i32,
                    ),
                    Size::new(width, (42.0 * scale) as u32),
                ),
                label: None,
                ids: std::array::from_fn(|_| Id::new()),
                armed: None,
            });
            repaint(comp);
        }
    } else if photograph(comp, rect, window) {
        dismiss(comp);
    }
}

fn stop(comp: &mut Compositor) {
    if comp.capture_tool.recording
        && !comp.capture_tool.finishing
        && comp.capture_tool.submit(Job::Stop)
    {
        comp.capture_tool.finishing = true;
        repaint(comp);
    }
}

pub(crate) fn key(comp: &mut Compositor, combo: &KeyCombo) -> bool {
    if !modal(comp.wm.backend()) {
        return false;
    }
    if combo.modifiers.contains(Modifiers::SUPER) {
        if let Some(chonk_shell::shell::KeyResolution::Action(wm_config::Action::Capture(mode))) =
            comp.shell.keymap_action(combo)
        {
            begin(comp, mode);
            return true;
        }
    }
    match combo.keysym {
        0xff1b => dismiss(comp), // Escape, including during a held drag.
        0xff0d | 0xff8d => commit(comp),
        0x20 => {
            let at = pointer(comp);
            let ui = comp.wm.backend_mut().capture_ui.as_mut().unwrap();
            if ui.drag.is_some() {
                ui.move_selection = !ui.move_selection;
                ui.drag = Some((at, ui.selection));
            } else {
                set_mode(
                    comp,
                    if comp.wm.backend().capture_ui.as_ref().unwrap().mode == Mode::Window {
                        Mode::Area
                    } else {
                        Mode::Window
                    },
                );
            }
        }
        0x31..=0x35 => set_mode(
            comp,
            [
                Mode::Screen,
                Mode::Window,
                Mode::Area,
                Mode::RecordScreen,
                Mode::RecordArea,
            ][(combo.keysym - 0x31) as usize],
        ),
        0xff51..=0xff54 => {
            if let Some(ui) = comp
                .wm
                .backend_mut()
                .capture_ui
                .as_mut()
                .filter(|ui| ui.mode.area())
            {
                let step = if combo.modifiers.contains(Modifiers::SHIFT) {
                    10
                } else {
                    1
                };
                let (dx, dy) = match combo.keysym {
                    0xff51 => (-step, 0),
                    0xff52 => (0, -step),
                    0xff53 => (step, 0),
                    _ => (0, step),
                };
                ui.selection = ui.selection.map(|r| moved_rect(r, dx, dy, ui.monitor));
            }
            repaint(comp);
        }
        _ => {}
    }
    true
}

fn set_mode(comp: &mut Compositor, mode: Mode) {
    if let Some(ui) = comp.wm.backend_mut().capture_ui.as_mut() {
        ui.mode = mode;
        ui.selection = matches!(mode, Mode::Screen | Mode::RecordScreen).then_some(ui.monitor);
        ui.window = None;
        ui.drag = None;
    }
    motion(comp, pointer(comp));
    repaint(comp);
}

/// Returns true only when the capture UI owns this motion.
pub(crate) fn motion(comp: &mut Compositor, at: Point) -> bool {
    if !modal(comp.wm.backend()) {
        return false;
    }
    if comp
        .wm
        .backend()
        .capture_ui
        .as_ref()
        .is_some_and(|ui| ui.toolbar.contains(at) && ui.drag.is_none())
    {
        return true;
    }
    let window = hovered_window(comp.wm.backend(), at);
    let monitor = comp
        .wm
        .backend()
        .monitors
        .iter()
        .find(|m| m.geometry.contains(at))
        .map(|m| m.geometry);
    let ui = comp.wm.backend_mut().capture_ui.as_mut().unwrap();
    let previous = ui.selection;
    match ui.mode {
        Mode::Window => {
            ui.window = window.map(|(id, _)| id);
            ui.selection = window.map(|(_, r)| r);
        }
        Mode::Screen | Mode::RecordScreen => {
            ui.selection = monitor;
        }
        Mode::Area | Mode::RecordArea => {
            if let Some((anchor, original)) = ui.drag {
                ui.selection = if ui.move_selection {
                    original.map(|r| moved_rect(r, at.x - anchor.x, at.y - anchor.y, ui.monitor))
                } else {
                    Some(drag_rect(anchor, at))
                };
            }
        }
    }
    if previous != ui.selection {
        comp.capture_tool.label_dirty = true;
    }
    comp.wm.backend_mut().mark_damaged();
    true
}

pub(crate) fn button(comp: &mut Compositor, code: u32, pressed: bool) -> bool {
    let at = pointer(comp);
    let capture_hit = comp
        .wm
        .backend()
        .capture_ui
        .as_ref()
        .is_some_and(|ui| !ui.badge || ui.toolbar.contains(at));
    let held = comp.capture_tool.buttons.contains(&code);
    if !capture_hit && !held {
        return false;
    }
    if pressed {
        comp.capture_tool.buttons.insert(code);
    } else {
        comp.capture_tool.buttons.remove(&code);
    }
    let Some(ui) = comp.wm.backend().capture_ui.as_ref() else {
        return true;
    };
    if ui.badge {
        if code == 0x110 && !pressed && held && capture_hit {
            stop(comp);
        }
        return true;
    }
    if code == 0x111 && pressed {
        dismiss(comp);
        return true;
    }
    if code != 0x110 {
        return true;
    }
    let hit = toolbar_hit(ui, at);
    if pressed {
        let ui = comp.wm.backend_mut().capture_ui.as_mut().unwrap();
        ui.armed = hit;
        if hit.is_none() && ui.mode.area() {
            let corner = ui
                .selection
                .and_then(|r| resize_anchor(r, at, (ui.toolbar.size.h / 10).max(6) as i32));
            let original = ui.selection.filter(|r| r.contains(at) && corner.is_none());
            ui.move_selection = original.is_some();
            ui.drag = Some((corner.unwrap_or(at), original));
            if original.is_none() && corner.is_none() {
                ui.selection = None;
            }
        }
    } else {
        let ui = comp.wm.backend_mut().capture_ui.as_mut().unwrap();
        let armed = ui.armed.take();
        ui.drag = None;
        if let Some(index) = hit.filter(|h| Some(*h) == armed) {
            match index {
                0 => dismiss(comp),
                1..=5 => set_mode(
                    comp,
                    [
                        Mode::Screen,
                        Mode::Window,
                        Mode::Area,
                        Mode::RecordScreen,
                        Mode::RecordArea,
                    ][index - 1],
                ),
                _ => commit(comp),
            }
        } else if armed.is_none() && ui.quick {
            commit(comp);
        }
    }
    repaint(comp);
    true
}

fn toolbar_hit(ui: &Overlay, at: Point) -> Option<usize> {
    ui.toolbar
        .contains(at)
        .then(|| (((at.x - ui.toolbar.pos.x) as u32 * 7 / ui.toolbar.size.w) as usize).min(6))
}

fn hovered_window(backend: &WaylandBackend, at: Point) -> Option<(WlWindowId, Rect)> {
    backend.stacking.iter().rev().find_map(|entry| {
        let (window, rect) = match entry {
            StackEntry::Window(id) => (*id, backend.windows.get(id)?.content),
            StackEntry::Frame(id) => {
                let frame = backend.frames.get(id)?;
                if !frame.mapped {
                    return None;
                }
                (frame.window, frame.geometry)
            }
        };
        backend
            .windows
            .get(&window)
            .filter(|r| r.mapped && rect.contains(at))
            .map(|_| (window, rect))
    })
}

fn contains_rect(outer: Rect, inner: Rect) -> bool {
    inner.pos.x >= outer.pos.x
        && inner.pos.y >= outer.pos.y
        && inner.pos.x as i64 + inner.size.w as i64 <= outer.pos.x as i64 + outer.size.w as i64
        && inner.pos.y as i64 + inner.size.h as i64 <= outer.pos.y as i64 + outer.size.h as i64
}

/// Request an even, output-logical enclosure, then crop in physical pixels.
/// wf-recorder truncates odd SHM buffers before filtering. Expanding the request
/// first preserves the last selected row/column; padding then makes H.264 happy.
fn recording_region(
    rect: Rect,
    output: Rect,
    scale: i32,
    transform: smithay::utils::Transform,
) -> Option<(String, String)> {
    use smithay::utils::Transform;
    if !contains_rect(output, rect) || scale < 1 {
        return None;
    }
    let axis = |start: i32, size: u32, limit: u32| {
        let quantum = if scale % 2 == 0 { scale } else { scale * 2 };
        let mut first = start / quantum * quantum;
        let mut end = ((start + size as i32 + quantum - 1) / quantum) * quantum;
        if end > limit as i32 {
            end = limit as i32 / quantum * quantum;
            if end < start + size as i32 {
                return None;
            }
        }
        if end <= first {
            first = end - quantum;
        }
        Some((first / scale, (end - first) / scale, start - first))
    };
    let (x, w, crop_x) = axis(rect.pos.x - output.pos.x, rect.size.w, output.size.w)?;
    let (y, h, crop_y) = axis(rect.pos.y - output.pos.y, rect.size.h, output.size.h)?;
    let orientation = match transform {
        Transform::Normal => "",
        Transform::_90 => "transpose=1,",
        Transform::_180 => "hflip,vflip,",
        Transform::_270 => "transpose=2,",
        Transform::Flipped => "hflip,",
        Transform::Flipped90 => "transpose=1,hflip,",
        Transform::Flipped180 => "vflip,",
        Transform::Flipped270 => "transpose=2,hflip,",
    };
    // An explicit transform in -F disables wf-recorder's automatic transform;
    // normalizing BEFORE crop keeps the same coordinates on rotated displays.
    Some((
        format!("{},{} {w}x{h}", output.pos.x + x, output.pos.y + y),
        format!(
            "{orientation}crop={}:{}:{crop_x}:{crop_y}:exact=1,pad=ceil(iw/2)*2:ceil(ih/2)*2",
            rect.size.w, rect.size.h
        ),
    ))
}

fn drag_rect(a: Point, b: Point) -> Rect {
    Rect::new(
        Point::new(a.x.min(b.x), a.y.min(b.y)),
        Size::new(a.x.abs_diff(b.x), a.y.abs_diff(b.y)),
    )
}

fn resize_anchor(rect: Rect, at: Point, radius: i32) -> Option<Point> {
    let right = rect.pos.x + rect.size.w as i32;
    let bottom = rect.pos.y + rect.size.h as i32;
    [
        (rect.pos, Point::new(right, bottom)),
        (
            Point::new(right, rect.pos.y),
            Point::new(rect.pos.x, bottom),
        ),
        (
            Point::new(rect.pos.x, bottom),
            Point::new(right, rect.pos.y),
        ),
        (Point::new(right, bottom), rect.pos),
    ]
    .into_iter()
    .find(|(p, _)| p.x.abs_diff(at.x) <= radius as u32 && p.y.abs_diff(at.y) <= radius as u32)
    .map(|(_, anchor)| anchor)
}

fn moved_rect(mut rect: Rect, dx: i32, dy: i32, bounds: Rect) -> Rect {
    rect.pos.x = (rect.pos.x + dx).clamp(
        bounds.pos.x,
        bounds.pos.x + bounds.size.w.saturating_sub(rect.size.w) as i32,
    );
    rect.pos.y = (rect.pos.y + dy).clamp(
        bounds.pos.y,
        bounds.pos.y + bounds.size.h.saturating_sub(rect.size.h) as i32,
    );
    rect
}

pub(crate) fn tick(comp: &mut Compositor) {
    if comp.capture_tool.label_dirty && Instant::now() >= comp.capture_tool.label_deadline {
        repaint(comp);
    }
    if modal(comp.wm.backend())
        && (comp.layer_shell.exclusive_focus.is_some() || comp.focus_grab.is_active())
    {
        dismiss(comp);
    }
    if comp.wm.backend().locked {
        if modal(comp.wm.backend()) {
            dismiss(comp);
        }
        stop(comp);
    }
    while let Ok(update) = comp.capture_tool.updates.try_recv() {
        match update {
            Update::ScreenshotDone => {
                comp.capture_tool.screenshots = comp.capture_tool.screenshots.saturating_sub(1);
            }
            Update::RecordingEnded => {
                comp.capture_tool.recording = false;
                comp.capture_tool.finishing = false;
                dismiss(comp);
            }
        }
    }
    if comp.capture_tool.recording {
        let second = comp.capture_tool.started.elapsed().as_secs();
        if second != comp.capture_tool.last_second {
            comp.capture_tool.last_second = second;
            repaint(comp);
        }
    }
    // Output removal/resize cancels stale selection coordinates immediately.
    if comp.wm.backend().capture_ui.as_ref().is_some_and(|ui| {
        !ui.badge
            && !comp
                .wm
                .backend()
                .monitors
                .iter()
                .any(|m| m.geometry == ui.monitor)
    }) {
        dismiss(comp);
    }
}

fn repaint(comp: &mut Compositor) {
    comp.capture_tool.label_dirty = false;
    comp.capture_tool.label_deadline = Instant::now() + std::time::Duration::from_millis(33);
    let Some(ui) = comp.wm.backend_mut().capture_ui.as_mut() else {
        return;
    };
    let (fonts, cache) = comp.capture_tool.fonts.get_or_insert_with(|| {
        (
            cosmic_text::FontSystem::new(),
            cosmic_text::SwashCache::new(),
        )
    });
    let Some(mut pixmap) = tiny_skia::Pixmap::new(ui.toolbar.size.w, ui.toolbar.size.h) else {
        return;
    };
    let scale = ui.toolbar.size.h as f32 / if ui.badge { 42.0 } else { 86.0 };
    let font = FontSpec {
        family: "sans-serif".into(),
        size: 12.0 * scale,
        weight: FontWeight::Normal,
        style: FontStyle::Normal,
    };
    let mut paint = tiny_skia::Paint::default();
    paint.set_color_rgba8(24, 27, 33, 246);
    if let Some(path) = rounded_rect(
        ui.toolbar.size.w as f32,
        ui.toolbar.size.h as f32,
        12.0 * scale,
    ) {
        pixmap.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::identity(),
            None,
        );
    }
    if ui.badge {
        let secs = comp.capture_tool.started.elapsed().as_secs();
        let text = if comp.capture_tool.finishing {
            "Finishing recording…".into()
        } else {
            format!("●  {:02}:{:02}     ■  Stop recording", secs / 60, secs % 60)
        };
        wm_theme::paint::draw_text(
            &mut pixmap,
            fonts,
            cache,
            &text,
            &font,
            Color::rgb(255, 170, 170),
            4,
            0,
            ui.toolbar.size.w - 8,
            ui.toolbar.size.h,
            TextAlign::Center,
        );
    } else {
        let labels = [
            "×  Close",
            "1  Screen",
            "2  Window",
            "3  Area",
            "4  Record",
            "5  Rec area",
            if ui.mode.recording() {
                "Record"
            } else {
                "Capture"
            },
        ];
        let selected = match ui.mode {
            Mode::Screen => 1,
            Mode::Window => 2,
            Mode::Area => 3,
            Mode::RecordScreen => 4,
            Mode::RecordArea => 5,
        };
        let width = ui.toolbar.size.w / 7;
        for (i, label) in labels.iter().enumerate() {
            let x = i as i32 * width as i32;
            if i == selected || i == 6 {
                wm_theme::paint::fill_rect(
                    &mut pixmap,
                    x + 4,
                    (10.0 * scale) as i32,
                    width.saturating_sub(8),
                    (34.0 * scale) as u32,
                    if i == 6 && ui.selection.is_some_and(|r| r.size.w > 0 && r.size.h > 0) {
                        Color::rgb(50, 107, 188)
                    } else {
                        Color::rgb(61, 66, 77)
                    },
                );
            }
            wm_theme::paint::draw_text(
                &mut pixmap,
                fonts,
                cache,
                label,
                &font,
                Color::rgb(246, 247, 249),
                x,
                (8.0 * scale) as i32,
                width,
                (38.0 * scale) as u32,
                TextAlign::Center,
            );
        }
        let dimensions = ui
            .selection
            .map(|r| format!("{} × {} px  ·  ", r.size.w, r.size.h))
            .unwrap_or_default();
        let hint = format!(
            "{dimensions}Drag to select · Space: window / area · Enter: capture · Esc: cancel"
        );
        let font = FontSpec {
            size: 11.0 * scale,
            ..font
        };
        wm_theme::paint::draw_text(
            &mut pixmap,
            fonts,
            cache,
            &hint,
            &font,
            Color::rgb(176, 183, 196),
            8,
            (48.0 * scale) as i32,
            ui.toolbar.size.w.saturating_sub(16),
            (30.0 * scale) as u32,
            TextAlign::Center,
        );
    }
    ui.label = crate::backend_impl::import_buffer(
        &DecorationBuffer {
            width: pixmap.width(),
            height: pixmap.height(),
            pixels: pixmap.take(),
        },
        false,
    );
    comp.wm.backend_mut().mark_damaged();
}

fn rounded_rect(w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut p = tiny_skia::PathBuilder::new();
    p.move_to(r, 0.0);
    p.line_to(w - r, 0.0);
    p.quad_to(w, 0.0, w, r);
    p.line_to(w, h - r);
    p.quad_to(w, h, w - r, h);
    p.line_to(r, h);
    p.quad_to(0.0, h, 0.0, h - r);
    p.line_to(0.0, r);
    p.quad_to(0.0, 0.0, r, 0.0);
    p.close();
    p.finish()
}

/// Scanout-only overlay. Stable solid IDs keep selection damage sparse; no
/// monitor-sized CPU images are allocated while the pointer moves.
pub(crate) fn render(
    elements: &mut Vec<SceneElement<GlesRenderer>>,
    renderer: &mut GlesRenderer,
    backend: &WaylandBackend,
    viewport: Rect,
) {
    if backend.locked {
        return;
    }
    let Some(ui) = backend.capture_ui.as_ref() else {
        return;
    };
    let mut overlay = Vec::with_capacity(10);
    if let Some(at) = crosshair(backend) {
        for (i, (x, y, w, h)) in [
            (at.x - 10, at.y - 1, 21, 3),
            (at.x - 1, at.y - 10, 3, 21),
            (at.x - 10, at.y, 21, 1),
            (at.x, at.y - 10, 1, 21),
        ]
        .into_iter()
        .enumerate()
        .rev()
        {
            overlay.push(
                SolidColorRenderElement::new(
                    ui.ids[8 + i].clone(),
                    SRect::<i32, Physical>::new(
                        (x - viewport.pos.x, y - viewport.pos.y).into(),
                        (w, h).into(),
                    ),
                    CommitCounter::default(),
                    if i < 2 {
                        Color32F::new(0.0, 0.0, 0.0, 1.0)
                    } else {
                        Color32F::new(1.0, 1.0, 1.0, 1.0)
                    },
                    Kind::Cursor,
                )
                .into(),
            );
        }
    }
    if let Some(buffer) = &ui.label {
        if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (
                (ui.toolbar.pos.x - viewport.pos.x) as f64,
                (ui.toolbar.pos.y - viewport.pos.y) as f64,
            ),
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ) {
            overlay.push(element.into());
        }
    }
    if !ui.badge {
        let selection = ui
            .selection
            .unwrap_or(Rect::new(viewport.pos, Size::new(0, 0)));
        let x1 = selection
            .pos
            .x
            .clamp(viewport.pos.x, viewport.pos.x + viewport.size.w as i32);
        let y1 = selection
            .pos
            .y
            .clamp(viewport.pos.y, viewport.pos.y + viewport.size.h as i32);
        let x2 = (selection.pos.x + selection.size.w as i32)
            .clamp(x1, viewport.pos.x + viewport.size.w as i32);
        let y2 = (selection.pos.y + selection.size.h as i32)
            .clamp(y1, viewport.pos.y + viewport.size.h as i32);
        let right = viewport.pos.x + viewport.size.w as i32;
        let bottom = viewport.pos.y + viewport.size.h as i32;
        let rects = [
            (
                viewport.pos.x,
                viewport.pos.y,
                viewport.size.w,
                (y1 - viewport.pos.y) as u32,
            ),
            (viewport.pos.x, y2, viewport.size.w, (bottom - y2) as u32),
            (
                viewport.pos.x,
                y1,
                (x1 - viewport.pos.x) as u32,
                (y2 - y1) as u32,
            ),
            (x2, y1, (right - x2) as u32, (y2 - y1) as u32),
            (x1, y1, (x2 - x1) as u32, 1),
            (x1, y2, (x2 - x1) as u32, 1),
            (x1, y1, 1, (y2 - y1) as u32),
            (x2, y1, 1, (y2 - y1) as u32),
        ];
        for (i, (x, y, w, h)) in rects.into_iter().enumerate() {
            if w == 0 || h == 0 {
                continue;
            }
            overlay.push(
                SolidColorRenderElement::new(
                    ui.ids[i].clone(),
                    SRect::<i32, Physical>::new(
                        (x - viewport.pos.x, y - viewport.pos.y).into(),
                        (w as i32, h as i32).into(),
                    ),
                    CommitCounter::default(),
                    if i < 4 {
                        Color32F::new(0.0, 0.0, 0.0, 0.42)
                    } else {
                        Color32F::new(1.0, 1.0, 1.0, 0.95)
                    },
                    Kind::Unspecified,
                )
                .into(),
            );
        }
        if ui.mode.area() && ui.selection.is_some() && !ui.quick {
            for (i, (x, y)) in [(x1, y1), (x2, y1), (x1, y2), (x2, y2)]
                .into_iter()
                .enumerate()
            {
                overlay.push(
                    SolidColorRenderElement::new(
                        ui.ids[12 + i].clone(),
                        SRect::<i32, Physical>::new(
                            (x - viewport.pos.x - 3, y - viewport.pos.y - 3).into(),
                            (7, 7).into(),
                        ),
                        CommitCounter::default(),
                        Color32F::new(1.0, 1.0, 1.0, 1.0),
                        Kind::Unspecified,
                    )
                    .into(),
                );
            }
        }
    }
    elements.splice(0..0, overlay);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn area_is_direction_independent_and_half_open() {
        let expected = Rect::new(Point::new(5, 8), Size::new(101, 53));
        assert_eq!(drag_rect(Point::new(106, 8), Point::new(5, 61)), expected);
        assert_eq!(drag_rect(Point::new(5, 61), Point::new(106, 8)), expected);
        assert_eq!(
            drag_rect(Point::new(5, 8), Point::new(5, 8)).size,
            Size::new(0, 0)
        );
    }
    #[test]
    fn recording_cannot_silently_cross_outputs() {
        let output = Rect::new(Point::new(1920, 0), Size::new(2560, 1440));
        assert!(contains_rect(output, output));
        assert!(!contains_rect(
            output,
            Rect::new(Point::new(1919, 0), Size::new(100, 100))
        ));
        assert!(!contains_rect(
            output,
            Rect::new(Point::new(4400, 0), Size::new(100, 100))
        ));
    }
    #[test]
    fn moving_selection_preserves_pixels_and_stays_visible() {
        let bounds = Rect::new(Point::new(1920, 0), Size::new(1280, 720));
        let rect = Rect::new(Point::new(2100, 50), Size::new(101, 53));
        let moved = moved_rect(rect, 4000, -1000, bounds);
        assert_eq!(moved.size, rect.size);
        assert!(contains_rect(bounds, moved));
    }

    #[test]
    fn recorder_region_encloses_device_pixels_before_crop_and_padding() {
        let output = Rect::new(Point::new(1920, 0), Size::new(2560, 1440));
        let selected = Rect::new(Point::new(1971, 41), Size::new(301, 201));
        let (geometry, filter) =
            recording_region(selected, output, 2, smithay::utils::Transform::Flipped180).unwrap();
        assert_eq!(geometry, "1945,20 151x101");
        assert_eq!(
            filter,
            "vflip,crop=301:201:1:1:exact=1,pad=ceil(iw/2)*2:ceil(ih/2)*2"
        );
        let (geometry, _) =
            recording_region(selected, output, 1, smithay::utils::Transform::Normal).unwrap();
        assert_eq!(geometry, "1970,40 302x202");
    }
}
