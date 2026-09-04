//! `hyprland-focus-grab-v1`: the protocol a shell uses to say "these
//! surfaces are a popup — dismiss them when the user clicks away".
//!
//! Omarchy 4's desktop shell (Quickshell/QML) opens its bar popouts,
//! its notification cards and its menus as layer surfaces and then asks
//! the compositor to watch the pointer for it. Without this global that
//! shell logs
//!
//! > The active compositor does not support the
//! > hyprland_focus_grab_v1 protocol. HyprlandFocusGrab will not work.
//!
//! and its `HyprlandFocusGrab` becomes a no-op: every popout it opens
//! stays open forever, stacking one over the next, because clicking
//! away is the *only* gesture those components offer to close them.
//! Nothing crashes; the desktop simply accumulates menus. That is the
//! bug this module exists to close, and it is why the protocol is
//! implemented here rather than waited on: it is a published protocol
//! with a public XML definition (`protocols/hyprland-focus-grab-v1.xml`,
//! vendored verbatim from `hyprwm/hyprland-protocols`), and it is the
//! only Hyprland-specific interface this compositor speaks.
//!
//! # Where the bindings come from
//!
//! Nowhere: no published crate carries this interface. `wayland-
//! protocols`, `-wlr`, `-misc` and `-plasma` — the four Smithay
//! re-exports every other protocol in this crate is built on (see
//! `protocols.rs`, `decoration.rs`) — none of them has it, so unlike
//! those modules this one cannot be dispatch impls alone.
//!
//! Of the two ways to get types for an XML file, this takes the
//! generator: [`bindings`] below expands `wayland_scanner::
//! generate_server_code!` over the vendored XML at compile time. The
//! alternative — writing the `Resource` impls out by hand — means
//! hand-transcribing a `wayland_backend::protocol::Interface`, its
//! `MessageDesc` table, the argument-type list for every request, and
//! the parse/write bodies that decode a wire message into them. That is
//! several hundred lines whose only correctness criterion is "identical
//! to what the generator would have produced", with a wire-format
//! mismatch — not a compile error — as the failure mode. The generator
//! is also what produced every other protocol type in this process, so
//! this one behaves exactly like its neighbours.
//!
//! It costs no new crate in the dependency graph: `wayland-scanner` is
//! already built here as the code generator inside `wayland-server`,
//! `wayland-protocols` and `wayland-protocols-wlr`. It is a proc-macro,
//! so there is also no `build.rs` and no generated `.rs` checked in
//! beside the XML to drift away from it.
//!
//! # The state machine
//!
//! Per grab object, two lists rather than one, because the protocol
//! stages: `add_surface` and `remove_surface` only edit a *pending*
//! whitelist and `commit` is what publishes it. So [`Whitelist`] holds
//! `staged` (what the client has asked for) and `committed` (what the
//! compositor is enforcing), and every transition worth acting on is a
//! transition of the second one:
//!
//! | committed | after commit | meaning |
//! |---|---|---|
//! | empty | non-empty | the grab starts |
//! | non-empty | different | still grabbing, new whitelist |
//! | non-empty | empty | the grab is over; one `cleared` is owed |
//!
//! [`Whitelist`] is generic over the surface type for one reason: it
//! makes the whole state machine testable without a Wayland connection.
//! The tests at the bottom of this file drive it with plain integers,
//! which is why "`cleared` fires exactly once" is a property this file
//! can *prove* rather than a thing a live session appears to do.
//!
//! # What clears a grab
//!
//! Exactly the events below, each of which sends `cleared` once (the
//! `Cleared` outcome empties the staged list too, so a grab cannot be
//! resurrected — or re-cleared — by a later `commit` of entries staged
//! before it ended):
//!
//! - a **button press** outside every whitelisted surface (`input.rs`);
//! - the client committing an empty whitelist;
//! - the last whitelisted `wl_surface` being destroyed, which the
//!   protocol defines as an implicit `remove_surface`;
//! - another grab starting, which supersedes this one;
//! - the session locking.
//!
//! Destroying the grab *object* also ends the grab, and is the one
//! ending that sends nothing — there is no object left to send it on.
//!
//! Pointer **motion** outside the whitelist deliberately does not
//! clear, though the specification's wording ("a mouse or touch input
//! outside of any whitelisted surfaces") would permit it. A menu that
//! closed the moment the pointer left it could not be used: the user
//! would have to keep the cursor inside the popout on the way to
//! clicking it, and every bar popout in Omarchy is opened by a click on
//! a bar item the pointer then has to travel away from. Hyprland — the
//! implementation Quickshell is written against — clears on press and
//! not on motion, and this matches it.
//!
//! # Ordering against everything else that moves the keyboard
//!
//! Four mechanisms in this compositor can point the seat's keyboard
//! somewhere. Highest wins:
//!
//! 1. **A session lock** (`lock.rs`). Absolute. A locked session shows
//!    only lock surfaces and routes input only to them, so a grab
//!    behind the lock is enforcing a whitelist nobody can see or click.
//!    [`refresh`] therefore ends an active grab outright when the
//!    session locks, and the input hooks sit *after* `input.rs`'s
//!    locked early-returns so nothing here can run while locked.
//! 2. **Layer-shell `exclusive` keyboard interactivity** (`layers.rs`).
//!    A surface holding it has said it is the only thing that may
//!    receive keys — it is what fuzzel types into — and the grab's own
//!    specification leaves the whitelisted surface to be
//!    "compositor-picked", so there is nothing to violate by picking
//!    the exclusive claimant. In practice they agree: the grabbing
//!    surface usually *is* that claimant.
//! 3. **This grab.** While one is active the keyboard stays on a
//!    whitelisted surface: `Compositor::apply_pending_focus` stops
//!    short of the seat, and `input.rs` suppresses both halves of
//!    layer-shell's on-demand click focus. Focus-follows-mouse cannot
//!    reach it either, because motion outside the whitelist queues no
//!    `PointerEnter`.
//! 4. **Ordinary window focus**, which is where the keyboard returns
//!    when the grab ends.
//!
//! An in-progress **drag** — a window move or resize, or a client's own
//! button-held gesture — outranks the grab for the *pointer* without
//! touching that order: every hook here is gated on `Route::target`
//! being `None`, which is true only when no drag grab and no implicit
//! grab hold the pointer. So a press inside a popout that drags outside
//! it (a slider, a scrollbar) keeps delivering to the popout and does
//! not dismiss it, and a window drag already under way is not
//! interrupted by a grab that starts mid-drag.
//!
//! # Integration contract
//!
//! One field, one init call, one call per dispatch pass, and four hooks
//! — the same shape `protocols.rs` documents, all of it in `state.rs`
//! except the hooks:
//!
//! 1. `Compositor` gains one field, `focus_grab: crate::focus_grab::FocusGrab`.
//! 2. In `run`, `let focus_grab = crate::focus_grab::init(&display_handle);`
//!    beside the other globals and, like all of them, *before* the
//!    `ListeningSocketSource` — a global missing when Quickshell binds
//!    might as well not exist.
//! 3. In `Compositor::dispatch_pending`, `crate::focus_grab::refresh(self);`
//!    beside the other per-pass reconciliations.
//! 4. The hooks: two in `input.rs` (motion and button), one in
//!    `Compositor::apply_pending_focus`, one in `layers::sync_keyboard`.
//!    Each is documented at its site.
//!
//! # Why the work is deferred to `refresh`
//!
//! Dead-surface pruning and the keyboard move both happen once per
//! dispatch pass rather than inside the request handlers, which is the
//! same deferral `protocols.rs::apply_minimize_requests` and
//! `WaylandBackend::pending_focus` document. Pruning by aliveness needs
//! no destruction hook on `wl_surface` at all — a destroyed surface is
//! simply not alive on the next pass — so the protocol's "destroying
//! the surface is treated the same as an explicit call to
//! `remove_surface`" clause cannot be forgotten by a future code path,
//! and cannot leak an entry either. The cost is one dispatch pass of
//! latency on a keyboard focus move nobody can perceive.

