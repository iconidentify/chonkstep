//! virtual-keyboard-unstable-v1: letting a program type.
//!
//! X11 had `XTestFakeKeyEvent`, and a decade of desktop plumbing was
//! built on it — paste-as-keystrokes into a field that refuses a real
//! paste, an emoji picker that inserts rather than copies, dictation
//! that lands in whatever is focused. Wayland deliberately removed the
//! X-Test hole and replaced it with `zwp_virtual_keyboard_manager_v1`:
//! a client creates a keyboard on a seat, uploads an xkb keymap of its
//! own, and sends key and modifier events through it.
//!
//! Without this global `wtype` prints
//!
//! ```text
//! Compositor does not support the virtual keyboard protocol
//! ```
//!
//! and exits, which takes three Omarchy features down with it:
//! `omarchy-clipboard-paste-text` and `-file` (paste into terminals
//! and password fields that ignore the clipboard), the emoji menu's
//! insert action (`omarchy-menu-emoji-insert`), and voice dictation
//! (`omarchy-voxtype-*`), whose entire delivery mechanism is "type the
//! transcript into the focused window". Every one of them shells out
//! to `wtype`, so all three fail identically and for this one reason.
//!
//! # Smithay ships the protocol; this module ships the decisions
//!
//! Unlike the wlr protocols in [`crate::protocols`], this one is fully
//! implemented upstream (`smithay::wayland::virtual_keyboard`) — the
//! keymap upload, the xkb state, the `NoKeymap` protocol error, the
//! delivery to focused `wl_keyboard`s. What is left is three choices
//! Smithay hands back to the compositor, and they are the whole of
//! this file.
//!
//! ## The seat, and the keyboard it must already have
//!
//! A virtual keyboard is created *on a `wl_seat` the client names*,
//! not on some ambient default. Smithay resolves that resource with
//! `Seat::from_resource` and then calls `get_keyboard().unwrap()` on
//! it when the first key arrives. Two consequences worth stating
//! outright:
//!
//! - This session has exactly one seat (`"chonkstep"`, built in
//!   `state::run`), so any seat a client can reach through the
//!   registry is that seat and the question of "which one" never
//!   becomes interesting. It would the moment a second seat appeared.
//! - That seat must carry a keyboard capability *before* a client
//!   sends a key, or the `unwrap` aborts the compositor — the whole
//!   session, from a synthetic keystroke, with no console under it in
//!   a real login. `run` adds one unconditionally and fails startup if
//!   it cannot; [`init`] re-checks and says so loudly rather than
//!   trusting that to stay true, because the failure is a process
//!   abort triggered remotely by an ordinary client.
//!
//! ## The keymap swap, and the thing that undoes it
//!
//! This is the part that looks like it works and does not. `wtype`
//! does not translate its text into this session's layout; it builds a
//! throwaway keymap containing exactly the characters it wants to
//! type, uploads that, and sends keycodes meaningful only in it. So
//! Smithay, before forwarding the key, re-sends *that* keymap to every
//! `wl_keyboard` on the seat — the focused client is briefly running
//! on `wtype`'s layout, which is the only way the keycodes mean what
//! `wtype` intended. Type "x" with a Dvorak session keymap left in
//! place and the client receives some other letter entirely.
//!
//! Nothing in this module puts the real keymap back. What does is
//! `KeyboardInnerHandle::input`, deep in Smithay's keyboard path,
//! which re-asserts the seat's own keymap on every physical key event
//! before delivering it. That restoration is therefore *conditional on
//! real input still going through `KeyboardHandle::input`* — which is
//! precisely what `crate::input::on_keyboard_key` does today. If
//! anyone ever routes physical keys around it and sends
//! `wl_keyboard.key` by hand, the visible symptom will not be a
//! virtual-keyboard bug: it will be the user's real keyboard typing
//! garbage in one window after a paste, until they focus something
//! else. Hence this paragraph rather than a line comment.
//!
//! ## Virtual keys reach clients, and only clients
//!
//! Smithay's handler writes straight to the focused surface's
//! `wl_keyboard`. It does not run the key through the seat's xkb state
//! and it does not pass through `crate::input::on_keyboard_key`, so
//! the WM's grab contract never sees it: a virtual keyboard cannot
//! trigger a compositor keybinding, cannot drive Alt-Tab, and cannot
//! switch VTs. That is the right shape — a program synthesising text
//! into a text field has no business reaching the window manager's own
//! bindings — but it is a real limit, and anyone who later wants
//! `wtype`-driven WM automation should know it is not a bug to fix
//! here.
//!
//! It follows that keys land wherever keyboard focus is, including a
//! lock surface while the session is locked. A client that already
//! holds a virtual keyboard when the screen locks can type into the
//! lock dialog. It cannot read anything back, and it cannot guess a
//! password it does not have, so this is noted as a known property
//! rather than defended against — the protocol offers no per-request
//! hook to defend it with, only the bind-time filter below, and a
//! filter evaluated at bind time cannot answer a question ("is the
//! session locked *now*?") whose answer changes after the bind.
//!
//! # Trust model
//!
//! [`may_create_virtual_keyboard`] is where a compositor says no, and
//! this one says yes to every client. That deserves an argument,
//! because the reflex — "this protocol lets any client type into any
//! other client" — is correct and the conclusion still does not
//! follow.
//!
//! The reasoning `test_door.rs` uses for its debugging socket
//! ("anything that can set the compositor's environment and reach the
//! socket path already runs as the user") does **not** transfer as
//! written. That door demands a privilege — writing the compositor's
//! environment before it starts — that an ordinary client does not
//! have. This global is bound off the registry by anything that opened
//! the Wayland socket. The premise is different, so the argument has
//! to be made again from scratch.
//!
//! Made again, it lands in the same place, for a different reason:
//! this session grants no client less than user privilege in the first
//! place. Any client on this socket can already capture every pixel of
//! every other client's window (`zwlr_screencopy_manager_v1`,
//! unfiltered — see [`crate::protocols::init`]), enumerate every
//! window and close or raise any of them
//! (`zwlr_foreign_toplevel_manager_v1`, likewise unfiltered), and lock
//! the session out from under the user (`ext_session_lock_v1`, whose
//! filter `state::run` passes `|_| true`, with its own note as to
//! why). Nearest of all: it can read every copy the user makes, at the
//! moment they make it and without ever holding focus, through
//! data-control (see [`crate::data_control`], which grants that to
//! every client for its own stated reasons). A protocol that hands out
//! the contents of the clipboard and one that hands out the ability to
//! type are the same trust decision viewed from two sides, and this
//! session has already made it once. It is also, being a process of
//! this user, free to `ptrace`
//! the compositor, write `~/.bashrc`, or start its own `wtype` against
//! any other compositor the user runs. Reading a password field is
//! already within reach; typing into one is a smaller step, not a new
//! kind of one. Refusing it would break `wtype` — and the three
//! Omarchy features above — while moving nothing out of an attacker's
//! reach.
//!
//! What would change the answer is a sandboxing story: a Flatpak-style
//! client that is *not* fully the user, admitted through
//! `wp_security_context_v1`, for which "can this client type into my
//! bank tab" is a real question with a real answer. Smithay implements
//! that protocol (`wayland::security_context`) and this compositor does
//! not use it; nothing here tags a client as untrusted, so there is
//! nothing for a filter to test. [`may_create_virtual_keyboard`] is
//! deliberately a named function taking the `Client` rather than an
//! inline `|_| true` so that the day a security context exists, the
//! gate is a body to fill in and a test to extend rather than a design
//! to invent.
//!
//! # Integration contract
//!
//! One field, one init call, no per-pass work — there is no state to
//! reconcile, because a virtual keyboard's entire lifetime lives on
//! its own protocol object.
//!
//! 1. `Compositor` gains one field:
//!
//!    ```ignore
//!    /// virtual-keyboard-v1: the global `wtype` (and every Omarchy
//!    /// feature built on it) looks for — see
//!    /// `crate::virtual_keyboard`.
//!    pub(crate) _virtual_keyboard: VirtualKeyboardManagerState,
//!    ```
//!
//!    Underscored for the same reason `Idle::_inhibit` is: it is held
//!    so the global's id outlives `run` and is never read again.
//!
//! 2. In `run`, after the seat has its keyboard and — the same timing
//!    rule every other global obeys — **before** the
//!    `ListeningSocketSource` is inserted, since a global missing when
//!    a client binds might as well not exist:
//!
//!    ```ignore
//!    let virtual_keyboard = crate::virtual_keyboard::init(&display_handle, &seat);
//!    ```
//!
//!    then `_virtual_keyboard: virtual_keyboard,` into the
//!    `Compositor { .. }` literal.

