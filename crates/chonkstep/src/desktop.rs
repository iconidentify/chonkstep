//! The desktop shell: a content-sized Dock (an identity tile plus its
//! widgets, WindowMaker-style, at the top-right of the screen), the
//! right-click root menu, and icon tiles for miniaturized windows.
//! None of these are "clients" from `wm-core`'s perspective — they're
//! unmanaged X11 windows the shell owns and draws directly with
//! `wm-theme`'s public `paint` primitives and its `menu`/`clock`/`icon`
//! renderers — the same SDK surface a third-party `chonk-ui` app draws
//! with, so the shell has no rendering code a real app couldn't also use.

use std::collections::HashMap;

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use wm_core::{Backend, ClientId, DragHandle};
use wm_theme::cascade::{CascadeMenu, MenuClick};
use wm_theme::menu::MenuItem;
use wm_theme::{icon, paint, Theme};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};
use wm_x11::X11Backend;
use x11rb::protocol::xproto::Window;

use crate::wallpaper::Wallpaper;
use crate::widgets::{DockWidget, NetLoadWidget, SysMonWidget};

/// The desktop background color — a cool lavender-gray sampled from a
/// reference NeXTSTEP desktop screenshot, not the neutral gray this
/// theme's window chrome uses. The dock has no separate backdrop panel
/// in that reference either: icons sit directly on this same color.
pub const DESKTOP_BG: (u8, u8, u8) = (128, 129, 159);

pub enum RootMenuAction {
    LaunchTerminal,
    LaunchAbout,
    LaunchBrowser,
    SetWallpaper(Wallpaper),
    Exit,
}

const ACTION_LAUNCH_TERMINAL: u32 = 1;
const ACTION_LAUNCH_ABOUT: u32 = 2;
const ACTION_EXIT: u32 = 3;
const ACTION_LAUNCH_BROWSER: u32 = 4;
const ACTION_WALLPAPER_BASE: u32 = 100;

fn root_menu_items(selected_wallpaper: Wallpaper) -> Vec<MenuItem> {
    let wallpaper_items = Wallpaper::ALL
        .into_iter()
        .enumerate()
        .map(|(index, wallpaper)| MenuItem::Action {
            label: if wallpaper == selected_wallpaper {
                format!("\u{2022} {}", wallpaper.label())
            } else {
                format!("  {}", wallpaper.label())
            },
            action: ACTION_WALLPAPER_BASE + index as u32,
        })
        .collect();

    vec![
        MenuItem::Action { label: "Terminal".to_string(), action: ACTION_LAUNCH_TERMINAL },
        MenuItem::Submenu {
            label: "Applications".to_string(),
            items: vec![
                MenuItem::Action { label: "Web Browser".to_string(), action: ACTION_LAUNCH_BROWSER },
                MenuItem::Action { label: "About chonkstep".to_string(), action: ACTION_LAUNCH_ABOUT },
            ],
        },
        MenuItem::Submenu { label: "Wallpaper".to_string(), items: wallpaper_items },
        MenuItem::Action { label: "Exit".to_string(), action: ACTION_EXIT },
    ]
}

fn resolve_action(action: u32) -> Option<RootMenuAction> {
    match action {
        ACTION_LAUNCH_TERMINAL => Some(RootMenuAction::LaunchTerminal),
        ACTION_LAUNCH_ABOUT => Some(RootMenuAction::LaunchAbout),
        ACTION_LAUNCH_BROWSER => Some(RootMenuAction::LaunchBrowser),
        ACTION_EXIT => Some(RootMenuAction::Exit),
        action if (ACTION_WALLPAPER_BASE..ACTION_WALLPAPER_BASE + Wallpaper::ALL.len() as u32)
            .contains(&action) => Some(RootMenuAction::SetWallpaper(
            Wallpaper::ALL[(action - ACTION_WALLPAPER_BASE) as usize],
        )),
        _ => None,
    }
}

