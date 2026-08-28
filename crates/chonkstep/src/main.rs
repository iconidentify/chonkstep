//! The X11 chonkstep binary: a thin event-loop driver over the
//! backend-generic desktop shell in `chonk-shell`. Everything the
//! desktop *is* — dock, Clip, launcher strip, menus, wallpaper, theme
//! semantics — lives in [`chonk_shell::shell::Shell`]; this binary owns
//! only what is irreducibly process- or X11-side: startup wiring, the
//! `poll`-driven loop with its motion coalescing, scale/theme/focus
//! precedence (env over config), the hot-restart `exec`, and process
//! exit. The future Wayland binary mirrors exactly this file over the
//! same `Shell`, which is what keeps the two desktops identical by
//! construction rather than by porting discipline.

use std::time::Duration;

use wm_core::{Backend, BackendEvent, FocusPolicy, WindowManager};
use wm_theme::RasterThemeEngine;
use wm_x11::X11Backend;

use chonk_shell::shell::{Shell, ShellOutcome};
use chonk_shell::startup::{ensure_xcursor_size, read_focus_follows_mouse, read_scale_factor, resolve_theme};
use chonk_shell::spawn;

fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "chonkstep starting \u{2014} a modern window manager in the classic NeXTSTEP style"
    );

    // User configuration is loaded before anything scale- or theme-
    // dependent is built. `wm_config::load()` never fails by contract:
    // no file yields the defaults, and a broken file logs what is wrong
    // and yields the defaults too. That last part is a hard requirement,
    // not a convenience — a typo in the config must never cost the user
    // their session, because with the WM refusing to start there is no
    // terminal to fix the typo from.
    let config = wm_config::load();

    let scale = read_scale_factor(config.scale);
    tracing::info!(scale, "UI scale (config `scale`; CHONKSTEP_SCALE overrides)");
    ensure_xcursor_size(scale);

    let mut backend = match X11Backend::connect_and_become_wm(None, scale) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(?e, "failed to start");
            std::process::exit(1);
        }
    };

    let screen = backend.screen_size();

    let theme = resolve_theme(config.theme.as_deref()).scaled(scale);
    tracing::info!(theme = %theme.id, "theme loaded");
    if let Some(opacity) = theme.terminal.opacity {
        backend.add_opacity_rule("URxvt", opacity);
    }
    let engine = RasterThemeEngine::new(theme.clone());

    // The entire desktop shell — dock, Clip, launcher strip, menus,
    // wallpaper, the `.desktop` application index — is built here in
    // one step, against the mutable backend, before `WindowManager::new`
    // takes ownership of it below. From here on the shell reaches the
    // backend only through the `WindowManager` handed to each of its
    // methods, which is the shape both backend binaries share.
    let mut shell = Shell::new(&mut backend, &config, theme, scale);

    // The wallpaper pixmap `Shell::new` just published (via its
    // `Desktop`) dies with the previous process's X connection on every
    // hot-restart, and the session compositor keeps referencing the
    // dead one — compositing translucent windows over black instead of
    // the wallpaper (confirmed live; a fresh picom picked the new
    // pixmap up fine). SIGUSR1 is picom's documented full-reset signal:
    // cheap, safe, and a no-op exit code when no compositor is running.
    spawn::spawn_detached("pkill", &["-USR1", "-x", "picom"]);

    let existing = backend.scan_existing_windows();
    let mut wm = WindowManager::new(backend, Box::new(engine));
    wm.set_workarea(shell.workarea(screen));
    wm.bind_default_keys();
    // Every configured combo is grabbed on top of the defaults — the
    // modal Alt+Tab grabs stay `wm-core`'s own (`bind_default_keys`),
    // but the X server only routes a configured combo's presses to the
    // WM at all if it is grabbed here. A combo that overlaps a default
    // grab is harmless: same-client grabs simply replace, and the
    // backend logs-and-continues on any grab it cannot take (an
    // unknown keysym in the config degrades to a dead binding, never a
    // dead session).
    for (combo, _) in &config.keybindings {
        wm.grab_key(*combo);
    }
    if read_focus_follows_mouse(config.focus_follows_mouse) {
        tracing::info!("focus-follows-mouse enabled (config `focus_follows_mouse`; CHONKSTEP_FOCUS_FOLLOWS_MOUSE overrides)");
        wm.set_focus_policy(FocusPolicy::FocusFollowsMouse);
    }
    // Initial window placement and drag edge snapping, straight from
    // the config file (`placement`, `edge_resistance`). `wm-config`
    // already validated both — anything broken fell back to its default
    // there, with a warning — so the values apply verbatim here.
    wm.set_placement_policy(config.placement);
    wm.set_snap_threshold(config.edge_resistance);
    for window in existing {
        wm.dispatch(BackendEvent::MapRequest(window));
    }

    tracing::info!(clients = wm.client_count(), "entering event loop");
    // The X11 root window's id, for the click routing below — the one
    // surface identity the shell cannot know (nothing backend-generic
    // could name it), so the binary tells root presses apart from
    // shell-surface clicks before anything reaches the shell.
    let root = wm.backend().root();
    loop {
        if restart_requested() {
            tracing::info!("restart requested — re-executing in place");
            restart_in_place();
        }

        // Consecutive `PointerMotion` events are coalesced to just the
        // most recent one instead of dispatching (and repainting for)
        // every single one — during a fast move/resize drag, dozens can
        // pile up in the queue between two loop iterations, and every
        // intermediate position is immediately superseded by the next.
        // Dispatching all of them was exactly why a drag used to visibly
        // "catch up" to the cursor in steps instead of tracking it —
        // real, wasted work for positions that were already stale by
        // the time they were drawn. Held back (not dispatched inline)
        // so a later non-motion event in the same burst — a button
        // release ending the drag, say — still commits after the
        // window has caught up to the *latest* position, not a stale
        // one from earlier in the burst.
        let mut pending_motion = None;
        let mut display_lost = false;
        let mut should_exit = false;
        while let Some(event) = wm.backend_mut().poll_event() {
            // The display server is gone: nothing below can succeed,
            // and looping on a dead connection is exactly how the
            // zombie chonkstep processes that outlived an X restart
            // were born (two found spinning at full poll rate,
            // confirmed live). Exit cleanly instead.
            if matches!(event, BackendEvent::ShutdownRequested) {
                display_lost = true;
                break;
            }
            if matches!(event, BackendEvent::PointerMotion { .. }) {
                pending_motion = Some(event);
                continue;
            }
            // Configured keybindings resolve here, BEFORE `wm.dispatch`:
            // a combo the user bound runs its action and the event stops
            // with it. Everything that misses the keymap MUST keep
            // flowing through to `wm-core` unchanged — during a modal
            // Alt+Tab session the switcher grabs the whole keyboard, so
            // every key the user presses arrives as a `KeyPress` here:
            // Tab steps the selection, Escape cancels, and any other
            // unbound key commits it, none of which appear in the
            // config keymap. Unbound keys flowing through is therefore
            // load-bearing; swallowing them would wedge the switcher
            // open and eat its Escape. (`KeyRelease` — the Alt release
            // that commits a cycle — is never intercepted at all.)
            if let BackendEvent::KeyPress(combo) = &event {
                if let Some(action) = shell.keymap_action(combo) {
                    // An action observes the same ordering rule as any
                    // other non-motion event: the held-back motion
                    // commits first, so e.g. a focus-follows-mouse focus
                    // change from this same burst lands before an action
                    // that targets the focused client.
                    if let Some(motion) = pending_motion.take() {
                        dispatch_motion(&mut wm, &mut shell, motion);
                    }
                    if exit_requested(shell.run_action(&mut wm, &action)) {
                        should_exit = true;
                    }
                    continue;
                }
            }
            if let Some(motion) = pending_motion.take() {
                dispatch_motion(&mut wm, &mut shell, motion);
            }
            wm.dispatch(event);
        }
        if display_lost {
            tracing::error!("display connection lost, exiting");
            break;
        }
        if let Some(motion) = pending_motion.take() {
            dispatch_motion(&mut wm, &mut shell, motion);
        }

        while let Some(notification) = wm.take_notification() {
            shell.on_notification(&mut wm, notification);
        }

        if let Some(new_size) = wm.backend_mut().take_screen_resize() {
            tracing::info!(width = new_size.w, height = new_size.h, "screen resized");
            shell.on_screen_resize(&mut wm, new_size);
            wm.set_workarea(shell.workarea(new_size));
        }

        // Shell-surface clicks drain to the shell, with the one routing
        // decision the shell cannot make for itself (see `root` above):
        // presses on the root window — the right-click that opens the
        // root menu, any other press that closes an open one — split
        // off into `on_root_press`. Root reacts on *press* because a
        // context menu should appear the instant you press the button,
        // same as everywhere else. Root *releases* still flow through
        // `on_shell_click`: an in-progress launcher-strip drag holds a
        // pointer grab, so its release can report against any window at
        // all, the root included, and the shell's release-before-
        // anything-else routing must get to see it (drag-off-the-strip
        // unpins) — a root release the shell has no drag in progress
        // for falls through its routing as the no-op it always was.
        while let Some((surface, local, button, pressed)) = wm.backend_mut().take_shell_click() {
            let outcome = if surface == root && pressed {
                shell.on_root_press(&mut wm, local, button)
            } else {
                shell.on_shell_click(&mut wm, surface, local, button, pressed)
            };
            if exit_requested(outcome) {
                should_exit = true;
            }
        }
        if should_exit {
            tracing::info!("exit requested from root menu, shutting down");
            break;
        }

        // No separate `take_shell_motion` drain here: the shell drains
        // it itself inside `on_motion` (menu hover rides the same
        // cadence as the drag trackers), and every pointer motion over
        // a shell surface reaches `on_motion` through the coalesced
        // `PointerMotion` dispatch above — the backend queues both from
        // the same X event, so a shell motion can never be left pending
        // past the burst that produced it.

        // Housekeeping: widgets, menu timers, workspace indicator,
        // launcher running-state — everything the shell refreshes per
        // tick rather than per event.
        shell.tick(&mut wm);

        // Blocks until the X11 socket actually has something to read,
        // instead of a fixed sleep — the entire reason drags/resizes
        // used to feel like they were catching up to the cursor in
        // steps: with a flat `sleep(100ms)` here, no input got
        // processed for up to 100ms at a time no matter how fast the
        // pointer was moving. `HOUSEKEEPING_INTERVAL` bounds the wait
        // so the clock/menu-hover-timeout ticks above still run
        // regularly even with zero X11 activity; real input wakes this
        // up immediately, every time, regardless of that bound.
        wait_for_x11_activity(wm.backend().connection_fd(), HOUSEKEEPING_INTERVAL);
    }
}

