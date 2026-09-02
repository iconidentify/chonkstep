//! Wayland protocol handlers on [`Compositor`]: wl_compositor/wl_shm/
//! wl_seat/wl_output plumbing, the two selection protocols
//! (wl_data_device and primary selection, whose handlers here are the
//! Wayland end of the XWayland clipboard bridge in `xwayland.rs`), plus
//! the xdg-shell and xdg-decoration mapping that turns client requests
//! into the exact `BackendEvent` shapes `wm-core` already speaks (read
//! alongside `wm-x11`'s `translate_event`/`translate_client_message` —
//! every event queued here mirrors a translation there).
//!
//! The one deliberate divergence from X11's lifecycle: xdg toplevels
//! have no MapRequest of their own. A toplevel "maps" by committing its
//! first buffer after the configure handshake, so `commit` below is
//! where `BackendEvent::MapRequest` is synthesized — and a later
//! null-buffer commit is the protocol's unmap, translated to
//! `BackendEvent::Unmapped` the same way `wm-x11` translates
//! UnmapNotify. `wm-core` cannot tell the difference, by construction.

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, Ordering};

use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::desktop::PopupKind;
use smithay::input::pointer::CursorImageStatus;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_output;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Resource};
use smithay::utils::Serial;
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    get_parent, with_states, CompositorClientState, CompositorHandler, CompositorState,
    SurfaceAttributes,
};
use smithay::wayland::output::OutputHandler;
use smithay::wayland::selection::data_device::{
    set_data_device_focus, ClientDndGrabHandler, DataDeviceHandler, DataDeviceState,
    ServerDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    set_primary_focus, PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};
use smithay::wayland::shell::xdg::decoration::XdgDecorationHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface, XdgShellHandler,
    XdgShellState, XdgToplevelSurfaceData,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::xwayland::XWaylandClientData;
use smithay::{
    delegate_compositor, delegate_data_device, delegate_output, delegate_primary_selection,
    delegate_seat, delegate_shm, delegate_xdg_decoration, delegate_xdg_shell,
};

use wm_core::{BackendEvent, NetState, NetStateAction};
use wm_theme_api::{Point, Rect, ResizeEdge, Size};

use crate::state::{ClientState, Compositor, ManagedSurface, WindowRecord, WlFrameId, WlWindowId};

type WmEvent = BackendEvent<WlWindowId, WlFrameId>;

/// Per-surface "MapRequest already emitted" marker, parked on the
/// surface's user-data map so its lifetime is exactly the surface's —
/// no cleanup path can forget it. Tracks the mapped/unmapped EDGE:
/// `wm-x11` gets that edge from the server (MapRequest/UnmapNotify),
/// here it is derived from buffer-presence transitions across commits.
#[derive(Default)]
struct MappedMarker(AtomicBool);

fn mapped_marker(surface: &WlSurface) -> bool {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(MappedMarker::default);
        states.data_map.get::<MappedMarker>().unwrap().0.load(Ordering::Relaxed)
    })
}

fn set_mapped_marker(surface: &WlSurface, value: bool) {
    with_states(surface, |states| {
        states.data_map.insert_if_missing_threadsafe(MappedMarker::default);
        states.data_map.get::<MappedMarker>().unwrap().0.store(value, Ordering::Relaxed);
    });
}

/// How many device pixels one of this surface's own pixels is worth:
/// the `wl_surface.set_buffer_scale` it last committed, floored at 1.
///
/// There is deliberately no session-wide answer to that question. The
/// outputs advertise the session's scale
/// (`state.rs::advertise_scale`), but that is an invitation, not a
/// fact about any surface: a native toolkit accepts it and commits 2x
/// buffers, while the Xwayland server beside it ignores it and keeps
/// committing 1x ones — and a client is free to change its answer
/// mid-session. What a surface's pixels are actually worth is whatever
/// that surface last committed, so that is the only number the ledger,
/// the renderer and the hit-test may multiply by. Asking the session
/// instead would get every 1x client wrong.
///
/// The number is read from the surface's double-buffered
/// [`SurfaceAttributes`], whose `current()` half is by construction
/// whatever the client's last `wl_surface.commit` made real — the
/// pending half is a request that has not happened yet. `smithay`'s
/// `RendererSurfaceState` holds a copy of the same value, but only from
/// the first buffer attach onward, and this is asked for a window's
/// geometry before a client has ever attached one; there the attribute
/// still answers, with smithay's default of 1.
pub(crate) fn committed_buffer_scale(surface: &WlSurface) -> i32 {
    usable_buffer_scale(with_states(surface, |states| {
        let mut guard = states.cached_state.get::<SurfaceAttributes>();
        guard.current().buffer_scale
    }))
}

/// Floors a buffer scale at 1. The protocol forbids anything smaller
/// and smithay refuses such a request before it is ever stored, so this
/// is not expected to fire — it is here because the value is a
/// *multiplier* on every size and offset that crosses into the ledger,
/// and a zero reaching that multiplication would not fail anywhere near
/// itself. `committed_content_size` checks the client's geometry for
/// positive extents *before* converting it, so a zero factor sails past
/// that guard and reports a 0x0 window as the size the client asked
/// for: `wm-core` lays out a frame around nothing, configures the
/// client to nothing, and no log line mentions a scale.
const fn usable_buffer_scale(scale: i32) -> i32 {
    if scale < 1 {
        1
    } else {
        scale
    }
}

/// Converts one surface-local length into the device pixels this
/// compositor stores and draws in: the length times the surface's
/// [`committed_surface_scale`], rounded to the nearest pixel — an
/// integer factor makes the rounding exact, so the whole-number cases
/// behave precisely as the old integer multiply did. Rounding is what
/// the
/// fractional-scale protocol itself specifies for buffer sizes, so
/// measuring a client's commit with the same rule is what makes the
/// ledger's rectangle land on the buffer the client actually drew.
pub(crate) fn scale_length(logical: i32, factor: f64) -> i32 {
    (logical as f64 * usable_surface_scale(factor)).round() as i32
}

/// The return leg: device pixels back into one surface's own logical
/// units, rounded. Every configure this compositor sends travels
/// through here, and it must be the exact inverse discipline of
/// [`scale_length`] or the configure/commit round trip drifts by a
/// pixel per pass (see `resize_client`'s unbounded-growth story).
pub(crate) fn physical_to_logical(physical: i32, factor: f64) -> i32 {
    (physical as f64 / usable_surface_scale(factor)).round() as i32
}

/// Floors a fractional factor at 1/8 — far below any real scale, but a
/// zero or negative factor is a divisor and a multiplier on everything
/// crossing the ledger boundary, and it must not annihilate a window
/// (same reasoning as [`usable_buffer_scale`], continuous edition).
fn usable_surface_scale(factor: f64) -> f64 {
    if factor.is_finite() && factor >= 0.125 {
        factor
    } else {
        1.0
    }
}

