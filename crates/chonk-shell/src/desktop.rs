//! The desktop shell: a content-sized Dock (an identity tile plus its
//! widgets, NeXTSTEP-style, at the top-right of the screen), the
//! right-click root menu, and icon tiles for miniaturized windows.
//! None of these are "clients" from `wm-core`'s perspective — they're
//! unmanaged shell surfaces (`Backend::ShellId`) the shell owns and
//! draws directly with
//! `wm-theme`'s public `paint`/`tile` primitives and its
//! `menu`/`clock`/`icon` renderers — the same SDK surface a third-party
//! `chonk-ui` app draws with, so the shell has no rendering code a real
//! app couldn't also use. Every square surface here sits on the tile
//! platform (`wm_theme::tile`): one face, relief, and ink recipe shared
//! with the Clip and every widget, so the dock reads as one family.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use wm_core::{Backend, ClientId, DragHandle};
use wm_theme::cascade::{CascadeMenu, MenuClick};
use wm_theme::menu::MenuItem;
use wm_theme::switcher::{self, SwitcherEntry};
use wm_theme::workspace;
use wm_theme::{icon, paint, panel, tile, Theme};
// `wm_theme_api::PopupHost` is deliberately referenced by full path in
// bounds rather than imported, and bounded per-method on `Desktop`'s
// menu-driving methods rather than on the whole impl: a receiver
// bounded by both it and `wm_core::Backend` at once makes every
// `backend.ungrab_pointer(..)` call ambiguous between the two traits
// (both name an `ungrab_pointer`), so the drag-teardown methods keep
// `Backend` as their only bound.
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use crate::dockapp::tile::{RemoteTile, ServiceContext, StopReason, TileState};
use crate::dockapp::{self, DockHost, Farewell};
use crate::overview::{OverviewHit, OverviewItem, OverviewPanel};
use crate::wallpaper::Wallpaper;
use crate::widgets::{
    run_detached, ClockWidget, DockInput, DockItem, DockWidget, Effect, NetTrafficWidget, PowerWidget, SamplerRegistry, SoundWidget, SupervisedWidget,
    SysLoadWidget, WifiWidget, WorkspaceShared,
};

/// The desktop background color — a cool lavender-gray sampled from a
/// reference NeXTSTEP desktop screenshot, not the neutral gray this
/// theme's window chrome uses. The dock has no separate backdrop panel
/// in that reference either: icons sit directly on this same color.
pub const DESKTOP_BG: (u8, u8, u8) = (128, 129, 159);

/// Remembering the order the user put the dock's tiles in.
///
/// # Why this had to exist before dockapps did
///
/// Middle-drag reordering has worked for as long as the dock has had
/// more than one tile, and until now it was never written down
/// anywhere. Every theme pick is a hot restart (`ShellOutcome::
/// Restart`), so the arrangement silently reverted on the most routine
/// thing a user does to this desktop. That was already a bug; a mixed
/// column of built-ins and out-of-process dockapps makes it a much
/// worse one, since "which tiles are in my dock, and where" stops being
/// a compile-time constant the moment a `.dockapp` registry exists.
///
/// One file describes the whole column, both kinds of tile, from day
/// one: one id per line, in top-to-bottom order, human-editable exactly
/// like the launcher's pin file and the theme and wallpaper files
/// beside it. Built-ins take the reserved
/// [`BUILTIN_PREFIX`](crate::widgets::BUILTIN_PREFIX) namespace so a
/// `.dockapp` declaring `id = "clock"` cannot displace the analog
/// clock.
///
/// # The rule about entries that do not resolve
///
/// An id in the file with no live tile behind it is *kept*, not
/// dropped. This is the difference between this and
/// `launchdock`'s pin file, and it is deliberate: a launcher pin that
/// stops resolving means an application was uninstalled, while a dock
/// entry that stops resolving is usually a dockapp that is between
/// versions, whose binary is mid-upgrade, or whose registry file is on
/// a filesystem that has not mounted yet. Forgetting where the user had
/// put it — permanently, on the next drag — because it happened to be
/// absent for one session is a worse answer than carrying a line
/// nobody can currently resolve. [`merge`] is what keeps it, and keeps
/// it *in place* rather than at the end.
pub(crate) mod dock_order {
    use std::path::{Path, PathBuf};

    /// `$XDG_STATE_HOME/chonkstep/dock-items`, or the `~/.local/state`
    /// fallback — the same resolution as `launchdock`'s `dock` file,
    /// `theme_select.rs`'s and `wallpaper.rs`'s, which all live in this
    /// same directory. The name is `dock-items` rather than `dock`
    /// because `dock` is already taken by the launcher strip's pins,
    /// and two files a user may edit by hand should not be one
    /// character apart in meaning.
    pub(crate) fn state_path() -> Option<PathBuf> {
        if let Some(root) = std::env::var_os("XDG_STATE_HOME") {
            return Some(PathBuf::from(root).join("chonkstep/dock-items"));
        }
        std::env::var_os("HOME").map(PathBuf::from).map(|home| home.join(".local/state/chonkstep/dock-items"))
    }

    /// The remembered order, or an empty list if there is no file yet
    /// (a fresh session, which then gets the built-in default order).
    ///
    /// Blank lines and `#` comments are skipped, because this is a file
    /// people are invited to edit and a file people edit acquires
    /// comments.
    pub(crate) fn load(path: &Path) -> Vec<String> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        text.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(str::to_string)
            .collect()
    }

    /// Writes one id per line.
    pub(crate) fn save(path: &Path, ids: &[String]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = String::new();
        for id in ids {
            text.push_str(id);
            text.push('\n');
        }
        std::fs::write(path, text)
    }

    /// Sorts `items` into the remembered order.
    ///
    /// Two rules, and both matter:
    ///
    /// * A remembered id with no live item is skipped here and
    ///   *preserved* by [`merge`] on the next write. See the module
    ///   docs.
    /// * A live item nobody remembers keeps its position relative to
    ///   the other unremembered items and lands after everything that
    ///   was remembered. That is what happens when an upgrade adds a
    ///   seventh instrument or the user drops in a new dockapp: it
    ///   appears at the bottom of the column, which is predictable,
    ///   rather than in the middle of an arrangement they built.
    ///
    /// Generic over the key so the whole rule is testable against
    /// plain strings, with no dock, no backend and no widgets.
    pub(crate) fn arrange<T>(items: Vec<T>, order: &[String], id_of: impl Fn(&T) -> String) -> Vec<T> {
        if order.is_empty() {
            return items;
        }
        let mut keyed: Vec<(String, Option<T>)> = items.into_iter().map(|item| (id_of(&item), Some(item))).collect();
        let mut arranged = Vec::with_capacity(keyed.len());
        for wanted in order {
            if let Some(slot) = keyed.iter_mut().find(|(id, item)| id == wanted && item.is_some()) {
                arranged.push(slot.1.take().expect("just checked it is Some"));
            }
        }
        arranged.extend(keyed.into_iter().filter_map(|(_, item)| item));
        arranged
    }

    /// The line-for-line contents to write after a reorder: the live
    /// column, plus every remembered id that did not resolve this
    /// session, put back where it was.
    ///
    /// "Where it was" means *after the same neighbour it used to
    /// follow*. Walking `remembered` and re-inserting each unresolved
    /// id after its nearest still-live predecessor keeps a dockapp that
    /// sat between the clock and the power tile between the clock and
    /// the power tile, through however many sessions it takes for its
    /// registry file to come back. Appending them at the end would be
    /// simpler and would quietly relocate every one of them to the
    /// bottom of the dock — which is the same forgetting this exists to
    /// prevent, one step slower.
    pub(crate) fn merge(live: &[String], remembered: &[String]) -> Vec<String> {
        let mut out: Vec<String> = live.to_vec();
        let mut anchor: Option<String> = None;
        for id in remembered {
            if live.contains(id) {
                anchor = Some(id.clone());
                continue;
            }
            if out.contains(id) {
                continue;
            }
            let at = match &anchor {
                // Position after the neighbour it used to follow. The
                // anchor is live by construction, so the `position` is
                // always found.
                Some(previous) => out.iter().position(|live| live == previous).map_or(0, |index| index + 1),
                // It was the very first line, and nothing before it
                // survives: the top of the column is where it goes.
                None => 0,
            };
            out.insert(at, id.clone());
            anchor = Some(id.clone());
        }
        out
    }
}

pub enum RootMenuAction {
    LaunchTerminal,
    LaunchAbout,
    /// An entry picked from the Applications submenu — the payload is
    /// an index into the same scanned `Vec<AppEntry>` handed to
    /// `Desktop::new` (read back through `Desktop::apps`), not a menu
    /// row position: the menu regroups entries by category, but the
    /// flat index is the identity both sides agree on.
    LaunchApp(usize),
    SetWallpaper(Wallpaper),
    /// The stable id of a built-in theme (`wm_theme::default_theme::
    /// CHOICES`) — handled by the shell orchestration (`crate::shell`),
    /// which persists it and reports `ShellOutcome::Restart` so the
    /// backend binary hot-restarts in place to redress every surface
    /// at once.
    SetTheme(&'static str),
    Exit,
}

/// Snapshot of one client's menu-relevant state at the moment its
/// commands menu opens — the event loop builds this from the live
/// client when `Notification::WindowMenuRequested` arrives. The item
/// labels reflect this snapshot ("Maximize" vs "Unmaximize"); the
/// action a pick eventually fires re-reads live state inside
/// `wm-core`, so a snapshot is all the menu itself ever needs.
pub struct WindowMenuContext {
    pub client: ClientId,
    pub title: String,
    pub shaded: bool,
    pub maximized: bool,
    pub fullscreen: bool,
    /// The window's current workspace (0-based) — bullet-marked in the
    /// Move To submenu the same way the root menu marks the active
    /// theme and wallpaper.
    pub workspace: usize,
    pub workspace_count: usize,
}

/// A pick from the per-window commands menu. Every variant maps onto
/// an existing `WindowManager` method; the menu adds no behavior of
/// its own, it only names things the WM can already do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowMenuAction {
    ToggleMaximize,
    Miniaturize,
    ToggleShade,
    ToggleFullscreen,
    /// 0-based; a value of `workspace_count` itself means "New
    /// Workspace" — the grow-on-demand convention
    /// `move_client_to_workspace` already supports.
    MoveToWorkspace(usize),
    /// The polite WM_DELETE_WINDOW request.
    Close,
    /// XKillClient — the last resort for a hung client that ignores
    /// `Close`, which is exactly why the menu carries both entries.
    Kill,
}

/// What a resolved menu click means, tagged by which kind of session
/// fired it — the root menu and the window menu share one popup stack
/// (see `ShellMenu`), so the one shared click path has to say which
/// menu actually spoke.
pub enum MenuAction {
    Root(RootMenuAction),
    Window(ClientId, WindowMenuAction),
    /// A pick from a dock tile's own right-click menu, carrying the
    /// tile's persistence id rather than its slot index: the column can
    /// be reordered (or a tile can crash out of it) while the menu sits
    /// open, and an index would then name a different tile than the one
    /// the user right-clicked.
    DockItem(String, DockItemMenuAction),
}

/// What a dock tile's own menu offers.
///
/// Only remote tiles have one. A built-in instrument is part of the
/// compositor: "Remove" would mean editing the dock's default column
/// and "Restart" would mean restarting the shell, neither of which is
/// what the word says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockItemMenuAction {
    /// Stop the process, clear the crash-loop budget, launch again.
    /// The one gesture that resets the budget — see
    /// `dockapp::tile::LaunchBudget`.
    Restart,
    /// Stop the process and take the tile out of the column.
    ///
    /// Session-scoped, deliberately and visibly: the dockapp is still
    /// registered, so a fresh session brings it back. Making removal
    /// permanent would mean a second state file recording "things the
    /// user does not want", whose failure mode is a dockapp that is
    /// installed, enabled, and invisible for reasons nothing in the UI
    /// explains. The log line names the `.dockapp` file, which is the
    /// thing to delete or edit for a permanent answer.
    Remove,
    /// Open the facts submenu: id, declaring file, state, pid.
    About,
}

// The action-id namespace for both menus, kept in documented, disjoint
// ranges so a fired id can never even *look* like it belongs to the
// other menu — `resolve_session_action`'s session check is the real
// guard, the ranges keep the ids honest and debuggable:
//   1..=99    root menu one-off entries
//   100..=199 root Wallpaper submenu (`ACTION_WALLPAPER_BASE` + index)
//   200..=299 root Theme submenu (`ACTION_THEME_BASE` + index)
//   300..=399 window menu commands (`ACTION_WINDOW_*`)
//   400..     window menu Move To entries (`ACTION_MOVE_TO_BASE` + n,
//             where n == the workspace count means "New Workspace")
//   500..=549 dock tile menu commands (`ACTION_DOCK_*`)
//   550..=599 dock tile About rows, which resolve to nothing
//             — both ranges sit numerically inside the open-ended Move
//             To one, so here (as with the Applications range below)
//             the session check genuinely is the guard rather than the
//             suspenders: a session that opened a dock tile's menu will
//             not resolve a Move To id, and vice versa
//   1000..    root Applications entries (`ACTION_APP_BASE` + the
//             app's index into the stored `.desktop` index) —
//             numerically past any Move To id a real session reaches,
//             but both ranges are open-ended, so here the session
//             check genuinely is the guard, not just the suspenders
const ACTION_LAUNCH_TERMINAL: u32 = 1;
const ACTION_LAUNCH_ABOUT: u32 = 2;
const ACTION_EXIT: u32 = 3;
const ACTION_WALLPAPER_BASE: u32 = 100;
const ACTION_THEME_BASE: u32 = 200;
const ACTION_WINDOW_MAXIMIZE: u32 = 300;
const ACTION_WINDOW_MINIATURIZE: u32 = 301;
const ACTION_WINDOW_SHADE: u32 = 302;
const ACTION_WINDOW_FULLSCREEN: u32 = 303;
const ACTION_WINDOW_CLOSE: u32 = 304;
const ACTION_WINDOW_KILL: u32 = 305;
const ACTION_MOVE_TO_BASE: u32 = 400;
const ACTION_DOCK_RESTART: u32 = 500;
const ACTION_DOCK_REMOVE: u32 = 501;
/// Rows in the About submenu. They fire ids that resolve to nothing,
/// which dismisses the menu — the classic behaviour of a menu entry
/// with nothing to do. `MenuItem` has no disabled variant, and adding
/// one to the theme SDK to grey out four lines of diagnostics would be
/// a change to every menu in the desktop for the benefit of this one.
const ACTION_DOCK_ABOUT_ROW: u32 = 550;
const ACTION_APP_BASE: u32 = 1000;

/// The root menu's fixed title — also what a fresh `ShellMenu` is
/// titled before any session opens.
const ROOT_MENU_TITLE: &str = "chonkstep";

/// The shared current-choice marker: a leading bullet on the selected
/// row, matching spaces on every other row so all labels in the column
/// start at the same x. Used by the root menu's Theme and Wallpaper
/// submenus and the window menu's Move To submenu alike.
fn bullet_label(selected: bool, label: &str) -> String {
    if selected {
        format!("\u{2022} {label}")
    } else {
        format!("  {label}")
    }
}

/// The Applications submenu body, generated from the scanned
/// `.desktop` index: one cascade per `AppCategory` that actually has
/// entries — an empty category would render as a dead-end cascade, so
/// it simply doesn't exist — in the enum's derived order (a `BTreeMap`
/// keyed by the category iterates exactly that order, no hand-kept
/// category list to drift out of sync with the enum). Within a
/// cascade, apps keep their index order: `scan_applications` delivers
/// the flat vec sorted by name, and filtering by category preserves
/// that, so each cascade is alphabetical for free. "About chonkstep"
/// closes the submenu after every cascade — with an empty index it is
/// the whole submenu, so Applications never opens onto nothing.
fn applications_items(apps: &[crate::apps::AppEntry]) -> Vec<MenuItem> {
    let mut by_category: BTreeMap<crate::apps::AppCategory, Vec<MenuItem>> = BTreeMap::new();
    for (index, app) in apps.iter().enumerate() {
        by_category.entry(app.category).or_default().push(MenuItem::Action {
            label: app.name.clone(),
            // The flat index, not a per-category position — the id has
            // to round-trip back into the stored vec (see
            // `RootMenuAction::LaunchApp`).
            action: ACTION_APP_BASE + index as u32,
        });
    }
    by_category
        .into_iter()
        .map(|(category, entries)| MenuItem::Submenu { label: category.label().to_string(), items: entries })
        .chain(std::iter::once(MenuItem::Action {
            label: "About chonkstep".to_string(),
            action: ACTION_LAUNCH_ABOUT,
        }))
        .collect()
}

