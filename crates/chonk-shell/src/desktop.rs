//! Desktop menus, wallpaper, and transient window navigation shared by
//! Wayland and X11. Persistent bars and workspace indicators are external
//! applications; constructing this desktop allocates no shell surfaces.

use std::cell::Cell;
use std::collections::BTreeMap;
use wm_core::{Backend, ClientId};
use wm_theme::cascade::{CascadeMenu, MenuClick, MenuKey};
use wm_theme::menu::MenuItem;
use wm_theme::switcher::{self, SwitcherEntry};
use wm_theme::Theme;
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};
use crate::omarchy_shell::BarVisibility;
use crate::overview::{OverviewHit, OverviewItem, OverviewPanel, OverviewRelease};
use crate::wallpaper::Wallpaper;

pub(crate) const MENU_DISMISS_KEYSYM: u32 = 0xff1b;
pub const DESKTOP_BG: (u8, u8, u8) = (128, 129, 159);
pub const DESKTOP_BG_LIGHT: (u8, u8, u8) = (198, 199, 216);

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
    /// The `Omarchy Bar` row: show the hosted shell's bar if it is
    /// hidden, hide it if it is shown (`Desktop::toggle_omarchy_bar`).
    ToggleOmarchyBar,
    /// The stable id of a built-in theme (`wm_theme::default_theme::
    /// CHOICES`) — handled by the shell orchestration (`crate::shell`),
    /// which persists it and reports `ShellOutcome::Restart` so the
    /// backend binary hot-restarts in place to redress every surface
    /// at once.
    SetTheme(&'static str),
    /// A command picked from the Omarchy submenu: `index` into the
    /// flat action list of the `omarchy_menu::OmarchyMenu` model that
    /// was current when the menu opened, identified by `generation`.
    /// The dispatch hands both back to the model, which refuses an
    /// index from a generation it has since rebuilt — a menu opened
    /// before an Omarchy upgrade landed cannot run the command that
    /// now sits at that index.
    OmarchyCommand {
        index: usize,
        generation: u64,
    },
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
    ForceQuitApplication(ClientId),
}

// Menu identifiers are resolved within the owning menu session.
const ACTION_LAUNCH_TERMINAL: u32 = 1;
const ACTION_LAUNCH_ABOUT: u32 = 2;
const ACTION_EXIT: u32 = 3;
/// The `Omarchy Bar` toggle — present only while this session hosts
/// Omarchy's shell (`Desktop::set_omarchy_bar`), marked when the bar
/// is shown.
const ACTION_OMARCHY_BAR: u32 = 4;
const ACTION_WALLPAPER_BASE: u32 = 100;
/// The Wallpaper submenu's Omarchy row — `Wallpaper::Omarchy`, which
/// is not in `Wallpaper::ALL` — in the slot right after the built-ins,
/// the same arrangement as the Theme submenu's follow row below.
/// Offered on the same condition as that row (Omarchy has a palette on
/// this machine), and like it always resolvable.
const ACTION_WALLPAPER_OMARCHY: u32 = ACTION_WALLPAPER_BASE + Wallpaper::ALL.len() as u32;
/// The host's own backgrounds, in the slots after Omarchy's row. Still
/// inside the wallpaper range (100..200), which has ample room: nine
/// built-ins, one Omarchy row and at most `HOST_ART_SLOTS` of these.
const ACTION_WALLPAPER_HOST_BASE: u32 = ACTION_WALLPAPER_OMARCHY + 1;
const ACTION_THEME_BASE: u32 = 200;
/// The Theme submenu's follow-Omarchy row: the slot right after the
/// built-ins, so it lives in the Theme range without displacing the
/// built-ins' indices. Only *offered* when Omarchy has a palette to
/// follow (`root_menu_items` takes its label as an `Option`), but
/// always *resolvable*: `resolve_action` is bounds, not availability,
/// and the shell's `SetTheme` path copes with a palette that has gone.
const ACTION_THEME_OMARCHY: u32 = ACTION_THEME_BASE + wm_theme::default_theme::CHOICES.len() as u32;
const ACTION_WINDOW_MAXIMIZE: u32 = 300;
// The Omarchy row must stay inside the Themes range; a ninth built-in
// would still leave room, a hundredth would not, and this says so at
// compile time rather than as a menu that quietly opens the wrong thing.
const _: () = assert!(ACTION_THEME_OMARCHY < ACTION_WINDOW_MAXIMIZE);
const ACTION_WINDOW_MINIATURIZE: u32 = 301;
const ACTION_WINDOW_SHADE: u32 = 302;
const ACTION_WINDOW_FULLSCREEN: u32 = 303;
const ACTION_WINDOW_CLOSE: u32 = 304;
const ACTION_WINDOW_KILL: u32 = 305;
const ACTION_MOVE_TO_BASE: u32 = 400;
/// Rows in the About submenu. They fire ids that resolve to nothing,
/// which dismisses the menu — the classic behaviour of a menu entry
/// with nothing to do. `MenuItem` has no disabled variant, and adding
/// one to the theme SDK to grey out four lines of diagnostics would be
/// a change to every menu in the desktop for the benefit of this one.
/// An Omarchy row whose `disabled` condition holds — "Vim" in the
/// Install list when vim is installed. Listed with the marker, fires
/// nothing; the row stays so the list reads as a catalogue with the
/// installed items ticked, which is how Omarchy's own menu shows it.
const ACTION_OMARCHY_INERT_ROW: u32 = 600;
const ACTION_APP_BASE: u32 = 1000;
const ACTION_OMARCHY_BASE: u32 = 1_000_000;

/// What the root menu is titled when the host does not say otherwise —
/// also what a fresh `ShellMenu` is titled before any session opens.
///
/// "Omarchy", not "chonkstep", because that is what this desktop *is*
/// to the person using it: a replacement desktop environment for
/// Omarchy, wearing different chrome. The name a user reads on screen
/// is the desktop they installed. Nothing below the surface is
/// renamed — crates, log lines, config paths and the session
/// signature all stay `chonkstep`, because renaming those breaks
/// existing installs and buys nothing anybody sees.
const DEFAULT_ROOT_MENU_TITLE: &str = "Omarchy";

/// The root menu's title: the name of the desktop the person installed,
/// taken from the host's `/etc/os-release`.
///
/// The reasoning above — that the title names the desktop, not the
/// window manager — was never specific to Omarchy; it was only ever
/// hardcoded because Omarchy was the one host. It is not any more, and a
/// menu captioned "Omarchy" on somebody's LCOS desktop is simply wrong.
///
/// `NAME=` is exactly the field for this, and the two hosts spell
/// themselves cleanly: Omarchy sets `NAME="Omarchy"`, LCOS sets
/// `NAME="LCOS"`. So on Omarchy this resolves to the identical string
/// the constant used to hold and nothing there changes at all — which is
/// the point. Read once; an unreadable or empty value falls back.
fn root_menu_title() -> &'static str {
    static TITLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TITLE
        .get_or_init(|| {
            os_release_field("NAME").unwrap_or_else(|| DEFAULT_ROOT_MENU_TITLE.to_string())
        })
        .as_str()
}

