//! Repeated real-client, Overview, dock, theme, IPC and screencopy churn.
//!
//! The normal e2e suite runs a short smoke test. For a sustained run:
//! CHONKSTEP_SOAK_SECONDS=1200 scripts/e2e.sh --headless --release --test stability_soak
//! All activity and /proc measurements target the nested compositor.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use chonk_testkit::{keys, poll_until, Session, SessionOptions, World};
use serde_json::{json, Value};

const EVENT: Duration = Duration::from_secs(15);
const KEY_D: u32 = 32;

fn config(theme: &str) -> String {
    format!(
        "omarchy_shell = false\nomarchy_menu = false\nhyprland_config = false\n\
         restore_session = false\nshow_dock = true\nscale = 1\ntheme = {theme:?}\n\
         [keybindings]\n\"super+d\" = \"toggle-dock\"\n"
    )
}

fn overview_open(world: &World) -> bool {
    world.shells.iter().any(|shell| {
        shell.mapped && shell.above && shell.w == world.output_w && shell.h == world.output_h
    })
}

fn wait_world(session: &mut Session, description: &str, matches: impl Fn(&World) -> bool) {
    poll_until(EVENT, description, || session.world().ok().filter(&matches))
        .unwrap_or_else(|error| panic!("{error}; artifacts: {}", session.dir.display()));
}

fn cycle(session: &mut Session, cycle: usize, initial_windows: usize) {
    let title: String = (0..12).map(|offset| {
        char::from_u32(0x4e00 + ((cycle * 12 + offset) % 0x5000) as u32).unwrap()
    }).collect();
    session.launch("foot", &[
        "--config=/dev/null", "--app-id=chonk-soak-client", "--override=locked-title=yes",
        "--window-size-pixels=400x240", &format!("--title={title}"), "/bin/sleep", "3600",
    ]).expect("launch isolated real client");
    session.wait_for_window("chonk-soak-client").expect("client maps");

    session.door().chord(keys::LEFTMETA, keys::UP).unwrap();
    wait_world(session, "Overview to open", overview_open);
    if cycle.is_multiple_of(25) {
        session.screenshot(&format!("soak-{cycle}"))
            .expect("real screencopy completes during the workload");
    }
    session.door().tap_key(keys::ESC).unwrap();
    wait_world(session, "Overview to close", |world| !overview_open(world));

    session.door().chord(keys::LEFTMETA, KEY_D).unwrap();
    wait_world(session, "dock to hide", |world| world.dock().is_none());
    session.door().chord(keys::LEFTMETA, KEY_D).unwrap();
    wait_world(session, "dock to return", |world| world.dock().is_some());

    // Poll through the actual Hyprland-compatible socket, not a mock.
    let signature = session.hyprland_signature().expect("Hyprland IPC enabled");
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap();
    let path = format!("{runtime}/hypr/{signature}/.socket.sock");
    for request in ["j/clients", "j/monitors", "j/workspaces", "j/activewindow"] {
        let mut stream = UnixStream::connect(&path).expect("connect Hyprland request socket");
        stream.set_read_timeout(Some(EVENT)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).expect("IPC reply reaches EOF");
        serde_json::from_str::<Value>(&reply).expect("IPC returns valid JSON");
    }

    if cycle.is_multiple_of(5) {
        for theme in ["graphite", "nextstep-classic"] {
            session.rewrite_config(&config(theme)).unwrap();
            session.request_reload().unwrap();
            wait_world(session, "live theme reload", |world| world.theme.id == theme);
        }
    }
    session.kill_clients();
    wait_world(session, "client resources to retire", |world| world.windows.len() == initial_windows);
    session.door().barrier().unwrap();
}

fn sample(session: &mut Session, cycle: usize, elapsed: Duration) -> Value {
    let pid = session.compositor_pid();
    let rollup = fs::read_to_string(format!("/proc/{pid}/smaps_rollup")).unwrap();
    let memory = |key: &str| -> u64 {
        rollup.lines().find_map(|line| {
            line.strip_prefix(key)?.split_whitespace().next()?.parse().ok()
        }).unwrap()
    };
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    let fields: Vec<_> = stat.rsplit(')').next().unwrap().split_whitespace().collect();
    let world = session.world().unwrap();
    let descriptors: Vec<_> = fs::read_dir(format!("/proc/{pid}/fd")).unwrap().flatten()
        .filter_map(|entry| fs::read_link(entry.path()).ok()).collect();
    let pipes = descriptors.iter().filter(|path| path.to_string_lossy().starts_with("pipe:[")).count();
    json!({
        "cycle": cycle, "elapsed_seconds": elapsed.as_secs_f64(), "pid": pid,
        "rss_kib": memory("Rss:"), "pss_kib": memory("Pss:"),
        "private_kib": memory("Private_Clean:") + memory("Private_Dirty:"),
        "private_dirty_kib": memory("Private_Dirty:"), "anonymous_pss_kib": memory("Pss_Anon:"),
        "cpu_ticks": fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap(),
        "threads": fs::read_dir(format!("/proc/{pid}/task")).unwrap().count(),
        "fds": descriptors.len(), "nonpipe_fds": descriptors.len() - pipes, "pipe_fds": pipes,
        "windows": world.windows.len(), "frames": world.frames.len(), "shells": world.shells.len(),
    })
}