use smithay::delegate_virtual_keyboard_manager;
use smithay::input::Seat;
use smithay::reexports::wayland_server::{Client, DisplayHandle};
use smithay::wayland::virtual_keyboard::VirtualKeyboardManagerState;

use crate::state::Compositor;

/// Registers `zwp_virtual_keyboard_manager_v1`. Called once from `run`
/// — see the module's integration contract for where, and why the
/// order matters in both directions (after the seat's keyboard exists,
/// before any client can connect).
///
/// Never fails. The one thing that can be wrong at this point — a seat
/// with no keyboard capability, which turns the first synthetic
/// keystroke into a process abort inside Smithay — is not a reason to
/// refuse to start a session that is otherwise fine, so it is reported
/// at `error` and the global goes up anyway.
pub(crate) fn init(display_handle: &DisplayHandle, seat: &Seat<Compositor>) -> VirtualKeyboardManagerState {
    if !seat_can_accept_virtual_keys(seat) {
        tracing::error!(
            seat = seat.name(),
            "virtual-keyboard advertised on a seat with no keyboard: the first key a \
             client sends will abort the compositor inside smithay's handler"
        );
    }
    let state =
        VirtualKeyboardManagerState::new::<Compositor, _>(display_handle, may_create_virtual_keyboard);
    tracing::info!("virtual-keyboard advertised");
    state
}

