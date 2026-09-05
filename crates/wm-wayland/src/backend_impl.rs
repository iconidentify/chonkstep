//! `wm_core::Backend` (and `wm_theme_api::PopupHost`) for the Smithay
//! compositor — the half of the Wayland port that makes the X11-era
//! policy brain drive a scene the compositor itself owns.
//!
//! Where `wm-x11` translates every verb into protocol requests against
//! a server it doesn't control, this backend IS the server: every verb
//! just mutates the records in [`WaylandBackend`] (`windows`, `frames`,
//! `shells`, and their two stacking orders) and sets the `damage` flag; the renderer walks
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
//! Stacking: `stacking` holds frames and client-decorated windows
//! bottom-to-top; `shell_stacking` holds only shell surfaces. The shell
//! order is partitioned — `above: false` desktop furniture renders below
//! every frame, `above: true` docks and menus above them — with relative
//! order applying within each band. A managed
//! window whose client draws its own chrome has no frame to hold its
//! place, so it holds one itself (`StackEntry::Window`) in the frame
//! band: same band, same order, one fewer buffer to paint. The
//! reordering verbs here therefore only need to get the relative order
//! within their own ledger right;
//! hit-testing must walk the same three bands top-down to agree with
//! what the renderer paints.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::element::memory::MemoryRenderBuffer;
use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State as XdgToplevelState;
use smithay::reexports::wayland_server::backend::protocol::ProtocolError;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::{Buffer, Logical, Rectangle as SmithayRect, Transform};
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::{SurfaceCachedState, ToplevelSurface, XdgToplevelSurfaceData};
use smithay::xwayland::xwm::WmWindowType;

use wm_core::{
    Backend, BackendEvent, DragHandle, KeyCombo, MonitorInfo, MouseButton, ScrollDelta, SizeHints, WindowType, WmClass,
    WmProtocol,
};
use wm_theme_api::{DecorationBuffer, DecorationLayout, DecorationSurface, Point, Rect, ResizeEdge, Size};

use crate::input::DragGrab;
use crate::state::{
    ensure_stack_entry, raise_stack_entry, replace_stack_entry, FramePart, FrameRecord, ManagedSurface,
    PointerGrabChange, RootBackground, ShellRecord, StackEntry, WaylandBackend, WlFrameId, WlShellId,
    WlWindowId,
};

/// `wm-theme` rect -> smithay logical rect. The theme side is
/// `i32`/`u32`, smithay is `i32`/`i32`; sizes are window-sized, nowhere
/// near the cast boundary.
fn smithay_rect(rect: Rect) -> SmithayRect<i32, Logical> {
    SmithayRect::new((rect.pos.x, rect.pos.y).into(), (rect.size.w as i32, rect.size.h as i32).into())
}

