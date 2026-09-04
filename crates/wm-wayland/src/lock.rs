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
//! - **Only the holder's `unlock_and_destroy` unlocks.** The lock
//!   *state* lives in this compositor ([`LockMachine`]), not in the
//!   locker process: a locker that crashes takes its surfaces with it
//!   and leaves the state `Locked`, so the session blanks and stays
//!   blanked until a new locker binds the manager and takes over —
//!   the spec's recovery path, and the difference between a lock
//!   screen and a screensaver. And the request is authenticated
//!   against the recorded holder before it is honoured: the manager
//!   global is advertised to every client, so "a client asked to
//!   unlock" and "the client holding the lock asked to unlock" are
//!   different sentences, and only the second one is an unlock.
//!   smithay 0.7.0 conflates them, which is a complete lock-screen
//!   bypass; see the `ext_session_lock_v1` dispatch guard below.
//!
//! The `locked` event is only sent after a locked frame has actually
//! been *presented* (`confirm_after_frame`, called from the dispatch
//! loop after the render): the spec demands the compositor blank
//! before telling the locker it is safe, because the locker's caller
//! (a suspend hook, say) proceeds on that event.
//!
//! And a fourth absolute, about the way *out*: **unlocking costs the
//! locker nothing but its lock, and leaves the session exactly as
//! lockable as it was before.** Its two halves are the two bugs this
//! module carries workarounds for, both of them smithay's, and both
//! found on the same live incident — Omarchy's Quickshell is the
//! locker, the bar and every OSD on ONE connection, so anything the
//! unlock does to it, it does to the whole desktop:
//!
//! - *Nothing but its lock.* smithay's session-lock plumbing leaves a
//!   pre-commit hook on the lock surface's `wl_surface` forever, and
//!   that hook kills the whole connection over a commit that is
//!   perfectly legal once the lock-surface role is gone. Qt's unmap of
//!   the surface it is about to drop is exactly such a commit, so the
//!   shell died the instant a lock→unlock cycle completed.
//!   [`install_defunct_lock_role_guard`] is the counter-hook, and this
//!   module writes nothing at all to the lock's objects on the way out
//!   — the client is destroying them, and racing it buys nothing.
//! - *As lockable as before.* The same negligence leaves smithay's
//!   configure bookkeeping on that `wl_surface`, where it can swallow
//!   the next lock's mandatory first configure and blank the session
//!   behind a locker that can never draw.
//!   [`prime_reused_lock_surface`] is the counter-write.
//!
//! The keyboard is the third thing an unlock owes back, and
//! `layers::keyboard_target` is where both this module and the layer
//! shell ask who it belongs to.

use smithay::output::Output;
use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_surface_v1::ExtSessionLockSurfaceV1,
    ext_session_lock_v1::{self, ExtSessionLockV1},
};
use smithay::reexports::wayland_server::protocol::wl_output::WlOutput;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DataInit, DisplayHandle, Resource};
use smithay::utils::SERIAL_COUNTER;
use smithay::wayland::compositor::{add_pre_commit_hook, with_states, BufferAssignment, SurfaceAttributes};
use smithay::wayland::session_lock::{
    ExtLockSurfaceUserData, LockSurface, SessionLockHandler, SessionLockManagerGlobalData,
    SessionLockManagerState, SessionLockState, SessionLocker,
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
    /// Whether output geometry or lock-surface liveness changed since
    /// the last reconciliation. Animated lockers can drive the loop at
    /// frame rate; neither fact needs polling at frame rate.
    refresh_dirty: bool,
}

impl SessionLock {
    pub(crate) fn new(state: SessionLockManagerState) -> Self {
        Self { state, machine: LockMachine::default(), pending_ack: None, holder: None, refresh_dirty: false }
    }

