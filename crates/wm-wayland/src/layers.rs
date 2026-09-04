//! wlr-layer-shell: the protocol every launcher, bar, notification
//! daemon and OSD targets (fuzzel, mako, waybar, wob — none of them
//! opens an xdg toplevel). Without this global those tools connect,
//! find no `zwlr_layer_shell_v1`, and exit; with it they become part
//! of the scene this compositor already composes by hand.
//!
//! Smithay ships the protocol plumbing (`wayland::shell::wlr_layer`:
//! roles, cached state, configure bookkeeping) and its reference
//! compositors integrate it through `desktop::Space`/`LayerMap`. This
//! compositor has no Space — `renderer.rs` composes an explicit
//! front-to-back walk and `input.rs` routes by an explicit hit walk,
//! both over the [`crate::state::WaylandBackend`] ledger — so the integration here is
//! the same shape as every other surface family: a ledger record per
//! layer surface ([`LayerRecord`] on the backend, where the renderer
//! and the hit-test can see it), and one reconciliation pass per
//! dispatch ([`refresh`]) that turns the protocol's cached state into
//! ledger geometry.
//!
//! # Where the four layers sit in the scene
//!
//! Bottom to top: wallpaper, `Background`, `below` shell surfaces,
//! `Bottom`, the frame band (managed windows), `Top`, `above` shell
//! surfaces (the dock and the shell's own menus), `Overlay`, the
//! cursor. Two of those placements are choices worth defending:
//! `Top` sits *below* the dock and the shell menus because a
//! notification daemon must not cover the menu the user just opened,
//! while `Overlay` sits above them because the protocol reserves it
//! for surfaces that must beat everything (an OSD, a keyboard
//! overlay) — and the input walk in `input.rs::hit_at` mirrors both,
//! because a band the renderer draws above the dock that the hit-test
//! walks below it is a click landing on something the user cannot see.
//!
//! # One coordinate discipline, again
//!
//! The ledger is in physical pixels; a layer client speaks its own
//! logical ones. Every number that crosses the boundary converts by a
//! per-surface factor exactly as `xdg.rs` does for toplevels: sizes,
//! margins and exclusive zones a client requests are multiplied up by
//! the factor it renders at, and the sizes this compositor configures
//! back are divided by it. Before a surface has committed a buffer
//! that factor is a prediction — the outputs' advertised scale, which
//! is what a scale-aware client will adopt — and from the first mapped
//! commit onward it is the scale the surface actually committed, the
//! only number that makes the drawn rectangle and the hit rectangle
//! the same rectangle (`renderer::push_surface_tree` draws 1 buffer
//! pixel : 1 screen pixel by that same factor).
//!
//! # Exclusive zones feed the workareas
//!
//! A bar that reserves an edge strip must push maximized windows out
//! of it. The shell owns the baseline workareas (the Dock's column on
//! the primary — `Shell::apply_workareas` re-asserts them from several
//! paths this module never sees: theme reloads, screen resizes), so
//! rather than hooking every one of those paths, [`apply_workareas`]
//! recomputes the composed answer once per dispatch pass while any
//! layer reservation exists: the shell's per-monitor baseline (its
//! primary workarea; full geometry elsewhere — exactly what
//! `Desktop::workareas` produces) intersected with the monitor minus
//! the layer insets, pushed through the same `set_workareas` call. The
//! pass runs after everything that could have reset the areas, so the
//! composed rects always land last; when no reservation exists the
//! module goes silent and the shell's own areas stand untouched.
//!
//! The Dock is the one piece of the shell's baseline that *reacts* to
//! a reservation rather than merely being intersected with it. Its
//! column hugs the primary's top-right corner, which is exactly where
//! a top bar puts its most important controls, and layer-shell has no
//! way for the bar to ask the compositor's chrome to move — so the
//! same pass hands the primary's top and right insets to the shell
//! (`Shell::set_edge_reservation`) before reading its baseline, and
//! the Dock hangs itself under the bar. The shell's workarea widens by
//! any right-edge inset for the same reason: the composition below is
//! an intersection, and the intersection of the Dock's displaced
//! column with the bar's own strip would otherwise leave the column
//! standing over windows.

use smithay::backend::renderer::utils::with_renderer_surface_state;
use smithay::delegate_layer_shell;
use smithay::output::Output;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::compositor::{add_pre_commit_hook, with_states};
use smithay::wayland::shell::wlr_layer::{
    Anchor, ExclusiveZone, KeyboardInteractivity, Layer, LayerSurface, LayerSurfaceCachedState, Margins,
    WlrLayerShellHandler, WlrLayerShellState, LAYER_SURFACE_ROLE,
};
use smithay::wayland::shell::xdg::PopupSurface;

use chonk_shell::desktop::EdgeReservation;
use wm_theme_api::{Point, Rect, Size};

use crate::state::Compositor;

/// A layer surface in the ledger's id space — same discipline as
/// `WlShellId`/`WlWindowId`: `Copy` ids from the shared allocator, so
/// routing state (`input.rs`'s `PressTarget`) can name a surface
/// without holding its protocol handle.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct LayerId(pub u64);

/// Ledger entry for one layer surface, on [`crate::state::WaylandBackend`]
/// beside the shell and window records for the same reason they are
/// there: the renderer and the hit-test read the ledger and nothing
/// else, and a surface family kept anywhere else would be invisible to
/// one of them.
pub(crate) struct LayerRecord {
    pub id: LayerId,
    pub surface: LayerSurface,
    /// Index into `Compositor::outputs`/`WaylandBackend::monitors` —
    /// the output this surface is arranged against. Resolved at
    /// creation from the client's `wl_output` (the primary when it
    /// named none, which the protocol leaves to the compositor).
    pub output: usize,
    /// The committed layer; a client may move between layers with
    /// `set_layer`, so [`refresh`] re-reads it every pass.
    pub layer: Layer,
    /// The committed keyboard interactivity, cached beside the layer
    /// for the focus logic (`sync_keyboard`, and `input.rs`'s
    /// on-demand click handling).
    pub interactivity: KeyboardInteractivity,
    /// Physical, global — the space every other rect in the ledger is
    /// in. Written by [`refresh`], read by the renderer and the hit
    /// walk.
    pub geometry: Rect,
    pub mapped: bool,
    /// The namespace the client declared ("launcher", "notifications").
    /// The only human-readable identity a layer surface has: kept for
    /// logs, and read by [`declined`], the one policy that keys on it.
    pub namespace: String,
}

/// The namespace Omarchy's shell gives its Background plugin's surface
/// (`shell/plugins/background/Background.qml`).
pub(crate) const OMARCHY_BACKGROUND_NAMESPACE: &str = "omarchy-background";