/// The granularity of the fractional-scale protocol: scales travel the
/// wire as multiples of 1/120, so a factor recovered from a buffer/
/// viewport ratio is snapped onto that grid before anyone multiplies by
/// it. 120 is divisible by 2, 3, 4, 5, 6, 8 and 10 — the protocol
/// chose it so every plausible scale (1.25, 1.5, 1.75, 2.4) is exact.
const SCALE_DENOMINATOR: f64 = 120.0;

/// How many device pixels one of this surface's logical units is worth,
/// fractions included — the factor the renderer draws the surface at,
/// the ledger measures its commits with, and the hit-test divides by.
///
/// Two statements a client can make, in precedence order:
///
/// - A `wp_viewport` destination together with the pixels the client
///   *presents*: a fractional-scale client told 1.5 commits a
///   `round(w × 1.5)` px buffer and sets the viewport destination to
///   `w` logical — the ratio between the two *is* its scale, stated
///   more precisely than `set_buffer_scale`'s integers ever could.
///   "Presents" and not "attached": a client may over-allocate its
///   buffer and show only a `wp_viewport.set_source` crop of it —
///   Chromium does exactly this for every frame of an interactive
///   resize, attaching a tile-rounded buffer (2560x2048 for a
///   2108x1568 window) and cropping the window out of its top-left
///   corner. The density statement is the crop over the destination
///   (exactly 2.0 in that trace); the buffer's slack rows say nothing.
///   The ratio is trusted only when it is the same in both axes (to
///   within the rounding the protocol itself allows) and at least 1: a
///   non-uniform or shrinking ratio is a viewport used for
///   *stretching* content, not a density statement, and smithay's
///   surface view applies that stretch inside the element, so the
///   right factor for the tree is the plain integer one.
/// - Otherwise `wl_surface.set_buffer_scale`, exactly as
///   [`committed_buffer_scale`] has always read it.
pub(crate) fn committed_surface_scale(surface: &WlSurface) -> f64 {
    let integer = committed_buffer_scale(surface) as f64;
    let sizes = with_renderer_surface_state(surface, |state| {
        // The surface view's `src` is the shown region in
        // buffer-logical units (already divided by the integer scale):
        // the `wp_viewport.set_source` crop when one is committed, the
        // whole buffer otherwise. Times the integer scale it is the
        // raw pixel extent the client actually presents.
        let view = state.view()?;
        let dst = state.surface_size()?;
        let scale = state.buffer_scale() as f64;
        Some(((view.src.size.w * scale, view.src.size.h * scale), (dst.w, dst.h)))
    })
    .flatten();
    let Some((shown, dst)) = sizes else {
        return integer;
    };
    ratio_scale(shown, dst).unwrap_or(integer)
}

/// The scale a shown-pixels/destination pair states, if it states one:
/// the presented region's pixel extent (the viewport source crop when
/// set, else the whole buffer — fractional because `set_source` speaks
/// wl_fixed) over the viewport destination, snapped onto the
/// protocol's 1/120 grid — provided both axes agree with the snapped
/// factor to within the pixel of rounding the protocol permits
/// (`round(size × scale)`) and the factor is at least 1. A non-uniform
/// or shrinking ratio is a viewport used for *stretching* content, not
/// a density statement, and answers `None`. Split from
/// [`committed_surface_scale`] because this arithmetic is the half that
/// fails silently, and it needs no live surface to pin down.
fn ratio_scale(shown: (f64, f64), dst: (i32, i32)) -> Option<f64> {
    let ((shown_w, shown_h), (dst_w, dst_h)) = (shown, dst);
    if dst_w <= 0
        || dst_h <= 0
        || !shown_w.is_finite()
        || !shown_h.is_finite()
        || shown_w <= 0.0
        || shown_h <= 0.0
    {
        return None;
    }
    let ratio_w = shown_w / dst_w as f64;
    let snapped = (ratio_w * SCALE_DENOMINATOR).round() / SCALE_DENOMINATOR;
    if snapped < 1.0 {
        return None;
    }
    let agrees = |shown: f64, dst: i32| ((dst as f64 * snapped).round() - shown).abs() < 1.0;
    (agrees(shown_w, dst_w) && agrees(shown_h, dst_h)).then_some(snapped)
}

/// The factor a surface is actually composed at on an output of
/// fractional scale `output_scale`, given the `declared` factor the
/// client committed ([`committed_surface_scale`]).
///
/// Almost always `declared` — a surface's pixels are worth what its
/// client said they are worth, the doctrine every comment in this file
/// repeats. The one exception is the integral *fallback* of a
/// fractional output: `wl_output.scale` cannot say 1.5, so such an
/// output advertises the ceiling ([`crate::state::advertised_output_scale`]
/// rounds UP on purpose), a client without fractional-scale renders at
/// 2x, and the extra density has to be put somewhere. Drawing it 1:1
/// would make every fallback client a third larger than its
/// fractional-aware neighbours; composing it at the output's real
/// factor instead downscales a crisp 2x buffer to 1.5x — sharper than
/// upscaling a 1x one, which is the entire reason the fallback rounds
/// up. Only that exact case is corrected: a client whose declared
/// factor is the ceiling of a genuinely fractional output scale. A
/// deliberate 2x commit on an integer-scale output (LibreOffice under
/// `GDK_SCALE=2` on a scale-1 desktop) is left alone, as it always was.
pub(crate) fn effective_surface_scale(declared: f64, output_scale: f64) -> f64 {
    let declared_is_integer = (declared - declared.round()).abs() < 1e-9;
    let output_fractional = (output_scale - output_scale.round()).abs() > 1e-9;
    if declared_is_integer && output_fractional && (declared - output_scale.ceil()).abs() < 1e-9 {
        output_scale
    } else {
        declared
    }
}

/// Where the client's window starts inside its own buffer — the
/// `xdg_surface.set_window_geometry` origin, which for a client drawing
/// its own chrome is the drop-shadow margin. See
/// `WindowRecord::content_offset`.
///
/// Converted by the same per-surface factor as the size below and as
/// the renderer draws with: the offset positions a rectangle measured
/// in physical pixels, and an inset scaled by anything else slides the
/// window out from under its own frame by the difference.
fn committed_content_offset(surface: &WlSurface, factor: f64) -> Point {
    with_states(surface, |states| {
        let mut guard = states.cached_state.get::<SurfaceCachedState>();
        guard.current().geometry
    })
    .filter(|geometry| geometry.size.w > 0 && geometry.size.h > 0)
    .map(|geometry| {
        Point::new(scale_length(geometry.loc.x, factor), scale_length(geometry.loc.y, factor))
    })
    .unwrap_or(Point::new(0, 0))
}

