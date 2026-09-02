//! The instrument panel's desktop half: one shell surface beside the
//! dock, wearing the desktop's chiseled chrome around pixels a dockapp
//! streamed.
//!
//! # Ownership split
//!
//! [`RemoteTile`](super::tile::RemoteTile) owns everything that came
//! over the wire — the grant, the newest frame, the dirty flags. This
//! type owns everything the *screen* needs — the surface, its place
//! beside the owning tile, and the chrome — and `Desktop` arbitrates
//! between them (`sync_instrument_panel`): exactly one panel on screen
//! desktop-wide, the newest opener winning. The split is the same one
//! the dock itself uses: pixels arrive on a socket, chrome is drawn by
//! the shell, and the two meet at one blit.
//!
//! # Rendering pattern
//!
//! The Overview's selection card is the model: a small, shell-owned
//! surface stacked above the desktop, repainted only when its content
//! actually changed and *configured* (not repainted) when it merely
//! moves. The chrome is the tile family's own vocabulary — the tile
//! face fill under a raised relief, with the streamed content set into
//! a sunken well (`wm_theme::tile::draw_tile_well`'s recipe, restated
//! at panel size) — so the panel reads as a detached instrument face,
//! not as a window that lost its titlebar. Repainted on
//! `ThemeChanged`/appearance switches by `Desktop::relayout`, which
//! discards the surface exactly as it discards the Overview's.
//!
//! # The invariant, restated for panels
//!
//! Nothing here waits on the dockapp. The panel draws the last frame it
//! was given, or the empty well; a client that stops streaming costs
//! the desktop a stale panel and nothing else, and the ping machinery
//! that dims a hung tile tears its panel down (`RemoteTile::
//! check_liveness`).

use tiny_skia::Pixmap;
use wm_core::Backend;
use wm_theme::{paint, Theme};
use wm_theme_api::{DecorationBuffer, Point, Rect, Size};

/// Border of shell-drawn chrome around the streamed content, in device
/// pixels: the raised outer relief, a course of tile face, and the
/// sunken well lip. Three bevel widths — the same arithmetic the tile
/// well uses, scaled with the theme like every other chrome metric.
pub(crate) fn chrome_inset(theme: &Theme) -> u32 {
    (theme.tile.bevel.width.max(1) as u32) * 3
}

/// Where a panel of `content` device pixels sits: horizontally flush
/// against the dock strip's left edge, vertically aligned with the top
/// of the owning tile's slot, then clamped into the workarea so a tile
/// near the bottom of a tall dock still unfolds a fully visible panel.
pub(crate) fn place(content: (u32, u32), inset: u32, dock: Rect, slot_top: i32, workarea: Rect) -> Rect {
    let size = Size::new(content.0 + inset * 2, content.1 + inset * 2);
    let x = dock.pos.x - size.w as i32;
    let mut y = dock.pos.y + slot_top;
    let bottom = workarea.pos.y + workarea.size.h as i32;
    if y + size.h as i32 > bottom {
        y = bottom - size.h as i32;
    }
    if y < workarea.pos.y {
        y = workarea.pos.y;
    }
    Rect { pos: Point::new(x.max(workarea.pos.x), y), size }
}

/// The chrome plus content, rasterized. `frame` is the newest streamed
/// frame, already guaranteed (by `RemoteTile::on_panel_frame`'s
/// equality check) to be exactly `content`-sized; `None` draws the
/// empty well — a panel that has been granted but not yet streamed
/// should read as an instrument warming up, not as a hole.
pub(crate) fn render(theme: &Theme, content: (u32, u32), frame: Option<&DecorationBuffer>) -> Option<DecorationBuffer> {
    let inset = chrome_inset(theme);
    let (w, h) = (content.0 + inset * 2, content.1 + inset * 2);
    let mut pixmap = Pixmap::new(w.max(1), h.max(1))?;
    let t = theme.tile.bevel.width.max(1) as u32;

    // The tile family's face and outer relief, at panel size.
    paint::fill_area(&mut pixmap, 0, 0, w, h, &theme.tile.fill);
    paint::draw_raised2_bevel(&mut pixmap, 0, 0, w, h, t);

    // The sunken well the content sits in — shaded down and chiseled,
    // so the streamed pixels read as set into the instrument rather
    // than stickered onto it. The well's rect includes one bevel course
    // around the content.
    let well = Rect {
        pos: Point::new((inset - t) as i32, (inset - t) as i32),
        size: Size::new(content.0 + t * 2, content.1 + t * 2),
    };
    paint::op_rect(&mut pixmap, well.pos.x, well.pos.y, well.size.w, well.size.h, -24);
    paint::draw_sunken_bevel(&mut pixmap, well.pos.x, well.pos.y, well.size.w, well.size.h, t);

    if let Some(frame) = frame {
        if (frame.width, frame.height) == content {
            crate::desktop::blit_into(&mut pixmap, inset, inset, frame);
        }
    }
    Some(DecorationBuffer { width: pixmap.width(), height: pixmap.height(), pixels: pixmap.data().to_vec() })
}

