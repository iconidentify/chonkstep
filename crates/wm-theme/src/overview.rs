//! The Overview: an Exposé-style modal panel showing every window on
//! the current workspace as a grid of live-thumbnail cards, with a
//! strip of workspace tiles along the bottom edge — all of it in this
//! theme's own chiseled language, assembled from the same recipes the
//! rest of the desktop is built from. Each card is a miniature window:
//! a real titlebar strip (the window titlebar's fill, relief and type,
//! active for the selected card exactly as focus paints a real
//! titlebar black) over a sunken well holding the captured content —
//! the same well a miniaturized window's icon tile frames its preview
//! with. The selected card sits on the Alt-Tab switcher's highlight
//! plate, the panel wears a menu's title strip and frame, and the
//! workspace strip reuses the Clip tile outright. Pure rasterization
//! and pure geometry; the desktop shell owns the full-screen surface
//! this is blitted onto, the input routing, and the modality.

use tiny_skia::Pixmap;
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

use crate::model::{TextAlign, Theme};
use crate::switcher::{blit_buffer, elide};
use crate::{paint, tile, workspace};

/// One window's card, borrowed from the shell's stored session state:
/// re-rendering on a selection move must not clone every captured
/// preview (a handful of window-sized buffers), so the entry borrows.
pub struct OverviewEntry<'a> {
    pub title: &'a str,
    /// The captured content, when the capture succeeded. A card with
    /// no preview still reads as a finished miniature window — empty
    /// well under a titled bar — never as a broken one.
    pub preview: Option<&'a DecorationBuffer>,
    /// Miniaturized windows are shown too (they live on this desk,
    /// just folded away), visually asleep: inactive titlebar always,
    /// preview dimmed. See `draw_card`.
    pub miniaturized: bool,
}

/// The panel's resolved geometry: where every card and workspace tile
/// sits, in panel-local pixels. Computed once per state change by
/// [`layout`] and kept by the shell, so rendering and hit-testing are
/// two readers of one set of rectangles and can never disagree about
/// where a click landed.
#[derive(Clone, Debug)]
pub struct OverviewLayout {
    pub panel: Size,
    /// The menu-style title strip across the top.
    pub header_h: u32,
    /// Base gutter, derived from the tile edge like the switcher's.
    pub pad: u32,
    /// Grid width in cards — what arrow-key movement steps by.
    pub cols: usize,
    /// One card rect per entry, row-major, rows and the last (possibly
    /// short) row centered.
    pub cells: Vec<Rect>,
    /// One Clip-sized tile per workspace, in a centered bottom row.
    pub strip: Vec<Rect>,
    /// The region the grid was laid out into — where the quiet
    /// "no windows" line goes when `cells` is empty.
    pub grid: Rect,
}

/// The panel's title-strip height: a real titlebar, exactly the rule
/// menus use for theirs (a posted panel's title strip *is* a window
/// titlebar, classically).
pub fn header_height(theme: &Theme) -> u32 {
    (theme.titlebar.height as u32).max(theme.menu.item_height as u32)
}

/// Lays the grid and the workspace strip into a `panel`-sized surface.
/// `tile` is the Clip/dock tile edge (already scaled — the same number
/// every other piece of shell chrome measures itself from), which
/// sizes the strip tiles and derives the gutter.
///
/// Degenerate inputs degrade instead of panicking: zero entries yield
/// an empty grid (the render draws the quiet empty state), zero
/// workspaces yield an empty strip, and a panel too small for the
/// chrome collapses cells toward zero-sized rects that draw as nothing.
pub fn layout(panel: Size, tile: u32, header_h: u32, entries: usize, workspaces: usize) -> OverviewLayout {
    let pad = (tile / 7).max(6);

    // The workspace strip claims the bottom band first — it is the
    // fixed-size furniture; the grid gets whatever is left. Tiles
    // shrink (never below legibility) rather than overflow when a
    // session has grown an implausible number of desks.
    let avail_w = panel.w.saturating_sub(pad * 2);
    let strip_edge = if workspaces == 0 {
        0
    } else {
        tile.min((avail_w / workspaces as u32).saturating_sub(pad)).max(16)
    };
    let strip_y = panel.h.saturating_sub(pad + strip_edge) as i32;
    let strip = centered_row(panel.w, strip_y, strip_edge, pad, workspaces);

    let grid = Rect {
        pos: Point::new(pad as i32, (header_h + pad) as i32),
        size: Size::new(
            panel.w.saturating_sub(pad * 2),
            (strip_y - pad as i32 - (header_h + pad) as i32).max(0) as u32,
        ),
    };

    let (cols, cells) = grid_cells(grid, pad, entries);
    OverviewLayout { panel, header_h, pad, cols, cells, strip, grid }
}

