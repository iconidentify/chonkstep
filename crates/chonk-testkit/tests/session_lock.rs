//! End-to-end coverage for the session-lock teardown: the moment a
//! locker unlocks must cost that client NOTHING but its lock objects,
//! and must leave the session exactly as lockable as it was before.
//!
//! The client under test (`chonk-lock-probe`, this crate's own locker)
//! is shaped like Omarchy's Quickshell, because that shape is what
//! found the bug: ONE connection holding both a layer surface (a bar)
//! and the ext-session-lock. On the live desktop, the first
//! lock→PAM→unlock cycle under chonkstep killed `omarchy-shell`
//! outright — "The Wayland connection broke. Did the Wayland
//! compositor die?" — taking the bar and every OSD with it, though the
//! compositor was fine.
//!
//! The mechanism, and the probe's script, are the real client's,
//! captured from `/usr/bin/qs` running Omarchy's `plugins/lock`
//! against a nested chonkstep under `WAYLAND_DEBUG` (see the probe's
//! module docs for the trace): Qt answers `unlock_and_destroy` by
//! destroying the role object and then unmapping the `wl_surface` it
//! is about to drop — `attach(nil)`, `commit`. smithay leaves its
//! session-lock pre-commit hook on that `wl_surface` after the role is
//! gone (the exact twin of its layer-shell bug, see
//! `layers::install_orphaned_role_guard`), so that commit was answered
//! with a fatal `null_buffer` protocol error on a dead object, which
//! kills the whole connection. `lock::install_defunct_lock_role_guard`
//! is the fix; this test is its regression net, and fails (the probe
//! exits 2 at "the unlock teardown") the day the guard stops covering
//! the hook.
//!
//! Three cycles, each earning its place:
//!
//! 1. the kept-surface teardown that killed the shell, after which the
//!    bar must still be serviced AND still hold the keyboard;
//! 2. the same cycle on a fresh `wl_surface`, destroyed in full — what
//!    the real client actually does, twice over;
//! 3. a re-lock on the surface from cycle 1, which already wore the
//!    lock-surface role. The spec calls that a client error and the
//!    real client never makes it; what this pins is that making it
//!    cannot blank the session behind a locker that never receives its
//!    mandatory first configure and so can never draw
//!    (`lock::prime_reused_lock_surface`).
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::time::Duration;

/// The probe's captured stdout/stderr — checkpoint lines, and on a
/// kill, the connection post-mortem it prints before exiting.
fn probe_log(session: &Session) -> String {
    std::fs::read_to_string(session.dir.join("client-0-chonk-lock-probe.log")).unwrap_or_default()
}

/// Waits for the probe to print `checkpoint`, failing with the whole
/// probe log — which, when the compositor killed it, contains its
/// "connection broke" post-mortem, the most useful thing a failure
/// here can say.
fn checkpoint(session: &Session, checkpoint: &str) {
    poll_until(Duration::from_secs(15), &format!("the probe to report {checkpoint:?}"), || {
        probe_log(session).contains(checkpoint).then_some(())
    })
    .unwrap_or_else(|timeout| panic!("{timeout}\n-- probe log --\n{}", probe_log(session)));
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_lockers_other_surfaces_survive_the_unlock_teardown() {
    let mut session =
        Session::boot("session-lock", SessionOptions { scale: Some(1.0), ..Default::default() }).unwrap();
    let probe = profile_binary("chonk-lock-probe").expect("cargo build -p chonk-testkit builds the probe");
    session.launch(probe.to_str().unwrap(), &[]).expect("the probe launches");

    // -- the bar maps and takes the keyboard, then the lock engages -------
    checkpoint(&session, "layer mapped");
    checkpoint(&session, "bar holds the keyboard");
    // `locked` on the probe's side means the compositor's whole accept
    // path ran: blank, present a locked frame, confirm. Its own log
    // agrees, in order.
    checkpoint(&session, "locked ");
    assert!(
        session.log().contains("session locking; blanking outputs"),
        "the compositor should have logged the lock engaging"
    );

    // -- the teardown under test ------------------------------------------
    // unlock_and_destroy, destroy the lock surface's role object, then
    // Qt's unmap of the kept wl_surface: attach(nil) + commit. Before
    // the defunct-role guard, the compositor answered that commit with
    // ext_session_lock_surface_v1.null_buffer on the destroyed object
    // and the probe died here with a broken connection, bar and all —
    // exactly how omarchy-shell went down on the live desktop.
    checkpoint(&session, "survived the unlock teardown");
    assert!(session.log().contains("session unlocked"), "the unlock itself must have gone through");

    // -- not merely unkilled: the same connection is still serviced -------
    // A fresh frame on the bar gets its frame callback back, and the
    // keyboard the lock took from the bar comes home to it — an unlock
    // that hands the seat to a window and stops there leaves an
    // exclusive-interactivity layer surface (Omarchy's popouts, its own
    // lock preview) on screen and deaf, with nothing left to re-assert
    // it: `layers::sync_keyboard` moves the seat only when the
    // exclusive claimant *changes*, and across a lock cycle it does not.
    checkpoint(&session, "layer surface serviced after unlock");
    checkpoint(&session, "bar has the keyboard back");

    // -- the whole cycle again, on a fresh wl_surface ---------------------
    // The real client's shape, destroy included.
    checkpoint(&session, "relocked ");
    checkpoint(&session, "survived the second unlock teardown");
    checkpoint(&session, "bar has the keyboard back again");

    // -- and the hostile third cycle --------------------------------------
    // A re-lock on the surface cycle 1 kept. smithay accepts the re-use
    // and would then dedup the mandatory first configure away against
    // the bookkeeping the first lock left on that wl_surface, which
    // blanks the session behind a locker that can never draw. Reaching
    // this checkpoint at all is the assertion: the probe blocks
    // forever on that configure otherwise.
    checkpoint(&session, "relocked on a reused surface ");
    checkpoint(&session, "survived the third unlock teardown");

    // The probe never printed a post-mortem, the bar was never closed,
    // and the compositor is still standing.
    let log = probe_log(&session);
    assert!(!log.contains("connection broke"), "the probe reported a broken connection:\n{log}");
    assert!(session.compositor_alive(), "the compositor must outlive all three cycles");
    session.kill_client("chonk-lock-probe");
}
