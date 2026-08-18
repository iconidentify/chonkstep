use std::collections::{HashMap, HashSet, VecDeque};

use wm_theme_api::{
    ButtonKind, DecorationBuffer, DecorationLayout, DecorationRequest, Point, Rect, ResizeEdge,
    Size, ThemeEngine,
};

use crate::backend::Backend;
use crate::client::MonitorInfo;
use crate::types::{BackendEvent, DragHandle, KeyCombo, MouseButton, SizeHints, WmClass, WmProtocol};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FakeWindowId(pub u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FakeFrameId(pub u64);

/// In-memory `Backend` double for unit-testing `wm-core`'s state
/// machine, focus policy, and hit-testing without any X server. Every
/// side effect a real backend would perform (mapping, painting,
/// destroying, focusing...) is instead recorded in a public field so
/// tests can assert on it directly.
#[derive(Default)]
pub struct FakeBackend {
    next_id: u64,
    queued_events: VecDeque<BackendEvent<FakeWindowId, FakeFrameId>>,
    titles: HashMap<FakeWindowId, String>,
    geometries: HashMap<FakeWindowId, Rect>,
    hints: HashMap<FakeWindowId, SizeHints>,
    monitors: Vec<MonitorInfo>,

    pub mapped_frames: HashSet<FakeFrameId>,
    pub unmapped_frames: HashSet<FakeFrameId>,
    pub destroyed_frames: HashSet<FakeFrameId>,
    pub painted_frames: HashSet<FakeFrameId>,
    /// How many times each frame has been painted — unlike
    /// `painted_frames` (a set, so it can't distinguish "painted once at
    /// creation" from "painted again after a later remap"), this lets a
    /// test assert a *fresh* repaint actually happened, not just that
    /// one happened at some point in the frame's history.
    pub paint_count: HashMap<FakeFrameId, u32>,
    pub last_frame_geometry: HashMap<FakeFrameId, Rect>,
    pub close_requests: HashSet<FakeWindowId>,
    pub focused_window: Option<FakeWindowId>,
    pub raised_frames: Vec<FakeFrameId>,
    /// Whether each client's own content window is currently mapped —
    /// defaults to "mapped" (absent from the map) the moment a window is
    /// created, matching a real client that maps itself before the WM
    /// takes over.
    pub client_mapped: HashMap<FakeWindowId, bool>,
    /// Windows with a currently-active passive button grab — a set, not
    /// a call counter, since what a test cares about is the *current*
    /// grabbed/ungrabbed state (matching real X11: grabbing an
    /// already-grabbed button is idempotent, not cumulative).
    pub passively_grabbed: HashSet<FakeWindowId>,
    pub replay_pointer_calls: u32,
    /// The last cursor `set_frame_cursor` set for each frame — `None`
    /// means "explicitly set to the default cursor", distinct from a
    /// frame that's never had its cursor touched at all (absent from
    /// the map).
    pub frame_cursor: HashMap<FakeFrameId, Option<ResizeEdge>>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn create_window(&mut self) -> FakeWindowId {
        self.next_id += 1;
        FakeWindowId(self.next_id)
    }

    pub fn push_event(&mut self, event: BackendEvent<FakeWindowId, FakeFrameId>) {
        self.queued_events.push_back(event);
    }

    pub fn set_title(&mut self, window: FakeWindowId, title: impl Into<String>) {
        self.titles.insert(window, title.into());
    }

    pub fn set_geometry(&mut self, window: FakeWindowId, geometry: Rect) {
        self.geometries.insert(window, geometry);
    }

    pub fn set_size_hints(&mut self, window: FakeWindowId, hints: SizeHints) {
        self.hints.insert(window, hints);
    }

    /// Sets the single monitor `Backend::monitors()` reports — tests
    /// default to none (matching a "no screen bounds known" baseline),
    /// so snapping/maximize tests that care about screen edges opt in
    /// explicitly.
    pub fn set_monitor(&mut self, geometry: Rect) {
        self.monitors = vec![MonitorInfo { geometry, name: "test-screen".to_string() }];
    }
}

const DEFAULT_GEOMETRY: Rect = Rect { pos: Point { x: 0, y: 0 }, size: Size { w: 200, h: 150 } };

impl Backend for FakeBackend {
    type WindowId = FakeWindowId;
    type FrameId = FakeFrameId;

    fn scan_existing_windows(&mut self) -> Vec<Self::WindowId> {
        Vec::new()
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.clone()
    }

    fn poll_event(&mut self) -> Option<BackendEvent<Self::WindowId, Self::FrameId>> {
        self.queued_events.pop_front()
    }

    fn window_title(&self, window: Self::WindowId) -> Option<String> {
        self.titles.get(&window).cloned()
    }

    fn window_class(&self, _window: Self::WindowId) -> Option<WmClass> {
        None
    }

    fn window_pid(&self, _window: Self::WindowId) -> Option<u32> {
        None
    }

    fn size_hints(&self, window: Self::WindowId) -> SizeHints {
        self.hints.get(&window).copied().unwrap_or_default()
    }

    fn supports_protocol(&self, _window: Self::WindowId, _protocol: WmProtocol) -> bool {
        false
    }

    fn window_geometry(&self, window: Self::WindowId) -> Rect {
        self.geometries.get(&window).copied().unwrap_or(DEFAULT_GEOMETRY)
    }

    fn capture_window_image(&self, _window: Self::WindowId, _size: Size) -> Option<DecorationBuffer> {
        None
    }

    fn create_decoration(&mut self, _window: Self::WindowId, _layout: &DecorationLayout) -> Self::FrameId {
        self.next_id += 1;
        FakeFrameId(self.next_id)
    }

    fn destroy_decoration(&mut self, frame: Self::FrameId) {
        self.destroyed_frames.insert(frame);
    }

    fn paint_decoration(&mut self, frame: Self::FrameId, _buffer: &DecorationBuffer) {
        self.painted_frames.insert(frame);
        *self.paint_count.entry(frame).or_insert(0) += 1;
    }

    fn set_frame_cursor(&mut self, frame: Self::FrameId, edge: Option<ResizeEdge>) {
        self.frame_cursor.insert(frame, edge);
    }

    fn set_frame_geometry(&mut self, frame: Self::FrameId, geometry: Rect) {
        self.last_frame_geometry.insert(frame, geometry);
    }

    fn resize_client(&mut self, _window: Self::WindowId, _size: Size) {}

    fn configure_unmanaged(&mut self, _window: Self::WindowId, _geometry: Rect) {}

    fn map_frame(&mut self, frame: Self::FrameId) {
        self.unmapped_frames.remove(&frame);
        self.mapped_frames.insert(frame);
    }

    fn unmap_frame(&mut self, frame: Self::FrameId) {
        self.mapped_frames.remove(&frame);
        self.unmapped_frames.insert(frame);
    }

    fn set_client_mapped(&mut self, window: Self::WindowId, mapped: bool) {
        self.client_mapped.insert(window, mapped);
    }

    fn raise(&mut self, frame: Self::FrameId) {
        self.raised_frames.push(frame);
    }

    fn restack(&mut self, _order_back_to_front: &[Self::FrameId]) {}

    fn set_input_focus(&mut self, window: Self::WindowId) {
        self.focused_window = Some(window);
    }

    fn send_close(&mut self, window: Self::WindowId) {
        self.close_requests.insert(window);
    }

    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        DragHandle(0)
    }
    fn ungrab_pointer(&mut self, _handle: DragHandle) {}
    fn grab_key(&mut self, _combo: KeyCombo) {}
    fn ungrab_key(&mut self, _combo: KeyCombo) {}
    fn grab_button_passive(&mut self, window: Self::WindowId, _button: MouseButton) {
        self.passively_grabbed.insert(window);
    }
    fn ungrab_button_passive(&mut self, window: Self::WindowId, _button: MouseButton) {
        self.passively_grabbed.remove(&window);
    }
    fn replay_pointer(&mut self) {
        self.replay_pointer_calls += 1;
    }
}

