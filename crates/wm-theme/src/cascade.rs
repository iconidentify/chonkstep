//! A reusable, stateful cascading-menu controller: owns the popup-window
//! stack, hover-to-open-submenu hysteresis, and edge-aware cascade
//! positioning for an arbitrarily-nested [`MenuItem`] tree. Generic over
//! [`PopupHost`] so any host — the desktop shell's `X11Backend`, or a
//! future `chonk-ui` app building its own dropdown menu over its own
//! connection — gets the identical cascade *behavior*, not just the same
//! item-tree rendering, by implementing that one small trait.
//! `chonkstep::desktop::Desktop` is this controller's first real caller,
//! not a parallel reimplementation of it.

use std::time::{Duration, Instant};

use wm_theme_api::{Point, PopupGrab, PopupHost, Rect, Size};

use crate::menu::{self, MenuItem};
use crate::model::Theme;

/// How long the pointer must dwell over a submenu row before it opens —
/// matches classic Mac/NeXT/WindowMaker menu hysteresis (WindowMaker's
/// own `MENU_SELECT_DELAY` is ~200ms) so merely sweeping the mouse across
/// a row on the way to something else doesn't flash every cascade open
/// along the path.
pub const SUBMENU_HOVER_DELAY: Duration = Duration::from_millis(180);

/// One level of an open menu/submenu chain — each is its own popup
/// window (matching real WindowMaker: cascades are separate top-level
/// windows positioned beside their parent, not a single window that
/// grows).
struct OpenLevel<Id> {
    window: Id,
    items: Vec<MenuItem>,
    item_rects: Vec<Rect>,
    highlighted: Option<usize>,
    geom: Rect,
    /// Index into the *parent* level's `items` this level cascaded from
    /// — `None` for the root level.
    opened_from_item: Option<usize>,
}

struct PendingSubmenu {
    level: usize,
    item_index: usize,
    hovered_since: Instant,
}

/// The result of a click that landed on one of this menu's own popups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuClick {
    /// A submenu row was opened immediately (a fast deliberate click
    /// beats the hover-open delay) — the chain stays open.
    OpenedSubmenu,
    /// An action row fired this id; the whole chain is now closed.
    Action(u32),
    /// The click landed inside the popup but not on any item — the
    /// close box, or blank space — so the whole chain is now closed with
    /// no action, exactly like clicking a window's close button.
    Dismissed,
}

/// Owns zero or more cascaded popup windows for one open menu session.
/// `Id` is the host's `PopupHost::PopupId`.
pub struct CascadeMenu<Id> {
    title: String,
    background: (u8, u8, u8),
    bounds: Size,
    levels: Vec<OpenLevel<Id>>,
    grab: Option<PopupGrab>,
    pending_submenu: Option<PendingSubmenu>,
}

impl<Id: Copy + Eq + std::fmt::Debug> CascadeMenu<Id> {
    /// `title` labels every popup's title bar (root and cascades alike —
    /// matching this SDK's existing single-app-identity menu chrome);
    /// `background` is the popup window's fallback clear color before
    /// the first paint.
    pub fn new(title: impl Into<String>, background: (u8, u8, u8)) -> Self {
        Self {
            title: title.into(),
            background,
            bounds: Size::new(0, 0),
            levels: Vec::new(),
            grab: None,
            pending_submenu: None,
        }
    }

    pub fn is_open(&self) -> bool {
        !self.levels.is_empty()
    }

