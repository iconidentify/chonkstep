//! End-to-end coverage for the root menu's `Omarchy` submenu: boot the
//! real nested compositor with `OMARCHY_PATH` pointed at a scratch tree
//! holding a menu definition of this test's own making, open the root
//! menu through the injection door with a real right-click, cascade
//! into `Omarchy`, pick a row, and watch the row's action land — the
//! action is `touch <marker>`, so "it ran" is a file appearing on
//! disk, observed with a bounded poll.
//!
//! The scratch definition exercises the condition model end to end: a
//! plain row, a row whose `when` holds (a flag file the test creates),
//! and a row whose `when` is `false`. The submenu must show exactly the
//! first two — which also proves the shell waited for the background
//! condition batch before building the menu rather than guessing.
//!
//! Same run rules as `e2e.rs`: needs a live Wayland session to nest
//! in, so `#[ignore]`d; run with `scripts/e2e.sh` or
//! `cargo test -p chonk-testkit --test omarchy_menu -- --ignored`.

use chonk_testkit::{keys, poll_until, session_dir, MenuMetrics, RootMenu, Session, SessionOptions, ShellInfo};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The root menu these sessions show: a menu definition is installed
/// (the scratch tree) and the shell is not hosted (the harness
/// default), so the Omarchy submenu is listed and the bar toggle is
/// not.
/// With a definition present, Omarchy's own top-level rows *are* the
/// root menu — the scratch tree's `Test` cascade and its `Marked`
/// action — with `Applications` and `Terminal` ahead of them and this
/// desk's `Dock` toggle and `Exit` after.
const WITH_OMARCHY: RootMenu = RootMenu { omarchy_bar: false, omarchy_rows: &["Test", "Marked"] };

/// Writes a scratch `OMARCHY_PATH` tree whose menu has three rows under
/// one `Omarchy` submenu — `Plain`, `Guarded` (shown, because the flag
/// file exists) and `Hidden` (`when: false`) — plus one top-level
/// `checked` row to give the marker gutter something to do. Returns
/// the tree's root; markers land beside it.
fn write_omarchy_tree(session_dir: &Path) -> PathBuf {
    let omarchy = session_dir.join("omarchy");
    let menu_dir = omarchy.join("default/omarchy");
    std::fs::create_dir_all(&menu_dir).unwrap();
    let markers = session_dir.join("markers");
    std::fs::create_dir_all(&markers).unwrap();
    std::fs::write(markers.join("flag"), "").unwrap();
    let definition = format!(
        r#"{{
  // A scratch menu for the e2e test, with the shipped file's shape.
  "test": {{"icon":"", "label":"Test"}},
  "test.plain": {{"icon":"", "label":"Plain", "action":"touch {m}/plain"}},
  "test.guarded": {{"icon":"", "label":"Guarded", "when":"[[ -e {m}/flag ]]", "action":"touch {m}/guarded"}},
  "test.hidden": {{"icon":"", "label":"Hidden", "when":"false", "action":"touch {m}/hidden"}}, // never shown
  "toggle": {{"icon":"", "label":"Marked", "checked":"true", "action":"touch {m}/marked"}},
}}
"#,
        m = markers.display()
    );
    std::fs::write(menu_dir.join("omarchy-menu.jsonc"), definition).unwrap();
    omarchy
}

