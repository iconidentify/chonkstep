//! Two real wl_output heads in one nested host framebuffer. Input, clients,
//! screencopy, workspace IPC and hotplug all use production compositor paths.
use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions, WindowInfo};
use serde_json::Value;
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};
const WAIT: Duration = Duration::from_secs(12);
fn request(s: &Session, command: &str) -> String {
    let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("hypr")
        .join(s.hyprland_signature().unwrap())
        .join(".socket.sock");
    let mut stream = UnixStream::connect(path).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(command.as_bytes()).unwrap();
    let mut response = String::new();
    stream
        .take(1024 * 1024)
        .read_to_string(&mut response)
        .unwrap();
    response
}
fn json(s: &Session, name: &str) -> Value {
    serde_json::from_str(&request(s, &format!("j/{name}"))).unwrap()
}
fn dispatch(s: &mut Session, command: &str) {
    assert_eq!(request(s, &format!("/dispatch {command}")).trim(), "ok");
    s.door().barrier().unwrap();
}
fn chord(s: &mut Session, mods: &[u32], key: u32) {
    for &m in mods {
        s.door().key(m, true).unwrap();
    }
    s.door().tap_key(key).unwrap();
    for &m in mods.iter().rev() {
        s.door().key(m, false).unwrap();
    }
    s.door().barrier().unwrap();
}
fn boot(name: &str) -> Session {
    let mut s = Session::boot(
        name,
        SessionOptions {
            config_extra: "interaction_mode = 'mac'\nshow_dock = false\nhyprland_config = false\n"
                .into(),
            ..Default::default()
        },
    )
    .unwrap();
    s.door().virtual_outputs(true).unwrap();
    poll_until(WAIT, "two output globals", || {
        (json(&s, "monitors").as_array()?.len() == 2).then_some(())
    })
    .unwrap();
    s
}
fn heads(s: &Session) -> (i64, i64) {
    let monitors = json(s, "monitors");
    (
        monitors[0]["activeWorkspace"]["id"].as_i64().unwrap(),
        monitors[1]["activeWorkspace"]["id"].as_i64().unwrap(),
    )
}
fn probe(s: &mut Session, title: &str, output: usize) -> WindowInfo {
    let monitors = json(s, "monitors");
    let x = monitors[output]["x"].as_f64().unwrap() + 280.0;
    s.door().motion(x, 260.0).unwrap();
    s.door().barrier().unwrap();
    let path = profile_binary("chonk-fullscreen-probe").unwrap();
    s.launch(path.to_str().unwrap(), &[title, title]).unwrap();
    s.wait_for_window(title).unwrap()
}
fn pixel(s: &mut Session, title: &str, label: &str) -> [u8; 4] {
    let window = s.world().unwrap().window_matching(title).unwrap().clone();
    let shot = s.screenshot(label).unwrap();
    shot.pixel((window.x + 30) as u32, (window.y + 30) as u32)
}
fn focus(s: &mut Session, title: &str) {
    let w = s.world().unwrap().window_matching(title).unwrap().clone();
    s.door()
        .click((w.x + 50) as f64, (w.y + 50) as f64)
        .unwrap();
    s.door().barrier().unwrap();
}
#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn control_arrows_switch_only_the_pointed_display_and_keep_other_pixels() {
    let mut s = boot("mac-spaces-independent");
    probe(&mut s, "Spaces Left", 0);
    probe(&mut s, "Spaces Right", 1);
    assert_eq!(heads(&s), (1, 2));
    assert_eq!(
        pixel(&mut s, "Spaces Right", "right-before"),
        [32, 64, 128, 255]
    );
    s.door().motion(250.0, 200.0).unwrap();
    s.door().barrier().unwrap();
    dispatch(&mut s, "workspace 3");
    assert_eq!(heads(&s), (3, 2));
    assert_eq!(
        pixel(&mut s, "Spaces Right", "right-while-left-parked"),
        [32, 64, 128, 255]
    );
    let world = s.world().unwrap();
    let left = world.window_matching("Spaces Left").unwrap();
    assert!(!world.frame_of(left.id).unwrap().mapped);
    let parked = s.screenshot("left-space-parked").unwrap();
    assert_ne!(
        parked.pixel((left.x + 30) as u32, (left.y + 30) as u32),
        [32, 64, 128, 255]
    );
    chord(&mut s, &[29], 105);
    assert_eq!(heads(&s), (1, 2));
    assert_eq!(
        pixel(&mut s, "Spaces Left", "left-restored"),
        [32, 64, 128, 255]
    );
    chord(&mut s, &[29], 106);
    assert_eq!(heads(&s), (3, 2));
    s.door().motion(950.0, 200.0).unwrap();
    chord(&mut s, &[29], 106);
    assert_eq!(
        heads(&s),
        (3, 2),
        "the right row ends here; do not cross into the left row"
    );
    focus(&mut s, "Spaces Right");
    chord(&mut s, &[125], 15);
    assert_eq!(
        heads(&s),
        (1, 2),
        "Command-Tab reveals the other application on its owning display"
    );
}
#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn fullscreen_is_independent_and_restores_both_display_geometries() {
    let mut s = boot("mac-spaces-fullscreen");
    let left = probe(&mut s, "Fullscreen Left", 0);
    let right = probe(&mut s, "Fullscreen Right", 1);
    focus(&mut s, "Fullscreen Left");
    chord(&mut s, &[125, 29], 33);
    assert_eq!(heads(&s), (3, 2));
    focus(&mut s, "Fullscreen Right");
    chord(&mut s, &[125, 29], 33);
    assert_eq!(heads(&s), (3, 4));
    let shot = s.screenshot("both-fullscreen").unwrap();
    assert_eq!(shot.pixel(10, 10), [32, 64, 128, 255]);
    assert_eq!(shot.pixel(shot.width - 10, 10), [32, 64, 128, 255]);
    chord(&mut s, &[125, 29], 33);
    assert_eq!(heads(&s), (3, 2));
    focus(&mut s, "Fullscreen Left");
    chord(&mut s, &[125, 29], 33);
    assert_eq!(heads(&s), (1, 2));
    let world = s.world().unwrap();
    for original in [left, right] {
        let restored = world.window_matching(&original.title).unwrap();
        assert_eq!(
            (restored.x, restored.y, restored.w, restored.h),
            (original.x, original.y, original.w, original.h)
        );
    }
}
#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn unplug_reconnect_preserves_membership_and_rehomes_client_pixels() {
    let mut s = boot("mac-spaces-hotplug");
    probe(&mut s, "Dock Left", 0);
    let right = probe(&mut s, "Dock Right", 1);
    s.door().virtual_outputs(false).unwrap();
    assert_eq!(json(&s, "monitors").as_array().unwrap().len(), 1);
    dispatch(&mut s, "workspace 2");
    let borrowed = s
        .world()
        .unwrap()
        .window_matching("Dock Right")
        .unwrap()
        .clone();
    assert!(borrowed.x < right.x);
    assert_eq!(
        pixel(&mut s, "Dock Right", "borrowed-right"),
        [32, 64, 128, 255]
    );
    s.door().virtual_outputs(true).unwrap();
    assert_eq!(heads(&s), (1, 2));
    let restored = s
        .world()
        .unwrap()
        .window_matching("Dock Right")
        .unwrap()
        .clone();
    assert_eq!((restored.x, restored.y), (right.x, right.y));
    assert_eq!(
        pixel(&mut s, "Dock Left", "left-after-reconnect"),
        [32, 64, 128, 255]
    );
    assert_eq!(
        pixel(&mut s, "Dock Right", "right-after-reconnect"),
        [32, 64, 128, 255]
    );
}
#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn native_workspace_clients_receive_two_independent_active_groups() {
    let mut s = boot("mac-spaces-protocol");
    let path = profile_binary("chonk-workspace-probe").unwrap();
    s.launch(path.to_str().unwrap(), &[]).unwrap();
    poll_until(
        WAIT,
        "two independently active native workspace groups",
        || {
            let log = s.client_log("chonk-workspace-probe");
            log.lines()
                .any(|line| line.contains("groups=2 count=2") && line.contains("active=1,1"))
                .then_some(())
        },
    )
    .unwrap_or_else(|e| panic!("{e}: {}", s.client_log("chonk-workspace-probe")));
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn a_crossing_window_is_clipped_until_its_center_moves_to_the_other_display() {
    let mut s = boot("mac-spaces-boundary");
    let left = probe(&mut s, "Boundary Left", 0);
    let frame = s.world().unwrap().frame_of(left.id).unwrap().clone();
    let edge = json(&s, "monitors")[1]["x"].as_i64().unwrap() as i32;
    // Put the right edge over the boundary while keeping the center on the left.
    s.door()
        .drag_to(
            (frame.x as f64 + 150.0, frame.y as f64 + 10.0),
            ((edge - 100) as f64, 100.0),
        )
        .unwrap();
    s.door().barrier().unwrap();
    let w = s
        .world()
        .unwrap()
        .window_matching("Boundary Left")
        .unwrap()
        .clone();
    let y = w.y + 40;
    assert!(
        w.x < edge && w.x + w.w as i32 > edge + 10,
        "fixture must straddle the boundary: {w:?}"
    );
    let shot = s.screenshot("clipped-at-display-boundary").unwrap();
    assert_eq!(shot.pixel((edge - 10) as u32, y as u32), [32, 64, 128, 255]);
    assert_ne!(shot.pixel((edge + 10) as u32, y as u32), [32, 64, 128, 255]);
    assert_eq!(s.door().hit(edge + 10, y).unwrap(), "root");
    let f = s.world().unwrap().frame_of(w.id).unwrap().clone();
    s.door()
        .drag_to(
            (f.x as f64 + 150.0, f.y as f64 + 10.0),
            ((edge + 160) as f64, 100.0),
        )
        .unwrap();
    s.door().barrier().unwrap();
    assert_eq!(
        pixel(&mut s, "Boundary Left", "joined-right-space"),
        [32, 64, 128, 255]
    );
    let clients = json(&s, "clients");
    let client = clients
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["title"] == "Boundary Left")
        .unwrap();
    assert_eq!(client["workspace"]["id"], 2);
    poll_until(
        WAIT,
        "native surface leaves left output and enters right",
        || {
            let log = s.client_log("chonk-fullscreen-probe");
            let output_id = |name: &str| {
                log.lines().find_map(|line| {
                    let rest = line.split("output id=").nth(1)?;
                    let (id, value) = rest.split_once(" name=")?;
                    (value == name).then_some(id.to_string())
                })
            };
            let left = output_id("chonkstep")?;
            let right = output_id("chonkstep-right")?;
            (log.contains(&format!("surface output leave id={left}"))
                && log.contains(&format!("surface output enter id={right}")))
            .then_some(())
        },
    )
    .unwrap_or_else(|error| panic!("{error}: {}", s.client_log("chonk-fullscreen-probe")));
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn overview_uses_the_selected_display_and_hotplug_releases_its_keyboard_grab() {
    let mut s = boot("mac-spaces-overview");
    probe(&mut s, "Overview Left", 0);
    let right = probe(&mut s, "Overview Right", 1);
    dispatch(&mut s, "workspace 3");
    dispatch(&mut s, "workspace 2");
    chord(&mut s, &[29], 103);
    let world = s.world().unwrap();
    assert!(world.overview.is_some());
    assert_eq!(world.overview_windows.len(), 1);
    assert_eq!(world.overview_windows[0].id, right.id);
    s.door().motion(100.0, 100.0).unwrap();
    chord(&mut s, &[29], 106);
    assert_eq!(
        heads(&s),
        (1, 3),
        "Overview navigation stays on its opening display"
    );
    assert!(s.world().unwrap().overview_windows.is_empty());
    chord(&mut s, &[29], 105);
    assert_eq!(heads(&s), (1, 2));
    assert_eq!(
        pixel(&mut s, "Overview Left", "left-during-right-overview"),
        [32, 64, 128, 255]
    );
    s.door().virtual_outputs(false).unwrap();
    assert!(s.world().unwrap().overview.is_none());
    focus(&mut s, "Overview Left");
    s.door().tap_key(33).unwrap();
    poll_until(
        WAIT,
        "client receives keyboard after overview output disappears",
        || {
            let clients = json(&s, "clients");
            clients
                .as_array()?
                .iter()
                .any(|c| {
                    c["title"] == "Overview Left"
                        && c["fullscreen"].as_u64().is_some_and(|state| state != 0)
                })
                .then_some(())
        },
    )
    .unwrap_or_else(|e| panic!("{e}: {}", s.client_log("chonk-fullscreen-probe")));
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn session_restore_keeps_empty_spaces_and_fullscreen_return_on_a_reconnected_display() {
    let spaces = serde_json::json!({
        "spaces": [[7,"connector:chonkstep","connector:chonkstep"],
            [9,"connector:chonkstep-right","connector:chonkstep-right"],
            [12,"connector:chonkstep-right","connector:chonkstep-right"]],
        "displays": [["connector:chonkstep","chonkstep",[0,0,640,800],7],
            ["connector:chonkstep-right","chonkstep-right",[640,0,640,800],12]],
        "selected": "connector:chonkstep-right", "next_id": 12
    });
    let record = format!("@spaces\t{spaces}\n@window\t\"foot\"\tnull\t30\t90\t400\t300\t2\t-\t\"chonkstep-right\"\t{{\"fullscreen_origin\":1,\"focused\":true}}\n");
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    let mut s = Session::boot("mac-spaces-restore", SessionOptions {
        config_extra: format!("interaction_mode = 'mac'\nshow_dock = false\nhyprland_config = false\nrestore_session = true\nterminal = [\"{}\", \"Restored Right\", \"foot\"]\n", probe.display()),
        state_files: vec![("session".into(), record)], ..Default::default()
    }).unwrap();
    s.wait_for_window("Restored Right").unwrap();
    s.door().virtual_outputs(true).unwrap();
    assert_eq!(heads(&s), (1, 3));
    let shot = s.screenshot("restored-fullscreen-right").unwrap();
    assert_eq!(shot.pixel(shot.width - 10, 10), [32, 64, 128, 255]);
    focus(&mut s, "Restored Right");
    chord(&mut s, &[125, 29], 33);
    assert_eq!(heads(&s), (1, 2));
    let w = s
        .world()
        .unwrap()
        .window_matching("Restored Right")
        .unwrap()
        .clone();
    assert_eq!((w.x, w.y, w.w, w.h), (670, 90, 400, 300));
    poll_until(WAIT, "stable surviving Space IDs persisted", || {
        let text = std::fs::read_to_string(s.state_file("session")).ok()?;
        let line = text
            .lines()
            .find_map(|line| line.strip_prefix("@spaces\t"))?;
        let saved: Value = serde_json::from_str(line).ok()?;
        (saved["spaces"].as_array()?.len() == 2
            && saved["spaces"][0][0] == 7
            && saved["spaces"][1][0] == 9)
            .then_some(())
    })
    .unwrap();
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn a_live_swipe_moves_only_its_display_and_stops_at_that_rows_boundary() {
    let mut s = boot("mac-spaces-swipe");
    probe(&mut s, "Swipe Left", 0);
    probe(&mut s, "Swipe Right", 1);
    s.door().motion(200.0, 150.0).unwrap();
    s.door().barrier().unwrap();
    dispatch(&mut s, "workspace 3");
    dispatch(&mut s, "workspace 1");
    s.door().swipe_begin_at(3, 0).unwrap();
    s.door().swipe_update_at(-140.0, 0.0, 200).unwrap();
    s.door().barrier().unwrap();
    assert!(s.world().unwrap().gesture.is_some());
    assert_eq!(heads(&s), (1, 2), "finger motion does not commit the Space");
    assert_eq!(
        pixel(&mut s, "Swipe Right", "right-during-left-swipe"),
        [32, 64, 128, 255]
    );
    assert_ne!(
        pixel(&mut s, "Swipe Left", "left-moved-before-swipe-commit"),
        [32, 64, 128, 255]
    );
    s.door().swipe_end_at(false, 350).unwrap();
    poll_until(WAIT, "left swipe settles", || {
        s.world().ok()?.gesture.is_none().then_some(())
    })
    .unwrap();
    assert_eq!(heads(&s), (3, 2));
    s.door().swipe_begin_at(3, 1000).unwrap();
    s.door().swipe_update_at(-500.0, 0.0, 1200).unwrap();
    s.door().swipe_end_at(false, 1350).unwrap();
    poll_until(WAIT, "boundary resistance settles", || {
        s.world().ok()?.gesture.is_none().then_some(())
    })
    .unwrap();
    assert_eq!(heads(&s), (3, 2));
    assert_eq!(s.world().unwrap().workspace_count, 3);
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn fullscreen_furniture_follows_visible_spaces_instead_of_global_keyboard_focus() {
    let mut s = boot("mac-spaces-fullscreen-furniture");
    let bar = profile_binary("chonk-fake-bar").unwrap();
    s.launch(bar.to_str().unwrap(), &["30"]).unwrap();
    poll_until(WAIT, "top bar maps", || {
        s.client_log("chonk-fake-bar")
            .contains("mapped ")
            .then_some(())
    })
    .unwrap();
    probe(&mut s, "Furniture Left", 0);
    probe(&mut s, "Furniture Right", 1);
    focus(&mut s, "Furniture Left");
    chord(&mut s, &[125, 29], 33);
    focus(&mut s, "Furniture Right");
    let shot = s.screenshot("left-fullscreen-while-right-focused").unwrap();
    assert_eq!(shot.pixel(100, 10), [32, 64, 128, 255]);
    assert_eq!(s.door().hit(100, 10).unwrap(), "content");
    dispatch(&mut s, "workspace 1");
    let shot = s.screenshot("bar-restored-on-ordinary-left-space").unwrap();
    let rgb = chonk_testkit::FAKE_BAR_RGB;
    assert_eq!(shot.pixel(100, 10), [rgb[0], rgb[1], rgb[2], 255]);
    assert_eq!(s.door().hit(100, 10).unwrap(), "layer");
}

fn geometry(w: &WindowInfo) -> (i32, i32, u32, u32) {
    (w.x, w.y, w.w, w.h)
}

fn reload_spaces(s: &mut Session, separate: bool) {
    let reloads = s.log().matches("reload requested").count();
    s.rewrite_config(&format!("interaction_mode = 'mac'\nshow_dock = false\nhyprland_config = false\n[mac]\nseparate_spaces = {separate}\n")).unwrap();
    s.request_reload().unwrap();
    poll_until(WAIT, "Spaces configuration reload", || {
        (s.log().matches("reload requested").count() > reloads).then_some(())
    })
    .unwrap();
    s.door().barrier().unwrap();
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn linking_displays_repairs_focus_before_more_keys_reach_clients() {
    let mut s = boot("mac-spaces-link-focus");
    let left = probe(&mut s, "Link Left", 0);
    probe(&mut s, "Link Right", 1);
    focus(&mut s, "Link Left");
    s.door().motion(1000.0, 600.0).unwrap();
    s.door().barrier().unwrap();
    reload_spaces(&mut s, false);
    assert!(!s.world().unwrap().frame_of(left.id).unwrap().mapped);
    assert_eq!(json(&s, "activewindow")["title"], "Link Right");
    s.door().tap_key(33).unwrap();
    poll_until(WAIT, "visible client receives its fullscreen key", || {
        s.client_log("chonk-fullscreen-probe")
            .contains("answer granted: asked fullscreen=true")
            .then_some(())
    })
    .unwrap();
    let hidden_log =
        std::fs::read_to_string(s.dir.join("client-0-chonk-fullscreen-probe.log")).unwrap();
    assert!(
        !hidden_log.contains("control enter fullscreen"),
        "hidden client received F: {hidden_log}"
    );
    assert!(!s.world().unwrap().frame_of(left.id).unwrap().mapped);
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn headless_swipes_are_ignored_and_reconnect_recovers_input_and_pixels() {
    let mut s = boot("mac-spaces-headless-swipe");
    let left = probe(&mut s, "Headless Left", 0);
    dispatch(&mut s, "workspace 3");
    dispatch(&mut s, "workspace 1");
    s.door().swipe_begin_at(3, 1000).unwrap();
    s.door().swipe_update_at(-150.0, 0.0, 1040).unwrap();
    s.door().barrier().unwrap();
    assert!(s.world().unwrap().gesture.is_some());
    s.door().set_virtual_outputs("none").unwrap();
    assert!(json(&s, "monitors").as_array().unwrap().is_empty());
    assert!(s.world().unwrap().gesture.is_none());
    assert!(!s.world().unwrap().frame_of(left.id).unwrap().mapped);
    s.door().swipe_begin_at(3, 2000).unwrap();
    s.door().swipe_update_at(-150.0, 0.0, 2040).unwrap();
    s.door().swipe_end_at(false, 2080).unwrap();
    s.door().barrier().unwrap();
    assert!(s.compositor_alive());
    assert!(s.world().unwrap().gesture.is_none());
    s.door().set_virtual_outputs("split").unwrap();
    assert_eq!(
        geometry(s.world().unwrap().window_matching("Headless Left").unwrap()),
        geometry(&left)
    );
    assert_eq!(
        pixel(&mut s, "Headless Left", "after-full-reconnect"),
        [32, 64, 128, 255]
    );
    focus(&mut s, "Headless Left");
    s.door().tap_key(33).unwrap();
    poll_until(WAIT, "input works after full reconnect", || {
        s.client_log("chonk-fullscreen-probe")
            .contains("answer granted: asked fullscreen=true")
            .then_some(())
    })
    .unwrap();
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn smaller_survivor_keeps_home_position_and_restore_geometry() {
    for fullscreen_before_disconnect in [true, false] {
        let mut s = boot(&format!("mac-spaces-smaller-survivor-{fullscreen_before_disconnect}"));
        probe(&mut s, "Sized Right", 1);
        dispatch(&mut s, "moveactive exact 850 350");
        let original = s.world().unwrap().window_matching("Sized Right").unwrap().clone();
        assert!(original.y >= 350);
        dispatch(&mut s, "fullscreen 1");
        if fullscreen_before_disconnect {
            chord(&mut s, &[125, 29], 33);
        }
        s.door().set_virtual_outputs("compact").unwrap();
        if !fullscreen_before_disconnect {
            dispatch(&mut s, "workspace 2");
            focus(&mut s, "Sized Right");
            chord(&mut s, &[125, 29], 33);
        }
        let compact = s.world().unwrap().window_matching("Sized Right").unwrap().clone();
        assert_eq!((compact.w, compact.h), (400, 300));
        s.door().set_virtual_outputs("split").unwrap();
        // Reconnect restores the home's remembered active Space. A fullscreen
        // Space created while borrowed may need explicit activation first.
        let clients = json(&s, "clients");
        let workspace = clients.as_array().unwrap().iter()
            .find(|c| c["title"] == "Sized Right").unwrap()["workspace"]["id"].as_u64().unwrap();
        dispatch(&mut s, &format!("workspace {workspace}"));
        focus(&mut s, "Sized Right");
        chord(&mut s, &[125, 29], 33);
        dispatch(&mut s, "fullscreen 1");
        assert_eq!(geometry(s.world().unwrap().window_matching("Sized Right").unwrap()), geometry(&original));
        assert_eq!(pixel(&mut s, "Sized Right", "home-geometry-restored"), [32, 64, 128, 255]);
    }
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn enabling_separate_spaces_preserves_the_live_linked_desktop() {
    let mut s = boot("mac-spaces-enable-live");
    reload_spaces(&mut s, false);
    dispatch(&mut s, "workspace 4");
    let left = probe(&mut s, "Enable Left", 0);
    let right = probe(&mut s, "Enable Right", 1);
    reload_spaces(&mut s, true);
    let world = s.world().unwrap();
    assert!(world.frame_of(left.id).unwrap().mapped && world.frame_of(right.id).unwrap().mapped);
    assert_eq!(json(&s, "activewindow")["title"], "Enable Right");
    assert_eq!(
        pixel(&mut s, "Enable Left", "active-left-preserved"),
        [32, 64, 128, 255]
    );
    assert_eq!(
        pixel(&mut s, "Enable Right", "active-right-preserved"),
        [32, 64, 128, 255]
    );
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn clicking_a_pinned_window_keeps_the_current_space() {
    let mut s = boot("mac-spaces-pinned-focus");
    probe(&mut s, "Pinned Left", 0);
    dispatch(&mut s, "pin");
    dispatch(&mut s, "workspace 3");
    focus(&mut s, "Pinned Left");
    assert_eq!(heads(&s), (3, 2));
    assert_eq!(json(&s, "activewindow")["title"], "Pinned Left");
    assert_eq!(
        pixel(&mut s, "Pinned Left", "pinned-focus-without-switch"),
        [32, 64, 128, 255]
    );
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn dragging_a_fullscreen_dialog_moves_a_consistent_family() {
    let mut s = boot("mac-spaces-fullscreen-dialog");
    let parent = probe(&mut s, "Dialog Parent", 0);
    chord(&mut s, &[125, 29], 33);
    s.door().tap_key(32).unwrap();
    let dialog = s.wait_for_window("Space Dialog").unwrap();
    let frame = s.world().unwrap().frame_of(dialog.id).unwrap().clone();
    s.door()
        .drag_to(
            (frame.x as f64 + 100.0, frame.y as f64 + 10.0),
            (1000.0, 300.0),
        )
        .unwrap();
    s.door().barrier().unwrap();
    let clients = json(&s, "clients");
    for title in ["Dialog Parent", "Space Dialog"] {
        let client = clients
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["title"] == title)
            .unwrap();
        assert_eq!(
            client["workspace"]["id"], 2,
            "family member must join the right Space"
        );
    }
    assert_eq!(
        s.world().unwrap().workspace_count,
        2,
        "retire the vacated fullscreen Space"
    );
    assert_eq!(
        s.world()
            .unwrap()
            .window_matching("Dialog Parent")
            .unwrap()
            .w,
        parent.w
    );
    assert_eq!(
        pixel(&mut s, "Space Dialog", "dialog-family-on-right"),
        [32, 64, 128, 255]
    );
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn saved_fullscreen_over_maximize_unwinds_to_the_original_window() {
    for borrowed in [false, true] {
        let mut source = boot(&format!("mac-spaces-save-zoom-{borrowed}"));
        source.door().motion(900.0, 300.0).unwrap();
        source.door().barrier().unwrap();
        let path = profile_binary("chonk-fullscreen-probe").unwrap();
        source
            .launch(path.to_str().unwrap(), &["Saved Zoom", "foot"])
            .unwrap();
        let original = source.wait_for_window("Saved Zoom").unwrap();
        dispatch(&mut source, "fullscreen 1");
        chord(&mut source, &[125, 29], 33);
        if borrowed {
            source.door().set_virtual_outputs("compact").unwrap();
        }
        let record = poll_until(WAIT, "fullscreen/maximize session snapshot", || {
            let text = std::fs::read_to_string(source.state_file("session")).ok()?;
            (text.contains("\"fullscreen_origin\":1")
                && text.contains("maximized")
                && (!borrowed || text.contains("\"home_geometry\":{")))
            .then_some(text)
        })
        .unwrap();
        drop(source);
        let mut s = Session::boot(&format!("mac-spaces-restore-zoom-{borrowed}"), SessionOptions {
        config_extra: format!("interaction_mode = 'mac'\nshow_dock = false\nhyprland_config = false\nrestore_session = true\nterminal = [\"{}\", \"Restored Zoom\", \"foot\"]\n", path.display()),
        state_files: vec![("session".into(), record)], ..Default::default()
    }).unwrap();
        s.wait_for_window("Restored Zoom").unwrap();
        s.door().set_virtual_outputs("split").unwrap();
        focus(&mut s, "Restored Zoom");
        chord(&mut s, &[125, 29], 33);
        dispatch(&mut s, "fullscreen 1");
        assert_eq!(
            geometry(s.world().unwrap().window_matching("Restored Zoom").unwrap()),
            geometry(&original)
        );
        assert_eq!(
            pixel(&mut s, "Restored Zoom", "restored-original-size"),
            [32, 64, 128, 255]
        );
    }
}
