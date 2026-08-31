//! ext-session-lock-v1: the desktop's ability to lock at all, and the
//! one protocol here where a partial implementation is worse than
//! none. The spec's contract (read it in full:
//! `/usr/share/wayland-protocols/staging/ext-session-lock/`) reduces
//! to three absolutes this module and its hooks enforce:
//!
//! - **While locked, only lock surfaces render.** `renderer::
//!   build_scene` branches on the ledger's `locked` flag before it
//!   walks anything else, so the scene behind the lock cannot leak
//!   through any path that draws — the on-screen frame, `grim` via
//!   screencopy, and the screenshot marker all go through that one
//!   function. An output with no lock surface (the locker has not
//!   drawn yet, the locker crashed, a fresh output appeared) is
//!   cleared to black rather than showing anything.
//! - **While locked, input reaches only lock surfaces.** `input.rs`
//!   branches to a dedicated locked path at the top of every entry
//!   point: no shell clicks, no WM events, no keybindings — only seat
//!   delivery against the lock surface, plus VT switching, which the
//!   spec explicitly leaves to the compositor and which is the user's
//!   only escape hatch on real hardware.
//! - **Only `unlock_and_destroy` unlocks.** The lock *state* lives in
//!   this compositor ([`LockMachine`]), not in the locker process: a
//!   locker that crashes takes its surfaces with it and leaves the
//!   state `Locked`, so the session blanks and stays blanked until a
//!   new locker binds the manager and takes over — the spec's
//!   recovery path, and the difference between a lock screen and a
//!   screensaver.
//!
//! The `locked` event is only sent after a locked frame has actually
//! been *presented* (`confirm_after_frame`, called from the dispatch
//! loop after the render): the spec demands the compositor blank
//! before telling the locker it is safe, because the locker's caller
//! (a suspend hook, say) proceeds on that event.

use smithay::delegate_session_lock;
use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_v1::ExtSessionLockV1;
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::Resource;
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::session_lock::{
    LockSurface, SessionLockHandler, SessionLockManagerState, SessionLocker,
};

use wm_core::BackendEvent;

use crate::state::Compositor;

/// One lock surface and the output it covers (an index into
/// `Compositor::outputs`/the monitor list — the same positional
/// identity everything else in this crate uses for outputs).
pub(crate) struct LockSurfaceEntry {
    pub output: usize,
    pub surface: LockSurface,
}

/// The compositor-side lock state: smithay's protocol state, the
/// lifecycle machine, the not-yet-confirmed locker, and the protocol
/// handle of whoever holds the lock (kept to tell a live locker from
/// a dead one when a second lock request arrives).
pub(crate) struct SessionLock {
    pub state: SessionLockManagerState,
    pub machine: LockMachine,
    /// The confirmation owed to the locking client, held until a
    /// locked frame has been presented. Dropping it unconfirmed sends
    /// `finished`, which is exactly right for every failure path.
    pub pending_ack: Option<SessionLocker>,
    /// The `ext_session_lock_v1` of the current holder; its liveness
    /// is what distinguishes "someone else holds the lock" (deny)
    /// from "the locker crashed" (allow a new one to recover).
    pub holder: Option<ExtSessionLockV1>,
}

impl SessionLock {
    pub(crate) fn new(state: SessionLockManagerState) -> Self {
        Self { state, machine: LockMachine::default(), pending_ack: None, holder: None }
    }
}

/// What a lock request is answered with.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum LockRequest {
    /// Take the lock: blank now, confirm after the next presented
    /// frame.
    Accept,
    /// A live locker already holds the session — the newcomer gets
    /// `finished` (by dropping its confirmation) and the session state
    /// does not move.
    Deny,
}

