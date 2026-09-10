//! User-facing capture. Selection is compositor-owned; PNG, clipboard and
//! recorder I/O run on one bounded worker. The overlay is added only to scanout,
//! never to the scenes exported through screencopy / image-copy-capture.

mod chrome;
pub(crate) mod dimming;
mod worker;

use std::collections::HashSet;
use std::sync::mpsc::{Receiver, SyncSender};
use std::time::Instant;

use smithay::backend::renderer::element::memory::{
    MemoryRenderBuffer, MemoryRenderBufferRenderElement,
};
use smithay::backend::renderer::element::solid::SolidColorRenderElement;
use smithay::backend::renderer::element::{Element, Id, Kind};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::CommitCounter;
use smithay::backend::renderer::Color32F;
use smithay::input::pointer::CursorImageStatus;
use smithay::utils::{Physical, Rectangle as SRect};
use wm_config::CaptureMode;
use wm_core::{Backend, KeyCombo, Modifiers};
use wm_theme::FontState;
use wm_theme_api::{Point, Rect, Size};

use crate::renderer::SceneElement;
use crate::state::{Compositor, StackEntry, WaylandBackend, WlWindowId};
use worker::{Destination, Job, Update};

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
    destination: Destination,
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
    hint: Option<MemoryRenderBuffer>,
    camera: Option<MemoryRenderBuffer>,
    dimming: dimming::Dimming,
    ids: [Id; 16],
    armed: Option<usize>,
    armed_window: Option<WlWindowId>,
    hovered: Option<usize>,
}

struct Worker {
    jobs: SyncSender<Job>,
    updates: Receiver<Update>,
    worker: Option<std::thread::JoinHandle<()>>,
}

struct PendingScreenshot {
    image: crate::capture::PendingImage,
    destination: Option<Destination>,
}

pub(crate) struct Service {
    worker: Option<Worker>,
    fonts: FontState,
    chrome: chrome::Cache,
    buttons: HashSet<u32>,
    recording: bool,
    finishing: bool,
    started: Instant,
    last_second: u64,
    screenshots: usize,
    downloads: Vec<PendingScreenshot>,
    download_poll: Instant,
    label_dirty: bool,
    label_deadline: Instant,
}

impl Service {
    pub fn new(fonts: FontState) -> Self {
        Self {
            worker: None,
            fonts,
            chrome: chrome::Cache::default(),
            buttons: HashSet::new(),
            recording: false,
            finishing: false,
            started: Instant::now(),
            last_second: 0,
            screenshots: 0,
            downloads: Vec::new(),
            download_poll: Instant::now(),
            label_dirty: false,
            label_deadline: Instant::now(),
        }
    }

    fn submit(&mut self, job: Job) -> bool {
        // Selecting pixels needs no I/O thread. Start it only when a file,
        // recording or notification is actually requested.
        let worker = self.worker.get_or_insert_with(|| {
            let (jobs, updates, worker) = worker::start();
            Worker {
                jobs,
                updates,
                worker: Some(worker),
            }
        });
        if let Err(error) = worker.jobs.try_send(job) {
            tracing::warn!(%error, "capture worker busy; request not queued");
            return false;
        }
        true
    }
}