/// How often the main loop wakes up on its own even with no X11
/// activity at all, to run `Shell::tick`/`restart_requested` — ~60Hz,
/// far more than any of those actually need, but cheap and keeps them
/// feeling responsive rather than picking a number tied to any one of
/// their specific timing requirements.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_millis(16);

/// Blocks the calling thread until `fd` is readable or `timeout`
/// elapses, whichever comes first — the integration pattern x11rb's own
/// `event_loop_integration` module docs recommend for exactly this
/// (`conn.stream().as_raw_fd()` + an external `poll`), rather than
/// guessing at a fixed sleep duration that either wastes latency (too
/// long) or busy-loops (too short).
fn wait_for_x11_activity(fd: std::os::unix::io::RawFd, timeout: Duration) {
    let mut fds = [libc::pollfd { fd, events: libc::POLLIN, revents: 0 }];
    // SAFETY: `fds` is a valid, appropriately-sized array for the
    // `nfds=1` we pass; `poll` only reads/writes through that pointer
    // for the duration of the call.
    unsafe {
        libc::poll(fds.as_mut_ptr(), 1, timeout.as_millis() as i32);
    }
}

/// Feeds one (already-coalesced) `PointerMotion` event to the shell —
/// which routes it to its icon, dock-widget, and launcher-strip drag
/// trackers, records the last root position for its own release
/// decisions, and drains the backend's pending shell-surface motion
/// into menu hover — and then to `wm-core` itself. Pulled out of the
/// main loop's drain since it's needed at two points there (mid-burst,
/// when a non-motion event follows one, and once more after the burst
/// ends).
fn dispatch_motion(
    wm: &mut WindowManager<X11Backend>,
    shell: &mut Shell<X11Backend>,
    event: BackendEvent<wm_x11::XWindow, wm_x11::XFrame>,
) {
    if let BackendEvent::PointerMotion { root, .. } = &event {
        shell.on_motion(wm, *root);
    }
    wm.dispatch(event);
}