struct IconTile {
    window: Window,
    client: ClientId,
    /// Current on-screen position — always authoritative, whether it
    /// came from `auto_slot`'s grid math or a manual drag.
    pos: Point,
    /// `Some(slot)` while this tile is still sitting where the
    /// auto-arrange grid put it; `None` once the user has dragged it
    /// anywhere, at which point it keeps its dragged `pos` forever and
    /// frees its old slot for the next auto-placed icon — matching real
    /// WindowMaker's `icon_moved` flag in `icon.c`, which permanently
    /// exempts a manually-repositioned icon from grid re-arrangement.
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
struct IconDrag {
    window: Window,
    /// Where within the tile the press landed — kept constant relative
    /// to the tile for the whole drag, so the pointer doesn't visually
    /// "jump" to the tile's corner on the first motion event.
    grab_offset: Point,
    /// Crossed `DRAG_THRESHOLD_PX`? A plain click (press, tiny or no
    /// motion, release) restores the window instead of "moving" it —
    /// matches `miniwindowMouseDown`'s `hasMoved` check in real
    /// WindowMaker's `icon.c`, which is exactly why a click still works
    /// at all despite every press arming a potential drag.
    moved: bool,
    grab: DragHandle,
}

/// What releasing after an icon press should do — see `end_icon_drag`.
pub enum IconDragResult {
    /// The press/release was a plain click (never crossed the drag
    /// threshold): restore this client's window.
    Restore(ClientId),
    /// The icon was dragged to a new position; no further action.
    Repositioned,
}

/// Height of the visible Dock chrome only: one identity tile plus the
/// current height of every widget. It is capped to the monitor so an
/// unusually large future widget stack cannot create an invalid X11
/// window, but it never fills spare space merely because it exists.
fn stacked_dock_height(tile: u32, screen_height: u32, widgets: &[Box<dyn DockWidget>]) -> u32 {
    widgets
        .iter()
        .fold(tile, |height, widget| {
            height.saturating_add(tile.saturating_mul(widget.tile_height().max(1)))
        })
        .min(screen_height.max(1))
}

pub struct Desktop {
    dock_window: Window,
    screen_width: u32,
    screen_height: u32,
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
    /// The root menu and its cascades — a generic, reusable SDK
    /// primitive (`wm_theme::cascade::CascadeMenu`), not desktop-shell-
    /// specific state; a `chonk-ui` app building its own dropdown menu
    /// over its own `PopupHost` gets the identical stack/hover/leak-safe
    /// teardown behavior for free.
    menu: CascadeMenu<Window>,
    /// Every instrument shown below the identity tile, top to bottom —
    /// see `crate::widgets` for the SDK these implement. Order is what
    /// `redraw_dock` draws and what a middle-click drag reorders.
    widgets: Vec<Box<dyn DockWidget>>,
    widget_drag: Option<WidgetDrag>,
    icons: HashMap<Window, IconTile>,
    icon_drag: Option<IconDrag>,
    wallpaper: Wallpaper,
    logo: Pixmap,
}

impl Desktop {
    /// `scale` multiplies every dock/icon pixel dimension — pass the
    /// same factor used for `Theme::scaled` so the shell's own chrome
    /// (which doesn't go through the theme engine) matches the WM's.
    pub fn new(backend: &mut X11Backend, screen: Size, scale: f32) -> Self {
        let tile = ((56.0 * scale).round() as u32).max(16);
        let pad = ((4.0 * scale).round() as u32).max(1);
        // The dock is exactly one tile wide, tiles touch directly with
        // no gap, and the identity tile sits flush at the very top —
        // matching real WindowMaker's Dock, a flush column of icons
        // touching both the screen edge and each other, not a WM
        // convention of its own. `pad` still spaces the *desktop's* icon
        // grid (miniaturized windows), which is a separate, unrelated
        // piece of chrome.
        let dock_width = tile;

        let wallpaper = Wallpaper::load();
        let widgets: Vec<Box<dyn DockWidget>> =
            vec![Box::new(SysMonWidget::new()), Box::new(NetLoadWidget::new())];
        let dock_height = stacked_dock_height(tile, screen.h, &widgets);
        let dock_geom = Rect {
            pos: Point::new((screen.w.saturating_sub(dock_width)) as i32, 0),
            size: Size::new(dock_width, dock_height),
        };
        let dock_window = backend
            .create_shell_window(dock_geom, wallpaper.dock_color(), true)
            .expect("failed to create dock window");
        let _ = backend.map_shell_window(dock_window);
        let _ = backend.raise_shell_window(dock_window);

        let logo = Pixmap::decode_png(include_bytes!("../assets/branding/chonkstep-logo-icon.png"))
            .expect("embedded ChonkStep logo should decode");
        let desktop = Self {
            dock_window,
            screen_width: screen.w,
            screen_height: screen.h,
            dock_width,
            tile,
            pad,
            drag_threshold: ((4.0 * scale).round() as i32).max(2),
            font_system: cosmic_text::FontSystem::new(),
            swash_cache: cosmic_text::SwashCache::new(),
            menu: CascadeMenu::new("chonkstep", DESKTOP_BG),
            widgets,
            widget_drag: None,
            icons: HashMap::new(),
            icon_drag: None,
            wallpaper,
            logo,
        };
        desktop.repaint_wallpaper(backend);
        desktop
    }