/// Whether a layer surface is one this desktop hosts but never shows,
/// as a matter of policy rather than of the user's choosing (the
/// user's choices live in `WaylandBackend::hidden_layer_namespaces`,
/// and `WaylandBackend::layer_presented` combines the two).
///
/// Exactly one: Omarchy's Background plugin. Chonkstep hosts Omarchy's
/// shell for its bar, panels, notifications and pickers
/// (`chonk_shell::omarchy_shell`), and that shell also ships a
/// full-screen `background`-layer surface that paints Omarchy's
/// wallpaper and takes every button on the desk — double-click opens
/// its pickers. Under this desktop the desk is already spoken for: the
/// root wallpaper (which wears Omarchy's own background when the theme
/// follows Omarchy) and the root menu on right-click. So the surface is
/// configured, committed and answered like any other, and neither
/// drawn nor hit-tested. The client sees a healthy surface; the user
/// sees chonkstep's desk.
///
/// Keyed on both the layer and the namespace: a wallpaper daemon of
/// the user's own choosing (`swaybg`, namespace `wallpaper`) is not
/// Omarchy's plugin, and a surface Omarchy moved off the background
/// layer with `set_layer` is no longer painting a wallpaper.
pub(crate) fn declined(layer: Layer, namespace: &str) -> bool {
    layer == Layer::Background && namespace == OMARCHY_BACKGROUND_NAMESPACE
}

/// Compositor-side state this module keeps between passes.
pub(crate) struct LayerShell {
    pub state: WlrLayerShellState,
    /// The layer surface currently holding *exclusive* keyboard focus,
    /// if any — see [`sync_keyboard`].
    pub exclusive_focus: Option<LayerId>,
    /// The layer surface holding *on-demand* keyboard focus (the user
    /// clicked it; `input.rs` sets and clears this).
    pub on_demand_focus: Option<LayerId>,
    /// Per-output insets the mapped exclusive zones reserved last
    /// pass, in `monitors` order. Read by [`apply_workareas`].
    pub reserved: Vec<EdgeInsets>,
    /// Whether the previous pass had any reservation at all — the
    /// transition back to zero must re-apply the shell's baseline
    /// once, or the strip a departed bar reserved stays reserved
    /// forever.
    pub reserved_last_pass: bool,
    /// Layer geometry is event-driven. Ordinary toplevel traffic must
    /// not repeatedly allocate layout scratch space and re-lock every
    /// layer's cached protocol state when no layer fact changed.
    pub needs_arrange: bool,
    /// The shell/workspace baseline onto which `reserved` was last
    /// composed. A Dock toggle or reload changes the core's revision;
    /// the next pass then reapplies layer reservations exactly once.
    pub applied_workarea_revision: u64,
}

impl LayerShell {
    pub(crate) fn new(state: WlrLayerShellState) -> Self {
        Self {
            state,
            exclusive_focus: None,
            on_demand_focus: None,
            reserved: Vec::new(),
            reserved_last_pass: false,
            needs_arrange: true,
            applied_workarea_revision: 0,
        }
    }
}

// ---------------------------------------------------------------------
// The pure geometry: anchors, margins, exclusive zones.
// All of it in physical pixels; the caller converts the client's
// logical values by the surface's factor before it gets here.
// ---------------------------------------------------------------------

/// Space reserved off each edge of an output by exclusive zones.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct EdgeInsets {
    pub left: i32,
    pub right: i32,
    pub top: i32,
    pub bottom: i32,
}

impl EdgeInsets {
    pub(crate) fn is_zero(&self) -> bool {
        *self == EdgeInsets::default()
    }
}

/// The edge an exclusive zone reserves from, per the protocol's rule:
/// meaningful only when the surface is anchored to exactly one edge,
/// or to one edge plus both edges perpendicular to it (a full-width
/// bar: `TOP | LEFT | RIGHT` reserves from the top). Any other anchor
/// combination — none, a corner, two parallel edges, all four — has no
/// single edge to reserve from and is treated as neutral, which is
/// what the spec says to do rather than a guess.
pub(crate) fn exclusive_edge(anchor: Anchor) -> Option<Anchor> {
    for (edge, span) in [
        (Anchor::TOP, Anchor::LEFT | Anchor::RIGHT),
        (Anchor::BOTTOM, Anchor::LEFT | Anchor::RIGHT),
        (Anchor::LEFT, Anchor::TOP | Anchor::BOTTOM),
        (Anchor::RIGHT, Anchor::TOP | Anchor::BOTTOM),
    ] {
        if anchor == edge || anchor == edge | span {
            return Some(edge);
        }
    }
    None
}

/// Shrinks `area` by `insets`, clamping so a pathological reservation
/// (two bars wider than the screen) degenerates to an empty rect at
/// the far corner instead of an inside-out one.
pub(crate) fn shrink(area: Rect, insets: EdgeInsets) -> Rect {
    let left = insets.left.max(0);
    let top = insets.top.max(0);
    let width = (area.size.w as i32 - left - insets.right.max(0)).max(0);
    let height = (area.size.h as i32 - top - insets.bottom.max(0)).max(0);
    Rect::new(
        Point::new(area.pos.x + left.min(area.size.w as i32), area.pos.y + top.min(area.size.h as i32)),
        Size::new(width as u32, height as u32),
    )
}

/// One axis of anchored placement: where a `len`-long surface starts
/// inside an `area_len`-long box. Anchored to the near edge it hugs
/// it (plus margin); the far edge likewise; both or neither centers —
/// both-anchored with an explicit size is the protocol's "stretch
/// intent, fixed size" case and centering is what every wlr
/// compositor does with it.
fn axis_place(
    area_start: i32,
    area_len: i32,
    len: i32,
    near: bool,
    far: bool,
    margin_near: i32,
    margin_far: i32,
) -> i32 {
    match (near, far) {
        (true, false) => area_start + margin_near,
        (false, true) => area_start + area_len - margin_far - len,
        _ => area_start + (area_len - len) / 2,
    }
}

/// One axis of size resolution: a client-requested 0 means "stretch
/// across the anchored axis" (the protocol only allows it when both
/// edges of the axis are anchored), which is the area minus both
/// margins; anything else is the client's own number. Floored at 1 so
/// a margin wider than the output cannot invert the rectangle.
fn axis_size(requested: i32, area_len: i32, margin_near: i32, margin_far: i32) -> i32 {
    if requested > 0 {
        requested
    } else {
        (area_len - margin_near - margin_far).max(1)
    }
}

/// Places one layer surface inside `area` (physical): resolves its
/// size (stretch axes filled from the area), then anchors it. `size`
/// and `margins` are already physical.
pub(crate) fn anchored_rect(area: Rect, size: Size, anchor: Anchor, margins: EdgeInsets) -> Rect {
    let x = axis_place(
        area.pos.x,
        area.size.w as i32,
        size.w as i32,
        anchor.contains(Anchor::LEFT),
        anchor.contains(Anchor::RIGHT),
        margins.left,
        margins.right,
    );
    let y = axis_place(
        area.pos.y,
        area.size.h as i32,
        size.h as i32,
        anchor.contains(Anchor::TOP),
        anchor.contains(Anchor::BOTTOM),
        margins.top,
        margins.bottom,
    );
    Rect::new(Point::new(x, y), size)
}