    /// Schedules one lock-surface geometry/focus reconciliation. Every
    /// path that can change its answers calls this; repeated changes in
    /// one dispatch pass coalesce.
    pub(crate) fn mark_dirty(&mut self) {
        self.refresh_dirty = true;
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

    /// Answers a client's `unlock_and_destroy`. `caller_is_holder` is
    /// whether the `ext_session_lock_v1` the request arrived on is the
    /// very object this compositor recorded as the holder — object
    /// identity, not client identity, because a client may hold more
    /// than one and only one of them took the lock.
    ///
    /// The lock is the compositor's single security boundary, so this
    /// is a whitelist: everything that is not provably the holder
    /// finishing a confirmed lock is refused, and refusals are named
    /// so the caller can log which one happened.
    pub(crate) fn request_unlock(&self, caller_is_holder: bool) -> UnlockRequest {
        match self {
            // The whole point. `Locked` is the state in which the
            // `locked` event has gone out, which is the state the spec
            // makes `unlock_and_destroy` legal in.
            LockMachine::Locked if caller_is_holder => UnlockRequest::Accept,
            LockMachine::Locked | LockMachine::Locking => {
                if caller_is_holder {
                    // The holder, but too early: `locked` has not been
                    // sent, so the spec's own name for this request is
                    // `invalid_unlock`. Upstream posts that error and
                    // then unlocks anyway; refusing keeps the blank up,
                    // and since the error kills the locker's connection
                    // the holder goes dead and the recovery path admits
                    // a replacement. Fail-secure, and recoverable.
                    UnlockRequest::DenyUnconfirmed
                } else {
                    UnlockRequest::DenyNotHolder
                }
            }
            // Nothing is locked; there is nothing to unlock. Harmless
            // in itself, but it is still a caller asking for the one
            // transition that matters, so it is refused by the same
            // rule rather than by luck.
            LockMachine::Unlocked => UnlockRequest::DenyNotHolder,
        }
    }
}

/// What an `unlock_and_destroy` request is answered with — the exit
/// half of [`LockMachine::request_lock`]'s entry decision.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UnlockRequest {
    /// The holder is ending a confirmed lock: let it through.
    Accept,
    /// The caller is not the object holding the lock. This is the
    /// lock-screen bypass, and a security event.
    DenyNotHolder,
    /// The holder asked before its `locked` event went out.
    DenyUnconfirmed,
}

impl UnlockRequest {
    /// The message that goes out with the protocol error, and into the
    /// log beside it.
    fn refusal(self) -> &'static str {
        match self {
            UnlockRequest::Accept => "",
            UnlockRequest::DenyNotHolder => "This object does not hold the session lock.",
            UnlockRequest::DenyUnconfirmed => "Session is not locked.",
        }
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
                self.mark_hyprland_state_dirty();
                self.session_lock.holder = Some(confirmation.ext_session_lock().clone());
                self.session_lock.pending_ack = Some(confirmation);
                self.session_lock.mark_dirty();
                let backend = self.wm.backend_mut();
                backend.locked = true;
                backend.mark_idle_policy_dirty();
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
                // Smithay also owns a client click grab and each
                // tablet tool owns an independent proximity focus.
                // End both and make the pointer leave the pre-lock
                // desktop now, before a physical event can reuse
                // either focus. There is no lock surface yet, so this
                // first sync deliberately focuses nothing; new_surface
                // below moves it onto the correct lock surface.
                crate::input::reset_client_input_focus(self);
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
        self.mark_hyprland_state_dirty();
        self.session_lock.machine.unlocked();
        self.session_lock.pending_ack = None;
        self.session_lock.holder = None;
        self.session_lock.refresh_dirty = false;
        let backend = self.wm.backend_mut();
        backend.locked = false;
        backend.mark_idle_policy_dirty();
        // The ledger forgets the lock's surfaces and NOTHING else is
        // written to them: no configure, no unmap, no destroy. The
        // client owns their teardown and is already performing it (the
        // spec: "lock surfaces created through this object should be
        // destroyed by the client"), and a compositor that answered
        // `unlock_and_destroy` by writing to those objects would be
        // racing the client's own destroys for no gain — the stale
        // state such a write would exist to overwrite is dealt with
        // where it is actually read, in [`prime_reused_lock_surface`].
        backend.lock_surfaces.clear();
        backend.mark_damaged();
        // The locker's pointer/tablet focus is no more valid on the
        // restored desktop than desktop focus was when the lock
        // landed. Break any lock-surface click/tip grab and recompute
        // the client under the saved pointer position immediately.
        crate::input::reset_client_input_focus(self);
        // The keyboard goes home — to whoever would be holding it had
        // the lock never happened, which is not always a window. A
        // layer surface with exclusive interactivity (Omarchy's
        // popouts, its own lock preview) outranks window focus, and
        // `layers::sync_keyboard` cannot repair a wrong answer here:
        // it acts only when the exclusive claimant *changes*, and the
        // claimant across a whole lock cycle is the same surface it
        // was before. Asking the one authority both paths share is
        // what keeps a popout that was open when the screen locked
        // from coming back deaf.
        let target = crate::layers::keyboard_target(self);
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
        prime_reused_lock_surface(&surface, logical);
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
        self.session_lock.mark_dirty();
        self.wm.backend_mut().mark_damaged();
        // Pointer focus was cleared as soon as the lock was accepted,
        // before this surface existed. Put it on the newly available
        // lock surface immediately rather than waiting for motion; on
        // a multi-output lock each arrival re-evaluates the pointer's
        // output and only that matching surface can win.
        crate::input::sync_pointer_focus(self);
        if focus_target {
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, Some(surface.wl_surface().clone()), SERIAL_COUNTER.next_serial());
            }
        }
    }
}

