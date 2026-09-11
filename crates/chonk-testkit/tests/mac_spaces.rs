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
    // Workspace publication precedes the clients' configure acknowledgments.
    // Await their real fullscreen buffers, not just the WM's geometry update.
    poll_until(WAIT, "both clients paint fullscreen buffers", || {
        let shot = s.screenshot("both-fullscreen").ok()?;
        (shot.pixel(10,10) == [32,64,128,255]
            && shot.pixel(shot.width-10,10) == [32,64,128,255]).then_some(())
    }).unwrap();
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
    // Output publication precedes the client's fullscreen resize response.
    // Wait for actual pixels, not merely the compositor's configure request.
    poll_until(WAIT, "restored fullscreen client paints the reconnected output", || {
        let shot = s.screenshot("restored-fullscreen-right").ok()?;
        (shot.pixel(shot.width - 10, 10) == [32, 64, 128, 255]).then_some(())
    }).unwrap();
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

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces thumbnails"]
fn desktop_thumbnails_show_real_placement_updates_moves_and_removal() {
    let mut s = boot("mac-spaces-thumbnails");
    let first = probe(&mut s, "Thumbnail First", 0);
    dispatch(&mut s, "movewindowpixel exact 40 190,title:^Thumbnail First$");
    let right = probe(&mut s, "Thumbnail Right", 1);
    s.door().motion(200.0, 400.0).unwrap();
    dispatch(&mut s, "workspace 3");
    let color = s.dir.join("preview-rgb");
    std::fs::write(&color, [210, 80, 45]).unwrap();
    let binary = profile_binary("chonk-fullscreen-probe").unwrap();
    s.launch_isolated("env", &[&format!("CHONKSTEP_PROBE_COLOR_FILE={}", color.display()),
        binary.to_str().unwrap(), "Thumbnail Second", "thumbnail-second", "animate-frame"]).unwrap();
    let second = s.wait_for_window("Thumbnail Second").unwrap();
    dispatch(&mut s, "resizewindowpixel exact 270 210,title:^Thumbnail Second$");
    dispatch(&mut s, "movewindowpixel exact 315 470,title:^Thumbnail Second$");
    dispatch(&mut s, "workspace 1");
    chord(&mut s, &[29], 103);
    poll_until(WAIT, "each Space lists its own windows", || {
        let world = s.world().ok()?;
        (world.overview_space_windows.contains(&(0, first.id))
            && world.overview_space_windows.contains(&(1, second.id))
            && !world.overview_space_windows.iter().any(|(_, id)| *id == right.id)).then_some(())
    }).unwrap();
    // Independent geometry oracle: left output is 640x800. Sample safely
    // inside each client's content, not the titlebar or the card caption.
    let sample = |s: &mut Session, index: usize, id: u64, expected: [u8; 4], label: &str| {
        poll_until(WAIT, label, || {
            let world = s.world().ok()?;
            let thumb = world.overview_spaces.iter().find(|t| t.index == index)?.rect;
            let window = world.windows.iter().find(|w| w.id == id)?;
            let scale = (thumb.size.w as f64 / 640.0).min(thumb.size.h as f64 / 800.0);
            let x = thumb.pos.x + ((window.x + 80) as f64 * scale).round() as i32;
            let y = thumb.pos.y + ((window.y + 80) as f64 * scale).round() as i32;
            let shot = s.screenshot(label).ok()?;
            (shot.pixel(x as u32, y as u32) == expected).then_some(())
        }).unwrap();
    };
    sample(&mut s, 0, first.id, [32,64,128,255], "first-desktop-window-pixels");
    sample(&mut s, 1, second.id, [210,80,45,255], "parked-desktop-window-pixels");
    // The parked client's new content must repaint its thumbnail while the
    // current desktop and keyboard focus remain unchanged.
    std::fs::write(&color, [40,190,100]).unwrap();
    dispatch(&mut s, "resizewindowpixel exact 280 220,title:^Thumbnail Second$");
    sample(&mut s, 1, second.id, [40,190,100,255], "live-parked-thumbnail-update");
    assert_eq!(heads(&s), (1,2));
    let callbacks = |s: &Session| s.client_log("env").lines()
        .filter_map(|line| line.strip_prefix("frame callback=")?.parse::<u64>().ok()).max().unwrap_or(0);
    let before_callbacks = callbacks(&s);
    poll_until(WAIT, "an inactive desktop animates only while its thumbnail is visible", || {
        (callbacks(&s) >= before_callbacks + 3).then_some(())
    }).unwrap();
    // Move with Overview's real drag and verify both membership and pixels.
    let world = s.world().unwrap();
    let card = world.overview_windows.iter().find(|w| w.id == first.id).unwrap().rect;
    let target = world.overview_spaces[1].rect;
    s.door().motion(f64::from(card.pos.x + card.size.w as i32/2), f64::from(card.pos.y + card.size.h as i32/2)).unwrap();
    s.door().button("left", true).unwrap();
    s.door().motion(f64::from(target.pos.x + target.size.w as i32/2), f64::from(target.pos.y + target.size.h as i32/2)).unwrap();
    s.door().button("left", false).unwrap();
    poll_until(WAIT, "drag updates both desktop thumbnails", || {
        let world = s.world().ok()?;
        (!world.overview_space_windows.contains(&(0, first.id))
            && world.overview_space_windows.contains(&(1, first.id))).then_some(())
    }).unwrap();
    sample(&mut s, 1, first.id, [32,64,128,255], "moved-window-thumbnail-pixels");
    dispatch(&mut s, "closewindow title:^Thumbnail First$");
    poll_until(WAIT, "closed window disappears from every thumbnail", || {
        (!s.world().ok()?.overview_space_windows.iter().any(|(_, id)| *id == first.id)).then_some(())
    }).unwrap();
    sample(&mut s, 1, second.id, [40,190,100,255], "remaining-window-thumbnail-pixels");
    chord(&mut s, &[], 1);
    assert!(s.world().unwrap().overview.is_none());
    assert_eq!(heads(&s), (1,2));
    std::thread::sleep(Duration::from_millis(300));
    let stopped = callbacks(&s);
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(callbacks(&s), stopped, "closing Overview must park off-desktop animation again");
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces"]
fn title_updates_preserve_an_active_desktop_drag() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt, CreateWindowAux, PropMode, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;
    let mut s = boot("mac-spaces-title-drag");
    s.door().motion(200.0, 400.0).unwrap();
    dispatch(&mut s, "workspace 3");
    dispatch(&mut s, "workspace 1");
    let (connection, screen) = s.connect_x11().unwrap();
    let root = connection.setup().roots[screen].root;
    let window = connection.generate_id().unwrap();
    connection.create_window(x11rb::COPY_DEPTH_FROM_PARENT, window, root,
        40, 190, 300, 250, 0, WindowClass::INPUT_OUTPUT, x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().background_pixel(0xd2502d)).unwrap().check().unwrap();
    connection.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME,
        AtomEnum::STRING, b"Drag Before").unwrap();
    connection.map_window(window).unwrap();
    connection.flush().unwrap();
    let client = s.wait_for_window("Drag Before").unwrap();
    chord(&mut s, &[29], 103);
    poll_until(WAIT, "Overview entry animation complete", || {
        (s.world().ok()?.overview?.progress == 1.0).then_some(())
    }).unwrap();
    let world = s.world().unwrap();
    let card = world.overview_windows.iter().find(|w| w.id == client.id).unwrap().rect;
    let target = world.overview_spaces[1].rect;
    let x = f64::from(card.pos.x + card.size.w as i32 / 2);
    let y = f64::from(card.pos.y + card.size.h as i32 / 2);
    s.door().motion(x, y).unwrap();
    s.door().barrier().unwrap();
    s.door().button("left", true).unwrap();
    s.door().barrier().unwrap();
    s.door().motion(x + 40.0, y - 20.0).unwrap();
    poll_until(WAIT, "card picked up", || s.world().ok()?.overview_drag)
        .unwrap_or_else(|e| panic!("{e}: card {card:?}, world {:?}", s.world().unwrap()));
    // Browser loads and terminal commands change titles during ordinary drags.
    // Wait for the compositor to observe the real X11 property change while
    // the button remains held; a semantic refresh must preserve the gesture.
    connection.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME,
        AtomEnum::STRING, b"Drag After").unwrap();
    connection.flush().unwrap();
    s.wait_for_window("Drag After").unwrap();
    s.door().barrier().unwrap();
    assert!(s.world().unwrap().overview_drag.is_some(), "title refresh cancelled the held drag");
    s.door().motion(f64::from(target.pos.x + target.size.w as i32 / 2),
        f64::from(target.pos.y + target.size.h as i32 / 2)).unwrap();
    s.door().button("left", false).unwrap();
    poll_until(WAIT, "renamed window dropped on Desktop 2", || {
        json(&s, "clients").as_array()?.iter().find(|w| w["title"] == "Drag After")
            .filter(|w| w["workspace"]["id"] == 3).map(|_| ())
    }).unwrap();
    let world = s.world().unwrap();
    assert!(world.overview_space_windows.contains(&(1, client.id)));
    assert!(!world.overview_space_windows.contains(&(0, client.id)));
    // A real geometry change is different: the old card coordinates are no
    // longer a valid drag contract. Consume release without moving/activating.
    s.door().click(f64::from(target.pos.x + target.size.w as i32 / 2),
        f64::from(target.pos.y + target.size.h as i32 / 2)).unwrap();
    s.door().barrier().unwrap();
    let world = s.world().unwrap();
    let card = world.overview_windows.iter().find(|w| w.id == client.id).unwrap().rect;
    let x = f64::from(card.pos.x + card.size.w as i32 / 2);
    let y = f64::from(card.pos.y + card.size.h as i32 / 2);
    s.door().motion(x, y).unwrap();
    s.door().barrier().unwrap();
    s.door().button("left", true).unwrap();
    s.door().barrier().unwrap();
    s.door().motion(x + 40.0, y - 20.0).unwrap();
    poll_until(WAIT, "second drag picked up", || s.world().ok()?.overview_drag).unwrap();
    dispatch(&mut s, "resizewindowpixel exact 350 280,title:^Drag After$");
    poll_until(WAIT, "geometry change cancels the stale drag", || {
        s.world().ok()?.overview_drag.is_none().then_some(())
    }).unwrap();
    let target = world.overview_spaces[0].rect;
    s.door().motion(f64::from(target.pos.x + target.size.w as i32 / 2),
        f64::from(target.pos.y + target.size.h as i32 / 2)).unwrap();
    s.door().button("left", false).unwrap();
    s.door().barrier().unwrap();
    assert!(s.world().unwrap().overview.is_some());
    assert!(json(&s, "clients").as_array().unwrap().iter()
        .any(|w| w["title"] == "Drag After" && w["workspace"]["id"] == 3));
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces thumbnails"]
fn desktop_thumbnails_respect_pinned_minimized_and_fullscreen_windows_per_display() {
    let mut s = boot("mac-spaces-thumbnail-lifecycle");
    let pinned = probe(&mut s, "Pinned Preview", 0);
    dispatch(&mut s, "pin");
    dispatch(&mut s, "workspace 3");
    let right = probe(&mut s, "Fullscreen Preview", 1);
    s.door().motion(200.0, 350.0).unwrap();
    chord(&mut s, &[29], 103);
    let world = s.world().unwrap();
    assert!(world.overview_space_windows.contains(&(0,pinned.id)));
    assert!(world.overview_space_windows.contains(&(1,pinned.id)));
    assert!(!world.overview_space_windows.iter().any(|(_,id)| *id == right.id));
    chord(&mut s, &[], 1);
    focus(&mut s, "Pinned Preview");
    chord(&mut s, &[125], 50);
    chord(&mut s, &[29], 103);
    assert!(!s.world().unwrap().overview_space_windows.iter().any(|(_,id)| *id == pinned.id),
        "minimized windows are absent from desktop miniatures");
    chord(&mut s, &[], 1);
    focus(&mut s, "Fullscreen Preview");
    chord(&mut s, &[29,125], 33);
    poll_until(WAIT, "fullscreen client fills its display", || {
        let window = s.world().ok()?.window_matching("Fullscreen Preview")?.clone();
        (window.x == 640 && window.y == 0 && window.w == 640 && window.h == 800).then_some(())
    }).unwrap();
    chord(&mut s, &[29], 103);
    let world = s.world().unwrap();
    assert!(world.overview_space_windows.iter().any(|(_,id)| *id == right.id));
    assert!(!world.overview_space_windows.iter().any(|(_,id)| *id == pinned.id));
    let index = world.overview_space_windows.iter().find(|(_,id)| *id == right.id).unwrap().0;
    let thumb = world.overview_spaces.iter().find(|t| t.index == index).unwrap().rect;
    poll_until(WAIT, "fullscreen desktop miniature contains its real pixels", || {
        let shot = s.screenshot("fullscreen-thumbnail").ok()?;
        let x = 640 + thumb.pos.x as u32 + thumb.size.w/2;
        let y = thumb.pos.y as u32 + thumb.size.h/2;
        (shot.pixel(x,y) == [32,64,128,255]).then_some(())
    }).unwrap();
    chord(&mut s, &[], 1);
    chord(&mut s, &[29,125], 33);
    chord(&mut s, &[29], 103);
    assert_eq!(s.world().unwrap().overview_spaces.len(),1,
        "exiting fullscreen retires its temporary desktop thumbnail");
}