fn root_menu_items(selected_wallpaper: Wallpaper, selected_theme_id: &str, apps: &[crate::apps::AppEntry]) -> Vec<MenuItem> {
    let wallpaper_items = Wallpaper::ALL
        .into_iter()
        .enumerate()
        .map(|(index, wallpaper)| MenuItem::Action {
            label: bullet_label(wallpaper == selected_wallpaper, wallpaper.label()),
            action: ACTION_WALLPAPER_BASE + index as u32,
        })
        .collect();
    let theme_items = wm_theme::default_theme::CHOICES
        .iter()
        .enumerate()
        .map(|(index, (id, label))| MenuItem::Action {
            label: bullet_label(*id == selected_theme_id, label),
            action: ACTION_THEME_BASE + index as u32,
        })
        .collect();

    vec![
        MenuItem::Action { label: "Terminal".to_string(), action: ACTION_LAUNCH_TERMINAL },
        MenuItem::Submenu { label: "Applications".to_string(), items: applications_items(apps) },
        MenuItem::Submenu { label: "Theme".to_string(), items: theme_items },
        MenuItem::Submenu { label: "Wallpaper".to_string(), items: wallpaper_items },
        MenuItem::Action { label: "Exit".to_string(), action: ACTION_EXIT },
    ]
}

/// `app_count` bounds the `ACTION_APP_BASE +` range: the length of the
/// app index the fired menu was built from, so a stale or corrupt id
/// past the vec's end dissolves to `None` instead of indexing out of
/// bounds downstream.
fn resolve_action(action: u32, app_count: usize) -> Option<RootMenuAction> {
    match action {
        ACTION_LAUNCH_TERMINAL => Some(RootMenuAction::LaunchTerminal),
        ACTION_LAUNCH_ABOUT => Some(RootMenuAction::LaunchAbout),
        ACTION_EXIT => Some(RootMenuAction::Exit),
        // Subtraction-then-compare rather than a `Range::contains`:
        // `ACTION_APP_BASE + app_count as u32` could in principle
        // overflow u32, and the subtraction form has no such edge.
        action if action >= ACTION_APP_BASE
            && ((action - ACTION_APP_BASE) as usize) < app_count =>
            Some(RootMenuAction::LaunchApp((action - ACTION_APP_BASE) as usize)),
        action if (ACTION_WALLPAPER_BASE..ACTION_WALLPAPER_BASE + Wallpaper::ALL.len() as u32)
            .contains(&action) => Some(RootMenuAction::SetWallpaper(
            Wallpaper::ALL[(action - ACTION_WALLPAPER_BASE) as usize],
        )),
        action if (ACTION_THEME_BASE
            ..ACTION_THEME_BASE + wm_theme::default_theme::CHOICES.len() as u32)
            .contains(&action) => Some(RootMenuAction::SetTheme(
            wm_theme::default_theme::CHOICES[(action - ACTION_THEME_BASE) as usize].0,
        )),
        _ => None,
    }
}

/// Longest window title the commands menu's title strip will show,
/// ellipsis included. Menus are content-sized (`menu::render_menu`
/// widens the popup to fit the title as well as the items), so an
/// unbounded title — a browser or xterm happily puts a whole URL there
/// — would stretch the popup across the screen. The classic recipe
/// bounds menu text the same way, truncating a long name to a fixed
/// character width. 24 comfortably out-measures every fixed item
/// label, so truncation only engages for genuinely long titles.
const WINDOW_MENU_TITLE_MAX_CHARS: usize = 24;

/// Truncation counts characters, not bytes — slicing a UTF-8 title at
/// a byte offset could split a code point and panic. The ellipsis
/// occupies the final slot of the cap rather than extending past it,
/// so the strip never exceeds `WINDOW_MENU_TITLE_MAX_CHARS` glyphs.
fn window_menu_title(title: &str) -> String {
    if title.chars().count() <= WINDOW_MENU_TITLE_MAX_CHARS {
        return title.to_string();
    }
    let mut truncated: String = title.chars().take(WINDOW_MENU_TITLE_MAX_CHARS - 1).collect();
    truncated.push('\u{2026}');
    truncated
}

/// The per-window commands menu, in the classic entry order —
/// Maximize, Miniaturize, Shade, Move To, Close, Kill — with
/// Fullscreen standing in for the "Other maximization" cascade this WM
/// doesn't have. Labels flip to their undo forms from the context
/// snapshot, so an already-maximized window offers Unmaximize in the
/// slot Maximize would otherwise hold.
fn window_menu_items(ctx: &WindowMenuContext) -> Vec<MenuItem> {
    let move_to = (0..ctx.workspace_count)
        .map(|n| MenuItem::Action {
            // 1-based labels over 0-based payloads: users count
            // workspaces from one, `move_client_to_workspace` from
            // zero.
            label: bullet_label(n == ctx.workspace, &format!("Workspace {}", n + 1)),
            action: ACTION_MOVE_TO_BASE + n as u32,
        })
        .chain(std::iter::once(MenuItem::Action {
            // One past the last existing workspace: resolves to
            // `MoveToWorkspace(workspace_count)`, which
            // `move_client_to_workspace` grows on demand. The
            // never-selected bullet gutter keeps its label aligned
            // with the workspace rows above it.
            label: bullet_label(false, "New Workspace"),
            action: ACTION_MOVE_TO_BASE + ctx.workspace_count as u32,
        }))
        .collect();

    vec![
        MenuItem::Action {
            label: if ctx.maximized { "Unmaximize" } else { "Maximize" }.to_string(),
            action: ACTION_WINDOW_MAXIMIZE,
        },
        MenuItem::Action { label: "Miniaturize".to_string(), action: ACTION_WINDOW_MINIATURIZE },
        MenuItem::Action {
            label: if ctx.shaded { "Unshade" } else { "Shade" }.to_string(),
            action: ACTION_WINDOW_SHADE,
        },
        MenuItem::Action {
            label: if ctx.fullscreen { "Exit Fullscreen" } else { "Fullscreen" }.to_string(),
            action: ACTION_WINDOW_FULLSCREEN,
        },
        MenuItem::Submenu { label: "Move To".to_string(), items: move_to },
        MenuItem::Action { label: "Close".to_string(), action: ACTION_WINDOW_CLOSE },
        MenuItem::Action { label: "Kill".to_string(), action: ACTION_WINDOW_KILL },
    ]
}

fn resolve_window_action(action: u32, workspace_count: usize) -> Option<WindowMenuAction> {
    match action {
        ACTION_WINDOW_MAXIMIZE => Some(WindowMenuAction::ToggleMaximize),
        ACTION_WINDOW_MINIATURIZE => Some(WindowMenuAction::Miniaturize),
        ACTION_WINDOW_SHADE => Some(WindowMenuAction::ToggleShade),
        ACTION_WINDOW_FULLSCREEN => Some(WindowMenuAction::ToggleFullscreen),
        ACTION_WINDOW_CLOSE => Some(WindowMenuAction::Close),
        ACTION_WINDOW_KILL => Some(WindowMenuAction::Kill),
        // `..=`, not `..`: one past the last workspace is the "New
        // Workspace" entry.
        action if (ACTION_MOVE_TO_BASE..=ACTION_MOVE_TO_BASE + workspace_count as u32)
            .contains(&action) => Some(WindowMenuAction::MoveToWorkspace(
            (action - ACTION_MOVE_TO_BASE) as usize,
        )),
        _ => None,
    }
}

/// Which menu the one shared popup stack currently hosts. The root
/// menu and the per-window commands menu deliberately share a single
/// `CascadeMenu` (exactly one menu session on screen — opening either
/// closes the other), so the shared click path needs a record of which
/// session opened last; resolving by id alone would silently make the
/// id ranges load-bearing for correctness instead of merely tidy.
enum MenuSession {
    Root {
        /// The app-index length the Applications submenu was built
        /// against: the resolver's bound for mapping `ACTION_APP_BASE
        /// + i` back into `LaunchApp(i)` — the root-session twin of
        /// the window session's `workspace_count`.
        app_count: usize,
    },
    /// A dock tile's own menu. Keyed by the tile's persistence id, not
    /// its slot: a middle-drag or a crash can change the column while
    /// the menu is open, and an index would then command a different
    /// tile than the one the user right-clicked.
    DockItem {
        id: String,
    },
    Window {
        /// Who the open menu commands — attached to every resolved
        /// action so the dispatch in `crate::shell` needs no other
        /// lookup.
        client: ClientId,
        /// The workspace count the Move To submenu was built against:
        /// the resolver's bound for mapping `ACTION_MOVE_TO_BASE + n`
        /// back into `MoveToWorkspace(n)`.
        workspace_count: usize,
    },
}

/// Decodes a fired action id strictly within the open session's own
/// namespace: a root id during a window session (or the reverse)
/// resolves to `None` — an effective dismissal, never a misattributed
/// command. The two menus already use disjoint id ranges, so this is
/// belt and suspenders — but menus fire commands as consequential as
/// `Kill`, and "which menu was open" is knowable, so it is checked
/// rather than assumed.
fn resolve_session_action(session: &MenuSession, action: u32) -> Option<MenuAction> {
    match session {
        MenuSession::Root { app_count } => resolve_action(action, *app_count).map(MenuAction::Root),
        MenuSession::DockItem { id } => resolve_dock_item_action(action).map(|command| MenuAction::DockItem(id.clone(), command)),
        MenuSession::Window { client, workspace_count } => {
            resolve_window_action(action, *workspace_count)
                .map(|window_action| MenuAction::Window(*client, window_action))
        }
    }
}

/// A dock tile menu id, or `None` for the About rows (which carry
/// diagnostics, not commands, and dismiss the menu when picked).
fn resolve_dock_item_action(action: u32) -> Option<DockItemMenuAction> {
    match action {
        ACTION_DOCK_RESTART => Some(DockItemMenuAction::Restart),
        ACTION_DOCK_REMOVE => Some(DockItemMenuAction::Remove),
        _ => None,
    }
}

/// The rows of one remote tile's menu.
///
/// About is a submenu of facts rather than a dialog, for the same
/// reason the window menu has no dialogs: this desktop has exactly one
/// popup mechanism and adding a second one for four lines of
/// diagnostics would be a new surface to lay out, theme, dismiss and
/// keep on the right monitor. What a user needs when a tile misbehaves
/// is which file declared it and what the shell currently thinks it is
/// doing, and those fit on rows.
fn dock_item_menu_items(tile: &RemoteTile, now: std::time::Instant) -> Vec<MenuItem> {
    let entry = tile.entry();
    let facts = vec![
        MenuItem::Action { label: format!("id: {}", entry.id), action: ACTION_DOCK_ABOUT_ROW },
        MenuItem::Action { label: format!("state: {}", describe_tile_state(tile.state(), now)), action: ACTION_DOCK_ABOUT_ROW },
        MenuItem::Action {
            label: match tile.pid() {
                Some(pid) => format!("pid: {pid}"),
                None => "pid: not running".to_string(),
            },
            action: ACTION_DOCK_ABOUT_ROW,
        },
        MenuItem::Action { label: format!("restart: {:?}", entry.restart), action: ACTION_DOCK_ABOUT_ROW },
        MenuItem::Action { label: format!("from: {}", entry.source.display()), action: ACTION_DOCK_ABOUT_ROW },
    ];
    vec![
        MenuItem::Action { label: "Restart".to_string(), action: ACTION_DOCK_RESTART },
        MenuItem::Action { label: "Remove".to_string(), action: ACTION_DOCK_REMOVE },
        MenuItem::Submenu { label: "About".to_string(), items: facts },
    ]
}

/// One tile state as a line a user can act on. Durations are included
/// where they are the whole story: "hung for 4s" and "hung for 40
/// minutes" call for different reactions.
fn describe_tile_state(state: TileState, now: std::time::Instant) -> String {
    match state {
        TileState::Waiting { until } => format!("restarting in {:?}", until.saturating_duration_since(now)),
        TileState::Starting { .. } => "starting".to_string(),
        // Worth naming rather than folding into "starting": the user
        // just restarted the shell, and "still running from before the
        // restart" is the interesting fact about this tile.
        TileState::Rejoining { until } => {
            format!("waiting {:?} for it to reconnect", until.saturating_duration_since(now))
        }
        TileState::Live => "running".to_string(),
        TileState::Hung { since } => format!("not responding for {:?}", now.saturating_duration_since(since)),
        TileState::Stopped { reason } => match reason {
            StopReason::CrashLooped => "stopped: crash-looped".to_string(),
            StopReason::PolicyNever => "stopped: restart = never".to_string(),
            StopReason::CleanExit => "stopped: exited cleanly".to_string(),
            StopReason::Removed => "stopped: removed".to_string(),
        },
    }
}

/// The title of a dock tile's menu — its label, so the menu says which
/// tile it belongs to on a column where several may look alike.
fn dock_item_menu_title(name: &str) -> String {
    name.to_string()
}

/// The desktop's single menu session: the root menu and the per-window
/// commands menu both run on this one `CascadeMenu`, tagged with which
/// kind of session is open so clicks resolve in the right namespace.
/// Generic over the popup id and driven through `PopupHost` alone —
/// no `Backend` in sight — so tests can drive real open/click
/// sequences against a fake host, the same seam `CascadeMenu`'s own
/// tests use; `Desktop` instantiates it at `B::ShellId`, the id type
/// its backend's `PopupHost` impl shares with the shell surfaces.
struct ShellMenu<Id> {
    menu: CascadeMenu<Id>,
    session: MenuSession,
}

impl<Id: Copy + Eq + std::fmt::Debug> ShellMenu<Id> {
    fn new() -> Self {
        // `app_count: 0` before any session opens: with no popup on
        // screen no click can reach the resolver, so the placeholder
        // bound is never consulted — and zero is the value that would
        // refuse every app id anyway.
        Self { menu: CascadeMenu::new(ROOT_MENU_TITLE, DESKTOP_BG), session: MenuSession::Root { app_count: 0 } }
    }

    /// Swaps in a fresh controller titled for the session about to
    /// open, closing whatever is on screen first. `CascadeMenu` fixes
    /// its title at construction (one app-identity title per
    /// controller), so a per-window title means a new controller — and
    /// the outgoing one must be explicitly closed before it is
    /// dropped: `CascadeMenu::open`'s own self-close cannot reach a
    /// predecessor the replacement never knew about, and a dropped but
    /// unclosed session would leak its popup windows and its pointer
    /// grab.
    fn begin_session<H: wm_theme_api::PopupHost<PopupId = Id>>(&mut self, host: &mut H, session: MenuSession, title: String) {
        self.menu.close(host);
        self.menu = CascadeMenu::new(title, DESKTOP_BG);
        self.session = session;
    }

    /// `app_count` must be the length of the same app index `items`
    /// was built from (`Desktop::open_root_menu` reads both from its
    /// one stored vec) — it becomes the session's bound for resolving
    /// `ACTION_APP_BASE +` ids back into `LaunchApp` indices.
    // Eight arguments, three over clippy's default. Grouping them into
    // a struct would only move the same eight values one line up at the
    // single call site, and this signature is deliberately a mirror of
    // `CascadeMenu::open`'s (host, theme, font system, items, position,
    // bounds) plus the two pieces of session identity — a reader
    // matching this against the SDK primitive it wraps is better served
    // by the parallel than by a bag.
    #[allow(clippy::too_many_arguments)]
    fn open_root<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        items: Vec<MenuItem>,
        app_count: usize,
        at: Point,
        bounds: Size,
    ) {
        self.begin_session(host, MenuSession::Root { app_count }, ROOT_MENU_TITLE.to_string());
        self.menu.open(host, theme, font_system, items, at, bounds, true);
    }

    // Nine arguments, two over clippy's default — and the same
    // judgement as `open_root` two methods down: this signature is
    // deliberately `CascadeMenu::open`'s (host, theme, font system,
    // items, position, bounds) plus the session's identity, and a
    // reader matching it against the SDK primitive it wraps is better
    // served by the parallel than by a bag that moves the same values
    // one line up at the single call site.
    #[allow(clippy::too_many_arguments)]
    fn open_dock_item<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        id: String,
        title: String,
        items: Vec<MenuItem>,
        at: Point,
        bounds: Size,
    ) {
        self.begin_session(host, MenuSession::DockItem { id }, title);
        // Not "closable": a tile menu is a transient command menu like
        // the window menu, not a posted one like the root menu, so it
        // gets no close box and vanishes on the first pick or miss.
        self.menu.open(host, theme, font_system, items, at, bounds, false);
    }

    fn open_window<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        ctx: &WindowMenuContext,
        at: Point,
        bounds: Size,
    ) {
        let session = MenuSession::Window { client: ctx.client, workspace_count: ctx.workspace_count };
        self.begin_session(host, session, window_menu_title(&ctx.title));
        self.menu.open(host, theme, font_system, window_menu_items(ctx), at, bounds, false);
    }

    /// See `Desktop::click_menu`, whose contract this implements.
    fn click<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        window: Id,
        local: Point,
    ) -> Option<MenuAction> {
        match self.menu.click(host, theme, font_system, window, local)? {
            MenuClick::Action(action) => resolve_session_action(&self.session, action),
            MenuClick::OpenedSubmenu | MenuClick::Dismissed => None,
        }
    }

    fn close<H: wm_theme_api::PopupHost<PopupId = Id>>(&mut self, host: &mut H) {
        self.menu.close(host);
    }

    fn hover<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        window: Id,
        local: Point,
    ) {
        self.menu.hover(host, theme, font_system, window, local);
    }

    fn tick<H: wm_theme_api::PopupHost<PopupId = Id>>(&mut self, host: &mut H, theme: &Theme, font_system: &mut cosmic_text::FontSystem) {
        self.menu.tick(host, theme, font_system);
    }
}

