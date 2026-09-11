//! Window activity must refresh an existing Overview without destroying a swipe.
use chonk_testkit::{keys, poll_until, Session, SessionOptions, World};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    time::Duration,
};

fn dispatch(s: &mut Session, command: &str) {
    let path = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").unwrap())
        .join("hypr")
        .join(s.hyprland_signature().unwrap())
        .join(".socket.sock");
    let mut stream = UnixStream::connect(path).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream
        .write_all(format!("/dispatch {command}").as_bytes())
        .unwrap();
    let mut response = String::new();
    stream
        .take(1024 * 1024)
        .read_to_string(&mut response)
        .unwrap();
    assert_eq!(response.trim(), "ok");
    s.door().barrier().unwrap();
}

fn settled(s: &mut Session) -> World {
    poll_until(Duration::from_secs(6), "Overview gesture settles", || {
        let world = s.world().ok()?;
        world.gesture.is_none().then_some(world)
    })
    .unwrap()
}

fn terminal(s: &mut Session, title: &str) -> u64 {
    s.launch(
        "foot",
        &[
            &format!("--title={title}"),
            "--window-size-pixels=300x180",
            "--override",
            "locked-title=yes",
        ],
    )
    .unwrap();
    poll_until(Duration::from_secs(30), "Overview test window", || {
        s.world().ok()?.window_matching(title).map(|w| w.id)
    })
    .unwrap()
}

#[test]
#[ignore = "requires nested Wayland"]
fn window_activity_preserves_opening_and_closing_overview_gestures() {
    for mode in ["desktop", "mac"] {
        let mut s = Session::boot(
            &format!("overview-refresh-{mode}"),
            SessionOptions {
                scale: Some(2.0),
                config_extra: format!("interaction_mode = '{mode}'\nhyprland_config = false\n"),
                ..Default::default()
            },
        )
        .unwrap();
        for space in 1..=3 {
            dispatch(&mut s, &format!("workspace {space}"));
            terminal(&mut s, &format!("Space{space}"));
        }
        for space in 1..=3 {
            dispatch(&mut s, &format!("workspace {space}"));
            settled(&mut s);
            let base = space * 10_000;
            s.door().swipe_begin_at(4, base).unwrap();
            s.door().swipe_update_at(0.0, -80.0, base + 80).unwrap();
            let before = s
                .world()
                .unwrap()
                .overview
                .expect("opening Overview")
                .progress;
            assert!(before > 0.0 && before < 1.0);
            dispatch(&mut s, "resizeactive exact 420 260");
            let after = s.world().unwrap();
            assert!(
                after.gesture.is_some(),
                "resize cancelled opening on {mode} Space {space}"
            );
            assert_eq!(
                after.overview.unwrap().progress,
                before,
                "refresh snapped progress"
            );
            let new = terminal(&mut s, &format!("Mapped{space}"));
            assert!(
                s.world().unwrap().gesture.is_some(),
                "map/focus cancelled opening"
            );
            s.door().swipe_update_at(0.0, -160.0, base + 160).unwrap();
            s.door().swipe_end_at(false, base + 200).unwrap();
            let open = settled(&mut s);
            assert_eq!(open.overview.unwrap().progress, 1.0);
            assert!(
                open.overview_windows.iter().any(|w| w.id == new),
                "mapped window is live in Overview"
            );
            s.door().swipe_begin_at(4, base + 1000).unwrap();
            s.door().swipe_update_at(0.0, 60.0, base + 1080).unwrap();
            let before = s.world().unwrap().overview.unwrap().progress;
            dispatch(&mut s, "resizeactive exact 460 280");
            assert_eq!(
                s.world().unwrap().overview.unwrap().progress,
                before,
                "closing refresh snapped progress"
            );
            s.door().swipe_update_at(0.0, 180.0, base + 1160).unwrap();
            s.door().swipe_end_at(false, base + 1200).unwrap();
            assert!(settled(&mut s).overview.is_none());
            s.door().tap_key(keys::ESC).unwrap();
        }
    }
}

#[test]
#[ignore = "requires nested Wayland"]
fn continuously_retitling_terminal_keeps_overview_under_the_fingers() {
    let mut s = Session::boot(
        "overview-retitling",
        SessionOptions {
            scale: Some(2.0),
            ..Default::default()
        },
    )
    .unwrap();
    s.launch(
        "foot",
        &[
            "--title=Busy",
            "--window-size-pixels=300x180",
            "sh",
            "-c",
            r#"i=0; while :; do i=$((i+1)); printf '\033]0;Busy %s\007' "$i"; sleep 0.03; done"#,
        ],
    )
    .unwrap();
    poll_until(Duration::from_secs(30), "retitling terminal", || {
        s.world().ok()?.window_matching("Busy ").map(|w| w.id)
    })
    .unwrap();
    for attempt in 0..3 {
        let base = 10_000 + attempt * 2000;
        s.door().swipe_begin_at(4, base).unwrap();
        let mut titles = std::collections::HashSet::new();
        for step in 1..=10 {
            s.door()
                .swipe_update_at(0.0, -24.0, base + step * 30)
                .unwrap();
            std::thread::sleep(Duration::from_millis(30));
            let world = s.world().unwrap();
            titles.insert(world.window_matching("Busy ").unwrap().title.clone());
            assert!(
                world.gesture.is_some() && world.overview.is_some(),
                "title change cancelled swipe"
            );
        }
        assert!(
            titles.len() >= 2,
            "the client really changed its title during the stroke"
        );
        s.door().swipe_end_at(false, base + 350).unwrap();
        assert_eq!(settled(&mut s).overview.unwrap().progress, 1.0);
        s.door().tap_key(keys::ESC).unwrap();
        assert!(settled(&mut s).overview.is_none());
    }
}