/// The size the client actually committed: its declared xdg window
/// geometry when set, else the buffer's logical size. `wm-core` reads
/// this through `Backend::window_geometry` at map time (how big does
/// the fresh client want to be), and `commit` compares it against the
/// record to detect client-side resizes.
///
/// `scale` converts the client's logical pixels into the physical ones
/// this compositor's ledger is kept in, and must be the surface's own
/// [`committed_buffer_scale`] — the factor `push_window_content`
/// renders it at. A client running at 2x declares a 300x226 window and
/// commits a 600x452 buffer, and 600x452 is what the frame has to be
/// drawn around, because 600x452 is what reaches the screen. Convert by
/// any other number and the three descriptions of one window stop
/// agreeing: the frame is drawn to one rectangle, the pointer routed by
/// a second, and the client's pixels land in a third.
fn committed_content_size(surface: &WlSurface, factor: f64) -> Option<Size> {
    let geometry = with_states(surface, |states| {
        let mut guard = states.cached_state.get::<SurfaceCachedState>();
        guard.current().geometry
    });
    if let Some(geometry) = geometry {
        if geometry.size.w > 0 && geometry.size.h > 0 {
            return Some(Size::new(
                scale_length(geometry.size.w, factor) as u32,
                scale_length(geometry.size.h, factor) as u32,
            ));
        }
    }
    with_renderer_surface_state(surface, |state| state.surface_size())
        .flatten()
        .filter(|size| size.w > 0 && size.h > 0)
        .map(|size| {
            Size::new(scale_length(size.w, factor) as u32, scale_length(size.h, factor) as u32)
        })
}

// -- wl_compositor -------------------------------------------------------

impl CompositorHandler for Compositor {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        // XWayland connects as a client of this compositor too, with
        // smithay's own client-data type rather than ours.
        if let Some(state) = client.get_data::<XWaylandClientData>() {
            return &state.compositor_state;
        }
        if let Some(state) = client.get_data::<ClientState>() {
            return &state.compositor_state;
        }
        panic!("unknown client data type");
    }

    fn new_surface(&mut self, surface: &WlSurface) {
        // Every surface gets the buffer-readiness pre-commit hook: a
        // commit whose dmabuf the client's GPU is still drawing into
        // must not land until it is finished (explicit syncobj acquire
        // point, or the dmabuf's own implicit fence). Sampling early is
        // not hypothetical — it is the Edge-on-NVIDIA flicker report;
        // see `dmabuf::install_readiness_hook` for the mechanism.
        crate::dmabuf::install_readiness_hook(surface);
        // And the guard against smithay's layer-shell pre-commit hook
        // outliving the role that installed it — a Qt shell that
        // destroys a layer surface and commits the same `wl_surface`
        // again is otherwise killed for a protocol error it did not
        // commit. It has to go on every surface, before the role
        // exists, because that is the only point early enough to be
        // ahead of smithay's hook in the surface's hook list; see
        // `layers::install_orphaned_role_guard` for the whole story.
        crate::layers::install_orphaned_role_guard(surface);
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Buffer bookkeeping first — everything below (and the
        // renderer, and the hit-test's subsurface walk) reads the
        // RendererSurfaceState this maintains.
        on_commit_buffer_handler::<Self>(surface);
        // Tell the surface where it is and how to draw for it. Both
        // are how a client hears the session's scale, and a toolkit
        // may honor either: `wl_surface.enter` names the outputs
        // whose advertised scale (see `state.rs`'s `advertise_scale`)
        // the client takes its maximum from, and
        // `preferred_buffer_scale` states the number outright for
        // clients binding `wl_surface` v6. Without the enter a GTK
        // client never consults the output at all and stays at 1x.
        // Every output, not the ones the surface overlaps: a window
        // can be dragged across a boundary between two commits, and
        // per-output enter/leave tracking buys nothing on a desktop
        // whose outputs all advertise one session-wide scale. Both
        // calls dedup internally (a set in smithay's `Output`, a
        // per-surface cache in `send_surface_state`), so a commit —
        // the hottest path in the protocol — sends nothing after the
        // first time. Xwayland's own surfaces are exempt from the
        // preferred-scale half: that server is told the scale through
        // XSETTINGS and `XCURSOR_SIZE` and must keep committing 1x
        // buffers over the ledger's 1x rectangles, so it is the one
        // client this compositor deliberately never invites to scale
        // itself. (Same reasoning as the Xdg-only filter in
        // `dispatch_pending`'s rescale drain.)
        for entry in &self.outputs {
            entry.output.enter(surface);
        }
        // Role logic (and the scale lookup below) runs against the
        // tree's root: a subsurface commit must not re-trigger toplevel
        // lifecycle, and a subsurface's scale is its window's.
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let xwayland = surface
            .client()
            .is_some_and(|client| client.get_data::<XWaylandClientData>().is_some());
        if !xwayland {
            // Which output's scale this surface should draw for. The
            // integer is the ceiling fallback (`advertised_output_scale`
            // rounds up on purpose); the exact fraction goes to any
            // fractional-scale object the client created. Both dedup
            // per surface inside smithay, so the hot path sends nothing
            // after the first commit at a given scale.
            let preferred = self.preferred_scale_for(&root);
            let advertised =
                crate::state::advertised_output_scale(preferred as f32).integer_scale();
            with_states(surface, |states| {
                smithay::wayland::compositor::send_surface_state(
                    surface,
                    states,
                    advertised,
                    smithay::utils::Transform::Normal,
                );
                smithay::wayland::fractional_scale::with_fractional_scale(states, |fractional| {
                    fractional.set_preferred_scale(preferred);
                });
            });
        }
        self.popups.commit(surface);

        // Layer surfaces first: their commit lifecycle (initial
        // configure, map/unmap edges, re-arrangement around the
        // committed size) lives in `layers.rs`, and a surface wearing
        // the layer role is by definition not a toplevel or popup.
        // Lock surfaces need no branch of their own — smithay's role
        // hooks police their commits, and the damage mark below is all
        // the compositor-side reaction a lock commit requires.
        if crate::layers::handle_commit(self, &root) {
            self.wm.backend_mut().mark_damaged();
            return;
        }

        let toplevel = self
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| *toplevel.wl_surface() == root)
            .cloned();
        if let Some(toplevel) = toplevel {
            self.toplevel_committed(&toplevel);
        } else if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(&root) {
            // A popup's first commit must be answered with its initial
            // configure or the client waits forever.
            if !popup.is_initial_configure_sent() {
                if let Err(error) = popup.send_configure() {
                    tracing::warn!(?error, "initial popup configure failed");
                }
            }
        }

        // Every commit can carry fresh pixels (client repaints are the
        // one damage source no backend verb sees), so the scene
        // redraws. Full-frame, same correctness-first call as the X11
        // side made with picom's --no-use-damage.
        self.wm.backend_mut().mark_damaged();
    }
}