/// The lock lifecycle, pure so the transitions are testable without a
/// display. The invariant the whole type exists to pin: once
/// [`LockMachine::locked`] answers true, the ONLY transition that
/// makes it answer false again is [`LockMachine::unlocked`] — the
/// protocol's unlock-and-destroy. Locker death is deliberately not a
/// transition at all.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum LockMachine {
    #[default]
    Unlocked,
    /// The lock is in force (rendering and input are already
    /// restricted) but the `locked` event has not been sent: a blanked
    /// frame has not reached the screen yet.
    Locking,
    /// Locked and confirmed.
    Locked,
}

impl LockMachine {
    /// Whether the session is under the lock's restrictions. True from
    /// the instant a lock is accepted — the gap between accepting and
    /// confirming must already render nothing and route nothing, or
    /// the first frames of a lock leak the desktop.
    pub(crate) fn locked(&self) -> bool {
        !matches!(self, LockMachine::Unlocked)
    }

    /// Answers a client's lock request. `holder_alive` is whether the
    /// previous locker's protocol handle still exists — false both
    /// when there never was one and when it crashed, and a crashed
    /// locker's session must accept a new one (the spec's recovery
    /// path: the alternative is a session locked forever with nothing
    /// to type a password into).
    pub(crate) fn request_lock(&mut self, holder_alive: bool) -> LockRequest {
        match self {
            LockMachine::Unlocked => {
                *self = LockMachine::Locking;
                LockRequest::Accept
            }
            LockMachine::Locking | LockMachine::Locked if !holder_alive => {
                // Recovery: the state stays locked throughout — a new
                // locker joining a dead one's session re-enters
                // `Locking` so its `locked` event also waits for a
                // presented frame.
                *self = LockMachine::Locking;
                LockRequest::Accept
            }
            _ => LockRequest::Deny,
        }
    }

    /// A locked frame reached the screen. Answers whether the pending
    /// `locked` confirmation should be sent now (exactly once per
    /// accepted lock).
    pub(crate) fn frame_presented(&mut self) -> bool {
        if matches!(self, LockMachine::Locking) {
            *self = LockMachine::Locked;
            true
        } else {
            false
        }
    }

    /// The protocol's `unlock_and_destroy` — the one and only way out.
    pub(crate) fn unlocked(&mut self) {
        *self = LockMachine::Unlocked;
    }
}