/// A centered horizontal row of `count` squares of `edge` at `y`.
fn centered_row(panel_w: u32, y: i32, edge: u32, pad: u32, count: usize) -> Vec<Rect> {
    if count == 0 || edge == 0 {
        return Vec::new();
    }
    let total = count as u32 * edge + (count as u32 - 1) * pad;
    let x0 = (panel_w as i32 - total as i32) / 2;
    (0..count)
        .map(|i| Rect {
            pos: Point::new(x0 + i as i32 * (edge + pad) as i32, y),
            size: Size::new(edge, edge),
        })
        .collect()
}

/// Picks the column count and produces the card rects. The column
/// search maximizes how large a window-shaped (roughly 16:10) card
/// fits in a cell, which is the quantity the eye actually judges a
/// grid by — maximizing raw cell area instead favors degenerate
/// one-row layouts whose cells are wide slivers.
fn grid_cells(grid: Rect, pad: u32, entries: usize) -> (usize, Vec<Rect>) {
    if entries == 0 || grid.size.w == 0 || grid.size.h == 0 {
        return (1, Vec::new());
    }
    // Cards breathe with a double gutter: a single pad left the
    // selected card's highlight plate (which inflates half a pad)
    // touching its neighbor edge-to-edge — confirmed on the first
    // rendered screenshot — and chrome that touches reads as mush,
    // the same lesson the icon tile's well margin records.
    let gap = (pad * 2) as f32;
    let (gw, gh) = (grid.size.w as f32, grid.size.h as f32);
    let mut best = (1usize, 0.0f32);
    for cols in 1..=entries {
        let rows = entries.div_ceil(cols);
        let cw = (gw - (cols as f32 - 1.0) * gap) / cols as f32;
        let ch = (gh - (rows as f32 - 1.0) * gap) / rows as f32;
        if cw <= 0.0 || ch <= 0.0 {
            continue;
        }
        // The edge of the largest 16:10 card the cell can hold.
        let fit = cw.min(ch * 1.6);
        if fit > best.1 {
            best = (cols, fit);
        }
    }
    let cols = best.0;
    let rows = entries.div_ceil(cols);
    let cw = ((gw - (cols as f32 - 1.0) * gap) / cols as f32).max(0.0);
    let ch = ((gh - (rows as f32 - 1.0) * gap) / rows as f32).max(0.0);
    // Cards are window-shaped on purpose: height is trimmed to keep a
    // landscape aspect (a captured desktop window is essentially never
    // portrait, and a portrait card is mostly letterbox band — the
    // first rendered cut looked exactly like that), and the width cap
    // keeps a one-window grid from producing a billboard that fills
    // the monitor edge to edge. A modal overview should read as a
    // panel of miniatures, not one screenful of window stretched to
    // another.
    //
    // Floored, not rounded: a half-pixel rounded up once per row adds
    // up across rows and pushes the centered grid a pixel past its
    // region — floor keeps `rows * cell_h + gaps <= grid.h` by
    // construction.
    let cell_w = cw.min(gw * 0.6).floor().max(0.0) as u32;
    let cell_h = ch.min(cell_w as f32 / 1.3).min(gh * 0.9).floor().max(0.0) as u32;

    let gap = gap as u32;
    let mut cells = Vec::with_capacity(entries);
    let rows_total_h = rows as u32 * cell_h + (rows as u32 - 1) * gap;
    let y0 = grid.pos.y + (grid.size.h as i32 - rows_total_h as i32) / 2;
    for row in 0..rows {
        // The last row may be short; every row centers what it holds.
        let in_row = (entries - row * cols).min(cols);
        let row_w = in_row as u32 * cell_w + (in_row as u32 - 1) * gap;
        let x0 = grid.pos.x + (grid.size.w as i32 - row_w as i32) / 2;
        for col in 0..in_row {
            cells.push(Rect {
                pos: Point::new(
                    x0 + col as i32 * (cell_w + gap) as i32,
                    y0 + row as i32 * (cell_h + gap) as i32,
                ),
                size: Size::new(cell_w, cell_h),
            });
        }
    }
    (cols, cells)
}

