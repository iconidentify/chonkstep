use std::collections::{HashMap, VecDeque};

use slotmap::SlotMap;
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationBuffer, DecorationLayout, DecorationRequest, Point, Rect, ResizeEdge, Size, ThemeEngine};

use crate::backend::Backend;
use crate::client::{Client, ClientFlags, ClientId, Lifecycle, MaximizeDirections, MonitorInfo};
use crate::focus::FocusPolicy;
use crate::hittest::{hit_test, HitTarget};
use crate::placement::{self, PlacementPolicy};
use crate::resize;
use crate::snap;
use crate::types::{BackendEvent, ClientChrome, DragHandle, KeyCombo, MouseButton, Modifiers, NetState, NetStateAction, SurfaceRef, WindowType};

/// How close together (in ms) two presses on the same titlebar must land
/// to count as a double-click (toggling maximize). Not backed by an
/// X server "double-click time" setting yet — a reasonable fixed
/// default, in the same ballpark as the classic desktop's.
const DOUBLE_CLICK_MS: u32 = 400;

/// How close (in pixels) a dragged frame edge must come to a screen edge
/// or another window's edge before it snaps flush — the classic "edge
/// resistance"/"attraction" behavior, simplified to a single always-on
/// threshold rather than separate resistance/attract modes.
const SNAP_THRESHOLD_PX: i32 = 10;

/// Keysym for the `Tab` key, per `<X11/keysymdef.h>` — the same numeric
/// space X11 and XKB (and so a future Wayland backend) both use, so
/// this is genuinely backend-agnostic despite the name. `Alt+Tab`/
/// `Alt+Shift+Tab` window cycling is the only keybinding `wm-core`
/// claims for itself — every other binding is config-driven from the
/// binary (see `bind_default_keys`).
const XK_TAB: u32 = 0xff09;
const XK_ESCAPE: u32 = 0xff1b;
const XK_ALT_L: u32 = 0xffe9;
const XK_ALT_R: u32 = 0xffea;

/// What every monitor query answers with when the backend reports no
/// outputs at all. Not a claim about any real screen — just a
/// non-degenerate rect, so placement and maximize arithmetic stays sane
/// instead of dividing into a zero-sized area. Only the test fake and a
/// backend mid-teardown ever reach it.
const NO_MONITOR_FALLBACK: Rect = Rect { pos: Point::new(0, 0), size: Size::new(800, 600) };

/// An in-progress move. `grab_offset` is the frame-local point that was
/// grabbed — since that's constant relative to the frame regardless of
/// where the frame currently sits, the new frame position is simply
/// `pointer_root - grab_offset` on every motion event.
///
/// Deliberately does not name a frame. A move can be started by
/// dragging our titlebar, and it can equally be started by the client
/// asking for one (`BackendEvent::MoveRequest`) — and a client that
/// asks is usually one that drew its own titlebar and therefore has no
/// frame at all. Which surface actually gets moved is resolved per
/// motion event from the client, so both kinds drag through one path.
struct ActiveMove {
    client: ClientId,
    grab_offset: Point,
}

/// An in-progress edge/corner resize drag. `start_frame` is the frame's
/// own geometry at press time — every motion event recomputes the new
/// size fresh from it and the current pointer position rather than
/// accumulating deltas
/// (same "no drift" reasoning as `ActiveMove::grab_offset`), anchored
/// at whichever corner/edge doesn't move for `edge`: the theme this WM
/// ships only ever offers south-facing handles (`South`/`SouthEast`/
/// `SouthWest`), so the top edge is always the fixed anchor and only
/// which horizontal edge is fixed varies.
struct ActiveResize {
    client: ClientId,
    edge: ResizeEdge,
    start_frame: Rect,
}

/// A titlebar button currently held down (pressed but not yet
/// released) — the standard "arm on press, commit on release-while-
/// still-over, cancel on release-elsewhere" interaction every button in
/// this theme follows. Drives the pressed/sunken-bevel visual feedback
/// via `decoration_request`'s `pressed_button` parameter.
struct ActiveButtonPress {
    client: ClientId,
    kind: ButtonKind,
}

/// A state change the desktop shell needs to react to but that `wm-core`
/// itself has no opinion on (icon tiles for miniaturized windows are a
/// desktop-shell concern, same as the dock/root menu). Drained via
/// `WindowManager::take_notification`, mirroring the `Backend`-side
/// shell-click/screen-resize side channels in `wm-x11`. No longer `Copy`
/// since `Miniaturized` carries an owned pixel buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Notification {
    /// A client was iconified — the shell should show an icon tile for
    /// it (clicking the tile should call `WindowManager::deminiaturize`).
    /// The `Option<DecorationBuffer>` is a snapshot of the window's
    /// content taken right before it unmapped (`None` if the capture
    /// failed) — see `Backend::capture_window_image`.
    Miniaturized(ClientId, Option<DecorationBuffer>),
    /// A client was restored from its icon — the shell should remove
    /// the tile.
    Deminiaturized(ClientId),
    /// A client was closed/destroyed — the shell should remove any icon
    /// tile it had (covers closing a window while it's miniaturized).
    Removed(ClientId),
    /// The Alt-Tab switcher session started or its selection moved —
    /// the shell should (re)draw the switch panel from
    /// `WindowManager::cycle_state`.
    CycleUpdated,
    /// The Alt-Tab switcher session ended (committed or cancelled) —
    /// the shell should take the panel down.
    CycleEnded,
    /// A client was just mapped and decorated for the first time — lets
    /// the shell react to new windows (e.g. giving a freshly spawned
    /// app a sane default size via `resize_client_content` when its own
    /// requested geometry can't be trusted, as some apps' initial size
    /// negotiation is unreliable across window-manager environments).
    Mapped(ClientId),
    /// The user right-clicked a window's titlebar — the shell should
    /// open the per-window commands menu at `at` (root coordinates) —
    /// the classic NeXTSTEP-style window menu. `wm-core` deliberately
    /// knows nothing about menus; it reports the gesture and executes
    /// whatever public-API calls the shell's menu dispatch makes.
    WindowMenuRequested { id: ClientId, at: Point },
}

/// Owns the backend, the theme engine, and all managed-client state.
/// `dispatch` is the sole entry point the event loop drives — plain and
/// synchronous, doing no I/O of its own beyond calls through `Backend`
/// and `ThemeEngine`.
pub struct WindowManager<B: Backend> {
    backend: B,
    theme: Box<dyn ThemeEngine>,
    clients: SlotMap<ClientId, Client<B>>,
    window_index: HashMap<B::WindowId, ClientId>,
    frame_index: HashMap<B::FrameId, ClientId>,
    focused: Option<ClientId>,
    active_move: Option<ActiveMove>,
    /// The pointer grab held for the duration of an interactive drag.
    ///
    /// A framed window did not need one: the window manager owns the
    /// frame the pointer is over, so the motion and the release land on
    /// it by construction. A window whose client draws its own chrome
    /// has no such surface — every event goes to the client — so a drag
    /// begun on one would start and then never end, and the window
    /// would follow the pointer with no button held until something
    /// else interrupted it. That was reported from a real session,
    /// dragging LibreOffice.
    ///
    /// `Some` exactly while a move or a resize is in flight. Paired
    /// with the backend's grab so a leak is visible in one place rather
    /// than distributed across every path a drag can end on — and a
    /// leaked pointer grab freezes the desktop for every client, which
    /// is worse than the bug it fixes.
    drag_grab: Option<DragHandle>,
    active_resize: Option<ActiveResize>,
    active_button_press: Option<ActiveButtonPress>,
    /// The most recent press on a titlebar drag region, for double-click
    /// detection — a second press on the *same* client's titlebar within
    /// `DOUBLE_CLICK_MS` toggles maximize instead of starting a move.
    last_titlebar_press: Option<(ClientId, u32)>,
    /// Per-monitor screen area windows should maximize into,
    /// reserved-space-aware — e.g. `chonkstep`'s desktop shell excludes
    /// its dock strip from the entry for the monitor the dock hangs on,
    /// by calling `set_workareas`. Indexed in `Backend::monitors()`
    /// order, and deliberately allowed to be *shorter* than that list
    /// (empty included): a monitor with no entry uses its full
    /// geometry, which is both the right answer for an output no panel
    /// reserves anything on and what makes a single-rect
    /// `set_workarea` call meaningful on a multi-head session.
    workareas: Vec<Rect>,
    /// The last root-relative pointer position the core has seen.
    /// `None` until the first `PointerMotion` arrives — a session's
    /// first window can map before the mouse has moved at all — so
    /// every reader needs a fallback rather than a default position,
    /// which would be indistinguishable from the pointer genuinely
    /// resting at the origin.
    last_pointer: Option<Point>,
    /// Where freshly mapped windows that expressed no position
    /// preference go — see `crate::placement`. Configured by the shell
    /// (`set_placement_policy`); `Smart` by default.
    placement_policy: PlacementPolicy,
    /// Monotonic count of placements performed, feeding the cascade
    /// staircase (`placement::place_frame`'s `cascade_index`).
    placements: usize,
    /// Edge-attraction distance for move drags — see `crate::snap`.
    /// Configurable (`set_snap_threshold`); `0` disables snapping.
    snap_threshold: i32,
    notifications: VecDeque<Notification>,
    focus_policy: FocusPolicy,
    /// 0-based, matching `Client::workspace`. Grows on demand the first
    /// time something switches to or moves a client onto an index past
    /// the current row's end — there is no fixed count and no way to
    /// destroy a workspace once created, the classic behavior.
    current_workspace: usize,
    workspace_count: usize,
    /// An in-progress Alt-Tab switcher session: the snapshot of cycle
    /// candidates and which one is currently selected. Selection moves
    /// on every Tab press while Alt stays held; releasing Alt commits
    /// it, Escape cancels. The shell renders the panel from
    /// `cycle_state` on every `Notification::CycleUpdated`.
    cycle: Option<CycleSession>,
    /// Managed clients' own windows in insertion (oldest-first) order —
    /// exactly the order EWMH wants `_NET_CLIENT_LIST` published in,
    /// which the unordered `window_index`/`clients` maps can't provide.
    managed_order: Vec<B::WindowId>,
    /// Content geometry to restore on leaving fullscreen. Deliberately a
    /// separate slot from `Client::restore_geometry` (maximize's
    /// snapshot): going fullscreen over a maximized window must come
    /// back to the *maximized* rect, and reusing maximize's slot would
    /// either clobber its own pre-maximize snapshot or restore too far.
    fullscreen_restore: HashMap<ClientId, Rect>,
}

struct CycleSession {
    order: Vec<ClientId>,
    selected: usize,
}

impl<B: Backend> WindowManager<B> {
    pub fn new(mut backend: B, theme: Box<dyn ThemeEngine>) -> Self {
        // Publish the initial workspace shape immediately: an EWMH pager
        // that starts alongside the WM reads `_NET_NUMBER_OF_DESKTOPS`/
        // `_NET_CURRENT_DESKTOP` right away, before any switch ever
        // happens to publish them as a side effect.
        backend.publish_workspaces(1, 0);
        Self {
            backend,
            theme,
            clients: SlotMap::with_key(),
            window_index: HashMap::new(),
            frame_index: HashMap::new(),
            focused: None,
            active_move: None,
            drag_grab: None,
            active_resize: None,
            active_button_press: None,
            last_titlebar_press: None,
            workareas: Vec::new(),
            last_pointer: None,
            placement_policy: PlacementPolicy::Smart,
            placements: 0,
            snap_threshold: SNAP_THRESHOLD_PX,
            notifications: VecDeque::new(),
            focus_policy: FocusPolicy::default(),
            current_workspace: 0,
            workspace_count: 1,
            cycle: None,
            managed_order: Vec::new(),
            fullscreen_restore: HashMap::new(),
        }
    }

    /// Switches between click-to-focus (the default) and focus-follows-
    /// mouse — the whole reason `HitTarget`/button handling and pointer-
    /// enter tracking are kept as separate concerns: a shell can flip
    /// this at runtime (e.g. a preferences toggle) without either code
    /// path needing to know the other exists.
    pub fn set_focus_policy(&mut self, policy: FocusPolicy) {
        self.focus_policy = policy;
    }

    /// Selects the initial-placement policy for windows that request no
    /// position of their own — the config file's `placement` entry.
    pub fn set_placement_policy(&mut self, policy: PlacementPolicy) {
        self.placement_policy = policy;
    }

    /// Sets the move-drag edge-attraction distance in pixels — the
    /// config file's `edge_resistance` entry. `0` disables snapping
    /// entirely (every position passes through untouched).
    pub fn set_snap_threshold(&mut self, px: u32) {
        self.snap_threshold = px.min(i32::MAX as u32) as i32;
    }

    /// Swaps the decoration engine and re-lays-out every managed client
    /// against it — how a live theme or UI-scale change reaches the
    /// window chrome without restarting the session.
    ///
    /// The sweep is part of the swap rather than a second call the
    /// caller makes afterward, because the two are not independently
    /// useful: an engine whose metrics no client has been reflowed
    /// against is a window manager whose `Client::layout` cache
    /// disagrees with what is on screen, and every hit-test and drag
    /// computed from that cache would be wrong. Making it one act means
    /// it cannot be half-done.
    ///
    /// `wm-core` still never sees a `Theme` — the caller builds the
    /// engine (see `wm_theme::RasterThemeEngine::with_fonts`, which
    /// reuses the loaded font database so a restyle costs no fontconfig
    /// scan) and this crate only knows it has a new source of layouts.
    pub fn set_theme_engine(&mut self, theme: Box<dyn ThemeEngine>) {
        self.theme = theme;
        self.relayout_all_clients();
    }

    /// Re-derives every managed client's decoration layout from the
    /// current engine, pushes the resulting frame geometry to the
    /// backend, and repaints the chrome.
    ///
    /// Withdrawn clients are skipped: they are unmapped and possibly
    /// already destroyed, and `reflow_frame` would still push a
    /// position and a size at the window. Miniaturized ones are *not* —
    /// their frame is unmapped but alive, and reflowing it now is what
    /// makes a later deminiaturize exact rather than restoring a window
    /// into chrome measured for the previous theme.
    ///
    /// A drag in flight is left alone deliberately rather than
    /// cancelled: `ActiveMove`'s grab offset was computed against the
    /// old layout, so a scale change landing mid-drag shifts the window
    /// under the pointer by the difference in titlebar height. That is
    /// a cosmetic jump in a race a user has to work to hit (a config
    /// reload while holding a titlebar), and cancelling the drag to
    /// avoid it would be the more surprising of the two.
    pub fn relayout_all_clients(&mut self) {
        let ids: Vec<ClientId> = self.clients.keys().collect();
        for id in ids {
            let Some(client) = self.clients.get(id) else {
                continue;
            };
            if client.lifecycle == Lifecycle::Withdrawn {
                continue;
            }
            // `reflow_frame` repaints the decoration itself as part of
            // pushing the new frame geometry, so this is one call, not
            // two — and a fullscreen client takes its branch, which
            // bypasses the theme entirely and is correct unchanged.
            self.reflow_frame(id);
        }
    }

    /// Reserves screen space windows should not maximize into (e.g. a
    /// dock/panel strip), one rect per monitor in `Backend::monitors()`
    /// order — a reusable SDK primitive: any desktop shell built on
    /// this crate calls this once at startup and again on output
    /// changes, rather than `wm-core` hardcoding a notion of "the
    /// dock".
    ///
    /// A vector shorter than the monitor list is legal and leaves every
    /// monitor past its end at that monitor's full geometry, so a shell
    /// that only reserves space on one output needs to say nothing
    /// about the rest.
    pub fn set_workareas(&mut self, areas: Vec<Rect>) {
        self.workareas = areas;
        self.publish_workarea_union();
    }

    /// The *primary* monitor's workarea — the single-rect form, and
    /// the whole call for a single-monitor session. Delegates to
    /// `set_workareas` with the primary's entry replaced and every
    /// other monitor left at its full geometry, so calling this on a
    /// multi-head session reserves space on exactly the one output the
    /// shell's chrome hangs on.
    pub fn set_workarea(&mut self, area: Rect) {
        let primary = self.primary_monitor_index();
        // Stops at the primary rather than covering every monitor: the
        // trailing ones are already "full geometry" by `set_workareas`'
        // short-vector rule, so spelling them out would only be a
        // snapshot to go stale.
        let mut areas: Vec<Rect> = self.backend.monitors().into_iter().take(primary + 1).map(|m| m.geometry).collect();
        match areas.get_mut(primary) {
            Some(slot) => *slot = area,
            // No monitors reported at all — this one rect is the only
            // screen information there is.
            None => areas.push(area),
        }
        self.set_workareas(areas);
    }

    /// `_NET_WORKAREA` is per-desktop in the property format and has no
    /// per-monitor dimension whatsoever (it predates multi-head), so a
    /// multi-monitor session publishes the bounding box of its
    /// per-monitor workareas — the only rect that is true of the whole
    /// desktop rather than of one output. The backend repeats it once
    /// per workspace, since the reserved strips are the same on all of
    /// them (see `Backend::publish_workarea`).
    fn publish_workarea_union(&mut self) {
        let union = self.effective_workareas().into_iter().reduce(union_rect).unwrap_or(NO_MONITOR_FALLBACK);
        self.backend.publish_workarea(union, self.workspace_count);
    }

    /// One workarea per reported monitor: the shell's own entry where
    /// it set one, that monitor's full geometry where it did not. With
    /// no monitors reported at all this is whatever `set_workareas` was
    /// handed, which is then the only screen information in existence.
    fn effective_workareas(&self) -> Vec<Rect> {
        let monitors = self.backend.monitors();
        if monitors.is_empty() {
            return self.workareas.clone();
        }
        monitors.iter().enumerate().map(|(index, m)| self.workareas.get(index).copied().unwrap_or(m.geometry)).collect()
    }

