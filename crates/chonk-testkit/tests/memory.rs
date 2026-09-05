//! The compositor's real transient-buffer workload must not ratchet
//! glibc's main heap arena upward. This complements the allocator-level
//! subprocess regression in `chonk-shell::startup`: it boots the
//! Wayland binary, opens Overview, changes theme twice, captures real
//! screenshots, and reads the compositor's own `/proc` accounting.

use std::path::Path;
use std::time::Duration;

use chonk_testkit::{keys, poll_until, Session, SessionOptions, World};

const SETTLE: Duration = Duration::from_secs(30);
const MAX_HEAP_GROWTH_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
struct HeapSnapshot {
    extent_bytes: usize,
    size_bytes: usize,
    rss_bytes: usize,
}

fn heap_snapshot(pid: u32) -> Result<HeapSnapshot, String> {
    let maps =
        std::fs::read_to_string(format!("/proc/{pid}/maps")).map_err(|error| error.to_string())?;
    let heap = maps
        .lines()
        .find(|line| line.ends_with("[heap]"))
        .ok_or_else(|| format!("/proc/{pid}/maps has no [heap] mapping"))?;
    let range = heap
        .split_whitespace()
        .next()
        .ok_or("heap mapping has no address range")?;
    let (start, end) = range
        .split_once('-')
        .ok_or("heap mapping has a malformed address range")?;
    let start = usize::from_str_radix(start, 16).map_err(|error| error.to_string())?;
    let end = usize::from_str_radix(end, 16).map_err(|error| error.to_string())?;

    let smaps =
        std::fs::read_to_string(format!("/proc/{pid}/smaps")).map_err(|error| error.to_string())?;
    let mut lines = smaps.lines().skip_while(|line| !line.ends_with("[heap]"));
    let _ = lines
        .next()
        .ok_or_else(|| format!("/proc/{pid}/smaps has no [heap] section"))?;
    let mut size_bytes = None;
    let mut rss_bytes = None;
    for line in lines {
        if line
            .split_whitespace()
            .next()
            .is_some_and(|word| word.contains('-'))
        {
            break;
        }
        let kib = |prefix: &str| {
            line.strip_prefix(prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|value| value.parse::<usize>().ok())
                .map(|value| value * 1024)
        };
        size_bytes = size_bytes.or_else(|| kib("Size:"));
        rss_bytes = rss_bytes.or_else(|| kib("Rss:"));
    }

    Ok(HeapSnapshot {
        extent_bytes: end.saturating_sub(start),
        size_bytes: size_bytes.ok_or("heap smaps section has no Size")?,
        rss_bytes: rss_bytes.ok_or("heap smaps section has no Rss")?,
    })
}

fn save_proc_snapshot(pid: u32, dir: &Path, label: &str) {
    for file in ["maps", "smaps", "smaps_rollup"] {
        std::fs::copy(
            format!("/proc/{pid}/{file}"),
            dir.join(format!("{label}.{file}")),
        )
        .unwrap_or_else(|error| panic!("save {label} {file}: {error}"));
    }
}

fn overview_is_open(world: &World) -> bool {
    world.shells.iter().any(|surface| {
        surface.mapped
            && surface.above
            && surface.w == world.output_w
            && surface.h == world.output_h
    })
}

fn set_theme(session: &mut Session, theme: &str) {
    session
        .rewrite_config(&format!(
            "omarchy_shell = false\nscale = 2\ntheme = \"{theme}\"\n"
        ))
        .expect("rewrite isolated config");
    session.request_reload().expect("request live theme reload");
    let door = session.door();
    poll_until(
        SETTLE,
        &format!("the {theme} theme to become observable"),
        || {
            door.windows()
                .ok()
                .filter(|world| world.theme.id == theme)
                .map(|_| ())
        },
    )
    .unwrap_or_else(|error| panic!("{error}: theme reload never settled"));
}

#[test]
#[ignore = "needs a live Wayland session to nest in: scripts/e2e.sh, or cargo test -p chonk-testkit -- --ignored --test-threads=1"]
fn overview_theme_and_screenshot_transients_do_not_raise_the_heap_high_water() {
    let mut session = Session::boot(
        "memory-high-water",
        SessionOptions {
            scale: Some(2.0),
            ..SessionOptions::default()
        },
    )
    .expect("session boots");
    session.door().barrier().expect("initial frame settles");

    let pid = session.compositor_pid();
    let before = heap_snapshot(pid).expect("read initial compositor heap");
    save_proc_snapshot(pid, &session.dir, "before");

    session
        .door()
        .chord(keys::LEFTMETA, keys::UP)
        .expect("open Overview");
    {
        let door = session.door();
        poll_until(SETTLE, "the Overview transient surface to map", || {
            door.windows().ok().filter(overview_is_open).map(|_| ())
        })
        .expect("Overview opens");
    }
    session
        .screenshot("overview")
        .expect("capture Overview through screencopy");
    session.door().tap_key(keys::ESC).expect("close Overview");
    {
        let door = session.door();
        poll_until(SETTLE, "the Overview transient surface to unmap", || {
            door.windows()
                .ok()
                .filter(|world| !overview_is_open(world))
                .map(|_| ())
        })
        .expect("Overview closes");
    }

    set_theme(&mut session, "graphite");
    set_theme(&mut session, "nextstep-classic");
    session.door().barrier().expect("final theme frame settles");
    session
        .screenshot("after-transients")
        .expect("capture the settled desktop");
    session
        .door()
        .barrier()
        .expect("screenshot resources are released");

    let after = heap_snapshot(pid).expect("read final compositor heap");
    save_proc_snapshot(pid, &session.dir, "after");
    let extent_growth = after.extent_bytes.saturating_sub(before.extent_bytes);
    let size_growth = after.size_bytes.saturating_sub(before.size_bytes);
    let rss_growth = after.rss_bytes.saturating_sub(before.rss_bytes);
    eprintln!(
        "glibc heap sample: before={before:?} after={after:?} growth: extent={extent_growth} size={size_growth} rss={rss_growth}; artifacts: {}",
        session.dir.display()
    );
    assert!(
        extent_growth <= MAX_HEAP_GROWTH_BYTES
            && size_growth <= MAX_HEAP_GROWTH_BYTES
            && rss_growth <= MAX_HEAP_GROWTH_BYTES,
        "Overview/theme/screenshot workload raised the glibc heap by more than 4 MiB: before={before:?}, after={after:?}; artifacts: {}",
        session.dir.display()
    );
}
