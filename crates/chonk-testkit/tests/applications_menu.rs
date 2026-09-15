//! End-to-end coverage for the root menu's `Applications` submenu
//! following the session: boot the real nested compositor with a
//! private `XDG_DATA_HOME` (and an empty `XDG_DATA_DIRS`, so the
//! machine's own installed applications stay out of the count), open
//! the root menu through the injection door with a real right-click,
//! cascade into `Applications` and count its rows. Then write a
//! `.desktop` file into that data home — exactly what Omarchy's
//! web-app installer and `pacman` do — wait for the shell to say it
//! rescanned, and see the row appear. Picking it proves "appears" is
//! also "launches": the entry's `Exec` is `touch <marker>`, so the
//! launch is a file appearing on disk. Deleting the entry takes the
//! row away again, all without a restart or a reload.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit --test applications_menu -- --ignored`.

use chonk_testkit::{keys, poll_until, session_dir, strip_ansi, MenuMetrics, RootMenu, Session, SessionOptions, ShellInfo};
use std::path::PathBuf;
use std::time::Duration;

/// No Omarchy rows (the key is off, whatever the developer's machine
/// has installed), no bar: the desk's own tree, where `Applications`
/// is the second row.
const ROOT: RootMenu = RootMenu { omarchy_bar: false, omarchy_rows: &[] };

/// Boots with a private data home beside the session directory (not
/// inside it: `Session::boot` clears its own directory first) whose
/// `applications` directory starts empty. Returns the session, that
/// directory, and a marker directory the installed entry touches.
fn boot(name: &str) -> (Session, PathBuf, PathBuf) {
    let beside = session_dir(&format!("{name}-data"));
    let _ = std::fs::remove_dir_all(&beside);
    let data_home = beside.join("data");
    let applications = data_home.join("applications");
    let markers = beside.join("markers");
    let no_system_data = beside.join("empty");
    for dir in [&applications, &markers, &no_system_data] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let session = Session::boot(
        name,
        SessionOptions {
            config_extra: "omarchy_menu = false\nhyprland_config = false\nrestore_session = false\n".to_string(),
            env: vec![
                ("XDG_DATA_HOME".to_string(), data_home.to_string_lossy().into_owned()),
                ("XDG_DATA_DIRS".to_string(), no_system_data.to_string_lossy().into_owned()),
            ],
            ..SessionOptions::default()
        },
    )
    .unwrap();
    (session, applications, markers)
}

/// Clicks row `row` of `menu` and waits for a menu surface that was not
/// there before — the cascade it opened.
fn cascade_from(session: &mut Session, metrics: &MenuMetrics, menu: &ShellInfo, row: usize) -> ShellInfo {
    let known: Vec<u64> = session.world().unwrap().menus().iter().map(|m| m.id).collect();
    let (x, y) = metrics.row_center(menu, row);
    let door = session.door();
    door.click(x, y).unwrap();
    poll_until(Duration::from_secs(10), &format!("the cascade from row {row} to map"), || {
        let world = door.windows().ok()?;
        world.menus().into_iter().find(|m| !known.contains(&m.id))
    })
    .expect("clicking a submenu row should open its cascade")
}

/// Right-clicks the desk and cascades into `Applications`; the
/// returned surface is that cascade.
fn open_applications(session: &mut Session, metrics: &MenuMetrics) -> ShellInfo {
    let root = session
        .open_root_menu(metrics, ROOT.row_count())
        .expect("a right-click on the desktop should open the root menu");
    cascade_from(session, metrics, &root, ROOT.row_of("Applications").unwrap())
}

/// Escape closes the whole cascade; wait until nothing is mapped so
/// the next open starts from a clean desk.
fn dismiss(session: &mut Session) {
    session.door().tap_key(keys::ESC).expect("the test door should deliver Escape");
    let mut last = Vec::new();
    poll_until(Duration::from_secs(10), "Escape to unmap every menu surface", || {
        last = session.world().ok()?.menus();
        last.is_empty().then_some(())
    })
    .unwrap_or_else(|e| panic!("Escape closes every menu surface: {e}; still mapped: {last:?}"));
}

/// Waits for the shell to log that a rescanned index with `count`
/// entries was swapped in. The line is the observable that the walk
/// ran off-thread and landed; the menu assertions that follow are
/// what the user sees.
fn wait_for_rescan(session: &mut Session, count: usize) {
    let wanted = format!("count={count} ");
    poll_until(Duration::from_secs(30), &format!("the application index to rescan to {count} entries (log line)"), || {
        strip_ansi(&session.log())
            .lines()
            .any(|line| line.contains("application index rescanned") && line.contains(&wanted))
            .then_some(())
    })
    .unwrap_or_else(|e| panic!("{e}\n{}", session.log()));
}

/// The whole path a user takes after installing something: the row is
/// there on the next right-click, it launches, and uninstalling takes
/// it away — no restart, no reload.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit --test applications_menu -- --ignored"]
fn an_application_installed_during_the_session_appears_launches_and_leaves_with_its_file() {
    let (mut session, applications, markers) = boot("applications-menu-rescan");
    let metrics = MenuMetrics::at_scale_1();

    // -- an empty index: Applications is just About ----------------------
    let cascade = open_applications(&mut session, &metrics);
    assert_eq!(metrics.rows_in(cascade.h), Some(1), "an empty index lists About alone (surface {cascade:?})");
    dismiss(&mut session);

    // -- install during the session ---------------------------------------
    // A `Utility` entry lands in the Accessories cascade; its Exec is
    // the launch's own witness.
    let marker = markers.join("launched");
    std::fs::write(
        applications.join("e2e-installed.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=Installed During Session\nExec=touch {}\nCategories=Utility;\n",
            marker.display()
        ),
    )
    .unwrap();
    wait_for_rescan(&mut session, 1);

    // -- the next right-click shows it, one level down --------------------
    let cascade = open_applications(&mut session, &metrics);
    assert_eq!(
        metrics.rows_in(cascade.h),
        Some(2),
        "Applications now lists the Accessories cascade and About (surface {cascade:?})"
    );
    session.screenshot("applications-with-installed-entry").unwrap();
    let accessories = cascade_from(&mut session, &metrics, &cascade, 0);
    assert_eq!(metrics.rows_in(accessories.h), Some(1), "Accessories holds the one installed entry (surface {accessories:?})");

    // -- and picking it launches it ----------------------------------------
    let (x, y) = metrics.row_center(&accessories, 0);
    session.door().click(x, y).unwrap();
    poll_until(Duration::from_secs(15), "the installed entry's Exec to run (marker file)", || marker.exists().then_some(()))
        .unwrap_or_else(|e| panic!("{e}\n{}", session.log()));
    let door = session.door();
    let mut last = Vec::new();
    poll_until(Duration::from_secs(10), "every menu surface to unmap after the pick", || {
        let world = door.windows().ok()?;
        last = world.menus();
        last.is_empty().then_some(())
    })
    .unwrap_or_else(|e| panic!("an app pick closes the whole cascade: {e}; still mapped: {last:?}"));

    // -- uninstall: the row goes with the file -----------------------------
    std::fs::remove_file(applications.join("e2e-installed.desktop")).unwrap();
    wait_for_rescan(&mut session, 0);
    let cascade = open_applications(&mut session, &metrics);
    assert_eq!(metrics.rows_in(cascade.h), Some(1), "the removed entry's cascade is gone; About alone again (surface {cascade:?})");
    dismiss(&mut session);
}
