//! The shell orchestrator: one backend-generic [`Shell`] both binaries
//! (the X11 `chonkstep` and the Wayland compositor) drive, extracted
//! whole from the X11 binary's original event loop so the desktop
//! behaves identically on either stack by construction. The split
//! follows one rule: everything that decides what the desktop *does*
//! (menu routing, launcher clicks, icon drags, spawning, widget
//! ticking) lives here; everything about how events physically arrive
//! (polling, `PointerMotion` coalescing) or what only a process can do
//! (exit, re-exec in place) stays in the binary. The seam between the
//! two is [`ShellOutcome`]: the shell reports the process-level act an
//! event calls for, the binary performs it.

use std::collections::HashMap;

use wm_config::Action;
use wm_core::{Backend, BackendEvent, ClientFlags, ClientId, KeyCombo, MonitorInfo, MouseButton, Notification, ScrollDelta, WindowManager};
use wm_theme::{FontState, RasterThemeEngine, Theme};
use wm_theme_api::{DecorationBuffer, Point, PopupHost, Rect, Size};

use crate::apps::{self, AppEntry};
use crate::desktop::{Desktop, IconDragResult, MenuAction, RootMenuAction, WindowMenuAction, WindowMenuContext};
use crate::dockapp::Farewell;
use crate::launchdock::{LaunchDock, LaunchDockAction};
use crate::startup::SessionState;
use crate::widgets::DockInput;
use crate::{spawn, theme_select, wallpaper};

/// What the binary's event loop must do after handing an event to the
/// shell. The shell never exits or re-execs the process itself — those
/// are process-level acts, and how a hot-restart actually happens is a
/// per-binary affair (the X11 binary re-execs its on-disk image and
/// relies on the SaveSet to keep clients alive; a Wayland compositor
/// has its own story) — so the two non-`Continue` outcomes name the
/// act for the binary to carry out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellOutcome {
    /// Nothing process-level to do; keep looping.
    Continue,
    /// The user asked to end the session (the root menu's Exit item).
    Exit,
    /// The process must re-exec its on-disk image.
    ///
    /// This used to mean "apply a change that touches every surface at
    /// once", and a theme pick raised it. It no longer does: a theme,
    /// a UI scale and every config-file setting are applied in place by
    /// [`Shell::apply_session_state`], and the only thing left that a
    /// running process genuinely cannot do to itself is *become a
    /// different build*. So this is now raised by exactly one gesture,
    /// the `restart` keybinding, and means what `scripts/update.sh`
    /// needs it to mean.
    Restart,
}

/// `foot`'s own default size is already reasonable, and it relayouts
/// its content to match a real resize — so this needs no default-size
/// workaround at all; just a legible font and a roomy geometry passed
/// directly at launch. (alacritty was tried before urxvt, see the git
/// history around this line for the resize saga; foot is not that
/// terminal and negotiates xdg-shell configures properly.)
//
// foot rather than urxvt because this desktop runs as a Wayland
// compositor: urxvt is an X11 client and every terminal the shell
// spawned had to detour through XWayland. foot is Wayland-native, so
// the terminal is a first-class `xdg_toplevel` on the same protocol as
// the rest of the session.
//
// Fallback chain for glyphs the primary font's own Nerd Font icon patch
// doesn't cover (some file-type icons in `ls`/`eza`-style aliases
// rendered as an empty tofu box otherwise) — foot's `--font` list is
// consulted in order for whatever glyph the first font is missing, the
// same way urxvt's `-fn` list was. Unlike urxvt (a classic Xft
// terminal with no color-glyph path, which corrupted nearby rendering
// when handed `Noto Color Emoji`), foot does render color emoji from
// its own fontconfig fallback, so nothing needs to be excluded here.
//
// The 16-color ANSI palette (`regular0`..`regular7`, `bright0`..
// `bright7`) plus fg/bg/cursor match the theme this desktop's apps
// already use elsewhere rather than foot's own stock scheme. foot
// takes them as `--override` config keys, and its color values are
// bare RRGGBB with no `#` prefix.
//
// `colors-dark`, not the bare `colors` section: foot 1.27 deprecates
// the latter ("[colors]: deprecated; use [colors-dark] instead") and
// warns once per key, which at twenty keys is twenty lines of noise in
// the session log on every terminal launch. Pinning
// `initial-color-theme=dark` alongside it is what makes the choice
// safe — foot picks `colors-dark` by default, but a user's own
// foot.ini setting `initial-color-theme=light` would otherwise send it
// to a `colors-light` section this desktop never populates, leaving a
// themed terminal wearing foot's stock palette.
// Font and geometry are deliberately *not* per-theme: every theme keeps
// the same terminal font, only its colors change. The size comes from
// the config's `terminal_font_px` (1x pixels) and tracks CHONKSTEP_SCALE
// the same way the WM's own chrome does.
//
// The window is sized in *pixels*, not in cells. It used to be a
// hand-tuned "92x26" pinned to whatever fitted a 1920-wide display at
// scale 2 — but a cell count is only safe while the font size is a
// constant, and the moment the font became a user setting that geometry
// would march off the edge of the screen the first time anyone raised
// it. A fraction of the actual head fits by construction at any font
// size, on any display, and lets the column count be what falls out.
// Wider than tall by less than it looks: the height fraction is the
// larger of the two because the chrome comes off the height and a
// terminal is judged on rows.
const TERMINAL_SCREEN_FRACTION: (f32, f32) = (0.70, 0.78);
// Floor for that fraction, so a small or oddly-shaped head still gets a
// usable terminal rather than a proportionally tiny one.
const TERMINAL_MIN_SIZE: (u32, u32) = (640, 400);

/// foot argument list for the active theme's terminal palette —
/// foreground/background/cursor plus the full 16-slot ANSI set, so
/// every theme restyles terminals along with the chrome. The scale for
/// the font size is recovered from the already-scaled theme (titlebar
/// font is 12px at 1x) rather than re-reading the environment.
fn terminal_args(theme: &Theme, font_px: f32, screen: Size) -> Vec<String> {
    // foot wants bare RRGGBB, not the `#rrggbb` urxvt took.
    let hex = |c: wm_theme::model::Color| format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    let px = (font_px * (theme.titlebar.font.size / 12.0)).round().max(8.0) as u32;
    let (window_w, window_h) = terminal_window_size(theme, screen);
    let mut args = vec![
        "--font".to_string(),
        format!("JetBrainsMono Nerd Font:pixelsize={px},Noto Sans Symbols 2:pixelsize={px}"),
        "--window-size-pixels".to_string(),
        format!("{window_w}x{window_h}"),
        "--override".to_string(),
        "initial-color-theme=dark".to_string(),
        "--override".to_string(),
        format!("colors-dark.foreground={}", hex(theme.terminal.fg)),
        "--override".to_string(),
        format!("colors-dark.background={}", hex(theme.terminal.bg)),
        // `cursor` is a *pair*: the text color drawn inside the cursor
        // block, then the block itself. urxvt's `-cr` set only the
        // block, so the background goes in the text slot to keep the
        // classic reversed look the themes were written against.
        "--override".to_string(),
        format!("colors-dark.cursor={} {}", hex(theme.terminal.bg), hex(theme.terminal.cursor)),
    ];
    // The theme's glass, applied by the terminal itself rather than by
    // a compositor opacity rule. On X11 that rule is what produces
    // translucency (`add_opacity_rule("URxvt", ..)` in the X11
    // binary's main), and client-side alpha was deliberately avoided
    // there because urxvt's 32-bit-visual path left stale framebuffer
    // garbage on scroll/resize. Neither constraint survives the move:
    // there is no per-app opacity rule on the Wayland side at all, so
    // without this the themes' `opacity` would simply do nothing, and
    // foot's own alpha is a clean premultiplied surface the compositor
    // composites correctly.
    if let Some(opacity) = theme.terminal.opacity {
        args.push("--override".to_string());
        args.push(format!("colors-dark.alpha={:.3}", f32::from(opacity) / 100.0));
    }
    for (index, color) in theme.terminal.ansi.iter().enumerate() {
        // 0-7 are the regular ANSI slots, 8-15 the bright ones; the
        // theme stores them as one flat 16-slot array.
        let key = if index < 8 {
            format!("colors-dark.regular{index}")
        } else {
            format!("colors-dark.bright{}", index - 8)
        };
        args.push("--override".to_string());
        args.push(format!("{key}={}", hex(*color)));
    }
    args
}

