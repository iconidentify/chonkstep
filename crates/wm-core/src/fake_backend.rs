use std::collections::{HashMap, HashSet, VecDeque};

use wm_theme_api::{
    ButtonKind, DecorationBuffer, DecorationLayout, DecorationRequest, Point, Rect, ResizeEdge,
    Size, ThemeEngine,
};

use crate::backend::Backend;
use crate::client::MonitorInfo;
use crate::types::{BackendEvent, DragHandle, KeyCombo, MouseButton, ScrollDelta, SizeHints, WindowType, WmClass, WmProtocol};

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
    /// Per-window `_NET_WM_WINDOW_TYPE` the fake reports — absent means
    /// `WindowType::Normal`, matching both the trait default and the
    /// EWMH fallback for windows that declare no type.
    window_types: HashMap<FakeWindowId, WindowType>,
    /// Which windows claim to draw their own chrome (the fake's stand-in
    /// for `_MOTIF_WM_HINTS` and xdg-decoration alike). Absent means
    /// they do not, which is what keeps an ordinary window framed.
    client_drawn_chrome: HashMap<FakeWindowId, bool>,

    /// Windows currently shown *without* a frame — the frameless
    /// counterpart of `mapped_frames`, recorded separately because a
    /// test asserting a client-decorated window is on screen cannot
    /// look for a frame that by definition does not exist.
    pub mapped_frameless: HashSet<FakeWindowId>,
    /// Frames handed back through `release_decoration` rather than
    /// `destroy_decoration`. The distinction is the whole point of that
    /// verb on X11, where the wrong one destroys the client along with
    /// its frame, so the fake records which one a caller reached for.
    pub released_frames: HashSet<FakeFrameId>,
    /// The last `_NET_FRAME_EXTENTS` published per window, in EWMH's
    /// own order: left, right, top, bottom. Recorded because the
    /// property is the only thing that tells a client how big its
    /// chrome is, and "published nothing" and "published zeros" mean
    /// opposite things to one.
    pub frame_extents: HashMap<FakeWindowId, (u32, u32, u32, u32)>,
    /// Windows raised through `raise_frameless`, in call order. A list
    /// rather than a set: "was it ever raised" is a weaker claim than
    /// "was it raised when it was clicked", and only the second one is
    /// the bug.
    pub raised_frameless: Vec<FakeWindowId>,
    /// Every `set_layer_surface_hidden` call, in order, as `(namespace,
    /// hidden)`. Recorded rather than reduced to a set because the
    /// shell promises to *clear* a namespace it no longer has a reason
    /// to hide, and only the sequence shows that.
    pub layer_visibility_calls: Vec<(String, bool)>,
    /// How many pointer grabs are currently outstanding: incremented by
    /// `grab_pointer_for_drag`, decremented by `ungrab_pointer`.
    ///
    /// A count rather than a flag because the number that matters is
    /// zero-or-not after a drag: a grab left behind freezes the pointer
    /// for every client on the session, and a double release would hand
    /// back a grab someone else holds.
    pub outstanding_pointer_grabs: i32,
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
    /// Dimensions of the last `DecorationBuffer` painted into each
    /// frame. A backend that owns no frame window of its own (the
    /// Wayland one composites the buffer directly, at the buffer's own
    /// size) draws exactly this rect and nothing else, so a buffer that
    /// disagrees with `last_frame_geometry` is a visible bug there even
    /// though X11's server-side clipping would hide it.
    pub last_paint_size: HashMap<FakeFrameId, Size>,
    pub last_frame_geometry: HashMap<FakeFrameId, Rect>,
    /// Where each client window was last positioned directly. For a
    /// framed window this is its offset inside its frame; for a
    /// frameless one it is a root position, and it is the only record
    /// of where such a window actually is.
    pub last_client_position: HashMap<FakeWindowId, Point>,
    pub close_requests: HashSet<FakeWindowId>,
    /// Monotonic id source for `create_shell_surface`.
    pub next_shell_id: u32,
    /// Where each shell surface currently is, as the shell last told
    /// this backend — written by both `create_shell_surface` and
    /// `configure_shell_surface`, so it always holds the live answer.
    ///
    /// Recorded because a shell surface that is *repainted* but never
    /// *reconfigured* is a whole class of bug this double could not see
    /// before: the dock and the launcher strip are sized from the UI
    /// scale, and a rescale that redrew them at the new size without
    /// moving the surface underneath would leave the picture right and
    /// every click landing somewhere else. That is the same asymmetry
    /// the Wayland backend documents on `set_frame_geometry` ("a caller
    /// that changes a frame's size without painting a buffer to match
    /// has moved the clickable rect out from under an unchanged
    /// picture"), read from the other end.
    pub shell_geometries: HashMap<u32, Rect>,
    /// Which shell surfaces are currently mapped — `true` after
    /// `map_shell_surface`, `false` after `unmap_shell_surface`, absent
    /// until one or the other is called (`create_shell_surface` maps
    /// nothing, exactly like the real backends).
    ///
    /// The state this double used to throw away, and the half of
    /// "hidden" a geometry cannot show: the Dock is hidden by unmapping
    /// its surface *and* releasing the strip it reserved, and a test
    /// that could only see the second one would pass for a Dock still
    /// sitting visibly in the corner over its own giveaway. A map
    /// (rather than a set of the mapped) so an unmap is distinguishable
    /// from a surface nobody has mapped yet.
    pub shell_mapped: HashMap<u32, bool>,
    /// Bytes in the last buffer painted into each shell surface. Absence
    /// means the backing was never painted or has been explicitly released.
    pub shell_buffer_bytes: HashMap<u32, usize>,
    /// Scroll events waiting for `Backend::take_shell_scroll`, oldest
    /// first. The fake has no input hardware, so tests stage them with
    /// `queue_shell_scroll` — the same shape a real backend's input
    /// machinery pushes (`wm-x11`'s `pending_shell_scrolls`,
    /// `wm-wayland`'s `shell_scrolls`), so a test drives the drain
    /// through the trait exactly as the event loop does, with no
    /// display server anywhere.
    pub queued_shell_scrolls: VecDeque<(u32, Point, ScrollDelta)>,
    /// Windows force-killed via `kill_client`, in call order.
    pub killed: Vec<FakeWindowId>,
    /// Per-window `WM_CLASS` class strings the existing `window_class`
    /// trait method serves through `WmClass` — absent means the client
    /// set no class at all (`None`).
    pub window_classes: HashMap<FakeWindowId, String>,
    pub focused_window: Option<FakeWindowId>,
    pub raised_frames: Vec<FakeFrameId>,
    /// Declared transient parents, as `xdg_toplevel.set_parent` and
    /// `WM_TRANSIENT_FOR` report them — see
    /// [`Self::set_window_parent`].
    pub parents: std::collections::HashMap<FakeWindowId, FakeWindowId>,
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
    /// Windows handed to `map_unmanaged` — the `WindowType::Unmanaged`
    /// path, mapped as-is with no frame or tracking.
    pub unmanaged_mapped: Vec<FakeWindowId>,
    /// Every `publish_client_list` call in order — a history rather
    /// than just the latest list, so a test can assert the list grew on
    /// map and shrank on destroy, not merely inspect the end state.
    pub published_client_lists: Vec<Vec<FakeWindowId>>,
    /// Every `publish_active_window` call in order (`None` = focus
    /// cleared).
    pub published_active_windows: Vec<Option<FakeWindowId>>,
    /// Every `publish_workspaces` call in order, as `(count, current)`.
    pub published_workspaces: Vec<(usize, usize)>,
    /// Every `publish_workarea` call in order, as
    /// `(area, workspace_count)`.
    pub published_workareas: Vec<(Rect, usize)>,
    /// Every `publish_net_state` call in order, as
    /// `(window, fullscreen, max_h, max_v, shaded, hidden)` — matching
    /// the trait method's parameter order exactly.
    pub published_net_states: Vec<(FakeWindowId, bool, bool, bool, bool, bool)>,
    /// Every `publish_window_desktop` call in order, as
    /// `(window, desktop)` — a history, so a test can assert both the
    /// initial manage-time publish and a later move's re-publish, not
    /// just the end state.
    pub published_window_desktops: Vec<(FakeWindowId, usize)>,
    /// Every `grab_key` call in order — a history rather than a set,
    /// so a test can assert exactly which combos were registered and
    /// that nothing beyond them was (e.g. that `bind_default_keys`
    /// claims only the modal cycling combos, everything else being
    /// config-driven from the binary).
    pub grabbed_keys: Vec<KeyCombo>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self {
            monitors: vec![MonitorInfo { geometry: FAKE_SCREEN, name: "test-screen".to_string(), primary: true }],
            ..Self::default()
        }
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

    /// Replaces the fake's monitor list with a single primary monitor
    /// — how a test states the one screen it wants to measure against
    /// instead of the stock `FAKE_SCREEN`.
    pub fn set_monitor(&mut self, geometry: Rect) {
        self.set_monitors(vec![MonitorInfo { geometry, name: "test-screen".to_string(), primary: true }]);
    }

    /// Sets the whole monitor list, in the stable order
    /// `Backend::monitors()` promises — for the multi-head shapes
    /// `set_monitor` cannot express. The caller owns the `primary`
    /// flags: this deliberately does not fix them up, so a test can
    /// exercise a list whose primary is not index 0, and one where the
    /// platform named no primary at all.
    pub fn set_monitors(&mut self, monitors: Vec<MonitorInfo>) {
        self.monitors = monitors;
    }

    /// Sets the `_NET_WM_WINDOW_TYPE` this fake reports for `window` —
    /// windows never set report `WindowType::Normal`, same as the trait
    /// default. Lets tests exercise the `Unmanaged` map path.
    pub fn set_window_type(&mut self, window: FakeWindowId, window_type: WindowType) {
        self.window_types.insert(window, window_type);
    }

    /// Makes `window` claim it has already drawn its own titlebar — the
    /// fake's `_MOTIF_WM_HINTS`. Settable after a map, because real
    /// clients change their minds and the window manager has to follow.
    pub fn set_client_draws_own_chrome(&mut self, window: FakeWindowId, draws: bool) {
        self.client_drawn_chrome.insert(window, draws);
    }

    /// Declares `child` a transient (dialog) child of `parent`, the way
    /// `xdg_toplevel.set_parent` and `WM_TRANSIENT_FOR` do.
    pub fn set_window_parent(&mut self, child: FakeWindowId, parent: FakeWindowId) {
        self.parents.insert(child, parent);
    }

    /// Stages a scroll for the next `take_shell_scroll` drain, as a
    /// real backend would after a wheel notch over `shell` at
    /// `local`.
    ///
    /// Deliberately a plain push with no validation: the fake's job is
    /// to let a test say "the user scrolled here", including saying
    /// things a well-behaved backend would not, so a consumer's
    /// handling of a multi-notch or diagonal delta is reachable
    /// without a mouse.
    pub fn queue_shell_scroll(&mut self, shell: u32, local: Point, delta: ScrollDelta) {
        self.queued_shell_scrolls.push_back((shell, local, delta));
    }

    /// Whether `shell` is mapped right now. A surface nobody has ever
    /// mapped reads as unmapped, which is what a real backend shows: a
    /// created surface is not visible until it is mapped.
    pub fn shell_is_mapped(&self, shell: u32) -> bool {
        self.shell_mapped.get(&shell).copied().unwrap_or(false)
    }
}

