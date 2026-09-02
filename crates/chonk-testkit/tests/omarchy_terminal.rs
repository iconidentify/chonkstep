//! Omarchy's terminal wears this desktop's frame, end to end.
//!
//! Omarchy configures alacritty `decorations = "None"` — the right
//! setting under Hyprland, which draws no titlebars — and launches it
//! as `org.omarchy.terminal` for `omarchy-update` and the installers.
//! Under this desktop that terminal asks for client-side chrome it
//! will never draw, and the compositor answers "server-side" — the
//! xdg-decoration protocol gives it the last word (see `wm-wayland`'s
//! `decoration` module), so no list has to name the class. The unit
//! tests pin the policy; this pins that the answer reaches a real
//! alacritty on a real socket, as a frame around it — and that
//! `[decorations] client_side` really does take the frame back off,
//! so a bare window is still a thing a user can ask for.
//!
//! The real alacritty is the client here (Omarchy's terminal *is*
//! alacritty), told on its command line to draw no decorations so the
//! test does not depend on whatever `~/.config/alacritty` says on
//! this machine.
//!
//! `#[ignore]`d like the rest of the suite (needs a Wayland session to
//! nest in): `cargo test -p chonk-testkit --test omarchy_terminal -- --ignored`.

use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions, WindowInfo};

const APP_ID: &str = "org.omarchy.terminal";

/// Launches alacritty the way `omarchy-launch-floating-terminal-with-
/// presentation` does, minus the presentation: Omarchy's class, and
/// no decorations of its own.
fn launch_omarchy_terminal(session: &mut Session) -> WindowInfo {
    session
        .launch("alacritty", &["--class", APP_ID, "--title", "Omarchy", "-o", "window.decorations=None"])
        .expect("alacritty launches");
    session.wait_for_window(APP_ID).expect("Omarchy's terminal maps")
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn omarchy_terminal_gets_this_desktops_frame_by_default() {
    let mut session = Session::boot("omarchy-terminal", SessionOptions { scale: Some(1.0), ..Default::default() }).unwrap();
    let window = launch_omarchy_terminal(&mut session);

    let (frame, window) = poll_until(Duration::from_secs(10), "a frame around Omarchy's terminal", || {
        let world = session.world().ok()?;
        let window = world.windows.iter().find(|w| w.id == window.id)?.clone();
        world.frame_of(window.id).cloned().map(|frame| (frame, window))
    })
    .expect("a terminal that asked for client-side chrome and draws none must be framed anyway");

    assert!(frame.y < window.y, "the frame carries a titlebar above the terminal's own pixels");
    assert!(frame.x <= window.x && frame.x + frame.w as i32 >= window.x + window.w as i32, "and wraps it");
    assert!(frame.x >= 0 && frame.y >= 0, "and every bit of that chrome is on the screen where it can be grabbed");
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn listing_it_under_client_side_lets_omarchy_terminal_stay_bare() {
    let options = SessionOptions {
        scale: Some(1.0),
        config_extra: format!("[decorations]\nclient_side = [\"{APP_ID}\"]\n"),
        ..Default::default()
    };
    let mut session = Session::boot("omarchy-terminal-bare", options).unwrap();
    let window = launch_omarchy_terminal(&mut session);

    // Asserting an *absence* needs a settled signal, not a timer: a
    // sleep only ever proves "no frame within N ms on this machine",
    // which on a slow one is no proof at all. The ledger already lists
    // the window as mapped (`wait_for_window`), and the shell decides
    // on a frame in the dispatch pass that maps it; two barrier
    // round-trips after that have certainly run that pass and rendered
    // its outcome, so "no frame in the ledger now" means "no frame".
    session.door().barrier().unwrap();
    session.door().barrier().unwrap();
    let world = session.world().unwrap();
    assert!(world.window_matching(APP_ID).is_some_and(|w| w.mapped), "the terminal is up");
    assert!(world.frame_of(window.id).is_none(), "listed under client_side, a client that asked to decorate itself is left alone");
}
