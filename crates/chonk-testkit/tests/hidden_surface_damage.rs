//! Invisible Wayland clients cannot schedule visible work.
//!
//! A frame-callback-driven application naturally pauses when its
//! workspace is parked, so it cannot expose an over-eager commit
//! handler. This test uses the fullscreen probe's self-timed mode:
//! the client keeps issuing real `wl_surface.commit` requests at about
//! 60 Hz and reports its own progress. Damage telemetry then proves
//! the same commits draw while visible and schedule zero frames after
//! Omarchy's silent-send chord parks the window.
//!
//! For repeatable profiling, `CHONKSTEP_DAMAGE_SAMPLE_COMMITS` lengthens the
//! hidden interval (in multiples of 30) and
//! `CHONKSTEP_DAMAGE_STATIC_CLIENTS` plants additional idle surfaces. Both
//! default to the smallest regression test. `CHONKSTEP_DAMAGE_LAYER_BAR=1`
//! keeps a real exclusive-zone layer surface mapped during the sample, which
//! isolates per-dispatch layer/workarea overhead.
//! `CHONKSTEP_DAMAGE_FOREIGN_TOPLEVEL=1` keeps a real wlr foreign-toplevel
//! subscriber connected to isolate publication overhead.

use std::time::Duration;

use chonk_testkit::{keys, poll_until, profile_binary, Session, SessionOptions};

const SETTLE: Duration = Duration::from_secs(10);

fn animation_frame(log: &str) -> Option<u64> {
    log.lines()
        .filter_map(|line| line.strip_prefix("animation frame=")?.parse().ok())
        .next_back()
}

fn wait_for_animation(session: &Session, program: &str, target: u64) {
    poll_until(SETTLE, &format!("the self-timed client to reach frame {target}"), || {
        animation_frame(&session.client_log(program)).filter(|frame| *frame >= target)
    })
    .unwrap_or_else(|error| panic!("{error}; client log:\n{}", session.client_log(program)));
}

fn rendered_frames(session: &Session) -> usize {
    session.log().lines().filter(|line| line.contains("frame damage")).count()
}

/// User + system CPU ticks from Linux's process accounting. This is
/// intentionally a counter rather than elapsed time: an A/B run may
/// share the host with anything, but only work done by the compositor
/// is charged here.
fn process_cpu_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_process_cpu_ticks(&stat)
}

fn parse_process_cpu_ticks(stat: &str) -> Option<u64> {
    // `comm` may contain spaces and parentheses. Its final `)` is the
    // only safe landmark; fields after it begin with field 3 (`state`),
    // making `utime` and `stime` offsets 11 and 12 in this iterator.
    let (_, rest) = stat.rsplit_once(") ")?;
    let mut fields = rest.split_whitespace();
    let user = fields.nth(11)?.parse::<u64>().ok()?;
    let system = fields.next()?.parse::<u64>().ok()?;
    Some(user + system)
}