#[test]
#[ignore = "scripts/e2e.sh --headless --test mac_spaces thumbnails"]
fn desktop_thumbnails_update_parked_xwayland_windows_and_clip_to_their_desktop() {
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ChangeWindowAttributesAux, ConnectionExt,
        CreateWindowAux, PropMode, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;
    let mut s = boot("mac-spaces-x11-thumbnails");
    let (connection, screen) = s.connect_x11().unwrap();
    let root = connection.setup().roots[screen].root;
    let window = connection.generate_id().unwrap();
    connection.create_window(x11rb::COPY_DEPTH_FROM_PARENT, window, root,
        40, 190, 300, 250, 0, WindowClass::INPUT_OUTPUT, x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().background_pixel(0xd2502d)).unwrap().check().unwrap();
    connection.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME,
        AtomEnum::STRING, b"X11 Thumbnail").unwrap();
    connection.map_window(window).unwrap();
    connection.flush().unwrap();
    let client = s.wait_for_window("X11 Thumbnail").unwrap();
    dispatch(&mut s, "movewindowpixel exact 450 350,title:^X11 Thumbnail$");
    dispatch(&mut s, "workspace 3");
    chord(&mut s, &[29], 103);
    let assert_color = |s: &mut Session, expected: [u8;4], label: &str| {
        poll_until(WAIT, label, || {
            let world = s.world().ok()?;
            let thumb = world.overview_spaces.iter().find(|t| t.index == 0)?.rect;
            if !world.overview_space_windows.contains(&(0,client.id)) { return None; }
            let scale = (thumb.size.w as f64 / 640.0).min(thumb.size.h as f64 / 800.0);
            let shot = s.screenshot(label).ok()?;
            let y = (thumb.pos.y as f64 + 430.0*scale).round() as u32;
            let x = (thumb.pos.x as f64 + 530.0*scale).round() as u32;
            // This client crosses the output boundary. Its miniature must end
            // at the desktop edge, not paint into the neighboring thumbnail.
            let outside = thumb.pos.x as u32 + thumb.size.w + 3;
            (shot.pixel(x,y) == expected && shot.pixel(outside,y) != expected).then_some(())
        }).unwrap();
    };
    assert_color(&mut s, [210,80,45,255], "parked-x11-thumbnail");
    connection.change_window_attributes(window,
        &ChangeWindowAttributesAux::new().background_pixel(0x28be64)).unwrap();
    connection.clear_area(false, window, 0, 0, 0, 0).unwrap();
    connection.flush().unwrap();
    assert_color(&mut s, [40,190,100,255], "parked-x11-thumbnail-repaint");
    connection.destroy_window(window).unwrap();
    connection.flush().unwrap();
    poll_until(WAIT, "destroyed X11 client leaves no thumbnail", || {
        (!s.world().ok()?.overview_space_windows.iter().any(|(_,id)| *id == client.id)).then_some(())
    }).unwrap();
}
