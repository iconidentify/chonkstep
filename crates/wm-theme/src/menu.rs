//! Rendering for WindowMaker/NeXTSTEP-style popup menus (the root menu,
//! and eventually app menus), ported recipe-for-recipe from the
//! WindowMaker source rather than eyeballed: a titlebar-styled title
//! strip over a stack of individually-reliefed entry strips.
//!
//! The specific recipes, and where they come from:
//! - Menus size themselves to their content (`wMenuRealize` in
//!   `src/menu.c`): widest entry text plus fixed paddings, with a gutter
//!   on the right for the cascade indicator, never a fixed width.
//! - Every entry is its own raised strip — `WREL_MENUENTRY` in
//!   `src/texture.c`: +80 add along the top and left, -40 subtract along
//!   the right and second-to-bottom row, absolute black along the bottom
//!   row. This stack of shallow strips (a softer cousin of the chrome's
//!   RAISED2) is the signature WindowMaker menu look.
//! - The hover highlight (`paintEntry` in `src/menu.c`) fills *inside*
//!   the entry's relief — inset one line on the left/right/top, three on
//!   the bottom — so the strip edges stay put while only the face
//!   inverts. The same lesson as the titlebar buttons: chrome that
//!   vanishes on interaction reads as breakage.
//! - The cascade indicator is `paintEntry`'s engraved chevron: three
//!   hard lines (dim upper diagonal, light lower diagonal, dark spine)
//!   in absolute colors derived from the item face, not a filled
//!   triangle glyph.
//! - The title strip is a real titlebar: the window titlebar's height
//!   and RAISED2 relief (menus in WindowMaker are `wFrameWindow`s, so
//!   this equality is by construction there — and by these shared
//!   constants here). No close box: WindowMaker only shows a titlebar
//!   button on a menu once it's been pinned ("buttoned"), and these
//!   popups are transient — clicking anywhere off an item dismisses.
//!
//! Menus are trees (`MenuItem::Submenu`), not flat lists — real
//! WindowMaker root menus nest arbitrarily deep (`Applications >
//! Internet > Firefox`). This module only renders *one level* at a time;
//! the popup-stack lifecycle (which submenu is open, hover-to-open
//! hysteresis, off-screen flip positioning) is `cascade::CascadeMenu`'s
//! job, with `chonkstep::desktop::Desktop` as the reference host.

use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use crate::model::{Color, Fill, TextAlign, Theme};
use crate::paint;
use crate::tile;

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
/// rect per `items` entry (same order as passed in). Anything outside
/// the item rects — the title strip, the border — is a dismissal.
pub struct MenuRender {
    pub buffer: DecorationBuffer,
    pub item_rects: Vec<Rect>,
}

/// The face color an entry's engraved details key off — a solid is
/// itself, a gradient averages its endpoints (same reasoning as
/// `tile::tile_ink`).
fn fill_average(fill: &Fill) -> Color {
    match fill {
        Fill::Solid(c) => *c,
        Fill::Gradient(g) => Color::rgb(
            ((g.from.r as u16 + g.to.r as u16) / 2) as u8,
            ((g.from.g as u16 + g.to.g as u16) / 2) as u8,
            ((g.from.b as u16 + g.to.b as u16) / 2) as u8,
        ),
    }
}

