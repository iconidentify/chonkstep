//! `wm_core::Backend` (and `wm_theme_api::PopupHost`) for the Smithay
//! compositor — the half of the Wayland port that makes the X11-era
//! policy brain drive a scene the compositor itself owns.
//!
//! Where `wm-x11` translates every verb into protocol requests against
//! a server it doesn't control, this backend IS the server: every verb
//! just mutates the records in [`WaylandBackend`] (`windows`, `frames`,
//! `shells`, `stacking`) and sets the `damage` flag; the renderer walks
//! those records on the next redraw. The only calls that leave the
//! process are the ones a client must hear about — xdg configures,
//! close requests, and their XWayland equivalents.
//!
//! Coordinate convention (shared with the renderer and input modules):
//! `FrameRecord::geometry`, `ShellRecord::geometry`, and
//! `WindowRecord::content` are all GLOBAL (output-space) rectangles.
//! `wm-core` thinks of the client as sitting at a frame-local offset
//! inside its frame (that's what reparenting means on X11), so the
//! verbs that speak frame-local coordinates (`create_decoration`'s
//! `client_offset`, `position_client`) are translated to global here,
//! and `set_frame_geometry` carries the content rect along with the
//! frame by the same delta. Keeping every stored rect in one space is
//! what lets the renderer and the pointer hit-test compare frame,
//! content, and shell rects directly.
//!
//! Stacking: `stacking` holds frames and shell surfaces bottom-to-top,
//! but the effective z-order is partitioned — `above: false` shells
//! (desktop furniture) render below every frame, `above: true` shells
//! (dock, menus) above them — with `stacking`'s relative order applying
//! within each band. The reordering verbs here (`raise`, `restack`,
//! `raise_shell_surface`) therefore only need to get the relative order
//! within a band right; hit-testing must walk the same three bands
//! top-down to agree with what the renderer paints.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::reexports::wayland_server::backend::protocol::ProtocolError;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Logical, Rectangle as SmithayRect, Transform};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use smithay::xwayland::xwm::WmWindowType;

use wm_core::{
    Backend, BackendEvent, DragHandle, KeyCombo, MonitorInfo, MouseButton, SizeHints, WindowType,
    WmClass, WmProtocol,
};
use wm_theme_api::{DecorationBuffer, DecorationLayout, Point, Rect, ResizeEdge, Size};

use crate::state::{
    FrameRecord, ManagedSurface, RootBackground, ShellRecord, StackEntry, WaylandBackend,
    WlFrameId, WlShellId, WlWindowId,
};

/// `wm-theme` rect -> smithay logical rect. The theme side is
/// `i32`/`u32`, smithay is `i32`/`i32`; sizes are window-sized, nowhere
/// near the cast boundary.
fn smithay_rect(rect: Rect) -> SmithayRect<i32, Logical> {
    SmithayRect::new(
        (rect.pos.x, rect.pos.y).into(),
        (rect.size.w as i32, rect.size.h as i32).into(),
    )
}

/// Imports theme-rendered pixels into a renderer-side buffer. The
/// `DecorationBuffer` contract is premultiplied RGBA8 straight from
/// tiny-skia's `data()`, which is exactly what smithay's renderers
/// assume for shm-style formats — so this is a plain copy, no channel
/// swizzling or (un)premultiplying. `Abgr8888` because DRM fourcc names
/// describe the packed little-endian word: bytes R,G,B,A in memory read
/// back as the 32-bit value 0xAABBGGRR, i.e. ABGR. Scale is 1 — the
/// theme already rasterizes at `CHONKSTEP_SCALE`, so its buffers are
/// physical pixels, not logical ones needing another scale factor.
/// `None` for an empty buffer (nothing to show; callers keep whatever
/// they had), mirroring `wm-x11`'s blit ignoring empty buffers.
fn import_buffer(buffer: &DecorationBuffer) -> Option<MemoryRenderBuffer> {
    if buffer.width == 0 || buffer.height == 0 {
        return None;
    }
    Some(MemoryRenderBuffer::from_slice(
        &buffer.pixels,
        Fourcc::Abgr8888,
        (buffer.width as i32, buffer.height as i32),
        1,
        Transform::Normal,
        None,
    ))
}

