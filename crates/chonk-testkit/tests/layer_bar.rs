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
fn the_dock_steps_under_a_bar_and_windows_maximize_between_them() {
    let mut session = Session::boot("layer-bar", SessionOptions { scale: Some(1.0), ..Default::default() }).unwrap();

    // -- the corner, unobstructed ----------------------------------------
    let world = session.world().unwrap();
    let home = world.dock().expect("the dock is in its corner on an empty desktop").clone();
    assert_eq!(home.y, 0, "with no bar the dock starts at the very top");

    // -- a top bar maps: the dock hangs itself under it -------------------
    raise_bar(&mut session, &[&BAR.to_string()]);
    let under = session.wait_for_dock_at(0, BAR as i32).expect("the dock should step out of the bar's reservation");
    assert_eq!((under.x, under.w), (home.x, home.w), "a top bar moves the column down, not sideways");
    assert_eq!(under.h, home.h, "a stack that fits below the bar keeps its height");

    // -- a maximized window stops under the bar and short of the dock -----
    session.launch("foot", &[]).unwrap();
    let window = session.wait_for_window("foot").unwrap();
    toggle_maximize(&mut session);
    let frame = poll_until(Duration::from_secs(10), "the frame to reach the workarea's top edge", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.y == BAR as i32).cloned()
    })
    .expect("a maximized window's frame should start exactly under the bar");
    assert_eq!(frame.x, 0, "the workarea still begins at the left edge");
    assert_eq!(frame.x + frame.w as i32, under.x, "and ends where the dock column begins");
    assert_eq!(frame.y + frame.h as i32, world.output_h as i32, "a top bar leaves the bottom edge alone");

    // -- the bar exits: the corner comes back, and so does the workarea ---
    session.kill_client("chonk-fake-bar");
    let back = session.wait_for_dock_at(0, 0).expect("the dock should return to its corner once the bar exits");
    assert_eq!((back.x, back.w, back.h), (home.x, home.w, home.h), "the column is exactly where it started");
    poll_until(Duration::from_secs(10), "the maximized frame to grow back to the top edge", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.y == 0).cloned()
    })
    .expect("with the bar gone the maximized window should take the strip back");

    // -- a right-edge panel: the dock steps left, the workarea follows ----
    // The bar is gone, so the panel is the only reservation; and the
    // window is still maximized, so its frame tracks the workarea live.
    let panel = 64u32;
    raise_bar(&mut session, &[&panel.to_string(), "right"]);
    let beside = session.wait_for_dock_at(panel, 0).expect("the dock should step left out of the panel's reservation");
    assert_eq!(beside.x, home.x - panel as i32, "the column steps left by exactly the panel's width");
    poll_until(Duration::from_secs(10), "the frame to stop short of the displaced dock", || {
        let world = session.world().ok()?;
        world.frame_of(window.id).filter(|f| f.x + f.w as i32 == beside.x).cloned()
    })
    .expect("a maximized window must end where the displaced column begins, not under it");

    // -- and back once more ------------------------------------------------
    session.kill_client("chonk-fake-bar");
    let back = session.wait_for_dock_at(0, 0).expect("the dock should return to its corner once the panel exits");
    assert_eq!(back.x, home.x);
}

/// Waits for the fake bar to report itself mapped — the line it prints
/// after the roundtrip that follows its first buffer, by which time the
/// compositor has run the commit. The log is `client-0-…`: the harness
/// numbers logs by the clients it is *currently* tracking, and
/// `kill_client` drops the previous bar, so each bar in turn is the
/// only one — `launch` truncates the file before this is called.
fn wait_for_client_mapped(session: &Session) {
    let log = session.dir.join("client-0-chonk-fake-bar.log");
    poll_until(Duration::from_secs(10), "the layer client to report itself mapped", || {
        std::fs::read_to_string(&log).ok().filter(|text| text.contains("mapped ")).map(|_| ())
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