const DEFAULT_GEOMETRY: Rect = Rect { pos: Point { x: 0, y: 0 }, size: Size { w: 200, h: 150 } };

/// The one screen a fresh `FakeBackend` reports, as both its
/// `screen_size` and its single primary monitor — the two must agree,
/// since no real backend has a screen its monitor list doesn't cover.
const FAKE_SCREEN: Rect = Rect { pos: Point { x: 0, y: 0 }, size: Size { w: 1600, h: 1200 } };

impl Backend for FakeBackend {
    type WindowId = FakeWindowId;
    type FrameId = FakeFrameId;
    type ShellId = u32;

    fn create_shell_surface(&mut self, geometry: wm_theme_api::Rect, _background: (u8, u8, u8), _above: bool) -> Option<Self::ShellId> {
        self.next_shell_id += 1;
        self.shell_geometries.insert(self.next_shell_id, geometry);
        Some(self.next_shell_id)
    }
    fn map_shell_surface(&mut self, id: Self::ShellId) {
        self.shell_mapped.insert(id, true);
    }
    fn unmap_shell_surface(&mut self, id: Self::ShellId) {
        self.shell_mapped.insert(id, false);
    }
    fn destroy_shell_surface(&mut self, id: Self::ShellId) {
        self.shell_geometries.remove(&id);
        self.shell_mapped.remove(&id);
        self.shell_buffer_bytes.remove(&id);
    }
    fn raise_shell_surface(&mut self, _id: Self::ShellId) {}
    fn configure_shell_surface(&mut self, id: Self::ShellId, geometry: wm_theme_api::Rect) {
        self.shell_geometries.insert(id, geometry);
    }
    fn paint_shell_surface(&mut self, id: Self::ShellId, buffer: &DecorationBuffer) {
        self.shell_buffer_bytes.insert(id, buffer.pixels.len());
    }
    fn release_shell_buffer(&mut self, id: Self::ShellId) {
        self.shell_buffer_bytes.remove(&id);
    }
    fn take_shell_scroll(&mut self) -> Option<(Self::ShellId, Point, ScrollDelta)> {
        self.queued_shell_scrolls.pop_front()
    }
    fn paint_root_color(&mut self, _rgb: (u8, u8, u8)) {}
    fn paint_root_image(&mut self, _buffer: &DecorationBuffer) {}
    fn set_layer_surface_hidden(&mut self, namespace: &str, hidden: bool) {
        self.layer_visibility_calls.push((namespace.to_string(), hidden));
    }
    fn screen_size(&self) -> Size {
        FAKE_SCREEN.size
    }

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

