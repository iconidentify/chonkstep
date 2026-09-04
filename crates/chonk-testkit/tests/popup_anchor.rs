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

use chonk_testkit::{poll_until, Session, SessionOptions, WindowInfo};

const EVENT: Duration = Duration::from_secs(15);
const BROWSER_STARTUP: Duration = Duration::from_secs(90);

fn wait_for_chromium(session: &mut Session) -> WindowInfo {
    let browser = poll_until(BROWSER_STARTUP, "Chromium to map its first window", || {
        if let Ok(world) = session.world() {
            if let Some(window) = world.window_matching("hromium") {
                return Some(Ok(window.clone()));
            }
        }
        match session.client_status("chromium") {
            Ok(Some(status)) => Some(Err(format!("Chromium exited before mapping: {status}"))),
            Ok(None) => None,
            Err(error) => Some(Err(error)),
        }
    })
    .and_then(|result| result);
    browser.unwrap_or_else(|error| {
        let client_log = std::fs::read_to_string(session.dir.join("client-0-chromium.log"))
            .unwrap_or_else(|read_error| format!("<could not read Chromium log: {read_error}>"));
        panic!(
            "{error}\n--- Chromium log ---\n{client_log}\n--- compositor log ---\n{}",
            session.log()
        );
    })
}

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

fn popup_configure(log: &str, popup_id: &str) -> Option<(i32, i32, u32, u32)> {
    let marker = format!("xdg_popup@{popup_id}.configure(");
    let line = log.lines().rev().find(|line| line.contains(&marker))?;
    let values = line
        .split(&marker)
        .nth(1)?
        .strip_suffix(')')?
        .split(',')
        .map(|value| value.trim().parse::<i32>().ok())
        .collect::<Option<Vec<_>>>()?;
    let [x, y, w, h] = values.as_slice() else { return None };
    Some((*x, *y, u32::try_from(*w).ok()?, u32::try_from(*h).ok()?))
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
    std::fs::create_dir_all(&profile).unwrap();
    let data = format!("--user-data-dir={}", profile.display());
    let mut chromium_args = vec![
        "--ozone-platform=wayland",
        data.as_str(),
        "--no-first-run",
        "--no-default-browser-check",
        "--window-size=900,600",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-default-apps",
        "--disable-extensions",
        "--disable-sync",
        "--metrics-recording-only",
        "--password-store=basic",
        "about:blank",
    ];
    if std::env::var_os("CI").is_some() {
        // Hosted runners deny Chromium's user namespace and expose no
        // DRM device. Match the established real-browser scale test:
        // an isolated about:blank profile plus SwANGLE keeps the actual
        // browser compositor path while making startup deterministic.
        chromium_args.push("--no-sandbox");
        chromium_args.extend(["--use-gl=angle", "--use-angle=swiftshader"]);
    }
    session
        .launch("chromium", &chromium_args)
        .unwrap();
    let window = wait_for_chromium(&mut session);
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
    let (popup_x, popup_y, popup_w, popup_h) = poll_until(
        EVENT,
        "Chromium's popup configure",
        || popup_configure(&session.log(), &popup_id),
    )
    .unwrap();
    let inside_x = window.x - window.offset_x + popup_x + i32::try_from(popup_w.min(20)).unwrap();
    let inside_y = window.y - window.offset_y + popup_y + i32::try_from(popup_h.min(20)).unwrap();
    poll_until(EVENT, "the mapped popup to join the compositor hit-test", || {
        (session.door().hit(inside_x, inside_y).ok()?.as_str() == "popup").then_some(())
    })
    .expect("the popup was tracked by the client but never entered the rendered input scene");

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
    poll_until(EVENT, "the dismissed popup to leave the compositor hit-test", || {
        (session.door().hit(inside_x, inside_y).ok()?.as_str() != "popup").then_some(())
    })
    .expect("the dismissed popup remained in the rendered input scene");
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