impl Compositor {
    /// The fractional scale a surface should render for: the scale of
    /// the output its window (or layer surface, or lock surface) lives
    /// on, falling back to the primary output's for a surface not yet
    /// anchored anywhere — which is every surface's state at its first
    /// commit, when the primary is the only honest guess.
    pub(crate) fn preferred_scale_for(&self, root: &WlSurface) -> f64 {
        let backend = self.wm.backend();
        if let Some(id) = backend.window_for_surface(root) {
            if let Some(record) = backend.windows.get(&id) {
                return backend.scale_at(record.content);
            }
        }
        if let Some(record) = backend
            .layers
            .iter()
            .find(|record| record.surface.wl_surface() == root)
        {
            return backend.scale_at(record.geometry);
        }
        for entry in &backend.lock_surfaces {
            if entry.surface.wl_surface() == root {
                if let Some(monitor) = backend.monitors.get(entry.output) {
                    return backend.scale_at(monitor.geometry);
                }
            }
        }
        self.outputs.first().map(|entry| entry.scale).unwrap_or(1.0)
    }

    /// The xdg toplevel lifecycle, driven from commits (see the module
    /// doc): initial configure, then buffer-presence edges into
    /// MapRequest/Unmapped, then client-side resize drift into
    /// ConfigureRequest.
    fn toplevel_committed(&mut self, toplevel: &ToplevelSurface) {
        let root = toplevel.wl_surface().clone();
        let initial_configure_sent = with_states(&root, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .map(|data| data.lock().unwrap().initial_configure_sent)
        })
        .unwrap_or(true);
        if !initial_configure_sent {
            // The protocol's opening move: the client committed its
            // (buffer-less) role setup and now awaits a configure. Sent
            // with no size — the client picks, and its answer becomes
            // the map-time geometry below.
            toplevel.send_configure();
            return;
        }

        let has_buffer =
            with_renderer_surface_state(&root, |state| state.buffer().is_some()).unwrap_or(false);
        let was_mapped = mapped_marker(&root);
        let backend = self.wm.backend_mut();
        let Some(id) = backend.window_for_surface(&root) else {
            return;
        };
        // The factor everything below measures by: the client's own
        // committed statement (viewport ratio or integer buffer scale),
        // corrected for the integral-fallback case on this window's
        // output — one number, shared with the renderer and the
        // hit-test through `window_surface_scale`.
        let surface_scale = backend
            .windows
            .get(&id)
            .map(|record| backend.window_surface_scale(record))
            .unwrap_or(1.0);
        let committed = committed_content_size(&root, surface_scale);
        if has_buffer && !was_mapped {
            set_mapped_marker(&root, true);
            // Seed the record with the client's own size so
            // `window_geometry` answers the map-time "how big do you
            // want to be" with the truth instead of the fallback.
            if let Some(size) = committed {
                if let Some(record) = backend.windows.get_mut(&id) {
                    record.content.size = size;
                }
            }
            let offset = committed_content_offset(&root, surface_scale);
            if let Some(record) = backend.windows.get_mut(&id) {
                record.content_offset = offset;
            }
            backend.queue(WmEvent::MapRequest(id));
        } else if !has_buffer && was_mapped {
            // Null-buffer commit: the xdg unmap. The toplevel survives
            // and may map again — the marker reset arms the next
            // MapRequest edge, mirroring how an X11 client can unmap
            // and re-map its window under the same WM bookkeeping.
            set_mapped_marker(&root, false);
            // The ledger has to hear it too. `mapped` is what the
            // renderer draws from and what the hit-test routes by, so a
            // record left mapped after its buffer is gone is an
            // invisible rectangle that still swallows clicks - and it
            // outlives the frame, because `wm-core`'s teardown removes
            // the decoration but never touches this flag.
            if let Some(record) = backend.windows.get_mut(&id) {
                record.mapped = false;
            }
            backend.queue(WmEvent::Unmapped(id));
        } else if has_buffer {
            // The window-geometry offset is re-read on every commit,
            // not only at the map edge, because a shadow inset is not a
            // constant of the window: GTK drops it entirely when
            // maximized and can change it with the theme. A stale
            // offset shifts everything anchored at `content.pos -
            // content_offset` — the drawn window slides out from under
            // its own hit rect by exactly the difference, most visibly
            // as a maximized window hanging off the top-left of the
            // screen by its former shadow.
            let offset = committed_content_offset(&root, surface_scale);
            if let Some(record) = backend.windows.get_mut(&id) {
                record.content_offset = offset;
            }
            // A managed client committing a size other than the one on
            // record is the Wayland spelling of an X11 self-resize
            // ConfigureRequest (a terminal snapping to its cell grid
            // after our configure, say) — translated to the same event
            // so `wm-core` reflows the decoration around the client's
            // real size. Converges: `wm-core` answers via
            // `resize_client`, which updates the record to match.
            //
            // With one gate, and the gate is load-bearing: a commit is
            // only a *client-initiated* resize if the client is caught
            // up with us. While one of our configures is still in
            // flight (sent, not yet acked), a mismatched commit is the
            // client's OLD size crossing our NEW one on the wire, and
            // adopting it reverts the geometry we just set. Observed
            // live as maximize bouncing full → old-size → full within
            // 60ms — and, when the race ended the other way, as a
            // "maximized" terminal parked at its old size in the
            // screen's corner. During an interactive resize the same
            // misreading fed the drag a stream of stale sizes to fight,
            // which is half of the Edge resize-flicker report (the
            // other half was min-size hints returned in logical units).
            // A caught-up client's snap (foot acking our configure and
            // committing the nearest cell grid) has no pending
            // configure left, so it still adopts exactly as before.
            let client_behind = with_states(&root, |states| {
                states
                    .data_map
                    .get::<XdgToplevelSurfaceData>()
                    .map(|data| !data.lock().unwrap().pending_configures().is_empty())
            })
            .unwrap_or(false);
            // A commit whose size matches anything we recently asked
            // for is the client obeying us — possibly obeying an ask
            // from two configures ago, because acks are immediate while
            // commits trail rendering. Obedience, prompt or tardy, is
            // never a resize request. See `WindowRecord::recent_asks`
            // for the ping-pong this gate broke.
            let echoes_ask = committed.is_some_and(|size| {
                backend
                    .windows
                    .get(&id)
                    .is_some_and(|record| record.recent_asks.contains(&size))
            });
            if let (Some(size), Some(record)) = (committed, backend.windows.get(&id)) {
                if record.mapped && !client_behind && !echoes_ask && size != record.content.size {
                    let requested = Rect { pos: record.content.pos, size };
                    backend.queue(WmEvent::ConfigureRequest { window: id, requested });
                }
            }
        }
    }

    /// Queues a `_NET_WM_STATE`-shaped request — the translation
    /// `wm-x11` does for EWMH client messages, reused for the xdg
    /// requests that mean exactly the same thing.
    fn queue_net_state(
        &mut self,
        surface: &WlSurface,
        action: NetStateAction,
        first: NetState,
        second: Option<NetState>,
    ) {
        let backend = self.wm.backend_mut();
        if let Some(window) = backend.window_for_surface(surface) {
            backend.queue(WmEvent::NetStateRequested { window, action, first, second });
        }
    }
}

