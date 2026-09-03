//! The stuck-fullscreen desktop, spelled as assertions.
//!
//! # The incident
//!
//! 2026-09-03, live: the user fullscreened a video in Microsoft Edge
//! and the desktop stopped answering. Edge appeared to vanish and
//! right-clicking the desk did nothing — because Edge had not vanished
//! at all. It was still fullscreen, an invisible sheet over the whole
//! screen swallowing every click, and the session log said so: `entered
//! fullscreen` with no `left fullscreen` after it, where every earlier
//! fullscreen in the same log had its matching pair. The compositor's
//! own `alt+shift+f` recovered it. Alongside that, a symptom the user
//! had lived with for months: fullscreening a video in Edge took *two*
//! clicks on the page's fullscreen control.
//!
//! # What the compositor was doing
//!
//! `XdgShellHandler::fullscreen_request` queued the request for
//! `wm-core` and then, in the same breath, sent a configure — built
//! from the toplevel's state at that instant, which is to say from
//! before `wm-core` had seen the request. So a client that asked to
//! become fullscreen was told, synchronously, that it was not.
//! Chromium reads that as a refusal, drops the fullscreen session the
//! page had just opened, and from then on the browser and the desktop
//! disagree about what the window is. The second click is that
//! disagreement being papered over. The freeze is its endgame: the page
//! exits a fullscreen the browser no longer has a session for, sends no
//! `unset_fullscreen`, and the compositor stays fullscreen with nothing
//! left to tell it otherwise. See `WaylandBackend::flush_configures` in
//! `wm-wayland` for the captured trace and the fix.
//!
//! # What these tests drive
//!
//! `chonk-fullscreen-probe` — a real xdg-shell client with a browser's
//! memory of what it asked for (see its module doc). The `f` key is its
//! fullscreen control; pressing it twice is fullscreen-then-exit, the
//! exact gesture pair the incident was made of. On the old compositor
//! the second press does not exit, because the probe's session was
//! dropped by the refusal — so the desktop is left fullscreen and these
//! tests fail loudly, which is the point of them.

use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions, WindowInfo};

/// evdev `KEY_F` / `KEY_M`: the probe's two controls. Declared here as
/// the tests' own vocabulary, the way `dock_toggle.rs` declares its
/// `KEY_D` — `chonk_testkit::keys` carries only the keys the harness
/// itself needs.
const KEY_F: u32 = 33;
const KEY_M: u32 = 50;

/// Boots a session with the probe mapped and focused, and hands back
/// its windowed rect — the one an unfullscreen has to come back to.
fn probe_session(name: &str) -> (Session, WindowInfo) {
    let probe = profile_binary("chonk-fullscreen-probe")
        .expect("cargo build -p chonk-testkit builds the probe");
    let mut session = Session::boot(name, SessionOptions { scale: Some(1.0), ..SessionOptions::default() })
        .expect("the nested compositor boots");
    session
        .launch(&probe.to_string_lossy(), &[])
        .expect("the probe launches against the nested session");
    let window = session.wait_for_window("chonk-fullscreen-probe").expect("the probe maps a window");
    // Click the window's middle before driving it: the injected keys go
    // wherever the seat's keyboard focus is, and a click is how a user
    // would have put it there. (A freshly mapped window is focused
    // already; this makes the test say so rather than assume it.)
    let door = session.door();
    door.click(window.x as f64 + window.w as f64 / 2.0, window.y as f64 + window.h as f64 / 2.0)
        .expect("a click lands on the probe");
    door.barrier().expect("the click settles");
    (session, window)
}

/// What the probe wrote about what it was told.
fn probe_log(session: &Session) -> String {
    std::fs::read_to_string(session.dir.join("client-0-chonk-fullscreen-probe.log")).unwrap_or_default()
}

/// Presses one of the probe's controls and lets the compositor settle.
fn press(session: &mut Session, key: u32) {
    let door = session.door();
    door.tap_key(key).expect("the control key reaches the probe");
    door.barrier().expect("the press settles");
}

/// The compositor's own account, ANSI-stripped: how many times it
/// entered and left fullscreen. An `entered` with no `left` after it is
/// precisely the incident.
fn fullscreen_transitions(session: &Session) -> (usize, usize) {
    let log = session.log();
    (
        log.lines().filter(|line| line.contains("entered fullscreen")).count(),
        log.lines().filter(|line| line.contains("left fullscreen")).count(),
    )
}

