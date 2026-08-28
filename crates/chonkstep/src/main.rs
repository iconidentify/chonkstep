mod desktop;
mod spawn;
mod theme_select;
mod wallpaper;
mod widgets;

use std::collections::HashMap;
use std::time::Duration;

use wm_config::Action;
use wm_core::{Backend, BackendEvent, FocusPolicy, KeyCombo, MouseButton, Notification, WindowManager};
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
// Font and geometry are deliberately *not* per-theme: every theme keeps
// the same terminal font, only its colors change. `pixelsize` tracks
// CHONKSTEP_SCALE (16px at 1x) the same way the WM's own chrome does.
const TERMINAL_FONT_BASE_PX: f32 = 16.0;
// Cells, not pixels — sized so the resulting window still fits the
// screen at CHONKSTEP_SCALE 2 on a 1920-wide display (the old 110x32
// exceeded it once the font scaled up).
const TERMINAL_GEOMETRY: &str = "92x26";

/// urxvt argument list for the active theme's terminal palette —
/// foreground/background/cursor plus the full 16-slot ANSI set, so
/// every theme restyles terminals along with the chrome. The scale for
/// the font size is recovered from the already-scaled theme (titlebar
/// font is 12px at 1x) rather than re-reading the environment.
fn terminal_args(theme: &Theme) -> Vec<String> {
    let hex = |c: wm_theme::model::Color| format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    let px = (theme.titlebar.font.size / 12.0 * TERMINAL_FONT_BASE_PX).round().max(8.0) as u32;
    let mut args = vec![
        "-fn".to_string(),
        format!("xft:JetBrainsMono Nerd Font:pixelsize={px},xft:Noto Sans Symbols 2:pixelsize={px}"),
        "-geometry".to_string(),
        TERMINAL_GEOMETRY.to_string(),
        "-fg".to_string(),
        hex(theme.terminal.fg),
        "-bg".to_string(),
        // Deliberately opaque: the theme's glass opacity is applied by
        // the compositor to the whole frame (`add_opacity_rule` in
        // `wm-x11`), not by the terminal itself. Client-side alpha via
        // a 32-bit visual was tried first and reverted: urxvt leaves
        // stale framebuffer garbage in regions it fails to repaint on
        // scroll/resize, so rows flickered between glass, garbage, and
        // fully transparent (confirmed live).
        hex(theme.terminal.bg),
        "-cr".to_string(),
        hex(theme.terminal.cursor),
    ];
    for (index, color) in theme.terminal.ansi.iter().enumerate() {
        args.push(format!("--color{index}"));
        args.push(hex(*color));
    }
    args
}

/// Launches the theme-styled terminal — the one path shared by the root
/// menu's Terminal item and the `spawn-terminal` keybinding, so the two
/// gestures can never drift apart on font, geometry, or palette.
fn spawn_terminal(theme: &Theme) {
    let args = terminal_args(theme);
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn::spawn_detached("urxvt", &arg_refs);
}

fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "chonkstep starting \u{2014} a modern window manager with WindowMaker parity"
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

    let mut desktop = Desktop::new(&mut backend, screen, scale, theme.id.clone());

    // The wallpaper pixmap `Desktop::new` just published dies with the
    // previous process's X connection on every hot-restart, and the
    // session compositor keeps referencing the dead one — compositing
    // translucent windows over black instead of the wallpaper
    // (confirmed live; a fresh picom picked the new pixmap up fine).
    // SIGUSR1 is picom's documented full-reset signal: cheap, safe,
    // and a no-op exit code when no compositor is running.
    spawn::spawn_detached("pkill", &["-USR1", "-x", "picom"]);

    let existing = backend.scan_existing_windows();
    let mut wm = WindowManager::new(backend, Box::new(engine));
    wm.set_workarea(desktop.workarea(screen));
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
    for window in existing {
        wm.dispatch(BackendEvent::MapRequest(window));
    }

    // Combo -> action lookup for the key interception in the event loop
    // below. Built once: the config is immutable for the life of the
    // process — editing the file and running `scripts/restart.sh`
    // re-execs a fresh process that re-reads it, the same hot-reload
    // path everything else uses. Should a combo somehow appear twice,
    // the later binding wins (plain insertion order), matching the
    // intuition that the line further down the file is the correction.
    let keymap: HashMap<KeyCombo, Action> = config.keybindings.iter().cloned().collect();

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
        let mut display_lost = false;
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
                if let Some(action) = keymap.get(combo) {
                    // An action observes the same ordering rule as any
                    // other non-motion event: the held-back motion
                    // commits first, so e.g. a focus-follows-mouse focus
                    // change from this same burst lands before an action
                    // that targets the focused client.
                    if let Some(motion) = pending_motion.take() {
                        dispatch_motion(&mut wm, &mut desktop, &theme, motion);
                    }
                    run_config_action(&mut wm, &theme, action);
                    continue;
                }
            }
            if let Some(motion) = pending_motion.take() {
                dispatch_motion(&mut wm, &mut desktop, &theme, motion);
            }
            wm.dispatch(event);
        }
        if display_lost {
            tracing::error!("display connection lost, exiting");
            break;
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
            if !handle_shell_click(&mut wm, &mut desktop, &theme, scale, window, local, button, pressed) {
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
        // Workspace plumbing between the WM and the dock indicator:
        // drain a click on the indicator into a real switch first, then
        // mirror the authoritative state into the shared cell so
        // `tick_widgets` repaints the tile exactly when it changed.
        if let Some(target) = desktop.take_workspace_request() {
            wm.switch_workspace(target);
        }
        let (current, count) = (wm.current_workspace(), wm.workspace_count());
        desktop.set_workspace_display(wm.backend_mut(), &theme, current, count);
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

/// The UI scale multiplies every pixel dimension in the theme and the
/// shell's own dock/icon chrome — for HiDPI displays (a nested X
/// server has no display-scaling of its own, so the WM's native ~1990s
/// pixel sizes read as tiny on a modern high-density panel).
/// Precedence: the `CHONKSTEP_SCALE` env var wins over the config
/// file's `scale`, which wins over 1.0 (no scaling) — the env var stays
/// on top because scripts (`dev-nested.sh`, per-display session
/// launchers) use it to override a user's baseline per invocation.
fn read_scale_factor(config_scale: Option<f32>) -> f32 {
    resolve_scale(std::env::var("CHONKSTEP_SCALE").ok().as_deref(), config_scale)
}

/// Pure core of [`read_scale_factor`], split out so the precedence
/// rules are unit-testable without mutating process-global env vars
/// (tests share one process and run on parallel threads; `set_var`
/// races between them). A value that fails validation — unparseable,
/// non-finite, zero, or negative — is *skipped*, not clamped: a broken
/// env var falls through to the config value and a broken config value
/// falls through to 1.0, so a typo degrades to the next-best answer
/// instead of either a garbage scale or a dead session.
fn resolve_scale(env: Option<&str>, config: Option<f32>) -> f32 {
    let valid = |s: &f32| s.is_finite() && *s > 0.0;
    env.and_then(|s| s.trim().parse::<f32>().ok())
        .filter(valid)
        .or_else(|| config.filter(valid))
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

/// Whether to switch from the default click-to-focus to focus-follows-
/// mouse. Precedence: the `CHONKSTEP_FOCUS_FOLLOWS_MOUSE` env var wins
/// over the config file's `focus_follows_mouse` — and it wins in *both*
/// directions: setting it to anything other than `1` forces the policy
/// off even when the config asks for it, so a session script can pin
/// either behavior regardless of what the user's config says. Only
/// when the env var is absent entirely does the config value apply.
fn read_focus_follows_mouse(config_value: bool) -> bool {
    resolve_focus_follows_mouse(std::env::var("CHONKSTEP_FOCUS_FOLLOWS_MOUSE").ok().as_deref(), config_value)
}

/// Pure core of [`read_focus_follows_mouse`] — same testability
/// rationale as [`resolve_scale`]: precedence logic stays out of reach
/// of process-global env state so tests can cover every branch without
/// racing each other on `set_var`.
fn resolve_focus_follows_mouse(env: Option<&str>, config: bool) -> bool {
    match env {
        Some(value) => value == "1",
        None => config,
    }
}

/// Resolves the theme this session dresses in. Precedence: the
/// persisted theme-menu choice (`theme_select`'s state file) wins over
/// the config file's `theme`, which wins over the flagship default —
/// the menu is the more recent, more deliberate gesture (picking a
/// theme from it hot-restarts on the spot), so a config line written
/// once must not keep overriding it on every subsequent restart.
///
/// `theme_select::load()` already implements the outer two layers
/// (state file, else flagship); the config layer slots between them
/// here rather than inside `theme_select` because that module is a pure
/// persist/recall mechanism also used by the menu itself — which theme
/// wins at *startup* is session policy, and session policy lives in
/// `main`.
fn resolve_theme(config_theme: Option<&str>) -> Theme {
    if !persisted_theme_choice_exists() {
        if let Some(theme) = config_theme_fallback(config_theme) {
            return theme;
        }
    }
    theme_select::load()
}

/// The config-file layer of [`resolve_theme`], split out (and kept free
/// of filesystem access) so its edges are unit-testable: `None` when no
/// theme is configured, and — critically — `None` with a warning, not a
/// panic or an error, when the configured id names a theme this build
/// does not ship. A misspelled theme must cost the user the flagship
/// look for one session, never the session itself.
fn config_theme_fallback(config_theme: Option<&str>) -> Option<Theme> {
    let requested = config_theme?;
    let theme = wm_theme::default_theme::theme_by_id(requested);
    if theme.is_none() {
        tracing::warn!(theme = requested, "config names an unknown theme; using the default instead");
    }
    theme
}

/// `true` exactly when `theme_select::load()` would return a persisted
/// menu choice rather than its flagship fallback: the state file exists
/// *and* names a theme this build ships (a stale id from another
/// version does not count — it should fall through to the config layer,
/// same as no file at all). The path mirrors `theme_select`'s own
/// `state_path` on purpose and must stay in lockstep with it;
/// `theme_select` deliberately does not expose "was it persisted?"
/// because no other caller distinguishes the fallback from a choice.
fn persisted_theme_choice_exists() -> bool {
    let path = if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
        std::path::PathBuf::from(root).join("chonkstep/theme")
    } else if let Some(home) = std::env::var_os("HOME") {
        std::path::PathBuf::from(home).join(".local/state/chonkstep/theme")
    } else {
        return false;
    };
    std::fs::read_to_string(path)
        .ok()
        .is_some_and(|id| wm_theme::default_theme::theme_by_id(id.trim()).is_some())
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

/// Runs one configured keybinding action (the event loop already
/// resolved the combo). Window-targeted actions operate on the focused
/// client and are silent no-ops when nothing is focused — pressing
/// "close" over an empty desktop should do exactly nothing, not warn.
/// Workspace moves guard the left edge (workspace 0, matching the
/// Clip's rewind arrow); the right edge needs no guard because
/// `switch_workspace` grows the workspace row on demand. The match is
/// deliberately exhaustive: a new `Action` variant in `wm-config` fails
/// compilation here instead of silently binding to nothing.
fn run_config_action(wm: &mut WindowManager<X11Backend>, theme: &Theme, action: &Action) {
    match action {
        Action::SpawnTerminal => spawn_terminal(theme),
        Action::Close => {
            if let Some(id) = wm.focused_client() {
                wm.close_client(id);
            }
        }
        Action::ToggleMaximize => {
            if let Some(id) = wm.focused_client() {
                wm.toggle_maximize_full(id);
            }
        }
        Action::ToggleShade => {
            if let Some(id) = wm.focused_client() {
                wm.toggle_shade(id);
            }
        }
        Action::Miniaturize => {
            if let Some(id) = wm.focused_client() {
                wm.miniaturize(id);
            }
        }
        Action::ToggleFullscreen => {
            if let Some(id) = wm.focused_client() {
                wm.toggle_fullscreen(id);
            }
        }
        Action::WorkspaceNext => wm.switch_workspace(wm.current_workspace() + 1),
        Action::WorkspacePrev => {
            if wm.current_workspace() > 0 {
                wm.switch_workspace(wm.current_workspace() - 1);
            }
        }
        Action::WorkspaceCarryNext => carry_focused_to_workspace(wm, wm.current_workspace() + 1),
        Action::WorkspaceCarryPrev => {
            if wm.current_workspace() > 0 {
                carry_focused_to_workspace(wm, wm.current_workspace() - 1);
            }
        }
        // The exact path `scripts/restart.sh` and the theme menu take:
        // re-exec the on-disk binary in place, windows surviving via
        // the X11 SaveSet — which is also what makes this binding the
        // config hot-reload gesture.
        Action::Restart => restart_in_place(),
    }
}

/// Moves the focused client to `workspace` and follows it there — the
/// keyboard "carry" gesture (real WindowMaker's "move to next/previous
/// workspace with window"). The refocus at the end is load-bearing:
/// `move_client_to_workspace` drops focus the instant the client leaves
/// the active workspace, and without re-focusing after arriving, the
/// second carry press in a row would find nothing focused and silently
/// do nothing — the whole point of the gesture is carrying one window
/// across several workspaces in repeated presses. The refocus rides the
/// public `ActivateRequested` path (the same one a pager's
/// `_NET_ACTIVE_WINDOW` message takes), so it also re-raises — correct
/// here, since the carried window was the focused one to begin with. A
/// no-op with nothing focused.
fn carry_focused_to_workspace(wm: &mut WindowManager<X11Backend>, workspace: usize) {
    let Some(id) = wm.focused_client() else {
        return;
    };
    let Some(window) = wm.client(id).map(|client| client.window) else {
        return;
    };
    wm.move_client_to_workspace(id, workspace);
    wm.switch_workspace(workspace);
    wm.dispatch(BackendEvent::ActivateRequested(window));
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
        Notification::CycleUpdated => {
            if let Some((candidates, selected)) = wm.cycle_state() {
                // Previews are captured once per session (and again only
                // if the candidate set itself changes) — stepping the
                // selection is just a re-render of stored entries.
                let entries = (desktop.switcher_entry_count() != Some(candidates.len())).then(|| {
                    candidates
                        .iter()
                        .map(|(id, title)| wm_theme::switcher::SwitcherEntry { title: title.clone(), preview: wm.client_preview(*id) })
                        .collect()
                });
                desktop.show_switcher(wm.backend_mut(), theme, entries, selected);
            }
        }
        Notification::CycleEnded => desktop.hide_switcher(wm.backend_mut()),
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
    scale: f32,
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

    if window == desktop.clip_window() {
        if pressed && button == MouseButton::Left {
            desktop.click_clip(local);
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
            RootMenuAction::LaunchTerminal => spawn_terminal(theme),
            RootMenuAction::LaunchAbout => {
                spawn::spawn_detached(&about_binary_path(), &[]);
            }
            RootMenuAction::LaunchBrowser => launch_browser(scale),
            RootMenuAction::SetWallpaper(wallpaper) => {
                desktop.set_wallpaper(wm.backend_mut(), theme, wallpaper);
            }
            RootMenuAction::SetTheme(id) => {
                if let Err(e) = theme_select::persist(id) {
                    tracing::warn!(?e, id, "failed to persist theme selection");
                }
                // A theme implies its wallpaper — persist that too, so
                // the fresh process composes the full look. The
                // Wallpaper menu can still override it afterward.
                if let Some(pack) = wm_theme::default_theme::theme_by_id(id) {
                    if let Some(wallpaper) = wallpaper::Wallpaper::from_id(&pack.wallpaper) {
                        if let Err(e) = wallpaper.persist() {
                            tracing::warn!(?e, id, "failed to persist theme wallpaper");
                        }
                    }
                }
                tracing::info!(theme = id, "theme selected \u{2014} hot-restarting in place to apply");
                restart_in_place();
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

/// Launches the system browser at this desktop's resolved scale (times
/// [`BROWSER_SCALE_FACTOR`]) — Microsoft Edge is a third-party,
/// chonkstep-unaware binary, so unlike `chonk-about` (a native app that
/// reads the scale itself via `chonk-ui::scale_factor`), it has to be
/// told through the flags/env vars its own toolkit understands. See
/// `spawn::chromium_scale_args`/`spawn::gtk_qt_scale_env` — the same two
/// calls are the whole recipe for scaling *any* future external app the
/// menu grows to launch, not just this one. The scale is passed in
/// rather than re-read from the environment so the config file's
/// `scale` fallback (env-less launches) applies here exactly as it does
/// to the WM's own chrome.
fn launch_browser(desktop_scale: f32) {
    let scale = desktop_scale * BROWSER_SCALE_FACTOR;
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

// The precedence helpers are tested through their pure cores
// (`resolve_scale`, `resolve_focus_follows_mouse`,
// `config_theme_fallback`) rather than the env-reading wrappers: tests
// share one process and run on parallel threads, so `set_var`-based
// tests race each other — the split exists precisely so every branch is
// reachable without touching process-global state.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_scale_wins_over_config_scale() {
        assert_eq!(resolve_scale(Some("2.0"), Some(1.5)), 2.0);
    }

    #[test]
    fn config_scale_applies_without_env() {
        assert_eq!(resolve_scale(None, Some(1.5)), 1.5);
    }

    #[test]
    fn scale_defaults_to_one_with_neither_source() {
        assert_eq!(resolve_scale(None, None), 1.0);
    }

    #[test]
    fn unparseable_env_scale_falls_through_to_config() {
        // A typo'd env var must degrade to the next-best answer, not to
        // the hard default — the config value is still a deliberate
        // user choice.
        assert_eq!(resolve_scale(Some("garbage"), Some(1.5)), 1.5);
    }

    #[test]
    fn out_of_range_env_scale_falls_through_to_config() {
        assert_eq!(resolve_scale(Some("0"), Some(1.5)), 1.5);
        assert_eq!(resolve_scale(Some("-2"), Some(1.5)), 1.5);
        // `parse::<f32>` accepts these spellings; the validity filter
        // must still reject them (a NaN scale poisons every pixel
        // computation downstream).
        assert_eq!(resolve_scale(Some("inf"), Some(1.5)), 1.5);
        assert_eq!(resolve_scale(Some("NaN"), Some(1.5)), 1.5);
    }

    #[test]
    fn invalid_config_scale_falls_through_to_default() {
        assert_eq!(resolve_scale(None, Some(0.0)), 1.0);
        assert_eq!(resolve_scale(None, Some(-1.0)), 1.0);
        assert_eq!(resolve_scale(None, Some(f32::NAN)), 1.0);
        assert_eq!(resolve_scale(None, Some(f32::INFINITY)), 1.0);
    }

    #[test]
    fn both_sources_invalid_still_yields_a_usable_scale() {
        // The end-to-end graceful-degradation guarantee: no combination
        // of broken inputs may leave the session without a scale.
        assert_eq!(resolve_scale(Some("bogus"), Some(-3.0)), 1.0);
    }

    #[test]
    fn whitespace_around_env_scale_is_tolerated() {
        assert_eq!(resolve_scale(Some(" 1.5 "), None), 1.5);
    }

    #[test]
    fn env_focus_var_enables_over_config_off() {
        assert!(resolve_focus_follows_mouse(Some("1"), false));
    }

    #[test]
    fn env_focus_var_disables_over_config_on() {
        // The env var wins in BOTH directions: any present value other
        // than "1" pins the policy off regardless of the config.
        assert!(!resolve_focus_follows_mouse(Some("0"), true));
        assert!(!resolve_focus_follows_mouse(Some(""), true));
        assert!(!resolve_focus_follows_mouse(Some("true"), true));
    }

    #[test]
    fn config_focus_value_applies_without_env() {
        assert!(resolve_focus_follows_mouse(None, true));
        assert!(!resolve_focus_follows_mouse(None, false));
    }

    #[test]
    fn config_theme_resolves_known_ids() {
        for id in ["window-maker", "amber-phosphor", "teal-blueprint", "graphite", "next-lavender"] {
            let theme = config_theme_fallback(Some(id));
            assert_eq!(theme.map(|t| t.id), Some(id.to_string()), "built-in theme id {id} must resolve from config");
        }
    }

    #[test]
    fn unknown_config_theme_degrades_to_none() {
        // `resolve_theme` then falls through to the flagship — a
        // misspelled theme name must never cost the user the session.
        assert!(config_theme_fallback(Some("no-such-theme")).is_none());
        assert!(config_theme_fallback(Some("")).is_none());
    }

    #[test]
    fn absent_config_theme_is_not_an_error() {
        assert!(config_theme_fallback(None).is_none());
    }

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