/// Imports theme-rendered pixels into a renderer-side buffer. The
/// `DecorationBuffer` contract is premultiplied RGBA8 straight from
/// tiny-skia's `data()`, which is exactly what smithay's renderers
/// assume for shm-style formats — so this is a plain copy, no channel
/// swizzling or (un)premultiplying. `Abgr8888` because DRM fourcc names
/// describe the packed little-endian word: bytes R,G,B,A in memory read
/// back as the 32-bit value 0xAABBGGRR, i.e. ABGR.
///
/// Buffer scale 1, permanently — not "until the outputs advertise the
/// session's scale", which they now do. The theme already rasterizes
/// at `CHONKSTEP_SCALE`, so these buffers are physical pixels, and
/// every damage tracker in this crate composes at scale 1 whatever the
/// outputs say (see `state.rs::physical_damage_tracker`), so a scale-1
/// declaration is what lands 1 buffer pixel on 1 screen pixel — with
/// no `size / scale` integer division to shave a pixel off odd-sized
/// chrome, which is what declaring the UI scale here would cost. (A
/// `ui_scale` field once threaded through here for the declare-at-UI-
/// scale design; it was removed as dead when the pinned-physical
/// design landed.)
///
/// `None` for an empty buffer (nothing to show; callers keep whatever
/// they had), mirroring `wm-x11`'s blit ignoring empty buffers.
fn import_buffer(buffer: &DecorationBuffer, opaque: bool) -> Option<MemoryRenderBuffer> {
    if buffer.width == 0 || buffer.height == 0 {
        return None;
    }
    let opaque_regions = opaque.then(|| {
        vec![SmithayRect::<i32, Buffer>::from_size(
            (buffer.width as i32, buffer.height as i32).into(),
        )]
    });
    Some(MemoryRenderBuffer::from_slice(
        &buffer.pixels,
        Fourcc::Abgr8888,
        (buffer.width as i32, buffer.height as i32),
        1,
        Transform::Normal,
        opaque_regions,
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

/// The string a `[decorations]` rule matches an XWayland window on: its
/// `WM_CLASS` instance if there is one, else its class.
///
/// Instance first because that is the more specific half and the one a
/// user reads off `xprop` — `WM_CLASS(STRING) = "libreoffice",
/// "libreoffice-writer"` — and because the rules match by prefix, so
/// naming the class still catches every instance under it.
fn x11_identity(surface: &smithay::xwayland::X11Surface) -> Option<String> {
    let instance = surface.instance();
    if !instance.is_empty() {
        return Some(instance);
    }
    let class = surface.class();
    if class.is_empty() {
        None
    } else {
        Some(class)
    }
}

/// The one honest projection of `wm-core`'s two maximize axes into the
/// single "maximized" both protocols speak (smithay's
/// `X11Surface::set_maximized` sets both `_NET_WM_STATE_MAXIMIZED_*`
/// atoms as a pair; xdg has one `Maximized` state): both axes set means
/// maximized, anything less means not. A half-maximized window
/// therefore publishes as unmaximized — the alternative, claiming the
/// full state for one axis, would have a toolkit square its corners
/// and pin its geometry for a shape it does not have.
fn both_axes_maximized(max_h: bool, max_v: bool) -> bool {
    max_h && max_v
}

fn set_combo_membership(combos: &mut Vec<KeyCombo>, combo: KeyCombo, enabled: bool) {
    if enabled {
        if !combos.contains(&combo) {
            combos.push(combo);
        }
    } else {
        combos.retain(|existing| *existing != combo);
    }
}

impl WaylandBackend {
    /// Cheap build/hardware half of `hyprctl systeminfo`. Kept apart
    /// from the full diagnostic dump so routine compatibility queries
    /// never walk every protocol object or `/proc` entry.
    pub(crate) fn system_snapshot(&self) -> String {
        use std::fmt::Write as _;

        let mut report = format!("graphics {}\n", self.graphics_diagnostics);
        for (index, monitor) in self.monitors.iter().enumerate() {
            let scale = self.monitor_scales.get(index).copied().unwrap_or(1.0);
            let hardware = self.monitor_outputs.get(index);
            let _ = writeln!(
                report,
                "output index={} name={:?} x={} y={} w={} h={} scale={} refresh_millihertz={} transform={}",
                index,
                monitor.name,
                monitor.geometry.pos.x,
                monitor.geometry.pos.y,
                monitor.geometry.size.w,
                monitor.geometry.size.h,
                scale,
                hardware.map_or(0, |output| output.refresh_millihertz),
                hardware.map_or(0, |output| output.transform),
            );
        }
        report
    }
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

    fn monitors_ref(&self) -> &[MonitorInfo] {
        &self.monitors
    }

    fn decoration_scale(&self, frame: Rect) -> f32 {
        self.scale_at(frame) as f32
    }

    fn diagnostic_snapshot(&self) -> String {
        use std::fmt::Write as _;

        let mut report = String::new();
        let last_damage = self
            .last_damage_source
            .map_or_else(|| "startup".to_string(), |location| format!("{}:{}", location.file(), location.line()));
        let _ = writeln!(
            report,
            "state locked={} damage={} last_damage={} monitors={} windows={} frames={} layers={} shells={} ime_popups={}",
            self.locked,
            self.damage,
            last_damage,
            self.monitors.len(),
            self.windows.len(),
            self.frames.len(),
            self.layers.len(),
            self.shells.len(),
            self.ime_popups.len(),
        );
        report.push_str(&self.system_snapshot());
        let _ = writeln!(
            report,
            "focus pending={:?} pointer={:?} keyboard_grab={} pointer_grab={} pending_pointer_grab={}",
            self.pending_focus,
            self.pointer,
            self.keyboard_grabbed,
            self.pointer_grab.is_some(),
            self.pending_pointer_grab.is_some(),
        );
        let _ = writeln!(report, "diagnostics {}", crate::diagnostics::describe());

        report.push_str("scene bottom-to-top\n");
        for entry in &self.stacking {
            match entry {
                StackEntry::Frame(id) => {
                    let Some(frame) = self.frames.get(id) else { continue };
                    let window = self.windows.get(&frame.window);
                    let _ = writeln!(
                        report,
                        " frame id={} window={} x={} y={} w={} h={} mapped={} app={:?} title={:?}",
                        id.0,
                        frame.window.0,
                        frame.geometry.pos.x,
                        frame.geometry.pos.y,
                        frame.geometry.size.w,
                        frame.geometry.size.h,
                        frame.mapped,
                        window.and_then(|record| record.app_id.as_deref()).unwrap_or(""),
                        window.and_then(|record| record.title.as_deref()).unwrap_or(""),
                    );
                }
                StackEntry::Window(id) => {
                    let Some(window) = self.windows.get(id) else { continue };
                    let _ = writeln!(
                        report,
                        " window id={} x={} y={} w={} h={} mapped={} app={:?} title={:?}",
                        id.0,
                        window.content.pos.x,
                        window.content.pos.y,
                        window.content.size.w,
                        window.content.size.h,
                        window.mapped,
                        window.app_id.as_deref().unwrap_or(""),
                        window.title.as_deref().unwrap_or(""),
                    );
                }
            }
        }
        for layer in &self.layers {
            let _ = writeln!(
                report,
                " layer id={} output={} kind={:?} namespace={:?} x={} y={} w={} h={} mapped={} visible={}",
                layer.id.0,
                layer.output,
                layer.layer,
                layer.namespace,
                layer.geometry.pos.x,
                layer.geometry.pos.y,
                layer.geometry.size.w,
                layer.geometry.size.h,
                layer.mapped,
                self.layer_presented(layer),
            );
        }
        for id in &self.shell_stacking {
            let Some(shell) = self.shells.get(id) else { continue };
            let _ = writeln!(
                report,
                " shell id={} x={} y={} w={} h={} mapped={} above={} buffer_bytes={}",
                id.0,
                shell.geometry.pos.x,
                shell.geometry.pos.y,
                shell.geometry.size.w,
                shell.geometry.size.h,
                shell.mapped,
                shell.above,
                shell.buffer_bytes,
            );
        }

        let handle = self.display_handle.backend_handle();
        let mut clients = Vec::new();
        handle.with_all_clients(|client| clients.push(client));
        clients.sort_by_key(|client| format!("{client:?}"));
        report.push_str("clients\n");
        for client_id in clients {
            let credentials = handle.get_client_credentials(client_id.clone()).ok();
            let pid = credentials.map(|credentials| credentials.pid);
            let executable = pid
                .and_then(|pid| std::fs::read_link(format!("/proc/{pid}/exe")).ok())
                .and_then(|path| path.file_name().map(|name| name.to_string_lossy().into_owned()))
                .unwrap_or_else(|| "unknown".to_string());
            let mut objects = Vec::new();
            let _ = handle.with_all_objects_for(client_id.clone(), |object| objects.push(object));
            let mut surfaces = 0usize;
            let mut buffer_bytes = 0usize;
            for object in objects {
                if object.interface().name != "wl_surface" {
                    continue;
                }
                surfaces = surfaces.saturating_add(1);
                let Ok(surface) = WlSurface::from_id(&self.display_handle, object) else {
                    continue;
                };
                let bytes = with_renderer_surface_state(&surface, |state| {
                    if state.buffer().is_none() {
                        return 0;
                    }
                    let Some(size) = state.buffer_size() else { return 0 };
                    let scale = usize::try_from(state.buffer_scale()).unwrap_or(0);
                    usize::try_from(size.w)
                        .unwrap_or(0)
                        .saturating_mul(usize::try_from(size.h).unwrap_or(0))
                        .saturating_mul(scale)
                        .saturating_mul(scale)
                        .saturating_mul(4)
                })
                .unwrap_or(0);
                buffer_bytes = buffer_bytes.saturating_add(bytes);
            }
            let _ = writeln!(
                report,
                " client id={:?} pid={} uid={} executable={:?} surfaces={} buffer_bytes_estimate={}",
                client_id,
                pid.map_or_else(|| "unknown".to_string(), |pid| pid.to_string()),
                credentials.map_or_else(|| "unknown".to_string(), |credentials| credentials.uid.to_string()),
                executable,
                surfaces,
                buffer_bytes,
            );
        }
        report
    }

    fn set_diagnostic(&mut self, name: &str, enabled: bool) -> Result<(), String> {
        crate::diagnostics::set(name, enabled)?;
        // Plane-path and full-damage changes need one frame to take
        // effect even when the desktop was otherwise idle.
        self.mark_damaged();
        Ok(())
    }

    fn set_log_filter(&mut self, directive: &str) -> Result<(), String> {
        crate::diagnostics::set_log_filter(directive)
    }

    // -- shell surfaces ---------------------------------------------------
    // On X11 these were override-redirect windows; here they are pure
    // scene records the renderer draws directly — creation cannot fail,
    // and none of these verbs involve a client at all.

    fn create_shell_surface(&mut self, geometry: Rect, background: (u8, u8, u8), above: bool) -> Option<Self::ShellId> {
        let id = WlShellId(self.alloc_id());
        self.shells.insert(
            id,
            ShellRecord {
                geometry,
                buffer: None,
                buffer_bytes: 0,
                background,
                above,
                mapped: false,
                fill_id: smithay::backend::renderer::element::Id::new(),
            },
        );
        self.shell_stacking.push(id);
        // Not visible until mapped — no damage yet.
        Some(id)
    }

    fn map_shell_surface(&mut self, id: Self::ShellId) {
        if let Some(shell) = self.shells.get_mut(&id) {
            shell.mapped = true;
            self.mark_damaged();
        }
    }

    fn unmap_shell_surface(&mut self, id: Self::ShellId) {
        if let Some(shell) = self.shells.get_mut(&id) {
            shell.mapped = false;
            self.mark_damaged();
        }
    }

    fn destroy_shell_surface(&mut self, id: Self::ShellId) {
        if self.shells.remove(&id).is_some() {
            self.shell_stacking.retain(|shell| *shell != id);
            self.mark_damaged();
        }
    }

    fn raise_shell_surface(&mut self, id: Self::ShellId) {
        let Some(index) = self.shell_stacking.iter().position(|shell| *shell == id) else {
            return;
        };
        let shell = self.shell_stacking.remove(index);
        self.shell_stacking.push(shell);
        self.mark_damaged();
    }

    fn configure_shell_surface(&mut self, id: Self::ShellId, geometry: Rect) {
        if let Some(shell) = self.shells.get_mut(&id) {
            shell.geometry = geometry;
            self.mark_damaged();
        }
    }

    fn paint_shell_surface(&mut self, id: Self::ShellId, buffer: &DecorationBuffer) {
        if let Some(shell) = self.shells.get_mut(&id) {
            if let Some(imported) = import_buffer(buffer, false) {
                shell.buffer = Some(imported);
                shell.buffer_bytes = buffer.pixels.len();
                self.mark_damaged();
            }
        }
    }

    fn release_shell_buffer(&mut self, id: Self::ShellId) {
        if let Some(shell) = self.shells.get_mut(&id) {
            if shell.buffer.take().is_some() {
                shell.buffer_bytes = 0;
                self.mark_damaged();
            }
        }
    }

    fn paint_root_color(&mut self, rgb: (u8, u8, u8)) {
        self.root_background = RootBackground::Color(rgb);
        self.mark_damaged();
    }

    fn set_layer_surface_hidden(&mut self, namespace: &str, hidden: bool) {
        let changed = if hidden {
            self.hidden_layer_namespaces.insert(namespace.to_string())
        } else {
            self.hidden_layer_namespaces.remove(namespace)
        };
        if changed {
            // The next dispatch pass re-arranges the layers (so a bar's
            // strip is reserved or released) and renders; nothing else
            // to poke.
            self.layer_layout_dirty = true;
            self.idle_policy_dirty = true;
            self.mark_damaged();
            tracing::info!(namespace, hidden, "layer surface visibility changed");
        }
    }

    fn paint_root_image(&mut self, buffer: &DecorationBuffer) {
        // An empty buffer keeps the previous background rather than
        // installing a zero-sized image nothing can render.
        if let Some(imported) = import_buffer(buffer, true) {
            self.root_background = RootBackground::Image(imported);
            self.mark_damaged();
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

    fn take_shell_scroll(&mut self) -> Option<(Self::ShellId, Point, ScrollDelta)> {
        self.shell_scrolls.pop_front()
    }

    fn take_screen_resize(&mut self) -> Option<Size> {
        self.pending_resize.take()
    }

    fn set_ui_scale(&mut self, scale: f32) {
        // Nothing in this ledger is sized from the UI scale: every
        // pixel it holds arrived pre-rasterized from the theme engine,
        // which has already been rebuilt at the new scale by the time
        // this is called. The compositor's *own* pointer is the
        // exception, and it is not reachable from here — it hangs off
        // `Compositor`, and a `Backend` verb runs inside the
        // `WindowManager`'s `&mut self`, which is precisely the borrow
        // that cannot also hold the compositor. So this records the
        // request and `Compositor::dispatch_pending` acts on it, the
        // same detour `set_input_focus` takes through `pending_focus`.
        //
        // Recording it is also what makes the rebuild unmissable: this
        // is the one thing every caller of
        // `Shell::apply_session_state` reaches, whether the scale
        // changed because the compositor loop saw a `reload` marker or
        // because a bound `Action::Reload` ran inside the shell and
        // never returned through that loop at all. See
        // `WaylandBackend::pending_cursor_scale`.
        self.pending_cursor_scale = Some(scale);
    }

    /// The pointer's current position, from the mirror `input.rs`
    /// keeps on the ledger — this backend's spelling of the X11
    /// `query_pointer` round trip, and NOT redundant with the
    /// `PointerMotion` stream: motion over a client's own content is
    /// never queued (it is the client's), so `wm-core`'s remembered
    /// position can be stale by the width of a client-decorated window
    /// at exactly the moment that window asks to be moved or resized.
    /// Answering honestly here is what keeps the first CSD titlebar
    /// drag anchored where the user pressed instead of wherever the
    /// pointer last crossed the desktop — the "first drag teleports
    /// the window" bug, live on LibreOffice until this existed.
    fn pointer_position(&self) -> Option<Point> {
        self.pointer
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
            ManagedSurface::Xdg(toplevel) => xdg_attribute(toplevel, |attributes| attributes.title.clone()),
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
                // Logical, like every size a client declares — and the
                // constraint engine these feed works in physical
                // pixels, so they convert by the surface's committed
                // buffer scale like everything else crossing this
                // boundary. Returning them raw halved a scaled
                // client's real minimum in the ledger's eyes: an
                // interactive resize dragged below it pushed a size
                // the client refused, the client answered with its
                // true minimum, the mismatch came back as a
                // ConfigureRequest, and the next motion pushed the
                // refused size again — the window flickering
                // larger/smaller at drag rate, observed live resizing
                // Microsoft Edge at scale 2.
                let hint_scale = self.window_surface_scale(record);
                let to_size = |size: smithay::utils::Size<i32, Logical>| {
                    if size.w > 0 && size.h > 0 {
                        Some(Size::new(
                            crate::xdg::scale_length(size.w, hint_scale) as u32,
                            crate::xdg::scale_length(size.h, hint_scale) as u32,
                        ))
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

    fn capture_window_image(&self, window: Self::WindowId, _size: Size) -> Option<DecorationBuffer> {
        // Served from the snapshot the renderer keeps refreshing (see
        // `crate::capture` for why a compositor cannot answer this
        // synchronously the way the X11 backend does with XGetImage).
        // `size` is advisory: the shell's icon and switcher renderers
        // scale whatever preview they are handed into their own wells,
        // and a caller that needs more pixels than the default
        // snapshots carry hints it through `set_preview_edge` and
        // re-asks when `preview_generation` moves.
        self.windows.get(&window).and_then(|record| record.snapshot.clone())
    }

    fn set_preview_edge(&mut self, edge: Option<u32>) {
        self.preview_edge = edge;
    }

    fn preview_generation(&self) -> u64 {
        self.preview_generation
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
            FrameRecord {
                window,
                geometry,
                parts: Vec::new(),
                fill_id: smithay::backend::renderer::element::Id::new(),
                mapped: false,
            },
        );
        // A window arriving here with a `StackEntry::Window` slot is one
        // that changed its mind: it mapped client-decorated, and a
        // `_MOTIF_WM_HINTS` rewrite has since made `wm-core` decide it
        // wants a frame after all (`BackendEvent::ChromeChanged`). The
        // frame's slot replaces the window's *in place*, so the window
        // does not jump to the front of the stack merely for growing a
        // titlebar; leaving both would draw and hit-test the client
        // twice, at two different depths.
        replace_stack_entry(&mut self.stacking, StackEntry::Window(window), StackEntry::Frame(frame));
        self.sync_managed_scene_index(window);
        frame
    }

    fn destroy_decoration(&mut self, frame: Self::FrameId) {
        // The cursor entry goes with the frame (both removal verbs do
        // this, matching `wm-x11`'s `frame_cursor.remove`): a stale
        // entry would re-apply a resize cursor to whatever frame the id
        // is ever reused for.
        self.frame_cursors.remove(&frame);
        if let Some(record) = self.frames.remove(&frame) {
            self.stacking.retain(|entry| !matches!(entry, StackEntry::Frame(f) if *f == frame));
            self.sync_managed_scene_index(record.window);
            self.mark_damaged();
        }
    }

    /// The other half of the chrome-changed round trip: the client now
    /// draws its own titlebar, so the frame goes but the window stays —
    /// mapped, focusable, and at the same depth it already had.
    ///
    /// The trait's default (plain `destroy_decoration`) is *nearly*
    /// right here and would still be wrong twice. There is no
    /// reparenting to undo — a frame in this ledger owns no client
    /// window, which is exactly why the default was written for
    /// backends like this one — but the frame's stacking slot is the
    /// window's only slot, and dropping it would leave a mapped,
    /// managed window with no entry in `stacking` at all: invisible to
    /// the renderer's frame band and to the hit-test's, and so
    /// unclickable rather than merely unframed. Reusing the slot also
    /// keeps the z-order the user arranged, which a push-to-top would
    /// silently rearrange in front of them.
    ///
    /// The client's own rect is deliberately left where it is. On X11
    /// this verb reparents the client to the root at its current
    /// absolute position, and the chrome simply vanishes from around
    /// it; matching that keeps the window under the pointer that was
    /// just interacting with it.
    fn release_decoration(&mut self, window: Self::WindowId, frame: Self::FrameId) {
        self.frame_cursors.remove(&frame);
        let Some(record) = self.frames.remove(&frame) else { return };
        replace_stack_entry(&mut self.stacking, StackEntry::Frame(frame), StackEntry::Window(window));
        if !record.mapped {
            // A framed window is hidden by unmapping its frame while its
            // client record stays mapped. Once the frame is gone the
            // client slot owns visibility, so transfer that hidden state;
            // a later workspace return can then enter `map_frameless`.
            if let Some(window) = self.windows.get_mut(&window) {
                window.mapped = false;
            }
        }
        self.sync_managed_scene_index(window);
        self.mark_damaged();
    }

    fn paint_decoration(&mut self, frame: Self::FrameId, surface: &DecorationSurface) {
        if let Some(record) = self.frames.get_mut(&frame) {
            let mut previous = std::mem::take(&mut record.parts).into_iter();
            let mut imported = Vec::with_capacity(surface.parts.len());
            for part in &surface.parts {
                let size = Size::new(part.buffer.width, part.buffer.height);
                if size.w == 0 || size.h == 0 {
                    continue;
                }
                let old = previous.next();
                let buffer = match old {
                    Some(mut old) if old.offset == part.offset && old.size == size => {
                        let pixels = &part.buffer.pixels;
                        let mut render = old.buffer.render();
                        let _ = render.draw(|memory| {
                            if memory.len() == pixels.len() {
                                memory.copy_from_slice(pixels);
                            }
                            Ok::<_, std::convert::Infallible>(vec![SmithayRect::<i32, Buffer>::from_size(
                                (size.w as i32, size.h as i32).into(),
                            )])
                        });
                        drop(render);
                        old.buffer
                    }
                    _ => match import_buffer(&part.buffer, true) {
                        Some(buffer) => buffer,
                        None => continue,
                    },
                };
                imported.push(FramePart { offset: part.offset, size, buffer });
            }
            record.parts = imported;
            self.mark_damaged();
        }
    }

    /// Records which cursor this frame wants shown, for the renderer to
    /// pick up: the pointer image is composited by the renderer from
    /// the input module's pointer state, so indicating a resize edge
    /// means swapping that cursor image, not flagging a window the way
    /// the X11 backend's `change_window_attributes` does. The swap
    /// itself happens in `push_cursor_elements`, which consults this
    /// map whenever the pointer is over the frame's chrome (via
    /// `input::pointer_subject`); recording is all that can happen
    /// here, for the reason every deferred field on the ledger states —
    /// a `Backend` verb runs inside the `WindowManager`'s `&mut self`
    /// and can reach nothing on `Compositor`.
    fn set_frame_cursor(&mut self, frame: Self::FrameId, edge: Option<ResizeEdge>) {
        let changed = match edge {
            Some(edge) => self.frame_cursors.insert(frame, edge) != Some(edge),
            None => self.frame_cursors.remove(&frame).is_some(),
        };
        // Damage only on a real change: this is called on every motion
        // over chrome, and the pointer's own movement already damages
        // the scene — but a change can also arrive with the pointer
        // still (a keybinding resize ending under it), and without the
        // flag the old cursor would linger until something else moved.
        if changed {
            self.mark_damaged();
        }
    }

    // -- geometry / visibility --------------------------------------------

    /// Note what `geometry.size` does and does not do here. It is the
    /// frame's *input* extent — `input.rs`'s `hit_at` tests points
    /// against exactly this rect — but not its visible one: there is no
    /// server-side frame window to clip anything, so `renderer.rs`
    /// composites the decoration buffer at `geometry.pos` at the
    /// buffer's own size and never reads this size at all. A caller that
    /// changes a frame's size without painting a buffer to match has
    /// moved the clickable rect out from under an unchanged picture.
    /// (That is the whole windowshade bug: `wm-core` shrank the frame
    /// for a shaded window but painted the full-height decoration into
    /// it, which X11's real frame window happened to clip and this
    /// backend does not — see `wm-core`'s `shaded_paint_inputs`.)
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
        let delta = Point::new(geometry.pos.x - record.geometry.pos.x, geometry.pos.y - record.geometry.pos.y);
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
        self.mark_damaged();
    }

    fn resize_client(&mut self, window: Self::WindowId, size: Size) {
        // The factor is read before the mutable borrow below — it needs
        // the whole ledger (`window_surface_scale` consults the monitor
        // list), and the answer cannot change between these two lines.
        let factor = match self.windows.get(&window) {
            Some(record) => self.window_surface_scale(record),
            None => return,
        };
        let Some(record) = self.windows.get_mut(&window) else {
            return;
        };
        let resized = record.content.size != size;
        record.content.size = size;
        let mut configure_owed = false;
        let mut popup_root = None;
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                if toplevel.alive() {
                    if resized {
                        popup_root = Some(toplevel.wl_surface().clone());
                    }
                    // The configure/ack/commit dance is how a Wayland
                    // client learns its size — there is no server-side
                    // resize to perform. `send_pending_configure`
                    // dedups: a size the client already has produces no
                    // event, which matters during interactive resize
                    // where this is called per motion event.
                    // Back into the client's own logical pixels, by the
                    // factor that client itself committed — the same one
                    // `xdg.rs` measured the buffer with and the renderer
                    // draws it at. This is the return leg of a round
                    // trip, and the two legs have to use one number or
                    // the window never stops growing: the ledger holds a
                    // 2x client's 600px buffer, a configure that calls
                    // 600 *logical* asks GTK for a 1200px buffer, the
                    // commit path reports 1200, `wm-core` reflows around
                    // it and resizes the client to 1200, and so on. The
                    // reflow at map time is enough to start it, so the
                    // window is already unbounded by the time it first
                    // appears.
                    //
                    // The session-wide `ui_scale` used to stand in
                    // here, and the outputs now advertising that scale
                    // does not make it right again: the advertisement
                    // is an invitation a client may decline (Xwayland
                    // always does), so the desktop's idea of a scale
                    // still says nothing about what any one client
                    // actually drew.
                    let logical = (
                        crate::xdg::physical_to_logical(size.w as i32, factor),
                        crate::xdg::physical_to_logical(size.h as i32, factor),
                    );
                    toplevel.with_pending_state(|state| {
                        state.size = Some(logical.into());
                    });
                    // Staged, not sent: the configure goes out at the
                    // end of the pass carrying every change this one
                    // made, so a resize that accompanies a state change
                    // can never reach the client ahead of the state (a
                    // fullscreen-sized window still labelled windowed is
                    // what taught us — see `xdg::flush_configures`).
                    configure_owed = true;
                    // What the client will commit if it obeys: the
                    // logical ask times its own factor — NOT `size`,
                    // which the round trip through logical units may
                    // have moved by a pixel (a certainty at fractional
                    // factors). See `WindowRecord::recent_asks`.
                    let expected = Size::new(
                        crate::xdg::scale_length(logical.0, factor) as u32,
                        crate::xdg::scale_length(logical.1, factor) as u32,
                    );
                    record.recent_asks.push_back(expected);
                    while record.recent_asks.len() > 8 {
                        record.recent_asks.pop_front();
                    }
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
        if configure_owed {
            self.note_configure(window);
        }
        if let Some(root) = popup_root {
            // Interactive resize may call this once per pointer motion.
            // One root entry is enough to dismiss its whole popup tree,
            // and retaining the Vec's allocation keeps this edge free of
            // allocator churn after the first resize.
            self.note_popup_parent_resize(root);
        }
        self.mark_damaged();
    }

    fn configure_unmanaged(&mut self, window: Self::WindowId, geometry: Rect) {
        let Some(record) = self.windows.get_mut(&window) else {
            return;
        };
        record.content = geometry;
        let mut configure_owed = false;
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
                // isn't a concept the protocol lets us grant. In the
                // client's own logical units, by the same factor as
                // `resize_client` — the ledger's size is physical and
                // a scaled client asked in its own pixels.
                if toplevel.alive() {
                    let factor = crate::xdg::committed_surface_scale(toplevel.wl_surface());
                    toplevel.with_pending_state(|state| {
                        state.size = Some(
                            (
                                crate::xdg::physical_to_logical(geometry.size.w as i32, factor),
                                crate::xdg::physical_to_logical(geometry.size.h as i32, factor),
                            )
                                .into(),
                        );
                    });
                    configure_owed = true;
                }
            }
        }
        let mapped = record.mapped;
        if configure_owed {
            self.note_configure(window);
        }
        if mapped {
            self.mark_damaged();
        }
    }

    fn map_frame(&mut self, frame: Self::FrameId) {
        if let Some(record) = self.frames.get_mut(&frame) {
            let changed = !record.mapped;
            if changed {
                record.mapped = true;
            }
            let window = record.window;
            self.sync_managed_scene_index(window);
            if changed {
                self.idle_policy_dirty = true;
                self.mark_damaged();
            }
        }
    }

    fn unmap_frame(&mut self, frame: Self::FrameId) {
        if let Some(record) = self.frames.get_mut(&frame) {
            let changed = record.mapped;
            if changed {
                record.mapped = false;
            }
            let window = record.window;
            self.sync_managed_scene_index(window);
            if changed {
                self.idle_policy_dirty = true;
                self.mark_damaged();
            }
        }
    }

    /// Shows a managed window that has no frame of its own.
    ///
    /// The trait's default forwards to `map_unmanaged`, and taking it
    /// would be a real bug on this backend rather than a shortcut:
    /// `map_unmanaged` records the ledger entry's `window_type` as
    /// `Unmanaged`, and both the renderer and the hit-test read that
    /// field to mean "override-redirect — draw and click me above
    /// everything, outside `stacking` entirely". That is right for an
    /// XWayland tooltip and wrong for Edge, which would then float over
    /// the dock and every other window for as long as it was open. The
    /// window type stays whatever it is; only the frame is missing.
    ///
    /// The stacking slot is what actually makes such a window visible
    /// (see `StackEntry::Window`). A window mapping for the first time
    /// gets one on top, which is where `create_decoration` puts a fresh
    /// frame; a window already holding one — remapped after a workspace
    /// switch or a deminiaturize — keeps its depth, because coming back
    /// from another workspace is not a raise.
    fn map_frameless(&mut self, window: Self::WindowId) {
        let Some(record) = self.windows.get_mut(&window) else {
            return;
        };
        let changed = !record.mapped;
        record.mapped = true;
        ensure_stack_entry(&mut self.stacking, StackEntry::Window(window));
        self.sync_managed_scene_index(window);
        if changed {
            self.idle_policy_dirty = true;
            self.mark_damaged();
        }
    }

    /// Hides one again. The slot stays: `unmap_frame` leaves a hidden
    /// window's frame in `stacking` for the same reason, so that a
    /// workspace switched away from and back to comes back in the order
    /// it was left rather than flattened into map order.
    fn unmap_frameless(&mut self, window: Self::WindowId) {
        if let Some(record) = self.windows.get_mut(&window) {
            let changed = record.mapped;
            if changed {
                record.mapped = false;
            }
            self.sync_managed_scene_index(window);
            if changed {
                self.idle_policy_dirty = true;
                self.mark_damaged();
            }
        }
    }

    fn set_client_mapped(&mut self, window: Self::WindowId, mapped: bool) {
        // Shading, purely compositor-side: the renderer stops drawing
        // the content while the frame stays. The client never learns —
        // no unmap event exists to suppress (unlike `wm-x11`, which has
        // to swallow the UnmapNotify its own request generates), and
        // Wayland clients keep their buffers, so the unshade path needs
        // no repaint coaxing either.
        let Some(record) = self.windows.get_mut(&window) else { return };
        let changed = record.mapped != mapped;
        record.mapped = mapped;
        self.sync_managed_scene_index(window);
        if changed {
            self.idle_policy_dirty = true;
            self.mark_damaged();
        }
    }

    // -- stacking ---------------------------------------------------------

    fn raise(&mut self, frame: Self::FrameId) {
        if raise_stack_entry(&mut self.stacking, StackEntry::Frame(frame)) {
            self.mark_damaged();
            self.stacking_dirty = true;
        }
    }

    /// The same raise for a window whose client draws its own chrome:
    /// it holds its own slot in the frame band rather than borrowing a
    /// frame's (see `StackEntry::Window`), so raising it is raising
    /// that slot.
    ///
    /// Deliberately the identical call to `raise` above, against a
    /// different spelling of the same place in the same vector. A
    /// frameless window that restacked by its own rules would be a
    /// second stacking path to keep in agreement with the first, and
    /// the reason this verb exists at all is that `wm-core`'s raise
    /// sites all named a `FrameId` and silently did nothing for these
    /// windows — clicking one focused it without bringing it forward,
    /// for the whole life of the window.
    fn raise_frameless(&mut self, window: Self::WindowId) {
        if raise_stack_entry(&mut self.stacking, StackEntry::Window(window)) {
            self.mark_damaged();
            self.stacking_dirty = true;
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
        let before = self.stacking.clone();
        self.stacking.retain(|entry| !matches!(entry, StackEntry::Frame(f) if order_back_to_front.contains(f)));
        self.stacking.extend(listed);
        self.mark_damaged();
        // Compared rather than assumed: `restack` is called from the
        // workspace-switch and Alt-Tab paths on every pass they run,
        // and a "dirty" that meant "mentioned" would grab the X server
        // once a frame (see `WaylandBackend::stacking_dirty`).
        if self.stacking != before {
            self.stacking_dirty = true;
        }
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
        self.pending_focus = Some(crate::state::FocusIntent::Window(window));
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
                            message: "killed by the window manager (close request unanswered)".to_string(),
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

    /// Records that the pointer belongs to a drag until further notice.
    /// The input module then routes every motion and button to the
    /// window manager instead of the surface under the cursor, and the
    /// seat takes the pointer off the client that had it (see
    /// `DragGrab`, and `input::apply_pointer_grab_change` for the
    /// half that has to reach the seat).
    ///
    /// This used to return a no-op handle, on the reasoning that a
    /// compositor sees every pointer event anyway so a drag's routing
    /// needs nothing declared. That held for as long as every managed
    /// window wore one of our frames — the drag runs over our own
    /// surface, so its release comes back to `wm-core` whether or not
    /// anything was grabbed — and stopped holding the day a window
    /// could have no frame at all.
    fn grab_pointer_for_drag(&mut self) -> DragHandle {
        // From the ledger's shared id counter, which starts at 1: a
        // handle is never reused, and never 0, so the `DragHandle(0)`
        // that a backend without grabs hands back can never be mistaken
        // for one of these.
        let handle = DragHandle(self.alloc_id());
        // Replaces rather than refuses. A second grab means a drag
        // started while another was somehow still recorded, and the
        // newer one is the one the user is making; keeping the older
        // would leave the pointer answering to a gesture that is over.
        self.pointer_grab = Some(DragGrab::new(handle));
        self.pending_pointer_grab = Some(PointerGrabChange::Taken);
        handle
    }

    /// Ends the drag `handle` names, and only that one.
    ///
    /// A handle that names no current grab is ignored deliberately,
    /// which is what the token is for: the shell hands a stale one back
    /// when it supersedes a press whose release never arrived
    /// (`LaunchDock::handle_click`), and `input.rs` can have already
    /// reclaimed the grab from a drag that outlived its buttons. In
    /// both cases the grab this names is gone and the one in flight, if
    /// any, belongs to somebody else.
    fn ungrab_pointer(&mut self, handle: DragHandle) {
        if self.pointer_grab.as_ref().is_some_and(|grab| grab.holds(handle)) {
            self.end_pointer_grab();
        }
    }

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

    fn set_key_release(&mut self, combo: KeyCombo, enabled: bool) {
        set_combo_membership(&mut self.release_combos, combo, enabled);
    }

    fn set_key_locked(&mut self, combo: KeyCombo, enabled: bool) {
        set_combo_membership(&mut self.locked_combos, combo, enabled);
    }

    fn set_key_repeating(&mut self, combo: KeyCombo, enabled: bool) {
        set_combo_membership(&mut self.repeating_combos, combo, enabled);
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
        self.mark_damaged();
    }

    // -- EWMH-shaped policy reads/acts ------------------------------------
    // The publish_* family buffers into the ledger's `EwmhLedger`, and
    // `Compositor::dispatch_pending` flushes it to the XWayland root
    // through the compositor's own X11 connection (see `xewmh.rs`) —
    // the record-now/act-later detour every deferred field on this
    // ledger takes, plus one more reason particular to these: the
    // connection may simply not exist yet (XWayland readiness is
    // asynchronous) or ever (it failed to start), and a verb must not
    // care. Wayland-native taskbars get the same information through
    // the wlr foreign-toplevel list in `protocols.rs`.

    fn publish_client_list(&mut self, clients: &[Self::WindowId]) {
        self.ewmh.note_client_list(clients);
    }

    fn publish_active_window(&mut self, window: Option<Self::WindowId>) {
        self.ewmh.note_active_window(window);
        // `None` is `wm-core` saying "no window is focused any more"
        // (miniaturize, the focused window closing, a workspace switch
        // away) with no `set_input_focus` to follow — every one of its
        // call sites is a focus *loss*, not a move. The seat must
        // follow it: left alone, keyboard focus stayed parked on the
        // hidden surface and its `Activated` state stayed set, so the
        // eventual restore of the very same window changed nothing on
        // the wire — no `wl_keyboard.enter`, no configure — and a
        // client that had minimized *itself* (Edge's own Minimize
        // item sends `xdg_toplevel.set_minimized`) kept believing it
        // was minimized and discarded all input until a click
        // elsewhere and back forced a real focus change. Deferred via
        // the same detour `set_input_focus` takes; see
        // [`crate::state::FocusIntent`]. `Some(_)` stays ledger-only
        // because `focus_client` always pairs it with
        // `set_input_focus`, which records the same intent.
        if window.is_none() {
            self.pending_focus = Some(crate::state::FocusIntent::Nothing);
        }
    }

    fn publish_workspaces(&mut self, count: usize, current: usize) {
        self.ewmh.note_workspaces(count, current);
        // The same row, for native Wayland clients. `note_workspaces`
        // is already a "changed, not mentioned" ledger; this flag rides
        // the same call so the two protocols cannot describe different
        // desktops, and `workspace::refresh` does its own comparison
        // before sending anything.
        self.workspaces_dirty = true;
    }

    fn publish_workarea(&mut self, area: Rect, workspace_count: usize) {
        self.ewmh.note_workarea(area, workspace_count);
    }

    fn publish_window_desktop(&mut self, window: Self::WindowId, desktop: usize) {
        self.ewmh.note_window_desktop(window, desktop);
    }

    fn publish_frame_extents(&mut self, window: Self::WindowId, left: u32, right: u32, top: u32, bottom: u32) {
        // Buffered for every managed window; the flush drops the
        // entries whose window turns out not to be X11 — an xdg
        // toplevel has no property to carry the answer, and no
        // Wayland protocol says "frame extents" at all.
        self.ewmh.note_frame_extents(window, left, right, top, bottom);
    }

    /// Pushes `wm-core`'s authoritative state flags back onto the
    /// client — the half of the `_NET_WM_STATE` round trip that was
    /// missing: requests flowed in (`NetStateRequested`), but a client
    /// that asked to maximize was never *told* it is maximized, so an
    /// X11 app that draws differently when maximized drew wrong and
    /// its `_NET_WM_STATE` read stale to anything that looked, and a
    /// Wayland client never got the Maximized/Fullscreen toplevel
    /// states its toolkit styles from (squared corners, no shadow).
    ///
    /// Unlike its root-property siblings above this acts inline, not
    /// through the EWMH ledger: both targets are client handles the
    /// ledger already owns — no `Compositor` access needed — and for
    /// X11 the property write must go through smithay's `X11Surface`
    /// setters, which rewrite `_NET_WM_STATE` from their own cached
    /// set (a second writer on our own connection would race them).
    ///
    /// The vocabulary mapping, stated honestly:
    /// * `wm-core` maximizes per axis; X11's smithay setter and xdg's
    ///   Maximized state are both single both-axes concepts, so
    ///   both-axes-set = maximized (the conventional reading) and a
    ///   single-axis maximize publishes as not-maximized rather than
    ///   half-claiming a state the protocol cannot spell.
    /// * `hidden` (miniaturized) maps to X11's `_NET_WM_STATE_HIDDEN`
    ///   via `set_suspended`, and to xdg's v6 Suspended state. An
    ///   earlier version withheld Suspended on the reasoning that
    ///   "stop rendering" was stronger than miniaturized wants — but
    ///   a miniaturized window renders only its icon-tile preview,
    ///   captured *before* the hide, so every frame the client keeps
    ///   producing is thrown away; Suspended is the protocol's exact
    ///   word for that. It is also half of how a self-minimized
    ///   Chromium learns it is visible again: the unset on restore is
    ///   a state change, so a configure actually goes out. Smithay
    ///   filters the state away for clients whose bound xdg_toplevel
    ///   predates v6, so old toolkits never see a word their version
    ///   cannot spell.
    /// * `shaded` has no vocabulary on either side (no smithay setter,
    ///   no xdg state) and is deliberately unpublished — which is why
    ///   `xewmh.rs` leaves `_NET_WM_STATE_SHADED` out of
    ///   `_NET_SUPPORTED`.
    fn publish_net_state(&mut self, window: Self::WindowId, state: wm_core::NetStateSnapshot) {
        let wm_core::NetStateSnapshot {
            fullscreen,
            maximized_horizontally: max_h,
            maximized_vertically: max_v,
            shaded,
            hidden,
            modal,
        } = state;
        let _ = shaded;
        self.ewmh.note_window_modal(window, modal);
        let Some(record) = self.windows.get_mut(&window) else {
            return;
        };
        record.modal = modal;
        let fullscreen_changed = record.fullscreen != fullscreen;
        record.fullscreen = fullscreen;
        let maximized = both_axes_maximized(max_h, max_v);
        let mut configure_owed = false;
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                if toplevel.alive() {
                    toplevel.with_pending_state(|state| {
                        if maximized {
                            state.states.set(XdgToplevelState::Maximized);
                        } else {
                            state.states.unset(XdgToplevelState::Maximized);
                        }
                        if fullscreen {
                            state.states.set(XdgToplevelState::Fullscreen);
                        } else {
                            state.states.unset(XdgToplevelState::Fullscreen);
                        }
                        if hidden {
                            state.states.set(XdgToplevelState::Suspended);
                        } else {
                            state.states.unset(XdgToplevelState::Suspended);
                        }
                    });
                    // Staged, not sent. The dedup that used to make
                    // this line cheap still applies — it now happens in
                    // `xdg::flush_configures`, which sends one configure
                    // per toplevel per pass — and deferring is what
                    // makes the state and the geometry `wm-core` set on
                    // either side of this call reach the client as one
                    // settled answer instead of two contradictory ones.
                    configure_owed = true;
                }
            }
            ManagedSurface::X11(surface) => {
                // Smithay's setters own the `_NET_WM_STATE` property on
                // the client window and dedup internally (no property
                // rewrite when nothing changed). Errors are logged and
                // dropped like every other X11 call on this backend:
                // the client may be mid-teardown, and state styling is
                // not worth failing anything over.
                if surface.alive() {
                    // `set_suspended` publishes EWMH hidden, but
                    // ICCCM clients also read `WM_STATE` to learn
                    // whether they are iconic. `X11Surface::set_mapped`
                    // would write it, but its real X unmap makes
                    // smithay dismantle the managed surface. Queue the
                    // property-only counterpart through our ordinary
                    // XWayland EWMH connection instead.
                    self.ewmh.note_window_iconic(window, hidden);
                    if let Err(error) = surface.set_fullscreen(fullscreen) {
                        tracing::warn!(?error, ?window, "X11 set_fullscreen failed");
                    }
                    if let Err(error) = surface.set_maximized(maximized) {
                        tracing::warn!(?error, ?window, "X11 set_maximized failed");
                    }
                    if let Err(error) = surface.set_suspended(hidden) {
                        tracing::warn!(?error, ?window, "X11 set_suspended failed");
                    }
                }
            }
        }
        if configure_owed {
            self.note_configure(window);
        }
        if fullscreen_changed {
            // The client geometry usually changes beside this state,
            // but band occlusion is independently visible policy. Its
            // own damage edge keeps a same-sized fullscreen transition
            // (and its inverse) from waiting for unrelated damage.
            self.mark_damaged();
        }
    }

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
                    Some(WmWindowType::Normal) | Some(WmWindowType::Utility) | None => WindowType::Normal,
                    // Menus, docks, tooltips, splashes, notifications:
                    // draw their own chrome, place themselves — same
                    // bucket `wm-x11` sorts these atoms into.
                    Some(_) => WindowType::Unmanaged,
                }
            }
        }
    }

    fn window_parent(&self, window: Self::WindowId) -> Option<Self::WindowId> {
        let record = self.windows.get(&window)?;
        match &record.surface {
            ManagedSurface::Xdg(toplevel) => {
                let _ = toplevel;
                record.parent
            }
            ManagedSurface::X11(surface) => {
                let parent = surface.is_transient_for()?;
                // smithay reports the parent as an X window id; the
                // ledger is keyed by our own ids, so find the record
                // wearing that id.
                self.windows
                    .iter()
                    .find(|(_, other)| match &other.surface {
                        ManagedSurface::X11(other) => other.window_id() == parent,
                        ManagedSurface::Xdg(_) => false,
                    })
                    .map(|(id, _)| *id)
            }
        }
    }

    fn window_is_modal(&self, window: Self::WindowId) -> bool {
        self.windows.get(&window).is_some_and(|record| record.modal)
    }

    fn set_keyboard_config(&mut self, config: wm_core::KeyboardConfig) {
        // Staged, not applied: the seat lives on `Compositor` and this
        // verb only ever sees the ledger. `apply_pending_keyboard`
        // installs it at the top of the next dispatch pass.
        self.pending_keyboard = Some(config);
    }

    fn set_pointer_config(&mut self, config: wm_core::PointerConfig) {
        self.pointer_config = config.clone();
        self.pending_pointer = Some(config);
    }

    fn set_decoration_rules(&mut self, rules: wm_core::DecorationRules) {
        self.decoration_rules = rules;
    }

    fn client_draws_own_chrome(&self, window: Self::WindowId) -> bool {
        let Some(record) = self.windows.get(&window) else {
            return false;
        };
        match &record.surface {
            // `_MOTIF_WM_HINTS` with the decorations bit present and
            // clear: the client has *said* it draws its own titlebar.
            // Smithay parses the property on every PropertyNotify and
            // answers from the parsed copy, so this is a field read, not
            // a round trip, and `property_notify` in `xwayland.rs` turns
            // the same edge into `BackendEvent::ChromeChanged`.
            //
            // Read smithay's method as `is_client_side_decorated`: it
            // answers true only when the decorations field is present
            // and ZERO — despite a name that suggests "wants a frame",
            // it means the opposite, and `wm-x11`'s own Motif read
            // (`flags bit set && decorations == 0`) agrees with this
            // orientation, not the negation. A `!` here once inverted
            // the whole table: Spotify, which asks for MWM_DECOR_ALL,
            // was stripped of its frame, controls and resize bars —
            // unnoticed for days because LibreOffice and Edge run
            // native Wayland and never take this arm.
            ManagedSurface::X11(surface) => {
                // A `[decorations]` rule reaches this leg too. It did
                // not before: the override was consulted only on the
                // native Wayland arm, and `record.app_id` was never
                // populated for an X11 surface at all, so a user told
                // to name their application in the config got silence
                // from every XWayland window on the desk.
                let identity = record.app_id.clone().or_else(|| x11_identity(surface));
                if let Some(force_server_side) = self.decoration_rules.decision_for(identity.as_deref()) {
                    return !force_server_side;
                }
                // `_MOTIF_WM_HINTS` with the decorations bit present and
                // clear: the client has *said* it draws its own
                // titlebar. Read through `wm-core`'s shared reader, so
                // this leg, the X11 session's window manager, and any
                // future one cannot drift apart on what the property
                // means — they had, over the property's minimum length.
                //
                // Read smithay's method as `is_client_side_decorated`:
                // it answers true only when the decorations field is
                // present and ZERO — despite a name that suggests
                // "wants a frame", it means the opposite, and
                // `wm-x11`'s own Motif read agrees with this
                // orientation, not the negation. A `!` here once
                // inverted the whole table: Spotify, which asks for
                // MWM_DECOR_ALL, was stripped of its frame, controls
                // and resize bars — unnoticed for days because
                // LibreOffice and Edge run native Wayland and never
                // take this arm.
                surface.is_decorated()
            }
            // Everything a native Wayland toplevel has told us, on
            // either decoration protocol, with a `[decorations]`
            // override above it. The reasoning — including why silence
            // is framed and why the KDE protocol has to be advertised
            // at all — lives in `crate::decoration`.
            ManagedSurface::Xdg(_) => self.xdg_client_draws_own_chrome(record),
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
        let Some(record) = self.windows.get_mut(&window) else { return };
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
        self.mark_damaged();
        if kind == WindowType::Unmanaged {
            self.scene_index.mark_unmanaged(window);
        } else {
            self.sync_managed_scene_index(window);
        }
    }

    fn position_client(&mut self, window: Self::WindowId, pos: Point) {
        // `pos` is frame-local (the chrome offset — see the trait doc:
        // this only fires when that offset changes, fullscreen in/out).
        // Translate to global against the owning frame.
        //
        // A window with no frame is the exception, and it is not an
        // error case: `wm-core`'s reflow for a client-decorated window
        // calls this with the content rect's *root* position, because
        // there is no frame for an offset to be relative to. Returning
        // early here (which this did, back when every managed window
        // had a frame) is what would pin such a window wherever it
        // first mapped — no move, no workspace placement, no
        // fullscreen. Frame-local coordinates against no frame ARE
        // global ones, which is the same identity `input.rs` relies on
        // for root-relative shell clicks.
        let frame_pos = self
            .frames
            .values()
            .find(|record| record.window == window)
            .map(|record| record.geometry.pos)
            .unwrap_or(Point::new(0, 0));
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
        self.mark_damaged();
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

    fn grab_keyboard(&mut self) {
        <Self as Backend>::grab_keyboard(self);
    }

    fn ungrab_keyboard(&mut self) {
        <Self as Backend>::ungrab_keyboard(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Everything else in this file needs a live client on a socket;
    // the state-flag projection is the pure decision underneath
    // `publish_net_state`, and the one a slipped `||` would silently
    // break in both protocols at once.

    #[test]
    fn only_both_axes_read_as_maximized() {
        assert!(both_axes_maximized(true, true));
        assert!(!both_axes_maximized(true, false));
        assert!(!both_axes_maximized(false, true));
        assert!(!both_axes_maximized(false, false));
    }
}
