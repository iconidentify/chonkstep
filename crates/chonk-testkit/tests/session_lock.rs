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

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions};
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
#[ignore = "real Qt locker: scripts/e2e.sh --headless --test session_lock"]
fn quickshell_lock_fills_the_output_at_fractional_and_integer_scales() {
    if !chonk_testkit::require_client("qs") {
        return;
    }
    for scale in [1.0, 1.5, 2.0] {
        let mut session = Session::boot(&format!("lock-scale-{scale}"), SessionOptions {
            scale: Some(scale), ..Default::default()
        }).unwrap();
        let fixture = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/scaled-lock.qml");
        session.launch("qs", &["-p", fixture.to_str().unwrap()]).unwrap();
        poll_until(Duration::from_secs(15), "Qt lock covers all four output corners", || {
            if !session.log().contains("lock surface created") { return None; }
            let shot = session.screenshot("quickshell-lock").ok()?;
            [(8, 8), (shot.width - 9, 8), (8, shot.height - 9), (shot.width - 9, shot.height - 9)]
                .into_iter().all(|(x, y)| shot.pixel(x, y) == [8, 24, 64, 255]).then_some(())
        }).unwrap_or_else(|error| panic!("{error}\n{}", session.client_log("qs")));
        assert_eq!(session.door().hit(1100, 650).unwrap(), "lock");
    }
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

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_second_output_resource_cannot_cover_one_physical_output_twice() {
    let mut session = Session::boot(
        "session-lock-duplicate-output",
        SessionOptions { scale: Some(1.0), ..Default::default() },
    )
    .unwrap();
    let probe = profile_binary("chonk-lock-probe").expect("cargo build -p chonk-testkit builds the probe");
    session
        .launch(probe.to_str().unwrap(), &["--duplicate-output"])
        .expect("the duplicate-output locker launches");
    checkpoint(&session, "ready for duplicate output request");
    assert_eq!(
        session.door().protocol_ledgers().unwrap().lock,
        1,
        "one accepted surface is the entire physical-output lock ledger"
    );

    session.door().tap_key(keys::SPACE).unwrap();
    checkpoint(&session, "duplicate output refused");
    poll_until(Duration::from_secs(5), "the refused client's lock ledger to be removed", || {
        (session.door().protocol_ledgers().ok()?.lock == 0).then_some(())
    })
    .unwrap();
    assert!(
        !session.log().contains("session unlocked"),
        "refusing the duplicate must leave the crashed-locker security domain locked"
    );
    assert!(session.compositor_alive(), "the compositor survives the refused duplicate");
}

/// The fill `chonk-lock-probe --stale-output` gives the lock surface it
/// requests for an output that is already gone (`STALE_MAGENTA`, B, G, R,
/// A in the probe).
const STALE_MAGENTA_RGB: [u8; 3] = [0xC0, 0x20, 0xC0];

/// The last `limit` lines of the compositor log, where a panic lands.
fn log_tail(session: &Session, limit: usize) -> String {
    let log = session.log();
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(limit)..].join("\n")
}

/// The newest line of the probe's log that starts with `prefix`.
fn last_report(session: &Session, prefix: &str) -> Option<String> {
    probe_log(session).lines().rev().find(|line| line.starts_with(prefix)).map(str::to_owned)
}