    pub fn dock_window(&self) -> Window {
        self.dock_window
    }

    /// The Dock is an always-on-top, content-sized object rather than a
    /// reserved sidebar, so maximized windows use the whole monitor.
    pub fn workarea(&self, screen: Size) -> Rect {
        Rect { pos: Point::new(0, 0), size: screen }
    }

    fn screen_size(&self) -> Size {
        Size::new(self.screen_width, self.screen_height)
    }

    /// Repositions/resizes the dock to hug the right edge of a new
    /// screen size (the nested X server's virtual screen was resized —
    /// e.g. the user dragged the edge of the Xephyr window this WM is
    /// running in) and repaints it at the current stack's compact
    /// content height. Icon tiles already on screen are left where they
    /// are.
    pub fn resize_to_screen(&mut self, backend: &mut X11Backend, theme: &Theme, screen: Size) {
        self.screen_width = screen.w;
        self.screen_height = screen.h;
        self.repaint_wallpaper(backend);
        self.redraw_dock(backend, theme);
    }

    /// Advances every dock widget by one event-loop tick and repaints
    /// the dock if anything actually changed — called unconditionally on
    /// every `tick()` (never short-circuited) so a widget further down
    /// the list still gets to sample/animate even if an earlier one had
    /// nothing new to report this iteration.
    pub fn tick_widgets(&mut self, backend: &mut X11Backend, theme: &Theme) {
        let mut changed = false;
        for widget in &mut self.widgets {
            if widget.tick() {
                changed = true;
            }
        }
        if changed {
            self.redraw_dock(backend, theme);
        }
    }

    /// Dock-local Y where the widget stack begins — directly below the
    /// identity tile, touching it, same as every other tile touches its
    /// neighbors.
    fn widgets_top(&self) -> i32 {
        self.tile as i32
    }

