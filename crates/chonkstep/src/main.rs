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

use std::time::{Duration, Instant};

use wm_core::{Backend, BackendEvent, WindowManager};
use wm_theme::{FontState, RasterThemeEngine};
use wm_x11::X11Backend;

use chonk_shell::dockapp::Farewell;
use chonk_shell::shell::{Shell, ShellOutcome};
use chonk_shell::spawn;
use chonk_shell::startup::{
    ensure_xcursor_size, pin_glibc_large_allocation_policy, SessionRequestPoller, SessionState,
};
use chonk_xsettings::{DesktopAppearance, XSettingsManager};

/// Answers `--version` and `-V` before anything else starts.
///
/// It exists so a bug report can name its build. The version was
/// previously reachable only through `pacman -Qi`, which is one more
/// thing a user has to know to produce a report the crash itself
/// cannot produce for them.
///
fn print_version_and_exit_if_asked() {
    let asked = std::env::args()
        .skip(1)
        .any(|arg| arg == "--version" || arg == "-V");
    if !asked {
        return;
    }
    println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    println!("source: {}", chonk_build_info::SOURCE_ID);
    match chonk_build_info::current_elf_build_id() {
        Ok(build_id) => println!("build id: {build_id}"),
        Err(error) => println!("build id: unavailable ({error})"),
    }
    std::process::exit(0);
}

fn inspect_config_and_exit_if_asked() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((index, print)) =
        args.iter()
            .enumerate()
            .find_map(|(index, arg)| match arg.as_str() {
                "--check-config" => Some((index, false)),
                "--print-config" => Some((index, true)),
                _ => None,
            })
    else {
        return;
    };
    // Offline inspection still needs the parser's per-entry warnings;
    // the normal subscriber is intentionally installed only after
    // this early-exit path.
    let _ = tracing_subscriber::fmt()
        .without_time()
        .with_max_level(tracing::Level::WARN)
        .try_init();
    let path = args
        .get(index + 1)
        .filter(|arg| !arg.starts_with('-'))
        .map(std::path::Path::new);
    let config = match wm_config::inspect(path) {
        Ok(config) => config,
        Err(error) => {
            eprintln!("config error: {error}");
            std::process::exit(1);
        }
    };
    if print {
        print!("{}", wm_config::effective_config_report(&config));
    } else {
        for diagnostic in &config.diagnostics {
            println!("{diagnostic}");
        }
        println!(
            "resolved {} bindings; {} diagnostics",
            config.keybindings.len(),
            config.diagnostics.len()
        );
    }
    std::process::exit(0);
}