impl SessionLockHandler for Compositor {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self.session_lock.state
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        let holder_alive = self
            .session_lock
            .holder
            .as_ref()
            .is_some_and(|holder| holder.is_alive());
        match self.session_lock.machine.request_lock(holder_alive) {
            LockRequest::Deny => {
                tracing::info!("refusing a session lock: a live locker already holds one");
                // Dropping the confirmation sends `finished` to the
                // newcomer; the session's lock state never moved.
                drop(confirmation);
            }
            LockRequest::Accept => {
                tracing::info!("session locking; blanking outputs until the locker draws");
                self.session_lock.holder = Some(confirmation.ext_session_lock().clone());
                self.session_lock.pending_ack = Some(confirmation);
                let backend = self.wm.backend_mut();
                backend.locked = true;
                // A recovering locker inherits no surfaces from the
                // dead one.
                backend.lock_surfaces.clear();
                // A drag in flight belongs to a desktop that just
                // stopped existing for input purposes: end it, tell
                // `wm-core` (its handler is idempotent), and drop the
                // implicit grab so a button held across the lock
                // cannot route the first post-unlock events to a
                // pre-lock target.
                backend.end_pointer_grab();
                backend.queue(BackendEvent::DragEnded);
                backend.mark_damaged();
                let seat = self.seat.clone();
                crate::input::clear_implicit_grab(&seat);
                // Focus leaves whatever window held it *now*: the very
                // next key must not reach a client behind the lock,
                // and the lock surface (which does not exist yet)
                // claims focus in `new_surface`.
                if let Some(keyboard) = self.seat.get_keyboard() {
                    keyboard.set_focus(self, None, SERIAL_COUNTER.next_serial());
                }
            }
        }
    }

    fn unlock(&mut self) {
        tracing::info!("session unlocked");
        self.session_lock.machine.unlocked();
        self.session_lock.pending_ack = None;
        self.session_lock.holder = None;
        let backend = self.wm.backend_mut();
        backend.locked = false;
        backend.lock_surfaces.clear();
        backend.mark_damaged();
        // The keyboard goes home: `wm-core` still believes its focused
        // window is focused (nothing told it otherwise — deliberately,
        // the lock is a seat-level override), so pointing the seat
        // back at that window's surface is all restoration takes.
        let target = crate::layers::focused_window_surface(self);
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, target, SERIAL_COUNTER.next_serial());
        }
        // Unlocking is user activity by definition — without this an
        // idle timer that expired behind the lock re-fires instantly.
        crate::idle::note_activity(self);
    }

    fn new_surface(&mut self, surface: LockSurface, wl_output: WlOutput) {
        let index = Output::from_resource(&wl_output)
            .and_then(|named| self.outputs.iter().position(|entry| entry.output == named))
            .unwrap_or(0);
        let (physical, advertised) = {
            let entry = &self.outputs[index];
            (entry.size, crate::state::advertised_output_scale(self.ui_scale).integer_scale().max(1))
        };
        // The configure is in the client's logical units; the client
        // multiplies back up by the scale the output advertises. An
        // odd physical extent loses its last pixel to the division —
        // the buffer lands one short and the clear color (black)
        // shows through the sliver, which on a lock screen is the
        // correct color to leak.
        let logical = (
            (physical.w as i32 / advertised).max(1) as u32,
            (physical.h as i32 / advertised).max(1) as u32,
        );
        surface.with_pending_state(|state| {
            state.size = Some(logical.into());
        });
        // smithay sends the initial configure right after this handler
        // returns, carrying the size set above.
        tracing::info!(output = index, w = logical.0, h = logical.1, "lock surface created");
        // The locker types its password somewhere: the first lock
        // surface takes keyboard focus (a multi-output locker creates
        // one per output; the primary's usually arrives first, and any
        // of them reaches the same client).
        let focus_target = self.wm.backend().lock_surfaces.is_empty();
        self.wm.backend_mut().lock_surfaces.push(LockSurfaceEntry { output: index, surface: surface.clone() });
        self.wm.backend_mut().mark_damaged();
        if focus_target {
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, Some(surface.wl_surface().clone()), SERIAL_COUNTER.next_serial());
            }
        }
    }
}

delegate_session_lock!(Compositor);

/// Sends the owed `locked` confirmation once a locked frame has been
/// presented. Called from `dispatch_pending` after the render: a
/// cleared damage flag (with no per-output flip still pending on the
/// session backend) means the frame the renderer just submitted was
/// built under the lock, because the lock set the flag when it landed
/// and every frame since branches into the locked scene.
pub(crate) fn confirm_after_frame(comp: &mut Compositor) {
    if comp.session_lock.pending_ack.is_none() || !comp.session_lock.machine.locked() {
        return;
    }
    let presented =
        !comp.wm.backend().damage && !crate::session::redraw_pending(&comp.graphics);
    if !presented {
        return;
    }
    if comp.session_lock.machine.frame_presented() {
        if let Some(locker) = comp.session_lock.pending_ack.take() {
            tracing::info!("locked frame presented; confirming the session lock");
            locker.lock();
        }
    }
}

