mod desktop;
mod spawn;
mod wallpaper;
mod widgets;

use std::time::Duration;

use wm_core::{Backend, BackendEvent, FocusPolicy, MouseButton, Notification, WindowManager};
use wm_theme::{RasterThemeEngine, Theme};
use wm_theme_api::Point;
use wm_x11::X11Backend;
use x11rb::protocol::xproto::Window;

use desktop::{Desktop, IconDragResult, RootMenuAction};

/// `urxvt`'s own default size (80x24, negotiated correctly through
/// normal ICCCM size hints) is already reasonable, and — unlike
/// alacritty, see the git history around this line for the saga — it
/// reliably relayouts its content to match a real resize (confirmed
/// live: resizing it externally correctly grows its reported terminal
/// grid). So this needs no default-size workaround at all; just a
/// legible font and a roomy geometry passed directly at launch.
// Fallback chain for glyphs the primary font's own Nerd Font icon patch
// doesn't cover (some file-type icons in `ls`/`eza`-style aliases
// rendered as an empty tofu box otherwise) — urxvt's `-fn` list is
// consulted in order for whatever glyph the first font is missing.
// Deliberately does *not* include `Noto Color Emoji`: tried it, and
// urxvt (a classic Xft-based terminal, not GPU-accelerated — it has no
// color-glyph rendering path the way alacritty/kitty do) doesn't just
// fail to show emoji with it, it visibly corrupts nearby rendering
// (a solid black rectangle over adjacent text, confirmed live). Emoji
// support isn't something urxvt can do; leaving them unrendered is the
// non-broken outcome.
//
// The 16-color ANSI palette (`--color0`..`--color15`) plus fg/bg/cursor
// match the theme this desktop's apps already use elsewhere (same
// values as the old alacritty config's `[colors]` section) rather than
// urxvt's own bland stock scheme.
const TERMINAL_ARGS: &[&str] = &[
    "-fn",
    "xft:JetBrainsMono Nerd Font:pixelsize=32,xft:Noto Sans Symbols 2:pixelsize=32",
    "-geometry",
    "110x32",
    "-fg",
    "#e2e8f0",
    "-bg",
    "#0b1220",
    "-cr",
    "#e2e8f0",
    "--color0",
    "#0b1220",
    "--color1",
    "#ef4444",
    "--color2",
    "#22c55e",
    "--color3",
    "#f59e0b",
    "--color4",
    "#3b82f6",
    "--color5",
    "#d946ef",
    "--color6",
    "#06b6d4",
    "--color7",
    "#e2e8f0",
    "--color8",
    "#475569",
    "--color9",
    "#f87171",
    "--color10",
    "#4ade80",
    "--color11",
    "#fde068",
    "--color12",
    "#60a5fa",
    "--color13",
    "#f0abfc",
    "--color14",
    "#67e8f9",
    "--color15",
    "#f8fafc",
];

fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "chonkstep starting \u{2014} a modern window manager with WindowMaker parity"
    );

    let scale = read_scale_factor();
    tracing::info!(scale, "UI scale (set CHONKSTEP_SCALE to change)");
    ensure_xcursor_size(scale);

    let mut backend = match X11Backend::connect_and_become_wm(None, scale) {
        Ok(b) => b,
        Err(e) => {
            tracing::error!(?e, "failed to start");
            std::process::exit(1);
        }
    };

    let screen = backend.screen_size();

    let theme = wm_theme::default_theme::nextstep_classic().scaled(scale);
    let engine = RasterThemeEngine::new(theme.clone());

    let mut desktop = Desktop::new(&mut backend, screen, scale);

    let existing = backend.scan_existing_windows();
    let mut wm = WindowManager::new(backend, Box::new(engine));
    wm.set_workarea(desktop.workarea(screen));
    wm.bind_default_keys();
    if read_focus_follows_mouse() {
        tracing::info!("focus-follows-mouse enabled (CHONKSTEP_FOCUS_FOLLOWS_MOUSE=1)");
        wm.set_focus_policy(FocusPolicy::FocusFollowsMouse);
    }
    for window in existing {
        wm.dispatch(BackendEvent::MapRequest(window));
    }

    tracing::info!(clients = wm.client_count(), "entering event loop");
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
        while let Some(event) = wm.backend_mut().poll_event() {
            if matches!(event, BackendEvent::PointerMotion { .. }) {
                pending_motion = Some(event);
                continue;
            }
            if let Some(motion) = pending_motion.take() {
                dispatch_motion(&mut wm, &mut desktop, &theme, motion);
            }
            wm.dispatch(event);
        }
        if let Some(motion) = pending_motion.take() {
            dispatch_motion(&mut wm, &mut desktop, &theme, motion);
        }

        while let Some(notification) = wm.take_notification() {
            handle_notification(&mut wm, &mut desktop, &theme, notification);
        }

        if let Some(new_size) = wm.backend_mut().take_screen_resize() {
            tracing::info!(width = new_size.w, height = new_size.h, "screen resized");
            desktop.resize_to_screen(wm.backend_mut(), &theme, new_size);
            wm.set_workarea(desktop.workarea(new_size));
        }

        let mut should_exit = false;
        while let Some((window, local, button, pressed)) = wm.backend_mut().take_shell_click() {
            if !handle_shell_click(&mut wm, &mut desktop, &theme, window, local, button, pressed) {
                should_exit = true;
            }
        }
        if should_exit {
            tracing::info!("exit requested from root menu, shutting down");
            break;
        }

        if let Some((window, local)) = wm.backend_mut().take_shell_motion() {
            desktop.hover_menu(wm.backend_mut(), &theme, window, local);
        }
        desktop.tick_menu(wm.backend_mut(), &theme);
        desktop.tick_widgets(wm.backend_mut(), &theme);

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
/// activity at all, to run `tick_menu`/`tick_clock`/`restart_requested`
/// — ~60Hz, far more than any of those actually need, but cheap and
/// keeps them feeling responsive rather than picking a number tied to
/// any one of their specific timing requirements.
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

/// `CHONKSTEP_SCALE` multiplies every pixel dimension in the theme and
/// the shell's own dock/icon chrome — for HiDPI displays (a nested X
/// server has no display-scaling of its own, so the WM's native ~1990s
/// pixel sizes read as tiny on a modern high-density panel). Defaults to
/// 1.0 (no scaling); `scripts/dev-nested.sh` sets a friendlier default.
fn read_scale_factor() -> f32 {
    std::env::var("CHONKSTEP_SCALE")
        .ok()
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .unwrap_or(1.0)
}

/// Sets `XCURSOR_SIZE` on *this* process (so it's inherited by every
/// app chonkstep spawns, and by the hot-restarted process itself — see
/// `restart_in_place`, which `exec`s in place and so keeps whatever env
/// this process already has) unless it's already set. chonkstep scales
/// its own chrome, decorations, and root cursor by `CHONKSTEP_SCALE`
/// (see `wm-x11`'s `create_scaled_cursor`), but apps that draw their
/// *own* cursor via the standard Xcursor mechanism (most modern
/// toolkits — GTK, Qt, winit) have no way to know about that; they read
/// this env var instead. Without it, such an app's cursor stays
/// whatever Xcursor's own DPI-unaware default is — visibly out of
/// proportion the instant the pointer crosses from chonkstep's chrome
/// onto that app's own content. Doing this here, not only in
/// `scripts/xsession.sh`, means it applies from the very first launch
/// (including inside `dev-nested.sh`) rather than depending on every
/// launcher remembering to set it — the shell scripts still set it too,
/// for the rare case something else spawns before chonkstep does, but
/// this is the one guaranteed to actually run.
fn ensure_xcursor_size(scale: f32) {
    if std::env::var_os("XCURSOR_SIZE").is_some() {
        return;
    }
    let size = (24.0 * scale).round().max(1.0) as u32;
    // SAFETY: called once, at the very start of `main`, before any
    // other thread exists — no concurrent env access is possible yet.
    unsafe {
        std::env::set_var("XCURSOR_SIZE", size.to_string());
    }
}

