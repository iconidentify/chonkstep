//! Product path: real native clients, compatibility IPC, input, live scenes,
//! persistence, and the existing finger-following Overview/workspace routes.
use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions, World};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

fn request(session: &Session, message: &str) -> String {
    let path = std::path::PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap())
        .join("hypr")
        .join(session.hyprland_signature().unwrap())
        .join(".socket.sock");
    let mut socket = UnixStream::connect(path).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    socket.write_all(message.as_bytes()).unwrap();
    let mut reply = String::new();
    socket.read_to_string(&mut reply).unwrap();
    reply
}

fn dispatch(session: &mut Session, command: &str) {
    assert_eq!(
        request(session, &format!("/dispatch {command}")),
        "ok",
        "{command}"
    );
    session.door().barrier().unwrap();
}

fn settled(session: &mut Session) -> World {
    poll_until(
        Duration::from_secs(5),
        "layout and gesture settlement",
        || {
            let world = session.world().ok()?;
            (!world.spatial.moving && world.gesture.is_none()).then_some(world)
        },
    )
    .unwrap()
}

fn mode(session: &mut Session, mode: &str) -> World {
    dispatch(session, &format!("layout {mode}"));
    settled(session)
}

fn geometry(world: &World, class: &str) -> (i32, i32, u32, u32) {
    let w = world.window_matching(class).unwrap();
    (w.x, w.y, w.w, w.h)
}

fn overview_roundtrip(session: &mut Session) {
    session.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    assert!(session.world().unwrap().overview.is_some());
    session.screenshot("overview").unwrap();
    session.door().tap_key(keys::ESC).unwrap();
    assert!(session.world().unwrap().overview.is_none());
}