    /// Dock-local `(index, rect)` for every widget slot, in order —
    /// widgets don't all occupy the same height (a widget with more
    /// than one face, like [`SysMonWidget`](crate::widgets::SysMonWidget),
    /// can be taller in one than the other), so this walks the stack
    /// accumulating each one's actual `tile_height()` rather than
    /// assuming a fixed stride. Both hit-testing and painting read from
    /// this single source of truth, so they can never disagree about
    /// where a widget sits. No gap between consecutive slots — tiles
    /// snap together, matching real WindowMaker's Dock.
    fn widget_slots(&self) -> Vec<(usize, Rect)> {
        let mut y = self.widgets_top();
        let mut slots = Vec::with_capacity(self.widgets.len());
        for (index, widget) in self.widgets.iter().enumerate() {
            let h = self.tile * widget.tile_height().max(1);
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
    pub fn begin_widget_drag(&mut self, backend: &mut X11Backend, theme: &Theme, local: Point) -> bool {
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
    pub fn drag_widget_motion(&mut self, backend: &mut X11Backend, theme: &Theme, root: Point) {
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
    pub fn end_widget_drag(&mut self, backend: &mut X11Backend, theme: &Theme) -> bool {
        let Some(drag) = self.widget_drag.take() else { return false };
        backend.ungrab_pointer(drag.grab);
        self.redraw_dock(backend, theme);
        true
    }

    /// Left-click handling for whichever widget sits at `local`, if any
    /// (e.g. `SysMonWidget` toggles its analog/dashboard face). Returns
    /// `false` if `local` isn't over a widget slot at all, so callers
    /// can tell whether the click was theirs to handle.
    pub fn click_widget(&mut self, backend: &mut X11Backend, theme: &Theme, local: Point) -> bool {
        let Some(index) = self.widget_index_at(local) else { return false };
        if self.widgets[index].on_click() {
            self.redraw_dock(backend, theme);
        }
        true
    }

    fn redraw_dock(&mut self, backend: &mut X11Backend, theme: &Theme) {
        let dock_height = stacked_dock_height(self.tile, self.screen_height, &self.widgets);
        let dock_geom = Rect {
            pos: Point::new((self.screen_width.saturating_sub(self.dock_width)) as i32, 0),
            size: Size::new(self.dock_width, dock_height),
        };
        let _ = backend.configure_shell_window(self.dock_window, dock_geom);

        let Some(mut pixmap) = Pixmap::new(self.dock_width, dock_height.max(1)) else {
            return;
        };
        let (r, g, b) = self.wallpaper.dock_color();
        paint::fill_rect(
            &mut pixmap,
            0,
            0,
            self.dock_width,
            dock_height,
            wm_theme::model::Color::rgb(r, g, b),
        );

        // Identity tile: flush at the dock's top-left corner, same as
        // every other tile touches its neighbors — the ChonkStep mark is
        // deliberately bold enough to survive the Dock's original
        // 56-pixel scale.
        paint::fill_area(&mut pixmap, 0, 0, self.tile, self.tile, &theme.titlebar.active);
        paint::draw_bevel(&mut pixmap, 0, 0, self.tile, self.tile, &theme.titlebar.bevel);
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
            let buffer = self.widgets[index].render(theme, self.tile);
            blit_into(&mut pixmap, rect.pos.x as u32, rect.pos.y as u32, &buffer);

            if self.widget_drag.as_ref().is_some_and(|d| d.index == index) {
                // A thin glow just inside the slot's own edge — the
                // visual half of "you've picked this up," matching the
                // bevel's own light tone so it reads as part of the same
                // chrome rather than an unrelated highlight color. Drawn
                // inset rather than in a surrounding gap: tiles snap
                // together now, so there's no gap to draw into.
                let light = theme.titlebar.bevel.light;
                let (x, y, w, h) = (rect.pos.x, rect.pos.y, rect.size.w, rect.size.h);
                paint::fill_rect(&mut pixmap, x, y, w, 1, light);
                paint::fill_rect(&mut pixmap, x, y + h as i32 - 1, w, 1, light);
                paint::fill_rect(&mut pixmap, x, y, 1, h, light);
                paint::fill_rect(&mut pixmap, x + w as i32 - 1, y, 1, h, light);
            }
        }

        backend.blit(self.dock_window, &pixmap_to_buffer(&pixmap));
    }

    pub fn open_root_menu(&mut self, backend: &mut X11Backend, theme: &Theme, at: Point) {
        let bounds = self.screen_size();
        self.menu.open(backend, theme, &mut self.font_system, root_menu_items(self.wallpaper), at, bounds);
    }

    /// Applies a built-in wallpaper immediately and repaints the dock to
    /// its matching quiet-edge color. Selection is intentionally a
    /// session preference for now; persistent settings are future work.
    pub fn set_wallpaper(&mut self, backend: &mut X11Backend, theme: &Theme, wallpaper: Wallpaper) {
        self.wallpaper = wallpaper;
        if let Err(error) = wallpaper.persist() {
            tracing::warn!(?error, wallpaper = wallpaper.label(), "failed to remember wallpaper selection");
        }
        self.repaint_wallpaper(backend);
        self.redraw_dock(backend, theme);
    }

    fn repaint_wallpaper(&self, backend: &mut X11Backend) {
        let result = match self.wallpaper.render(self.screen_size()) {
            Some(buffer) => backend.paint_background_image(&buffer),
            None => backend.paint_background(DESKTOP_BG),
        };
        if let Err(error) = result {
            tracing::warn!(?error, wallpaper = self.wallpaper.label(), "failed to paint desktop wallpaper");
        }
    }

    pub fn close_menu(&mut self, backend: &mut X11Backend) {
        self.menu.close(backend);
    }

    /// If `window` belongs to the open menu chain, resolves a click on
    /// it and, if it fired an action, returns the resolved
    /// `RootMenuAction` — see `CascadeMenu::click` for the full
    /// click/dismiss/cascade contract.
    pub fn click_menu(&mut self, backend: &mut X11Backend, theme: &Theme, window: Window, local: Point) -> Option<RootMenuAction> {
        match self.menu.click(backend, theme, &mut self.font_system, window, local) {
            Some(MenuClick::Action(action)) => resolve_action(action),
            _ => None,
        }
    }

    pub fn hover_menu(&mut self, backend: &mut X11Backend, theme: &Theme, window: Window, local: Point) {
        self.menu.hover(backend, theme, &mut self.font_system, window, local);
    }

    /// Opens whatever submenu has been hovered long enough — called once
    /// per event-loop iteration (like `tick_widgets`).
    pub fn tick_menu(&mut self, backend: &mut X11Backend, theme: &Theme) {
        self.menu.tick(backend, theme, &mut self.font_system);
    }

    /// Shows an icon tile for a client that was just miniaturized —
    /// classic WindowMaker "miniaturize to icon", not minimize-to-a-
    /// taskbar (there is no taskbar). Tiles fill left-to-right along the
    /// bottom-left of the screen, wrapping upward — matching the icon
    /// row layout in the reference NeXTSTEP screenshot this theme is
    /// matched against — clear of the dock on the right.
    pub fn show_icon(&mut self, backend: &mut X11Backend, theme: &Theme, client: ClientId, title: &str, preview: Option<&DecorationBuffer>) {
        let slot = (0..).find(|s| !self.icons.values().any(|icon| icon.auto_slot == Some(*s))).unwrap_or(0);
        let pos = self.icon_slot_position(slot);
        let geom = Rect { pos, size: Size::new(self.tile, self.tile) };

        let Ok(window) = backend.create_shell_window(geom, DESKTOP_BG, true) else {
            tracing::warn!(?client, "failed to create icon tile window");
            return;
        };
        let _ = backend.map_shell_window(window);
        let _ = backend.raise_shell_window(window);
        let buffer = icon::render_icon_tile(theme, &mut self.font_system, &mut self.swash_cache, self.tile, title, preview);
        backend.blit(window, &buffer);

        self.icons.insert(window, IconTile { window, client, pos, auto_slot: Some(slot) });
    }

    fn icon_slot_position(&self, slot: usize) -> Point {
        let stride = self.tile + self.pad;
        let usable_width = self.screen_width.max(stride);
        let cols = (usable_width / stride).max(1) as usize;
        let (row, col) = (slot / cols, slot % cols);
        let x = self.pad as i32 + col as i32 * stride as i32;
        let y = self.screen_height as i32 - ((row as u32 + 1) * stride) as i32;
        Point::new(x, y)
    }

    /// Removes the icon tile for `client`, if one is showing (a no-op
    /// otherwise — covers both a normal restore and closing a window
    /// while it's still miniaturized).
    pub fn remove_icon_for_client(&mut self, backend: &mut X11Backend, client: ClientId) {
        let Some(window) = self.icons.values().find(|icon| icon.client == client).map(|icon| icon.window) else {
            return;
        };
        self.icons.remove(&window);
        let _ = backend.destroy_shell_window(window);
    }

    /// Starts tracking a press on `window` as a potential icon drag —
    /// every press on a tile arms both a possible drag *and* a possible
    /// plain click, resolved by `end_icon_drag` on release (see
    /// `IconDrag::moved`'s doc comment). A no-op (and returns `false`)
    /// if `window` isn't a tracked icon tile, so callers can use the
    /// return value to know whether to swallow the press.
    pub fn begin_icon_drag(&mut self, backend: &mut X11Backend, window: Window, local: Point) -> bool {
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
    pub fn drag_icon_motion(&mut self, backend: &mut X11Backend, root: Point) {
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
        let _ = backend.configure_shell_window(drag.window, Rect { pos: new_pos, size: Size::new(self.tile, self.tile) });
    }

    /// Resolves whatever press `begin_icon_drag` armed, if any: a
    /// release without crossing the move threshold restores the
    /// window (matching a plain click), one that did just leaves the
    /// icon at its new dragged position. Returns `None` if no icon
    /// drag was in progress — callers should fall through to their
    /// normal release handling (e.g. menu clicks) in that case.
    pub fn end_icon_drag(&mut self, backend: &mut X11Backend) -> Option<IconDragResult> {
        let drag = self.icon_drag.take()?;
        backend.ungrab_pointer(drag.grab);

        if drag.moved {
            return Some(IconDragResult::Repositioned);
        }
        let icon = self.icons.remove(&drag.window)?;
        let _ = backend.destroy_shell_window(icon.window);
        Some(IconDragResult::Restore(icon.client))
    }
}

/// Pure drag-threshold arithmetic, kept separate from `drag_icon_motion`
/// so it's testable without an `X11Backend`: `None` if motion hasn't
/// crossed `threshold` yet (and hadn't already elsewhere in the drag),
/// `Some(new_pos)` once it has. Matches `miniwindowMouseDown`'s
/// `hasMoved` check in real WindowMaker's `icon.c` — the reason a
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

    struct FixedHeightWidget(u32);

    impl DockWidget for FixedHeightWidget {
        fn tick(&mut self) -> bool {
            false
        }

        fn render(&self, _theme: &Theme, _tile: u32) -> DecorationBuffer {
            DecorationBuffer { width: 1, height: 1, pixels: vec![0; 4] }
        }

        fn tile_height(&self) -> u32 {
            self.0
        }
    }

    #[test]
    fn dock_height_is_only_the_identity_and_current_widget_stack() {
        let widgets: Vec<Box<dyn DockWidget>> =
            vec![Box::new(FixedHeightWidget(1)), Box::new(FixedHeightWidget(3))];

        assert_eq!(stacked_dock_height(56, 1_080, &widgets), 280);
        assert_eq!(stacked_dock_height(56, 200, &widgets), 200, "oversized stacks are screen-clamped");
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
                resolve_action(ACTION_WALLPAPER_BASE + index as u32),
                Some(RootMenuAction::SetWallpaper(resolved)) if resolved == wallpaper
            ));
        }
    }

    #[test]
    fn wallpaper_submenu_marks_the_current_selection() {
        let items = root_menu_items(Wallpaper::TealBlueprint);
        let submenu = items.iter().find(|item| item.label() == "Wallpaper").expect("wallpaper submenu");
        let MenuItem::Submenu { items, .. } = submenu else { panic!("expected submenu") };
        assert_eq!(items.len(), Wallpaper::ALL.len());
        assert!(items.iter().any(|item| item.label() == "\u{2022} Teal Blueprint"));
    }
}