impl BufferHandler for Compositor {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

// -- wl_shm --------------------------------------------------------------

impl ShmHandler for Compositor {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

// -- wl_output -----------------------------------------------------------

impl OutputHandler for Compositor {
    fn output_bound(&mut self, _output: Output, _wl_output: wl_output::WlOutput) {}
}

// -- wl_seat -------------------------------------------------------------

impl SeatHandler for Compositor {
    // Plain wl_surfaces as focus targets: both xdg toplevels and
    // XWayland's X11 windows resolve to one (see `ManagedSurface`), so
    // no bespoke focus enum is needed — `wm-core` owns focus POLICY,
    // and the backend only ever points the seat at a surface.
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Compositor> {
        &mut self.seat_state
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, target: Option<&WlSurface>) {
        // Keyboard focus carries clipboard access with it — without
        // this, paste silently targets whichever client focused first.
        // Both selections follow it, and for the same reason: the
        // protocols gate `set_selection` on the requesting client
        // holding focus, and gate the offers a client is told about on
        // the same thing, so a focus change that only moved one of them
        // leaves the other reading a stale client's clipboard.
        let display_handle = self.display_handle.clone();
        let client = target.and_then(|surface| surface.client());
        set_data_device_focus(&display_handle, seat, client.clone());
        set_primary_focus(&display_handle, seat, client);
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        // The renderer composites the pointer itself (there is no
        // hardware cursor plane on the winit backend), so a client
        // changing its cursor is scene damage like any other.
        self.cursor_status = image;
        self.wm.backend_mut().mark_damaged();
    }
}

// -- selections / drag-and-drop ------------------------------------------

/// The Wayland-to-X11 half of the clipboard bridge.
///
/// Both callbacks fire only for *client* requests — `set_selection` on
/// a `wl_data_device` or a `zwp_primary_selection_device_v1` — never
/// for the compositor-side selections `xwayland.rs` installs when an X
/// client takes ownership. That asymmetry is what keeps the two halves
/// from ping-ponging a selection back and forth forever, and it is a
/// property of smithay's dispatch rather than of a guard here, so it is
/// worth knowing before adding one.
impl SelectionHandler for Compositor {
    type SelectionUserData = ();

    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: Seat<Self>,
    ) {
        // A Wayland client copied something. Xwayland owns the X-side
        // selection window, so it has to be told to claim CLIPBOARD (or
        // PRIMARY) on the X server and advertise these mime types;
        // `None` means the client dropped the selection, which releases
        // the X ownership again. Before this existed, copying in a
        // Wayland app and pasting in xterm produced nothing at all — X
        // clients asked the X server who owned the selection and the
        // answer was nobody.
        let Some(xwm) = self.xwm.as_mut() else {
            // XWayland is not running (or failed to start); there is no
            // X server to mirror the selection onto and native clients
            // already have it.
            return;
        };
        if let Err(error) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
            tracing::warn!(?error, ?ty, "could not hand the selection to XWayland");
        }
    }

    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: Seat<Self>,
        _user_data: &(),
    ) {
        // A Wayland client is pasting a selection this compositor owns
        // on behalf of an X client (the one `xwayland.rs`'s
        // `new_selection` installed). Fetching the bytes is an X
        // round-trip — INCR transfers included — so `X11Wm` runs it as
        // a calloop source and writes into `fd` when it completes,
        // which is why the loop handle goes with it. Nothing blocks
        // here; the pasting client simply reads its pipe when data
        // arrives.
        let loop_handle = self.loop_handle.clone();
        let Some(xwm) = self.xwm.as_mut() else {
            return;
        };
        if let Err(error) = xwm.send_selection(ty, mime_type, fd, loop_handle) {
            tracing::warn!(?error, ?ty, "could not read the X11 selection for a Wayland client");
        }
    }
}

impl DataDeviceHandler for Compositor {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl PrimarySelectionHandler for Compositor {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection_state
    }
}

// Defaults throughout: client-to-client DnD works through the seat's
// own grab machinery; a rendered drag icon is a follow-up for the
// renderer (the icon surface arrives in `started`, unused for now).
impl ClientDndGrabHandler for Compositor {}
impl ServerDndGrabHandler for Compositor {}

// -- xdg-shell -----------------------------------------------------------