/// `CHONKSTEP_FOCUS_FOLLOWS_MOUSE=1` switches from the default click-
/// to-focus to focus-follows-mouse — a real preferences UI is future
/// work; an env var is enough to prove and use the underlying
/// `wm-core` policy today.
fn read_focus_follows_mouse() -> bool {
    std::env::var("CHONKSTEP_FOCUS_FOLLOWS_MOUSE").is_ok_and(|v| v == "1")
}

/// Feeds one (already-coalesced) `PointerMotion` event to the icon drag
/// tracker, the dock widget drag tracker, and `wm-core` itself — pulled
/// out of the main loop's drain since it's needed at two points there
/// (mid-burst, when a non-motion event follows one, and once more after
/// the burst ends).
fn dispatch_motion(wm: &mut WindowManager<X11Backend>, desktop: &mut Desktop, theme: &Theme, event: BackendEvent<wm_x11::XWindow, wm_x11::XFrame>) {
    if let BackendEvent::PointerMotion { root, .. } = &event {
        desktop.drag_icon_motion(wm.backend_mut(), *root);
        desktop.drag_widget_motion(wm.backend_mut(), theme, *root);
    }
    wm.dispatch(event);
}

/// Reacts to a `wm-core` state change the shell needs to know about but
/// that `wm-core` itself has no opinion on — currently just icon tiles
/// for miniaturized windows.
fn handle_notification(wm: &mut WindowManager<X11Backend>, desktop: &mut Desktop, theme: &Theme, notification: Notification) {
    match notification {
        Notification::Miniaturized(id, preview) => {
            let title = wm.client(id).map(|c| c.title.clone()).unwrap_or_default();
            desktop.show_icon(wm.backend_mut(), theme, id, &title, preview.as_ref());
        }
        Notification::Deminiaturized(id) | Notification::Removed(id) => {
            desktop.remove_icon_for_client(wm.backend_mut(), id);
        }
        Notification::Mapped(_) => {}
    }
}

/// Returns `false` if the root menu's Exit item was chosen.
///
/// Root reacts on *press* (a context menu should appear the instant you
/// press the button, same as everywhere else) — everything else
/// (restoring an icon, picking a menu item) commits on *release*,
/// matching the arm-on-press/commit-on-release convention every button
/// in this theme follows. Without an explicit pointer grab while the
/// menu is open (see `Desktop::open_root_menu`), release events for a
/// held button would keep reporting against whatever window the press
/// landed on — X11's implicit grab — rather than the menu now under the
/// pointer; that grab is what makes press-drag-release-to-pick work.
fn handle_shell_click(
    wm: &mut WindowManager<X11Backend>,
    desktop: &mut Desktop,
    theme: &Theme,
    window: Window,
    local: Point,
    button: MouseButton,
    pressed: bool,
) -> bool {
    let root = wm.backend().root();

    if window == root {
        if pressed {
            if button == MouseButton::Right {
                desktop.open_root_menu(wm.backend_mut(), theme, local);
            } else {
                desktop.close_menu(wm.backend_mut());
            }
        }
        return true;
    }

    if window == desktop.dock_window() {
        // Middle-click-drag on a widget picks it up for reordering; see
        // `Desktop::begin_widget_drag`/`drag_widget_motion` (the latter
        // fires from `dispatch_motion` on every pointer move, not from
        // here). A plain left click instead fires the widget's own
        // click behavior (e.g. `SysMonWidget` toggling its analog/
        // dashboard face). Everything else on the dock is still just a
        // click-through identity tile.
        match button {
            MouseButton::Middle => {
                if pressed {
                    desktop.begin_widget_drag(wm.backend_mut(), theme, local);
                } else {
                    desktop.end_widget_drag(wm.backend_mut(), theme);
                }
            }
            MouseButton::Left if pressed => {
                desktop.click_widget(wm.backend_mut(), theme, local);
            }
            _ => {}
        }
        return true;
    }

    // Every press on an icon tile arms a potential drag (see
    // `Desktop::begin_icon_drag`); it's resolved into either a restore
    // or a reposition on release, whichever `end_icon_drag` decides
    // based on whether the pointer actually moved.
    if pressed {
        desktop.begin_icon_drag(wm.backend_mut(), window, local);
        return true;
    }

    if let Some(result) = desktop.end_icon_drag(wm.backend_mut()) {
        if let IconDragResult::Restore(client_id) = result {
            wm.deminiaturize(client_id);
        }
        return true;
    }

    if let Some(action) = desktop.click_menu(wm.backend_mut(), theme, window, local) {
        match action {
            RootMenuAction::LaunchTerminal => {
                spawn::spawn_detached("urxvt", TERMINAL_ARGS);
            }
            RootMenuAction::LaunchAbout => {
                spawn::spawn_detached(&about_binary_path(), &[]);
            }
            RootMenuAction::LaunchBrowser => launch_browser(),
            RootMenuAction::SetWallpaper(wallpaper) => {
                desktop.set_wallpaper(wm.backend_mut(), theme, wallpaper);
            }
            RootMenuAction::Exit => return false,
        }
    }

    true
}