/// Resolves the physical size a surface should be configured to inside
/// `area`: the client's request where it made one, the anchored span
/// (minus margins) where it asked to stretch.
pub(crate) fn resolved_size(requested: Size, area: Rect, margins: EdgeInsets) -> Size {
    Size::new(
        axis_size(requested.w as i32, area.size.w as i32, margins.left, margins.right) as u32,
        axis_size(requested.h as i32, area.size.h as i32, margins.top, margins.bottom) as u32,
    )
}

/// Adds one mapped surface's exclusive reservation to `insets` and
/// answers the inset the *next* surface's usable area should lose.
/// The zone is measured from the surface's anchored edge and, per the
/// spec, includes the margin on that edge — a bar 30 tall with a 10
/// margin reserves 40.
pub(crate) fn reserve(insets: &mut EdgeInsets, edge: Anchor, zone: i32, margins: EdgeInsets) {
    let zone = zone.max(0);
    match edge {
        Anchor::TOP => insets.top += zone + margins.top.max(0),
        Anchor::BOTTOM => insets.bottom += zone + margins.bottom.max(0),
        Anchor::LEFT => insets.left += zone + margins.left.max(0),
        Anchor::RIGHT => insets.right += zone + margins.right.max(0),
        _ => {}
    }
}

/// The overlapping part of two rects, or `None` when disjoint —
/// [`apply_workareas`] composes the shell's baseline with the layer
/// reservation through it. (protocols.rs carries its own private
/// copy; both are four lines and neither module may reach the other's.)
fn intersect(a: Rect, b: Rect) -> Option<Rect> {
    let left = a.pos.x.max(b.pos.x);
    let top = a.pos.y.max(b.pos.y);
    let right = (a.pos.x + a.size.w as i32).min(b.pos.x + b.size.w as i32);
    let bottom = (a.pos.y + a.size.h as i32).min(b.pos.y + b.size.h as i32);
    if right <= left || bottom <= top {
        return None;
    }
    Some(Rect::new(Point::new(left, top), Size::new((right - left) as u32, (bottom - top) as u32)))
}

// ---------------------------------------------------------------------
// Reconciliation.
// ---------------------------------------------------------------------

/// The factor between this surface's logical pixels and the ledger's
/// physical ones. Once mapped it is the scale the surface committed —
/// the factor the renderer draws it at, so layout and drawing describe
/// one rectangle. Before the first buffer there is nothing committed
/// to read (the attribute would answer its default of 1), so the
/// prediction is the outputs' advertised scale: that is the number a
/// scale-aware client is about to adopt, and configuring fuzzel's
/// stretch width in physical pixels on a 2x session would have it
/// commit a double-width buffer. A client that then ignores the
/// advertisement and maps at 1x is re-measured by what it actually
/// committed on the very next pass.
fn surface_factor(record: &LayerRecord, output_scale: f64) -> f64 {
    if record.mapped {
        crate::xdg::effective_surface_scale(
            crate::xdg::committed_surface_scale(record.surface.wl_surface()),
            output_scale,
        )
    } else {
        output_scale.max(0.125)
    }
}

/// The physical size of the surface's committed buffer contents, when
/// it has any — the truth the geometry must be kept around, exactly as
/// `xdg.rs::committed_content_size` keeps toplevel frames honest.
fn committed_physical_size(surface: &WlSurface, factor: f64) -> Option<Size> {
    with_renderer_surface_state(surface, |state| state.surface_size())
        .flatten()
        .filter(|size| size.w > 0 && size.h > 0)
        .map(|size| {
            Size::new(crate::xdg::scale_length(size.w, factor) as u32, crate::xdg::scale_length(size.h, factor) as u32)
        })
}

/// Client-committed layer state, read once per surface per pass.
fn cached_state(surface: &WlSurface) -> LayerSurfaceCachedState {
    with_states(surface, |states| {
        let mut guard = states.cached_state.get::<LayerSurfaceCachedState>();
        *guard.current()
    })
}

fn physical_margins(margin: Margins, factor: f64) -> EdgeInsets {
    EdgeInsets {
        left: crate::xdg::scale_length(margin.left, factor),
        right: crate::xdg::scale_length(margin.right, factor),
        top: crate::xdg::scale_length(margin.top, factor),
        bottom: crate::xdg::scale_length(margin.bottom, factor),
    }
}

/// The whole per-pass reconciliation: prune dead records, lay every
/// layer surface out against its output, send the configures those
/// layouts imply, settle exclusive keyboard focus, and feed the
/// exclusive zones into the workareas. Called once per dispatch pass
/// (beside `protocols::refresh`, before the damage test so a layout
/// change renders this frame) and from the commit handler for the
/// initial-configure case, where a client is blocked waiting on it.
pub(crate) fn refresh(comp: &mut Compositor) {
    let backend_dirty = std::mem::take(&mut comp.wm.backend_mut().layer_layout_dirty);
    let layout_changed = comp.layer_shell.needs_arrange || backend_dirty;
    let baseline_changed = comp.layer_shell.applied_workarea_revision != comp.wm.workarea_revision();
    if layout_changed {
        comp.layer_shell.needs_arrange = false;
        arrange(comp);
        sync_keyboard(comp);
    }
    if layout_changed || baseline_changed {
        apply_workareas(comp);
        comp.layer_shell.applied_workarea_revision = comp.wm.workarea_revision();
    }
}