/// Reads one field of the xdg toplevel's role attributes (title,
/// app_id) — smithay parks them on the surface's user-data map as
/// `XdgToplevelSurfaceData`. `None` if the toplevel is gone, the data
/// was never attached, or the field itself was never set.
fn xdg_attribute<T>(
    toplevel: &ToplevelSurface,
    read: impl FnOnce(&smithay::wayland::shell::xdg::XdgToplevelSurfaceRoleAttributes) -> Option<T>,
) -> Option<T> {
    if !toplevel.alive() {
        return None;
    }
    with_states(toplevel.wl_surface(), |states| {
        let data = states.data_map.get::<XdgToplevelSurfaceData>()?;
        let attributes = data.lock().ok()?;
        read(&attributes)
    })
}

impl Backend for WaylandBackend {
    type WindowId = WlWindowId;
    type FrameId = WlFrameId;
    type ShellId = WlShellId;

    // -- lifecycle --------------------------------------------------------

    fn scan_existing_windows(&mut self) -> Vec<Self::WindowId> {
        // A compositor owns its display from the first instant — no
        // client can have connected before `run()` created the socket,
        // so unlike an X11 WM adopting a running session, there is
        // never anything pre-existing to adopt.
        Vec::new()
    }

    fn monitors(&self) -> Vec<MonitorInfo> {
        // The layout `state.rs`'s `run` discovered, verbatim: one entry
        // per output, named by its connector (`eDP-1`, `HDMI-A-2`;
        // "chonkstep" for the nested backend's host window), positioned
        // in the same global space every rect in this backend lives in,
        // primary first. A compositor knows this without asking anyone
        // — it did the mode setting — so unlike `wm-x11`'s RandR query
        // there is nothing here that can fail or go stale between
        // calls.
        self.monitors.clone()
    }

    // -- shell surfaces ---------------------------------------------------
    // On X11 these were override-redirect windows; here they are pure
    // scene records the renderer draws directly — creation cannot fail,
    // and none of these verbs involve a client at all.

    fn create_shell_surface(
        &mut self,
        geometry: Rect,
        background: (u8, u8, u8),
        above: bool,
    ) -> Option<Self::ShellId> {
        let id = WlShellId(self.alloc_id());
        self.shells.insert(
            id,
            ShellRecord { geometry, buffer: None, background, above, mapped: false },
        );
        self.stacking.push(StackEntry::Shell(id));
        // Not visible until mapped — no damage yet.
        Some(id)
    }

    fn map_shell_surface(&mut self, id: Self::ShellId) {
        if let Some(shell) = self.shells.get_mut(&id) {
            shell.mapped = true;
            self.damage = true;
        }
    }

    fn unmap_shell_surface(&mut self, id: Self::ShellId) {
        if let Some(shell) = self.shells.get_mut(&id) {
            shell.mapped = false;
            self.damage = true;
        }
    }

    fn destroy_shell_surface(&mut self, id: Self::ShellId) {
        if self.shells.remove(&id).is_some() {
            self.stacking
                .retain(|entry| !matches!(entry, StackEntry::Shell(s) if *s == id));
            self.damage = true;
        }
    }

    fn raise_shell_surface(&mut self, id: Self::ShellId) {
        // To the top of `stacking`; whether that puts it over managed
        // frames is decided by its `above` flag (see the module doc's
        // stacking bands), exactly like an override-redirect window's
        // stacking versus reparented frames on X11.
        if let Some(index) = self
            .stacking
            .iter()
            .position(|entry| matches!(entry, StackEntry::Shell(s) if *s == id))
        {
            let entry = self.stacking.remove(index);
            self.stacking.push(entry);
            self.damage = true;
        }
    }

    fn configure_shell_surface(&mut self, id: Self::ShellId, geometry: Rect) {
        if let Some(shell) = self.shells.get_mut(&id) {
            shell.geometry = geometry;
            self.damage = true;
        }
    }

    fn paint_shell_surface(&mut self, id: Self::ShellId, buffer: &DecorationBuffer) {
        if let Some(shell) = self.shells.get_mut(&id) {
            if let Some(imported) = import_buffer(buffer) {
                shell.buffer = Some(imported);
                self.damage = true;
            }
        }
    }

    fn paint_root_color(&mut self, rgb: (u8, u8, u8)) {
        self.root_background = RootBackground::Color(rgb);
        self.damage = true;
    }