/// One field from the host's `/etc/os-release`, unquoted and trimmed;
/// `None` when the file is unreadable or the key is absent or empty.
///
/// The file is read once. `strip_prefix` anchors at the start of the
/// line, so asking for `NAME` cannot accidentally match `PRETTY_NAME=`
/// or `ID` match `ID_LIKE=`.
pub(crate) fn os_release_field(key: &str) -> Option<String> {
    static TEXT: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let text = TEXT.get_or_init(|| std::fs::read_to_string("/etc/os-release").ok()).as_ref()?;
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .map(|value| value.trim().trim_matches('"').trim().to_string())
        .filter(|value| !value.is_empty())
}

/// Mark the active menu choice.
pub(crate) fn bullet_label(selected: bool, label: &str) -> String {
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
/// that, so each cascade is alphabetical for free. "About"
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
        .chain(std::iter::once(MenuItem::Action { label: "About".to_string(), action: ACTION_LAUNCH_ABOUT }))
        .collect()
}

/// What a root menu was built against, and therefore what its fired
/// ids may resolve to: the resolver's bounds. `app_count` is the
/// length of the app index the Applications submenu came from;
/// `omarchy_count` and `omarchy_generation` are the Omarchy model's
/// flat action count and its generation stamp (zero of each when the
/// submenu is absent, which refuses every Omarchy id).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RootMenuBounds {
    pub app_count: usize,
    pub omarchy_count: usize,
    pub omarchy_generation: u64,
}

fn root_menu_items(
    selected_wallpaper: Wallpaper,
    selected_theme_id: &str,
    apps: &[crate::apps::AppEntry],
    follow_omarchy: Option<&str>,
    omarchy: Vec<MenuItem>,
    omarchy_bar: Option<BarVisibility>,
) -> Vec<MenuItem> {
    let wallpaper_items = Wallpaper::ALL
        .into_iter()
        .enumerate()
        .map(|(index, wallpaper)| MenuItem::Action {
            label: bullet_label(wallpaper == selected_wallpaper, wallpaper.label()),
            action: ACTION_WALLPAPER_BASE + index as u32,
        })
        .chain(follow_omarchy.map(|_| MenuItem::Action {
            label: bullet_label(selected_wallpaper == Wallpaper::Omarchy, Wallpaper::Omarchy.label()),
            action: ACTION_WALLPAPER_OMARCHY,
        }))
        // The host's own pictures, when it ships any. Empty everywhere
        // that does, so no row appears and no id is offered.
        .chain((0..crate::wallpaper::host_art().len() as u8).map(|slot| MenuItem::Action {
            label: bullet_label(selected_wallpaper == Wallpaper::HostArt(slot), Wallpaper::HostArt(slot).label()),
            action: ACTION_WALLPAPER_HOST_BASE + slot as u32,
        }))
        .collect();
    let theme_items = wm_theme::default_theme::CHOICES
        .iter()
        .enumerate()
        .map(|(index, (id, label))| MenuItem::Action {
            label: bullet_label(*id == selected_theme_id, label),
            action: ACTION_THEME_BASE + index as u32,
        })
        .chain(follow_omarchy.map(|label| MenuItem::Action {
            label: bullet_label(selected_theme_id == wm_theme::omarchy::ID, label),
            action: ACTION_THEME_OMARCHY,
        }))
        .collect();

    // Mirror Omarchy when available; retain the native Applications menu.
    if !omarchy.is_empty() {
        let mut items = vec![
            MenuItem::Submenu { label: "Applications".to_string(), items: applications_items(apps) },
            MenuItem::Action { label: "Terminal".to_string(), action: ACTION_LAUNCH_TERMINAL },
        ];
        items.extend(omarchy);
        if let Some(bar) = omarchy_bar {
            items.push(MenuItem::Action {
                label: bullet_label(!bar.is_hidden(), "Omarchy Bar"),
                action: ACTION_OMARCHY_BAR,
            });
        }
        items.push(MenuItem::Action { label: "Exit".to_string(), action: ACTION_EXIT });
        return items;
    }

    // No Omarchy to read: this desk's own tree, unchanged. A session on
    // a machine without Omarchy installed still needs a way to pick a
    // theme, a wallpaper and an application, so the rows the branch
    // above drops are exactly the rows that must survive here.
    let mut items = vec![
        MenuItem::Action { label: "Terminal".to_string(), action: ACTION_LAUNCH_TERMINAL },
        MenuItem::Submenu { label: "Applications".to_string(), items: applications_items(apps) },
        MenuItem::Submenu { label: "Theme".to_string(), items: theme_items },
        MenuItem::Submenu { label: "Wallpaper".to_string(), items: wallpaper_items },
    ];
    if let Some(bar) = omarchy_bar {
        items.push(MenuItem::Action {
            label: bullet_label(!bar.is_hidden(), "Omarchy Bar"),
            action: ACTION_OMARCHY_BAR,
        });
    }
    items.push(MenuItem::Action { label: "Exit".to_string(), action: ACTION_EXIT });
    items
}