/// Lays out every layer surface and records the reserved insets.
///
/// Two passes per output, mirroring the protocol's model: mapped
/// surfaces with a usable exclusive zone claim their strips first
/// (top-most layer first, then creation order — deterministic, and the
/// order wlroots allocates in), progressively shrinking the usable
/// area; then everything neutral is placed inside what remains, and
/// `DontCare` surfaces against the full output, which is exactly what
/// that value asks for.
fn arrange(comp: &mut Compositor) {
    let backend = comp.wm.backend_mut();
    let outputs: Vec<Rect> = backend.monitors.iter().map(|m| m.geometry).collect();
    // Each output's own fractional scale — the factor an unmapped
    // surface on it is predicted to adopt, and the correction basis for
    // a mapped one (`surface_factor`).
    let output_scales: Vec<f64> =
        (0..outputs.len()).map(|index| backend.monitor_scales.get(index).copied().unwrap_or(1.0)).collect();
    let had_dead = backend.layers.iter().any(|record| !record.surface.alive());
    if had_dead {
        backend.layers.retain(|record| record.surface.alive());
        backend.mark_damaged();
    }

    let mut reserved = vec![EdgeInsets::default(); outputs.len()];
    // (record index, resolved plan) for the configure sends below.
    for (output_index, full) in outputs.iter().copied().enumerate() {
        let mut insets = EdgeInsets::default();
        // Pass 1: exclusive strips, overlay-first.
        for band in [Layer::Overlay, Layer::Top, Layer::Bottom, Layer::Background] {
            for index in 0..backend.layers.len() {
                if backend.layers[index].output != output_index {
                    continue;
                }
                let cached = cached_state(backend.layers[index].surface.wl_surface());
                if cached.layer != band {
                    continue;
                }
                let record = &backend.layers[index];
                let (Some(edge), ExclusiveZone::Exclusive(zone)) =
                    (exclusive_edge(cached.anchor), cached.exclusive_zone)
                else {
                    continue;
                };
                let factor = surface_factor(record, output_scales[output_index]);
                let margins = physical_margins(cached.margin, factor);
                let usable = shrink(full, insets);
                plan_surface(backend, index, cached, usable, factor);
                // A hidden or declined surface occupies no strip: the
                // Dock and the windows take the space back the moment
                // the user switches Omarchy's bar off.
                if backend.layer_presented(&backend.layers[index]) {
                    reserve(
                        &mut insets,
                        edge,
                        crate::xdg::scale_length(zone.min(i32::MAX as u32) as i32, factor),
                        margins,
                    );
                }
            }
        }
        // Pass 2: everything else, inside the remaining usable area —
        // or the full output for a surface that said `DontCare`.
        let usable = shrink(full, insets);
        for index in 0..backend.layers.len() {
            if backend.layers[index].output != output_index {
                continue;
            }
            let cached = cached_state(backend.layers[index].surface.wl_surface());
            let exclusive = matches!(
                (exclusive_edge(cached.anchor), cached.exclusive_zone),
                (Some(_), ExclusiveZone::Exclusive(_))
            );
            if exclusive {
                continue; // already placed in pass 1
            }
            let area = match cached.exclusive_zone {
                ExclusiveZone::DontCare => full,
                _ => usable,
            };
            let factor = surface_factor(&backend.layers[index], output_scales[output_index]);
            plan_surface(backend, index, cached, area, factor);
        }
        reserved[output_index] = insets;
    }
    comp.layer_shell.reserved = reserved;
}

/// Lays out one surface inside `area`: updates its ledger record
/// (layer, interactivity, geometry) and sends the configure the layout
/// implies. Damage only when the visible geometry actually moved — a
/// pass over an idle desktop must not wake the renderer.
fn plan_surface(
    backend: &mut crate::state::WaylandBackend,
    index: usize,
    cached: LayerSurfaceCachedState,
    area: Rect,
    factor: f64,
) {
    let margins = physical_margins(cached.margin, factor);
    let requested = Size::new(
        crate::xdg::scale_length(cached.size.w, factor).max(0) as u32,
        crate::xdg::scale_length(cached.size.h, factor).max(0) as u32,
    );
    let configured = resolved_size(requested, area, margins);
    // The geometry is anchored around what the client actually drew
    // once it has drawn anything — a bottom-anchored surface hangs
    // from the bottom edge by its real height, not the configured one
    // it has not acked yet.
    let record = &backend.layers[index];
    let actual = record
        .mapped
        .then(|| committed_physical_size(record.surface.wl_surface(), factor))
        .flatten()
        .unwrap_or(configured);
    let geometry = anchored_rect(area, actual, cached.anchor, margins);

    // Configure in the client's own units. A fixed axis echoes the
    // client's number verbatim (dividing the multiplied value back
    // would not round-trip identically at a fractional factor, and the
    // client's literal is the safer identity either way); a stretch
    // axis is the physical span converted down by the factor the
    // client renders at.
    let configure_logical: (u32, u32) = (
        if cached.size.w > 0 {
            cached.size.w as u32
        } else {
            crate::xdg::physical_to_logical(configured.w as i32, factor).max(1) as u32
        },
        if cached.size.h > 0 {
            cached.size.h as u32
        } else {
            crate::xdg::physical_to_logical(configured.h as i32, factor).max(1) as u32
        },
    );
    let record = &mut backend.layers[index];
    record.layer = cached.layer;
    record.interactivity = cached.keyboard_interactivity;
    if record.geometry != geometry {
        record.geometry = geometry;
        backend.damage = true;
    }
    let surface = backend.layers[index].surface.clone();
    surface.with_pending_state(|state| {
        state.size = Some((configure_logical.0 as i32, configure_logical.1 as i32).into());
    });
    // Deduped internally: sends the initial configure a fresh surface
    // is blocked on, sends again when the size moved, stays silent on
    // every idle pass.
    surface.send_pending_configure();
}

/// Pins the keyboard to the top-most mapped `Top`/`Overlay` surface
/// that asked for *exclusive* interactivity — the lock-screen-adjacent
/// half of the protocol, and what a launcher like fuzzel relies on to
/// type into itself the instant it opens. When the holder goes away
/// the keyboard returns to whatever window `wm-core` believes is
/// focused, because `wm-core` was never told about the excursion —
/// deliberately: focus *policy* stays in the policy brain, and this is
/// a seat-level override with a seat-level end.
fn sync_keyboard(comp: &mut Compositor) {
    let want = exclusive_claimant(comp.wm.backend());
    let want_id = want.as_ref().map(|(id, _)| *id);
    if want_id == comp.layer_shell.exclusive_focus {
        return;
    }
    comp.layer_shell.exclusive_focus = want_id;
    if comp.wm.backend().locked {
        // The lock owns the keyboard outright; the bookkeeping above
        // still records who would hold it, for the unlock to restore.
        return;
    }
    let target = keyboard_target_given(comp, want);
    let Some(keyboard) = comp.seat.get_keyboard() else {
        return;
    };
    keyboard.set_focus(comp, target, SERIAL_COUNTER.next_serial());
}

/// The top-most mapped, presented `Top`/`Overlay` layer surface asking
/// for exclusive keyboard interactivity, with its id — [`LayerShell`]'s
/// `exclusive_focus` bookkeeping is this answer, remembered.
///
/// Overlay outranks Top; within a band the newest mapped claimant wins,
/// which is the "most recently opened wins" behavior every wlr
/// compositor exhibits.
fn exclusive_claimant(backend: &crate::state::WaylandBackend) -> Option<(LayerId, WlSurface)> {
    let claimant = |band: Layer| {
        backend
            .layers
            .iter()
            .rev()
            .find(|record| {
                backend.layer_presented(record)
                    && record.layer == band
                    && record.interactivity == KeyboardInteractivity::Exclusive
            })
            .map(|record| (record.id, record.surface.wl_surface().clone()))
    };
    claimant(Layer::Overlay).or_else(|| claimant(Layer::Top))
}