use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use smithay::utils::SERIAL_COUNTER;

use crate::state::Compositor;

/// The generated protocol types, expanded from the vendored XML.
///
/// The three `use` items are not decoration: the generator emits code
/// that names `super::wayland_server`, `super::wayland_backend` (inside
/// `__interfaces`) and the core-protocol types by bare name, exactly as
/// the `wayland_protocol!` macro in `wayland-protocols-wlr` sets them
/// up. Smithay re-exports `wayland-server`, and `wayland_server::backend`
/// re-exports the `wayland_backend::protocol` items the interface
/// tables are built from, so no crate beyond the scanner is needed to
/// satisfy them.
#[allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]
#[allow(dead_code, unused_imports, unused_variables, unused_unsafe)]
#[allow(missing_docs, clippy::all)]
pub(crate) mod bindings {
    use smithay::reexports::wayland_server;
    use smithay::reexports::wayland_server::protocol::*;

    pub mod __interfaces {
        use smithay::reexports::wayland_server::backend as wayland_backend;
        use smithay::reexports::wayland_server::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!("protocols/hyprland-focus-grab-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_server_code!("protocols/hyprland-focus-grab-v1.xml");
}

use bindings::hyprland_focus_grab_manager_v1::{self, HyprlandFocusGrabManagerV1};
use bindings::hyprland_focus_grab_v1::{self, HyprlandFocusGrabV1};

/// The only version the protocol has ever had. Advertised as an
/// explicit constant rather than inlined so a future version bump is
/// one edit made in sight of this comment: version 2 would have to be
/// audited request by request before this number moves.
const FOCUS_GRAB_VERSION: u32 = 1;

/// How far up a subsurface parent chain [`whitelisted`] will walk
/// before giving up.
///
/// A cap, not a limit anyone reaches: real popup trees are two or three
/// deep. It exists because this walk runs inside pointer dispatch in a
/// login session, and a cycle in the parent chain — which a compositor
/// bug or a malicious client could produce — would otherwise hang the
/// desktop with no console to read the hang from. Answering "not
/// whitelisted" for an absurdly deep tree at worst dismisses a popup
/// early.
const MAX_SUBSURFACE_DEPTH: usize = 32;

// ---------------------------------------------------------------------
// The state machine.
// ---------------------------------------------------------------------

/// What a [`Whitelist::commit`] (or any other mutation) did to the
/// grab, in the only terms the rest of this module cares about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Commit {
    /// The committed whitelist is what it was. Nothing to do — and in
    /// particular no `cleared` to send, which is what makes a client
    /// that commits twice cost nothing.
    Unchanged,
    /// Empty to non-empty: the grab starts. The caller supersedes any
    /// other active grab and moves the keyboard.
    Started,
    /// Non-empty to a *different* non-empty: the grab continues with a
    /// new whitelist. The keyboard may need re-picking, because the
    /// surface it is on can be one of the ones just removed.
    Rewhitelisted,
    /// Non-empty to empty: the grab is over and owes exactly one
    /// `cleared`.
    Cleared,
}

/// One grab object's staged and committed surface whitelists.
///
/// Generic over the surface type so the state machine can be tested
/// with plain values — see the module docs. Nothing here touches the
/// protocol, the seat, or the clock; every method is a pure transition
/// plus the outcome the caller must act on.
#[derive(Debug)]
pub(crate) struct Whitelist<S> {
    /// What `add_surface`/`remove_surface` have built up since the last
    /// `commit`. Not enforced against anything.
    staged: Vec<S>,
    /// What the compositor is actually enforcing. A grab is active
    /// exactly when this is non-empty.
    committed: Vec<S>,
}

impl<S: Clone + PartialEq> Whitelist<S> {
    /// An empty, inert whitelist — the state a freshly created
    /// `hyprland_focus_grab_v1` is in until its first `commit`.
    pub(crate) fn new() -> Self {
        Whitelist { staged: Vec::new(), committed: Vec::new() }
    }