/// Launches `chonk-lock-probe --stale-output CONNECTOR`, waits until it
/// holds a lock on every output and is parked before its stale request,
/// and returns the marker file that releases that request.
fn launch_stale_output_locker(session: &mut Session, connector: &str, outputs: &str) -> std::path::PathBuf {
    let marker = session.dir.join("stale-output-request");
    let probe = profile_binary("chonk-lock-probe").expect("cargo build -p chonk-testkit builds the probe");
    session
        .launch(probe.to_str().unwrap(), &["--stale-output", connector, marker.to_str().unwrap()])
        .expect("the stale-output locker launches");
    checkpoint(session, &format!("locked on every output: {outputs}\n"));
    checkpoint(session, "ready for stale output request");
    marker
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_lock_surface_for_the_last_output_to_leave_keeps_an_empty_desk_locked() {
    // The race of a lone monitor unplugged, or dropping hotplug detect as
    // it sleeps, while a locker sets up: the locker bound the output,
    // the output left, and the locker's `get_lock_surface` for it was
    // already on its way when the `global_remove` went out. The request
    // used to fall back to output index 0 and index an empty output list
    // inside Wayland dispatch, which killed the compositor on the path
    // that sets up a lock.
    let mut session =
        Session::boot("session-lock-stale-output-empty-desk", SessionOptions { scale: Some(1.0), ..Default::default() })
            .unwrap();
    let marker = launch_stale_output_locker(&mut session, "chonkstep", "chonkstep");

    // -- the last output leaves under a parked request ----------------------
    session.door().set_virtual_outputs("none").unwrap();
    assert_eq!(
        session.door().protocol_ledgers().unwrap().lock,
        1,
        "the unplugged output's lock surface stays its client's"
    );
    std::fs::write(&marker, "").unwrap();
    poll_until(Duration::from_secs(15), "the stale lock request to be answered", || {
        let log = probe_log(&session);
        (log.contains("stale lock surface configured") || log.contains("connection broke")).then_some(())
    })
    .unwrap_or_else(|timeout| panic!("{timeout}\n-- chonk-lock-probe log --\n{}", probe_log(&session)));
    assert!(
        !probe_log(&session).contains("connection broke"),
        "the locker lost its connection over a lock surface for a vanished output:\n{}\n-- compositor log tail --\n{}",
        probe_log(&session),
        log_tail(&session, 40)
    );
    assert!(
        session.compositor_alive(),
        "a lock surface naming a vanished output must not take the compositor down:\n{}",
        log_tail(&session, 40)
    );

    // -- still the locker's session, still locked ---------------------------
    // The stale surface got its mandatory first configure (the probe
    // printed it) and is on the ledger beside the unplugged one.
    assert_eq!(session.door().protocol_ledgers().unwrap().lock, 2);
    assert!(
        !session.log().contains("session unlocked"),
        "an unresolvable lock surface must leave the session locked"
    );

    // A connector coming back is a new wl_output the locker has not
    // covered yet: the locked scene's black, not the desktop and not
    // either of the surfaces that name no output.
    session.door().set_virtual_outputs("single").unwrap();
    let returned = session.screenshot("stale-output-connector-returns").expect("grim captures the returned output");
    for y in (5..returned.height).step_by(97) {
        for x in (5..returned.width).step_by(89) {
            assert_eq!(
                returned.pixel(x, y)[..3],
                [0, 0, 0],
                "the returned output should show the uncovered locked scene at ({x}, {y}) in {}",
                returned.path.display()
            );
        }
    }
    assert!(!session.log().contains("session unlocked"), "the session must still be locked after the reconnect");
    assert!(session.compositor_alive(), "the compositor must outlive the reconnect");
    session.kill_client("chonk-lock-probe");
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_lock_surface_for_an_unplugged_output_never_covers_the_remaining_one() {
    // The same race with a monitor left over: one of two outputs leaves
    // while the locker's request for it is in flight. That request used
    // to be filed under the primary, beside the primary's own lock
    // surface and past the one-surface-per-output guard, so which of the
    // two was drawn on top and which took the pointer came down to list
    // order rather than to the locker.
    let mut session =
        Session::boot("session-lock-stale-output-split", SessionOptions { scale: Some(1.0), ..Default::default() })
            .unwrap();
    session.door().set_virtual_outputs("split").unwrap();
    let marker = launch_stale_output_locker(&mut session, "chonkstep-right", "chonkstep, chonkstep-right");
    assert_eq!(session.door().protocol_ledgers().unwrap().lock, 2, "one lock surface per output");

    // -- the right-hand output leaves under a parked request ----------------
    session.door().set_virtual_outputs("single").unwrap();
    assert_eq!(
        session.door().protocol_ledgers().unwrap().lock,
        2,
        "the unplugged output's lock surface stays its client's"
    );
    std::fs::write(&marker, "").unwrap();
    checkpoint(&session, "stale lock surface configured");
    assert_eq!(session.door().protocol_ledgers().unwrap().lock, 3);

    // -- drawn: the primary's own surface, edge to edge ---------------------
    // The primary grew to the whole desk, so its surface is reconfigured
    // and redrawn. The stale surface was configured to the same size and
    // painted magenta; no pixel of it may show, over or beside the navy.
    let shot = poll_until(Duration::from_secs(15), "the primary's lock surface to cover the whole output", || {
        session.door().barrier().ok()?;
        let shot = session.screenshot("stale-output-primary").ok()?;
        let navy = (5..shot.height).step_by(61).all(|y| {
            (5..shot.width).step_by(67).all(|x| shot.pixel(x, y)[..3] == LOCK_NAVY_RGB)
        });
        navy.then_some(shot)
    })
    .unwrap_or_else(|timeout| panic!("{timeout}\n-- chonk-lock-probe log --\n{}", probe_log(&session)));
    assert_ne!(shot.pixel(shot.width - 1, shot.height - 1)[..3], STALE_MAGENTA_RGB);

    // -- hit-tested: the pointer reaches the primary's surface --------------
    // Both over what was the right-hand output, where the stale surface
    // was requested, and over the left.
    for (x, y) in [(shot.width * 3 / 4, shot.height / 2), (shot.width / 4, shot.height / 3)] {
        assert_eq!(session.door().hit(x as i32, y as i32).unwrap(), "lock");
        session.door().motion(f64::from(x), f64::from(y)).unwrap();
        session.door().barrier().unwrap();
        poll_until(Duration::from_secs(15), "the pointer to land on the primary's lock surface", || {
            (last_report(&session, "pointer on ").as_deref() == Some("pointer on the chonkstep lock surface"))
                .then_some(())
        })
        .unwrap_or_else(|timeout| panic!("{timeout} at ({x}, {y})\n-- chonk-lock-probe log --\n{}", probe_log(&session)));
    }

    // -- and the keyboard stays on the surface the user can see -------------
    session.door().tap_key(keys::SPACE).unwrap();
    checkpoint(&session, "key on the chonkstep lock surface");

    let log = probe_log(&session);
    assert!(!log.contains("on the stale lock surface"), "the stale lock surface received input:\n{log}");
    assert!(!log.contains("connection broke"), "the locker's connection was broken:\n{log}");
    assert!(!session.log().contains("session unlocked"), "the session must stay locked throughout");
    assert!(session.compositor_alive(), "the compositor must outlive the unplug");
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

    // Input and rendering must describe the same security domain. The
    // production hit-test is also what tablet motion now uses, so an
    // answer of `lock` here proves a point visibly covered by the lock
    // can resolve neither a normal window nor a layer/shell surface
    // behind it.
    assert_eq!(
        session.door().hit(640, 400).expect("the locked hit-test answers"),
        "lock"
    );

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

/// One request to the session's Hyprland socket, in `hyprctl`'s shape.
fn hypr_request(session: &Session, command: &str) -> String {
    use std::io::{Read, Write};
    let signature = poll_until(Duration::from_secs(10), "the Hyprland instance signature", || session.hyprland_signature())
        .expect("the IPC server reports its instance signature");
    let path = std::path::PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    let mut stream = std::os::unix::net::UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    stream.write_all(command.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    response
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_locked_session_stays_locked_while_an_output_is_disabled_and_enabled() {
    // A laptop lid closing under a locked session: the panel leaves the
    // layout, its lock surface stays its client's and names no output,
    // the rest of the desk stays covered, and when the panel comes back
    // it shows the locked scene before any client content.
    let mut session =
        Session::boot("session-lock-park", SessionOptions { scale: Some(1.0), ..Default::default() }).unwrap();
    session.door().set_virtual_outputs("split").unwrap();
    let marker = launch_stale_output_locker(&mut session, "chonkstep-right", "chonkstep, chonkstep-right");
    assert_eq!(session.door().protocol_ledgers().unwrap().lock, 2, "one lock surface per output");

    // -- the right-hand output is disabled under the lock -------------------
    assert_eq!(hypr_request(&session, "keyword monitor chonkstep-right,disable").trim(), "ok");
    session.door().barrier().unwrap();
    assert_eq!(session.door().protocol_ledgers().unwrap().lock, 2, "the parked output's lock surface stays its client's");
    assert!(!session.log().contains("session unlocked"), "disabling an output must not unlock the session");

    // A lock surface that names the parked output's withdrawn wl_output
    // is filed under no other output.
    std::fs::write(&marker, "").unwrap();
    checkpoint(&session, "stale lock surface configured");
    assert_eq!(session.door().protocol_ledgers().unwrap().lock, 3);
    let shot = poll_until(Duration::from_secs(15), "the primary's lock surface to cover the whole desk", || {
        session.door().barrier().ok()?;
        let shot = session.screenshot("park-locked-primary").ok()?;
        let navy = (5..shot.height).step_by(61).all(|y| {
            (5..shot.width).step_by(67).all(|x| shot.pixel(x, y)[..3] == LOCK_NAVY_RGB)
        });
        navy.then_some(shot)
    })
    .unwrap_or_else(|timeout| panic!("{timeout}\n-- chonk-lock-probe log --\n{}", probe_log(&session)));
    assert_ne!(shot.pixel(shot.width - 1, shot.height - 1)[..3], STALE_MAGENTA_RGB);
    assert_eq!(session.door().hit(shot.width as i32 * 3 / 4, shot.height as i32 / 2).unwrap(), "lock");

    // -- and re-enabled: the locked scene first, never the desktop ---------
    assert_eq!(hypr_request(&session, "keyword monitor chonkstep-right,preferred,auto,auto").trim(), "ok");
    session.door().barrier().unwrap();
    let returned = session
        .screenshot_output("park-locked-returned", "chonkstep-right")
        .expect("grim captures the returned output");
    for y in (5..returned.height).step_by(97) {
        for x in (5..returned.width).step_by(89) {
            assert_eq!(
                returned.pixel(x, y)[..3],
                [0, 0, 0],
                "the returned output should show the uncovered locked scene at ({x}, {y}) in {}",
                returned.path.display()
            );
        }
    }
    assert_eq!(session.door().hit(shot.width as i32 * 3 / 4, shot.height as i32 / 2).unwrap(), "lock");
    assert!(!session.log().contains("session unlocked"), "the session must still be locked after the output returned");
    assert!(session.compositor_alive(), "the compositor must outlive the disable and enable");
    session.kill_client("chonk-lock-probe");
}