#[test]
#[ignore = "requires native nested Wayland"]
fn application_popups_are_visible_and_receive_input_in_every_style() {
    for scale in [1.0, 1.5, 2.0] {
        let mut session = Session::boot(
            &format!("spatial-popup-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: "show_dock = false\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let probe = profile_binary("chonk-fullscreen-probe").unwrap();
        session
            .launch(probe.to_str().unwrap(), &["Popup", "spatial-popup"])
            .unwrap();
        session.wait_for_window("spatial-popup").unwrap();
        for style in ["freeform", "mosaic", "flow"] {
            let world = mode(&mut session, style);
            poll_until(
                Duration::from_secs(5),
                "client-visible layout state",
                || {
                    let log = session.client_log("chonk-fullscreen-probe");
                    let configure = log
                        .lines()
                        .rev()
                        .find(|line| line.starts_with("configure "))?;
                    (configure.contains("tiled-left") == (style != "freeform")).then_some(())
                },
            )
            .unwrap();
            let window = world.window_matching("spatial-popup").unwrap();
            // This probe deliberately keeps 1x buffers even on scaled outputs;
            // its surface-local popup anchor follows that declared scale.
            let x = window.x + 80;
            let y = window.y + 80;
            session.door().tap_key(25).unwrap();
            poll_until(Duration::from_secs(5), "native popup map", || {
                (session.door().hit(x, y).ok()?.as_str() == "popup").then_some(())
            })
            .unwrap();
            let shot = session.screenshot(&format!("{style}-popup")).unwrap();
            let [r, g, b, _] = shot.pixel(x as u32, y as u32);
            assert!(
                g > 190 && r < 70 && b < 100,
                "{style} scale {scale}: popup input exists but pixels are {r},{g},{b}"
            );
            let before = session
                .client_log("chonk-fullscreen-probe")
                .matches("popup clicked")
                .count();
            session.door().click(x as f64, y as f64).unwrap();
            poll_until(Duration::from_secs(5), "native popup click", || {
                (session
                    .client_log("chonk-fullscreen-probe")
                    .matches("popup clicked")
                    .count()
                    > before)
                    .then_some(())
            })
            .unwrap();
            session.door().tap_key(25).unwrap();
            session.door().barrier().unwrap();
        }
        let focus = session.world().unwrap().logical_focus;
        session.door().tap_key(49).unwrap(); // N: commit an infeasible minimum.
        poll_until(
            Duration::from_secs(5),
            "late constraints suspend participation",
            || (session.world().ok()?.spatial.managed == 0).then_some(()),
        )
        .unwrap();
        session.door().tap_key(49).unwrap();
        poll_until(
            Duration::from_secs(5),
            "cleared constraints restore participation",
            || (session.world().ok()?.spatial.managed == 1).then_some(()),
        )
        .unwrap();
        assert_eq!(settled(&mut session).logical_focus, focus);
    }
}

#[test]
#[ignore = "requires native nested Wayland"]
fn three_views_preserve_intent_with_native_input_and_live_surfaces_at_each_scale() {
    for scale in [1.0, 1.5, 2.0] {
        let mut session = Session::boot(
            &format!("spatial-acceptance-{scale}"),
            SessionOptions {
                scale: Some(scale),
                config_extra: "show_dock = false\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        for class in ["spatial-a", "spatial-b", "spatial-c"] {
            let probe = profile_binary("chonk-fullscreen-probe").unwrap();
            session
                .launch(probe.to_str().unwrap(), &[class, class])
                .unwrap();
            session.wait_for_window(class).unwrap();
        }
        for (class, x, y, w, h) in [
            ("spatial-a", 80, 100, 700, 450),
            ("spatial-b", 900, 120, 550, 600),
            ("spatial-c", 600, 700, 600, 450),
        ] {
            dispatch(
                &mut session,
                &format!("resizewindowpixel exact {w} {h},class:{class}"),
            );
            dispatch(
                &mut session,
                &format!("movewindowpixel exact {x} {y},class:{class}"),
            );
        }
        let original = session.world().unwrap();
        let focus = original.logical_focus;
        assert_eq!(original.spatial.mode, "Freeform");
        overview_roundtrip(&mut session);
        session.door().chord(keys::LEFTMETA, 38).unwrap(); // Super+L
        let following = session.world().unwrap();
        assert_eq!(following.spatial.mode, "Mosaic");
        assert!(
            following.spatial.moving,
            "mode switch uses a live transition"
        );
        session.screenshot("mosaic-in-motion").unwrap();
        let tiled = settled(&mut session);
        assert_eq!(tiled.logical_focus, focus);
        assert_eq!(tiled.spatial.managed, 3);
        let windows: Vec<_> = tiled.frames.iter().filter(|f| f.mapped).collect();
        for (i, a) in windows.iter().enumerate() {
            for b in windows.iter().skip(i + 1) {
                assert!(
                    a.x + a.w as i32 <= b.x
                        || b.x + b.w as i32 <= a.x
                        || a.y + a.h as i32 <= b.y
                        || b.y + b.h as i32 <= a.y
                );
            }
        }
        dispatch(&mut session, "focuswindow class:spatial-b");
        session.door().chord(keys::LEFTMETA, 20).unwrap(); // Super+T
        let floated = settled(&mut session);
        assert_eq!(
            geometry(&floated, "spatial-b"),
            geometry(&original, "spatial-b")
        );
        dispatch(&mut session, "resizeactive 60 20");
        dispatch(&mut session, "moveactive 80 30");
        session.door().chord(keys::LEFTMETA, 20).unwrap();
        let rejoined = settled(&mut session);
        assert_eq!(
            geometry(&rejoined, "spatial-b"),
            geometry(&tiled, "spatial-b")
        );
        assert_eq!(rejoined.logical_focus, floated.logical_focus);
        overview_roundtrip(&mut session);
        session.door().chord(keys::LEFTMETA, 38).unwrap();
        assert_eq!(settled(&mut session).spatial.mode, "Flow");
        dispatch(&mut session, "movefocus r");
        let flow = settled(&mut session);
        let selected = flow.logical_focus;
        dispatch(&mut session, "movefocus u");
        assert_eq!(session.world().unwrap().logical_focus, selected);
        dispatch(&mut session, "resizeactive 120 0");
        assert_eq!(settled(&mut session).logical_focus, selected);
        dispatch(&mut session, "movewindow l");
        assert_eq!(settled(&mut session).logical_focus, selected);
        dispatch(&mut session, "layoutmsg togglesplit");
        overview_roundtrip(&mut session);
        // Minimize/restore through ChonkStep's existing keyboard semantics.
        session.door().key(keys::LEFTALT, true).unwrap();
        session.door().key(keys::LEFTSHIFT, true).unwrap();
        session.door().tap_key(50).unwrap(); // M
        session.door().key(keys::LEFTSHIFT, false).unwrap();
        session.door().key(keys::LEFTALT, false).unwrap();
        assert_eq!(settled(&mut session).spatial.managed, 2);
        dispatch(&mut session, "focuswindow class:spatial-c");
        settled(&mut session);
        mode(&mut session, "mosaic");
        let restored = mode(&mut session, "freeform");
        for class in ["spatial-a", "spatial-b", "spatial-c"] {
            assert_eq!(geometry(&restored, class), geometry(&original, class));
        }
        mode(&mut session, "flow");
        session.door().swipe_begin_at(3, 0).unwrap();
        session.door().swipe_update_at(-120.0, 0.0, 200).unwrap();
        assert_eq!(session.world().unwrap().current_workspace, 0);
        session.door().swipe_end_at(false, 350).unwrap();
        assert_eq!(settled(&mut session).current_workspace, 1);
        dispatch(&mut session, "workspace 1");
        assert_eq!(settled(&mut session).spatial.mode, "Flow");
        let saved = poll_until(Duration::from_secs(6), "persisted workspace mode", || {
            let text = std::fs::read_to_string(session.state_file("session")).ok()?;
            text.contains("@workspace\t0\tscrolling").then_some(text)
        })
        .unwrap();
        assert!(saved.contains("flow_width"));
    }
}

#[test]
#[ignore = "requires native nested Wayland"]
fn reflow_stages_final_configures_once_and_has_no_animation_deadline_after_settlement() {
    let mut session = Session::boot(
        "spatial-live-configures",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let binary = profile_binary("chonk-input-probe").unwrap();
    session
        .launch(binary.to_str().unwrap(), &["1", "--interactive=resize"])
        .unwrap();
    session.wait_for_window("input-probe").unwrap();
    session.door().barrier().unwrap();
    let initial = session.world().unwrap();
    dispatch(&mut session, "layout mosaic");
    let moving = session.world().unwrap();
    assert!(moving.spatial.moving);
    let screenshot = session.screenshot("live-intermediate").unwrap();
    assert!(screenshot.width > 0);
    let final_state = settled(&mut session);
    assert_eq!(final_state.logical_focus, initial.logical_focus);
    assert!(
        final_state.spatial.configures - initial.spatial.configures <= 2,
        "one final resize, plus optional activation state"
    );
    let calculations = final_state.spatial.calculations;
    session.door().frame_stats().unwrap();
    // No synthetic frames or layout recalculation after settling, even with
    // the caption visible. Its expiry is a single damage event.
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(session.door().frame_stats().unwrap().render_calls, 0);
    assert_eq!(session.world().unwrap().spatial.calculations, calculations);
}

#[test]
#[ignore = "requires native nested Wayland"]
fn native_pointer_reorder_resize_cancel_and_modal_takeover_leave_no_transforms() {
    let mut session = Session::boot(
        "spatial-pointer",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let binary = profile_binary("chonk-fullscreen-probe").unwrap();
    for class in ["pointer-a", "pointer-b", "pointer-c"] {
        session
            .launch(binary.to_str().unwrap(), &[class, class])
            .unwrap();
        session.wait_for_window(class).unwrap();
    }
    for style in ["mosaic", "flow"] {
        let before = mode(&mut session, style);
        let a = before.window_matching("pointer-a").unwrap();
        dispatch(&mut session, "focuswindow class:pointer-a");
        let before = settled(&mut session);
        let source = before.frame_of(a.id).unwrap().clone();
        let b = before.window_matching("pointer-b").unwrap();
        let target = before.frame_of(b.id).unwrap();
        let destination = (
            (target.x + target.w as i32 / 2).clamp(15, before.output_w as i32 - 15) as f64,
            (target.y + target.h as i32 / 2) as f64,
        );
        let grip = (
            (source.x + source.w as i32 / 2) as f64,
            (source.y + 8) as f64,
        );
        session.door().drag_to(grip, destination).unwrap();
        session
            .screenshot(&format!("{style}-drop-preview"))
            .unwrap();
        // The live drag is presentation only until release.
        assert_eq!(
            geometry(&session.world().unwrap(), "pointer-a"),
            geometry(&before, "pointer-a")
        );
        session.door().tap_key(keys::ESC).unwrap();
        session.door().button("left", false).unwrap();
        session.door().barrier().unwrap();
        assert_eq!(
            geometry(&settled(&mut session), "pointer-a"),
            geometry(&before, "pointer-a")
        );
        session.door().drag_to(grip, destination).unwrap();
        session.door().button("left", false).unwrap();
        session.door().barrier().unwrap();
        assert_ne!(
            geometry(&settled(&mut session), "pointer-a"),
            geometry(&before, "pointer-a")
        );
        let initial = settled(&mut session);
        let frame = initial.frame_of(a.id).unwrap();
        let content = initial.window_matching("pointer-a").unwrap();
        // Modifier resize uses the same shared-boundary path as native grips.
        let corner = (
            if frame.x > 5 {
                (content.x + 20) as f64
            } else {
                (content.x + content.w as i32 - 20).min(initial.output_w as i32 - 20) as f64
            },
            (frame.y + frame.h as i32 / 2) as f64,
        );
        session.door().motion(corner.0, corner.1).unwrap();
        session.door().barrier().unwrap();
        session.door().key(keys::LEFTALT, true).unwrap();
        session.door().button("right", true).unwrap();
        session.door().barrier().unwrap();
        session.door().motion(corner.0 - 70.0, corner.1).unwrap();
        session.door().barrier().unwrap();
        assert_ne!(
            geometry(&session.world().unwrap(), "pointer-a"),
            geometry(&initial, "pointer-a")
        );
        session.door().tap_key(keys::ESC).unwrap();
        session.door().button("right", false).unwrap();
        session.door().key(keys::LEFTALT, false).unwrap();
        assert_eq!(
            geometry(&settled(&mut session), "pointer-a"),
            geometry(&initial, "pointer-a")
        );
    }
    dispatch(&mut session, "layout mosaic");
    dispatch(&mut session, "layout flow");
    overview_roundtrip(&mut session);
    assert!(!settled(&mut session).spatial.moving);
    dispatch(&mut session, "layout freeform");
    session.kill_clients();
    poll_until(
        Duration::from_secs(5),
        "destroyed clients clear layout animations",
        || {
            let world = session.world().ok()?;
            (world.windows.is_empty() && !world.spatial.moving).then_some(())
        },
    )
    .unwrap();
}

#[test]
#[ignore = "requires native nested Wayland"]
fn a_crashed_managed_desktop_restores_organization_and_original_freeform_geometry() {
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    let mut session = Session::boot(
        "spatial-before-crash",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    for title in ["restore-a", "restore-b", "restore-c"] {
        session
            .launch(probe.to_str().unwrap(), &[title, "foot"])
            .unwrap();
        session.wait_for_window(title).unwrap();
    }
    for (title, x, y) in [
        ("restore-a", 80, 100),
        ("restore-b", 600, 120),
        ("restore-c", 300, 380),
    ] {
        dispatch(
            &mut session,
            &format!("resizewindowpixel exact 430 280,title:{title}"),
        );
        dispatch(
            &mut session,
            &format!("movewindowpixel exact {x} {y},title:{title}"),
        );
    }
    let mut originals: Vec<_> = session
        .world()
        .unwrap()
        .windows
        .iter()
        .map(|w| (w.x, w.y, w.w, w.h))
        .collect();
    originals.sort();
    mode(&mut session, "mosaic");
    dispatch(&mut session, "focuswindow title:restore-a");
    dispatch(&mut session, "resizeactive 80 0");
    mode(&mut session, "flow");
    dispatch(&mut session, "resizeactive -90 0");
    dispatch(&mut session, "movewindow r");
    dispatch(&mut session, "focuswindow title:restore-b");
    dispatch(&mut session, "togglefloating");
    dispatch(&mut session, "moveactive 40 20");
    // An empty workspace's mode is part of the organization, too.
    dispatch(&mut session, "workspace 3");
    mode(&mut session, "mosaic");
    dispatch(&mut session, "workspace 1");
    settled(&mut session);
    let saved = poll_until(
        Duration::from_secs(8),
        "complete spatial session persisted",
        || {
            let text = std::fs::read_to_string(session.state_file("session")).ok()?;
            let records: Vec<_> = text
                .lines()
                .filter(|l| l.starts_with("foot\t") || l.starts_with("@window\t\"foot\"\t"))
                .collect();
            (records.len() == 3
                && records.iter().any(|l| l.contains("\"floating\":true"))
                && text.contains("@workspace\t2\tdwindle"))
            .then_some(text)
        },
    )
    .unwrap();
    session.kill_compositor();
    drop(session);
    let config = format!("show_dock = false\nrestore_session = true\nterminal = [\"{}\", \"restored-foot\", \"foot\"]\n",probe.display());
    let mut restored = Session::boot(
        "spatial-after-crash",
        SessionOptions {
            config_extra: config,
            state_files: vec![("session".into(), saved.clone())],
            ..Default::default()
        },
    )
    .unwrap();
    poll_until(
        Duration::from_secs(10),
        "all three applications restored",
        || {
            let world = restored.world().ok()?;
            (world.windows.len() == 3
                && world.windows.iter().all(|w| w.mapped && w.w > 0)
                && world.spatial.mode == "Flow"
                && !world.spatial.moving)
                .then_some(())
        },
    )
    .unwrap();
    let metadata = |text: &str| -> Vec<serde_json::Value> {
        let mut records: Vec<_> = text
            .lines()
            .filter(|l| l.starts_with("foot\t") || l.starts_with("@window\t\"foot\"\t"))
            .map(|l| {
                let mut value: serde_json::Value = serde_json::from_str(
                    l.split('\t')
                        .nth(if l.starts_with("@window\t") { 10 } else { 9 })
                        .unwrap(),
                )
                .unwrap();
                value.as_object_mut().unwrap().remove("focused");
                value
            })
            .collect();
        records.sort_by_key(|v| v["order"].as_u64());
        records
    };
    poll_until(
        Duration::from_secs(8),
        "restored organization written without drift",
        || {
            let text = std::fs::read_to_string(restored.state_file("session")).ok()?;
            (metadata(&text) == metadata(&saved)).then_some(())
        },
    )
    .unwrap();
    dispatch(&mut restored, "workspace 3");
    assert_eq!(settled(&mut restored).spatial.mode, "Mosaic");
    dispatch(&mut restored, "workspace 1");
    let freeform = mode(&mut restored, "freeform");
    let mut geometry: Vec<_> = freeform
        .windows
        .iter()
        .map(|w| (w.x, w.y, w.w, w.h))
        .collect();
    geometry.sort();
    assert_eq!(geometry, originals);
}

#[test]
#[ignore = "requires native nested Wayland"]
fn benchmark_real_reflow_focus_and_reversible_membership() {
    let mut session = Session::boot(
        "spatial-benchmark",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    for count in 1..=8 {
        let class = format!("benchmark-{count}");
        session
            .launch(probe.to_str().unwrap(), &[&class, &class])
            .unwrap();
        session.wait_for_window(&class).unwrap();
        if count != 4 && count != 8 {
            continue;
        }
        for style in ["mosaic", "flow"] {
            let before = session.world().unwrap().spatial;
            session.door().frame_stats().unwrap();
            mode(&mut session, style);
            let after = session.world().unwrap().spatial;
            let frames = session.door().frame_stats().unwrap();
            eprintln!("{count}-window {style}: calculations={} reflow_us={} transitions={} setup_us={} configures={} renders={} render_us={} render_max_us={}",
                after.calculations-before.calculations,after.calculation_us-before.calculation_us,after.setups-before.setups,after.setup_us-before.setup_us,
                after.configures-before.configures,frames.render_calls,frames.render_us,frames.render_max_us);
            assert_eq!(after.managed, count);
            assert!(after.configures - before.configures <= count as u64 * 2);
            eprintln!("layout scene: builds={} culled={} build_us={} (excludes GPU submission/presentation waits)",
                after.builds - before.builds, after.culled - before.culled, after.build_us - before.build_us);
            if count == 8 && style == "flow" {
                assert!(
                    after.culled > before.culled,
                    "offscreen Flow cells must skip element construction"
                );
            }
        }
    }
    if let Ok(before) = session.door().memory_statistics() {
        dispatch(&mut session, "layout mosaic");
        let during = session.door().memory_statistics().unwrap();
        std::thread::sleep(Duration::from_millis(120));
        let after = session.door().memory_statistics().unwrap();
        eprintln!("allocation-profile animation interval (120ms, includes renderer/client commits/test IPC): operations={} allocated_bytes={}; transition setup operations={}",
            after.get("rust_operations").unwrap()-during.get("rust_operations").unwrap(),
            after.get("rust_allocated_bytes").unwrap()-during.get("rust_allocated_bytes").unwrap(),
            during.get("rust_operations").unwrap()-before.get("rust_operations").unwrap());
        mode(&mut session, "flow");
    }
    let memory_before = session.door().memory_statistics().ok();
    let before = session.world().unwrap().spatial;
    for _ in 0..8 {
        for _ in 0..7 {
            dispatch(&mut session, "movefocus l");
        }
        for _ in 0..7 {
            dispatch(&mut session, "movefocus r");
        }
        dispatch(&mut session, "layout mosaic");
        dispatch(&mut session, "layout flow");
        dispatch(&mut session, "togglefloating");
        dispatch(&mut session, "togglefloating");
    }
    let after = settled(&mut session).spatial;
    eprintln!(
        "rapid focus/reflow: calculations={} reflow_us={} transitions={} setup_us={} configures={}",
        after.calculations - before.calculations,
        after.calculation_us - before.calculation_us,
        after.setups - before.setups,
        after.setup_us - before.setup_us,
        after.configures - before.configures
    );
    if let (Some(before), Ok(after)) = (memory_before, session.door().memory_statistics()) {
        eprintln!("allocation-profile complete operations (includes clients, chrome and test IPC): allocations={} allocated_bytes={} retained_bytes_before={} retained_bytes_after={}",
            after.get("rust_operations").unwrap()-before.get("rust_operations").unwrap(),after.get("rust_allocated_bytes").unwrap()-before.get("rust_allocated_bytes").unwrap(),
            before.get("rust_live_bytes").unwrap(),after.get("rust_live_bytes").unwrap());
    }
    assert_eq!(after.managed, 8);
    session.door().frame_stats().unwrap();
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(session.door().frame_stats().unwrap().render_calls, 0);
}

#[test]
#[ignore = "requires native nested Wayland; use memory-profile binary for allocation counters"]
fn steady_flow_rendering_profile_with_a_live_frame_paced_client() {
    let mut session = Session::boot(
        "spatial-steady-profile",
        SessionOptions {
            config_extra: "show_dock = false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    let probe = profile_binary("chonk-fullscreen-probe").unwrap();
    for i in 0..8 {
        let class = format!("steady-{i}");
        let animation = if i == 7 { "animate-frame" } else { "static" };
        session
            .launch(probe.to_str().unwrap(), &[&class, &class, animation])
            .unwrap();
        session.wait_for_window(&class).unwrap();
    }
    for style in ["mosaic", "flow", "mosaic", "flow"] {
        mode(&mut session, style);
        // Caption expiry is outside the measurement. The client, not test
        // screenshots or injected motion, supplies every measured live frame.
        std::thread::sleep(Duration::from_millis(1100));
        let before = session.world().unwrap().spatial;
        session.door().frame_stats().unwrap();
        let memory_before = session.door().memory_statistics().ok();
        std::thread::sleep(Duration::from_millis(500));
        let memory_after = session.door().memory_statistics().ok();
        let frames = session.door().frame_stats().unwrap();
        let after = session.world().unwrap().spatial;
        assert!(
            frames.render_calls >= 5,
            "the actual client must keep rendering"
        );
        assert_eq!(after.calculations, before.calculations);
        assert_eq!(after.configures, before.configures);
        eprintln!(
            "steady {style}: renders={} builds={} culled={} build_us={}",
            frames.render_calls,
            after.builds - before.builds,
            after.culled - before.culled,
            after.build_us - before.build_us
        );
        if let (Some(before), Some(after)) = (memory_before, memory_after) {
            let operations =
                after.get("rust_operations").unwrap() - before.get("rust_operations").unwrap();
            let bytes = after.get("rust_allocated_bytes").unwrap()
                - before.get("rust_allocated_bytes").unwrap();
            eprintln!("steady {style} allocator: operations={operations} bytes={bytes} operations_per_render={:.1} bytes_per_render={:.1} retained_before={} retained_after={}",
                operations as f64 / frames.render_calls as f64, bytes as f64 / frames.render_calls as f64,
                before.get("rust_live_bytes").unwrap(), after.get("rust_live_bytes").unwrap());
        }
    }
}
