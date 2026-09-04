//! ext-idle-notify-v1 (plus idle-inhibit-unstable-v1): idle timers for
//! lockers and power daemons, driven off the real input path.
//!
//! `swayidle` (and anything like it) binds `ext_idle_notifier_v1`,
//! asks for a notification after N ms of silence, and locks or dims
//! when it fires. Smithay's `IdleNotifierState` owns the whole timer
//! mechanism as calloop sources; the only integration a compositor
//! owes it is the truth about activity — one `notify_activity` call
//! from the input path ([`note_activity`], called by
//! `input::process_input_event` for every keyboard, button, motion and
//! axis event on either backend, since both funnel through that one
//! entry point).
//!
//! idle-inhibit rides along because smithay's delegate makes it two
//! callbacks: a client showing a video creates an inhibitor on its
//! surface, and while that surface is visible the idle timers pause.
//! "Visible" is judged from indexed ownership after relevant scene
//! edges ([`refresh`]): the inhibiting surface's window (or layer
//! surface) must belong to the renderer's current stack, and the
//! session must not be locked. An inhibitor therefore cannot keep the
//! screen awake from behind a lock screen, and a dead or parked
//! surface's inhibition ends whether or not its client remembered to
//! destroy the inhibitor (the spec explicitly leaves ignoring
//! invisible inhibitors to the compositor).
//!
//! `CHONKSTEP_IDLE_LOG=1` names each actual policy reconciliation. It
//! is deliberately edge-oriented telemetry: a busy client that changes
//! only pixels should produce no lines after the initial map.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::IsAlive;
use smithay::wayland::compositor::get_parent;
use smithay::wayland::idle_inhibit::{IdleInhibitHandler, IdleInhibitManagerState};
use smithay::wayland::idle_notify::{IdleNotifierHandler, IdleNotifierState};
use smithay::{delegate_idle_inhibit, delegate_idle_notify};

use crate::state::{Compositor, WaylandBackend};

/// Idle bookkeeping on [`Compositor`].
pub(crate) struct Idle {
    pub notifier: IdleNotifierState<Compositor>,
    /// Held so the global stays registered; the state object itself is
    /// never consulted again.
    pub _inhibit: IdleInhibitManagerState,
    /// Surfaces with a live `zwp_idle_inhibitor_v1`. Liveness and
    /// visibility are judged after invalidation in [`refresh`].
    pub inhibitors: Vec<WlSurface>,
    /// A protocol inhibitor was added or removed. Window/layer/lock
    /// visibility edges carry their own flag on `WaylandBackend`.
    inhibitors_dirty: bool,
}

impl Idle {
    pub(crate) fn new(notifier: IdleNotifierState<Compositor>, inhibit: IdleInhibitManagerState) -> Self {
        Self { notifier, _inhibit: inhibit, inhibitors: Vec::new(), inhibitors_dirty: true }
    }
}

impl IdleNotifierHandler for Compositor {
    fn idle_notifier_state(&mut self) -> &mut IdleNotifierState<Compositor> {
        &mut self.idle.notifier
    }
}

impl IdleInhibitHandler for Compositor {
    fn inhibit(&mut self, surface: WlSurface) {
        self.idle.inhibitors.push(surface);
        self.idle.inhibitors_dirty = true;
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        let before = self.idle.inhibitors.len();
        self.idle.inhibitors.retain(|held| *held != surface);
        self.idle.inhibitors_dirty |= self.idle.inhibitors.len() != before;
    }
}

delegate_idle_notify!(Compositor);
delegate_idle_inhibit!(Compositor);

/// Reports one unit of user activity to every idle timer. Called from
/// the input translation for each real input event — the compositor is
/// the input path, so no heuristic stands between a keypress and the
/// timer reset.
pub(crate) fn note_activity(comp: &mut Compositor) {
    let seat = comp.seat.clone();
    comp.idle.notifier.notify_activity(&seat);
}

/// Recomputes whether idling is inhibited after a protocol or scene-
/// visibility edge. Pixel commits and unrelated dispatches take two
/// booleans and return; an invalidated pass walks only two sparse sets:
/// windows whose rules explicitly inhibit idle, and protocol inhibitor
/// surfaces.
pub(crate) fn refresh(comp: &mut Compositor) {
    let inhibitors_changed = std::mem::take(&mut comp.idle.inhibitors_dirty);
    let visibility_changed = comp.wm.backend_mut().take_idle_policy_dirty();
    if !inhibitors_changed && !visibility_changed {
        return;
    }
    comp.idle.inhibitors.retain(IsAlive::alive);
    let inhibited = !comp.wm.backend().locked && {
        let rule_inhibited = comp.wm.rule_idle_inhibited();
        let backend = comp.wm.backend();
        rule_inhibited || comp.idle.inhibitors.iter().any(|surface| surface_visible(backend, surface))
    };
    comp.idle.notifier.set_is_inhibited(inhibited);
    if idle_log_enabled() {
        tracing::info!(
            inhibited,
            protocol_inhibitors = comp.idle.inhibitors.len(),
            "idle policy reconciled"
        );
    }
}

fn idle_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("CHONKSTEP_IDLE_LOG").is_some_and(|value| value != "0"))
}

/// Whether the window or layer surface owning `surface` is mapped.
/// Judged against the tree's root because an inhibitor may sit on a
/// subsurface (a video element) of the toplevel the ledger records.
fn surface_visible(backend: &WaylandBackend, surface: &WlSurface) -> bool {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    let window_presented = backend
        .window_for_surface(&root)
        .is_some_and(|window| crate::xdg::window_is_in_scene(backend, window));
    let layer_mapped =
        backend.layers.iter().any(|record| backend.layer_presented(record) && *record.surface.wl_surface() == root);
    window_presented || layer_mapped
}