impl XdgShellHandler for Compositor {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        // Chrome is ours: advertise server-side decorations from the
        // very first configure so toolkits (GTK, Qt) never draw their
        // own titlebars. Clients that skip xdg-decoration entirely get
        // framed regardless — same as every X11 client under a
        // reparenting WM.
        surface.with_pending_state(|state| {
            state.decoration_mode = Some(DecorationMode::ServerSide);
        });
        // Record now, MapRequest later: the id must exist before the
        // first commit (which may map immediately), but the initial
        // configure is only legal in response to that commit — see
        // `toplevel_committed`.
        let backend = self.wm.backend_mut();
        let id = WlWindowId(backend.alloc_id());
        backend.windows.insert(
            id,
            WindowRecord::new(ManagedSurface::Xdg(surface), Rect::default()),
        );
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        // Position straight from the positioner, parent-relative — the
        // renderer and the hit-test both resolve it against the
        // parent's content rect via `PopupManager`. No unconstraining
        // pass yet: NeXTSTEP-style menus hug their parent, and a
        // popup off the output edge is the client's own placement to
        // fix. (Initial configure happens at first commit.)
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
        });
        if let Err(error) = self.popups.track_popup(PopupKind::from(surface)) {
            tracing::warn!(?error, "failed to track xdg popup");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        // Not granted: smithay's popup-grab machinery needs a focus
        // type convertible from popups (`KeyboardFocus: From<PopupKind>`),
        // which the deliberately-plain `WlSurface` focus above isn't.
        // Menus still open, render, and take pointer input through the
        // ordinary hit-test; the cost is no popup_done auto-dismissal
        // on outside clicks (toolkits dismiss on their own focus/click
        // handling). Strictly the protocol prefers dismissing an
        // ungranted grab — deliberately not done, since insta-closing
        // every menu is uselessness dressed up as compliance. Revisit
        // with a dedicated focus-target enum if it bites.
    }

    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        // A client-initiated interactive move (a CSD titlebar drag).
        // Under server-side decorations moves start from OUR titlebar,
        // which `wm-core` runs off frame button events, and this request
        // used to be dropped for that reason — the same reason X11's
        // `_NET_WM_MOVERESIZE` was.
        //
        // What changed is that a managed window can now have no chrome
        // of ours at all, and for one of those this request is not a
        // second way to move the window, it is the only way. So it is
        // answered for every toplevel rather than only the frameless
        // ones: a framed client that asks gets the same drag it would
        // have got from our titlebar, and `wm-core` refuses the request
        // outright while another drag is in flight, so a client cannot
        // steal a move the user is already making.
        //
        // The serial is not checked against a real button press. Doing
        // so would need the seat's grab history threaded through here,
        // and the failure it would prevent — a client starting a move
        // with no pointer down — already ends the moment the user
        // presses and releases a button, because `wm-core` anchors the
        // drag on the pointer and finishes it on release.
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            backend.queue(WmEvent::MoveRequest(id));
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        // This used to be dropped, on the reasoning that unlike a move
        // it was not the only path left to an unframed window: a client
        // that draws its own chrome draws its own resize grips and
        // could resize itself by committing a new size. The second half
        // of that was wrong in practice — a grip does not commit sizes,
        // it sends exactly this request and waits for the compositor to
        // run the drag, so dropping it made every client-decorated
        // window's edges dead weight: the grip arms, the cursor
        // changes, and nothing ever moves. `wm-core` now runs the same
        // interactive resize a resizebar drag runs (grab taken, ends on
        // release, size hints respected), anchored on its own record of
        // the window; the edge is the one thing only the client knows.
        //
        // The serial is unchecked for the same reason `move_request`'s
        // is: the failure it would prevent ends on its own at the next
        // press-and-release, and checking would mean threading the
        // seat's grab history through here.
        let Some(edge) = wm_resize_edge(edges) else {
            // `None` (and any future protocol value): a resize with no
            // edge has no geometry to solve for, so it is refused
            // rather than guessed at.
            return;
        };
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            backend.queue(WmEvent::ResizeRequest { window: id, edge });
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        // One request toggling both axes — byte-for-byte the
        // `_NET_WM_STATE` maximize message shape `wm-x11` translates.
        self.queue_net_state(
            surface.wl_surface(),
            NetStateAction::Add,
            NetState::MaximizedHorz,
            Some(NetState::MaximizedVert),
        );
        // The protocol demands a configure in reply whether or not the
        // request is honored; `wm-core`'s own reconfigure follows once
        // it acts on the queued event.
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        self.queue_net_state(
            surface.wl_surface(),
            NetStateAction::Remove,
            NetState::MaximizedHorz,
            Some(NetState::MaximizedVert),
        );
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<wl_output::WlOutput>,
    ) {
        // Single output today — the output hint has nothing to select.
        self.queue_net_state(surface.wl_surface(), NetStateAction::Add, NetState::Fullscreen, None);
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        self.queue_net_state(
            surface.wl_surface(),
            NetStateAction::Remove,
            NetState::Fullscreen,
            None,
        );
        if surface.is_initial_configure_sent() {
            surface.send_configure();
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        // This used to be ignored on the reasoning that miniaturization
        // is a WM gesture with no request-shaped path into `wm-core`.
        // That reasoning died with client-side decorations: a window
        // whose client draws its own chrome draws its own minimize
        // button, and this request is that button. Dropping it left
        // LibreOffice's minimize dead. `MinimizeRequest` routes into
        // the same miniaturize the titlebar button runs, so the client
        // ends up as an icon tile exactly as if ours had been clicked.
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            backend.queue(WmEvent::MinimizeRequest(id));
        }
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        // Same reason this event exists on X11: most apps set their
        // real title well after mapping (see
        // `BackendEvent::TitleChanged`'s doc), so the titlebar must
        // repaint on the property change, not the map.
        let title = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().title.clone())
        });
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            if let Some(record) = backend.windows.get_mut(&id) {
                // Keep the record's cache current per its contract
                // (`WindowRecord::title`'s doc in state.rs).
                record.title = title;
            }
            backend.queue(WmEvent::TitleChanged(id));
        }
    }

    fn app_id_changed(&mut self, surface: ToplevelSurface) {
        let app_id = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().unwrap().app_id.clone())
        });
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            let changed = backend.windows.get(&id).is_some_and(|record| record.app_id != app_id);
            if let Some(record) = backend.windows.get_mut(&id) {
                record.app_id = app_id;
            }
            // The identity is an input to the decoration decision — it
            // is what a `[decorations]` rule matches on — and it is not
            // guaranteed to arrive before the window maps. Nothing
            // re-asked, so a client that set its `app_id` after mapping
            // kept whatever chrome it was given for the rest of its
            // life, and a rule naming it did nothing at all. (LibreOffice
            // sets `soffice` and then immediately `libreoffice-writer`,
            // so this is not a hypothetical.)
            if changed {
                backend.queue(WmEvent::ChromeChanged(id));
            }
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // The record goes immediately (`wm-x11` drops `known_clients`
        // on DestroyNotify the same way); `wm-core`'s teardown then
        // calls backend verbs that all tolerate the missing window.
        let backend = self.wm.backend_mut();
        if let Some(id) = backend.window_for_surface(surface.wl_surface()) {
            backend.forget_window(id);
            backend.queue(WmEvent::Destroyed(id));
            backend.mark_damaged();
        }
    }

    fn popup_destroyed(&mut self, _surface: PopupSurface) {
        // PopupManager prunes dead popups itself (the loop calls its
        // `cleanup`); the scene just needs a repaint without it.
        self.wm.backend_mut().mark_damaged();
    }
}

/// The protocol's resize edge -> the theme vocabulary `wm-core` runs
/// its resize state machine in. `None` for `xdg_toplevel`'s literal
/// `None` edge (and for any value a future protocol revision adds):
/// the request means "resize from nowhere", which no drag can honor —
/// the compass points map one-to-one and nothing else does.
fn wm_resize_edge(
    edge: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
) -> Option<ResizeEdge> {
    use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge as Xdg;
    match edge {
        Xdg::Top => Some(ResizeEdge::North),
        Xdg::Bottom => Some(ResizeEdge::South),
        Xdg::Left => Some(ResizeEdge::West),
        Xdg::Right => Some(ResizeEdge::East),
        Xdg::TopLeft => Some(ResizeEdge::NorthWest),
        Xdg::TopRight => Some(ResizeEdge::NorthEast),
        Xdg::BottomLeft => Some(ResizeEdge::SouthWest),
        Xdg::BottomRight => Some(ResizeEdge::SouthEast),
        _ => None,
    }
}

// -- xdg-decoration ------------------------------------------------------
// The standard decoration protocol, and — this being the surprise that
// took a swarm to find — not the one most of this desktop's clients
// speak. GTK binds only KDE's older interface; see `crate::decoration`,
// which holds the whole policy, the evidence model, and the KDE half.
//
// What is left here is bookkeeping and one rule: the mode configured on
// the wire is computed from the same decision that decides whether a
// frame gets built, so the two cannot disagree. They did. A Chrome
// web-app window asked for server-side decorations, was answered
// server-side, and was then left unframed because the framing decision
// consulted an `app_id` list instead of the answer we had just given
// it — chrome from neither side, on the most-used window on the desk.

