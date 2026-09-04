//! The living desktop, end to end: session-layout persistence and
//! restore against a real nested compositor. Same running story as
//! `e2e.rs` — these need a live Wayland session to nest inside, so
//! they are `#[ignore]`d; run them with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.
//! `foot` must be installed: it is the terminal the desktop itself
//! spawns, and the client these tests record and restore.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use chonk_testkit::{poll_until, profile_binary, session_dir, Session, SessionOptions};

/// Layout writes are debounced (2s of stillness — see
/// `chonk_shell::session_layout::DEBOUNCE`), so waits that span one
/// get headroom beyond the default.
const PERSIST: Duration = Duration::from_secs(10);

/// One serialized layout record for a foot terminal, in the store's
/// tab-separated wire format (`session_layout.rs` documents it).
fn foot_record(x: i32, y: i32, w: u32, h: u32) -> String {
    format!("foot\t-\t{x}\t{y}\t{w}\t{h}\t0\t-\n")
}

/// The recorded geometry of the first foot record in the layout file,
/// if the file exists and holds one.
fn recorded_foot(session: &Session) -> Option<(i32, i32, u32, u32)> {
    let text = std::fs::read_to_string(session.state_file("session")).ok()?;
    let line = text.lines().find(|line| line.starts_with("foot\t"))?;
    let fields: Vec<&str> = line.split('\t').collect();
    Some((
        fields.get(2)?.parse().ok()?,
        fields.get(3)?.parse().ok()?,
        fields.get(4)?.parse().ok()?,
        fields.get(5)?.parse().ok()?,
    ))
}

/// Pillar 1, the recording half: arrange a window, and the arrangement
/// is on disk — debounced, atomically — *before* anything goes wrong,
/// so that a SIGKILL (the harshest crash there is: no flushes, no
/// destructors) costs nothing that was settled.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_settled_layout_survives_sigkill() {
    let mut session = Session::boot("layout-survives-kill", SessionOptions::default()).unwrap();
    session.launch("foot", &[]).expect("foot should launch");
    let window = session.wait_for_window("foot").expect("the terminal should map");

    // Drag it by its server-drawn titlebar to somewhere deliberate.
    let world = session.world().unwrap();
    let frame = world.frame_of(window.id).expect("foot is server-decorated here").clone();
    let grip = (frame.x as f64 + frame.w as f64 / 2.0, frame.y as f64 + 8.0);
    session.door().drag_to(grip, (grip.0 + 150.0, grip.1 + 120.0)).unwrap();

    // The move lands in the ledger first...
    let moved = poll_until(PERSIST, "the terminal to arrive at its dragged position", || {
        let world = session.world().ok()?;
        let now = world.window_matching("foot")?;
        (now.x != window.x || now.y != window.y).then_some((now.x, now.y, now.w, now.h))
    })
    .unwrap();

    // ...and, once the debounce elapses, in the layout file — the
    // moved position exactly, not the launch position.
    poll_until(PERSIST, "the layout file to hold the settled geometry", || {
        (recorded_foot(&session) == Some(moved)).then_some(())
    })
    .unwrap();

    // SIGKILL: no destructor, no flush, nothing. What was on disk
    // stays on disk — which is the entire crash-recovery contract.
    session.kill_compositor();
    assert_eq!(recorded_foot(&session), Some(moved), "the layout on disk must be exactly what had settled before the kill");
}

/// Pillar 1, the restore half: a fresh session with `restore_session =
/// true` and a seeded layout file relaunches the recorded application
/// and puts its window back at the recorded content geometry.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_recorded_layout_is_relaunched_and_replaced_at_startup() {
    let recorded = (320, 240, 500, 380);
    // Use the repository's deterministic xdg client as the configured
    // terminal and have it identify as `foot`, the class that maps a
    // saved terminal record to `RelaunchPlan::Terminal`. Ubuntu's Foot
    // package has intermittently retained its command-line startup
    // width after the restore configure (the position still restored),
    // which made this a test of an external release's cell negotiation
    // rather than of Chonkstep's restore pipeline.
    let probe = profile_binary("chonk-fullscreen-probe").expect("the restore probe is built");
    let config = format!(
        "restore_session = true\nterminal = [\"{}\", \"restored-foot\", \"foot\"]\n",
        probe.display()
    );
    let mut session = Session::boot(
        "layout-restores",
        SessionOptions {
            config_extra: config,
            state_files: vec![("session".to_string(), foot_record(recorded.0, recorded.1, recorded.2, recorded.3))],
            ..SessionOptions::default()
        },
    )
    .unwrap();

    // Nothing was launched from this test: the client in the ledger is
    // the shell restoring its terminal record. The in-tree probe obeys
    // compositor sizes exactly, so both position and size can assert
    // the saved rectangle without client-specific tolerance.
    let _ = session.wait_for_window("restored-foot").expect("restore should relaunch the recorded terminal");
    poll_until(PERSIST, "the restored terminal to take its recorded geometry", || {
        let world = session.world().ok()?;
        let now = world.window_matching("restored-foot")?;
        ((now.x, now.y, now.w, now.h) == recorded).then_some(())
    })
    .unwrap_or_else(|e| {
        let world = session.world().ok();
        panic!("{e}; ledger was {world:?}");
    });
}

