//! Browser popup anchoring across a compositor-driven parent resize.
//!
//! Chromium's application menu uses a non-reactive `xdg_positioner`.
//! It neither sends `xdg_popup.reposition` nor creates a replacement
//! popup after its toplevel accepts a resize, leaving the compositor
//! with no protocol-level description of the toolbar button's new
//! location. The only correct fallback is `xdg_popup.popup_done`; this
//! test records both halves of that contract from the real wire.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use chonk_testkit::{poll_until, Session, SessionOptions};

const EVENT: Duration = Duration::from_secs(15);

fn hyprland_request(session: &Session, payload: &str) -> String {
    let signature = poll_until(EVENT, "the Hyprland compatibility socket", || {
        session.hyprland_signature()
    })
    .expect("compositor announces its compatibility socket");
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR");
    let socket = PathBuf::from(runtime)
        .join("hypr")
        .join(signature)
        .join(".socket.sock");
    let mut stream = UnixStream::connect(socket).expect("connect to the Hyprland request socket");
    stream
        .set_read_timeout(Some(EVENT))
        .expect("set request timeout");
    stream.write_all(payload.as_bytes()).expect("write request");
    stream.flush().expect("flush request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("read request response");
    response
}

fn newest_popup_id(log: &str) -> Option<&str> {
    let request = log
        .lines()
        .rev()
        .find(|line| line.contains(".get_popup, ("))?;
    request.split(".get_popup, (").nth(1)?.split(',').next()
}

#[test]
#[ignore = "needs a Wayland session and Chromium"]
fn chromium_popup_without_a_new_anchor_is_dismissed_on_parent_resize() {
    let chromium_on_path = std::env::var_os("PATH")
        .is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join("chromium").is_file()));
    if !chromium_on_path {
        eprintln!("chromium not installed; skipping");
        return;
    }
    let mut session = Session::boot(
        "chromium-popup-anchor",
        SessionOptions {
            env: vec![
                ("WAYLAND_DEBUG".to_string(), "server".to_string()),
                ("CHONKSTEP_HYPRLAND_IPC".to_string(), "1".to_string()),
            ],
            config_extra: "show_dock = false\n".to_string(),
            ..SessionOptions::default()
        },
    )
    .unwrap();
    let profile = session.dir.join("chromium-data");
    let data = format!("--user-data-dir={}", profile.display());
    session
        .launch(
            "chromium",
            &[
                "--ozone-platform=wayland",
                data.as_str(),
                "--no-first-run",
                "--no-default-browser-check",
                "--window-size=900,600",
                "about:blank",
            ],
        )
        .unwrap();
    let window = session.wait_for_window("hromium").unwrap();
    session.door().barrier().unwrap();
    session
        .door()
        .click(
            (window.x + window.w as i32 - 24) as f64,
            (window.y + 66) as f64,
        )
        .unwrap();
    session.door().barrier().unwrap();
    let popup_id = poll_until(
        EVENT,
        "Chromium to create its application-menu popup",
        || {
            let log = session.log();
            newest_popup_id(&log).map(str::to_owned)
        },
    )
    .unwrap();

    let before_resize = session.log();
    let popup_trace = before_resize
        .lines()
        .skip_while(|line| !line.contains(".create_positioner"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !popup_trace.contains(".set_reactive"),
        "Chromium unexpectedly made this positioner reactive"
    );
    assert!(
        !popup_trace.contains(".reposition"),
        "Chromium unexpectedly supplied an updated anchor"
    );

    assert_eq!(
        hyprland_request(&session, "/dispatch resizeactive 120 80"),
        "ok"
    );
    poll_until(EVENT, "the parent resize to apply", || {
        let resized = session.world().ok()?.window_matching("hromium")?.clone();
        (resized.w != window.w || resized.h != window.h).then_some(())
    })
    .unwrap();
    poll_until(
        EVENT,
        "the compositor to dismiss the now-unanchored popup",
        || {
            let event = format!("xdg_popup@{popup_id}.popup_done");
            session.log().contains(&event).then_some(())
        },
    )
    .unwrap();
    let completed_trace = session
        .log()
        .lines()
        .skip_while(|line| !line.contains(".create_positioner"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !completed_trace.contains(".reposition"),
        "Chromium supplied a replacement anchor, so this run did not exercise the fallback"
    );
}