    /// Whether this grab is being enforced.
    pub(crate) fn is_active(&self) -> bool {
        !self.committed.is_empty()
    }

    /// Whether `surface` is on the enforced whitelist. Staged entries
    /// deliberately do not count: a surface the client has added but
    /// not committed is not protected yet, which is the whole point of
    /// the protocol's staging.
    pub(crate) fn contains(&self, surface: &S) -> bool {
        self.committed.contains(surface)
    }

    /// The enforced whitelist, in the order the client added it. The
    /// keyboard goes to the first live entry, which makes "which
    /// surface does a grab focus" answerable from the client's own
    /// ordering rather than from a hash iteration order that changes
    /// between runs.
    pub(crate) fn committed(&self) -> &[S] {
        &self.committed
    }

    /// `add_surface`: stages an addition. Duplicates are ignored, as
    /// the protocol requires.
    pub(crate) fn add(&mut self, surface: S) {
        if !self.staged.contains(&surface) {
            self.staged.push(surface);
        }
    }

    /// `remove_surface`: stages a removal.
    ///
    /// Note that an `add` after a `remove` of the same surface, with no
    /// `commit` between them, leaves the surface staged — the plain
    /// reading of "does not take effect until commit is called".
    /// Hyprland's own implementation keeps a per-entry pending state
    /// instead and lets the removal win that race; no client has been
    /// observed to depend on either answer, and this one is the one
    /// that can be explained.
    pub(crate) fn remove(&mut self, surface: &S) {
        self.staged.retain(|staged| staged != surface);
    }

