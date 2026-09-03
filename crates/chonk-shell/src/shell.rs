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
use wm_core::{Backend, BackendEvent, ClientFlags, ClientId, KeyCombo, Lifecycle, MaximizeDirections, MonitorInfo, MouseButton, Notification, ScrollDelta, WindowManager};
use wm_theme::{FontState, RasterThemeEngine, Theme};
use wm_theme_api::{DecorationBuffer, Point, PopupHost, Rect, Size};

use crate::apps::{self, AppEntry};
use crate::desktop::{Desktop, EdgeReservation, IconDragResult, MenuAction, RootMenuAction, WindowMenuAction, WindowMenuContext};
use crate::control::{self, ControlSocket};
use crate::dockapp::Farewell;
use chonk_dock_proto::wire::PanelCloseReason;
use crate::launchdock::{LaunchDock, LaunchDockAction};
use crate::overview::{OverviewHit, OverviewItem};
use crate::session_layout::{RelaunchPlan, SessionLayout, WindowRecord};
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
// The window opens at the terminal world's one universal default —
// 80 columns by 24 rows, the shape every terminal since the VT100
// agrees on — asked for in cells (`--window-size-chars`), so foot's
// own font metrics make it exact. Cells are only safe while they fit:
// the font size is a user setting with a ceiling of 96px, at which 80
// columns is wider than any screen, and this file once carried a
// hand-tuned "92x26" that marched off the edge for exactly that
// reason. So the standard shape is requested when a conservative
// estimate says it fits the workarea, and everything below is the
// fallback for the pathological font/screen pairing: a fraction of
// the actual head, which fits by construction.
const TERMINAL_STANDARD_CELLS: (u32, u32) = (80, 24);
// Cell-size estimate per font pixel for the fits-check, deliberately
// generous (JetBrains Mono's advance is 0.6em, line height ~1.25):
// overestimating the footprint only makes the fallback a little
// eager, while underestimating would overhang the screen.
const TERMINAL_CELL_ESTIMATE: (f32, f32) = (0.65, 1.4);
const TERMINAL_SCREEN_FRACTION: (f32, f32) = (0.70, 0.78);
// Floor for that fraction, so a small or oddly-shaped head still gets a
// usable terminal rather than a proportionally tiny one.
const TERMINAL_MIN_SIZE: (u32, u32) = (640, 400);

/// foot argument list for the active theme's terminal palette —
/// foreground/background/cursor plus the full 16-slot ANSI set, so
/// every theme restyles terminals along with the chrome. The scale for
/// the font size is recovered from the already-scaled theme (titlebar
/// font is 12px at 1x) rather than re-reading the environment.
///
/// Both the font size and the window size are given to the terminal in
/// *its* pixels, which on the Wayland session are logical ones: the
/// compositor tells it the output scale and it renders itself at that,
/// so handing it pre-multiplied numbers would double the scale. The
/// division is by the same factor the theme was scaled by, recovered
/// the same way, so a 1x session divides by one and nothing moves.
fn terminal_args(theme: &Theme, font_px: f32, screen: Size, client_scale: f32) -> Vec<String> {
    let ui_scale = theme.titlebar.font.size / 12.0;
    let px = (font_px * ui_scale * (client_scale / ui_scale.max(0.01))).round().max(8.0) as u32;
    let divisor = (ui_scale / client_scale.max(0.01)).max(1.0);
    let logical_screen = Size::new(
        ((screen.w as f32 / divisor) as u32).max(1),
        ((screen.h as f32 / divisor) as u32).max(1),
    );
    let size_args = if standard_cells_fit(px as f32 / client_scale.max(0.01), theme, logical_screen) {
        let (cols, rows) = TERMINAL_STANDARD_CELLS;
        ("--window-size-chars".to_string(), format!("{cols}x{rows}"))
    } else {
        let (window_w, window_h) = terminal_window_size(theme, logical_screen);
        ("--window-size-pixels".to_string(), format!("{window_w}x{window_h}"))
    };
    let mut args = vec![
        "--font".to_string(),
        format!("JetBrainsMono Nerd Font:pixelsize={px},Noto Sans Symbols 2:pixelsize={px}"),
        size_args.0,
        size_args.1,
        // Which of the two populated sections the terminal starts in —
        // the appearance the resolved theme is wearing right now.
        "--override".to_string(),
        format!("initial-color-theme={}", theme.appearance.name()),
    ];
    // BOTH of foot's color sections are populated — this rendition's
    // palette in its own section and the counterpart rendition's in
    // the other — so a running terminal can follow a live appearance
    // switch: foot swaps sections on SIGUSR1 (dark) / SIGUSR2 (light),
    // which `Shell::retint_terminals` sends on every switch. A
    // terminal spawned in one mood and switched to the other is
    // indistinguishable from one spawned there.
    let counterpart = wm_theme::default_theme::theme_variant(&theme.id, theme.appearance.toggled())
        .map(|other| other.terminal)
        // A theme this build cannot name in the other mood (nothing
        // built-in today) keeps its one palette in both sections: the
        // switch then changes nothing, rather than half of something.
        .unwrap_or_else(|| theme.terminal.clone());
    let (dark, light) = match theme.appearance {
        wm_theme::Appearance::Dark => (&theme.terminal, &counterpart),
        wm_theme::Appearance::Light => (&counterpart, &theme.terminal),
    };
    push_palette_args(&mut args, "colors-dark", dark);
    push_palette_args(&mut args, "colors-light", light);
    args
}

/// Appends one `TerminalPalette` as foot `--override`s for one of its
/// color sections (`colors-dark` / `colors-light`).
fn push_palette_args(args: &mut Vec<String>, section: &str, palette: &wm_theme::model::TerminalPalette) {
    // foot wants bare RRGGBB, not the `#rrggbb` urxvt took.
    let hex = |c: wm_theme::model::Color| format!("{:02x}{:02x}{:02x}", c.r, c.g, c.b);
    args.push("--override".to_string());
    args.push(format!("{section}.foreground={}", hex(palette.fg)));
    args.push("--override".to_string());
    args.push(format!("{section}.background={}", hex(palette.bg)));
    // `cursor` is a *pair*: the text color drawn inside the cursor
    // block, then the block itself. urxvt's `-cr` set only the
    // block, so the background goes in the text slot to keep the
    // classic reversed look the themes were written against.
    args.push("--override".to_string());
    args.push(format!("{section}.cursor={} {}", hex(palette.bg), hex(palette.cursor)));
    // The theme's glass, applied by the terminal itself rather than by
    // a compositor opacity rule. On X11 that rule is what produces
    // translucency (`add_opacity_rule("URxvt", ..)` in the X11
    // binary's main), and client-side alpha was deliberately avoided
    // there because urxvt's 32-bit-visual path left stale framebuffer
    // garbage on scroll/resize. Neither constraint survives the move:
    // there is no per-app opacity rule on the Wayland side at all, so
    // without this the themes' `opacity` would simply do nothing, and
    // foot's own alpha is a clean premultiplied surface the compositor
    // composites correctly. Alpha is a per-section key, so each
    // rendition carries its own glass — pale glass spends less
    // contrast on the wallpaper, so light renditions run more opaque.
    if let Some(opacity) = palette.opacity {
        args.push("--override".to_string());
        args.push(format!("{section}.alpha={:.3}", f32::from(opacity) / 100.0));
    }
    for (index, color) in palette.ansi.iter().enumerate() {
        // 0-7 are the regular ANSI slots, 8-15 the bright ones; the
        // theme stores them as one flat 16-slot array.
        let key = if index < 8 {
            format!("{section}.regular{index}")
        } else {
            format!("{section}.bright{}", index - 8)
        };
        args.push("--override".to_string());
        args.push(format!("{key}={}", hex(*color)));
    }
}

/// The head a freshly launched terminal has to fit — the primary
/// monitor, the same rectangle every other piece of shell chrome hangs
/// on.
fn terminal_screen<B: Backend + PopupHost<PopupId = B::ShellId>>(shell: &Shell<B>) -> Size {
    shell.desktop.primary_workarea().size
}

/// Whether the standard 80x24 opens comfortably on this head: the
/// estimated cell footprint plus the frame's chrome inside the
/// (logical) workarea. `font_px` is the font size in the same logical
/// units as `screen`.
fn standard_cells_fit(font_px: f32, theme: &Theme, screen: Size) -> bool {
    let (cols, rows) = TERMINAL_STANDARD_CELLS;
    let (cw, ch) = TERMINAL_CELL_ESTIMATE;
    let chrome_h = f32::from(theme.titlebar.height)
        + f32::from(theme.resize_bar.height)
        + 2.0 * f32::from(theme.border.width);
    let need_w = cols as f32 * font_px * cw;
    let need_h = rows as f32 * font_px * ch + chrome_h;
    need_w <= screen.w as f32 && need_h <= screen.h as f32
}

/// The fallback launch size, in pixels, for a head the standard cell
/// shape cannot fit.
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
///
/// The returned handle is how a live appearance switch reaches this
/// terminal later (`Shell::retint_terminals` signals it); a caller
/// that drops it launches a terminal that simply keeps its palette.
///
/// `configured` is the user's `terminal` setting. When it is set the
/// desktop steps out of the way entirely: that argv is launched as
/// written, with no geometry, font or palette arguments bolted on. It
/// has to be that way — those arguments are foot's command-line
/// spelling of the theme, and there is no portable spelling to
/// translate them into. So a configured terminal keeps its own colors
/// and its own idea of how big it should be, and an appearance switch
/// leaves it alone.
///
/// That is a real loss of integration, and it is the right trade: a
/// session that can only ever run one terminal cannot host a desktop
/// that ships four of them and picks between them with
/// `xdg-terminal-exec`.
#[must_use]
fn spawn_terminal(
    configured: Option<&[String]>,
    theme: &Theme,
    font_px: f32,
    screen: Size,
) -> Option<spawn::SpawnedChild> {
    let scale = theme.titlebar.font.size / 12.0;
    if let Some(argv) = configured {
        return spawn_configured_terminal(argv, &[], scale);
    }
    spawn_foot(terminal_args(theme, font_px, screen, terminal_client_scale(theme)), scale)
}

