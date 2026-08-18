//! Rendering for WindowMaker/NeXTSTEP-style popup menus (the root menu,
//! and eventually app menus): a titled bar over a vertical item list,
//! each item inverting to the theme's highlight colors on hover. The
//! title bar carries a close box in its top-right corner, matching a
//! window titlebar's close button both in position and in glyph —
//! reuses the exact same drawing code (`raster::draw_button_glyph`) so
//! the two stay visually identical by construction, not by convention.
//!
//! Menus are trees (`MenuItem::Submenu`), not flat lists — real
//! WindowMaker root menus nest arbitrarily deep (`Applications >
//! Internet > Firefox`). This module only renders *one level* at a time;
//! the popup-stack lifecycle (which submenu is open, hover-to-open
//! hysteresis, off-screen flip positioning) is a desktop-shell concern —
//! see `chonkstep::desktop::Desktop` for the reference implementation any
//! `chonk-ui` app can follow.

use wm_theme_api::{ButtonKind, DecorationBuffer, Point, Rect, Size};

use crate::model::{Theme, TextAlign};
use crate::raster::draw_button_glyph;
use crate::paint;

/// One row of a menu, at any nesting level. Actions carry an opaque
/// `u32` the caller assigns and interprets — this crate has no opinion
/// on what a menu is *for*, only how it looks and hit-tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MenuItem {
    Action { label: String, action: u32 },
    Submenu { label: String, items: Vec<MenuItem> },
}

impl MenuItem {
    pub fn label(&self) -> &str {
        match self {
            MenuItem::Action { label, .. } | MenuItem::Submenu { label, .. } => label,
        }
    }

    pub fn is_submenu(&self) -> bool {
        matches!(self, MenuItem::Submenu { .. })
    }
}

/// A rasterized menu popup plus everything needed for hit-testing: one
/// rect per `items` entry (same order as passed in) and the title bar's
/// close box.
pub struct MenuRender {
    pub buffer: DecorationBuffer,
    pub item_rects: Vec<Rect>,
    pub close_rect: Rect,
}