    /// Every physical output, in the backend's stable order — the same
    /// indices `set_workareas` addresses. The shell reads this to hang
    /// its chrome on the primary and to compute one workarea per
    /// monitor.
    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.backend.monitors()
    }

    /// Index of the monitor the shell's chrome belongs on: the one the
    /// backend flagged `primary`, else the first (see
    /// `Backend::monitors`, which allows a platform to name none).
    /// Always a valid index into a non-empty monitor list; `0` for an
    /// empty one, where nothing indexes it anyway.
    fn primary_monitor_index(&self) -> usize {
        self.backend.monitors().iter().position(|m| m.primary).unwrap_or(0)
    }

    /// The monitor `point` falls on: the one containing it, else the
    /// nearest one. "Nearest" rather than "none" is what makes every
    /// caller total — the dead corner of an L-shaped dual-head
    /// arrangement belongs to no output at all, and a frame dragged
    /// mostly off-screen still has to maximize somewhere.
    pub fn monitor_index_at(&self, point: Point) -> usize {
        let monitors = self.backend.monitors();
        if let Some(index) = monitors.iter().position(|m| m.geometry.contains(point)) {
            return index;
        }
        monitors
            .iter()
            .enumerate()
            .min_by_key(|(_, m)| squared_distance_to(m.geometry, point))
            .map(|(index, _)| index)
            .unwrap_or(0)
    }

    /// The full geometry of the monitor `point` falls on.
    pub fn monitor_rect_at(&self, point: Point) -> Rect {
        self.backend.monitors().get(self.monitor_index_at(point)).map(|m| m.geometry).unwrap_or(NO_MONITOR_FALLBACK)
    }

    /// That monitor's workarea: the shell-reserved area if one was set
    /// for it, else its full geometry. The per-monitor twin of
    /// `usable_area`, and what maximize actually measures against.
    pub fn usable_area_at(&self, point: Point) -> Rect {
        let index = self.monitor_index_at(point);
        self.workareas.get(index).copied().unwrap_or_else(|| self.monitor_rect_at(point))
    }

    /// Registers the Alt+Tab / Alt+Shift+Tab window-cycling grabs — the
    /// only keybindings `wm-core` claims for itself. The split is
    /// deliberate: cycling is *modal* machinery (a switcher session, an
    /// exclusive keyboard grab, commit-on-Alt-release, cancel-on-Escape,
    /// the lost-release fallback) that only works woven through this
    /// crate's internal state, so it lives here; everything else — close,
    /// maximize, workspace switching, and the rest — is a plain
    /// combo-to-action mapping the binary drives from user config
    /// (`wm-config`), registering each configured combo via `grab_key`
    /// and calling the matching public method when its `KeyPress`
    /// arrives. Call once after construction.
    pub fn bind_default_keys(&mut self) {
        self.backend.grab_key(KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT });
        self.backend.grab_key(KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT | Modifiers::SHIFT });
    }

    /// Registers a global keybinding with the backend — a pure
    /// passthrough. The binary calls this once per configured combo at
    /// startup and then reacts to the resulting
    /// `BackendEvent::KeyPress`es itself: `wm-core` neither knows nor
    /// cares what action a combo is bound to (the sole exception being
    /// the modal Alt+Tab machinery — see `bind_default_keys`).
    pub fn grab_key(&mut self, combo: KeyCombo) {
        self.backend.grab_key(combo);
    }

    /// Releases a passive grab taken by [`Self::grab_key`] — how a
    /// config reload lets go of a combo the user has just unbound.
    /// Without it a rebind could only ever add grabs, and a combo
    /// removed from the config file would keep being swallowed by this
    /// session for as long as it ran.
    pub fn ungrab_key(&mut self, combo: KeyCombo) {
        self.backend.ungrab_key(combo);
    }

    pub fn current_workspace(&self) -> usize {
        self.current_workspace
    }

    /// Iterates every managed client — the shell reads this for
    /// cross-client concerns wm-core has no opinion on (the launcher
    /// dock's running-app indicators match `Client::class` against
    /// `.desktop` entries).
    pub fn iter_clients(&self) -> impl Iterator<Item = (ClientId, &Client<B>)> {
        self.clients.iter()
    }

    pub fn workspace_count(&self) -> usize {
        self.workspace_count
    }

    /// Switches to `workspace`, growing the workspace row on demand if
    /// it's past the current end (there is no fixed count and no way to
    /// destroy a workspace once created). Every *mapped* client not on
    /// the target workspace gets its frame unmapped; every mapped
    /// client that IS on it gets remapped. Miniaturized/withdrawn
    /// clients are left alone entirely — their icon tile (a
    /// desktop-shell concern, not `wm-core`'s) stays visible regardless
    /// of workspace, a deliberately simpler choice than the classic
    /// opt-out-able per-workspace icon hiding. A no-op if already on
    /// `workspace`.
    pub fn switch_workspace(&mut self, workspace: usize) {
        if workspace == self.current_workspace {
            return;
        }
        self.workspace_count = self.workspace_count.max(workspace + 1);
        self.current_workspace = workspace;
        self.backend.publish_workspaces(self.workspace_count, self.current_workspace);

        let ids: Vec<ClientId> = self.clients.keys().collect();
        for id in ids {
            let Some(client) = self.clients.get(id) else {
                continue;
            };
            if client.lifecycle != Lifecycle::Normal {
                continue;
            }
            // No `frame` guard: a client that draws its own chrome has
            // none and must still follow its workspace on and off the
            // screen.
            if client.workspace == workspace {
                self.show_client_surface(id);
                // Same reasoning as `deminiaturize`: a remapped frame
                // isn't guaranteed to still hold its old pixel content
                // (no backing-store requested), so repaint explicitly
                // rather than hope an `Expose` arrives and gets replayed.
                // A no-op for a frameless window, which has no
                // decoration of ours to repaint.
                self.repaint_decoration(id);
            } else {
                self.hide_client_surface(id);
            }
        }

        let still_visible = self.focused.and_then(|id| self.clients.get(id)).is_some_and(|c| c.workspace == workspace);
        if !still_visible {
            if let Some(prev) = self.focused.take() {
                if let Some(c) = self.clients.get_mut(prev) {
                    c.flags.remove(ClientFlags::FOCUSED);
                }
                self.backend.publish_active_window(None);
            }
        }
        tracing::info!(workspace, "switched workspace");
    }

    /// Moves `id` onto `workspace` (growing the row if needed) and
    /// hides its frame immediately if that isn't the active workspace —
    /// the classic "move to next/previous workspace" window actions.
    /// A no-op if `id` is already on `workspace`.
    pub fn move_client_to_workspace(&mut self, id: ClientId, workspace: usize) {
        if workspace + 1 > self.workspace_count {
            self.workspace_count = workspace + 1;
            // Growth is a count change even though `current` stayed put
            // — a pager showing the workspace row needs to hear it.
            self.backend.publish_workspaces(self.workspace_count, self.current_workspace);
        }
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if client.workspace == workspace {
            return;
        }
        client.workspace = workspace;
        let window = client.window;
        // A move is precisely when a window's `_NET_WM_DESKTOP`
        // changes — pagers track membership from this property, not by
        // guessing from map/unmap traffic.
        self.backend.publish_window_desktop(window, workspace);
        if workspace != self.current_workspace {
            self.hide_client_surface(id);
            if self.focused == Some(id) {
                if let Some(c) = self.clients.get_mut(id) {
                    c.flags.remove(ClientFlags::FOCUSED);
                }
                self.focused = None;
                self.backend.publish_active_window(None);
            }
        }
    }

    /// Drains one pending desktop-shell notification (icon-tile
    /// lifecycle for miniaturized windows) — call this in a loop each
    /// time around the event loop, same as `Backend::poll_event`.
    pub fn take_notification(&mut self) -> Option<Notification> {
        self.notifications.pop_front()
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    pub fn client(&self, id: ClientId) -> Option<&Client<B>> {
        self.clients.get(id)
    }

    pub fn client_for_window(&self, window: B::WindowId) -> Option<ClientId> {
        self.window_index.get(&window).copied()
    }

    pub fn client_for_frame(&self, frame: B::FrameId) -> Option<ClientId> {
        self.frame_index.get(&frame).copied()
    }

    pub fn clients(&self) -> impl Iterator<Item = (ClientId, &Client<B>)> {
        self.clients.iter()
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    pub fn focused_client(&self) -> Option<ClientId> {
        self.focused
    }

    /// The event loop's sole entry point: drain `Backend::poll_event`
    /// into this on every wakeup.
    pub fn dispatch(&mut self, event: BackendEvent<B::WindowId, B::FrameId>) {
        match event {
            BackendEvent::MapRequest(window) => self.handle_map_request(window),
            BackendEvent::Unmapped(window) => self.handle_unmap(window),
            BackendEvent::Destroyed(window) => self.handle_destroy(window),
            BackendEvent::ConfigureRequest { window, requested } => {
                self.handle_configure_request(window, requested)
            }
            BackendEvent::PointerButton { surface, local, button, pressed, time_ms, mods } => {
                self.handle_pointer_button(surface, local, button, pressed, time_ms, mods)
            }
            BackendEvent::PointerMotion { root, surface_local } => {
                self.handle_pointer_motion(root);
                // Only while idle — during an active move/resize drag,
                // the cursor should stay whatever it was when the drag
                // started (typically already the right resize shape),
                // not flicker based on hovering in/out of the original
                // hitbox as the drag moves the pointer around.
                if self.active_move.is_none() && self.active_resize.is_none() {
                    if let Some((surface, local)) = surface_local {
                        self.handle_frame_hover(surface, local);
                    }
                }
            }
            BackendEvent::TitleChanged(window) => self.handle_title_changed(window),
            BackendEvent::ChromeChanged(window) => self.handle_chrome_changed(window),
            BackendEvent::MoveRequest(window) => self.handle_move_request(window),
            BackendEvent::DragEnded => self.end_active_drag(),
            BackendEvent::KeyPress(combo) => self.handle_key_press(combo),
            BackendEvent::KeyRelease(combo) => self.handle_key_release(combo),
            BackendEvent::PointerEnter { surface } => self.handle_pointer_enter(surface),
            BackendEvent::ActivateRequested(window) => self.handle_activate_request(window),
            BackendEvent::CloseRequested(window) => self.handle_close_request(window),
            BackendEvent::NetStateRequested { window, action, first, second } => {
                self.handle_net_state_request(window, action, first, second)
            }
            // `_NET_CURRENT_DESKTOP` rides the exact same path as the
            // keyboard switch — a pager and Alt+Right can never disagree
            // about what switching means (growth on demand included).
            BackendEvent::DesktopSwitchRequested(workspace) => self.switch_workspace(workspace),
            // `_NET_WM_DESKTOP`: a pager dragging a window onto another
            // desktop. Only the window's own workspace changes — the
            // active workspace stays put, unlike the follow-the-window
            // keyboard combos.
            BackendEvent::WindowDesktopRequested { window, desktop } => {
                if let Some(&id) = self.window_index.get(&window) {
                    self.move_client_to_workspace(id, desktop);
                }
            }
            // The event-loop driver (the binary) watches for this itself
            // and exits — there's no per-client state worth unwinding
            // when the display connection is already gone.
            BackendEvent::ShutdownRequested => {}
            other => {
                tracing::debug!(?other, "backend event not yet handled");
            }
        }
    }

    /// Starts tracking a newly mapped window and immediately realizes
    /// its decoration: asks the theme for a layout, creates the frame,
    /// renders and paints it, and maps it.
    fn handle_map_request(&mut self, window: B::WindowId) {
        if self.window_index.contains_key(&window) {
            return;
        }

        // Decoration policy from `_NET_WM_WINDOW_TYPE`, decided before
        // any frame or tracking state exists. Kept as an explicit
        // three-arm match (not `Unmanaged` vs. catch-all) so the
        // Dialog/Normal distinction stays visible for the future
        // transient-for/placement policy `WindowType::Dialog`'s doc
        // comment anticipates.
        let window_type = self.backend.window_type(window);
        match window_type {
            WindowType::Unmanaged => {
                // Docks/menus/tooltips draw their own chrome and place
                // themselves — map as the client created it, track
                // nothing.
                tracing::info!(?window, "unmanaged window type — mapping without decoration");
                self.backend.map_unmanaged(window);
                return;
            }
            // Both decorated and managed normally today.
            WindowType::Dialog => {}
            WindowType::Normal => {}
        }

        // The second half of the decoration question, and the one this
        // window manager never used to ask: `WindowType` said what kind
        // of window this is, but not whether its client has already
        // drawn a titlebar. Asking both is what stops Edge, LibreOffice
        // and every other client-decorated application from wearing two.
        let chrome = if self.backend.client_draws_own_chrome(window) {
            ClientChrome::ClientDrawn
        } else {
            ClientChrome::ServerDrawn
        };

        let title = self.backend.window_title(window).unwrap_or_default();
        let content = self.backend.window_geometry(window);
        tracing::debug!(?window, ?content, "map request — client's own geometry at map time");
        let mut client = Client::new(window, title);
        client.chrome = chrome;
        client.class = self.backend.window_class(window).map(|c| c.class).unwrap_or_default();
        client.geometry = content;
        client.workspace = self.current_workspace;

        let request = Self::decoration_request(&client, None);
        // A client-decorated window is laid out as though the frame were
        // exactly its content, so every placement and geometry
        // calculation below reads the same for both kinds.
        let layout = match chrome {
            ClientChrome::ServerDrawn => self.theme.layout(&request),
            ClientChrome::ClientDrawn => frameless_layout(content.size),
        };
        // Place the FRAME at the client's own requested position rather
        // than deriving it by subtracting the chrome offset from it.
        // Most apps request (0, 0) as a "don't care, WM decides"
        // placeholder rather than a real intent to sit flush with the
        // screen's corner; treating that as the *content* position and
        // subtracting the titlebar/border offset would push the frame
        // (and its titlebar) to negative coordinates, off-screen.
        // Initial placement: a client that asked for a real position (a
        // terminal launched with `-geometry +x+y`) is honored verbatim,
        // but the overwhelmingly common (0, 0) means "don't care, WM
        // decides" (see the comment below), and that is exactly what
        // the placement policy exists for. Dialogs center regardless of
        // policy — they are conversations, not workspace furniture.
        let frame_pos = if content.pos != Point::new(0, 0) {
            content.pos
        } else {
            let workarea = self.placement_area();
            let existing: Vec<Rect> = self
                .clients
                .iter()
                .filter(|(_, c)| c.lifecycle == Lifecycle::Normal && c.workspace == self.current_workspace)
                .map(|(_, c)| Rect {
                    pos: Point::new(c.geometry.pos.x - c.layout.client_offset.x, c.geometry.pos.y - c.layout.client_offset.y),
                    size: c.layout.frame_size,
                })
                .collect();
            let policy = if window_type == WindowType::Dialog {
                PlacementPolicy::Center
            } else {
                self.placement_policy
            };
            let cascade_step = layout.titlebar_height.max(16);
            let pos = placement::place_frame(policy, workarea, layout.frame_size, &existing, self.placements, cascade_step);
            self.placements += 1;
            pos
        };
        let frame_geom = Rect { pos: frame_pos, size: layout.frame_size };
        client.geometry.pos = Point::new(
            frame_geom.pos.x + layout.client_offset.x,
            frame_geom.pos.y + layout.client_offset.y,
        );

        let frame = match chrome {
            ClientChrome::ServerDrawn => {
                let frame = self.backend.create_decoration(window, &layout);
                self.backend.set_frame_geometry(frame, frame_geom);
                let buffer = self.theme.render(&request, &layout);
                self.backend.paint_decoration(frame, &buffer);
                self.backend.map_frame(frame);
                Some(frame)
            }
            ClientChrome::ClientDrawn => {
                // No frame is created, so nothing reparents the client
                // or maps it as a side effect the way `create_decoration`
                // does: it has to be placed and shown directly. The
                // position is the frame position, which for this layout
                // is the content position.
                self.backend.position_client(window, frame_geom.pos);
                self.backend.resize_client(window, frame_geom.size);
                self.backend.map_frameless(window);
                None
            }
        };

        client.frame = frame;
        client.layout = layout;

        let id = self.clients.insert(client);
        self.window_index.insert(window, id);
        if let Some(frame) = frame {
            self.frame_index.insert(frame, id);
        }
        self.managed_order.push(window);
        self.backend.publish_client_list(&self.managed_order);
        // A fresh window's `_NET_WM_DESKTOP` — published once here (it
        // lands on the current workspace) and again on every later
        // move; a pager that never sees the property at all would have
        // to treat the window as on-every-desktop.
        self.backend.publish_window_desktop(window, self.current_workspace);
        match chrome {
            ClientChrome::ServerDrawn => tracing::info!(?window, "mapped and decorated window"),
            ClientChrome::ClientDrawn => {
                tracing::info!(?window, "mapped window undecorated — its client draws its own chrome")
            }
        }

        self.publish_frame_extents(id);
        self.notifications.push_back(Notification::Mapped(id));
        self.focus_client(id);
    }

    /// Explicitly resizes a managed client's content, independent of any
    /// `ConfigureRequest` from the client itself — for the shell to call
    /// right after a `Notification::Mapped` when an app's own initial
    /// size can't be trusted (see that variant's doc comment). Shares
    /// its resize path with a client's own post-map `ConfigureRequest`
    /// (`handle_configure_request`), so both end up equally "real" as
    /// far as layout/repaint are concerned.
    pub fn resize_client_content(&mut self, id: ClientId, size: Size) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        client.geometry.size = size;
        self.reflow_frame(id);
    }

    fn handle_unmap(&mut self, window: B::WindowId) {
        if self.forget(window) {
            tracing::info!(?window, "unmapped, no longer tracked");
        }
    }

    fn handle_destroy(&mut self, window: B::WindowId) {
        if self.forget(window) {
            tracing::info!(?window, "destroyed, no longer tracked");
        }
    }

    fn forget(&mut self, window: B::WindowId) -> bool {
        let Some(id) = self.window_index.remove(&window) else {
            return false;
        };

        if self.focused == Some(id) {
            self.focused = None;
            self.backend.publish_active_window(None);
        }
        if self.active_move.as_ref().is_some_and(|m| m.client == id)
            || self.active_resize.as_ref().is_some_and(|r| r.client == id)
        {
            // The window being dragged has gone. Ending the drag here
            // is what stops the grab outliving it — a pointer grab with
            // nothing left to move is a frozen desktop.
            self.end_active_drag();
        }
        self.fullscreen_restore.remove(&id);
        if let Some(client) = self.clients.remove(id) {
            if let Some(frame) = client.frame {
                self.frame_index.remove(&frame);
                self.backend.destroy_decoration(frame);
            }
        }
        self.managed_order.retain(|&other| other != window);
        self.backend.publish_client_list(&self.managed_order);
        // Keep an active switcher session honest when one of its
        // candidates disappears mid-cycle: prune it from the snapshot
        // (clamping the selection) and let the shell redraw — or end
        // the session outright if nothing is left to switch to.
        if let Some(session) = &mut self.cycle {
            let before = session.order.len();
            session.order.retain(|&other| other != id);
            if session.order.is_empty() {
                self.cycle_end(false);
            } else if session.order.len() != before {
                session.selected = session.selected.min(session.order.len() - 1);
                self.notifications.push_back(Notification::CycleUpdated);
            }
        }
        self.notifications.push_back(Notification::Removed(id));
        true
    }

    /// Re-reads `WM_NAME` for an already-managed window and repaints its
    /// decoration if it actually changed — a no-op for a window we don't
    /// track (e.g. a property change on something never mapped).
    fn handle_title_changed(&mut self, window: B::WindowId) {
        let Some(id) = self.client_for_window(window) else {
            return;
        };
        let title = self.backend.window_title(window).unwrap_or_default();
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if client.title == title {
            return;
        }
        client.title = title;
        self.repaint_decoration(id);
    }

    fn handle_configure_request(&mut self, window: B::WindowId, requested: Rect) {
        let Some(&id) = self.window_index.get(&window) else {
            // Not yet managed (configure can arrive before the first
            // map request) — ICCCM requires honoring it directly.
            tracing::debug!(?window, ?requested, "configure request for an unmanaged window — applying directly");
            self.backend.configure_unmanaged(window, requested);
            return;
        };

        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        // Once a client is managed (reparented into a frame), its own
        // ConfigureRequest x/y are relative to its new parent — the
        // frame, not root — so they don't mean anything as root-relative
        // `Client::geometry`. Only the requested *size* is meaningful
        // here; position is entirely WM-managed from this point on
        // (interactive move), so it's left untouched.
        if client.flags.contains(ClientFlags::SIZE_LOCKED) {
            tracing::debug!(?id, ?requested, "configure request from a size-locked client — ignored");
            return;
        }
        tracing::debug!(?id, ?requested, "configure request from a managed client — applying");
        client.geometry.size = requested.size;
        self.reflow_frame(id);
    }

    /// See `ClientFlags::SIZE_LOCKED`'s doc comment for what this does
    /// and why it exists. A no-op for an unknown `id`.
    pub fn set_size_locked(&mut self, id: ClientId, locked: bool) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        client.flags.set(ClientFlags::SIZE_LOCKED, locked);
    }

    fn handle_pointer_button(
        &mut self,
        surface: SurfaceRef<B::WindowId, B::FrameId>,
        local: Point,
        button: MouseButton,
        pressed: bool,
        time_ms: u32,
        mods: Modifiers,
    ) {
        // A drag ends on the button coming up, wherever that happens.
        // Not only on the frame: a window whose client draws its own
        // chrome is dragged with the pointer over the *client*, and
        // under the drag grab the release may be reported against the
        // root, the frame or the client depending on the backend. Any
        // of them means the same thing, and treating only one of them
        // as the end is what left the window stuck to the cursor.
        if !pressed && button == MouseButton::Left && (self.active_move.is_some() || self.active_resize.is_some()) {
            self.end_active_drag();
        }
        match surface {
            SurfaceRef::Frame(frame) => self.handle_frame_button(frame, local, button, pressed, time_ms, mods),
            SurfaceRef::Client(window) => self.handle_client_button(window, pressed),
        }
    }

    /// Focus-follows-mouse: the pointer entering a client's frame
    /// focuses it immediately, no click needed. A no-op under the
    /// default click-to-focus policy, and a no-op for a surface that
    /// isn't (or is no longer) a managed client.
    fn handle_pointer_enter(&mut self, surface: SurfaceRef<B::WindowId, B::FrameId>) {
        if self.focus_policy != FocusPolicy::FocusFollowsMouse {
            return;
        }
        let id = match surface {
            SurfaceRef::Frame(frame) => self.frame_index.get(&frame).copied(),
            SurfaceRef::Client(window) => self.window_index.get(&window).copied(),
        };
        if let Some(id) = id {
            self.focus_client(id);
        }
    }

    fn handle_client_button(&mut self, window: B::WindowId, pressed: bool) {
        if !pressed {
            return;
        }
        if let Some(&id) = self.window_index.get(&window) {
            self.focus_client(id);
        }
        // A click on a client's own content only ever reaches this
        // handler at all via the passive grab `focus_client` maintains
        // on unfocused clients (an already-focused client isn't
        // grabbed, so its own clicks go straight to it without `wm-core`
        // seeing them) — so every call here came through that grab, and
        // needs replaying: without it, the click that just focused this
        // window would never actually reach the window itself (to place
        // a text cursor, say), only the WM would see it.
        self.backend.replay_pointer();
    }

    fn handle_frame_button(
        &mut self,
        frame: B::FrameId,
        local: Point,
        button: MouseButton,
        pressed: bool,
        time_ms: u32,
        mods: Modifiers,
    ) {
        let Some(&id) = self.frame_index.get(&frame) else {
            return;
        };

        if !pressed {
            self.handle_frame_button_release(id, local, button);
            return;
        }

        self.focus_client(id);

        let Some(client) = self.clients.get(id) else {
            return;
        };
        match hit_test(&client.layout, local) {
            // Arm on press — the button visibly goes "down" (sunken
            // bevel) immediately, but the Close/Miniaturize action only
            // commits on release, and only if the pointer is still over
            // the same button then. This is the universal press/release
            // contract every button in this theme follows, matching how
            // real buttons everywhere behave: you can press-and-drag
            // away to cancel.
            HitTarget::Button(kind) if button == MouseButton::Left => {
                self.active_button_press = Some(ActiveButtonPress { client: id, kind });
                self.repaint_decoration(id);
            }
            // Right-click anywhere on the titlebar surface asks the
            // shell for the per-window commands menu — reported at root
            // coordinates so the shell can pop the menu under the
            // pointer without knowing frame geometry.
            HitTarget::TitlebarDrag | HitTarget::Button(_) if button == MouseButton::Right => {
                let frame_pos = Point::new(
                    client.geometry.pos.x - client.layout.client_offset.x,
                    client.geometry.pos.y - client.layout.client_offset.y,
                );
                let at = Point::new(frame_pos.x + local.x, frame_pos.y + local.y);
                self.notifications.push_back(Notification::WindowMenuRequested { id, at });
            }
            HitTarget::TitlebarDrag if button == MouseButton::Left => {
                // A second press on this same client's titlebar within
                // `DOUBLE_CLICK_MS` triggers a titlebar action instead of
                // starting a drag — exactly the classic titlebar
                // double-click behavior: a plain double-click shades
                // (rolls the window up to just its titlebar), and
                // maximizing needs a modifier — Ctrl alone for vertical-
                // only, Shift alone for horizontal-only, both together
                // for full. (The classic desktop also offers a
                // preference that swaps the plain case to full-maximize
                // instead of shade; not implemented here — shade is the
                // flagship default.) No X server gives double-click
                // detection for free; this is why
                // `BackendEvent::PointerButton` carries a timestamp.
                let is_double_click = self
                    .last_titlebar_press
                    .take_if(|(prev_id, prev_time)| *prev_id == id && time_ms.saturating_sub(*prev_time) <= DOUBLE_CLICK_MS)
                    .is_some();
                if is_double_click {
                    let ctrl = mods.contains(Modifiers::CONTROL);
                    let shift = mods.contains(Modifiers::SHIFT);
                    match (ctrl, shift) {
                        (true, true) => self.toggle_maximize(id, MaximizeDirections::FULL),
                        (true, false) => self.toggle_maximize(id, MaximizeDirections::VERTICAL),
                        (false, true) => self.toggle_maximize(id, MaximizeDirections::HORIZONTAL),
                        (false, false) => self.toggle_shade(id),
                    }
                } else {
                    self.last_titlebar_press = Some((id, time_ms));
                    self.active_move = Some(ActiveMove { client: id, grab_offset: local });
                    self.begin_drag_grab();
                }
            }
            // A shaded window has nothing to resize — the classic
            // behavior is to refuse the drag outright, rather than
            // letting a resize silently reshape a window the user can't
            // even see the content of.
            HitTarget::ResizeEdge(edge) if button == MouseButton::Left && !client.flags.contains(ClientFlags::SHADED) => {
                let start_frame = Rect {
                    pos: Point::new(client.geometry.pos.x - client.layout.client_offset.x, client.geometry.pos.y - client.layout.client_offset.y),
                    size: client.layout.frame_size,
                };
                self.active_resize = Some(ActiveResize { client: id, edge, start_frame });
                self.begin_drag_grab();
            }
            _ => {}
        }
    }

    fn handle_frame_button_release(&mut self, id: ClientId, local: Point, button: MouseButton) {
        if button != MouseButton::Left {
            return;
        }
        if self.active_move.as_ref().is_some_and(|m| m.client == id)
            || self.active_resize.as_ref().is_some_and(|r| r.client == id)
        {
            self.end_active_drag();
        }

        let Some(active) = self.active_button_press.take_if(|p| p.client == id) else {
            return;
        };

        let still_over = self
            .clients
            .get(id)
            .is_some_and(|c| matches!(hit_test(&c.layout, local), HitTarget::Button(k) if k == active.kind));

        if still_over {
            match active.kind {
                ButtonKind::Close => {
                    if let Some(client) = self.clients.get(id) {
                        self.backend.send_close(client.window);
                    }
                }
                ButtonKind::Miniaturize => self.miniaturize(id),
                ButtonKind::Maximize => self.toggle_maximize(id, MaximizeDirections::FULL),
            }
        }
        // Always repaint to clear the pressed/sunken visual, whether the
        // action fired or the press was cancelled by releasing elsewhere.
        self.repaint_decoration(id);
    }

    /// Updates a frame's cursor to indicate a resize is available
    /// wherever the pointer is currently hovering — the visual half of
    /// the resize-corner affordance; `render_decoration`'s grip marks
    /// are the other half. A no-op for `SurfaceRef::Client`: the frame
    /// (chonkstep's own border/titlebar) is the only surface whose
    /// cursor is ours to manage — a client's own content may set its
    /// own cursor (an I-beam over a text area, say), which this must
    /// never override.
    fn handle_frame_hover(&mut self, surface: SurfaceRef<B::WindowId, B::FrameId>, local: Point) {
        let SurfaceRef::Frame(frame) = surface else {
            return;
        };
        let Some(&id) = self.frame_index.get(&frame) else {
            return;
        };
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let edge = match hit_test(&client.layout, local) {
            HitTarget::ResizeEdge(edge) => Some(edge),
            _ => None,
        };
        self.backend.set_frame_cursor(frame, edge);
    }

    fn handle_pointer_motion(&mut self, root: Point) {
        // Recorded before any drag branch returns: this is the core's
        // only sighting of where the user's attention is, and new-window
        // placement reads it to open on the monitor being looked at
        // (see `placement_area`). A drag in progress is no reason to
        // stop tracking — it is the most emphatic pointer motion there
        // is.
        self.last_pointer = Some(root);

        if self.active_resize.is_some() {
            self.handle_resize_motion(root);
            return;
        }

        let Some(active) = &self.active_move else {
            return;
        };
        let (client_id, grab_offset) = (active.client, active.grab_offset);

        let Some(client) = self.clients.get(client_id) else {
            self.end_active_drag();
            return;
        };
        // A shaded window's *displayed* frame is only `shaded_frame_
        // height` tall — pushing the full unshaded `frame_size` here
        // (as if nothing were shaded) would force it back to full
        // height on the very first motion event, visibly "unrolling"
        // it the instant a drag starts. `client.geometry` itself is
        // untouched either way, so this is purely about what gets
        // pushed to the backend during the drag, matching what
        // `reflow_frame` already does once the drag ends.
        let frame_size = if client.flags.contains(ClientFlags::SHADED) {
            Size::new(client.layout.frame_size.w, client.layout.shaded_frame_height)
        } else {
            client.layout.frame_size
        };
        let (surface_frame, surface_window) = (client.frame, client.window);
        let raw_pos = Point::new(root.x - grab_offset.x, root.y - grab_offset.y);

        // Edge resistance/attraction: pull the dragged frame flush
        // against the screen edge or another window's frame edge once
        // it's within `SNAP_THRESHOLD_PX`. Pure geometry against every
        // other *visible* client's current frame rect, recomputed fresh
        // each motion event — cheap at WM scale.
        let mut targets: Vec<Rect> = self.backend.monitors().into_iter().map(|m| m.geometry).collect();
        for (other_id, other) in self.clients.iter() {
            if other_id == client_id || other.lifecycle != Lifecycle::Normal {
                continue;
            }
            let other_frame_pos = Point::new(
                other.geometry.pos.x - other.layout.client_offset.x,
                other.geometry.pos.y - other.layout.client_offset.y,
            );
            targets.push(Rect { pos: other_frame_pos, size: other.layout.frame_size });
        }
        let new_frame_pos = snap::snap_position(Rect { pos: raw_pos, size: frame_size }, &targets, self.snap_threshold);

        // A framed window is moved by its frame; a client-decorated one
        // has only itself to move, and its content sits at the frame
        // origin (its layout has a zero offset), so the same computed
        // position applies to both.
        match (surface_frame, surface_window) {
            (Some(frame), _) => self.backend.set_frame_geometry(frame, Rect { pos: new_frame_pos, size: frame_size }),
            (None, window) => self.backend.position_client(window, new_frame_pos),
        }

        let Some(client) = self.clients.get_mut(client_id) else {
            return;
        };
        client.geometry.pos = Point::new(
            new_frame_pos.x + client.layout.client_offset.x,
            new_frame_pos.y + client.layout.client_offset.y,
        );
    }

    /// Recomputes content size/position fresh from `start_frame` and
    /// the current pointer position for whichever edge is being
    /// dragged, enforces the client's `SizeHints` (min/max/resize
    /// increment), and pushes the result through the normal
    /// `reflow_frame` path. All eight edges/corners are handled:
    /// dragging a north or west handle keeps the *opposite* edge
    /// anchored, so the frame's origin moves with the drag while the
    /// far edge stays put — the size-hint constraint
    /// is applied to the size first and the anchored edge re-derived
    /// from the constrained result, so a terminal snapping to its cell
    /// grid never makes the anchored edge drift.
    fn handle_resize_motion(&mut self, root: Point) {
        let Some(active) = &self.active_resize else {
            return;
        };
        let (client_id, edge, start_frame) = (active.client, active.edge, active.start_frame);

        let Some(client) = self.clients.get(client_id) else {
            self.active_resize = None;
            return;
        };
        let overhead_w = client.layout.frame_size.w.saturating_sub(client.geometry.size.w);
        let overhead_h = client.layout.frame_size.h.saturating_sub(client.geometry.size.h);
        let start_right = start_frame.pos.x + start_frame.size.w as i32;
        let start_bottom = start_frame.pos.y + start_frame.size.h as i32;

        let raw_frame_w = match edge {
            ResizeEdge::North | ResizeEdge::South => start_frame.size.w as i32,
            ResizeEdge::East | ResizeEdge::NorthEast | ResizeEdge::SouthEast => root.x - start_frame.pos.x,
            ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest => start_right - root.x,
        }
        .max(1);
        let raw_frame_h = match edge {
            ResizeEdge::East | ResizeEdge::West => start_frame.size.h as i32,
            ResizeEdge::South | ResizeEdge::SouthEast | ResizeEdge::SouthWest => root.y - start_frame.pos.y,
            ResizeEdge::North | ResizeEdge::NorthEast | ResizeEdge::NorthWest => start_bottom - root.y,
        }
        .max(1);

        let raw_content = Size::new((raw_frame_w as u32).saturating_sub(overhead_w), (raw_frame_h as u32).saturating_sub(overhead_h));
        let hints = self.backend.size_hints(client.window);
        let content = resize::constrain_size(raw_content, hints);

        let new_frame_w = content.w + overhead_w;
        let new_frame_h = content.h + overhead_h;
        let new_frame_x = match edge {
            ResizeEdge::West | ResizeEdge::NorthWest | ResizeEdge::SouthWest => start_right - new_frame_w as i32,
            _ => start_frame.pos.x,
        };
        let new_frame_y = match edge {
            ResizeEdge::North | ResizeEdge::NorthEast | ResizeEdge::NorthWest => start_bottom - new_frame_h as i32,
            _ => start_frame.pos.y,
        };

        let Some(client) = self.clients.get_mut(client_id) else {
            return;
        };
        client.geometry.pos = Point::new(new_frame_x + client.layout.client_offset.x, new_frame_y + client.layout.client_offset.y);
        client.geometry.size = content;
        self.reflow_frame(client_id);
    }

    /// Screen area windows should maximize into on the *primary*
    /// monitor: the shell-reserved workarea if one was set for it, else
    /// that monitor's full geometry. Anything that knows which monitor
    /// it means wants `usable_area_at` instead; this is the answer for
    /// the cases that genuinely have no anchor (a window that belongs
    /// to no monitor yet).
    fn usable_area(&self) -> Rect {
        let primary = self.primary_monitor_index();
        self.workareas
            .get(primary)
            .copied()
            .or_else(|| self.backend.monitors().get(primary).map(|m| m.geometry))
            .unwrap_or(NO_MONITOR_FALLBACK)
    }

    /// A client's frame center in root coordinates — the point that
    /// decides which monitor a window "is on", for fullscreen and
    /// maximize alike. Center rather than origin: a window straddling
    /// two outputs belongs to the one showing most of it, whereas the
    /// origin would hand it to whichever output happens to hold its
    /// top-left corner (so a window nudged one pixel left across the
    /// seam would maximize onto the monitor it is barely touching).
    /// `None` for an unknown or stale id.
    fn client_frame_center(&self, id: ClientId) -> Option<Point> {
        let client = self.clients.get(id)?;
        let frame_pos = Point::new(
            client.geometry.pos.x - client.layout.client_offset.x,
            client.geometry.pos.y - client.layout.client_offset.y,
        );
        Some(Point::new(
            frame_pos.x + client.layout.frame_size.w as i32 / 2,
            frame_pos.y + client.layout.frame_size.h as i32 / 2,
        ))
    }

    /// Where a fresh window that expressed no position preference
    /// should be placed: the workarea of the monitor the user is
    /// actually looking at. "Looking at" is the pointer's monitor —
    /// the mouse is where attention is, and it is also where the
    /// launcher click or menu pick that spawned the window happened,
    /// so a window launched from the second head opens on the second
    /// head. With no pointer seen yet the focused window's monitor is
    /// the next best guess (a keyboard-spawned window joins its
    /// siblings), and the primary is the last.
    fn placement_area(&self) -> Rect {
        if let Some(pointer) = self.last_pointer {
            return self.usable_area_at(pointer);
        }
        if let Some(center) = self.focused.and_then(|id| self.client_frame_center(id)) {
            return self.usable_area_at(center);
        }
        self.usable_area()
    }

    /// Re-derives layout from a client's current `geometry.size`, then
    /// pushes the resulting frame geometry/decoration/client size to the
    /// backend. Shared tail of any operation that changes a client's
    /// content size in place (`ConfigureRequest`, maximize, unmaximize).
    fn reflow_frame(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        // Fullscreen bypasses the theme entirely: the frame is exactly
        // the client's monitor and the content fills it edge-to-edge —
        // no titlebar, border, or resizebar, and nothing to paint. The
        // synthetic layout (offset 0,0, no hitboxes) keeps every
        // consumer of `Client::layout` (hit-testing, drag math)
        // consistent with what's actually on screen.
        if client.flags.contains(ClientFlags::FULLSCREEN) {
            let monitor = self.fullscreen_monitor_rect(id);
            let Some(client) = self.clients.get_mut(id) else {
                return;
            };
            client.geometry = monitor;
            client.layout = DecorationLayout {
                frame_size: monitor.size,
                client_offset: Point::new(0, 0),
                titlebar_height: 0,
                button_hitboxes: Vec::new(),
                resize_hitboxes: Vec::new(),
                shaded_frame_height: 0,
            };
            let window = client.window;
            let frame = client.frame;
            if let Some(frame) = frame {
                self.backend.set_frame_geometry(frame, monitor);
            }
            self.backend.position_client(window, Point::new(0, 0));
            self.backend.resize_client(window, monitor.size);
            return;
        }
        // A client-decorated window has no chrome to lay out and no
        // frame to move: its content *is* the window, positioned in root
        // coordinates. Handled before the theme is consulted at all,
        // for the same reason the fullscreen branch above is — asking a
        // theme to describe chrome that is not drawn produces a layout
        // every consumer would then have to second-guess.
        if client.chrome == ClientChrome::ClientDrawn {
            let content = client.geometry;
            let window = client.window;
            let layout = frameless_layout(content.size);
            self.backend.position_client(window, content.pos);
            self.backend.resize_client(window, content.size);
            if let Some(client) = self.clients.get_mut(id) {
                client.layout = layout;
            }
            self.publish_frame_extents(id);
            return;
        }
        let request = Self::decoration_request(client, None);
        let layout = self.theme.layout(&request);
        // Shaded windows show only the titlebar — the frame's *visible*
        // height is overridden to `shaded_frame_height`, but the client's
        // own content geometry (and everything the theme computed from
        // it) is left completely untouched, so unshading is exact.
        let shaded = client.flags.contains(ClientFlags::SHADED);
        let frame_height = if shaded { layout.shaded_frame_height } else { layout.frame_size.h };
        let frame_geom = Rect {
            pos: Point::new(
                client.geometry.pos.x - layout.client_offset.x,
                client.geometry.pos.y - layout.client_offset.y,
            ),
            size: Size::new(layout.frame_size.w, frame_height),
        };
        let window = client.window;
        let content_size = client.geometry.size;

        if let Some(frame) = client.frame {
            self.backend.set_frame_geometry(frame, frame_geom);
            // Painted from the shade's *own* inputs, never the unshaded
            // `layout` — see `shaded_paint_inputs` for why the frame
            // rect alone is not enough.
            let buffer = if shaded {
                let (shaded_request, shaded_layout) = shaded_paint_inputs(&request, &layout);
                self.theme.render(&shaded_request, &shaded_layout)
            } else {
                self.theme.render(&request, &layout)
            };
            self.backend.paint_decoration(frame, &buffer);
        }
        self.backend.position_client(window, layout.client_offset);
        self.backend.resize_client(window, content_size);

        if let Some(client) = self.clients.get_mut(id) {
            client.layout = layout;
        }
        self.publish_frame_extents(id);
    }

    /// Grows `id` to fill the usable screen area along `directions`,
    /// remembering its current geometry (the first time it's maximized
    /// in either axis) so `unmaximize` can restore it. No titlebar button
    /// triggers this — see `MaximizeDirections`'s doc comment.
    pub fn maximize(&mut self, id: ClientId, directions: MaximizeDirections) {
        // The window's *own* monitor, not the primary: a window dragged
        // onto the second head must maximize there. Same frame-center
        // rule fullscreen picks its monitor by (`client_frame_center`),
        // but through that monitor's workarea rather than its raw rect
        // — maximize respects the shell's reserved strip, fullscreen
        // deliberately does not.
        let usable = match self.client_frame_center(id) {
            Some(center) => self.usable_area_at(center),
            None => self.usable_area(),
        };
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };

        if client.restore_geometry.is_none() {
            client.restore_geometry = Some(client.geometry);
        }

        // Overhead (border/titlebar/resize-bar chrome) is constant
        // regardless of content size for this theme, so the client's own
        // currently-cached layout already tells us how much of the
        // usable area's edge-to-edge size is *not* content.
        let overhead_w = client.layout.frame_size.w.saturating_sub(client.geometry.size.w);
        let overhead_h = client.layout.frame_size.h.saturating_sub(client.geometry.size.h);

        if directions.contains(MaximizeDirections::HORIZONTAL) {
            client.geometry.pos.x = usable.pos.x + client.layout.client_offset.x;
            client.geometry.size.w = usable.size.w.saturating_sub(overhead_w);
            client.flags.insert(ClientFlags::MAXIMIZED_H);
        }
        if directions.contains(MaximizeDirections::VERTICAL) {
            client.geometry.pos.y = usable.pos.y + client.layout.client_offset.y;
            client.geometry.size.h = usable.size.h.saturating_sub(overhead_h);
            client.flags.insert(ClientFlags::MAXIMIZED_V);
        }

        self.reflow_frame(id);
        self.publish_client_net_state(id);
        tracing::info!(?id, ?directions, "maximized");
    }

    /// Restores the geometry saved by the most recent `maximize` call, if
    /// any (a no-op on an already-unmaximized client).
    pub fn unmaximize(&mut self, id: ClientId) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        let Some(restore) = client.restore_geometry.take() else {
            return;
        };
        client.geometry = restore;
        client.flags.remove(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V);
        self.reflow_frame(id);
        self.publish_client_net_state(id);
        tracing::info!(?id, "unmaximized");
    }

    /// Maximizes along `directions` if not already exactly in that state,
    /// otherwise restores. Always restores to the *original* (pre-any-
    /// maximize) geometry first when switching between direction
    /// combinations, so e.g. toggling from full-maximize to
    /// vertical-only-maximize starts from a clean slate rather than
    /// compounding — simpler to reason about than XOR-based incremental
    /// toggling, at the cost of not preserving an in-progress partial
    /// state across a direction change.
    pub fn toggle_maximize(&mut self, id: ClientId, directions: MaximizeDirections) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let mut current = MaximizeDirections::empty();
        if client.flags.contains(ClientFlags::MAXIMIZED_H) {
            current |= MaximizeDirections::HORIZONTAL;
        }
        if client.flags.contains(ClientFlags::MAXIMIZED_V) {
            current |= MaximizeDirections::VERTICAL;
        }

        if current == directions {
            self.unmaximize(id);
            return;
        }
        if !current.is_empty() {
            self.unmaximize(id);
        }
        self.maximize(id, directions);
    }

    /// Flips full (both-axes) maximization — the config-driven
    /// keybinding entry point, so the binary never needs to know
    /// `MaximizeDirections` exists just to bind one key. Exactly
    /// `toggle_maximize` with `MaximizeDirections::FULL`: the client's
    /// current `MAXIMIZED_H`/`MAXIMIZED_V` flags decide whether this
    /// maximizes or restores, and a partially (one-axis) maximized
    /// window goes to full first rather than restoring, per
    /// `toggle_maximize`'s clean-slate semantics. A no-op for an
    /// unknown/stale `id`.
    pub fn toggle_maximize_full(&mut self, id: ClientId) {
        self.toggle_maximize(id, MaximizeDirections::FULL);
    }

    /// Rolls a client up to just its titlebar (the classic "shade") —
    /// the content window is hidden (not resized to nothing; the window
    /// is genuinely unmapped) but its geometry is left completely
    /// untouched, so `unshade` restores exactly. A no-op if already
    /// shaded.
    pub fn shade(&mut self, id: ClientId) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if client.flags.contains(ClientFlags::SHADED) {
            return;
        }
        client.flags.insert(ClientFlags::SHADED);
        let window = client.window;
        self.backend.set_client_mapped(window, false);
        self.reflow_frame(id);
        self.publish_client_net_state(id);
        tracing::info!(?id, "shaded");
    }

    /// Reverses `shade`. A no-op if not currently shaded.
    pub fn unshade(&mut self, id: ClientId) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if !client.flags.contains(ClientFlags::SHADED) {
            return;
        }
        client.flags.remove(ClientFlags::SHADED);
        let window = client.window;
        let content_size = client.geometry.size;
        self.backend.set_client_mapped(window, true);
        self.reflow_frame(id);
        // Nudge the client to repaint everything: X preserves no pixels
        // across the unmap, and some clients (urxvt among them) repaint
        // lazily enough on remap that stale buffer garbage stays
        // visible until their next full redraw.
        self.backend.refresh_client(window, content_size);
        self.publish_client_net_state(id);
        tracing::info!(?id, "unshaded");
    }

    pub fn toggle_shade(&mut self, id: ClientId) {
        match self.clients.get(id) {
            Some(client) if client.flags.contains(ClientFlags::SHADED) => self.unshade(id),
            Some(_) => self.shade(id),
            None => {}
        }
    }

    /// The monitor a client should fullscreen onto: whichever one its
    /// frame's center lands on (`monitor_index_at`'s nearest-monitor
    /// rule covers a frame dragged mostly off every output), and the
    /// primary for a client that no longer exists to have a center.
    /// Note this is the raw monitor rect, deliberately not
    /// `usable_area_at`: fullscreen covers the dock strip too — that's
    /// the whole point of the state.
    fn fullscreen_monitor_rect(&self, id: ClientId) -> Rect {
        match self.client_frame_center(id) {
            Some(center) => self.monitor_rect_at(center),
            None => self.backend.monitors().get(self.primary_monitor_index()).map(|m| m.geometry).unwrap_or(NO_MONITOR_FALLBACK),
        }
    }

    /// Enters EWMH fullscreen: the frame becomes exactly the client's
    /// monitor and the content fills it completely, no chrome (see
    /// `reflow_frame`'s fullscreen branch). The pre-fullscreen content
    /// geometry goes into `fullscreen_restore` — its own slot, separate
    /// from `Client::restore_geometry`, so fullscreening a maximized
    /// window comes back to the maximized rect and a later `unmaximize`
    /// still finds its own pre-maximize snapshot intact. A no-op if
    /// already fullscreen.
    pub fn fullscreen(&mut self, id: ClientId) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if client.flags.contains(ClientFlags::FULLSCREEN) {
            return;
        }
        self.fullscreen_restore.insert(id, client.geometry);
        client.flags.insert(ClientFlags::FULLSCREEN);
        self.reflow_frame(id);
        // Raise on entering: fullscreen is a "take over the screen"
        // request — a video player going fullscreen *behind* other
        // windows would be useless.
        self.raise_client(id);
        self.publish_client_net_state(id);
        tracing::info!(?id, "entered fullscreen");
    }

    /// Reverses `fullscreen`, restoring the exact content geometry saved
    /// on entry through the normal reflow path (theme layout recomputed,
    /// chrome repainted). A no-op if not currently fullscreen.
    pub fn unfullscreen(&mut self, id: ClientId) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if !client.flags.contains(ClientFlags::FULLSCREEN) {
            return;
        }
        client.flags.remove(ClientFlags::FULLSCREEN);
        if let Some(saved) = self.fullscreen_restore.remove(&id) {
            if let Some(client) = self.clients.get_mut(id) {
                client.geometry = saved;
            }
        }
        self.reflow_frame(id);
        self.publish_client_net_state(id);
        tracing::info!(?id, "left fullscreen");
    }

    /// Flips fullscreen via the existing `fullscreen`/`unfullscreen`
    /// pair — the config-driven keybinding entry point, routed through
    /// the exact same toggle logic an EWMH `_NET_WM_STATE` Toggle
    /// message takes (`apply_fullscreen_action`), so a bound key and a
    /// pager can never disagree about what toggling means. A no-op for
    /// an unknown/stale `id` (both halves of the pair already bail on
    /// one).
    pub fn toggle_fullscreen(&mut self, id: ClientId) {
        self.apply_fullscreen_action(id, NetStateAction::Toggle);
    }

    /// Iconifies a client: unmaps its frame and marks it `Miniaturized`.
    /// Pushes `Notification::Miniaturized` so the desktop shell can show
    /// an icon tile in its place — clicking that tile should call
    /// `deminiaturize` with this same id. Captures a snapshot of the
    /// window's content first, while it's still mapped and viewable —
    /// once unmapped, there's nothing left to capture (see
    /// `Backend::capture_window_image`).
    pub fn miniaturize(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        if client.lifecycle != Lifecycle::Normal {
            return;
        }
        let window = client.window;
        let content_size = client.geometry.size;
        let preview = self.backend.capture_window_image(window, content_size);

        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        client.lifecycle = Lifecycle::Miniaturized;
        self.hide_client_surface(id);
        if self.focused == Some(id) {
            self.focused = None;
            self.backend.publish_active_window(None);
        }
        self.notifications.push_back(Notification::Miniaturized(id, preview));
        self.publish_client_net_state(id);
        tracing::info!(?id, "miniaturized");
    }

    pub fn deminiaturize(&mut self, id: ClientId) {
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if client.lifecycle != Lifecycle::Miniaturized {
            return;
        }
        client.lifecycle = Lifecycle::Normal;
        let window = client.window;
        let content_size = client.geometry.size;
        // A window restores onto the workspace the user is looking at,
        // not the one it was miniaturized on. Icon tiles are visible on
        // every workspace, so restoring from a different one is a
        // normal gesture — and without this reassignment the window
        // became a ghost: mapped over the current workspace but still
        // *owned* by the old one, so it lingered there on the next
        // switch back and vanished from here. (Moving *to* the current
        // workspace never unmaps or drops focus — see
        // `move_client_to_workspace` — so ordering against the map
        // below doesn't matter; it's pure assignment plus the
        // `_NET_WM_DESKTOP` republish pagers need.)
        let current = self.current_workspace;
        self.move_client_to_workspace(id, current);
        self.show_client_surface(id);
        // Same nudge unshade needs (see there): the client's own pixels
        // weren't retained while unmapped either.
        self.backend.refresh_client(window, content_size);
        // Explicit, not left to an `Expose` reply: a window without
        // backing-store (every frame here — `create_decoration` never
        // requests it) isn't guaranteed by X11 to retain its pixel
        // content while unmapped, so remapping after miniaturize can
        // surface a blank/undefined frame until *something* repaints it.
        // Painting it ourselves right here removes that dependency on
        // Expose timing entirely instead of hoping one arrives.
        self.repaint_decoration(id);
        self.notifications.push_back(Notification::Deminiaturized(id));
        self.publish_client_net_state(id);
        self.focus_client(id);
    }

    /// `_NET_ACTIVE_WINDOW`: a pager/launcher/tool asked for this window
    /// to be activated. Restored out of miniaturized/shaded first —
    /// "activate" means "show me this window", and focusing one that's
    /// unmapped or rolled up would visibly do nothing — then focused
    /// (which raises). Same restore-before-focus order the Alt-Tab
    /// commit path uses.
    fn handle_activate_request(&mut self, window: B::WindowId) {
        let Some(&id) = self.window_index.get(&window) else {
            return;
        };
        if self.clients.get(id).is_some_and(|c| c.lifecycle == Lifecycle::Miniaturized) {
            self.deminiaturize(id);
        }
        if self.clients.get(id).is_some_and(|c| c.flags.contains(ClientFlags::SHADED)) {
            self.unshade(id);
        }
        self.focus_client(id);
    }

    /// Closes `id` — the config-driven keybinding entry point, routed
    /// through exactly the mechanism the titlebar close button's commit
    /// and `_NET_CLOSE_WINDOW` (`handle_close_request`) use:
    /// `Backend::send_close`, which does the ICCCM dance
    /// (`WM_DELETE_WINDOW` when the client supports it, force-kill
    /// otherwise). One shared mechanism means a bound key can never
    /// disagree with the other two entry points about how a window
    /// dies. A no-op for an unknown/stale `id`.
    pub fn close_client(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        self.backend.send_close(client.window);
    }

    /// Force-kills `id`'s client connection — the window menu's "Kill"
    /// entry, for an application that hangs and stops answering the
    /// polite `WM_DELETE_WINDOW` close. Deliberately a separate verb
    /// from `close_client`: closing asks, killing doesn't, and a menu
    /// should never quietly escalate one into the other.
    pub fn kill_client(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        self.backend.kill_client(client.window);
    }

    /// `_NET_CLOSE_WINDOW`: identical to the titlebar close button's
    /// commit — `Backend::send_close` does the ICCCM dance
    /// (`WM_DELETE_WINDOW` when supported, force-kill otherwise), so
    /// both paths can never disagree about how a window dies.
    fn handle_close_request(&mut self, window: B::WindowId) {
        if !self.window_index.contains_key(&window) {
            return;
        }
        self.backend.send_close(window);
    }

    /// `_NET_WM_STATE`: applies `action` to the (up to two) states in
    /// the message. Fullscreen is handled per-state; the two maximize
    /// properties are collected and applied as one `MaximizeDirections`
    /// set, because pagers conventionally send horz+vert together as a
    /// single "maximize" — applying them one at a time through the
    /// clean-slate maximize machinery would snapshot the intermediate
    /// half-maximized geometry as the restore point.
    fn handle_net_state_request(&mut self, window: B::WindowId, action: NetStateAction, first: NetState, second: Option<NetState>) {
        let Some(&id) = self.window_index.get(&window) else {
            return;
        };
        let mut maximize_directions = MaximizeDirections::empty();
        for state in [Some(first), second].into_iter().flatten() {
            match state {
                NetState::Fullscreen => self.apply_fullscreen_action(id, action),
                NetState::MaximizedHorz => maximize_directions |= MaximizeDirections::HORIZONTAL,
                NetState::MaximizedVert => maximize_directions |= MaximizeDirections::VERTICAL,
            }
        }
        if !maximize_directions.is_empty() {
            self.apply_maximize_action(id, action, maximize_directions);
        }
    }

    fn apply_fullscreen_action(&mut self, id: ClientId, action: NetStateAction) {
        let currently = self.clients.get(id).is_some_and(|c| c.flags.contains(ClientFlags::FULLSCREEN));
        let target = match action {
            NetStateAction::Add => true,
            NetStateAction::Remove => false,
            NetStateAction::Toggle => !currently,
        };
        // `fullscreen`/`unfullscreen` are no-ops when already in the
        // target state, so Add/Remove are naturally idempotent.
        if target {
            self.fullscreen(id);
        } else {
            self.unfullscreen(id);
        }
    }

    /// Maps an EWMH maximize action onto the existing maximize
    /// machinery: the target direction set is computed from the client's
    /// current `MAXIMIZED_H`/`MAXIMIZED_V` flags per Remove/Add/Toggle
    /// semantics, then reached the way `toggle_maximize` does — a full
    /// restore to the original geometry first when the direction set
    /// changes — so the EWMH path and the titlebar double-click path can
    /// never disagree about what "maximized" means.
    fn apply_maximize_action(&mut self, id: ClientId, action: NetStateAction, directions: MaximizeDirections) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let mut current = MaximizeDirections::empty();
        if client.flags.contains(ClientFlags::MAXIMIZED_H) {
            current |= MaximizeDirections::HORIZONTAL;
        }
        if client.flags.contains(ClientFlags::MAXIMIZED_V) {
            current |= MaximizeDirections::VERTICAL;
        }
        let target = match action {
            NetStateAction::Add => current | directions,
            NetStateAction::Remove => current - directions,
            NetStateAction::Toggle => current ^ directions,
        };
        if target == current {
            return;
        }
        if target.is_empty() {
            self.unmaximize(id);
            return;
        }
        if !current.is_empty() {
            self.unmaximize(id);
        }
        self.maximize(id, target);
    }

    /// Only the modal Alt+Tab machinery reacts to key presses here —
    /// every other grabbed combo reaches the binary through this same
    /// `BackendEvent::KeyPress` and is matched against user config
    /// there (see `bind_default_keys` for why the line is drawn where
    /// it is).
    fn handle_key_press(&mut self, combo: KeyCombo) {
        match combo.keysym {
            XK_TAB => {
                if combo.modifiers.contains(Modifiers::SHIFT) {
                    self.cycle_step(-1);
                } else if combo.modifiers.contains(Modifiers::ALT) {
                    self.cycle_step(1);
                }
            }
            XK_ESCAPE if self.cycle.is_some() => self.cycle_end(false),
            // Any other key without Alt held means the Alt release was
            // lost (it can slip into the gap before the modal keyboard
            // grab activates) — commit rather than leaving the panel
            // stuck on screen.
            _ if self.cycle.is_some() && !combo.modifiers.contains(Modifiers::ALT) => self.cycle_end(true),
            _ => {}
        }
    }

    fn handle_key_release(&mut self, combo: KeyCombo) {
        if self.cycle.is_some() && matches!(combo.keysym, XK_ALT_L | XK_ALT_R) {
            self.cycle_end(true);
        }
    }

    /// Alt+Tab / Alt+Shift+Tab window switching, modal like the classic
    /// switch panel: the first press opens a session (snapshotting the
    /// candidates and grabbing the keyboard so the Alt release is
    /// visible), every further Tab moves the selection, and nothing is
    /// focused or raised until the session commits —
    /// `Notification::CycleUpdated` tells the shell to draw its panel
    /// in the meantime. Candidates are mapped, non-miniaturized clients
    /// on the *current* workspace, in `SlotMap` iteration order, stable
    /// for the session because the order is snapshotted (and pruned on
    /// client destruction). The workspace restriction matters: a client
    /// parked on another workspace is `Lifecycle::Normal` but its frame
    /// is unmapped — cycling onto it set input focus on an invisible
    /// window, and nothing visibly happened.
    fn cycle_step(&mut self, direction: i32) {
        if self.cycle.is_none() {
            let order: Vec<ClientId> = self
                .clients
                .iter()
                .filter(|(_, c)| c.lifecycle == Lifecycle::Normal && c.workspace == self.current_workspace)
                .map(|(id, _)| id)
                .collect();
            if order.is_empty() {
                return;
            }
            let selected = self.focused.and_then(|focused| order.iter().position(|&id| id == focused)).unwrap_or(0);
            self.backend.grab_keyboard();
            self.cycle = Some(CycleSession { order, selected });
        }
        let session = self.cycle.as_mut().expect("session exists or was just created");
        session.selected = (session.selected as i32 + direction).rem_euclid(session.order.len() as i32) as usize;
        self.notifications.push_back(Notification::CycleUpdated);
    }

    /// Ends the switcher session; `commit` focuses and raises the
    /// selected client (Alt released), `!commit` leaves focus exactly
    /// where it was (Escape).
    fn cycle_end(&mut self, commit: bool) {
        let Some(session) = self.cycle.take() else {
            return;
        };
        self.backend.ungrab_keyboard();
        if commit {
            if let Some(&id) = session.order.get(session.selected) {
                if let Some(client) = self.clients.get(id) {
                    let window = client.window;
                    let content_size = client.geometry.size;
                    // Switching to a rolled-up window means "show me
                    // that window", so the commit unshades. Without
                    // this, committing to a shaded client set input
                    // focus on an *unmapped* window and nothing visibly
                    // happened.
                    self.unshade(id);
                    self.focus_client(id);
                    // Repaint nudge after the raise: the session
                    // compositor (picom xrender on the reference VM)
                    // does not reliably refresh a window's region on a
                    // bare restack — the raised window kept showing
                    // whatever previously covered it until some later
                    // damage arrived (a drag "repaired" it, confirmed
                    // live). A full-window redraw from the client is
                    // exactly that damage.
                    self.backend.refresh_client(window, content_size);
                }
            }
        }
        self.notifications.push_back(Notification::CycleEnded);
    }

    /// The live switcher session for the shell's panel: `(candidates
    /// as (id, title), selected index)`, `None` when no session is
    /// active.
    pub fn cycle_state(&self) -> Option<(Vec<(ClientId, String)>, usize)> {
        let session = self.cycle.as_ref()?;
        let entries = session
            .order
            .iter()
            .map(|&id| (id, self.clients.get(id).map(|c| c.title.clone()).unwrap_or_default()))
            .collect();
        Some((entries, session.selected))
    }

    /// A live thumbnail of a client's current content for the switcher
    /// panel — same capture path miniaturize previews use.
    pub fn client_preview(&self, id: ClientId) -> Option<DecorationBuffer> {
        let client = self.clients.get(id)?;
        self.backend.capture_window_image(client.window, client.geometry.size)
    }

    fn focus_client(&mut self, id: ClientId) {
        if self.focused == Some(id) {
            // Re-assert input focus rather than assume it landed. This
            // early return is the path a user takes to *repair* focus —
            // they click the window that already looks focused — so it
            // is the one place that must not trust `self.focused`. The
            // two can genuinely disagree: a Wayland window focused
            // before its `wl_surface` existed, an X11 focus the server
            // refused. Both backends treat an unchanged focus as a
            // no-op (Smithay short-circuits it outright), so this is
            // free whenever they already agree.
            if let Some(window) = self.clients.get(id).map(|client| client.window) {
                self.backend.set_input_focus(window);
                self.raise_client(id);
            }
            return;
        }

        if let Some(prev) = self.focused.take() {
            let prev_window = self.clients.get_mut(prev).map(|c| {
                c.flags.remove(ClientFlags::FOCUSED);
                c.window
            });
            if let Some(window) = prev_window {
                // Re-grab now that it's losing focus — its content
                // needs to be click-to-focus-able again (see
                // `handle_client_button`/`grab_button_passive`'s doc
                // comments for the full mechanism).
                self.backend.grab_button_passive(window, MouseButton::Left);
            }
            self.repaint_decoration(prev);
        }

        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        client.flags.insert(ClientFlags::FOCUSED);
        let window = client.window;
        self.focused = Some(id);
        // Ungrab: a focused client's own clicks (placing a text cursor,
        // clicking a button inside it, ...) should reach it directly,
        // not detour through the WM on every single click the way an
        // unfocused client's first click does.
        self.backend.ungrab_button_passive(window, MouseButton::Left);
        self.backend.set_input_focus(window);
        self.backend.publish_active_window(Some(window));
        self.raise_client(id);
        self.repaint_decoration(id);
    }

    /// Pushes a client's current `_NET_WM_STATE`-relevant flags to the
    /// backend — called from every state transition that changes any of
    /// them (fullscreen, maximize, shade, miniaturize). The backend is
    /// a dumb mirror of these authoritative flags, never the other way
    /// around (see `Backend::publish_net_state`).
    fn publish_client_net_state(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        self.backend.publish_net_state(
            client.window,
            client.flags.contains(ClientFlags::FULLSCREEN),
            client.flags.contains(ClientFlags::MAXIMIZED_H),
            client.flags.contains(ClientFlags::MAXIMIZED_V),
            client.flags.contains(ClientFlags::SHADED),
            // EWMH `_NET_WM_STATE_HIDDEN` means "would be shown by a
            // pager's activate request" — exactly this WM's
            // miniaturized state, and nothing else here qualifies.
            client.lifecycle == Lifecycle::Miniaturized,
        );
    }

    /// Put a managed client on screen, whichever way it is realized.
    ///
    /// Exists because "map this window" stopped being one call the day
    /// a managed window could have no frame. Every caller — workspace
    /// switch, deminiaturize, unshade — means the same thing and none
    /// of them should have to know which kind of client it is holding.
    fn show_client_surface(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        match (client.frame, client.window) {
            (Some(frame), _) => self.backend.map_frame(frame),
            (None, window) => self.backend.map_frameless(window),
        }
    }

    /// The counterpart of [`Self::show_client_surface`]. A frameless
    /// window that is not hidden here stays visible on every workspace
    /// and survives being miniaturized, which is exactly the bug the
    /// `unmap_frameless` backend verb exists to prevent.
    fn hide_client_surface(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        match (client.frame, client.window) {
            (Some(frame), _) => self.backend.unmap_frame(frame),
            (None, window) => self.backend.unmap_frameless(window),
        }
    }

    /// A managed client changed its mind about drawing its own chrome:
    /// add or drop its frame to match, in place, without the window
    /// blinking out of existence.
    ///
    /// This is not a hypothetical. Applications rewrite
    /// `_MOTIF_WM_HINTS` on a mapped window — a browser leaving
    /// fullscreen, an application whose "use system title bar" setting
    /// is toggled — and a window manager that only reads the hint at
    /// map time gets the answer permanently wrong for every one of
    /// them.
    ///
    /// The content geometry is the fixed point across the transition,
    /// not the frame: what the user is looking at is the application's
    /// own pixels, and those must not jump. The frame is created around
    /// them or taken away from around them.
    fn handle_chrome_changed(&mut self, window: B::WindowId) {
        let Some(&id) = self.window_index.get(&window) else {
            return;
        };
        let wants = if self.backend.client_draws_own_chrome(window) {
            ClientChrome::ClientDrawn
        } else {
            ClientChrome::ServerDrawn
        };
        let Some(client) = self.clients.get(id) else {
            return;
        };
        if client.chrome == wants {
            return;
        }
        let content = client.geometry;
        let existing_frame = client.frame;

        match wants {
            ClientChrome::ClientDrawn => {
                // `release_decoration`, never `destroy_decoration`: on
                // X11 the client is a child of the frame, and destroying
                // a parent destroys its children.
                if let Some(frame) = existing_frame {
                    self.backend.release_decoration(window, frame);
                    self.frame_index.remove(&frame);
                }
                if let Some(client) = self.clients.get_mut(id) {
                    client.frame = None;
                    client.chrome = ClientChrome::ClientDrawn;
                }
            }
            ClientChrome::ServerDrawn => {
                let request = self
                    .clients
                    .get(id)
                    .map(|client| Self::decoration_request(client, None))
                    .unwrap_or_else(|| Self::decoration_request(&Client::new(window, String::new()), None));
                let layout = self.theme.layout(&request);
                let frame = self.backend.create_decoration(window, &layout);
                // Anchor the *content* where it already is; the frame is
                // built around it, extending up and left by the chrome's
                // own offset.
                let frame_geom = Rect {
                    pos: Point::new(content.pos.x - layout.client_offset.x, content.pos.y - layout.client_offset.y),
                    size: layout.frame_size,
                };
                self.backend.set_frame_geometry(frame, frame_geom);
                let buffer = self.theme.render(&request, &layout);
                self.backend.paint_decoration(frame, &buffer);
                self.backend.map_frame(frame);
                self.frame_index.insert(frame, id);
                if let Some(client) = self.clients.get_mut(id) {
                    client.frame = Some(frame);
                    client.chrome = ClientChrome::ServerDrawn;
                    client.layout = layout;
                }
            }
        }
        tracing::info!(?window, ?wants, "client changed its decoration preference");
        self.reflow_frame(id);
        // Whichever direction it went, the window's visibility has to be
        // restated. Taking a frame away removes the only mapped surface
        // a framed window had, and creating one maps the frame but says
        // nothing about a window that should currently be hidden — so
        // ask what this client's lifecycle and workspace say it should
        // be, rather than assuming the transition left it right. Caught
        // by `a_client_that_starts_drawing_its_own_chrome_loses_its_frame_in_place`,
        // which found the window gone from the screen entirely.
        let visible = self
            .clients
            .get(id)
            .is_some_and(|client| client.lifecycle == Lifecycle::Normal && client.workspace == self.current_workspace);
        if visible {
            self.show_client_surface(id);
        } else {
            self.hide_client_surface(id);
        }
    }

    /// Publish `_NET_FRAME_EXTENTS` for `id` from whatever its layout
    /// currently says.
    ///
    /// Derived rather than remembered, and derived in one place, so the
    /// property cannot drift from the chrome actually on screen: the
    /// left and top edges are the offset the content sits at inside the
    /// frame, and the right and bottom are whatever frame is left over
    /// once the content is accounted for. A frameless window's layout
    /// makes all four fall out as zero without a special case, and so
    /// does a fullscreen one.
    fn publish_frame_extents(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let window = client.window;
        let left = client.layout.client_offset.x.max(0) as u32;
        let top = client.layout.client_offset.y.max(0) as u32;
        let right = client.layout.frame_size.w.saturating_sub(left.saturating_add(client.geometry.size.w));
        let bottom = client.layout.frame_size.h.saturating_sub(top.saturating_add(client.geometry.size.h));
        self.backend.publish_frame_extents(window, left, right, top, bottom);
    }

    /// Bring a managed client to the front, whichever way it is
    /// realized. The frameless counterpart of `Backend::raise`, and the
    /// reason no raise site in this file names a frame directly any
    /// more.
    fn raise_client(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        match (client.frame, client.window) {
            (Some(frame), _) => self.backend.raise(frame),
            (None, window) => self.backend.raise_frameless(window),
        }
    }

    /// The client asked to be moved — begin an interactive move from
    /// wherever the pointer currently is.
    ///
    /// The grab offset is derived rather than reported: the client's
    /// request carries no anchor, and the honest anchor is the pointer's
    /// offset within the frame at the moment the request arrives, which
    /// is exactly what a titlebar press would have recorded. Refused
    /// outright while another drag is in flight, and refused when the
    /// pointer's position is not yet known — a move anchored on a
    /// guessed pointer position would teleport the window on its first
    /// motion event.
    fn handle_move_request(&mut self, window: B::WindowId) {
        if self.active_move.is_some() || self.active_resize.is_some() {
            return;
        }
        let Some(&id) = self.window_index.get(&window) else {
            return;
        };
        // Asked of the server first: see `Backend::pointer_position`
        // for why the remembered position is not good enough here.
        let Some(pointer) = self.backend.pointer_position().or(self.last_pointer) else {
            return;
        };
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let origin = Point::new(
            client.geometry.pos.x - client.layout.client_offset.x,
            client.geometry.pos.y - client.layout.client_offset.y,
        );
        self.active_move = Some(ActiveMove {
            client: id,
            grab_offset: Point::new(pointer.x - origin.x, pointer.y - origin.y),
        });
        // The grab is what makes this kind of drag finishable at all:
        // the client asked for it precisely because the pointer is over
        // its own chrome, so without one every later motion and the
        // release go to the client and never come back here.
        self.begin_drag_grab();
        tracing::debug!(?window, "client asked to be moved — interactive move begun");
    }

    /// Takes the pointer for a drag that is starting, if one is not
    /// already held. Idempotent: a second press mid-drag must not
    /// strand the first grab.
    fn begin_drag_grab(&mut self) {
        if self.drag_grab.is_some() {
            return;
        }
        self.drag_grab = Some(self.backend.grab_pointer_for_drag());
    }

    /// Ends any interactive drag and releases the pointer.
    ///
    /// The single exit. Every way a drag can stop — the button coming
    /// up anywhere at all, the client being destroyed under it, a
    /// workspace switching out from under it — goes through here, so
    /// that "the drag is over" and "the pointer is free" cannot come
    /// apart. Safe to call when nothing is dragging.
    fn end_active_drag(&mut self) {
        self.active_move = None;
        self.active_resize = None;
        if let Some(handle) = self.drag_grab.take() {
            self.backend.ungrab_pointer(handle);
        }
    }

    fn repaint_decoration(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        // A fullscreen frame is entirely covered by the client's own
        // content — there's no chrome to paint, and the synthetic
        // fullscreen layout has no decoration geometry to render into
        // anyway (see `reflow_frame`'s fullscreen branch).
        if client.flags.contains(ClientFlags::FULLSCREEN) {
            return;
        }
        let Some(frame) = client.frame else {
            return;
        };
        let pressed_button = self.active_button_press.as_ref().filter(|p| p.client == id).map(|p| p.kind);
        let request = Self::decoration_request(client, pressed_button);
        // `client.layout` is deliberately kept at its full unshaded
        // shape while shaded (that is what makes unshading exact), so
        // every repaint of a shaded frame — a focus change, a title
        // update, the button release that immediately follows the
        // shading double-click itself — has to re-derive the shade's
        // paint inputs rather than hand the theme that layout.
        let buffer = if client.flags.contains(ClientFlags::SHADED) {
            let (shaded_request, shaded_layout) = shaded_paint_inputs(&request, &client.layout);
            self.theme.render(&shaded_request, &shaded_layout)
        } else {
            self.theme.render(&request, &client.layout)
        };
        self.backend.paint_decoration(frame, &buffer);
    }

    fn decoration_request(client: &Client<B>, pressed_button: Option<ButtonKind>) -> DecorationRequest {
        DecorationRequest {
            content_size: client.geometry.size,
            title: client.title.clone(),
            focused: client.flags.contains(ClientFlags::FOCUSED),
            resizable: true,
            buttons: vec![
                ButtonRuntimeState {
                    kind: ButtonKind::Close,
                    hovered: false,
                    pressed: pressed_button == Some(ButtonKind::Close),
                },
                ButtonRuntimeState {
                    kind: ButtonKind::Maximize,
                    hovered: false,
                    pressed: pressed_button == Some(ButtonKind::Maximize),
                },
                ButtonRuntimeState {
                    kind: ButtonKind::Miniaturize,
                    hovered: false,
                    pressed: pressed_button == Some(ButtonKind::Miniaturize),
                },
            ],
        }
    }
}