/// The head a freshly launched terminal has to fit — the primary
/// monitor, the same rectangle every other piece of shell chrome hangs
/// on.
fn terminal_screen<B: Backend + PopupHost<PopupId = B::ShellId>>(shell: &Shell<B>) -> Size {
    shell.desktop.primary_workarea().size
}

/// The terminal's launch size, in pixels, for a given head.
///
/// foot's `--window-size-pixels` sizes the terminal's *own* surface,
/// but what has to fit the screen is the decorated frame — so the
/// chrome the WM is about to wrap around it (titlebar, resizebar, both
/// borders) comes off the height first. Without that subtraction the
/// frame overhangs the bottom of the head by exactly one titlebar,
/// which is the sort of thing nobody notices until the screen is small.
fn terminal_window_size(theme: &Theme, screen: Size) -> (u32, u32) {
    let chrome_h = u32::from(theme.titlebar.height)
        + u32::from(theme.resize_bar.height)
        + 2 * u32::from(theme.border.width);
    let width = ((screen.w as f32 * TERMINAL_SCREEN_FRACTION.0) as u32)
        .max(TERMINAL_MIN_SIZE.0)
        .min(screen.w.max(1));
    let height = ((screen.h as f32 * TERMINAL_SCREEN_FRACTION.1) as u32)
        .max(TERMINAL_MIN_SIZE.1)
        .min(screen.h.saturating_sub(chrome_h).max(1));
    (width, height)
}

/// Launches the theme-styled terminal — the one path shared by the root
/// menu's Terminal item and the `spawn-terminal` keybinding, so the two
/// gestures can never drift apart on font, geometry, or palette.
fn spawn_terminal(theme: &Theme, font_px: f32, screen: Size) {
    spawn_foot(terminal_args(theme, font_px, screen));
}

/// The single foot spawn step: [`spawn_terminal`] passes the themed
/// args alone, [`launch_app`] appends `-e` plus a `.desktop` entry's
/// command line for `Terminal=true` apps. Factored so the two callers
/// can never drift on how the arg list actually reaches the process.
fn spawn_foot(args: Vec<String>) {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn::spawn_detached("foot", &arg_refs);
}

/// Launches one `.desktop` entry — the shared dispatch behind both the
/// root menu's Applications submenu and the launcher dock's tiles, so
/// the two gestures can never disagree on how an entry runs.
/// `Terminal=true` entries run inside the themed terminal, so a TUI app
/// gets the exact font/geometry/palette the Terminal menu item itself
/// would. foot takes the program to exec as its trailing arguments and
/// accepts `-e` as an explicit no-op for xterm compatibility, so the
/// separator is kept: it costs nothing and keeps the command line
/// readable as "terminal options, then the thing to run".
/// An empty parsed command line — a malformed entry the scanner let
/// through — is a logged no-op, never a panic.
fn launch_app(entry: &AppEntry, theme: &Theme, font_px: f32, screen: Size) {
    // Scale recovered from the already-scaled theme (titlebar font is
    // 12px at 1x) — the same trick `terminal_args` uses, so launch
    // fixups need no separate scale plumbing.
    let scale = theme.titlebar.font.size / 12.0;
    let Some((program, args)) = entry.exec.split_first() else {
        tracing::warn!(app = %entry.id, "desktop entry has an empty command line; not launching");
        return;
    };
    if entry.terminal {
        let mut argv = terminal_args(theme, font_px, screen);
        argv.push("-e".to_string());
        argv.extend(entry.exec.iter().cloned());
        spawn_foot(argv);
        return;
    }
    // External GUI launches get the environment/argument fixups the
    // old dedicated browser launcher carried, now applied generically:
    // every app is told the desktop's scale through the GTK/Qt env
    // vars (no XSETTINGS daemon or portal here to advertise it), and
    // the Chromium family additionally gets its own scale flag plus
    // `--password-store=basic` — without which Chromium blocks ~25s at
    // startup on a D-Bus secrets service this session doesn't provide
    // (the whole story lives on the spawn.rs helpers). Confirmed live:
    // the first .desktop-launched Chromium hung exactly that way.
    //
    // The ozone platform is the one fixup that differs between the two
    // stacks, and it is asked for as a question about the session
    // rather than decided here: this function is as backend-blind as
    // the rest of the shell, and `spawn::current_display_stack` is the
    // single place that is allowed to know the answer and says at
    // length why it can be trusted.
    let mut argv: Vec<String> = args.to_vec();
    let base = program.rsplit('/').next().unwrap_or(program);
    // `starts_with`, not `==`, for Edge: every Edge desktop entry on a
    // real installation execs `/usr/bin/microsoft-edge-stable` (the
    // beta and dev channels install `-beta` and `-dev` alongside it),
    // and an exact match on `microsoft-edge` therefore matched no
    // launch this desktop has ever performed. Edge was silently
    // receiving none of these fixups — not the scale flag, not the
    // secrets-service workaround, not the ozone platform — which is a
    // large part of why it behaved worse here than any other browser.
    if base.contains("chrom") || base.contains("chrome") || base.starts_with("microsoft-edge") || base.starts_with("brave") {
        argv.extend(spawn::chromium_scale_args(scale));
        argv.extend(spawn::chromium_avoid_secrets_service_hang_args());
        argv.extend(spawn::chromium_platform_args(spawn::current_display_stack()));
    }
    let arg_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    // Toolkit scaling *and* the pointer size, both in this child's own
    // environment rather than the session's: the scale can change while
    // the session runs, and the process environment cannot safely be
    // rewritten once threads exist. See `startup::xcursor_size_env`.
    let mut env = spawn::gtk_qt_scale_env(scale);
    env.extend(crate::startup::xcursor_size_env(scale));
    spawn::spawn_detached_with_env(program, &arg_refs, &env, &[]);
}

/// Path to the `chonk-about` demo binary — resolved relative to the
/// shell binary's own running image (`chonk-about` always builds into
/// the same output directory, debug or release), not the process's
/// current working directory. A real xsession launched by a display
/// manager has no reason for `cwd` to be sitting inside this project's
/// checkout — the previous relative-path version only ever worked by
/// coincidence, when run from a dev shell already `cd`'d there, and
/// would silently fail to launch anywhere else (a real
/// `scripts/xsession.sh` session included).
fn about_binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join("chonk-about")))
        .filter(|p| p.exists())
        .and_then(|p| p.to_str().map(str::to_string))
        .unwrap_or_else(|| "chonk-about".to_string())
}

