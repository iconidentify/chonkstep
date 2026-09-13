//! End-to-end coverage for the exclusive-zone contract between a
//! layer-shell bar and the shell's own chrome: boot the real nested
//! compositor, run a real bar (`chonk-fake-bar`, this crate's own
//! wlr-layer-shell client) that claims an edge, and assert what the
//! desktop does about it — the Dock hangs itself under the bar, a
//! maximized window stops under the bar and short of the Dock, and
//! everything goes back the moment the bar exits. Then the same with
//! a right-edge panel, where the Dock steps *left* and the workarea
//! must widen to match, or windows would sit under the displaced
//! column.
//!
//! This is the seam Omarchy's shell lives on: its bar is a top layer
//! surface whose power button sits exactly where the Dock's identity
//! tile used to be. The unit tests in `chonk-shell::desktop` and
//! `wm-wayland::layers` pin the arithmetic; this test pins that the
//! arithmetic is what a real client on a real socket gets.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit -- --ignored --test-threads=1`.

use chonk_testkit::{
    keys, near, poll_until, profile_binary, session_dir, MenuMetrics, RootMenu, Session, SessionOptions, FAKE_BAR_RGB,
};
use std::time::Duration;

/// The bar's thickness in the nested output's physical pixels. Scale
/// 1, so buffer pixels, layer-shell pixels and ledger pixels agree.
const BAR: u32 = 48;

/// Runs the fake bar and waits until it reports itself mapped.
fn raise_bar(session: &mut Session, args: &[&str]) {
    let bar = profile_binary("chonk-fake-bar").expect("cargo build -p chonk-testkit builds the bar");
    let bar = bar.to_str().unwrap().to_string();
    session.launch(&bar, args).expect("the bar launches");
}

/// Toggles maximize on the focused window with the default binding.
/// Two modifiers, so the door's single-modifier `chord` does not fit.
fn toggle_maximize(session: &mut Session) {
    let door = session.door();
    door.key(keys::LEFTALT, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.barrier().unwrap();
    door.tap_key(keys::X).unwrap();
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTALT, false).unwrap();
    door.barrier().unwrap();
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn external_bars_are_the_only_reserved_chrome() {
    let mut session = Session::boot("layer-bar", SessionOptions { scale: Some(1.0), ..Default::default() }).unwrap();
    let world = session.world().unwrap();
    assert!(world.shells.is_empty(), "the core creates no dock or workspace surfaces");
    raise_bar(&mut session, &[&BAR.to_string()]);
    wait_for_client_mapped(&session);
    session.launch("foot", &["--config=/dev/null"]).unwrap();
    let window = session.wait_for_window("foot").unwrap();
    toggle_maximize(&mut session);
    let assert_frame = |session: &mut Session, x, y, w, h| {
        poll_until(Duration::from_secs(10), "window to follow the external reservation", || {
            let world = session.world().ok()?;
            world.frame_of(window.id).filter(|f| (f.x, f.y, f.w, f.h) == (x,y,w,h)).cloned()
        }).unwrap();
    };
    assert_frame(&mut session, 0, BAR as i32, world.output_w, world.output_h - BAR);
    session.kill_client("chonk-fake-bar");
    assert_frame(&mut session, 0, 0, world.output_w, world.output_h);
    let panel = 64;
    raise_bar(&mut session, &[&panel.to_string(), "right"]);
    assert_frame(&mut session, 0, 0, world.output_w - panel, world.output_h);
    session.kill_client("chonk-fake-bar");
    assert_frame(&mut session, 0, 0, world.output_w, world.output_h);
}

/// Waits for the fake bar to report itself mapped — the line it prints
/// after the roundtrip that follows its first buffer, by which time the
/// compositor has run the commit. Read the latest launch's log: prior
/// client evidence deliberately survives reaping and must not satisfy
/// readiness for a replacement surface that has not committed yet.
fn wait_for_client_mapped(session: &Session) {
    poll_until(Duration::from_secs(10), "the layer client to report itself mapped", || {
        session.client_log("chonk-fake-bar").contains("mapped ").then_some(())
    })
    .expect("the background surface should map like any other layer surface");
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn omarchys_background_surface_is_hosted_but_the_desk_stays_chonksteps() {
    // An empty Omarchy root, so the root menu's shape at the end is
    // exact: no menu definition means no Omarchy submenu, whatever
    // this machine has installed under /usr/share/omarchy.
    let no_omarchy = session_dir("layer-background-omarchy");
    let _ = std::fs::remove_dir_all(&no_omarchy);
    std::fs::create_dir_all(&no_omarchy).unwrap();
    let mut session = Session::boot(
        "layer-background",
        SessionOptions {
            scale: Some(1.0),
            env: vec![("OMARCHY_PATH".to_string(), no_omarchy.to_string_lossy().into_owned())],
            ..Default::default()
        },
    )
    .unwrap();
    let desk = session.screenshot("desk").unwrap().centre_rgb();
    assert!(!near(desk, FAKE_BAR_RGB), "the fixture colour must not be the wallpaper's own");

    // -- the control: a wallpaper daemon on the background layer shows --
    raise_bar(&mut session, &["background", "wallpaper"]);
    wait_for_client_mapped(&session);
    poll_until(Duration::from_secs(10), "the background-layer surface to paint the desk", || {
        let shot = session.screenshot("wallpaper-daemon").ok()?;
        near(shot.centre_rgb(), FAKE_BAR_RGB).then_some(())
    })
    .expect("a background-layer surface under any other namespace is drawn over the wallpaper");
    session.kill_client("chonk-fake-bar");
    poll_until(Duration::from_secs(10), "the desk to come back once the daemon exits", || {
        let shot = session.screenshot("daemon-gone").ok()?;
        near(shot.centre_rgb(), desk.map(|c| c.round() as u8)).then_some(())
    })
    .expect("the wallpaper returns when the surface goes");

    // -- Omarchy's plugin: same surface, its namespace, not shown ------
    raise_bar(&mut session, &["background", "omarchy-background"]);
    wait_for_client_mapped(&session);
    assert!(
        session.log().contains("declining Omarchy's background surface"),
        "the compositor names the surface it is declining"
    );
    // The client is healthy — configured, committed, mapped — yet the
    // desk is still chonkstep's wallpaper, and stays so.
    let shot = session.screenshot("omarchy-background").unwrap();
    assert!(
        near(shot.centre_rgb(), desk.map(|c| c.round() as u8)),
        "Omarchy's background surface must not paint over the desk: {:?}",
        shot.centre_rgb()
    );

    // -- the harness default, `omarchy_shell = false`, is what booted --
    // The verdict is logged once at startup by
    // `chonk_shell::shell::host_omarchy_shell`; polled rather than read
    // outright only so the assertion never races the first tick.
    poll_until(Duration::from_secs(10), "the shell to say it is not hosting Omarchy's shell", || {
        session.log().contains("not hosting Omarchy's shell").then_some(())
    })
    .expect("a session the harness boots by default declines to host Omarchy's shell, and says so");

    // -- and a right-click on the desk is still the root menu ----------
    // The row count is exact: no hosted shell means no `Omarchy Bar`
    // row, and the empty `OMARCHY_PATH` means no `Omarchy` submenu.
    let metrics = MenuMetrics::at_scale_1();
    let unhosted = RootMenu::default();
    assert_eq!(unhosted.row_of("Omarchy Bar"), None, "the bar toggle belongs only to a session hosting the shell");
    session
        .open_root_menu(&metrics, unhosted.row_count())
        .expect("a right-click on the desk must reach the root menu, not Omarchy's background surface");
    session.kill_client("chonk-fake-bar");
}