/// Path to the `chonk-about` demo binary — resolved relative to
/// chonkstep's own running binary (`chonk-about` always builds into the
/// same output directory as `chonkstep` itself, debug or release), not
/// the process's current working directory. A real xsession launched
/// by a display manager has no reason for `cwd` to be sitting inside
/// this project's checkout — the previous relative-path version only
/// ever worked by coincidence, when run from a dev shell already `cd`'d
/// there, and would silently fail to launch anywhere else (a real
/// `scripts/xsession.sh` session included).
fn about_binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("chonk-about")))
        .filter(|p| p.exists())
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "chonk-about".to_string())
}

/// Edge's own UI (toolbar, tabs, page content) reads a touch large next
/// to the rest of chonkstep's chrome at a 1:1 match to `CHONKSTEP_SCALE`
/// — confirmed by eye, not a technical constraint — so its own scale is
/// nudged down slightly rather than matched exactly. Purely a per-app
/// tuning knob: change this one number to adjust, independently of the
/// desktop's own scale.
const BROWSER_SCALE_FACTOR: f32 = 0.85;

/// Launches the system browser at this desktop's `CHONKSTEP_SCALE`
/// (times [`BROWSER_SCALE_FACTOR`]) — Microsoft Edge is a third-party,
/// chonkstep-unaware binary, so unlike `chonk-about` (a native app that
/// reads the scale itself via `chonk-ui::scale_factor`), it has to be
/// told through the flags/env vars its own toolkit understands. See
/// `spawn::chromium_scale_args`/`spawn::gtk_qt_scale_env` — the same two
/// calls are the whole recipe for scaling *any* future external app the
/// menu grows to launch, not just this one.
fn launch_browser() {
    let scale = read_scale_factor() * BROWSER_SCALE_FACTOR;
    let mut args = spawn::chromium_scale_args(scale);
    args.extend(spawn::chromium_avoid_secrets_service_hang_args());
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env = spawn::gtk_qt_scale_env(scale);
    spawn::spawn_detached_with_env("microsoft-edge-stable", &arg_refs, &env);
}

/// Path to the marker file `scripts/restart.sh` touches to ask a
/// running chonkstep to hot-restart itself — polled once per event-loop
/// tick (the loop already sleeps 100ms/iteration, so this adds one
/// cheap `remove_file` attempt to that, not a new busy-loop).
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
/// process's image) rather than `std::env::current_exe()` — deliberately
/// hardcoded, not resolved from the running process, because the whole
/// point is to pick up whatever a `cargo build --release` just put at
/// this path; `current_exe()` on Linux resolves through `/proc/self/exe`,
/// which keeps pointing at the *original* (now-replaced) inode this
/// process was loaded from, not the fresh file now sitting at the same
/// path. Existing client windows survive the swap: they were added to
/// the X11 SaveSet in `create_decoration`, so when this process's X
/// connection closes (implied by `exec`, which closes non-inherited
/// fds), the server reparents them straight back to root instead of
/// destroying them — the freshly exec'd process then finds them again
/// via `scan_existing_windows` and redecorates them, same as a normal
/// startup.
fn restart_in_place() -> ! {
    use std::os::unix::process::CommandExt;
    const BIN: &str = "/home/chrisk/chonkstep/target/release/chonkstep";
    let err = std::process::Command::new(BIN).exec();
    tracing::error!(?err, bin = BIN, "re-exec failed; exiting instead of restarting");
    std::process::exit(1);
}