    fn paint_root_image(&mut self, buffer: &DecorationBuffer) {
        // An empty buffer keeps the previous background rather than
        // installing a zero-sized image nothing can render.
        if let Some(imported) = import_buffer(buffer) {
            self.root_background = RootBackground::Image(imported);
            self.damage = true;
        }
    }

    // -- input/event queues -----------------------------------------------
    // The input module translates raw seat events into these queues (it
    // has the hit-testing context); the binary loop drains them here,
    // through the same trait methods it uses on X11.

    fn take_shell_click(&mut self) -> Option<(Self::ShellId, Point, MouseButton, bool)> {
        self.shell_clicks.pop_front()
    }

    fn take_shell_motion(&mut self) -> Option<(Self::ShellId, Point)> {
        self.shell_motions.pop_front()
    }

    fn take_screen_resize(&mut self) -> Option<Size> {
        self.pending_resize.take()
    }

    fn screen_size(&self) -> Size {
        // The union bounding box of every output, which is the extent
        // of the one coordinate space this backend stores rects in.
        // With a single monitor it is that monitor's size, so nothing
        // that predates multi-output sees a change; with several it is
        // deliberately NOT any one screen's size, and callers that mean
        // "the monitor over there" want `monitors()` (or `wm-core`'s
        // per-monitor queries) instead.
        self.output_size
    }

    fn poll_event(&mut self) -> Option<BackendEvent<Self::WindowId, Self::FrameId>> {
        // Protocol handlers already translated everything into
        // `pending` during dispatch — polling is a plain drain, with no
        // connection to go dead underneath us (`ShutdownRequested` is
        // queued by whoever tears the session down, not synthesized
        // here).
        self.pending.pop_front()
    }

    // -- properties -------------------------------------------------------