// ---------------------------------------------------------------------
// Workaround: smithay's `unlock_and_destroy` unlocks for anyone.
// ---------------------------------------------------------------------

// `delegate_session_lock!(Compositor)` would emit all four of the
// delegations below. Three of them are taken verbatim; the fourth —
// `ext_session_lock_v1` itself — is written out by hand underneath so
// that `unlock_and_destroy` can be authenticated before upstream sees
// it. Keep this list in step with the macro
// (`smithay-0.7.0/src/wayland/session_lock/mod.rs:227-244`) if smithay
// ever adds an interface to it.
smithay::reexports::wayland_server::delegate_global_dispatch!(Compositor: [
    ExtSessionLockManagerV1: SessionLockManagerGlobalData
] => SessionLockManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ExtSessionLockManagerV1: ()
] => SessionLockManagerState);
smithay::reexports::wayland_server::delegate_dispatch!(Compositor: [
    ExtSessionLockSurfaceV1: ExtLockSurfaceUserData
] => SessionLockManagerState);

/// Authenticates `unlock_and_destroy` against the recorded holder, then
/// hands every request on to smithay's implementation.
///
/// # The upstream bug
///
/// [`SessionLockHandler::unlock`] takes no argument, so the identity of
/// the caller cannot reach a compositor through the handler at all —
/// and smithay does not check it either.
/// `smithay-0.7.0/src/wayland/session_lock/lock.rs:181-189`:
///
/// ```text
/// Request::UnlockAndDestroy => {
///     // Ensure session is locked.
///     if !data.lock_status.load(Ordering::Relaxed) {
///         lock.post_error(Error::InvalidUnlock, "Session is not locked.");
///     }
///
///     state.lock_state().locked_outputs.clear();
///     state.unlock();
/// }
/// ```
///
/// There is no `return` after the `post_error`, and `lock_status` is a
/// **per-object** flag minted false for every `lock` request and set
/// true only when the compositor confirms
/// (`session_lock/mod.rs:143-152` and `:217-222`). So a client whose
/// lock request this compositor *denied* holds a live
/// `ext_session_lock_v1` with `lock_status == false` — the deny branch
/// answers by dropping the confirmation, which sends `finished` but
/// cannot destroy an object only the client may destroy — and sending
/// `unlock_and_destroy` on it posts a protocol error that kills the
/// *attacker's* connection and then falls through to `state.unlock()`,
/// which is this module's. Three requests from any process that can
/// open the Wayland socket — bind the manager, `lock`, then
/// `unlock_and_destroy` on the object the denial left behind — and the
/// screen in front of an unattended machine is the user's desktop
/// again, keyboard and all.
///
/// # What this does instead
///
/// Every request is forwarded unchanged except `unlock_and_destroy`,
/// which is put to [`LockMachine::request_unlock`] first. The identity
/// compared is the protocol object: `session_lock.holder` is written
/// only in `lock()`'s accept branch, from the confirmation's own
/// `ext_session_lock_v1`, so equality here means "the object that took
/// the lock now in force". `ObjectId` equality covers the client and
/// the object's generation as well as its numeric id, so a destroyed
/// object's id being handed out again cannot be mistaken for the
/// holder.
///
/// A refused request is answered with the protocol's own
/// `invalid_unlock`, which is accurate on both refusal paths — neither
/// caller ever received `locked` on the object it is asking with — and
/// which disconnects it. The compositor's lock state is not touched, so
/// upstream's `unlock()` is never reached and the blank stays up.
///
/// # Removing this
///
/// When smithay returns after that `post_error` *and* passes the
/// `&ExtSessionLockV1` to `SessionLockHandler::unlock`, this whole
/// section collapses back to `delegate_session_lock!(Compositor)` plus
/// a holder comparison at the top of [`Compositor::unlock`]. Until it
/// does both, the delegation has to be split, because the handler
/// signature is where the caller's identity is lost.
impl smithay::reexports::wayland_server::Dispatch<ExtSessionLockV1, SessionLockState> for Compositor {
    fn request(
        state: &mut Self,
        client: &Client,
        lock: &ExtSessionLockV1,
        request: ext_session_lock_v1::Request,
        data: &SessionLockState,
        dhandle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        // Matching binds nothing, so the request is not moved and is
        // still available to forward below.
        if matches!(request, ext_session_lock_v1::Request::UnlockAndDestroy) {
            let caller_is_holder =
                state.session_lock.holder.as_ref().is_some_and(|holder| holder == lock);
            let verdict = state.session_lock.machine.request_unlock(caller_is_holder);
            if verdict != UnlockRequest::Accept {
                // A refused unlock is an attempt on the session's one
                // security boundary, not a routine protocol denial, so
                // it is logged at `error` with enough to identify the
                // process behind it. The pid is the only durable
                // handle a user has on "which program did this"; a
                // client that has already gone is not worth a failure
                // here, hence the `ok()`.
                let pid = client.get_credentials(dhandle).ok().map(|credentials| credentials.pid);
                tracing::error!(
                    ?verdict,
                    pid,
                    object = lock.id().protocol_id(),
                    "refusing unlock_and_destroy: the session stays locked"
                );
                lock.post_error(ext_session_lock_v1::Error::InvalidUnlock, verdict.refusal());
                return;
            }
        }
        <SessionLockManagerState as smithay::reexports::wayland_server::Dispatch<
            ExtSessionLockV1,
            SessionLockState,
            Compositor,
        >>::request(state, client, lock, request, data, dhandle, data_init);
    }
}

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