/// The layout a window whose client draws its own chrome wears.
///
/// Every consumer of `Client::layout` — hit-testing, drag math,
/// placement, the frame geometry the backend is told — reads it
/// unconditionally, so a frameless window needs a layout that honestly
/// describes having no chrome rather than an absent one: the frame is
/// exactly the content, the content sits at the frame's origin, and
/// there are no hitboxes because there is nothing to hit. That is the
/// same shape `reflow_frame` already synthesises for a fullscreen
/// window, and for the same reason.
///
/// `shaded_frame_height` is the full height: a client-decorated window
/// cannot be shaded (there is no titlebar of ours to roll it into), and
/// a zero here would let any code that shades one collapse it to
/// nothing.
fn frameless_layout(content: Size) -> DecorationLayout {
    DecorationLayout {
        frame_size: content,
        client_offset: Point::new(0, 0),
        titlebar_height: 0,
        button_hitboxes: Vec::new(),
        resize_hitboxes: Vec::new(),
        shaded_frame_height: content.h,
    }
}

/// The request/layout pair a *shaded* frame's decoration must be
/// rasterized from, derived from the unshaded pair the theme produced.
///
/// Shrinking the frame rect (`Backend::set_frame_geometry`) is only half
/// of rolling a window up. On X11 it passes for the whole thing by
/// accident: the frame is a real server window, so shortening it clips
/// whatever oversized pixmap was last blitted into it and the roll-up
/// looks right even though the buffer is still full height. A Wayland
/// frame has no such window — `wm-wayland`'s renderer composites the
/// decoration buffer at `frame.geometry.pos` at the *buffer's* own size
/// and never consults `frame.geometry.size` — so there the buffer IS the
/// frame's outline. Painting the unshaded buffer left the full-size
/// outline on screen (client area filled opaque black, resizebar and
/// all) while the unmapped content vanished behind it: exactly the
/// reported "double-click blanks the window instead of rolling it up".
///
/// `resizable: false` is not incidental. A rolled-up window has no
/// bottom edge to drag — `handle_frame_button_press` already refuses a
/// resize drag on a `SHADED` client — and `resizable` is precisely what
/// tells the theme to spend height on a resizebar. Left set, the theme
/// would still paint one, and at shaded height there is no room below
/// the titlebar for it to land: it would be drawn straight over the
/// titlebar's bottom edge. Clearing `resize_hitboxes` keeps the layout
/// self-consistent with that (hit-testing a shaded frame never reaches
/// them anyway — they sit below its bottom edge, outside the rect the
/// backends test against — but a layout that paints no grip must not
/// claim to have one). Neither of these is stored back on the client:
/// `Client::layout` deliberately keeps the unshaded shape, which is
/// what makes unshading exact, so this pair exists only for the length
/// of one `ThemeEngine::render` call.
fn shaded_paint_inputs(request: &DecorationRequest, layout: &DecorationLayout) -> (DecorationRequest, DecorationLayout) {
    let mut request = request.clone();
    request.resizable = false;
    let mut layout = layout.clone();
    layout.frame_size.h = layout.shaded_frame_height;
    layout.resize_hitboxes.clear();
    (request, layout)
}