    /// Opens (replacing any existing session) a root-level popup at `at`
    /// and grabs the pointer for the whole session. `bounds` is the
    /// host's screen/window extent, used to keep later cascades on
    /// screen.
    pub fn open<H: PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        items: Vec<MenuItem>,
        at: Point,
        bounds: Size,
    ) {
        self.close(host);
        self.bounds = bounds;
        if let Some(level) = self.open_level(host, theme, font_system, items, at, None) {
            self.levels.push(level);
        }
        self.grab = Some(host.grab_pointer());
    }

    pub fn close<H: PopupHost<PopupId = Id>>(&mut self, host: &mut H) {
        self.truncate(host, 0);
        if let Some(grab) = self.grab.take() {
            host.ungrab_pointer(grab);
        }
    }

    /// If `window` belongs to this menu's chain, resolves a click on it:
    /// a submenu row opens immediately (for a fast deliberate click that
    /// beats the hover-open delay), an action row closes the whole chain
    /// and returns the action id, and a click anywhere else in the popup
    /// (including the title bar's close box) dismisses the whole chain
    /// with no action. Returns `None` if `window` isn't one of this
    /// menu's own popups.
    pub fn click<H: PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        window: Id,
        local: Point,
    ) -> Option<MenuClick> {
        let level = self.level_for_window(window)?;
        let hit = self.levels[level].item_rects.iter().position(|r| r.contains(local));

        let Some(hit) = hit else {
            self.close(host);
            return Some(MenuClick::Dismissed);
        };

        if self.levels[level].items[hit].is_submenu() {
            self.open_submenu(host, theme, font_system, level, hit);
            return Some(MenuClick::OpenedSubmenu);
        }

        let MenuItem::Action { action, .. } = self.levels[level].items[hit] else {
            self.close(host);
            return Some(MenuClick::Dismissed);
        };
        self.close(host);
        Some(MenuClick::Action(action))
    }

    /// If `window` belongs to this menu's chain, re-renders that level
    /// with whichever item is now under the pointer highlighted (a no-op
    /// if unchanged, so hovering within one row doesn't repaint on every
    /// pixel of motion), truncates any now-stale cascade hanging off a
    /// sibling item, and arms the hover-open hysteresis for a freshly
    /// hovered submenu row (see `tick`). Returns whether `window`
    /// belonged to this menu at all.
    pub fn hover<H: PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        window: Id,
        local: Point,
    ) -> bool {
        let Some(level) = self.level_for_window(window) else {
            return false;
        };
        let hovered = self.levels[level].item_rects.iter().position(|r| r.contains(local));
        if hovered == self.levels[level].highlighted {
            return true;
        }
        self.levels[level].highlighted = hovered;
        let render = menu::render_menu(theme, font_system, &self.title, &self.levels[level].items, hovered);
        host.paint_popup(self.levels[level].window, &render.buffer);

        let child_belongs_to = self.levels.get(level + 1).and_then(|c| c.opened_from_item);
        if hovered != child_belongs_to {
            self.truncate(host, level + 1);
        }

        self.pending_submenu = match hovered {
            Some(i) if self.levels[level].items[i].is_submenu() && child_belongs_to != Some(i) => {
                Some(PendingSubmenu { level, item_index: i, hovered_since: Instant::now() })
            }
            _ => None,
        };
        true
    }

    /// Opens whatever submenu has been hovered long enough — call once
    /// per event-loop tick (like a clock tick) so a submenu still opens
    /// even if the pointer stops moving entirely once it lands on the
    /// row, not just on the next motion event.
    pub fn tick<H: PopupHost<PopupId = Id>>(&mut self, host: &mut H, theme: &Theme, font_system: &mut cosmic_text::FontSystem) {
        let Some(pending) = &self.pending_submenu else {
            return;
        };
        if pending.hovered_since.elapsed() < SUBMENU_HOVER_DELAY {
            return;
        }
        let (level, item_index) = (pending.level, pending.item_index);
        self.pending_submenu = None;
        self.open_submenu(host, theme, font_system, level, item_index);
    }

    fn open_level<H: PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        items: Vec<MenuItem>,
        at: Point,
        opened_from_item: Option<usize>,
    ) -> Option<OpenLevel<Id>> {
        let render = menu::render_menu(theme, font_system, &self.title, &items, None);
        let geom = Rect { pos: at, size: Size::new(render.buffer.width, render.buffer.height) };
        let window = host.create_popup(geom, self.background)?;
        host.paint_popup(window, &render.buffer);
        Some(OpenLevel { window, items, item_rects: render.item_rects, highlighted: None, geom, opened_from_item })
    }

    /// Opens a submenu cascading from `item_index` in level
    /// `parent_level`, positioned beside the parent: to its right,
    /// flipping to the left if that would run off the screen's right
    /// edge, row-aligned with the item it cascades from and clamped so
    /// it doesn't run off the bottom — matches real WindowMaker's
    /// cascade placement (`menu.c`'s `open_to_left` logic).
    fn open_submenu<H: PopupHost<PopupId = Id>>(
        &mut self,
        host: &mut H,
        theme: &Theme,
        font_system: &mut cosmic_text::FontSystem,
        parent_level: usize,
        item_index: usize,
    ) {
        // Anything deeper than the item we're cascading from is stale —
        // destroy it before creating the new one, or its window leaks
        // (found via visual testing: a stale submenu window left behind
        // after hovering back to a sibling item).
        self.truncate(host, parent_level + 1);

        let Some(parent) = self.levels.get(parent_level) else {
            return;
        };
        let MenuItem::Submenu { items, .. } = parent.items[item_index].clone() else {
            return;
        };
        if items.is_empty() {
            return;
        }

        let menu_width = parent.geom.size.w;
        let mut x = parent.geom.pos.x + parent.geom.size.w as i32;
        if x + menu_width as i32 > self.bounds.w as i32 {
            x = (parent.geom.pos.x - menu_width as i32).max(0);
        }
        let mut y = parent.geom.pos.y + parent.item_rects[item_index].pos.y;
        let approx_height = parent.item_rects.first().map(|r| r.size.h).unwrap_or(20) * (items.len() as u32 + 1);
        if y + approx_height as i32 > self.bounds.h as i32 {
            y = (self.bounds.h as i32 - approx_height as i32).max(0);
        }

        if let Some(level) = self.open_level(host, theme, font_system, items, Point::new(x, y), Some(item_index)) {
            self.levels.push(level);
        }
    }

    /// Drops every level from `len` onward, destroying each one's popup
    /// window first — dropping the tracking struct alone would leak an
    /// orphaned popup that stays on screen forever.
    fn truncate<H: PopupHost<PopupId = Id>>(&mut self, host: &mut H, len: usize) {
        while self.levels.len() > len {
            if let Some(level) = self.levels.pop() {
                host.destroy_popup(level.window);
            }
        }
        self.pending_submenu = None;
    }

    fn level_for_window(&self, window: Id) -> Option<usize> {
        self.levels.iter().position(|level| level.window == window)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;
    use std::collections::HashSet;
    use std::thread;
    use wm_theme_api::DecorationBuffer;

    #[derive(Default)]
    struct FakeHost {
        next_id: u32,
        open: HashSet<u32>,
        destroyed_total: u32,
        grabs: u32,
        ungrabs: u32,
    }

    impl PopupHost for FakeHost {
        type PopupId = u32;

        fn create_popup(&mut self, _geometry: Rect, _background: (u8, u8, u8)) -> Option<u32> {
            self.next_id += 1;
            self.open.insert(self.next_id);
            Some(self.next_id)
        }

        fn destroy_popup(&mut self, popup: u32) {
            self.open.remove(&popup);
            self.destroyed_total += 1;
        }

        fn paint_popup(&mut self, _popup: u32, _buffer: &DecorationBuffer) {}

        fn grab_pointer(&mut self) -> PopupGrab {
            self.grabs += 1;
            PopupGrab(0)
        }

        fn ungrab_pointer(&mut self, _grab: PopupGrab) {
            self.ungrabs += 1;
        }
    }

    fn action(label: &str, action: u32) -> MenuItem {
        MenuItem::Action { label: label.to_string(), action }
    }

    fn submenu(label: &str, items: Vec<MenuItem>) -> MenuItem {
        MenuItem::Submenu { label: label.to_string(), items }
    }

    /// The first item row's vertical midpoint — inside every item's hit
    /// rect regardless of which row, since rows are uniform height.
    fn first_row_point(theme: &Theme) -> Point {
        Point::new(4, theme.menu.item_height as i32 + 2)
    }

    /// Inside the title bar, above every item row — a guaranteed miss.
    fn title_bar_point() -> Point {
        Point::new(1, 1)
    }

    struct Fixture {
        theme: Theme,
        font_system: cosmic_text::FontSystem,
        host: FakeHost,
        cascade: CascadeMenu<u32>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                theme: nextstep_classic(),
                font_system: cosmic_text::FontSystem::new(),
                host: FakeHost::default(),
                cascade: CascadeMenu::new("chonkstep", (0, 0, 0)),
            }
        }

        fn open(&mut self, items: Vec<MenuItem>) {
            self.cascade.open(&mut self.host, &self.theme, &mut self.font_system, items, Point::new(0, 0), Size::new(1600, 1000));
        }

        fn click(&mut self, window: u32, local: Point) -> Option<MenuClick> {
            self.cascade.click(&mut self.host, &self.theme, &mut self.font_system, window, local)
        }

        fn hover(&mut self, window: u32, local: Point) -> bool {
            self.cascade.hover(&mut self.host, &self.theme, &mut self.font_system, window, local)
        }

        fn tick(&mut self) {
            self.cascade.tick(&mut self.host, &self.theme, &mut self.font_system);
        }

        fn only_open_window(&self) -> u32 {
            assert_eq!(self.host.open.len(), 1, "expected exactly one open popup");
            *self.host.open.iter().next().unwrap()
        }
    }

    #[test]
    fn opening_creates_exactly_one_popup_and_grabs_the_pointer() {
        let mut f = Fixture::new();
        f.open(vec![action("Terminal", 1)]);

        assert_eq!(f.host.open.len(), 1);
        assert_eq!(f.host.grabs, 1);
        assert!(f.cascade.is_open());
    }

    #[test]
    fn clicking_a_miss_dismisses_the_whole_chain_and_destroys_every_popup() {
        // Regression test: clicking inside the popup but outside every
        // item row (the close box, or blank space) previously did
        // nothing at all instead of closing the menu.
        let mut f = Fixture::new();
        f.open(vec![action("Terminal", 1)]);
        let window = f.only_open_window();

        let result = f.click(window, title_bar_point());

        assert_eq!(result, Some(MenuClick::Dismissed));
        assert!(!f.cascade.is_open());
        assert!(f.host.open.is_empty(), "dismissing must destroy the popup, not just forget it");
        assert_eq!(f.host.ungrabs, 1, "dismissing must release the pointer grab");
    }

    #[test]
    fn clicking_an_action_row_closes_the_menu_and_returns_its_action_id() {
        let mut f = Fixture::new();
        f.open(vec![action("Terminal", 42)]);
        let window = f.only_open_window();
        let row = first_row_point(&f.theme);

        let result = f.click(window, row);

        assert_eq!(result, Some(MenuClick::Action(42)));
        assert!(!f.cascade.is_open());
        assert!(f.host.open.is_empty());
    }

    #[test]
    fn clicking_an_unrelated_window_is_ignored() {
        let mut f = Fixture::new();
        f.open(vec![action("Terminal", 1)]);

        let result = f.click(999, Point::new(0, 0));

        assert_eq!(result, None);
        assert!(f.cascade.is_open(), "an unrelated window's click must not affect this menu");
    }

    #[test]
    fn clicking_a_submenu_row_opens_a_second_popup_without_closing() {
        let mut f = Fixture::new();
        f.open(vec![submenu("Applications", vec![action("About", 2)])]);
        let root = f.only_open_window();
        let row = first_row_point(&f.theme);

        let result = f.click(root, row);

        assert_eq!(result, Some(MenuClick::OpenedSubmenu));
        assert!(f.cascade.is_open());
        assert_eq!(f.host.open.len(), 2, "root and its submenu should both be open");
    }

    #[test]
    fn opening_a_second_submenu_destroys_the_first_before_creating_the_second() {
        // Regression test for the leaked-popup bug: replacing an open
        // submenu (clicking a different submenu row on the same parent)
        // must destroy the stale one first, never leaving two cascades
        // open from the same parent level.
        let mut f = Fixture::new();
        f.open(vec![
            submenu("Applications", vec![action("About", 2)]),
            submenu("Utilities", vec![action("Terminal", 3)]),
        ]);
        let root = f.only_open_window();
        let row0 = first_row_point(&f.theme);
        let row1 = Point::new(4, f.theme.menu.item_height as i32 * 2 + 2);

        f.click(root, row0);
        assert_eq!(f.host.open.len(), 2, "first submenu open");
        let destroyed_before = f.host.destroyed_total;

        f.click(root, row1);

        assert_eq!(f.host.open.len(), 2, "still exactly root + one submenu, never three");
        assert_eq!(f.host.destroyed_total, destroyed_before + 1, "the first submenu's popup must be destroyed");
    }

    #[test]
    fn hovering_a_submenu_row_does_not_open_it_before_the_delay_elapses() {
        let mut f = Fixture::new();
        f.open(vec![submenu("Applications", vec![action("About", 2)])]);
        let root = f.only_open_window();
        let row = first_row_point(&f.theme);

        assert!(f.hover(root, row));
        f.tick();

        assert_eq!(f.host.open.len(), 1, "must not open before SUBMENU_HOVER_DELAY elapses");
    }

    #[test]
    fn hovering_a_submenu_row_opens_it_after_the_delay_elapses() {
        let mut f = Fixture::new();
        f.open(vec![submenu("Applications", vec![action("About", 2)])]);
        let root = f.only_open_window();
        let row = first_row_point(&f.theme);

        f.hover(root, row);
        thread::sleep(SUBMENU_HOVER_DELAY + Duration::from_millis(60));
        f.tick();

        assert_eq!(f.host.open.len(), 2, "hover-open should have cascaded the submenu");
    }

    #[test]
    fn hovering_back_to_a_different_row_closes_the_open_submenu() {
        let mut f = Fixture::new();
        f.open(vec![submenu("Applications", vec![action("About", 2)]), action("Exit", 9)]);
        let root = f.only_open_window();
        let submenu_row = first_row_point(&f.theme);
        let exit_row = Point::new(4, f.theme.menu.item_height as i32 * 2 + 2);

        f.click(root, submenu_row);
        assert_eq!(f.host.open.len(), 2);

        f.hover(root, exit_row);

        assert_eq!(f.host.open.len(), 1, "hovering away from the submenu's parent row should close it");
    }
}