/// Where the keyboard belongs when no lock is holding it: THE one
/// answer, so that everything which has to put the seat back
/// ([`sync_keyboard`] when a claimant goes away, `lock::unlock` when
/// the screen unlocks) puts it in the same place.
///
/// Three rungs, highest first — an exclusive layer surface, then an
/// active focus grab's whitelist, then the window `wm-core` calls
/// focused. `wm-core` is never told about the top two: focus *policy*
/// stays in the policy brain, and both are seat-level overrides with
/// seat-level ends.
pub(crate) fn keyboard_target(comp: &Compositor) -> Option<WlSurface> {
    keyboard_target_given(comp, exclusive_claimant(comp.wm.backend()))
}

/// [`keyboard_target`] for a caller that has already asked
/// [`exclusive_claimant`] and needs the id half of the answer for its
/// own bookkeeping.
fn keyboard_target_given(comp: &Compositor, want: Option<(LayerId, WlSurface)>) -> Option<WlSurface> {
    match want {
        Some((_, surface)) => Some(surface),
        // Nobody claims exclusivity, so the keyboard goes to a window —
        // but not necessarily straight there. A focus grab
        // (`focus_grab.rs`) sits one rung below exclusive interactivity
        // and one above window focus, so if one is holding, the seat
        // lands on its whitelist instead. Without this arm an Omarchy
        // popout that is *also* a grab would lose the keyboard the
        // moment any unrelated launcher closed, because `sync_keyboard`
        // runs before `focus_grab::refresh` and that pass only
        // re-asserts focus on the passes something changed.
        None => focus_grab_target(comp).or_else(|| focused_window_surface(comp)),
    }
}

/// The surface an active focus grab holds the keyboard on, if one
/// does — [`keyboard_target_given`]'s middle rung, asked before it
/// falls back to window focus. `None` in every session where no client
/// has started a grab, which is every session not running a shell that
/// speaks `hyprland-focus-grab-v1`.
fn focus_grab_target(comp: &Compositor) -> Option<WlSurface> {
    if !comp.focus_grab.is_active() {
        return None;
    }
    // Ask through the same predicate the input path uses, so there is
    // one definition of "on the whitelist" in the crate: the currently
    // focused surface keeps the keyboard if it is already inside.
    let focused = comp.seat.get_keyboard().and_then(|keyboard| keyboard.current_focus());
    match focused {
        Some(surface) if !comp.focus_grab.escapes(Some(&surface), None) => Some(surface),
        _ => comp.focus_grab.keyboard_surface(),
    }
}

/// The surface of the window `wm-core` currently calls focused — where
/// the keyboard returns when a layer surface stops holding it.
pub(crate) fn focused_window_surface(comp: &Compositor) -> Option<WlSurface> {
    let focused = comp.wm.focused_client()?;
    let (_, client) = comp.wm.clients().find(|(id, _)| *id == focused)?;
    let record = comp.wm.backend().windows.get(&client.window)?;
    record.surface.alive().then(|| record.surface.wl_surface()).flatten()
}

/// Composes the layer reservations into the workareas — see the module
/// docs for why this re-asserts per pass instead of hooking the
/// shell's own workarea paths. Silent (and cheap: one `any()` over a
/// short vec) whenever no layer surface reserves anything and none did
/// last pass.
fn apply_workareas(comp: &mut Compositor) {
    let any = comp.layer_shell.reserved.iter().any(|insets| !insets.is_zero());
    if !any && !comp.layer_shell.reserved_last_pass {
        return;
    }
    comp.layer_shell.reserved_last_pass = any;
    let monitors = comp.wm.monitors();
    // The Dock steps out of the strips a bar reserves on the primary's
    // top and right edges — before the baseline is read below, since
    // a displaced column changes what the shell's own workarea says.
    // The pass that follows the last bar leaving pushes zero insets
    // here, which is what hangs the Dock back in its corner.
    let primary_insets = monitors
        .iter()
        .position(|monitor| monitor.primary)
        .and_then(|index| comp.layer_shell.reserved.get(index).copied())
        .unwrap_or_default();
    comp.shell.set_edge_reservation(&mut comp.wm, dock_reservation(primary_insets));
    let output_size = comp.wm.backend().output_size;
    let areas: Vec<Rect> = monitors
        .iter()
        .enumerate()
        .map(|(index, monitor)| {
            // The shell's baseline: its primary workarea (the Dock's
            // reservation), full geometry elsewhere — the same rects
            // `Shell::apply_workareas` would set.
            let base = if monitor.primary { comp.shell.workarea(output_size) } else { monitor.geometry };
            let insets = comp.layer_shell.reserved.get(index).copied().unwrap_or_default();
            let carved = shrink(monitor.geometry, insets);
            // Disjoint only when a bar reserved the very strip the
            // shell's own area is — hand the carved rect through then,
            // because "the layer client owns that edge" is the truer
            // of the two claims.
            intersect(base, carved).unwrap_or(carved)
        })
        .collect();
    comp.wm.set_workareas(areas);
}

/// The part of a monitor's layer reservation the Dock can yield to:
/// the top and right strips, the two edges its corner touches. A
/// bottom or left bar shares no edge with a top-right column, so it
/// says nothing to it. Insets are non-negative by construction
/// (`reserve` only ever adds), so the clamp is belt and braces.
fn dock_reservation(insets: EdgeInsets) -> EdgeReservation {
    EdgeReservation { top: insets.top.max(0) as u32, right: insets.right.max(0) as u32 }
}

/// The commit-time half of the lifecycle, called from
/// `CompositorHandler::commit` for surfaces wearing the layer role.
/// Returns whether the surface was one, so the caller skips the
/// toplevel/popup role logic.
///
/// The initial-configure case is why this cannot wait for the next
/// dispatch pass's [`refresh`]: the client's first commit carries its
/// anchors and no buffer, and the client then *blocks* until a
/// configure arrives, so the arrangement (which computes the size that
/// configure must carry) runs right here.
pub(crate) fn handle_commit(comp: &mut Compositor, root: &WlSurface) -> bool {
    let Some(index) = comp.wm.backend().layers.iter().position(|record| record.surface.wl_surface() == root) else {
        return false;
    };
    let has_buffer = with_renderer_surface_state(root, |state| state.buffer().is_some()).unwrap_or(false);
    let transition = {
        let backend = comp.wm.backend_mut();
        let record = &mut backend.layers[index];
        if record.mapped == has_buffer {
            None
        } else {
            record.mapped = has_buffer;
            tracing::info!(id = record.id.0, namespace = %record.namespace, mapped = has_buffer, "layer surface map state changed");
            Some(record.namespace.clone())
        }
    };
    if let Some(namespace) = transition {
        comp.shell.set_layer_namespace_active(comp.wm.backend_mut(), &namespace, has_buffer);
        comp.wm.backend_mut().mark_damaged();
    }
    // Whether this was the blocked initial commit or a later one, the
    // full pass answers it: the initial configure goes out (the send
    // is deduped, so nothing extra travels otherwise), the geometry
    // absorbs whatever size the client actually committed, exclusive
    // zones re-reserve, and keyboard focus settles.
    comp.layer_shell.needs_arrange = true;
    refresh(comp);
    true
}