    /// `commit`: publishes the staged whitelist.
    pub(crate) fn commit(&mut self) -> Commit {
        let was_active = self.is_active();
        let changed = self.staged != self.committed;
        if changed {
            self.committed = self.staged.clone();
        }
        self.settle(was_active, changed)
    }

    /// Drops every entry `keep` rejects, from both lists, as if the
    /// client had removed and committed them itself.
    ///
    /// This is the protocol's "destroying the surface is treated the
    /// same as an explicit call to `remove_surface`" clause, expressed
    /// as a filter rather than a destruction callback so that a
    /// surface which dies while nothing is looking still leaves the
    /// whitelist on the next pass. See [`refresh`].
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&S) -> bool) -> Commit {
        let was_active = self.is_active();
        let before = self.staged.len() + self.committed.len();
        self.staged.retain(|surface| keep(surface));
        self.committed.retain(|surface| keep(surface));
        let changed = before != self.staged.len() + self.committed.len();
        self.settle(was_active, changed)
    }

    /// Ends the grab unconditionally: everything staged and everything
    /// committed goes. Answers whether there was an active grab to
    /// end — which is exactly whether a `cleared` is owed, for the
    /// callers that owe one (supersession, session lock) and equally
    /// for the one that does not (the grab object being destroyed).
    pub(crate) fn finish(&mut self) -> bool {
        let was_active = self.is_active();
        self.staged.clear();
        self.committed.clear();
        was_active
    }

    /// Classifies a transition, and enforces the one invariant that
    /// makes `cleared` fire exactly once: a grab that just went inert
    /// keeps nothing staged.
    ///
    /// Without that clear, a client which staged a surface, committed
    /// it, had the grab cleared by a click outside, and then committed
    /// again would silently restart the grab from entries it staged
    /// before the clear — and a *second* `cleared` would follow the
    /// next time it ended. Hyprland's `CFocusGrab::finish` empties its
    /// map for the same reason.
    fn settle(&mut self, was_active: bool, changed: bool) -> Commit {
        if !changed {
            return Commit::Unchanged;
        }
        match (was_active, self.is_active()) {
            (false, true) => Commit::Started,
            (true, true) => Commit::Rewhitelisted,
            (true, false) => {
                self.staged.clear();
                Commit::Cleared
            }
            // Inert before, inert after: `changed` can only have been
            // set by a staged-list edit, which no one is owed an event
            // for.
            (false, false) => Commit::Unchanged,
        }
    }
}

// ---------------------------------------------------------------------
// Compositor-side state.
// ---------------------------------------------------------------------

/// One live `hyprland_focus_grab_v1` object and its whitelist.
struct Grab {
    /// The protocol object, kept to send `cleared` on and to match
    /// incoming requests against. Removed by `Dispatch::destroyed`, so
    /// this vector cannot outlive its clients.
    resource: HyprlandFocusGrabV1,
    list: Whitelist<WlSurface>,
}

/// Everything this protocol keeps between dispatch passes.
///
/// At most one grab in `grabs` is ever active — starting one ends any
/// other — so "the active grab" is derived by searching rather than
/// stored in a second field that could disagree with the first.
pub(crate) struct FocusGrab {
    grabs: Vec<Grab>,
    /// Set whenever something happened that the seat's keyboard focus
    /// might have to answer for: a grab started, changed, or ended.
    /// Consumed by [`refresh`].
    ///
    /// A flag rather than a `keyboard.set_focus` at the point of
    /// decision, because those points are protocol request handlers:
    /// moving the seat from inside one re-enters `SeatHandler` in the
    /// middle of a client's request, and the surface a request just
    /// named may not survive the rest of the same dispatch pass.
    refocus: bool,
    /// Whether the last settled pass left the keyboard parked on a
    /// grab's whitelist. This is how the keyboard finds its way *back*:
    /// when it is true and no grab is active any more, [`refresh`]
    /// hands the seat to whatever would have had it.
    held_keyboard: bool,
}

impl FocusGrab {
    /// Whether a grab is currently being enforced. Every hook outside
    /// this module tests this first, so a session with no grabbing
    /// client pays one `Vec::iter().any()` over an empty vector.
    pub(crate) fn is_active(&self) -> bool {
        self.grabs.iter().any(|grab| grab.list.is_active())
    }

