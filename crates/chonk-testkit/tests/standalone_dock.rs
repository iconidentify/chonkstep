//! The restored dock is a client. These tests exercise the process boundary,
//! its layer reservation, popup lifecycle, and the absent-dock default.
use chonk_testkit::{poll_until, profile_binary, Session, SessionOptions};
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(12);

fn launch(session: &mut Session, args: &[&str]) {
    let binary = profile_binary("chonk-dock").expect("build with cargo build -p chonk-dock");
    // The fixture is a sibling of the compositor. Do not inherit a host
    // desktop's control socket when connecting to the fixture's display.
    let wrapper = session.dir.join("dock-run");
    std::fs::write(&wrapper, format!("#!/bin/sh\nunset CHONKSTEP_CONTROL_SOCKET\nexport XDG_DATA_DIRS=\"$XDG_DATA_HOME\"\nexec '{}' \"$@\"\n", binary.display().to_string().replace('\'', "'\\''"))).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
    session
        .launch_isolated(wrapper.to_str().unwrap(), args)
        .unwrap();
}

#[test]
#[ignore = "cargo build -p chonk-dock; scripts/e2e.sh --headless --test standalone_dock"]
fn dock_is_opt_in_and_stopping_releases_every_surface() {
    let mut session = Session::boot(
        "standalone-dock",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "desktop='omarchy'\nhyprland_config=false\ntheme='nextstep-classic'\n"
                .into(),
            ..Default::default()
        },
    )
    .unwrap();
    let world = session.world().unwrap();
    assert!(
        world.shells.is_empty(),
        "Omarchy does not construct dock surfaces"
    );
    let x = world.output_w as i32 - 28;
    assert_eq!(session.door().hit(x, 28).unwrap(), "root");
    launch(&mut session, &["--theme", "nextstep-classic"]);
    poll_until(WAIT, "standalone dock maps", || {
        (session.door().hit(x, 28).ok()? == "layer").then_some(())
    })
    .unwrap();
    assert!(
        session.world().unwrap().shells.is_empty(),
        "dock belongs to a Wayland client, not compositor shell surfaces"
    );
    session.screenshot("standalone-dock").unwrap();
    // Reopening a panel must create a valid new popup role, and dismissing it
    // must return input to the desktop every time.
    for pass in 0..3 {
        // The Link instrument follows the identity, NET, CPU and sound tiles.
        session
            .door()
            .right_click(x as f64, (56 * 4 + 28) as f64)
            .unwrap();
        poll_until(WAIT, "instrument panel maps", || {
            (session.door().hit(x - 100, 56 * 4 + 28).ok()? != "root").then_some(())
        })
        .unwrap();
        session.screenshot("instrument-panel").unwrap();
        if pass == 1 {
            session.door().click(400.0, 650.0).unwrap();
        } else {
            session.door().tap_key(1).unwrap();
        }
        poll_until(
            WAIT,
            "Escape and outside clicks dismiss instrument panels",
            || (session.door().hit(x - 100, 56 * 4 + 28).ok()? == "root").then_some(()),
        )
        .unwrap();
    }
    launch(&mut session, &["--stop"]);
    poll_until(WAIT, "dock process exits and releases input", || {
        (session.door().hit(x, 28).ok()? == "root").then_some(())
    })
    .unwrap();
    assert!(session.compositor_alive());
    launch(&mut session, &["--theme", "nextstep-classic"]);
    poll_until(WAIT, "a new process can reclaim the dock sockets", || {
        (session.door().hit(x, 28).ok()? == "layer").then_some(())
    })
    .unwrap();
    launch(&mut session, &["--stop"]);
    poll_until(WAIT, "second dock stops", || {
        (session.door().hit(x, 28).ok()? == "root").then_some(())
    })
    .unwrap();
}

