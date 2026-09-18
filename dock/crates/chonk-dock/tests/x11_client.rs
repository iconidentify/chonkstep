//! Uses a dedicated Xvfb from dock/check-x11.sh, never the user's desktop.
// Blocking waits here only reap isolated fixture processes on the test thread.
#![allow(clippy::disallowed_methods)]
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use x11rb::{
    connection::Connection,
    protocol::{xproto::*, xtest::ConnectionExt as _},
    rust_connection::RustConnection,
    wrapper::ConnectionExt as _,
    CURRENT_TIME, NONE,
};

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn until<T>(label: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(Instant::now() < deadline, "timed out: {label}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn atom(c: &RustConnection, name: &str) -> Atom {
    c.intern_atom(false, name.as_bytes())
        .unwrap()
        .reply()
        .unwrap()
        .atom
}
fn words(c: &RustConnection, w: Window, a: Atom) -> Vec<u32> {
    c.get_property(false, w, a, AtomEnum::ANY, 0, 4096)
        .ok()
        .and_then(|r| r.reply().ok())
        .and_then(|r| r.value32().map(|v| v.collect()))
        .unwrap_or_default()
}
fn dock_windows(c: &RustConnection, root: Window) -> Vec<Window> {
    c.query_tree(root)
        .unwrap()
        .reply()
        .unwrap()
        .children
        .into_iter()
        .filter(|w| {
            c.get_property(false, *w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 32)
                .ok()
                .and_then(|r| r.reply().ok())
                .is_some_and(|r| r.value == b"chonk-dock\0ChonkDock\0")
        })
        .collect()
}
fn mapped(c: &RustConnection, w: Window) -> bool {
    c.get_window_attributes(w)
        .ok()
        .and_then(|r| r.reply().ok())
        .is_some_and(|r| r.map_state == MapState::VIEWABLE)
}
fn click(c: &RustConnection, root: Window, x: i16, y: i16, button: u8) {
    c.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, CURRENT_TIME, root, x, y, 0)
        .unwrap();
    c.xtest_fake_input(BUTTON_PRESS_EVENT, button, CURRENT_TIME, root, 0, 0, 0)
        .unwrap();
    c.xtest_fake_input(BUTTON_RELEASE_EVENT, button, CURRENT_TIME, root, 0, 0, 0)
        .unwrap();
    c.flush().unwrap();
}
fn key(c: &RustConnection, root: Window, symbol: u32, pressed: bool) {
    let setup = c.setup();
    let reply = c
        .get_keyboard_mapping(setup.min_keycode, setup.max_keycode - setup.min_keycode + 1)
        .unwrap()
        .reply()
        .unwrap();
    let offset = reply
        .keysyms
        .chunks(reply.keysyms_per_keycode as usize)
        .position(|s| s.contains(&symbol))
        .unwrap();
    c.xtest_fake_input(
        if pressed {
            KEY_PRESS_EVENT
        } else {
            KEY_RELEASE_EVENT
        },
        setup.min_keycode + offset as u8,
        CURRENT_TIME,
        root,
        0,
        0,
        0,
    )
    .unwrap();
    c.flush().unwrap();
}
fn message(c: &RustConnection, root: Window, w: Window, name: &str, data: [u32; 5]) {
    c.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
        ClientMessageEvent::new(32, w, atom(c, name), data),
    )
    .unwrap();
    c.flush().unwrap();
}
fn command(args: &[&str]) {
    let output = Command::new(env!("CARGO_BIN_EXE_chonk-dock"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[ignore = "dock/check-x11.sh supplies the isolated X server and WM"]
fn x11_dock_lifecycle_input_launchers_scaling_and_reservations() {
    let scratch =
        PathBuf::from(std::env::var_os("CHONK_DOCK_X11_TEST").expect("use dock/check-x11.sh"));
    let wm_log = fs::File::create(scratch.join("wm.log")).unwrap();
    let mut wm = Process(
        Command::new(std::env::var_os("CHONKSTEP_X11_BIN").unwrap())
            .stdout(Stdio::null())
            .stderr(wm_log)
            .spawn()
            .unwrap(),
    );
    let (c, index) = RustConnection::connect(None).unwrap();
    let root = c.setup().roots[index].root;
    until("WM ready", || {
        (!words(&c, root, atom(&c, "_NET_SUPPORTING_WM_CHECK")).is_empty()).then_some(())
    });
    assert!(dock_windows(&c, root).is_empty(), "Omarchy starts no dock");
    let apps = PathBuf::from(std::env::var_os("XDG_DATA_HOME").unwrap()).join("applications");
    fs::create_dir_all(&apps).unwrap();
    let launch_script = scratch.join("launch-fixture");
    let launched = scratch.join("launched");
    fs::write(
        &launch_script,
        format!("#!/bin/sh\ntouch '{}'\n", launched.display()),
    )
    .unwrap();
    fs::set_permissions(&launch_script, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        apps.join("launch-fixture.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=Launch Fixture\nExec={}\n",
            launch_script.display()
        ),
    )
    .unwrap();
    fs::write(
        apps.join("fixture.desktop"),
        "[Desktop Entry]\nType=Application\nName=Fixture\nExec=true\nStartupWMClass=DockFixture\n",
    )
    .unwrap();
    let state = PathBuf::from(std::env::var_os("XDG_STATE_HOME").unwrap()).join("chonkstep");
    fs::create_dir_all(&state).unwrap();
    fs::write(state.join("dock"), "launch-fixture\nfixture\n").unwrap();

    let client = c.generate_id().unwrap();
    c.create_window(
        0,
        client,
        root,
        100,
        100,
        500,
        400,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new().background_pixel(0x224466),
    )
    .unwrap();
    c.change_property8(
        PropMode::REPLACE,
        client,
        AtomEnum::WM_CLASS,
        AtomEnum::STRING,
        b"fixture\0DockFixture\0",
    )
    .unwrap();
    c.change_property8(
        PropMode::REPLACE,
        client,
        AtomEnum::WM_NAME,
        AtomEnum::STRING,
        b"Dock fixture",
    )
    .unwrap();
    c.map_window(client).unwrap();
    c.flush().unwrap();
    let frame = until("client framed", || {
        let parent = c.query_tree(client).ok()?.reply().ok()?.parent;
        (parent != root).then_some(parent)
    });
    message(
        &c,
        root,
        client,
        "_NET_WM_STATE",
        [
            1,
            atom(&c, "_NET_WM_STATE_MAXIMIZED_HORZ"),
            atom(&c, "_NET_WM_STATE_MAXIMIZED_VERT"),
            2,
            0,
        ],
    );
    until("window maximized without dock", || {
        (c.get_geometry(frame).ok()?.reply().ok()?.width == 1280).then_some(())
    });

    // A hosted child must exit with the dock on X11 too.
    let config =
        PathBuf::from(std::env::var_os("XDG_CONFIG_HOME").unwrap()).join("chonkstep/dockapps");
    fs::create_dir_all(&config).unwrap();
    let child = scratch.join("clock-child");
    let pidfile = scratch.join("clock.pid");
    fs::write(
        &child,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec '{}'\n",
            pidfile.display(),
            PathBuf::from(std::env::var_os("CHONK_DOCKCLOCK_BIN").unwrap()).display()
        ),
    )
    .unwrap();
    fs::set_permissions(&child, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(
        config.join("clock.dockapp"),
        format!("id='chonk-dockclock'\nexec=['{}']\n", child.display()),
    )
    .unwrap();
    let log = fs::File::create(scratch.join("dock.log")).unwrap();
    // Explicit X11 selection must also work when a parent exported Wayland.
    let mut dock = Process(
        Command::new(env!("CARGO_BIN_EXE_chonk-dock"))
            .args(["--x11", "--theme", "nextstep-classic"])
            .env("WAYLAND_DISPLAY", "not-this-display")
            .env("CHONKSTEP_CONTROL_SOCKET", scratch.join("no-control.sock"))
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    let dock_window = until("dock maps", || {
        dock_windows(&c, root).into_iter().find(|w| {
            mapped(&c, *w) && words(&c, *w, atom(&c, "_NET_WM_STRUT_PARTIAL")).get(1) == Some(&56)
        })
    });
    let child_pid: u32 = until("hosted clock starts", || {
        fs::read_to_string(&pidfile).ok()?.trim().parse().ok()
    });
    until("external strut resizes maximized window", || {
        (c.get_geometry(frame).ok()?.reply().ok()?.width == 1224).then_some(())
    });
    assert_eq!(words(&c, root, atom(&c, "_NET_WORKAREA"))[2], 1224);
    command(&["--x11", "--status"]);
    command(&["--x11"]); // second start finds the same owner
    click(&c, root, 28, 28, 1);
    until("launcher executes application", || {
        launched.exists().then_some(())
    });
    let other = c.generate_id().unwrap();
    c.create_window(
        0,
        other,
        root,
        600,
        500,
        100,
        100,
        0,
        WindowClass::INPUT_OUTPUT,
        0,
        &CreateWindowAux::new(),
    )
    .unwrap();
    c.map_window(other).unwrap();
    c.flush().unwrap();
    until("other window focused", || {
        (words(&c, root, atom(&c, "_NET_ACTIVE_WINDOW")) == vec![other]).then_some(())
    });
    click(&c, root, 28, 84, 1);
    until("launcher focuses running application", || {
        (words(&c, root, atom(&c, "_NET_ACTIVE_WINDOW")) == vec![client]).then_some(())
    });
    c.destroy_window(other).unwrap();
    c.flush().unwrap();

    let panel_count = || {
        dock_windows(&c, root)
            .into_iter()
            .filter(|w| {
                mapped(&c, *w)
                    && words(&c, *w, atom(&c, "_NET_WM_WINDOW_TYPE"))
                        .contains(&atom(&c, "_NET_WM_WINDOW_TYPE_POPUP_MENU"))
            })
            .count()
    };
    for pass in 0..3 {
        click(&c, root, 1252, 56 * 4 + 28, 3);
        until("panel opens", || (panel_count() == 1).then_some(()));
        if pass == 1 {
            click(&c, root, 400, 700, 1);
        } else {
            key(&c, root, 0xff1b, true);
            key(&c, root, 0xff1b, false);
        }
        until("panel dismisses", || (panel_count() == 0).then_some(()));
    }
    // No pointer or keyboard grab survives dismissal.
    assert_eq!(
        c.grab_pointer(
            false,
            root,
            EventMask::BUTTON_PRESS,
            GrabMode::ASYNC,
            GrabMode::ASYNC,
            NONE,
            NONE,
            CURRENT_TIME
        )
        .unwrap()
        .reply()
        .unwrap()
        .status,
        GrabStatus::SUCCESS
    );
    c.ungrab_pointer(CURRENT_TIME).unwrap();
    assert_eq!(
        c.grab_keyboard(false, root, CURRENT_TIME, GrabMode::ASYNC, GrabMode::ASYNC)
            .unwrap()
            .reply()
            .unwrap()
            .status,
        GrabStatus::SUCCESS
    );
    c.ungrab_keyboard(CURRENT_TIME).unwrap();
    c.flush().unwrap();

    // DPI changes repaint and resize the live dock and its reservation.
    for (dpi, width) in [(144, 84), (192, 112), (96, 56)] {
        c.change_property8(
            PropMode::REPLACE,
            root,
            atom(&c, "RESOURCE_MANAGER"),
            AtomEnum::STRING,
            format!("Xft.dpi: {dpi}\n").as_bytes(),
        )
        .unwrap();
        c.flush().unwrap();
        until("DPI resizes dock", || {
            (c.get_geometry(dock_window).ok()?.reply().ok()?.width == width).then_some(())
        });
        until("DPI resizes workarea", || {
            (c.get_geometry(frame).ok()?.reply().ok()?.width == 1280 - width).then_some(())
        });
    }
    let pixels = c
        .get_image(ImageFormat::Z_PIXMAP, dock_window, 0, 0, 56, 56, u32::MAX)
        .unwrap()
        .reply()
        .unwrap()
        .data;
    assert!(
        pixels
            .as_chunks::<4>()
            .0
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            > 16,
        "dock artwork was painted"
    );

    // Use the WM's real miniaturize key, then the client's restore tile.
    // ChonkStep creates workspaces on demand; establish a second desktop.
    message(&c, root, root, "_NET_CURRENT_DESKTOP", [1, 0, 0, 0, 0]);
    until("second workspace exists", || {
        (words(&c, root, atom(&c, "_NET_CURRENT_DESKTOP")) == vec![1]).then_some(())
    });
    message(&c, root, root, "_NET_CURRENT_DESKTOP", [0, 0, 0, 0, 0]);
    until("first workspace selected", || {
        (words(&c, root, atom(&c, "_NET_CURRENT_DESKTOP")) == vec![0]).then_some(())
    });
    click(&c, root, 1278, 746, 1);
    until("Clip advances using EWMH", || {
        (words(&c, root, atom(&c, "_NET_CURRENT_DESKTOP")) == vec![1]).then_some(())
    });
    click(&c, root, 1226, 798, 1);
    until("Clip rewinds using EWMH", || {
        (words(&c, root, atom(&c, "_NET_CURRENT_DESKTOP")) == vec![0]).then_some(())
    });
    message(&c, root, client, "_NET_ACTIVE_WINDOW", [2, 0, 0, 0, 0]);
    until("fixture focused", || {
        (words(&c, root, atom(&c, "_NET_ACTIVE_WINDOW")) == vec![client]).then_some(())
    });
    key(&c, root, 0xffeb, true);
    key(&c, root, 0x6d, true);
    key(&c, root, 0x6d, false);
    key(&c, root, 0xffeb, false);
    until("minimized tile maps", || {
        dock_windows(&c, root).into_iter().find(|w| {
            c.get_geometry(*w)
                .ok()
                .and_then(|r| r.reply().ok())
                .is_some_and(|r| r.x == 0 && r.y == 744 && mapped(&c, *w))
        })
    });
    click(&c, root, 28, 772, 1);
    until("tile restores window", || mapped(&c, client).then_some(()));

    command(&["--x11", "--stop"]);
    until("dock process exits", || dock.0.try_wait().unwrap());
    until("dock surfaces released", || {
        dock_windows(&c, root).is_empty().then_some(())
    });
    until("hosted process exits", || {
        (!PathBuf::from(format!("/proc/{child_pid}")).exists()).then_some(())
    });
    until("reservation released", || {
        (c.get_geometry(frame).ok()?.reply().ok()?.width == 1280).then_some(())
    });
    assert!(wm.0.try_wait().unwrap().is_none());

    // Automatic backend selection and toggle both address this X11 display.
    let log = fs::File::create(scratch.join("restart.log")).unwrap();
    let mut restarted = Process(
        Command::new(env!("CARGO_BIN_EXE_chonk-dock"))
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .unwrap(),
    );
    until("dock restarts", || {
        (!dock_windows(&c, root).is_empty()).then_some(())
    });
    command(&["--toggle"]);
    until("toggle exits dock", || restarted.0.try_wait().unwrap());
}