impl XdgDecorationHandler for Compositor {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        let backend = self.wm.backend_mut();
        let mut client_side = false;
        if let Some(id) = backend.window_for_surface(toplevel.wl_surface()) {
            if let Some(record) = backend.windows.get_mut(&id) {
                record.decoration.xdg_object = true;
            }
            // Creating the object without asking for a mode means "you
            // decide", and this desktop decides its own chrome — unless
            // a rule says otherwise, which is checked here so the very
            // first configure already carries the final answer.
            if let Some(record) = backend.windows.get(&id) {
                client_side = backend.xdg_client_draws_own_chrome(record);
            }
            backend.queue(WmEvent::ChromeChanged(id));
        }
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(if client_side { DecorationMode::ClientSide } else { DecorationMode::ServerSide });
        });
        // No configure here: if this races the initial commit, the
        // initial configure carries the mode; otherwise request_mode/
        // unset_mode follow immediately and send one.
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        // The ask is recorded, then read back through the one policy
        // every other decoration path goes through. A ClientSide ask is
        // honored — which is what KWin, labwc and cosmic-comp do, and
        // what the protocol is shaped for — because the clients that
        // ask for it and mean it (a browser whose frame is fused with
        // its tab strip, a libadwaita headerbar) cannot drop their
        // chrome on request, so imposing ours gives two titlebars with
        // no way back. The client that asks and then draws nothing is
        // real too (a terminal configured `decorations = "None"` for a
        // tiling desktop), and it is answered by the modifier-drag that
        // moves and resizes any window, and by one line of
        // `[decorations] server_side` if the user wants its frame back.
        let backend = self.wm.backend_mut();
        let asked_client_side = mode == DecorationMode::ClientSide;
        let mut client_side = asked_client_side;
        if let Some(id) = backend.window_for_surface(toplevel.wl_surface()) {
            if let Some(record) = backend.windows.get_mut(&id) {
                record.decoration.xdg_object = true;
                record.decoration.xdg_client_side = Some(asked_client_side);
            }
            if let Some(record) = backend.windows.get(&id) {
                client_side = backend.xdg_client_draws_own_chrome(record);
            }
            if client_side != asked_client_side {
                tracing::debug!(?id, asked_client_side, "a [decorations] rule overrides this client's decoration request");
            }
            backend.queue(WmEvent::ChromeChanged(id));
        }
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(if client_side { DecorationMode::ClientSide } else { DecorationMode::ServerSide });
        });
        send_decoration_configure(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        // No preference means the choice is genuinely ours, and ours is
        // server-side: this desktop's chrome is the product.
        let backend = self.wm.backend_mut();
        let mut client_side = false;
        if let Some(id) = backend.window_for_surface(toplevel.wl_surface()) {
            if let Some(record) = backend.windows.get_mut(&id) {
                record.decoration.xdg_object = true;
                record.decoration.xdg_client_side = None;
            }
            if let Some(record) = backend.windows.get(&id) {
                client_side = backend.xdg_client_draws_own_chrome(record);
            }
            backend.queue(WmEvent::ChromeChanged(id));
        }
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(if client_side { DecorationMode::ClientSide } else { DecorationMode::ServerSide });
        });
        send_decoration_configure(&toplevel);
    }
}

/// Sends the configure a decoration request has to be answered with,
/// and does so even when nothing else about the surface changed.
///
/// The subtlety that made this a function rather than a line: smithay's
/// `send_pending_configure` sends nothing when `has_pending_changes()`
/// is false, and that predicate does not consider whether the
/// *decoration* configure has been sent — only `send_configure` does.
/// Because a toplevel is primed with `ServerSide` the moment it is
/// created, imposing `ServerSide` on a client that asked for
/// `ClientSide` is a no-op state change, so a client that binds the
/// decoration object after its first (buffer-less) commit could ask,
/// be answered with silence, and — forbidden by the protocol from
/// attaching a buffer before its first decoration configure — never map
/// at all.
fn send_decoration_configure(toplevel: &ToplevelSurface) {
    if !toplevel.is_initial_configure_sent() {
        // The initial configure will carry the mode when it goes.
        return;
    }
    if toplevel.send_pending_configure().is_none() {
        // Nothing else changed, so force one: the client is waiting on
        // the decoration configure specifically.
        toplevel.send_configure();
    }
}

// -- fractional-scale / viewporter ---------------------------------------

impl smithay::wayland::fractional_scale::FractionalScaleHandler for Compositor {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Answer immediately: a toolkit decides its first buffer's size
        // from this, and one that hears nothing before its first commit
        // renders at 1x and resizes visibly a frame later.
        let mut root = surface.clone();
        while let Some(parent) = get_parent(&root) {
            root = parent;
        }
        let preferred = self.preferred_scale_for(&root);
        with_states(&surface, |states| {
            smithay::wayland::fractional_scale::with_fractional_scale(states, |fractional| {
                fractional.set_preferred_scale(preferred);
            });
        });
    }
}

smithay::delegate_fractional_scale!(Compositor);
smithay::delegate_viewporter!(Compositor);

delegate_compositor!(Compositor);
delegate_shm!(Compositor);
delegate_output!(Compositor);
delegate_seat!(Compositor);
delegate_data_device!(Compositor);
delegate_primary_selection!(Compositor);
delegate_xdg_shell!(Compositor);
delegate_xdg_decoration!(Compositor);

#[cfg(test)]
mod tests {
    use super::*;

    /// The invariant that keeps every existing client exactly where it
    /// was: a surface that draws one pixel per pixel is measured
    /// unchanged. Everything in this module runs through this
    /// conversion, so a factor that misbehaved at 1 would move every
    /// Xwayland window and every toolkit that ignores `GDK_SCALE`.
    #[test]
    fn a_one_to_one_surface_is_measured_unchanged() {
        assert_eq!(scale_length(300, 1.0), 300);
        assert_eq!(scale_length(0, 1.0), 0);
        assert_eq!(scale_length(-24, 1.0), -24);
    }

    /// The bug this conversion exists for, in numbers: LibreOffice with
    /// `GDK_SCALE=2` declares a 300x226 window and hands over a 600x452
    /// buffer. The ledger has to hold 600x452 or the frame is drawn at
    /// half the size of the pixels inside it.
    #[test]
    fn a_two_x_surface_is_measured_in_the_pixels_it_drew() {
        assert_eq!(scale_length(300, 2.0), 600);
        assert_eq!(scale_length(226, 2.0), 452);
        // Window-geometry origins are converted by the same factor, and
        // a client-drawn drop shadow makes them negative as often as
        // not.
        assert_eq!(scale_length(-13, 2.0), -26);
    }

    /// A fractional client is measured by its exact fraction, rounded
    /// the way the protocol rounds buffer sizes: 640 logical at 1.5 is
    /// 960 physical, and an odd extent rounds to the nearest pixel
    /// rather than truncating a half away.
    #[test]
    fn a_fractional_surface_is_measured_by_its_fraction() {
        assert_eq!(scale_length(640, 1.5), 960);
        assert_eq!(scale_length(333, 1.5), 500); // 499.5 rounds up
        assert_eq!(scale_length(100, 1.25), 125);
    }

    /// A scale below the floor cannot arrive over the protocol, and the
    /// point of the floor is that if one ever did it would not silently
    /// annihilate the window: 0 would collapse every size to nothing.
    #[test]
    fn an_absent_or_impossible_scale_degrades_to_one() {
        assert_eq!(scale_length(300, 0.0), 300);
        assert_eq!(scale_length(300, -2.0), 300);
        assert_eq!(scale_length(300, f64::NAN), 300);
        assert_eq!(physical_to_logical(300, 0.0), 300);
    }