/// Launches a user-configured terminal, optionally running `command`
/// inside it.
///
/// The separator is `-e`, which is the one terminal-launching
/// convention old enough to be everywhere: xterm, alacritty, ghostty,
/// kitty, foot and wezterm all accept it. It is a guess, but it is the
/// guess with the best odds, and a terminal that wants a different flag
/// can be named with the flag already in its argv.
///
/// Detached, never supervised, and so never in `Shell::terminals`:
/// that list exists to be signalled SIGUSR1/SIGUSR2 on an appearance
/// switch, which is foot's colour-theme swap and, for a terminal that
/// does not handle those signals (alacritty does not), the default
/// action — termination. A configured terminal keeps its own colours,
/// as the doc above says, and an appearance switch must leave it alone
/// in the most literal sense. Always `None` for that reason.
#[must_use]
fn spawn_configured_terminal(
    argv: &[String],
    command: &[String],
    scale: f32,
) -> Option<spawn::SpawnedChild> {
    let Some((program, args)) = argv.split_first() else {
        tracing::warn!("configured terminal has an empty argument list; not launching");
        return None;
    };
    let mut owned: Vec<String> = args.to_vec();
    if !command.is_empty() {
        owned.push("-e".to_string());
        owned.extend(command.iter().cloned());
    }
    let arg_refs: Vec<&str> = owned.iter().map(String::as_str).collect();
    let env: Vec<(String, String)> = crate::startup::xcursor_size_env(scale).into_iter().collect();
    if spawn::spawn_detached_with_env(program, &arg_refs, &env, &[]).is_none() {
        tracing::warn!(program = %program, "configured terminal failed to start");
    }
    None
}

/// The factor the terminal will scale itself by, which is what
/// `terminal_args`'s `client_scale` means. Under the Wayland
/// compositor the outputs advertise the session's scale and foot — a
/// native Wayland client — renders at it, so the terminal gets logical
/// numbers (client_scale 1) and the division in `terminal_args` maps
/// the physical screen and the theme-scaled font back down; handing it
/// pre-multiplied numbers there would double the scale, exactly as
/// that function's own contract warns. On the X11 stack foot can only
/// be talking to some *other* Wayland display (a dev session nesting
/// the X11 desktop), which advertises nothing about this session's
/// scale — so there the theme's factor rides along as it always has.
fn terminal_client_scale(theme: &Theme) -> f32 {
    match spawn::current_display_stack() {
        spawn::DisplayStack::Wayland => 1.0,
        spawn::DisplayStack::X11 => theme.titlebar.font.size / 12.0,
    }
}

/// The single foot spawn step: [`spawn_terminal`] passes the themed
/// args alone, [`launch_app`] appends `-e` plus a `.desktop` entry's
/// command line for `Terminal=true` apps. Factored so the two callers
/// can never drift on how the arg list actually reaches the process.
///
/// `scale` feeds the per-child `XCURSOR_SIZE` — passed explicitly
/// rather than inherited, because the inherited process value is the
/// X11-flavored pre-multiplied one and foot, a native Wayland client,
/// multiplies whatever it reads by the output scale on its own. The
/// terminal was the client this shipped visibly wrong on: a scale-2
/// session's pointer doubled to 96px the moment it crossed onto a
/// terminal. See `startup::xcursor_size_env` for the per-stack rule.
/// Supervised rather than fire-and-forget (`spawn_supervised`, not
/// `spawn_detached_with_env`) so the shell can hold a handle to every
/// terminal it launched: a live appearance switch retints running
/// terminals by signal (foot's SIGUSR1/SIGUSR2 color-theme switch),
/// and a signal needs a pid that is provably still the terminal's —
/// which the supervised reaper guarantees (see `SpawnedChild::signal`).
fn spawn_foot(args: Vec<String>, scale: f32) -> Option<spawn::SpawnedChild> {
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env: Vec<(String, String)> = crate::startup::xcursor_size_env(scale).into_iter().collect();
    spawn::spawn_supervised("foot", &arg_refs, &env, &[])
}