/// Whether a layer surface may take keyboard focus from a click —
/// on-demand interactivity, consulted by `input.rs`'s button routing.
pub(crate) fn accepts_focus_on_click(backend: &crate::state::WaylandBackend, id: LayerId) -> bool {
    backend
        .layers
        .iter()
        .find(|record| record.id == id)
        .is_some_and(|record| record.interactivity != KeyboardInteractivity::None)
}

// ---------------------------------------------------------------------
// Workaround: smithay's layer-shell pre-commit hook outlives its role.
// ---------------------------------------------------------------------

/// The exact condition smithay's layer-shell pre-commit hook treats as
/// a fatal client mistake: a zero width without both horizontal
/// anchors, or a zero height without both vertical ones. Restated here
/// because [`install_orphaned_role_guard`] has to answer "would
/// upstream kill this client?" *before* upstream is asked, and the
/// only honest way to answer it is to ask the same question in the
/// same words. Upstream's copy is the hook installed around line 125
/// of `smithay-0.7.0/src/wayland/shell/wlr_layer/handlers.rs`.
fn upstream_rejects(pending: &LayerSurfaceCachedState) -> bool {
    (pending.size.w == 0 && !pending.anchor.anchored_horizontally())
        || (pending.size.h == 0 && !pending.anchor.anchored_vertically())
}

/// Rewrites the pending state of a surface whose layer role is gone
/// into a shape [`upstream_rejects`] accepts at any size: all four
/// anchors, the one anchor value that satisfies both halves of that
/// check unconditionally.
///
/// Only `anchor` is touched, and the rewrite stays invisible even in
/// the one case where the state can be read again. A client may
/// `get_layer_surface` the same `wl_surface` a second time — smithay's
/// `set_role` accepts a role it has already given — and smithay does
/// not clear the cached state when it does, so whatever is left here
/// is what that surface starts from. It makes no difference:
/// everywhere this module reads an anchor, all four edges and none
/// mean the same thing. [`axis_place`] centers on `(true, true)`
/// exactly as it centers on `(false, false)`; [`axis_size`] ignores
/// anchors entirely once the client has asked for a real size;
/// [`exclusive_edge`] calls both combinations neutral. The one branch
/// where all-four would differ is [`axis_size`]'s stretch, and it is
/// out of reach: a client that commits without asking for a size is
/// committing a zero, which is legal only when it anchored that axis
/// itself.
fn neutralize_orphan(pending: &mut LayerSurfaceCachedState) {
    pending.anchor = Anchor::all();
}

/// Installs the per-surface guard that stops a *destroyed* layer
/// surface from killing its client on the next commit. Called once per
/// surface from `CompositorHandler::new_surface`, beside the dmabuf
/// readiness hook; silent on every surface that never takes the layer
/// role.
///
/// # The upstream bug
///
/// `smithay-0.7.0/src/wayland/shell/wlr_layer/handlers.rs` adds a
/// pre-commit hook to the `wl_surface` when a layer surface is created
/// (around line 125) and never removes it — there is no
/// `remove_pre_commit_hook` call anywhere in that file, and the
/// `HookId` that would allow one is dropped on the floor. The hook
/// belongs to the `wl_surface`, so it outlives the role. Meanwhile
/// `destroyed` (around line 352 of the same file) resets the cached
/// state to `Default::default()` — size 0×0, no anchors — which is
/// precisely the shape the surviving hook calls a protocol error.
///
/// So a client that destroys its `zwlr_layer_surface_v1` and then
/// commits the same `wl_surface` again is killed by a hook belonging
/// to an object it already destroyed, over state it never wrote.
///
/// That sequence is neither hypothetical nor rare — it is how Qt
/// unmaps a layer surface: `zwlr_layer_surface_v1.destroy()`, then
/// `wl_surface.attach(nil)` and `commit()`, keeping the surface for
/// the next show. Quickshell is built that way, and Quickshell is the
/// whole of Omarchy's desktop shell, so without this guard the bar,
/// the menus and the notification surfaces die together the first time
/// the user closes a menu: one `zwlr_layer_surface_v1` error, code 1,
/// "width 0 requested without setting left and right anchors", and the
/// shell's connection is gone.
///
/// # Why a hook of our own, and why it runs first
///
/// smithay keeps pre-commit hooks in a `Vec` and invokes them in
/// registration order — `wayland/compositor/tree.rs`:
/// `add_pre_commit_hook` pushes, `invoke_pre_commit_hooks` iterates
/// the clone front to back. This guard is registered from
/// `new_surface`, which runs when the `wl_surface` is created;
/// smithay's is registered from `get_layer_surface`, which cannot
/// happen before the surface it is given exists. Ours is therefore
/// always earlier in that vector and always runs first, and the
/// pending state it leaves behind is what smithay's hook then reads.
///
/// # What this must not do
///
/// Silence upstream's error for a layer surface that is still alive. A
/// living client that commits a zero width without anchoring both
/// sides really has broken the protocol, and the compositor has no
/// size to give it; that error is correct and still goes out. The
/// guard therefore fires on exactly one condition — state upstream
/// will reject, on a surface smithay no longer lists as a layer
/// surface.
///
/// # Removing this
///
/// When smithay calls `remove_pre_commit_hook` in the layer surface's
/// `destroyed`, delete this function, [`upstream_rejects`],
/// [`neutralize_orphan`], the call in `xdg.rs`'s `new_surface`, and
/// the tests that name them. Nothing else refers to any of it.
pub(crate) fn install_orphaned_role_guard(surface: &WlSurface) {
    add_pre_commit_hook::<Compositor, _>(surface, |comp, _dh, surface| {
        // One cheap read first: this runs on the commit path of every
        // surface in the session, and for all but a handful the answer
        // is "never had the layer role".
        let pending = with_states(surface, |states| {
            if states.role != Some(LAYER_SURFACE_ROLE) {
                return None;
            }
            let mut cached = states.cached_state.get::<LayerSurfaceCachedState>();
            Some(*cached.pending())
        });
        let Some(pending) = pending else {
            return;
        };
        if !upstream_rejects(&pending) {
            return;
        }
        // State upstream will object to. Whether the objection is
        // right turns on one thing: whether the role is still there to
        // object on behalf of. smithay drops the surface from
        // `known_layers` in the same `destroyed` that resets the
        // state, so that list is the authority — and it is the very
        // list whose mutation opens this hole.
        if comp.layer_shell.state.layer_surfaces().any(|layer| layer.wl_surface() == surface) {
            return;
        }
        with_states(surface, |states| {
            let mut cached = states.cached_state.get::<LayerSurfaceCachedState>();
            neutralize_orphan(cached.pending());
        });
        tracing::debug!("neutralized a commit on a destroyed layer surface — smithay's stale pre-commit hook");
    });
}

// ---------------------------------------------------------------------
// Protocol handler.
// ---------------------------------------------------------------------