/// A window closed by the user is forgotten: restore must never
/// resurrect what was deliberately dismissed, so the record's removal
/// on close is as load-bearing as its creation.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_closed_window_is_forgotten_by_the_layout() {
    let mut session = Session::boot("layout-forgets", SessionOptions::default()).unwrap();
    session.launch("foot", &[]).expect("foot should launch");
    let _ = session.wait_for_window("foot").expect("the terminal should map");

    // Recorded once mapped and settled...
    poll_until(PERSIST, "the layout file to record the terminal", || {
        recorded_foot(&session).map(|_| ())
    })
    .unwrap();

    // ...then closed (foot exits when its shell does — kill the
    // client), and forgotten once the change settles.
    session.kill_clients();
    session.wait_for_window_gone("foot").expect("the terminal should close");
    poll_until(PERSIST, "the layout file to forget the closed terminal", || {
        recorded_foot(&session).is_none().then_some(())
    })
    .unwrap();
}

/// Pillar 2's compositor half, isolated from the supervisor (which has
/// its own non-ignored script tests in `supervisor.rs`): a session
/// that boots with the supervisor's recovery marker starts inside the
/// lock domain, launches the configured locker, and lets that client
/// take over before any ordinary scene can receive input.
#[cfg(unix)]
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_recovery_marker_locks_before_the_first_ordinary_frame() {
    let probe = profile_binary("chonk-lock-probe").expect("the recovery lock probe is built");
    // Stand in for the login shell rather than starting the developer's
    // real Omarchy UI. The production branch still hands this process
    // the exact retry script, while the stand-in records that argv and
    // turns the successful retry into a deterministic ext-session-lock
    // client on the nested compositor.
    let launcher_dir = std::env::temp_dir().join(format!(
        "chonk-testkit-recovery-launcher-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&launcher_dir);
    std::fs::create_dir_all(&launcher_dir).unwrap();
    let bash = launcher_dir.join("bash");
    let args_file = launcher_dir.join("args");
    std::fs::write(
        &bash,
        "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$RECOVERY_LAUNCH_ARGS\"\nexec \"$RECOVERY_LOCKER\" --recovery-hold\n",
    )
    .unwrap();
    std::fs::set_permissions(&bash, std::fs::Permissions::from_mode(0o755))
        .unwrap();
    let mut session = Session::boot(
        "recovery-marker",
        SessionOptions {
            config_extra: "desktop = \"omarchy\"\n".to_string(),
            state_files: vec![("recovery".to_string(), String::new())],
            env: vec![
                (
                    "CHONKSTEP_TEST_RECOVERY_SHELL".to_string(),
                    bash.display().to_string(),
                ),
                (
                    "RECOVERY_LAUNCH_ARGS".to_string(),
                    args_file.display().to_string(),
                ),
                ("RECOVERY_LOCKER".to_string(), probe.display().to_string()),
            ],
            ..SessionOptions::default()
        },
    )
    .unwrap();

    poll_until(Duration::from_secs(5), "the recovery to be logged", || {
        let log = session.log();
        (log.contains("RECOVERED FROM A CRASH") && log.contains("locked frame presented"))
            .then_some(())
    })
    .unwrap();
    assert_eq!(
        session
            .door()
            .hit(640, 400)
            .expect("the recovery hit-test answers"),
        "lock",
        "input must resolve inside the lock domain before any ordinary scene"
    );
    assert!(
        !session.state_file("recovery").exists(),
        "the marker must be consumed — recovery is acknowledged exactly once"
    );
    let launch_args = std::fs::read_to_string(&args_file)
        .expect("the login-shell stand-in recorded its argv");
    assert!(
        launch_args.starts_with("-lc "),
        "the Omarchy fallback must use a login shell: {launch_args:?}"
    );
    assert!(
        launch_args.contains("omarchy-shell lock lock")
            && launch_args.contains("exec omarchy-system-lock"),
        "the asynchronous launcher must wait on Omarchy's lock IPC, then run its system entry point: {launch_args:?}"
    );
    drop(session);
    let _ = std::fs::remove_dir_all(launcher_dir);
}

/// No locker is a startup failure, not permission to resurrect the
/// desktop unlocked. The real supervisor observes this nonzero exit,
/// exhausts its bounded retry policy, and returns control to the login
/// boundary where the failure is visible and the user's session is not.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn recovery_without_a_locker_refuses_to_expose_the_desktop() {
    let name = "recovery-without-locker";
    let error = match Session::boot(
        name,
        SessionOptions {
            state_files: vec![("recovery".to_string(), String::new())],
            ..SessionOptions::default()
        },
    ) {
        Ok(_) => panic!("a recovered session without a locker must not boot"),
        Err(error) => error,
    };
    assert!(
        error.contains("compositor exited during boot"),
        "failure was not a nonzero compositor exit:\n{error}"
    );
    assert!(
        error.contains("refusing to expose an unlocked desktop"),
        "the visible startup failure must name its security reason:\n{error}"
    );
    assert!(
        !session_dir(name).join("state/chonkstep/recovery").exists(),
        "the marker must be consumed even when fail-closed recovery cannot continue"
    );
}