/// Builds its own `SwashCache` per call: menus are re-rendered only on
/// open/hover-change, not per frame, so the lost glyph-cache reuse
/// across calls is a fine trade for a simpler signature.
pub fn render_menu(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    title: &str,
    items: &[MenuItem],
    highlighted: Option<usize>,
) -> MenuRender {
    let mut swash_cache = cosmic_text::SwashCache::new();
    let menu = &theme.menu;
    let item_h = menu.item_height as u32;
    let title_h = item_h;
    let width = menu.min_width as u32;
    let bevel_w = menu.bevel.width as u32;
    let height = title_h + item_h * items.len() as u32 + bevel_w * 2;

    let mut pixmap = tiny_skia::Pixmap::new(width.max(1), height.max(1)).expect("nonzero menu size");

    paint::fill_area(&mut pixmap, 0, 0, width, height, &menu.background);
    paint::fill_area(&mut pixmap, 0, 0, width, title_h, &menu.title_bar);

    // Close box: sized/positioned like a window titlebar close button,
    // reusing that same button style so the two families of chrome
    // (window titlebars, menu titles) share one visual vocabulary —
    // including sitting flush against the title strip's own edges (no
    // margin), matching real WindowMaker's own buttons. Clamped to
    // `title_h` defensively: the window titlebar button style now sizes
    // itself to match its *own* titlebar height, which happens to equal
    // the menu's `item_height` in the flagship theme, but nothing
    // guarantees a future theme keeps those two values in lockstep.
    let close_style = theme.titlebar.buttons.iter().find(|b| b.kind == ButtonKind::Close);
    let close_size = close_style.map(|s| s.size as u32).unwrap_or(title_h.saturating_sub(6)).min(title_h);
    let close_rect = Rect::new(Point::new((width.saturating_sub(close_size)) as i32, 0), Size::new(close_size, close_size));
    if let Some(style) = close_style {
        // Same parity fix as window titlebars: the close box is the
        // menu's own title bar fill/text color showing through, not a
        // separately-colored control — `menu.text_color` is for the
        // list items below, not text sitting on `menu.title_bar`. And,
        // also same as window titlebars: no re-fill here, only the
        // bevel — `menu.title_bar` is a gradient already painted across
        // the *whole* title strip above; re-filling just this small
        // close-box rect with the same `Fill::Gradient` would recompute
        // it relative to that smaller rect and produce a visibly
        // mismatched, steeper gradient than the strip around it.
        paint::draw_bevel(&mut pixmap, close_rect.pos.x, close_rect.pos.y, close_rect.size.w, close_rect.size.h, &style.bevel);
        draw_button_glyph(&mut pixmap, ButtonKind::Close, close_rect, menu.title_text_color);
    }

    let title_text_w = close_rect.pos.x.saturating_sub(12).max(0) as u32;
    paint::draw_text(
        &mut pixmap,
        font_system,
        &mut swash_cache,
        title,
        &menu.title_font,
        menu.title_text_color,
        6,
        0,
        title_text_w,
        title_h,
        TextAlign::Center,
    );
    // No separate bevel around just the title strip: the whole menu
    // already gets one bevel below (same reasoning as window titlebars
    // in `raster.rs` — a second, overlapping bevel right at the close
    // box's corner doubled up into a thick flat stripe instead of a
    // clean highlight).

    // Room reserved on the right of every row for the cascade arrow, so
    // a submenu's label never runs into it — kept constant across rows
    // (whether or not that particular row is a submenu) so text doesn't
    // jump around as items highlight.
    let arrow_size = (item_h as f32 * 0.4) as u32;
    let arrow_gutter = arrow_size + 10;

    let mut item_rects = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let y = title_h + i as u32 * item_h;
        let is_highlighted = highlighted == Some(i);
        let (fill, text_color) = if is_highlighted {
            (&menu.highlight_background, menu.highlight_text_color)
        } else {
            (&menu.background, menu.text_color)
        };
        paint::fill_area(&mut pixmap, 0, y as i32, width, item_h, fill);
        paint::draw_text(
            &mut pixmap,
            font_system,
            &mut swash_cache,
            item.label(),
            &menu.item_font,
            text_color,
            8,
            y as i32,
            width.saturating_sub(14).saturating_sub(arrow_gutter),
            item_h,
            TextAlign::Left,
        );
        if item.is_submenu() {
            let arrow_x = width.saturating_sub(arrow_gutter) as i32 + 2;
            let arrow_y = y as i32 + ((item_h.saturating_sub(arrow_size)) / 2) as i32;
            paint::draw_cascade_arrow(&mut pixmap, arrow_x, arrow_y, arrow_size, text_color);
        }
        item_rects.push(Rect::new(Point::new(0, y as i32), Size::new(width, item_h)));
    }

    paint::draw_bevel(&mut pixmap, 0, 0, width, height, &menu.bevel);

    MenuRender {
        buffer: DecorationBuffer { width, height, pixels: pixmap.data().to_vec() },
        item_rects,
        close_rect,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_theme::nextstep_classic;

    fn action(label: &str, action: u32) -> MenuItem {
        MenuItem::Action { label: label.to_string(), action }
    }

    fn submenu(label: &str, items: Vec<MenuItem>) -> MenuItem {
        MenuItem::Submenu { label: label.to_string(), items }
    }

    #[test]
    fn render_menu_produces_one_rect_per_item_with_no_overlap() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let items = [action("Terminal", 1), action("Restart", 2), action("Exit", 3)];

        let render = render_menu(&theme, &mut font_system, "Chonkstep", &items, Some(1));

        assert_eq!(render.item_rects.len(), items.len());
        assert_eq!(render.buffer.pixels.len(), (render.buffer.width * render.buffer.height * 4) as usize);
        for pair in render.item_rects.windows(2) {
            let (a, b) = (pair[0], pair[1]);
            assert!(a.pos.y + a.size.h as i32 <= b.pos.y, "menu item rects must not overlap");
        }
    }

    #[test]
    fn close_box_sits_within_the_title_bar_top_right() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let render = render_menu(&theme, &mut font_system, "Chonkstep", &[action("Exit", 1)], None);

        assert!(render.close_rect.pos.x > 0, "close box should not sit at the left edge");
        assert!(
            (render.close_rect.pos.y + render.close_rect.size.h as i32) as u32 <= theme.menu.item_height as u32,
            "close box should stay within the title bar's height"
        );
    }

    #[test]
    fn a_submenu_row_renders_differently_from_a_plain_action_row() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let items = [submenu("Applications", vec![action("About", 1)])];

        let render = render_menu(&theme, &mut font_system, "Chonkstep", &items, None);
        let plain = render_menu(&theme, &mut font_system, "Chonkstep", &[action("Applications", 1)], None);

        assert_ne!(render.buffer.pixels, plain.buffer.pixels, "cascade arrow should make a submenu row paint differently");
    }
}