/// The one open panel's surface and identity. Owned by `Desktop`.
pub(crate) struct InstrumentPanel<B: Backend> {
    window: Option<B::ShellId>,
    /// Root geometry the surface currently wears.
    geometry: Rect,
    /// The dockapp id the panel belongs to — the key `Desktop` uses to
    /// find the pixels, deliver input, and tear down on death. An id
    /// rather than a slot index for the same reason hover tracking uses
    /// one: the column reorders under a stationary panel.
    owner: Option<String>,
    /// Content size (without chrome) the surface was staged for.
    content: (u32, u32),
    /// Whether the pointer is inside the content area, so panel
    /// `Enter`/`Leave` are sent once per crossing.
    hovered: bool,
    /// The last content-local point a `Motion` was sent for, so a
    /// stationary pointer costs no traffic and a moving one costs at
    /// most one event per motion dispatch.
    last_motion: Option<Point>,
    visible: bool,
}

impl<B: Backend> Default for InstrumentPanel<B> {
    fn default() -> Self {
        Self { window: None, geometry: Rect::default(), owner: None, content: (0, 0), hovered: false, last_motion: None, visible: false }
    }
}

impl<B: Backend> InstrumentPanel<B> {
    pub(crate) fn visible(&self) -> bool {
        self.visible
    }

    pub(crate) fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    pub(crate) fn owns(&self, surface: B::ShellId) -> bool {
        self.visible && self.window == Some(surface)
    }

    pub(crate) fn geometry(&self) -> Rect {
        self.geometry
    }

    pub(crate) fn hovered(&self) -> bool {
        self.hovered
    }

    pub(crate) fn set_hovered(&mut self, hovered: bool) {
        self.hovered = hovered;
        if !hovered {
            self.last_motion = None;
        }
    }

    /// Records a content-local pointer position, answering whether it
    /// moved since the last record — the dedupe behind panel `Motion`.
    pub(crate) fn note_motion(&mut self, point: Point) -> bool {
        if self.last_motion == Some(point) {
            return false;
        }
        self.last_motion = Some(point);
        true
    }

    /// Maps a point local to the panel surface into content-local
    /// coordinates — the space `PanelInput` speaks. `None` for a point
    /// on the chrome: the border belongs to the shell, and a click on
    /// it is neither the client's nor a dismissal.
    pub(crate) fn content_point(&self, theme: &Theme, local: Point) -> Option<Point> {
        let inset = chrome_inset(theme) as i32;
        let (x, y) = (local.x - inset, local.y - inset);
        (x >= 0 && y >= 0 && (x as u32) < self.content.0 && (y as u32) < self.content.1).then_some(Point::new(x, y))
    }

    /// Stages the panel for `owner` at `geometry` and paints it. The
    /// surface is created on first use and kept across opens — the
    /// switcher/Overview rule, for the same reason: destroy/recreate
    /// churn is the expensive way to obtain the buffer we just had.
    pub(crate) fn show(
        &mut self,
        backend: &mut B,
        theme: &Theme,
        owner: &str,
        content: (u32, u32),
        geometry: Rect,
        frame: Option<&DecorationBuffer>,
    ) {
        self.owner = Some(owner.to_string());
        self.content = content;
        if self.window.is_none() {
            self.window = backend.create_shell_surface(geometry, wm_theme::switcher::panel_background(theme), true);
            if self.window.is_none() {
                tracing::warn!("failed to create the instrument panel surface");
                return;
            }
        }
        let Some(window) = self.window else { return };
        backend.configure_shell_surface(window, geometry);
        self.geometry = geometry;
        self.repaint(backend, theme, frame);
        if !self.visible {
            backend.map_shell_surface(window);
            self.visible = true;
        }
        backend.raise_shell_surface(window);
    }

    /// Repaints chrome and content from the stored geometry. Cheap
    /// relative to the dock's own redraw — the buffer is at most the
    /// granted panel plus its border.
    pub(crate) fn repaint(&mut self, backend: &mut B, theme: &Theme, frame: Option<&DecorationBuffer>) {
        let Some(window) = self.window else { return };
        if let Some(buffer) = render(theme, self.content, frame) {
            backend.paint_shell_surface(window, &buffer);
        }
    }