    fn window_title(&self, window: Self::WindowId) -> Option<String> {
        let record = self.windows.get(&window)?;
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                xdg_attribute(toplevel, |attributes| attributes.title.clone())
            }
            ManagedSurface::X11(surface) => {
                // Smithay tracks `_NET_WM_NAME`/`WM_NAME` itself and
                // hands back one string; empty means "never set", which
                // callers expect as `None`, not "" (see `wm-x11`'s
                // `get_text_property`).
                let title = surface.title();
                if title.is_empty() {
                    None
                } else {
                    Some(title)
                }
            }
        }
    }

    fn window_class(&self, window: Self::WindowId) -> Option<WmClass> {
        let record = self.windows.get(&window)?;
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                // Wayland has one identity string (the xdg app_id, by
                // convention the desktop-file id) where ICCCM has an
                // instance/class pair; reporting it as both halves lets
                // every `WM_CLASS`-keyed policy in the shell (launch
                // matching, opacity rules) keep working unchanged.
                let app_id = xdg_attribute(toplevel, |attributes| attributes.app_id.clone())?;
                Some(WmClass { instance: app_id.clone(), class: app_id })
            }
            ManagedSurface::X11(surface) => {
                let class = surface.class();
                let instance = surface.instance();
                if class.is_empty() && instance.is_empty() {
                    None
                } else {
                    Some(WmClass { instance, class })
                }
            }
        }
    }

    fn window_pid(&self, window: Self::WindowId) -> Option<u32> {
        let record = self.windows.get(&window)?;
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                // No property to trust here — the kernel tells us who
                // is on the other end of the socket (SO_PEERCRED),
                // which is strictly more reliable than X11's
                // client-asserted `_NET_WM_PID`.
                if !toplevel.alive() {
                    return None;
                }
                let client = toplevel.wl_surface().client()?;
                let credentials = client.get_credentials(&self.display_handle).ok()?;
                u32::try_from(credentials.pid).ok()
            }
            ManagedSurface::X11(surface) => surface.pid(),
        }
    }

    fn size_hints(&self, window: Self::WindowId) -> SizeHints {
        let Some(record) = self.windows.get(&window) else {
            return SizeHints::default();
        };
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                if !toplevel.alive() {
                    return SizeHints::default();
                }
                // xdg_toplevel's set_min_size/set_max_size land in the
                // surface's committed cached state; (0, 0) is the
                // protocol's "no limit", which is exactly `None` here.
                // Wayland has no resize-increment concept at all —
                // terminals snap themselves — so that stays `None`
                // rather than being faked.
                let (min, max) = with_states(toplevel.wl_surface(), |states| {
                    let mut guard = states.cached_state.get::<SurfaceCachedState>();
                    let cached = guard.current();
                    (cached.min_size, cached.max_size)
                });
                let to_size = |size: smithay::utils::Size<i32, Logical>| {
                    if size.w > 0 && size.h > 0 {
                        Some(Size::new(size.w as u32, size.h as u32))
                    } else {
                        None
                    }
                };
                SizeHints { min_size: to_size(min), max_size: to_size(max), resize_increment: None }
            }
            ManagedSurface::X11(surface) => {
                let to_size = |size: smithay::utils::Size<i32, Logical>| {
                    if size.w > 0 && size.h > 0 {
                        Some(Size::new(size.w as u32, size.h as u32))
                    } else {
                        None
                    }
                };
                SizeHints {
                    min_size: surface.min_size().and_then(to_size),
                    max_size: surface.max_size().and_then(to_size),
                    // `WM_NORMAL_HINTS` does carry increments and
                    // smithay parses them — pass them through so an
                    // xterm under XWayland resizes in cell steps.
                    resize_increment: surface.size_hints().and_then(|hints| {
                        let (w, h) = hints.size_increment?;
                        if w > 0 && h > 0 {
                            Some(Size::new(w as u32, h as u32))
                        } else {
                            None
                        }
                    }),
                }
            }
        }
    }

    fn supports_protocol(&self, window: Self::WindowId, protocol: WmProtocol) -> bool {
        let _ = window;
        match protocol {
            // Every xdg toplevel understands xdg_toplevel.close, and
            // smithay's `X11Surface::close` handles the WM_DELETE-vs-
            // destroy decision internally — so from `wm-core`'s
            // perspective a polite close is always available.
            WmProtocol::DeleteWindow => true,
            // No Wayland analog of `WM_TAKE_FOCUS` exists (focus is
            // compositor-assigned, never client-negotiated), so no
            // client "supports" it — `wm-core` then just focuses
            // directly, which is the right thing here.
            WmProtocol::TakeFocus => false,
        }
    }

    fn window_geometry(&self, window: Self::WindowId) -> Rect {
        // Queried at map time for the fresh client's desired size (see
        // the trait doc). (0, 0) as the position is `wm-core`'s own
        // "don't care, WM decides" convention — which is what a Wayland
        // client, having no say over its position at all, always means.
        let fallback = Rect { pos: Point::new(0, 0), size: Size::new(200, 150) };
        let Some(record) = self.windows.get(&window) else {
            return fallback;
        };
        match &record.surface {
            // The XWM tracks X11 geometry live (clients configure
            // themselves pre-map), so ask it rather than our record.
            ManagedSurface::X11(surface) => {
                let geometry = surface.geometry();
                Rect {
                    pos: Point::new(geometry.loc.x, geometry.loc.y),
                    size: Size::new(geometry.size.w.max(1) as u32, geometry.size.h.max(1) as u32),
                }
            }
            ManagedSurface::Xdg(_) => {
                if record.content.size.w == 0 || record.content.size.h == 0 {
                    fallback
                } else {
                    record.content
                }
            }
        }
    }

    /// Best-effort `None` for now: the miniaturize preview needs the
    /// window's rendered pixels, which on this backend means rendering
    /// the surface tree into an offscreen GLES target and reading it
    /// back — a self-contained follow-up (the renderer module owns the
    /// GLES context this needs). The icon/switcher SDK already designs
    /// for missing previews, so the cost of shipping without it is a
    /// generic tile instead of a live thumbnail, not a broken feature.
    fn capture_window_image(&self, window: Self::WindowId, _size: Size) -> Option<DecorationBuffer> {
        // Served from the snapshot the renderer keeps refreshing (see
        // `crate::capture` for why a compositor cannot answer this
        // synchronously the way the X11 backend does with XGetImage).
        // `size` is advisory: the shell's icon and switcher renderers
        // scale whatever preview they are handed into their own wells.
        self.windows.get(&window).and_then(|record| record.snapshot.clone())
    }

    // -- decoration realization -------------------------------------------

    fn create_decoration(&mut self, window: Self::WindowId, layout: &DecorationLayout) -> Self::FrameId {
        let frame = WlFrameId(self.alloc_id());
        // Born at the origin; `wm-core` always follows up with
        // `set_frame_geometry` to place it (see `manage_window`), and
        // that call carries the content rect along.
        let geometry = Rect { pos: Point::new(0, 0), size: layout.frame_size };
        if let Some(record) = self.windows.get_mut(&window) {
            // The X11 backend reparents the client to `client_offset`
            // inside the frame and maps it here; our equivalent is
            // pinning the content rect to that offset (frame currently
            // at the origin, so global == frame-local) and marking the
            // content drawable.
            record.content.pos = Point::new(layout.client_offset.x, layout.client_offset.y);
            record.mapped = true;
        }
        self.frames.insert(
            frame,
            FrameRecord { window, geometry, buffer: None, mapped: false },
        );
        self.stacking.push(StackEntry::Frame(frame));
        frame
    }

    fn destroy_decoration(&mut self, frame: Self::FrameId) {
        if self.frames.remove(&frame).is_some() {
            self.stacking
                .retain(|entry| !matches!(entry, StackEntry::Frame(f) if *f == frame));
            self.damage = true;
        }
    }

    fn paint_decoration(&mut self, frame: Self::FrameId, buffer: &DecorationBuffer) {
        if let Some(record) = self.frames.get_mut(&frame) {
            if let Some(imported) = import_buffer(buffer) {
                record.buffer = Some(imported);
                self.damage = true;
            }
        }
    }

    /// No-op for now: the pointer image is composited by the renderer
    /// from the input module's pointer state, so indicating a resize
    /// edge means swapping that cursor image, not flagging a window —
    /// a follow-up in the input/renderer pair once a second cursor
    /// image exists to swap to. Harmless meanwhile: resize itself works,
    /// only the hover affordance is missing.
    fn set_frame_cursor(&mut self, _frame: Self::FrameId, _edge: Option<ResizeEdge>) {}

    // -- geometry / visibility --------------------------------------------

    fn set_frame_geometry(&mut self, frame: Self::FrameId, geometry: Rect) {
        let Some(record) = self.frames.get_mut(&frame) else {
            return;
        };
        // The client rides inside the frame (on X11 the server moves it
        // for free, being a child window) — translate the content rect
        // by the same delta so both stay in global coordinates. Pure
        // moves are exactly this; resizes leave the content alone until
        // the separate `resize_client` call that always accompanies
        // them.
        let delta = Point::new(
            geometry.pos.x - record.geometry.pos.x,
            geometry.pos.y - record.geometry.pos.y,
        );
        record.geometry = geometry;
        let window = record.window;
        if let Some(window_record) = self.windows.get_mut(&window) {
            window_record.content.pos.x += delta.x;
            window_record.content.pos.y += delta.y;
            // An X11 client must be told where it now sits — apps
            // position popups relative to their own believed root
            // coordinates, and a stale idea of those puts menus in the
            // wrong place (the classic reparenting-WM bug).
            if let ManagedSurface::X11(surface) = &window_record.surface {
                if surface.alive() {
                    if let Err(error) = surface.configure(smithay_rect(window_record.content)) {
                        tracing::warn!(?error, ?window, "X11 configure after frame move failed");
                    }
                }
            }
        }
        self.damage = true;
    }

    fn resize_client(&mut self, window: Self::WindowId, size: Size) {
        let Some(record) = self.windows.get_mut(&window) else {
            return;
        };
        record.content.size = size;
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                if toplevel.alive() {
                    // The configure/ack/commit dance is how a Wayland
                    // client learns its size — there is no server-side
                    // resize to perform. `send_pending_configure`
                    // dedups: a size the client already has produces no
                    // event, which matters during interactive resize
                    // where this is called per motion event.
                    toplevel.with_pending_state(|state| {
                        state.size = Some((size.w as i32, size.h as i32).into());
                    });
                    let _ = toplevel.send_pending_configure();
                }
            }
            ManagedSurface::X11(surface) => {
                if surface.alive() {
                    if let Err(error) = surface.configure(smithay_rect(record.content)) {
                        tracing::warn!(?error, ?window, "X11 resize configure failed");
                    }
                }
            }
        }
        self.damage = true;
    }

    fn configure_unmanaged(&mut self, window: Self::WindowId, geometry: Rect) {
        let Some(record) = self.windows.get_mut(&window) else {
            return;
        };
        record.content = geometry;
        match &record.surface {
            ManagedSurface::X11(surface) => {
                // The ICCCM case this verb exists for: an X11 client
                // configuring itself before its first map must be
                // acknowledged or it deadlocks waiting.
                if surface.alive() {
                    if let Err(error) = surface.configure(smithay_rect(geometry)) {
                        tracing::warn!(?error, ?window, "X11 unmanaged configure failed");
                    }
                }
            }
            ManagedSurface::Xdg(toplevel) => {
                // xdg clients never configure themselves, so `wm-core`
                // only reaches here via a translated size request —
                // answer with a configure for the size half; position
                // isn't a concept the protocol lets us grant.
                if toplevel.alive() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some((geometry.size.w as i32, geometry.size.h as i32).into());
                    });
                    let _ = toplevel.send_pending_configure();
                }
            }
        }
        if record.mapped {
            self.damage = true;
        }
    }

    fn map_frame(&mut self, frame: Self::FrameId) {
        if let Some(record) = self.frames.get_mut(&frame) {
            record.mapped = true;
            self.damage = true;
        }
    }

    fn unmap_frame(&mut self, frame: Self::FrameId) {
        if let Some(record) = self.frames.get_mut(&frame) {
            record.mapped = false;
            self.damage = true;
        }
    }

    fn set_client_mapped(&mut self, window: Self::WindowId, mapped: bool) {
        // Shading, purely compositor-side: the renderer stops drawing
        // the content while the frame stays. The client never learns —
        // no unmap event exists to suppress (unlike `wm-x11`, which has
        // to swallow the UnmapNotify its own request generates), and
        // Wayland clients keep their buffers, so the unshade path needs
        // no repaint coaxing either.
        if let Some(record) = self.windows.get_mut(&window) {
            record.mapped = mapped;
            self.damage = true;
        }
    }

    // -- stacking ---------------------------------------------------------

    fn raise(&mut self, frame: Self::FrameId) {
        if let Some(index) = self
            .stacking
            .iter()
            .position(|entry| matches!(entry, StackEntry::Frame(f) if *f == frame))
        {
            let entry = self.stacking.remove(index);
            self.stacking.push(entry);
            self.damage = true;
        }
    }

    fn restack(&mut self, order_back_to_front: &[Self::FrameId]) {
        // Pull the listed frames out and re-append them in the given
        // order. Their position relative to shell entries in the Vec is
        // irrelevant — the renderer's banding (module doc) decides
        // frame-vs-shell layering — so only the relative order among
        // frames needs to be exact. Frames the caller didn't list
        // (other workspaces, miniaturized) keep their slots below the
        // relisted ones, which matches X11's restack pushing the listed
        // stack to the bottom-to-front order as a block.
        let listed: Vec<StackEntry> = order_back_to_front
            .iter()
            .filter(|frame| self.frames.contains_key(frame))
            .map(|&frame| StackEntry::Frame(frame))
            .collect();
        self.stacking
            .retain(|entry| !matches!(entry, StackEntry::Frame(f) if order_back_to_front.contains(f)));
        self.stacking.extend(listed);
        self.damage = true;
    }

    // -- focus / close ----------------------------------------------------

    fn set_input_focus(&mut self, window: Self::WindowId) {
        // Deferred, not applied: `KeyboardHandle::set_focus` needs
        // `&mut Compositor` (it dispatches enter/leave through the
        // handler traits), and this backend lives INSIDE the
        // `Compositor` (via `WindowManager`) — so the seat call from
        // here would be a self-reborrow. The main loop drains this
        // after every dispatch/notification pass and performs the seat
        // focus (plus `X11Surface::set_activated` for XWayland
        // windows) with the whole `Compositor` in hand.
        self.pending_focus = Some(window);
    }

    fn send_close(&mut self, window: Self::WindowId) {
        let Some(record) = self.windows.get(&window) else {
            return;
        };
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                if toplevel.alive() {
                    toplevel.send_close();
                }
            }
            ManagedSurface::X11(surface) => {
                // Smithay's close() already implements this trait
                // method's exact contract: WM_DELETE_WINDOW when the
                // client supports it, outright destruction otherwise.
                if surface.alive() {
                    if let Err(error) = surface.close() {
                        tracing::warn!(?error, ?window, "X11 close request failed");
                    }
                }
            }
        }
    }

    fn kill_client(&mut self, window: Self::WindowId) {
        let Some(record) = self.windows.get(&window) else {
            return;
        };
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                // The Wayland equivalent of XKillClient: sever the
                // client's connection. Posting a protocol error is the
                // mechanism wayland-server offers for a server-initiated
                // disconnect; the object fields are zero because no
                // object misbehaved — the user did the killing.
                if let Some(client) = toplevel.wl_surface().client() {
                    client.kill(
                        &self.display_handle,
                        ProtocolError {
                            code: 0,
                            object_id: 0,
                            object_interface: String::new(),
                            message: "killed by the window manager (close request unanswered)"
                                .to_string(),
                        },
                    );
                }
            }
            ManagedSurface::X11(surface) => {
                // Smithay exposes no XKillClient; close() at least
                // destroys the window outright for clients without
                // WM_DELETE_WINDOW. A true connection kill for a hung
                // XWayland client is a follow-up (needs the XWM's own
                // connection, which the xwayland module owns).
                if surface.alive() {
                    if let Err(error) = surface.close() {
                        tracing::warn!(?error, ?window, "X11 kill (close) failed");
                    }
                }
            }
        }
    }

    // -- input grabs ------------------------------------------------------

    /// A compositor owns the pointer unconditionally — every motion and
    /// button event already flows through this process before any
    /// client sees it, which is the entire condition an X11 pointer
    /// grab exists to create. During a drag the input module simply
    /// keeps routing events to the WM instead of the surface under the
    /// pointer, so a no-op handle is the CORRECT implementation, not a
    /// stub.
    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        DragHandle(0)
    }

    fn ungrab_pointer(&mut self, _handle: DragHandle) {}

    fn grab_key(&mut self, combo: KeyCombo) {
        // No server to register grabs with — the keyboard handler
        // checks every press against this list before deciding whether
        // the client gets the key (see the input module). Idempotent so
        // a rebind pass can't inflate the list.
        if !self.grabbed_combos.contains(&combo) {
            self.grabbed_combos.push(combo);
        }
    }

    fn ungrab_key(&mut self, combo: KeyCombo) {
        self.grabbed_combos.retain(|existing| *existing != combo);
    }

    fn grab_keyboard(&mut self) {
        // The modal Alt-Tab grab: while set, the input module routes
        // every press AND release to `wm-core` (as KeyPress/KeyRelease)
        // and none to clients — same effect as the X11 active grab,
        // with no server round-trip to fail.
        self.keyboard_grabbed = true;
    }

    fn ungrab_keyboard(&mut self) {
        self.keyboard_grabbed = false;
    }

    fn refresh_client(&mut self, _window: Self::WindowId, _size: Size) {
        // Wayland clients own their buffers — the compositor never
        // discards content the way an unmapped X11 window loses its
        // pixels, so there is nothing to ask the client to repaint;
        // redrawing our own scene from the retained buffers suffices.
        self.damage = true;
    }

    // -- EWMH-shaped policy reads/acts ------------------------------------
    // The publish_* family stays at the trait's no-op defaults: a
    // Wayland session has no EWMH root properties to publish. A later
    // foreign-toplevel-management pass can override them to feed
    // Wayland-native taskbars the same information.

    fn window_type(&self, window: Self::WindowId) -> WindowType {
        let Some(record) = self.windows.get(&window) else {
            return WindowType::Normal;
        };
        match &record.surface {
            // xdg toplevels are, by protocol construction, exactly the
            // decorate-and-manage kind — menus/tooltips arrive as
            // xdg_popups, which never reach `wm-core` as MapRequests at
            // all (the xdg module positions and renders them directly).
            ManagedSurface::Xdg(_) => WindowType::Normal,
            ManagedSurface::X11(surface) => {
                // Override-redirect means "stay out of my way" by
                // definition, before any type property is consulted.
                if surface.is_override_redirect() {
                    return WindowType::Unmanaged;
                }
                match surface.window_type() {
                    Some(WmWindowType::Dialog) => WindowType::Dialog,
                    Some(WmWindowType::Normal) | Some(WmWindowType::Utility) | None => {
                        WindowType::Normal
                    }
                    // Menus, docks, tooltips, splashes, notifications:
                    // draw their own chrome, place themselves — same
                    // bucket `wm-x11` sorts these atoms into.
                    Some(_) => WindowType::Unmanaged,
                }
            }
        }
    }

    fn map_unmanaged(&mut self, window: Self::WindowId) {
        // `wm-core` only calls this for windows `window_type` classified
        // `Unmanaged`, so record that on the ledger entry as well.
        // Without it the field keeps the `Normal` it was born with, and
        // the renderer's override-redirect pass - which is the *only*
        // thing that draws a frameless window, since these have no
        // frame to draw them with - never matches. The window is then
        // invisible while still answering clicks through the hit-test,
        // which reads the same field. Every XWayland menu, dropdown and
        // tooltip lands here.
        let kind = self.window_type(window);
        if let Some(record) = self.windows.get_mut(&window) {
            record.window_type = kind;
            record.mapped = true;
            if let ManagedSurface::X11(surface) = &record.surface {
                // A non-OR X11 window that asked to map still needs its
                // MapRequest honored by the (X11) WM even when we
                // decline to decorate it; OR windows mapped themselves
                // already and set_mapped would rightly refuse them.
                if surface.alive() && !surface.is_override_redirect() {
                    if let Err(error) = surface.set_mapped(true) {
                        tracing::warn!(?error, ?window, "X11 unmanaged map failed");
                    }
                }
            }
            self.damage = true;
        }
    }

    fn position_client(&mut self, window: Self::WindowId, pos: Point) {
        // `pos` is frame-local (the chrome offset — see the trait doc:
        // this only fires when that offset changes, fullscreen in/out).
        // Translate to global against the owning frame.
        let Some(frame_pos) = self
            .frames
            .values()
            .find(|record| record.window == window)
            .map(|record| record.geometry.pos)
        else {
            return;
        };
        let Some(record) = self.windows.get_mut(&window) else {
            return;
        };
        record.content.pos = Point::new(frame_pos.x + pos.x, frame_pos.y + pos.y);
        if let ManagedSurface::X11(surface) = &record.surface {
            if surface.alive() {
                if let Err(error) = surface.configure(smithay_rect(record.content)) {
                    tracing::warn!(?error, ?window, "X11 position configure failed");
                }
            }
        }
        self.damage = true;
    }

    // A compositor sees every click before any client does, so there is
    // no first-click race for click-to-focus and nothing to passively
    // grab or replay — the trait's own doc calls these out as no-ops on
    // exactly this kind of backend.
    fn grab_button_passive(&mut self, _window: Self::WindowId, _button: MouseButton) {}

    fn ungrab_button_passive(&mut self, _window: Self::WindowId, _button: MouseButton) {}

    fn replay_pointer(&mut self) {}
}