    /// Whether an input event landing on these surfaces falls *outside*
    /// an active grab — the single predicate `input.rs` routes by.
    ///
    /// Answers `false` when no grab is active, so callers need no
    /// separate guard. Two surfaces are offered because a hit-test
    /// result names two useful things and a client may have
    /// whitelisted either: `surface` is the exact `wl_surface` under
    /// the pointer (a subsurface, or a popup of the popup) and `root`
    /// is the toplevel or layer surface that owns it. Quickshell
    /// whitelists the layer surface; a client that whitelisted only the
    /// popup it just opened is served by the same call.
    pub(crate) fn escapes(&self, surface: Option<&WlSurface>, root: Option<&WlSurface>) -> bool {
        let Some(grab) = self.grabs.iter().find(|grab| grab.list.is_active()) else {
            return false;
        };
        let inside = [surface, root]
            .into_iter()
            .flatten()
            .any(|candidate| whitelisted(&grab.list, candidate));
        !inside
    }

    /// The surface the keyboard belongs on while this grab holds it:
    /// the first live entry of the active whitelist, in the order the
    /// client added them.
    ///
    /// `pub(crate)` because `layers::sync_keyboard` needs the same
    /// answer: its pass runs before [`refresh`] does, so it has to know
    /// where a grab wants the keyboard at the moment an exclusive layer
    /// surface lets go of it.
    pub(crate) fn keyboard_surface(&self) -> Option<WlSurface> {
        let grab = self.grabs.iter().find(|grab| grab.list.is_active())?;
        grab.list.committed().iter().find(|surface| surface.is_alive()).cloned()
    }

    /// Ends the active grab (if any) and sends its `cleared`. Answers
    /// whether there was one, so a caller can log the cause.
    ///
    /// This is the shared tail of every ending except the grab object
    /// being destroyed, which must not send on an object that is gone.
    fn clear_active(&mut self) -> bool {
        let mut cleared = false;
        for grab in self.grabs.iter_mut() {
            if grab.list.finish() {
                grab.resource.cleared();
                cleared = true;
            }
        }
        if cleared {
            self.refocus = true;
        }
        cleared
    }
}

/// Whether `surface`, or any subsurface parent of it, is on the grab's
/// committed whitelist.
///
/// The walk upward is what makes a whitelisted popup keep working after
/// the client wraps its content in a subsurface — a thing every
/// toolkit does eventually, and a thing that would otherwise turn every
/// click *inside* the popup into a dismissal.
fn whitelisted(list: &Whitelist<WlSurface>, surface: &WlSurface) -> bool {
    let mut probe = Some(surface.clone());
    for _ in 0..MAX_SUBSURFACE_DEPTH {
        let Some(current) = probe else {
            return false;
        };
        if list.contains(&current) {
            return true;
        }
        probe = smithay::wayland::compositor::get_parent(&current);
    }
    false
}

/// Creates the global. See the module's integration contract for where
/// this is called from and why the position in `run` matters.
pub(crate) fn init(display_handle: &DisplayHandle) -> FocusGrab {
    // The `GlobalId` is dropped deliberately, exactly as in
    // `protocols::init`: dropping one does not withdraw the global, and
    // nothing in a session's life withdraws this one.
    let _global = display_handle
        .create_global::<Compositor, HyprlandFocusGrabManagerV1, ()>(FOCUS_GRAB_VERSION, ());
    tracing::info!(version = FOCUS_GRAB_VERSION, "hyprland-focus-grab advertised");
    FocusGrab { grabs: Vec::new(), refocus: false, held_keyboard: false }
}

/// Per-pass reconciliation: session lock, dead surfaces, keyboard.
///
/// Ordered, and the order is the ordering table in the module docs read
/// top down. Cheap to nothing when no client has ever bound the
/// protocol, which is every session that is not running a shell that
/// speaks it.
pub(crate) fn refresh(comp: &mut Compositor) {
    if comp.focus_grab.grabs.is_empty() && !comp.focus_grab.refocus {
        return;
    }
    // A session lock outranks everything. The lock has already taken
    // the seat and `input.rs` routes nothing but lock surfaces, so a
    // grab left armed here would only be waiting to fight the unlock
    // for the keyboard — and its client's popup is not on screen to be
    // dismissed by hand.
    if comp.wm.backend().locked && comp.focus_grab.clear_active() {
        tracing::debug!("focus grab cleared by the session lock");
    }
    prune_dead_surfaces(comp);
    settle_keyboard(comp);
}