impl OverviewLayout {
    /// The card under a panel-local point, if any.
    pub fn cell_at(&self, p: Point) -> Option<usize> {
        self.cells.iter().position(|cell| cell.contains(p))
    }

    /// The workspace tile under a panel-local point, if any.
    pub fn workspace_at(&self, p: Point) -> Option<usize> {
        self.strip.iter().position(|tile| tile.contains(p))
    }
}

/// Arrow-key movement over the row-major grid: `(dx, dy)` in
/// {-1, 0, 1} steps one column or one row, clamped at every edge
/// (movement off the grid stays put — the classic panel feel, no
/// wrapping surprises). A vertical step into the short last row lands
/// on that row's nearest existing card rather than off the end.
pub fn move_selection(selected: usize, count: usize, cols: usize, dx: i32, dy: i32) -> usize {
    if count == 0 {
        return 0;
    }
    let cols = cols.max(1);
    let selected = selected.min(count - 1);
    let rows = count.div_ceil(cols);
    let (mut row, mut col) = (selected / cols, selected % cols);
    col = col.saturating_add_signed(dx as isize).min(cols - 1);
    row = row.saturating_add_signed(dy as isize).min(rows - 1);
    (row * cols + col).min(count - 1)
}

/// Rasterizes the whole panel from a layout and the entries it was
/// laid out for. `selected` is clamped, not trusted, like everywhere
/// else a shell index crosses into rendering.
#[allow(clippy::too_many_arguments)]
pub fn render_overview(
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    entries: &[OverviewEntry],
    selected: usize,
    workspace: (usize, usize),
    layout: &OverviewLayout,
) -> DecorationBuffer {
    let Some(mut pixmap) = Pixmap::new(layout.panel.w.max(1), layout.panel.h.max(1)) else {
        return DecorationBuffer { width: 0, height: 0, pixels: Vec::new() };
    };
    let selected = selected.min(entries.len().saturating_sub(1));
    let bevel_t = theme.menu.bevel.width.max(1) as u32;

    paint::fill_area(&mut pixmap, 0, 0, layout.panel.w, layout.panel.h, &theme.menu.background);

    // The title strip: a menu's — which is to say a window titlebar's —
    // treatment, the same fill/relief/type recipe `menu::render_menu`
    // uses for its own strip.
    let (current, count) = (workspace.0, workspace.1.max(1));
    paint::fill_area(&mut pixmap, 0, 0, layout.panel.w, layout.header_h, &theme.menu.title_bar);
    paint::draw_raised2_bevel(&mut pixmap, 0, 0, layout.panel.w, layout.header_h, (theme.titlebar.bevel.width as u32).max(1));
    paint::draw_text(
        &mut pixmap,
        font_system,
        swash_cache,
        &format!("Overview \u{2014} Desk {} of {}", current + 1, count),
        &theme.menu.title_font,
        theme.menu.title_text_color,
        layout.pad as i32,
        0,
        layout.panel.w.saturating_sub(layout.pad * 2),
        layout.header_h,
        TextAlign::Center,
    );

    if entries.is_empty() {
        // The quiet empty state: one line of menu-item type, centered
        // in the grid region. Deliberately not a boxed card — an empty
        // desk should look calm, not like a broken widget.
        paint::draw_text(
            &mut pixmap,
            font_system,
            swash_cache,
            "No windows on this desk",
            &theme.menu.item_font,
            theme.menu.text_color,
            layout.grid.pos.x,
            layout.grid.pos.y + (layout.grid.size.h as i32 - theme.menu.item_height as i32) / 2,
            layout.grid.size.w,
            (theme.menu.item_height as u32).max(12),
            TextAlign::Center,
        );
    }

    for (index, (entry, cell)) in entries.iter().zip(&layout.cells).enumerate() {
        draw_card(&mut pixmap, theme, font_system, swash_cache, entry, *cell, index == selected, layout.pad);
    }

    // The workspace strip: the Clip tile itself, one per desk, each
    // rendered as if that desk were current so every tile wears its
    // own number — the current desk marked by the switcher's highlight
    // plate underneath rather than by a different tile face.
    for (index, tile_rect) in layout.strip.iter().enumerate() {
        if index == current {
            highlight_plate(&mut pixmap, theme, *tile_rect, layout.pad, bevel_t);
        }
        let clip = workspace::render_clip_tile(theme, font_system, swash_cache, tile_rect.size.w, index, count);
        blit_buffer(&mut pixmap, &clip, tile_rect.pos.x, tile_rect.pos.y);
    }

    // The panel's own raised frame last, over everything, exactly like
    // the switcher's: the edge must read above the plates inside it.
    paint::draw_raised2_bevel(&mut pixmap, 0, 0, layout.panel.w, layout.panel.h, bevel_t);

    DecorationBuffer { width: layout.panel.w, height: layout.panel.h, pixels: pixmap.data().to_vec() }
}

