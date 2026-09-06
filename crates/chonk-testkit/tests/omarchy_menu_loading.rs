//! Menu discovery is boot work once, while an explicit reload still rereads.

use chonk_testkit::{poll_until, Session, SessionOptions};
use std::time::Duration;

fn loads(session: &Session) -> usize {
    session
        .log()
        .lines()
        .filter(|line| line.contains("omarchy menu loaded"))
        .count()
}

#[test]
#[ignore = "needs a nested Wayland session; scripts/e2e.sh --headless --release"]
fn startup_loads_the_menu_once_and_explicit_reload_reads_it_again() {
    let temporary = tempfile::tempdir().unwrap();
    let menu = temporary.path().join("default/omarchy/omarchy-menu.jsonc");
    std::fs::create_dir_all(menu.parent().unwrap()).unwrap();
    std::fs::write(&menu, r#"{"a":{"label":"A","action":"true"}}"#).unwrap();
    let mut session = Session::boot("omarchy-menu-loading", SessionOptions {
        config_extra: "omarchy_menu = true\nhyprland_config = false\nrestore_session = false\nshow_dock = false\n".into(),
        env: vec![("OMARCHY_PATH".into(), temporary.path().to_str().unwrap().into())],
        ..Default::default()
    }).unwrap();
    session.door().barrier().unwrap();
    assert_eq!(
        loads(&session),
        1,
        "startup should not parse the same menu twice\n{}",
        session.log()
    );

    // Leave the file unchanged so its independent mtime watcher cannot race
    // this assertion. Explicit reload must reread even with identical mtimes.
    session.request_reload().unwrap();
    poll_until(
        Duration::from_secs(10),
        "explicit configuration reload to reread the menu",
        || (loads(&session) >= 2).then_some(()),
    )
    .unwrap();
    session.door().barrier().unwrap();
    assert_eq!(
        loads(&session),
        2,
        "one reload should parse once\n{}",
        session.log()
    );
    assert!(session.compositor_alive());
}