/// Whether `seat` is in a state where Smithay's virtual-keyboard
/// handler can deliver a key without panicking.
///
/// The handler reaches for `seat.get_keyboard().unwrap()` on every
/// `key` and `modifiers` request, so a keyboardless seat is not a
/// degraded virtual keyboard — it is a client-triggerable abort of the
/// whole session. Split out from [`init`] so the invariant has a name
/// and a test, rather than living only in a startup log line nobody
/// reads.
fn seat_can_accept_virtual_keys(seat: &Seat<Compositor>) -> bool {
    seat.get_keyboard().is_some()
}

/// Which clients may create a virtual keyboard. Every one of them, on
/// this desktop, for the reasons argued at length in the module's
/// trust-model section — the short version being that a client on this
/// socket already holds user privilege by every other route
/// (screencopy, foreign-toplevel control, session lock, `ptrace`), so
/// denying it synthetic keystrokes breaks `wtype` without taking
/// anything away from an attacker.
///
/// This is a function and not `|_| true` on purpose: it is the exact
/// place a `wp_security_context_v1` check belongs if this compositor
/// ever admits clients that are less than the user, and having the
/// `Client` already in hand makes that a body to write rather than a
/// signature to thread.
fn may_create_virtual_keyboard(_client: &Client) -> bool {
    true
}

delegate_virtual_keyboard_manager!(Compositor);

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::input::keyboard::XkbConfig;
    use smithay::input::SeatState;
    use smithay::reexports::wayland_server::Display;

    // Typing itself needs a client on the other end of a socket and a
    // focused surface to type into, so the delivery path is proved by
    // running a session and pointing `wtype` at it. What is testable
    // here is the invariant whose violation is a remote abort, and the
    // trust decision, which is policy and should not change silently.

    /// The precondition Smithay's handler `unwrap`s on. A seat is born
    /// without a keyboard and gains one in `run`; if that call is ever
    /// made conditional (a session with no input devices, a hot-plug
    /// rework), this is the check that has to move with it.
    #[test]
    fn a_seat_only_accepts_virtual_keys_once_it_has_a_keyboard() {
        let display = Display::<Compositor>::new().expect("wayland display");
        let display_handle = display.handle();
        let mut seat_state = SeatState::<Compositor>::new();
        let mut seat = seat_state.new_wl_seat(&display_handle, "test");

        assert!(
            !seat_can_accept_virtual_keys(&seat),
            "a fresh seat has no keyboard capability yet"
        );

        seat.add_keyboard(XkbConfig::default(), 200, 25)
            .expect("the default xkb keymap must compile");
        assert!(seat_can_accept_virtual_keys(&seat));
    }

    /// The filter is unfiltered, deliberately. This test is here to
    /// make that a decision with a paper trail: anyone narrowing it is
    /// meant to arrive at the module's trust-model section by way of a
    /// failing assertion.
    #[test]
    fn every_client_on_this_socket_may_create_a_virtual_keyboard() {
        let display = Display::<Compositor>::new().expect("wayland display");
        let mut display_handle = display.handle();
        let (compositor_end, _client_end) =
            std::os::unix::net::UnixStream::pair().expect("socketpair");
        let client = display_handle
            .insert_client(compositor_end, std::sync::Arc::new(crate::state::ClientState::default()))
            .expect("admit a client");

        assert!(may_create_virtual_keyboard(&client));
    }
}