fn hidden_sample_commits() -> u64 {
    std::env::var("CHONKSTEP_DAMAGE_SAMPLE_COMMITS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|commits: &u64| *commits >= 30 && commits.is_multiple_of(30))
        .unwrap_or(60)
}

fn static_client_count() -> usize {
    std::env::var("CHONKSTEP_DAMAGE_STATIC_CLIENTS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|clients: &usize| *clients <= 200)
        .unwrap_or(0)
}

fn profile_layer_bar() -> bool {
    matches!(std::env::var("CHONKSTEP_DAMAGE_LAYER_BAR").as_deref(), Ok("1"))
}

fn profile_foreign_toplevel() -> bool {
    matches!(std::env::var("CHONKSTEP_DAMAGE_FOREIGN_TOPLEVEL").as_deref(), Ok("1"))
}

/// Omarchy's `super+shift+alt+2`: send the focused window to workspace
/// two without following it. The barriers make the before/after frame
/// counters strict boundaries rather than timer-dependent samples.
fn send_to_workspace_two(session: &mut Session) {
    let door = session.door();
    door.key(keys::LEFTMETA, true).unwrap();
    door.key(keys::LEFTSHIFT, true).unwrap();
    door.key(keys::LEFTALT, true).unwrap();
    door.barrier().unwrap();
    door.tap_key(keys::TWO).unwrap();
    door.key(keys::LEFTALT, false).unwrap();
    door.key(keys::LEFTSHIFT, false).unwrap();
    door.key(keys::LEFTMETA, false).unwrap();
    door.barrier().unwrap();
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn a_self_timed_client_on_a_parked_workspace_schedules_no_frames() {
    let layer_bar = profile_layer_bar();
    let foreign_toplevel = profile_foreign_toplevel();
    let options = SessionOptions {
        config_extra: "desktop = \"omarchy\"\nomarchy_bar = false\nshow_dock = false\n".into(),
        env: vec![("CHONKSTEP_DAMAGE_LOG".into(), "1".into())],
        ..Default::default()
    };
    let mut session = Session::boot("hidden-surface-damage", options).expect("session boots");
    if layer_bar {
        let bar = profile_binary("chonk-fake-bar").expect("fake layer bar is built");
        let bar = bar.display().to_string();
        session.launch(&bar, &["48"]).expect("profiling layer bar launches");
        poll_until(SETTLE, "the profiling layer bar to map", || {
            session.client_log(&bar).contains("mapped ").then_some(())
        })
        .expect("profiling layer bar maps");
    }
    let probe = profile_binary("chonk-fullscreen-probe").expect("probe is built");
    let program = probe.display().to_string();
    let static_clients = static_client_count();
    for index in 0..static_clients {
        let title = format!("DamageIdle{index}");
        session.launch(&program, &[&title, &title]).expect("static probe launches");
    }
    if static_clients > 0 {
        poll_until(SETTLE, &format!("all {static_clients} static surfaces to map"), || {
            let world = session.world().ok()?;
            (world.windows.iter().filter(|window| window.title.starts_with("DamageIdle")).count() == static_clients)
                .then_some(())
        })
        .unwrap();
    }
    if foreign_toplevel {
        let observer = profile_binary("chonk-toplevel-mapping-probe").expect("foreign-toplevel probe is built");
        let observer = observer.display().to_string();
        session.launch(&observer, &[]).expect("foreign-toplevel observer launches");
        poll_until(SETTLE, "the foreign-toplevel observer to bind", || {
            session.client_log(&observer).contains("**mapping ready**").then_some(())
        })
        .expect("foreign-toplevel observer binds");
    }
    session
        .launch(&program, &["HiddenPulse", "hidden-pulse", "animate"])
        .expect("self-timed probe launches");
    session.wait_for_window("HiddenPulse").expect("self-timed probe maps");

    wait_for_animation(&session, &program, 30);
    session.door().barrier().expect("visible sample starts at a rendered boundary");
    let visible_start = rendered_frames(&session);
    let visible_target = animation_frame(&session.client_log(&program)).unwrap() + 60;
    wait_for_animation(&session, &program, visible_target);
    session.door().barrier().expect("visible sample ends at a rendered boundary");
    let visible_frames = rendered_frames(&session) - visible_start;
    assert!(
        visible_frames >= 50,
        "60 independently committed visible frames produced only {visible_frames} renders; compositor log:\n{}",
        session.log()
    );

    send_to_workspace_two(&mut session);
    let hidden_start = rendered_frames(&session);
    let sample_commits = hidden_sample_commits();
    let cpu_start = process_cpu_ticks(session.compositor_pid()).expect("Linux process accounting is readable");
    let hidden_target = animation_frame(&session.client_log(&program)).unwrap() + sample_commits;
    let sample_timeout = Duration::from_millis(sample_commits.saturating_mul(20).saturating_add(10_000));
    poll_until(sample_timeout, &format!("the hidden client to commit {sample_commits} frames"), || {
        animation_frame(&session.client_log(&program)).filter(|frame| *frame >= hidden_target)
    })
    .unwrap_or_else(|error| panic!("{error}; client log:\n{}", session.client_log(&program)));
    let cpu_ticks = process_cpu_ticks(session.compositor_pid())
        .expect("Linux process accounting remains readable")
        .saturating_sub(cpu_start);
    let hidden_frames = rendered_frames(&session) - hidden_start;
    eprintln!(
        "self-timed damage sample: {visible_frames} visible render submissions; \
         {hidden_frames} renders and {cpu_ticks} compositor CPU ticks across {sample_commits} parked commits \
         with {static_clients} additional surfaces, layer_bar={layer_bar}, \
         and foreign_toplevel={foreign_toplevel}"
    );
    let allowed_background_frames = usize::from(static_clients > 0 || layer_bar);
    assert!(
        hidden_frames <= allowed_background_frames,
        "a parked client's {sample_commits}-commit burst scheduled {hidden_frames} invisible renders \
         (allowance {allowed_background_frames} with {static_clients} profiling surfaces); compositor log:\n{}",
        session.log()
    );
}

#[test]
fn animation_progress_uses_the_latest_complete_marker() {
    assert_eq!(animation_frame("noise\nanimation frame=30\nanimation frame=60\n"), Some(60));
    assert_eq!(animation_frame("animation frame=not-a-number\n"), None);
}

#[test]
fn linux_process_ticks_are_parsed_after_a_hostile_comm_field() {
    let stat = "123 (a name ) with spaces) S 1 2 3 4 5 6 7 8 9 10 17 19";
    assert_eq!(parse_process_cpu_ticks(stat), Some(36));
}