/// Applies the protocol's implicit `remove_surface` on destruction, by
/// dropping whitelist entries whose surface is no longer alive.
fn prune_dead_surfaces(comp: &mut Compositor) {
    for index in 0..comp.focus_grab.grabs.len() {
        let outcome = comp.focus_grab.grabs[index].list.retain(WlSurface::is_alive);
        match outcome {
            Commit::Unchanged => {}
            Commit::Cleared => {
                tracing::debug!("focus grab cleared: its last whitelisted surface was destroyed");
                comp.focus_grab.grabs[index].resource.cleared();
                comp.focus_grab.refocus = true;
            }
            // `Started` cannot come out of a filter, but treating it
            // like any other change costs a bool and never lies.
            Commit::Started | Commit::Rewhitelisted => comp.focus_grab.refocus = true,
        }
    }
}

/// Points the seat's keyboard at the grab while one holds it, and hands
/// it back when none does.
///
/// Runs only on passes where something changed, so an idle session with
/// a grab open does not re-assert focus sixty times a second — which
/// would be visible, because every `set_focus` that actually moves runs
/// `SeatHandler::focus_changed` and reassigns clipboard access with it.
fn settle_keyboard(comp: &mut Compositor) {
    if !std::mem::take(&mut comp.focus_grab.refocus) {
        return;
    }
    // Rung 1 of the ordering table: the lock owns the seat outright and
    // `lock.rs` puts the keyboard on its own surface. Leaving the flag
    // consumed is right — the lock's own refresh re-derives focus when
    // it lets go.
    if comp.wm.backend().locked {
        return;
    }
    // Rung 2: a layer surface holding *exclusive* interactivity has
    // already been given the keyboard by `layers::sync_keyboard`, and
    // the grab's specification lets the compositor pick which surface
    // is entered. Picking the exclusive claimant is the pick that keeps
    // a launcher typeable.
    if comp.layer_shell.exclusive_focus.is_some() {
        return;
    }
    let Some(keyboard) = comp.seat.get_keyboard() else {
        return;
    };
    match comp.focus_grab.keyboard_surface() {
        Some(target) => {
            comp.focus_grab.held_keyboard = true;
            // The protocol only asks for an enter "if a whitelisted
            // surface is not already entered": a client that whitelists
            // its popup *and* its parent keeps the keyboard where the
            // user put it instead of being yanked to whichever entry
            // happens to be first.
            let already_inside = keyboard
                .current_focus()
                .is_some_and(|focus| !comp.focus_grab.escapes(Some(&focus), None));
            if already_inside {
                return;
            }
            keyboard.set_focus(comp, Some(target), SERIAL_COUNTER.next_serial());
        }
        None => {
            // Nothing to hand back if the grab never had the keyboard —
            // and this matters, because `refocus` is also set by a grab
            // object being destroyed, which is something an idle client
            // does routinely without ever having grabbed anything.
            if !std::mem::take(&mut comp.focus_grab.held_keyboard) {
                return;
            }
            let target = keyboard_fallback(comp);
            keyboard.set_focus(comp, target, SERIAL_COUNTER.next_serial());
        }
    }
}

/// Where the keyboard goes when a grab ends: back to whoever would have
/// had it, which is the same answer `layers.rs` gives when a layer
/// surface stops holding the seat.
///
/// The on-demand layer holder is consulted first because a click on
/// such a surface can predate the grab — `input.rs` suppresses new
/// claims while grabbed, but not ones already made — and handing the
/// keyboard past it to a window would silently revoke a claim nobody
/// released.
fn keyboard_fallback(comp: &Compositor) -> Option<WlSurface> {
    if let Some(id) = comp.layer_shell.on_demand_focus {
        let record = comp.wm.backend().layers.iter().find(|record| record.id == id);
        if let Some(record) = record.filter(|record| record.surface.alive()) {
            return Some(record.surface.wl_surface().clone());
        }
    }
    crate::layers::focused_window_surface(comp)
}

/// Ends the active grab because the user clicked outside it. Called
/// from `input.rs`'s button routing — see the module docs for why a
/// press and not a motion.
pub(crate) fn dismiss(comp: &mut Compositor) {
    if comp.focus_grab.clear_active() {
        tracing::debug!("focus grab cleared by a press outside its whitelist");
    }
}

// ---------------------------------------------------------------------
// Protocol dispatch.
// ---------------------------------------------------------------------

impl GlobalDispatch<HyprlandFocusGrabManagerV1, ()> for Compositor {
    fn bind(
        _state: &mut Self,
        _handle: &DisplayHandle,
        _client: &Client,
        resource: New<HyprlandFocusGrabManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        // The manager is a factory and nothing else: it carries no
        // state, and destroying it explicitly does not destroy the
        // grabs it made (the protocol says so). So there is nothing to
        // record here — the grabs record themselves.
        data_init.init(resource, ());
    }
}

