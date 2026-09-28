//! A display that disappears for a few seconds must give freeform windows
//! back their monitor, position and size when it returns. Spaces mode
//! already did this; Desktop mode is the regression.
//!
//! Run in an isolated Weston host, never against the interactive desktop:
//! `scripts/e2e.sh --headless --host-renderer pixman --test desktop_hotplug_restore`
use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions, WindowInfo};
use serde_json::Value;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::{Duration, Instant},
};

const WAIT: Duration = Duration::from_secs(12);

fn request(session: &Session, command: &str) -> String {
    let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("hypr")
        .join(session.hyprland_signature().unwrap())
        .join(".socket.sock");
    let mut stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(command.as_bytes()).unwrap();
    let mut response = String::new();
    stream.take(1024 * 1024).read_to_string(&mut response).unwrap();
    response
}

fn json(session: &Session, command: &str) -> Value {
    serde_json::from_str(&request(session, &format!("j/{command}"))).unwrap()
}

fn dispatch(session: &mut Session, command: &str) {
    assert_eq!(request(session, &format!("/dispatch {command}")).trim(), "ok");
    session.door().barrier().unwrap();
}

fn geometry(window: &WindowInfo) -> (i32, i32, u32, u32) {
    (window.x, window.y, window.w, window.h)
}

fn window(session: &mut Session, title: &str) -> WindowInfo {
    session.world().unwrap().windows.iter().find(|window| window.title == title).unwrap().clone()
}

fn probe(session: &mut Session, title: &str, x: i32, y: i32, w: u32, h: u32) -> WindowInfo {
    session.door().motion(x as f64, y as f64).unwrap();
    session.door().barrier().unwrap();
    let path = profile_binary("chonk-fullscreen-probe").unwrap();
    session.launch(path.to_str().unwrap(), &[title, title]).unwrap();
    session.wait_for_window(title).unwrap();
    dispatch(session, &format!("resizeactive exact {w} {h}"));
    dispatch(session, &format!("moveactive exact {x} {y}"));
    poll_until(WAIT, "probe geometry acknowledged", || {
        let result = window(session, title);
        (geometry(&result) == (x, y, w, h)).then_some(result)
    })
    .unwrap()
}

fn reconnect_restores(mode: &str) {
    let mut session = Session::boot(
        &format!("{mode}-six-second-reconnect"),
        SessionOptions {
            config_extra: format!(
                "interaction_mode = '{mode}'\nkeyboard_mode = 'desktop'\nshow_dock = false\nhyprland_config = false\n"
            ),
            ..Default::default()
        },
    )
    .unwrap();
    session.door().virtual_outputs(true).unwrap();
    let original_outputs = json(&session, "monitors");
    assert_eq!(original_outputs.as_array().unwrap().len(), 2);
    let left = probe(&mut session, "Restore Left", 40, 100, 180, 100);
    let right_x = original_outputs[1]["x"].as_i64().unwrap() as i32 + 100;
    let right = probe(&mut session, "Restore Right", right_x, 100, 500, 450);
    eprintln!("{mode}: before left={:?} right={:?}", geometry(&left), geometry(&right));

    // The compact survivor forces evacuation without touching real DRM outputs.
    session.door().set_virtual_outputs("compact").unwrap();
    assert_eq!(json(&session, "monitors").as_array().unwrap().len(), 1);
    let removed_at = Instant::now();
    poll_until(WAIT, "six seconds with the external display absent", || {
        (removed_at.elapsed() >= Duration::from_secs(6)).then_some(())
    })
    .unwrap();
    let borrowed = window(&mut session, "Restore Right");
    eprintln!("{mode}: absent for {:?}, right={:?}", removed_at.elapsed(), geometry(&borrowed));
    assert_ne!(geometry(&borrowed), geometry(&right), "fixture must evacuate the right window");
    session.door().virtual_outputs(true).unwrap();
    let returned_outputs = json(&session, "monitors");
    assert_eq!(returned_outputs.as_array().unwrap().len(), 2);
    for index in 0..2 {
        for field in ["name", "x", "y", "width", "height", "scale"] {
            assert_eq!(returned_outputs[index][field], original_outputs[index][field], "{field}");
        }
    }
    let result = poll_until(WAIT, "returning display restores original window rectangles", || {
        let stayed = window(&mut session, "Restore Left");
        let restored = window(&mut session, "Restore Right");
        (geometry(&stayed) == geometry(&left) && geometry(&restored) == geometry(&right)).then_some(())
    });
    eprintln!(
        "{mode}: reconnected left={:?} right={:?}",
        geometry(&window(&mut session, "Restore Left")),
        geometry(&window(&mut session, "Restore Right"))
    );
    result.unwrap();
}

#[test]
#[ignore = "needs isolated Weston: scripts/e2e.sh --headless --test desktop_hotplug_restore"]
fn desktop_reconnect_after_six_seconds_restores_window_rectangles() {
    reconnect_restores("desktop");
}

#[test]
#[ignore = "needs isolated Weston: scripts/e2e.sh --headless --test desktop_hotplug_restore"]
fn spaces_reconnect_after_six_seconds_restores_window_rectangles() {
    reconnect_restores("spaces");
}