/// Applies a [`ShellOutcome`] to the process — the two acts the shell
/// deliberately cannot perform itself stay here in the binary: `Exit`
/// is reported back as `true` for the loop to break on, and `Restart`
/// re-execs on the spot (the shell already persisted whatever the
/// fresh process must read back — theme choice, wallpaper — before
/// returning it). Split out because outcomes surface at two points in
/// the loop (a configured key action mid-drain, a shell click after
/// it) that must not drift on what each variant means.
fn exit_requested(outcome: ShellOutcome) -> bool {
    match outcome {
        ShellOutcome::Continue => false,
        ShellOutcome::Exit => true,
        // The exact path `scripts/restart.sh` takes: re-exec the
        // on-disk binary in place, windows surviving via the X11
        // SaveSet — which is also what makes the theme menu (and a
        // bound `Restart` action) the config hot-reload gesture.
        ShellOutcome::Restart => restart_in_place(),
    }
}









/// Path to the marker file `scripts/restart.sh` touches to ask a
/// running chonkstep to hot-restart itself — polled once per event-loop
/// tick (the loop already blocks on the X11 socket with a bounded
/// timeout, so this adds one cheap `remove_file` attempt per wakeup,
/// not a new busy-loop).
fn restart_marker_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".local/state/chonkstep/restart")
}

