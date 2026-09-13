//! Hyprland IPC uses the same logical output coordinates as Wayland clients.
use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use serde_json::{json, Value};
use std::{io::{Read, Write}, os::unix::net::UnixStream, path::PathBuf, time::Duration};

const WAIT: Duration = Duration::from_secs(10);

fn request(session: &Session, payload: &str) -> String {
    let socket = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("hypr").join(session.hyprland_signature().unwrap()).join(".socket.sock");
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.set_read_timeout(Some(WAIT)).unwrap();
    stream.write_all(payload.as_bytes()).unwrap();
    let mut reply = String::new();
    stream.take(1024 * 1024).read_to_string(&mut reply).unwrap();
    reply
}

fn query(session: &Session, name: &str) -> Value {
    let reply = request(session, &format!("j/{name}"));
    serde_json::from_str(&reply).unwrap_or_else(|error| panic!("{name}: {reply:?}: {error}"))
}

fn dispatch(session: &mut Session, payload: &str) {
    assert_eq!(request(session, &format!("/dispatch {payload}")), "ok");
    session.door().barrier().unwrap();
}

#[test]
#[ignore = "needs nested Wayland"]
fn scaled_cursor_windows_and_geometry_dispatch_share_logical_coordinates() {
    for scale in [1.0, 1.5, 2.0] {
        let mut session = Session::boot(&format!("hypr-coordinates-{scale}"), SessionOptions {
            scale: Some(scale),
            config_extra: "show_dock=false\nomarchy_menu=false\nhyprland_config=false\n".into(),
            ..Default::default()
        }).unwrap();
        session.door().set_virtual_outputs("aligned").unwrap();
        let world = session.world().unwrap();
        let physical = (world.output_w - 1, world.output_h - 1);
        session.door().motion(physical.0 as f64, physical.1 as f64).unwrap();
        session.door().barrier().unwrap();
        let expected = ((physical.0 as f64 / scale as f64).floor() as i32,
            (physical.1 as f64 / scale as f64).floor() as i32);
        assert_eq!(query(&session, "cursorpos"), json!({"x": expected.0, "y": expected.1}));
        assert_eq!(request(&session, "/cursorpos"), format!("{}, {}", expected.0, expected.1));
        let monitors = query(&session, "monitors");
        assert_eq!(monitors[0]["width"], world.output_w);
        assert_eq!(monitors[0]["height"], world.output_h);
        assert_eq!(monitors[0]["scale"], scale);

        let binary = profile_binary("chonk-fullscreen-probe").unwrap();
        session.launch_isolated(binary.to_str().unwrap(), &["coordinate-probe", "coordinate-probe"]).unwrap();
        session.wait_for_window("coordinate-probe").unwrap();
        dispatch(&mut session, "resizewindowpixel exact 240 180,title:^coordinate-probe$");
        dispatch(&mut session, "movewindowpixel exact 100 120,title:^coordinate-probe$");
        poll_until(WAIT, "logical geometry reaches the client's physical buffer", || {
            let world = session.world().ok()?;
            let window = world.window_matching("coordinate-probe")?;
            (window.x == (100.0 * scale) as i32 && window.y == (120.0 * scale) as i32
                && window.w == (240.0 * scale) as u32 && window.h == (180.0 * scale) as u32
                && window.w == window.presented_w && window.h == window.presented_h).then_some(())
        }).unwrap();
        let client = query(&session, "clients")[0].clone();
        assert_eq!(client["at"], json!([100, 120]));
        assert_eq!(client["size"], json!([240, 180]));
        assert_eq!(query(&session, "activewindow")["at"], client["at"]);
        assert!(request(&session, "/clients").contains("\tat: 100,120\n\tsize: 240,180\n"));
        session.door().motion(200.0 * scale as f64, 200.0 * scale as f64).unwrap();
        session.door().barrier().unwrap();
        assert_eq!(query(&session, "cursorpos"), json!({"x": 200, "y": 200}));
        dispatch(&mut session, "moveactive 20 10");
        assert_eq!(request(&session,
            "eval hl.dispatch(hl.dsp.window.resize({ x = 20, y = 10, relative = true }))"), "ok");
        poll_until(WAIT, "relative logical geometry is applied", || {
            let client = query(&session, "activewindow");
            (client["at"] == json!([120, 130]) && client["size"] == json!([260, 190])).then_some(())
        }).unwrap();
        // Reading and writing the reported dimensions must be an identity.
        dispatch(&mut session, "resizeactive exact 260 190");
        assert_eq!(query(&session, "activewindow")["size"], json!([260, 190]));
    }
}

