use std::collections::{HashMap, VecDeque};

use slotmap::SlotMap;
use wm_theme_api::{ButtonKind, ButtonRuntimeState, DecorationBuffer, DecorationRequest, Point, Rect, ResizeEdge, Size, ThemeEngine};

use crate::backend::Backend;
use crate::client::{Client, ClientFlags, ClientId, Lifecycle, MaximizeDirections};
use crate::focus::FocusPolicy;
use crate::hittest::{hit_test, HitTarget};
use crate::resize;
use crate::snap;
use crate::types::{BackendEvent, KeyCombo, MouseButton, Modifiers, SurfaceRef};

/// How close together (in ms) two presses on the same titlebar must land
/// to count as a double-click (toggling maximize). Not backed by an
/// X server "double-click time" setting yet — a reasonable fixed default,
/// in the same ballpark WindowMaker itself defaults to.
const DOUBLE_CLICK_MS: u32 = 400;

/// How close (in pixels) a dragged frame edge must come to a screen edge
/// or another window's edge before it snaps flush — WindowMaker's "edge
/// resistance"/"attraction" (`src/moveres.c`), simplified to a single
/// always-on threshold rather than separate resistance/attract modes.
const SNAP_THRESHOLD_PX: i32 = 10;

/// Keysym for the `Tab` key, per `<X11/keysymdef.h>` — the same numeric
/// space X11 and XKB (and so a future Wayland backend) both use, so
/// this is genuinely backend-agnostic despite the name. `Alt+Tab`/
/// `Alt+Shift+Tab` are this WM's only default global keybinding today
/// (window cycling, `WindowMaker`'s `cycling.c`).
const XK_TAB: u32 = 0xff09;
const XK_LEFT: u32 = 0xff51;
const XK_RIGHT: u32 = 0xff53;

/// An in-progress titlebar-drag move. `grab_offset` is the frame-local
/// point that was clicked — since that's constant relative to the frame
/// regardless of where the frame currently sits, the new frame position
/// is simply `pointer_root - grab_offset` on every motion event.
struct ActiveMove<B: Backend> {
    client: ClientId,
    frame: B::FrameId,
    grab_offset: Point,
}

/// An in-progress edge/corner resize drag (WindowMaker's
/// `wMouseResizeWindow`). `start_frame` is the frame's own geometry at
/// press time — every motion event recomputes the new size fresh from
/// it and the current pointer position rather than accumulating deltas
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
    /// A client was just mapped and decorated for the first time — lets
    /// the shell react to new windows (e.g. giving a freshly spawned
    /// app a sane default size via `resize_client_content` when its own
    /// requested geometry can't be trusted, as some apps' initial size
    /// negotiation is unreliable across window-manager environments).
    Mapped(ClientId),
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
    active_move: Option<ActiveMove<B>>,
    active_resize: Option<ActiveResize>,
    active_button_press: Option<ActiveButtonPress>,
    /// The most recent press on a titlebar drag region, for double-click
    /// detection — a second press on the *same* client's titlebar within
    /// `DOUBLE_CLICK_MS` toggles maximize instead of starting a move.
    last_titlebar_press: Option<(ClientId, u32)>,
    /// Screen area windows should maximize into, reserved-space-aware —
    /// e.g. `chonkstep`'s desktop shell excludes its dock strip by
    /// calling `set_workarea`. Defaults to the primary monitor's full
    /// geometry when unset.
    workarea: Option<Rect>,
    notifications: VecDeque<Notification>,
    focus_policy: FocusPolicy,
    /// 0-based, matching `Client::workspace`. Grows on demand the first
    /// time something switches to or moves a client onto an index past
    /// the current row's end (`WindowMaker`'s `wWorkspaceMake`) — there
    /// is no fixed count and no way to destroy a workspace once
    /// created, matching real WindowMaker exactly.
    current_workspace: usize,
    workspace_count: usize,
}

