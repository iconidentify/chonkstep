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
//! "Visible" is judged from the ledger each pass ([`refresh`]): the
//! inhibiting surface's window (or layer surface) is mapped and the
//! session is not locked — an inhibitor must not keep the screen awake
//! from behind a lock screen, and a dead or unmapped surface's
//! inhibition ends whether or not its client remembered to destroy the
//! inhibitor (the spec explicitly leaves ignoring invisible inhibitors
//! to the compositor).

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
    /// visibility are judged per pass in [`refresh`], not here.
    pub inhibitors: Vec<WlSurface>,
}

impl Idle {
    pub(crate) fn new(notifier: IdleNotifierState<Compositor>, inhibit: IdleInhibitManagerState) -> Self {
        Self { notifier, _inhibit: inhibit, inhibitors: Vec::new() }
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
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.idle.inhibitors.retain(|held| *held != surface);
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

/// Recomputes whether idling is inhibited, once per dispatch pass.
/// `set_is_inhibited` is a no-op on an unchanged answer, so the steady
/// state costs one walk over a nearly-always-empty vec.
pub(crate) fn refresh(comp: &mut Compositor) {
    comp.idle.inhibitors.retain(IsAlive::alive);
    let inhibited = !comp.wm.backend().locked
        && {
            let backend = comp.wm.backend();
            comp.idle
                .inhibitors
                .iter()
                .any(|surface| surface_visible(backend, surface))
        };
    comp.idle.notifier.set_is_inhibited(inhibited);
}

/// Whether the window or layer surface owning `surface` is mapped.
/// Judged against the tree's root because an inhibitor may sit on a
/// subsurface (a video element) of the toplevel the ledger records.
fn surface_visible(backend: &WaylandBackend, surface: &WlSurface) -> bool {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    let window_mapped = backend.windows.values().any(|record| {
        record.mapped && record.surface.wl_surface().as_ref() == Some(&root)
    });
    let layer_mapped = backend
        .layers
        .iter()
        .any(|record| record.mapped && *record.surface.wl_surface() == root);
    window_mapped || layer_mapped
}