/// Runs one entry from `[commands]`, detached.
///
/// Detached rather than supervised, and that is the whole design of
/// this verb. A supervised child is one the desktop intends to keep
/// talking to — the terminals it retints by signal, the dockapps it
/// restarts. A command the user named is none of those: it is a thing
/// they asked to happen, which then belongs to them. Supervising it
/// would mean holding a reaper thread per press for a process the
/// desktop has no opinion about, and would make a long-lived one
/// (`omarchy-launch-shell`, say) look like a leak.
///
/// The scale goes in for the same reason it does everywhere else here:
/// a launched program that guesses its own cursor size guesses wrong on
/// a fractional display.
fn run_named_command(name: &str, argv: &[String], scale: f32) {
    let Some((program, args)) = argv.split_first() else {
        // `argv_from_value` rejects empty command lines, so reaching
        // here means the invariant broke upstream rather than that the
        // user typed something odd. Say so and carry on.
        tracing::warn!(command = %name, "command has an empty argument list; not running it");
        return;
    };
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let env: Vec<(String, String)> = crate::startup::xcursor_size_env(scale).into_iter().collect();
    match spawn::spawn_detached_with_env(program, &arg_refs, &env, &[]) {
        Some(pid) => tracing::info!(command = %name, program = %program, pid, "ran command"),
        None => tracing::warn!(command = %name, program = %program, "command failed to start"),
    }
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
/// Returns a supervised handle when the launch went through the themed
/// terminal (`Terminal=true`), so appearance switches can retint it —
/// `None` for GUI launches and failures.
fn launch_app(
    configured_terminal: Option<&[String]>,
    entry: &AppEntry,
    theme: &Theme,
    font_px: f32,
    screen: Size,
) -> Option<spawn::SpawnedChild> {
    // Scale recovered from the already-scaled theme (titlebar font is
    // 12px at 1x) — the same trick `terminal_args` uses, so launch
    // fixups need no separate scale plumbing.
    let scale = theme.titlebar.font.size / 12.0;
    let Some((program, args)) = entry.exec.split_first() else {
        tracing::warn!(app = %entry.id, "desktop entry has an empty command line; not launching");
        return None;
    };
    if entry.terminal {
        // A `Terminal=true` entry has to run in whichever terminal the
        // session actually uses, or the desktop would be launching TUI
        // apps into a terminal the user replaced.
        if let Some(argv) = configured_terminal {
            return spawn_configured_terminal(argv, &entry.exec, scale);
        }
        let mut argv = terminal_args(theme, font_px, screen, terminal_client_scale(theme));
        argv.push("-e".to_string());
        argv.extend(entry.exec.iter().cloned());
        return spawn_foot(argv, scale);
    }
    // External GUI launches get the environment/argument fixups the
    // old dedicated browser launcher carried, now applied generically:
    // every app is told the desktop's scale through the Qt env vars —
    // GTK clients get theirs from `chonk_xsettings::XSettingsManager`
    // instead (see `gtk_qt_scale_env`'s doc comment for why the two
    // can't both hand a GTK client the scale) — and the Chromium family
    // additionally gets its own scale flag plus `--password-store=basic`
    // — without which Chromium blocks ~25s at startup on a D-Bus secrets
    // service this session doesn't provide (the whole story lives on the
    // spawn.rs helpers). Confirmed live: the first .desktop-launched
    // Chromium hung exactly that way.
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
    // Every scale fixup below exists to tell a client something the
    // display server could not. Under the Wayland compositor the server
    // now *can*: `wl_output` carries the scale and toolkits act on it
    // (a GTK client answers `wl_output.scale(2)` with
    // `set_buffer_scale(2)` on its own — see `state.rs`'s
    // `advertise_scale` in `wm-wayland`). Applying these there as well
    // would have the client scale itself twice — `QT_SCALE_FACTOR=2`
    // *multiplies onto* the platform scale an output already declared,
    // and Chromium's `--force-device-scale-factor` likewise — so on
    // that stack the desktop says nothing and lets the protocol do it.
    //
    // The X11 session keeps every one of them. There is no output scale
    // in X11 for a client to read, which is the whole reason this
    // machinery was written.
    //
    // This withholding has flip-flopped once, so the history is worth
    // keeping: it was first withheld on exactly the reasoning above,
    // back when the compositor did not yet advertise a scale — which
    // left every launched client at 1x, and the withholding was
    // reverted. The outputs advertise for real now, verified end to
    // end under `WAYLAND_DEBUG`, so the reasoning has finally caught
    // up with the code it was written for. Withheld means *absent*,
    // not "passed as 1": every one of these is an override, and an
    // explicit 1 would pin to 1x exactly the client the output is
    // telling to draw at 2x.
    let stack = spawn::current_display_stack();
    let scale_fixups = matches!(stack, spawn::DisplayStack::X11);
    if base.contains("chrom") || base.contains("chrome") || base.starts_with("microsoft-edge") || base.starts_with("brave") {
        if scale_fixups {
            argv.extend(spawn::chromium_scale_args(scale));
        }
        argv.extend(spawn::chromium_avoid_secrets_service_hang_args());
        argv.extend(spawn::chromium_platform_args(stack));
    }
    let arg_refs: Vec<&str> = argv.iter().map(String::as_str).collect();
    spawn::spawn_detached_with_env(program, &arg_refs, &launch_env(&theme.id, crate::appearance::load_published(), scale), &[]);
    None
}

/// The Omarchy submenu's source for a session state: `None` when the
/// key is off or no Omarchy menu is installed. A fresh handle each
/// time — the read is two small files, and a fresh handle is what
/// makes "reload" mean re-read.
fn omarchy_menu_for(state: &SessionState) -> Option<crate::omarchy_menu::OmarchyMenu> {
    if !state.omarchy_menu {
        return None;
    }
    let menu = crate::omarchy_menu::OmarchyMenu::discover();
    if menu.is_none() {
        tracing::info!("no Omarchy menu definition installed; the root menu carries no Omarchy submenu");
    }
    menu
}

/// The environment every detached GUI launch from the shell carries:
/// the look this desktop is wearing, toolkit scaling, and the pointer
/// size — all in the child's own environment rather than the
/// session's, because any of them can change while the session runs
/// and the process environment cannot safely be rewritten once threads
/// exist. See `startup::xcursor_size_env`.
///
/// **The look first, because it is the part a chonkstep app reads.**
/// `CHONKSTEP_THEME` / `CHONKSTEP_APPEARANCE` / `CHONKSTEP_SCALE` are
/// the published channel by which an SDK app (`chonk_ui::active_theme`,
/// `chonk_ui::scale`) learns what the desk looks like; with them absent
/// it falls back to NeXTSTEP Classic at 1x, which is how a first-party
/// dialog ends up in different clothes from the desktop that opened
/// it. Every GUI the shell starts — the About box, an Omarchy menu
/// action, and a window a dock instrument's panel opens — goes through
/// here so that cannot happen in one place and not another.
///
/// The toolkit scale variables ride the X11 stack only — the reasoning
/// is spelled out at length in `launch_app`, whose fixups these are.
/// The pointer size rides both, but the *value* is per-stack: a
/// Wayland client treats `XCURSOR_SIZE` as a logical size and
/// multiplies the output scale in itself, so it gets the unscaled
/// base, while an X11 client has nothing to multiply by and gets the
/// pre-multiplied size. `xcursor_size_env` owns that rule.
/// `appearance` is an `Option` for the same reason the dockapp launch
/// reads it from the published state file and omits it when absent:
/// saying nothing is better than guessing a mood, and the callers that
/// have the live value (the desktop, which is wearing it) pass `Some`
/// while the ones that would have to go and look pass whatever the
/// published file says.
pub(crate) fn launch_env(theme_id: &str, appearance: Option<wm_theme::Appearance>, scale: f32) -> Vec<(String, String)> {
    let mut env = vec![
        ("CHONKSTEP_THEME".to_string(), theme_id.to_string()),
        // Four decimals, exactly as the dockapp launch writes it — one
        // format for one number, so a child cannot parse two.
        ("CHONKSTEP_SCALE".to_string(), format!("{scale:.4}")),
    ];
    if let Some(appearance) = appearance {
        env.push(("CHONKSTEP_APPEARANCE".to_string(), appearance.name().to_string()));
    }
    let scale_fixups = matches!(spawn::current_display_stack(), spawn::DisplayStack::X11);
    if scale_fixups {
        env.extend(spawn::gtk_qt_scale_env(scale));
    }
    env.extend(crate::startup::xcursor_size_env(scale));
    env
}

/// Runs one Omarchy menu command exactly as `omarchy-shell` does —
/// `bash -lc <command>`, detached, never waited on — with the same
/// launch environment every other GUI the shell starts gets, since
/// most Omarchy actions end in a window (a webapp, a floating
/// terminal, a picker).
/// How long a menu-launched Omarchy theme pick stays armed for
/// adoption. Long enough to browse the picker at leisure; short enough
/// that an abandoned pick cannot re-dress the desk much later.
const ADOPTION_ARM_WINDOW: std::time::Duration = std::time::Duration::from_secs(180);

fn run_omarchy_command(command: &str, theme: &Theme) {
    let scale = theme.titlebar.font.size / 12.0;
    let (program, args) = crate::omarchy_menu::action_argv(command);
    tracing::info!(command, "running omarchy menu command");
    spawn::spawn_detached_with_env(program, &args, &launch_env(&theme.id, crate::appearance::load_published(), scale), &[]);
}

/// Starts Omarchy's shell if this session is one that should host it
/// (`crate::omarchy_shell` owns the rule), through the exact launch
/// path an Omarchy menu action takes — `bash -lc`, detached, with the
/// desktop's launch environment — running the launcher by the path the
/// verdict resolved. Every outcome is logged once, so the session log
/// answers "why is there no bar".
fn host_omarchy_shell(verdict: &crate::omarchy_shell::Verdict, theme_id: &str, appearance: wm_theme::Appearance, scale: f32) {
    use crate::omarchy_shell::{self, Verdict};
    match verdict {
        Verdict::Launch(paths) => {
            let command = omarchy_shell::launch_command(paths);
            let (program, args) = crate::omarchy_menu::action_argv(&command);
            match spawn::spawn_detached_with_env(program, &args, &launch_env(theme_id, Some(appearance), scale), &[]) {
                Some(pid) => tracing::info!(pid, launcher = %paths.launcher.display(), "hosting Omarchy's shell"),
                None => tracing::warn!(launcher = %paths.launcher.display(), "Omarchy's shell launcher failed to start"),
            }
        }
        Verdict::Disabled => tracing::info!("not hosting Omarchy's shell: omarchy_shell = false"),
        Verdict::NotWayland => tracing::debug!("not hosting Omarchy's shell: not a Wayland session"),
        Verdict::NotInstalled => tracing::info!("no Omarchy shell installed; nothing to host"),
    }
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

// The keysyms the modal Overview owns, spelled as the X11 values both
// backends deliver (the same table `wm_config::parse_key` speaks).
const XK_LEFT: u32 = 0xff51;
const XK_UP: u32 = 0xff52;
const XK_RIGHT: u32 = 0xff53;
const XK_DOWN: u32 = 0xff54;
const XK_RETURN: u32 = 0xff0d;
const XK_KP_ENTER: u32 = 0xff8d;

/// What one key press means inside an open Overview session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverviewIntent {
    /// Step the selection one card in `(dx, dy)`.
    Move(i32, i32),
    /// Focus + raise the selected card and leave.
    Commit,
    /// Leave without committing.
    Dismiss,
}

/// Resolves a key pressed while the Overview holds the keyboard.
/// Bare arrows move, bare Return (either Enter) commits, and
/// *everything else* dismisses — Escape by convention, and any other
/// key because a modal panel that silently eats typing is worse than
/// one that steps aside (the Alt-Tab switcher treats a stray key the
/// same way, for the same reason). Modified arrows dismiss rather
/// than move so a workspace chord like alt+ctrl+right pressed out of
/// habit closes the panel instead of invisibly rebinding itself to
/// selection movement. The toggle binding itself is resolved by the
/// caller against the keymap before this is consulted, so a
/// super+up-style binding closes the panel rather than reading as a
/// modified arrow.
fn overview_intent(combo: &KeyCombo) -> OverviewIntent {
    if !combo.modifiers.is_empty() {
        return OverviewIntent::Dismiss;
    }
    match combo.keysym {
        XK_LEFT => OverviewIntent::Move(-1, 0),
        XK_RIGHT => OverviewIntent::Move(1, 0),
        XK_UP => OverviewIntent::Move(0, -1),
        XK_DOWN => OverviewIntent::Move(0, 1),
        XK_RETURN | XK_KP_ENTER => OverviewIntent::Commit,
        _ => OverviewIntent::Dismiss,
    }
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
        | RootMenuAction::OmarchyCommand { .. }
        | RootMenuAction::ToggleOmarchyBar
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

/// The live client set as session-layout records — what the layout
/// store debounces and persists (see `crate::session_layout`). Windows
/// with no class are skipped: there is nothing to relaunch them by and
/// nothing to match them back to, so a record would only ever expire.
/// A maximized window records its *pre-maximize* geometry (the flag
/// re-derives the maximize against the next session's workarea), which
/// also keeps restore-then-unmaximize exact.
fn layout_snapshot<B: Backend>(wm: &WindowManager<B>, apps: &[AppEntry]) -> Vec<WindowRecord> {
    wm.iter_clients()
        .filter(|(_, client)| !client.class.is_empty())
        .map(|(_, client)| {
            let maximized = client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V);
            WindowRecord {
                class: client.class.clone(),
                app: apps::match_window_class(apps, &client.class).map(|index| apps[index].id.clone()),
                geometry: if maximized { client.restore_geometry.unwrap_or(client.geometry) } else { client.geometry },
                workspace: client.workspace,
                maximized,
                shaded: client.flags.contains(ClientFlags::SHADED),
                miniaturized: client.lifecycle == Lifecycle::Miniaturized,
            }
        })
        .collect()
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
    /// The session-layout store: records the live window arrangement
    /// (debounced, from `tick`) and holds the previous session's
    /// records while a restore is matching mapped windows against
    /// them. See `crate::session_layout` for all the rules.
    layout: SessionLayout,
    /// The key press an open Overview session intercepted, parked
    /// between the two halves of the binaries' key protocol:
    /// [`Shell::keymap_action`] resolves a combo, [`Shell::run_action`]
    /// runs the resolved action, and the action type
    /// (`wm_config::Action`) deliberately stays a closed set of config
    /// verbs with one `Overview` entry rather than growing internal
    /// move/commit variants no config file may name. So while the
    /// Overview is modal, `keymap_action` answers `Overview` for every
    /// key and parks the combo here for `run_action` to route. The two
    /// calls are adjacent in both binaries' loops by construction;
    /// `run_action` `take()`s, so a stale combo cannot leak into a
    /// later session.
    overview_key: Option<KeyCombo>,
    /// Every terminal this shell launched that has not been observed
    /// to exit — the retint list for live appearance switches. foot
    /// swaps its color sections on SIGUSR1/SIGUSR2, and these handles
    /// are the only pids the shell can prove are still its terminals
    /// (their reaper threads keep the pids from being recycled — see
    /// `spawn::SpawnedChild`). Pruned on every switch; a terminal the
    /// user opened by hand is not in here and simply keeps its colors,
    /// which `docs/appearance.md` says out loud.
    terminals: Vec<spawn::SpawnedChild>,
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
    /// The control socket third-party bars read the desktop through
    /// (`docs/control-socket.md`). Serviced in [`Shell::tick`]; costs
    /// nothing while no bar is connected.
    control: ControlSocket,
    /// Set by [`Shell::keymap_action`] when the panel-dismiss Escape
    /// lands, consumed by [`Shell::tick`]. Parked rather than acted on
    /// inline because `keymap_action` has no `WindowManager` to close
    /// the panel with — the same two-phase shape the Overview's parked
    /// combo uses, and `tick` runs later in the very same loop
    /// iteration, so the dismissal is never a frame behind.
    panel_escape: bool,
    /// Notices Omarchy switching its theme underneath a session that
    /// follows it (`SessionState::following`); asked once a second from
    /// [`Shell::tick`] while following — and while an adoption is
    /// armed (below), so a pick can be noticed before following began.
    omarchy: crate::omarchy_follow::Watch,
    /// Set when the user launches Omarchy's own theme flow from this
    /// desktop's menu while wearing a built-in theme. Their intent —
    /// "give me that theme" — is expressed at launch, but honored only
    /// when Omarchy's current theme actually changes on disk: the
    /// picker cancelled is a desk unchanged. Expires rather than
    /// lingering, so a pick abandoned this morning cannot re-dress the
    /// desk this afternoon.
    omarchy_adoption_armed: Option<std::time::Instant>,
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

        // Both chrome owners get handles to the caller's font state —
        // the one `FontSystem` this session ever builds. They used to
        // construct their own, which cost two more full font scans at
        // boot for databases identical to the one already in hand.
        let mut desktop = Desktop::new(backend, screen, primary, scale, &theme, state.appearance, apps.clone(), fonts.clone());
        desktop.set_omarchy_menu(omarchy_menu_for(state));
        // The control socket, bound here — after the dock socket it
        // sits beside, and before the first process meant to see it
        // (the layout relaunch and autostart below; dockapp tiles,
        // already spawned by `Desktop::new`, are deliberately kept from
        // it — see `spawn::DOCKAPP_WITHHELD_ENV`), so
        // `CHONKSTEP_CONTROL_SOCKET` is in every such child's environment.
        // Binding declares the path to `spawn`, which is what puts it
        // there; a failed bind declares nothing and the session simply
        // has no control socket.
        let control = ControlSocket::new(&crate::dockapp::current_display());
        // The launcher strip below the Clip. Its tile size mirrors
        // `Desktop::new`'s own derivation (56px at 1x, scaled, floored
        // at 16) rather than inventing a second number: the strip's
        // tiles must read as the same family as the Clip above them
        // and the miniwindow icon tiles pins are dropped from.
        // It is handed the *primary's* size rather than the screen's,
        // so the strip's height clamp is measured against the head it
        // sits on rather than against every head at once.
        let launchdock = LaunchDock::new(backend, &theme, primary, crate::desktop::tile_px(scale), &apps, fonts.clone());

        // Session-layout restore, opt-in and only for a genuinely new
        // session: a hot restart on the X11 stack keeps every client
        // alive through the SaveSet, and relaunching the recorded
        // layout into that session would duplicate every window on
        // the screen — `session_continues` is that continuation's
        // marker. Launches ride the exact paths the menus use
        // (`spawn_terminal`, `launch_app`), so a restored application
        // gets the same scale/platform fixups a hand-launched one
        // would; the windows are then matched and re-placed as they
        // map, in `on_notification`.
        let restore = state.restore_session && !crate::startup::session_continues();
        let (layout, relaunch) = SessionLayout::start(restore, &apps, std::time::Instant::now());
        let mut terminals = Vec::new();
        for plan in relaunch {
            let terminal = match plan {
                RelaunchPlan::Terminal => spawn_terminal(state.terminal.as_deref(), &theme, state.terminal_font_px, primary.size),
                RelaunchPlan::App(entry) => launch_app(state.terminal.as_deref(), &entry, &theme, state.terminal_font_px, primary.size),
            };
            terminals.extend(terminal);
        }

        // Autostart, skipped only on an X11 hot restart — the one case
        // where the previous session's clients are still alive (through
        // the SaveSet) and re-running the list would leave the user
        // with two of everything they asked to start once. A Wayland
        // re-exec kills every client, so there the list runs again;
        // `startup::autostart_runs` owns the rule.
        //
        // Ordered and detached. The order is the file's, because a list
        // that starts a shell and then a thing that talks to that shell
        // has an order that matters; detached because these are the
        // user's processes to own, exactly as `[commands]` entries are.
        // Nothing waits and nothing is retried: an autostart entry that
        // fails costs the user that entry, never the session.
        if crate::startup::autostart_runs(crate::startup::session_continues(), spawn::current_display_stack()) {
            for argv in &state.autostart {
                run_named_command("autostart", argv, state.scale);
            }
        }

        // Omarchy's shell, where Hyprland's `autostart.lua` would start
        // it. Wayland-only, so the gate above is moot for it: a Wayland
        // re-exec kills the shell with the display it was drawing on,
        // and a continuation has none until it starts one — see
        // `crate::omarchy_shell`.
        // The bar's visibility is settled *before* the launch, so the
        // compositor already knows to keep the bar off the screen when
        // its surface arrives a few hundred milliseconds later.
        let verdict = crate::omarchy_shell::decide(state.omarchy_shell);
        let hosted = matches!(verdict, crate::omarchy_shell::Verdict::Launch(_));
        desktop.set_omarchy_bar(backend, hosted.then(crate::omarchy_shell::BarVisibility::load));
        host_omarchy_shell(&verdict, &theme.id, state.appearance, state.scale);

        // Publish the resolved appearance so the contract's reader half
        // (`$XDG_STATE_HOME/chonkstep/appearance`) is present from the
        // session's first frame — a dockapp that polls it must never
        // find nothing there. Publishing is not propagating: GSettings
        // is only touched when the mode actually *changes*, so booting
        // rewrites no one's preferences.
        crate::appearance::publish(state.appearance);

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
            overview_key: None,
            grabbed: to_grab,
            layout,
            terminals,
            state: state.clone(),
            theme,
            fonts,
            pointer_root: Point::new(0, 0),
            control,
            panel_escape: false,
            omarchy: crate::omarchy_follow::Watch::new(),
            omarchy_adoption_armed: None,
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
        wm.set_drag_modifier(next.drag_modifier);
        // The scale belongs in this list rather than in the metrics
        // step below: `wm-core` re-lays-out nothing on it — every pixel
        // it draws comes pre-scaled from the theme engine step 3 swaps
        // in — and wants it only to size the handful of measurements
        // written into that crate as *logical* numbers (today, the
        // fixed size Omarchy's own windows float at).
        wm.set_ui_scale(next.scale);
        // Straight through to the backend, which is what answers the
        // decoration protocols and decides who gets a frame...
        wm.backend_mut().set_decoration_rules(next.decorations.clone());
        // ...and then re-ask for every window already on the desk. A
        // rule that only reached windows opened after it was written
        // would mean closing and reopening the very window whose chrome
        // the user is trying to fix.
        wm.refresh_all_client_chrome();
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
        // The Omarchy submenu is policy too: re-resolved from the key
        // on every pass, which also re-reads the definition files —
        // a reload is the user's way of saying "look again".
        self.desktop.set_omarchy_menu(omarchy_menu_for(&next));

        // 2. Metrics.
        let theme = next.theme();
        let scale_changed = self.desktop.set_scale(next.scale);
        let theme_changed = theme != self.theme;
        let appearance_changed = next.appearance != self.state.appearance;
        self.state = next;
        if !scale_changed && !theme_changed {
            // Nothing that is drawn has moved. Repainting anyway would
            // be a visible flash on a reload that only rebound a key.
            return;
        }
        self.theme = theme;
        self.desktop.set_theme_id(self.theme.id.clone());
        if appearance_changed {
            // Before the relayout below repaints anything: the desktop
            // must already know which rendition of the wallpaper to
            // compose, and the published file must already say the new
            // mode by the time the first repainted frame is visible
            // (a dockapp reading it must never see the old word over
            // the new desktop).
            self.desktop.set_appearance(self.state.appearance);
            crate::appearance::publish(self.state.appearance);
            // Running terminals follow by signal, everything foreign
            // follows through GSettings/the portal — both are fire-and
            // -forget from here; the desktop's own repaint is not
            // gated on any other process noticing.
            self.retint_terminals();
            crate::appearance::propagate_to_applications(self.state.appearance);
        }
        tracing::info!(
            theme = %self.theme.id,
            appearance = %self.state.appearance.name(),
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

    /// Dresses the session in a built-in theme, keeping every other
    /// piece of session state as it is. The theme menu's whole job.
    /// Picking a built-in is also how a session stops following Omarchy
    /// — the choice is now a theme, not "whatever Omarchy says".
    fn apply_theme(&mut self, wm: &mut WindowManager<B>, base: Theme) {
        let mut next = self.state.clone();
        next.base_theme = base;
        next.following = None;
        self.apply_session_state(wm, next);
    }

    /// Re-resolves the whole session from the config file and applies
    /// it — what a reload does, and what following Omarchy does when
    /// Omarchy's theme changes. One function so the two cannot resolve
    /// by different rules.
    fn reresolve(&mut self, wm: &mut WindowManager<B>) {
        self.apply_session_state(wm, SessionState::resolve(&wm_config::load()));
    }

    /// Moves the session to the other side of the light/dark axis (or
    /// to an explicitly named side): the current theme re-resolves in
    /// its `mode` rendition and applies through the exact path a theme
    /// pick takes. A no-op when the session is already there.
    pub fn set_appearance(&mut self, wm: &mut WindowManager<B>, mode: crate::appearance::Appearance) {
        if mode == self.state.appearance {
            return;
        }
        if self.state.following.is_some() {
            // An Omarchy theme has exactly one rendition, and its mode
            // is the session's appearance for as long as the session
            // follows it (`startup::resolve_look`). Re-deriving a
            // second mood would paint colours the theme's author never
            // chose while every Omarchy terminal beside them kept the
            // real ones; flipping the published mode over unchanged
            // chrome would desynchronise dockapps and GTK from the
            // desk. So the request is declined, out loud. Omarchy's own
            // light/dark switch is to set a light or dark theme.
            tracing::warn!(
                requested = mode.name(),
                current = self.state.appearance.name(),
                "appearance request ignored: this session follows Omarchy, whose theme decides the mode"
            );
            return;
        }
        let mut next = self.state.clone();
        next.appearance = mode;
        match wm_theme::default_theme::theme_variant(&next.base_theme.id, mode) {
            Some(base) => next.base_theme = base,
            // A theme with no rendition on that side (nothing built-in
            // today): the mode still flips — published file, terminals,
            // GSettings — and the chrome keeps the one dress it has.
            None => tracing::warn!(
                theme = %next.base_theme.id,
                mode = mode.name(),
                "theme has no rendition for this appearance; switching the mode around it"
            ),
        }
        self.apply_session_state(wm, next);
    }

    /// The side of the light/dark axis this session is currently on.
    pub fn appearance(&self) -> crate::appearance::Appearance {
        self.state.appearance
    }

    /// `Some("omarchy")` while the session's theme choice is to follow
    /// Omarchy's current theme, `None` while it wears a built-in — see
    /// `SessionState::following`. What the control socket's `theme`
    /// event reports as `following`.
    pub fn following(&self) -> Option<&'static str> {
        self.state.following
    }

    /// The whole resolved session state, read-only — for the pieces a
    /// binary still publishes itself (the X11 binary's XSETTINGS).
    pub fn session_state(&self) -> &SessionState {
        &self.state
    }

    /// Signals every terminal this shell launched to swap to the color
    /// section matching the current appearance — foot's documented
    /// SIGUSR1 (dark) / SIGUSR2 (light) color-theme switch, against
    /// the `colors-dark`/`colors-light` sections `terminal_args`
    /// populated at spawn. Exited terminals are pruned first, so a
    /// recycled pid can never be signaled.
    fn retint_terminals(&mut self) {
        self.terminals.retain(|terminal| terminal.exited().is_none());
        let signal = match self.state.appearance {
            crate::appearance::Appearance::Dark => libc::SIGUSR1,
            crate::appearance::Appearance::Light => libc::SIGUSR2,
        };
        for terminal in &self.terminals {
            terminal.signal(signal);
        }
        if !self.terminals.is_empty() {
            tracing::info!(count = self.terminals.len(), appearance = self.state.appearance.name(), "retinted running terminals");
        }
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

    /// Tells the Dock what another shell has claimed off the primary's
    /// top and right edges, so it can step out of the way — the Wayland
    /// binary calls this with the layer-shell exclusive zones before it
    /// composes the workareas, because the answer changes what
    /// [`Shell::workarea`] says. Returns whether anything moved; a
    /// caller that re-pushes the same zones every dispatch pass pays
    /// for nothing.
    pub fn set_edge_reservation(&mut self, wm: &mut WindowManager<B>, reserved: EdgeReservation) -> bool {
        self.desktop.set_reservation(wm.backend_mut(), &self.theme, reserved)
    }

    /// Resolves a configured key combo to its action, for the binary's
    /// key interception. A miss MUST leave the event flowing through to
    /// `wm-core` unchanged — during a modal Alt+Tab session the
    /// switcher grabs the whole keyboard, and Tab/Escape/any-other-key
    /// arrive as ordinary `KeyPress` events that appear in no keymap;
    /// swallowing them would wedge the switcher open.
    ///
    /// The one exception is the shell's own modal session: while the
    /// Overview is open it holds the keyboard the same way the
    /// switcher does, and *every* press resolves to
    /// [`Action::Overview`] (the combo parked in `overview_key` for
    /// `run_action` to route — arrows move, Return commits, anything
    /// else dismisses). Nothing may flow through to `wm-core` then:
    /// an Alt+Tab leaking past an open Overview would start a second
    /// modal session on top of the first, and the two would fight over
    /// one keyboard grab. No Alt+Tab session can be live at that
    /// moment for the miss rule above to serve — the Overview declines
    /// to open during one (see `run_action`) and its grab intercepts
    /// the keys that would start one.
    pub fn keymap_action(&mut self, combo: &KeyCombo) -> Option<Action> {
        if self.desktop.overview_visible() {
            self.overview_key = Some(*combo);
            return Some(Action::Overview);
        }
        // The instrument panel's Escape. The key is only grabbed while
        // a panel is up (`Desktop::set_panel_key_grab`), so this arm is
        // unreachable otherwise; the dismissal itself is parked for
        // `tick`, which runs later in this same loop iteration and has
        // the `WindowManager` this method does not. The combo still
        // answers `None` — Escape is bound to nothing in the keymap,
        // and swallowing the flow-through would break the rule the
        // switcher relies on.
        if self.desktop.instrument_panel_visible()
            && combo.modifiers.is_empty()
            && combo.keysym == crate::desktop::PANEL_DISMISS_KEYSYM
        {
            self.panel_escape = true;
        }
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
                let terminal = spawn_terminal(self.state.terminal.as_deref(), &self.theme, self.state.terminal_font_px, terminal_screen(self));
                self.terminals.extend(terminal);
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
            // The window commands menu, from the keyboard. The whole
            // point is the window that has no titlebar to right-click:
            // a client that draws its own chrome gets no frame from us,
            // and before this verb the only route to its commands was
            // the Overview. `wm-core` reports the request through the
            // same `WindowMenuRequested` notification a titlebar
            // right-click raises, so the menu, its items and its
            // dispatch are shared verbatim.
            Action::WindowMenu => wm.request_window_menu_for_focused(),
            Action::WorkspaceNext => wm.switch_workspace(wm.current_workspace() + 1),
            Action::WorkspacePrev => {
                if wm.current_workspace() > 0 {
                    wm.switch_workspace(wm.current_workspace() - 1);
                }
            }
            // The modal Overview. Closed: open it (declined during a
            // live Alt+Tab session — two modal keyboard owners cannot
            // share one grab). Open: route the parked key press. The
            // toggle's own combo is recognized through the keymap
            // rather than hardcoded, so however the user rebinds
            // `overview`, a second press of that binding closes it.
            Action::Overview => {
                let key = self.overview_key.take();
                if !self.desktop.overview_visible() {
                    if wm.cycle_state().is_none() {
                        self.open_overview(wm);
                    }
                } else {
                    let rebound_toggle =
                        key.as_ref().is_some_and(|combo| self.keymap.get(combo) == Some(&Action::Overview));
                    match key {
                        Some(combo) if !rebound_toggle => match overview_intent(&combo) {
                            OverviewIntent::Move(dx, dy) => {
                                self.desktop.move_overview_selection(wm.backend_mut(), &self.theme, dx, dy)
                            }
                            OverviewIntent::Commit => self.commit_overview(wm),
                            OverviewIntent::Dismiss => self.close_overview(wm),
                        },
                        // The rebound toggle, or (defensively) no
                        // parked key at all: close. A missing key can
                        // only mean a caller ran the action without
                        // resolving a combo first, and "the toggle
                        // toggles" is the only safe reading.
                        _ => self.close_overview(wm),
                    }
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
            Action::Reload => self.reresolve(wm),
            // Re-exec the on-disk binary. Since `Action::Reload` exists
            // this is no longer the config hot-reload gesture; it is
            // how a session picks up a *new build* of itself, which is
            // the one thing it cannot do without exec.
            Action::Restart => return ShellOutcome::Restart,
            // Named argv from `[commands]`. The lookup cannot normally
            // miss — `wm_config::parse` drops any binding naming a
            // command it did not find — but the keymap and the command
            // table are two pieces of state that a reload replaces, and
            // a miss here is a warning rather than an assumption. The
            // desktop stays up either way; that is the whole contract
            // this crate's config layer is written to.
            Action::Run(name) => match self.state.commands.get(name) {
                Some(argv) => run_named_command(name, argv, self.state.scale),
                None => tracing::warn!(
                    command = %name,
                    "no such command in [commands]; nothing to run"
                ),
            },
        }
        ShellOutcome::Continue
    }

    /// Opens the Overview and takes the modal keyboard grab — the same
    /// `Backend::grab_keyboard` the Alt-Tab cycle uses, taken from the
    /// shell layer because this modality lives here. Grabbed only if
    /// the panel actually came up: grabbing beside a surface that
    /// failed to create would leave a desk with dead keys and nothing
    /// on screen explaining why.
    fn open_overview(&mut self, wm: &mut WindowManager<B>) {
        // The Overview covers the monitor the panel hangs on, and its
        // modal grab would eat the panel's Escape: exactly one of the
        // two shell modes at a time.
        if self.desktop.instrument_panel_visible() {
            self.desktop.dismiss_instrument_panel(wm.backend_mut(), PanelCloseReason::Dismissed);
        }
        self.populate_overview(wm);
        if self.desktop.overview_visible() {
            wm.backend_mut().grab_keyboard();
        }
    }

    /// Captures the current workspace into the panel — fresh previews
    /// every time, because windows change constantly and a stale
    /// thumbnail defeats the whole "live overview" promise. Shared by
    /// entry, the workspace-strip switch, and the mid-session refresh
    /// `on_notification` triggers when a card's window closes or
    /// miniaturizes under an open panel (a right-click menu pick can
    /// do both), so the grid can never keep showing a window that is
    /// gone.
    ///
    /// Miniaturized windows are included, visually asleep (dimmed
    /// preview, inactive titlebar — see `wm_theme::overview`):
    /// they live on this desk too, and the Overview restoring one in a
    /// single gesture is strictly more useful than pretending it is
    /// not there. Their preview is whatever the capture path can still
    /// produce for an unmapped window — `None` degrades to the empty
    /// well, never an error.
    fn populate_overview(&mut self, wm: &mut WindowManager<B>) {
        let current = wm.current_workspace();
        let mut items: Vec<OverviewItem<B>> = wm
            .iter_clients()
            .filter(|(_, client)| {
                client.workspace == current
                    && matches!(client.lifecycle, Lifecycle::Normal | Lifecycle::Miniaturized)
            })
            .map(|(id, client)| OverviewItem {
                client: id,
                window: client.window,
                title: client.title.clone(),
                preview: None,
                miniaturized: client.lifecycle == Lifecycle::Miniaturized,
            })
            .collect();
        // Previews in a second pass: `client_preview` borrows the WM
        // immutably, which the iteration above also does — but the
        // panel handoff below needs `backend_mut`, so everything is
        // gathered before the mutable borrow starts (the same
        // collect-then-paint dance `icon_clients` documents).
        for item in &mut items {
            item.preview = wm.client_preview(item.client);
        }
        let selected = wm
            .focused_client()
            .and_then(|focused| items.iter().position(|item| item.client == focused))
            .unwrap_or(0);
        let workspace = (current, wm.workspace_count());
        self.desktop.show_overview(wm.backend_mut(), &self.theme, items, workspace, selected);
    }

    /// Ends the session without committing: grab released first, so
    /// even a hide that finds no surface leaves the keyboard live.
    /// Any open menu goes with it — a window menu opened from one of
    /// the panel's cards would otherwise be left floating over the
    /// bare desktop after an Escape, commanding a card that no longer
    /// exists on screen. A no-op when no menu is open.
    fn close_overview(&mut self, wm: &mut WindowManager<B>) {
        wm.backend_mut().ungrab_keyboard();
        self.desktop.close_menu(wm.backend_mut());
        self.desktop.hide_overview(wm.backend_mut());
    }

    /// Commits the selection: close, then focus + raise the chosen
    /// window through the public `ActivateRequested` path (the same
    /// one a pager's `_NET_ACTIVE_WINDOW` message and the launcher
    /// strip ride), deminiaturizing first when the card was asleep —
    /// activating an unmapped window would set focus on nothing
    /// visible. Closing before activating keeps the raise honest: the
    /// full-screen panel is already gone when the window comes up.
    fn commit_overview(&mut self, wm: &mut WindowManager<B>) {
        let target = self
            .desktop
            .overview_item(self.desktop.overview_selected())
            .map(|item| (item.client, item.window, item.miniaturized));
        self.close_overview(wm);
        if let Some((client, window, miniaturized)) = target {
            if miniaturized {
                wm.deminiaturize(client);
            }
            wm.dispatch(BackendEvent::ActivateRequested(window));
        }
    }

    /// A click on the open Overview, both edges. The arm-on-press /
    /// commit-on-release convention every button in this theme
    /// follows: pressing a card selects it (visibly, on the highlight
    /// plate), releasing over the same card commits — so a press
    /// dragged off a card and released elsewhere changes the selection
    /// but commits nothing, exactly like backing out of a menu row.
    fn on_overview_click(&mut self, wm: &mut WindowManager<B>, local: Point, button: MouseButton, pressed: bool) -> ShellOutcome {
        let hit = self.desktop.overview_hit(local);
        if pressed {
            match (button, hit) {
                (MouseButton::Left, OverviewHit::Card(index)) => {
                    self.desktop.select_overview_card(wm.backend_mut(), &self.theme, index);
                }
                // Right-click: the same window-commands menu a
                // titlebar right-click opens, for the window this card
                // shows. The menu is its own shell surface stacked
                // over the panel and resolves through the ordinary
                // menu routing; a pick that changes the desk (close,
                // miniaturize) reaches the panel back through
                // `on_notification`'s refresh.
                (MouseButton::Right, OverviewHit::Card(index)) => {
                    self.desktop.select_overview_card(wm.backend_mut(), &self.theme, index);
                    if let Some(id) = self.desktop.overview_item(index).map(|item| item.client) {
                        if let Some(client) = wm.client(id) {
                            let ctx = WindowMenuContext {
                                client: id,
                                title: client.title.clone(),
                                shaded: client.flags.contains(ClientFlags::SHADED),
                                maximized: client.flags.intersects(ClientFlags::MAXIMIZED_H | ClientFlags::MAXIMIZED_V),
                                fullscreen: client.flags.contains(ClientFlags::FULLSCREEN),
                                workspace: client.workspace,
                                workspace_count: wm.workspace_count(),
                            };
                            let at = self.pointer_root;
                            self.desktop.open_window_menu(wm.backend_mut(), &self.theme, at, ctx);
                        }
                    }
                }
                // A workspace tile switches the desk under the open
                // panel and re-populates the grid with fresh captures
                // of what just became visible — the panel stays up,
                // which is the point of having the strip at all.
                (MouseButton::Left, OverviewHit::Workspace(target)) => {
                    if target != wm.current_workspace() {
                        wm.switch_workspace(target);
                        self.populate_overview(wm);
                    }
                }
                // Pressing the empty panel backs out, like clicking
                // away from a menu.
                (MouseButton::Left, OverviewHit::Background) => self.close_overview(wm),
                _ => {}
            }
        } else if button == MouseButton::Left {
            if let OverviewHit::Card(index) = hit {
                if index == self.desktop.overview_selected() {
                    self.commit_overview(wm);
                }
            }
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
        // Any press on the bare desktop is a click away from an open
        // instrument panel; the press then keeps meaning what it always
        // did (a right press still opens the root menu).
        if self.desktop.instrument_panel_visible() {
            self.desktop.dismiss_instrument_panel(wm.backend_mut(), PanelCloseReason::Dismissed);
        }
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
        // The modal Overview first: while it is open it covers the
        // primary monitor, so a click on its surface can mean nothing
        // else — and no strip/icon drag can be in progress under a
        // panel that was opened from the keyboard. Clicks on surfaces
        // it does not own (a window menu opened from one of its cards)
        // fall through to the ordinary routing below.
        if self.desktop.overview_owns(surface) {
            // The selection surface rides over the panel, so a click
            // on the selected card lands surface-local to *it*; the
            // translation folds both surfaces into the one coordinate
            // space the layout hit-tests in.
            let local = self.desktop.overview_panel_point(surface, local);
            return self.on_overview_click(wm, local, button, pressed);
        }

        // The instrument panel. Clicks on its own surface go to the
        // owning dockapp as `PanelInput` (chrome is inert); a *press*
        // on any other shell surface is the click-away that dismisses
        // it — and then keeps routing, so the press still does what it
        // always did. The dock surface is exempted here because its
        // routing owns the subtler half (the owning tile's re-click is
        // a toggle whose press must be consumed) — see
        // `Desktop::dock_input`.
        if self.desktop.instrument_panel_visible() {
            if self.desktop.instrument_panel_owns(surface) {
                self.desktop.instrument_panel_click(&self.theme, local, button, pressed);
                return ShellOutcome::Continue;
            }
            if pressed && surface != self.desktop.dock_window() {
                self.desktop.dismiss_instrument_panel(wm.backend_mut(), PanelCloseReason::Dismissed);
            }
        }

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
                        let terminal = launch_app(self.state.terminal.as_deref(), &entry, &self.theme, self.state.terminal_font_px, terminal_screen(self));
                        self.terminals.extend(terminal);
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
                        //
                        // A built-in that offers a panel takes the
                        // button instead: right-click opens (and
                        // re-click toggles) its detail panel — the
                        // built-in counterpart of the panel a remote
                        // tile opens for itself after a click. The
                        // fall-through order costs nothing: a tile is
                        // remote (menu) or built-in (panel or
                        // nothing), never both.
                        if !self.desktop.open_dock_item_menu(wm.backend_mut(), &self.theme, local, self.pointer_root) {
                            self.desktop.toggle_builtin_panel(wm.backend_mut(), &self.theme, local);
                        }
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
        let mut fds = self.desktop.extra_poll_fds();
        fds.extend(self.control.poll_fds());
        fds
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
        let panel = self.desktop.instrument_panel_owns(surface);
        if !panel && surface != self.desktop.dock_window() {
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
            // The panel rides the same replay-as-discrete-steps rule as
            // the dock, for the same third-party-code reason.
            if panel {
                self.desktop.instrument_panel_scroll(&self.theme, local, step);
            } else {
                self.desktop.dock_input(wm.backend_mut(), &self.theme, DockInput::Scroll { local, delta: step });
            }
        }
    }

    /// The side-effect half of a root-menu pick; its outcome half is
    /// [`root_action_outcome`], which `on_shell_click` pairs with this
    /// so the split can never let a pick's act and its outcome drift.
    fn run_root_menu_action(&mut self, wm: &mut WindowManager<B>, action: RootMenuAction) {
        match action {
            RootMenuAction::LaunchTerminal => {
                let terminal = spawn_terminal(self.state.terminal.as_deref(), &self.theme, self.state.terminal_font_px, terminal_screen(self));
                self.terminals.extend(terminal);
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
                // that would duplicate `startup::resolve_look`'s
                // precedence (env, then config, then default) in a
                // second crate and drift from it silently. The launcher
                // knows the live answer; it should say so.
                //
                // Phase 4b's dockapp launch wants the same variable, and
                // a running dockapp additionally gets `ThemeChanged`
                // pushed down its socket — this env var is only how a
                // freshly-spawned one learns the theme it starts in.
                //
                // One env for every GUI the shell starts: `launch_env`
                // carries the theme id, the appearance beside it (the
                // pair `chonk_ui::active_theme` resolves to the exact
                // rendition this desktop is wearing), the scale, and
                // the same per-stack cursor-size rule every other
                // launch gets — chonk-about is a native client and must
                // not inherit the pre-multiplied process value.
                let env = launch_env(&self.theme.id, Some(self.state.appearance), self.state.scale);
                spawn::spawn_detached_with_env(&about_binary_path(), &[], &env, &[]);
            }
            // Indexes the same apps vec the desktop's menu was built
            // from, so `i` means the same entry on both sides; the
            // bounds-safe get covers the impossible desync anyway —
            // menus fire `Kill`-grade commands, so "impossible" still
            // doesn't get to panic.
            RootMenuAction::LaunchApp(i) => {
                if let Some(entry) = self.apps.get(i) {
                    let terminal = launch_app(self.state.terminal.as_deref(), entry, &self.theme, self.state.terminal_font_px, terminal_screen(self));
                    self.terminals.extend(terminal);
                } else {
                    tracing::warn!(index = i, count = self.apps.len(), "menu fired an out-of-range application index");
                }
            }
            RootMenuAction::OmarchyCommand { index, generation } => {
                match self.desktop.omarchy_command(index, generation) {
                    Some(command) => {
                        // A theme flow launched from this desktop's own
                        // menu carries an expectation the follow gate
                        // would otherwise swallow: the user asked for
                        // that theme, so when the pick lands (and only
                        // then — see the armed field), the desk adopts
                        // Omarchy's theme instead of silently keeping
                        // its own while Omarchy changes underneath.
                        if command.contains("omarchy-theme-set") && self.state.following.is_none() {
                            self.omarchy_adoption_armed = Some(std::time::Instant::now());
                        }
                        run_omarchy_command(&command, &self.theme);
                        self.desktop.note_omarchy_action_fired();
                    }
                    // Not a bug to shout about: the menu was open
                    // across a reload of the definition, and the
                    // generation guard did its job.
                    None => tracing::info!(index, generation, "omarchy menu pick outlived its definition; ignoring it"),
                }
            }
            RootMenuAction::SetWallpaper(wallpaper) => {
                self.desktop.set_wallpaper(wm.backend_mut(), &self.theme, wallpaper);
            }
            RootMenuAction::ToggleOmarchyBar => {
                self.desktop.toggle_omarchy_bar(wm.backend_mut());
            }
            RootMenuAction::SetTheme(id) if id == wm_theme::omarchy::ID => {
                // Following is a choice, not a theme: persist the choice
                // and re-resolve the session through the one path, which
                // reads Omarchy's palette (or wears the flagship and
                // waits, if there is none yet — `startup::resolve_look`).
                if let Err(e) = theme_select::persist(id) {
                    tracing::warn!(?e, id, "failed to persist theme selection");
                }
                let next = SessionState::resolve(&wm_config::load());
                self.adopt_wallpaper_of(wm, &next.base_theme);
                self.apply_session_state(wm, next);
            }
            RootMenuAction::SetTheme(id) => {
                // Resolved first: a pick naming a theme that does not
                // exist must change nothing at all, rather than persist
                // a choice this session then declines to wear. Not
                // reachable from the menu, which is generated from the
                // same list — but this is also the path a future
                // scripted theme change would take.
                // Resolved in the session's *current* appearance: a
                // theme pick changes which desktop you have, never
                // which side of the light/dark axis you are on.
                let Some(base) = wm_theme::default_theme::theme_variant(id, self.state.appearance) else {
                    tracing::warn!(theme = id, "theme menu named a theme that does not exist; keeping the current one");
                    return;
                };
                if let Err(e) = theme_select::persist(id) {
                    tracing::warn!(?e, id, "failed to persist theme selection");
                }
                self.adopt_wallpaper_of(wm, &base);
                self.apply_theme(wm, base);
            }
            RootMenuAction::Exit => {}
        }
    }

    /// A theme implies its wallpaper. Applied *and* persisted: applied
    /// because the desktop holds its wallpaper as loaded state that a
    /// restyle does not re-read, and persisted so the next session
    /// composes the same full look. The Wallpaper menu can still
    /// override it afterward.
    fn adopt_wallpaper_of(&mut self, wm: &mut WindowManager<B>, base: &Theme) {
        if let Some(paper) = wallpaper::Wallpaper::from_id(&base.wallpaper) {
            if let Err(e) = paper.persist() {
                tracing::warn!(?e, theme = %base.id, "failed to persist theme wallpaper");
            }
            self.desktop.set_wallpaper(wm.backend_mut(), &self.theme, paper);
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
        // The panel's Enter/Leave, from the same root-motion stream and
        // for the same never-latch reason.
        self.desktop.update_panel_hover(&self.theme, root);
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
            if self.desktop.overview_owns(surface) {
                // Hover is the switcher's selection treatment applied
                // by pointer: the card under the pointer becomes the
                // selection. `select_overview_card` restages only on
                // an actual change (and a change moves the small
                // selection surface, never the panel buffer), so
                // motion wandering inside one card costs nothing.
                // Motion over the selection surface itself arrives
                // local to it — translated back so leaving the
                // selected card's plate onto a neighbor still selects
                // the neighbor.
                let local = self.desktop.overview_panel_point(surface, local);
                if let OverviewHit::Card(index) = self.desktop.overview_hit(local) {
                    self.desktop.select_overview_card(wm.backend_mut(), &self.theme, index);
                }
            } else {
                self.desktop.hover_menu(wm.backend_mut(), &self.theme, surface, local);
            }
        }
    }

    /// Reacts to a `wm-core` state change the shell needs to know about
    /// but that `wm-core` itself has no opinion on — icon tiles for
    /// miniaturized windows, the Alt+Tab switcher, and the titlebar
    /// right-click window menu.
    pub fn on_notification(&mut self, wm: &mut WindowManager<B>, notification: Notification) {
        // An open Overview must track the desk it is showing: a window
        // that closed, mapped, miniaturized or restored while the
        // panel sat there (a right-click menu pick, an app exiting on
        // its own) would otherwise leave a card pointing at nothing —
        // and a click on that card would activate a ghost. Decided
        // before the match consumes the notification, applied after
        // the ordinary handling so icon tiles and the panel agree.
        let refresh_overview = self.desktop.overview_visible()
            && matches!(
                &notification,
                Notification::Miniaturized(..)
                    | Notification::Deminiaturized(_)
                    | Notification::Removed(_)
                    | Notification::Mapped(_)
            );
        match notification {
            Notification::Miniaturized(id, preview) => {
                let title = wm.client(id).map(|c| c.title.clone()).unwrap_or_default();
                self.desktop.show_icon(wm.backend_mut(), &self.theme, id, &title, preview.as_ref());
            }
            Notification::Deminiaturized(id) | Notification::Removed(id) => {
                self.desktop.remove_icon_for_client(wm.backend_mut(), id);
            }
            // A freshly mapped window is where session restore lands:
            // the first pending record with this window's class claims
            // it (first-come-first-matched — see `session_layout`) and
            // its remembered geometry, workspace and shape are applied
            // through the same public calls the menus and keybindings
            // use. A window with no record — the steady state once
            // restore is over — follows normal placement untouched.
            Notification::Mapped(id) => {
                let Some(class) = wm.client(id).map(|client| client.class.clone()) else {
                    return;
                };
                let Some(record) = self.layout.claim(&class, std::time::Instant::now()) else {
                    return;
                };
                tracing::info!(class = %record.class, ?record.geometry, workspace = record.workspace, "restoring a recorded window");
                wm.set_client_content_geometry(id, record.geometry);
                if record.workspace != wm.current_workspace() {
                    wm.move_client_to_workspace(id, record.workspace);
                }
                // Geometry before flags, deliberately: maximize records
                // the current geometry as its restore point, so the
                // remembered rect must be in place first for a later
                // unmaximize to return to it.
                if record.maximized {
                    wm.maximize(id, MaximizeDirections::FULL);
                }
                if record.shaded {
                    wm.shade(id);
                }
                if record.miniaturized {
                    wm.miniaturize(id);
                }
            }
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
        if refresh_overview {
            self.populate_overview(wm);
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
        // The appearance-request file, consumed the way the binaries
        // consume the reload/restart markers and on the same cadence:
        // this method runs once per event-loop wakeup (~16ms), and the
        // check costs one failed read on a path that is almost never
        // there. Consumed-then-acted so a request is honored exactly
        // once; a request naming the mode the session is already in is
        // consumed and does nothing (`set_appearance`'s no-op arm).
        if let Some(request) = crate::appearance::take_request() {
            let mode = request.resolve(self.state.appearance);
            tracing::info!(requested = ?request, resolved = mode.name(), "appearance-request received");
            self.set_appearance(wm, mode);
        }
        // Following Omarchy: when its current theme changes on disk,
        // re-resolve exactly as a reload would. The watch rate-limits
        // itself to a look per second and is consulted while following
        // — or while an adoption is armed, which is how a theme picked
        // through this desktop's menu can be noticed *before* the desk
        // follows. A session wearing a built-in with nothing armed
        // never polls.
        let armed = self
            .omarchy_adoption_armed
            .is_some_and(|since| since.elapsed() < ADOPTION_ARM_WINDOW);
        if self.state.following.is_none() && !armed {
            self.omarchy_adoption_armed = None;
        } else if self.omarchy.changed(std::time::Instant::now()) {
            if self.state.following.is_some() {
                tracing::info!("Omarchy's current theme or background changed; re-dressing");
                self.reresolve(wm);
                // The background is part of the look and the watch fires
                // for it too, but a background swap leaves the palette —
                // and so the resolved theme — exactly as it was, and
                // `apply_session_state` rightly repaints nothing for an
                // unchanged theme. The picture is repainted here instead,
                // and only if the desk is showing Omarchy's.
                self.desktop.refresh_wallpaper(wm.backend_mut());
            } else {
                // The armed pick landed: Omarchy's theme really changed,
                // so honor the intent expressed at the menu and follow —
                // through the same persisted path the menu's own
                // "Omarchy" entry takes, so the choice survives restart.
                tracing::info!("adopting Omarchy's theme: it changed after a pick from this desktop's menu");
                self.omarchy_adoption_armed = None;
                self.run_root_menu_action(wm, RootMenuAction::SetTheme(wm_theme::omarchy::ID));
            }
        }
        if let Some(target) = self.desktop.take_workspace_request() {
            wm.switch_workspace(target);
        }
        self.service_control(wm);
        // The instrument panel's parked Escape (see `keymap_action`) —
        // consumed here because this is the first point after the key
        // event where a `WindowManager` is in hand, still inside the
        // same loop iteration.
        if std::mem::take(&mut self.panel_escape) {
            self.desktop.dismiss_instrument_panel(wm.backend_mut(), PanelCloseReason::Dismissed);
        }
        // The Overview's one-shot preview catch-up: the panel opened
        // against whatever captures the backend had (icon-sized, on
        // the compositor), the entry hinted the card size, and this
        // fires exactly once when the backend reports card-sized
        // captures exist — see `OverviewPanel`'s preview-resolution
        // doc. Almost every call is one integer comparison.
        let generation = wm.backend().preview_generation();
        if self.desktop.overview_wants_fresh_previews(generation) {
            let previews: Vec<Option<DecorationBuffer>> = self
                .desktop
                .overview_clients()
                .into_iter()
                .map(|client| wm.client_preview(client))
                .collect();
            self.desktop.update_overview_previews(wm.backend_mut(), &self.theme, previews, generation);
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
        // The session-layout store rides the same cadence: hand it the
        // live arrangement and let its debounce decide whether the
        // moment has come to write. Almost every call is a comparison
        // that finds nothing changed and returns.
        self.layout.service(layout_snapshot(wm, &self.apps), std::time::Instant::now());
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
        // The layout does need one thing on the way out that the
        // paragraph above says nothing else does: a window closed
        // moments before this must be forgotten *now*, not after a
        // debounce that will never elapse.
        self.layout.flush();
        self.desktop.shut_down_dockapps(farewell);
        // Bars are told nothing but EOF, on both kinds of exit: a hot
        // restart rebinds the same path, and the spec tells a client
        // that loses the connection to reconnect and take the fresh
        // snapshot — which is also the whole recovery story, so there
        // is no farewell message to design.
        self.control.shut_down();
    }

    /// One pass of the control socket (`crate::control`): admit new
    /// bars, answer requests, publish what changed.
    ///
    /// The snapshot is taken only once someone is connected, so a
    /// desktop without a bar pays one `accept` that returns nothing.
    /// A `focus-workspace` a client asked for is applied here and the
    /// result published in the same pass, so the client's answer is
    /// the `workspaces` event the switch produced, not one a tick late.
    fn service_control(&mut self, wm: &mut WindowManager<B>) {
        self.control.accept();
        if !self.control.has_clients() {
            return;
        }
        let commands = self.control.service(&self.control_snapshot(wm));
        if commands.is_empty() {
            return;
        }
        for command in commands {
            match command {
                control::Command::FocusWorkspace(index) => wm.switch_workspace(index),
            }
        }
        self.control.publish(&self.control_snapshot(wm));
    }

    fn control_snapshot(&self, wm: &WindowManager<B>) -> control::Snapshot {
        control::snapshot(
            wm,
            &control::Surroundings {
                theme: &self.theme,
                appearance: self.state.appearance,
                scale: self.state.scale,
                pointer_root: self.pointer_root,
                // The *choice* to follow, not whether the palette was
                // found: a bar showing "Omarchy" while the flagship
                // stands in is telling the truth about what the desk
                // will wear the moment `omarchy-theme-set` runs.
                following: self.state.following.map(str::to_string),
            },
        )
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
    fn the_standard_terminal_shape_fits_any_ordinary_desk() {
        // 18px on a 1920x1080 logical head: 80x24 must open as cells.
        let theme = wm_theme::default_theme::nextstep_classic();
        assert!(standard_cells_fit(18.0, &theme, Size::new(1920, 1080)));
        // A modest laptop at a modest font still takes the standard.
        assert!(standard_cells_fit(16.0, &theme, Size::new(1280, 800)));
    }

    #[test]
    fn a_pathological_font_falls_back_to_the_fitted_pixel_size() {
        // The config ceiling: 96px makes 80 columns wider than any
        // screen this side of a video wall — the fallback must engage
        // rather than let the frame march off the edge.
        let theme = wm_theme::default_theme::nextstep_classic();
        assert!(!standard_cells_fit(96.0, &theme, Size::new(1920, 1080)));
        let (w, h) = terminal_window_size(&theme, Size::new(1920, 1080));
        assert!(w <= 1920 && h <= 1080);
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
    fn bare_arrows_and_return_drive_the_overview_and_everything_else_dismisses() {
        use wm_core::Modifiers;
        let bare = |keysym| KeyCombo { keysym, modifiers: Modifiers::empty() };
        assert_eq!(overview_intent(&bare(XK_LEFT)), OverviewIntent::Move(-1, 0));
        assert_eq!(overview_intent(&bare(XK_RIGHT)), OverviewIntent::Move(1, 0));
        assert_eq!(overview_intent(&bare(XK_UP)), OverviewIntent::Move(0, -1));
        assert_eq!(overview_intent(&bare(XK_DOWN)), OverviewIntent::Move(0, 1));
        assert_eq!(overview_intent(&bare(XK_RETURN)), OverviewIntent::Commit);
        assert_eq!(overview_intent(&bare(XK_KP_ENTER)), OverviewIntent::Commit);
        // Escape dismisses by convention; any other stray key steps
        // aside the way the Alt-Tab switcher does rather than eating
        // the user's typing.
        assert_eq!(overview_intent(&bare(0xff1b)), OverviewIntent::Dismiss);
        assert_eq!(overview_intent(&bare('a' as u32)), OverviewIntent::Dismiss);
    }

    #[test]
    fn modified_arrows_dismiss_instead_of_moving_the_selection() {
        // A workspace chord (alt+ctrl+right) pressed out of habit over
        // an open Overview must close it, not invisibly become
        // selection movement bound to nothing the user chose.
        let chord = KeyCombo { keysym: XK_RIGHT, modifiers: Modifiers::ALT | Modifiers::CONTROL };
        assert_eq!(overview_intent(&chord), OverviewIntent::Dismiss);
        let super_up = KeyCombo { keysym: XK_UP, modifiers: Modifiers::SUPER };
        assert_eq!(overview_intent(&super_up), OverviewIntent::Dismiss);
    }

    #[test]
    fn every_other_root_action_continues_the_session() {
        for action in [
            RootMenuAction::LaunchTerminal,
            RootMenuAction::LaunchAbout,
            RootMenuAction::LaunchApp(0),
            RootMenuAction::OmarchyCommand { index: 0, generation: 1 },
            RootMenuAction::SetWallpaper(Wallpaper::LavenderGrid),
        ] {
            assert_eq!(root_action_outcome(&action), ShellOutcome::Continue);
        }
    }
}
