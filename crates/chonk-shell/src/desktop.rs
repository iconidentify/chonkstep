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

use crate::wallpaper::Wallpaper;
use crate::widgets::{
    run_detached, ClockWidget, DockInput, DockWidget, Effect, NetTrafficWidget, PowerWidget, SamplerRegistry, SoundWidget, SupervisedWidget, SysLoadWidget,
    WifiWidget, WorkspaceShared,
};

/// The desktop background color — a cool lavender-gray sampled from a
/// reference NeXTSTEP desktop screenshot, not the neutral gray this
/// theme's window chrome uses. The dock has no separate backdrop panel
/// in that reference either: icons sit directly on this same color.
pub const DESKTOP_BG: (u8, u8, u8) = (128, 129, 159);

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
        MenuSession::Window { client, workspace_count } => {
            resolve_window_action(action, *workspace_count)
                .map(|window_action| MenuAction::Window(*client, window_action))
        }
    }
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
struct WidgetDrag {
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
fn stacked_dock_height(tile: u32, screen_height: u32, widgets: &[SupervisedWidget]) -> u32 {
    widgets
        .iter()
        // `SupervisedWidget::tile_height` already floors a widget's own
        // answer at one tile, and already answers exactly one for an
        // evicted widget — so an evicted multi-tile instrument shrinks
        // the dock rather than leaving a hole where its extra tiles
        // used to be.
        .fold(tile, |height, widget| height.saturating_add(tile.saturating_mul(widget.tile_height())))
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
    widgets: Vec<SupervisedWidget>,
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
    widget_drag: Option<WidgetDrag>,
    icons: HashMap<B::ShellId, IconTile<B::ShellId>>,
    icon_drag: Option<IconDrag<B::ShellId>>,
    wallpaper: Wallpaper,
    /// The Alt-Tab switch panel, while a cycle session is live.
    switcher: Option<SwitcherPanel<B::ShellId>>,
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
        let tile = ((56.0 * scale).round() as u32).max(16);
        let pad = ((4.0 * scale).round() as u32).max(1);
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
        // The dock's instrument stack, top to bottom, under the
        // identity tile: the five instruments — network traffic,
        // system load, sound, link, power — with the analog clock as
        // the bookend at the bottom, closing the rack the way the
        // identity tile opens it (the two non-instrument faces frame
        // the glass screens between them). Middle-click drag reorders
        // live; this is just the default order.
        let mut samplers = SamplerRegistry::new();
        let widgets: Vec<SupervisedWidget> = [
            Box::new(NetTrafficWidget::new()) as Box<dyn DockWidget>,
            Box::new(SysLoadWidget::new()),
            Box::new(SoundWidget::new()),
            Box::new(WifiWidget::new()),
            Box::new(PowerWidget::new()),
            Box::new(ClockWidget::new()),
        ]
        .into_iter()
        // Supervision is applied here, at the one place widgets enter
        // the dock, rather than being something each widget opts into:
        // the widget that needs it most is by definition the one that
        // would not have thought to.
        .map(SupervisedWidget::new)
        // And the same argument for sampling: a widget's sources are
        // registered and bound at the one place it enters the dock, so
        // no widget can be constructed into the stack with its sampling
        // half half-wired.
        .map(|mut widget| {
            widget.bind(&mut samplers);
            widget
        })
        .collect();
        let dock_height = stacked_dock_height(tile, primary.size.h, &widgets);
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
            drag_threshold: ((4.0 * scale).round() as i32).max(2),
            font_system: cosmic_text::FontSystem::new(),
            swash_cache: cosmic_text::SwashCache::new(),
            menu: ShellMenu::new(),
            widgets,
            samplers,
            workspace,
            clip_window,
            clip_drawn: (usize::MAX, 0),
            widget_drag: None,
            icons: HashMap::new(),
            icon_drag: None,
            wallpaper,
            switcher: None,
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
    /// its whole rect, minus whatever the Dock reserves — which is
    /// nothing today (see `workareas`).
    pub fn primary_workarea(&self) -> Rect {
        self.primary
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
    pub fn tick_widgets(&mut self, backend: &mut B, theme: &Theme) {
        self.samplers.refresh();
        let mut changed = false;
        {
            // Scoped so the borrow of `samplers` ends before the
            // repaint, which needs all of `self`.
            let samples = self.samplers.samples();
            for widget in &mut self.widgets {
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
    /// `tick_widgets` pass notices the change and repaints the dock
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
    fn widgets_top(&self) -> i32 {
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
    fn widget_slots(&self) -> Vec<(usize, Rect)> {
        let mut y = self.widgets_top();
        let mut slots = Vec::with_capacity(self.widgets.len());
        for (index, widget) in self.widgets.iter().enumerate() {
            let h = self.tile * widget.tile_height();
            slots.push((index, Rect { pos: Point::new(0, y), size: Size::new(self.tile, h) }));
            y += h as i32;
        }
        slots
    }

    /// Which widget slot (if any) `local` — in dock-local coordinates —
    /// falls within. Misses both the identity tile above and the
    /// inter-widget gaps between slots.
    fn widget_index_at(&self, local: Point) -> Option<usize> {
        self.widget_slots().into_iter().find(|(_, rect)| rect.contains(local)).map(|(index, _)| index)
    }

    /// Starts a middle-click drag-to-reorder on whichever widget sits at
    /// `local`, if any. Returns `false` (and does nothing) if `local`
    /// isn't over a widget slot, so callers know whether to treat the
    /// press as consumed.
    pub fn begin_widget_drag(&mut self, backend: &mut B, theme: &Theme, local: Point) -> bool {
        let Some(index) = self.widget_index_at(local) else { return false };
        // Same reasoning as `begin_icon_drag`: without a grab, a fast
        // drag could outrun the dock's own (narrow) window bounds and
        // stop reporting motion against it.
        let grab = backend.grab_pointer_for_drag();
        self.widget_drag = Some(WidgetDrag { index, grab });
        self.redraw_dock(backend, theme);
        true
    }

    /// Feeds root-relative pointer motion to an in-progress widget drag
    /// — call this on every `PointerMotion`, not just dock-targeted
    /// motion, for the same reason `drag_icon_motion` does. The dock
    /// starts at screen `y = 0`, so root-Y and dock-local-Y are already
    /// the same value; no translation needed.
    pub fn drag_widget_motion(&mut self, backend: &mut B, theme: &Theme, root: Point) {
        let Some(dragged) = self.widget_drag.as_ref().map(|d| d.index) else { return };
        let Some(target) = self.widget_index_at(Point::new(0, root.y)) else { return };
        if target == dragged {
            return;
        }
        self.widgets.swap(dragged, target);
        if let Some(drag) = &mut self.widget_drag {
            drag.index = target;
        }
        self.redraw_dock(backend, theme);
    }

    /// Ends whatever widget drag is in progress, if any. Returns `false`
    /// if no drag was active, so callers can tell whether the release
    /// was actually theirs to handle.
    pub fn end_widget_drag(&mut self, backend: &mut B, theme: &Theme) -> bool {
        let Some(drag) = self.widget_drag.take() else { return false };
        backend.ungrab_pointer(drag.grab);
        self.redraw_dock(backend, theme);
        true
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
        let Some((index, rect)) = self.widget_slots().into_iter().find(|(_, rect)| rect.contains(local)) else {
            return false;
        };
        let effects = self.widgets[index].on_input(input.translated(rect.pos), self.tile);
        self.apply_effects(backend, theme, effects);
        true
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
        let dock_height = stacked_dock_height(self.tile, self.primary.size.h, &self.widgets);
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

        for (index, rect) in self.widget_slots() {
            // `None` is an evicted widget: the dock draws its own
            // tombstone rather than calling code it has already
            // disowned. `render_dead_tile` is the same powered-off
            // face the instruments already use for "no sink", "no
            // interface", "no battery" — an evicted slot should read as
            // a dead instrument, which belongs to the family, and not
            // as a hole punched in the column. The widget's own name is
            // the label, so the dock says *which* one went dark without
            // the user having to find the log.
            let buffer = match self.widgets[index].render(theme, self.tile, &mut self.font_system, &mut self.swash_cache) {
                Some(buffer) => buffer,
                None => {
                    let label = self.widgets[index].name();
                    panel::render_dead_tile(theme, &mut self.font_system, &mut self.swash_cache, self.tile, label)
                }
            };
            blit_into(&mut pixmap, rect.pos.x as u32, rect.pos.y as u32, &buffer);

            if self.widget_drag.as_ref().is_some_and(|d| d.index == index) {
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
    /// per event-loop iteration (like `tick_widgets`).
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

        self.icons.insert(window, IconTile { window, client, pos, auto_slot: Some(slot) });
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

fn blit_into(dest: &mut Pixmap, x: u32, y: u32, src: &DecorationBuffer) {
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
        let widgets: Vec<SupervisedWidget> =
            [Box::new(FixedHeightWidget(1)) as Box<dyn DockWidget>, Box::new(FixedHeightWidget(3))].into_iter().map(SupervisedWidget::new).collect();

        assert_eq!(stacked_dock_height(56, 1_080, &widgets), 280);
        assert_eq!(stacked_dock_height(56, 200, &widgets), 200, "oversized stacks are screen-clamped");
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
