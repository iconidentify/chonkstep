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
//! The second test in this file is about the way out rather than the
//! teardown: `ext-session-lock-v1` is the session's one security
//! boundary, and `unlock_and_destroy` has to be answered on the
//! strength of WHO sent it. Two clients — the locker holding a
//! confirmed lock, and `chonk-lock-thief` making the three requests a
//! bypass needs — against the assertion that the screen is still a
//! wall afterwards. See its own comments for the mechanism.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::time::Duration;

/// The probe's captured stdout/stderr — checkpoint lines, and on a
/// kill, the connection post-mortem it prints before exiting.
fn probe_log(session: &Session) -> String {
    session.client_log("chonk-lock-probe")
}

/// Waits for the probe to print `checkpoint`, failing with the whole
/// probe log — which, when the compositor killed it, contains its
/// "connection broke" post-mortem, the most useful thing a failure
/// here can say.
fn checkpoint(session: &Session, checkpoint: &str) {
    client_checkpoint(session, "chonk-lock-probe", checkpoint);
}

/// [`checkpoint`] for any of this test file's probes, named by binary.
fn client_checkpoint(session: &Session, client: &str, checkpoint: &str) {
    poll_until(Duration::from_secs(15), &format!("{client} to report {checkpoint:?}"), || {
        session.client_log(client).contains(checkpoint).then_some(())
    })
    .unwrap_or_else(|timeout| {
        panic!("{timeout}\n-- {client} log --\n{}", session.client_log(client))
    });
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

/// The lock screen's fill, as `chonk-lock-probe` paints it —
/// `LOCK_NAVY` in the probe is premultiplied ARGB8888 little-endian
/// (B, G, R, A), so the RGB a screenshot reads back is its middle
/// three bytes reversed.
const LOCK_NAVY_RGB: [u8; 3] = [0x08, 0x18, 0x40];

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_client_that_does_not_hold_the_lock_cannot_unlock_the_session() {
    // The lock-screen bypass, driven over the real wire by two real
    // clients. `chonk-lock-probe --hold` is the user's locker: it takes
    // the lock, draws, is confirmed, and then does nothing — the state
    // a locker sits in while its owner is away from the machine.
    // `chonk-lock-thief` is any other process on the socket, and makes
    // the only three requests the bypass needs.
    //
    // The regression this pins is not the thief's fate — a compositor
    // may answer an impostor with a protocol error or with silence, and
    // either is defensible. It is whether the SESSION is still locked
    // afterwards, which is why the assertions below are a screenshot of
    // the desk and the compositor's own log, not the attacker's report.
    let mut session =
        Session::boot("session-lock-bypass", SessionOptions { scale: Some(1.0), ..Default::default() })
            .unwrap();
    let probe = profile_binary("chonk-lock-probe").expect("cargo build -p chonk-testkit builds the probe");
    let thief = profile_binary("chonk-lock-thief").expect("cargo build -p chonk-testkit builds the thief");

    // -- the locker takes the session and keeps it ------------------------
    session.launch(probe.to_str().unwrap(), &["--hold"]).expect("the locker launches");
    checkpoint(&session, "locked ");
    checkpoint(&session, "holding the lock");

    // What a locked session looks like from outside: the locker's navy
    // fills the output, because `renderer::build_scene` returns before
    // it can reach a single non-lock surface — including for `grim`,
    // which is the client taking this picture.
    session.door().barrier().expect("the compositor answers a barrier while locked");
    let locked = session.screenshot("locked").expect("grim captures the locked session");
    assert!(
        chonk_testkit::near(locked.centre_rgb(), LOCK_NAVY_RGB),
        "the session should be showing the lock screen before the attack, saw {:?} in {}",
        locked.centre_rgb(),
        locked.path.display()
    );

    // -- the attack --------------------------------------------------------
    session.launch(thief.to_str().unwrap(), &[]).expect("the thief launches");
    client_checkpoint(&session, "chonk-lock-thief", "bound the lock manager");
    // The compositor must refuse a second lock while a live locker
    // holds one — the denial is what leaves the thief a live
    // `ext_session_lock_v1` to send the bypass on, so a session that
    // GRANTED the lock here would be broken in a worse way.
    client_checkpoint(&session, "chonk-lock-thief", "lock refused");
    assert!(
        session.log().contains("refusing a session lock"),
        "the compositor should have logged refusing the second lock"
    );
    client_checkpoint(&session, "chonk-lock-thief", "unlock_and_destroy sent");
    // Whichever way it was answered, the answer has been dispatched by
    // the time the thief prints this.
    poll_until(Duration::from_secs(15), "the thief to report how the unlock was answered", || {
        let log = session.client_log("chonk-lock-thief");
        (log.contains("refused: ") || log.contains("accepted without error")).then_some(())
    })
    .unwrap_or_else(|timeout| {
        panic!("{timeout}\n-- thief log --\n{}", session.client_log("chonk-lock-thief"))
    });

    // -- the session is still a wall ---------------------------------------
    // `unlock()` is the only writer of "session unlocked" and the only
    // path that clears `backend.locked`, so its absence is a direct
    // assertion on the flag every render and input gate reads.
    let log = session.log();
    assert!(
        !log.contains("session unlocked"),
        "a client that does not hold the lock unlocked the session:\n{}",
        chonk_testkit::strip_ansi(&log)
    );
    // And the refusal is on the record as a security event, at `error`,
    // with the offending process named.
    assert!(
        log.contains("refusing unlock_and_destroy"),
        "the refused unlock should have been logged:\n{}",
        chonk_testkit::strip_ansi(&log)
    );

    // The picture is the proof: still the locker's navy, and the same
    // frame as before the attack rather than the desktop behind it.
    session.door().barrier().expect("the compositor still answers a barrier");
    let after = session.screenshot("after-bypass-attempt").expect("grim captures the session again");
    assert!(
        chonk_testkit::near(after.centre_rgb(), LOCK_NAVY_RGB),
        "the session came out of the lock: centre {:?} in {}",
        after.centre_rgb(),
        after.path.display()
    );
    assert!(
        locked.diff_fraction(&after, 8) < 0.01,
        "the screen changed across the bypass attempt: {} in {}",
        locked.diff_fraction(&after, 8),
        after.path.display()
    );

    // The locker itself was never collateral: refusing the impostor
    // must cost the client that legitimately holds the lock nothing.
    let held = probe_log(&session);
    assert!(!held.contains("connection broke"), "the locker's connection was broken:\n{held}");
    assert!(session.compositor_alive(), "the compositor must outlive the bypass attempt");
    session.kill_client("chonk-lock-thief");
    session.kill_client("chonk-lock-probe");
}