impl Dispatch<HyprlandFocusGrabManagerV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        _resource: &HyprlandFocusGrabManagerV1,
        request: hyprland_focus_grab_manager_v1::Request,
        _data: &(),
        _display_handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        match request {
            hyprland_focus_grab_manager_v1::Request::CreateGrab { grab } => {
                let resource = data_init.init(grab, ());
                // Inert until its first non-empty `commit`, which is
                // why creating one costs nothing: Quickshell creates a
                // grab per popup component at load and commits only the
                // ones the user opens.
                state.focus_grab.grabs.push(Grab { resource, list: Whitelist::new() });
            }
            // Destructors need no body: `Dispatch::destroyed` runs
            // either way, and it is where the bookkeeping lives so that
            // a client which disconnects without saying `destroy` is
            // handled by the same code.
            hyprland_focus_grab_manager_v1::Request::Destroy => {}
        }
    }
}

impl Dispatch<HyprlandFocusGrabV1, ()> for Compositor {
    fn request(
        state: &mut Self,
        _client: &Client,
        resource: &HyprlandFocusGrabV1,
        request: hyprland_focus_grab_v1::Request,
        _data: &(),
        _display_handle: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
        let Some(index) = state.focus_grab.grabs.iter().position(|grab| &grab.resource == resource)
        else {
            // A request against a grab this compositor has already
            // forgotten. Not reachable through `destroyed`, which runs
            // after the last request, but reachable if the vector is
            // ever pruned from somewhere else — so it degrades instead
            // of indexing.
            return;
        };
        match request {
            hyprland_focus_grab_v1::Request::AddSurface { surface } => {
                state.focus_grab.grabs[index].list.add(surface);
            }
            hyprland_focus_grab_v1::Request::RemoveSurface { surface } => {
                state.focus_grab.grabs[index].list.remove(&surface);
            }
            hyprland_focus_grab_v1::Request::Commit => on_commit(state, index),
            hyprland_focus_grab_v1::Request::Destroy => {}
        }
    }

    fn destroyed(
        state: &mut Self,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &HyprlandFocusGrabV1,
        _data: &(),
    ) {
        // "Destroy the grab object and remove the grab if active" — and
        // send nothing, because `cleared` would have to travel on the
        // object that just went away. The keyboard still has to be
        // handed back, which is what the flag is for.
        let Some(index) = state.focus_grab.grabs.iter().position(|grab| &grab.resource == resource)
        else {
            return;
        };
        let mut grab = state.focus_grab.grabs.remove(index);
        if grab.list.finish() {
            state.focus_grab.refocus = true;
        }
    }
}

/// Publishes one grab's staged whitelist and acts on the transition.
fn on_commit(comp: &mut Compositor, index: usize) {
    match comp.focus_grab.grabs[index].list.commit() {
        Commit::Unchanged => {}
        Commit::Cleared => {
            // The client emptied its own whitelist. It still gets the
            // event: the protocol says `cleared` is sent "regardless of
            // cause", and Hyprland — which Quickshell is written
            // against — sends it here too, so a shell that treats the
            // event as its single dismissal path behaves the same on
            // both.
            tracing::debug!("focus grab cleared by its own client committing an empty whitelist");
            comp.focus_grab.grabs[index].resource.cleared();
            comp.focus_grab.refocus = true;
        }
        Commit::Started => {
            // "The same will happen if another focus grab or similar
            // action is started at the compositor's discretion." Only
            // one whitelist can be enforced at a time — two would mean
            // a click that is outside one and inside the other — so the
            // newcomer wins and the incumbent is told.
            for other in 0..comp.focus_grab.grabs.len() {
                if other == index {
                    continue;
                }
                if comp.focus_grab.grabs[other].list.finish() {
                    tracing::debug!("focus grab cleared: superseded by a newer grab");
                    comp.focus_grab.grabs[other].resource.cleared();
                }
            }
            comp.focus_grab.refocus = true;
        }
        Commit::Rewhitelisted => comp.focus_grab.refocus = true,
    }
}

#[cfg(test)]
mod tests {
    use super::{Commit, Whitelist};

    /// The state machine under test, driven with plain integers for
    /// surfaces — see the module docs for why it is generic.
    fn list() -> Whitelist<u32> {
        Whitelist::new()
    }