#[test]
#[ignore = "cargo build -p chonk-dock; scripts/e2e.sh --headless --test standalone_dock"]
fn dock_reservation_is_owned_by_the_client_process() {
    let mut session = Session::boot(
        "standalone-dock-reservation",
        SessionOptions {
            scale: Some(1.0),
            config_extra: "hyprland_config=false\n[keybindings]\n'super+x'='toggle-maximize'\n"
                .into(),
            ..Default::default()
        },
    )
    .unwrap();
    session.launch("foot", &["--config=/dev/null"]).unwrap();
    let window = session.wait_for_window("foot").unwrap();
    session
        .door()
        .key(chonk_testkit::keys::LEFTMETA, true)
        .unwrap();
    session.door().tap_key(chonk_testkit::keys::X).unwrap();
    session
        .door()
        .key(chonk_testkit::keys::LEFTMETA, false)
        .unwrap();
    let width = session.world().unwrap().output_w;
    poll_until(WAIT, "window maximizes", || {
        let world = session.world().ok()?;
        (world.frame_of(window.id)?.w == width).then_some(())
    })
    .unwrap();
    launch(&mut session, &[]);
    poll_until(WAIT, "client's exclusive zone reserves dock column", || {
        let world = session.world().ok()?;
        (world.frame_of(window.id)?.w == width - 56).then_some(())
    })
    .unwrap();
    launch(&mut session, &["--stop"]);
    poll_until(WAIT, "exiting client returns column", || {
        let world = session.world().ok()?;
        (world.frame_of(window.id)?.w == width).then_some(())
    })
    .unwrap();
}

#[test]
#[ignore = "cargo build -p chonk-dock -p chonk-dockclock; scripts/e2e.sh --headless --test standalone_dock"]
fn fractional_scale_and_live_resize_keep_the_clip_on_the_output() {
    let mut session = Session::boot(
        "standalone-dock-scale",
        SessionOptions {
            scale: Some(1.5),
            config_extra: "hyprland_config=false\n".into(),
            ..Default::default()
        },
    )
    .unwrap();
    launch(&mut session, &[]);
    for scale in [1.5, 2.0, 3.0, 1.0] {
        session.door().set_primary_scale(scale).unwrap();
        let world = session.world().unwrap();
        let x = world.output_w as i32 - 20;
        let y = world.output_h as i32 - 20;
        poll_until(WAIT, "Clip follows logical output size and scale", || {
            (session.door().hit(x, y).ok()? == "layer").then_some(())
        })
        .unwrap();
        session.screenshot("dock-scale").unwrap();
    }
    launch(&mut session, &["--stop"]);
}

#[test]
#[ignore = "cargo build -p chonk-dock -p chonk-dockclock; scripts/e2e.sh --headless --test standalone_dock"]
fn hosted_dockapp_exits_with_the_dock() {
    let mut session = Session::boot(
        "standalone-dock-child",
        SessionOptions {
            scale: Some(1.0),
            ..Default::default()
        },
    )
    .unwrap();
    let root = session.dir.join("client-config/chonkstep/dockapps");
    std::fs::create_dir_all(&root).unwrap();
    let clock = profile_binary("chonk-dockclock").expect("build the reference dockapp");
    let pidfile = session.dir.join("clock.pid");
    let child = session.dir.join("clock-child");
    std::fs::write(
        &child,
        format!(
            "#!/bin/sh\necho $$ > '{}'\nexec '{}'\n",
            pidfile.display(),
            clock.display()
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        root.join("clock.dockapp"),
        format!(
            "id='chonk-dockclock'\nname='CLK'\nexec=['{}']\ntile_units=1\nrestart='on-crash'\n",
            child.display()
        ),
    )
    .unwrap();
    launch(&mut session, &[]);
    let pid: u32 = poll_until(WAIT, "dock starts registered child", || {
        std::fs::read_to_string(&pidfile).ok()?.trim().parse().ok()
    })
    .unwrap();
    let x = session.world().unwrap().output_w as i32 - 28;
    poll_until(WAIT, "registered child tile maps", || {
        (session.door().hit(x, 56 * 8 + 28).ok()? == "layer").then_some(())
    })
    .unwrap();
    launch(&mut session, &["--stop"]);
    poll_until(WAIT, "stopped dock takes child down", || {
        (!std::path::Path::new(&format!("/proc/{pid}")).exists()).then_some(())
    })
    .unwrap();
    assert!(session.compositor_alive());
}
