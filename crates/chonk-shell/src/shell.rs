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

use wm_config::{Action, Config};
use wm_core::{Backend, BackendEvent, ClientFlags, KeyCombo, MonitorInfo, MouseButton, Notification, WindowManager};
use wm_theme::Theme;
use wm_theme_api::{Point, PopupHost, Rect, Size};

use crate::apps::{self, AppEntry};
use crate::desktop::{Desktop, IconDragResult, MenuAction, RootMenuAction, WindowMenuAction, WindowMenuContext};
use crate::launchdock::{LaunchDock, LaunchDockAction};
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
    /// The session must be rebuilt from scratch to apply a change that
    /// touches every surface at once — a theme pick, or the configured
    /// restart keybinding (which doubles as the config hot-reload
    /// gesture, since a fresh process re-reads the config file).
    Restart,
}

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
        // the compositor to the whole frame (the X11 binary registers
        // a per-app opacity rule for urxvt), not by the terminal
        // itself.
        // Client-side alpha via a 32-bit visual was tried first and
        // reverted: urxvt leaves stale framebuffer garbage in regions
        // it fails to repaint on scroll/resize, so rows flickered
        // between glass, garbage, and fully transparent (confirmed
        // live).
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
    spawn_urxvt(terminal_args(theme));
}

/// The single urxvt spawn step: [`spawn_terminal`] passes the themed
/// args alone, [`launch_app`] appends `-e` plus a `.desktop` entry's
/// command line for `Terminal=true` apps. Factored so the two callers
/// can never drift on how the arg list actually reaches the process.
fn spawn_urxvt(args: Vec<String>) {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    spawn::spawn_detached("urxvt", &arg_refs);
}