/// Combo -> action lookup for [`Shell::keymap_action`]. Built once: the
/// config is immutable for the life of the process — editing the file
/// and hot-restarting re-execs a fresh process that re-reads it, the
/// same hot-reload path everything else uses. Should a combo somehow
/// appear twice, the later binding wins (plain insertion order),
/// matching the intuition that the line further down the file is the
/// correction.
/// Which passive key grabs to release and which to take, moving from
/// the combos currently held to the combos a config now asks for.
///
/// Pure, and separated from the calls it implies, for the reason every
/// resolver in [`crate::startup`] is: the interesting part is the rule,
/// the rule is easy to get subtly wrong (a combo present in both lists
/// must be left alone, not dropped and re-taken), and a test for it
/// should not need a display server.
///
/// A combo bound twice in one config yields one grab: the keymap
/// resolves duplicates by last-one-wins, and grabbing the same combo
/// twice would leave the second grab held after the first is released.
fn grab_delta(previous: &[KeyCombo], next: &[(KeyCombo, Action)]) -> (Vec<KeyCombo>, Vec<KeyCombo>) {
    let mut wanted: Vec<KeyCombo> = next.iter().map(|(combo, _)| *combo).collect();
    wanted.sort_by_key(|combo| (combo.keysym, combo.modifiers.bits()));
    wanted.dedup();
    let to_ungrab = previous.iter().filter(|combo| !wanted.contains(combo)).copied().collect();
    let to_grab = wanted.iter().filter(|combo| !previous.contains(combo)).copied().collect();
    (to_ungrab, to_grab)
}

fn build_keymap(bindings: &[(KeyCombo, Action)]) -> HashMap<KeyCombo, Action> {
    bindings.iter().cloned().collect()
}

/// The outcome half of a root-menu pick, split from the side effects in
/// `run_root_menu_action` so the contract the binary's control flow
/// hangs on — Exit ends the session, SetTheme demands a restart,
/// everything else continues — is pinned by unit test without a
/// backend. Deliberately exhaustive: a new `RootMenuAction` variant
/// fails compilation here instead of silently continuing.
fn root_action_outcome(action: &RootMenuAction) -> ShellOutcome {
    match action {
        RootMenuAction::Exit => ShellOutcome::Exit,
        // A theme redresses every surface at once, and used to need a
        // fresh process to do it. `run_root_menu_action` now applies it
        // in place, so there is nothing left for the binary to carry
        // out.
        RootMenuAction::SetTheme(_) => ShellOutcome::Continue,
        RootMenuAction::LaunchTerminal
        | RootMenuAction::LaunchAbout
        | RootMenuAction::LaunchApp(_)
        | RootMenuAction::SetWallpaper(_) => ShellOutcome::Continue,
    }
}

/// The launcher strip's view of what is currently running: one
/// `(WM_CLASS class, window id)` pair per managed client —
/// `iter_clients` only ever yields live clients, so no lifecycle
/// filtering happens here. The id crosses to the strip and back as a
/// plain `B::WindowId`: the strip hands it straight back through
/// `LaunchDockAction::Focus`, where the dispatch feeds it to the same
/// `ActivateRequested` path a pager's `_NET_ACTIVE_WINDOW` message
/// takes.
fn running_pairs<B: Backend>(wm: &WindowManager<B>) -> Vec<(String, B::WindowId)> {
    wm.iter_clients().map(|(_, client)| (client.class.clone(), client.window)).collect()
}

/// Moves the focused client to `workspace` and follows it there — the
/// keyboard "carry" gesture — move to the next or previous workspace
/// with the window in hand. The refocus at the end is load-bearing:
/// `move_client_to_workspace` drops focus the instant the client leaves
/// the active workspace, and without re-focusing after arriving, the
/// second carry press in a row would find nothing focused and silently
/// do nothing — the whole point of the gesture is carrying one window
/// across several workspaces in repeated presses. The refocus rides the
/// public `ActivateRequested` path (the same one a pager's
/// `_NET_ACTIVE_WINDOW` message takes), so it also re-raises — correct
/// here, since the carried window was the focused one to begin with. A
/// no-op with nothing focused.
/// The primary monitor's rect — where every piece of shell chrome
/// hangs. The `primary` flag decides it, the first entry stands in
/// where the platform named none (matching `Backend::monitors`' own
/// contract), and an origin-anchored rect of the whole screen is the
/// last resort for a backend reporting no monitors at all — which is
/// exactly the single-screen assumption the shell made before it was
/// monitor-aware, so nothing regresses on a backend that never reports
/// one.
fn primary_rect(monitors: &[MonitorInfo], screen: Size) -> Rect {
    monitors
        .iter()
        .find(|m| m.primary)
        .or_else(|| monitors.first())
        .map(|m| m.geometry)
        .unwrap_or(Rect { pos: Point::new(0, 0), size: screen })
}