fn main() {
    // Before the subscriber, so `--version` prints one clean line
    // rather than a line preceded by whatever RUST_LOG asked for.
    // Also before `restart_in_place`'s re-exec can ever matter: that
    // path passes no arguments, so a restarted session never sees one.
    print_version_and_exit_if_asked();
    inspect_config_and_exit_if_asked();
    let allocator_policy_pinned = pin_glibc_large_allocation_policy();
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    if !allocator_policy_pinned {
        tracing::warn!("glibc rejected the fixed mmap/trim thresholds; transient buffers may raise the heap high-water mark");
    }
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "chonkstep starting \u{2014} a modern window manager in the classic NeXTSTEP style"
    );

    // This binary IS the X11 session, and says so rather than leaving
    // the shell to deduce it from the name of the file it is running —
    // see `chonk_shell::spawn::declare_display_stack`.
    chonk_shell::spawn::declare_display_stack(chonk_shell::spawn::DisplayStack::X11);

    // Take the hot-restart marker out of the environment before it can
    // be inherited by anything this session launches — see the function
    // for what a leaked marker does to a nested session. Here, beside
    // the stack declaration, because both are one-shot process facts
    // that must be settled while this process is still single-threaded.
    chonk_shell::startup::consume_session_continuation();

    // User configuration is loaded before anything scale- or theme-
    // dependent is built. `wm_config::load()` never fails by contract:
    // no file yields the defaults, and a broken file logs what is wrong
    // and yields the defaults too. That last part is a hard requirement,
    // not a convenience — a typo in the config must never cost the user
    // their session, because with the WM refusing to start there is no
    // terminal to fix the typo from.
    let config = wm_config::load();

    // Everything a user can change without restarting, resolved in one
    // place and by one set of rules — the same call a live reload makes
    // (see `reload_requested` below), so a session that has been
    // reloaded is indistinguishable from one that started that way.
    let state = SessionState::resolve(&config);
    tracing::info!(scale = state.scale, "UI scale (config `scale`; CHONKSTEP_SCALE overrides)");
    ensure_xcursor_size(state.scale);
    // The `env` lines from the desktop's own Hyprland configuration,
    // applied here for the same reason and in the same window as the
    // cursor size above: they exist to be *inherited*, so they have to
    // be in place before this process starts anything, and this is
    // still the single-threaded part of startup where setting them is
    // sound. Empty on a session that reads no such configuration.
    chonk_shell::startup::apply_session_env(&state.session_env);

    let mut backend = match X11Backend::connect_and_become_wm(None, state.scale) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(?e, "failed to start");
            std::process::exit(1);
        }
    };

    let screen = backend.screen_size();

    let theme = state.theme();
    tracing::info!(theme = %theme.id, "theme loaded");
    if let Some(opacity) = theme.terminal.opacity {
        backend.add_opacity_rule("URxvt", opacity);
    }
    // The font database is built here rather than inside the engine, so
    // that the shell can hold a handle to it and build the engine's
    // replacements around the same one on every later restyle — see
    // `wm_theme::FontState`.
    let fonts = FontState::new();
    let engine = RasterThemeEngine::with_fonts_at_scale(theme, fonts.clone(), state.scale);

    // The entire desktop shell — dock, Clip, launcher strip, menus,
    // wallpaper, the `.desktop` application index — is built here in
    // one step, against the mutable backend, before `WindowManager::new`
    // takes ownership of it below. From here on the shell reaches the
    // backend only through the `WindowManager` handed to each of its
    // methods, which is the shape both backend binaries share.
    let mut shell = Shell::new(&mut backend, &state, fonts);

    // The wallpaper pixmap `Shell::new` just published (via its
    // `Desktop`) dies with the previous process's X connection on every
    // hot-restart, and the session compositor keeps referencing the
    // dead one — compositing translucent windows over black instead of
    // the wallpaper (confirmed live; a fresh picom picked the new
    // pixmap up fine). SIGUSR1 is picom's documented full-reset signal:
    // cheap, safe, and a no-op exit code when no compositor is running.
    spawn::spawn_detached("pkill", &["-USR1", "-x", "picom"]);

    let existing = backend.scan_existing_windows();
    // XSETTINGS: the standard way an X desktop tells every client its
    // DPI, scaling factor and cursor size, and the only way one this
    // session did not launch itself ever hears about them. The
    // per-child environment variables `spawn` sets reach applications
    // started from the Applications menu and nothing else — a terminal
    // the user opens a program from passes on whatever it inherited,
    // and neither mechanism can update an application that is already
    // running.
    //
    // Failure here is a warning and a session that carries on, never a
    // startup failure: another settings manager already owning the
    // selection is a legitimate configuration (a user running
    // `xsettingsd` themselves), and it is theirs, not ours to take.
    let mut xsettings = match XSettingsManager::acquire(None) {
        Ok(manager) => Some(manager),
        Err(e) => {
            tracing::warn!(
                ?e,
                "could not become the XSETTINGS manager; X clients will not be told this desktop's scale"
            );
            None
        }
    };
    publish_appearance(&mut xsettings, &state);
    let mut published_appearance = state.appearance;

    let mut wm = WindowManager::new(backend, Box::new(engine));
    // Session policy — focus, placement, edge resistance, the keymap —
    // is applied through the very same call a live reload makes. The
    // look half of it is already correct (the shell was just built from
    // this state), so the applier finds nothing changed there and
    // repaints nothing; what it does do is put the four policy setters
    // in one place instead of two, which is what stops a setting from
    // being reloadable but not startable, or the reverse.
    //
    // The modal Alt+Tab grabs are `wm-core`'s own and are taken
    // separately, below: the applier only ever reconciles grabs the
    // *config* asked for.
    shell.apply_session_state(&mut wm, state);
    wm.set_workarea(shell.workarea(screen));
    wm.bind_default_keys();
    for window in existing {
        wm.dispatch(BackendEvent::MapRequest(window));
    }

    tracing::info!(clients = wm.client_count(), "entering event loop");
    // The X11 root window's id, for the click routing below — the one
    // surface identity the shell cannot know (nothing backend-generic
    // could name it), so the binary tells root presses apart from
    // shell-surface clicks before anything reaches the shell.
    let root = wm.backend().root();
    // Reused across iterations so building the wait set is a `clear`
    // and a few pushes rather than an allocation on every wake — see
    // `wait_for_activity`.
    let mut wait_fds: Vec<std::os::unix::io::RawFd> = Vec::new();
    let mut request_poller = SessionRequestPoller::new(Instant::now());
    loop {
        // The cheap request first. A reload keeps every window, every
        // client connection and every dockapp; a restart keeps the
        // windows (via the SaveSet) but costs a process image. Checking
        // reload first means that when both markers somehow exist, the
        // session applies the config it was asked to apply before
        // throwing itself away — the restart then starts from it.
        let requests = request_poller.poll(Instant::now());
        if requests.reload {
            tracing::info!("reload requested — re-reading the config and applying it in place");
            let next = SessionState::resolve(&wm_config::load());
            // Before the shell, so that any application relaunched as a
            // consequence of the new state already sees the new
            // settings. Republishing is free when nothing moved — the
            // manager compares and declines to write.
            published_appearance = next.appearance;
            publish_appearance(&mut xsettings, &next);
            shell.apply_session_state(&mut wm, next);
        }

        if requests.restart {
            tracing::info!("restart requested — re-executing in place");
            // Dockapps first: `restart_in_place` never returns, and the
            // replacement process has to be told which of them are still
            // out there. `Restarting` leaves them running and hands
            // their tokens forward, so they are readopted rather than
            // relaunched. See `Shell::shut_down`.
            shell.shut_down(Farewell::Restarting);
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
                if let Some(resolution) = shell.keymap_action(combo) {
                    // An action observes the same ordering rule as any
                    // other non-motion event: the held-back motion
                    // commits first, so e.g. a focus-follows-mouse focus
                    // change from this same burst lands before an action
                    // that targets the focused client.
                    if let Some(motion) = pending_motion.take() {
                        dispatch_motion(&mut wm, &mut shell, motion);
                    }
                    let outcome = match resolution {
                        chonk_shell::shell::KeyResolution::Action(action) => {
                            shell.run_action(&mut wm, &action)
                        }
                        chonk_shell::shell::KeyResolution::Menu(key) => {
                            shell.run_menu_key(&mut wm, key)
                        }
                        chonk_shell::shell::KeyResolution::Consumed => {
                            chonk_shell::shell::ShellOutcome::Continue
                        }
                    };
                    if exit_requested(&mut shell, outcome) {
                        should_exit = true;
                    }
                    continue;
                }
            }
            if let BackendEvent::KeyRelease(combo) = &event {
                if let Some(action) = shell.keymap_release_action(combo) {
                    if let Some(motion) = pending_motion.take() {
                        dispatch_motion(&mut wm, &mut shell, motion);
                    }
                    let outcome = shell.run_action(&mut wm, &action);
                    if exit_requested(&mut shell, outcome) {
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
            if exit_requested(&mut shell, outcome) {
                should_exit = true;
            }
        }
        if should_exit {
            tracing::info!("exit requested from root menu, shutting down");
            break;
        }

        // Scroll drains beside the clicks, and separately from them:
        // X11 reports a wheel as button 4/5 press/release pairs, which
        // the backend turns into notch counts rather than clicks (see
        // `Backend::take_shell_scroll`), so nothing here can
        // double-count the same physical input. Queued rather than
        // coalesced, unlike motion, because every notch is its own
        // command — three notches on a volume tile is three steps, and
        // keeping only the last would swallow input the user gave.
        while let Some((surface, local, delta)) = wm.backend_mut().take_shell_scroll() {
            shell.on_shell_scroll(&mut wm, surface, local, delta);
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
        // Interactive resize/move invalidations are coalesced across this
        // entire input burst and rasterized once at its display boundary.
        wm.flush_decorations();

        // The tick above may have consumed an appearance-request and
        // switched the session's mode; XSETTINGS is the binary's to
        // publish (the manager lives here), so mirror the change out
        // to X clients. Comparing first keeps this a no-op integer
        // check on the hot path.
        if shell.appearance() != published_appearance {
            published_appearance = shell.appearance();
            publish_appearance(&mut xsettings, shell.session_state());
        }

        // Blocks until the X11 socket actually has something to read,
        // instead of a fixed sleep — the entire reason drags/resizes
        // used to feel like they were catching up to the cursor in
        // steps: with a flat `sleep(100ms)` here, no input got
        // processed for up to 100ms at a time no matter how fast the
        // pointer was moving. The shell supplies the bound: exact
        // menu/dockapp deadlines win, with a conservative idle ceiling
        // for sampler results and marker files. Real input wakes this
        // immediately regardless.
        // The X socket plus everything the shell is waiting on that
        // the display server knows nothing about: the dockapp
        // listener, and one fd per connected dockapp. Rebuilt every
        // pass because the set changes as dockapps connect, die and
        // restart — a stale fd here is at best a spurious wakeup, and
        // at worst a wait on a descriptor this process has since reused
        // for something else. The `Vec` lives outside the loop so the
        // rebuild is a `clear` and some pushes rather than an
        // allocation on every wake.
        wait_fds.clear();
        wait_fds.push(wm.backend().connection_fd());
        shell.extend_extra_poll_fds(&mut wait_fds);
        let now = Instant::now();
        let wait = shell
            .next_housekeeping_in(now)
            .min(request_poller.next_deadline().saturating_duration_since(now));
        wait_for_activity(&wait_fds, wait);
    }

    // Whatever ended the loop — the root menu's Exit, a lost display —
    // the dockapps this session launched are its responsibility and
    // nothing else will collect them. Nothing is handed forward: there
    // is no incoming shell to hand it to.
    shell.shut_down(Farewell::SessionOver);
}

/// Blocks the calling thread until one of `fds` is readable or
/// `timeout` elapses, whichever comes first — the integration pattern
/// x11rb's own `event_loop_integration` module docs recommend for
/// exactly this (`conn.stream().as_raw_fd()` + an external `poll`),
/// rather than guessing at a fixed sleep duration that either wastes
/// latency (too long) or busy-loops (too short).
///
/// It takes a *set* of descriptors because the X socket is no longer
/// the only thing this session waits on: an out-of-process dock tile
/// pushes its pixels down a Unix socket the display server knows
/// nothing about, and a frame that arrived has to wake this loop the
/// same way an X event does. That is the entire X11-side cost of
/// dockapps — everything else about them is backend-generic, because a
/// dockapp is not a display-server client at all.
///
/// Nothing is read here. `poll` only says *that* something is ready;
/// which fd it was does not matter, because the loop's next pass
/// services every dockapp regardless and each of those reads until
/// `EAGAIN`. Skipping the readiness bookkeeping costs one wasted pass
/// over a handful of sockets and saves this function from having to
/// know what any of them are.
fn wait_for_activity(fds: &[std::os::unix::io::RawFd], timeout: Duration) {
    if fds.is_empty() {
        return;
    }
    let mut poll_fds: Vec<libc::pollfd> =
        fds.iter().map(|&fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 }).collect();
    // SAFETY: `poll_fds` is a valid array of exactly the length passed
    // as `nfds`; `poll` only reads/writes through that pointer for the
    // duration of the call.
    unsafe {
        libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as libc::nfds_t, timeout.as_millis() as i32);
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
fn exit_requested(shell: &mut Shell<X11Backend>, outcome: ShellOutcome) -> bool {
    match outcome {
        ShellOutcome::Continue => false,
        ShellOutcome::Exit => true,
        // The exact path `scripts/restart.sh` takes: re-exec the
        // on-disk binary in place, windows surviving via the X11
        // SaveSet — which is also what makes the theme menu (and a
        // bound `Restart` action) the config hot-reload gesture.
        ShellOutcome::Restart => {
            // `restart_in_place` never returns, so the outgoing shell
            // has to let go of its dockapps first — leaving them
            // running, and handing their tokens to the process that is
            // about to replace it. See `Shell::shut_down`.
            shell.shut_down(Farewell::Restarting);
            restart_in_place()
        }
    }
}

/// Publishes this session's scale and theme to every X client through
/// XSETTINGS, if this session managed to become the settings manager.
///
/// Takes the whole [`SessionState`] rather than a scale so that the
/// call site cannot drift from what was actually applied, and so a
/// later setting worth publishing (a font, an icon theme) is added in
/// one place.
fn publish_appearance(manager: &mut Option<XSettingsManager>, state: &SessionState) {
    let Some(manager) = manager.as_mut() else {
        return;
    };
    // Scale and DPI always; a theme name only when it is *true*. The
    // original rule here was "say the true things about DPI and say
    // nothing about taste", because publishing a theme name GTK cannot
    // find (chonkstep ships no GTK theme) makes every GTK client fall
    // back to its default while overriding the user's own
    // `gtk-3.0/settings.ini`. The light/dark appearance axis earns a
    // narrow exception: when a known light/dark GTK theme *pair* is
    // verifiably installed (`chonk_shell::appearance::gtk_theme_name`
    // checks the theme directories for real gtk-3.0/gtk-4.0 payloads —
    // Adwaita/Adwaita-dark on a stock system), the member matching the
    // session's mode is published so X11/XWayland GTK clients follow a
    // switch live via XSETTINGS. No pair installed, no name — exactly
    // the old behavior.
    let theme_name = chonk_shell::appearance::gtk_theme_name(state.appearance).unwrap_or("");
    let appearance = DesktopAppearance::new(state.scale, theme_name);
    match manager.publish_appearance(&appearance) {
        // `false` means nothing moved and nothing was written, which is
        // the common case on a reload that changed something else.
        Ok(changed) => {
            if changed {
                tracing::info!(scale = state.scale, "published XSETTINGS to X clients");
            }
        }
        Err(e) => tracing::warn!(?e, "failed to publish XSETTINGS"),
    }
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
    // Tell the replacement it is a continuation of a session that
    // never really ended — every client survives this exec via the
    // SaveSet — so session-layout restore must not relaunch the
    // recorded applications on top of their own live windows. See
    // `chonk_shell::startup::session_continues`.
    let err = std::process::Command::new(&bin).env("CHONKSTEP_SESSION_CONTINUES", "1").exec();
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
        assert!(!example.restore_session, "example must leave restore_session at its default (commented out)");
        assert!(example.lock_command.is_none(), "example must leave lock_command unset (commented out)");
        assert_eq!(
            example.decorations, defaults.decorations,
            "example must leave [decorations] at its default (commented out) — and the commented line must restate it"
        );

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