/// Event-driven upkeep while locked: keep every lock surface configured
/// to its output's current size after a size/scale change, and move the
/// keyboard after a lock surface dies. The output and lock ledgers are
/// disjoint fields, so the walk borrows them together and allocates no
/// intermediate surface list. An animated but otherwise unchanged lock
/// screen returns before touching either ledger.
pub(crate) fn refresh(comp: &mut Compositor) {
    if !comp.wm.backend().locked {
        comp.session_lock.refresh_dirty = false;
        return;
    }
    if !std::mem::take(&mut comp.session_lock.refresh_dirty) {
        return;
    }
    let advertised = crate::state::advertised_output_scale(comp.ui_scale).integer_scale().max(1);
    {
        let Compositor { outputs, wm, .. } = comp;
        for lock in wm.backend().lock_surfaces.iter().filter(|entry| entry.surface.alive()) {
            let Some(entry) = outputs.get(lock.output) else { continue };
            let logical = (
                (entry.size.w as i32 / advertised).max(1) as u32,
                (entry.size.h as i32 / advertised).max(1) as u32,
            );
            lock.surface.with_pending_state(|state| {
                state.size = Some(logical.into());
            });
            // Dedups internally: sends only when the size actually
            // moved.
            lock.surface.send_configure();
        }
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

/// Guarantees the mandatory first configure reaches a lock surface
/// whose `wl_surface` has worn the lock-surface role before, by putting
/// a size on record that the real one cannot be deduped against.
///
/// # Why a surface can arrive already carrying that history
///
/// The spec is unambiguous that it should not: ext-session-lock-v1 on
/// `get_lock_surface` — "Providing a wl_surface which already has a
/// role or already has a buffer attached or committed is a protocol
/// error" — and the real client obeys it. Quickshell, traced against
/// this compositor while running Omarchy's own `plugins/lock`, destroys
/// the lock surface's `wl_surface` along with the role and creates a
/// fresh one for the next lock. smithay nevertheless *accepts* the
/// re-use: `give_role` is `set_role`, which returns `Ok` for a role a
/// surface already has, and the buffer check finds nothing because the
/// renderer's own commit handler has long since taken that buffer out
/// of the surface's current state.
///
/// # What accepting it costs, without this
///
/// `LockSurfaceAttributes` — smithay's per-`wl_surface` configure
/// bookkeeping — lives in the surface's `data_map` for the surface's
/// whole life, and `get_lock_surface` re-uses it as it stands (it
/// overwrites only the `ExtSessionLockSurfaceV1` handle). So the second
/// lock's `last_acked` is the first lock's acked size. `send_configure`
/// dedups against exactly that, and the second lock is invariably the
/// same size as the first — it is the same output. The initial
/// configure is therefore silently dropped. The protocol makes it
/// mandatory ("On binding this interface the compositor will
/// immediately send the first configure event"), a client blocks on it
/// before it may attach anything, and meanwhile this compositor has
/// blanked the outputs and — since blanking alone satisfies the
/// `locked` event — told the locker the session is secure. The result
/// is the worst state a lock screen has: a black session, a locker
/// that can never draw a password prompt, and no way out but a VT
/// switch. `chonk-testkit`'s `session_lock.rs` reproduces it in its
/// third cycle.
///
/// # The nudge
///
/// One configure at `real + 1` wide, sent before the real one and in
/// the same batch, moves the record off the size that would be deduped;
/// the real configure that follows always differs from it, so it always
/// goes out. Both reach the client together, before it can respond to
/// either, and the spec's own rule for that case — "If the client
/// receives multiple configure events before it can respond to one, it
/// only has to ack the last configure event" — means the primer is
/// never drawn. It is skipped entirely for the fresh surfaces every
/// correct client brings, which get exactly one configure, so a trace
/// of a normal lock looks like the protocol's own description of one.
///
/// `current_state()` is the test: it is smithay's `current` for this
/// surface, written only by the post-commit hook out of `last_acked`,
/// so a size on record means this `wl_surface` has completed a
/// configure→ack→commit cycle as a lock surface already. A surface that
/// was configured but never acked before its lock ended is not caught
/// by it and would still be deduped into the lockout above — the
/// bookkeeping that would say so is unreachable (`LockSurfaceAttributes`
/// is not exported), and reaching that state needs a locker that took
/// the lock, never drew, unlocked anyway, and then re-locked the same
/// undrawn surface at the same size.
fn prime_reused_lock_surface(surface: &LockSurface, logical: (u32, u32)) {
    if surface.current_state().size.is_none() {
        return;
    }
    tracing::info!(
        "priming a lock surface whose wl_surface already wore the role: \
         smithay would dedup its mandatory first configure away"
    );
    surface.with_pending_state(|state| {
        state.size = Some((logical.0.saturating_add(1), logical.1).into());
    });
    surface.send_configure();
}

// ---------------------------------------------------------------------
// Workaround: smithay's session-lock pre-commit hook outlives its role.
// ---------------------------------------------------------------------

/// The role string smithay gives a lock surface's `wl_surface` —
/// restated because upstream's copy (`LOCK_SURFACE_ROLE`, line 20 of
/// `smithay-0.7.0/src/wayland/session_lock/lock.rs`) is private. It is
/// the protocol interface's own name, which the test below pins so the
/// two cannot drift apart.
const LOCK_SURFACE_ROLE: &str = "ext_session_lock_surface_v1";

/// Whether the guard hook should stand down for `surface`: a surface
/// of the CURRENT lock, whose protocol errors are real and must still
/// go out — the same "not a blanket amnesty" line
/// `layers::install_orphaned_role_guard` draws with smithay's
/// `layer_surfaces()` list. The ledger is this module's equivalent
/// authority: `lock()` and `unlock()` are the only writers, so
/// membership says "this is one of the surfaces the lock in force is
/// showing" and nothing else.
fn current_lock_surface(backend: &crate::state::WaylandBackend, surface: &WlSurface) -> bool {
    backend
        .lock_surfaces
        .iter()
        .any(|entry| entry.surface.alive() && entry.surface.wl_surface() == surface)
}

/// Installs the per-surface guard that stops a *defunct* lock surface
/// from killing its client on a later commit. Called once per surface
/// from `CompositorHandler::new_surface` beside
/// `layers::install_orphaned_role_guard`; silent on every surface that
/// never takes the lock-surface role.
///
/// # The upstream bug — the layer-shell one's twin
///
/// `smithay-0.7.0/src/wayland/session_lock/lock.rs` adds a pre-commit
/// hook to the `wl_surface` in `GetLockSurface` (around line 105) and
/// never removes it: the `HookId` is dropped on the floor, and the
/// lock surface's `Request::Destroy` arm is empty — there is no
/// cleanup of any kind. The hook posts fatal protocol errors for a
/// null-buffer commit (`null_buffer`), a commit before the first ack
/// (`commit_before_first_ack`), and a buffer that does not match the
/// acked configure (`dimensions_mismatch`). All three are correct law
/// for a *lock surface* — and the hook keeps enforcing them on a
/// `wl_surface` whose lock-surface days are over, posting the error on
/// an object the client already destroyed. Posting an error on any
/// object, dead or alive, kills the whole connection
/// (`wayland-backend`'s `post_error` looks up only the client id).
///
/// The sequence that hits it is the spec's own teardown, spelled the
/// way Qt spells it. ext-session-lock-v1, `unlock_and_destroy`:
/// "After this request is made, lock surfaces created through this
/// object should be destroyed by the client." Qt destroys the role
/// object and unmaps the kept `wl_surface` — `attach(nil)`, `commit`
/// — the very pattern `layers.rs` documents for layer surfaces. The
/// surviving hook answers that unmap with `null_buffer`, and the
/// client's connection is gone. Quickshell is built that way, and
/// Quickshell's one connection is Omarchy's locker, bar and OSDs
/// together — the observed incident (`omarchy-shell`, Sep 2 13:14:20:
/// lock, PAM auth, unlock, "The Wayland connection broke"), and
/// `chonk-testkit`'s `chonk-lock-probe` reproduces it on demand.
///
/// # What the guard does
///
/// Registered from `new_surface`, which runs when the `wl_surface` is
/// created and therefore before any `get_lock_surface` can reach it,
/// so it sits ahead of smithay's hook in the surface's hook list and
/// runs first (smithay invokes pre-commit hooks in registration
/// order). On a commit to a surface wearing the lock role that is
/// *not* one of the current lock's surfaces ([`current_lock_surface`]
/// — the ledger empties at `unlock()` and at a recovery takeover), it
/// takes the pending buffer assignment out of the commit before
/// smithay's hook can read it: a removal (the unmap) simply vanishes,
/// and a late in-flight frame is released back to the client unused.
/// With no assignment pending, upstream's hook has nothing to object
/// to. The state it cannot reach (`LockSurfaceAttributes` is not
/// exported) stays untouched: the neutralized commit needs none of it,
/// and the one place where what is left there still matters — a
/// re-lock on the same `wl_surface`, whose mandatory first configure
/// it would otherwise swallow — is [`prime_reused_lock_surface`]'s.
///
/// # What this must not do, and what it cannot
///
/// A surface of the lock in force keeps upstream's full enforcement —
/// the guard stands down on ledger membership, so a live locker that
/// really commits a null buffer still dies for it, as the spec
/// demands. Two corners stay open, both out of reach without smithay
/// exporting `LockSurfaceAttributes` or fixing the hook's lifetime: a
/// defunct lock surface that never acked anything still dies to
/// `commit_before_first_ack` (no real toolkit commits before its
/// first ack), and a client that destroys one lock surface's role
/// object MID-lock and then commits its `wl_surface` is still killed,
/// because the ledger has no way to see a role object die (the entry
/// leaves only at unlock).
///
/// # Removing this
///
/// When smithay removes its hook in the lock surface's destructor (or
/// scopes it to the role's lifetime), delete this function,
/// [`current_lock_surface`], [`LOCK_SURFACE_ROLE`], the call in
/// `xdg.rs`'s `new_surface`, and the tests that name them. Nothing
/// else refers to any of it. A smithay that also cleared
/// `LockSurfaceAttributes` with the role — or refused the re-used
/// surface outright, as the spec entitles it to — would retire
/// [`prime_reused_lock_surface`] the same way; the two are
/// independent, and `session_lock.rs` fails distinctly for each.
pub(crate) fn install_defunct_lock_role_guard(surface: &WlSurface) {
    add_pre_commit_hook::<Compositor, _>(surface, |comp, _dh, surface| {
        // One cheap read first: this runs on every commit of every
        // surface in the session, and for all but a lock cycle's worth
        // the answer is "never had the lock role".
        let is_lock_role = with_states(surface, |states| states.role == Some(LOCK_SURFACE_ROLE));
        if !is_lock_role {
            return;
        }
        if current_lock_surface(comp.wm.backend(), surface) {
            return;
        }
        with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceAttributes>();
            let pending = guard.pending();
            match pending.buffer.take() {
                None => {}
                Some(BufferAssignment::Removed) => {
                    // Qt's unmap. Swallowed whole: the stale renderer
                    // state it leaves behind belongs to a surface the
                    // scene never draws (not in the ledger), and the
                    // next lock's first commit replaces it.
                    tracing::debug!(
                        "neutralized an unmap commit on a defunct lock surface — smithay's stale pre-commit hook"
                    );
                }
                Some(BufferAssignment::NewBuffer(buffer)) => {
                    // A frame that raced the unlock. It will never be
                    // shown; releasing it keeps the client's swapchain
                    // whole where silently dropping it would strand
                    // the buffer unreleased forever.
                    buffer.release();
                    tracing::debug!(
                        "released a late frame committed to a defunct lock surface — smithay's stale pre-commit hook"
                    );
                }
            }
        });
    });
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
    fn a_client_that_does_not_hold_the_lock_cannot_unlock() {
        // The bypass, in the state machine: the real locker holds a
        // confirmed lock, and a second client asks to unlock with an
        // object that is not the holder's. smithay's dispatch would
        // reach `unlock()` here; the guard is what stops it.
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        machine.frame_presented();
        assert_eq!(machine.request_unlock(false), UnlockRequest::DenyNotHolder);
        assert!(machine.locked(), "a refused unlock must not disturb the lock");
    }

    #[test]
    fn the_holder_may_unlock_a_confirmed_lock() {
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        machine.frame_presented();
        assert_eq!(machine.request_unlock(true), UnlockRequest::Accept);
    }

    #[test]
    fn even_the_holder_may_not_unlock_before_its_locked_event() {
        // `Locking` is the window between accepting the lock and
        // presenting the first blanked frame. The spec's own name for
        // an unlock here is `invalid_unlock` — "unlock requested but
        // locked event was never sent" — so it is refused, and the
        // blank holds rather than lifting on a client that is by
        // definition not yet showing a lock screen.
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        assert_eq!(machine.request_unlock(true), UnlockRequest::DenyUnconfirmed);
        assert_eq!(machine.request_unlock(false), UnlockRequest::DenyNotHolder);
        assert!(machine.locked());
    }

    #[test]
    fn an_unlock_on_an_unlocked_session_is_refused_rather_than_ignored() {
        let machine = LockMachine::default();
        assert_eq!(machine.request_unlock(true), UnlockRequest::DenyNotHolder);
        assert_eq!(machine.request_unlock(false), UnlockRequest::DenyNotHolder);
    }

    #[test]
    fn a_recovering_locker_may_unlock_the_session_it_took_over() {
        // Refusing impostors must not cost the recovery path its exit:
        // a locker that took over a dead one's session is the holder
        // from the moment it is accepted, and unlocks like any other
        // once its own frame is on screen.
        let mut machine = LockMachine::default();
        machine.request_lock(false);
        machine.frame_presented();
        // The holder dies; a replacement takes over and confirms.
        assert_eq!(machine.request_lock(false), LockRequest::Accept);
        assert!(machine.frame_presented());
        assert_eq!(machine.request_unlock(true), UnlockRequest::Accept);
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

    // The defunct-role guard and the configure primer both need a
    // client on a socket to reach — `chonk-testkit`'s `session_lock.rs`
    // e2e drives `chonk-lock-probe` (a locker that is also a layer
    // client, the Quickshell shape) through three full
    // lock→unlock→Qt-teardown cycles against the real compositor, and
    // is the test that either one actually works. With the guard
    // removed the probe's connection is killed at the first teardown
    // (`ext_session_lock_surface_v1` code 1, "Surface attached a NULL
    // buffer", on an object it had already destroyed); with the primer
    // removed it blocks forever on the third lock's configure. What a
    // unit test CAN pin is the one assumption the guard's cheap read
    // rests on.

    #[test]
    fn the_restated_role_string_is_the_interface_smithay_names_lock_surfaces_with() {
        // smithay's private `LOCK_SURFACE_ROLE` is the interface's own
        // name; if either ever changes, the guard's role check goes
        // silently blind and the unlock teardown kills lockers again —
        // this assertion is what fails first instead.
        use smithay::reexports::wayland_protocols::ext::session_lock::v1::server::ext_session_lock_surface_v1::ExtSessionLockSurfaceV1;
        use smithay::reexports::wayland_server::Resource as _;
        assert_eq!(LOCK_SURFACE_ROLE, ExtSessionLockSurfaceV1::interface().name);
    }
}