    fn window_class(&self, window: Self::WindowId) -> Option<WmClass> {
        self.window_classes
            .get(&window)
            .map(|class| WmClass { instance: class.to_lowercase(), class: class.clone() })
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

    fn paint_decoration(&mut self, frame: Self::FrameId, buffer: &DecorationBuffer) {
        self.painted_frames.insert(frame);
        *self.paint_count.entry(frame).or_insert(0) += 1;
        self.last_paint_size.insert(frame, Size::new(buffer.width, buffer.height));
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

    fn window_parent(&self, window: Self::WindowId) -> Option<Self::WindowId> {
        self.parents.get(&window).copied()
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

    fn kill_client(&mut self, window: Self::WindowId) {
        self.killed.push(window);
    }

    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        self.outstanding_pointer_grabs += 1;
        DragHandle(self.outstanding_pointer_grabs as u64)
    }
    fn ungrab_pointer(&mut self, _handle: DragHandle) {
        self.outstanding_pointer_grabs -= 1;
    }
    fn grab_key(&mut self, combo: KeyCombo) {
        self.grabbed_keys.push(combo);
    }
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

    fn publish_frame_extents(&mut self, window: Self::WindowId, left: u32, right: u32, top: u32, bottom: u32) {
        self.frame_extents.insert(window, (left, right, top, bottom));
    }

    fn client_draws_own_chrome(&self, window: Self::WindowId) -> bool {
        self.client_drawn_chrome.get(&window).copied().unwrap_or(false)
    }

    fn position_client(&mut self, window: Self::WindowId, pos: Point) {
        self.last_client_position.insert(window, pos);
    }

    fn raise_frameless(&mut self, window: Self::WindowId) {
        self.raised_frameless.push(window);
    }

    fn map_frameless(&mut self, window: Self::WindowId) {
        self.mapped_frameless.insert(window);
    }

    fn unmap_frameless(&mut self, window: Self::WindowId) {
        self.mapped_frameless.remove(&window);
    }

    fn release_decoration(&mut self, _window: Self::WindowId, frame: Self::FrameId) {
        self.released_frames.insert(frame);
        self.destroy_decoration(frame);
    }

    fn window_type(&self, window: Self::WindowId) -> WindowType {
        self.window_types.get(&window).copied().unwrap_or_default()
    }

    fn map_unmanaged(&mut self, window: Self::WindowId) {
        self.unmanaged_mapped.push(window);
    }

    fn publish_client_list(&mut self, clients: &[Self::WindowId]) {
        self.published_client_lists.push(clients.to_vec());
    }

    fn publish_active_window(&mut self, window: Option<Self::WindowId>) {
        self.published_active_windows.push(window);
    }

    fn publish_workspaces(&mut self, count: usize, current: usize) {
        self.published_workspaces.push((count, current));
    }

    fn publish_workarea(&mut self, area: Rect, workspace_count: usize) {
        self.published_workareas.push((area, workspace_count));
    }

    fn publish_net_state(&mut self, window: Self::WindowId, fullscreen: bool, max_h: bool, max_v: bool, shaded: bool, hidden: bool) {
        self.published_net_states.push((window, fullscreen, max_h, max_v, shaded, hidden));
    }

    fn publish_window_desktop(&mut self, window: Self::WindowId, desktop: usize) {
        self.published_window_desktops.push((window, desktop));
    }
}

/// In-memory `ThemeEngine` double: fixed 20px titlebar, two 14x14
/// buttons (close top-left, miniaturize top-right — the classic
/// NeXTSTEP-style arrangement), plus south-facing resize hitboxes
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

/// The fake's own contract tests. Only the parts of it that carry
/// behavior rather than record-keeping are worth testing here, and the
/// scroll queue is one: it is the sole path by which a consumer of
/// `Backend::take_shell_scroll` can be exercised with no display
/// server, so if its ordering or its drain-to-empty were wrong, every
/// test built on it would be wrong in the same direction and none of
/// them would say so.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolls_drain_oldest_first_and_then_stop() {
        let mut backend = FakeBackend::default();
        backend.queue_shell_scroll(7, Point::new(4, 5), ScrollDelta { up: 1, right: 0 });
        backend.queue_shell_scroll(7, Point::new(4, 5), ScrollDelta { up: -2, right: 0 });

        assert_eq!(backend.take_shell_scroll(), Some((7, Point::new(4, 5), ScrollDelta { up: 1, right: 0 })));
        assert_eq!(backend.take_shell_scroll(), Some((7, Point::new(4, 5), ScrollDelta { up: -2, right: 0 })));
        assert_eq!(backend.take_shell_scroll(), None, "a drained queue must end the loop, not repeat");
    }

    /// Scroll and click are separate channels on purpose (a wheel is
    /// not a `MouseButton`), so draining one must not consume or
    /// invent the other — the event loop calls both drains every pass.
    #[test]
    fn the_scroll_queue_is_independent_of_the_click_queue() {
        let mut backend = FakeBackend::default();
        backend.queue_shell_scroll(1, Point::new(0, 0), ScrollDelta { up: 0, right: 1 });

        assert_eq!(backend.take_shell_click(), None);
        assert!(backend.take_shell_scroll().is_some());
    }

    #[test]
    fn a_zero_delta_is_the_only_one_a_backend_may_not_queue() {
        assert!(ScrollDelta::default().is_zero());
        assert!(!ScrollDelta { up: -1, right: 0 }.is_zero());
        assert!(!ScrollDelta { up: 0, right: 1 }.is_zero());
    }
}