/// `true` at most once per `touch` of the marker file — `remove_file`
/// both checks for and consumes the request in one step, so a restart
/// can never fire twice for a single request.
fn restart_requested() -> bool {
    std::fs::remove_file(restart_marker_path()).is_ok()
}

/// Re-execs the *on-disk* binary in place (same PID, replaces this
/// process's image) rather than `std::env::current_exe()` — resolved
/// from `argv[0]`, not from the running process, because the whole
/// point is to pick up whatever a `cargo build --release` just put at
/// that path; `current_exe()` on Linux resolves through `/proc/self/exe`,
/// which keeps pointing at the *original* (now-replaced) inode this
/// process was loaded from, not the fresh file now sitting at the same
/// path. `argv[0]` is the path the session script launched (and each
/// re-exec below re-passes), so it tracks wherever the repo lives
/// without hardcoding a home directory. Existing client windows survive
/// the swap: they were added to
/// the X11 SaveSet in `create_decoration`, so when this process's X
/// connection closes (implied by `exec`, which closes non-inherited
/// fds), the server reparents them straight back to root instead of
/// destroying them — the freshly exec'd process then finds them again
/// via `scan_existing_windows` and redecorates them, same as a normal
/// startup.
fn restart_in_place() -> ! {
    use std::os::unix::process::CommandExt;
    let bin = std::env::args_os().next().unwrap_or_else(|| "chonkstep".into());
    let err = std::process::Command::new(&bin).exec();
    tracing::error!(?err, bin = ?bin, "re-exec failed; exiting instead of restarting");
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    use wm_config::Action;
    use wm_core::KeyCombo;

    /// Keeps `docs/config.example.toml` honest: the example is parsed
    /// with the real parser, must restate exactly the default bindings
    /// (every option line is commented out, so copying the file
    /// verbatim changes nothing), and must not smuggle in anything
    /// extra. Documentation that the test suite does not check drifts;
    /// this one cannot.
    #[test]
    fn example_config_parses_and_matches_the_defaults() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/config.example.toml");
        let text = std::fs::read_to_string(path).expect("docs/config.example.toml must exist");
        let example = wm_config::parse(&text).expect("the example config must parse cleanly");
        let defaults = wm_config::Config::default_config();

        assert!(!example.focus_follows_mouse, "example must leave focus_follows_mouse at its default (commented out)");
        assert!(example.scale.is_none(), "example must leave scale unset (commented out)");
        assert!(example.theme.is_none(), "example must leave theme unset (commented out)");

        let as_set = |config: &wm_config::Config| {
            let mut bindings: Vec<(KeyCombo, Action)> = config.keybindings.clone();
            bindings.sort_by_key(|(combo, _)| (combo.keysym, combo.modifiers.bits()));
            bindings
        };
        assert_eq!(
            as_set(&example),
            as_set(&defaults),
            "the example's [keybindings] must restate exactly the default set — no drift, no extras"
        );
    }
}