fn save_proc(session: &Session, label: &str) {
    for name in ["status", "smaps", "smaps_rollup", "maps", "stat"] {
        fs::copy(format!("/proc/{}/{name}", session.compositor_pid()),
            session.dir.join(format!("{label}.{name}"))).unwrap();
    }
    let descriptors: std::collections::BTreeMap<_, _> =
        fs::read_dir(format!("/proc/{}/fd", session.compositor_pid())).unwrap()
            .flatten().filter_map(|entry| {
                let target = fs::read_link(entry.path()).ok()?;
                Some((entry.file_name().to_string_lossy().into_owned(), target.to_string_lossy().into_owned()))
            }).collect();
    fs::write(session.dir.join(format!("{label}.fds.json")),
        serde_json::to_vec_pretty(&descriptors).unwrap()).unwrap();
}

fn check_growth(before: &Value, after: &Value, transient: bool) {
    let metric = |value: &Value, key: &str| value[key].as_u64().unwrap();
    // Anonymous memory is insensitive to another process mapping the
    // same library text and reclassifying Private_Clean / Pss_File.
    assert!(metric(after, "anonymous_pss_kib") <= metric(before, "anonymous_pss_kib") + 64 * 1024,
        "retained anonymous memory grew by more than 64 MiB: {before} -> {after}");
    let slack = if transient { 32 } else { 4 };
    // Visible instruments continuously start bounded command samples.
    // Their short-lived pipes varied by five FDs in the GPU smoke run;
    // inspect stable descriptors separately without hiding a pipe leak.
    assert!(metric(after, "fds") <= metric(before, "fds") + if transient { 32 } else { 16 },
        "file descriptors accumulated: {before} -> {after}");
    assert!(metric(after, "nonpipe_fds") <= metric(before, "nonpipe_fds") + slack,
        "non-pipe descriptors accumulated: {before} -> {after}");
    assert!(metric(after, "threads") <= metric(before, "threads") + slack,
        "retired sampler threads accumulated: {before} -> {after}");
    for key in ["windows", "frames"] {
        assert_eq!(after[key], before[key], "retired {key} accumulated");
    }
    // A theme swap discards the reusable, unmapped Overview surfaces;
    // opening it again recreates them. Losing those is not a leak.
    assert!(metric(after, "shells") <= metric(before, "shells"),
        "retired shell surfaces accumulated: {before} -> {after}");
}

#[test]
#[ignore = "needs a nested Wayland session; set CHONKSTEP_SOAK_SECONDS for sustained coverage"]
fn real_desktop_churn_keeps_resources_bounded_and_protocols_responsive() {
    let seconds = std::env::var("CHONKSTEP_SOAK_SECONDS").ok()
        .map(|value| value.parse::<u64>().expect("CHONKSTEP_SOAK_SECONDS is an integer"))
        .unwrap_or(0);
    assert!(seconds <= 7200, "split longer soak runs into individually bounded two-hour runs");
    let mut session = Session::boot("stability-soak", SessionOptions {
        config_extra: "omarchy_menu = false\nhyprland_config = false\nrestore_session = false\n\
            [keybindings]\n\"super+d\" = \"toggle-dock\"\n".into(),
        scale: Some(1.0),
        env: vec![("CHONKSTEP_HYPRLAND_IPC".into(), "1".into())],
        ..Default::default()
    }).expect("isolated compositor boots");
    let initial_windows = session.world().unwrap().windows.len();
    let mut output = File::create(session.dir.join("samples.jsonl")).unwrap();
    // Populate both themes, transient surfaces, clients and screenshot
    // paths before establishing the sustained-growth reference.
    for index in 0..7 {
        cycle(&mut session, index, initial_windows);
    }
    // Match the final sample's settling interval, including deferred
    // renderer / X11 initialization and in-flight sampler commands.
    std::thread::sleep(Duration::from_secs(2));
    let started = Instant::now();
    let before = sample(&mut session, 7, started.elapsed());
    writeln!(output, "{before}").unwrap();
    save_proc(&session, "before");
    let mut index = 7;
    loop {
        cycle(&mut session, index, initial_windows);
        index += 1;
        if index.is_multiple_of(10) || started.elapsed() >= Duration::from_secs(seconds) {
            let value = sample(&mut session, index, started.elapsed());
            writeln!(output, "{value}").unwrap();
            output.flush().unwrap();
            eprintln!("soak: {value}");
            // Abort a leaking run while it is still small, not only
            // after a long requested duration has elapsed.
            check_growth(&before, &value, true);
        }
        if started.elapsed() >= Duration::from_secs(seconds) {
            break;
        }
    }
    // A sampler already doing a bounded command may finish after its
    // old theme has been retired. Let those in-flight workers exit.
    std::thread::sleep(Duration::from_secs(2));
    let after = sample(&mut session, index, started.elapsed());
    writeln!(output, "{after}").unwrap();
    save_proc(&session, "after");
    eprintln!("soak complete: before={before}; after={after}; artifacts={}", session.dir.display());
    check_growth(&before, &after, false);
}
