//! Omarchy's terminal wears this desktop's frame, end to end.
//!
//! Omarchy configures alacritty `decorations = "None"` — the right
//! setting under Hyprland, which draws no titlebars — and launches it
//! as `org.omarchy.terminal` for `omarchy-update` and the installers.
//! Under this desktop that terminal negotiates client-side chrome and
//! draws none, which is why `[decorations] server_side` ships with it
//! named (`wm_config::DEFAULT_SERVER_SIDE`). The unit tests pin the
//! rule; this pins that the rule reaches a real alacritty on a real
//! socket, as a frame around it — and that `server_side = []` really
//! does take the frame back off, so the default is a default and not
//! a hard-coded exception.
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
fn clearing_server_side_lets_omarchy_terminal_stay_bare() {
    let options = SessionOptions {
        scale: Some(1.0),
        config_extra: "[decorations]\nserver_side = []\n".into(),
        ..Default::default()
    };
    let mut session = Session::boot("omarchy-terminal-bare", options).unwrap();
    let window = launch_omarchy_terminal(&mut session);

    // Give the shell a moment it would have used to frame the window,
    // then assert it did not: the client asked for client-side chrome,
    // the user cleared the rescue list, and the desktop believes the
    // client again.
    std::thread::sleep(Duration::from_millis(500));
    let world = session.world().unwrap();
    assert!(world.window_matching(APP_ID).is_some_and(|w| w.mapped), "the terminal is up");
    assert!(world.frame_of(window.id).is_none(), "with server_side = [] a client that asked to decorate itself is left alone");
}
