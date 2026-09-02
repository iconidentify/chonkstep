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

use chonk_testkit::{poll_until, Session, SessionOptions, ShellInfo, World};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the root menu is opened: well inside the desktop, clear of
/// the dock column on the right edge and the launcher strip under the
/// Clip.
const CLICK_AT: (f64, f64) = (400.0, 400.0);

/// The root menu's rows, in `root_menu_items` order, when the Omarchy
/// submenu is present.
const ROOT_ROWS_WITH_OMARCHY: &[&str] = &["Terminal", "Applications", "Theme", "Wallpaper", "Omarchy", "Exit"];

/// The menu's row geometry restated from `wm_theme::menu::render_menu`
/// at scale 1: the title strip is the titlebar's height (never shorter
/// than a row), rows are `menu.item_height` tall, and a `border.width`
/// outline wraps everything. Aiming with the same numbers the shell
/// hit-tests through is what keeps this test from encoding a magic
/// coordinate that breaks the day a pad changes.
struct MenuMetrics {
    border: u32,
    title_h: u32,
    item_h: u32,
}

impl MenuMetrics {
    fn at_scale_1() -> Self {
        let theme = wm_theme::default_theme::nextstep_classic();
        let item_h = (theme.menu.item_height as u32).max(4);
        Self {
            border: (theme.border.width as u32).max(1),
            title_h: (theme.titlebar.height as u32).max(item_h),
            item_h,
        }
    }

    /// The expected surface height of a menu with `rows` rows.
    fn height_for(&self, rows: usize) -> u32 {
        self.title_h + self.item_h * rows as u32 + self.border * 2
    }

    /// How many rows a menu surface of height `h` carries.
    fn rows_in(&self, h: u32) -> Option<usize> {
        let body = h.checked_sub(self.title_h + self.border * 2)?;
        (body % self.item_h == 0).then_some((body / self.item_h) as usize)
    }

    /// The centre of row `row` of the menu surface `menu`.
    fn row_center(&self, menu: &ShellInfo, row: usize) -> (f64, f64) {
        let y = menu.y as u32 + self.border + self.title_h + self.item_h * row as u32 + self.item_h / 2;
        (menu.x as f64 + menu.w as f64 / 2.0, y as f64)
    }
}

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
    // The tree is written where the session will be, before the
    // session is booted; `Session::boot` clears its directory first,
    // so the tree lives beside it rather than inside it.
    let session_dir = std::env::temp_dir().join("chonk-testkit").join(format!("{name}-omarchy"));
    let _ = std::fs::remove_dir_all(&session_dir);
    std::fs::create_dir_all(&session_dir).unwrap();
    let omarchy = write_omarchy_tree(&session_dir);
    let session = Session::boot(
        name,
        SessionOptions {
            config_extra: config_extra.to_string(),
            env: vec![("OMARCHY_PATH".to_string(), omarchy.to_string_lossy().into_owned())],
            ..SessionOptions::default()
        },
    )
    .unwrap();
    (session, session_dir.join("markers"))
}

/// Mapped menu surfaces: every mapped, raised shell that does not sit
/// flush against the right edge of the output — the dock column and
/// the launcher strip under it do, and they are the only other shells
/// the desktop keeps raised. A menu opened at `CLICK_AT` never does.
fn menus(world: &World) -> Vec<ShellInfo> {
    world
        .shells
        .iter()
        .filter(|s| s.mapped && s.above && s.x + (s.w as i32) < world.output_w as i32)
        .cloned()
        .collect()
}

/// Right-clicks the desktop and waits for the root menu to map.
fn open_root_menu(session: &mut Session, metrics: &MenuMetrics, expected_rows: usize) -> ShellInfo {
    let door = session.door();
    door.motion(CLICK_AT.0, CLICK_AT.1).unwrap();
    door.barrier().unwrap();
    door.button("right", true).unwrap();
    door.barrier().unwrap();
    door.button("right", false).unwrap();
    door.barrier().unwrap();
    let wanted = metrics.height_for(expected_rows);
    poll_until(Duration::from_secs(10), &format!("the root menu ({expected_rows} rows, {wanted}px tall) to map"), || {
        let world = door.windows().ok()?;
        menus(&world).into_iter().find(|m| m.h == wanted)
    })
    .expect("a right-click on the desktop should open the root menu")
}

/// Clicks row `row` of `menu` and waits for a menu surface that was not
/// there before — the cascade it opened.
fn cascade_from(session: &mut Session, metrics: &MenuMetrics, menu: &ShellInfo, row: usize) -> ShellInfo {
    let known: Vec<u64> = menus(&session.world().unwrap()).iter().map(|m| m.id).collect();
    let (x, y) = metrics.row_center(menu, row);
    let door = session.door();
    door.click(x, y).unwrap();
    poll_until(Duration::from_secs(10), &format!("the cascade from row {row} to map"), || {
        let world = door.windows().ok()?;
        menus(&world).into_iter().find(|m| !known.contains(&m.id))
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

/// The whole path a user takes: right-click, Omarchy, a row, and the
/// row's command running — with the condition model visible in the
/// row count along the way.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit --test omarchy_menu -- --ignored"]
fn omarchy_submenu_lists_the_conditioned_rows_and_runs_a_picked_action() {
    let (mut session, markers) = boot("omarchy-menu", "");
    let metrics = MenuMetrics::at_scale_1();
    poll_until(Duration::from_secs(30), "the scratch menu to load (log line)", || {
        session.log().contains("omarchy menu loaded").then_some(())
    })
    .expect("the shell should find the scratch OMARCHY_PATH");
    wait_for_conditions(&mut session);

    // -- root menu carries the Omarchy row --------------------------------
    let root = open_root_menu(&mut session, &metrics, ROOT_ROWS_WITH_OMARCHY.len());
    session.screenshot("root-menu").unwrap();

    // -- Omarchy cascades to the scratch definition's top level ----------
    // Two top-level rows: the `Test` submenu and the `Marked` action.
    // `Test`'s three children are one level further in.
    let omarchy_row = ROOT_ROWS_WITH_OMARCHY.iter().position(|r| *r == "Omarchy").unwrap();
    let omarchy = cascade_from(&mut session, &metrics, &root, omarchy_row);
    assert_eq!(metrics.rows_in(omarchy.h), Some(2), "Omarchy submenu should list Test and Marked (surface {omarchy:?})");
    session.screenshot("omarchy-submenu").unwrap();

    // -- Test cascades to exactly the two rows whose `when` allows -------
    let test = cascade_from(&mut session, &metrics, &omarchy, 0);
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
        last = menus(&world);
        last.is_empty().then_some(())
    })
    .unwrap_or_else(|e| panic!("an action pick closes the whole cascade: {e}; still mapped: {last:?}"));
}

/// The one config key: `omarchy_menu = false` leaves the root menu
/// exactly as it is without Omarchy, even with a definition installed.
#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit --test omarchy_menu -- --ignored"]
fn omarchy_menu_false_leaves_the_root_menu_without_the_submenu() {
    let (mut session, _markers) = boot("omarchy-menu-off", "omarchy_menu = false\n");
    let metrics = MenuMetrics::at_scale_1();
    // Nothing to wait for on the Omarchy side — the whole point — so
    // the boot's own readiness is the only gate.
    let root = open_root_menu(&mut session, &metrics, ROOT_ROWS_WITH_OMARCHY.len() - 1);
    assert_eq!(metrics.rows_in(root.h), Some(5));
    assert!(!session.log().contains("omarchy menu loaded"), "the key off means the definition is never read");
    session.screenshot("root-menu-without-omarchy").unwrap();
}