impl<B: Backend> WindowManager<B> {
    pub fn new(backend: B, theme: Box<dyn ThemeEngine>) -> Self {
        Self {
            backend,
            theme,
            clients: SlotMap::with_key(),
            window_index: HashMap::new(),
            frame_index: HashMap::new(),
            focused: None,
            active_move: None,
            active_resize: None,
            active_button_press: None,
            last_titlebar_press: None,
            workarea: None,
            notifications: VecDeque::new(),
            focus_policy: FocusPolicy::default(),
            current_workspace: 0,
            workspace_count: 1,
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

    /// Reserves screen space windows should not maximize into (e.g. a
    /// dock/panel strip) — a reusable SDK primitive: any desktop shell
    /// built on this crate calls this once at startup and again on
    /// resize, rather than `wm-core` hardcoding a notion of "the dock".
    pub fn set_workarea(&mut self, area: Rect) {
        self.workarea = Some(area);
    }

    /// Registers this WM's default global keybindings with the backend
    /// — currently just Alt+Tab / Alt+Shift+Tab window cycling
    /// (`cycle_focus`). Call once after construction; a real
    /// configurable-keybinding story (`wm-config`) is future work, but
    /// the `Backend::grab_key`/`KeyPress` plumbing this exercises is the
    /// same plumbing that story would ride on.
    pub fn bind_default_keys(&mut self) {
        self.backend.grab_key(KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT });
        self.backend.grab_key(KeyCombo { keysym: XK_TAB, modifiers: Modifiers::ALT | Modifiers::SHIFT });
        let workspace_mods = Modifiers::ALT | Modifiers::CONTROL;
        self.backend.grab_key(KeyCombo { keysym: XK_RIGHT, modifiers: workspace_mods });
        self.backend.grab_key(KeyCombo { keysym: XK_LEFT, modifiers: workspace_mods });
    }

    pub fn current_workspace(&self) -> usize {
        self.current_workspace
    }

    pub fn workspace_count(&self) -> usize {
        self.workspace_count
    }

    /// Switches to `workspace`, growing the workspace row on demand if
    /// it's past the current end (real WindowMaker's `wWorkspaceMake`
    /// — there is no fixed count and no way to destroy a workspace
    /// once created). Every *mapped* client not on the target
    /// workspace gets its frame unmapped; every mapped client that IS
    /// on it gets remapped. Miniaturized/withdrawn clients are left
    /// alone entirely — their icon tile (a desktop-shell concern, not
    /// `wm-core`'s) stays visible regardless of workspace, a
    /// deliberately simpler choice than real WindowMaker's opt-out-able
    /// per-workspace icon hiding. A no-op if already on `workspace`.
    pub fn switch_workspace(&mut self, workspace: usize) {
        if workspace == self.current_workspace {
            return;
        }
        self.workspace_count = self.workspace_count.max(workspace + 1);
        self.current_workspace = workspace;

        let ids: Vec<ClientId> = self.clients.keys().collect();
        for id in ids {
            let Some(client) = self.clients.get(id) else {
                continue;
            };
            if client.lifecycle != Lifecycle::Normal {
                continue;
            }
            let Some(frame) = client.frame else {
                continue;
            };
            if client.workspace == workspace {
                self.backend.map_frame(frame);
                // Same reasoning as `deminiaturize`: a remapped frame
                // isn't guaranteed to still hold its old pixel content
                // (no backing-store requested), so repaint explicitly
                // rather than hope an `Expose` arrives and gets replayed.
                self.repaint_decoration(id);
            } else {
                self.backend.unmap_frame(frame);
            }
        }

        let still_visible = self.focused.and_then(|id| self.clients.get(id)).is_some_and(|c| c.workspace == workspace);
        if !still_visible {
            if let Some(prev) = self.focused.take() {
                if let Some(c) = self.clients.get_mut(prev) {
                    c.flags.remove(ClientFlags::FOCUSED);
                }
            }
        }
        tracing::info!(workspace, "switched workspace");
    }

    /// Moves `id` onto `workspace` (growing the row if needed) and
    /// hides its frame immediately if that isn't the active workspace
    /// — matches real WindowMaker's `MoveToNextWorkspace`/
    /// `MoveToPrevWorkspace` window actions. A no-op if `id` is
    /// already on `workspace`.
    pub fn move_client_to_workspace(&mut self, id: ClientId, workspace: usize) {
        self.workspace_count = self.workspace_count.max(workspace + 1);
        let Some(client) = self.clients.get_mut(id) else {
            return;
        };
        if client.workspace == workspace {
            return;
        }
        client.workspace = workspace;
        if workspace != self.current_workspace {
            if let Some(frame) = client.frame {
                self.backend.unmap_frame(frame);
            }
            if self.focused == Some(id) {
                if let Some(c) = self.clients.get_mut(id) {
                    c.flags.remove(ClientFlags::FOCUSED);
                }
                self.focused = None;
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
            BackendEvent::KeyPress(combo) => self.handle_key_press(combo),
            BackendEvent::PointerEnter { surface } => self.handle_pointer_enter(surface),
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

        let title = self.backend.window_title(window).unwrap_or_default();
        let content = self.backend.window_geometry(window);
        tracing::debug!(?window, ?content, "map request — client's own geometry at map time");
        let mut client = Client::new(window, title);
        client.geometry = content;
        client.workspace = self.current_workspace;

        let request = Self::decoration_request(&client, None);
        let layout = self.theme.layout(&request);
        // Place the FRAME at the client's own requested position rather
        // than deriving it by subtracting the chrome offset from it.
        // Most apps request (0, 0) as a "don't care, WM decides"
        // placeholder rather than a real intent to sit flush with the
        // screen's corner; treating that as the *content* position and
        // subtracting the titlebar/border offset would push the frame
        // (and its titlebar) to negative coordinates, off-screen.
        let frame_geom = Rect { pos: content.pos, size: layout.frame_size };
        client.geometry.pos = Point::new(
            frame_geom.pos.x + layout.client_offset.x,
            frame_geom.pos.y + layout.client_offset.y,
        );

        let frame = self.backend.create_decoration(window, &layout);
        self.backend.set_frame_geometry(frame, frame_geom);
        let buffer = self.theme.render(&request, &layout);
        self.backend.paint_decoration(frame, &buffer);
        self.backend.map_frame(frame);

        client.frame = Some(frame);
        client.layout = layout;

        let id = self.clients.insert(client);
        self.window_index.insert(window, id);
        self.frame_index.insert(frame, id);
        tracing::info!(?window, "mapped and decorated window");

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
        }
        if self.active_move.as_ref().is_some_and(|m| m.client == id) {
            self.active_move = None;
        }
        if let Some(client) = self.clients.remove(id) {
            if let Some(frame) = client.frame {
                self.frame_index.remove(&frame);
                self.backend.destroy_decoration(frame);
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
            HitTarget::TitlebarDrag if button == MouseButton::Left => {
                // A second press on this same client's titlebar within
                // `DOUBLE_CLICK_MS` triggers a titlebar action instead of
                // starting a drag — matching real WindowMaker's own
                // `titlebarDblClick` exactly: a plain double-click shades
                // (rolls the window up to just its titlebar), and
                // maximizing needs a modifier — Ctrl alone for vertical-
                // only, Shift alone for horizontal-only, both together
                // for full. (WindowMaker also has a `double_click_
                // fullscreen` preference that swaps the plain case to
                // full-maximize instead of shade; not implemented here —
                // shade is the flagship default.) No X server gives
                // double-click detection for free; this is why
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
                    self.active_move = Some(ActiveMove { client: id, frame, grab_offset: local });
                }
            }
            // A shaded window has nothing to resize — real WindowMaker
            // refuses outright (`wMouseResizeWindow` bails if
            // `wwin->flags.shaded`); matching that rather than letting
            // a resize silently reshape a window the user can't even
            // see the content of.
            HitTarget::ResizeEdge(edge) if button == MouseButton::Left && !client.flags.contains(ClientFlags::SHADED) => {
                let start_frame = Rect {
                    pos: Point::new(client.geometry.pos.x - client.layout.client_offset.x, client.geometry.pos.y - client.layout.client_offset.y),
                    size: client.layout.frame_size,
                };
                self.active_resize = Some(ActiveResize { client: id, edge, start_frame });
            }
            _ => {}
        }
    }

    fn handle_frame_button_release(&mut self, id: ClientId, local: Point, button: MouseButton) {
        if button != MouseButton::Left {
            return;
        }
        if self.active_move.as_ref().is_some_and(|m| m.client == id) {
            self.active_move = None;
        }
        if self.active_resize.as_ref().is_some_and(|r| r.client == id) {
            self.active_resize = None;
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
        if self.active_resize.is_some() {
            self.handle_resize_motion(root);
            return;
        }

        let Some(active) = &self.active_move else {
            return;
        };
        let (client_id, frame, grab_offset) = (active.client, active.frame, active.grab_offset);

        let Some(client) = self.clients.get(client_id) else {
            self.active_move = None;
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
        let raw_pos = Point::new(root.x - grab_offset.x, root.y - grab_offset.y);

        // Edge resistance/attraction (WindowMaker's `src/moveres.c`): pull
        // the dragged frame flush against the screen edge or another
        // window's frame edge once it's within `SNAP_THRESHOLD_PX`. Pure
        // geometry against every other *visible* client's current frame
        // rect, recomputed fresh each motion event — cheap at WM scale.
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
        let new_frame_pos = snap::snap_position(Rect { pos: raw_pos, size: frame_size }, &targets, SNAP_THRESHOLD_PX);

        self.backend
            .set_frame_geometry(frame, Rect { pos: new_frame_pos, size: frame_size });

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
    /// increment — `WindowMaker`'s `wWindowConstrainSize`), and pushes
    /// the result through the normal `reflow_frame` path. All eight
    /// edges/corners are handled: dragging a north or west handle keeps
    /// the *opposite* edge anchored, so the frame's origin moves with
    /// the drag while the far edge stays put — the size-hint constraint
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

    /// Screen area windows should maximize into: the shell-reserved
    /// `workarea` if one was set, else the primary monitor's full
    /// geometry (falling back to a sane default if the backend somehow
    /// reports no monitors at all).
    fn usable_area(&self) -> Rect {
        self.workarea.unwrap_or_else(|| {
            self.backend
                .monitors()
                .into_iter()
                .next()
                .map(|m| m.geometry)
                .unwrap_or(Rect { pos: Point::new(0, 0), size: wm_theme_api::Size::new(800, 600) })
        })
    }

    /// Re-derives layout from a client's current `geometry.size`, then
    /// pushes the resulting frame geometry/decoration/client size to the
    /// backend. Shared tail of any operation that changes a client's
    /// content size in place (`ConfigureRequest`, maximize, unmaximize).
    fn reflow_frame(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let request = Self::decoration_request(client, None);
        let layout = self.theme.layout(&request);
        // Shaded windows show only the titlebar — the frame's *visible*
        // height is overridden to `shaded_frame_height`, but the client's
        // own content geometry (and everything the theme computed from
        // it) is left completely untouched, so unshading is exact.
        let frame_height = if client.flags.contains(ClientFlags::SHADED) { layout.shaded_frame_height } else { layout.frame_size.h };
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
            let buffer = self.theme.render(&request, &layout);
            self.backend.paint_decoration(frame, &buffer);
        }
        self.backend.resize_client(window, content_size);

        if let Some(client) = self.clients.get_mut(id) {
            client.layout = layout;
        }
    }

    /// Grows `id` to fill the usable screen area along `directions`,
    /// remembering its current geometry (the first time it's maximized
    /// in either axis) so `unmaximize` can restore it. No titlebar button
    /// triggers this — see `MaximizeDirections`'s doc comment.
    pub fn maximize(&mut self, id: ClientId, directions: MaximizeDirections) {
        let usable = self.usable_area();
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
        tracing::info!(?id, "unmaximized");
    }

    /// Maximizes along `directions` if not already exactly in that state,
    /// otherwise restores. Always restores to the *original* (pre-any-
    /// maximize) geometry first when switching between direction
    /// combinations, so e.g. toggling from full-maximize to
    /// vertical-only-maximize starts from a clean slate rather than
    /// compounding — simpler to reason about than WindowMaker's XOR-based
    /// incremental toggling, at the cost of not preserving an
    /// in-progress partial state across a direction change.
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

    /// Rolls a client up to just its titlebar (WindowMaker's "shade") —
    /// the content window is hidden (not resized to nothing; a real
    /// window is genuinely unmapped, matching `wShadeWindow`) but its
    /// geometry is left completely untouched, so `unshade` restores
    /// exactly. A no-op if already shaded.
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
        self.backend.set_client_mapped(window, true);
        self.reflow_frame(id);
        tracing::info!(?id, "unshaded");
    }

    pub fn toggle_shade(&mut self, id: ClientId) {
        match self.clients.get(id) {
            Some(client) if client.flags.contains(ClientFlags::SHADED) => self.unshade(id),
            Some(_) => self.shade(id),
            None => {}
        }
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
        if let Some(frame) = client.frame {
            self.backend.unmap_frame(frame);
        }
        if self.focused == Some(id) {
            self.focused = None;
        }
        self.notifications.push_back(Notification::Miniaturized(id, preview));
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
        if let Some(frame) = client.frame {
            self.backend.map_frame(frame);
        }
        // Explicit, not left to an `Expose` reply: a window without
        // backing-store (every frame here — `create_decoration` never
        // requests it) isn't guaranteed by X11 to retain its pixel
        // content while unmapped, so remapping after miniaturize can
        // surface a blank/undefined frame until *something* repaints it.
        // Painting it ourselves right here removes that dependency on
        // Expose timing entirely instead of hoping one arrives.
        self.repaint_decoration(id);
        self.notifications.push_back(Notification::Deminiaturized(id));
        self.focus_client(id);
    }

    fn handle_key_press(&mut self, combo: KeyCombo) {
        match combo.keysym {
            XK_TAB => {
                if combo.modifiers.contains(Modifiers::SHIFT) {
                    self.cycle_focus(-1);
                } else {
                    self.cycle_focus(1);
                }
            }
            XK_RIGHT => self.switch_workspace(self.current_workspace + 1),
            XK_LEFT if self.current_workspace > 0 => self.switch_workspace(self.current_workspace - 1),
            _ => {}
        }
    }

    /// Alt+Tab / Alt+Shift+Tab window cycling (WindowMaker's
    /// `cycling.c`): steps focus to the next/previous mapped, non-
    /// miniaturized client — in `SlotMap` iteration order, which is
    /// stable across calls as long as the client set itself doesn't
    /// change — and raises it immediately. Deliberately no modal
    /// preview panel (real WindowMaker's `switchpanel.c`): each press
    /// commits its step right away, the interaction model most modern
    /// WMs use for Alt-Tab.
    fn cycle_focus(&mut self, direction: i32) {
        let ids: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.lifecycle == Lifecycle::Normal).map(|(id, _)| id).collect();
        if ids.is_empty() {
            return;
        }
        let current = self.focused.and_then(|focused| ids.iter().position(|&id| id == focused));
        let next_index = match current {
            Some(i) => (i as i32 + direction).rem_euclid(ids.len() as i32) as usize,
            None => 0,
        };
        self.focus_client(ids[next_index]);
    }

    fn focus_client(&mut self, id: ClientId) {
        if self.focused == Some(id) {
            if let Some(client) = self.clients.get(id) {
                if let Some(frame) = client.frame {
                    self.backend.raise(frame);
                }
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
        let frame = client.frame;
        self.focused = Some(id);
        // Ungrab: a focused client's own clicks (placing a text cursor,
        // clicking a button inside it, ...) should reach it directly,
        // not detour through the WM on every single click the way an
        // unfocused client's first click does.
        self.backend.ungrab_button_passive(window, MouseButton::Left);
        self.backend.set_input_focus(window);
        if let Some(frame) = frame {
            self.backend.raise(frame);
        }
        self.repaint_decoration(id);
    }

    fn repaint_decoration(&mut self, id: ClientId) {
        let Some(client) = self.clients.get(id) else {
            return;
        };
        let Some(frame) = client.frame else {
            return;
        };
        let pressed_button = self.active_button_press.as_ref().filter(|p| p.client == id).map(|p| p.kind);
        let request = Self::decoration_request(client, pressed_button);
        let buffer = self.theme.render(&request, &client.layout);
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

        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id2).unwrap().flags.contains(ClientFlags::FOCUSED));
        let frame1 = wm.client(id1).unwrap().frame.unwrap();
        assert!(wm.backend().raised_frames.contains(&frame1), "cycling must raise the newly-focused window");
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

        wm.dispatch(alt_tab()); // focused w2 -> w1
        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));
        wm.dispatch(alt_tab()); // w1 -> wraps to w2
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
        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));

        wm.dispatch(alt_shift_tab());
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

        assert!(wm.client(id1).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id3).unwrap().flags.contains(ClientFlags::FOCUSED));
        assert!(!wm.client(id2).unwrap().flags.intersects(ClientFlags::FOCUSED), "a miniaturized client must never be cycled to");
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

    #[test]
    fn alt_ctrl_right_then_left_round_trips_the_workspace() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);
        let mods = Modifiers::ALT | Modifiers::CONTROL;

        wm.dispatch(BackendEvent::KeyPress(KeyCombo { keysym: XK_RIGHT, modifiers: mods }));
        assert_eq!(wm.current_workspace(), 1);

        wm.dispatch(BackendEvent::KeyPress(KeyCombo { keysym: XK_LEFT, modifiers: mods }));
        assert_eq!(wm.current_workspace(), 0);
    }

    #[test]
    fn alt_ctrl_left_at_workspace_zero_does_not_panic_or_go_negative() {
        let backend = FakeBackend::new();
        let mut wm = wm(backend);
        let mods = Modifiers::ALT | Modifiers::CONTROL;

        wm.dispatch(BackendEvent::KeyPress(KeyCombo { keysym: XK_LEFT, modifiers: mods }));

        assert_eq!(wm.current_workspace(), 0);
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
    fn titlebar_double_click_toggles_shade() {
        // Matches real WindowMaker's `titlebarDblClick` default exactly:
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
        let frame_geom = wm.backend().last_frame_geometry.get(&frame).unwrap();
        assert_eq!(frame_geom.size.h, wm.client(id).unwrap().layout.frame_size.h);
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
}