    #[test]
    fn adding_a_surface_does_not_start_the_grab() {
        let mut list = list();
        list.add(1);
        assert!(!list.is_active(), "add_surface alone must not begin enforcing");
        assert!(!list.contains(&1), "a staged surface is not on the enforced whitelist");
    }

    #[test]
    fn commit_is_what_starts_the_grab() {
        let mut list = list();
        list.add(1);
        assert_eq!(list.commit(), Commit::Started);
        assert!(list.is_active());
        assert!(list.contains(&1));
    }

    #[test]
    fn a_commit_that_changes_nothing_is_silent() {
        let mut list = list();
        list.add(1);
        assert_eq!(list.commit(), Commit::Started);
        assert_eq!(list.commit(), Commit::Unchanged, "a second commit must not re-start it");
    }

    #[test]
    fn committing_nothing_at_all_is_not_a_transition() {
        let mut list = list();
        assert_eq!(list.commit(), Commit::Unchanged);
        assert!(!list.is_active());
    }

    #[test]
    fn duplicate_additions_are_ignored() {
        let mut list = list();
        list.add(1);
        list.add(1);
        list.commit();
        assert_eq!(list.committed(), &[1], "the protocol requires duplicates to collapse");
    }

    #[test]
    fn a_removal_before_commit_never_takes_effect() {
        let mut list = list();
        list.add(1);
        list.add(2);
        list.remove(&2);
        assert_eq!(list.commit(), Commit::Started);
        assert_eq!(list.committed(), &[1]);
    }

    #[test]
    fn removing_the_last_surface_and_committing_clears_once() {
        let mut list = list();
        list.add(1);
        list.commit();
        list.remove(&1);
        assert_eq!(list.commit(), Commit::Cleared);
        assert!(!list.is_active());
        // The whole point: a second commit finds nothing to say.
        assert_eq!(list.commit(), Commit::Unchanged, "cleared must fire exactly once");
    }

    #[test]
    fn a_cleared_grab_cannot_be_resurrected_by_stale_staging() {
        let mut list = list();
        list.add(1);
        list.add(2);
        list.commit();
        // The compositor ends it — a click outside, a lock, a newer
        // grab. Everything the client staged goes with it.
        assert!(list.finish());
        assert_eq!(
            list.commit(),
            Commit::Unchanged,
            "committing after a compositor-side clear must not restart the grab"
        );
        assert!(!list.is_active());
    }

    #[test]
    fn shrinking_a_whitelist_without_emptying_it_keeps_the_grab() {
        let mut list = list();
        list.add(1);
        list.add(2);
        list.commit();
        list.remove(&1);
        assert_eq!(list.commit(), Commit::Rewhitelisted);
        assert!(list.is_active());
        assert_eq!(list.committed(), &[2]);
    }

    #[test]
    fn a_destroyed_surface_leaves_the_whitelist_without_being_asked() {
        let mut list = list();
        list.add(1);
        list.add(2);
        list.commit();
        assert_eq!(list.retain(|surface| *surface != 1), Commit::Rewhitelisted);
        assert_eq!(list.committed(), &[2], "destruction is an implicit remove_surface");
    }

    #[test]
    fn destroying_the_last_whitelisted_surface_clears_the_grab_once() {
        let mut list = list();
        list.add(1);
        list.commit();
        assert_eq!(list.retain(|_| false), Commit::Cleared);
        assert!(!list.is_active());
        assert_eq!(list.retain(|_| false), Commit::Unchanged, "cleared must fire exactly once");
    }

    #[test]
    fn destroying_a_staged_surface_leaves_the_enforced_whitelist_alone() {
        let mut list = list();
        list.add(1);
        list.commit();
        list.add(2);
        assert_eq!(
            list.retain(|surface| *surface != 2),
            Commit::Rewhitelisted,
            "the staged entry left, and the enforced list is unchanged in content"
        );
        assert!(list.is_active());
        assert_eq!(list.committed(), &[1]);
    }

    #[test]
    fn a_filter_that_rejects_nothing_is_free() {
        let mut list = list();
        list.add(1);
        list.commit();
        assert_eq!(list.retain(|_| true), Commit::Unchanged);
    }

    #[test]
    fn finish_answers_whether_a_cleared_is_owed() {
        let mut list = list();
        list.add(1);
        assert!(!list.finish(), "an inert grab owes nobody an event");
        list.add(1);
        list.commit();
        assert!(list.finish(), "an active grab owes exactly one");
        assert!(!list.finish(), "and never a second");
    }
}