impl Drop for Worker {
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

pub(crate) fn selection_cursor(backend: &WaylandBackend) -> Option<Point> {
    let ui = backend.capture_ui.as_ref().filter(|ui| !ui.badge)?;
    backend.pointer.filter(|p| !ui.toolbar.contains(*p))
}

pub(crate) fn owns_cursor(backend: &WaylandBackend, at: Point) -> bool {
    !backend.locked
        && backend
            .capture_ui
            .as_ref()
            .is_some_and(|ui| !ui.badge || ui.toolbar.contains(at))
}

pub(crate) fn begin(comp: &mut Compositor, mode: CaptureMode) {
    let (mode, destination) = match mode {
        CaptureMode::ScreenClipboard => (CaptureMode::Screen, Destination::Clipboard),
        CaptureMode::AreaClipboard => (CaptureMode::Area, Destination::Clipboard),
        CaptureMode::WindowClipboard => (CaptureMode::Window, Destination::Clipboard),
        mode => (mode, if comp.wm.mac_mode() { Destination::File } else { Destination::Legacy }),
    };
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
        photograph(comp, rect, None, destination);
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
        destination,
        quick: mode != CaptureMode::Toolbar,
        badge: false,
        monitor,
        selection: None,
        window: None,
        drag: None,
        move_selection: false,
        toolbar,
        label: None,
        hint: None,
        camera: None,
        dimming: dimming::Dimming::default(),
        ids: std::array::from_fn(|_| Id::new()),
        armed: None,
        armed_window: None,
        hovered: None,
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

fn photograph(comp: &mut Compositor, rect: Rect, window: Option<WlWindowId>, destination: Destination) -> bool {
    // Bound readback memory as well as worker queue length, including the image
    // currently being encoded. Repeated shortcuts cannot accumulate 4K buffers.
    if comp.capture_tool.screenshots >= 2 || comp.capture_tool.downloads.len() >= 2 {
        comp.capture_tool.submit(Job::Error(
            "Finishing previous screenshots; please try again shortly".into(),
        ));
        return false;
    }
    match crate::capture::capture_user_pixels(comp, rect, window) {
        Some(image) => {
            comp.capture_tool.downloads.push(PendingScreenshot { image, destination: Some(destination) });
            comp.capture_tool.download_poll = Instant::now() + std::time::Duration::from_millis(4);
            comp.capture_tool.screenshots += 1;
            return true;
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
    let destination = if ui.destination == Destination::File && comp.wm.mac_mode()
        && comp.seat.get_keyboard().is_some_and(|keyboard| keyboard.modifier_state().ctrl) {
        Destination::Clipboard
    } else { ui.destination };
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
            // Match the transform advertised to the screencopy client. The
            // nested backend adds an EGL flip absent from the layout transform.
            entry.output.current_transform(),
        ) else {
            comp.capture_tool.submit(Job::Error(
                "This recording region cannot be represented by the output's capture grid".into(),
            ));
            return;
        };
        if comp.capture_tool.submit(Job::Record {
            policy: destination,
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
                destination,
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
                hint: None,
                camera: None,
                dimming: dimming::Dimming::default(),
                ids: std::array::from_fn(|_| Id::new()),
                armed: None,
                armed_window: None,
                hovered: None,
            });
            // The badge may appear under a stationary pointer. Retire the
            // previous client's cursor authority immediately, without waiting
            // for the next physical pointer report.
            crate::input::sync_pointer_focus(comp);
            if owns_cursor(comp.wm.backend(), pointer(comp)) {
                comp.cursor_status = CursorImageStatus::default_named();
            }
            repaint(comp);
        }
    } else if photograph(comp, rect, window, destination) {
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
            comp.wm.backend_mut().mark_damaged();
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
        ui.armed = None;
        ui.armed_window = None;
        ui.move_selection = false;
    }
    motion(comp, pointer(comp));
    repaint(comp);
}

/// Returns true only when the capture UI owns this motion.
pub(crate) fn motion(comp: &mut Compositor, at: Point) -> bool {
    if !owns_cursor(comp.wm.backend(), at) {
        return false;
    }
    if comp
        .wm
        .backend()
        .capture_ui
        .as_ref()
        .is_some_and(|ui| ui.badge)
        && (crate::input::capture_pointer_busy(&comp.seat)
            || comp.wm.interactive_drag_active()
            || comp.seat.get_pointer().is_some_and(|p| p.is_grabbed()))
    {
        return false;
    }
    comp.cursor_status = CursorImageStatus::default_named();
    // The cursor is itself damage, including over a stationary toolbar.
    // Chrome changes only when a different control is hovered.
    let ui = comp.wm.backend_mut().capture_ui.as_mut().unwrap();
    let hovered = toolbar_hit(ui, at);
    if ui.hovered != hovered {
        ui.hovered = hovered;
        comp.capture_tool.label_dirty = true;
    }
    comp.wm.backend_mut().mark_damaged();
    if comp
        .wm
        .backend()
        .capture_ui
        .as_ref()
        .is_some_and(|ui| ui.badge || (ui.toolbar.contains(at) && ui.drag.is_none()))
    {
        return true;
    }
    let window = (comp.wm.backend().capture_ui.as_ref().unwrap().mode == Mode::Window)
        .then(|| hovered_window(comp.wm.backend(), at))
        .flatten();
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
    if previous.map(|r| r.size) != ui.selection.map(|r| r.size) {
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
    if comp
        .wm
        .backend()
        .capture_ui
        .as_ref()
        .is_some_and(|ui| ui.badge)
        && !held
        && (!pressed
            || crate::input::capture_pointer_busy(&comp.seat)
            || comp.wm.interactive_drag_active()
            || comp.seat.get_pointer().is_some_and(|p| p.is_grabbed()))
    {
        // A nonmodal badge never acquires the release of an application's
        // earlier press, nor a new button in its active implicit grab.
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
        ui.armed_window = if !ui.toolbar.contains(at) && ui.mode == Mode::Window {
            ui.window
        } else {
            None
        };
        if hit.is_none() && ui.mode.area() && !ui.toolbar.contains(at) {
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
        let armed_window = ui.armed_window.take();
        let dragged = ui.drag.take().is_some();
        // Only finish a click we acquired. In particular, dragging out of a
        // toolbar control or its padding must not photograph a nearby window.
        if !held {
            return true;
        }
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
        } else if armed.is_none()
            && !ui.toolbar.contains(at)
            && ((ui.mode == Mode::Window && armed_window.is_some() && armed_window == ui.window)
                || (ui.quick && ui.mode.area() && dragged))
        {
            commit(comp);
        }
    }
    repaint(comp);
    true
}

fn toolbar_hit(ui: &Overlay, at: Point) -> Option<usize> {
    if !ui.toolbar.contains(at) {
        return None;
    }
    if ui.badge {
        return Some(0);
    }
    let y = (at.y - ui.toolbar.pos.y) as u32;
    // The status line and the outer padding describe the action; only the
    // visible controls activate it.
    if y < ui.toolbar.size.h * 7 / 86 || y >= ui.toolbar.size.h * 57 / 86 {
        return None;
    }
    Some((((at.x - ui.toolbar.pos.x) as u32 * 7 / ui.toolbar.size.w) as usize).min(6))
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

fn service_screenshots(comp: &mut Compositor) {
    let now = Instant::now();
    if comp.capture_tool.downloads.is_empty() || now < comp.capture_tool.download_poll { return; }
    let Compositor { wm, graphics, capture_tool: service, .. } = comp;
    let renderer = crate::capture::graphics_renderer(graphics);
    let mut pending = std::mem::take(&mut service.downloads);
    pending.retain_mut(|pending| {
        if (wm.backend().locked || pending.image.pixels.expired(now)) && pending.destination.take().is_some() {
            service.screenshots = service.screenshots.saturating_sub(1);
            service.submit(Job::Error("Screenshot canceled before its pixels became available".into()));
        }
        // Retire canceled downloads in their original bounded slots until the
        // GPU has finished reading client buffers; never block the input loop.
        if !pending.image.pixels.ready() { return true; }
        pending.image.pixels.release_scene();
        if let Some(destination) = pending.destination.take() {
            let submitted = match pending.image.copy_pixels(renderer) {
                Ok(pixels) => service.submit(Job::Screenshot(pixels, destination)),
                Err(error) => { service.submit(Job::Error(error)); false }
            };
            if !submitted { service.screenshots = service.screenshots.saturating_sub(1); }
        }
        false
    });
    service.download_poll = now + std::time::Duration::from_millis(
        if pending.iter().all(|pending| pending.destination.is_none()) { 100 } else { 4 });
    service.downloads = pending;
}

pub(crate) fn tick(comp: &mut Compositor) {
    service_screenshots(comp);
    if comp.capture_tool.label_dirty && Instant::now() >= comp.capture_tool.label_deadline {
        repaint(comp);
    }
    if modal(comp.wm.backend())
        && (comp.layer_shell.exclusive_focus.is_some() || comp.focus_grab.is_active())
    {
        dismiss(comp);
    }
    if comp.wm.backend().locked {
        comp.capture_tool.buttons.clear();
        if modal(comp.wm.backend()) {
            dismiss(comp);
        }
        stop(comp);
    }
    while let Some(update) = comp
        .capture_tool
        .worker
        .as_ref()
        .and_then(|worker| worker.updates.try_recv().ok())
    {
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

/// Held capture input cannot survive losing the seat: releases may have gone
/// to another session, so cancel the selection and retire its ownership.
pub(crate) fn reset_input(comp: &mut Compositor) {
    comp.capture_tool.buttons.clear();
    if modal(comp.wm.backend()) {
        dismiss(comp);
    }
}

fn repaint(comp: &mut Compositor) {
    comp.capture_tool.label_dirty = false;
    comp.capture_tool.label_deadline = Instant::now() + std::time::Duration::from_millis(33);
    let Some(ui) = comp.wm.backend_mut().capture_ui.as_mut() else {
        return;
    };
    let service = &mut comp.capture_tool;
    if service.chrome.paint(
        ui,
        &service.fonts,
        service.started.elapsed().as_secs(),
        service.finishing,
    ) {
        comp.wm.backend_mut().mark_damaged();
    }
}

/// Only pending label work or a running recording contributes a deadline.
/// A settled selector adds no periodic wakeups to the compositor.
pub(crate) fn deadline(comp: &Compositor) -> Option<Instant> {
    let service = &comp.capture_tool;
    let ui = if service.label_dirty {
        Some(service.label_deadline)
    } else if service.recording && !service.finishing {
        Some(service.started + std::time::Duration::from_secs(service.last_second + 1))
    } else { None };
    ui.into_iter().chain((!service.downloads.is_empty()).then_some(service.download_poll)).min()
}

/// Scanout-only overlay. Stable geometry keeps selection damage sparse; no
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
    // Scenes are front-to-back: keep the existing pointer above the toolbar.
    // Append into the retained scene vector and rotate in place, avoiding a
    // fresh overlay allocation (and growth) on every pointer frame.
    let insertion = elements
        .iter()
        .take_while(|e| e.kind() == Kind::Cursor)
        .count();
    let scene_end = elements.len();
    if let Some(at) = selection_cursor(backend) {
        if ui.mode == Mode::Window {
            if let Some(buffer) = &ui.camera {
                let scale = ui.toolbar.size.h as f64 / 86.0;
                if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
                    renderer,
                    (
                        (at.x - viewport.pos.x) as f64 - 16.0 * scale,
                        (at.y - viewport.pos.y) as f64 - 16.0 * scale,
                    ),
                    buffer,
                    None,
                    None,
                    None,
                    Kind::Cursor,
                ) {
                    elements.push(element.into());
                }
            }
        } else {
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
                elements.push(
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
    }
    if let Some(buffer) = &ui.hint {
        if let Ok(element) = MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            (
                (ui.toolbar.pos.x) as f64 - viewport.pos.x as f64,
                (ui.toolbar.pos.y + chrome::hint_y(ui)) as f64 - viewport.pos.y as f64,
            ),
            buffer,
            None,
            None,
            None,
            Kind::Unspecified,
        ) {
            elements.push(element.into());
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
            elements.push(element.into());
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
        let rects = [
            (x1, y1, (x2 - x1) as u32, 1),
            (x1, y2, (x2 - x1) as u32, 1),
            (x1, y1, 1, (y2 - y1) as u32),
            (x2, y1, 1, (y2 - y1) as u32),
        ];
        for (i, (x, y, w, h)) in rects.into_iter().enumerate() {
            if w == 0 || h == 0 {
                continue;
            }
            elements.push(
                SolidColorRenderElement::new(
                    ui.ids[4 + i].clone(),
                    SRect::<i32, Physical>::new(
                        (x - viewport.pos.x, y - viewport.pos.y).into(),
                        (w as i32, h as i32).into(),
                    ),
                    CommitCounter::default(),
                    Color32F::new(1.0, 1.0, 1.0, 0.95),
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
                elements.push(
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
        elements.push(ui.dimming.element(ui.selection, viewport).into());
    }
    let overlay_len = elements.len() - scene_end;
    elements[insertion..].rotate_right(overlay_len);
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