/// The desktop shell's popup family (cascade menus, tooltips), on the
/// same id space as the shell surfaces so `chonk-shell`'s
/// `Shell<B: Backend + PopupHost<PopupId = B::ShellId>>` bound holds —
/// popups are simply above-band shell surfaces created pre-mapped and
/// pre-raised.
impl wm_theme_api::PopupHost for WaylandBackend {
    type PopupId = WlShellId;

    fn create_popup(&mut self, geometry: Rect, background: (u8, u8, u8)) -> Option<Self::PopupId> {
        let popup = <Self as Backend>::create_shell_surface(self, geometry, background, true)?;
        <Self as Backend>::map_shell_surface(self, popup);
        <Self as Backend>::raise_shell_surface(self, popup);
        Some(popup)
    }

    fn destroy_popup(&mut self, popup: Self::PopupId) {
        <Self as Backend>::destroy_shell_surface(self, popup);
    }

    fn paint_popup(&mut self, popup: Self::PopupId, buffer: &DecorationBuffer) {
        <Self as Backend>::paint_shell_surface(self, popup, buffer);
    }

    /// Trivial token: on X11 this is a real server-side pointer grab so
    /// a press-drag-release over a menu doesn't get swallowed by the
    /// implicit grab of wherever the press started — but this
    /// compositor already receives every pointer event and does its own
    /// top-down hit-test per event, so the "grab" the cascade
    /// controller wants is simply how input routing always works here.
    fn grab_pointer(&mut self) -> wm_theme_api::PopupGrab {
        wm_theme_api::PopupGrab(0)
    }

    fn ungrab_pointer(&mut self, _grab: wm_theme_api::PopupGrab) {}
}