/// The switcher's selection treatment: a highlight-filled plate
/// inflated half a pad beyond the rect, wearing the raised relief.
fn highlight_plate(pixmap: &mut Pixmap, theme: &Theme, rect: Rect, pad: u32, bevel_t: u32) {
    let ring = (pad / 2).max(2);
    let x = rect.pos.x - ring as i32;
    let y = rect.pos.y - ring as i32;
    let w = rect.size.w + ring * 2;
    let h = rect.size.h + ring * 2;
    paint::fill_area(pixmap, x, y, w, h, &theme.menu.highlight_background);
    paint::draw_raised2_bevel(pixmap, x, y, w, h, bevel_t);
}

/// One window card: a miniature of real window chrome. Titlebar strip
/// on top (active fill for the selected card — the same signal focus
/// paints on a real window — inactive otherwise, and always inactive
/// for a miniaturized window: it is asleep, and selecting it should
/// not pretend otherwise), a sunken well below holding the letterboxed
/// preview, the whole card framed by the raised relief every piece of
/// chrome here wears.
#[allow(clippy::too_many_arguments)]
fn draw_card(
    pixmap: &mut Pixmap,
    theme: &Theme,
    font_system: &mut cosmic_text::FontSystem,
    swash_cache: &mut cosmic_text::SwashCache,
    entry: &OverviewEntry,
    cell: Rect,
    selected: bool,
    pad: u32,
) {
    if cell.size.w < 8 || cell.size.h < 8 {
        return;
    }
    let t = (theme.titlebar.bevel.width as u32).max(1);
    if selected {
        highlight_plate(pixmap, theme, cell, pad, theme.menu.bevel.width.max(1) as u32);
    }

    let (x, y) = (cell.pos.x, cell.pos.y);
    let (w, h) = (cell.size.w, cell.size.h);

    // The card face under everything, so the sliver between titlebar
    // and well is theme material, not highlight bleed.
    paint::fill_area(pixmap, x, y, w, h, &theme.menu.background);

    // Titlebar: the real theme titlebar, clamped only when the cell is
    // too short to give it a third.
    let bar_h = (theme.titlebar.height as u32).min(h / 3).max(10);
    let awake = selected && !entry.miniaturized;
    let bar_fill = if awake { &theme.titlebar.active } else { &theme.titlebar.inactive };
    let ink = if awake { theme.titlebar.text_color_active } else { theme.titlebar.text_color_inactive };
    paint::fill_area(pixmap, x, y, w, bar_h, bar_fill);
    paint::draw_raised2_bevel(pixmap, x, y, w, bar_h, t);
    let label = elide(entry.title, w.saturating_sub(pad), theme.titlebar.font.size);
    paint::draw_text(
        pixmap,
        font_system,
        swash_cache,
        &label,
        &theme.titlebar.font,
        ink,
        x + (pad / 2) as i32,
        y,
        w.saturating_sub(pad),
        bar_h,
        theme.titlebar.text_align,
    );

    // The content well: shaded down and sunken, the icon tile's
    // preview treatment scaled up — the preview stays inside the
    // sunken bevel so the recess lines keep framing it, and the
    // letterbox bars show the well's shaded floor.
    let well_y = y + bar_h as i32;
    let well_h = h.saturating_sub(bar_h);
    tile::draw_tile_well(pixmap, x, well_y, w, well_h, theme);
    let inner = theme.tile.bevel.width.max(1) as i32;
    let px = x + inner;
    let py = well_y + inner;
    let pw = (w as i32 - inner * 2).max(0) as u32;
    let ph = (well_h as i32 - inner * 2).max(0) as u32;
    if let Some(src) = entry.preview.filter(|b| b.width > 0 && b.height > 0) {
        draw_preview(pixmap, src, px, py, pw, ph);
    }
    if entry.miniaturized {
        // Asleep: the content dimmed in place. Together with the
        // always-inactive titlebar this is the whole "miniaturized"
        // signal — legible at a glance without inventing a badge no
        // other chrome uses.
        paint::op_rect(pixmap, px, py, pw, ph, -48);
    }

    // The card's outer relief last, so its edge sits above both the
    // titlebar strip and the well rim.
    paint::draw_raised2_bevel(pixmap, x, y, w, h, t);
}