/// The Alt-Tab switch panel's popup window and the candidate set it
/// renders — see `Desktop::show_switcher`. The window is deliberately
/// long-lived: it is unmapped between sessions, not destroyed, and
/// only recreated when the rendered size changes. Destroying and
/// recreating it on every session wedged picom's xrender scene on the
/// VM (the dead panel kept compositing while live frames vanished —
/// confirmed live and cleared by a compositor restart), and rapid
/// map/unmap of one stable window is the churn compositors are
/// actually built for.
struct SwitcherPanel<Id> {
    window: Option<Id>,
    size: Size,
    entries: Vec<SwitcherEntry>,
    visible: bool,
}

struct IconTile<Id> {
    window: Id,
    client: ClientId,
    /// The window title this tile was drawn with. Kept so a live theme
    /// or scale change can re-render the tile without asking the window
    /// manager for it again — the tile outlives any particular pass
    /// through the shell, and a miniaturized client's title is not
    /// otherwise reachable from here.
    title: String,
    /// Current on-screen position — always authoritative, whether it
    /// came from `auto_slot`'s grid math or a manual drag.
    pos: Point,
    /// `Some(slot)` while this tile is still sitting where the
    /// auto-arrange grid put it; `None` once the user has dragged it
    /// anywhere, at which point it keeps its dragged `pos` forever and
    /// frees its old slot for the next auto-placed icon: a manually
    /// repositioned icon is permanently exempt from grid
    /// re-arrangement.
    auto_slot: Option<usize>,
}

/// A dock widget currently picked up for reordering (middle-click drag).
/// Unlike icon dragging, there's no separate window to move — every
/// widget lives in one shared dock pixmap — so this just tracks which
/// slot is "held"; `drag_widget_motion` does the actual reordering the
/// moment the pointer crosses into a neighboring slot.
struct ItemDrag {
    index: usize,
    grab: DragHandle,
}

/// An icon tile currently being pressed, possibly mid-drag.
struct IconDrag<Id> {
    window: Id,
    /// Where within the tile the press landed — kept constant relative
    /// to the tile for the whole drag, so the pointer doesn't visually
    /// "jump" to the tile's corner on the first motion event.
    grab_offset: Point,
    /// Crossed `DRAG_THRESHOLD_PX`? A plain click (press, tiny or no
    /// motion, release) restores the window instead of "moving" it —
    /// the press-never-moved rule is exactly why a click still works
    /// at all despite every press arming a potential drag.
    moved: bool,
    grab: DragHandle,
}

/// What releasing after an icon press should do — see `end_icon_drag`.
pub enum IconDragResult {
    /// The press/release was a plain click (never crossed the drag
    /// threshold): restore this client's window.
    Restore(ClientId),
    /// The icon was dragged to a new position. `root` is the pointer's
    /// root-relative position at release, so a drop target (the
    /// launcher dock's pin slots) hit-tests against where the pointer
    /// actually let go, not against the tile's top-left corner —
    /// dropping a tile whose corner hangs off a target while the
    /// pointer is squarely on it must still count.
    Repositioned { client: ClientId, root: Point },
}

/// Height of the visible Dock chrome only: one identity tile plus the
/// current height of every widget. It is capped to the monitor so an
/// unusually large future widget stack cannot request an invalidly
/// tall shell surface, but it never fills spare space merely because
/// it exists.
fn stacked_dock_height(tile: u32, screen_height: u32, items: &[SupervisedWidget]) -> u32 {
    items
        .iter()
        // `SupervisedWidget::tile_height` already floors a widget's own
        // answer at one tile, and already answers exactly one for an
        // evicted widget — so an evicted multi-tile instrument shrinks
        // the dock rather than leaving a hole where its extra tiles
        // used to be.
        .fold(tile, |height, item| height.saturating_add(tile.saturating_mul(item.tile_height())))
        .min(screen_height.max(1))
}

/// Root geometry of the Dock: a one-tile-wide column hugging the top-
/// right corner of the monitor it belongs to. Anchored to `primary`'s
/// own corner rather than to the screen's, because on a multi-head
/// desktop the screen's top-right corner belongs to whichever output
/// happens to sit furthest right — and the screen's origin can be
/// negative outright, for a second head placed left of the primary.
fn dock_geometry(primary: Rect, dock_width: u32, dock_height: u32) -> Rect {
    Rect {
        pos: Point::new(primary.pos.x + primary.size.w.saturating_sub(dock_width) as i32, primary.pos.y),
        size: Size::new(dock_width, dock_height),
    }
}

/// Root geometry of the Clip: a single tile in the primary monitor's
/// top-left corner (the Clip's stock position).
fn clip_geometry(primary: Rect, tile: u32) -> Rect {
    Rect { pos: primary.pos, size: Size::new(tile, tile) }
}

/// The dock/Clip/icon tile edge at `scale`, in device pixels.
///
/// One function rather than the formula written out wherever a tile is
/// measured, because it is measured in three places that must agree:
/// `Desktop::new`, `Desktop::set_scale`, and the launcher strip the
/// shell builds alongside the Clip (`crate::shell::Shell::new`). Two of
/// those already held the same literal expression; a live scale change
/// is exactly the kind of edit that updates one and not the other, and
/// a strip whose tiles are a different size from the Clip above them is
/// the visible result.
///
/// The floor is what keeps a sub-1.0 scale from producing a tile too
/// small to draw the chrome into at all.
pub fn tile_px(scale: f32) -> u32 {
    ((56.0 * scale).round() as u32).max(16)
}

/// Gap between miniwindow icon tiles on the desktop grid, in device
/// pixels. Not used for the dock, whose tiles touch flush by design —
/// see `Desktop::new`.
pub fn icon_pad_px(scale: f32) -> u32 {
    ((4.0 * scale).round() as u32).max(1)
}

/// How far the pointer must travel from a press before it counts as a
/// drag rather than a click, in device pixels — scaled so the gesture
/// feels the same at any UI scale.
pub fn drag_threshold_px(scale: f32) -> i32 {
    ((4.0 * scale).round() as i32).max(2)
}

/// Root position of miniwindow icon slot `slot`: tiles fill
/// left-to-right along the primary monitor's bottom edge and wrap
/// upward — the icon row layout in the reference NeXTSTEP screenshot,
/// clear of the Dock on the right.
fn icon_slot_position(primary: Rect, tile: u32, pad: u32, slot: usize) -> Point {
    let stride = tile + pad;
    let usable_width = primary.size.w.max(stride);
    let cols = (usable_width / stride).max(1) as usize;
    let (row, col) = (slot / cols, slot % cols);
    Point::new(
        primary.pos.x + pad as i32 + col as i32 * stride as i32,
        primary.pos.y + primary.size.h as i32 - ((row as u32 + 1) * stride) as i32,
    )
}

/// One workarea per monitor — the whole body of `Desktop::workareas`,
/// split out so the per-monitor rule is testable without standing up a
/// backend. The primary is matched by rect rather than by index because
/// that is the only identity the Desktop stores; two monitors with
/// identical rects would be indistinguishable, which is a configuration
/// no arrangement produces (two outputs cannot occupy the same space).
fn workareas_for(monitors: &[Rect], primary: Rect, primary_workarea: Rect) -> Vec<Rect> {
    monitors.iter().map(|&monitor| if monitor == primary { primary_workarea } else { monitor }).collect()
}

/// Root position that centers a `size` panel — the Alt-Tab switcher —
/// on the primary monitor, so it appears where the user is looking
/// rather than straddling the seam between two heads.
fn centered_on(primary: Rect, size: Size) -> Point {
    Point::new(
        primary.pos.x + (primary.size.w as i32 - size.w as i32) / 2,
        primary.pos.y + (primary.size.h as i32 - size.h as i32) / 2,
    )
}

/// The dock's default instrument stack, top to bottom, under the
/// identity tile: the five instruments — network traffic, system load,
/// sound, link, power — with the analog clock as the bookend at the
/// bottom, closing the rack the way the identity tile opens it (the two
/// non-instrument faces frame the glass screens between them).
/// Middle-click drag reorders live and `dock_order` remembers it; this
/// is only what a session with no remembered order starts from.
///
/// Each one carries its persistence id here, beside the constructor it
/// names, because this is the only place the pairing is visible at a
/// glance: the literal below is character-for-character the line that
/// appears in the user's `dock-items` file, so a grep for the line in
/// the file finds the line in the source.
///
/// Deliberately *not* derived from `DockWidget::name`, which is a
/// display label drawn on the dead-screen tile. Renaming "LOAD" is a
/// cosmetic change; if it doubled as the persistence key it would
/// silently reset every user's dock arrangement.
///
/// A free function rather than an inline array so that
/// `builtin_ids_are_reserved_and_unique` can check the ids without
/// standing up a backend — the constructors themselves are cheap and
/// touch nothing.
fn builtin_items() -> Vec<DockItem> {
    vec![
        DockItem::builtin("builtin:net", Box::new(NetTrafficWidget::new()) as Box<dyn DockWidget>),
        DockItem::builtin("builtin:sysload", Box::new(SysLoadWidget::new())),
        DockItem::builtin("builtin:sound", Box::new(SoundWidget::new())),
        DockItem::builtin("builtin:wifi", Box::new(WifiWidget::new())),
        DockItem::builtin("builtin:power", Box::new(PowerWidget::new())),
        DockItem::builtin("builtin:clock", Box::new(ClockWidget::new())),
    ]
}

pub struct Desktop<B: Backend> {
    dock_window: B::ShellId,
    /// The whole desktop's bounding box — every monitor at once. That
    /// is the surface the wallpaper has to cover and the extent menus
    /// are kept inside; it is *not* where chrome goes (see `primary`).
    screen: Size,
    /// The primary monitor's rect, in root coordinates: where every
    /// piece of the shell's own chrome hangs. Deliberately not derived
    /// from `screen` — see `dock_geometry`.
    primary: Rect,
    dock_width: u32,
    tile: u32,
    pad: u32,
    /// How far the pointer must move from the press point before a
    /// press-on-an-icon becomes a drag rather than a click — scaled
    /// like every other dock/icon dimension so it feels the same at any
    /// `CHONKSTEP_SCALE`.
    drag_threshold: i32,
    font_system: cosmic_text::FontSystem,
    swash_cache: cosmic_text::SwashCache,
    /// The one open menu session — root menu or per-window commands
    /// menu, whichever opened last — riding on `wm_theme::cascade::
    /// CascadeMenu`, a generic, reusable SDK primitive rather than
    /// desktop-shell-specific state; a `chonk-ui` app building its own
    /// dropdown menu over its own `PopupHost` gets the identical
    /// stack/hover/leak-safe teardown behavior for free. `ShellMenu`
    /// adds only the session-kind tag that keeps click resolution in
    /// the right action namespace.
    menu: ShellMenu<B::ShellId>,
    /// Every instrument shown below the identity tile, top to bottom —
    /// see `crate::widgets` for the SDK these implement. Order is what
    /// `redraw_dock` draws and what a middle-click drag reorders.
    /// Each one wrapped in a [`SupervisedWidget`], which is what times
    /// the calls across the trait boundary and evicts a widget that
    /// keeps blocking the repaint thread — see its doc comment for why
    /// the dock does not simply trust its widgets.
    items: Vec<SupervisedWidget>,
    /// Every registered out-of-process tile's listener and pending
    /// connections — see [`crate::dockapp`]. Separate from `items`
    /// because a `Hello` names an id and resolving an id to a slot
    /// means looking at the column, which is this type's business and
    /// not a socket's.
    dockapps: DockHost,
    /// The session's UI scale, kept because a launched dockapp has to
    /// be *told* it (`CHONKSTEP_SCALE`) — it has no display connection
    /// to ask, which is the whole point of it.
    scale: f32,
    /// The dock order as last read from (or written to) disk — see
    /// [`dock_order`].
    ///
    /// Kept rather than re-read, because it is the only record of ids
    /// that did not resolve this session, and those have to survive the
    /// next reorder's rewrite. See `dock_order::merge`.
    remembered_order: Vec<String>,
    /// Every sampler thread the widget stack asked for, and the
    /// readings they have produced — see `crate::widgets::sampling`.
    ///
    /// The dock owns these rather than the widgets, and that is the
    /// point rather than an implementation detail: a widget declares
    /// what it needs and is handed the result, so there is no moment at
    /// which one could read `/proc`, walk sysfs or wait on `nmcli` from
    /// the compositor's repaint thread. It also owns the executor for
    /// the [`Effect`]s a click returns, for exactly the same reason —
    /// `wpctl set-volume` arrives on this thread too.
    samplers: SamplerRegistry,
    /// The workspace state shared with the Clip tile — see
    /// `WorkspaceShared`'s doc comment for why this
    /// crosses the `Box<dyn DockWidget>` boundary as a shared cell
    /// rather than through the trait.
    workspace: Rc<RefCell<WorkspaceShared>>,
    /// The Clip: the workspace tile, pinned at the screen's top-left
    /// corner (its stock position) — corner arrows switch workspaces,
    /// the face shows the current one.
    clip_window: B::ShellId,
    /// What the Clip last rendered, so workspace churn repaints it
    /// exactly once per actual change.
    clip_drawn: (usize, usize),
    item_drag: Option<ItemDrag>,
    /// The id of the dock item the pointer is currently inside, so
    /// `Enter`/`Leave` are delivered once per crossing rather than once
    /// per motion event.
    ///
    /// An id rather than a slot index: the column can be reordered
    /// under a stationary pointer (a middle-drag does exactly that),
    /// and an index would then silently start naming a different tile
    /// without any crossing having happened.
    hovered_item: Option<String>,
    icons: HashMap<B::ShellId, IconTile<B::ShellId>>,
    icon_drag: Option<IconDrag<B::ShellId>>,
    wallpaper: Wallpaper,
    /// The Alt-Tab switch panel, while a cycle session is live.
    switcher: Option<SwitcherPanel<B::ShellId>>,
    /// The modal Overview panel — surface, entries, selection, layout.
    /// The shell orchestrator owns the modality (opening, the keyboard
    /// grab, key/click meaning); this is its drawing and hit-testing
    /// half, held here because rendering needs the desktop's font
    /// state. See `crate::overview`.
    overview: OverviewPanel<B>,
    /// Stable id of the active theme — only used to bullet-mark the
    /// Theme submenu; the `Theme` itself lives with the shell
    /// orchestration (`crate::shell::Shell`).
    theme_id: String,
    /// The scanned `.desktop` index, sorted by name — the one vec the
    /// Applications submenu is generated from and
    /// `RootMenuAction::LaunchApp` indexes back into. Captured once at
    /// startup: rescanning on a schedule is future work, and a stale
    /// menu entry merely fails to launch rather than misfiring.
    apps: Vec<crate::apps::AppEntry>,
    logo: Pixmap,
}

impl<B: Backend> Desktop<B> {
    /// `scale` multiplies every dock/icon pixel dimension — pass the
    /// same factor used for `Theme::scaled` so the shell's own chrome
    /// (which doesn't go through the theme engine) matches the WM's.
    /// `screen` is the whole desktop's bounding box (what the wallpaper
    /// covers); `primary` is the monitor rect every piece of chrome
    /// hangs on. On a single-monitor session the two agree, and every
    /// caller must still pass both — the shell cannot recover the
    /// primary's origin from a size.
    pub fn new(backend: &mut B, screen: Size, primary: Rect, scale: f32, theme_id: String, apps: Vec<crate::apps::AppEntry>) -> Self {
        let tile = tile_px(scale);
        let pad = icon_pad_px(scale);
        // The dock is exactly one tile wide, tiles touch directly with
        // no gap, and the identity tile sits flush at the very top —
        // the classic dock is a flush column of icons touching both
        // the screen edge and each other, not a WM convention of its
        // own. `pad` still spaces the *desktop's* icon grid
        // (miniaturized windows), which is a separate, unrelated piece
        // of chrome.
        let dock_width = tile;

        let wallpaper = Wallpaper::load();
        // The WM reports the real workspace state after startup; until
        // then "first of one" is what a fresh session actually has.
        let workspace = Rc::new(RefCell::new(WorkspaceShared { current: 0, count: 1, requested: None }));
        // (The workspace indicator used to live here as a dock widget;
        // it is now the Clip tile at the screen's top-left — see
        // `clip_window` below.)
        // The dockapp socket and registry, before the column is built:
        // a registered dockapp is a slot in the same list as a built-in
        // instrument, so the two have to be known at the same moment
        // for `dock_order` to arrange them together. A session where
        // the socket could not be bound gets an empty registry and
        // every built-in, which is the desktop as it was.
        let dockapps = DockHost::new(&dockapp::current_display());
        let registered = if dockapps.is_listening() { dockapp::registry::scan() } else { Vec::new() };
        let now = std::time::Instant::now();
        // Tokens the *previous* shell left behind, if this start is a
        // hot restart. Read and deleted here, before the column is
        // built, so a tile whose dockapp is still running out there
        // holds its slot open for the survivor instead of launching a
        // second copy of it. See `dockapp::handoff` — this is the half
        // that turns the SDK's reconnect-on-EOF loop (written in Phase
        // 4a, unhonoured until now) into restart survival.
        let mut inherited = dockapps.handoff_path().map(|path| dockapp::handoff::take(&path)).unwrap_or_default();

        let mut samplers = SamplerRegistry::new();
        let items: Vec<SupervisedWidget> = builtin_items()
            .into_iter()
            .chain(registered.into_iter().map(|entry| {
                let mut remote = RemoteTile::new(entry, tile, now);
                if let Some(token) = inherited.remove(remote.id()) {
                    remote.rejoin(token, now);
                }
                DockItem::Remote(Box::new(remote))
            }))
            // Supervision is applied here, at the one place items enter
            // the dock, rather than being something each item opts
            // into: the item that needs it most is by definition the
            // one that would not have thought to.
            .map(SupervisedWidget::new)
            // And the same argument for sampling: an item's sources are
            // registered and bound at the one place it enters the dock,
            // so nothing can be constructed into the stack with its
            // sampling half half-wired.
            .map(|mut item| {
                item.bind(&mut samplers);
                item
            })
            .collect();
        // The user's arrangement, applied before anything is measured
        // or drawn: `stacked_dock_height` sums heights in column order,
        // so reordering after it would size the dock for a column that
        // no longer exists. An absent or empty file leaves the default
        // order above exactly as written.
        let remembered_order = dock_order::state_path().map(|path| dock_order::load(&path)).unwrap_or_default();
        let items = dock_order::arrange(items, &remembered_order, |item| item.id().to_string());
        let dock_height = stacked_dock_height(tile, primary.size.h, &items);
        let dock_geom = dock_geometry(primary, dock_width, dock_height);
        let dock_window = backend
            .create_shell_surface(dock_geom, wallpaper.dock_color(), true)
            .expect("failed to create dock window");
        backend.map_shell_surface(dock_window);
        backend.raise_shell_surface(dock_window);

        let clip_geom = clip_geometry(primary, tile);
        let clip_window = backend
            .create_shell_surface(clip_geom, wallpaper.dock_color(), true)
            .expect("failed to create clip window");
        backend.map_shell_surface(clip_window);
        backend.raise_shell_surface(clip_window);

        let logo = Pixmap::decode_png(include_bytes!("../assets/branding/chonkstep-logo-icon.png"))
            .expect("embedded ChonkStep logo should decode");
        let desktop = Self {
            dock_window,
            screen,
            primary,
            dock_width,
            tile,
            pad,
            drag_threshold: drag_threshold_px(scale),
            font_system: cosmic_text::FontSystem::new(),
            swash_cache: cosmic_text::SwashCache::new(),
            menu: ShellMenu::new(),
            items,
            dockapps,
            scale,
            remembered_order,
            samplers,
            workspace,
            clip_window,
            clip_drawn: (usize::MAX, 0),
            item_drag: None,
            hovered_item: None,
            icons: HashMap::new(),
            icon_drag: None,
            wallpaper,
            switcher: None,
            overview: OverviewPanel::default(),
            theme_id,
            apps,
            logo,
        };
        desktop.repaint_wallpaper(backend);
        desktop
    }