fn boot(name: &str, config_extra: &str) -> (Session, PathBuf) {
    // The tree is written in a directory *beside* the session's, not
    // inside it: `Session::boot` clears its own directory first.
    let beside = session_dir(&format!("{name}-omarchy"));
    let _ = std::fs::remove_dir_all(&beside);
    std::fs::create_dir_all(&beside).unwrap();
    let omarchy = write_omarchy_tree(&beside);
    let session = Session::boot(
        name,
        SessionOptions {
            config_extra: config_extra.to_string(),
            env: vec![("OMARCHY_PATH".to_string(), omarchy.to_string_lossy().into_owned())],
            ..SessionOptions::default()
        },
    )
    .unwrap();
    (session, beside.join("markers"))
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

/// Waits for the shell's first completed condition batch, which is the
/// moment the `when`-guarded row can appear. The shell logs one line
/// per completed batch.
fn wait_for_conditions(session: &mut Session) {
    poll_until(Duration::from_secs(30), "the omarchy condition batch to complete (log line)", || {
        session.log().contains("omarchy menu conditions evaluated").then_some(())
    })
    .expect("the shell should evaluate the scratch menu's conditions");
}

/// The whole path a user takes: right-click, a row of Omarchy's own
/// menu, and the row's command running — with the condition model
/// visible in the row count along the way.
///
/// Omarchy's rows sit at the top level rather than behind a cascade,
/// because this desktop presents *as* Omarchy rather than hosting it.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit --test omarchy_menu -- --ignored"]
fn omarchys_rows_are_the_root_menu_and_a_picked_action_runs() {
    let (mut session, markers) = boot("omarchy-menu", "");
    let metrics = MenuMetrics::at_scale_1();
    poll_until(Duration::from_secs(30), "the scratch menu to load (log line)", || {
        session.log().contains("omarchy menu loaded").then_some(())
    })
    .expect("the shell should find the scratch OMARCHY_PATH");
    wait_for_conditions(&mut session);

    // -- the root menu is Omarchy's own -----------------------------------
    let root = session
        .open_root_menu(&metrics, WITH_OMARCHY.row_count())
        .expect("a right-click on the desktop should open the root menu carrying Omarchy's rows");
    session.screenshot("root-menu").unwrap();

    // -- `Test` cascades straight from the root, with no wrapper ---------
    // Its three children are one level in; exactly the two whose `when`
    // allows are listed.
    let test = cascade_from(&mut session, &metrics, &root, WITH_OMARCHY.row_of("Test").unwrap());
    assert_eq!(
        metrics.rows_in(test.h),
        Some(2),
        "Test should list Plain and Guarded and hide Hidden (`when: false`) (surface {test:?})"
    );
    session.screenshot("omarchy-test-submenu").unwrap();

    // -- picking Guarded runs `touch <markers>/guarded` -------------------
    let (x, y) = metrics.row_center(&test, 1);
    session.door().click(x, y).unwrap();
    poll_until(Duration::from_secs(15), "the Guarded row's marker file to appear", || markers.join("guarded").exists().then_some(()))
        .expect("the picked Omarchy action should run as `bash -lc touch ...`");
    assert!(!markers.join("plain").exists(), "only the picked row's action runs");
    assert!(!markers.join("hidden").exists(), "a hidden row's action can never run");

    // -- the pick dismissed the menu ---------------------------------------
    let door = session.door();
    let mut last = Vec::new();
    poll_until(Duration::from_secs(10), "every menu surface to unmap after the pick", || {
        let world = door.windows().ok()?;
        last = world.menus();
        last.is_empty().then_some(())
    })
    .unwrap_or_else(|e| panic!("an action pick closes the whole cascade: {e}; still mapped: {last:?}"));
}

/// Escape owns the whole open menu session, not merely its deepest
/// surface. Exercise a three-level cascade because closing just the root
/// (or just the leaf) can look correct in a unit ledger while leaving an
/// orphaned popup mapped by the real compositor.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit --test omarchy_menu -- --ignored"]
fn escape_closes_the_root_menu_and_every_open_submenu_without_running_an_action() {
    let (mut session, markers) = boot("omarchy-menu-escape", "");
    let metrics = MenuMetrics::at_scale_1();
    poll_until(Duration::from_secs(30), "the scratch menu to load (log line)", || {
        session.log().contains("omarchy menu loaded").then_some(())
    })
    .expect("the shell should find the scratch OMARCHY_PATH");
    wait_for_conditions(&mut session);

    let root = session
        .open_root_menu(&metrics, WITH_OMARCHY.row_count())
        .expect("a right-click should open the root menu");
    // `Test` opens straight from the root now, so the deepest stack is
    // two surfaces rather than three — Escape still has to close all of
    // them, which is what this test is about.
    let test = cascade_from(&mut session, &metrics, &root, WITH_OMARCHY.row_of("Test").unwrap());
    let _ = &test;
    assert_eq!(session.world().unwrap().menus().len(), 2, "the root and its cascade are mapped before Escape");

    session.door().tap_key(keys::ESC).expect("the test door should deliver Escape");
    let mut last = Vec::new();
    poll_until(Duration::from_secs(10), "Escape to unmap the complete menu cascade", || {
        last = session.world().ok()?.menus();
        last.is_empty().then_some(())
    })
    .unwrap_or_else(|e| panic!("Escape closes every menu surface: {e}; still mapped: {last:?}"));

    for action in ["plain", "guarded", "hidden", "marked"] {
        assert!(!markers.join(action).exists(), "Escape must not run the {action} action");
    }
}

/// The one config key: `omarchy_menu = false` leaves the root menu
/// exactly as it is without Omarchy, even with a definition installed.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit --test omarchy_menu -- --ignored"]
fn omarchy_menu_false_leaves_the_root_menu_without_the_submenu() {
    let (mut session, _markers) = boot("omarchy-menu-off", "omarchy_menu = false\n");
    let metrics = MenuMetrics::at_scale_1();
    // Nothing to wait for on the Omarchy side — the whole point — so
    // the boot's own readiness is the only gate. The row count is the
    // assertion: the menu that maps is the one without the submenu.
    session
        .open_root_menu(&metrics, RootMenu::default().row_count())
        .expect("a right-click on the desktop should open the root menu without an Omarchy row");
    // Only a negative on the log: with the key off, the shell's
    // `omarchy_menu_for` returns before it would look for — or say
    // anything about — a definition ("no Omarchy menu definition
    // installed" is logged only when the key is *on* and discovery
    // finds nothing), so there is no positive line to pin. Should the
    // shell grow one, assert it here instead.
    assert!(!session.log().contains("omarchy menu loaded"), "the key off means the definition is never read");
    session.screenshot("root-menu-without-omarchy").unwrap();
}