    /// The configure/commit round trip at a fractional factor: the
    /// physical ask converts to logical and back to the physical size
    /// the client will actually commit, and the two legs must be exact
    /// inverses of each other's rounding or every configure drifts the
    /// window by a pixel (`resize_client`'s unbounded-growth story, in
    /// fractional form).
    #[test]
    fn the_fractional_round_trip_is_stable() {
        for physical in [200, 333, 501, 960, 1279] {
            for factor in [1.0, 1.25, 1.5, 2.0] {
                let logical = physical_to_logical(physical, factor);
                let expected = scale_length(logical, factor);
                // One more round trip lands exactly where the first did.
                assert_eq!(physical_to_logical(expected, factor), logical, "{physical} @ {factor}");
                assert_eq!(scale_length(logical, factor), expected);
            }
        }
    }

    /// The buffer/viewport ratio is a scale statement only when it is
    /// uniform, at least 1, and lands on the protocol's 1/120 grid to
    /// within the rounding the protocol itself allows.
    #[test]
    fn a_viewport_ratio_is_read_as_a_scale_only_when_it_is_one() {
        // The canonical fractional client: round(w × 1.5) buffer over a
        // w-logical destination.
        assert_eq!(ratio_scale((960.0, 720.0), (640, 480)), Some(1.5));
        // Odd extents round, and the snap still recovers the factor.
        assert_eq!(ratio_scale((500.0, 350.0), (333, 233)), Some(1.5));
        // GTK4's spelling of 2x: viewport-backed rather than
        // set_buffer_scale.
        assert_eq!(ratio_scale((1280.0, 960.0), (640, 480)), Some(2.0));
        // 1.25, the other common step.
        assert_eq!(ratio_scale((800.0, 600.0), (640, 480)), Some(1.25));
        // A video player stretching 640 to 1280 is not rendering at
        // half density — the ratio shrinks, so it is not a scale.
        assert_eq!(ratio_scale((640.0, 480.0), (1280, 960)), None);
        // A non-uniform stretch is not a scale either.
        assert_eq!(ratio_scale((960.0, 480.0), (640, 480)), None);
        // Degenerate extents answer nothing rather than infinity.
        assert_eq!(ratio_scale((960.0, 720.0), (0, 480)), None);
        assert_eq!(ratio_scale((f64::NAN, 720.0), (640, 480)), None);
    }

    /// The mid-resize scale flash, from the captured wire (Chromium
    /// `--ozone-platform=wayland` at scale 2, corner drag): each drag
    /// frame attaches a tile-rounded over-allocation and presents a
    /// `set_source` crop of it. The evidence handed to [`ratio_scale`]
    /// must be the crop over the destination — exactly the session
    /// scale — while the raw buffer over the destination is the
    /// non-uniform garbage that used to collapse the factor to 1 and
    /// flash the window (then bounce its geometry through the
    /// client-resize adoption path).
    #[test]
    fn a_cropped_over_allocated_buffer_still_states_its_scale() {
        // Captured commits: buffer 2560x2048 raw, src crop / dst pairs
        // from three consecutive drag motions.
        for (src, dst) in [
            ((2108.0, 1568.0), (1054, 784)),
            ((2112.0, 1572.0), (1056, 786)),
            ((2128.0, 1588.0), (1064, 794)),
        ] {
            assert_eq!(ratio_scale(src, dst), Some(2.0), "{src:?} over {dst:?}");
        }
        // The regression, pinned: the full buffer extent over the same
        // destinations is NOT a scale statement (2560/1054 and
        // 2048/784 do not even agree), which is why the crop — not the
        // allocation — must be what is measured.
        assert_eq!(ratio_scale((2560.0, 2048.0), (1054, 784)), None);
        // Fractional crops (set_source speaks wl_fixed) still land on
        // the grid within the protocol's own rounding.
        assert_eq!(ratio_scale((832.5, 619.5), (666, 496)), Some(1.25));
    }

    /// The integral-fallback correction fires for exactly one shape of
    /// mismatch: an integer declaration that is the ceiling of a
    /// fractional output scale. Everything else keeps the client's own
    /// number — most importantly a deliberate 2x commit on an integer
    /// output, which is the case the per-surface doctrine exists for.
    #[test]
    fn only_the_ceiling_fallback_is_composed_at_the_outputs_fraction() {
        // A 2x buffer on a 1.5 output is the fallback: downscale.
        assert_eq!(effective_surface_scale(2.0, 1.5), 1.5);
        assert_eq!(effective_surface_scale(2.0, 1.25), 1.25);
        // A fractional-aware client already matches; untouched.
        assert_eq!(effective_surface_scale(1.5, 1.5), 1.5);
        // LibreOffice under GDK_SCALE=2 on a scale-1 desktop: the
        // ledger holds its 600px buffer, exactly as always.
        assert_eq!(effective_surface_scale(2.0, 1.0), 2.0);
        // An Xwayland-style 1x commit on a fractional output stays 1x —
        // its pixels are 1:1 by construction.
        assert_eq!(effective_surface_scale(1.0, 1.5), 1.0);
        // A 3x commit on a 1.5 output is not the ceiling; the client's
        // word stands.
        assert_eq!(effective_surface_scale(3.0, 1.5), 3.0);
        // Integer output, integer client: identity.
        assert_eq!(effective_surface_scale(2.0, 2.0), 2.0);
    }

    /// Every compass point of `xdg_toplevel.resize` maps to the edge of
    /// the same name — a swapped pair here is a window that grows left
    /// when its right grip is dragged, which no test of the drag itself
    /// would localize to this table.
    #[test]
    fn every_xdg_resize_edge_maps_to_its_compass_point() {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge as Xdg;
        assert_eq!(wm_resize_edge(Xdg::Top), Some(ResizeEdge::North));
        assert_eq!(wm_resize_edge(Xdg::Bottom), Some(ResizeEdge::South));
        assert_eq!(wm_resize_edge(Xdg::Left), Some(ResizeEdge::West));
        assert_eq!(wm_resize_edge(Xdg::Right), Some(ResizeEdge::East));
        assert_eq!(wm_resize_edge(Xdg::TopLeft), Some(ResizeEdge::NorthWest));
        assert_eq!(wm_resize_edge(Xdg::TopRight), Some(ResizeEdge::NorthEast));
        assert_eq!(wm_resize_edge(Xdg::BottomLeft), Some(ResizeEdge::SouthWest));
        assert_eq!(wm_resize_edge(Xdg::BottomRight), Some(ResizeEdge::SouthEast));
    }

    /// The protocol's `None` edge is refused, not guessed at: a resize
    /// from no edge has no geometry to solve for, and `wm-core` must
    /// never see a request the drag machinery cannot finish.
    #[test]
    fn a_resize_from_no_edge_is_refused() {
        use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge as Xdg;
        assert_eq!(wm_resize_edge(Xdg::None), None);
    }
}