    pub fn dock_window(&self) -> B::ShellId {
        self.dock_window
    }

    /// One workarea per monitor, in the order `monitors` arrives in —
    /// exactly the positional order `WindowManager::set_workareas`
    /// indexes. The Dock is an always-on-top, content-sized object
    /// rather than a reserved sidebar (the classic dock behaves the
    /// same way), so it carves nothing out of the monitor it hangs
    /// on and every entry is that monitor's full geometry. This stays a
    /// per-monitor computation regardless, so that the day the Dock does
    /// reserve a strip, only the primary's entry has to change.
    pub fn workareas(&self, monitors: &[Rect]) -> Vec<Rect> {
        workareas_for(monitors, self.primary, self.primary_workarea())
    }

    /// The rectangle managed windows may occupy on the primary monitor:
    /// its whole rect minus the Dock's column on the right edge.
    ///
    /// The README promised this reservation from the start and the code
    /// returned the full monitor anyway, so a maximized window slid
    /// under the instruments — recorded as a known wrong in the
    /// compatibility notes until now. The full height is reserved, not
    /// just the instruments' current extent: the dock grows downward as
    /// tiles are added and miniaturized windows return to it, and a
    /// workarea that tracked its momentary height would reflow every
    /// maximized window each time a tile came or went.
    ///
    /// The Clip and the launcher strip on the left deliberately reserve
    /// nothing: they are corner furniture in the classic desktop, and
    /// windows sliding under the Clip is how the original behaved too.
    pub fn primary_workarea(&self) -> Rect {
        Rect {
            pos: self.primary.pos,
            size: Size::new(self.primary.size.w.saturating_sub(self.dock_width).max(1), self.primary.size.h),
        }
    }

    /// The whole desktop's extent — the union of every monitor, which
    /// is what the wallpaper covers and what menus are kept inside.
    /// Menus deliberately use this rather than `primary`: a menu opens
    /// wherever the pointer is, which on a multi-head session is
    /// routinely not the primary at all.
    fn screen_size(&self) -> Size {
        self.screen
    }

    /// Rehangs the dock on a new screen/monitor arrangement (the
    /// backend reported one via `take_screen_resize` — e.g. the user
    /// dragged the edge of the nested Xephyr window an X11 session runs
    /// in, or an output was plugged in) and repaints it at the current
    /// stack's compact content height. Icon tiles already on screen are
    /// left where they are: a tile the user placed is theirs, and one
    /// left in its auto slot is re-slotted the next time the grid is
    /// consulted anyway.
    pub fn resize_to_screen(&mut self, backend: &mut B, theme: &Theme, screen: Size, primary: Rect) {
        self.screen = screen;
        self.primary = primary;
        self.repaint_wallpaper(backend);
        self.redraw_dock(backend, theme);
        self.reposition_clip(backend, theme);
        // A full-screen surface sized for the old arrangement can only
        // be wrong on the new one; the next entry rebuilds it against
        // the primary rect just stored.
        self.overview.discard(backend);
    }

    /// Re-derives every metric this desktop measured from the UI scale
    /// at construction, and reports whether any of them moved.
    ///
    /// Metrics only: nothing is repainted here. The caller pairs this
    /// with [`Desktop::relayout`] because a theme change needs the
    /// repaint without the metric update, and doing both in one method
    /// would mean a theme pick recomputing a scale that did not change.
    ///
    /// Compared through `to_bits` rather than `==`: a derived `PartialEq`
    /// over an `f32` is not reflexive, so a NaN scale would compare
    /// unequal to itself and report a change on every single call. The
    /// scale resolver upstream refuses a non-finite value, and the
    /// dockapp broadcast guards the same way for the same reason — this
    /// is the third place that lesson applies, so it is applied here
    /// too rather than trusted to the caller.
    pub fn set_scale(&mut self, scale: f32) -> bool {
        if self.scale.to_bits() == scale.to_bits() {
            return false;
        }
        self.scale = scale;
        self.tile = tile_px(scale);
        self.pad = icon_pad_px(scale);
        // The dock is exactly one tile wide — the same identity
        // `Desktop::new` establishes, restated here rather than left to
        // drift.
        self.dock_width = self.tile;
        self.drag_threshold = drag_threshold_px(scale);
        true
    }

    /// The active theme's id, for the Themes submenu's bullet. Set by
    /// the shell when a theme is applied live; the `Theme` itself lives
    /// with the shell orchestration, not here.
    pub fn set_theme_id(&mut self, id: String) {
        self.theme_id = id;
    }

    /// Repaints every surface this desktop owns from the current theme
    /// and the current metrics — the theme/scale twin of
    /// [`Desktop::resize_to_screen`], which does the same for a changed
    /// monitor arrangement.
    ///
    /// It does strictly more than `resize_to_screen`: that one leaves
    /// icon tiles alone on purpose (a tile the user placed is theirs,
    /// and a rearrangement does not change how big it is), but a scale
    /// change *does* change how big it is, and a theme change changes
    /// what is drawn in it. So icon tiles are re-rendered here and not
    /// there.
    ///
    /// `previews` supplies the window thumbnail for each miniaturized
    /// client, keyed by `ClientId` — see [`Desktop::icon_clients`] for
    /// why the caller collects them rather than this method fetching
    /// them.
    pub fn relayout(&mut self, backend: &mut B, theme: &Theme, previews: &[(ClientId, Option<DecorationBuffer>)]) {
        self.repaint_wallpaper(backend);
        self.redraw_dock(backend, theme);
        self.reposition_clip(backend, theme);
        self.relayout_icons(backend, theme, previews);
        self.discard_switcher(backend);
        // The Overview's surface and its stored layout are both sized
        // and styled for the metrics that just changed; discarding lets
        // the next entry rebuild them (and, if a session was somehow
        // live — a reload marker can fire mid-session — releases its
        // keyboard grab; see `OverviewPanel::discard`).
        self.overview.discard(backend);
    }

    /// The clients that currently have an icon tile on the desktop.
    ///
    /// Exists so the shell can gather each one's thumbnail from the
    /// window manager *before* handing this type the backend: the
    /// preview comes from `WindowManager::client_preview` (an immutable
    /// borrow of the WM) while painting needs `WindowManager::
    /// backend_mut` (a mutable one), and the two cannot overlap. The
    /// caller collects, then paints.
    pub fn icon_clients(&self) -> Vec<ClientId> {
        self.icons.values().map(|icon| icon.client).collect()
    }

    /// Resizes, re-slots and re-renders every icon tile against the
    /// current theme and tile edge.
    ///
    /// A tile still sitting in its auto-arranged slot is re-slotted, so
    /// the grid stays a grid at the new tile size. A tile the user
    /// dragged somewhere keeps its position — the same rule
    /// `icon_slot_position`'s `auto_slot: None` case encodes everywhere
    /// else, and the same one `resize_to_screen` honors: a placed icon
    /// is the user's, and a restyle is not a reason to move it.
    ///
    /// A client with no entry in `previews` (or a `None` one) is drawn
    /// without a thumbnail rather than skipped: an unmapped window
    /// cannot always be re-captured, and a tile wearing the new theme
    /// with no preview is a better answer than one still wearing the
    /// old theme.
    fn relayout_icons(&mut self, backend: &mut B, theme: &Theme, previews: &[(ClientId, Option<DecorationBuffer>)]) {
        let tile = self.tile;
        let entries: Vec<(B::ShellId, ClientId, Option<usize>, String)> = self
            .icons
            .values()
            .map(|icon| (icon.window, icon.client, icon.auto_slot, icon.title.clone()))
            .collect();
        for (window, client, auto_slot, title) in entries {
            let pos = match auto_slot {
                Some(slot) => self.icon_slot_position(slot),
                None => self.icons.get(&window).map(|icon| icon.pos).unwrap_or(Point::new(0, 0)),
            };
            backend.configure_shell_surface(window, Rect { pos, size: Size::new(tile, tile) });
            let preview = previews
                .iter()
                .find(|(id, _)| *id == client)
                .and_then(|(_, preview)| preview.as_ref());
            let buffer = icon::render_icon_tile(theme, &mut self.font_system, &mut self.swash_cache, tile, &title, preview);
            backend.paint_shell_surface(window, &buffer);
            if let Some(icon) = self.icons.get_mut(&window) {
                icon.pos = pos;
            }
        }
    }

    /// Drops the Alt-Tab panel's surface so the next cycle rebuilds it.
    ///
    /// Cheaper and more certain than re-rendering it: the panel is
    /// recreated whenever its rendered size changes anyway
    /// (`show_switcher`), it is only ever visible while a modal cycle is
    /// held, and a restyle cannot land in the middle of one — the key
    /// grab that drives the cycle owns the keyboard for its duration.
    fn discard_switcher(&mut self, backend: &mut B) {
        if let Some(panel) = self.switcher.take() {
            if let Some(window) = panel.window {
                backend.destroy_shell_surface(window);
            }
        }
    }

    /// Moves the Clip back to the primary monitor's corner after the
    /// arrangement changed. Separate from `redraw_dock` only because
    /// the two surfaces are separate; both are pure "put the chrome
    /// back where it belongs" work.
    fn reposition_clip(&mut self, backend: &mut B, theme: &Theme) {
        backend.configure_shell_surface(self.clip_window, clip_geometry(self.primary, self.tile));
        self.repaint_clip(backend, theme);
    }

    /// Collects whatever the sampler threads have finished, folds it
    /// into every dock widget, and repaints the dock if anything
    /// actually changed.
    ///
    /// One `refresh` for the whole stack, before any widget sees
    /// anything: that is what makes `Samples::fresh` mean "new since
    /// your last `update`" for every widget alike, and what stops one
    /// widget's pass from observing a different instant than its
    /// neighbour's.
    ///
    /// Every widget is updated (never short-circuited) so one further
    /// down the list still gets to fold even if an earlier one had
    /// nothing new. All of it is timed and budgeted by
    /// [`SupervisedWidget`]: this loop runs on the compositor's single
    /// repaint thread, so a widget that blocks here freezes the whole
    /// desktop, and one that does it repeatedly is dropped from the
    /// dock rather than allowed to keep doing it. What it can no longer
    /// block on is the system — that moved to the sampler threads
    /// `samplers` owns.
    pub fn tick_items(&mut self, backend: &mut B, theme: &Theme) {
        // Out-of-process tiles first, so a frame that arrived this pass
        // is folded by the same `update` sweep that folds a sampler
        // reading, and reaches the screen on the same repaint. Doing it
        // after would show every dockapp frame one pass (16ms) late for
        // no reason.
        self.service_dockapps(theme);
        self.samplers.refresh();
        let mut changed = false;
        {
            // Scoped so the borrow of `samplers` ends before the
            // repaint, which needs all of `self`.
            let samples = self.samplers.samples();
            for widget in &mut self.items {
                if widget.update(&samples) {
                    changed = true;
                }
            }
        }
        if changed {
            self.redraw_dock(backend, theme);
        }
    }

    /// Feeds the WM's authoritative workspace state to the dock's
    /// indicator tile. No repaint here on purpose: the next
    /// `tick_items` pass notices the change and repaints the dock
    /// through the one shared path, instead of this method growing a
    /// second redraw entry point.
    pub fn set_workspace_display(&mut self, backend: &mut B, theme: &Theme, current: usize, count: usize) {
        {
            let mut shared = self.workspace.borrow_mut();
            shared.current = current;
            shared.count = count;
        }
        if self.clip_drawn != (current, count) {
            self.clip_drawn = (current, count);
            self.repaint_clip(backend, theme);
        }
    }

    fn repaint_clip(&mut self, backend: &mut B, theme: &Theme) {
        let (current, count) = self.clip_drawn;
        let current = if current == usize::MAX { 0 } else { current };
        let buffer = workspace::render_clip_tile(theme, &mut self.font_system, &mut self.swash_cache, self.tile, current, count.max(1));
        backend.paint_shell_surface(self.clip_window, &buffer);
    }

    pub fn clip_window(&self) -> B::ShellId {
        self.clip_window
    }

    /// A click on the Clip: the classic diagonal corner zones — the
    /// top-right arrow advances, the bottom-left one goes back, the
    /// body is inert. Same semantics as Alt+Ctrl+Left/Right rather
    /// than wrapping: forward past the last workspace grows a new one
    /// on demand (`switch_workspace`'s own behavior), rewind saturates
    /// at the first — wrapping made the arrows dead on a fresh
    /// session's single workspace. The switch is routed through the
    /// shared request cell like every other workspace change, so the
    /// tile repaints when the WM confirms.
    pub fn click_clip(&mut self, local: Point) {
        let mut shared = self.workspace.borrow_mut();
        match workspace::clip_hit(self.tile, local.x, local.y) {
            workspace::ClipZone::Forward => shared.requested = Some(shared.current + 1),
            workspace::ClipZone::Rewind => shared.requested = shared.current.checked_sub(1),
            workspace::ClipZone::Body => {}
        }
    }

    /// Drains the workspace switch a click on the indicator tile
    /// requested, if any — the event loop performs the actual switch and
    /// then reports back via `set_workspace_display`. Take-semantics so
    /// one click means one switch, not one per loop iteration.
    pub fn take_workspace_request(&mut self) -> Option<usize> {
        self.workspace.borrow_mut().requested.take()
    }

    /// Dock-local Y where the widget stack begins — directly below the
    /// identity tile, touching it, same as every other tile touches its
    /// neighbors.
    fn items_top(&self) -> i32 {
        self.tile as i32
    }

    /// Dock-local `(index, rect)` for every widget slot, in order —
    /// widgets don't all occupy the same height (a widget is free to
    /// report a taller `tile_height()` for a multi-tile face), so this
    /// walks the stack
    /// accumulating each one's actual `tile_height()` rather than
    /// assuming a fixed stride. Both hit-testing and painting read from
    /// this single source of truth, so they can never disagree about
    /// where a widget sits. No gap between consecutive slots — tiles
    /// snap together, the way the classic dock stacks them.
    fn item_slots(&self) -> Vec<(usize, Rect)> {
        let mut y = self.items_top();
        let mut slots = Vec::with_capacity(self.items.len());
        for (index, widget) in self.items.iter().enumerate() {
            let h = self.tile * widget.tile_height();
            slots.push((index, Rect { pos: Point::new(0, y), size: Size::new(self.tile, h) }));
            y += h as i32;
        }
        slots
    }

    /// Which widget slot (if any) `local` — in dock-local coordinates —
    /// falls within. Misses both the identity tile above and the
    /// inter-widget gaps between slots.
    fn item_index_at(&self, local: Point) -> Option<usize> {
        self.item_slots().into_iter().find(|(_, rect)| rect.contains(local)).map(|(index, _)| index)
    }