/// The load-bearing regression. Two presses of one control — enter,
/// then exit — and the desktop must be back where it started.
///
/// On the unfixed compositor the first press is answered with a
/// configure saying "not fullscreen", the probe drops its session, and
/// the second press asks to *enter* again instead of leaving. Nothing
/// ever sends `unset_fullscreen`, so `left fullscreen` never happens
/// and the window stays welded over the whole output: the invisible
/// sheet, reproduced.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_clients_fullscreen_control_enters_and_leaves_in_one_press_each() {
    let (mut session, windowed) = probe_session("fullscreen-control");
    let world = session.world().unwrap();
    let (output_w, output_h) = (world.output_w, world.output_h);
    assert!(
        windowed.w < output_w && windowed.h < output_h,
        "the probe starts windowed, or there is nothing for fullscreen to change: {windowed:?}"
    );

    // -- one press in ---------------------------------------------------
    press(&mut session, KEY_F);
    let full = poll_until(Duration::from_secs(10), "the probe to cover the whole output", || {
        let world = session.world().ok()?;
        world
            .window_matching("chonk-fullscreen-probe")
            .filter(|w| (w.x, w.y, w.w, w.h) == (0, 0, output_w, output_h))
            .cloned()
    })
    .expect("one press of a fullscreen control fullscreens the window");
    // Fullscreen deliberately takes the raw monitor rect, dock strip
    // included — see `fullscreen_monitor_rect`.
    assert_eq!((full.x, full.y), (0, 0));

    // The client's half, and the invariant the fix states: the answer
    // to a request describes the decision on that request. A refusal
    // here is the bug even when the geometry above looks right.
    let log = probe_log(&session);
    assert!(
        !log.contains("answer REFUSED"),
        "a granted fullscreen must not be answered with a configure saying otherwise:\n{log}"
    );
    assert!(
        log.contains("answer granted: asked fullscreen=true, told fullscreen=true"),
        "the configure answering set_fullscreen must carry the fullscreen state:\n{log}"
    );

    // -- one press out --------------------------------------------------
    press(&mut session, KEY_F);
    let back = poll_until(Duration::from_secs(10), "the probe to return to its windowed rect", || {
        let world = session.world().ok()?;
        world
            .window_matching("chonk-fullscreen-probe")
            .filter(|w| (w.w, w.h) == (windowed.w, windowed.h))
            .cloned()
    })
    .expect("a second press of the same control leaves fullscreen");
    assert_eq!(
        (back.x, back.y, back.w, back.h),
        (windowed.x, windowed.y, windowed.w, windowed.h),
        "unfullscreen restores the exact rect the window had before"
    );

    let log = probe_log(&session);
    assert!(
        log.contains("control exit fullscreen: sent unset"),
        "the second press must be an exit, not another enter — a client whose \
         request was refused has no session left to exit, which is how the \
         desktop got stuck:\n{log}"
    );
    assert!(
        log.contains("answer granted: asked fullscreen=false, told fullscreen=false"),
        "the configure answering unset_fullscreen must carry no fullscreen state:\n{log}"
    );
    assert!(!log.contains("answer REFUSED"), "no request in this test was refused:\n{log}");

    // The compositor's own ledger of the incident: every `entered` has
    // its `left`.
    let (entered, left) = fullscreen_transitions(&session);
    assert_eq!(
        (entered, left),
        (1, 1),
        "an `entered fullscreen` with no `left fullscreen` is the stuck desktop itself"
    );
}

/// The same request/answer pair one interface up: `set_maximized` had
/// byte-for-byte the same defect (queue the request, answer with the
/// state that preceded it), and shares the fix. A maximized window
/// stops at the Dock's reserved strip rather than covering it, so this
/// asserts on the answer and on the window having grown, not on an
/// exact rect.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_clients_maximize_control_is_answered_with_maximized() {
    let (mut session, windowed) = probe_session("maximize-control");

    press(&mut session, KEY_M);
    let big = poll_until(Duration::from_secs(10), "the probe to grow into a maximized rect", || {
        let world = session.world().ok()?;
        world.window_matching("chonk-fullscreen-probe").filter(|w| w.w > windowed.w).cloned()
    })
    .expect("one press of a maximize control maximizes the window");
    assert!(big.h > windowed.h, "maximize grows both axes");
    let log = probe_log(&session);
    assert!(
        log.contains("answer granted: asked maximized=true, told maximized=true"),
        "the configure answering set_maximized must carry the maximized state:\n{log}"
    );

    press(&mut session, KEY_M);
    let back = poll_until(Duration::from_secs(10), "the probe to return to its windowed rect", || {
        let world = session.world().ok()?;
        world
            .window_matching("chonk-fullscreen-probe")
            .filter(|w| (w.w, w.h) == (windowed.w, windowed.h))
            .cloned()
    })
    .expect("a second press of the same control unmaximizes");
    assert_eq!((back.w, back.h), (windowed.w, windowed.h));
    let log = probe_log(&session);
    assert!(
        log.contains("answer granted: asked maximized=false, told maximized=false"),
        "the configure answering unset_maximized must carry no maximized state:\n{log}"
    );
    assert!(!log.contains("answer REFUSED"), "no request in this test was refused:\n{log}");
}