    /// Takes the panel off screen and forgets its owner. The surface is
    /// kept for the next open.
    pub(crate) fn hide(&mut self, backend: &mut B) {
        if let Some(window) = self.window {
            backend.unmap_shell_surface(window);
        }
        self.visible = false;
        self.owner = None;
        self.hovered = false;
        self.last_motion = None;
    }

    /// Destroys the surface outright — for a restyle, rescale or
    /// monitor change, after which its size and pixels are both wrong.
    pub(crate) fn discard(&mut self, backend: &mut B) {
        if let Some(window) = self.window.take() {
            backend.destroy_shell_surface(window);
        }
        self.visible = false;
        self.owner = None;
        self.hovered = false;
        self.last_motion = None;
        self.geometry = Rect::default();
        self.content = (0, 0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        wm_theme::default_theme::all_themes().into_iter().next().expect("the theme set is never empty")
    }

    fn area(pos: (i32, i32), size: (u32, u32)) -> Rect {
        Rect { pos: Point::new(pos.0, pos.1), size: Size::new(size.0, size.1) }
    }

    #[test]
    fn a_panel_sits_flush_against_the_dock_and_level_with_its_tile() {
        let dock = area((1864, 0), (56, 448));
        let workarea = area((0, 0), (1864, 1080));
        let inset = 6;
        let rect = place((300, 200), inset, dock, 112, workarea);
        assert_eq!(rect.pos.x + rect.size.w as i32, dock.pos.x, "flush against the dock strip");
        assert_eq!(rect.pos.y, 112, "level with the owning tile's slot");
        assert_eq!(rect.size, Size::new(300 + inset * 2, 200 + inset * 2));
    }

    #[test]
    fn a_panel_for_a_low_tile_is_clamped_into_the_workarea() {
        let dock = area((1864, 0), (56, 1080));
        let workarea = area((0, 0), (1864, 1080));
        let rect = place((300, 400), 6, dock, 1000, workarea);
        assert_eq!(rect.pos.y + rect.size.h as i32, 1080, "pulled up so it stays fully on screen");
        let rect = place((300, 2000), 6, dock, 0, workarea);
        assert_eq!(rect.pos.y, 0, "never pushed above the workarea either");
    }

    #[test]
    fn the_chrome_is_a_border_around_untouched_content_pixels() {
        let theme = theme();
        let inset = chrome_inset(&theme);
        let content = (40, 30);
        let frame = DecorationBuffer {
            width: 40,
            height: 30,
            pixels: [0x00, 0xFF, 0x00, 0xFF].repeat(40 * 30), // premultiplied pure green
        };
        let buffer = render(&theme, content, Some(&frame)).unwrap();
        assert_eq!((buffer.width, buffer.height), (40 + inset * 2, 30 + inset * 2));
        // Center of the content region is the client's pixel verbatim.
        let (cx, cy) = (inset + 20, inset + 15);
        let at = ((cy * buffer.width + cx) * 4) as usize;
        assert_eq!(&buffer.pixels[at..at + 4], &[0x00, 0xFF, 0x00, 0xFF], "streamed pixels are blitted, not restyled");
        // The corner is chrome: opaque and not the client's green.
        assert_eq!(buffer.pixels[3], 255, "chrome is opaque");
        assert_ne!(&buffer.pixels[0..3], &[0x00, 0xFF, 0x00], "the border is the shell's, not the client's");
    }

    #[test]
    fn an_unstreamed_panel_renders_the_empty_well_not_garbage() {
        let theme = theme();
        let buffer = render(&theme, (64, 48), None).unwrap();
        assert_eq!(buffer.pixels.len(), (buffer.width * buffer.height * 4) as usize);
        assert!(buffer.pixels.as_chunks::<4>().0.iter().all(|px| px[3] == 255), "fully painted, fully opaque");
    }

    #[test]
    fn a_wrong_sized_frame_is_never_blitted_into_the_chrome() {
        // Defence in depth behind `on_panel_frame`'s equality check —
        // the same third lock the tile's render path keeps.
        let theme = theme();
        let inset = chrome_inset(&theme);
        let wrong = DecorationBuffer { width: 10, height: 10, pixels: [0xFF, 0x00, 0x00, 0xFF].repeat(100) };
        let buffer = render(&theme, (40, 30), Some(&wrong)).unwrap();
        let (cx, cy) = (inset + 5, inset + 5);
        let at = ((cy * buffer.width + cx) * 4) as usize;
        assert_ne!(&buffer.pixels[at..at + 4], &[0xFF, 0x00, 0x00, 0xFF], "a stale-grant frame is refused, not drawn");
    }
}