    /// Starts a middle-click drag-to-reorder on whichever widget sits at
    /// `local`, if any. Returns `false` (and does nothing) if `local`
    /// isn't over a widget slot, so callers know whether to treat the
    /// press as consumed.
    pub fn begin_item_drag(&mut self, backend: &mut B, theme: &Theme, local: Point) -> bool {
        let Some(index) = self.item_index_at(local) else { return false };
        // Same reasoning as `begin_icon_drag`: without a grab, a fast
        // drag could outrun the dock's own (narrow) window bounds and
        // stop reporting motion against it.
        let grab = backend.grab_pointer_for_drag();
        self.item_drag = Some(ItemDrag { index, grab });
        self.redraw_dock(backend, theme);
        true
    }

    /// Feeds root-relative pointer motion to an in-progress widget drag
    /// — call this on every `PointerMotion`, not just dock-targeted
    /// motion, for the same reason `drag_icon_motion` does. The dock
    /// starts at screen `y = 0`, so root-Y and dock-local-Y are already
    /// the same value; no translation needed.
    pub fn drag_item_motion(&mut self, backend: &mut B, theme: &Theme, root: Point) {
        let Some(dragged) = self.item_drag.as_ref().map(|d| d.index) else { return };
        let Some(target) = self.item_index_at(Point::new(0, root.y)) else { return };
        if target == dragged {
            return;
        }
        self.items.swap(dragged, target);
        if let Some(drag) = &mut self.item_drag {
            drag.index = target;
        }
        self.redraw_dock(backend, theme);
    }

    /// Ends whatever widget drag is in progress, if any. Returns `false`
    /// if no drag was active, so callers can tell whether the release
    /// was actually theirs to handle.
    pub fn end_item_drag(&mut self, backend: &mut B, theme: &Theme) -> bool {
        let Some(drag) = self.item_drag.take() else { return false };
        backend.ungrab_pointer(drag.grab);
        self.persist_order();
        self.redraw_dock(backend, theme);
        true
    }

    /// Writes the column's current order to
    /// `$XDG_STATE_HOME/chonkstep/dock-items`.
    ///
    /// Called when a drag *ends*, not on every slot the pointer crosses
    /// mid-drag: `drag_item_motion` reorders continuously as the
    /// pointer sweeps, so persisting there would write the file once
    /// per crossed tile for an arrangement the user has not committed
    /// to yet. The release is the commit. (The cost of that choice is
    /// that a session killed mid-drag forgets the drag, which is the
    /// correct thing to forget.)
    ///
    /// A write failure is a warning and nothing else. The dock is
    /// arranged the way the user just arranged it either way; what has
    /// been lost is only that the *next* session will not know, and
    /// refusing to run over a read-only state directory would trade a
    /// small forgetting for a broken desktop.
    fn persist_order(&mut self) {
        let Some(path) = dock_order::state_path() else {
            return;
        };
        let live: Vec<String> = self.items.iter().map(|item| item.id().to_string()).collect();
        let merged = dock_order::merge(&live, &self.remembered_order);
        if let Err(error) = dock_order::save(&path, &merged) {
            tracing::warn!(?error, path = %path.display(), "failed to remember the dock's order");
            return;
        }
        self.remembered_order = merged;
    }

    /// Pointer input for whichever widget sits under it, if any (e.g.
    /// the network instrument cycles interfaces, the sound instrument's
    /// zones adjust volume). `input` arrives in dock-local coordinates
    /// and is re-anchored to the widget's own tile before delivery, so
    /// widgets can carve their face into control zones without knowing
    /// where the dock stacked them. Returns `false` if the input isn't
    /// over a widget slot at all, so callers can tell whether it was
    /// theirs to handle.
    ///
    /// Whatever the widget wants done comes back as [`Effect`]s and is
    /// performed here, off this thread where it needs to be — see
    /// `apply_effects`.
    pub fn dock_input(&mut self, backend: &mut B, theme: &Theme, input: DockInput) -> bool {
        let Some(local) = input.local() else { return false };
        let Some((index, rect)) = self.item_slots().into_iter().find(|(_, rect)| rect.contains(local)) else {
            return false;
        };
        let effects = self.items[index].on_input(input.translated(rect.pos), self.tile);
        self.apply_effects(backend, theme, effects);
        true
    }

    // -----------------------------------------------------------------
    // Out-of-process tiles
    // -----------------------------------------------------------------

    /// Every file descriptor the binary's event loop must add to its
    /// wait, beyond whatever the display connection already gives it:
    /// the dockapp listener, every connection that has not identified
    /// itself yet, and one per connected dockapp.
    ///
    /// **This is the whole of the backend-specific cost of dockapps.**
    /// The X11 binary adds these to the `pollfd` array it already
    /// builds around the X socket; the Wayland binary wraps each in a
    /// calloop `Generic` source. Nothing else about a dockapp differs
    /// between the two stacks, because a dockapp is not a
    /// display-server client — it is a process on the end of a Unix
    /// socket, and both loops already know how to wait on one of those.
    ///
    /// Recomputed per wait rather than cached: the set changes whenever
    /// a dockapp connects, dies or is restarted, and a stale fd in a
    /// `poll` set is either a spurious wakeup (harmless) or a wait on a
    /// descriptor this process has reused for something else (not).
    /// Both loops call this immediately before they wait, which is the
    /// only moment the answer is knowable.
    ///
    /// Getting this *wrong* is bounded, which is worth knowing before
    /// anyone spends a day on it: both loops already wake on a 16ms
    /// housekeeping bound, so an fd omitted here costs a dockapp frame
    /// up to 16ms of latency and nothing else. It is a latency
    /// optimisation with a correctness-shaped API, not a correctness
    /// requirement.
    pub fn extra_poll_fds(&self) -> Vec<std::os::fd::RawFd> {
        let mut fds = self.dockapps.poll_fds();
        fds.extend(self.items.iter().filter_map(|item| item.remote().and_then(RemoteTile::poll_fd)));
        fds
    }

    /// One servicing pass over every out-of-process tile: admit new
    /// connections, read whatever arrived, ping, flush, relaunch.
    ///
    /// Nothing here can block. Every `recv` and `send` is
    /// `MSG_DONTWAIT`, every send goes through a bounded queue that
    /// drops rather than waits, and a dockapp that has stopped reading
    /// or writing simply stops appearing. A hung dockapp costs this
    /// function one `encode` and one non-blocking `send` every two
    /// seconds and costs the compositor nothing else at all — see
    /// `crate::dockapp::tile` for why the liveness check therefore
    /// exists to inform the user rather than to protect the desktop.
    fn service_dockapps(&mut self, theme: &Theme) {
        let now = std::time::Instant::now();
        // Once, before anything else in the pass, so every tile serviced
        // below — and every `Welcome` sent above — describes the same
        // dock. Recomputing the serialized theme only happens when it
        // actually changed; see `ThemeBroadcast`.
        self.dockapps.refresh_theme(self.tile, self.scale, theme);

        let socket_path = self.dockapps.socket_path().clone();
        // Taken out first so the mutable borrow of `dockapps` ends
        // before the shared one below begins; handed back at the end.
        let mut scratch = std::mem::take(self.dockapps.scratch());
        let admissions = self.dockapps.service(now);

        // Borrowed, never cloned. `theme_toml` is a few kilobytes and
        // this runs at ~60Hz on the repaint thread, so a clone per pass
        // would be a quarter of a megabyte a second of copying to
        // produce a value that is almost always identical. Disjoint
        // field borrows (`dockapps` shared, `items` mutable) are what
        // make that possible, and are why `admit` is a free function
        // over an iterator rather than a method on `&mut self`.
        let theme_state = self.dockapps.theme();
        for admission in admissions {
            dockapp::admit(self.items.iter_mut().filter_map(|item| item.remote_mut()), admission, theme_state, now);
        }
        let mut ctx = ServiceContext { now, theme: theme_state, socket_path: &socket_path, scratch: &mut scratch };
        for item in &mut self.items {
            // A tile the supervisor evicted is one the dock has
            // disowned, and continuing to run its process would leave a
            // dockapp drawing frames nobody will ever blit. Shut it
            // down once; `shut_down` is idempotent.
            if item.evicted() {
                if let Some(tile) = item.remote_mut() {
                    if !matches!(tile.state(), TileState::Stopped { reason: StopReason::Removed }) {
                        tracing::warn!(id = %tile.id(), "shutting down an evicted dockapp");
                        tile.shut_down(chonk_dock_proto::wire::GoodbyeReason::Removed);
                    }
                }
                continue;
            }
            if let Some(tile) = item.remote_mut() {
                tile.service(&mut ctx);
            }
        }
        // Hand the buffer back so the next pass reuses the same
        // allocation rather than asking the allocator for a quarter of
        // a megabyte on the repaint thread.
        *self.dockapps.scratch() = scratch;
    }