/// In-memory `ThemeEngine` double: fixed 20px titlebar, two 14x14
/// buttons (close top-left, miniaturize top-right — matching real
/// WindowMaker's convention), plus south-facing resize hitboxes
/// (`South`/`SouthEast`/`SouthWest`, matching the real flagship
/// theme's shape) so resize-drag tests can exercise them without an X
/// server. `render` fills a buffer of the right size with a constant
/// color; tests only care that a buffer of the right shape was
/// produced, not its contents.
pub struct FakeTheme;

const TITLEBAR_HEIGHT: u32 = 20;
const BUTTON_SIZE: u32 = 14;
const RESIZE_HANDLE: u32 = 10;

impl ThemeEngine for FakeTheme {
    fn layout(&self, request: &DecorationRequest) -> DecorationLayout {
        let frame_size = Size::new(request.content_size.w, request.content_size.h + TITLEBAR_HEIGHT);
        DecorationLayout {
            frame_size,
            client_offset: Point::new(0, TITLEBAR_HEIGHT as i32),
            titlebar_height: TITLEBAR_HEIGHT,
            button_hitboxes: vec![
                (ButtonKind::Close, Rect::new(Point::new(2, 2), Size::new(BUTTON_SIZE, BUTTON_SIZE))),
                (
                    ButtonKind::Miniaturize,
                    Rect::new(
                        Point::new(frame_size.w as i32 - BUTTON_SIZE as i32 - 2, 2),
                        Size::new(BUTTON_SIZE, BUTTON_SIZE),
                    ),
                ),
            ],
            resize_hitboxes: {
                let handle = RESIZE_HANDLE.min(frame_size.w / 2).min(frame_size.h / 2).max(1);
                vec![
                    (
                        ResizeEdge::SouthEast,
                        Rect::new(
                            Point::new(frame_size.w as i32 - handle as i32, frame_size.h as i32 - handle as i32),
                            Size::new(handle, handle),
                        ),
                    ),
                    (ResizeEdge::SouthWest, Rect::new(Point::new(0, frame_size.h as i32 - handle as i32), Size::new(handle, handle))),
                    (
                        ResizeEdge::South,
                        Rect::new(
                            Point::new(handle as i32, frame_size.h as i32 - handle as i32),
                            Size::new(frame_size.w.saturating_sub(handle * 2), handle),
                        ),
                    ),
                ]
            },
            shaded_frame_height: TITLEBAR_HEIGHT,
        }
    }

    fn render(&self, _request: &DecorationRequest, layout: &DecorationLayout) -> DecorationBuffer {
        let (w, h) = (layout.frame_size.w, layout.frame_size.h);
        DecorationBuffer { width: w, height: h, pixels: vec![128u8; (w * h * 4) as usize] }
    }
}