/// Letterboxed, never cropped — the same rule (and rationale) as the
/// icon tile's preview: cropping hides real content to gain nothing
/// but a filled corner.
fn draw_preview(dest: &mut Pixmap, src: &DecorationBuffer, x: i32, y: i32, w: u32, h: u32) {
    if w == 0 || h == 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let Some(size) = tiny_skia::IntSize::from_wh(src.width, src.height) else { return };
    let Some(src_pixmap) = tiny_skia::Pixmap::from_vec(src.pixels.clone(), size) else { return };
    let scale = (w as f32 / src.width as f32).min(h as f32 / src.height as f32);
    let dst_w = src.width as f32 * scale;
    let dst_h = src.height as f32 * scale;
    let dx = x as f32 + (w as f32 - dst_w) / 2.0;
    let dy = y as f32 + (h as f32 - dst_h) / 2.0;
    let paint = tiny_skia::PixmapPaint { quality: tiny_skia::FilterQuality::Bilinear, ..Default::default() };
    dest.draw_pixmap(0, 0, src_pixmap.as_ref(), &paint, tiny_skia::Transform::from_row(scale, 0.0, 0.0, scale, dx, dy), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn overlap(a: Rect, b: Rect) -> bool {
        a.pos.x < b.pos.x + b.size.w as i32
            && b.pos.x < a.pos.x + a.size.w as i32
            && a.pos.y < b.pos.y + b.size.h as i32
            && b.pos.y < a.pos.y + a.size.h as i32
    }

    fn inside(outer: Rect, inner: Rect) -> bool {
        inner.pos.x >= outer.pos.x
            && inner.pos.y >= outer.pos.y
            && inner.pos.x + inner.size.w as i32 <= outer.pos.x + outer.size.w as i32
            && inner.pos.y + inner.size.h as i32 <= outer.pos.y + outer.size.h as i32
    }

    /// Scale 1 (56px tile, laptop-ish panel) and scale 2 (112px tile,
    /// 4K) both must produce a grid of uniform, non-overlapping cards
    /// that stay inside the grid region, for every count a desktop
    /// plausibly holds. This is the desk the feature ships to (the
    /// reference machine runs scale 2 on 3840x2160), so it is the case
    /// the test hammers.
    #[test]
    fn grids_of_one_through_twelve_fit_without_overlap_at_both_scales() {
        for (panel, tile) in [(Size::new(1920, 1080), 56u32), (Size::new(3840, 2160), 112)] {
            for n in 1..=12usize {
                let l = layout(panel, tile, 40, n, 3);
                assert_eq!(l.cells.len(), n, "{n} windows want {n} cells");
                let (w0, h0) = (l.cells[0].size.w, l.cells[0].size.h);
                assert!(w0 > 0 && h0 > 0, "cells must have area (n={n}, panel={panel:?})");
                for (i, cell) in l.cells.iter().enumerate() {
                    assert_eq!((cell.size.w, cell.size.h), (w0, h0), "cell {i} not uniform (n={n})");
                    assert!(inside(l.grid, *cell), "cell {i} escapes the grid (n={n}, panel={panel:?})");
                    for (j, other) in l.cells.iter().enumerate().skip(i + 1) {
                        assert!(!overlap(*cell, *other), "cells {i} and {j} overlap (n={n})");
                    }
                }
            }
        }
    }

    #[test]
    fn cards_keep_window_like_proportions_and_never_billboard() {
        // One window must not become a monitor-filling billboard, and
        // no count may produce sliver cells thinner than they are
        // meaningful.
        for n in 1..=12usize {
            let l = layout(Size::new(3840, 2160), 112, 80, n, 2);
            let cell = l.cells[0];
            assert!(cell.size.w as f32 <= l.grid.size.w as f32 * 0.6 + 1.0, "n={n} card spans the panel");
            let aspect = cell.size.w as f32 / cell.size.h as f32;
            assert!((1.0..=2.2).contains(&aspect), "n={n} card aspect {aspect} not window-like (landscape)");
        }
    }

    #[test]
    fn zero_windows_is_an_empty_grid_not_a_panic() {
        let l = layout(Size::new(3840, 2160), 112, 80, 0, 1);
        assert!(l.cells.is_empty());
        assert_eq!(l.strip.len(), 1);
        assert!(l.grid.size.h > 0);
    }

    #[test]
    fn a_tiny_panel_degrades_instead_of_panicking() {
        // A panel smaller than its own chrome: nothing sensible to
        // draw, but layout and hit-testing must stay total functions.
        let l = layout(Size::new(40, 30), 56, 40, 5, 3);
        // Hit-testing over the degenerate layout stays total too.
        let _ = l.cell_at(Point::new(10, 10));
        let _ = l.workspace_at(Point::new(10, 10));
        assert!(l.cells.len() <= 5);
    }

    #[test]
    fn the_strip_maps_hits_back_to_workspace_indices() {
        let l = layout(Size::new(3840, 2160), 112, 80, 4, 3);
        assert_eq!(l.strip.len(), 3);
        for (i, tile) in l.strip.iter().enumerate() {
            let center = Point::new(tile.pos.x + tile.size.w as i32 / 2, tile.pos.y + tile.size.h as i32 / 2);
            assert_eq!(l.workspace_at(center), Some(i), "strip tile {i} misses its own center");
            assert_eq!(l.cell_at(center), None, "strip tile {i} collides with the grid");
        }
        // Between two tiles is nobody's.
        let a = l.strip[0];
        let gap = Point::new(a.pos.x + a.size.w as i32 + (l.pad / 2) as i32, a.pos.y + 4);
        assert_eq!(l.workspace_at(gap), None);
    }

    #[test]
    fn many_workspaces_shrink_the_strip_rather_than_overflow_it() {
        let l = layout(Size::new(1920, 1080), 56, 40, 2, 40);
        assert_eq!(l.strip.len(), 40);
        let first = l.strip.first().unwrap();
        let last = l.strip.last().unwrap();
        assert!(first.pos.x >= 0, "strip ran off the left edge");
        assert!(last.pos.x + (last.size.w as i32) <= 1920, "strip ran off the right edge");
        assert!(first.size.w >= 16, "strip tiles shrank below legibility");
    }

    #[test]
    fn every_cell_maps_hits_back_to_its_own_index() {
        let l = layout(Size::new(3840, 2160), 112, 80, 7, 2);
        for (i, cell) in l.cells.iter().enumerate() {
            let center = Point::new(cell.pos.x + cell.size.w as i32 / 2, cell.pos.y + cell.size.h as i32 / 2);
            assert_eq!(l.cell_at(center), Some(i));
        }
        assert_eq!(l.cell_at(Point::new(0, 0)), None, "the header is not a card");
    }

    #[test]
    fn selection_moves_by_row_and_column_and_clamps_at_every_edge() {
        // A 3-column grid of 7: rows are [0 1 2] [3 4 5] [6].
        let (count, cols) = (7, 3);
        assert_eq!(move_selection(0, count, cols, 1, 0), 1);
        assert_eq!(move_selection(2, count, cols, 1, 0), 2, "right edge clamps");
        assert_eq!(move_selection(0, count, cols, -1, 0), 0, "left edge clamps");
        assert_eq!(move_selection(1, count, cols, 0, 1), 4, "down one row");
        assert_eq!(move_selection(1, count, cols, 0, -1), 1, "top edge clamps");
        assert_eq!(move_selection(5, count, cols, 0, 1), 6, "into the short last row lands on its nearest card");
        assert_eq!(move_selection(6, count, cols, 0, 1), 6, "bottom edge clamps");
        assert_eq!(move_selection(0, 0, cols, 1, 0), 0, "no cards is a quiet zero");
        assert_eq!(move_selection(99, count, cols, 0, 0), 6, "an out-of-range selection clamps first");
    }

    fn render(n: usize, selected: usize) -> DecorationBuffer {
        let theme = crate::default_theme::nextstep_classic();
        let mut fs = cosmic_text::FontSystem::new();
        let mut sc = cosmic_text::SwashCache::new();
        let titles: Vec<String> = (0..n).map(|i| format!("window {i}")).collect();
        let entries: Vec<OverviewEntry> = titles
            .iter()
            .map(|t| OverviewEntry { title: t, preview: None, miniaturized: false })
            .collect();
        let l = layout(Size::new(960, 540), 56, header_height(&theme), n, 2);
        render_overview(&theme, &mut fs, &mut sc, &entries, selected, (0, 2), &l)
    }

    #[test]
    fn the_panel_renders_at_the_layout_size_for_zero_and_many_windows() {
        for n in [0usize, 1, 5] {
            let buffer = render(n, 0);
            assert_eq!((buffer.width, buffer.height), (960, 540), "n={n}");
            assert_eq!(buffer.pixels.len(), 960 * 540 * 4, "n={n}");
        }
    }

    #[test]
    fn moving_the_selection_visibly_changes_the_panel() {
        assert_ne!(render(4, 0).pixels, render(4, 1).pixels, "the highlight plate must move with the selection");
    }

    #[test]
    fn an_out_of_range_selection_is_clamped_not_panicking() {
        let buffer = render(2, 99);
        assert!(buffer.width > 0);
    }
}