    /// Opens a dock tile's own right-click menu, if the tile at `local`
    /// has one.
    ///
    /// Returns whether a menu opened, so the caller can tell a
    /// right-click that landed on a remote tile from one that landed on
    /// a built-in (which has no menu) or on the identity tile.
    pub fn open_dock_item_menu(&mut self, backend: &mut B, theme: &Theme, local: Point, root: Point) -> bool
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let Some(index) = self.item_index_at(local) else { return false };
        let now = std::time::Instant::now();
        let Some(tile) = self.items[index].remote() else { return false };
        let (id, title, items) = (tile.id().to_string(), dock_item_menu_title(tile.name()), dock_item_menu_items(tile, now));
        let bounds = self.screen_size();
        self.menu.open_dock_item(backend, theme, &mut self.font_system, id, title, items, root, bounds);
        true
    }

    /// Performs a pick from a dock tile's menu. A stale id — the tile
    /// was removed while its menu sat open — is silently nothing,
    /// matching every other stale-target path in the shell.
    pub fn dock_item_menu_action(&mut self, backend: &mut B, theme: &Theme, id: &str, action: DockItemMenuAction) {
        let now = std::time::Instant::now();
        match action {
            DockItemMenuAction::Restart => {
                if let Some(tile) = self.items.iter_mut().filter_map(|item| item.remote_mut()).find(|tile| tile.id() == id) {
                    tile.user_restart(now);
                }
            }
            DockItemMenuAction::Remove => {
                let Some(index) = self.items.iter().position(|item| item.id() == id) else { return };
                if let Some(tile) = self.items[index].remote_mut() {
                    tracing::info!(%id, source = %tile.entry().source.display(), "removing a dockapp tile for this session; delete or edit that file to remove it permanently");
                    tile.shut_down(chonk_dock_proto::wire::GoodbyeReason::Removed);
                }
                self.items.remove(index);
                // The column changed, so the remembered order should
                // say so — and `merge` keeps the removed id as an
                // unresolved entry, which is exactly right: bring the
                // dockapp back next session and it returns to the slot
                // the user had put it in.
                self.persist_order();
            }
            // The About rows carry no command; they resolve to `None`
            // and never reach here.
            DockItemMenuAction::About => {}
        }
        self.redraw_dock(backend, theme);
    }

    /// Tracks which dock item the pointer is inside and delivers
    /// `Enter`/`Leave` as it crosses between them.
    ///
    /// Driven from root coordinates rather than from the surface-local
    /// motion the backend queues, and that is deliberate. The
    /// surface-local stream only reports motion *over* the dock, so
    /// there is no event at the moment the pointer leaves it — a tile
    /// would latch into a permanent hover state the first time the
    /// pointer wandered off, which is precisely the bug the protocol's
    /// `CROSSING` mask exists to make impossible ("a tile that wants
    /// one always wants the other"). Root motion arrives for every
    /// pointer move on the desktop, so the leaving edge is always seen.
    pub fn update_dock_hover(&mut self, backend: &mut B, theme: &Theme, root: Point) {
        let dock_height = stacked_dock_height(self.tile, self.primary.size.h, &self.items);
        let dock = dock_geometry(self.primary, self.dock_width, dock_height);
        let inside = dock.contains(root).then(|| Point::new(root.x - dock.pos.x, root.y - dock.pos.y));
        let target = inside.and_then(|local| self.item_index_at(local)).map(|index| self.items[index].id().to_string());
        if target == self.hovered_item {
            return;
        }
        let left = self.hovered_item.take();
        if let Some(id) = left {
            let effects = self.deliver_by_id(&id, DockInput::Leave);
            self.apply_effects(backend, theme, effects);
        }
        if let Some(id) = target {
            let effects = self.deliver_by_id(&id, DockInput::Enter);
            self.apply_effects(backend, theme, effects);
            self.hovered_item = Some(id);
        }
    }

    /// Hands one input to the item with this id, if it is still in the
    /// column. Used by the crossing events, which name a tile rather
    /// than a position for the reason on `hovered_item`.
    fn deliver_by_id(&mut self, id: &str, input: DockInput) -> Vec<Effect> {
        let Some(index) = self.items.iter().position(|item| item.id() == id) else { return Vec::new() };
        self.items[index].on_input(input, self.tile)
    }

    /// Lets go of every out-of-process tile, for a session that is
    /// ending or re-execing.
    ///
    /// The decision is [`dockapp::shut_down`]'s; this supplies the
    /// tiles and the path. See there for what the two farewells mean and
    /// why the restarting one is the whole of Phase 4c's payoff.
    pub fn shut_down_dockapps(&mut self, farewell: Farewell) {
        let handoff = self.dockapps.handoff_path();
        dockapp::shut_down(self.items.iter_mut().filter_map(|item| item.remote_mut()), handoff.as_deref(), farewell);
    }

    /// Performs what a widget asked for.
    ///
    /// [`Effect::Run`] is the one that matters: it goes to a thread of
    /// its own, because `wpctl set-volume` and `nmcli radio wifi off`
    /// arrive on the compositor's repaint thread and can park it every
    /// bit as thoroughly as a sample could. That a widget can only
    /// *return* one of these — never run it — is the click path's half
    /// of the same guarantee `SamplerRegistry` gives the sampling path.
    ///
    /// Repaints are coalesced: a widget that emits several effects gets
    /// one redraw, not one per effect.
    fn apply_effects(&mut self, backend: &mut B, theme: &Theme, effects: Vec<Effect>) {
        let mut repaint = false;
        for effect in effects {
            match effect {
                Effect::Repaint => repaint = true,
                Effect::Resample(id) => {
                    if let Some(resampler) = self.samplers.resampler(id) {
                        resampler.resample_soon();
                    }
                }
                Effect::Run { program, args, then } => {
                    run_detached(program, args, then.and_then(|id| self.samplers.resampler(id)));
                }
            }
        }
        if repaint {
            self.redraw_dock(backend, theme);
        }
    }

    fn redraw_dock(&mut self, backend: &mut B, theme: &Theme) {
        let dock_height = stacked_dock_height(self.tile, self.primary.size.h, &self.items);
        let dock_geom = dock_geometry(self.primary, self.dock_width, dock_height);
        backend.configure_shell_surface(self.dock_window, dock_geom);

        let Some(mut pixmap) = Pixmap::new(self.dock_width, dock_height.max(1)) else {
            return;
        };
        // The tile stack covers the whole column — the identity tile
        // plus every widget slot sums to at least `dock_height`, since
        // `stacked_dock_height`'s clamp only ever shortens it — so no
        // flat filler should ever be visible between tiles. The base
        // coat is still painted with the tile *face* rather than a
        // solid color, so that even a defensive gap (say, a widget
        // rendering short) reads as tile family, not as a hole in it.
        // `wallpaper.dock_color` stays only as the X11 window background
        // set at creation: the behind-everything fallback for the
        // instant before the first blit lands.
        paint::fill_area(&mut pixmap, 0, 0, self.dock_width, dock_height, &theme.tile.fill);

        // Identity tile: flush at the dock's top-left corner, on the
        // same tile face/relief as every other square surface (it used
        // to borrow the titlebar's fill and bevel, which made the top of
        // the dock read as window chrome rather than as the column's
        // first tile), with the ChonkStep mark composited centered on
        // top — the mark is deliberately bold enough to survive the
        // Dock's original 56-pixel scale.
        tile::draw_tile_base(&mut pixmap, 0, 0, self.tile, theme);
        let logo_inset = (self.tile / 9).max(2);
        let logo_size = self.tile.saturating_sub(logo_inset * 2);
        let logo_scale = logo_size as f32 / self.logo.width() as f32;
        let logo_paint = PixmapPaint {
            quality: FilterQuality::Bicubic,
            ..PixmapPaint::default()
        };
        pixmap.draw_pixmap(
            0,
            0,
            self.logo.as_ref(),
            &logo_paint,
            Transform::from_row(logo_scale, 0.0, 0.0, logo_scale, logo_inset as f32, logo_inset as f32),
            None,
        );

        for (index, rect) in self.item_slots() {
            // `None` is an evicted widget: the dock draws its own
            // tombstone rather than calling code it has already
            // disowned. `render_dead_tile` is the same powered-off
            // face the instruments already use for "no sink", "no
            // interface", "no battery" — an evicted slot should read as
            // a dead instrument, which belongs to the family, and not
            // as a hole punched in the column. The widget's own name is
            // the label, so the dock says *which* one went dark without
            // the user having to find the log.
            let buffer = match self.items[index].render(theme, self.tile, &mut self.font_system, &mut self.swash_cache) {
                Some(buffer) => buffer,
                None => {
                    let label = self.items[index].name();
                    panel::render_dead_tile(theme, &mut self.font_system, &mut self.swash_cache, self.tile, label)
                }
            };
            blit_into(&mut pixmap, rect.pos.x as u32, rect.pos.y as u32, &buffer);

            if self.item_drag.as_ref().is_some_and(|d| d.index == index) {
                // "You've picked this up": brighten the slot's outermost
                // pixel ring with the relief's own +80 light delta
                // (`tile::op_line`, relative) rather than stamping an
                // absolute chrome color over it — the tile's RAISED2
                // edge stays structurally intact and simply reads as
                // lit, so the pickup highlight speaks the tile family's
                // language instead of borrowing the titlebar bevel's.
                // Drawn on the edge itself rather than in a surrounding
                // gap: tiles snap together, so there's no gap to use.
                let (x, y) = (rect.pos.x, rect.pos.y);
                let (w, h) = (rect.size.w as i32, rect.size.h as i32);
                tile::op_line(&mut pixmap, x, y, x + w - 1, y, 80);
                tile::op_line(&mut pixmap, x, y + h - 1, x + w - 1, y + h - 1, 80);
                tile::op_line(&mut pixmap, x, y, x, y + h - 1, 80);
                tile::op_line(&mut pixmap, x + w - 1, y, x + w - 1, y + h - 1, 80);
            }
        }

        backend.paint_shell_surface(self.dock_window, &pixmap_to_buffer(&pixmap));
    }

    pub fn open_root_menu(&mut self, backend: &mut B, theme: &Theme, at: Point)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let bounds = self.screen_size();
        let items = root_menu_items(self.wallpaper, &self.theme_id, &self.apps);
        self.menu.open_root(backend, theme, &mut self.font_system, items, self.apps.len(), at, bounds);
    }

    /// The stored application index `RootMenuAction::LaunchApp`'s
    /// payload indexes into — the dispatch that launches a picked
    /// entry reads it back through here, so the flat index names the
    /// same app on both sides of the menu round-trip.
    pub fn apps(&self) -> &[crate::apps::AppEntry] {
        &self.apps
    }

    /// Opens the per-window commands menu at `at` (root coordinates —
    /// where the titlebar right-click landed, as reported by
    /// `Notification::WindowMenuRequested`), titled with the window's
    /// own (truncated) title. Replaces whatever menu session is
    /// already open, root or window — exactly one menu on screen at a
    /// time, the classic one-open-menu-per-screen rule.
    pub fn open_window_menu(&mut self, backend: &mut B, theme: &Theme, at: Point, ctx: WindowMenuContext)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let bounds = self.screen_size();
        self.menu.open_window(backend, theme, &mut self.font_system, &ctx, at, bounds);
    }

    /// Applies a built-in wallpaper immediately and repaints the dock to
    /// its matching quiet-edge color. Selection is intentionally a
    /// session preference for now; persistent settings are future work.
    pub fn set_wallpaper(&mut self, backend: &mut B, theme: &Theme, wallpaper: Wallpaper) {
        self.wallpaper = wallpaper;
        if let Err(error) = wallpaper.persist() {
            tracing::warn!(?error, wallpaper = wallpaper.label(), "failed to remember wallpaper selection");
        }
        self.repaint_wallpaper(backend);
        self.redraw_dock(backend, theme);
    }

    fn repaint_wallpaper(&self, backend: &mut B) {
        match self.wallpaper.render(self.screen_size()) {
            Some(buffer) => backend.paint_root_image(&buffer),
            None => backend.paint_root_color(DESKTOP_BG),
        }
    }

    /// Shows (or updates) the Alt-Tab switch panel. `entries: Some`
    /// replaces the candidate set (previews included) — passed on the
    /// first update of a session and again if the set changes mid-cycle
    /// — while `None` reuses the stored one, so stepping the selection
    /// never re-captures every window. The popup window is recreated
    /// only when the rendered size changes.
    pub fn show_switcher(&mut self, backend: &mut B, theme: &Theme, entries: Option<Vec<SwitcherEntry>>, selected: usize) {
        match (entries, self.switcher.as_mut()) {
            (Some(new_entries), Some(panel)) => panel.entries = new_entries,
            (Some(new_entries), None) => {
                self.switcher = Some(SwitcherPanel { window: None, size: Size::new(0, 0), entries: new_entries, visible: false });
            }
            (None, _) => {}
        }
        let Self { switcher, font_system, swash_cache, tile, primary, .. } = self;
        let Some(panel) = switcher.as_mut() else {
            return;
        };
        let buffer = switcher::render_switcher(theme, font_system, swash_cache, &panel.entries, selected, *tile);
        if buffer.width == 0 || buffer.height == 0 {
            return;
        }
        let size = Size::new(buffer.width, buffer.height);
        if panel.window.is_none() || panel.size != size {
            if let Some(window) = panel.window.take() {
                backend.destroy_shell_surface(window);
            }
            let geom = Rect { pos: centered_on(*primary, size), size };
            match backend.create_shell_surface(geom, switcher::panel_background(theme), true) {
                Some(window) => {
                    panel.window = Some(window);
                    panel.size = size;
                    panel.visible = false;
                }
                None => {
                    tracing::warn!("failed to create switcher window");
                    return;
                }
            }
        }
        if let Some(window) = panel.window {
            if !panel.visible {
                backend.map_shell_surface(window);
                panel.visible = true;
            }
            backend.raise_shell_surface(window);
            backend.paint_shell_surface(window, &buffer);
        }
    }

    /// How many candidates the *visible* panel is showing, `None` when
    /// no session is on screen — the caller rebuilds the entry set
    /// (fresh previews) at the start of every session, and again only
    /// if the candidate count changes mid-session.
    pub fn switcher_entry_count(&self) -> Option<usize> {
        self.switcher.as_ref().filter(|panel| panel.visible).map(|panel| panel.entries.len())
    }

    pub fn hide_switcher(&mut self, backend: &mut B) {
        if let Some(panel) = self.switcher.as_mut() {
            if let Some(window) = panel.window {
                backend.unmap_shell_surface(window);
            }
            panel.visible = false;
        }
    }

    // -- the Overview -----------------------------------------------------
    //
    // Thin delegation into `crate::overview::OverviewPanel`: the panel
    // owns its surface, entries, selection and layout, but rendering
    // needs this desktop's font state and metrics, so the shell drives
    // it through these wrappers — the same split the menus use.

    /// Opens (or re-populates, while open) the Overview over the
    /// primary monitor with a fresh entry set. The shell captures the
    /// entries — previews come from `WindowManager::client_preview`,
    /// which this type cannot call while the backend is borrowed.
    pub fn show_overview(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        items: Vec<OverviewItem<B>>,
        workspace: (usize, usize),
        selected: usize,
    ) {
        let Self { overview, font_system, swash_cache, tile, primary, .. } = self;
        overview.show(backend, theme, font_system, swash_cache, *primary, *tile, items, workspace, selected);
    }

    pub fn overview_visible(&self) -> bool {
        self.overview.visible()
    }

    pub fn overview_owns(&self, surface: B::ShellId) -> bool {
        self.overview.owns(surface)
    }

    pub fn overview_hit(&self, local: Point) -> OverviewHit {
        self.overview.hit(local)
    }

    pub fn overview_selected(&self) -> usize {
        self.overview.selected()
    }

    pub fn overview_item(&self, index: usize) -> Option<&OverviewItem<B>> {
        self.overview.item(index)
    }

    /// Moves the selection to `index` (hover, click-arm), repainting
    /// only when it actually changed.
    pub fn select_overview_card(&mut self, backend: &mut B, theme: &Theme, index: usize) {
        let Self { overview, font_system, swash_cache, .. } = self;
        overview.select(backend, theme, font_system, swash_cache, index);
    }

    /// Arrow-key selection movement, clamped by the panel's grid math.
    pub fn move_overview_selection(&mut self, backend: &mut B, theme: &Theme, dx: i32, dy: i32) {
        let Self { overview, font_system, swash_cache, .. } = self;
        overview.move_selection(backend, theme, font_system, swash_cache, dx, dy);
    }

    /// Closes the session, keeping the surface for the next entry.
    /// The keyboard grab is the shell's to release — it took it.
    pub fn hide_overview(&mut self, backend: &mut B) {
        self.overview.hide(backend);
    }

    pub fn close_menu(&mut self, backend: &mut B)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        self.menu.close(backend);
    }

    /// If `window` belongs to the open menu chain, resolves a click on
    /// it and, if it fired an action, returns the resolved
    /// `MenuAction` — `Root(..)` when the root menu is the open
    /// session, `Window(client, ..)` when the per-window commands menu
    /// is, decided by which session actually opened rather than by
    /// inspecting the id (see `resolve_session_action`). Misses and
    /// dismissals return `None` exactly as before — see
    /// `CascadeMenu::click` for the full click/dismiss/cascade
    /// contract.
    pub fn click_menu(&mut self, backend: &mut B, theme: &Theme, window: B::ShellId, local: Point) -> Option<MenuAction>
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        self.menu.click(backend, theme, &mut self.font_system, window, local)
    }

    pub fn hover_menu(&mut self, backend: &mut B, theme: &Theme, window: B::ShellId, local: Point)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        self.menu.hover(backend, theme, &mut self.font_system, window, local);
    }

    /// Opens whatever submenu has been hovered long enough — called once
    /// per event-loop iteration (like `tick_items`).
    pub fn tick_menu(&mut self, backend: &mut B, theme: &Theme)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        self.menu.tick(backend, theme, &mut self.font_system);
    }

    /// Shows an icon tile for a client that was just miniaturized —
    /// the classic "miniaturize to icon", not minimize-to-a-taskbar
    /// (there is no taskbar). Tiles fill left-to-right along the
    /// bottom-left of the screen, wrapping upward — matching the icon
    /// row layout in the reference NeXTSTEP screenshot this theme is
    /// matched against — clear of the dock on the right.
    pub fn show_icon(&mut self, backend: &mut B, theme: &Theme, client: ClientId, title: &str, preview: Option<&DecorationBuffer>) {
        let slot = (0..).find(|s| !self.icons.values().any(|icon| icon.auto_slot == Some(*s))).unwrap_or(0);
        let pos = self.icon_slot_position(slot);
        let geom = Rect { pos, size: Size::new(self.tile, self.tile) };

        let Some(window) = backend.create_shell_surface(geom, DESKTOP_BG, true) else {
            tracing::warn!(?client, "failed to create icon tile window");
            return;
        };
        backend.map_shell_surface(window);
        backend.raise_shell_surface(window);
        let buffer = icon::render_icon_tile(theme, &mut self.font_system, &mut self.swash_cache, self.tile, title, preview);
        backend.paint_shell_surface(window, &buffer);

        self.icons.insert(window, IconTile { window, client, title: title.to_string(), pos, auto_slot: Some(slot) });
    }

    fn icon_slot_position(&self, slot: usize) -> Point {
        icon_slot_position(self.primary, self.tile, self.pad, slot)
    }

    /// Removes the icon tile for `client`, if one is showing (a no-op
    /// otherwise — covers both a normal restore and closing a window
    /// while it's still miniaturized).
    pub fn remove_icon_for_client(&mut self, backend: &mut B, client: ClientId) {
        let Some(window) = self.icons.values().find(|icon| icon.client == client).map(|icon| icon.window) else {
            return;
        };
        self.icons.remove(&window);
        backend.destroy_shell_surface(window);
    }

    /// Starts tracking a press on `window` as a potential icon drag —
    /// every press on a tile arms both a possible drag *and* a possible
    /// plain click, resolved by `end_icon_drag` on release (see
    /// `IconDrag::moved`'s doc comment). A no-op (and returns `false`)
    /// if `window` isn't a tracked icon tile, so callers can use the
    /// return value to know whether to swallow the press.
    pub fn begin_icon_drag(&mut self, backend: &mut B, window: B::ShellId, local: Point) -> bool {
        if !self.icons.contains_key(&window) {
            return false;
        }
        // One pointer grab for the drag's lifetime, exactly like window-
        // move and the root menu — without it, a fast drag would outrun
        // the tiny tile window's own bounds and start reporting motion
        // against whatever's underneath instead (see `drag_icon_motion`,
        // which reads root-relative motion regardless of which window
        // it's nominally attached to, so the grab just has to keep
        // *some* motion event flowing, not necessarily one addressed to
        // this specific tile).
        let grab = backend.grab_pointer_for_drag();
        self.icon_drag = Some(IconDrag { window, grab_offset: local, moved: false, grab });
        true
    }

    /// Feeds root-relative pointer motion to an in-progress icon drag —
    /// call this on every `BackendEvent::PointerMotion`, not just shell-
    /// targeted motion, since once the pointer leaves the tile's own
    /// small bounds further motion is no longer reported against it.
    pub fn drag_icon_motion(&mut self, backend: &mut B, root: Point) {
        let Some(drag) = &mut self.icon_drag else {
            return;
        };
        let Some(icon) = self.icons.get_mut(&drag.window) else {
            self.icon_drag = None;
            return;
        };

        let Some(new_pos) = resolve_drag_position(icon.pos, drag.grab_offset, root, self.drag_threshold, drag.moved) else {
            return;
        };
        drag.moved = true;
        icon.pos = new_pos;
        icon.auto_slot = None;
        backend.configure_shell_surface(drag.window, Rect { pos: new_pos, size: Size::new(self.tile, self.tile) });
    }

    /// Resolves whatever press `begin_icon_drag` armed, if any: a
    /// release without crossing the move threshold restores the
    /// window (matching a plain click), one that did leaves the icon
    /// at its new dragged position and reports where the pointer let
    /// go so the caller can hit-test drop targets. Returns `None` if
    /// no icon drag was in progress — callers should fall through to
    /// their normal release handling (e.g. menu clicks) in that case.
    pub fn end_icon_drag(&mut self, backend: &mut B) -> Option<IconDragResult> {
        let drag = self.icon_drag.take()?;
        backend.ungrab_pointer(drag.grab);

        if drag.moved {
            let icon = self.icons.get(&drag.window)?;
            // The release position isn't handed to this method, but the
            // drag state determines it: every motion placed the tile at
            // `pointer - grab_offset` (see `resolve_drag_position`), so
            // the pointer's last root position is exactly the tile's
            // final position plus the in-tile grab offset.
            let root = Point::new(icon.pos.x + drag.grab_offset.x, icon.pos.y + drag.grab_offset.y);
            return Some(IconDragResult::Repositioned { client: icon.client, root });
        }
        let icon = self.icons.remove(&drag.window)?;
        backend.destroy_shell_surface(icon.window);
        Some(IconDragResult::Restore(icon.client))
    }
}

/// Pure drag-threshold arithmetic, kept separate from `drag_icon_motion`
/// so it's testable without a backend: `None` if motion hasn't
/// crossed `threshold` yet (and hadn't already elsewhere in the drag),
/// `Some(new_pos)` once it has. The threshold is the reason a
/// press-then-release with no real movement still works as a plain
/// click instead of "dragging" the icon a few pixels and never
/// restoring the window.
fn resolve_drag_position(icon_pos: Point, grab_offset: Point, root: Point, threshold: i32, already_moved: bool) -> Option<Point> {
    let candidate = Point::new(root.x - grab_offset.x, root.y - grab_offset.y);
    if already_moved {
        return Some(candidate);
    }
    let (dx, dy) = (candidate.x - icon_pos.x, candidate.y - icon_pos.y);
    if dx.abs() < threshold && dy.abs() < threshold {
        None
    } else {
        Some(candidate)
    }
}

fn pixmap_to_buffer(pixmap: &Pixmap) -> DecorationBuffer {
    DecorationBuffer { width: pixmap.width(), height: pixmap.height(), pixels: pixmap.data().to_vec() }
}