/// `bounds.app_count` bounds the `ACTION_APP_BASE +` range: the length
/// of the app index the fired menu was built from, so a stale or
/// corrupt id past the vec's end dissolves to `None` instead of
/// indexing out of bounds downstream. `bounds.omarchy_count` does the
/// same for the `ACTION_OMARCHY_BASE +` range.
fn resolve_action(action: u32, bounds: RootMenuBounds) -> Option<RootMenuAction> {
    match action {
        ACTION_LAUNCH_TERMINAL => Some(RootMenuAction::LaunchTerminal),
        ACTION_LAUNCH_ABOUT => Some(RootMenuAction::LaunchAbout),
        ACTION_EXIT => Some(RootMenuAction::Exit),
        ACTION_OMARCHY_BAR => Some(RootMenuAction::ToggleOmarchyBar),
        // Subtraction-then-compare rather than a `Range::contains`:
        // `ACTION_OMARCHY_BASE + omarchy_count as u32` could in
        // principle overflow u32, and the subtraction form has no such
        // edge. Checked before the app range, whose upper bound is
        // this range's base.
        action if action >= ACTION_OMARCHY_BASE && ((action - ACTION_OMARCHY_BASE) as usize) < bounds.omarchy_count => {
            Some(RootMenuAction::OmarchyCommand {
                index: (action - ACTION_OMARCHY_BASE) as usize,
                generation: bounds.omarchy_generation,
            })
        }
        action
            if (ACTION_APP_BASE..ACTION_OMARCHY_BASE).contains(&action)
                && ((action - ACTION_APP_BASE) as usize) < bounds.app_count =>
        {
            Some(RootMenuAction::LaunchApp((action - ACTION_APP_BASE) as usize))
        }
        action if (ACTION_WALLPAPER_BASE..ACTION_WALLPAPER_OMARCHY).contains(&action) => {
            Some(RootMenuAction::SetWallpaper(Wallpaper::ALL[(action - ACTION_WALLPAPER_BASE) as usize]))
        }
        ACTION_WALLPAPER_OMARCHY => Some(RootMenuAction::SetWallpaper(Wallpaper::Omarchy)),
        // Bounds, not availability — the same contract the Omarchy rows
        // keep: a slot that no longer has a file resolves, and the
        // render falls back rather than leaving a bare desk.
        action
            if (ACTION_WALLPAPER_HOST_BASE
                ..ACTION_WALLPAPER_HOST_BASE + crate::wallpaper::HOST_ART_SLOTS as u32)
                .contains(&action) =>
        {
            Some(RootMenuAction::SetWallpaper(Wallpaper::HostArt((action - ACTION_WALLPAPER_HOST_BASE) as u8)))
        }
        action if (ACTION_THEME_BASE..ACTION_THEME_OMARCHY).contains(&action) => {
            Some(RootMenuAction::SetTheme(wm_theme::default_theme::CHOICES[(action - ACTION_THEME_BASE) as usize].0))
        }
        ACTION_THEME_OMARCHY => Some(RootMenuAction::SetTheme(wm_theme::omarchy::ID)),
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
        action if (ACTION_MOVE_TO_BASE..=ACTION_MOVE_TO_BASE + workspace_count as u32).contains(&action) => {
            Some(WindowMenuAction::MoveToWorkspace((action - ACTION_MOVE_TO_BASE) as usize))
        }
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
    ForceQuit { applications: Vec<ClientId> },
    Root {
        /// What the Applications and Omarchy submenus were built
        /// against: the resolver's bounds for mapping `ACTION_APP_BASE
        /// + i` back into `LaunchApp(i)` and `ACTION_OMARCHY_BASE + i`
        /// into `OmarchyCommand` — the root-session twin of the window
        /// session's `workspace_count`.
        bounds: RootMenuBounds,
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
        MenuSession::ForceQuit { applications } => action.checked_sub(1)
            .and_then(|index| applications.get(index as usize)).copied().map(MenuAction::ForceQuitApplication),
        MenuSession::Root { bounds } => resolve_action(action, *bounds).map(MenuAction::Root),
        MenuSession::Window { client, workspace_count } => resolve_window_action(action, *workspace_count)
            .map(|window_action| MenuAction::Window(*client, window_action)),
    }
}

/// The single active root or window menu.
struct ShellMenu<Id> {
    menu: CascadeMenu<Id>,
    session: MenuSession,
}

impl<Id: Copy + Eq + std::fmt::Debug> ShellMenu<Id> {
    fn new() -> Self {
        // Zero bounds before any session opens: with no popup on
        // screen no click can reach the resolver, so the placeholder
        // is never consulted — and zero is the value that would refuse
        // every app and Omarchy id anyway.
        Self {
            menu: CascadeMenu::new(root_menu_title(), DESKTOP_BG),
            session: MenuSession::Root { bounds: RootMenuBounds::default() },
        }
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
    fn begin_session<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        session: MenuSession,
        title: String,
    ) {
        self.menu.close(host);
        let chrome = self.menu.chrome().cloned();
        self.menu = CascadeMenu::new(title, DESKTOP_BG);
        self.menu.set_chrome(chrome);
        self.session = session;
    }

    /// `bounds` must describe the same app index and Omarchy model
    /// `items` was built from (`Desktop::open_root_menu` reads both
    /// from its own stored state) — it becomes the session's bounds
    /// for resolving `ACTION_APP_BASE +` and `ACTION_OMARCHY_BASE +`
    /// ids back into indices.
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
        root_bounds: RootMenuBounds,
        at: Point,
        bounds: Size,
    ) {
        self.begin_session(host, MenuSession::Root { bounds: root_bounds }, root_menu_title().to_string());
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

    fn key<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        key: MenuKey,
    ) -> Option<MenuAction> {
        match self.menu.key(host, theme, font_system, key)? {
            MenuClick::Action(action) => resolve_session_action(&self.session, action),
            MenuClick::OpenedSubmenu | MenuClick::Dismissed => None,
        }
    }

    fn close<H: wm_theme_api::PopupHost<PopupId = Id>>(&mut self, host: &mut H) {
        self.menu.close(host);
    }

    fn is_open(&self) -> bool {
        self.menu.is_open()
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

    fn tick<H: wm_theme_api::PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
    ) {
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


#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EdgeReservation {
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
    pub left: u32,
}

/// Clamp independent edge claims into one nonempty monitor-local area.
fn reserved_area(primary: Rect, reserved: EdgeReservation) -> Rect {
    let left = reserved.left.min(primary.size.w.saturating_sub(1));
    let right = reserved.right.min(primary.size.w.saturating_sub(left).saturating_sub(1));
    let top = reserved.top.min(primary.size.h.saturating_sub(1));
    let bottom = reserved.bottom.min(primary.size.h.saturating_sub(top).saturating_sub(1));
    Rect::new(Point::new(primary.pos.x.saturating_add(left as i32), primary.pos.y.saturating_add(top as i32)),
        Size::new(primary.size.w.saturating_sub(left + right).max(1), primary.size.h.saturating_sub(top + bottom).max(1)))
}


pub(crate) fn switcher_preview_px(scale: f32) -> u32 {
    ((56.0 * scale).round() as u32).max(16)
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
    screen: Size,
    primary: Rect,
    reserved: EdgeReservation,
    tile: u32,
    scale: f32,
    fonts: wm_theme::FontState,
    chrome: wm_theme::UiChrome,
    menu: ShellMenu<B::ShellId>,
    wallpaper: Wallpaper,
    wallpaper_drawn: Cell<Option<(Wallpaper, Size, wm_theme::Appearance)>>,
    appearance: wm_theme::Appearance,
    switcher: Option<SwitcherPanel<B::ShellId>>,
    overview: OverviewPanel<B>,
    escape_key_grabbed: bool,
    theme_id: String,
    apps: Vec<crate::apps::AppEntry>,
    omarchy: Option<crate::omarchy_menu::OmarchyMenu>,
    omarchy_bar: Option<BarVisibility>,
}

impl<B: Backend> Desktop<B> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backend: &mut B, screen: Size, primary: Rect, scale: f32,
        theme: &Theme, appearance: wm_theme::Appearance,
        apps: Vec<crate::apps::AppEntry>, fonts: wm_theme::FontState,
    ) -> Self {
        let desktop = Self {
            screen, primary, scale, reserved: EdgeReservation::default(),
            tile: switcher_preview_px(scale),
            chrome: wm_theme::UiChrome::new(theme, fonts.clone(), wm_theme::DecorationStyle::WindowMaker, scale),
            fonts, menu: ShellMenu::new(),
            wallpaper: Wallpaper::load_or(&theme.wallpaper),
            wallpaper_drawn: Cell::new(None), appearance,
            switcher: None, overview: OverviewPanel::default(),
            escape_key_grabbed: false, theme_id: theme.id.clone(), apps,
            omarchy: None, omarchy_bar: None,
        };
        desktop.repaint_wallpaper(backend);
        desktop
    }

    pub fn workareas(&self, monitors: &[Rect]) -> Vec<Rect> {
        workareas_for(monitors, self.primary, self.primary_workarea())
    }

    pub fn primary_workarea(&self) -> Rect {
        reserved_area(self.primary, self.reserved)
    }

    pub fn set_reservation(&mut self, reserved: EdgeReservation) -> bool {
        if reserved == self.reserved { return false; }
        self.reserved = reserved;
        true
    }

    fn screen_size(&self) -> Size {
        self.screen
    }

    pub fn resize_to_screen(&mut self, backend: &mut B, screen: Size, primary: Rect) {
        self.screen = screen;
        self.primary = primary;
        self.repaint_wallpaper(backend);
        self.discard_switcher(backend);
        self.overview.discard(backend);
    }

    pub fn set_scale(&mut self, scale: f32) -> bool {
        if self.scale.to_bits() == scale.to_bits() { return false; }
        self.scale = scale;
        self.tile = switcher_preview_px(scale);
        true
    }

    pub fn set_appearance(&mut self, appearance: wm_theme::Appearance) {
        self.appearance = appearance;
    }

    pub fn set_chrome(&mut self, backend: &mut B, theme: &Theme, style: wm_theme::DecorationStyle, scale: f32)
    where B: wm_theme_api::PopupHost<PopupId = B::ShellId> {
        self.close_menu(backend);
        self.chrome = wm_theme::UiChrome::new(theme, self.fonts.clone(), style, scale);
        self.menu.menu.set_chrome(Some(self.chrome.clone()));
        self.overview.set_chrome(self.chrome.clone());
    }

    pub fn chrome(&self) -> &wm_theme::UiChrome { &self.chrome }

    pub fn set_theme_id(&mut self, id: String) {
        self.theme_id = id;
    }

    pub fn follow_theme_wallpaper(&mut self, previous: &str, next: &str) {
        let wallpaper = self.wallpaper.following_theme(previous, next);
        if wallpaper != self.wallpaper {
            self.wallpaper = wallpaper;
            if let Err(error) = wallpaper.persist() {
                tracing::warn!(?error, "failed to remember the new theme's wallpaper");
            }
        }
    }

    pub fn relayout(&mut self, backend: &mut B) {
        self.repaint_wallpaper(backend);
        self.discard_switcher(backend);
        self.overview.discard(backend);
    }

    fn discard_switcher(&mut self, backend: &mut B) {
        if let Some(panel) = self.switcher.take() {
            if let Some(window) = panel.window {
                backend.destroy_shell_surface(window);
            }
        }
    }

    pub fn next_housekeeping_deadline(&self) -> Option<std::time::Instant> {
        self.menu.menu.next_deadline()
    }

    fn sync_escape_key_grab(&mut self, backend: &mut B) {
        let wanted = self.menu.is_open();
        if wanted == self.escape_key_grabbed {
            return;
        }
        let combo = wm_core::KeyCombo { keysym: MENU_DISMISS_KEYSYM, modifiers: wm_core::Modifiers::empty() };
        if wanted {
            backend.grab_key(combo);
        } else {
            backend.ungrab_key(combo);
        }
        self.escape_key_grabbed = wanted;
    }

    pub fn open_root_menu(&mut self, backend: &mut B, theme: &Theme, at: Point)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let bounds = self.screen_size();
        let omarchy_items = self
            .omarchy
            .as_ref()
            .map(|menu| menu.items(ACTION_OMARCHY_BASE, ACTION_OMARCHY_INERT_ROW))
            .unwrap_or_default();
        let root_bounds = RootMenuBounds {
            app_count: self.apps.len(),
            omarchy_count: self.omarchy.as_ref().map_or(0, |menu| menu.action_count()),
            omarchy_generation: self.omarchy.as_ref().map_or(0, |menu| menu.generation()),
        };
        // Omarchy's presence is checked when the menu opens, not cached:
        // the row must appear the moment `omarchy-theme-set` first runs
        // and vanish if Omarchy is removed, and a menu open is a user
        // gesture that can afford two file reads.
        let follow_omarchy = wm_theme::omarchy::is_available().then(|| {
            wm_theme::omarchy::display_name(
                &wm_theme::omarchy::current_theme_name().map(|n| wm_theme::omarchy::title_case(&n)).unwrap_or_default(),
            )
        });
        let items = root_menu_items(
            self.wallpaper,
            &self.theme_id,
            &self.apps,
            follow_omarchy.as_deref(),
            omarchy_items,
            self.omarchy_bar,
        );
        self.menu.open_root(backend, theme, &mut self.fonts.system(), items, root_bounds, at, bounds);
        self.sync_escape_key_grab(backend);
    }

    pub fn open_force_quit_menu(&mut self, backend: &mut B, theme: &Theme, applications: Vec<(ClientId, String)>)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let bounds = self.screen_size();
        let mut items = vec![MenuItem::Action { label: "Cancel".into(), action: 0 }];
        for (index, (_, name)) in applications.iter().enumerate() {
            items.push(MenuItem::Submenu { label: window_menu_title(name), items: vec![
                MenuItem::Action { label: "Cancel".into(), action: 0 },
                MenuItem::Action { label: "Force Quit — discard unsaved changes".into(), action: index as u32 + 1 },
            ] });
        }
        let session = MenuSession::ForceQuit { applications: applications.into_iter().map(|(id, _)| id).collect() };
        self.menu.begin_session(backend, session, "Force Quit Applications".into());
        let at = Point::new((bounds.w / 3) as i32, (bounds.h / 4) as i32);
        self.menu.menu.open(backend, theme, &mut self.fonts.system(), items, at, bounds, false);
        self.sync_escape_key_grab(backend);
    }

    pub fn set_omarchy_bar(&mut self, backend: &mut B, bar: Option<BarVisibility>) {
        self.omarchy_bar = bar;
        let hidden = bar.is_some_and(BarVisibility::is_hidden);
        backend.set_layer_surface_hidden(crate::omarchy_shell::BAR_NAMESPACE, hidden);
    }

    pub fn toggle_omarchy_bar(&mut self, backend: &mut B) {
        let Some(bar) = self.omarchy_bar else {
            return;
        };
        let next = bar.toggled();
        if let Err(e) = next.persist() {
            tracing::warn!(?e, "failed to persist the Omarchy bar choice");
        }
        tracing::info!(bar = next.id(), "Omarchy bar toggled");
        self.set_omarchy_bar(backend, Some(next));
    }

    pub fn apps(&self) -> &[crate::apps::AppEntry] {
        &self.apps
    }

    pub fn set_omarchy_menu(&mut self, menu: Option<crate::omarchy_menu::OmarchyMenu>) {
        self.omarchy = menu;
    }

    pub fn omarchy_command(&self, index: usize, generation: u64) -> Option<String> {
        self.omarchy.as_ref().and_then(|menu| menu.command(generation, index)).map(str::to_string)
    }

    pub fn note_omarchy_action_fired(&mut self) {
        if let Some(menu) = self.omarchy.as_mut() {
            menu.note_action_fired(std::time::Instant::now());
        }
    }

    pub fn open_window_menu(&mut self, backend: &mut B, theme: &Theme, at: Point, ctx: WindowMenuContext)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let bounds = self.screen_size();
        self.menu.open_window(backend, theme, &mut self.fonts.system(), &ctx, at, bounds);
        self.sync_escape_key_grab(backend);
    }

    pub fn set_wallpaper(&mut self, backend: &mut B, wallpaper: Wallpaper) {
        self.wallpaper = wallpaper;
        if let Err(error) = wallpaper.persist() {
            tracing::warn!(?error, wallpaper = wallpaper.label(), "failed to remember wallpaper selection");
        }
        self.repaint_wallpaper(backend);
    }

    pub fn refresh_wallpaper(&self, backend: &mut B) {
        if self.wallpaper == Wallpaper::Omarchy {
            self.repaint_wallpaper(backend);
        }
    }

    fn repaint_wallpaper(&self, backend: &mut B) {
        let key = (self.wallpaper, self.screen_size(), self.appearance);
        if self.wallpaper != Wallpaper::Omarchy && self.wallpaper_drawn.get() == Some(key) {
            return;
        }
        match self.wallpaper.render(self.screen_size(), self.appearance) {
            Some(buffer) => backend.paint_root_image(&buffer),
            // The solid-color artwork (and any artwork that fails to
            // decode) falls back to its own quiet color in the current
            // mood, so even the fallback follows the axis.
            None => backend.paint_root_color(self.wallpaper.background_color(self.appearance)),
        }
        self.wallpaper_drawn.set(Some(key));
    }

    pub fn show_switcher(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        entries: Option<Vec<SwitcherEntry>>,
        selected: usize,
    ) {
        match (entries, self.switcher.as_mut()) {
            (Some(new_entries), Some(panel)) => panel.entries = new_entries,
            (Some(new_entries), None) => {
                self.switcher =
                    Some(SwitcherPanel { window: None, size: Size::new(0, 0), entries: new_entries, visible: false });
            }
            (None, _) => {}
        }
        let Self { switcher, fonts, tile, primary, chrome, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        let Some(panel) = switcher.as_mut() else {
            return;
        };
        let buffer =
            chrome.switcher(theme, &mut font_system, &mut swash_cache, &panel.entries, selected, *tile);
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

    pub fn switcher_entry_count(&self) -> Option<usize> {
        self.switcher.as_ref().filter(|panel| panel.visible).map(|panel| panel.entries.len())
    }

    pub fn hide_switcher(&mut self, backend: &mut B) {
        if let Some(panel) = self.switcher.as_mut() {
            if let Some(window) = panel.window {
                backend.unmap_shell_surface(window);
                backend.release_shell_buffer(window);
            }
            panel.entries.clear();
            panel.visible = false;
        }
    }

    pub fn show_overview(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        items: Vec<OverviewItem<B>>,
        workspace: (usize, usize),
        workspace_windows: Vec<Vec<wm_core::OverviewThumbnail<B::WindowId, B::FrameId>>>,
        selection: (usize, Option<Rect>),
    ) {
        let (selected, area) = selection;
        let area = area.unwrap_or(self.primary);
        let stage = self.overview_stage(area);
        let Self { overview, fonts, tile, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        overview.show(backend, theme, &mut font_system, &mut swash_cache, area, stage, *tile, items, workspace, workspace_windows, selected);
    }

    pub(crate) fn overview_stage(&self, area: Rect) -> Rect {
        if area == self.primary { reserved_area(area, self.reserved) } else { area }
    }

    pub fn overview_visible(&self) -> bool {
        self.overview.visible()
    }

    pub fn overview_owns(&self, surface: B::ShellId) -> bool {
        self.overview.owns(surface)
    }

    pub fn overview_panel_point(&self, surface: B::ShellId, local: Point) -> Point {
        self.overview.panel_point(surface, local)
    }

    pub fn overview_wants_fresh_previews(&self, generation: u64) -> bool {
        self.overview.wants_fresh_previews(generation)
    }

    pub fn overview_clients(&self) -> Vec<ClientId> {
        self.overview.clients()
    }

    pub fn update_overview_previews(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        previews: Vec<Option<DecorationBuffer>>,
        generation: u64,
    ) {
        let Self { overview, fonts, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        overview.update_previews(backend, theme, &mut font_system, &mut swash_cache, previews, generation);
    }

    pub fn overview_hit(&self, backend: &B, local: Point) -> OverviewHit {
        self.overview.hit(backend, local)
    }

    pub fn overview_selected(&self) -> usize {
        self.overview.selected()
    }

    pub fn overview_workspace(&self) -> (usize, usize) { self.overview.workspace() }

    pub fn overview_pointer_pending(&self) -> bool { self.overview.pointer_pending() }

    pub fn overview_pointer_press(&mut self, backend: &mut B, index: usize, local: Point) {
        self.overview.pointer_press(backend, index, local);
    }

    pub fn overview_pointer_motion(&mut self, backend: &mut B, theme: &Theme, root: Point) -> bool {
        let Self { overview, fonts, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        overview.pointer_motion(backend, theme, &mut font_system, &mut swash_cache, root)
    }

    pub fn overview_pointer_release(&mut self, backend: &mut B, theme: &Theme, local: Point) -> Option<OverviewRelease> {
        let Self { overview, fonts, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        overview.pointer_release(backend, theme, &mut font_system, &mut swash_cache, local)
    }

    pub fn cancel_overview_pointer(&mut self, backend: &mut B, theme: &Theme) -> bool {
        let Self { overview, fonts, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        overview.cancel_pointer(backend, theme, &mut font_system, &mut swash_cache)
    }

    pub fn overview_item(&self, index: usize) -> Option<&OverviewItem<B>> {
        self.overview.item(index)
    }

    pub fn select_overview_card(&mut self, backend: &mut B, theme: &Theme, index: usize) {
        let Self { overview, fonts, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        overview.select(backend, theme, &mut font_system, &mut swash_cache, index);
    }

    pub fn move_overview_selection(&mut self, backend: &mut B, theme: &Theme, dx: i32, dy: i32) {
        let Self { overview, fonts, .. } = self;
        let (mut font_system, mut swash_cache) = (fonts.system(), fonts.swash());
        overview.move_selection(backend, theme, &mut font_system, &mut swash_cache, dx, dy);
    }

    pub fn hide_overview(&mut self, backend: &mut B) {
        self.overview.hide(backend);
    }

    pub fn close_menu(&mut self, backend: &mut B)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        self.menu.close(backend);
        self.sync_escape_key_grab(backend);
    }

    pub fn menu_visible(&self) -> bool {
        self.menu.is_open()
    }

    pub fn click_menu(&mut self, backend: &mut B, theme: &Theme, window: B::ShellId, local: Point) -> Option<MenuAction>
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let action = self.menu.click(backend, theme, &mut self.fonts.system(), window, local);
        self.sync_escape_key_grab(backend);
        action
    }

    pub fn key_menu(&mut self, backend: &mut B, theme: &Theme, key: MenuKey) -> Option<MenuAction>
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        let action = self.menu.key(backend, theme, &mut self.fonts.system(), key);
        self.sync_escape_key_grab(backend);
        action
    }

    pub fn hover_menu(&mut self, backend: &mut B, theme: &Theme, window: B::ShellId, local: Point)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        self.menu.hover(backend, theme, &mut self.fonts.system(), window, local);
    }

    pub fn tick_menu(&mut self, backend: &mut B, theme: &Theme)
    where
        B: wm_theme_api::PopupHost<PopupId = B::ShellId>,
    {
        self.menu.tick(backend, theme, &mut self.fonts.system());
        // The Omarchy source's own housekeeping — the mtime poll and
        // the condition batch — rides the same once-per-iteration
        // cadence. Both are non-blocking; the batch runs on its thread.
        if let Some(omarchy) = self.omarchy.as_mut() {
            omarchy.tick(std::time::Instant::now());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::{AppCategory, AppEntry};
    #[test]
    fn every_theme_starts_without_persistent_shell_surfaces_or_reserved_columns() {
        use wm_core::fake_backend::FakeBackend;
        let fonts = wm_theme::FontState::new();
        let primary = Rect::new(Point::new(640, 40), Size::new(640, 480));
        for theme in wm_theme::default_theme::all_themes() {
            let mut backend = FakeBackend::new();
            let mut desktop = Desktop::new(&mut backend, Size::new(1280, 520), primary,
                1.5, &theme, wm_theme::Appearance::Dark, vec![], fonts.clone());
            assert_eq!(backend.next_shell_id, 0, "{} creates persistent surfaces", theme.id);
            assert_eq!(desktop.primary_workarea(), primary);
            let reservation = EdgeReservation { top: 32, right: 16, bottom: 20, left: 8 };
            assert!(desktop.set_reservation(reservation));
            assert_eq!(desktop.primary_workarea(), Rect::new(Point::new(648, 72), Size::new(616, 428)));
            desktop.set_scale(2.0);
            desktop.relayout(&mut backend);
            assert_eq!(backend.next_shell_id, 0, "{} recreates retired chrome on reload", theme.id);
            assert_eq!(desktop.next_housekeeping_deadline(), None);
        }
    }

    const TEST_SCREEN: Size = Size { w: 640, h: 480 };
    fn test_theme() -> wm_theme::Theme {
        wm_theme::default_theme::theme_variant("nextstep-classic", wm_theme::Appearance::Dark)
            .expect("the flagship theme exists")
    }
    #[test]
    fn unchanged_embedded_wallpaper_is_not_decoded_or_uploaded_on_restyle() {
        use wm_core::fake_backend::FakeBackend;

        let theme = test_theme();
        let primary = Rect { pos: Point::new(0, 0), size: TEST_SCREEN };
        let mut backend = FakeBackend::new();
        let mut desktop = Desktop::new(&mut backend, TEST_SCREEN, primary, 1.0,
            &theme, wm_theme::Appearance::Dark, Vec::new(), wm_theme::FontState::new());
        // Independent of any wallpaper selected in this developer's
        // state directory, which Desktop::new normally restores.
        desktop.wallpaper = Wallpaper::LavenderGrid;
        desktop.repaint_wallpaper(&mut backend);
        let painted = backend.root_paint_count;
        for _ in 0..5 {
            desktop.relayout(&mut backend);
        }
        desktop.set_scale(2.0);
        desktop.relayout(&mut backend);
        assert_eq!(backend.root_paint_count, painted, "unchanged root pixels remain backend-owned");

        desktop.set_appearance(wm_theme::Appearance::Light);
        desktop.relayout(&mut backend);
        assert_eq!(backend.root_paint_count, painted + 1, "appearance selects different pixels");
        let larger = Size::new(TEST_SCREEN.w + 20, TEST_SCREEN.h);
        desktop.resize_to_screen(&mut backend, larger, Rect { size: larger, ..primary });
        assert_eq!(backend.root_paint_count, painted + 2, "output extent requires a new cover image");
        desktop.wallpaper = Wallpaper::ClassicLavender;
        desktop.repaint_wallpaper(&mut backend);
        assert_eq!(backend.root_paint_count, painted + 3, "artwork selection is part of the identity");
        desktop.repaint_wallpaper(&mut backend);
        assert_eq!(backend.root_paint_count, painted + 3, "solid backgrounds are reusable too");
    }
    #[test]
    fn wallpaper_actions_resolve_to_every_built_in_wallpaper() {
        for (index, wallpaper) in Wallpaper::ALL.into_iter().enumerate() {
            assert!(matches!(
                resolve_action(ACTION_WALLPAPER_BASE + index as u32, RootMenuBounds::default()),
                Some(RootMenuAction::SetWallpaper(resolved)) if resolved == wallpaper
            ));
        }
    }

    fn wallpaper_submenu(selected: Wallpaper, omarchy: Option<&str>) -> Vec<MenuItem> {
        let items =
            root_menu_items(selected, "nextstep-classic", &[], omarchy, Vec::new(), None);
        let submenu = items.iter().find(|item| item.label() == "Wallpaper").expect("wallpaper submenu");
        let MenuItem::Submenu { items, .. } = submenu else { panic!("expected submenu") };
        items.clone()
    }

    /// The bar row is offered exactly when a shell is hosted, sits
    /// among chonkstep's own rows (before the Omarchy submenu and
    /// Exit), is marked when the bar is shown, and resolves to the
    /// toggle.
    #[test]
    fn the_omarchy_bar_row_comes_with_a_hosted_shell_and_marks_a_shown_bar() {
        let omarchy = vec![MenuItem::Action { label: "Row".to_string(), action: ACTION_OMARCHY_BASE }];
        let labels = |bar: Option<BarVisibility>| -> Vec<String> {
            root_menu_items(
                Wallpaper::DEFAULT,
                "nextstep-classic",
                &[],
                None,
                omarchy.clone(),
                bar,
            )
            .iter()
            .map(|item| item.label().to_string())
            .collect()
        };
        assert!(labels(None).iter().all(|label| !label.contains("Omarchy Bar")), "no shell, no row");
        let hidden = labels(Some(BarVisibility::Hidden));
        let row = hidden.iter().position(|label| label == "  Omarchy Bar").expect("an unmarked row for a hidden bar");
        // This desk's own furniture sits after Omarchy's rows and
        // before the one row that ends the session.
        assert_eq!(hidden[row + 1], "Exit", "and Exit closes the menu");
        assert_eq!(hidden.last().unwrap(), "Exit");
        assert!(labels(Some(BarVisibility::Shown)).contains(&"\u{2022} Omarchy Bar".to_string()), "marked when shown");
        assert!(matches!(
            resolve_action(ACTION_OMARCHY_BAR, RootMenuBounds::default()),
            Some(RootMenuAction::ToggleOmarchyBar)
        ));
    }

    /// The choice reaches the compositor as the bar's namespace, hidden
    /// or not — and a desk that stops hosting a shell un-hides it, so a
    /// bar the user starts by other means is never silently invisible.
    #[test]
    fn the_omarchy_bar_choice_is_applied_to_the_compositor_and_cleared_with_the_shell() {
        use wm_core::fake_backend::FakeBackend;
        let primary = Rect { pos: Point::new(0, 0), size: TEST_SCREEN };
        let mut backend = FakeBackend::new();
        let mut desktop: Desktop<FakeBackend> = Desktop::new(
            &mut backend,
            TEST_SCREEN,
            primary,
            1.0,
            &test_theme(),
            wm_theme::Appearance::Dark,
            Vec::new(),
            wm_theme::FontState::new(),
        );
        backend.layer_visibility_calls.clear();

        desktop.set_omarchy_bar(&mut backend, Some(BarVisibility::Hidden));
        desktop.toggle_omarchy_bar(&mut backend);
        desktop.set_omarchy_bar(&mut backend, None);
        let bar = crate::omarchy_shell::BAR_NAMESPACE.to_string();
        assert_eq!(
            backend.layer_visibility_calls,
            vec![(bar.clone(), true), (bar.clone(), false), (bar, false)],
            "hidden by default, shown by the toggle, and cleared when no shell is hosted"
        );
        // With no shell hosted the toggle is inert: a stale menu id must
        // not hide anything.
        backend.layer_visibility_calls.clear();
        desktop.toggle_omarchy_bar(&mut backend);
        assert!(backend.layer_visibility_calls.is_empty());
    }

    #[test]
    fn wallpaper_submenu_marks_the_current_selection() {
        let items = wallpaper_submenu(Wallpaper::TealBlueprint, None);
        assert_eq!(items.len(), Wallpaper::ALL.len());
        assert!(items.iter().any(|item| item.label() == "\u{2022} Teal Blueprint"));
    }

    /// Omarchy's background is offered exactly when Omarchy is here to
    /// have one, after the built-ins, and its row resolves to the
    /// variant that reads Omarchy's link.
    #[test]
    fn omarchys_background_is_a_wallpaper_row_only_on_a_desk_with_omarchy() {
        let without = wallpaper_submenu(Wallpaper::Omarchy, None);
        assert_eq!(without.len(), Wallpaper::ALL.len(), "no Omarchy, no row");
        assert!(without.iter().all(|item| !item.label().contains("Omarchy")));

        let with = wallpaper_submenu(Wallpaper::Omarchy, Some("Omarchy (Tokyo Night)"));
        assert_eq!(with.len(), Wallpaper::ALL.len() + 1);
        assert_eq!(
            with.last().unwrap().label(),
            "\u{2022} Omarchy's Background",
            "offered last, and marked when current"
        );
        let MenuItem::Action { action, .. } = with.last().unwrap() else { panic!("expected an action row") };
        assert!(matches!(
            resolve_action(*action, RootMenuBounds::default()),
            Some(RootMenuAction::SetWallpaper(Wallpaper::Omarchy))
        ));

        let unmarked = wallpaper_submenu(Wallpaper::TealBlueprint, Some("Omarchy"));
        assert_eq!(unmarked.last().unwrap().label(), "  Omarchy's Background");
        assert_eq!(
            unmarked.iter().filter(|item| item.label().starts_with('\u{2022}')).count(),
            1,
            "one bullet in the submenu"
        );
    }

    fn theme_submenu(selected: &str, omarchy: Option<&str>) -> Vec<MenuItem> {
        let items =
            root_menu_items(Wallpaper::TealBlueprint, selected, &[], omarchy, Vec::new(), None);
        let submenu = items.iter().find(|item| item.label() == "Theme").expect("theme submenu");
        let MenuItem::Submenu { items, .. } = submenu else { panic!("expected submenu") };
        items.clone()
    }

    #[test]
    fn theme_submenu_offers_omarchy_only_when_it_has_a_palette_to_follow() {
        let without = theme_submenu("nextstep-classic", None);
        assert_eq!(without.len(), wm_theme::default_theme::CHOICES.len());
        assert!(!without.iter().any(|item| item.label().contains("Omarchy")));

        let with = theme_submenu("nextstep-classic", Some("Omarchy (Tokyo Night)"));
        assert_eq!(with.len(), wm_theme::default_theme::CHOICES.len() + 1);
        let row = with.last().unwrap();
        assert_eq!(
            row.label(),
            "  Omarchy (Tokyo Night)",
            "labelled with the current Omarchy theme, unbulleted while not following"
        );
        assert!(matches!(row, MenuItem::Action { action, .. } if *action == ACTION_THEME_OMARCHY));
    }

    #[test]
    fn theme_submenu_bullets_omarchy_while_following_and_no_built_in() {
        let items = theme_submenu(wm_theme::omarchy::ID, Some("Omarchy (Rose Pine)"));
        assert_eq!(items.last().unwrap().label(), "\u{2022} Omarchy (Rose Pine)");
        assert_eq!(items.iter().filter(|item| item.label().starts_with('\u{2022}')).count(), 1, "exactly one bullet");
    }

    #[test]
    fn the_omarchy_theme_action_resolves_and_the_slot_after_it_does_not() {
        assert!(matches!(
            resolve_action(ACTION_THEME_OMARCHY, RootMenuBounds::default()),
            Some(RootMenuAction::SetTheme(wm_theme::omarchy::ID))
        ));
        assert!(
            resolve_action(ACTION_THEME_OMARCHY + 1, RootMenuBounds::default()).is_none(),
            "the Theme range ends at the Omarchy row"
        );
        // The built-ins keep their slots below it.
        for (index, (id, _)) in wm_theme::default_theme::CHOICES.iter().enumerate() {
            assert!(
                matches!(resolve_action(ACTION_THEME_BASE + index as u32, RootMenuBounds::default()), Some(RootMenuAction::SetTheme(resolved)) if resolved == *id)
            );
        }
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
        let items = root_menu_items(
            Wallpaper::TealBlueprint,
            "nextstep-classic",
            apps,
            None,
            Vec::new(),
            None,
        );
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
        assert_eq!(labels, ["Development", "Graphics", "Internet", "About"]);

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
            let MenuItem::Submenu { items, .. } = cascade else {
                continue;
            };
            for item in items {
                let MenuItem::Action { label, action } = item else { panic!("app rows are actions") };
                let Some(RootMenuAction::LaunchApp(index)) = resolve_action(*action, apps_only(apps.len())) else {
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
        assert!(resolve_action(ACTION_APP_BASE + apps.len() as u32, apps_only(apps.len())).is_none());
        assert!(resolve_action(ACTION_APP_BASE, RootMenuBounds::default()).is_none());
        assert!(resolve_action(u32::MAX, apps_only(apps.len())).is_none());
    }

    /// Bounds for a root menu with an app index and no Omarchy submenu.
    fn apps_only(app_count: usize) -> RootMenuBounds {
        RootMenuBounds { app_count, ..RootMenuBounds::default() }
    }

    #[test]
    fn omarchy_ids_resolve_within_their_count_and_carry_the_generation() {
        let bounds = RootMenuBounds { app_count: 4, omarchy_count: 3, omarchy_generation: 7 };
        assert!(matches!(
            resolve_action(ACTION_OMARCHY_BASE, bounds),
            Some(RootMenuAction::OmarchyCommand { index: 0, generation: 7 })
        ));
        assert!(matches!(
            resolve_action(ACTION_OMARCHY_BASE + 2, bounds),
            Some(RootMenuAction::OmarchyCommand { index: 2, generation: 7 })
        ));
        assert!(resolve_action(ACTION_OMARCHY_BASE + 3, bounds).is_none());
        assert!(resolve_action(ACTION_OMARCHY_BASE, apps_only(4)).is_none());
        // The inert row an installed `disabled` entry fires means
        // nothing: the pick dismisses the menu.
        assert!(resolve_action(ACTION_OMARCHY_INERT_ROW, bounds).is_none());
    }

    #[test]
    fn the_app_range_stops_where_the_omarchy_range_begins() {
        // An app count large enough to reach past `ACTION_OMARCHY_BASE`
        // is not a real index, and an id in the Omarchy range must
        // never come back as an app even against one: the two
        // open-ended ranges are kept disjoint by the app arm's ceiling.
        let huge = RootMenuBounds { app_count: usize::MAX, omarchy_count: 0, omarchy_generation: 0 };
        assert!(matches!(resolve_action(ACTION_OMARCHY_BASE - 1, huge), Some(RootMenuAction::LaunchApp(_))));
        assert!(resolve_action(ACTION_OMARCHY_BASE, huge).is_none());
        assert!(resolve_action(ACTION_OMARCHY_BASE + 5, huge).is_none());
    }

    #[test]
    fn the_root_menu_is_omarchys_tree_when_there_is_one_and_this_desks_own_when_there_is_not() {
        // No Omarchy to read: this desk's own tree, and it must still
        // carry the rows a user needs to dress the desktop, because
        // nothing else on the machine offers them.
        let without = root_menu_items(
            Wallpaper::TealBlueprint,
            "nextstep-classic",
            &[],
            None,
            Vec::new(),
            None,
        );
        let labels: Vec<&str> = without.iter().map(MenuItem::label).collect();
        assert_eq!(labels, ["Terminal", "Applications", "Theme", "Wallpaper", "Exit"]);

        // With Omarchy present its rows are the menu, at the top level
        // rather than behind an `Omarchy` cascade.
        let rows = vec![
            MenuItem::Submenu { label: "Style".to_string(), items: Vec::new() },
            MenuItem::Submenu { label: "System".to_string(), items: Vec::new() },
        ];
        let with =
            root_menu_items(Wallpaper::TealBlueprint, "nextstep-classic", &[], None, rows, None);
        let labels: Vec<&str> = with.iter().map(MenuItem::label).collect();
        assert_eq!(
            labels,
            ["Applications", "Terminal", "Style", "System", "Exit"],
            "Omarchy's rows are the menu; this desk folds Applications in ahead of them and its own toggles after"
        );
        assert!(
            !labels.contains(&"Omarchy"),
            "no `Omarchy` wrapper: the menu is Omarchy's, it does not contain one"
        );
        // The second theme list is gone. `style.theme` survives the
        // model's filter and drives `omarchy-theme-set`, which this
        // compositor already follows.
        assert!(
            !labels.contains(&"Theme") && !labels.contains(&"Wallpaper"),
            "one theme system: chonkstep's built-ins reach Omarchy's picker by export, not by a menu of their own"
        );
    }

    #[test]
    fn an_empty_app_index_leaves_applications_as_just_about() {
        let applications = applications_submenu(&[]);
        let labels: Vec<&str> = applications.iter().map(|item| item.label()).collect();
        assert_eq!(labels, ["About"], "no empty category cascades, About still reachable");
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
            assert_eq!(resolve_window_action(*action, 3), Some(WindowMenuAction::MoveToWorkspace(index)),);
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
        let root_session = MenuSession::Root { bounds: apps_only(3) };

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
            let items = root_menu_items(
                Wallpaper::TealBlueprint,
                "nextstep-classic",
                &[],
                None,
                Vec::new(),
                None,
            );
            self.menu.open_root(
                &mut self.host,
                &self.theme,
                &mut self.font_system,
                items,
                RootMenuBounds::default(),
                Point::new(0, 0),
                Size::new(1600, 1000),
            );
        }

        fn open_window(&mut self, ctx: &WindowMenuContext) {
            self.menu.open_window(
                &mut self.host,
                &self.theme,
                &mut self.font_system,
                ctx,
                Point::new(0, 0),
                Size::new(1600, 1000),
            );
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

        fn close(&mut self) {
            self.menu.close(&mut self.host);
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
        assert!(matches!(f.click(window, row), Some(MenuAction::Window(_, WindowMenuAction::ToggleMaximize))));
        assert!(f.host.open.is_empty(), "firing an action closes the session");
    }

    #[test]
    fn reopening_the_root_menu_after_a_window_session_restores_root_resolution() {
        let mut f = MenuFixture::new();
        f.open_window(&window_ctx(0, 1));
        f.open_root();
        assert_eq!(f.host.open.len(), 1);

        let window = f.only_open_window();
        let items = root_menu_items(
            Wallpaper::TealBlueprint,
            "nextstep-classic",
            &[],
            None,
            Vec::new(),
            None,
        );
        let row = f.row_point(root_menu_title(), &items, 0);
        assert!(matches!(f.click(window, row), Some(MenuAction::Root(RootMenuAction::LaunchTerminal))));
    }

    #[test]
    fn closing_a_root_menu_tears_down_its_entire_open_cascade() {
        let mut f = MenuFixture::new();
        f.open_root();
        let root = f.only_open_window();
        let items = root_menu_items(
            Wallpaper::TealBlueprint,
            "nextstep-classic",
            &[],
            None,
            Vec::new(),
            None,
        );
        let applications = f.row_point(root_menu_title(), &items, 1);
        assert!(f.click(root, applications).is_none());
        assert_eq!(f.host.open.len(), 2, "the Applications cascade opened");
        assert!(f.menu.is_open());

        f.close();

        assert!(f.host.open.is_empty(), "Escape's close path removes every cascade level");
        assert!(!f.menu.is_open());
        assert_eq!(f.host.ungrabs, 1, "the menu's pointer grab is released exactly once");
    }

    // ------------------------------------------------------------------
    // The built-in instrument panel.

}