impl WlrLayerShellHandler for Compositor {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell.state
    }

    fn new_layer_surface(
        &mut self,
        surface: LayerSurface,
        wl_output: Option<WlOutput>,
        layer: Layer,
        namespace: String,
    ) {
        // The client's output by name, the primary when it named none
        // (the protocol leaves that choice to the compositor, and the
        // primary is where this desktop's user is looking).
        let output = wl_output
            .as_ref()
            .and_then(Output::from_resource)
            .and_then(|named| self.outputs.iter().position(|entry| entry.output == named))
            .unwrap_or(0);
        let backend = self.wm.backend_mut();
        let id = LayerId(backend.alloc_id());
        tracing::info!(id = id.0, ?layer, %namespace, output, "new layer surface");
        if declined(layer, &namespace) {
            tracing::info!(id = id.0, "declining Omarchy's background surface: the desk stays chonkstep's");
        }
        backend.layers.push(LayerRecord {
            id,
            surface,
            output,
            layer,
            interactivity: KeyboardInteractivity::None,
            geometry: Rect::default(),
            mapped: false,
            namespace,
        });
        // No configure yet: the protocol forbids one before the
        // client's initial commit, which is where `handle_commit`
        // sends it.
    }

    fn new_popup(&mut self, _parent: LayerSurface, popup: PopupSurface) {
        // Tracked like any xdg popup; the renderer and the hit walk
        // find it through `PopupManager::popups_for_surface` on the
        // layer surface, exactly as they do for a toplevel's menus.
        if let Err(error) = self.popups.track_popup(smithay::desktop::PopupKind::from(popup)) {
            tracing::warn!(?error, "failed to track a layer surface's popup");
        }
    }

    fn layer_destroyed(&mut self, surface: LayerSurface) {
        let removed = self.wm.backend().layers.iter().find(|record| record.surface == surface)
            .map(|record| (record.id, record.namespace.clone(), record.mapped));
        if let Some((id, namespace, mapped)) = &removed {
            tracing::info!(id = id.0, namespace = %namespace, "layer surface destroyed");
            if *mapped {
                self.shell.set_layer_namespace_active(self.wm.backend_mut(), namespace, false);
            }
        }
        let backend = self.wm.backend_mut();
        backend.layers.retain(|record| record.surface != surface);
        backend.mark_damaged();
        self.layer_shell.needs_arrange = true;
        // Focus, exclusive zones and workareas settle on the pass this
        // destroy was dispatched in — `refresh` runs before the damage
        // test either way.
    }
}

delegate_layer_shell!(Compositor);

#[cfg(test)]
mod tests {
    use super::*;

    // The layout arithmetic is the part a protocol test cannot reach
    // without a client on a socket, and the part whose failures are
    // silent: a bar drawn at the wrong edge, a launcher off-center, a
    // workarea that never gave the strip back. All physical pixels, as
    // the callers convert before calling.

    fn output() -> Rect {
        Rect::new(Point::new(0, 0), Size::new(2560, 1600))
    }

    #[test]
    fn a_full_width_bar_hugs_its_edge_and_stretches_between_margins() {
        // A waybar: TOP|LEFT|RIGHT, height 40, no margins.
        let anchor = Anchor::TOP | Anchor::LEFT | Anchor::RIGHT;
        let size = resolved_size(Size::new(0, 40), output(), EdgeInsets::default());
        assert_eq!(size, Size::new(2560, 40));
        let rect = anchored_rect(output(), size, anchor, EdgeInsets::default());
        assert_eq!(rect, Rect::new(Point::new(0, 0), Size::new(2560, 40)));
    }

    #[test]
    fn margins_inset_both_the_stretch_and_the_anchor() {
        let anchor = Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT;
        let margins = EdgeInsets { left: 10, right: 10, top: 0, bottom: 8 };
        let size = resolved_size(Size::new(0, 60), output(), margins);
        assert_eq!(size, Size::new(2540, 60));
        let rect = anchored_rect(output(), size, anchor, margins);
        // 10 in from the left, 8 up from the bottom.
        assert_eq!(rect.pos, Point::new(10, 1600 - 8 - 60));
    }

    #[test]
    fn an_unanchored_surface_is_centered() {
        // fuzzel's launcher panel: fixed size, no anchors.
        let rect = anchored_rect(output(), Size::new(600, 400), Anchor::empty(), EdgeInsets::default());
        assert_eq!(rect.pos, Point::new((2560 - 600) / 2, (1600 - 400) / 2));
    }

    #[test]
    fn a_corner_anchor_lands_in_the_corner() {
        // mako's default: TOP|RIGHT with margins.
        let margins = EdgeInsets { left: 0, right: 10, top: 10, bottom: 0 };
        let rect = anchored_rect(output(), Size::new(300, 100), Anchor::TOP | Anchor::RIGHT, margins);
        assert_eq!(rect.pos, Point::new(2560 - 10 - 300, 10));
    }

    #[test]
    fn the_exclusive_edge_is_the_single_anchored_edge_or_the_bars() {
        assert_eq!(exclusive_edge(Anchor::TOP), Some(Anchor::TOP));
        assert_eq!(exclusive_edge(Anchor::TOP | Anchor::LEFT | Anchor::RIGHT), Some(Anchor::TOP));
        assert_eq!(exclusive_edge(Anchor::LEFT | Anchor::TOP | Anchor::BOTTOM), Some(Anchor::LEFT));
        // A corner, two parallel edges, everything, nothing: no single
        // edge to reserve from — the spec's "treat as neutral" cases.
        assert_eq!(exclusive_edge(Anchor::TOP | Anchor::LEFT), None);
        assert_eq!(exclusive_edge(Anchor::LEFT | Anchor::RIGHT), None);
        assert_eq!(exclusive_edge(Anchor::all()), None);
        assert_eq!(exclusive_edge(Anchor::empty()), None);
    }

    #[test]
    fn reservations_stack_and_include_the_edge_margin() {
        let mut insets = EdgeInsets::default();
        reserve(&mut insets, Anchor::TOP, 40, EdgeInsets { top: 10, ..Default::default() });
        assert_eq!(insets.top, 50, "the zone includes its margin per the spec");
        reserve(&mut insets, Anchor::TOP, 20, EdgeInsets::default());
        assert_eq!(insets.top, 70, "two bars on one edge stack");
        reserve(&mut insets, Anchor::LEFT, 64, EdgeInsets::default());
        assert_eq!((insets.left, insets.right, insets.bottom), (64, 0, 0));
    }

    #[test]
    fn shrinking_by_the_reservation_yields_the_usable_area() {
        let insets = EdgeInsets { top: 40, left: 64, right: 0, bottom: 0 };
        let usable = shrink(output(), insets);
        assert_eq!(usable, Rect::new(Point::new(64, 40), Size::new(2560 - 64, 1600 - 40)));
        // A later surface stretched inside it never overlaps the bar.
        let rect = anchored_rect(
            usable,
            resolved_size(Size::new(0, 0), usable, EdgeInsets::default()),
            Anchor::all(),
            EdgeInsets::default(),
        );
        assert_eq!(rect, usable);
    }