/// Launches one `.desktop` entry — the shared dispatch behind both the
/// root menu's Applications submenu and the launcher dock's tiles, so
/// the two gestures can never disagree on how an entry runs.
/// `Terminal=true` entries run inside the themed terminal (urxvt's `-e`
/// consumes the rest of the command line as the program to exec), so a
/// TUI app gets the exact font/geometry/palette the Terminal menu item
/// itself would. An empty parsed command line — a malformed entry the
/// scanner let through — is a logged no-op, never a panic.
fn launch_app(entry: &AppEntry, theme: &Theme) {
    // Scale recovered from the already-scaled theme (titlebar font is
    // 12px at 1x) — the same trick `terminal_args` uses, so launch
    // fixups need no separate scale plumbing.
    let scale = theme.titlebar.font.size / 12.0;
    let Some((program, args)) = entry.exec.split_first() else {
        tracing::warn!(app = %entry.id, "desktop entry has an empty command line; not launching");
        return;
    };
    if entry.terminal {
        let mut argv = terminal_args(theme);
        argv.push("-e".to_string());
        argv.extend(entry.exec.iter().cloned());
        spawn_urxvt(argv);
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
    let mut argv: Vec<String> = args.to_vec();
    let base = program.rsplit('/').next().unwrap_or(program);
    if base.contains("chrom") || base.contains("chrome") || base == "microsoft-edge" || base.starts_with("brave") {
        argv.extend(spawn::chromium_scale_args(scale));
        argv.extend(spawn::chromium_avoid_secrets_service_hang_args());
        argv.extend(spawn::chromium_x11_platform_args());
    }
    let arg_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    spawn::spawn_detached_with_env(program, &arg_refs, &spawn::gtk_qt_scale_env(scale));
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
        // A theme redresses every surface at once; only a fresh
        // process composes the full look (see `run_root_menu_action`,
        // which persists the choice this outcome asks the binary to
        // apply).
        RootMenuAction::SetTheme(_) => ShellOutcome::Restart,
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
    theme: Theme,
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
    /// scans applications, raises the Dock/Clip/launcher chrome, and
    /// compiles the configured keymap. `scale` must be the same factor
    /// the theme was scaled by, so the shell's own chrome (which does
    /// not go through the theme engine) matches the WM's.
    pub fn new(backend: &mut B, config: &Config, theme: Theme, scale: f32) -> Self {
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
        let launchdock = LaunchDock::new(backend, &theme, primary, ((56.0 * scale).round() as u32).max(16), &apps);

        Self { desktop, launchdock, apps, theme, keymap: build_keymap(&config.keybindings), pointer_root: Point::new(0, 0) }
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
            Action::SpawnTerminal => spawn_terminal(&self.theme),
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
            // The same act the theme menu's pick asks for: the binary
            // rebuilds the session in place (the X11 binary re-execs
            // its on-disk image, windows surviving via the SaveSet) —
            // which is also what makes this binding the config
            // hot-reload gesture.
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
                    LaunchDockAction::Launch(entry) => launch_app(&entry, &self.theme),
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
            // see `Desktop::begin_widget_drag`/`drag_widget_motion`
            // (the latter fires from `on_motion` on every pointer
            // move, not from here). A plain left click instead fires
            // the widget's own click behavior (e.g. the system monitor
            // toggling its analog/dashboard face). Everything else on
            // the dock is still just a click-through identity tile.
            match button {
                MouseButton::Middle => {
                    if pressed {
                        self.desktop.begin_widget_drag(wm.backend_mut(), &self.theme, local);
                    } else {
                        self.desktop.end_widget_drag(wm.backend_mut(), &self.theme);
                    }
                }
                MouseButton::Left if pressed => {
                    self.desktop.click_widget(wm.backend_mut(), &self.theme, local);
                }
                _ => {}
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

    /// The side-effect half of a root-menu pick; its outcome half is
    /// [`root_action_outcome`], which `on_shell_click` pairs with this
    /// so the split can never let a pick's act and its outcome drift.
    fn run_root_menu_action(&mut self, wm: &mut WindowManager<B>, action: RootMenuAction) {
        match action {
            RootMenuAction::LaunchTerminal => spawn_terminal(&self.theme),
            RootMenuAction::LaunchAbout => {
                spawn::spawn_detached(&about_binary_path(), &[]);
            }
            // Indexes the same apps vec the desktop's menu was built
            // from, so `i` means the same entry on both sides; the
            // bounds-safe get covers the impossible desync anyway —
            // menus fire `Kill`-grade commands, so "impossible" still
            // doesn't get to panic.
            RootMenuAction::LaunchApp(i) => {
                if let Some(entry) = self.apps.get(i) {
                    launch_app(entry, &self.theme);
                } else {
                    tracing::warn!(index = i, count = self.apps.len(), "menu fired an out-of-range application index");
                }
            }
            RootMenuAction::SetWallpaper(wallpaper) => {
                self.desktop.set_wallpaper(wm.backend_mut(), &self.theme, wallpaper);
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
        self.desktop.drag_widget_motion(wm.backend_mut(), &self.theme, root);
        self.launchdock.handle_motion(wm.backend_mut(), &self.theme, root);
        // Menu hover rides the same cadence: every motion over a shell
        // surface also arrives as a root-relative motion event (that
        // is what got us called), so draining the backend's
        // latest-wins shell motion here highlights exactly the row
        // under the pointer's final position.
        if let Some((surface, local)) = wm.backend_mut().take_shell_motion() {
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
        self.desktop.tick_widgets(wm.backend_mut(), &self.theme);
        // Same cadence as the widget tick: refresh the launcher
        // strip's running-app indicators from the live client set — a
        // cheap no-op inside `update_running` whenever nothing
        // changed.
        let running = running_pairs(wm);
        self.launchdock.update_running(wm.backend_mut(), &self.theme, &running);
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
    fn set_theme_maps_to_the_restart_outcome() {
        // The pick's persistence happens in `run_root_menu_action`;
        // the restart itself is the binary's act, reached only through
        // this mapping — if it ever stopped saying Restart, a theme
        // pick would persist silently and apply one session late.
        assert_eq!(root_action_outcome(&RootMenuAction::SetTheme("graphite")), ShellOutcome::Restart);
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