/// `pub(crate)` because a remote tile composes its own dead face out
/// of a square dead screen and plain tile base — see
/// `dockapp::tile::dead_face`. One clipping blit, used by both.
pub(crate) fn blit_into(dest: &mut Pixmap, x: u32, y: u32, src: &DecorationBuffer) {
    let (dest_w, dest_h) = (dest.width(), dest.height());
    for row in 0..src.height {
        let dy = y + row;
        if dy >= dest_h {
            break;
        }
        for col in 0..src.width {
            let dx = x + col;
            if dx >= dest_w {
                continue;
            }
            let sidx = ((row * src.width + col) * 4) as usize;
            if sidx + 4 > src.pixels.len() {
                continue;
            }
            let (r, g, b, a) = (src.pixels[sidx], src.pixels[sidx + 1], src.pixels[sidx + 2], src.pixels[sidx + 3]);
            if let Some(px) = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a) {
                let pidx = (dy * dest_w + dx) as usize;
                dest.pixels_mut()[pidx] = px;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::{AppCategory, AppEntry};
    use crate::widgets::BUILTIN_PREFIX;

    /// The invariant the whole live-scale path rests on: a session that
    /// was rescaled is indistinguishable from one that started at that
    /// scale.
    ///
    /// It is worth a test rather than a reading of `set_scale`, because
    /// the failure it guards against is not a wrong formula — it is a
    /// *forgotten* one. `Desktop::new` derives five things from the
    /// scale; `set_scale` has to derive the same five, and nothing but
    /// this assertion notices the day someone adds a sixth to one and
    /// not the other.
    /// A screen small enough that the wallpaper render every
    /// `Desktop::new` performs stays cheap. Nothing these tests assert
    /// depends on the size — the dock hangs off the primary's right
    /// edge and the Clip off its top-left corner at any of them — and a
    /// desktop-sized pixmap per construction is most of what makes
    /// these the slowest tests in the crate.
    const TEST_SCREEN: Size = Size { w: 640, h: 480 };

    /// The invariant the whole live-scale path rests on: a session that
    /// was rescaled is indistinguishable from one that started at that
    /// scale.
    ///
    /// It is worth a test rather than a reading of `set_scale`, because
    /// the failure it guards against is not a wrong formula — it is a
    /// *forgotten* one. `Desktop::new` derives five things from the
    /// scale; `set_scale` has to derive the same five, and nothing but
    /// this assertion notices the day someone adds a sixth to one and
    /// not the other.
    #[test]
    fn the_workarea_stops_at_the_dock_column() {
        // A maximized window must not slide under the instruments. The
        // reservation is exactly one dock column off the right edge of
        // the primary — no more (the Clip reserves nothing), and it has
        // to track a live rescale, since the column widens with the
        // tile.
        use wm_core::fake_backend::FakeBackend;

        let primary = Rect { pos: Point::new(0, 0), size: TEST_SCREEN };
        let mut backend = FakeBackend::new();
        let mut desktop: Desktop<FakeBackend> =
            Desktop::new(&mut backend, TEST_SCREEN, primary, 1.0, "nextstep-classic".to_string(), Vec::new());

        let area = desktop.primary_workarea();
        assert_eq!(area.size.w, TEST_SCREEN.w - tile_px(1.0), "one dock column reserved on the right");
        assert_eq!(area.size.h, TEST_SCREEN.h, "nothing reserved vertically");
        assert_eq!(area.pos, primary.pos);

        desktop.set_scale(2.0);
        assert_eq!(desktop.primary_workarea().size.w, TEST_SCREEN.w - tile_px(2.0), "the reservation follows the tile");
    }

    #[test]
    fn a_rescaled_desktop_is_the_desktop_that_scale_would_have_built() {
        use wm_core::fake_backend::FakeBackend;

        let primary = Rect { pos: Point::new(0, 0), size: TEST_SCREEN };
        let build = |backend: &mut FakeBackend, scale: f32| -> Desktop<FakeBackend> {
            Desktop::new(backend, TEST_SCREEN, primary, scale, "nextstep-classic".to_string(), Vec::new())
        };

        let mut backend = FakeBackend::new();
        let mut rescaled = build(&mut backend, 1.0);
        assert!(rescaled.set_scale(2.0), "moving to a different scale must report a change");
        let native = build(&mut backend, 2.0);

        assert_eq!(rescaled.tile, native.tile);
        assert_eq!(rescaled.pad, native.pad);
        assert_eq!(rescaled.dock_width, native.dock_width);
        assert_eq!(rescaled.drag_threshold, native.drag_threshold);
        assert_eq!(rescaled.scale, native.scale);

        // The counterpart, asserted here rather than in a test of its
        // own because a `Desktop` costs a fontconfig scan to stand up
        // and this test already has two: re-applying the scale it is
        // already at reports no change. Load-bearing for the applier,
        // which repaints nothing when this is false — a reload that
        // only rebound a key must not flash the desktop.
        assert!(!rescaled.set_scale(2.0));
    }

    /// The test above proves `set_scale` re-derives the right numbers.
    /// This one proves they reach the screen: that the dock's *surface*
    /// is reconfigured, not merely repainted at a new size inside its
    /// old rect. Those two failures look identical in a screenshot of
    /// the dock and completely different to anything trying to click on
    /// it.
    #[test]
    fn a_rescale_moves_the_dock_surface_and_not_just_its_pixels() {
        use wm_core::fake_backend::FakeBackend;

        let primary = Rect { pos: Point::new(0, 0), size: TEST_SCREEN };
        let mut backend = FakeBackend::new();
        let mut desktop: Desktop<FakeBackend> =
            Desktop::new(&mut backend, TEST_SCREEN, primary, 1.0, "nextstep-classic".to_string(), Vec::new());

        let dock = desktop.dock_window();
        let clip = desktop.clip_window();
        assert_eq!(backend.shell_geometries[&dock].size.w, tile_px(1.0), "the dock is one tile wide");

        desktop.set_scale(2.0);
        desktop.relayout(&mut backend, &wm_theme::default_theme::nextstep_classic().scaled(2.0), &[]);

        let after = backend.shell_geometries[&dock];
        assert_eq!(after.size.w, tile_px(2.0), "the dock surface must be reconfigured to the new tile width");
        // Still anchored to the monitor's right edge: it grew leftward
        // rather than sliding off the screen.
        assert_eq!(after.pos.x + after.size.w as i32, primary.size.w as i32);
        assert_eq!(backend.shell_geometries[&clip].size, Size::new(tile_px(2.0), tile_px(2.0)));
    }

    #[test]
    fn a_scale_change_survives_a_round_trip() {
        // Scaling is lossy — `Theme::scaled` rounds every metric to
        // whole pixels — which is why a session keeps its theme at 1x
        // and re-derives, rather than rescaling what it already has.
        // These metrics are derived the same way, from the scale alone
        // rather than from their own previous value, so going up and
        // coming back lands exactly where it started. Asserted on the
        // derivation rather than on a `Desktop`, because standing one
        // up costs a fontconfig scan and this needs no backend at all.
        assert_eq!((tile_px(1.0), icon_pad_px(1.0)), (56, 4));
        assert_eq!((tile_px(2.0), icon_pad_px(2.0)), (112, 8));
        assert_eq!((tile_px(1.0), icon_pad_px(1.0)), (56, 4));
    }

    #[test]
    fn a_tile_stays_drawable_at_an_absurdly_small_scale() {
        // The floors in `tile_px`/`icon_pad_px` are what stop a
        // hand-edited `scale = 0.01` from asking the backend for a
        // zero-sized surface. `resolve_scale` refuses zero and
        // negatives; it does not refuse "very small".
        assert_eq!(tile_px(0.001), 16);
        assert_eq!(icon_pad_px(0.001), 1);
        assert_eq!(drag_threshold_px(0.001), 2);
    }

    /// Ids are the persistence key for the whole column, so two rules
    /// hold and neither is checkable by reading `builtin_items` alone
    /// once it has more entries than fit on a screen: every built-in
    /// sits in the reserved namespace (so no `.dockapp` can claim one),
    /// and no two share an id (a duplicate would make `dock_order`
    /// place one of them and silently drop the other on the next
    /// rewrite).
    #[test]
    fn builtin_ids_are_reserved_and_unique() {
        let items = builtin_items();
        let ids: Vec<&str> = items.iter().map(|item| item.id()).collect();
        for id in &ids {
            assert!(id.starts_with(BUILTIN_PREFIX), "{id} must sit in the reserved namespace");
            assert!(id.len() > BUILTIN_PREFIX.len(), "{id} is the bare prefix with no name after it");
        }
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "two built-ins share a persistence id: {ids:?}");
    }

    /// The ids are also a *published* format — they appear in a file
    /// the user is invited to edit — so changing one is a breaking
    /// change to their dock arrangement, not a rename. Pinning them
    /// here makes that cost visible in the diff rather than in a bug
    /// report six months later saying the dock "randomly reset".
    #[test]
    fn builtin_ids_are_the_ones_already_written_to_users_dock_item_files() {
        let items = builtin_items();
        let ids: Vec<&str> = items.iter().map(|item| item.id()).collect();
        assert_eq!(ids, ["builtin:net", "builtin:sysload", "builtin:sound", "builtin:wifi", "builtin:power", "builtin:clock"]);
    }

    struct FixedHeightWidget(u32);

    impl DockWidget for FixedHeightWidget {
        fn name(&self) -> &'static str {
            "FIX"
        }

        fn update(&mut self, _samples: &crate::widgets::Samples) -> bool {
            false
        }

        fn render(&self, _theme: &Theme, _tile: u32, _fonts: &mut cosmic_text::FontSystem, _swash: &mut cosmic_text::SwashCache) -> DecorationBuffer {
            DecorationBuffer { width: 1, height: 1, pixels: vec![0; 4] }
        }

        fn tile_height(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn dock_height_is_only_the_identity_and_current_widget_stack() {
        let items: Vec<SupervisedWidget> = [
            DockItem::builtin("builtin:one", Box::new(FixedHeightWidget(1)) as Box<dyn DockWidget>),
            DockItem::builtin("builtin:three", Box::new(FixedHeightWidget(3))),
        ]
        .into_iter()
        .map(SupervisedWidget::new)
        .collect();

        assert_eq!(stacked_dock_height(56, 1_080, &items), 280);
        assert_eq!(stacked_dock_height(56, 200, &items), 200, "oversized stacks are screen-clamped");
    }

    /// A second head placed to the *left* of the primary: the whole
    /// desktop then spans x -1920..1920, so anything that anchored
    /// chrome to the screen's own origin or size would land it on the
    /// wrong output. Every geometry test below measures against this.
    const AUX_LEFT: Rect = Rect { pos: Point { x: -1920, y: 0 }, size: Size { w: 1920, h: 1080 } };
    const PRIMARY: Rect = Rect { pos: Point { x: 0, y: 0 }, size: Size { w: 1600, h: 1200 } };

    #[test]
    fn the_dock_hugs_the_primary_monitors_top_right_corner() {
        let dock = dock_geometry(PRIMARY, 56, 400);
        assert_eq!(dock, Rect { pos: Point::new(1544, 0), size: Size::new(56, 400) });

        // Moving the primary itself carries the dock with it — the
        // case a screen-anchored dock got wrong, since the desktop's
        // own top-right corner belongs to whichever head sits furthest
        // right.
        let offset_primary = Rect { pos: Point::new(1920, 200), size: Size::new(1600, 1200) };
        let dock = dock_geometry(offset_primary, 56, 400);
        assert_eq!(dock, Rect { pos: Point::new(3464, 200), size: Size::new(56, 400) });
    }

    #[test]
    fn a_dock_wider_than_its_monitor_pins_to_that_monitors_left_edge() {
        // `saturating_sub` rather than a wrap into a huge positive x:
        // an absurd dock width must still leave the surface on the
        // monitor it belongs to.
        let narrow = Rect { pos: Point::new(-1920, 0), size: Size::new(40, 1080) };
        assert_eq!(dock_geometry(narrow, 56, 100).pos, Point::new(-1920, 0));
    }

    #[test]
    fn the_clip_sits_in_the_primary_monitors_own_corner() {
        assert_eq!(clip_geometry(PRIMARY, 56), Rect { pos: Point::new(0, 0), size: Size::new(56, 56) });
        // On a primary that is *not* at the desktop's origin, the Clip
        // follows the monitor rather than staying at root (0, 0) — which
        // on this arrangement is still on the primary only by accident.
        let right_primary = Rect { pos: Point::new(1920, 0), size: Size::new(1600, 1200) };
        assert_eq!(clip_geometry(right_primary, 56), Rect { pos: Point::new(1920, 0), size: Size::new(56, 56) });
    }

    #[test]
    fn icon_slots_fill_the_primary_monitors_bottom_edge_and_wrap_upward() {
        // 1600 wide, stride 60 (56 + 4 pad) — 26 columns before the row
        // wraps, and the first row sits one stride up from the
        // monitor's bottom edge.
        assert_eq!(icon_slot_position(PRIMARY, 56, 4, 0), Point::new(4, 1140));
        assert_eq!(icon_slot_position(PRIMARY, 56, 4, 1), Point::new(64, 1140));
        assert_eq!(icon_slot_position(PRIMARY, 56, 4, 26), Point::new(4, 1080), "the 27th tile wraps onto a second row");

        // The same slots on a head at negative x land in *its* bottom
        // -left corner, not at the desktop's origin.
        assert_eq!(icon_slot_position(AUX_LEFT, 56, 4, 0), Point::new(-1916, 1020));
    }

    #[test]
    fn the_switcher_panel_centers_on_the_primary_monitor() {
        assert_eq!(centered_on(PRIMARY, Size::new(600, 200)), Point::new(500, 500));
        // Centered on the head, not on the desktop: an arrangement
        // whose union is twice as wide must not push the panel onto the
        // seam between the two.
        assert_eq!(centered_on(AUX_LEFT, Size::new(600, 200)), Point::new(-1260, 440));
    }

    #[test]
    fn every_monitor_gets_a_workarea_in_the_order_the_wm_indexes_them() {
        // The Dock reserves nothing today, so the primary's entry is
        // its own full rect — but it is still computed as the primary's
        // entry, positionally, so a future reserved strip lands on the
        // right head and only there.
        let monitors = [AUX_LEFT, PRIMARY];
        assert_eq!(workareas_for(&monitors, PRIMARY, PRIMARY), vec![AUX_LEFT, PRIMARY]);

        // With a strip actually reserved, only the primary's entry
        // shrinks; every other head keeps its full geometry.
        let reserved = Rect { pos: Point::new(0, 0), size: Size::new(1600, 1140) };
        assert_eq!(workareas_for(&monitors, PRIMARY, reserved), vec![AUX_LEFT, reserved]);
    }

    #[test]
    fn tiny_motion_below_threshold_does_not_start_a_drag() {
        let icon_pos = Point::new(100, 100);
        let grab_offset = Point::new(4, 4);
        let root = Point::new(105, 102); // candidate = (101, 98) — within threshold of (100,100)

        let result = resolve_drag_position(icon_pos, grab_offset, root, 8, false);

        assert_eq!(result, None);
    }

    #[test]
    fn motion_past_threshold_starts_a_drag_at_the_grab_relative_position() {
        let icon_pos = Point::new(100, 100);
        let grab_offset = Point::new(4, 4);
        let root = Point::new(120, 100); // candidate = (116, 96) — 16px past threshold on x

        let result = resolve_drag_position(icon_pos, grab_offset, root, 8, false);

        assert_eq!(result, Some(Point::new(116, 96)));
    }

    #[test]
    fn once_already_moved_every_motion_tracks_without_re_checking_the_threshold() {
        // A drag already past the threshold must keep tracking the
        // pointer exactly, even for a motion event smaller than the
        // threshold — otherwise a slow final approach to the drop point
        // would visibly stick a few pixels short.
        let icon_pos = Point::new(100, 100);
        let grab_offset = Point::new(0, 0);
        let root = Point::new(200, 201); // a 1px nudge, well under any real threshold

        let result = resolve_drag_position(icon_pos, grab_offset, root, 8, true);

        assert_eq!(result, Some(Point::new(200, 201)));
    }

    #[test]
    fn drag_position_accounts_for_the_press_point_within_the_tile() {
        // Pressing off-center (not the tile's top-left corner) must not
        // make the icon jump to align its corner with the pointer.
        let icon_pos = Point::new(100, 100);
        let grab_offset = Point::new(20, 10); // pressed 20px right, 10px down from the tile's corner
        let root = Point::new(150, 150);

        let result = resolve_drag_position(icon_pos, grab_offset, root, 8, false);

        assert_eq!(result, Some(Point::new(130, 140)));
    }

    #[test]
    fn wallpaper_actions_resolve_to_every_built_in_wallpaper() {
        for (index, wallpaper) in Wallpaper::ALL.into_iter().enumerate() {
            assert!(matches!(
                resolve_action(ACTION_WALLPAPER_BASE + index as u32, 0),
                Some(RootMenuAction::SetWallpaper(resolved)) if resolved == wallpaper
            ));
        }
    }

    #[test]
    fn wallpaper_submenu_marks_the_current_selection() {
        let items = root_menu_items(Wallpaper::TealBlueprint, "nextstep-classic", &[]);
        let submenu = items.iter().find(|item| item.label() == "Wallpaper").expect("wallpaper submenu");
        let MenuItem::Submenu { items, .. } = submenu else { panic!("expected submenu") };
        assert_eq!(items.len(), Wallpaper::ALL.len());
        assert!(items.iter().any(|item| item.label() == "\u{2022} Teal Blueprint"));
    }

    /// A minimal scanned entry — only `name` and `category` matter to
    /// the menu; the rest is inert plumbing the launcher consumes.
    fn app(name: &str, category: AppCategory) -> AppEntry {
        AppEntry {
            id: name.to_lowercase(),
            name: name.to_string(),
            exec: vec![name.to_lowercase()],
            terminal: false,
            category,
            startup_wm_class: None,
        }
    }

    /// A name-sorted index (as `scan_applications` delivers) whose
    /// categories are deliberately *not* encountered in enum order —
    /// Chromium (Internet) sorts first — so a test over it can tell
    /// derived-order grouping apart from first-seen grouping.
    fn app_index() -> Vec<AppEntry> {
        vec![
            app("Chromium", AppCategory::Internet),
            app("Emacs", AppCategory::Development),
            app("GIMP", AppCategory::Graphics),
            app("Inkscape", AppCategory::Graphics),
        ]
    }

    /// The Applications submenu's item list, dug out of a full root
    /// menu build so these tests exercise the real assembly path, not
    /// `applications_items` in isolation.
    fn applications_submenu(apps: &[AppEntry]) -> Vec<MenuItem> {
        let items = root_menu_items(Wallpaper::TealBlueprint, "nextstep-classic", apps);
        let submenu = items.iter().find(|item| item.label() == "Applications").expect("Applications submenu");
        let MenuItem::Submenu { items, .. } = submenu else { panic!("expected a submenu") };
        items.clone()
    }

    #[test]
    fn applications_builds_one_cascade_per_populated_category_in_derived_order() {
        let applications = applications_submenu(&app_index());

        // Only the three populated categories appear — no empty
        // cascade for Games, Office, or the rest — grouped in
        // `AppCategory`'s derived order even though Internet was the
        // first category encountered in the index, with About closing
        // the submenu after every cascade.
        let labels: Vec<&str> = applications.iter().map(|item| item.label()).collect();
        assert_eq!(labels, ["Development", "Graphics", "Internet", "About chonkstep"]);

        // A multi-app category lists its apps in index order — which
        // is alphabetical, since the index arrives name-sorted — and
        // every id is `ACTION_APP_BASE` plus the app's *flat* index,
        // not its position within the cascade.
        let MenuItem::Submenu { items: graphics, .. } = &applications[1] else { panic!("expected a cascade") };
        let rows: Vec<(&str, u32)> = graphics
            .iter()
            .map(|item| {
                let MenuItem::Action { label, action } = item else { panic!("app rows are actions") };
                (label.as_str(), *action)
            })
            .collect();
        assert_eq!(rows, [("GIMP", ACTION_APP_BASE + 2), ("Inkscape", ACTION_APP_BASE + 3)]);
    }

    #[test]
    fn every_app_item_round_trips_through_resolve_action() {
        let apps = app_index();
        let applications = applications_submenu(&apps);

        let mut resolved = 0;
        for cascade in &applications {
            let MenuItem::Submenu { items, .. } = cascade else { continue };
            for item in items {
                let MenuItem::Action { label, action } = item else { panic!("app rows are actions") };
                let Some(RootMenuAction::LaunchApp(index)) = resolve_action(*action, apps.len()) else {
                    panic!("app id {action} must resolve to LaunchApp");
                };
                // The resolved index names the very app the label
                // promised — the whole point of carrying flat indices
                // through the category regrouping.
                assert_eq!(&apps[index].name, label);
                resolved += 1;
            }
        }
        assert_eq!(resolved, apps.len(), "every indexed app must be reachable from some cascade");
    }

    #[test]
    fn app_ids_past_the_index_end_resolve_to_none() {
        let apps = app_index();
        // First id past the vec's end, and the base id against an
        // empty index: both out of bounds, both must dissolve rather
        // than index into the stored vec downstream.
        assert!(resolve_action(ACTION_APP_BASE + apps.len() as u32, apps.len()).is_none());
        assert!(resolve_action(ACTION_APP_BASE, 0).is_none());
        assert!(resolve_action(u32::MAX, apps.len()).is_none());
    }

    #[test]
    fn an_empty_app_index_leaves_applications_as_just_about() {
        let applications = applications_submenu(&[]);
        let labels: Vec<&str> = applications.iter().map(|item| item.label()).collect();
        assert_eq!(labels, ["About chonkstep"], "no empty category cascades, About still reachable");
        assert!(
            applications.iter().all(|item| matches!(item, MenuItem::Action { .. })),
            "an empty index must produce no cascade at all, not empty ones"
        );
    }

    fn window_ctx(workspace: usize, workspace_count: usize) -> WindowMenuContext {
        WindowMenuContext {
            client: ClientId::default(),
            title: "xterm".to_string(),
            shaded: false,
            maximized: false,
            fullscreen: false,
            workspace,
            workspace_count,
        }
    }

    #[test]
    fn window_menu_labels_flip_to_their_undo_forms() {
        let plain = window_menu_items(&window_ctx(0, 1));
        let labels: Vec<&str> = plain.iter().map(|item| item.label()).collect();
        assert_eq!(
            labels,
            ["Maximize", "Miniaturize", "Shade", "Fullscreen", "Move To", "Close", "Kill"],
            "the classic entry order, with the plain do-forms for an untouched window"
        );

        let mut engaged = window_ctx(0, 1);
        engaged.maximized = true;
        engaged.shaded = true;
        engaged.fullscreen = true;
        let items = window_menu_items(&engaged);
        let labels: Vec<&str> = items.iter().map(|item| item.label()).collect();
        assert_eq!(
            labels,
            ["Unmaximize", "Miniaturize", "Unshade", "Exit Fullscreen", "Move To", "Close", "Kill"],
            "engaged states must offer their undo forms"
        );
    }

    #[test]
    fn move_to_submenu_lists_every_workspace_marks_the_current_and_ends_with_new_workspace() {
        let items = window_menu_items(&window_ctx(1, 3));
        let submenu = items.iter().find(|item| item.label() == "Move To").expect("Move To submenu");
        let MenuItem::Submenu { items: move_to, .. } = submenu else { panic!("expected a submenu") };

        let labels: Vec<&str> = move_to.iter().map(|item| item.label()).collect();
        assert_eq!(
            labels,
            ["  Workspace 1", "\u{2022} Workspace 2", "  Workspace 3", "  New Workspace"],
            "1-based labels, the window's own workspace bulleted, New Workspace last"
        );

        // Payloads are 0-based, in order, with New Workspace resolving
        // one past the last existing workspace — the id
        // `move_client_to_workspace` grows a workspace for.
        for (index, item) in move_to.iter().enumerate() {
            let MenuItem::Action { action, .. } = item else { panic!("Move To rows are actions") };
            assert_eq!(
                resolve_window_action(*action, 3),
                Some(WindowMenuAction::MoveToWorkspace(index)),
            );
        }
    }

    #[test]
    fn short_window_titles_pass_through_untruncated() {
        assert_eq!(window_menu_title("xterm"), "xterm");
        let exactly_at_cap = "a".repeat(WINDOW_MENU_TITLE_MAX_CHARS);
        assert_eq!(window_menu_title(&exactly_at_cap), exactly_at_cap);
    }

    #[test]
    fn long_window_titles_truncate_to_the_cap_with_a_trailing_ellipsis() {
        let truncated = window_menu_title(&"x".repeat(60));
        assert_eq!(truncated.chars().count(), WINDOW_MENU_TITLE_MAX_CHARS);
        assert!(truncated.ends_with('\u{2026}'));

        // Counted in characters, not bytes — a multibyte title must
        // truncate cleanly at the same visible length, never split a
        // code point (which would panic in a byte-indexed slice).
        let multibyte = window_menu_title(&"\u{00e9}".repeat(60));
        assert_eq!(multibyte.chars().count(), WINDOW_MENU_TITLE_MAX_CHARS);
        assert!(multibyte.ends_with('\u{2026}'));
    }

    #[test]
    fn action_ids_resolve_only_within_their_own_sessions_namespace() {
        let window_session = MenuSession::Window { client: ClientId::default(), workspace_count: 2 };
        let root_session = MenuSession::Root { app_count: 3 };

        // A root-menu id fired during a window session (stale event,
        // stray id — however it happened) must dissolve into nothing,
        // never decode as a window command. App ids included: they are
        // root-session ids like any other.
        assert!(resolve_session_action(&window_session, ACTION_LAUNCH_TERMINAL).is_none());
        assert!(resolve_session_action(&window_session, ACTION_WALLPAPER_BASE).is_none());
        assert!(resolve_session_action(&window_session, ACTION_THEME_BASE).is_none());
        assert!(resolve_session_action(&window_session, ACTION_APP_BASE).is_none());

        // And the reverse: window ids mean nothing to a root session.
        assert!(resolve_session_action(&root_session, ACTION_WINDOW_KILL).is_none());
        assert!(resolve_session_action(&root_session, ACTION_MOVE_TO_BASE).is_none());

        // While each session still resolves its own namespace.
        assert!(matches!(
            resolve_session_action(&window_session, ACTION_WINDOW_CLOSE),
            Some(MenuAction::Window(_, WindowMenuAction::Close))
        ));
        assert!(matches!(
            resolve_session_action(&root_session, ACTION_LAUNCH_TERMINAL),
            Some(MenuAction::Root(RootMenuAction::LaunchTerminal))
        ));
        assert!(matches!(
            resolve_session_action(&root_session, ACTION_APP_BASE + 2),
            Some(MenuAction::Root(RootMenuAction::LaunchApp(2)))
        ));
    }

    /// Minimal `PopupHost` for driving `ShellMenu` without any backend
    /// — the same seam `CascadeMenu`'s own tests use. `ShellMenu` is
    /// generic over the popup id, so `PopupId = u32` satisfies its
    /// bounds with a plain counter.
    #[derive(Default)]
    struct FakeHost {
        next_id: u32,
        open: std::collections::HashSet<u32>,
        grabs: u32,
        ungrabs: u32,
    }

    impl wm_theme_api::PopupHost for FakeHost {
        type PopupId = u32;

        fn create_popup(&mut self, _geometry: Rect, _background: (u8, u8, u8)) -> Option<u32> {
            self.next_id += 1;
            self.open.insert(self.next_id);
            Some(self.next_id)
        }

        fn destroy_popup(&mut self, popup: u32) {
            self.open.remove(&popup);
        }

        fn paint_popup(&mut self, _popup: u32, _buffer: &DecorationBuffer) {}

        fn grab_pointer(&mut self) -> wm_theme_api::PopupGrab {
            self.grabs += 1;
            wm_theme_api::PopupGrab(0)
        }

        fn ungrab_pointer(&mut self, _grab: wm_theme_api::PopupGrab) {
            self.ungrabs += 1;
        }
    }

    struct MenuFixture {
        theme: Theme,
        font_system: cosmic_text::FontSystem,
        host: FakeHost,
        menu: ShellMenu<u32>,
    }

    impl MenuFixture {
        fn new() -> Self {
            Self {
                theme: wm_theme::default_theme::nextstep_classic(),
                font_system: cosmic_text::FontSystem::new(),
                host: FakeHost::default(),
                menu: ShellMenu::new(),
            }
        }

        fn open_root(&mut self) {
            let items = root_menu_items(Wallpaper::TealBlueprint, "nextstep-classic", &[]);
            self.menu.open_root(&mut self.host, &self.theme, &mut self.font_system, items, 0, Point::new(0, 0), Size::new(1600, 1000));
        }

        fn open_window(&mut self, ctx: &WindowMenuContext) {
            self.menu.open_window(&mut self.host, &self.theme, &mut self.font_system, ctx, Point::new(0, 0), Size::new(1600, 1000));
        }

        /// Center of item row `index`, computed from a real render of
        /// the same title/items the open session used — honest about
        /// where rows actually land as the menu's layout recipe
        /// evolves, same as `cascade.rs`'s own row-point helper.
        fn row_point(&mut self, title: &str, items: &[MenuItem], index: usize) -> Point {
            let render = wm_theme::menu::render_menu(&self.theme, &mut self.font_system, title, items, None, false);
            let rect = render.item_rects[index];
            Point::new(rect.pos.x + rect.size.w as i32 / 2, rect.pos.y + rect.size.h as i32 / 2)
        }

        fn click(&mut self, window: u32, local: Point) -> Option<MenuAction> {
            self.menu.click(&mut self.host, &self.theme, &mut self.font_system, window, local)
        }

        fn only_open_window(&self) -> u32 {
            assert_eq!(self.host.open.len(), 1, "expected exactly one open popup");
            *self.host.open.iter().next().unwrap()
        }
    }

    #[test]
    fn opening_the_window_menu_over_an_open_root_menu_leaves_one_session() {
        let mut f = MenuFixture::new();
        f.open_root();
        assert_eq!(f.host.open.len(), 1);

        let ctx = window_ctx(0, 2);
        f.open_window(&ctx);

        assert_eq!(f.host.open.len(), 1, "the root session must be torn down, not shadowed");
        assert_eq!(f.host.ungrabs, 1, "the root session's pointer grab must be released");
        assert_eq!(f.host.grabs, 2, "the window session holds its own grab");

        // And the surviving session resolves clicks in the *window*
        // namespace: its first row is Maximize, not the root menu's
        // Terminal.
        let window = f.only_open_window();
        let items = window_menu_items(&ctx);
        let row = f.row_point(&window_menu_title(&ctx.title), &items, 0);
        assert!(matches!(
            f.click(window, row),
            Some(MenuAction::Window(_, WindowMenuAction::ToggleMaximize))
        ));
        assert!(f.host.open.is_empty(), "firing an action closes the session");
    }

    #[test]
    fn reopening_the_root_menu_after_a_window_session_restores_root_resolution() {
        let mut f = MenuFixture::new();
        f.open_window(&window_ctx(0, 1));
        f.open_root();
        assert_eq!(f.host.open.len(), 1);

        let window = f.only_open_window();
        let items = root_menu_items(Wallpaper::TealBlueprint, "nextstep-classic", &[]);
        let row = f.row_point(ROOT_MENU_TITLE, &items, 0);
        assert!(matches!(
            f.click(window, row),
            Some(MenuAction::Root(RootMenuAction::LaunchTerminal))
        ));
    }
}

#[cfg(test)]
mod dock_order_tests {
    use super::dock_order::{arrange, load, merge, save};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    fn ids(order: &[&str]) -> Vec<String> {
        order.iter().map(|id| id.to_string()).collect()
    }

    fn arranged(items: &[&str], order: &[String]) -> Vec<String> {
        arrange(ids(items), order, |item| item.clone())
    }

    /// A unique per-test state file under the system temp dir, so
    /// parallel tests never share a file and no environment variable is
    /// mutated (env is process-global; a test touching it would race
    /// every other test in the binary). Same shape as
    /// `launchdock`'s own fixture, for the same reason.
    fn temp_state_file(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("chonkstep-dock-order-{}-{tag}-{unique}", std::process::id())).join("dock-items")
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn a_session_with_no_remembered_order_keeps_the_built_in_default() {
        assert_eq!(arranged(&["a", "b", "c"], &[]), ids(&["a", "b", "c"]));
    }

    #[test]
    fn the_remembered_order_is_what_the_column_comes_up_in() {
        assert_eq!(arranged(&["a", "b", "c"], &ids(&["c", "a", "b"])), ids(&["c", "a", "b"]));
    }

    /// The upgrade case: a release adds a seventh instrument that no
    /// existing `dock-items` file mentions. It must appear — a new
    /// instrument that is invisible until the user finds a file to edit
    /// is a broken feature — and it must appear somewhere predictable
    /// rather than in the middle of an arrangement they built.
    #[test]
    fn an_item_nobody_remembers_lands_at_the_bottom_in_declaration_order() {
        assert_eq!(arranged(&["a", "b", "new1", "new2"], &ids(&["b", "a"])), ids(&["b", "a", "new1", "new2"]));
    }

    /// The rule this whole module turns on. A dockapp whose registry
    /// file is missing this session — mid-upgrade, unmounted
    /// filesystem, a typo the user is about to fix — must not lose its
    /// place. It is skipped at load, and put back by the next write.
    #[test]
    fn an_entry_that_did_not_resolve_keeps_its_place_through_a_reorder() {
        let remembered = ids(&["clock", "absent", "power"]);
        // It resolves to nothing, so the live column has two tiles.
        assert_eq!(arranged(&["clock", "power"], &remembered), ids(&["clock", "power"]));
        // The user then drags power above clock. The absent entry is
        // still between them, because that is where they left it.
        assert_eq!(merge(&ids(&["power", "clock"]), &remembered), ids(&["power", "clock", "absent"]));
        // ...and with the original arrangement it is still in the
        // middle, following the same neighbour it always followed.
        assert_eq!(merge(&ids(&["clock", "power"]), &remembered), ids(&["clock", "absent", "power"]));
    }

    #[test]
    fn an_unresolved_first_entry_goes_back_to_the_top() {
        assert_eq!(merge(&ids(&["b", "c"]), &ids(&["absent", "b", "c"])), ids(&["absent", "b", "c"]));
    }

    #[test]
    fn consecutive_unresolved_entries_keep_their_own_order() {
        assert_eq!(merge(&ids(&["a", "z"]), &ids(&["a", "x", "y", "z"])), ids(&["a", "x", "y", "z"]));
    }

    /// Nothing above is worth anything if the file it round-trips
    /// through cannot carry it. Comments and blank lines survive being
    /// ignored because this is a file people are told they may edit.
    #[test]
    fn the_order_round_trips_through_the_state_file() {
        let path = temp_state_file("roundtrip");
        save(&path, &ids(&["builtin:clock", "chonk-dockclock", "builtin:net"])).unwrap();
        assert_eq!(load(&path), ids(&["builtin:clock", "chonk-dockclock", "builtin:net"]));

        std::fs::write(&path, "# hand-edited\n\nbuiltin:net\n  builtin:clock  \n").unwrap();
        assert_eq!(load(&path), ids(&["builtin:net", "builtin:clock"]), "comments and padding are not ids");

        cleanup(&path);
    }

    #[test]
    fn a_missing_file_is_an_empty_order_rather_than_an_error() {
        assert!(load(Path::new("/nonexistent/chonkstep/dock-items")).is_empty());
    }

    /// A duplicated line — trivially producible by hand-editing — must
    /// not clone a tile into two slots.
    #[test]
    fn a_duplicated_remembered_id_places_the_item_once() {
        assert_eq!(arranged(&["a", "b"], &ids(&["b", "b", "a"])), ids(&["b", "a"]));
    }
}