    #[test]
    fn a_pathological_reservation_degrades_to_an_empty_rect() {
        // Two "bars" together taller than the screen must clamp, not
        // invert: an inside-out workarea would place windows at
        // negative sizes.
        let insets = EdgeInsets { top: 1000, bottom: 1000, left: 0, right: 0 };
        let usable = shrink(output(), insets);
        assert_eq!(usable.size.h, 0);
        assert!(usable.size.w == 2560);
    }

    #[test]
    fn only_the_top_and_right_insets_reach_the_dock() {
        // A top bar with a left dock-style panel (waybar + nwg-dock):
        // the Dock steps under the bar and ignores the panel entirely.
        let insets = EdgeInsets { top: 40, left: 64, right: 0, bottom: 0 };
        assert_eq!(dock_reservation(insets), EdgeReservation { top: 40, right: 0 });
        // A right-edge panel and a bottom bar: only the panel counts.
        let insets = EdgeInsets { top: 0, left: 0, right: 48, bottom: 32 };
        assert_eq!(dock_reservation(insets), EdgeReservation { top: 0, right: 48 });
        // No bars at all is the reservation the Dock started with, so
        // the pass after the last bar leaves puts it back exactly.
        assert_eq!(dock_reservation(EdgeInsets::default()), EdgeReservation::default());
    }

    #[test]
    fn only_omarchys_background_surface_is_declined() {
        assert!(declined(Layer::Background, OMARCHY_BACKGROUND_NAMESPACE));
        // Another wallpaper daemon on the same layer is the user's
        // choice, and stays.
        assert!(!declined(Layer::Background, "wallpaper"));
        // Omarchy's other surfaces — its bar, its panels, its OSD —
        // are the whole point of hosting the shell.
        for namespace in ["omarchy-bar", "omarchy-panel", "omarchy-osd", "omarchy-notifications"] {
            for layer in [Layer::Background, Layer::Bottom, Layer::Top, Layer::Overlay] {
                assert!(!declined(layer, namespace), "{namespace:?} on {layer:?}");
            }
        }
        // And the plugin's own surface, moved off the background layer,
        // is no longer painting a wallpaper over the desk.
        for layer in [Layer::Bottom, Layer::Top, Layer::Overlay] {
            assert!(!declined(layer, OMARCHY_BACKGROUND_NAMESPACE), "{layer:?}");
        }
    }

    #[test]
    fn a_zero_inset_is_recognized_as_no_reservation() {
        // `apply_workareas` goes silent on this test — the common case
        // of a desktop with no bar running must not re-set workareas
        // sixty times a second.
        assert!(EdgeInsets::default().is_zero());
        assert!(!EdgeInsets { top: 1, ..Default::default() }.is_zero());
    }

    // The orphaned-role guard. The hook itself needs a client on a
    // socket to run, so what is pinned here is the pair of judgements
    // it makes — which pending states upstream kills for, and what
    // this compositor writes over them — plus the claim the rewrite
    // rests on: that all four anchors and none are the same anchor to
    // every reader in this module.

    fn sized(w: i32, h: i32, anchor: Anchor) -> LayerSurfaceCachedState {
        LayerSurfaceCachedState { size: (w, h).into(), anchor, ..Default::default() }
    }

    #[test]
    fn the_state_smithay_leaves_behind_is_the_state_it_kills_for() {
        // This is the whole bug in one assertion. `destroyed` in
        // smithay's wlr_layer/handlers.rs writes exactly this value
        // into the pending state, and the pre-commit hook it forgot to
        // remove reads exactly this value on the client's next commit.
        assert!(upstream_rejects(&LayerSurfaceCachedState::default()));
    }

    #[test]
    fn the_guard_leaves_a_state_upstream_accepts() {
        for mut pending in [
            LayerSurfaceCachedState::default(),
            sized(0, 0, Anchor::TOP),
            sized(0, 40, Anchor::empty()),
            sized(300, 0, Anchor::LEFT | Anchor::RIGHT),
        ] {
            assert!(upstream_rejects(&pending), "the fixture should be one upstream objects to");
            neutralize_orphan(&mut pending);
            assert!(!upstream_rejects(&pending));
        }
    }

    #[test]
    fn a_healthy_layer_surface_never_reaches_the_guard() {
        // Both shapes real clients commit, and the reason the guard
        // costs a live bar nothing: it returns before it ever consults
        // the shell's surface list.
        assert!(!upstream_rejects(&sized(0, 40, Anchor::TOP | Anchor::LEFT | Anchor::RIGHT)));
        assert!(!upstream_rejects(&sized(600, 400, Anchor::empty())));
    }

    #[test]
    fn the_guards_trigger_still_recognizes_a_real_protocol_violation() {
        // Half of "the guard is not a blanket amnesty": these are the
        // shapes a client has genuinely no right to commit, and
        // [`upstream_rejects`] must keep saying so.
        assert!(upstream_rejects(&sized(0, 40, Anchor::TOP)));
        assert!(upstream_rejects(&sized(600, 0, Anchor::LEFT)));
    }

    // The other half — that a *live* layer surface committing those
    // shapes still gets killed, because the guard returns early when
    // `layer_surfaces()` still lists the surface — has no unit test,
    // and the honest reason is that it needs a real `Compositor` with a
    // populated `WlrLayerShellState`, which is not constructible here.
    //
    // It is not untested, though; it is tested from outside. Against a
    // nested session, a client committing `set_size(0, 100)` with only
    // `Anchor::Top` while its layer surface is alive is killed with
    // `zwlr_layer_surface_v1` code 1, while the same client destroying
    // its role first survives (`examples/layer-role-repro` is that
    // client). That pair is the property this module actually promises.
    // If this file grows a way to build a `Compositor` in a test, this
    // is the first thing to bring inside.

    #[test]
    fn all_four_anchors_read_exactly_as_none_do() {
        // What lets [`neutralize_orphan`] write into state a
        // re-created layer surface might inherit: every reader of an
        // anchor in this module treats the two the same, so the
        // residue cannot move, resize or re-reserve anything.
        let size = Size::new(600, 400);
        assert_eq!(
            anchored_rect(output(), size, Anchor::all(), EdgeInsets::default()),
            anchored_rect(output(), size, Anchor::empty(), EdgeInsets::default()),
        );
        let margins = EdgeInsets { left: 10, right: 30, top: 4, bottom: 12 };
        assert_eq!(
            anchored_rect(output(), size, Anchor::all(), margins),
            anchored_rect(output(), size, Anchor::empty(), margins),
        );
        assert_eq!(exclusive_edge(Anchor::all()), exclusive_edge(Anchor::empty()));
    }
}