/// Bounding box of two rects — how the per-monitor workareas collapse
/// into the one `_NET_WORKAREA` rect (see `publish_workarea_union`).
/// Not generic over "empty" rects: a monitor is never zero-sized in
/// practice, so a zero-sized input still contributes its origin rather
/// than being skipped, which keeps this a plain fold with no
/// first-non-empty bookkeeping.
fn union_rect(a: Rect, b: Rect) -> Rect {
    let left = a.pos.x.min(b.pos.x);
    let top = a.pos.y.min(b.pos.y);
    let right = (a.pos.x + a.size.w as i32).max(b.pos.x + b.size.w as i32);
    let bottom = (a.pos.y + a.size.h as i32).max(b.pos.y + b.size.h as i32);
    Rect {
        pos: Point::new(left, top),
        size: Size::new((right - left).max(0) as u32, (bottom - top).max(0) as u32),
    }
}

/// Squared distance from `point` to the nearest point of `rect`, `0`
/// for a point inside it — the ordering `monitor_index_at` picks its
/// nearest monitor by. Squared because only the ordering matters and a
/// square root would cost precision for nothing; `i64` because a
/// desktop spanning several 4K outputs has coordinates whose square
/// overflows `i32`.
fn squared_distance_to(rect: Rect, point: Point) -> i64 {
    // `Rect::contains` is half-open, so the last pixel actually inside
    // is one short of the far edge — measuring to the edge itself would
    // report a one-pixel gap as zero distance.
    let last_x = rect.pos.x as i64 + rect.size.w as i64 - 1;
    let last_y = rect.pos.y as i64 + rect.size.h as i64 - 1;
    let dx = (rect.pos.x as i64 - point.x as i64).max(point.x as i64 - last_x).max(0);
    let dy = (rect.pos.y as i64 - point.y as i64).max(point.y as i64 - last_y).max(0);
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_backend::{FakeBackend, FakeFrameId, FakeTheme, FakeWindowId};
    use crate::types::SizeHints;
    use wm_theme_api::Size;

    fn wm(backend: FakeBackend) -> WindowManager<FakeBackend> {
        WindowManager::new(backend, Box::new(FakeTheme))
    }

    fn alt_tab() -> BackendEvent<FakeWindowId, FakeFrameId> {
        BackendEvent::KeyPress(KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT })
    }

    fn alt_shift_tab() -> BackendEvent<FakeWindowId, FakeFrameId> {
        BackendEvent::KeyPress(KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT | Modifiers::SHIFT })
    }

    fn alt_release() -> BackendEvent<FakeWindowId, FakeFrameId> {
        BackendEvent::KeyRelease(KeyCombo { keysym: XK_ALT_L, modifiers: Modifiers::ALT })
    }

    fn escape() -> BackendEvent<FakeWindowId, FakeFrameId> {
        BackendEvent::KeyPress(KeyCombo { keysym: XK_ESCAPE, modifiers: Modifiers::empty() })
    }

    #[test]
    fn alt_tab_cycles_focus_to_the_next_client_and_raises_it() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id1 = wm.client_for_window(w1).unwrap();
        let id2 = wm.client_for_window(w2).unwrap();
        assert!(wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED), "mapping w2 last should focus it");

        wm.dispatch(alt_tab());
        // Modal: pressing Tab only moves the switcher selection — focus
        // must not change until Alt is released.
        assert!(wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED), "selection alone must not move focus");
        let (entries, selected) = wm.cycle_state().expect("session should be active");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[selected].0, id1, "first Tab selects the next client");

        wm.dispatch(alt_release());

        assert!(wm.cycle_state().is_none(), "releasing Alt ends the session");
        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED));
        let frame1 = wm.client(id1).unwrap().frame.unwrap();
        assert!(wm.backend().raised_frames.contains(&frame1), "cycling must raise the newly-focused window");
    }

    #[test]
    fn committing_to_a_shaded_client_unshades_it() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id1 = wm.client_for_window(w1).unwrap();
        wm.shade(id1);
        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::SHADED));

        // Focused is w2; one Tab selects w1 (shaded), commit.
        wm.dispatch(alt_tab());
        wm.dispatch(alt_release());

        let client = wm.client(id1).unwrap();
        assert!(!client.flags.contains(ClientFlags::SHADED), "cycling to a shaded window must unroll it");
        assert!(client.flags.contains(ClientFlags::FOCUSED));
    }

    #[test]
    fn escape_cancels_the_switcher_without_moving_focus() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id2 = wm.client_for_window(w2).unwrap();

        wm.dispatch(alt_tab());
        assert!(wm.cycle_state().is_some());
        wm.dispatch(escape());

        assert!(wm.cycle_state().is_none(), "Escape ends the session");
        assert!(wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED), "cancelling must leave focus untouched");
    }

    #[test]
    fn alt_tab_wraps_around_past_the_last_client() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id1 = wm.client_for_window(w1).unwrap();
        let id2 = wm.client_for_window(w2).unwrap();

        wm.dispatch(alt_tab()); // selection: w2 -> w1
        wm.dispatch(alt_tab()); // selection: w1 -> wraps to w2
        wm.dispatch(alt_release());
        let _ = id1;
        assert!(wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED), "cycling forward from the last client must wrap to the first");
    }

    #[test]
    fn alt_shift_tab_cycles_backward() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id1 = wm.client_for_window(w1).unwrap();
        let id2 = wm.client_for_window(w2).unwrap();

        // Focused is w2; Alt+Tab (forward) goes to w1; Alt+Shift+Tab
        // (backward) from w1 must go back to w2, not onward to w1 again.
        wm.dispatch(alt_tab());
        wm.dispatch(alt_shift_tab());
        wm.dispatch(alt_release());
        let _ = id1;
        assert!(wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED));
    }

    #[test]
    fn alt_tab_skips_miniaturized_clients() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let w3 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        wm.dispatch(BackendEvent::MapRequest(w3));
        let id1 = wm.client_for_window(w1).unwrap();
        let id2 = wm.client_for_window(w2).unwrap();
        let id3 = wm.client_for_window(w3).unwrap();
        wm.miniaturize(id2);

        // Focused is w3 (mapped last); Alt+Tab must skip miniaturized w2
        // entirely and land on w1.
        wm.dispatch(alt_tab());
        wm.dispatch(alt_release());

        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id3).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id2).unwrap().flags.intersects(ClientFlags::FOCUSED), "a miniaturized client must never be cycled to");
    }

    /// Clicking the window that already looks focused is how a user
    /// tries to *repair* focus, so that path must re-assert it rather
    /// than trust `self.focused`. Regression test for a live Wayland
    /// bug: an XWayland window focused before its `wl_surface` existed
    /// left `wm-core` focused and the seat empty, and every click on
    /// the window hit the "already focused" early return without
    /// re-sending focus — so it stayed keyboard-dead until the user
    /// focused a different window and came back.
    #[test]
    fn clicking_the_already_focused_window_reasserts_input_focus() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        assert_eq!(wm.focused_client(), Some(id));
        assert_eq!(wm.backend().focused_window, Some(window));

        // Stands in for the display server never taking the focus the
        // WM believes it set.
        wm.backend_mut().focused_window = None;

        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: Point::new(200, 5),
            button: MouseButton::Left,
            pressed: true,
            time_ms: 0,
            mods: Modifiers::empty(),
        });

        assert_eq!(wm.focused_client(), Some(id), "the WM's own focus is unchanged");
        assert_eq!(
            wm.backend().focused_window,
            Some(window),
            "input focus must be re-asserted on the early-return path, not assumed"
        );
    }

    /// Regression test: clicking a window's own *content* (not just its
    /// titlebar) must be able to focus it. This depends on a passive
    /// button grab existing on every unfocused client — without one, a
    /// click on unfocused content never reaches `wm-core` at all (the
    /// client itself receives it directly, unintercepted), and only
    /// clicking the frame/titlebar (which `wm-core` always sees, being
    /// its own window) ever changed focus.
    #[test]
    fn clicking_an_unfocused_clients_content_focuses_it() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id1 = wm.client_for_window(w1).unwrap();
        let id2 = wm.client_for_window(w2).unwrap();

        // w2 was mapped last, so it's focused — meaning w1, now
        // unfocused, must have a passive grab so a content click on it
        // can be seen at all, and w2 (focused) must not have one, so
        // its own clicks go straight to it.
        assert!(wm.backend().passively_grabbed.contains(&w1), "unfocused client must be passively grabbed");
        assert!(!wm.backend().passively_grabbed.contains(&w2), "focused client must not be grabbed");

        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Client(w1),
            local: Point::new(50, 50),
            button: MouseButton::Left,
            pressed: true,
            time_ms: 0,
            mods: Modifiers::empty(),
        });

        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED), "clicking content must focus the client");
        assert!(!wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.backend().passively_grabbed.contains(&w1), "now focused, w1 must be ungrabbed");
        assert!(wm.backend().passively_grabbed.contains(&w2), "now unfocused, w2 must become grabbed");
        assert_eq!(wm.backend().replay_pointer_calls, 1, "the triggering click must be replayed through to the client");
    }

    /// Regression test: hovering a resize corner/edge must set a
    /// distinct cursor there (the visual half of the resize affordance —
    /// `render_decoration`'s grip marks are the other half), and it
    /// must revert to the default the moment the pointer moves off the
    /// hitbox onto plain frame area, not stick.
    #[test]
    fn hovering_a_resize_corner_sets_a_distinct_cursor() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let se_corner = wm.client(id).unwrap().layout.resize_hitboxes.iter().find(|(e, _)| *e == ResizeEdge::SouthEast).unwrap().1.pos;

        wm.dispatch(BackendEvent::PointerMotion {
            root: Point::new(0, 0),
            surface_local: Some((SurfaceRef::Frame(frame), se_corner)),
        });
        assert_eq!(wm.backend().frame_cursor.get(&frame), Some(&Some(ResizeEdge::SouthEast)), "hovering the SE corner must set the SE resize cursor");

        wm.dispatch(BackendEvent::PointerMotion {
            root: Point::new(0, 0),
            surface_local: Some((SurfaceRef::Frame(frame), Point::new(5, 5))), // plain titlebar area
        });
        assert_eq!(wm.backend().frame_cursor.get(&frame), Some(&None), "moving off the hitbox must revert to the default cursor");
    }

    #[test]
    fn pointer_enter_is_ignored_under_the_default_click_to_focus_policy() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id1 = wm.client_for_window(w1).unwrap();
        let frame1 = wm.client(id1).unwrap().frame.unwrap();

        wm.dispatch(BackendEvent::PointerEnter { surface: SurfaceRef::Frame(frame1) });

        assert!(!wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED), "click-to-focus must not react to a bare pointer-enter");
    }

    #[test]
    fn focus_follows_mouse_focuses_on_pointer_enter() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.set_focus_policy(FocusPolicy::FocusFollowsMouse);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        let id1 = wm.client_for_window(w1).unwrap();
        let id2 = wm.client_for_window(w2).unwrap();
        let frame1 = wm.client(id1).unwrap().frame.unwrap();
        assert!(wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED), "mapping w2 last should focus it");

        wm.dispatch(BackendEvent::PointerEnter { surface: SurfaceRef::Frame(frame1) });

        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED));
    }

    #[test]
    fn new_clients_map_onto_the_current_workspace() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.switch_workspace(2);

        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.client(id).unwrap().workspace, 2);
    }

    #[test]
    fn switching_workspace_hides_clients_not_on_it_and_shows_clients_that_are() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1)); // lands on workspace 0
        let id1 = wm.client_for_window(w1).unwrap();
        let frame1 = wm.client(id1).unwrap().frame.unwrap();
        wm.switch_workspace(1);
        wm.dispatch(BackendEvent::MapRequest(w2)); // lands on workspace 1
        let id2 = wm.client_for_window(w2).unwrap();
        let frame2 = wm.client(id2).unwrap().frame.unwrap();
        assert!(wm.backend().mapped_frames.contains(&frame2));

        wm.switch_workspace(0);

        assert!(wm.backend().mapped_frames.contains(&frame1), "workspace 0's client must be shown again");
        assert!(wm.backend().unmapped_frames.contains(&frame2), "workspace 1's client must be hidden");
        assert_eq!(wm.current_workspace(), 0);
    }

    /// Regression test: same bug as `deminiaturize_repaints_the_frame_
    /// after_remapping`, different trigger — switching back to a
    /// workspace remapped its clients' frames via `map_frame` alone,
    /// with no repaint, so a frame without backing-store could come back
    /// blank instead of showing its titlebar again.
    #[test]
    fn switching_back_to_a_workspace_repaints_its_remapped_frames() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1)); // lands on workspace 0
        let id1 = wm.client_for_window(w1).unwrap();
        let frame1 = wm.client(id1).unwrap().frame.unwrap();
        wm.switch_workspace(1);
        let paints_before = wm.backend().paint_count.get(&frame1).copied().unwrap_or(0);

        wm.switch_workspace(0);

        let paints_after = wm.backend().paint_count.get(&frame1).copied().unwrap_or(0);
        assert!(paints_after > paints_before, "switching back onto a workspace must repaint the frames it just remapped");
    }

    #[test]
    fn switching_workspace_grows_the_row_on_demand() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);
        assert_eq!(wm.workspace_count(), 1);

        wm.switch_workspace(3);

        assert_eq!(wm.workspace_count(), 4, "switching to index 3 means 4 workspaces (0..=3) now exist");
    }

    #[test]
    fn switching_away_defocuses_a_client_left_behind() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        assert!(wm.client(id).unwrap().flags.contains(ClientFlags::FOCUSED));

        wm.switch_workspace(1);

        assert!(!wm.client(id).unwrap().flags.contains(ClientFlags::FOCUSED), "a client hidden by a workspace switch must not stay marked focused");
    }

    #[test]
    fn miniaturized_clients_are_unaffected_by_workspace_switching() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        wm.miniaturize(id);

        wm.switch_workspace(1);
        wm.switch_workspace(0);

        // Still miniaturized, not silently remapped by the round trip.
        assert_eq!(wm.client(id).unwrap().lifecycle, Lifecycle::Miniaturized);
    }

    /// Regression test: a window miniaturized on workspace 0 and
    /// restored while viewing workspace 1 stayed *assigned* to 0 — it
    /// appeared over workspace 1 (the map is unconditional) but
    /// lingered on 0 after the next switch back and vanished from 1,
    /// because `deminiaturize` never updated the client's workspace.
    /// Restoring must adopt the workspace that's active at restore
    /// time: the icon tiles are visible everywhere, so the gesture
    /// means "bring it here".
    #[test]
    fn deminiaturize_restores_onto_the_active_workspace() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();

        wm.miniaturize(id);
        wm.switch_workspace(1);
        wm.deminiaturize(id);

        assert_eq!(wm.client(id).unwrap().workspace, 1, "restore must adopt the active workspace");
        assert!(wm.backend().mapped_frames.contains(&frame), "restored window must be visible here");
        assert_eq!(
            wm.backend().published_window_desktops.last(),
            Some(&(window, 1)),
            "pagers must hear the new workspace via _NET_WM_DESKTOP"
        );

        // The exact ghost the bug produced: back on workspace 0 the
        // window must be gone, and on workspace 1 it must be present.
        wm.switch_workspace(0);
        assert!(wm.backend().unmapped_frames.contains(&frame), "the old workspace must not keep the window");
        wm.switch_workspace(1);
        assert!(wm.backend().mapped_frames.contains(&frame), "the adopting workspace keeps the window");
    }

    /// Restoring on the same workspace the window was miniaturized on
    /// must not churn state: no workspace change, no `_NET_WM_DESKTOP`
    /// republish.
    #[test]
    fn deminiaturize_on_the_same_workspace_republishes_nothing() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();

        wm.miniaturize(id);
        let publishes_before = wm.backend().published_window_desktops.len();
        wm.deminiaturize(id);

        assert_eq!(wm.client(id).unwrap().workspace, 0);
        assert_eq!(
            wm.backend().published_window_desktops.len(),
            publishes_before,
            "a same-workspace restore must not republish the desktop property"
        );
    }

    #[test]
    fn move_client_to_workspace_hides_it_when_not_on_the_active_workspace() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();

        wm.move_client_to_workspace(id, 1);

        assert_eq!(wm.client(id).unwrap().workspace, 1);
        assert!(wm.backend().unmapped_frames.contains(&frame));
        assert_eq!(wm.current_workspace(), 0, "moving a client must not itself change the active workspace");
    }

    // Workspace keybindings are config-driven from the binary now
    // (see `bind_default_keys`), so the public methods ARE the
    // dispatch path a bound key takes — these tests exercise them
    // directly instead of synthesizing hardcoded arrow-key presses.

    #[test]
    fn switch_workspace_round_trips() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);

        wm.switch_workspace(1);
        assert_eq!(wm.current_workspace(), 1);

        wm.switch_workspace(0);
        assert_eq!(wm.current_workspace(), 0);
    }

    #[test]
    fn switching_to_the_current_workspace_is_a_complete_no_op() {
        // The "don't go below zero" guard lives with the keybinding
        // driver in the binary; what wm-core itself guarantees is that
        // a same-index switch changes nothing and publishes nothing —
        // a pager re-sending the current desktop must not trigger a
        // spurious remap/republish storm.
        let backend = FakeBackend::new();
        let mut wm = wm(backend);
        let publishes_before = wm.backend().published_workspaces.len();

        wm.switch_workspace(0);

        assert_eq!(wm.current_workspace(), 0);
        assert_eq!(wm.backend().published_workspaces.len(), publishes_before, "a same-workspace switch must not re-publish");
    }

    /// The carry gesture (formerly hardcoded Alt+Shift+arrows) is
    /// driven from the binary now as move + switch + re-focus through
    /// the public API. This locks in that the sequence works end to
    /// end — in particular that focus can be re-established after
    /// `move_client_to_workspace` deliberately drops it (the client
    /// leaves the active workspace mid-sequence), since carrying one
    /// window across several workspaces in repeated presses depends on
    /// it ending up focused after every hop.
    #[test]
    fn carrying_a_client_via_public_calls_moves_switches_and_refocuses() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        assert_eq!(
            wm.backend().published_window_desktops.last(),
            Some(&(window, 0)),
            "manage time must publish the window's initial workspace"
        );

        wm.move_client_to_workspace(id, 1);
        assert!(!wm.client(id).unwrap().flags.contains(ClientFlags::FOCUSED), "focus drops the moment the client leaves the active workspace");
        wm.switch_workspace(1);
        wm.dispatch(BackendEvent::ActivateRequested(window));

        assert_eq!(wm.current_workspace(), 1, "the switch must follow the carried client (growing the row on demand)");
        assert_eq!(wm.client(id).unwrap().workspace, 1);
        assert!(wm.backend().mapped_frames.contains(&frame), "the carried client must be visible on arrival");
        assert!(wm.client(id).unwrap().flags.contains(ClientFlags::FOCUSED), "the carried client must end up focused, so a repeated carry keeps carrying it");
        assert_eq!(wm.backend().published_window_desktops.last(), Some(&(window, 1)), "the move must re-publish _NET_WM_DESKTOP");
    }

    #[test]
    fn moving_a_client_to_its_own_workspace_is_a_no_op() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let desktops_before = wm.backend().published_window_desktops.len();

        wm.move_client_to_workspace(id, 0);

        assert_eq!(wm.client(id).unwrap().workspace, 0);
        assert!(!wm.backend().unmapped_frames.contains(&frame), "a move that changes nothing must not hide the frame");
        assert!(wm.client(id).unwrap().flags.contains(ClientFlags::FOCUSED), "a no-op move must not drop focus");
        assert_eq!(wm.backend().published_window_desktops.len(), desktops_before, "no _NET_WM_DESKTOP re-publish for a move that changes nothing");
    }

    #[test]
    fn desktop_switch_request_switches_the_workspace() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);

        // A pager's `_NET_CURRENT_DESKTOP` message — must behave
        // exactly like the keyboard switch, growth on demand included.
        wm.dispatch(BackendEvent::DesktopSwitchRequested(2));

        assert_eq!(wm.current_workspace(), 2);
        assert_eq!(wm.workspace_count(), 3, "switching to index 2 means 3 workspaces (0..=2) now exist");
    }

    #[test]
    fn window_desktop_request_moves_the_window_and_hides_it() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();

        // A pager's `_NET_WM_DESKTOP` message: move the window, but —
        // unlike the Alt+Shift keyboard combos — never follow it.
        wm.dispatch(BackendEvent::WindowDesktopRequested { window, desktop: 1 });

        assert_eq!(wm.client(id).unwrap().workspace, 1);
        assert!(wm.backend().unmapped_frames.contains(&frame), "a window sent off the active workspace must be hidden");
        assert_eq!(wm.current_workspace(), 0, "a pager moving a window must not switch the active workspace");
        assert_eq!(wm.backend().published_window_desktops.last(), Some(&(window, 1)));
    }

    #[test]
    fn alt_tab_skips_clients_parked_on_other_workspaces() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let w3 = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(w1));
        wm.dispatch(BackendEvent::MapRequest(w2));
        wm.dispatch(BackendEvent::MapRequest(w3));
        let id1 = wm.client_for_window(w1).unwrap();
        let id2 = wm.client_for_window(w2).unwrap();
        let id3 = wm.client_for_window(w3).unwrap();
        wm.move_client_to_workspace(id2, 1);

        // Focused is w3 (mapped last); Alt+Tab must skip w2 — still
        // `Lifecycle::Normal`, but parked on workspace 1 with its frame
        // unmapped — and land on w1. Cycling onto it would set input
        // focus on a window that isn't visible anywhere on screen.
        wm.dispatch(alt_tab());
        wm.dispatch(alt_release());

        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id3).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED), "a client on another workspace must never be cycled to");
    }

    #[test]
    fn alt_tab_with_no_mapped_clients_does_not_panic() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);

        wm.dispatch(alt_tab());

        assert_eq!(wm.client_count(), 0);
    }

    #[test]
    fn map_request_tracks_a_new_client() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        assert_eq!(wm.client_count(), 1);
        assert!(wm.client_for_window(window).is_some());
    }

    /// Regression test: a client mapping at (0, 0) — the extremely
    /// common "don't care, WM decides" placeholder position most toolkits
    /// use — must not end up with its frame (and titlebar) pushed to
    /// negative coordinates, off-screen. Caught visually: a real xterm's
    /// titlebar was rendering entirely above the top edge of the screen.
    #[test]
    fn mapping_at_origin_keeps_the_frame_on_screen() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(0, 0), size: Size::new(300, 200) });
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let frame_pos = wm.backend().last_frame_geometry.get(&frame).unwrap().pos;
        assert!(frame_pos.x >= 0 && frame_pos.y >= 0, "frame should stay on-screen, got {frame_pos:?}");
    }

    #[test]
    fn a_client_that_draws_its_own_chrome_is_managed_but_never_framed() {
        // The two-titlebar bug, stated as an assertion. Edge and
        // LibreOffice ask not to be decorated; before this, the window
        // manager framed them anyway and they wore both.
        //
        // "Managed but not framed" is the whole point: the window still
        // has to be tracked, focused and workspaced like any other, so
        // asserting the absence of a frame is only half of it.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(40, 30), size: Size::new(800, 600) });
        backend.set_client_draws_own_chrome(window, true);
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).expect("a client-decorated window is still managed");
        let client = wm.client(id).unwrap();
        assert!(client.frame.is_none(), "a client that drew its own chrome must not be framed");
        assert_eq!(client.chrome, ClientChrome::ClientDrawn);
        assert!(wm.backend().mapped_frameless.contains(&window), "it still has to be shown");
        // Its layout must describe having no chrome rather than being
        // left at whatever the theme would have said: every hit-test and
        // drag calculation reads these.
        assert_eq!(client.layout.titlebar_height, 0);
        assert_eq!(client.layout.client_offset, Point::new(0, 0));
        assert_eq!(client.layout.frame_size, Size::new(800, 600));
        // And it must publish four zeros rather than nothing at all. A
        // client born undecorated never goes through `create_decoration`
        // or `release_decoration`, so if the property were published by
        // the backend's decoration verbs alone it would never appear for
        // exactly the windows it matters most to. It is published from
        // here, at the end of the map, for both kinds of window.
        assert_eq!(
            wm.backend().frame_extents.get(&window),
            Some(&(0, 0, 0, 0)),
            "a window born frameless must still publish its (zero) extents"
        );
    }

    #[test]
    fn a_client_that_starts_drawing_its_own_chrome_loses_its_frame_in_place() {
        // Applications rewrite `_MOTIF_WM_HINTS` on a mapped window —
        // a browser leaving fullscreen, a "use system title bar"
        // setting being toggled. What must not happen is the content
        // jumping: the user is looking at the application's pixels, and
        // only the frame around them is going away.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(40, 30), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        let framed = wm.client(id).unwrap();
        let frame = framed.frame.expect("it starts framed");
        let content_before = framed.geometry;

        wm.backend_mut().set_client_draws_own_chrome(window, true);
        wm.dispatch(BackendEvent::ChromeChanged(window));

        let client = wm.client(id).unwrap();
        assert!(client.frame.is_none());
        assert_eq!(client.chrome, ClientChrome::ClientDrawn);
        assert_eq!(client.geometry, content_before, "the content must not move when the frame goes");
        // `release_decoration`, not `destroy_decoration`: on X11 the
        // client is a child of the frame, and destroying a parent
        // destroys its children. Getting this wrong closes the
        // application.
        assert!(wm.backend().released_frames.contains(&frame), "the frame must be released, not destroyed");
        assert!(wm.backend().mapped_frameless.contains(&window), "the window stays on screen throughout");
    }

    #[test]
    fn a_client_that_stops_drawing_its_own_chrome_gains_a_frame_around_its_content() {
        // The same transition in reverse, and the same fixed point: the
        // frame is built around the content where it already is, which
        // means the frame's own origin moves up and left by the chrome
        // offset rather than the content moving down and right.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(40, 30), size: Size::new(800, 600) });
        backend.set_client_draws_own_chrome(window, true);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        let content_before = wm.client(id).unwrap().geometry;

        wm.backend_mut().set_client_draws_own_chrome(window, false);
        wm.dispatch(BackendEvent::ChromeChanged(window));

        let client = wm.client(id).unwrap();
        let frame = client.frame.expect("it must be framed again");
        assert_eq!(client.chrome, ClientChrome::ServerDrawn);
        assert_eq!(client.geometry, content_before, "the content must not move when the frame arrives");
        assert!(client.layout.titlebar_height > 0, "it wears real chrome now");
        assert!(wm.backend().mapped_frames.contains(&frame));
        // The frame must be reachable by id again, or every later click
        // on this window's chrome resolves to no client.
        assert_eq!(wm.client_for_frame(frame), Some(id));
    }

    #[test]
    fn a_frameless_window_follows_its_workspace_off_and_on_screen() {
        // The `unmap_frameless` backend verb exists for exactly this:
        // with the no-op default, a client-decorated window stays
        // visible on every workspace at once.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(0, 0), size: Size::new(400, 300) });
        backend.set_client_draws_own_chrome(window, true);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        assert!(wm.backend().mapped_frameless.contains(&window));

        wm.switch_workspace(1);
        assert!(!wm.backend().mapped_frameless.contains(&window), "it must leave with its workspace");

        wm.switch_workspace(0);
        assert!(wm.backend().mapped_frameless.contains(&window), "and come back with it");
    }

    #[test]
    fn frame_extents_describe_the_chrome_and_go_to_zero_when_it_does() {
        // `_NET_FRAME_EXTENTS` is how a client learns how much bigger
        // than its content the thing on screen is. Publishing zeros for
        // a frameless window is not a degenerate case to skip: it is
        // the message that the frame has gone.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(40, 30), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        let (left, right, top, bottom) = wm.backend().frame_extents[&window];
        let layout = wm.client(id).unwrap().layout.clone();
        assert_eq!(top, layout.client_offset.y as u32, "the top extent is the titlebar");
        assert!(top > 0, "a framed window has chrome above its content");
        assert_eq!(left, layout.client_offset.x as u32);
        // The four must add up to the frame the window actually wears,
        // or a client reasoning from them lands off by the difference.
        assert_eq!(left + 800 + right, layout.frame_size.w);
        assert_eq!(top + 600 + bottom, layout.frame_size.h);

        wm.backend_mut().set_client_draws_own_chrome(window, true);
        wm.dispatch(BackendEvent::ChromeChanged(window));
        assert_eq!(wm.backend().frame_extents[&window], (0, 0, 0, 0), "losing the frame must be published, not just done");
    }

    #[test]
    fn focusing_a_frameless_window_raises_it() {
        // Every raise in this crate used to name a `FrameId`, guarded by
        // `if let Some(frame)`. A client-decorated window has none, so
        // it mapped at one depth and stayed there for the rest of its
        // life: clicking it focused it and did not bring it forward.
        // The bug is invisible in testing precisely because it only
        // affects applications that draw their own titlebars.
        let mut backend = FakeBackend::new();
        let under = backend.create_window();
        let over = backend.create_window();
        backend.set_geometry(under, Rect { pos: Point::new(0, 0), size: Size::new(400, 300) });
        backend.set_geometry(over, Rect { pos: Point::new(0, 0), size: Size::new(400, 300) });
        backend.set_client_draws_own_chrome(over, true);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(under));
        wm.dispatch(BackendEvent::MapRequest(over));

        let over_id = wm.client_for_window(over).unwrap();
        let under_id = wm.client_for_window(under).unwrap();
        wm.focus_client(under_id);
        wm.backend_mut().raised_frameless.clear();

        wm.focus_client(over_id);
        assert_eq!(
            wm.backend().raised_frameless.last(),
            Some(&over),
            "focusing a client-decorated window must bring it forward, not only focus it"
        );
    }

    #[test]
    fn a_client_decorated_window_can_still_be_moved_by_asking() {
        // Taking our titlebar away takes away the only handle this
        // window manager offered for dragging the window. If the
        // client's own `_NET_WM_MOVERESIZE` is dropped as well — as
        // both backends used to — the window becomes pinned wherever it
        // first mapped, which is a worse outcome than the spare
        // titlebar removing the chrome was meant to fix.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(100, 100), size: Size::new(400, 300) });
        backend.set_client_draws_own_chrome(window, true);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));

        // The pointer has to be somewhere known before a client can ask
        // to be dragged by it.
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(150, 120), surface_local: None });
        wm.dispatch(BackendEvent::MoveRequest(window));
        // Grab offset is (50, 20) into the window; dragging the pointer
        // to (250, 220) must put the window's origin at (200, 200).
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(250, 220), surface_local: None });

        assert_eq!(
            wm.backend().last_client_position.get(&window),
            Some(&Point::new(200, 200)),
            "the window must follow the pointer with the offset it was grabbed at"
        );
        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.client(id).unwrap().geometry.pos, Point::new(200, 200), "and the core must agree where it is");
    }

    #[test]
    fn a_drag_on_a_frameless_window_ends_when_the_button_comes_up() {
        // The bug this guards, reported from a real session dragging
        // LibreOffice: the window followed the cursor with no button
        // held. A window whose client draws its own chrome has no frame
        // for the release to land on, so the release arrives against
        // the *client* — and the old code only ended a drag on a frame
        // release. The pointer grab has to come back too, because a
        // leaked one freezes the pointer for every client on the
        // session.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(100, 100), size: Size::new(400, 300) });
        backend.set_client_draws_own_chrome(window, true);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));

        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(150, 120), surface_local: None });
        wm.dispatch(BackendEvent::MoveRequest(window));
        assert_eq!(wm.backend().outstanding_pointer_grabs, 1, "a drag must take the pointer");

        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(250, 220), surface_local: None });
        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.client(id).unwrap().geometry.pos, Point::new(200, 200), "it tracks while held");

        // Released over the client, which is where the pointer is.
        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Client(window),
            local: Point::new(0, 0),
            button: MouseButton::Left,
            pressed: false,
            time_ms: 0,
            mods: Modifiers::empty(),
        });
        assert_eq!(wm.backend().outstanding_pointer_grabs, 0, "and gives the pointer back");

        // Moving afterwards must not move the window any more.
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(600, 600), surface_local: None });
        assert_eq!(
            wm.client(id).unwrap().geometry.pos,
            Point::new(200, 200),
            "the window must stay where it was dropped, not follow the cursor"
        );
    }

    #[test]
    fn a_drag_whose_window_disappears_gives_the_pointer_back() {
        // The other way a leaked grab happens: the dragged client dies
        // mid-drag. A pointer grab with nothing left to move is a
        // frozen desktop.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(0, 0), size: Size::new(400, 300) });
        backend.set_client_draws_own_chrome(window, true);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(10, 10), surface_local: None });
        wm.dispatch(BackendEvent::MoveRequest(window));
        assert_eq!(wm.backend().outstanding_pointer_grabs, 1);

        wm.dispatch(BackendEvent::Destroyed(window));
        assert_eq!(wm.backend().outstanding_pointer_grabs, 0, "the grab must not outlive the window");
    }

    #[test]
    fn an_ordinary_client_is_still_framed() {
        // The regression guard for the fix itself: the default answer
        // to "does this client draw its own chrome" is no, and every
        // ordinary X11 application depends on that staying true.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(10, 10), size: Size::new(300, 200) });
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        let client = wm.client(wm.client_for_window(window).unwrap()).unwrap();
        assert_eq!(client.chrome, ClientChrome::ServerDrawn);
        assert!(client.frame.is_some());
    }

    #[test]
    fn map_request_creates_and_maps_a_decorated_frame() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(10, 10), size: Size::new(300, 200) });
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        let client = wm.client(id).unwrap();
        let frame = client.frame.expect("frame should have been created");
        assert!(wm.backend().mapped_frames.contains(&frame));
        assert!(wm.backend().painted_frames.contains(&frame));
    }

    #[test]
    fn mapping_a_window_focuses_it() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.focused_client(), Some(id));
        assert!(wm.client(id).unwrap().flags.contains(ClientFlags::FOCUSED));
    }

    #[test]
    fn duplicate_map_request_is_ignored() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));
        wm.dispatch(BackendEvent::MapRequest(window));

        assert_eq!(wm.client_count(), 1);
    }

    #[test]
    fn unmap_stops_tracking_the_client_and_destroys_its_frame() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        wm.dispatch(BackendEvent::Unmapped(window));

        assert_eq!(wm.client_count(), 0);
        assert!(wm.client_for_window(window).is_none());
        assert!(wm.backend().destroyed_frames.contains(&frame));
    }

    #[test]
    fn destroy_stops_tracking_the_client() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));
        wm.dispatch(BackendEvent::Destroyed(window));

        assert_eq!(wm.client_count(), 0);
    }

    #[test]
    fn map_request_captures_window_title() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_title(window, "xterm");
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.client(id).unwrap().title, "xterm");
    }

    #[test]
    fn title_changed_after_map_updates_the_client_and_repaints() {
        // Regression test: many real apps (a terminal whose shell sets
        // its title only once the prompt is ready) set `WM_NAME` well
        // after their first `MapRequest`, not before it — a WM that only
        // reads the title once at map time would show a permanently
        // blank titlebar for them.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.client(id).unwrap().title, "", "no WM_NAME was set yet at map time");

        wm.backend_mut().set_title(window, "chrisk@imac:~/chonkstep");
        wm.dispatch(BackendEvent::TitleChanged(window));

        assert_eq!(wm.client(id).unwrap().title, "chrisk@imac:~/chonkstep");
        let frame = wm.client(id).unwrap().frame.unwrap();
        assert!(wm.backend().painted_frames.contains(&frame));
    }

    #[test]
    fn title_changed_on_an_unmanaged_window_is_ignored() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);

        // Never mapped — must not panic or create tracking state.
        wm.dispatch(BackendEvent::TitleChanged(window));

        assert!(wm.client_for_window(window).is_none());
    }

    #[test]
    fn poll_event_driven_dispatch_matches_direct_dispatch() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.push_event(BackendEvent::MapRequest(window));
        let mut wm = wm(backend);

        while let Some(event) = wm.backend_mut().poll_event() {
            wm.dispatch(event);
        }

        assert_eq!(wm.client_count(), 1);
    }

    #[test]
    fn close_button_click_sends_close_to_the_client() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let close_point = wm.client(id).unwrap().layout.button_hitboxes[0].1.pos;

        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: close_point,
            button: MouseButton::Left,
            pressed: true,
            time_ms: 0,
            mods: Modifiers::empty(),
        });
        // Pressing alone must not fire the action yet — only committed
        // on release-while-still-over (see `press_arms_but_does_not_commit_the_close_action`).
        assert!(!wm.backend().close_requests.contains(&window));

        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: close_point,
            button: MouseButton::Left,
            pressed: false,
            time_ms: 0,
            mods: Modifiers::empty(),
        });

        assert!(wm.backend().close_requests.contains(&window));
    }

    #[test]
    fn press_arms_but_does_not_commit_the_close_action() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let close_point = wm.client(id).unwrap().layout.button_hitboxes[0].1.pos;

        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: close_point,
            button: MouseButton::Left,
            pressed: true,
            time_ms: 0,
            mods: Modifiers::empty(),
        });

        assert!(!wm.backend().close_requests.contains(&window), "close must not fire on press alone");
        assert!(wm.backend().painted_frames.contains(&frame), "the pressed/sunken visual should have been painted");
    }

    #[test]
    fn releasing_away_from_the_button_cancels_the_action() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let close_point = wm.client(id).unwrap().layout.button_hitboxes[0].1.pos;

        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: close_point,
            button: MouseButton::Left,
            pressed: true,
            time_ms: 0,
            mods: Modifiers::empty(),
        });
        // Release somewhere clearly outside the close button's hitbox.
        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: Point::new(close_point.x + 500, close_point.y + 500),
            button: MouseButton::Left,
            pressed: false,
            time_ms: 0,
            mods: Modifiers::empty(),
        });

        assert!(!wm.backend().close_requests.contains(&window), "dragging off the button should cancel, not close");
    }

    #[test]
    fn miniaturize_button_click_unmaps_the_frame() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let miniaturize_point = wm.client(id).unwrap().layout.button_hitboxes[1].1.pos;

        for pressed in [true, false] {
            wm.dispatch(BackendEvent::PointerButton {
                surface: SurfaceRef::Frame(frame),
                local: miniaturize_point,
                button: MouseButton::Left,
                pressed,
                time_ms: 0,
                mods: Modifiers::empty(),
            });
        }

        assert_eq!(wm.client(id).unwrap().lifecycle, Lifecycle::Miniaturized);
        assert!(wm.backend().unmapped_frames.contains(&frame));
    }

    #[test]
    fn miniaturize_and_deminiaturize_notify_the_shell() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.take_notification(), Some(Notification::Mapped(id)));

        wm.miniaturize(id);
        assert_eq!(wm.take_notification(), Some(Notification::Miniaturized(id, None)));
        assert_eq!(wm.take_notification(), None);

        wm.deminiaturize(id);
        assert_eq!(wm.take_notification(), Some(Notification::Deminiaturized(id)));
        assert_eq!(wm.client(id).unwrap().lifecycle, Lifecycle::Normal);
    }

    /// Regression test: `deminiaturize` remapped the frame via
    /// `map_frame` but never repainted it, relying entirely on an
    /// `Expose` reply to restore its content — not guaranteed, since a
    /// frame without backing-store isn't required to retain its pixel
    /// content while unmapped. On real hardware this showed up as a
    /// window's titlebar coming back solid black (buttons still in the
    /// right place, since the frame itself was fine — just never
    /// repainted) after being miniaturized and restored.
    #[test]
    fn deminiaturize_repaints_the_frame_after_remapping() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let paints_before = wm.backend().paint_count.get(&frame).copied().unwrap_or(0);

        wm.miniaturize(id);
        wm.deminiaturize(id);

        let paints_after = wm.backend().paint_count.get(&frame).copied().unwrap_or(0);
        assert!(paints_after > paints_before, "deminiaturize must repaint the frame it just remapped, not rely on Expose");
    }

    #[test]
    fn closing_a_miniaturized_client_notifies_removal() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        assert_eq!(wm.take_notification(), Some(Notification::Mapped(id)));

        wm.miniaturize(id);
        assert_eq!(wm.take_notification(), Some(Notification::Miniaturized(id, None)));

        wm.dispatch(BackendEvent::Destroyed(window));
        assert_eq!(wm.take_notification(), Some(Notification::Removed(id)));
    }

    #[test]
    fn dragging_a_shaded_window_keeps_it_shaded_instead_of_unrolling() {
        // Regression test: `handle_pointer_motion` used to push the
        // client's full unshaded `frame_size` to the backend on every
        // drag motion event, regardless of the `SHADED` flag — so
        // dragging a rolled-up window visibly "unrolled" it back to
        // full height the instant you moved it.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        wm.shade(id);
        let shaded_height = wm.client(id).unwrap().layout.shaded_frame_height;

        wm.dispatch(titlebar_press(frame, Point::new(30, 2), 0, Modifiers::empty()));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(130, 130), surface_local: None });

        let frame_geom = wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(frame_geom.size.h, shaded_height, "the frame pushed to the backend mid-drag must stay at shaded height");
        assert!(wm.client(id).unwrap().flags.contains(ClientFlags::SHADED), "the client must still be flagged shaded");
    }

    #[test]
    fn titlebar_drag_moves_the_frame() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();

        // Grab a point inside the titlebar drag region (not on a button).
        let grab = Point::new(30, 2);
        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: grab,
            button: MouseButton::Left,
            pressed: true,
            time_ms: 0,
            mods: Modifiers::empty(),
        });
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(130, 130), surface_local: None });

        let last_geom = *wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(last_geom.pos, Point::new(100, 128));
    }

    /// Regression test: a client's post-map `ConfigureRequest` (e.g.
    /// xterm resizing itself once it knows its font metrics) reports
    /// x/y relative to its new parent — the frame — not root. Treating
    /// that as a root-relative reposition previously shot the frame
    /// (and its titlebar) off-screen while the client itself stayed
    /// roughly in place, a bug only caught by an actual screenshot.
    #[test]
    fn post_map_configure_request_does_not_move_the_frame() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let frame_pos_before = wm.backend().last_frame_geometry.get(&frame).unwrap().pos;

        // xterm-style resize: x/y = (0, 0) (frame-relative, meaningless
        // as root coordinates), only the size is a real request.
        wm.dispatch(BackendEvent::ConfigureRequest {
            window,
            requested: Rect { pos: Point::new(0, 0), size: Size::new(120, 110) },
        });

        let frame_pos_after = wm.backend().last_frame_geometry.get(&frame).unwrap().pos;
        assert_eq!(frame_pos_after, frame_pos_before, "frame must not move from a client's own configure request");
        assert_eq!(wm.client(id).unwrap().geometry.size, Size::new(120, 110));
    }

    /// Regression test: a size-locked client's own `ConfigureRequest`
    /// must be ignored outright — not raced against, not applied and
    /// then corrected on the next tick. Exists for exactly one real
    /// case found in practice: a terminal emulator whose own startup
    /// self-corrects to a stale computed size shortly after the WM sets
    /// a real one, undoing it every time — see `ClientFlags::
    /// SIZE_LOCKED`'s doc comment.
    #[test]
    fn size_locked_client_ignores_its_own_configure_request() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();

        wm.resize_client_content(id, Size::new(500, 400));
        wm.set_size_locked(id, true);

        wm.dispatch(BackendEvent::ConfigureRequest {
            window,
            requested: Rect { pos: Point::new(0, 0), size: Size::new(100, 100) },
        });

        assert_eq!(wm.client(id).unwrap().geometry.size, Size::new(500, 400), "a locked client's own resize attempt must not apply");

        wm.set_size_locked(id, false);
        wm.dispatch(BackendEvent::ConfigureRequest {
            window,
            requested: Rect { pos: Point::new(0, 0), size: Size::new(100, 100) },
        });
        assert_eq!(wm.client(id).unwrap().geometry.size, Size::new(100, 100), "unlocking must restore normal configure-request handling");
    }

    fn titlebar_press(frame: FakeFrameId, local: Point, time_ms: u32, mods: Modifiers) -> BackendEvent<FakeWindowId, FakeFrameId> {
        BackendEvent::PointerButton { surface: SurfaceRef::Frame(frame), local, button: MouseButton::Left, pressed: true, time_ms, mods }
    }

    fn frame_press(frame: FakeFrameId, local: Point) -> BackendEvent<FakeWindowId, FakeFrameId> {
        BackendEvent::PointerButton { surface: SurfaceRef::Frame(frame), local, button: MouseButton::Left, pressed: true, time_ms: 0, mods: Modifiers::empty() }
    }

    fn frame_release(frame: FakeFrameId, local: Point) -> BackendEvent<FakeWindowId, FakeFrameId> {
        BackendEvent::PointerButton { surface: SurfaceRef::Frame(frame), local, button: MouseButton::Left, pressed: false, time_ms: 0, mods: Modifiers::empty() }
    }

    /// Sets up a fresh client (content 100x100 at root (50,70), matching
    /// `FakeTheme`'s frame at root (50,50) size 100x120) and returns its
    /// id/frame — every resize test starts from exactly this geometry.
    fn client_for_resize(backend: FakeBackend) -> (WindowManager<FakeBackend>, ClientId, FakeFrameId) {
        let mut backend = backend;
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        (wm, id, frame)
    }

    #[test]
    fn resize_from_southeast_grows_width_and_height_without_moving_the_frame() {
        let (mut wm, id, frame) = client_for_resize(FakeBackend::new());

        wm.dispatch(frame_press(frame, Point::new(95, 115))); // inside the SE handle
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(200, 220), surface_local: None });

        let client = wm.client(id).unwrap();
        assert_eq!(client.geometry.size, Size::new(150, 150));
        assert_eq!(client.geometry.pos, Point::new(50, 70), "growing from the SE corner must not move the frame");
    }

    #[test]
    fn resize_from_southwest_grows_width_leftward_and_moves_the_frame() {
        let (mut wm, id, frame) = client_for_resize(FakeBackend::new());

        wm.dispatch(frame_press(frame, Point::new(5, 115))); // inside the SW handle
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(0, 220), surface_local: None });

        let client = wm.client(id).unwrap();
        assert_eq!(client.geometry.size, Size::new(150, 150));
        assert_eq!(client.geometry.pos, Point::new(0, 70), "the SW corner's own edge must move as the frame grows leftward");
    }

    #[test]
    fn resize_from_south_only_changes_height() {
        let (mut wm, id, frame) = client_for_resize(FakeBackend::new());

        wm.dispatch(frame_press(frame, Point::new(50, 115))); // inside the South handle
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(999, 250), surface_local: None }); // x is irrelevant for South

        let client = wm.client(id).unwrap();
        assert_eq!(client.geometry.size, Size::new(100, 180), "width must stay exactly as it was");
        assert_eq!(client.geometry.pos, Point::new(50, 70));
    }

    #[test]
    fn resize_never_shrinks_past_the_min_size_hint() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_size_hints(window, SizeHints { min_size: Some(Size::new(200, 200)), max_size: None, resize_increment: None });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();

        wm.dispatch(frame_press(frame, Point::new(95, 115)));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(60, 60), surface_local: None }); // a shrink attempt

        assert_eq!(wm.client(id).unwrap().geometry.size, Size::new(200, 200), "must clamp to min_size, not shrink below it");
    }

    #[test]
    fn a_shaded_window_cannot_be_resized() {
        let (mut wm, id, frame) = client_for_resize(FakeBackend::new());
        wm.shade(id);
        let original_size = wm.client(id).unwrap().geometry.size;

        wm.dispatch(frame_press(frame, Point::new(95, 115)));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(200, 220), surface_local: None });

        assert_eq!(wm.client(id).unwrap().geometry.size, original_size, "a shaded window has no content to resize");
    }

    #[test]
    fn releasing_ends_the_resize_so_further_motion_has_no_effect() {
        let (mut wm, id, frame) = client_for_resize(FakeBackend::new());
        wm.dispatch(frame_press(frame, Point::new(95, 115)));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(200, 220), surface_local: None });
        let sized_at_release = wm.client(id).unwrap().geometry.size;

        wm.dispatch(frame_release(frame, Point::new(200, 220)));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(400, 400), surface_local: None });

        assert_eq!(wm.client(id).unwrap().geometry.size, sized_at_release, "motion after release must not keep resizing");
    }

    #[test]
    fn maximize_fills_the_usable_area_and_saves_restore_geometry() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        wm.maximize(id, MaximizeDirections::FULL);

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        assert_eq!(client.restore_geometry, Some(original_geometry));
        let frame_geom = *wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(frame_geom, Rect { pos: Point::new(0, 0), size: Size::new(800, 600) }, "frame should fill the monitor edge-to-edge");
    }

    #[test]
    fn unmaximize_restores_the_original_geometry() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        wm.maximize(id, MaximizeDirections::FULL);
        wm.unmaximize(id);

        let client = wm.client(id).unwrap();
        assert_eq!(client.geometry, original_geometry);
        assert!(!client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        assert_eq!(client.restore_geometry, None);
    }

    #[test]
    fn unmaximize_after_vertical_only_maximize_restores_the_whole_original_rect() {
        // `restore_geometry` snapshots the whole pre-maximize `Rect` once,
        // not per-axis — this locks in that a direction-only maximize
        // still restores the untouched axis (x/width here) correctly,
        // not just the axis that was actually maximized.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        wm.maximize(id, MaximizeDirections::VERTICAL);
        wm.unmaximize(id);

        let client = wm.client(id).unwrap();
        assert_eq!(client.geometry, original_geometry, "x/width must be restored even though only height was ever maximized");
        assert!(!client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
    }

    #[test]
    fn toggling_between_maximize_directions_does_not_clobber_the_original_restore_geometry() {
        // `toggle_maximize` always fully unmaximizes before re-maximizing
        // along a different direction set, so the second maximize's
        // `restore_geometry` snapshot must come from the *true* original
        // geometry, not the intermediate full-maximized one.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        wm.toggle_maximize(id, MaximizeDirections::FULL);
        wm.toggle_maximize(id, MaximizeDirections::VERTICAL);

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_V));
        assert!(!client.flags.contains(ClientFlags::MAXIMIZED_H));
        assert_eq!(
            client.restore_geometry,
            Some(original_geometry),
            "restore_geometry must still point at the pre-maximize original, not the intermediate full-maximized state"
        );

        wm.unmaximize(id);
        assert_eq!(wm.client(id).unwrap().geometry, original_geometry, "unmaximizing after the direction switch must land on the true original");
    }

    #[test]
    fn toggle_maximize_full_round_trips_geometry() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        wm.toggle_maximize_full(id);

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        assert_eq!(
            wm.backend().last_frame_geometry.get(&frame),
            Some(&Rect { pos: Point::new(0, 0), size: Size::new(800, 600) }),
            "the first toggle must fill the monitor edge-to-edge"
        );

        wm.toggle_maximize_full(id);

        let client = wm.client(id).unwrap();
        assert_eq!(client.geometry, original_geometry, "the second toggle must restore the pre-maximize geometry exactly");
        assert!(!client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        assert_eq!(client.restore_geometry, None, "the restore snapshot must be consumed, not left to go stale");
    }

    #[test]
    fn toggle_maximize_full_over_a_partial_maximize_goes_full_not_restored() {
        // `toggle_maximize`'s clean-slate semantics, reached through the
        // keybinding entry point: vertical-only is not "full", so the
        // toggle must complete the maximize (from the true original
        // geometry), and only the next toggle restores.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;
        wm.maximize(id, MaximizeDirections::VERTICAL);

        wm.toggle_maximize_full(id);

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V), "a partial maximize must be completed, not toggled off");
        assert_eq!(client.restore_geometry, Some(original_geometry), "the restore snapshot must still be the true original");

        wm.toggle_maximize_full(id);
        assert_eq!(wm.client(id).unwrap().geometry, original_geometry);
    }

    #[test]
    fn titlebar_double_click_toggles_shade() {
        // Matches the classic titlebar double-click default exactly:
        // a plain double-click (no modifiers) shades, not maximizes —
        // full maximize needs both Ctrl and Shift held together (see
        // `ctrl_shift_double_click_toggles_full_maximize`).
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let drag_point = Point::new(30, 2);

        wm.dispatch(titlebar_press(frame, drag_point, 0, Modifiers::empty()));
        wm.dispatch(titlebar_press(frame, drag_point, 150, Modifiers::empty()));

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::SHADED));
        assert!(!client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V), "plain double-click must not maximize");

        // A fresh double-click (two more presses within the window of
        // each other) toggles back off, same as maximize's toggle.
        wm.dispatch(titlebar_press(frame, drag_point, 600, Modifiers::empty()));
        wm.dispatch(titlebar_press(frame, drag_point, 650, Modifiers::empty()));
        assert!(!wm.client(id).unwrap().flags.contains(ClientFlags::SHADED));
    }

    #[test]
    fn ctrl_shift_double_click_toggles_full_maximize() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let drag_point = Point::new(30, 2);
        let both = Modifiers::CONTROL | Modifiers::SHIFT;

        wm.dispatch(titlebar_press(frame, drag_point, 0, Modifiers::empty()));
        wm.dispatch(titlebar_press(frame, drag_point, 150, both));

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        assert!(!client.flags.contains(ClientFlags::SHADED));
    }

    #[test]
    fn shade_hides_the_content_and_shrinks_the_frame_to_the_titlebar() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;
        let shaded_height = wm.client(id).unwrap().layout.shaded_frame_height;

        wm.shade(id);

        assert!(wm.client(id).unwrap().flags.contains(ClientFlags::SHADED));
        assert_eq!(wm.client(id).unwrap().geometry, original_geometry, "content geometry must be untouched by shading");
        let frame_geom = wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(frame_geom.size.h, shaded_height, "frame should shrink to just the titlebar");
        assert_eq!(wm.backend().client_mapped.get(&window), Some(&false), "content window must be unmapped while shaded");
    }

    #[test]
    fn shade_paints_a_titlebar_only_decoration_buffer() {
        // Regression test: shading shrank the frame *rect* but still
        // painted the full-height decoration into it. On X11 the frame
        // window clips the oversized buffer, so the roll-up looked
        // right; on Wayland the buffer is the frame's outline (the
        // renderer draws it at the buffer's own size), so the window
        // went blank at full size instead of rolling up. Asserting the
        // painted buffer against the frame rect catches it on either
        // backend.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let shaded_height = wm.client(id).unwrap().layout.shaded_frame_height;

        wm.shade(id);

        let painted = *wm.backend().last_paint_size.get(&frame).unwrap();
        let frame_geom = *wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(painted.h, shaded_height, "the decoration buffer must be rasterized at shaded height");
        assert_eq!(painted, frame_geom.size, "the painted buffer must cover the frame rect exactly, no more");
    }

    #[test]
    fn a_shaded_frame_stays_rolled_up_across_repaints() {
        // The double-click that shades is followed immediately by the
        // matching button *release*, which repaints the frame to clear
        // any pressed button — and `repaint_decoration` renders from
        // `client.layout`, which stays at its full unshaded shape by
        // design. Before the fix that repaint re-inflated the buffer a
        // few microseconds after the shade painted it correctly, so the
        // user never saw a rolled-up frame at all.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let shaded_height = wm.client(id).unwrap().layout.shaded_frame_height;

        // A real double-click on the titlebar drag region, then the
        // release that ends it.
        let drag_point = Point::new(30, 2);
        wm.dispatch(titlebar_press(frame, drag_point, 0, Modifiers::empty()));
        wm.dispatch(titlebar_press(frame, drag_point, 100, Modifiers::empty()));
        wm.dispatch(frame_release(frame, drag_point));

        assert!(wm.client(id).unwrap().flags.contains(ClientFlags::SHADED));
        let painted = *wm.backend().last_paint_size.get(&frame).unwrap();
        assert_eq!(painted.h, shaded_height, "the repaint after the double-click must stay at shaded height");

        // Any later repaint (focus change, title update) must hold the
        // shade too — same trap, different trigger.
        wm.backend_mut().set_title(window, "chrisk@imac:~/chonkstep");
        wm.dispatch(BackendEvent::TitleChanged(window));
        let painted = *wm.backend().last_paint_size.get(&frame).unwrap();
        assert_eq!(painted.h, shaded_height, "a title-change repaint must not re-inflate a shaded frame");
    }

    #[test]
    fn every_frame_state_change_paints_a_buffer_that_matches_the_frame_rect() {
        // The invariant the shade bug broke, swept over the state
        // changes that move a frame's edges. It is stated as "buffer ==
        // rect" rather than "buffer is at least the rect" because a
        // backend that composites the buffer directly (Wayland) shows
        // every pixel of it: too small leaves the frame stunted, too
        // large paints chrome over screen the frame doesn't own.
        // Fullscreen is deliberately out of scope — that path paints no
        // decoration at all (`reflow_frame`'s fullscreen branch), the
        // client's own content covering the frame edge to edge.
        type Op = (&'static str, fn(&mut WindowManager<FakeBackend>, ClientId));
        let ops: &[Op] = &[
            ("shade", |wm, id| wm.shade(id)),
            ("unshade", |wm, id| {
                wm.shade(id);
                wm.unshade(id)
            }),
            ("maximize while shaded", |wm, id| {
                wm.shade(id);
                wm.maximize(id, MaximizeDirections::FULL)
            }),
            ("maximize", |wm, id| wm.maximize(id, MaximizeDirections::FULL)),
            ("unmaximize", |wm, id| {
                wm.maximize(id, MaximizeDirections::FULL);
                wm.unmaximize(id)
            }),
            ("miniaturize and back", |wm, id| {
                wm.miniaturize(id);
                wm.deminiaturize(id)
            }),
        ];

        for (name, op) in ops {
            let mut backend = FakeBackend::new();
            let window = backend.create_window();
            backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
            let mut wm = wm(backend);
            wm.dispatch(BackendEvent::MapRequest(window));
            let id = wm.client_for_window(window).unwrap();
            let frame = wm.client(id).unwrap().frame.unwrap();
            assert_eq!(
                wm.backend().last_paint_size.get(&frame).copied(),
                wm.backend().last_frame_geometry.get(&frame).map(|geometry| geometry.size),
                "map: decoration buffer and frame rect disagree"
            );

            op(&mut wm, id);

            assert_eq!(
                wm.backend().last_paint_size.get(&frame).copied(),
                wm.backend().last_frame_geometry.get(&frame).map(|geometry| geometry.size),
                "{name}: decoration buffer and frame rect disagree"
            );
        }
    }

    #[test]
    fn unshade_restores_full_frame_and_remaps_content() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();

        wm.shade(id);
        wm.unshade(id);

        assert!(!wm.client(id).unwrap().flags.contains(ClientFlags::SHADED));
        assert_eq!(wm.backend().client_mapped.get(&window), Some(&true));
        let frame_geom = *wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(frame_geom.size.h, wm.client(id).unwrap().layout.frame_size.h);
        // The buffer has to grow back with the rect: a Wayland frame is
        // only as big as the decoration painted into it, so a shaded
        // buffer left behind here would leave the window a titlebar-
        // sized strip with its content hanging out below it.
        let painted = *wm.backend().last_paint_size.get(&frame).unwrap();
        assert_eq!(painted, frame_geom.size, "unshading must repaint the decoration at full frame size");
    }

    #[test]
    fn shading_does_not_generate_a_spurious_unmapped_event() {
        // Regression guard for the whole reason `Backend::set_client_
        // mapped` exists rather than reusing `unmap_frame`: shading must
        // never look like the client withdrew.
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();

        wm.shade(id);

        assert!(wm.client_for_window(window).is_some(), "client must still be tracked while shaded");
        assert_eq!(wm.client(id).unwrap().lifecycle, Lifecycle::Normal);
    }

    #[test]
    fn ctrl_double_click_maximizes_vertical_only() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let drag_point = Point::new(30, 2);

        wm.dispatch(titlebar_press(frame, drag_point, 0, Modifiers::empty()));
        wm.dispatch(titlebar_press(frame, drag_point, 150, Modifiers::CONTROL));

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_V));
        assert!(!client.flags.contains(ClientFlags::MAXIMIZED_H), "Ctrl double-click should only maximize vertically");
    }

    #[test]
    fn a_second_press_after_the_double_click_window_starts_a_move_instead() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let drag_point = Point::new(30, 2);

        wm.dispatch(titlebar_press(frame, drag_point, 0, Modifiers::empty()));
        wm.dispatch(titlebar_press(frame, drag_point, 900, Modifiers::empty()));

        assert!(!wm.client(id).unwrap().flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(130, 130), surface_local: None });
        let last_geom = wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_ne!(last_geom.pos, Point::new(50, 50), "the second (non-double-click) press should still have armed a drag");
    }

    #[test]
    fn dragging_near_the_screen_edge_snaps_the_frame_flush() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(4, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();

        let grab = Point::new(30, 2);
        wm.dispatch(BackendEvent::PointerButton {
            surface: SurfaceRef::Frame(frame),
            local: grab,
            button: MouseButton::Left,
            pressed: true,
            time_ms: 0,
            mods: Modifiers::empty(),
        });
        // Root-relative motion that would land the frame's left edge at
        // x=4 — within the snap threshold of the screen's x=0 edge.
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(34, 52), surface_local: None });

        let last_geom = wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(last_geom.pos.x, 0, "frame should have snapped flush with the screen's left edge");
    }

    #[test]
    fn activate_request_restores_and_focuses_a_miniaturized_client() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        wm.miniaturize(id);
        assert_eq!(wm.client(id).unwrap().lifecycle, Lifecycle::Miniaturized);
        assert_eq!(
            wm.backend().published_net_states.last(),
            Some(&(window, false, false, false, false, true)),
            "miniaturizing must publish the client as hidden"
        );

        wm.dispatch(BackendEvent::ActivateRequested(window));

        let client = wm.client(id).unwrap();
        assert_eq!(client.lifecycle, Lifecycle::Normal, "activation must restore a miniaturized client");
        assert!(client.flags.contains(ClientFlags::FOCUSED));
        assert_eq!(wm.focused_client(), Some(id));
        assert_eq!(
            wm.backend().published_active_windows.last(),
            Some(&Some(window)),
            "the focus change must be published as the active window"
        );
        assert_eq!(
            wm.backend().published_net_states.last(),
            Some(&(window, false, false, false, false, false)),
            "the restored client must be re-published as not hidden"
        );
    }

    #[test]
    fn close_request_uses_the_same_backend_close_path_as_the_close_button() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));

        wm.dispatch(BackendEvent::CloseRequested(window));

        // Same mechanism `close_button_click_sends_close_to_the_client`
        // asserts on: `Backend::send_close`, which does the
        // WM_DELETE_WINDOW-or-kill dance for both entry points.
        assert!(wm.backend().close_requests.contains(&window));
    }

    #[test]
    fn close_client_uses_the_same_backend_close_path_as_the_close_button() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();

        wm.close_client(id);

        // The third entry point into the one shared close mechanism —
        // `Backend::send_close`, exactly what the titlebar button's
        // commit and `_NET_CLOSE_WINDOW` record here too.
        assert!(wm.backend().close_requests.contains(&window));
    }

    /// A config keybinding fires against whatever `focused_client`
    /// returned a moment ago — a window can die between the two, so
    /// every keybinding-facing method must shrug off a stale id rather
    /// than panic or touch the backend.
    #[test]
    fn keybinding_action_methods_ignore_a_stale_client_id() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        wm.dispatch(BackendEvent::Destroyed(window));

        wm.close_client(id);
        wm.toggle_maximize_full(id);
        wm.toggle_fullscreen(id);

        assert!(wm.backend().close_requests.is_empty(), "a stale id must not close anything");
        assert_eq!(wm.client_count(), 0);
    }

    #[test]
    fn fullscreen_toggle_covers_the_monitor_and_a_second_toggle_restores_exactly() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let monitor = Rect { pos: Point::new(0, 0), size: Size::new(800, 600) };
        backend.set_monitor(monitor);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        let toggle = BackendEvent::NetStateRequested {
            window,
            action: NetStateAction::Toggle,
            first: NetState::Fullscreen,
            second: None,
        };
        wm.dispatch(toggle.clone());

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::FULLSCREEN));
        assert_eq!(client.geometry, monitor, "content must fill the monitor edge-to-edge");
        assert_eq!(client.layout.client_offset, Point::new(0, 0), "no chrome offset — the client sits at the frame's own origin");
        assert_eq!(wm.backend().last_frame_geometry.get(&frame), Some(&monitor), "the frame must be exactly the monitor rect");
        assert!(wm.backend().raised_frames.contains(&frame), "entering fullscreen must raise");
        assert_eq!(wm.backend().published_net_states.last(), Some(&(window, true, false, false, false, false)));

        wm.dispatch(toggle);

        let client = wm.client(id).unwrap();
        assert!(!client.flags.contains(ClientFlags::FULLSCREEN));
        assert_eq!(client.geometry, original_geometry, "a second toggle must restore the prior geometry exactly");
        assert_eq!(wm.backend().published_net_states.last(), Some(&(window, false, false, false, false, false)));
    }

    /// Mirror of `fullscreen_toggle_covers_the_monitor_and_a_second_
    /// toggle_restores_exactly`, through the keybinding entry point
    /// instead of a `_NET_WM_STATE` message — the two must be
    /// indistinguishable, since `toggle_fullscreen` routes through the
    /// same `apply_fullscreen_action` a pager's Toggle takes.
    #[test]
    fn toggle_fullscreen_round_trips_exactly() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        let monitor = Rect { pos: Point::new(0, 0), size: Size::new(800, 600) };
        backend.set_monitor(monitor);
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        wm.toggle_fullscreen(id);

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::FULLSCREEN));
        assert_eq!(client.geometry, monitor, "content must fill the monitor edge-to-edge");
        assert_eq!(wm.backend().last_frame_geometry.get(&frame), Some(&monitor), "the frame must be exactly the monitor rect");
        assert!(wm.backend().raised_frames.contains(&frame), "entering fullscreen must raise");
        assert_eq!(wm.backend().published_net_states.last(), Some(&(window, true, false, false, false, false)));

        wm.toggle_fullscreen(id);

        let client = wm.client(id).unwrap();
        assert!(!client.flags.contains(ClientFlags::FULLSCREEN));
        assert_eq!(client.geometry, original_geometry, "the second toggle must restore the prior geometry exactly");
        assert_eq!(wm.backend().published_net_states.last(), Some(&(window, false, false, false, false, false)));
    }

    /// The reason `fullscreen_restore` is its own slot and not
    /// `Client::restore_geometry`: leaving fullscreen must land back on
    /// the maximized rect (the geometry fullscreen replaced), while
    /// maximize's own pre-maximize snapshot stays intact for a later
    /// `unmaximize`.
    #[test]
    fn fullscreen_over_a_maximized_window_restores_the_maximized_rect_then_the_original() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;
        wm.maximize(id, MaximizeDirections::FULL);
        let maximized_geometry = wm.client(id).unwrap().geometry;

        wm.fullscreen(id);
        wm.unfullscreen(id);

        let client = wm.client(id).unwrap();
        assert_eq!(client.geometry, maximized_geometry, "leaving fullscreen must restore the maximized rect it replaced");
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V), "the maximized state must survive the fullscreen round trip");

        wm.unmaximize(id);
        assert_eq!(wm.client(id).unwrap().geometry, original_geometry, "maximize's own restore snapshot must still be intact");
    }

    #[test]
    fn net_state_maximize_add_of_both_axes_maps_to_full_maximize_and_remove_restores() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(50, 50), size: Size::new(100, 100) });
        backend.set_monitor(Rect { pos: Point::new(0, 0), size: Size::new(800, 600) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let original_geometry = wm.client(id).unwrap().geometry;

        // The conventional pager "maximize": both axes in one message.
        wm.dispatch(BackendEvent::NetStateRequested {
            window,
            action: NetStateAction::Add,
            first: NetState::MaximizedHorz,
            second: Some(NetState::MaximizedVert),
        });

        let client = wm.client(id).unwrap();
        assert!(client.flags.contains(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        assert_eq!(wm.backend().published_net_states.last(), Some(&(window, false, true, true, false, false)));

        wm.dispatch(BackendEvent::NetStateRequested {
            window,
            action: NetStateAction::Remove,
            first: NetState::MaximizedHorz,
            second: Some(NetState::MaximizedVert),
        });

        let client = wm.client(id).unwrap();
        assert!(!client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V));
        assert_eq!(client.geometry, original_geometry, "removing both axes must restore the pre-maximize geometry");
    }

    #[test]
    fn an_unmanaged_window_type_is_mapped_as_is_and_never_tracked() {
        let mut backend = FakeBackend::new();
        let window = backend.create_window();
        backend.set_window_type(window, WindowType::Unmanaged);
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(window));

        assert!(wm.backend().unmanaged_mapped.contains(&window), "must be mapped via `map_unmanaged`, as the client created it");
        assert_eq!(wm.client_count(), 0, "no client entry may exist");
        assert!(wm.client_for_window(window).is_none());
        assert!(wm.backend().mapped_frames.is_empty(), "no decoration frame may have been created or mapped");
    }

    #[test]
    fn client_list_publishes_on_map_and_shrinks_on_destroy() {
        let mut backend = FakeBackend::new();
        let w1 = backend.create_window();
        let w2 = backend.create_window();
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(w1));
        assert_eq!(wm.backend().published_client_lists.last(), Some(&vec![w1]));

        wm.dispatch(BackendEvent::MapRequest(w2));
        assert_eq!(wm.backend().published_client_lists.last(), Some(&vec![w1, w2]), "oldest first — insertion order, not focus order");

        wm.dispatch(BackendEvent::Destroyed(w1));
        assert_eq!(wm.backend().published_client_lists.last(), Some(&vec![w2]));
    }

    #[test]
    fn grab_key_forwards_the_combo_to_the_backend() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);
        let combo = KeyCombo { keysym: 0x0071, modifiers: Modifiers::ALT | Modifiers::SHIFT }; // XK_q

        wm.grab_key(combo);

        assert_eq!(wm.backend().grabbed_keys, vec![combo], "the combo must reach Backend::grab_key unaltered");
    }

    #[test]
    fn bind_default_keys_grabs_only_the_modal_cycling_combos() {
        // The keybinding split (see `bind_default_keys`): everything
        // beyond Alt+Tab / Alt+Shift+Tab is config-driven from the
        // binary via `grab_key`, so nothing else may be claimed here —
        // a leftover hardcoded grab would shadow the user's own
        // binding for that combo.
        let backend = FakeBackend::new();
        let mut wm = wm(backend);

        wm.bind_default_keys();

        assert_eq!(
            wm.backend().grabbed_keys,
            vec![
                KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT },
                KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT | Modifiers::SHIFT },
            ]
        );
    }

    #[test]
    fn workspace_and_workarea_changes_are_published_for_pagers() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);
        assert_eq!(wm.backend().published_workspaces.first(), Some(&(1, 0)), "the initial workspace shape must be published at startup");

        wm.switch_workspace(2);
        assert_eq!(wm.backend().published_workspaces.last(), Some(&(3, 2)), "growth to index 2 means 3 workspaces, current 2");

        let area = Rect { pos: Point::new(0, 0), size: Size::new(800, 576) };
        wm.set_workarea(area);
        assert_eq!(wm.backend().published_workareas.last(), Some(&(area, 3)), "the workarea must be published with the current workspace count");
    }

    /// Two 800x600 heads side by side, the left one primary — the
    /// arrangement every multi-monitor test below measures against.
    fn dual_monitors() -> Vec<MonitorInfo> {
        vec![
            MonitorInfo { geometry: LEFT_HEAD, name: "left".to_string(), primary: true },
            MonitorInfo { geometry: RIGHT_HEAD, name: "right".to_string(), primary: false },
        ]
    }

    const LEFT_HEAD: Rect = Rect { pos: Point { x: 0, y: 0 }, size: Size { w: 800, h: 600 } };
    const RIGHT_HEAD: Rect = Rect { pos: Point { x: 800, y: 0 }, size: Size { w: 800, h: 600 } };
    /// The left head with a 40px dock strip carved off its bottom.
    const LEFT_WORKAREA: Rect = Rect { pos: Point { x: 0, y: 0 }, size: Size { w: 800, h: 560 } };

    #[test]
    fn maximize_fills_the_workarea_of_the_monitor_holding_the_window() {
        let mut backend = FakeBackend::new();
        backend.set_monitors(dual_monitors());
        let window = backend.create_window();
        // Squarely on the right-hand head, by the client's own
        // requested position.
        backend.set_geometry(window, Rect { pos: Point::new(1000, 100), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        // A dock strip reserved on the primary only: the right head has
        // no entry, so it keeps its full geometry (`set_workareas`'
        // short-vector rule).
        wm.set_workareas(vec![LEFT_WORKAREA]);

        wm.maximize(id, MaximizeDirections::FULL);

        assert_eq!(
            wm.backend().last_frame_geometry.get(&frame),
            Some(&RIGHT_HEAD),
            "a window on the second head must maximize into that head, not into the primary's reserved workarea"
        );
    }

    #[test]
    fn a_window_dragged_onto_the_second_monitor_maximizes_there() {
        let mut backend = FakeBackend::new();
        backend.set_monitors(dual_monitors());
        let window = backend.create_window();
        backend.set_geometry(window, Rect { pos: Point::new(100, 100), size: Size::new(100, 100) });
        let mut wm = wm(backend);
        wm.dispatch(BackendEvent::MapRequest(window));
        let id = wm.client_for_window(window).unwrap();
        let frame = wm.client(id).unwrap().frame.unwrap();
        let right_workarea = Rect { pos: Point::new(800, 0), size: Size::new(800, 560) };
        wm.set_workareas(vec![LEFT_WORKAREA, right_workarea]);

        // Where it started: the primary, and its reserved workarea.
        wm.maximize(id, MaximizeDirections::FULL);
        assert_eq!(wm.backend().last_frame_geometry.get(&frame), Some(&LEFT_WORKAREA));
        wm.unmaximize(id);

        // Titlebar-drag it across the seam. Far enough from every
        // monitor edge that snapping has nothing to say about the
        // landing position.
        wm.dispatch(titlebar_press(frame, Point::new(30, 2), 0, Modifiers::empty()));
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(1230, 202), surface_local: None });
        wm.dispatch(frame_release(frame, Point::new(1230, 202)));

        wm.maximize(id, MaximizeDirections::FULL);

        assert_eq!(
            wm.backend().last_frame_geometry.get(&frame),
            Some(&right_workarea),
            "maximize must follow the window across the seam, into the second head's own workarea"
        );
    }

    #[test]
    fn initial_placement_lands_on_the_monitor_under_the_pointer() {
        let mut backend = FakeBackend::new();
        backend.set_monitors(dual_monitors());
        let first = backend.create_window();
        let second = backend.create_window();
        let mut wm = wm(backend);

        // The user is working on the right-hand head when the first
        // window opens...
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(1200, 300), surface_local: None });
        wm.dispatch(BackendEvent::MapRequest(first));
        let first_frame = frame_rect(&wm, first);
        assert!(
            RIGHT_HEAD.contains(first_frame.pos),
            "a window opened while the pointer is on the second head must place there, got {first_frame:?}"
        );

        // ...and back on the primary when the second one does.
        wm.dispatch(BackendEvent::PointerMotion { root: Point::new(200, 300), surface_local: None });
        wm.dispatch(BackendEvent::MapRequest(second));
        let second_frame = frame_rect(&wm, second);
        assert!(
            LEFT_HEAD.contains(second_frame.pos),
            "the pointer moving back to the primary must move where new windows open, got {second_frame:?}"
        );
    }

    #[test]
    fn with_no_pointer_seen_placement_follows_the_focused_windows_monitor() {
        // A session's very first windows can map before the mouse has
        // moved at all (autostarted apps), so placement must still
        // resolve to the head the user is demonstrably on.
        let mut backend = FakeBackend::new();
        backend.set_monitors(dual_monitors());
        let anchor = backend.create_window();
        let fresh = backend.create_window();
        backend.set_geometry(anchor, Rect { pos: Point::new(1000, 100), size: Size::new(100, 100) });
        let mut wm = wm(backend);

        wm.dispatch(BackendEvent::MapRequest(anchor));
        wm.dispatch(BackendEvent::MapRequest(fresh));

        let placed = frame_rect(&wm, fresh);
        assert!(
            RIGHT_HEAD.contains(placed.pos),
            "with no pointer ever seen, a fresh window must join the focused window's head, got {placed:?}"
        );
    }

    /// The frame geometry the backend was last told for `window`'s
    /// client — where a placed window actually landed.
    fn frame_rect(wm: &WindowManager<FakeBackend>, window: FakeWindowId) -> Rect {
        let id = wm.client_for_window(window).expect("window should be managed");
        let frame = wm.client(id).unwrap().frame.unwrap();
        *wm.backend().last_frame_geometry.get(&frame).expect("a placed frame must have been configured")
    }

    #[test]
    fn a_point_outside_every_monitor_resolves_to_the_nearest_one() {
        // A vertical stack with a 100px gap between the outputs — the
        // dead band no monitor covers, which a pointer really can sit
        // in on a mismatched arrangement.
        let top = Rect { pos: Point::new(0, 0), size: Size::new(800, 600) };
        let bottom = Rect { pos: Point::new(0, 700), size: Size::new(800, 600) };
        let mut backend = FakeBackend::new();
        backend.set_monitors(vec![
            MonitorInfo { geometry: top, name: "top".to_string(), primary: true },
            MonitorInfo { geometry: bottom, name: "bottom".to_string(), primary: false },
        ]);
        let mut wm = wm(backend);
        let top_area = Rect { pos: Point::new(0, 0), size: Size::new(800, 560) };
        let bottom_area = Rect { pos: Point::new(0, 700), size: Size::new(800, 560) };
        wm.set_workareas(vec![top_area, bottom_area]);

        assert_eq!(wm.usable_area_at(Point::new(400, 640)), top_area, "in the gap, nearer the top output");
        assert_eq!(wm.usable_area_at(Point::new(400, 680)), bottom_area, "in the gap, nearer the bottom one");
        assert_eq!(wm.monitor_rect_at(Point::new(400, 680)), bottom, "the raw rect resolves the same way");
        assert_eq!(
            wm.usable_area_at(Point::new(-5000, -5000)),
            top_area,
            "a point nowhere near any output still has to resolve to one"
        );
    }

    #[test]
    fn set_workarea_reserves_space_on_the_primary_monitor_only() {
        // The primary is deliberately the *second* entry: an index-0
        // assumption would reserve the dock strip on the wrong head.
        let mut backend = FakeBackend::new();
        backend.set_monitors(vec![
            MonitorInfo { geometry: LEFT_HEAD, name: "aux".to_string(), primary: false },
            MonitorInfo { geometry: RIGHT_HEAD, name: "main".to_string(), primary: true },
        ]);
        let mut wm = wm(backend);
        let reserved = Rect { pos: Point::new(800, 0), size: Size::new(800, 560) };

        wm.set_workarea(reserved);

        assert_eq!(wm.usable_area_at(Point::new(1200, 300)), reserved, "the primary keeps the strip it was given");
        assert_eq!(wm.usable_area_at(Point::new(400, 300)), LEFT_HEAD, "every other head keeps its full geometry");
    }

    #[test]
    fn net_workarea_publishes_the_union_of_the_per_monitor_workareas() {
        let mut backend = FakeBackend::new();
        backend.set_monitors(dual_monitors());
        let mut wm = wm(backend);

        wm.set_workareas(vec![Rect { pos: Point::new(0, 40), size: Size::new(800, 560) }]);

        // The property carries no per-monitor dimension at all, so the
        // bounding box of the reserved primary and the untouched second
        // head is the only honest thing to hand a pager.
        assert_eq!(
            wm.backend().published_workareas.last(),
            Some(&(Rect { pos: Point::new(0, 0), size: Size::new(1600, 600) }, 1)),
            "_NET_WORKAREA must span every monitor's workarea"
        );
    }
}