fn shift(c: Color, delta: i16) -> Color {
    let op = |v: u8| (v as i16 + delta).clamp(0, 255) as u8;
    Color::rgb(op(c.r), op(c.g), op(c.b))
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
    let item_h = (menu.item_height as u32).max(4);
    // WindowMaker menu title strips are window titlebars (menus are
    // wFrameWindows there), so the height and relief come from the
    // titlebar style, not the item style — visibly taller than an entry.
    let title_h = (theme.titlebar.height as u32).max(item_h);

    // All of WindowMaker's menu metrics are in unscaled pixels against
    // its stock ~20px entry; ours scale with the theme, so a stock
    // metric `v` becomes `px(v)` here.
    let px = |v: i32| -> i32 { ((v * item_h as i32) + 10) / 20 };
    let t = (menu.bevel.width as u32).max(1);

    // Content-driven width, `wMenuRealize` verbatim: widest entry text
    // plus 10, a right gutter of 16 where any row cascades (4 otherwise),
    // never narrower than the title text plus its titlebar-derived
    // padding. No fixed or minimum width at all.
    let any_submenu = items.iter().any(|i| i.is_submenu());
    let gutter = if any_submenu { px(16) } else { px(4) } as u32;
    let widest_label = items
        .iter()
        .map(|i| paint::text_width(font_system, &menu.item_font, i.label()))
        .max()
        .unwrap_or(0);
    let title_w = paint::text_width(font_system, &menu.title_font, title) + title_h + px(16) as u32;
    let content_w = (widest_label + px(10) as u32 + gutter).max(title_w);

    // The 1px (scaled) outline around everything is the frame border
    // every WindowMaker menu window carries, same as its sibling window
    // frames — content sits inside it.
    let bw = (theme.border.width as u32).max(1);
    let width = content_w + bw * 2;
    let height = title_h + item_h * items.len() as u32 + bw * 2;

    let mut pixmap = tiny_skia::Pixmap::new(width.max(1), height.max(1)).expect("nonzero menu size");

    // Title strip: the window titlebar treatment — fill plus the same
    // RAISED2 relief recipe, centered title text (menus inherit the
    // frame's stock center justification).
    let x0 = bw as i32;
    paint::fill_area(&mut pixmap, x0, bw as i32, content_w, title_h, &menu.title_bar);
    let title_t = (theme.titlebar.bevel.width as u32).max(1);
    paint::draw_raised2_bevel(&mut pixmap, x0, bw as i32, content_w, title_h, title_t);
    paint::draw_text(
        &mut pixmap,
        font_system,
        &mut swash_cache,
        title,
        &menu.title_font,
        menu.title_text_color,
        x0 + px(8),
        bw as i32,
        content_w.saturating_sub(px(16) as u32),
        title_h,
        TextAlign::Center,
    );

    // The chevron's three inks are absolute colors derived from the
    // item face — WindowMaker draws them with the item texture's
    // light/dim/dark GCs, so they hold still when the highlight fill
    // arrives underneath instead of vanishing into it.
    let face = fill_average(&menu.background);
    let (chev_light, chev_dim, chev_dark) = (shift(face, 80), shift(face, -40), shift(face, -90));

    let mut item_rects = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let y0 = (bw + title_h + i as u32 * item_h) as i32;
        let is_highlighted = highlighted == Some(i);

        // Entry face, then its own WREL_MENUENTRY relief: every row is
        // a shallow raised strip, and the stack of strips *is* the menu
        // body — no flat shared background, no whole-menu bevel.
        paint::fill_area(&mut pixmap, x0, y0, content_w, item_h, &menu.background);
        let w = content_w;
        paint::op_rect(&mut pixmap, x0 + t as i32, y0, w - t * 2, t, 80);
        paint::op_rect(&mut pixmap, x0, y0, t, item_h, 80);
        paint::op_rect(&mut pixmap, x0 + (w - t) as i32, y0, t, item_h, -40);
        paint::op_rect(&mut pixmap, x0 + t as i32, y0 + (item_h - t * 2) as i32, w - t * 2, t, -40);
        paint::fill_rect(&mut pixmap, x0, y0 + (item_h - t) as i32, w, t, Color::rgb(0, 0, 0));

        // The highlight fills inside the relief — one line in on the
        // left/right/top, three on the bottom (`paintEntry`'s
        // `1, y+1, w-2, h-3`) — so the strip edges survive the hover.
        let text_color = if is_highlighted {
            paint::fill_area(
                &mut pixmap,
                x0 + t as i32,
                y0 + t as i32,
                w.saturating_sub(t * 2),
                item_h.saturating_sub(t * 3),
                &menu.highlight_background,
            );
            menu.highlight_text_color
        } else {
            menu.text_color
        };

        paint::draw_text(
            &mut pixmap,
            font_system,
            &mut swash_cache,
            item.label(),
            &menu.item_font,
            text_color,
            x0 + px(5),
            y0,
            content_w.saturating_sub(px(5) as u32 + gutter),
            item_h,
            TextAlign::Left,
        );

        if item.is_submenu() {
            // `paintEntry`'s engraved chevron, in entry-local stock
            // coordinates: dark spine at w-12 from y+6 down to y+h-8,
            // dim upper diagonal and light lower diagonal meeting at
            // (w-6, h/2) — thickened leftward like the Clip's creases
            // so it scales with the rest of the chrome.
            let ex = x0 + w as i32;
            let (top, bottom) = (y0 + px(6), y0 + item_h as i32 - px(8));
            let mid = y0 + (item_h / 2) as i32 - 1;
            for k in 0..t as i32 {
                tile::draw_line(&mut pixmap, ex - px(12) - k, top, ex - px(12) - k, bottom, chev_dark);
                tile::draw_line(&mut pixmap, ex - px(11) - k, top, ex - px(6) - k, mid, chev_dim);
                tile::draw_line(&mut pixmap, ex - px(11) - k, bottom, ex - px(6) - k, mid, chev_light);
            }
        }

        item_rects.push(Rect::new(Point::new(x0, y0), Size::new(content_w, item_h)));
    }

    // The frame border, drawn last so nothing overpaints it.
    let border = theme.border.color_active;
    paint::fill_rect(&mut pixmap, 0, 0, width, bw, border);
    paint::fill_rect(&mut pixmap, 0, (height - bw) as i32, width, bw, border);
    paint::fill_rect(&mut pixmap, 0, 0, bw, height, border);
    paint::fill_rect(&mut pixmap, (width - bw) as i32, 0, bw, height, border);

    MenuRender {
        buffer: DecorationBuffer { width, height, pixels: pixmap.data().to_vec() },
        item_rects,
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

    fn px_at(buffer: &DecorationBuffer, x: u32, y: u32) -> (u8, u8, u8) {
        let i = ((y * buffer.width + x) * 4) as usize;
        (buffer.pixels[i], buffer.pixels[i + 1], buffer.pixels[i + 2])
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

    /// `wMenuRealize` behavior: the menu is exactly as wide as its
    /// content requires — a longer label yields a wider popup, with no
    /// fixed width flooring everything to the same size.
    #[test]
    fn menu_width_tracks_the_widest_label() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();

        let short = render_menu(&theme, &mut font_system, "M", &[action("Exit", 1)], None);
        let long = render_menu(
            &theme,
            &mut font_system,
            "M",
            &[action("Exit", 1), action("A considerably longer menu entry", 2)],
            None,
        );

        assert!(
            long.buffer.width > short.buffer.width,
            "content sizing: {} should exceed {}",
            long.buffer.width,
            short.buffer.width
        );
    }

    /// The title strip is a real titlebar — item rows start below the
    /// window titlebar's height (plus the frame border), not below one
    /// item-height.
    #[test]
    fn title_strip_uses_the_window_titlebar_height() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();

        let render = render_menu(&theme, &mut font_system, "Chonkstep", &[action("Exit", 1)], None);

        let expected = theme.border.width.max(1) as i32 + theme.titlebar.height as i32;
        assert_eq!(render.item_rects[0].pos.y, expected);
    }

    /// Each entry is its own raised strip (`WREL_MENUENTRY`): lighter
    /// along its top edge than in its face, and terminated by an
    /// absolute black bottom line.
    #[test]
    fn entries_carry_their_own_relief() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();

        let render = render_menu(&theme, &mut font_system, "Chonkstep", &[action("Exit", 1)], None);
        let row = render.item_rects[0];
        let cx = (row.pos.x + row.size.w as i32 / 2) as u32;

        let top = px_at(&render.buffer, cx, row.pos.y as u32);
        let face = px_at(&render.buffer, cx, (row.pos.y + row.size.h as i32 / 2) as u32);
        let bottom = px_at(&render.buffer, cx, (row.pos.y + row.size.h as i32 - 1) as u32);

        assert!(top.0 > face.0, "top relief line must be lighter than the face");
        assert_eq!(bottom, (0, 0, 0), "entry must terminate in the absolute black line");
    }

    /// `paintEntry` insets the highlight fill inside the entry relief —
    /// hovering must not erase the strip's black bottom line.
    #[test]
    fn highlight_preserves_the_entry_relief_edges() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();

        let render = render_menu(&theme, &mut font_system, "Chonkstep", &[action("Exit", 1)], Some(0));
        let row = render.item_rects[0];
        let cx = (row.pos.x + row.size.w as i32 / 2) as u32;

        let bottom = px_at(&render.buffer, cx, (row.pos.y + row.size.h as i32 - 1) as u32);
        assert_eq!(bottom, (0, 0, 0), "highlighted entry must keep its black bottom line");

        // And the face between the edges really is the highlight fill
        // (sampled away from the centered label's ink).
        let fx = (row.pos.x + 2) as u32 + theme.menu.bevel.width.max(1) as u32;
        let face = px_at(&render.buffer, fx, (row.pos.y + row.size.h as i32 / 2) as u32);
        let crate::model::Fill::Solid(hl) = theme.menu.highlight_background else {
            panic!("flagship highlight is solid");
        };
        assert_eq!(face, (hl.r, hl.g, hl.b), "highlighted face must show the highlight fill");
    }

    #[test]
    fn a_submenu_row_renders_differently_from_a_plain_action_row() {
        let theme = nextstep_classic();
        let mut font_system = cosmic_text::FontSystem::new();
        let items = [submenu("Applications", vec![action("About", 1)])];

        let render = render_menu(&theme, &mut font_system, "Chonkstep", &items, None);
        let plain = render_menu(&theme, &mut font_system, "Chonkstep", &[action("Applications", 1)], None);

        assert_ne!(render.buffer.pixels, plain.buffer.pixels, "cascade chevron should make a submenu row paint differently");
    }
}