/// Per-pass upkeep while locked: keep every lock surface configured to
/// its output's current size (an output resize mid-lock re-configures;
/// the send is deduped so idle passes cost a comparison), and keep the
/// keyboard on a lock surface — a locker that maps its surface after
/// the lock was granted has no other path to focus.
pub(crate) fn refresh(comp: &mut Compositor) {
    if !comp.wm.backend().locked {
        return;
    }
    let advertised = crate::state::advertised_output_scale(comp.ui_scale).integer_scale().max(1);
    let sizes: Vec<(usize, LockSurface)> = comp
        .wm
        .backend()
        .lock_surfaces
        .iter()
        .filter(|entry| entry.surface.alive())
        .map(|entry| (entry.output, entry.surface.clone()))
        .collect();
    for (output, surface) in sizes {
        let Some(entry) = comp.outputs.get(output) else { continue };
        let logical = (
            (entry.size.w as i32 / advertised).max(1) as u32,
            (entry.size.h as i32 / advertised).max(1) as u32,
        );
        surface.with_pending_state(|state| {
            state.size = Some(logical.into());
        });
        // Dedups internally: sends only when the size actually moved.
        surface.send_configure();
    }
    // A locker whose focused surface died (output unplugged, client
    // recovering) gets the keyboard onto another of its surfaces; a
    // session with no live lock surface keeps focus on nothing, which
    // is exactly what a blanked, keyboardless lock should hold.
    let focused_dead = comp
        .seat
        .get_keyboard()
        .and_then(|keyboard| keyboard.current_focus())
        .map(|surface| !comp.wm.backend().lock_surfaces.iter().any(|entry| {
            entry.surface.alive() && *entry.surface.wl_surface() == surface
        }))
        .unwrap_or(true);
    if focused_dead {
        let next = comp
            .wm
            .backend()
            .lock_surfaces
            .iter()
            .find(|entry| entry.surface.alive())
            .map(|entry| entry.surface.wl_surface().clone());
        if let Some(keyboard) = comp.seat.get_keyboard() {
            keyboard.set_focus(comp, next, SERIAL_COUNTER.next_serial());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The lifecycle is the security property, so it is pinned as pure
    // transitions: every path that could unlock without the protocol's
    // unlock is a failure a screenshot cannot reliably catch.

    #[test]
    fn a_fresh_session_is_unlocked_and_a_lock_is_accepted() {
        let mut machine = LockMachine::default();
        assert!(!machine.locked());
        assert_eq!(machine.request_lock(false), LockRequest::Accept);
        assert!(machine.locked(), "restrictions apply from the instant of acceptance");
    }

    #[test]
    fn the_locked_event_waits_for_a_presented_frame_and_fires_once() {
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        assert!(machine.frame_presented(), "the first locked frame confirms");
        assert!(!machine.frame_presented(), "and only the first");
        assert!(machine.locked());
    }

    #[test]
    fn a_second_lock_is_refused_while_the_first_locker_lives() {
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        machine.frame_presented();
        assert_eq!(machine.request_lock(true), LockRequest::Deny);
        assert!(machine.locked(), "a refused request must not disturb the lock");
    }

    #[test]
    fn a_crashed_locker_leaves_the_session_locked() {
        // The centerpiece: killing swaylock must NOT unlock. There is
        // no "locker died" transition at all — the machine cannot
        // express one.
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        machine.frame_presented();
        // ... the locker process dies here; nothing calls unlocked().
        assert!(machine.locked());
    }

    #[test]
    fn a_new_locker_may_recover_a_dead_lockers_session() {
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        machine.frame_presented();
        // Holder dead: the recovery request is accepted, the session
        // stays under lock the whole way through, and the newcomer's
        // confirmation waits for a presented frame again.
        assert_eq!(machine.request_lock(false), LockRequest::Accept);
        assert!(machine.locked());
        assert!(machine.frame_presented());
    }

    #[test]
    fn unlock_is_the_only_way_out() {
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        machine.frame_presented();
        machine.unlocked();
        assert!(!machine.locked());
    }

    #[test]
    fn a_lock_that_dies_before_confirming_still_holds() {
        // The locker crashed in the gap between `lock` and its first
        // frame: the session is in `Locking`, which is locked — the
        // blank must hold, and a recovering locker re-enters cleanly.
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        assert!(machine.locked());
        assert_eq!(machine.request_lock(false), LockRequest::Accept);
        assert!(machine.locked());
    }
}