#[test]
#[ignore = "needs nested Wayland"]
fn mixed_scales_and_offset_origins_match_xdg_output_and_cross_output_moves() {
    assert!(chonk_testkit::require_client("wayland-info"), "wayland-info is needed to verify the advertised geometry");
    let mut session = Session::boot("hypr-coordinates-mixed", SessionOptions {
        scale: Some(2.0),
        config_extra: "show_dock=false\nomarchy_menu=false\nhyprland_config=false\n".into(),
        ..Default::default()
    }).unwrap();
    session.door().virtual_outputs(true).unwrap();
    session.launch("wlr-randr", &["--output", "chonkstep", "--scale", "2", "--pos", "-640,-120",
        "--output", "chonkstep-right", "--scale", "1.5", "--pos", "0,0"]).unwrap();
    poll_until(WAIT, "mixed output layout is applied", || {
        session.client_status("wlr-randr").ok()?.map(|status| assert!(status.success(), "{}", session.client_log("wlr-randr")))
    }).unwrap();
    session.door().barrier().unwrap();
    let monitors = query(&session, "monitors");
    // Output management normalizes negative layouts. Reuse the advertised
    // origins: the second output is offset in both axes, with a different scale.
    assert_eq!((monitors[0]["x"].clone(), monitors[0]["y"].clone()), (json!(0), json!(0)));
    assert_eq!(monitors[0]["scale"], 2.0);
    assert_eq!(monitors[1]["scale"], 1.5);
    for (physical, logical) in [((1, 1), (0, 0)), ((639, 799), (319, 399)), ((1279, 919), (1066, 652))] {
        session.door().motion(physical.0 as f64, physical.1 as f64).unwrap();
        session.door().barrier().unwrap();
        assert_eq!(query(&session, "cursorpos"), json!({"x": logical.0, "y": logical.1}));
    }
    session.launch("wayland-info", &[]).unwrap();
    poll_until(WAIT, "Wayland output metadata is read", || {
        session.client_status("wayland-info").ok()?.map(|status| assert!(status.success()))
    }).unwrap();
    let output_report = session.client_log("wayland-info");
    for expected in ["logical_x: 0, logical_y: 0", "logical_width: 320, logical_height: 400",
        "logical_x: 640, logical_y: 120", "logical_width: 427, logical_height: 533"] {
        assert!(output_report.contains(expected), "missing {expected}\n{output_report}");
    }

    let binary = profile_binary("chonk-fullscreen-probe").unwrap();
    session.launch_isolated(binary.to_str().unwrap(), &["coordinate-probe", "coordinate-probe"]).unwrap();
    session.wait_for_window("coordinate-probe").unwrap();
    dispatch(&mut session, "resizeactive exact 240 180");
    for (logical, physical, size) in [((40, 60), (80, 120), (480, 360)), ((680, 180), (700, 210), (360, 270))] {
        dispatch(&mut session, &format!("moveactive exact {} {}", logical.0, logical.1));
        poll_until(WAIT, "cross-output move preserves the logical size", || {
            let world = session.world().ok()?;
            let window = world.window_matching("coordinate-probe")?;
            ((window.x, window.y) == physical && (window.w, window.h) == size
                && window.w == window.presented_w && window.h == window.presented_h).then_some(())
        }).unwrap_or_else(|error| panic!("{error}\n{:?}\n{}", session.world().unwrap().windows, session.log()));
        let client = query(&session, "activewindow");
        assert_eq!(client["at"], json!([logical.0, logical.1]));
        assert_eq!(client["size"], json!([240, 180]));
    }
}