fn carry_focused_to_workspace<B: Backend>(wm: &mut WindowManager<B>, workspace: usize) {
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

/// The one desktop shell, orchestrated: the Desktop (dock, Clip, root
/// and window menus, icon tiles, wallpaper), the launcher strip, the
/// scanned `.desktop` index, the active theme, and the configured
/// keymap, behind the handful of entry points a backend binary's event
/// loop drives. The fields are exactly the state the original event
/// loop threaded between its handler functions; the methods are those
/// handlers, verbatim in behavior.
pub struct Shell<B: Backend + PopupHost<PopupId = B::ShellId>> {
    desktop: Desktop<B>,
    launchdock: LaunchDock<B>,
    /// The `.desktop` application index, scanned once at startup — one
    /// vec, three consumers that must agree on entry positions: the
    /// desktop keeps a clone for the root menu's Applications submenu
    /// (`RootMenuAction::LaunchApp(i)` indexes it), the launcher dock
    /// resolves its persisted pins against it, and the launch dispatch
    /// in `on_shell_click` indexes this copy again when either of
    /// those fires.
    apps: Vec<AppEntry>,
    /// Everything a live change can alter, as resolved — the source
    /// `theme` below is derived from, and the base every later
    /// [`Shell::apply_session_state`] diffs against.
    state: SessionState,
    /// `state.theme()`, cached: the scaled theme every surface is drawn
    /// from, recomputed only when the state it comes from changes.
    /// `Theme::scaled` clones and re-rounds every metric in the theme,
    /// which is not something to do on the paint path.
    theme: Theme,
    /// The font database the decoration engine rasterizes with, held so
    /// a restyle can build the replacement engine around the *same*
    /// one — see [`FontState`], and `RasterThemeEngine::with_fonts`.
    fonts: FontState,
    /// The config-file key combos this shell currently holds passive
    /// grabs for. Owned here rather than by the binaries because a
    /// reload has to release what the user unbound, which means
    /// knowing what was bound before — and two binaries tracking that
    /// separately is two chances to leak a grab.
    grabbed: Vec<KeyCombo>,
    keymap: HashMap<KeyCombo, Action>,
    /// The pointer's last known root-relative position, recorded by
    /// every [`Shell::on_motion`] call. Shell button events carry only
    /// surface-local coordinates, but the launcher strip's release/pin
    /// decisions need a root position — and the most recent motion is
    /// exact for them in practice, since a drag's release is always
    /// preceded by the motion that put the pointer wherever it is
    /// released. Lives on the struct because the release drains from
    /// `take_shell_click` on a later loop iteration than the motion
    /// that preceded it.
    pointer_root: Point,
}

impl<B: Backend + PopupHost<PopupId = B::ShellId>> Shell<B> {
    /// Builds the whole shell against an already-connected backend:
    /// scans applications, raises the Dock/Clip/launcher chrome,
    /// compiles the configured keymap and takes its key grabs.
    ///
    /// Takes the resolved [`SessionState`] rather than a `Config` plus
    /// a theme plus a scale, so that the values a fresh session starts
    /// from and the values [`Shell::apply_session_state`] moves it to
    /// are the same type, resolved by the same rules. `fonts` is the
    /// font state the caller's decoration engine was built with — the
    /// shell needs it to build that engine's replacements.
    pub fn new(backend: &mut B, state: &SessionState, fonts: FontState) -> Self {
        let theme = state.theme();
        let scale = state.scale;
        let screen = backend.screen_size();
        // Chrome hangs on the primary monitor, not on the screen: the
        // screen spans every output at once, so its own corners are
        // wherever the outermost heads happen to be.
        let primary = primary_rect(&backend.monitors(), screen);

        let apps = apps::scan_applications();
        tracing::info!(count = apps.len(), "application entries scanned");

        let desktop = Desktop::new(backend, screen, primary, scale, theme.id.clone(), apps.clone());
        // The launcher strip below the Clip. Its tile size mirrors
        // `Desktop::new`'s own derivation (56px at 1x, scaled, floored
        // at 16) rather than inventing a second number: the strip's
        // tiles must read as the same family as the Clip above them
        // and the miniwindow icon tiles pins are dropped from.
        // It is handed the *primary's* size rather than the screen's,
        // so the strip's height clamp is measured against the head it
        // sits on rather than against every head at once.
        let launchdock = LaunchDock::new(backend, &theme, primary, crate::desktop::tile_px(scale), &apps);

        // Take the configured grabs through the same delta the applier
        // uses, from an empty starting set: one implementation, so a
        // fresh session and a reloaded one cannot end up holding
        // different grabs for the same config file.
        let (_, to_grab) = grab_delta(&[], &state.keybindings);
        for combo in &to_grab {
            backend.grab_key(*combo);
        }

        Self {
            desktop,
            launchdock,
            apps,
            keymap: build_keymap(&state.keybindings),
            grabbed: to_grab,
            state: state.clone(),
            theme,
            fonts,
            pointer_root: Point::new(0, 0),
        }
    }

    /// Moves this session to `next` — the one path a theme pick, a UI
    /// scale change and a config-file reload all take.
    ///
    /// Ordering is load-bearing and the reason this is one function
    /// rather than a handful the callers compose:
    ///
    /// 1. Policy first, unconditionally. These are plain setters with
    ///    nothing to repaint, and they must land even when the look is
    ///    identical — a reload that only changed `edge_resistance` has
    ///    no theme work to do at all.
    /// 2. Metrics next, before anything paints: `Desktop::set_scale`
    ///    re-derives the tile edge every later step measures against.
    /// 3. The decoration engine, which re-lays-out every managed client
    ///    as part of the swap (`WindowManager::set_theme_engine`).
    /// 4. The shell's own chrome, which is not drawn through that
    ///    engine and so has to be told separately.
    /// 5. Workareas last, because the dock's height is an input to them
    ///    and step 4 is what settles it.
    ///
    /// Dockapps are deliberately absent from that list. They already
    /// poll the tile edge, the scale and the whole theme once per
    /// servicing pass and push a `ThemeChanged` when any of it moves,
    /// so updating `Desktop`'s fields in step 2 *is* telling them —
    /// within one 16ms tick, with no call here that a fourth trigger
    /// could forget to make.
    pub fn apply_session_state(&mut self, wm: &mut WindowManager<B>, next: SessionState) {
        // 1. Policy.
        wm.set_focus_policy(next.focus);
        wm.set_placement_policy(next.placement);
        wm.set_snap_threshold(next.edge_resistance);
        self.keymap = build_keymap(&next.keybindings);
        let (to_ungrab, to_grab) = grab_delta(&self.grabbed, &next.keybindings);
        for combo in &to_ungrab {
            wm.ungrab_key(*combo);
        }
        for combo in &to_grab {
            wm.grab_key(*combo);
        }
        self.grabbed.retain(|combo| !to_ungrab.contains(combo));
        self.grabbed.extend(to_grab);

        // 2. Metrics.
        let theme = next.theme();
        let scale_changed = self.desktop.set_scale(next.scale);
        let theme_changed = theme != self.theme;
        self.state = next;
        if !scale_changed && !theme_changed {
            // Nothing that is drawn has moved. Repainting anyway would
            // be a visible flash on a reload that only rebound a key.
            return;
        }
        self.theme = theme;
        self.desktop.set_theme_id(self.theme.id.clone());
        tracing::info!(
            theme = %self.theme.id,
            scale = self.state.scale,
            scale_changed,
            theme_changed,
            "applying a new look in place"
        );

        // 3. The decoration engine, and with it every client's chrome.
        wm.set_theme_engine(Box::new(RasterThemeEngine::with_fonts(self.theme.clone(), self.fonts.clone())));
        if scale_changed {
            // The only pixels in the session the theme engine does not
            // produce: the backend's own pointer cursors.
            wm.backend_mut().set_ui_scale(self.state.scale);
        }

        // 4. The shell's own chrome. Icon-tile thumbnails are gathered
        //    before the backend is borrowed mutably — see
        //    `Desktop::icon_clients`.
        let previews: Vec<(ClientId, Option<DecorationBuffer>)> = self
            .desktop
            .icon_clients()
            .into_iter()
            .map(|id| (id, wm.client_preview(id)))
            .collect();
        let tile = crate::desktop::tile_px(self.state.scale);
        self.desktop.relayout(wm.backend_mut(), &self.theme, &previews);
        self.launchdock.restyle(wm.backend_mut(), &self.theme, tile);

        // 5. Workareas, now that the dock has settled its height.
        self.apply_workareas(wm);
    }

    /// Dresses the session in a different theme, keeping every other
    /// piece of session state as it is. The theme menu's whole job.
    fn apply_theme(&mut self, wm: &mut WindowManager<B>, base: Theme) {
        let mut next = self.state.clone();
        next.base_theme = base;
        self.apply_session_state(wm, next);
    }

    /// The theme every surface (the shell's own chrome included) is
    /// dressed in — the binary reads it back for the pieces it still
    /// owns (the theme engine, per-app compositor rules).
    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    /// The rectangle managed windows may occupy on the *primary*
    /// monitor — the single-rect form for `WindowManager::set_workarea`,
    /// which means exactly that.
    ///
    /// `_screen` is vestigial: the desktop tracks real monitor geometry
    /// itself now, and on a multi-head session the screen's own size
    /// spans every output at once, which is never the answer to "where
    /// may a window on the primary go". The parameter stays so the
    /// single-monitor callers that pass it keep working unchanged;
    /// anything holding a live `WindowManager` should call
    /// [`Shell::apply_workareas`] instead, which pushes one rect per
    /// monitor.
    pub fn workarea(&self, _screen: Size) -> Rect {
        self.desktop.primary_workarea()
    }

    /// Pushes one workarea per monitor into the WM — the multi-monitor
    /// form of [`Shell::workarea`], and what a backend binary should
    /// call at startup and on every output change. Reads the monitor
    /// list straight from the WM so the rects land in the same
    /// positional order `set_workareas` indexes; a backend reporting no
    /// monitors gets the primary's single rect, which is the whole
    /// screen on such a backend.
    pub fn apply_workareas(&self, wm: &mut WindowManager<B>) {
        let monitors: Vec<Rect> = wm.monitors().into_iter().map(|m| m.geometry).collect();
        let areas = if monitors.is_empty() {
            vec![self.desktop.primary_workarea()]
        } else {
            self.desktop.workareas(&monitors)
        };
        wm.set_workareas(areas);
    }

    /// Resolves a configured key combo to its action, for the binary's
    /// key interception. A miss MUST leave the event flowing through to
    /// `wm-core` unchanged — during a modal Alt+Tab session the
    /// switcher grabs the whole keyboard, and Tab/Escape/any-other-key
    /// arrive as ordinary `KeyPress` events that appear in no keymap;
    /// swallowing them would wedge the switcher open.
    pub fn keymap_action(&self, combo: &KeyCombo) -> Option<Action> {
        self.keymap.get(combo).cloned()
    }

    /// Runs one configured keybinding action (the binary already
    /// resolved the combo). Window-targeted actions operate on the
    /// focused client and are silent no-ops when nothing is focused —
    /// pressing "close" over an empty desktop should do exactly
    /// nothing, not warn. Workspace moves guard the left edge
    /// (workspace 0, matching the Clip's rewind arrow); the right edge
    /// needs no guard because `switch_workspace` grows the workspace
    /// row on demand. The match is deliberately exhaustive: a new
    /// `Action` variant in `wm-config` fails compilation here instead
    /// of silently binding to nothing.
    pub fn run_action(&mut self, wm: &mut WindowManager<B>, action: &Action) -> ShellOutcome {
        match action {
            Action::SpawnTerminal => {
                spawn_terminal(&self.theme, self.state.terminal_font_px, terminal_screen(self))
            }
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
            // Re-read the config file and apply it here and now:
            // theme, UI scale, focus policy, placement, edge resistance
            // and these bindings themselves, with nothing closed and
            // nothing re-execed. A broken file at this point is not
            // fatal and never was — `wm_config::load` warns and hands
            // back the defaults — but note what that means for a live
            // reload specifically: a typo does not leave the session
            // alone, it moves the session to the defaults. That is the
            // same thing a restart with a broken file has always done,
            // and the warning it logs is the same one.
            Action::Reload => {
                self.apply_session_state(wm, SessionState::resolve(&wm_config::load()));
            }
            // Re-exec the on-disk binary. Since `Action::Reload` exists
            // this is no longer the config hot-reload gesture; it is
            // how a session picks up a *new build* of itself, which is
            // the one thing it cannot do without exec.
            Action::Restart => return ShellOutcome::Restart,
        }
        ShellOutcome::Continue
    }

    /// A button press on the desktop background itself. Root reacts on
    /// *press* (a context menu should appear the instant you press the
    /// button, same as everywhere else) — everything else in the shell
    /// (restoring an icon, picking a menu item) commits on *release*,
    /// matching the arm-on-press/commit-on-release convention every
    /// button in this theme follows. Releases on the root are the
    /// binary's to route through [`Shell::on_shell_click`], whose
    /// launcher-release offer must still see them (see its doc).
    pub fn on_root_press(&mut self, wm: &mut WindowManager<B>, at: Point, button: MouseButton) -> ShellOutcome {
        if button == MouseButton::Right {
            self.desktop.open_root_menu(wm.backend_mut(), &self.theme, at);
        } else {
            self.desktop.close_menu(wm.backend_mut());
        }
        ShellOutcome::Continue
    }

    /// A button press/release on a shell surface — the routing heart of
    /// the desktop.
    ///
    /// Two menu kinds share the pick path at the bottom: the root menu
    /// is opened by [`Shell::on_root_press`], while the window menu is
    /// opened from the `WindowMenuRequested` notification the titlebar
    /// right-click emits (see [`Shell::on_notification`]) — but once
    /// open, both are shell surfaces, so both deliver their clicks
    /// through this method and resolve in the one `click_menu`
    /// dispatch below. Without an explicit pointer grab while a menu is
    /// open (see `Desktop::open_root_menu`), release events for a held
    /// button would keep reporting against whatever surface the press
    /// landed on rather than the menu now under the pointer; that grab
    /// is what makes press-drag-release-to-pick work.
    ///
    /// The launcher strip routes ahead of everything else — releases
    /// even ahead of the binary's own root-window branch: an
    /// in-progress strip drag holds a pointer grab (like the icon
    /// drags), so its release can report against any surface at all,
    /// the root included, which is why the binary must route root
    /// *releases* through here rather than dropping them.
    /// `pointer_root` is the pointer's last known root position (shell
    /// clicks themselves carry only surface-local coordinates), which
    /// is exactly where the release happened — see the field's doc.
    pub fn on_shell_click(
        &mut self,
        wm: &mut WindowManager<B>,
        surface: B::ShellId,
        local: Point,
        button: MouseButton,
        pressed: bool,
    ) -> ShellOutcome {
        // A release first offers itself to an in-progress strip drag
        // (drag-off-the-strip unpins); one that no strip drag consumes
        // falls through to the ordinary routing below, including the
        // strip's own click resolution when the release is on the
        // strip.
        if !pressed && self.launchdock.handle_release(wm.backend_mut(), &self.theme, self.pointer_root) {
            return ShellOutcome::Continue;
        }

        // Clicks on the strip itself — mirroring how the desktop's own
        // clip/dock surfaces are routed below. The running pairs give
        // the click its focus-or-launch answer for the pressed tile.
        if self.launchdock.owns_window(surface) {
            let running = running_pairs(wm);
            if let Some(action) = self.launchdock.handle_click(wm.backend_mut(), &self.theme, local, pressed, &running) {
                match action {
                    LaunchDockAction::Launch(entry) => {
                        launch_app(&entry, &self.theme, self.state.terminal_font_px, terminal_screen(self))
                    }
                    // The same activate path a pager's
                    // _NET_ACTIVE_WINDOW message rides — focuses,
                    // raises, and switches workspace as needed, with
                    // `wm-core` re-validating the id (a stale one is
                    // silently nothing).
                    LaunchDockAction::Focus(target) => wm.dispatch(BackendEvent::ActivateRequested(target)),
                }
            }
            return ShellOutcome::Continue;
        }

        if surface == self.desktop.clip_window() {
            if pressed && button == MouseButton::Left {
                self.desktop.click_clip(local);
            }
            return ShellOutcome::Continue;
        }

        if surface == self.desktop.dock_window() {
            // Middle-click-drag on a widget picks it up for reordering;
            // see `Desktop::begin_item_drag`/`drag_item_motion`
            // (the latter fires from `on_motion` on every pointer
            // move, not from here). Middle stays the dock's own gesture
            // and is never offered to a widget: a tile that could
            // swallow it could make itself un-reorderable.
            //
            // Left goes to the widget as a `DockInput`, both edges of
            // it. Widgets act on the press and ignore the release — but
            // they are *told* about the release, because press/release
            // is the shape the out-of-process tile protocol needs, and
            // delivering only half of it now would bake the narrower
            // shape into every widget written between here and there.
            //
            // Right opens the tile's own menu (Restart, Remove,
            // About) and is never delivered to a tile. It was reserved
            // before there was anything to put in it for exactly this
            // reason: a tile that had already been given right-click
            // could not have it taken back.
            match button {
                MouseButton::Middle => {
                    if pressed {
                        self.desktop.begin_item_drag(wm.backend_mut(), &self.theme, local);
                    } else {
                        self.desktop.end_item_drag(wm.backend_mut(), &self.theme);
                    }
                }
                MouseButton::Left => {
                    let input = if pressed {
                        DockInput::Press { local, button }
                    } else {
                        DockInput::Release { local, button }
                    };
                    self.desktop.dock_input(wm.backend_mut(), &self.theme, input);
                }
                MouseButton::Right => {
                    if pressed {
                        // On press, like every other context menu in
                        // this desktop — a menu should appear the
                        // instant the button goes down. Only remote
                        // tiles have one: a built-in instrument is part
                        // of the compositor, where "Remove" would mean
                        // editing the default column and "Restart"
                        // would mean restarting the shell.
                        self.desktop.open_dock_item_menu(wm.backend_mut(), &self.theme, local, self.pointer_root);
                    }
                }
            }
            return ShellOutcome::Continue;
        }

        // Every press on an icon tile arms a potential drag (see
        // `Desktop::begin_icon_drag`); it's resolved into either a
        // restore or a reposition on release, whichever `end_icon_drag`
        // decides based on whether the pointer actually moved.
        if pressed {
            self.desktop.begin_icon_drag(wm.backend_mut(), surface, local);
            return ShellOutcome::Continue;
        }

        if let Some(result) = self.desktop.end_icon_drag(wm.backend_mut()) {
            match result {
                IconDragResult::Restore(client_id) => wm.deminiaturize(client_id),
                // Dropping a miniwindow icon over the launcher strip
                // pins its application: the client's WM_CLASS resolves
                // back through the `.desktop` index, and `try_pin_at`
                // decides whether the drop actually landed on the
                // strip's pin zone. A miss on either count — no class
                // match, or a drop anywhere else on the desktop — is
                // silently a plain reposition, exactly the
                // pre-launcher behavior.
                IconDragResult::Repositioned { client, root } => {
                    let matched = wm.client(client).and_then(|c| apps::match_window_class(&self.apps, &c.class));
                    if let Some(index) = matched {
                        self.launchdock.try_pin_at(wm.backend_mut(), &self.theme, root, &self.apps[index]);
                    }
                }
            }
            return ShellOutcome::Continue;
        }

        if let Some(action) = self.desktop.click_menu(wm.backend_mut(), &self.theme, surface, local) {
            match action {
                MenuAction::Root(action) => {
                    let outcome = root_action_outcome(&action);
                    self.run_root_menu_action(wm, action);
                    return outcome;
                }
                // A window-menu pick carries the client it was opened
                // for. Every call below is a stale-id-safe no-op by
                // `wm-core` contract — the client may well have
                // vanished while the menu sat open — so no
                // re-validation is needed here.
                // A dock tile's own menu. The pick carries the tile's
                // persistence id rather than its slot, so a reorder or
                // a crash while the menu sat open cannot make it
                // command a different tile than the one right-clicked;
                // a stale id is silently nothing, like every other
                // stale target here.
                MenuAction::DockItem(id, action) => {
                    self.desktop.dock_item_menu_action(wm.backend_mut(), &self.theme, &id, action);
                }
                MenuAction::Window(client, action) => match action {
                    WindowMenuAction::ToggleMaximize => wm.toggle_maximize_full(client),
                    WindowMenuAction::Miniaturize => wm.miniaturize(client),
                    WindowMenuAction::ToggleShade => wm.toggle_shade(client),
                    WindowMenuAction::ToggleFullscreen => wm.toggle_fullscreen(client),
                    WindowMenuAction::MoveToWorkspace(ws) => wm.move_client_to_workspace(client, ws),
                    WindowMenuAction::Close => wm.close_client(client),
                    WindowMenuAction::Kill => wm.kill_client(client),
                },
            }
        }

        ShellOutcome::Continue
    }

    /// Every file descriptor the binary's event loop must wait on
    /// besides its own display connection — the dockapp listener and
    /// one per connected dockapp.
    ///
    /// This is the *entire* backend-specific cost of out-of-process
    /// dock tiles, and it is deliberately shaped as a list of raw fds
    /// rather than as anything cleverer: the X11 binary appends them to
    /// the `pollfd` array it already builds around the X socket, and
    /// the Wayland binary wraps each in a calloop `Generic` source.
    /// Neither needs to know what is on the other end, because a
    /// dockapp is not a display-server client — it is a process on a
    /// Unix socket, and both loops already wait on those.
    ///
    /// Call it immediately before waiting: the set changes as dockapps
    /// connect, die and restart, and a stale fd is at best a spurious
    /// wakeup. Getting it wrong is bounded — both loops already wake on
    /// a 16ms housekeeping bound, so a missing fd costs a dockapp frame
    /// up to 16ms and nothing else.
    pub fn extra_poll_fds(&self) -> Vec<std::os::fd::RawFd> {
        self.desktop.extra_poll_fds()
    }

    /// A scroll over a shell surface, resolved to a dock tile the same
    /// way a click is.
    ///
    /// # `delta` is a count, and it is replayed as one
    ///
    /// `ScrollDelta` carries whole wheel notches, and a backend may
    /// legitimately fold several that arrived together into one entry —
    /// `wm-wayland` accumulates a high-resolution wheel's 120ths into
    /// detents, and a hard flick produces more than one per report. So
    /// a delta of three means *three* steps, and it is delivered as
    /// three `DockInput::Scroll` events rather than one carrying a 3.
    ///
    /// That choice costs two extra messages and buys correctness by
    /// construction on the far side of a boundary this shell does not
    /// control. A dockapp is third-party code; the obvious naive
    /// implementation of its scroll handler adjusts by one step per
    /// event and would silently swallow two notches out of three
    /// forever, in a way neither side could see. The wire keeps a
    /// signed `delta` so the direction travels with the event and a
    /// future high-resolution path has somewhere to go.
    ///
    /// The step count is capped at
    /// [`MAX_SCROLL_STEPS`](crate::dockapp::tile::MAX_SCROLL_STEPS),
    /// because "replay it N times" with an unbounded N read off an
    /// input event is a loop on the repaint thread whose length a
    /// backend bug decides.
    ///
    /// Only the vertical axis is delivered. The dock is a vertical
    /// column of square tiles and `DockInput::Scroll` carries one
    /// delta; inventing a rule that folds `right` into it would make
    /// two different gestures indistinguishable to every tile.
    pub fn on_shell_scroll(&mut self, wm: &mut WindowManager<B>, surface: B::ShellId, local: Point, delta: ScrollDelta) {
        if surface != self.desktop.dock_window() {
            return;
        }
        let notches = delta.up;
        if notches == 0 {
            return;
        }
        let wanted = notches.unsigned_abs();
        let steps = wanted.min(crate::dockapp::tile::MAX_SCROLL_STEPS as u32);
        if steps < wanted {
            tracing::warn!(notches, delivered = steps, "clamping an implausibly large scroll report");
        }
        let step = notches.signum();
        for _ in 0..steps {
            self.desktop.dock_input(wm.backend_mut(), &self.theme, DockInput::Scroll { local, delta: step });
        }
    }

    /// The side-effect half of a root-menu pick; its outcome half is
    /// [`root_action_outcome`], which `on_shell_click` pairs with this
    /// so the split can never let a pick's act and its outcome drift.
    fn run_root_menu_action(&mut self, wm: &mut WindowManager<B>, action: RootMenuAction) {
        match action {
            RootMenuAction::LaunchTerminal => {
                spawn_terminal(&self.theme, self.state.terminal_font_px, terminal_screen(self))
            }
            RootMenuAction::LaunchAbout => {
                // `CHONKSTEP_THEME` is the one published channel by
                // which an SDK app learns which theme the desktop is
                // wearing: `chonk_ui::active_theme` reads it and falls
                // back to NeXTSTEP Classic when it is absent. Until this
                // line existed the variable had a consumer and no
                // producer, so `chonk-about` — the SDK's own showcase —
                // rendered in Classic on every other theme, which is
                // exactly the mismatch the SDK exists to prevent.
                //
                // Deliberately not a state-file read inside `chonk-ui`:
                // that would duplicate `startup::resolve_theme`'s
                // precedence (env, then config, then default) in a
                // second crate and drift from it silently. The launcher
                // knows the live answer; it should say so.
                //
                // Phase 4b's dockapp launch wants the same variable, and
                // a running dockapp additionally gets `ThemeChanged`
                // pushed down its socket — this env var is only how a
                // freshly-spawned one learns the theme it starts in.
                spawn::spawn_detached_with_env(
                    &about_binary_path(),
                    &[],
                    &[("CHONKSTEP_THEME".to_string(), self.theme.id.clone())],
                    &[],
                );
            }
            // Indexes the same apps vec the desktop's menu was built
            // from, so `i` means the same entry on both sides; the
            // bounds-safe get covers the impossible desync anyway —
            // menus fire `Kill`-grade commands, so "impossible" still
            // doesn't get to panic.
            RootMenuAction::LaunchApp(i) => {
                if let Some(entry) = self.apps.get(i) {
                    launch_app(entry, &self.theme, self.state.terminal_font_px, terminal_screen(self));
                } else {
                    tracing::warn!(index = i, count = self.apps.len(), "menu fired an out-of-range application index");
                }
            }
            RootMenuAction::SetWallpaper(wallpaper) => {
                self.desktop.set_wallpaper(wm.backend_mut(), &self.theme, wallpaper);
            }
            RootMenuAction::SetTheme(id) => {
                // Resolved first: a pick naming a theme that does not
                // exist must change nothing at all, rather than persist
                // a choice this session then declines to wear. Not
                // reachable from the menu, which is generated from the
                // same list — but this is also the path a future
                // scripted theme change would take.
                let Some(base) = wm_theme::default_theme::theme_by_id(id) else {
                    tracing::warn!(theme = id, "theme menu named a theme that does not exist; keeping the current one");
                    return;
                };
                if let Err(e) = theme_select::persist(id) {
                    tracing::warn!(?e, id, "failed to persist theme selection");
                }
                // A theme implies its wallpaper. Applied *and*
                // persisted: applied because the desktop holds its
                // wallpaper as loaded state that a restyle does not
                // re-read, and persisted so the next session composes
                // the same full look. The Wallpaper menu can still
                // override it afterward.
                if let Some(paper) = wallpaper::Wallpaper::from_id(&base.wallpaper) {
                    if let Err(e) = paper.persist() {
                        tracing::warn!(?e, id, "failed to persist theme wallpaper");
                    }
                    self.desktop.set_wallpaper(wm.backend_mut(), &self.theme, paper);
                }
                self.apply_theme(wm, base);
            }
            RootMenuAction::Exit => {}
        }
    }

    /// Feeds one (already-coalesced) pointer motion's root position to
    /// the icon drag tracker, the dock widget drag tracker, and the
    /// launcher strip's own drag tracker, records it into
    /// `pointer_root` (the cell shell button handling reads back for
    /// release decisions that need root coordinates), and drains the
    /// backend's pending shell-surface motion into menu hover. The
    /// binary calls this for every motion it dispatches — mid-burst,
    /// when a non-motion event follows one, and once more after the
    /// burst ends — so the shell sees exactly the positions `wm-core`
    /// does.
    pub fn on_motion(&mut self, wm: &mut WindowManager<B>, root: Point) {
        self.pointer_root = root;
        self.desktop.drag_icon_motion(wm.backend_mut(), root);
        self.desktop.drag_item_motion(wm.backend_mut(), &self.theme, root);
        // Which dock tile the pointer is inside, from root coordinates
        // rather than from the dock's own surface-local motion: only
        // root motion reports the moment the pointer *leaves* the dock,
        // and a tile that never receives `Leave` latches into a
        // permanent hover state. See `Desktop::update_dock_hover`.
        self.desktop.update_dock_hover(wm.backend_mut(), &self.theme, root);
        self.launchdock.handle_motion(wm.backend_mut(), &self.theme, root);
        // Menu hover rides the same cadence: every motion over a shell
        // surface also arrives as a root-relative motion event (that is
        // what got us called), so the pointer's final position is the
        // one that should highlight a row.
        //
        // Drained to empty rather than one-per-call, because the two
        // backends queue differently: X11 keeps only the latest shell
        // motion (a compressed MotionNotify), while the compositor
        // queues each one. Taking a single entry would leave the
        // compositor's queue permanently one behind and growing under a
        // fast sweep, highlighting rows the pointer left long ago. The
        // last entry wins here for the same reason it does on X11.
        let mut hover = None;
        while let Some(entry) = wm.backend_mut().take_shell_motion() {
            hover = Some(entry);
        }
        if let Some((surface, local)) = hover {
            self.desktop.hover_menu(wm.backend_mut(), &self.theme, surface, local);
        }
    }

    /// Reacts to a `wm-core` state change the shell needs to know about
    /// but that `wm-core` itself has no opinion on — icon tiles for
    /// miniaturized windows, the Alt+Tab switcher, and the titlebar
    /// right-click window menu.
    pub fn on_notification(&mut self, wm: &mut WindowManager<B>, notification: Notification) {
        match notification {
            Notification::Miniaturized(id, preview) => {
                let title = wm.client(id).map(|c| c.title.clone()).unwrap_or_default();
                self.desktop.show_icon(wm.backend_mut(), &self.theme, id, &title, preview.as_ref());
            }
            Notification::Deminiaturized(id) | Notification::Removed(id) => {
                self.desktop.remove_icon_for_client(wm.backend_mut(), id);
            }
            Notification::Mapped(_) => {}
            Notification::CycleUpdated => {
                if let Some((candidates, selected)) = wm.cycle_state() {
                    // Previews are captured once per session (and again
                    // only if the candidate set itself changes) —
                    // stepping the selection is just a re-render of
                    // stored entries.
                    let entries = (self.desktop.switcher_entry_count() != Some(candidates.len())).then(|| {
                        candidates
                            .iter()
                            .map(|(id, title)| wm_theme::switcher::SwitcherEntry { title: title.clone(), preview: wm.client_preview(*id) })
                            .collect()
                    });
                    self.desktop.show_switcher(wm.backend_mut(), &self.theme, entries, selected);
                }
            }
            Notification::CycleEnded => self.desktop.hide_switcher(wm.backend_mut()),
            Notification::WindowMenuRequested { id, at } => {
                // Titlebar right-click: `wm-core` reports which client
                // and where, the shell owns what the menu contains.
                // The context is a snapshot of the client's state at
                // open time — that's what the item labels reflect —
                // while the action a pick eventually fires re-reads
                // live state inside `wm-core`, so a snapshot is all
                // the menu needs. A stale id (the client vanished
                // between the click and this drain) is silently
                // nothing, matching every other stale-id path.
                if let Some(client) = wm.client(id) {
                    let ctx = WindowMenuContext {
                        client: id,
                        title: client.title.clone(),
                        shaded: client.flags.contains(ClientFlags::SHADED),
                        // Either axis counts: the menu's toggle drives
                        // `toggle_maximize_full`, whose own un-maximize
                        // branch fires when either flag is set.
                        maximized: client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V),
                        fullscreen: client.flags.contains(ClientFlags::FULLSCREEN),
                        workspace: client.workspace,
                        workspace_count: wm.workspace_count(),
                    };
                    self.desktop.open_window_menu(wm.backend_mut(), &self.theme, at, ctx);
                }
            }
        }
    }

    /// One housekeeping pass, called once per event-loop iteration
    /// (the binary bounds its event wait so this still runs regularly
    /// with zero input activity).
    ///
    /// Workspace plumbing between the WM and the Clip runs first:
    /// drain a click on the indicator into a real switch, then mirror
    /// the authoritative state into the shared cell so the widget tick
    /// repaints the tile exactly when it changed.
    pub fn tick(&mut self, wm: &mut WindowManager<B>) {
        if let Some(target) = self.desktop.take_workspace_request() {
            wm.switch_workspace(target);
        }
        let (current, count) = (wm.current_workspace(), wm.workspace_count());
        self.desktop.set_workspace_display(wm.backend_mut(), &self.theme, current, count);
        self.desktop.tick_menu(wm.backend_mut(), &self.theme);
        self.desktop.tick_items(wm.backend_mut(), &self.theme);
        // Same cadence as the widget tick: refresh the launcher
        // strip's running-app indicators from the live client set — a
        // cheap no-op inside `update_running` whenever nothing
        // changed.
        let running = running_pairs(wm);
        self.launchdock.update_running(wm.backend_mut(), &self.theme, &running);
    }

    /// Winds the session down, in the way the binary says it is ending.
    ///
    /// The binary calls this once it has decided to exit or re-exec,
    /// before it does either, and the argument is the difference between
    /// the two: [`Farewell::SessionOver`] stops every out-of-process
    /// tile, [`Farewell::Restarting`] leaves them running and hands
    /// their tokens to the incoming shell so they are readopted rather
    /// than relaunched. A hot restart is the most routine thing a user
    /// does to this desktop — every theme pick is one — which is why it
    /// is worth having the second mode at all. See
    /// `Desktop::shut_down_dockapps`.
    ///
    /// Nothing else needs winding down: sampler threads, popup
    /// surfaces and shell surfaces all die with the process, and the
    /// dock order was already persisted at the moment the user
    /// committed a drag.
    pub fn shut_down(&mut self, farewell: Farewell) {
        self.desktop.shut_down_dockapps(farewell);
    }

    /// The screen/output arrangement changed (the binary drained the
    /// backend's resize event): rehang the desktop chrome on the new
    /// primary monitor's edges and re-derive one workarea per monitor.
    /// `size` is the whole desktop's new extent; the monitor list is
    /// re-read from the backend here rather than passed in, so an
    /// output being plugged in or unplugged lands the same way a plain
    /// resize does.
    pub fn on_screen_resize(&mut self, wm: &mut WindowManager<B>, size: Size) {
        let primary = primary_rect(&wm.monitors(), size);
        self.desktop.resize_to_screen(wm.backend_mut(), &self.theme, size, primary);
        // The launcher strip anchors to the primary too, and unlike
        // the dock and Clip it is not owned by `Desktop`, so it has to
        // be told separately - otherwise it stays on the old monitor
        // while the Clip moves to the new one.
        self.launchdock.reposition(wm.backend_mut(), &self.theme, primary);
        self.apply_workareas(wm);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wallpaper::Wallpaper;
    use wm_core::Modifiers;

    fn combo(keysym: u32) -> KeyCombo {
        KeyCombo { keysym, modifiers: Modifiers::ALT }
    }

    #[test]
    fn a_later_duplicate_binding_wins_in_the_keymap() {
        // The line further down the config file is the correction —
        // plain insertion order into the map delivers exactly that.
        let bindings = vec![(combo(1), Action::Close), (combo(1), Action::Miniaturize)];
        let keymap = build_keymap(&bindings);
        assert_eq!(keymap.get(&combo(1)), Some(&Action::Miniaturize));
    }

    #[test]
    fn distinct_combos_keep_their_own_bindings() {
        let bindings = vec![(combo(1), Action::Close), (combo(2), Action::Restart)];
        let keymap = build_keymap(&bindings);
        assert_eq!(keymap.get(&combo(1)), Some(&Action::Close));
        assert_eq!(keymap.get(&combo(2)), Some(&Action::Restart));
    }

    #[test]
    fn an_unbound_combo_misses_the_keymap() {
        // Load-bearing for the modal Alt+Tab switcher: a miss must let
        // the binary pass the key through to `wm-core` unchanged
        // rather than resolve to anything.
        let keymap = build_keymap(&[(combo(1), Action::Close)]);
        assert_eq!(keymap.get(&combo(99)), None);
    }

    #[test]
    fn exit_maps_to_the_exit_outcome() {
        assert_eq!(root_action_outcome(&RootMenuAction::Exit), ShellOutcome::Exit);
    }

    #[test]
    fn set_theme_asks_the_binary_for_nothing() {
        // This assertion used to read `ShellOutcome::Restart`, and
        // inverting it is the whole point of the live-apply work: a
        // theme pick is applied by `run_root_menu_action` in place, so
        // there is no process-level act left for the binary to carry
        // out. If this ever says Restart again, every theme pick has
        // silently started costing the user their Wayland clients.
        assert_eq!(root_action_outcome(&RootMenuAction::SetTheme("graphite")), ShellOutcome::Continue);
    }

    fn key(keysym: u32) -> KeyCombo {
        KeyCombo { keysym, modifiers: Modifiers::ALT }
    }

    #[test]
    fn a_grab_delta_takes_only_what_is_new_and_releases_only_what_is_gone() {
        // The combo present in both lists is the interesting one: it
        // must be left strictly alone. Releasing and re-taking it would
        // work on X11 by luck (same-client grabs replace) and is
        // exactly the kind of churn that becomes a dropped keypress on
        // a backend where it does not.
        let previous = vec![key(1), key(2)];
        let next = vec![(key(2), Action::Close), (key(3), Action::Miniaturize)];
        let (to_ungrab, to_grab) = grab_delta(&previous, &next);
        assert_eq!(to_ungrab, vec![key(1)]);
        assert_eq!(to_grab, vec![key(3)]);
    }

    #[test]
    fn a_grab_delta_from_nothing_takes_everything() {
        // The startup path: `Shell::new` reconciles from an empty set
        // rather than having its own grab loop, so this is the case
        // that has to behave like the old dedicated loop did.
        let next = vec![(key(1), Action::Close), (key(2), Action::Restart)];
        let (to_ungrab, to_grab) = grab_delta(&[], &next);
        assert!(to_ungrab.is_empty());
        assert_eq!(to_grab, vec![key(1), key(2)]);
    }

    #[test]
    fn a_combo_bound_twice_is_grabbed_once() {
        // A config may bind the same combo twice (last one wins in the
        // keymap). Grabbing it twice would leave the second grab held
        // after the first is released, so the session would keep
        // swallowing a key the user had just unbound.
        let next = vec![(key(1), Action::Close), (key(1), Action::Miniaturize)];
        let (_, to_grab) = grab_delta(&[], &next);
        assert_eq!(to_grab, vec![key(1)]);
    }

    fn monitor(geometry: Rect, primary: bool) -> MonitorInfo {
        MonitorInfo { geometry, name: "test".to_string(), primary }
    }

    #[test]
    fn chrome_hangs_on_the_flagged_primary_whatever_its_position_in_the_list() {
        let left = Rect { pos: Point::new(-1920, 0), size: Size::new(1920, 1080) };
        let right = Rect { pos: Point::new(0, 0), size: Size::new(1600, 1200) };
        let monitors = [monitor(left, false), monitor(right, true)];

        assert_eq!(
            primary_rect(&monitors, Size::new(3520, 1200)),
            right,
            "the flagged primary wins, not the first entry"
        );
    }

    #[test]
    fn with_no_flagged_primary_the_first_monitor_stands_in() {
        // `Backend::monitors` allows a platform that names no primary
        // at all — index 0 is then the primary, matching what wm-core
        // itself does with the same list.
        let first = Rect { pos: Point::new(0, 0), size: Size::new(1600, 1200) };
        let second = Rect { pos: Point::new(1600, 0), size: Size::new(1920, 1080) };
        let monitors = [monitor(first, false), monitor(second, false)];

        assert_eq!(primary_rect(&monitors, Size::new(3520, 1200)), first);
    }

    #[test]
    fn a_backend_reporting_no_monitors_falls_back_to_the_whole_screen() {
        // Exactly the origin-anchored, screen-sized assumption the
        // shell made before it was monitor-aware, so such a backend
        // behaves the way it always did.
        let screen = Size::new(1600, 1200);
        assert_eq!(primary_rect(&[], screen), Rect { pos: Point::new(0, 0), size: screen });
    }

    #[test]
    fn every_other_root_action_continues_the_session() {
        for action in [
            RootMenuAction::LaunchTerminal,
            RootMenuAction::LaunchAbout,
            RootMenuAction::LaunchApp(0),
            RootMenuAction::SetWallpaper(Wallpaper::LavenderGrid),
        ] {
            assert_eq!(root_action_outcome(&action), ShellOutcome::Continue);
        }
    }
}
